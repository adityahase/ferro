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
use std::path::Path;
use std::sync::Arc;

pub struct PyFall {
    /// Dotted paths of every installed app's whitelisted methods. Built cheaply at startup by a
    /// Rust source scan — NO interpreter — so `has()` can decide routing without paying CPython.
    whitelisted: HashSet<String>,
    /// The embedded interpreter + shim + apps are loaded LAZILY, on the first call that actually
    /// needs Python. A Desk-only / browse-only tenant therefore never boots CPython and idles at
    /// the pure-Rust ~15-25 MB instead of ~46-63 MB. `OnceLock` makes the first-call init race-safe.
    interp: std::sync::OnceLock<Result<(), String>>,
    db_path: String,
    metas: Arc<MetaCache>,
    apps: Vec<String>,
}

/// The shim dir to put on sys.path — `FERRO_SHIM` overrides the vendored deploy default.
fn shim_dir() -> String {
    std::env::var("FERRO_SHIM").unwrap_or_else(|_| "/opt/ferro/stack/framework/shim".to_string())
}

/// The app source roots dir (`FERRO_REPOS`) the shim imports from — also where we scan for
/// whitelisted methods without an interpreter.
fn repos_dir() -> String {
    std::env::var("FERRO_REPOS").unwrap_or_else(|_| "/opt/ferro/app-mirror".to_string())
}

impl PyFall {
    /// Build the routing map (Rust scan, cheap) and stash everything needed to boot the interpreter
    /// on first use. Does NOT start CPython. `metas` is shared with the request fast path so the
    /// Python `ferro_rt` callbacks resolve meta against the same cache.
    pub fn new(db_path: &str, metas: Arc<MetaCache>, apps: &[String]) -> PyFall {
        let whitelisted = scan_whitelisted(Path::new(&repos_dir()), apps);
        eprintln!("[ferro:pyfall] {} app method(s) routable to Python (interpreter starts on first use)", whitelisted.len());
        PyFall {
            whitelisted,
            interp: std::sync::OnceLock::new(),
            db_path: db_path.to_string(),
            metas,
            apps: apps.to_vec(),
        }
    }

    /// Eager boot — load the interpreter immediately (used by `ferro install-hooks`, which exists
    /// purely to run app Python). Returns Err if the interpreter/shim/apps fail to load.
    pub fn boot(db_path: &str, metas: Arc<MetaCache>, apps: &[String]) -> PyResult<PyFall> {
        let pf = PyFall::new(db_path, metas, apps);
        match pf.ensure_interp() {
            Ok(()) => Ok(pf),
            Err(e) => Err(pyo3::exceptions::PyRuntimeError::new_err(e)),
        }
    }

    /// Load the interpreter + shim + apps exactly once (idempotent, race-safe). Cheap no-op after
    /// the first successful call. This is where the ~30 MB CPython cost is actually paid.
    fn ensure_interp(&self) -> Result<(), String> {
        let apps = self.apps.clone();
        let db_path = self.db_path.clone();
        let metas = self.metas.clone();
        self.interp
            .get_or_init(move || {
                pyrt::init_state(db_path, metas);
                Python::with_gil(|py| -> PyResult<()> {
                    let sys = py.import("sys")?;
                    sys.getattr("path")?.call_method1("insert", (0, shim_dir()))?;
                    let _ = py.import("frappe")?;
                    let boot = py.import("ferro_boot")?;
                    let kwargs = PyDict::new(py);
                    kwargs.set_item("apps", apps.clone())?;
                    kwargs.set_item("mode", "all")?;
                    kwargs.set_item("catch_all", true)?;
                    let stats = boot.call_method("load", (), Some(&kwargs))?;
                    eprintln!("[ferro:pyfall] interpreter booted on demand: {}", stats.str()?);
                    Ok(())
                })
                .map_err(|e| e.to_string())
            })
            .clone()
    }

    pub fn count(&self) -> usize {
        self.whitelisted.len()
    }

    /// Run each app's `after_install` hook against `con` (so the app's seed/master data — default
    /// statuses, fields layouts, … — is created). Returns the JSON summary the shim produces.
    pub fn run_after_install(&self, con: &Connection, apps: &[String]) -> PyResult<String> {
        self.ensure_interp().map_err(pyo3::exceptions::PyRuntimeError::new_err)?;
        pyrt::set_request_con(con);
        pyrt::set_user("Administrator");
        let apps_json = serde_json::to_string(apps).unwrap_or_else(|_| "[]".into());
        let out = Python::with_gil(|py| -> PyResult<String> {
            let boot = py.import("ferro_boot")?;
            let res = boot.call_method1("run_after_install", (apps_json,))?;
            res.extract::<String>()
        });
        pyrt::clear_request_con();
        out
    }
    pub fn has(&self, method: &str) -> bool {
        self.whitelisted.contains(method)
    }

    /// Resolve the v2 doctype-scoped method form (`/api/v2/method/<doctype>/<method>`) to its
    /// controller-module dotted path (e.g. `GP Project` + `get_joined_spaces` ->
    /// `gameplan.…doctype.gp_project.gp_project.get_joined_spaces`). Returns None if unknown.
    pub fn resolve_doctype_method(&self, doctype: &str, method: &str) -> Option<String> {
        self.ensure_interp().ok()?;
        Python::with_gil(|py| {
            let boot = py.import("ferro_boot").ok()?;
            let res = boot.call_method1("resolve_doctype_method", (doctype, method)).ok()?;
            res.extract::<Option<String>>().ok().flatten()
        })
    }

    /// Run a whitelisted method under the GIL, sharing `con` so its ferro_rt reads/writes see the
    /// request's SQLite state. Returns a `{"message": …}` success envelope or a Frappe error one.
    pub fn call(&self, con: &Connection, dev: bool, method: &str, args_json: &str, user: &str) -> (u16, Value) {
        if let Err(e) = self.ensure_interp() {
            return crate::err(dev, 500, "ServerError", format!("interpreter init failed: {e}"));
        }
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

// --------------------------------------------------------------------- whitelist scan (no interp)
//
// The shim's authoritative map is a Python AST scan; replicating a close-enough text scan in Rust
// lets ferro know which /api/method/<app>.* paths to route to Python WITHOUT booting CPython — the
// prerequisite for the lazy interpreter. Module-level `@frappe.whitelist`-decorated defs only
// (mirrors the shim's tree.body scan). False negatives just 404 (as today); false positives route
// to Python and 404 there — neither leaks.

/// Dotted paths of every installed app's whitelisted methods, scanned from source under `repos`.
fn scan_whitelisted(repos: &Path, apps: &[String]) -> HashSet<String> {
    let mut out = HashSet::new();
    for app in apps {
        let app_root = repos.join(app); // e.g. app-mirror/crm
        let pkg = app_root.join(app); // the inner package, e.g. app-mirror/crm/crm
        if pkg.is_dir() {
            scan_dir(&pkg, &app_root, &mut out);
        }
    }
    out
}

fn scan_dir(dir: &Path, app_root: &Path, out: &mut HashSet<String>) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for ent in entries.flatten() {
        let p = ent.path();
        if p.is_dir() {
            let name = p.file_name().and_then(|s| s.to_str()).unwrap_or("");
            if name.starts_with('.') || name == "node_modules" || name == "__pycache__" || name == "tests" {
                continue;
            }
            scan_dir(&p, app_root, out);
        } else if p.extension().and_then(|e| e.to_str()) == Some("py") {
            let fname = p.file_name().and_then(|s| s.to_str()).unwrap_or("");
            if fname.starts_with("test_") {
                continue;
            }
            if let Some(module) = module_path(&p, app_root) {
                scan_file(&p, &module, out);
            }
        }
    }
}

/// File path relative to the app root -> dotted module (strip .py, '/'->'.', drop trailing __init__).
fn module_path(file: &Path, app_root: &Path) -> Option<String> {
    let rel = file.strip_prefix(app_root).ok()?;
    let mut parts: Vec<String> = rel
        .components()
        .filter_map(|c| c.as_os_str().to_str().map(|s| s.to_string()))
        .collect();
    let last = parts.last_mut()?;
    *last = last.strip_suffix(".py").unwrap_or(last).to_string();
    if parts.last().map(|s| s == "__init__").unwrap_or(false) {
        parts.pop();
    }
    if parts.is_empty() {
        return None;
    }
    Some(parts.join("."))
}

fn scan_file(file: &Path, module: &str, out: &mut HashSet<String>) {
    let src = match std::fs::read_to_string(file) {
        Ok(s) => s,
        Err(_) => return,
    };
    let mut armed = false; // saw a column-0 @frappe.whitelist, awaiting its def
    for line in src.lines() {
        let at_col0 = !line.starts_with(char::is_whitespace);
        let trimmed = line.trim_start();
        if at_col0 && trimmed.starts_with('@') {
            let dec = trimmed[1..].trim_start();
            if dec.starts_with("frappe.whitelist") || dec.starts_with("whitelist") {
                armed = true;
            }
            continue; // stacked decorators keep `armed`
        }
        if armed {
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue; // comments/blank between a decorator and its def
            }
            if at_col0 && (trimmed.starts_with("def ") || trimmed.starts_with("async def ")) {
                if let Some(name) = fn_name(trimmed) {
                    out.insert(format!("{module}.{name}"));
                }
            }
            armed = false; // consumed, or the decorator chain broke
        }
    }
}

fn fn_name(def_line: &str) -> Option<String> {
    let after = def_line.strip_prefix("async def ").or_else(|| def_line.strip_prefix("def "))?;
    let name: String = after.chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
    if name.is_empty() {
        None
    } else {
        Some(name)
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
