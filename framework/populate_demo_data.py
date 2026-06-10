#!/usr/bin/env python3
"""
populate_demo_data.py — fill the read-path doctypes with representative rows so a
load-test/measure peak reflects real list-view payloads (every physical column
populated). Idempotent-ish: tops each table up to TARGET rows.

  python3 populate_demo_data.py --site sites/dev.localhost --rows 3000
  python3 populate_demo_data.py --db path/to/site.db 3000 --doctypes "CRM Deal,HD Ticket"

  env: FERRO_SITE_DB / FERRO_SITE   (the site .db / site dir)
"""
import argparse
import glob
import os
import sqlite3

# the doctypes a typical load-test reads (ferrod.rs read_doctypes); only those present are filled
DEFAULT_DOCTYPES = [
    "CRM Deal", "CRM Lead", "HD Ticket", "GP Project", "Employee", "Sales Invoice",
    "Item", "Customer", "Contact", "ToDo", "Sales Order", "Purchase Invoice",
]
NOW = "2026-06-07 00:00:00.000000"
STR = "Representative demo value for list-view payload sizing"  # ~54 chars


def q(s):
    return '"' + s.replace('"', '""') + '"'


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
    raise SystemExit("need --db, --site, or FERRO_SITE_DB")


# JSON-meta columns the frontend JSON.parses; a non-JSON value throws and blanks the view.
_JSON_META = {"_assign", "_liked_by", "_comments", "_user_tags", "_seen"}
NOW_DATE = "2026-06-07"
NOW_TIME = "10:30:00"


def _demo_value(dt, c, sqltype, fieldtype, options, i):
    """A type-valid demo value for column `c` (Frappe `fieldtype` when known, else SQLite type)."""
    if c == "name":
        return f"{dt}-DEMO-{i:06d}"
    if c in ("creation", "modified"):
        return NOW
    if c in ("owner", "modified_by"):
        return "Administrator"
    if c in ("docstatus", "idx"):
        return 0
    if c in ("parent", "parentfield", "parenttype"):
        return None
    if c in _JSON_META:
        return None  # leave NULL: frontend treats absent as empty, parses present as JSON
    ft = fieldtype or ""
    if ft in ("Datetime",):
        return NOW
    if ft in ("Date",):
        return NOW_DATE
    if ft in ("Time",):
        return NOW_TIME
    if ft in ("Check",):
        return i % 2
    if ft in ("Int", "Long Int"):
        return i % 97
    if ft in ("Float", "Currency", "Percent"):
        return round((i % 1000) * 1.5, 2)
    if ft in ("Duration",):
        return (i % 12) * 3600
    if ft in ("JSON", "Code") and (c.endswith("_json") or ft == "JSON"):
        return "{}"
    if ft in ("Select",):
        # first non-empty option, else NULL (an arbitrary string isn't a valid Select)
        if options:
            for opt in str(options).split("\n"):
                if opt.strip():
                    return opt.strip()
        return None
    if ft in ("Link", "Dynamic Link", "Table", "Table MultiSelect"):
        return None  # a fabricated link target won't resolve; NULL renders cleanly
    if ft in ("Attach", "Attach Image", "Image", "Color", "Signature", "Password"):
        return None
    # fall back to the SQLite storage type for std/unmapped columns
    if "INT" in sqltype:
        return i % 97
    if "REAL" in sqltype or sqltype in ("FLOAT", "DOUBLE"):
        return round((i % 1000) * 1.5, 2)
    if ft in ("Data", "Small Text", "Text", "Long Text", "Text Editor", "HTML Editor", "", None):
        return STR
    return STR


def main():
    ap = argparse.ArgumentParser(description="Populate ferro read-path doctypes with demo rows.")
    ap.add_argument("rows", nargs="?", type=int, default=3000, help="rows per doctype (default 3000)")
    ap.add_argument("--rows", dest="rows_opt", type=int, help="rows per doctype")
    ap.add_argument("--db", help="path to the site .db")
    ap.add_argument("--site", help="site dir (uses <site>/db/*.db)")
    ap.add_argument("--doctypes", help="comma-separated doctype list (default: the read-path set)")
    args = ap.parse_args()

    target = args.rows_opt if args.rows_opt is not None else args.rows
    db = resolve_db(args)
    doctypes = ([d.strip() for d in args.doctypes.split(",")] if args.doctypes else DEFAULT_DOCTYPES)

    con = sqlite3.connect(db)
    con.execute("PRAGMA foreign_keys=OFF")
    total = 0
    for dt in doctypes:
        table = "tab" + dt
        try:
            info = list(con.execute(f"PRAGMA table_info({q(table)})"))
        except sqlite3.OperationalError:
            print(f"  skip {dt} (no table)")
            continue
        if not info:
            print(f"  skip {dt} (no table — install its app first)")
            continue
        have = con.execute(f"SELECT COUNT(*) FROM {q(table)}").fetchone()[0]
        need = max(0, target - have)
        if need == 0:
            print(f"  {dt:18s} already has {have}")
            continue
        cols = [(r[1], (r[2] or "").upper()) for r in info]  # (name, type)
        colnames = [c for c, _ in cols]
        # Frappe fieldtype per column (from tabDocField) so demo values are *type-valid*: the SPA
        # frontends parse/format these (dayjs on Datetime, JSON.parse on _assign/_liked_by, number
        # formatters on Float/Currency). Stuffing the placeholder string into a Datetime or JSON
        # column threw in the browser and blanked the whole list/form view.
        ftype = {}
        fopts = {}
        try:
            for fn, ft, op in con.execute(
                'SELECT fieldname, fieldtype, options FROM "tabDocField" WHERE parent=?', (dt,)
            ):
                ftype[fn] = ft
                fopts[fn] = op
        except sqlite3.OperationalError:
            pass
        placeholders = ", ".join("?" for _ in colnames)
        sql = f'INSERT OR IGNORE INTO {q(table)} ({", ".join(q(c) for c in colnames)}) VALUES ({placeholders})'
        rows = []
        for i in range(have, have + need):
            vals = []
            for c, t in cols:
                vals.append(_demo_value(dt, c, t, ftype.get(c), fopts.get(c), i))
            rows.append(vals)
        con.executemany(sql, rows)
        total += need
        print(f"  {dt:18s} +{need} -> {have + need}  ({len(colnames)} cols)")
    con.commit()
    con.close()
    print(f"done: inserted ~{total} rows; target {target}/doctype; db={db}")


if __name__ == "__main__":
    main()
