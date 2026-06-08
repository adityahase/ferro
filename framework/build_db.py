#!/usr/bin/env python3
"""
build_db.py — materialise Frappe app DocType schema into a ferro SQLite site.

This is a faithful, dependency-free re-implementation of the part of `bench migrate`
that the ferro runtime depends on: for every `tab<DocType>` it
  - CREATE TABLE with the 14 standard columns + one column per physical docfield, and
  - INSERT metadata rows into tabDocType / tabDocField / tabDocPerm.

It needs neither the real Frappe framework nor a server: a DocType `*.json` file is a
self-contained schema. Idempotent (CREATE TABLE IF NOT EXISTS / INSERT OR REPLACE /
ALTER TABLE ADD COLUMN), so re-running it migrates new fields in place.

Paths are resolved from flags or the environment so it works against any forge/site:

  python3 build_db.py --site sites/dev.localhost --apps crm,hrms
  python3 build_db.py --db path/to/site.db --repos /path/to/apps crm helpdesk

  env: FERRO_SITE_DB   path to the site .db          (overridden by --db/--site)
       FERRO_REPOS     dir holding the app repos      (overridden by --repos)
       FERRO_APPS      comma list of apps             (overridden by --apps/positional)
"""
import argparse
import glob
import hashlib
import json
import os
import sqlite3
import sys

# fieldtypes that get NO physical column in the parent table (mirror ferro meta.rs is_virtual_column)
NO_COLUMN = {
    "Table", "Table MultiSelect", "Section Break", "Column Break", "Tab Break",
    "HTML", "Heading", "Button", "Fold",
}
INT_TYPES = {"Int", "Long Int", "Check"}
FLT_TYPES = {"Currency", "Float", "Percent", "Rate"}

STANDARD_COLS = [
    ("name", "TEXT PRIMARY KEY"),
    ("creation", "TEXT"),
    ("modified", "TEXT"),
    ("modified_by", "TEXT"),
    ("owner", "TEXT"),
    ("docstatus", "INTEGER DEFAULT 0"),
    ("idx", "INTEGER DEFAULT 0"),
    ("parent", "TEXT"),
    ("parentfield", "TEXT"),
    ("parenttype", "TEXT"),
    ("_user_tags", "TEXT"),
    ("_comments", "TEXT"),
    ("_assign", "TEXT"),
    ("_liked_by", "TEXT"),
]


def q(ident):
    return '"' + ident.replace('"', '""') + '"'


def col_type(fieldtype):
    if fieldtype in INT_TYPES:
        return "INTEGER DEFAULT 0"
    if fieldtype in FLT_TYPES:
        return "REAL DEFAULT 0"
    return "TEXT"


def inner_pkg_dir(repos, app):
    cand = os.path.join(repos, app, app)
    return cand if os.path.isdir(cand) else os.path.join(repos, app)


def find_doctype_jsons(repos, app):
    base = inner_pkg_dir(repos, app)
    out = []
    for path in glob.glob(os.path.join(base, "**", "doctype", "*", "*.json"), recursive=True):
        d = os.path.dirname(path)
        # the doctype JSON shares the folder name (scrubbed): .../doctype/<scrub>/<scrub>.json
        if os.path.basename(path)[:-5] != os.path.basename(d):
            continue
        try:
            j = json.load(open(path))
        except Exception:
            continue
        if j.get("doctype") == "DocType" and j.get("name"):
            out.append((path, j))
    return out


def colset(con, table):
    try:
        return {r[1] for r in con.execute(f"PRAGMA table_info({q(table)})")}
    except sqlite3.OperationalError:
        return set()


def name_id(*parts):
    return hashlib.md5("::".join(parts).encode()).hexdigest()[:10]


def resolve_db(args):
    if args.db:
        return args.db
    if os.environ.get("FERRO_SITE_DB"):
        return os.environ["FERRO_SITE_DB"]
    site = args.site or os.environ.get("FERRO_SITE")
    if site:
        hits = sorted(glob.glob(os.path.join(site, "db", "*.db")))
        if hits:
            return hits[0]
        raise SystemExit(f"no .db under {site}/db — run `ferro new-site` first")
    raise SystemExit("need --db, --site, or FERRO_SITE_DB")


def resolve_apps(args, repos):
    raw = args.apps or os.environ.get("FERRO_APPS")
    if args.positional:
        names = args.positional
    elif raw:
        names = [a.strip() for a in raw.split(",") if a.strip()]
    else:  # every repo dir present
        names = sorted(d for d in os.listdir(repos) if os.path.isdir(os.path.join(repos, d)))
    return [a for a in names if os.path.isdir(os.path.join(repos, a))]


def main():
    ap = argparse.ArgumentParser(description="Materialise Frappe app schema into a ferro SQLite site.")
    ap.add_argument("positional", nargs="*", help="app names (alt to --apps)")
    ap.add_argument("--db", help="path to the site .db")
    ap.add_argument("--site", help="site dir (uses <site>/db/*.db)")
    ap.add_argument("--repos", default=os.environ.get("FERRO_REPOS"), help="dir holding app repos")
    ap.add_argument("--apps", help="comma-separated app names")
    args = ap.parse_args()

    repos = args.repos
    if not repos or not os.path.isdir(repos):
        raise SystemExit("need --repos (or FERRO_REPOS) pointing at the apps dir")
    db = resolve_db(args)
    apps = resolve_apps(args, repos)
    if not apps:
        raise SystemExit(f"no apps found under {repos}")

    con = sqlite3.connect(db)
    con.execute("PRAGMA foreign_keys=OFF")

    dt_cols = colset(con, "tabDocType")
    df_cols = colset(con, "tabDocField")
    dp_cols = colset(con, "tabDocPerm")
    if not dt_cols:
        raise SystemExit(f"{db} has no tabDocType — is this a ferro site DB? Run `ferro new-site`.")

    NOW = "2026-06-07 00:00:00.000000"
    stats = {a: dict(doctypes=0, tables=0, singles=0, virtual=0, child=0, fields=0, perms=0, skipped=0) for a in apps}
    # Protect frappe-core meta: a DocType name is globally unique in Frappe, so an app never
    # *redefines* a core doctype (Contact/Address/ToDo/…) — it extends it via custom fields.
    # The seed marks core doctypes with app IS NULL; app doctypes carry app=<name> and may be
    # (re)materialised idempotently. If there's no `app` column, conservatively protect every
    # pre-existing doctype so we can never clobber the seed.
    if "app" in dt_cols:
        protected = {r[0] for r in con.execute("SELECT name FROM tabDocType WHERE app IS NULL")}
    else:
        protected = {r[0] for r in con.execute("SELECT name FROM tabDocType")}
    fresh = set()  # intra-run dedup (two apps shipping the same DocType name)

    for app in apps:
        for path, j in find_doctype_jsons(repos, app):
            dt = j["name"]
            if dt in fresh:
                stats[app]["skipped"] += 1
                continue
            if dt in protected:
                # never overwrite a core / seed-owned doctype's table or meta
                stats[app]["skipped"] += 1
                continue
            fresh.add(dt)
            issingle = int(bool(j.get("issingle")))
            istable = int(bool(j.get("istable")))
            is_virtual = int(bool(j.get("is_virtual")))
            table = "tab" + dt
            fields = j.get("fields", [])

            # ---- physical table ----
            if not issingle and not is_virtual:
                created = {c for c, _ in STANDARD_COLS}
                coldefs = [f"{q(c)} {t}" for c, t in STANDARD_COLS]
                for f in fields:
                    fn = f.get("fieldname")
                    ft = f.get("fieldtype", "Data")
                    if not fn or ft in NO_COLUMN or fn in created:
                        continue
                    created.add(fn)
                    coldefs.append(f"{q(fn)} {col_type(ft)}")
                con.execute(f'CREATE TABLE IF NOT EXISTS {q(table)} ({", ".join(coldefs)})')
                # add any columns missing on a pre-existing table (idempotent migrate)
                existing = colset(con, table)
                for f in fields:
                    fn = f.get("fieldname"); ft = f.get("fieldtype", "Data")
                    if fn and ft not in NO_COLUMN and fn not in existing and fn not in {c for c, _ in STANDARD_COLS}:
                        try:
                            con.execute(f"ALTER TABLE {q(table)} ADD COLUMN {q(fn)} {col_type(ft)}")
                            existing.add(fn)
                        except sqlite3.OperationalError:
                            pass
                stats[app]["tables"] += 1
                if istable:
                    stats[app]["child"] += 1
            elif issingle:
                stats[app]["singles"] += 1
            elif is_virtual:
                stats[app]["virtual"] += 1

            # ---- tabDocType metadata row ----
            row = {
                "name": dt, "creation": NOW, "modified": NOW, "modified_by": "Administrator",
                "owner": "Administrator", "docstatus": 0, "idx": 0,
                "module": j.get("module", app), "issingle": issingle, "istable": istable,
                "is_virtual": is_virtual, "autoname": j.get("autoname"),
                "naming_rule": j.get("naming_rule"), "title_field": j.get("title_field"),
                "sort_field": j.get("sort_field", "modified"),
                "sort_order": j.get("sort_order", "DESC"),
                "is_submittable": int(bool(j.get("is_submittable"))),
                "custom": 0, "engine": "InnoDB", "app": app,
                "track_changes": int(bool(j.get("track_changes"))),
                "editable_grid": int(bool(j.get("editable_grid", 1))),
            }
            row = {k: v for k, v in row.items() if k in dt_cols}
            cols = ", ".join(q(k) for k in row)
            ph = ", ".join("?" for _ in row)
            con.execute(f'INSERT OR REPLACE INTO "tabDocType" ({cols}) VALUES ({ph})', list(row.values()))

            # ---- tabDocField rows ----
            con.execute('DELETE FROM "tabDocField" WHERE parent=?', (dt,))
            for idx, f in enumerate(fields, start=1):
                fn = f.get("fieldname")
                if not fn:
                    continue
                r = {
                    "name": name_id(dt, fn), "creation": NOW, "modified": NOW,
                    "modified_by": "Administrator", "owner": "Administrator", "docstatus": 0,
                    "parent": dt, "parenttype": "DocType", "parentfield": "fields", "idx": idx,
                    "fieldname": fn, "label": f.get("label"), "fieldtype": f.get("fieldtype", "Data"),
                    "options": f.get("options"), "reqd": int(bool(f.get("reqd"))),
                    "default": f.get("default"), "permlevel": int(f.get("permlevel", 0) or 0),
                    "read_only": int(bool(f.get("read_only"))),
                    "in_list_view": int(bool(f.get("in_list_view"))),
                    "unique": int(bool(f.get("unique"))),
                    "is_virtual": int(bool(f.get("is_virtual"))),
                    "fetch_from": f.get("fetch_from"),
                    "precision": f.get("precision"),
                }
                r = {k: v for k, v in r.items() if k in df_cols}
                cols = ", ".join(q(k) for k in r)
                ph = ", ".join("?" for _ in r)
                # OR REPLACE: a malformed DocType JSON with a duplicate fieldname (same
                # md5(dt::fieldname) PK) must not abort the whole migration mid-run.
                con.execute(f'INSERT OR REPLACE INTO "tabDocField" ({cols}) VALUES ({ph})', list(r.values()))
                stats[app]["fields"] += 1

            # ---- tabDocPerm rows ----
            con.execute('DELETE FROM "tabDocPerm" WHERE parent=?', (dt,))
            for idx, p in enumerate(j.get("permissions", []), start=1):
                r = {
                    "name": name_id(dt, "perm", str(idx)), "creation": NOW, "modified": NOW,
                    "modified_by": "Administrator", "owner": "Administrator", "docstatus": 0,
                    "parent": dt, "parenttype": "DocType", "parentfield": "permissions", "idx": idx,
                    "role": p.get("role"), "permlevel": int(p.get("permlevel", 0) or 0),
                    "read": int(bool(p.get("read"))), "write": int(bool(p.get("write"))),
                    "create": int(bool(p.get("create"))), "delete": int(bool(p.get("delete"))),
                    "submit": int(bool(p.get("submit"))), "cancel": int(bool(p.get("cancel"))),
                    "report": int(bool(p.get("report"))), "export": int(bool(p.get("export"))),
                    "share": int(bool(p.get("share"))), "print": int(bool(p.get("print"))),
                    "email": int(bool(p.get("email"))), "if_owner": int(bool(p.get("if_owner"))),
                }
                r = {k: v for k, v in r.items() if k in dp_cols}
                cols = ", ".join(q(k) for k in r)
                ph = ", ".join("?" for _ in r)
                con.execute(f'INSERT OR REPLACE INTO "tabDocPerm" ({cols}) VALUES ({ph})', list(r.values()))
                stats[app]["perms"] += 1

            stats[app]["doctypes"] += 1

    con.commit()
    total_dt = con.execute('SELECT COUNT(*) FROM "tabDocType"').fetchone()[0]
    tab_tables = con.execute("SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name LIKE 'tab%'").fetchone()[0]
    con.close()

    print("=== build_db: per-app ===")
    for a in apps:
        s = stats[a]
        print(f"  {a:10s} doctypes={s['doctypes']:4d} tables={s['tables']:4d} child={s['child']:4d} "
              f"singles={s['singles']:3d} virtual={s['virtual']:3d} fields={s['fields']:6d} "
              f"perms={s['perms']:5d} skipped={s['skipped']}")
    print(f"=== totals: tabDocType rows={total_dt}  tab* tables={tab_tables}  db={db}")


if __name__ == "__main__":
    main()
