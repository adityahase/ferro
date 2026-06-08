#!/usr/bin/env python3
"""
populate_demo_data.py — fill the read-path doctypes with representative rows so the load-test
peak reflects real list-view payloads (every physical column populated), addressing the audit's
"reads hit mostly-empty tables" finding. Idempotent-ish: tops each table up to TARGET rows.
"""
import sqlite3, glob, sys, os

TARGET = int(sys.argv[1]) if len(sys.argv) > 1 else 3000
HERE = os.path.dirname(os.path.abspath(__file__))
DB = glob.glob(os.path.join(HERE, "site", "db", "*.db"))[0]

# the doctypes the load-test reads (ferrod.rs read_doctypes)
DOCTYPES = ["CRM Deal", "CRM Lead", "HD Ticket", "GP Project", "Employee", "Sales Invoice",
            "Item", "Customer", "Contact", "ToDo", "Sales Order", "Purchase Invoice"]

NOW = "2026-06-07 00:00:00.000000"
STR = "Representative demo value for list-view payload sizing"  # ~54 chars


def q(s):
    return '"' + s.replace('"', '""') + '"'


def main():
    con = sqlite3.connect(DB)
    con.execute("PRAGMA foreign_keys=OFF")
    total = 0
    for dt in DOCTYPES:
        table = "tab" + dt
        try:
            info = list(con.execute(f'PRAGMA table_info({q(table)})'))
        except sqlite3.OperationalError:
            print(f"  skip {dt} (no table)")
            continue
        if not info:
            continue
        have = con.execute(f'SELECT COUNT(*) FROM {q(table)}').fetchone()[0]
        need = max(0, TARGET - have)
        if need == 0:
            print(f"  {dt:18s} already has {have}")
            continue
        cols = [(r[1], (r[2] or "").upper()) for r in info]  # (name, type)
        colnames = [c for c, _ in cols]
        # build one parameterized insert
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
                elif t in ("REAL", "FLOAT", "DOUBLE") or "REAL" in t:
                    vals.append(round((i % 1000) * 1.5, 2))
                else:  # TEXT/VARCHAR/etc.
                    vals.append(STR)
            rows.append(vals)
        con.executemany(sql, rows)
        total += need
        print(f"  {dt:18s} +{need} -> {have + need}  ({len(colnames)} cols)")
    con.commit()
    con.close()
    print(f"done: inserted ~{total} rows; target {TARGET}/doctype; db={DB}")


if __name__ == "__main__":
    main()
