# demos/ — runnable demonstrations

| Demo | What it shows |
|---|---|
| [`pyo3-apps/`](pyo3-apps/00-DEMO.md) | 5 real Frappe apps (crm · helpdesk · gameplan · hrms · erpnext) running on **`ferrod`** (ferro + embedded CPython): reads served in Rust, writes driving the real controller lifecycle, all under 64 MB. |

The interpreter-free counterpart — the same apps with controllers transpiled to Rust into the
`ferro-native` binary — lives in [`../transpile/`](../transpile/README.md).
