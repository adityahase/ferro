//! Python fallthrough — the `web_runtime=ferrod` tier of the full ferro server.
//!
//! When a site sets `web_runtime: "ferrod"` (and this binary is built `--features python`), serve()
//! boots one embedded CPython interpreter, loads the shim + the site's installed apps, and builds a
//! map of every app whitelisted method on worker start. A `/api/method/<m>` call that ferro doesn't
//! serve natively (ferro_method / desk::route_method) is then auto-routed into the app's REAL Python
//! iff `m` is in that map — so the SPA tail (crm.api.*, gameplan.api.*, …) works, while Desk, the
//! app SPAs, reads and pure-CRUD writes stay on the no-GIL Rust fast path.
//!
//! This reuses the same `ferro_rt` callback module + `ferro_boot` shim as the standalone `ferrod`
//! binary; the difference is the host — here it's the full desk+spa+native server, not a minimal one.

use crate::meta::MetaCache;
use crate::pyrt;
use pyo3::prelude::*;
use pyo3::types::PyDict;
use rusqlite::Connection;
use serde_json::{Map, Value};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

pub struct PyFall {
    /// Dotted paths of every installed app's whitelisted methods, mapped on worker start.
    whitelisted: HashSet<String>,
}

/// The shim dir to put on sys.path — `FERRO_SHIM` overrides the vendored deploy default.
fn shim_dir() -> String {
    std::env::var("FERRO_SHIM").unwrap_or_else(|_| "/opt/ferro/stack/framework/shim".to_string())
}

impl PyFall {
    /// Boot the interpreter, load the shim + `apps`, and pull the whitelisted-method map. `metas`
    /// is shared with the request fast path so the Python `ferro_rt` callbacks resolve meta against
    /// the same cache. (`ferro_boot` reads the app source roots from `FERRO_REPOS` itself.)
    pub fn boot(db_path: &str, metas: Arc<MetaCache>, apps: &[String]) -> PyResult<PyFall> {
        pyrt::init_state(db_path.to_string(), metas);
        Python::with_gil(|py| {
            let sys = py.import("sys")?;
            sys.getattr("path")?.call_method1("insert", (0, shim_dir()))?;
            let _ = py.import("frappe")?; // import side effect: registers frappe.* in sys.modules
            let boot = py.import("ferro_boot")?;

            let kwargs = PyDict::new(py);
            kwargs.set_item("apps", apps.to_vec())?;
            kwargs.set_item("mode", "all")?;
            kwargs.set_item("catch_all", true)?;
            let stats = boot.call_method("load", (), Some(&kwargs))?;
            eprintln!("[ferro:pyfall] load: {}", stats.str()?);

            let wl: String = boot.call_method0("whitelisted_methods_json")?.extract()?;
            let whitelisted: HashSet<String> =
                serde_json::from_str::<Vec<String>>(&wl).unwrap_or_default().into_iter().collect();
            eprintln!("[ferro:pyfall] whitelisted app methods: {}", whitelisted.len());
            Ok(PyFall { whitelisted })
        })
    }

    pub fn count(&self) -> usize {
        self.whitelisted.len()
    }
    pub fn has(&self, method: &str) -> bool {
        self.whitelisted.contains(method)
    }

    /// Run a whitelisted method under the GIL, sharing `con` so its ferro_rt reads/writes see the
    /// request's SQLite state. Returns a `{"message": …}` success envelope or a Frappe error one.
    pub fn call(&self, con: &Connection, dev: bool, method: &str, args_json: &str, user: &str) -> (u16, Value) {
        pyrt::set_request_con(con);
        pyrt::set_user(user);
        let out = Python::with_gil(|py| -> Result<Value, (u16, Value)> {
            let boot = py.import("ferro_boot").map_err(|e| map_py_err(dev, py, &e))?;
            let res = boot
                .call_method1("call_method", (method, args_json, user))
                .map_err(|e| map_py_err(dev, py, &e))?;
            let s: String = res.extract().map_err(|e| map_py_err(dev, py, &e))?;
            serde_json::from_str::<Value>(&s)
                .map_err(|e| crate::err(dev, 500, "ServerError", format!("bad method response json: {e}")))
        });
        pyrt::clear_request_con();
        match out {
            Ok(v) => (200, v),
            Err(e) => e,
        }
    }
}

/// Merge query params + JSON body into the form-dict args a whitelisted method receives.
pub fn args_json(params: &HashMap<String, String>, body: &str) -> String {
    let mut args = Map::new();
    for (k, v) in params {
        args.insert(k.clone(), Value::from(v.clone()));
    }
    let t = body.trim();
    if !t.is_empty() {
        if let Ok(Value::Object(m)) = serde_json::from_str::<Value>(t) {
            for (k, v) in m {
                args.insert(k, v);
            }
        }
    }
    serde_json::to_string(&Value::Object(args)).unwrap_or_else(|_| "{}".into())
}

/// Map a Python exception from a controller into an HTTP status + Frappe error envelope.
fn map_py_err(dev: bool, py: Python<'_>, e: &PyErr) -> (u16, Value) {
    let tname = e
        .get_type(py)
        .name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|_| "Error".to_string());
    let msg = e.value(py).str().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let (status, exc) = if tname.contains("DoesNotExist") || tname == "KeyError" {
        (404, "DoesNotExistError")
    } else if tname.contains("Duplicate") {
        (409, "DuplicateEntryError")
    } else if tname.contains("Permission") {
        (403, "PermissionError")
    } else if tname.contains("Validation") || tname.contains("Mandatory") || tname == "ValueError" {
        (417, "ValidationError")
    } else if tname.contains("LookupError") {
        (404, "NotFound")
    } else {
        (500, "ServerError")
    };
    crate::err(dev, status, exc, if msg.is_empty() { tname } else { msg })
}
