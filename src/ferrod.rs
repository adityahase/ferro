//! ferrod — ferro + embedded CPython, running real Frappe app controllers on the Rust data plane.
//!
//! Same data plane as `ferro` (auth/meta/orm/naming/crypto reused from the lib). On top it embeds
//! one CPython 3.13 interpreter (PyO3), imports the native `frappe` shim + every installed app's
//! controllers, and drives the controller lifecycle for the doctypes that carry Python — while
//! reads and pure-CRUD writes stay 100% in Rust (no GIL).
//!
//!   ferrod measure <site> [--apps a,b] [--load all|none] [--hold N]   # load + report memory
//!   ferrod serve   <site> [--port N] [--threads N] [--apps ...] [--load all|none] [--user U]
//!   ferrod request <site> METHOD url [body] [--user U]                # one in-process request

use ferro::auth::{self, AuthOutcome};
use ferro::meta::MetaCache;
use ferro::orm::{self, ListQuery, OrmError, ReadAcl};
use ferro::pyrt;
use ferro::util;
use pyo3::prelude::*;
use pyo3::types::PyDict;
use rusqlite::Connection;
use serde_json::{json, Map, Value};
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;
use std::thread;
use tiny_http::{Header, Method, Response, Server};

const SHIM_DIR: &str = "/home/frappe/ferro-demo/shim";
const DEFAULT_REPOS: &str = "/home/frappe/ferro-apps-investigation/repos";

// ----------------------------------------------------------------- memory reporting
struct Mem {
    rss: f64,
    pss: f64,
    uss: f64,
}
fn read_smaps_rollup() -> Mem {
    let mut rss = 0.0;
    let mut pss = 0.0;
    let mut pc = 0.0;
    let mut pd = 0.0;
    if let Ok(s) = std::fs::read_to_string("/proc/self/smaps_rollup") {
        for l in s.lines() {
            let kb = |l: &str| -> f64 {
                l.split_whitespace().nth(1).and_then(|v| v.parse::<f64>().ok()).unwrap_or(0.0) / 1024.0
            };
            if l.starts_with("Rss:") {
                rss = kb(l);
            } else if l.starts_with("Pss:") {
                pss = kb(l);
            } else if l.starts_with("Private_Clean:") {
                pc = kb(l);
            } else if l.starts_with("Private_Dirty:") {
                pd = kb(l);
            }
        }
    }
    Mem { rss, pss, uss: pc + pd }
}
fn vmrss() -> f64 {
    if let Ok(s) = std::fs::read_to_string("/proc/self/status") {
        for l in s.lines() {
            if let Some(v) = l.strip_prefix("VmRSS:") {
                return v.split_whitespace().next().and_then(|x| x.parse::<f64>().ok()).unwrap_or(0.0) / 1024.0;
            }
        }
    }
    0.0
}

// ----------------------------------------------------------------- needs-python registry
#[derive(Default)]
struct Registry {
    by_doctype: HashMap<String, HashSet<String>>,
    wildcard: HashSet<String>,
}
impl Registry {
    /// True if any of `events` has Python attached for `doctype` (controller method or hook).
    fn needs(&self, doctype: &str, events: &[&str]) -> bool {
        for e in events {
            if self.wildcard.contains(*e) {
                return true;
            }
            if let Some(set) = self.by_doctype.get(doctype) {
                if set.contains(*e) {
                    return true;
                }
            }
        }
        false
    }
}

const INSERT_EVENTS: &[&str] = &[
    "before_validate", "validate", "before_save", "before_insert", "after_insert", "on_update", "on_change",
];
const UPDATE_EVENTS: &[&str] = &["before_validate", "validate", "before_save", "on_update", "on_change"];
const DELETE_EVENTS: &[&str] = &["on_trash", "after_delete"];

// ----------------------------------------------------------------- app state
struct App {
    metas: Arc<MetaCache>,
    registry: Registry,
    default_user: String,
    py_enabled: bool,
    dev: bool,
}

// ----------------------------------------------------------------- python boot
fn resolve_db_path(arg: &str) -> String {
    let p = Path::new(arg);
    if p.is_file() {
        return arg.to_string();
    }
    let dbdir = p.join("db");
    if let Ok(entries) = std::fs::read_dir(&dbdir) {
        for e in entries.flatten() {
            let path = e.path();
            if path.extension().map(|x| x == "db").unwrap_or(false) {
                return path.to_string_lossy().into_owned();
            }
        }
    }
    arg.to_string()
}

/// Initialise the interpreter, set sys.path, import the shim, load controllers, read the registry.
fn py_boot(apps: &Option<Vec<String>>, load_mode: &str, verbose: bool) -> PyResult<Registry> {
    Python::with_gil(|py| {
        // sys.path: the shim first, then ferro_boot adds the app repo roots itself.
        let sys = py.import("sys")?;
        let path = sys.getattr("path")?;
        path.call_method1("insert", (0, SHIM_DIR))?;

        let frappe = py.import("frappe")?;
        let _ = frappe; // import side effect: registers frappe.* in sys.modules
        let boot = py.import("ferro_boot")?;

        let kwargs = PyDict::new(py);
        if let Some(a) = apps {
            kwargs.set_item("apps", a.clone())?;
        }
        kwargs.set_item("mode", load_mode)?;
        kwargs.set_item("catch_all", true)?;
        kwargs.set_item("verbose", verbose)?;
        let stats = boot.call_method("load", (), Some(&kwargs))?;
        eprintln!("[ferrod] load: {}", stats.str()?);

        // needs-python registry (via JSON for robustness across pyo3 conversions)
        let reg_json: String = boot.call_method0("needs_python_registry_json")?.extract()?;
        let parsed: Value = serde_json::from_str(&reg_json).unwrap_or(Value::Null);
        let mut registry = Registry::default();
        if let Some(by) = parsed.get("by_doctype").and_then(|v| v.as_object()) {
            for (dt, events) in by {
                let set: HashSet<String> = events
                    .as_array()
                    .map(|a| a.iter().filter_map(|e| e.as_str().map(|s| s.to_string())).collect())
                    .unwrap_or_default();
                registry.by_doctype.insert(dt.clone(), set);
            }
        }
        if let Some(w) = parsed.get("wildcard").and_then(|v| v.as_array()) {
            registry.wildcard = w.iter().filter_map(|e| e.as_str().map(|s| s.to_string())).collect();
        }
        eprintln!(
            "[ferrod] needs-python: {} doctypes, wildcard={:?}",
            registry.by_doctype.len(),
            registry.wildcard
        );
        Ok(registry)
    })
}

/// Map a Python exception raised by the controller to an HTTP status + Frappe error envelope.
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
    } else {
        (500, "ServerError")
    };
    err(dev, status, exc, if msg.is_empty() { tname } else { msg })
}

// ----------------------------------------------------------------- envelope helpers
fn err(dev: bool, status: u16, exc_type: &str, msg: String) -> (u16, Value) {
    let msg_obj = json!({ "message": msg, "title": "Message" });
    let server_messages =
        serde_json::to_string(&vec![serde_json::to_string(&msg_obj).unwrap_or_default()]).unwrap_or_default();
    let mut o = Map::new();
    o.insert("exc_type".into(), Value::from(exc_type.to_string()));
    o.insert("_server_messages".into(), Value::from(server_messages));
    if dev {
        o.insert("exception".into(), Value::from(format!("{exc_type}: {msg}")));
    }
    (status, Value::Object(o))
}
fn map_orm_err(dev: bool, e: OrmError) -> (u16, Value) {
    match e {
        OrmError::NotFound(m) => err(dev, 404, "DoesNotExistError", m),
        OrmError::Validation(m) => err(dev, 417, "ValidationError", m),
        OrmError::Duplicate(m) => err(dev, 409, "DuplicateEntryError", m),
        OrmError::Db(e) => err(dev, 500, "DatabaseError", if dev { e.to_string() } else { "Internal Server Error".into() }),
    }
}

// ----------------------------------------------------------------- the hooked-write driver
/// Drive a Python controller write on this thread, sharing `con` with the controller's ORM calls.
fn run_py_write(con: &Connection, doctype: &str, op: &str, data: &Value, user: &str) -> Result<Value, PyErrBox> {
    let data_json = serde_json::to_string(data).unwrap_or_else(|_| "{}".into());
    pyrt::set_request_con(con);
    pyrt::set_user(user);
    let result = Python::with_gil(|py| -> Result<Value, PyErrBox> {
        let boot = py.import("ferro_boot").map_err(PyErrBox::from_err)?;
        let out = boot
            .call_method1("dispatch_write", (doctype, op, data_json.as_str(), user))
            .map_err(PyErrBox::from_err)?;
        let s: String = out.extract().map_err(PyErrBox::from_err)?;
        match serde_json::from_str::<Value>(&s) {
            Ok(v) => Ok(v),
            Err(_) => Ok(Value::Object(Map::new())),
        }
    });
    pyrt::clear_request_con();
    result
}

/// A captured Python error (status computed under the GIL we already held).
struct PyErrBox {
    status: u16,
    env: Value,
}
impl PyErrBox {
    fn from_err(e: PyErr) -> Self {
        Python::with_gil(|py| {
            let (status, env) = map_py_err(true, py, &e);
            PyErrBox { status, env }
        })
    }
}

// ----------------------------------------------------------------- routing
#[allow(clippy::too_many_arguments)]
fn route(con: &Connection, app: &App, method: &str, url: &str, body: &str, auth_header: Option<&str>) -> (u16, Value) {
    let (segments, params) = util::parse_url(url);
    let ident = match auth::resolve_user(con, auth_header, &app.default_user, None) {
        AuthOutcome::Ok(id) => id,
        AuthOutcome::Unauthorized => return err(app.dev, 401, "AuthenticationError", "Invalid credentials".into()),
    };
    if segments.is_empty() {
        return (200, json!({"application":"ferrod","user":ident.user,"python": app.py_enabled}));
    }
    if segments.first().map(|s| s.as_str()) != Some("api") {
        return err(app.dev, 404, "NotFound", "unknown path".into());
    }
    match segments.get(1).map(|s| s.as_str()) {
        Some("method") => {
            let m = segments.get(2).map(|s| s.as_str()).unwrap_or("");
            match m {
                "ping" | "frappe.ping" => (200, json!({"message":"pong"})),
                "frappe.auth.get_logged_user" => (200, json!({"message": ident.user})),
                other => err(app.dev, 404, "NotFound", format!("Method '{other}' not implemented")),
            }
        }
        Some("resource") => route_resource(con, app, &ident.user, method, &segments, &params, body),
        _ => err(app.dev, 404, "NotFound", "unknown path".into()),
    }
}

#[allow(clippy::too_many_arguments)]
fn route_resource(
    con: &Connection,
    app: &App,
    user: &str,
    method: &str,
    segments: &[String],
    params: &HashMap<String, String>,
    body: &str,
) -> (u16, Value) {
    let dev = app.dev;
    let doctype = match segments.get(2) {
        Some(d) if !d.is_empty() => d.clone(),
        _ => return err(dev, 404, "NotFound", "no doctype".into()),
    };
    let name: Option<String> = if segments.len() > 3 { Some(segments[3..].join("/")) } else { None };

    let meta = match app.metas.get(con, &doctype) {
        Ok(m) => m,
        Err(ferro::meta::MetaError::NotFound(d)) => return err(dev, 404, "DoesNotExistError", format!("DocType {d} not found")),
        Err(ferro::meta::MetaError::Db(e)) => return map_orm_err(dev, OrmError::Db(e)),
    };
    let acl = ReadAcl::all(); // Administrator demo: full field access

    let parse_body = || -> Result<Map<String, Value>, (u16, Value)> {
        let mut out = Map::new();
        let t = body.trim();
        if !t.is_empty() {
            match serde_json::from_str::<Value>(t) {
                Ok(Value::Object(m)) => out = m,
                Ok(_) => return Err(err(dev, 417, "ValidationError", "body must be a JSON object".into())),
                Err(e) => return Err(err(dev, 417, "ValidationError", format!("invalid JSON: {e}"))),
            }
        }
        out.remove("doctype");
        Ok(out)
    };

    match (method, name) {
        ("GET", None) => {
            let q = build_list_query(params);
            match orm::get_list(con, &meta, &acl, &q, None) {
                Ok(data) => (200, json!({ "data": data })),
                Err(e) => map_orm_err(dev, e),
            }
        }
        ("GET", Some(n)) => match orm::get_doc(con, &meta, &acl, &n) {
            Ok(data) => (200, json!({ "data": data })),
            Err(e) => map_orm_err(dev, e),
        },
        ("POST", None) => {
            let mut data = match parse_body() {
                Ok(d) => d,
                Err(e) => return e,
            };
            data.insert("doctype".into(), Value::from(doctype.clone()));
            if app.py_enabled && app.registry.needs(&doctype, INSERT_EVENTS) {
                match run_py_write(con, &doctype, "insert", &Value::Object(data), user) {
                    Ok(v) => wrap_data(con, v),
                    Err(b) => (b.status, b.env),
                }
            } else {
                data.remove("doctype");
                match orm::insert(con, &meta, &acl, &data, user) {
                    Ok(doc) => (200, json!({ "data": doc })),
                    Err(e) => map_orm_err(dev, e),
                }
            }
        }
        ("PUT", Some(n)) | ("PATCH", Some(n)) => {
            let mut data = match parse_body() {
                Ok(d) => d,
                Err(e) => return e,
            };
            data.insert("doctype".into(), Value::from(doctype.clone()));
            data.insert("name".into(), Value::from(n.clone()));
            if app.py_enabled && app.registry.needs(&doctype, UPDATE_EVENTS) {
                match run_py_write(con, &doctype, "update", &Value::Object(data), user) {
                    Ok(v) => wrap_data(con, v),
                    Err(b) => (b.status, b.env),
                }
            } else {
                data.remove("doctype");
                data.remove("name");
                match orm::update(con, &meta, &acl, &n, &data, user) {
                    Ok(doc) => (200, json!({ "data": doc })),
                    Err(e) => map_orm_err(dev, e),
                }
            }
        }
        ("DELETE", Some(n)) => {
            if app.py_enabled && app.registry.needs(&doctype, DELETE_EVENTS) {
                let mut data = Map::new();
                data.insert("doctype".into(), Value::from(doctype.clone()));
                data.insert("name".into(), Value::from(n.clone()));
                match run_py_write(con, &doctype, "delete", &Value::Object(data), user) {
                    Ok(_) => (202, json!({ "data": "ok" })),
                    Err(b) => (b.status, b.env),
                }
            } else {
                match orm::delete(con, &meta, &n) {
                    Ok(()) => (202, json!({ "data": "ok" })),
                    Err(e) => map_orm_err(dev, e),
                }
            }
        }
        (m, _) => err(dev, 405, "MethodNotAllowed", format!("{m} not allowed")),
    }
}

/// Attach any controller messages (frappe.msgprint) to the success envelope.
fn wrap_data(_con: &Connection, doc: Value) -> (u16, Value) {
    let msgs = pyrt::take_messages();
    let mut o = Map::new();
    o.insert("data".into(), doc);
    if !msgs.is_empty() {
        let arr: Vec<String> = msgs
            .into_iter()
            .map(|m| serde_json::to_string(&json!({"message": m, "title": "Message"})).unwrap_or_default())
            .collect();
        o.insert("_server_messages".into(), Value::from(serde_json::to_string(&arr).unwrap_or_default()));
    }
    (200, Value::Object(o))
}

fn build_list_query(params: &HashMap<String, String>) -> ListQuery {
    let mut q = ListQuery::default();
    if let Some(fields) = params.get("fields") {
        if let Ok(Value::Array(arr)) = serde_json::from_str::<Value>(fields) {
            let parsed: Vec<String> = arr.into_iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect();
            if !parsed.is_empty() {
                q.fields = parsed;
            }
        }
    }
    if let Some(f) = params.get("filters") {
        if let Ok(v) = serde_json::from_str::<Value>(f) {
            q.filters = v;
        }
    }
    if let Some(o) = params.get("order_by") {
        if !o.is_empty() {
            q.order_by = Some(o.clone());
        }
    }
    if let Some(v) = params.get("limit_page_length").or_else(|| params.get("limit")).and_then(|s| s.parse().ok()) {
        q.limit_page_length = v;
    }
    if let Some(v) = params.get("limit_start").and_then(|s| s.parse().ok()) {
        q.limit_start = v;
    }
    q
}

// ----------------------------------------------------------------- HTTP server
fn header_value(req: &tiny_http::Request, name: &str) -> Option<String> {
    req.headers()
        .iter()
        .find(|h| h.field.as_str().as_str().eq_ignore_ascii_case(name))
        .map(|h| h.value.as_str().to_string())
}

fn handle(mut req: tiny_http::Request, con: &Connection, app: &App) {
    let method = match req.method() {
        Method::Get => "GET",
        Method::Post => "POST",
        Method::Put => "PUT",
        Method::Patch => "PATCH",
        Method::Delete => "DELETE",
        _ => "OTHER",
    }
    .to_string();
    let url = req.url().to_string();
    let auth_header = header_value(&req, "Authorization");
    let mut body = String::new();
    if matches!(method.as_str(), "POST" | "PUT" | "PATCH") {
        use std::io::Read;
        let _ = req.as_reader().take(8 * 1024 * 1024).read_to_string(&mut body);
    }
    let (status, value) = route(con, app, &method, &url, &body, auth_header.as_deref());
    let payload = serde_json::to_vec(&value).unwrap_or_else(|_| b"{}".to_vec());
    let header = Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..]).unwrap();
    let _ = req.respond(Response::from_data(payload).with_status_code(status).with_header(header));
}

// ----------------------------------------------------------------- subcommands
struct Args {
    site: String,
    port: u16,
    threads: usize,
    apps: Option<Vec<String>>,
    load: String,
    user: String,
    hold: u64,
    dev: bool,
}
fn parse_args(a: &[String]) -> Args {
    let mut args = Args {
        site: a.first().cloned().unwrap_or_default(),
        port: 8080,
        threads: 4,
        apps: None,
        load: "all".into(),
        user: "Administrator".into(),
        hold: 0,
        dev: false,
    };
    let mut i = 1;
    while i < a.len() {
        match a[i].as_str() {
            "--port" => { args.port = a.get(i + 1).and_then(|s| s.parse().ok()).unwrap_or(args.port); i += 2; }
            "--threads" => { args.threads = a.get(i + 1).and_then(|s| s.parse().ok()).unwrap_or(args.threads); i += 2; }
            "--apps" => { args.apps = a.get(i + 1).map(|s| s.split(',').map(|x| x.trim().to_string()).collect()); i += 2; }
            "--load" => { args.load = a.get(i + 1).cloned().unwrap_or(args.load); i += 2; }
            "--user" | "--default-user" => { args.user = a.get(i + 1).cloned().unwrap_or(args.user); i += 2; }
            "--hold" => { args.hold = a.get(i + 1).and_then(|s| s.parse().ok()).unwrap_or(0); i += 2; }
            "--dev" => { args.dev = true; i += 1; }
            _ => i += 1,
        }
    }
    if let Some(a) = &args.apps {
        std::env::set_var("FERRO_REPOS", std::env::var("FERRO_REPOS").unwrap_or_else(|_| DEFAULT_REPOS.into()));
        let _ = a;
    }
    args
}

fn boot_state(args: &Args) -> (Arc<App>, String) {
    let db_path = resolve_db_path(&args.site);
    let metas = Arc::new(MetaCache::new(2048));
    pyrt::init_state(db_path.clone(), metas.clone());
    let registry = py_boot(&args.apps, &args.load, false).expect("python boot");
    let app = Arc::new(App {
        metas,
        registry,
        default_user: args.user.clone(),
        py_enabled: true,
        dev: args.dev,
    });
    (app, db_path)
}

fn cmd_measure(a: &[String]) {
    let args = parse_args(a);
    eprintln!("[ferrod] pre-boot VmRSS = {:.1} MB", vmrss());
    let (app, db_path) = boot_state(&args);
    eprintln!(
        "[ferrod] loaded apps={:?} load={} doctypes_with_python={}",
        args.apps.clone().unwrap_or_else(|| vec!["<all>".into()]),
        args.load,
        app.registry.by_doctype.len()
    );
    let m = read_smaps_rollup();
    println!("=== ferrod memory (all controllers resident, embedded CPython 3.13) ===");
    println!("db: {db_path}");
    println!("RSS = {:.1} MB", m.rss);
    println!("PSS = {:.1} MB", m.pss);
    println!("USS = {:.1} MB", m.uss);
    println!("under 64 MB: {}", if m.rss < 64.0 { "YES" } else { "NO" });
    if args.hold > 0 {
        eprintln!("[ferrod] holding {}s for external measurement (pid {})", args.hold, std::process::id());
        std::thread::sleep(std::time::Duration::from_secs(args.hold));
    }
}

fn cmd_request(a: &[String]) {
    // ferrod request <site> METHOD url [body] [--user U]
    let args = parse_args(a);
    let method = a.get(1).map(|s| s.to_uppercase()).unwrap_or_else(|| "GET".into());
    let url = a.get(2).cloned().unwrap_or_else(|| "/".into());
    let body = a.get(3).filter(|s| !s.starts_with("--")).cloned().unwrap_or_default();
    let (app, db_path) = boot_state(&args);
    let con = open_conn(&db_path);
    let (status, value) = route(&con, &app, &method, &url, &body, None);
    println!("HTTP {status}");
    println!("{}", serde_json::to_string_pretty(&value).unwrap_or_default());
}

fn open_conn(db_path: &str) -> Connection {
    let con = Connection::open(db_path).expect("open sqlite");
    con.busy_timeout(std::time::Duration::from_secs(5)).ok();
    con.pragma_update(None, "foreign_keys", "OFF").ok();
    // 256 KiB page cache per worker connection (down from 2 MiB): the per-thread cache is the
    // dominant per-thread cost, and the working set per request is tiny. Keeps the concurrent
    // peak flat as --threads scales. Override with FERRO_CACHE_KB.
    let kb: i64 = std::env::var("FERRO_CACHE_KB").ok().and_then(|s| s.parse().ok()).unwrap_or(256);
    con.pragma_update(None, "cache_size", -kb).ok();
    con.pragma_update(None, "mmap_size", 0i64).ok();
    con
}

fn cmd_serve(a: &[String]) {
    let args = parse_args(a);
    let (app, db_path) = boot_state(&args);
    let addr = format!("0.0.0.0:{}", args.port);
    let server = Arc::new(Server::http(&addr).expect("bind"));
    eprintln!(
        "ferrod serving {} on http://{} ({} threads, user={}, python={}, doctypes_with_python={})",
        db_path, addr, args.threads, app.default_user, app.py_enabled, app.registry.by_doctype.len()
    );
    let m = read_smaps_rollup();
    eprintln!("[ferrod] post-boot RSS={:.1} PSS={:.1} USS={:.1} MB", m.rss, m.pss, m.uss);

    let mut handles = Vec::new();
    for _ in 0..args.threads {
        let server = server.clone();
        let app = app.clone();
        let dbp = db_path.clone();
        let builder = thread::Builder::new().stack_size(2 * 1024 * 1024);
        handles.push(
            builder
                .spawn(move || {
                    let con = open_conn(&dbp);
                    loop {
                        match server.recv() {
                            Ok(req) => handle(req, &con, &app),
                            Err(_) => break,
                        }
                    }
                })
                .expect("spawn worker"),
        );
    }
    for h in handles {
        let _ = h.join();
    }
}

/// In-process load test: drive the real route() across N threads (reads = Rust, writes = Python
/// lifecycle), sampling /proc/self/smaps_rollup, and report peak RSS/PSS/USS. Self-contained
/// (no detached server), so it works under the sandbox and isolates server-side memory.
fn cmd_loadtest(a: &[String]) {
    let args = parse_args(a);
    let (app, db_path) = boot_state(&args);
    let rounds = {
        let mut r = 6usize;
        let mut i = 0;
        while i < a.len() {
            if a[i] == "--rounds" {
                r = a.get(i + 1).and_then(|s| s.parse().ok()).unwrap_or(r);
            }
            i += 1;
        }
        r
    };
    let m0 = read_smaps_rollup();
    eprintln!("[ferrod] post-boot (idle): RSS={:.1} PSS={:.1} USS={:.1} MB", m0.rss, m0.pss, m0.uss);

    let read_doctypes = [
        "CRM Deal", "CRM Lead", "HD Ticket", "GP Project", "Employee", "Sales Invoice",
        "Item", "Customer", "Contact", "ToDo", "Sales Order", "Purchase Invoice",
    ];
    let peak = Arc::new(std::sync::Mutex::new((m0.rss, m0.pss, m0.uss)));
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));

    // sampler thread
    let sp = peak.clone();
    let ss = stop.clone();
    let sampler = thread::spawn(move || {
        while !ss.load(std::sync::atomic::Ordering::Relaxed) {
            let m = read_smaps_rollup();
            let mut p = sp.lock().unwrap();
            p.0 = p.0.max(m.rss);
            p.1 = p.1.max(m.pss);
            p.2 = p.2.max(m.uss);
            drop(p);
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    });

    let nthreads = args.threads.max(1);
    let ok = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let er = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let t0 = std::time::Instant::now();
    let mut handles = Vec::new();
    for tid in 0..nthreads {
        let app = app.clone();
        let dbp = db_path.clone();
        let ok = ok.clone();
        let er = er.clone();
        let rd = read_doctypes;
        handles.push(
            thread::Builder::new()
                .stack_size(2 * 1024 * 1024)
                .spawn(move || {
                    let con = open_conn(&dbp);
                    let mut bump = |status: u16| {
                        if (200..300).contains(&status) || status == 417 || status == 409 {
                            ok.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        } else {
                            er.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        }
                    };
                    for r in 0..rounds {
                        for dt in rd.iter() {
                            // realistic list-view read: 20 rows, ALL physical columns (fields=["*"])
                            let url = format!(
                                "/api/resource/{}?fields=%5B%22*%22%5D&limit_page_length=20",
                                dt
                            );
                            let (s, _) = route(&con, &app, "GET", &url, "", None);
                            bump(s);
                        }
                        // Python-driven write (CRM Deal has a controller validate)
                        let body = format!(
                            "{{\"organization\":\"Load-{tid}-{r}\",\"status\":\"Qualification\",\"deal_owner\":\"Administrator\"}}"
                        );
                        let (s, _) = route(&con, &app, "POST", "/api/resource/CRM Deal", &body, None);
                        bump(s);
                        // pure-CRUD write (ToDo: frappe-core doctype, no app controller -> Rust)
                        let body2 = format!("{{\"description\":\"todo-{tid}-{r}\",\"status\":\"Open\"}}");
                        let (s, _) = route(&con, &app, "POST", "/api/resource/ToDo", &body2, None);
                        bump(s);
                    }
                })
                .expect("spawn"),
        );
    }
    for h in handles {
        let _ = h.join();
    }
    let dt = t0.elapsed().as_secs_f64();
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    let _ = sampler.join();

    let p = peak.lock().unwrap();
    let nok = ok.load(std::sync::atomic::Ordering::Relaxed);
    let ner = er.load(std::sync::atomic::Ordering::Relaxed);
    let mf = read_smaps_rollup();
    println!("=== ferrod under load (all 5 apps resident, {} threads) ===", nthreads);
    println!("requests: ok={} err={} in {:.1}s ({:.0} req/s)", nok, ner, dt, (nok + ner) as f64 / dt);
    println!("PEAK:  RSS={:.1} MB   PSS={:.1} MB   USS={:.1} MB", p.0, p.1, p.2);
    println!("FINAL: RSS={:.1} MB   PSS={:.1} MB   USS={:.1} MB", mf.rss, mf.pss, mf.uss);
    println!("under 64 MB: {}", if p.0 < 64.0 { "YES" } else { "NO" });
}

fn main() {
    // Drop per-instruction position tables from code objects (smaller resident controller code).
    if std::env::var_os("PYTHONNODEBUGRANGES").is_none() {
        std::env::set_var("PYTHONNODEBUGRANGES", "1");
    }
    // register the native module BEFORE the interpreter initialises
    pyo3::append_to_inittab!(ferro_rt);
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(|s| s.as_str()) {
        Some("measure") => cmd_measure(&args[2..]),
        Some("serve") => cmd_serve(&args[2..]),
        Some("loadtest") => cmd_loadtest(&args[2..]),
        Some("request") => cmd_request(&args[2..]),
        _ => {
            eprintln!("usage: ferrod <measure|serve|loadtest|request> <site> [opts]");
            std::process::exit(2);
        }
    }
}

// bring the native module's init symbol into scope for append_to_inittab!
use ferro::pyrt::ferro_rt;
