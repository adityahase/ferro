# pyo3-apps — Frappe apps on ferro via embedded CPython (ferrod), under 64 MB

Full write-up: **[00-DEMO.md](00-DEMO.md)**. Numbers: [RESULTS.txt](RESULTS.txt). Proof: [VERIFY.txt](VERIFY.txt).

The five investigated Frappe apps (crm, helpdesk, gameplan, hrms, erpnext) served by **ferro**
(Rust data plane) with their **real Python controllers** running on an **embedded CPython 3.13**,
the whole worker measured under 64 MB.

## Quickstart

```bash
python3 build_db.py                 # 5 apps' ~800 doctypes -> SQLite site (once)
python3 populate_demo_data.py 3000  # representative rows for read-path doctypes (once)
cd /home/frappe/ferro && \
  PYO3_PYTHON=/home/frappe/.pyenv/versions/3.13.13/bin/python3 \
  cargo build --release --features python --bin ferrod    # build ferrod (once)

APPS=crm,helpdesk,gameplan,hrms,erpnext   # everything below via the low-memory launcher:
./run-ferrod.sh measure  site --apps $APPS --load lazy
./run-ferrod.sh loadtest site --apps $APPS --load lazy --threads 4
./run-ferrod.sh serve    site --port 8099 --apps $APPS --load lazy --user Administrator
./run-ferrod.sh request  site POST "/api/resource/CRM Deal" '{"organization":"Acme","status":"Qualification"}'

bash bench.sh     # reproduce the memory matrix  -> RESULTS.txt
bash verify.sh    # reproduce the functional proof -> VERIFY.txt
```

## Result (measured, smaps_rollup, realistic load)

- **Lazy (recommended), all 5 apps available:** idle 26 MB, peak **30 / 36 / 46 / 65 MB** at 1 / 2 / 4 / 8 threads — **under 64 at ≤4 threads**.
- **Eager, all 779 controllers resident (stress ceiling):** idle 50 MB, peak 55 / 60 / 70 / 91 MB — under at ≤2 threads, over at ≥4.
- For reference: pure-Rust ferro ~8 MB; `ferro-native` (transpiled, no interpreter — parallel track) ~18 MB; a real CPython+Frappe worker ~115–155 MB.
- The apps genuinely run (`verify.sh` 8/8): reads in Rust (no Python), writes drive the real controller lifecycle in CPython incl. child tables (e.g. hrms `RetentionBonus.validate` → HTTP 417 on a past date).
- Numbers re-measured after an adversarial audit; see [00-DEMO.md](00-DEMO.md) §"Audit & corrections".

> Note: `transpile/` + `src/native/` is a **separate, parallel**
> track (transpiling controllers to Rust → zero interpreter). Not part of this `ferrod` demo.
