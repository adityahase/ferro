#!/usr/bin/env python3
"""Import an installed app's Number Card + Dashboard Chart fixtures into a ferro site DB.

Workspaces reference number cards and charts by name (the `number_cards`/`charts` child
rows hold a `number_card_name`/`chart_name`). The Desk number-card / chart *widgets* then
`frappe.model.with_doc("Number Card", name)` / `("Dashboard Chart", name)` — so unless the
actual record exists, the widget silently renders nothing and the workspace shows empty
boxes where its dashboards should be. build_db.py / the carved seed materialise frappe's
records, but ERPNext's (`<app>/<module>/number_card/<name>/<name>.json`,
`<app>/<module>/dashboard_chart/<name>/<name>.json`) are never loaded by the native install.
This loads them, the same PRAGMA-authoritative way import-workspaces.py loads Workspaces.

Usage: import_dashboards.py --db <site.db> --app-dir <repo/app> [--app-dir ...]
Idempotent: an existing record (by name) is replaced (its old child rows are deleted first).
"""
import argparse, glob, json, os, sqlite3, time

# (doctype, fixture-folder) pairs to import.
FIXTURES = [
    ("Number Card", "number_card"),
    ("Dashboard Chart", "dashboard_chart"),
]
NOW = time.strftime("%Y-%m-%d %H:%M:%S")


def cols(con, table):
    try:
        return {r[1] for r in con.execute(f'PRAGMA table_info("{table}")')}
    except sqlite3.Error:
        return set()


def child_fields(con, doctype):
    """{fieldname: child_doctype} for every Table / Table MultiSelect field of `doctype`."""
    out = {}
    try:
        for fn, opt in con.execute(
            'SELECT fieldname, options FROM "tabDocField" WHERE parent=? '
            "AND fieldtype IN ('Table','Table MultiSelect')",
            (doctype,),
        ):
            if fn and opt:
                out[fn] = opt.strip()
    except sqlite3.Error:
        pass
    return out


def insert_row(con, table, have, values):
    row = {k: v for k, v in values.items() if k in have}
    for k, v in list(row.items()):
        if isinstance(v, (dict, list)):
            row[k] = json.dumps(v)
        elif isinstance(v, bool):
            row[k] = 1 if v else 0
    if not row:
        return
    placeholders = ",".join("?" for _ in row)
    collist = ",".join(f'"{c}"' for c in row)
    con.execute(f'INSERT INTO "{table}" ({collist}) VALUES ({placeholders})', list(row.values()))


def import_doc(con, doctype, doc, pcols, kids, child_have):
    name = doc.get("name") or doc.get("label") or doc.get("chart_name")
    if not name:
        return 0
    table = "tab" + doctype
    con.execute(f'DELETE FROM "{table}" WHERE name=?', (name,))
    for fn, cdt in kids.items():
        con.execute(f'DELETE FROM "tab{cdt}" WHERE parent=? AND parenttype=?', (name, doctype))

    parent = {k: v for k, v in doc.items() if k not in kids and k != "doctype"}
    parent.setdefault("name", name)
    parent.setdefault("creation", NOW)
    parent.setdefault("modified", NOW)
    parent.setdefault("owner", "Administrator")
    parent.setdefault("modified_by", "Administrator")
    parent.setdefault("docstatus", 0)
    insert_row(con, table, pcols, parent)

    for fn, cdt in kids.items():
        rows = doc.get(fn) or []
        have = child_have.get(cdt)
        if not have:
            continue
        for idx, crow in enumerate(rows, 1):
            if not isinstance(crow, dict):
                continue
            r = dict(crow)
            r.setdefault("name", f"{name[:40]}-{fn}-{idx}-{int(time.time()*1000)%100000}")
            r["parent"] = name
            r["parenttype"] = doctype
            r["parentfield"] = fn
            r.setdefault("idx", idx)
            r.setdefault("creation", NOW)
            r.setdefault("modified", NOW)
            r.setdefault("owner", "Administrator")
            r.setdefault("modified_by", "Administrator")
            r.setdefault("docstatus", 0)
            insert_row(con, f"tab{cdt}", have, r)
    return 1


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--db", required=True)
    ap.add_argument("--app-dir", action="append", required=True,
                    help="path to an app repo root (or the app package dir)")
    args = ap.parse_args()

    con = sqlite3.connect(args.db)
    con.execute("PRAGMA foreign_keys=OFF")
    total = 0
    for doctype, folder in FIXTURES:
        table = "tab" + doctype
        pcols = cols(con, table)
        if not pcols:
            print(f"no {table} table; skipping {doctype}")
            continue
        kids = child_fields(con, doctype)
        child_have = {cdt: cols(con, f"tab{cdt}") for cdt in kids.values()}
        n = 0
        for app_dir in args.app_dir:
            for f in glob.glob(os.path.join(app_dir, "**", folder, "*", "*.json"), recursive=True):
                try:
                    doc = json.load(open(f))
                except (json.JSONDecodeError, OSError):
                    continue
                if doc.get("doctype") != doctype:
                    continue
                try:
                    n += import_doc(con, doctype, doc, pcols, kids, child_have)
                except sqlite3.Error as e:
                    print(f"  skip {os.path.basename(f)}: {e}")
        con.commit()
        print(f"imported {n} {doctype} records")
        total += n
    con.close()
    print(f"done: {total} dashboard fixtures")


if __name__ == "__main__":
    main()
