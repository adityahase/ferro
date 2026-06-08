#!/usr/bin/env python3
"""Import an installed app's Workspace fixtures into a ferro site DB so the Desk sidebar
reflects the app. build_db.py materialises DocType *schema* only; Workspace records are
fixtures (`<app>/<module>/workspace/<name>/<name>.json`) — this loads them.

A Workspace fixture is one Workspace doc whose child rows (links/shortcuts/charts/
number_cards/quick_lists/custom_blocks/roles) are nested arrays. We insert the parent row
into tabWorkspace and each child array into its child table, keeping only columns that the
real table actually has (PRAGMA-authoritative, same discipline as build_db).

Usage: import_workspaces.py --db <site.db> --app-dir <repo/app> [--app-dir ...]
Idempotent: an existing Workspace name is replaced (its old child rows are deleted first).
"""
import argparse, glob, json, os, sqlite3, time

# child array key -> (child table, parentfield)
CHILD_TABLES = {
    "links":         ("Workspace Link", "links"),
    "shortcuts":     ("Workspace Shortcut", "shortcuts"),
    "charts":        ("Workspace Chart", "charts"),
    "number_cards":  ("Workspace Number Card", "number_cards"),
    "quick_lists":   ("Workspace Quick List", "quick_lists"),
    "custom_blocks": ("Workspace Custom Block", "custom_blocks"),
    "roles":         ("Has Role", "roles"),
}
NOW = time.strftime("%Y-%m-%d %H:%M:%S")


def cols(con, table):
    try:
        return {r[1] for r in con.execute(f'PRAGMA table_info("{table}")')}
    except sqlite3.Error:
        return set()


def insert_row(con, table, have, values):
    """Insert `values` (dict) into `table`, keeping only existing columns."""
    row = {k: v for k, v in values.items() if k in have}
    # JSON-encode any dict/list that survived into a scalar column
    for k, v in list(row.items()):
        if isinstance(v, (dict, list)):
            row[k] = json.dumps(v)
        elif isinstance(v, bool):
            row[k] = 1 if v else 0
    placeholders = ",".join("?" for _ in row)
    collist = ",".join(f'"{c}"' for c in row)
    con.execute(f'INSERT INTO "{table}" ({collist}) VALUES ({placeholders})', list(row.values()))


def import_workspace(con, doc, wcols, child_have):
    name = doc.get("name") or doc.get("label")
    if not name:
        return 0
    # replace any existing record of the same name (idempotent re-runs)
    con.execute('DELETE FROM "tabWorkspace" WHERE name=?', (name,))
    for ckey, (ctable, _pf) in CHILD_TABLES.items():
        con.execute(f'DELETE FROM "tab{ctable}" WHERE parent=? AND parenttype=\'Workspace\'', (name,))

    parent = {k: v for k, v in doc.items() if k not in CHILD_TABLES}
    parent.setdefault("name", name)
    parent.setdefault("creation", NOW)
    parent.setdefault("modified", NOW)
    parent.setdefault("owner", "Administrator")
    parent.setdefault("modified_by", "Administrator")
    parent.setdefault("docstatus", 0)
    parent.setdefault("public", 1)
    insert_row(con, "tabWorkspace", wcols, parent)

    n = 1
    for ckey, (ctable, pf) in CHILD_TABLES.items():
        rows = doc.get(ckey) or []
        have = child_have.get(ctable)
        if not have:
            continue
        for idx, crow in enumerate(rows, 1):
            if not isinstance(crow, dict):
                continue
            r = dict(crow)
            r.setdefault("name", f"{name[:40]}-{ckey}-{idx}-{int(time.time()*1000)%100000}")
            r["parent"] = name
            r["parenttype"] = "Workspace"
            r["parentfield"] = pf
            r.setdefault("idx", idx)
            r.setdefault("creation", NOW)
            r.setdefault("modified", NOW)
            r.setdefault("owner", "Administrator")
            r.setdefault("modified_by", "Administrator")
            r.setdefault("docstatus", 0)
            insert_row(con, f"tab{ctable}", have, r)
            n += 1
    return 1


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--db", required=True)
    ap.add_argument("--app-dir", action="append", required=True,
                    help="path to an app repo root (or the app package dir)")
    args = ap.parse_args()

    con = sqlite3.connect(args.db)
    con.execute("PRAGMA foreign_keys=OFF")
    if not cols(con, "tabWorkspace"):
        print("no tabWorkspace table in this site; nothing to do")
        return
    wcols = cols(con, "tabWorkspace")
    child_have = {ctable: cols(con, f"tab{ctable}") for (ctable, _pf) in CHILD_TABLES.values()}

    total = 0
    for app_dir in args.app_dir:
        files = glob.glob(os.path.join(app_dir, "**", "workspace", "*", "*.json"), recursive=True)
        for f in files:
            # skip non-fixture json (e.g. a __init__ or test); a Workspace fixture's basename
            # matches its folder name and doctype == "Workspace"
            try:
                doc = json.load(open(f))
            except (json.JSONDecodeError, OSError):
                continue
            if doc.get("doctype") != "Workspace":
                continue
            try:
                total += import_workspace(con, doc, wcols, child_have)
            except sqlite3.Error as e:
                print(f"  skip {os.path.basename(f)}: {e}")
    con.commit()
    print(f"imported {total} workspaces")


if __name__ == "__main__":
    main()
