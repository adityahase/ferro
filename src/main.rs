//! ferro — a Rust runtime that serves the Frappe REST API against an existing Frappe
//! SQLite site, in place of the CPython+Frappe worker.
//!
//! Usage:
//!   ferro serve <site-dir-or-db> [--port N] [--threads N] [--default-user U] [--meta-cap N] [--dev]
//!   ferro request <site-dir-or-db> <METHOD> <url-path-with-query> [json-body] [--user U] [--token k:s]
//!   ferro provision-key <site-dir-or-db> <user>
//!   ferro <db>                      # legacy smoke test (counts meta tables)

mod auth;
mod crypto;
mod meta;
mod naming;
mod orm;
mod util;

use auth::AuthOutcome;
use meta::MetaCache;
use orm::{ListQuery, OrmError, ReadAcl};
use rusqlite::Connection;
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::io::Read;
use std::path::Path;
use std::sync::Arc;
use std::thread;
use tiny_http::{Header, Method, Response, Server};

/// Maximum request body we will read (DoS guard). 413 above this.
const MAX_BODY: u64 = 8 * 1024 * 1024;

struct App {
    metas: MetaCache,
    default_user: String,
    encryption_key: Option<String>,
    dev: bool,
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(|s| s.as_str()) {
        Some("serve") => serve(&args[2..]),
        Some("provision-key") => provision(&args[2..]),
        Some("request") => request_cli(&args[2..]),
        Some(db) if args.len() == 2 => smoke(db),
        _ => {
            eprintln!(
                "usage:\n  ferro serve <site-dir-or-db> [--port N] [--threads N] [--default-user U] [--meta-cap N] [--dev]\n  ferro request <site-dir-or-db> <METHOD> <url-path-with-query> [json-body] [--user U] [--token k:s]\n  ferro provision-key <site-dir-or-db> <user>"
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
    let text = std::fs::read_to_string(site_dir.join("site_config.json")).ok()?;
    let v: Value = serde_json::from_str(&text).ok()?;
    v.get("encryption_key").and_then(|x| x.as_str()).map(|s| s.to_string())
}

fn open_conn(db_path: &str) -> Connection {
    let con = Connection::open(db_path).expect("open sqlite");
    con.busy_timeout(std::time::Duration::from_secs(5)).ok();
    con.pragma_update(None, "foreign_keys", "OFF").ok();
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
            other => {
                if body.is_empty() {
                    body = other.to_string();
                }
                i += 1;
            }
        }
    }
    let con = open_conn(&resolve_db_path(path));
    let app = App {
        metas: MetaCache::new(512),
        default_user,
        encryption_key: load_encryption_key(path),
        dev,
    };
    let auth_header = token.map(|t| format!("token {t}"));
    // Infer content-type from the body shape so the CLI can exercise both JSON and form bodies.
    let ct = if body.trim_start().starts_with(['{', '[']) {
        Some("application/json")
    } else {
        Some("application/x-www-form-urlencoded")
    };
    let (status, value) = route(&con, &app, &method, &url, &body, ct, auth_header.as_deref());
    println!("HTTP {status}");
    println!("{}", serde_json::to_string_pretty(&value).unwrap_or_default());
}

fn serve(args: &[String]) {
    let path = args.first().expect("need <site-dir-or-db>");
    let db_path = resolve_db_path(path);
    let encryption_key = load_encryption_key(path);
    let mut port = 8080u16;
    // 4 worker threads is a good default for SQLite (writes serialize anyway) and keeps
    // resident memory low; raise --threads for more read concurrency (≈2 MiB/thread).
    let mut threads = 4usize;
    let mut default_user = "Guest".to_string();
    let mut meta_cap = 512usize;
    let mut dev = false;
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--port" => {
                port = args.get(i + 1).and_then(|s| s.parse().ok()).unwrap_or(port);
                i += 2;
            }
            "--threads" => {
                threads = args.get(i + 1).and_then(|s| s.parse().ok()).unwrap_or(threads);
                i += 2;
            }
            "--default-user" => {
                default_user = args.get(i + 1).cloned().unwrap_or(default_user);
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
            _ => i += 1,
        }
    }

    let app = Arc::new(App {
        metas: MetaCache::new(meta_cap),
        default_user,
        encryption_key,
        dev,
    });

    let addr = format!("0.0.0.0:{port}");
    let server = Arc::new(Server::http(&addr).expect("bind"));
    eprintln!(
        "ferro serving {} on http://{} ({} threads, default-user={}, fernet={})",
        db_path,
        addr,
        threads,
        app.default_user,
        app.encryption_key.is_some()
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

    // Body-size DoS guard: reject oversize bodies up front, and cap the actual read.
    if let Some(cl) = header_value(&req, "Content-Length").and_then(|s| s.parse::<u64>().ok()) {
        if cl > MAX_BODY {
            return respond(req, app, err(app.dev, 413, "RequestEntityTooLarge", "Request body too large".into()));
        }
    }

    let mut body = String::new();
    if matches!(method.as_str(), "POST" | "PUT" | "PATCH") {
        let mut limited = req.as_reader().take(MAX_BODY + 1);
        let mut raw = Vec::new();
        let _ = limited.read_to_end(&mut raw);
        if raw.len() as u64 > MAX_BODY {
            return respond(req, app, err(app.dev, 413, "RequestEntityTooLarge", "Request body too large".into()));
        }
        body = String::from_utf8_lossy(&raw).into_owned();
    }

    let resp = route(con, app, &method, &url, &body, content_type.as_deref(), auth_header.as_deref());
    respond(req, app, resp);
}

fn respond(req: tiny_http::Request, _app: &App, resp: (u16, Value)) {
    let (status, value) = resp;
    let payload = serde_json::to_vec(&value).unwrap_or_else(|_| b"{}".to_vec());
    let header = Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..]).unwrap();
    let response = Response::from_data(payload).with_status_code(status).with_header(header);
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

fn route(
    con: &Connection,
    app: &App,
    method: &str,
    url: &str,
    body: &str,
    content_type: Option<&str>,
    auth_header: Option<&str>,
) -> (u16, Value) {
    let (segments, params) = util::parse_url(url);
    let ident = match auth::resolve_user(con, auth_header, &app.default_user, app.encryption_key.as_deref()) {
        AuthOutcome::Ok(id) => id,
        AuthOutcome::Unauthorized => {
            return err(app.dev, 401, "AuthenticationError", "Invalid authentication credentials".into())
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
        Some("method") => route_method(app, &ident, &segments),
        Some("resource") => route_resource(con, app, &ident, method, &segments, &params, body, content_type),
        _ => err(app.dev, 404, "NotFound", format!("Unknown path /{}", segments.join("/"))),
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

    let ptype = auth::ptype_for_method(method);
    let perm = auth::permission(con, &meta, &ident.user, ptype);
    if !perm.allowed {
        return err(
            dev,
            403,
            "PermissionError",
            format!("No '{ptype}' permission for {} on {doctype}", ident.user),
        );
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
                Ok(data) => (200, json!({ "data": data })),
                Err(e) => map_orm_err(dev, e),
            }
        }
        ("GET", Some(n)) => {
            if owner_violation(&n) {
                return err(dev, 403, "PermissionError", format!("No permission for {} {n}", meta.name));
            }
            match orm::get_doc(con, &meta, &acl, &n) {
                Ok(data) => (200, json!({ "data": data })),
                Err(e) => map_orm_err(dev, e),
            }
        }
        ("POST", None) => {
            let data = match build_doc_data(content_type, body, params) {
                Ok(d) => d,
                Err(e) => return err(dev, 417, "ValidationError", e),
            };
            match orm::insert(con, &meta, &acl, &data, &ident.user) {
                Ok(doc) => (200, json!({ "data": doc })),
                Err(e) => map_orm_err(dev, e),
            }
        }
        ("PUT", Some(n)) | ("PATCH", Some(n)) => {
            if owner_violation(&n) {
                return err(dev, 403, "PermissionError", format!("No permission for {} {n}", meta.name));
            }
            let data = match build_doc_data(content_type, body, params) {
                Ok(d) => d,
                Err(e) => return err(dev, 417, "ValidationError", e),
            };
            match orm::update(con, &meta, &acl, &n, &data, &ident.user) {
                Ok(doc) => (200, json!({ "data": doc })),
                Err(e) => map_orm_err(dev, e),
            }
        }
        ("DELETE", Some(n)) => {
            if owner_violation(&n) {
                return err(dev, 403, "PermissionError", format!("No permission for {} {n}", meta.name));
            }
            match orm::delete(con, &meta, &n) {
                Ok(()) => (202, json!({ "data": "ok" })),
                Err(e) => map_orm_err(dev, e),
            }
        }
        (m, _) => err(dev, 405, "MethodNotAllowed", format!("{m} not allowed here")),
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
