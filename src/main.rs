//! ferro — a Rust runtime that serves the Frappe REST API against an existing Frappe
//! SQLite site, in place of the CPython+Frappe worker.
//!
//! Usage:
//!   ferro serve [<site-dir-or-db>] [--bench-mode [--site NAME] [--sites-path PATH]]
//!               [--port N | -b host:port] [--threads N] [--desk|--no-desk]
//!               [--default-user U] [--meta-cap N] [--dev]
//!   ferro request <site-dir-or-db> <METHOD> <url-path-with-query> [json-body] [--user U] [--token k:s]
//!   ferro provision-key <site-dir-or-db> <user>
//!   ferro <db>                      # legacy smoke test (counts meta tables)
//!
//! Drop-in: inside a Frappe bench, `ferro serve --bench-mode -b 127.0.0.1:8000` replaces the
//! `gunicorn frappe.app:application` web process and reads sites/ exactly like `bench serve`.

mod auth;
mod bench;
mod cache;
mod crypto;
mod desk;
mod install;
mod jobs;
mod meta;
mod naming;
mod newsite;
mod orm;
mod realtime;
mod schema;
mod spa;
mod util;
// The embedded-Python fallthrough (web_runtime=ferrod tier). Compiled only with --features python;
// `pyrt` is the same ferro_rt callback module the `ferrod` binary uses, compiled into this binary.
#[cfg(feature = "python")]
#[path = "pyrt.rs"]
mod pyrt;
#[cfg(feature = "python")]
mod pyfall;

use auth::AuthOutcome;
use meta::MetaCache;
use orm::{ListQuery, OrmError, ReadAcl};
use rusqlite::Connection;
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread;
use tiny_http::{Header, Method, Response, Server};
// brings the native module's init symbol into scope for append_to_inittab!
#[cfg(feature = "python")]
use pyrt::ferro_rt;

/// Maximum request body we will read (DoS guard). 413 above this.
const MAX_BODY: u64 = 8 * 1024 * 1024;

struct App {
    metas: Arc<MetaCache>,
    default_user: String,
    encryption_key: Option<String>,
    dev: bool,
    /// `maintenance_mode` from site/common config: when true, all writes return 503 InReadOnlyMode.
    maintenance_mode: bool,
    /// When set, ferro also serves the Frappe Desk SPA (HTML shell + assets + desk.* methods).
    desk: Option<Arc<desk::Desk>>,
    /// When set, ferro also serves installed apps' frappe-ui SPAs (crm/gameplan/helpdesk/…) at the
    /// routes their hooks.py declares — mirroring Frappe's website router.
    spa: Option<Arc<spa::Spa>>,
    /// When set (web_runtime=ferrod + built --features python), installed apps' whitelisted
    /// `/api/method/<app>.*` calls route into embedded CPython running their real code.
    #[cfg(feature = "python")]
    pyfall: Option<Arc<pyfall::PyFall>>,
    /// The Frappe sitename — the Socket.IO namespace browsers connect to, and the site we emit to.
    sitename: String,
    /// In-process cache (replaces redis_cache).
    cache: Arc<cache::Cache>,
    /// In-process realtime hub (replaces the Node socketio + redis_socketio). None if disabled.
    realtime: Option<Arc<realtime::Realtime>>,
    /// In-process background-job queue (replaces the rq worker + redis_queue). None if disabled.
    jobs: Option<Arc<jobs::JobQueue>>,
}

/// Publish the same realtime events Frappe's `Document.notify_update` does, so the Desk's open
/// forms and list views refresh live — but in-process, with no redis hop.
fn notify_write(app: &App, doctype: &str, name: &str, user: &str, modified: &str) {
    let rt = match &app.realtime {
        Some(rt) => rt,
        None => return,
    };
    // doc_update -> the document's room (forms subscribe via doc_subscribe)
    rt.emit(
        &app.sitename,
        &format!("doc:{}/{}", doctype, name),
        "doc_update",
        &json!({"modified": modified, "doctype": doctype, "name": name}),
    );
    // list_update -> the site room "all" (every System User joins it on connect)
    rt.emit(
        &app.sitename,
        "all",
        "list_update",
        &json!({"doctype": doctype, "name": name, "user": user}),
    );
}

fn main() {
    // Register the native `ferro_rt` module before the interpreter can initialise (no-op for the
    // non-python build / the request/bench paths that never start CPython).
    #[cfg(feature = "python")]
    pyo3::append_to_inittab!(ferro_rt);
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(|s| s.as_str()) {
        Some("serve") => serve(&args[2..]),
        Some("provision-key") => provision(&args[2..]),
        Some("request") => request_cli(&args[2..]),
        Some("bench") => bench::dispatch(&args[2..]),
        Some(db) if args.len() == 2 => smoke(db),
        _ => {
            eprintln!(
                "usage:\n  ferro serve [<site-dir-or-db>] [--bench-mode [--site NAME] [--sites-path PATH]] [--port N | -b host:port] [--threads N] [--desk|--no-desk] [--default-user U | --insecure-desk-admin] [--meta-cap N] [--dev]\n  ferro request <site-dir-or-db> <METHOD> <url-path-with-query> [json-body] [--user U] [--token k:s] [--desk]\n  ferro provision-key <site-dir-or-db> <user>"
            );
            std::process::exit(2);
        }
    }
}

/// Resolve a CLI path argument to a concrete .db file.
fn resolve_db_path(arg: &str) -> String {
    let p = Path::new(arg);
    if p.is_file() {
        return arg.to_string();
    }
    let cfg_path = p.join("site_config.json");
    if let Ok(text) = std::fs::read_to_string(&cfg_path) {
        if let Ok(cfg) = serde_json::from_str::<Value>(&text) {
            if let Some(db_name) = cfg.get("db_name").and_then(|v| v.as_str()) {
                let db = p.join("db").join(format!("{db_name}.db"));
                if db.is_file() {
                    return db.to_string_lossy().into_owned();
                }
            }
        }
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

/// Read the site `encryption_key` (needed to decrypt Fernet-encrypted api_secrets) from
/// site_config.json located next to the site (arg may be the site dir or the .db file).
fn load_encryption_key(arg: &str) -> Option<String> {
    let p = Path::new(arg);
    let site_dir = if p.is_dir() {
        p.to_path_buf()
    } else {
        // <site>/db/<name>.db -> site dir is the grandparent
        p.parent().and_then(|d| d.parent()).map(|x| x.to_path_buf())?
    };
    // site_config.json wins; fall back to the bench-wide common_site_config.json (Frappe merges
    // both, with site_config taking precedence).
    if let Ok(text) = std::fs::read_to_string(site_dir.join("site_config.json")) {
        if let Ok(v) = serde_json::from_str::<Value>(&text) {
            if let Some(k) = v.get("encryption_key").and_then(|x| x.as_str()) {
                return Some(k.to_string());
            }
        }
    }
    let common = site_dir.parent()?.join("common_site_config.json");
    let text = std::fs::read_to_string(common).ok()?;
    let v: Value = serde_json::from_str(&text).ok()?;
    v.get("encryption_key").and_then(|x| x.as_str()).map(|s| s.to_string())
}

/// Read `maintenance_mode` from site_config.json (then common_site_config.json). Frappe treats a
/// truthy value (1/true) as read-only: all writes return 503. Defaults to false.
fn load_maintenance_mode(arg: &str) -> bool {
    let p = Path::new(arg);
    let site_dir = if p.is_dir() {
        p.to_path_buf()
    } else {
        match p.parent().and_then(|d| d.parent()) {
            Some(x) => x.to_path_buf(),
            None => return false,
        }
    };
    let truthy = |v: &Value| -> bool {
        v.as_i64().map(|n| n != 0).or_else(|| v.as_bool()).unwrap_or(false)
    };
    for cfg in [site_dir.join("site_config.json"), site_dir.parent().map(|d| d.join("common_site_config.json")).unwrap_or_default()] {
        if let Ok(text) = std::fs::read_to_string(&cfg) {
            if let Ok(v) = serde_json::from_str::<Value>(&text) {
                if let Some(m) = v.get("maintenance_mode") {
                    if truthy(m) {
                        return true;
                    }
                }
            }
        }
    }
    false
}

/// The site's `installed_apps` list (arg may be the site dir or the .db file). Used to gate which
/// apps' SPA routes ferro serves — the forge symlinks *all* apps in via the shared mirror, so the
/// site config is the source of truth for what's actually installed.
fn load_installed_apps(arg: &str) -> Vec<String> {
    let p = Path::new(arg);
    let site_dir = if p.is_dir() {
        p.to_path_buf()
    } else {
        match p.parent().and_then(|d| d.parent()) {
            Some(x) => x.to_path_buf(),
            None => return Vec::new(),
        }
    };
    std::fs::read_to_string(site_dir.join("site_config.json"))
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .and_then(|v| {
            v.get("installed_apps").and_then(|a| a.as_array()).map(|arr| {
                arr.iter().filter_map(|x| x.as_str().map(String::from)).collect()
            })
        })
        .unwrap_or_default()
}

/// The site's `web_runtime` ("ferro" | "ferrod" | …), site_config winning over common_site_config.
/// "ferrod" turns on the embedded-Python method tier (when this binary has the python feature).
fn load_web_runtime(arg: &str) -> String {
    let p = Path::new(arg);
    let site_dir = if p.is_dir() {
        p.to_path_buf()
    } else {
        match p.parent().and_then(|d| d.parent()) {
            Some(x) => x.to_path_buf(),
            None => return "ferro".to_string(),
        }
    };
    let read = |path: PathBuf| -> Option<String> {
        let v: Value = serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()?;
        v.get("web_runtime").and_then(|x| x.as_str()).map(String::from)
    };
    read(site_dir.join("site_config.json"))
        .or_else(|| site_dir.parent().and_then(|sites| read(sites.join("common_site_config.json"))))
        .unwrap_or_else(|| "ferro".to_string())
}

/// Derive `<bench>/sites/assets` from the site-dir-or-db argument. The site lives at
/// `<sites>/<site>` (a dir) or its db at `<sites>/<site>/db/<name>.db`; assets are `<sites>/assets`.
fn default_assets_dir(arg: &str) -> PathBuf {
    let p = Path::new(arg);
    let site_dir = if p.is_dir() {
        p.to_path_buf()
    } else {
        // <site>/db/<name>.db -> site dir is the grandparent
        p.parent().and_then(|d| d.parent()).map(|x| x.to_path_buf()).unwrap_or_else(|| p.to_path_buf())
    };
    site_dir
        .parent()
        .map(|sites| sites.join("assets"))
        .unwrap_or_else(|| PathBuf::from("sites/assets"))
}

/// Resolve a site from a bench `sites/` layout the way `bench serve` does, returning the site
/// directory and the configured `webserver_port` (if any). Resolution order for the site name:
/// explicit `--site`, then `default_site` in common_site_config.json, then `currentsite.txt`,
/// then — if exactly one site exists — that single site.
fn resolve_bench_site(sites_path: Option<&str>, site: Option<&str>) -> Option<(String, Option<u16>)> {
    // The bench web process runs from the bench root, so sites live at ./sites by default.
    let sp = if let Some(s) = sites_path {
        PathBuf::from(s)
    } else if Path::new("sites").is_dir() {
        PathBuf::from("sites")
    } else {
        PathBuf::from(".")
    };

    let csc: Value = std::fs::read_to_string(sp.join("common_site_config.json"))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or(Value::Null);
    let webserver_port = csc.get("webserver_port").and_then(|v| v.as_u64()).map(|p| p as u16);

    let site_name: Option<String> = site
        .map(|s| s.to_string())
        .or_else(|| csc.get("default_site").and_then(|v| v.as_str()).map(str::to_string))
        .or_else(|| {
            std::fs::read_to_string(sp.join("currentsite.txt"))
                .ok()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
        })
        .or_else(|| {
            // Single-site fallback: a dir holding site_config.json is a site.
            let mut found = None;
            let mut count = 0usize;
            if let Ok(entries) = std::fs::read_dir(&sp) {
                for e in entries.flatten() {
                    let p = e.path();
                    if p.is_dir() && p.join("site_config.json").is_file() {
                        count += 1;
                        found = p.file_name().map(|n| n.to_string_lossy().into_owned());
                    }
                }
            }
            if count == 1 {
                found
            } else {
                None
            }
        });

    let site_dir = sp.join(site_name?);
    if !site_dir.join("site_config.json").is_file() {
        return None;
    }
    Some((site_dir.to_string_lossy().into_owned(), webserver_port))
}

/// The Frappe sitename (= the Socket.IO namespace browsers connect to) from a site-dir-or-db arg.
/// The forge `apps/` dir for a site arg (`<forge>/sites/<host>` or its `.db`), i.e. `<forge>/apps`.
/// Returns None when that directory doesn't exist (e.g. a bare site with no apps symlink).
fn site_apps_dir(arg: &str) -> Option<PathBuf> {
    let p = Path::new(arg);
    let site_dir = if p.is_dir() {
        p.to_path_buf()
    } else {
        // <site>/db/<name>.db -> site dir is the grandparent
        p.parent().and_then(|d| d.parent())?.to_path_buf()
    };
    let apps = site_dir.parent().and_then(|sites| sites.parent()).map(|forge| forge.join("apps"))?;
    apps.is_dir().then_some(apps)
}

fn site_name_from(path: &str) -> String {
    let p = Path::new(path);
    let dir = if p.is_dir() {
        p.to_path_buf()
    } else if p.extension().map(|e| e == "db").unwrap_or(false) {
        // <site>/db/<name>.db -> the site dir is the grandparent
        p.parent().and_then(|d| d.parent()).map(|x| x.to_path_buf()).unwrap_or_else(|| p.to_path_buf())
    } else {
        p.to_path_buf()
    };
    dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

/// Read a numeric key from the bench-wide common_site_config.json next to the site.
fn read_common_cfg_u64(site_path: &str, key: &str) -> Option<u64> {
    let p = Path::new(site_path);
    let sites = if p.is_dir() {
        p.parent()
    } else {
        p.parent().and_then(|d| d.parent())
    }?;
    let text = std::fs::read_to_string(sites.join("common_site_config.json")).ok()?;
    let v: Value = serde_json::from_str(&text).ok()?;
    v.get(key).and_then(|x| x.as_u64())
}

fn open_conn(db_path: &str) -> Connection {
    let con = Connection::open(db_path).expect("open sqlite");
    con.busy_timeout(std::time::Duration::from_secs(5)).ok();
    con.pragma_update(None, "foreign_keys", "OFF").ok();
    // Turn SQLite's double-quoted-string misfeature OFF: a double-quoted token that isn't a real
    // column becomes an ERROR instead of silently degrading to a string literal. Belt-and-suspenders
    // for FIX-8 — every identifier ferro emits is a genuine column, so this only catches bugs.
    con.pragma_update(None, "legacy_double_quoted_strings", "OFF").ok();
    // Bound per-connection page cache so total memory stays flat as --threads scales
    // (each serve thread owns one Connection). Negative = KiB; -2048 ≈ 2 MiB/connection.
    con.pragma_update(None, "cache_size", -2048).ok();
    con
}

fn smoke(db: &str) {
    let con = open_conn(&resolve_db_path(db));
    let n: i64 = con
        .query_row("SELECT COUNT(*) FROM \"tabDocType\"", [], |r| r.get(0))
        .unwrap();
    let f: i64 = con
        .query_row("SELECT COUNT(*) FROM \"tabDocField\"", [], |r| r.get(0))
        .unwrap();
    println!("tabDocType={n} tabDocField={f}");
}

fn provision(args: &[String]) {
    let path = args.first().expect("need <site-dir-or-db>");
    let user = args.get(1).map(|s| s.as_str()).unwrap_or("Administrator");
    let con = open_conn(&resolve_db_path(path));
    let (key, secret) = auth::provision_key(&con, user).expect("provision");
    println!("api_key={key}");
    println!("api_secret={secret}");
    println!("Authorization: token {key}:{secret}");
}

/// In-process request: exercises the full route()/auth/meta/orm stack without a socket.
fn request_cli(args: &[String]) {
    let path = args.first().expect("need <site-dir-or-db>");
    let method = args.get(1).map(|s| s.to_uppercase()).unwrap_or_else(|| "GET".into());
    let url = args.get(2).cloned().unwrap_or_else(|| "/".into());
    let mut body = String::new();
    let mut default_user = "Administrator".to_string();
    let mut token: Option<String> = None;
    let mut dev = false;
    let mut enable_desk = false;
    let mut i = 3;
    while i < args.len() {
        match args[i].as_str() {
            "--user" | "--default-user" => {
                default_user = args.get(i + 1).cloned().unwrap_or(default_user);
                i += 2;
            }
            "--token" => {
                token = args.get(i + 1).cloned();
                i += 2;
            }
            "--dev" => {
                dev = true;
                i += 1;
            }
            // Expose the `frappe.client.*` / `frappe.desk.*` method surface in-process, so the
            // verification harness can exercise the desk-method permission path without a socket.
            "--desk" => {
                enable_desk = true;
                i += 1;
            }
            other => {
                if body.is_empty() {
                    body = other.to_string();
                }
                i += 1;
            }
        }
    }
    let con = open_conn(&resolve_db_path(path));
    let desk = if enable_desk {
        Some(Arc::new(desk::Desk::new(
            default_assets_dir(path),
            None,
            site_apps_dir(path),
            load_installed_apps(path),
        )))
    } else {
        None
    };
    let app = App {
        metas: Arc::new(MetaCache::new(512)),
        default_user,
        encryption_key: load_encryption_key(path),
        dev,
        maintenance_mode: load_maintenance_mode(path),
        desk,
        spa: None,
        #[cfg(feature = "python")]
        pyfall: None,
        // The one-shot `request` CLI never serves realtime/jobs — just exercises the ORM.
        sitename: site_name_from(path),
        cache: Arc::new(cache::Cache::new()),
        realtime: None,
        jobs: None,
    };
    let auth_header = token.map(|t| format!("token {t}"));
    // Infer content-type from the body shape so the CLI can exercise both JSON and form bodies.
    let ct = if body.trim_start().starts_with(['{', '[']) {
        Some("application/json")
    } else {
        Some("application/x-www-form-urlencoded")
    };
    let (status, value) = route(&con, &app, &method, &url, &body, ct, auth_header.as_deref(), None);
    println!("HTTP {status}");
    println!("{}", serde_json::to_string_pretty(&value).unwrap_or_default());
}

fn serve(args: &[String]) {
    // The positional <site-dir-or-db> is OPTIONAL: in --bench-mode the site is resolved from the
    // bench's sites/ layout (common_site_config.json + currentsite.txt / default_site) exactly the
    // way `bench serve` does, so this binary is a byte-for-byte launch swap for gunicorn.
    let mut positional: Option<String> = None;
    let mut port: Option<u16> = None;
    let mut bind_host = "0.0.0.0".to_string();
    // 4 worker threads is a good default for SQLite (writes serialize anyway) and keeps
    // resident memory low; raise --threads for more read concurrency (≈2 MiB/thread).
    let mut threads = 4usize;
    let mut default_user = "Guest".to_string();
    let mut default_user_set = false;
    let mut meta_cap = 512usize;
    let mut dev = false;
    let mut enable_desk = false;
    let mut disable_desk = false;
    let mut bench_mode = false;
    let mut site_opt: Option<String> = None;
    let mut sites_path_opt: Option<String> = None;
    let mut assets_dir: Option<PathBuf> = None;
    let mut desk_boot: Option<PathBuf> = None;
    // Internal backend subsystems (the Procfile collapse). All default ON in bench-mode.
    let mut no_realtime = false;
    let mut no_workers = false;
    let mut no_scheduler = false;
    let mut socketio_port: Option<u16> = None;
    let mut worker_count: Option<usize> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--port" | "-p" => {
                port = args.get(i + 1).and_then(|s| s.parse().ok()).or(port);
                i += 2;
            }
            // gunicorn-compatible bind: `-b host:port` (or `host`, or `:port`). Lets the prod
            // nginx upstream stay byte-for-byte identical when swapping gunicorn -> ferro.
            "-b" | "--bind" => {
                if let Some(v) = args.get(i + 1) {
                    if let Some((h, p)) = v.rsplit_once(':') {
                        if !h.is_empty() {
                            bind_host = h.to_string();
                        }
                        if let Ok(pp) = p.parse() {
                            port = Some(pp);
                        }
                    } else if let Ok(pp) = v.parse::<u16>() {
                        port = Some(pp);
                    } else {
                        bind_host = v.to_string();
                    }
                }
                i += 2;
            }
            "--threads" | "-w" => {
                threads = args.get(i + 1).and_then(|s| s.parse().ok()).unwrap_or(threads);
                i += 2;
            }
            "--default-user" => {
                default_user = args.get(i + 1).cloned().unwrap_or(default_user);
                default_user_set = true;
                i += 2;
            }
            "--meta-cap" => {
                meta_cap = args.get(i + 1).and_then(|s| s.parse().ok()).unwrap_or(meta_cap);
                i += 2;
            }
            "--dev" => {
                dev = true;
                i += 1;
            }
            "--desk" => {
                enable_desk = true;
                i += 1;
            }
            // Explicit, auditable opt-in to "every unauthenticated request is Administrator"
            // (a single-tenant sandbox posture). Equivalent to `--default-user Administrator`,
            // but its name makes the security trade-off impossible to set by accident.
            "--insecure-desk-admin" => {
                default_user = "Administrator".to_string();
                default_user_set = true;
                i += 1;
            }
            "--no-desk" => {
                disable_desk = true;
                i += 1;
            }
            // The drop-in switch: resolve the site from the surrounding bench sites/ layout.
            "--bench-mode" => {
                bench_mode = true;
                i += 1;
            }
            "--site" => {
                site_opt = args.get(i + 1).cloned();
                i += 2;
            }
            "--sites-path" => {
                sites_path_opt = args.get(i + 1).cloned();
                i += 2;
            }
            "--assets" => {
                assets_dir = args.get(i + 1).map(PathBuf::from);
                i += 2;
            }
            "--desk-boot" => {
                desk_boot = args.get(i + 1).map(PathBuf::from);
                i += 2;
            }
            // ---- internal backend subsystems (collapse redis/socketio/worker/schedule) ----
            "--no-realtime" => {
                no_realtime = true;
                i += 1;
            }
            "--no-workers" => {
                no_workers = true;
                i += 1;
            }
            "--no-scheduler" => {
                no_scheduler = true;
                i += 1;
            }
            "--no-backend" => {
                // turn off ALL internal subsystems (pure web runtime, like the original ferro)
                no_realtime = true;
                no_workers = true;
                no_scheduler = true;
                i += 1;
            }
            "--socketio-port" => {
                socketio_port = args.get(i + 1).and_then(|s| s.parse().ok());
                i += 2;
            }
            "--workers" => {
                worker_count = args.get(i + 1).and_then(|s| s.parse().ok());
                i += 2;
            }
            other => {
                // First bare (non-flag) token is the optional positional site path.
                if !other.starts_with('-') && positional.is_none() {
                    positional = Some(other.to_string());
                }
                i += 1;
            }
        }
    }

    // Resolve the effective site path: an explicit positional wins; otherwise --bench-mode reads
    // the bench layout. `cfg_port` is the bench's webserver_port, used only if no port was given.
    let mut cfg_port: Option<u16> = None;
    let path: String = if let Some(p) = positional {
        p
    } else if bench_mode {
        match resolve_bench_site(sites_path_opt.as_deref(), site_opt.as_deref()) {
            Some((site_dir, wp)) => {
                cfg_port = wp;
                eprintln!("ferro bench-mode: resolved site -> {site_dir}");
                site_dir
            }
            None => {
                eprintln!(
                    "ferro serve --bench-mode: could not resolve a site under sites/ \
                     (pass --site NAME or --sites-path PATH)"
                );
                std::process::exit(2);
            }
        }
    } else {
        eprintln!("ferro serve: need <site-dir-or-db> (or --bench-mode to read the bench layout)");
        std::process::exit(2);
    };

    let db_path = resolve_db_path(&path);
    let encryption_key = load_encryption_key(&path);
    let port = port.or(cfg_port).unwrap_or(8080);

    // In bench-mode, serving the Desk SPA is the whole point of the swap: enable it unless the
    // operator explicitly opted out with --no-desk.
    if bench_mode && !disable_desk {
        enable_desk = true;
    }

    // GUARDRAIL (FIX-7): never silently authenticate every unauthenticated request as
    // Administrator just because the presentation flag --desk is set — that turns an
    // internet-facing deploy into "auth off". Admin-default is now an explicit opt-in
    // (`--default-user Administrator` or `--insecure-desk-admin`); otherwise desk runs with the
    // safe Guest default and we warn that the per-user Desk needs token/session auth.
    if enable_desk && !default_user_set {
        eprintln!(
            "ferro: WARNING --desk without --default-user: serving Desk with default_user=Guest. \
             Unauthenticated requests are treated as Guest (read-gated). For a single-tenant admin \
             sandbox pass --insecure-desk-admin (or --default-user Administrator) to make that \
             posture explicit."
        );
    }

    let sitename = site_name_from(&path);
    // The forge `apps/` dir, derived from the *site* path (`<forge>/sites/<host>` -> `<forge>/apps`).
    // NB: do NOT derive this from --assets: in the signup deployment --assets points at a SHARED
    // built tree (`/opt/ferro/assets`), not the forge's own `sites/assets`, so the apps live
    // relative to the site, not the assets.
    let apps_dir = site_apps_dir(&path);

    let (desk, spa) = if enable_desk {
        let adir = assets_dir.unwrap_or_else(|| default_assets_dir(&path));
        eprintln!("ferro desk: serving assets from {}", adir.display());
        // SPA frontends: serve each installed app's frappe-ui SPA at the routes its hooks.py
        // declares. Installed-app gated so we don't serve a route for an app that's only
        // symlinked-in via the shared mirror.
        let spa = apps_dir
            .as_ref()
            .and_then(|ad| spa::Spa::discover(ad, &load_installed_apps(&path), &sitename, &default_user))
            .map(Arc::new);
        if let Some(s) = &spa {
            eprintln!("ferro spa: serving {} app SPA route(s)", s.route_count());
        }
        let installed = load_installed_apps(&path);
        (Some(Arc::new(desk::Desk::new(adir, desk_boot, apps_dir.clone(), installed))), spa)
    } else {
        (None, None)
    };

    // ---- internal backend subsystems (collapse redis_cache/redis_queue/socketio/worker/schedule
    // into this one process). Default ON in bench-mode; individual --no-* flags opt out. ----
    let cache = Arc::new(cache::Cache::new());

    let backend = bench_mode; // the drop-in scenario is where "one process" matters
    let want_realtime = backend && !no_realtime;
    let want_workers = backend && !no_workers;
    let want_scheduler = backend && !no_scheduler;

    // Realtime hub: resolve the connecting user from ferro's default desk identity. (ferro's Desk
    // runs every request as `default_user`; the realtime side mirrors that.)
    let realtime = if want_realtime {
        let du = default_user.clone();
        let resolver: realtime::AuthResolver = Arc::new(move |_site, _cookie, _auth| {
            let user_type = if du == "Guest" { "Guest" } else { "System User" };
            (du.clone(), user_type.to_string())
        });
        Some(realtime::Realtime::new(resolver))
    } else {
        None
    };

    let jobs = if want_workers {
        let q = jobs::JobQueue::new();
        jobs::register_builtins(&q);
        Some(q)
    } else {
        None
    };

    let metas = Arc::new(MetaCache::new(meta_cap));

    // Python fallthrough tier. When the site's web_runtime is "ferrod" (and this is the --features
    // python build), boot one embedded interpreter, load the installed apps, and map their
    // whitelisted methods so /api/method/<app>.* runs real controller code. Everything else (Desk,
    // SPAs, reads, pure-CRUD writes) stays on the no-GIL Rust path. The map is shared via `metas`.
    let web_runtime = load_web_runtime(&path);
    #[cfg(feature = "python")]
    let pyfall = if web_runtime == "ferrod" {
        let apps: Vec<String> = load_installed_apps(&path).into_iter().filter(|a| a != "frappe").collect();
        match pyfall::PyFall::boot(&db_path, metas.clone(), &apps) {
            Ok(pf) => {
                eprintln!("ferro: web_runtime=ferrod — {} app method(s) routed to Python", pf.count());
                Some(Arc::new(pf))
            }
            Err(e) => {
                eprintln!("ferro: web_runtime=ferrod but interpreter boot failed: {e}");
                None
            }
        }
    } else {
        None
    };
    #[cfg(not(feature = "python"))]
    if web_runtime == "ferrod" {
        eprintln!(
            "ferro: web_runtime=ferrod but this binary was built without --features python — \
             app /api/method/* will 404; use the python-enabled build for the ferrod tier"
        );
    }

    let app = Arc::new(App {
        metas,
        default_user,
        encryption_key,
        dev,
        maintenance_mode: load_maintenance_mode(&path),
        desk,
        spa,
        #[cfg(feature = "python")]
        pyfall,
        sitename: sitename.clone(),
        cache: cache.clone(),
        realtime: realtime.clone(),
        jobs: jobs.clone(),
    });

    // Launch realtime listener (socket.io on socketio_port, default 9000).
    if let Some(rt) = &realtime {
        let sio_port = socketio_port
            .or_else(|| read_common_cfg_u64(&path, "socketio_port").map(|p| p as u16))
            .unwrap_or(9000);
        let sio_addr = format!("{bind_host}:{sio_port}");
        match realtime::serve(rt.clone(), &sio_addr) {
            Ok(_) => {}
            Err(e) => eprintln!("ferro realtime: could not bind {sio_addr}: {e} (realtime disabled)"),
        }
    }

    // Launch worker pool + scheduler, sharing the cache and realtime hub so jobs can push events.
    if let Some(q) = &jobs {
        let rt_for_jobs = realtime.clone().unwrap_or_else(|| {
            // jobs need a realtime handle for progress; if realtime is off, use a detached hub.
            let resolver: realtime::AuthResolver =
                Arc::new(|_s, _c, _a| ("Administrator".to_string(), "System User".to_string()));
            realtime::Realtime::new(resolver)
        });
        let ctx = Arc::new(jobs::JobContext { cache: cache.clone(), realtime: rt_for_jobs, queue: q.clone() });
        let nworkers = worker_count
            .or_else(|| read_common_cfg_u64(&path, "background_workers").map(|n| n as usize))
            .unwrap_or(1)
            .max(1);
        jobs::start_workers(q.clone(), ctx, nworkers);
        eprintln!("ferro workers: {nworkers} background worker(s) running");

        if want_scheduler {
            let sched = jobs::Scheduler::new(q.clone(), &sitename);
            // The maintenance jobs Frappe's scheduler enqueues; ferro fires them on the same cadence.
            sched.every(jobs::Every::all(), "frappe.utils.scheduler.enqueue_events", "default");
            sched.every(jobs::Every::hourly(), "frappe.sessions.clear_expired_sessions", "default");
            jobs::start_scheduler(sched);
        }
    }

    let addr = format!("{bind_host}:{port}");
    let server = Arc::new(Server::http(&addr).expect("bind"));
    eprintln!(
        "ferro serving {} on http://{} ({} threads, site={}, default-user={}, fernet={}, desk={}, realtime={}, workers={})",
        db_path,
        addr,
        threads,
        app.sitename,
        app.default_user,
        app.encryption_key.is_some(),
        app.desk.is_some(),
        app.realtime.is_some(),
        app.jobs.is_some(),
    );

    let mut handles = Vec::new();
    for _ in 0..threads {
        let server = server.clone();
        let app = app.clone();
        let dbp = db_path.clone();
        // 1 MiB stack is ample for request handling (no deep recursion; serde_json self-limits
        // nesting) and keeps per-thread memory low vs Rust's 2 MiB default.
        let builder = thread::Builder::new().stack_size(1024 * 1024);
        let handle = builder
            .spawn(move || {
                let con = open_conn(&dbp);
                loop {
                    match server.recv() {
                        Ok(req) => handle(req, &con, &app),
                        Err(_) => break,
                    }
                }
            })
            .expect("spawn worker");
        handles.push(handle);
    }
    for h in handles {
        let _ = h.join();
    }
}

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
        Method::Head => "HEAD",
        _ => "OTHER",
    }
    .to_string();
    let url = req.url().to_string();
    let auth_header = header_value(&req, "Authorization");
    let content_type = header_value(&req, "Content-Type");
    let sid = header_value(&req, "Cookie").and_then(|c| cookie_value(&c, "sid"));

    // Body-size DoS guard: reject oversize bodies up front, and cap the actual read.
    if let Some(cl) = header_value(&req, "Content-Length").and_then(|s| s.parse::<u64>().ok()) {
        if cl > MAX_BODY {
            return respond_json(req, err(app.dev, 413, "RequestEntityTooLarge", "Request body too large".into()));
        }
    }

    let mut body = String::new();
    if matches!(method.as_str(), "POST" | "PUT" | "PATCH") {
        let mut limited = req.as_reader().take(MAX_BODY + 1);
        let mut raw = Vec::new();
        let _ = limited.read_to_end(&mut raw);
        if raw.len() as u64 > MAX_BODY {
            return respond_json(req, err(app.dev, 413, "RequestEntityTooLarge", "Request body too large".into()));
        }
        body = String::from_utf8_lossy(&raw).into_owned();
    }

    // Desk mode: serve static assets / the HTML shell / socket.io 404 / the /app->/desk redirect
    // before the JSON API router. These are "raw" responses (HTML, binary, custom headers).
    if let Some(desk) = &app.desk {
        let user = match auth::resolve_user_session(con, auth_header.as_deref(), sid.as_deref(), &app.default_user, app.encryption_key.as_deref()) {
            AuthOutcome::Ok(id) => id.user,
            AuthOutcome::Unauthorized => app.default_user.clone(),
        };
        if let Some(raw) = desk::try_raw(desk, con, &user, &method, &url) {
            return respond_raw(req, raw);
        }
    }

    // App SPA routes (crm/gameplan/helpdesk/…): serve the frappe-ui shell at the app's own path,
    // exactly where its hooks.py mounts it. Assets (`/assets/...`) were handled by desk above.
    if let Some(spa) = &app.spa {
        if let Some(raw) = spa.try_route(&url) {
            return respond_raw(req, raw);
        }
    }

    let resp = route(con, app, &method, &url, &body, content_type.as_deref(), auth_header.as_deref(), sid.as_deref());

    let path = url.split('?').next().unwrap_or("");
    // Login (FIX-2): on a verified login, mint a real tabSessions row and set the sid cookie to it,
    // so subsequent cookie requests resolve to the credentialed user (not the server default).
    if app.desk.is_some() && path == "/api/method/login" && resp.0 == 200 {
        let user = resp.1.get("user").and_then(|v| v.as_str()).unwrap_or(&app.default_user).to_string();
        let real_sid = auth::create_session(con, &user).unwrap_or_else(util::random_name);
        let mut raw = desk::RawResp::json(resp.0, &resp.1);
        attach_login_cookies(&mut raw, &user, &real_sid);
        return respond_raw(req, raw);
    }
    // Logout: invalidate the session and clear the cookie.
    if app.desk.is_some() && (path == "/api/method/logout" || path == "/api/method/frappe.auth.logout") {
        if let Some(s) = &sid {
            auth::delete_session(con, s);
        }
        let mut raw = desk::RawResp::json(resp.0, &resp.1);
        raw.headers.push(("Set-Cookie".into(), "sid=Guest; Path=/; HttpOnly; SameSite=Lax".into()));
        return respond_raw(req, raw);
    }

    respond_json(req, resp);
}

/// Parse a single cookie value out of a `Cookie:` header (`a=1; sid=xyz; b=2`).
fn cookie_value(cookie_header: &str, key: &str) -> Option<String> {
    cookie_header.split(';').find_map(|kv| {
        let (k, v) = kv.split_once('=')?;
        if k.trim() == key {
            Some(v.trim().to_string())
        } else {
            None
        }
    })
}

/// Append the Set-Cookie headers a Frappe login returns, carrying the real sid + credentialed user.
fn attach_login_cookies(raw: &mut desk::RawResp, user: &str, sid: &str) {
    for c in [
        format!("sid={sid}; Path=/; HttpOnly; SameSite=Lax"),
        "system_user=yes; Path=/; SameSite=Lax".to_string(),
        format!("full_name={user}; Path=/; SameSite=Lax"),
        format!("user_id={user}; Path=/; SameSite=Lax"),
        "user_lang=en; Path=/; SameSite=Lax".to_string(),
    ] {
        raw.headers.push(("Set-Cookie".into(), c));
    }
}

fn respond_json(req: tiny_http::Request, resp: (u16, Value)) {
    respond_raw(req, desk::RawResp::json(resp.0, &resp.1));
}

fn respond_raw(req: tiny_http::Request, raw: desk::RawResp) {
    let mut response = Response::from_data(raw.body).with_status_code(raw.status);
    if let Ok(h) = Header::from_bytes(b"Content-Type".as_slice(), raw.content_type.as_bytes()) {
        response = response.with_header(h);
    }
    for (k, v) in &raw.headers {
        if let Ok(h) = Header::from_bytes(k.as_bytes(), v.as_bytes()) {
            response = response.with_header(h);
        }
    }
    let _ = req.respond(response);
}

/// Build a Frappe-v1 error envelope. `exception` is included only in dev mode (matches Frappe,
/// which exposes the traceback line only under developer_mode); `_server_messages` is the
/// JSON-array-of-JSON-encoded-message-objects shape the frappe-js-sdk expects.
fn err(dev: bool, status: u16, exc_type: &str, msg: String) -> (u16, Value) {
    let msg_obj = json!({ "message": msg, "title": "Message" });
    let server_messages = serde_json::to_string(&vec![
        serde_json::to_string(&msg_obj).unwrap_or_default()
    ])
    .unwrap_or_default();
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
        OrmError::Db(e) => {
            // Don't leak raw SQL/driver text in production.
            let m = if dev { e.to_string() } else { "Internal Server Error".to_string() };
            err(dev, 500, "DatabaseError", m)
        }
    }
}

/// Build a Frappe-**v2** error envelope. v2 diverges from v1: errors go in an `errors` array of
/// `{type, message, exception?}` objects (not `exc_type`/`_server_messages`), which is what
/// frappe-ui's v2 request layer reads. Mirrors `frappe/utils/response.py` ApiVersion.V2.
fn err_v2(dev: bool, status: u16, exc_type: &str, msg: String) -> (u16, Value) {
    let mut e = Map::new();
    e.insert("type".into(), Value::from(exc_type.to_string()));
    e.insert("message".into(), Value::from(msg.clone()));
    if dev {
        e.insert("exception".into(), Value::from(format!("{exc_type}: {msg}")));
    }
    (status, json!({ "errors": [Value::Object(e)] }))
}

/// Re-map an ORM error into the v2 error envelope (same status mapping as `map_orm_err`).
fn map_orm_err_v2(dev: bool, e: OrmError) -> (u16, Value) {
    match e {
        OrmError::NotFound(m) => err_v2(dev, 404, "DoesNotExistError", m),
        OrmError::Validation(m) => err_v2(dev, 417, "ValidationError", m),
        OrmError::Duplicate(m) => err_v2(dev, 409, "DuplicateEntryError", m),
        OrmError::Db(e) => {
            let m = if dev { e.to_string() } else { "Internal Server Error".to_string() };
            err_v2(dev, 500, "DatabaseError", m)
        }
    }
}

/// Convert a v1-shaped handler response into a v2 one. v1 method handlers return either
/// `{"message": X}` (the desk/ferro convention) or `{"exc_type":..,"_server_messages":..}` on
/// error. v2 wants `{"data": X}` on success and `{"errors":[..]}` on error. CRUD handlers return
/// `{"data": X}` already, so that key passes through unchanged.
fn v1_to_v2(dev: bool, status: u16, body: Value) -> (u16, Value) {
    if status >= 400 {
        // Re-shape a v1 error envelope into v2's errors[] array.
        let exc = body.get("exc_type").and_then(|v| v.as_str()).unwrap_or("ServerError").to_string();
        // Pull the human message out of v1's _server_messages (a JSON array of JSON strings).
        let msg = body
            .get("_server_messages")
            .and_then(|v| v.as_str())
            .and_then(|s| serde_json::from_str::<Vec<String>>(s).ok())
            .and_then(|v| v.into_iter().next())
            .and_then(|s| serde_json::from_str::<Value>(&s).ok())
            .and_then(|o| o.get("message").and_then(|m| m.as_str()).map(|s| s.to_string()))
            .or_else(|| body.get("exception").and_then(|v| v.as_str()).map(|s| s.to_string()))
            .unwrap_or_else(|| exc.clone());
        return err_v2(dev, status, &exc, msg);
    }
    // Success: unwrap {"message": X} / {"data": X}; otherwise wrap the whole body as data.
    let data = body
        .get("message")
        .or_else(|| body.get("data"))
        .cloned()
        .unwrap_or(body);
    (status, json!({ "data": data }))
}

#[allow(clippy::too_many_arguments)]
fn route(
    con: &Connection,
    app: &App,
    method: &str,
    url: &str,
    body: &str,
    content_type: Option<&str>,
    auth_header: Option<&str>,
    sid: Option<&str>,
) -> (u16, Value) {
    let (segments, params) = util::parse_url(url);
    let ident = match auth::resolve_user_session(con, auth_header, sid, &app.default_user, app.encryption_key.as_deref()) {
        AuthOutcome::Ok(id) => id,
        AuthOutcome::Unauthorized => {
            // B-REST-1: a /api/v2/* request must get the v2 error envelope, not the v1 shape.
            let mk = if segments.get(1).map(|s| s.as_str()) == Some("v2") { err_v2 } else { err };
            return mk(app.dev, 401, "AuthenticationError", "Invalid authentication credentials".into());
        }
    };

    if segments.is_empty() {
        return (
            200,
            json!({
                "application": "ferro",
                "description": "Rust runtime serving the Frappe REST API",
                "user": ident.user,
            }),
        );
    }

    if segments[0] != "api" {
        return err(app.dev, 404, "NotFound", format!("Unknown path /{}", segments.join("/")));
    }

    match segments.get(1).map(|s| s.as_str()) {
        Some("method") => {
            let mname = segments.get(2).map(|s| s.as_str()).unwrap_or("");
            // B-REST-4: read-only mode also blocks the document-mutating method surface (the desk
            // save / client.set_value / client.delete / bulk paths), not just /api/resource.
            if app.maintenance_mode && method_is_write(mname) {
                return err(app.dev, 503, "InReadOnlyMode", "Site is in read-only mode".into());
            }
            // ferro's own introspection / job-control methods for the internal backend subsystems.
            if let Some(r) = ferro_method(app, &ident, mname, &params) {
                return r;
            }
            // Desk's frappe.* whitelisted methods (list/form/boot), mapped onto ferro's ORM.
            if app.desk.is_some() {
                if let Some(r) = desk::route_method(con, &app.metas, &ident.user, mname, &params, body, content_type, method) {
                    return r;
                }
            }
            // web_runtime=ferrod tier: installed apps' whitelisted methods run their real Python.
            #[cfg(feature = "python")]
            if let Some(pf) = &app.pyfall {
                if pf.has(mname) {
                    let args = pyfall::args_json(&params, body);
                    return pf.call(con, app.dev, mname, &args, &ident.user);
                }
            }
            route_method(app, &ident, &segments)
        }
        Some("resource") => route_resource(con, app, &ident, method, &segments, &params, body, content_type),
        // Frappe v2 REST API (`/api/v2/document/*`, `/api/v2/method/*`). frappe-ui's request layer
        // (used by Gameplan/CRM/Helpdesk frontends) talks v2; it differs from v1 only in the URL
        // shape and the response/error envelope, so these delegate to the same handlers.
        Some("v2") => match segments.get(2).map(|s| s.as_str()) {
            Some("document") => route_v2_document(con, app, &ident, method, &segments, &params, body, content_type),
            Some("method") => {
                // Two forms: `/api/v2/method/<dotted>` (segment 3 is the whole method name) and the
                // doctype-scoped `/api/v2/method/<doctype>/<method>` (segments 3 + 4), which runs the
                // method on the doctype's controller module. Resolve the latter to a dotted path.
                let seg3 = segments.get(3).map(|s| s.as_str()).unwrap_or("");
                let resolved: Option<String> = match segments.get(4).map(|s| s.as_str()) {
                    Some(meth) if !meth.is_empty() => {
                        #[cfg(feature = "python")]
                        {
                            app.pyfall.as_ref().and_then(|pf| pf.resolve_doctype_method(seg3, meth))
                        }
                        #[cfg(not(feature = "python"))]
                        {
                            let _ = meth;
                            None
                        }
                    }
                    _ => None,
                };
                let mname = resolved.as_deref().unwrap_or(seg3);
                route_v2_method(con, app, &ident, method, mname, &params, body, content_type)
            }
            // B-REST-2: /api/v2/doctype/<dt>/meta and /api/v2/doctype/<dt>/count
            Some("doctype") => route_v2_doctype(con, app, &ident, &segments, &params),
            _ => err_v2(app.dev, 404, "NotFound", format!("Unknown path /{}", segments.join("/"))),
        },
        _ => err(app.dev, 404, "NotFound", format!("Unknown path /{}", segments.join("/"))),
    }
}

/// `/api/v2/document/<doctype>[/<name>]` — the v2 CRUD surface. Same permission model and ORM
/// calls as `route_resource`; only the path offset (doctype at segment 3, not 2) and the response
/// envelope differ. Lists carry `has_next_page`; creates return 200 with the new doc as `data`.
#[allow(clippy::too_many_arguments)]
fn route_v2_document(
    con: &Connection,
    app: &App,
    ident: &auth::Identity,
    method: &str,
    segments: &[String],
    params: &HashMap<String, String>,
    body: &str,
    content_type: Option<&str>,
) -> (u16, Value) {
    let dev = app.dev;
    let doctype = match segments.get(3) {
        Some(d) if !d.is_empty() => d.clone(),
        _ => return err_v2(dev, 404, "NotFound", "No doctype in path".into()),
    };
    let name: Option<String> = if segments.len() > 4 {
        Some(segments[4..].join("/"))
    } else {
        None
    };

    let meta = match app.metas.get(con, &doctype) {
        Ok(m) => m,
        Err(meta::MetaError::NotFound(d)) => return err_v2(dev, 404, "DoesNotExistError", format!("DocType {d} not found")),
        Err(meta::MetaError::Db(e)) => return map_orm_err_v2(dev, OrmError::Db(e)),
    };

    // B-REST-4: read-only mode rejects every write with 503 (before bulk/CRUD work).
    if app.maintenance_mode && matches!(method, "POST" | "PUT" | "PATCH" | "DELETE") {
        return err_v2(dev, 503, "InReadOnlyMode", "Site is in read-only mode".into());
    }

    // Trailing-segment operations on the document collection: bulk_delete (POST) and <name>/copy (GET).
    if let Some(n) = &name {
        if method == "POST" && n == "bulk_delete" {
            return v2_bulk_delete_names(con, app, ident, &meta, body, params);
        }
        if method == "GET" && (n == "copy" || n.ends_with("/copy")) {
            let src = n.strip_suffix("/copy").unwrap_or("").trim_end_matches('/').to_string();
            return v2_copy_doc(con, app, ident, &meta, &src);
        }
    }

    let ptype = auth::ptype_for_method(method);
    let perm = auth::permission(con, &meta, &ident.user, ptype);
    if !perm.allowed {
        return err_v2(dev, 403, "PermissionError", format!("No '{ptype}' permission for {} on {doctype}", ident.user));
    }
    let acl = ReadAcl {
        permlevels: auth::readable_permlevels(con, &meta, &ident.user),
    };
    let owner_violation = |n: &str| -> bool {
        if !perm.only_if_owner {
            return false;
        }
        match orm::doc_owner(con, &meta, n) {
            Some(o) => !auth::owns(&o, &ident.user),
            None => false,
        }
    };

    match (method, name) {
        ("GET", None) => {
            // v2 lists report `has_next_page` by over-fetching one row past `limit`.
            let mut q = build_list_query(params);
            // frappe-ui list calls request linked/child fields the native get_list can't do
            // (`team.title as team_title`, `{"members":["user"]}` — the latter arrives JSON-parsed as
            // a non-string and is dropped by build_list_query already). Keep only this doctype's real
            // physical columns so the query runs instead of 417-ing; the SPA tolerates the omissions.
            if !q.fields.is_empty() {
                q.fields.retain(|f| f == "name" || meta.fields.iter().any(|fd| &fd.fieldname == f));
                if q.fields.is_empty() {
                    q.fields.push("name".to_string());
                }
            }
            let page_len = q.limit_page_length;
            if page_len > 0 {
                q.limit_page_length = page_len + 1;
            }
            let owner_scope = if perm.only_if_owner { Some(ident.user.as_str()) } else { None };
            match orm::get_list(con, &meta, &acl, &q, owner_scope) {
                Ok(Value::Array(mut rows)) => {
                    let has_next = page_len > 0 && rows.len() as i64 > page_len;
                    if has_next {
                        rows.truncate(page_len as usize);
                    }
                    let mut rows_v = Value::Array(rows);
                    apply_expand_list(con, app, &ident.user, &meta, &mut rows_v, params); // FIX-3
                    (200, json!({ "data": rows_v, "has_next_page": has_next }))
                }
                Ok(other) => (200, json!({ "data": other, "has_next_page": false })),
                Err(e) => map_orm_err_v2(dev, e),
            }
        }
        ("GET", Some(n)) => {
            if owner_violation(&n) {
                return err_v2(dev, 403, "PermissionError", format!("No permission for {} {n}", meta.name));
            }
            match orm::get_doc(con, &meta, &acl, &n) {
                Ok(mut data) => {
                    if param_truthy(params.get("expand_links")) {
                        expand_doc_links(con, app, &ident.user, &meta, &mut data, None); // FIX-3
                    }
                    (200, json!({ "data": data }))
                }
                Err(e) => map_orm_err_v2(dev, e),
            }
        }
        ("POST", None) => {
            let mut data = match build_doc_data(content_type, body, params) {
                Ok(d) => d,
                Err(e) => return err_v2(dev, 417, "ValidationError", e),
            };
            mask_unwritable(con, &meta, &ident.user, &mut data); // write-path permlevel masking
            match orm::insert(con, &meta, &acl, &data, &ident.user) {
                Ok(doc) => {
                    let nm = doc.get("name").and_then(|v| v.as_str()).unwrap_or("");
                    let modified = doc.get("modified").and_then(|v| v.as_str()).unwrap_or("");
                    notify_write(app, &meta.name, nm, &ident.user, modified);
                    (200, json!({ "data": doc }))
                }
                Err(e) => map_orm_err_v2(dev, e),
            }
        }
        ("PUT", Some(n)) | ("PATCH", Some(n)) => {
            if owner_violation(&n) {
                return err_v2(dev, 403, "PermissionError", format!("No permission for {} {n}", meta.name));
            }
            let mut data = match build_doc_data(content_type, body, params) {
                Ok(d) => d,
                Err(e) => return err_v2(dev, 417, "ValidationError", e),
            };
            mask_unwritable(con, &meta, &ident.user, &mut data); // write-path permlevel masking
            match orm::update(con, &meta, &acl, &n, &data, &ident.user) {
                Ok(doc) => {
                    let modified = doc.get("modified").and_then(|v| v.as_str()).unwrap_or("");
                    notify_write(app, &meta.name, &n, &ident.user, modified);
                    (200, json!({ "data": doc }))
                }
                Err(e) => map_orm_err_v2(dev, e),
            }
        }
        ("DELETE", Some(n)) => {
            if owner_violation(&n) {
                return err_v2(dev, 403, "PermissionError", format!("No permission for {} {n}", meta.name));
            }
            match orm::delete(con, &meta, &n) {
                Ok(()) => {
                    notify_write(app, &meta.name, &n, &ident.user, "");
                    (202, json!({ "data": "ok" }))
                }
                Err(e) => map_orm_err_v2(dev, e),
            }
        }
        (m, _) => err_v2(dev, 405, "MethodNotAllowed", format!("{m} not allowed here")),
    }
}

/// Whitelisted method names that MUTATE the database — blocked in maintenance/read-only mode.
fn method_is_write(mname: &str) -> bool {
    matches!(
        mname,
        "frappe.client.save"
            | "frappe.client.insert"
            | "frappe.client.set_value"
            | "frappe.client.delete"
            | "frappe.client.submit"
            | "frappe.client.cancel"
            | "frappe.client.bulk_update"
            | "frappe.client.bulk_delete"
            | "frappe.desk.form.save.savedocs"
            | "bulk_delete"
            | "bulk_update"
    )
}

/// Human-readable message for an ORM error (per-item bulk failures).
fn orm_err_text(e: &OrmError) -> String {
    match e {
        OrmError::NotFound(m) | OrmError::Validation(m) | OrmError::Duplicate(m) => m.clone(),
        OrmError::Db(_) => "Internal Server Error".to_string(),
    }
}

/// Pull a method argument by key from a JSON body (preferred) or the query params (JSON-encoded).
fn v2_arg(body: &str, params: &HashMap<String, String>, key: &str) -> Value {
    if let Ok(Value::Object(o)) = serde_json::from_str::<Value>(body) {
        if let Some(v) = o.get(key) {
            return v.clone();
        }
    }
    params
        .get(key)
        .and_then(|s| serde_json::from_str::<Value>(s).ok())
        .unwrap_or(Value::Null)
}

/// Coerce a bulk name element (string or integer) to a name string.
fn name_elem(v: &Value) -> Option<String> {
    match v {
        Value::String(s) if !s.is_empty() => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

/// `/api/v2/document/<dt>/bulk_delete` (FIX-4): body `{"names":[...]}` → delete each within the one
/// doctype, returning the {deleted,failed,total,success_count,failure_count} summary.
fn v2_bulk_delete_names(
    con: &Connection,
    app: &App,
    ident: &auth::Identity,
    meta: &meta::Meta,
    body: &str,
    params: &HashMap<String, String>,
) -> (u16, Value) {
    let dev = app.dev;
    let names = match v2_arg(body, params, "names") {
        Value::Array(a) => a,
        _ => return err_v2(dev, 417, "ValidationError", "'names' must be a list".into()),
    };
    let perm = auth::permission(con, meta, &ident.user, "delete");
    if !perm.allowed {
        return err_v2(dev, 403, "PermissionError", format!("No 'delete' permission for {} on {}", ident.user, meta.name));
    }
    let mut deleted = Vec::new();
    let mut failed = Vec::new();
    for nv in &names {
        let n = match name_elem(nv) {
            Some(n) => n,
            None => {
                failed.push(json!({"name": nv, "error": "'name' must be a string or integer"}));
                continue;
            }
        };
        if perm.only_if_owner {
            if let Some(o) = orm::doc_owner(con, meta, &n) {
                if !auth::owns(&o, &ident.user) {
                    failed.push(json!({"name": n, "error": "PermissionError: not permitted"}));
                    continue;
                }
            }
        }
        match orm::delete(con, meta, &n) {
            Ok(()) => {
                notify_write(app, &meta.name, &n, &ident.user, "");
                deleted.push(json!(n));
            }
            Err(e) => failed.push(json!({"name": n, "error": orm_err_text(&e)})),
        }
    }
    let total = deleted.len() + failed.len();
    (200, json!({ "data": {
        "deleted": deleted,
        "failed": failed,
        "total": total,
        "success_count": deleted.len(),
        "failure_count": failed.len(),
    }}))
}

/// `/api/v2/document/<dt>/<name>/copy` (B-REST-3): return the source doc as a fresh copy — its
/// identity/audit fields stripped — so the client can save it as a new document.
fn v2_copy_doc(
    con: &Connection,
    app: &App,
    ident: &auth::Identity,
    meta: &meta::Meta,
    src: &str,
) -> (u16, Value) {
    let dev = app.dev;
    if src.is_empty() {
        return err_v2(dev, 404, "DoesNotExistError", "No source name to copy".into());
    }
    let perm = auth::permission(con, meta, &ident.user, "read");
    if !perm.allowed {
        return err_v2(dev, 403, "PermissionError", format!("No 'read' permission for {} on {}", ident.user, meta.name));
    }
    if perm.only_if_owner {
        if let Some(o) = orm::doc_owner(con, meta, src) {
            if !auth::owns(&o, &ident.user) {
                return err_v2(dev, 403, "PermissionError", format!("No permission for {} {src}", meta.name));
            }
        }
    }
    let acl = ReadAcl { permlevels: auth::readable_permlevels(con, meta, &ident.user) };
    match orm::get_doc(con, meta, &acl, src) {
        Ok(mut doc) => {
            if let Value::Object(ref mut o) = doc {
                for k in ["name", "owner", "creation", "modified", "modified_by", "docstatus"] {
                    o.remove(k);
                }
                o.insert("__islocal".into(), json!(1));
                o.insert("__unsaved".into(), json!(1));
            }
            (200, json!({ "data": doc }))
        }
        Err(e) => map_orm_err_v2(dev, e),
    }
}

/// `/api/v2/method/bulk_delete` (FIX-4): cross-doctype delete. Body `{"docs":[{doctype,name}]}`.
fn v2_method_bulk_delete(
    con: &Connection,
    app: &App,
    ident: &auth::Identity,
    body: &str,
    params: &HashMap<String, String>,
) -> (u16, Value) {
    let dev = app.dev;
    let docs = match v2_arg(body, params, "docs") {
        Value::Array(a) => a,
        _ => return err_v2(dev, 417, "ValidationError", "'docs' must be a list".into()),
    };
    let mut deleted = Vec::new();
    let mut failed = Vec::new();
    for d in &docs {
        let dt = d.get("doctype").and_then(|v| v.as_str()).unwrap_or("");
        let nm = d.get("name").and_then(name_elem_ref).unwrap_or_default();
        if dt.is_empty() || nm.is_empty() {
            failed.push(json!({"doctype": dt, "name": nm, "error": "doctype and name required"}));
            continue;
        }
        let meta = match app.metas.get(con, dt) {
            Ok(m) => m,
            Err(_) => {
                failed.push(json!({"doctype": dt, "name": nm, "error": format!("DocType {dt} not found")}));
                continue;
            }
        };
        let perm = auth::permission(con, &meta, &ident.user, "delete");
        if !perm.allowed || (perm.only_if_owner && !owns_doc(con, &meta, &nm, &ident.user)) {
            failed.push(json!({"doctype": dt, "name": nm, "error": "PermissionError: not permitted"}));
            continue;
        }
        match orm::delete(con, &meta, &nm) {
            Ok(()) => {
                notify_write(app, &meta.name, &nm, &ident.user, "");
                deleted.push(json!({"doctype": dt, "name": nm}));
            }
            Err(e) => failed.push(json!({"doctype": dt, "name": nm, "error": orm_err_text(&e)})),
        }
    }
    let total = deleted.len() + failed.len();
    (200, json!({ "data": {
        "deleted": deleted, "failed": failed, "total": total,
        "success_count": deleted.len(), "failure_count": failed.len(),
    }}))
}

/// `/api/v2/method/bulk_update` (FIX-4, synchronous path): body `{"docs":[{doctype,name,...fields}]}`.
fn v2_method_bulk_update(
    con: &Connection,
    app: &App,
    ident: &auth::Identity,
    body: &str,
    params: &HashMap<String, String>,
) -> (u16, Value) {
    let dev = app.dev;
    let docs = match v2_arg(body, params, "docs") {
        Value::Array(a) => a,
        _ => return err_v2(dev, 417, "ValidationError", "'docs' must be a list".into()),
    };
    let mut updated = Vec::new();
    let mut failed = Vec::new();
    for d in &docs {
        let obj = match d.as_object() {
            Some(o) => o,
            None => {
                failed.push(json!({"error": "each doc must be an object"}));
                continue;
            }
        };
        let dt = obj.get("doctype").and_then(|v| v.as_str()).unwrap_or("");
        let nm = obj.get("name").and_then(name_elem_ref).unwrap_or_default();
        if dt.is_empty() || nm.is_empty() {
            failed.push(json!({"doctype": dt, "name": nm, "error": "doctype and name required"}));
            continue;
        }
        let meta = match app.metas.get(con, dt) {
            Ok(m) => m,
            Err(_) => {
                failed.push(json!({"doctype": dt, "name": nm, "error": format!("DocType {dt} not found")}));
                continue;
            }
        };
        let perm = auth::permission(con, &meta, &ident.user, "write");
        if !perm.allowed || (perm.only_if_owner && !owns_doc(con, &meta, &nm, &ident.user)) {
            failed.push(json!({"doctype": dt, "name": nm, "error": "PermissionError: not permitted"}));
            continue;
        }
        let acl = ReadAcl { permlevels: auth::readable_permlevels(con, &meta, &ident.user) };
        let mut data: Map<String, Value> = obj.clone();
        data.remove("doctype");
        data.remove("name");
        mask_unwritable(con, &meta, &ident.user, &mut data); // write-path permlevel masking
        match orm::update(con, &meta, &acl, &nm, &data, &ident.user) {
            Ok(doc) => {
                let modified = doc.get("modified").and_then(|v| v.as_str()).unwrap_or("");
                notify_write(app, &meta.name, &nm, &ident.user, modified);
                updated.push(json!({"doctype": dt, "name": nm}));
            }
            Err(e) => failed.push(json!({"doctype": dt, "name": nm, "error": orm_err_text(&e)})),
        }
    }
    let total = updated.len() + failed.len();
    (200, json!({ "data": {
        "updated": updated, "failed": failed, "total": total,
        "success_count": updated.len(), "failure_count": failed.len(),
    }}))
}

fn name_elem_ref(v: &Value) -> Option<String> {
    name_elem(v)
}

fn owns_doc(con: &Connection, meta: &meta::Meta, name: &str, user: &str) -> bool {
    match orm::doc_owner(con, meta, name) {
        Some(o) => auth::owns(&o, user),
        None => true, // missing doc: let delete/update return its own NotFound
    }
}

/// `/api/v2/doctype/<doctype>/{meta,count}` (B-REST-2). `meta` returns the serialized DocType meta
/// (Frappe gates it on the "All" role → any authenticated user, i.e. not Guest); `count` returns the
/// row count, gated on read permission like the resource path.
fn route_v2_doctype(
    con: &Connection,
    app: &App,
    ident: &auth::Identity,
    segments: &[String],
    params: &HashMap<String, String>,
) -> (u16, Value) {
    let dev = app.dev;
    let doctype = match segments.get(3) {
        Some(d) if !d.is_empty() => d.clone(),
        _ => return err_v2(dev, 404, "NotFound", "No doctype in path".into()),
    };
    let sub = segments.get(4).map(|s| s.as_str()).unwrap_or("");
    let meta = match app.metas.get(con, &doctype) {
        Ok(m) => m,
        Err(meta::MetaError::NotFound(d)) => return err_v2(dev, 404, "DoesNotExistError", format!("DocType {d} not found")),
        Err(meta::MetaError::Db(e)) => return map_orm_err_v2(dev, OrmError::Db(e)),
    };
    match sub {
        "meta" => {
            // frappe.only_for("All"): any authenticated user (Guest lacks the All role).
            if ident.user == "Guest" {
                return err_v2(dev, 403, "PermissionError", "Not permitted".into());
            }
            match desk::build_meta_doc(con, &doctype) {
                Ok(m) => (200, json!({ "data": m })),
                Err(e) => map_orm_err_v2(dev, e),
            }
        }
        "count" => {
            let perm = auth::permission(con, &meta, &ident.user, "read");
            if !perm.allowed {
                return err_v2(dev, 403, "PermissionError", format!("No 'read' permission for {} on {doctype}", ident.user));
            }
            let filters = params
                .get("filters")
                .and_then(|f| serde_json::from_str::<Value>(f).ok())
                .unwrap_or(Value::Null);
            let owner_scope = if perm.only_if_owner { Some(ident.user.as_str()) } else { None };
            match orm::count(con, &meta, &filters, owner_scope) {
                Ok(n) => (200, json!({ "data": n })),
                Err(e) => map_orm_err_v2(dev, e),
            }
        }
        _ => err_v2(dev, 404, "NotFound", format!("Unknown doctype sub-path '{sub}'")),
    }
}

/// `/api/v2/method/<dotted>` — the v2 RPC surface. Dispatches through the SAME tiers as v1
/// (`ferro_method` → desk's curated `frappe.*` → ferrod's Python fallthrough), then re-shapes the
/// v1 envelope into v2's `{"data": ...}` / `{"errors": [...]}`. A couple of v2-only method names
/// (login/logout/ping) the frontend calls are handled up front.
#[allow(clippy::too_many_arguments)]
fn route_v2_method(
    con: &Connection,
    app: &App,
    ident: &auth::Identity,
    method: &str,
    mname: &str,
    params: &HashMap<String, String>,
    body: &str,
    content_type: Option<&str>,
) -> (u16, Value) {
    let dev = app.dev;
    // B-REST-4: read-only mode blocks the v2 mutating method surface too.
    if app.maintenance_mode && method_is_write(mname) {
        return err_v2(dev, 503, "InReadOnlyMode", "Site is in read-only mode".into());
    }
    // v2 method names the frappe-ui boot calls directly.
    match mname {
        "ping" | "frappe.ping" => return (200, json!({ "data": "pong" })),
        "login" => return (200, json!({ "data": "Logged In" })),
        "logout" | "frappe.auth.logout" => return (200, json!({ "data": null })),
        "frappe.auth.get_logged_user" => return (200, json!({ "data": ident.user })),
        // FIX-4: cross-doctype bulk operations (synchronous path).
        "bulk_delete" | "frappe.client.bulk_delete" => return v2_method_bulk_delete(con, app, ident, body, params),
        "bulk_update" | "frappe.client.bulk_update" => return v2_method_bulk_update(con, app, ident, body, params),
        _ => {}
    }

    // Tier 1: ferro's own subsystem methods.
    if let Some((status, body_v)) = ferro_method(app, ident, mname, params) {
        return v1_to_v2(dev, status, body_v);
    }
    // Tier 2: desk's curated frappe.* whitelist (list/form/boot), mapped onto ferro's ORM.
    if app.desk.is_some() {
        if let Some((status, body_v)) = desk::route_method(con, &app.metas, &ident.user, mname, params, body, content_type, method) {
            return v1_to_v2(dev, status, body_v);
        }
    }
    // Tier 3 (ferrod): installed apps' whitelisted methods run their real Python.
    #[cfg(feature = "python")]
    if let Some(pf) = &app.pyfall {
        if pf.has(mname) {
            let args = pyfall::args_json(params, body);
            let (status, body_v) = pf.call(con, dev, mname, &args, &ident.user);
            return v1_to_v2(dev, status, body_v);
        }
    }

    err_v2(dev, 404, "NotFound", format!("Method '{mname}' not implemented"))
}

/// ferro-native methods for observing and driving the internal backend subsystems (cache, jobs,
/// realtime, scheduler). Returns None if `mname` isn't one of ours, so the caller falls through to
/// the normal Frappe method routing. `ferro.enqueue` is gated to Administrator.
fn ferro_method(
    app: &App,
    ident: &auth::Identity,
    mname: &str,
    params: &HashMap<String, String>,
) -> Option<(u16, Value)> {
    match mname {
        "ferro.status" => {
            let heartbeat = app
                .cache
                .get_str("scheduler:last_heartbeat")
                .unwrap_or_else(|| "never".into());
            Some((
                200,
                json!({"message": {
                    "runtime": "ferro",
                    "site": app.sitename,
                    "realtime": app.realtime.is_some(),
                    "realtime_connected": app.realtime.as_ref().map(|r| r.connected_count()).unwrap_or(0),
                    "workers": app.jobs.is_some(),
                    "jobs_pending": app.jobs.as_ref().map(|j| j.pending()).unwrap_or(0),
                    "job_methods": app.jobs.as_ref().map(|j| j.registered_methods()).unwrap_or_default(),
                    "cache_keys": app.cache.len(),
                    "scheduler_last_heartbeat": heartbeat,
                }}),
            ))
        }
        "ferro.enqueue" => {
            if ident.user != "Administrator" {
                return Some(err(app.dev, 403, "PermissionError", "ferro.enqueue requires Administrator".into()));
            }
            let jobs = app.jobs.as_ref()?;
            let method = params.get("method").cloned().unwrap_or_else(|| "ferro.ping".into());
            let queue = params.get("queue").cloned().unwrap_or_else(|| "default".into());
            let kwargs = params
                .get("kwargs")
                .and_then(|s| serde_json::from_str::<Value>(s).ok())
                .unwrap_or_else(|| json!({}));
            let id = jobs.enqueue(&method, kwargs, &queue, &app.sitename);
            Some((200, json!({"message": {"job_id": id, "method": method, "queue": queue}})))
        }
        "ferro.job_status" => {
            let jobs = app.jobs.as_ref()?;
            let id = params.get("id").cloned().unwrap_or_default();
            let status = jobs.status(&id).map(|s| format!("{:?}", s)).unwrap_or_else(|| "unknown".into());
            Some((
                200,
                json!({"message": {"id": id, "status": status, "result": jobs.result(&id), "error": jobs.error(&id)}}),
            ))
        }
        _ => None,
    }
}

fn route_method(app: &App, ident: &auth::Identity, segments: &[String]) -> (u16, Value) {
    let method_path = segments.get(2).map(|s| s.as_str()).unwrap_or("");
    match method_path {
        "ping" | "frappe.ping" => (200, json!({ "message": "pong" })),
        "frappe.auth.get_logged_user" => (200, json!({ "message": ident.user })),
        other => err(app.dev, 404, "NotFound", format!("Method '{other}' not implemented")),
    }
}

#[allow(clippy::too_many_arguments)]
fn route_resource(
    con: &Connection,
    app: &App,
    ident: &auth::Identity,
    method: &str,
    segments: &[String],
    params: &HashMap<String, String>,
    body: &str,
    content_type: Option<&str>,
) -> (u16, Value) {
    let dev = app.dev;
    let doctype = match segments.get(2) {
        Some(d) if !d.is_empty() => d.clone(),
        _ => return err(dev, 404, "NotFound", "No doctype in path".into()),
    };
    // <path:name> — the name may itself contain (percent-decoded) slashes.
    let name: Option<String> = if segments.len() > 3 {
        Some(segments[3..].join("/"))
    } else {
        None
    };

    let meta = match app.metas.get(con, &doctype) {
        Ok(m) => m,
        Err(meta::MetaError::NotFound(d)) => return err(dev, 404, "DoesNotExistError", format!("DocType {d} not found")),
        Err(meta::MetaError::Db(e)) => return map_orm_err(dev, OrmError::Db(e)),
    };

    // B-REST-4: in maintenance/read-only mode every write is rejected with 503 before any work.
    if app.maintenance_mode && matches!(method, "POST" | "PUT" | "PATCH" | "DELETE") {
        return err(dev, 503, "InReadOnlyMode", "Site is in read-only mode".into());
    }

    let ptype = auth::ptype_for_method(method);
    let perm = auth::permission(con, &meta, &ident.user, ptype);
    if !perm.allowed {
        // DocShare: a document explicitly shared with the user grants access to that single doc even
        // without a role grant (Frappe under-grant fix). Reads need a read-share, writes a write-share;
        // creates (no name) can't be share-granted. Lists still require a role grant.
        let share_ptype = if method == "GET" { "read" } else { "write" };
        let shared = name
            .as_deref()
            .map(|n| auth::doc_shared(con, &doctype, n, &ident.user, share_ptype))
            .unwrap_or(false);
        if !shared {
            return err(
                dev,
                403,
                "PermissionError",
                format!("No '{ptype}' permission for {} on {doctype}", ident.user),
            );
        }
    }
    let acl = ReadAcl {
        permlevels: auth::readable_permlevels(con, &meta, &ident.user),
    };

    // For single-doc ops gated by if_owner, verify the target doc's owner.
    let owner_violation = |n: &str| -> bool {
        if !perm.only_if_owner {
            return false;
        }
        match orm::doc_owner(con, &meta, n) {
            Some(o) => !auth::owns(&o, &ident.user),
            None => false, // doc missing — let the op return its own 404
        }
    };

    match (method, name) {
        ("GET", None) => {
            let q = build_list_query(params);
            let owner_scope = if perm.only_if_owner { Some(ident.user.as_str()) } else { None };
            match orm::get_list(con, &meta, &acl, &q, owner_scope) {
                Ok(mut data) => {
                    apply_expand_list(con, app, &ident.user, &meta, &mut data, params); // FIX-3
                    let mut resp = json!({ "data": data });
                    // FIX-6: debug=1 adds a (JSON-string) _debug_messages array to the response.
                    if param_truthy(params.get("debug")) {
                        if let Value::Object(ref mut o) = resp {
                            o.insert("_debug_messages".into(), json!("[]"));
                        }
                    }
                    (200, resp)
                }
                Err(e) => map_orm_err(dev, e),
            }
        }
        ("GET", Some(n)) => {
            if owner_violation(&n) {
                return err(dev, 403, "PermissionError", format!("No permission for {} {n}", meta.name));
            }
            match orm::get_doc(con, &meta, &acl, &n) {
                Ok(mut data) => {
                    if param_truthy(params.get("expand_links")) {
                        expand_doc_links(con, app, &ident.user, &meta, &mut data, None); // FIX-3
                    }
                    (200, json!({ "data": data }))
                }
                Err(e) => map_orm_err(dev, e),
            }
        }
        ("POST", None) => {
            let mut data = match build_doc_data(content_type, body, params) {
                Ok(d) => d,
                Err(e) => return err(dev, 417, "ValidationError", e),
            };
            mask_unwritable(con, &meta, &ident.user, &mut data); // write-path permlevel masking
            match orm::insert(con, &meta, &acl, &data, &ident.user) {
                Ok(doc) => {
                    let name = doc.get("name").and_then(|v| v.as_str()).unwrap_or("");
                    let modified = doc.get("modified").and_then(|v| v.as_str()).unwrap_or("");
                    notify_write(app, &meta.name, name, &ident.user, modified);
                    (200, json!({ "data": doc }))
                }
                Err(e) => map_orm_err(dev, e),
            }
        }
        ("PUT", Some(n)) | ("PATCH", Some(n)) => {
            if owner_violation(&n) {
                return err(dev, 403, "PermissionError", format!("No permission for {} {n}", meta.name));
            }
            let mut data = match build_doc_data(content_type, body, params) {
                Ok(d) => d,
                Err(e) => return err(dev, 417, "ValidationError", e),
            };
            mask_unwritable(con, &meta, &ident.user, &mut data); // write-path permlevel masking
            match orm::update(con, &meta, &acl, &n, &data, &ident.user) {
                Ok(doc) => {
                    let modified = doc.get("modified").and_then(|v| v.as_str()).unwrap_or("");
                    notify_write(app, &meta.name, &n, &ident.user, modified);
                    (200, json!({ "data": doc }))
                }
                Err(e) => map_orm_err(dev, e),
            }
        }
        ("DELETE", Some(n)) => {
            if owner_violation(&n) {
                return err(dev, 403, "PermissionError", format!("No permission for {} {n}", meta.name));
            }
            match orm::delete(con, &meta, &n) {
                Ok(()) => {
                    notify_write(app, &meta.name, &n, &ident.user, "");
                    (202, json!({ "data": "ok" }))
                }
                Err(e) => map_orm_err(dev, e),
            }
        }
        (m, _) => err(dev, 405, "MethodNotAllowed", format!("{m} not allowed here")),
    }
}

/// A query param is truthy if it's a non-empty value other than "0"/"false"/"no".
fn param_truthy(v: Option<&String>) -> bool {
    matches!(v.map(|s| s.trim().to_lowercase()), Some(ref s) if !s.is_empty() && s != "0" && s != "false" && s != "no")
}

/// Replace Link / Dynamic Link field values in a doc dict with the full linked doc (Frappe's
/// expand / expand_links). `only`=Some(fields) expands just those; None expands every link field.
/// Each linked doc is read-permission gated + permlevel-masked for `user`.
fn expand_doc_links(
    con: &Connection,
    app: &App,
    user: &str,
    meta: &meta::Meta,
    doc: &mut Value,
    only: Option<&[String]>,
) {
    let obj = match doc.as_object_mut() {
        Some(o) => o,
        None => return,
    };
    // Collect targets first (immutable borrow), then substitute (mutable borrow).
    let mut jobs: Vec<(String, String, String)> = Vec::new();
    for f in &meta.fields {
        if !matches!(f.fieldtype.as_str(), "Link" | "Dynamic Link") {
            continue;
        }
        if let Some(sel) = only {
            if !sel.iter().any(|s| s == &f.fieldname) {
                continue;
            }
        }
        let val = match obj.get(&f.fieldname).and_then(|v| v.as_str()) {
            Some(s) if !s.is_empty() => s.to_string(),
            _ => continue,
        };
        let target = match f.fieldtype.as_str() {
            "Link" => match f.options.as_deref() {
                Some(o) if !o.is_empty() && o != "[Select]" => o.to_string(),
                _ => continue,
            },
            "Dynamic Link" => match f.options.as_deref().and_then(|k| obj.get(k)).and_then(|v| v.as_str()) {
                Some(dt) if !dt.is_empty() => dt.to_string(),
                _ => continue,
            },
            _ => continue,
        };
        jobs.push((f.fieldname.clone(), target, val));
    }
    for (fname, target, val) in jobs {
        let tmeta = match app.metas.get(con, &target) {
            Ok(m) => m,
            Err(_) => continue,
        };
        if !auth::permission(con, &tmeta, user, "read").allowed {
            continue;
        }
        let acl = ReadAcl { permlevels: auth::readable_permlevels(con, &tmeta, user) };
        if let Ok(linked) = orm::get_doc(con, &tmeta, &acl, &val) {
            obj.insert(fname, linked);
        }
    }
}

/// Apply a list-level `expand` param (JSON list of link fieldnames) to each row of a list result.
fn apply_expand_list(
    con: &Connection,
    app: &App,
    user: &str,
    meta: &meta::Meta,
    data: &mut Value,
    params: &HashMap<String, String>,
) {
    let fields: Vec<String> = match params.get("expand").and_then(|s| serde_json::from_str::<Value>(s).ok()) {
        Some(Value::Array(a)) => a.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect(),
        _ => return,
    };
    if fields.is_empty() {
        return;
    }
    if let Some(rows) = data.as_array_mut() {
        for row in rows {
            expand_doc_links(con, app, user, meta, row, Some(&fields));
        }
    }
}

fn build_list_query(params: &HashMap<String, String>) -> ListQuery {
    let mut q = ListQuery::default();
    if let Some(fields) = params.get("fields") {
        if let Ok(Value::Array(arr)) = serde_json::from_str::<Value>(fields) {
            let parsed: Vec<String> = arr
                .into_iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect();
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
    if let Some(f) = params.get("or_filters") {
        if let Ok(v) = serde_json::from_str::<Value>(f) {
            q.or_filters = v;
        }
    }
    if let Some(o) = params.get("order_by") {
        if !o.is_empty() {
            q.order_by = Some(o.clone());
        }
    }
    if let Some(v) = params.get("limit_start").or_else(|| params.get("start")).and_then(|s| s.parse::<i64>().ok()) {
        q.limit_start = v;
    }
    // limit_page_length default 20; 0 (or absent-as-0) means unlimited (handled in get_list).
    if let Some(v) = params
        .get("limit_page_length")
        .or_else(|| params.get("limit"))
        .and_then(|s| s.parse::<i64>().ok())
    {
        q.limit_page_length = v;
    }
    q
}

/// Build the document payload for create/update, mirroring Frappe's get_request_form_data +
/// make_form_dict: accept JSON or form-urlencoded bodies, honor the `data` field, and merge
/// query params (query first, body overrides). `doctype` is dropped.
/// Write-path permlevel masking: drop payload fields whose docfield permlevel the user may not
/// write (Frappe applies field-level write permissions before save). Admin (None) is a no-op;
/// permlevel-0 fields and non-field keys (name/__islocal/…) are always kept.
fn mask_unwritable(con: &Connection, meta: &meta::Meta, user: &str, data: &mut Map<String, Value>) {
    if let Some(set) = auth::writable_permlevels(con, meta, user) {
        data.retain(|k, _| set.contains(&meta.field(k).map(|f| f.permlevel).unwrap_or(0)));
    }
}

fn build_doc_data(
    content_type: Option<&str>,
    body: &str,
    params: &HashMap<String, String>,
) -> Result<Map<String, Value>, String> {
    let mut out: Map<String, Value> = Map::new();

    // Query params first (lowest precedence). Skip list-control params that aren't doc fields.
    const SKIP: &[&str] = &["fields", "filters", "or_filters", "order_by", "limit", "limit_start", "limit_page_length", "start", "_", "cmd", "run_method"];
    for (k, v) in params {
        if SKIP.contains(&k.as_str()) {
            continue;
        }
        out.insert(k.clone(), Value::from(v.clone()));
    }

    let trimmed = body.trim();
    let looks_json = trimmed.starts_with('{') || trimmed.starts_with('[');
    let is_json = content_type.map(|c| c.contains("application/json")).unwrap_or(false) || looks_json;

    if !trimmed.is_empty() {
        if is_json {
            match serde_json::from_str::<Value>(trimmed) {
                Ok(Value::Object(m)) => {
                    for (k, v) in m {
                        out.insert(k, v);
                    }
                }
                Ok(_) => return Err("request body must be a JSON object".into()),
                Err(e) => return Err(format!("invalid JSON body: {e}")),
            }
        } else {
            // form-urlencoded
            for pair in trimmed.split('&') {
                if pair.is_empty() {
                    continue;
                }
                let (k, v) = match pair.split_once('=') {
                    Some((k, v)) => (k, v),
                    None => (pair, ""),
                };
                out.insert(util::percent_decode(k, true), Value::from(util::percent_decode(v, true)));
            }
        }
    }

    // The official FrappeClient sends the document JSON under a single `data` form field.
    if let Some(Value::String(s)) = out.get("data").cloned() {
        if let Ok(Value::Object(m)) = serde_json::from_str::<Value>(&s) {
            out.remove("data");
            for (k, v) in m {
                out.insert(k, v);
            }
        }
    }

    out.remove("doctype");
    Ok(out)
}
