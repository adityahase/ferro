# framework/ — Python `frappe` shim, seed DB, and schema tooling

The pieces that let `ferrod` (ferro + embedded CPython) and `ferro-native` run real
Frappe controllers, plus the data tooling the CLI uses to stand a site up without a DB
server.

```
framework/
├── shim/
│   ├── ferro_boot.py          # boot loader: controller registry, lazy/eager import, write dispatch
│   └── frappe/                # a thin, native-backed `frappe` package (the controllers import this,
│                              #   not the real ~100 MB framework) — Document, db, qb, NestedSet,
│                              #   WebsiteGenerator, meta, exceptions, utils
├── build_db.py                # schema materialiser: doctype JSON -> SQLite tables (the slice of
│                              #   `bench migrate` ferro needs)
├── populate_demo_data.py      # representative demo rows
└── seed/
    ├── core.db.gz             # pristine frappe-core SQLite seed (278 doctypes, Administrator,
    │                          #   no apps/data) — `ferro new-site` decompresses this; no DB server
    └── README.md
```

The shim is the single source of truth for the embedded-Python side; the `demos/pyo3-apps`
demo loads this exact shim via `FERRO_SHIM`. See [`../docs/architecture.md`](../docs/architecture.md)
for how the shim, the `ferro_rt` native module, and the runtime fit together.
