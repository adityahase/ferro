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
        placeholders = ", ".join("?" for _ in colnames)
        sql = f'INSERT OR IGNORE INTO {q(table)} ({", ".join(q(c) for c in colnames)}) VALUES ({placeholders})'
        rows = []
        for i in range(have, have + need):
            vals = []
            for c, t in cols:
                if c == "name":
                    vals.append(f"{dt}-DEMO-{i:06d}")
                elif c in ("creation", "modified"):
                    vals.append(NOW)
                elif c in ("owner", "modified_by"):
                    vals.append("Administrator")
                elif c in ("docstatus", "idx"):
                    vals.append(0)
                elif c in ("parent", "parentfield", "parenttype"):
                    vals.append(None)
                elif "INT" in t:
                    vals.append(i % 97)
                elif "REAL" in t or t in ("FLOAT", "DOUBLE"):
                    vals.append(round((i % 1000) * 1.5, 2))
                else:  # TEXT/VARCHAR/etc.
                    vals.append(STR)
            rows.append(vals)
        con.executemany(sql, rows)
        total += need
        print(f"  {dt:18s} +{need} -> {have + need}  ({len(colnames)} cols)")
    con.commit()
    con.close()
    print(f"done: inserted ~{total} rows; target {target}/doctype; db={db}")


if __name__ == "__main__":
    main()
