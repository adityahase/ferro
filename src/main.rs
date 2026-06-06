//! ferro — a Rust runtime that serves the Frappe REST API against an existing Frappe
//! SQLite site, in place of the CPython+Frappe worker.
//!
//! Usage:
//!   ferro serve <site-dir-or-db> [--port N] [--threads N] [--default-user U] [--meta-cap N]
//!   ferro provision-key <site-dir-or-db> <user>
//!   ferro <db>                      # legacy smoke test (counts meta tables)

mod auth;
mod meta;
mod orm;
mod util;

use meta::MetaCache;
use orm::{ListQuery, OrmError};
use rusqlite::Connection;
use serde_json::{json, Map, Value};
use std::io::Read;
use std::sync::Arc;
use std::thread;
use tiny_http::{Header, Method, Response, Server};

struct App {
    metas: MetaCache,
    default_user: String,
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
                "usage:\n  ferro serve <site-dir-or-db> [--port N] [--threads N] [--default-user U] [--meta-cap N]\n  ferro request <site-dir-or-db> <METHOD> <url-path-with-query> [json-body] [--user U] [--token k:s]\n  ferro provision-key <site-dir-or-db> <user>"
            );
            std::process::exit(2);
        }
    }
}

/// Resolve a CLI path argument to a concrete .db file.
/// Accepts the .db file directly, or a site directory containing site_config.json.
fn resolve_db_path(arg: &str) -> String {
    let p = std::path::Path::new(arg);
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

fn open_conn(db_path: &str) -> Connection {
    let con = Connection::open(db_path).expect("open sqlite");
    con.busy_timeout(std::time::Duration::from_secs(5)).ok();
    con.pragma_update(None, "foreign_keys", "OFF").ok();
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
    let path = args.get(0).expect("need <site-dir-or-db>");
    let user = args.get(1).map(|s| s.as_str()).unwrap_or("Administrator");
    let con = open_conn(&resolve_db_path(path));
    let (key, secret) = auth::provision_key(&con, user).expect("provision");
    println!("api_key={key}");
    println!("api_secret={secret}");
    println!("Authorization: token {key}:{secret}");
}

/// In-process request: exercises the full route()/auth/meta/orm stack without a socket.
/// `ferro request <db> <METHOD> <url> [body] [--user U] [--token k:s] [--default-user U]`
fn request_cli(args: &[String]) {
    let path = args.get(0).expect("need <site-dir-or-db>");
    let method = args.get(1).map(|s| s.to_uppercase()).unwrap_or_else(|| "GET".into());
    let url = args.get(2).cloned().unwrap_or_else(|| "/".into());
    let mut body = String::new();
    let mut default_user = "Administrator".to_string();
    let mut token: Option<String> = None;
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
    };
    let auth_header = token.map(|t| format!("token {t}"));
    let (status, value) = route(&con, &app, &method, &url, &body, auth_header.as_deref());
    println!("HTTP {status}");
    println!("{}", serde_json::to_string_pretty(&value).unwrap_or_default());
}

fn serve(args: &[String]) {
    let path = args.get(0).expect("need <site-dir-or-db>");
    let db_path = resolve_db_path(path);
    let mut port = 8080u16;
    let mut threads = 2usize;
    let mut default_user = "Guest".to_string();
    let mut meta_cap = 512usize;
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
            _ => i += 1,
        }
    }

    let app = Arc::new(App {
        metas: MetaCache::new(meta_cap),
        default_user,
    });

    let addr = format!("0.0.0.0:{port}");
    let server = Arc::new(Server::http(&addr).expect("bind"));
    eprintln!(
        "ferro serving {} on http://{} ({} threads, default-user={})",
        db_path, addr, threads, app.default_user
    );

    let mut handles = Vec::new();
    for _ in 0..threads {
        let server = server.clone();
        let app = app.clone();
        let dbp = db_path.clone();
        handles.push(thread::spawn(move || {
            let con = open_conn(&dbp);
            loop {
                match server.recv() {
                    Ok(req) => handle(req, &con, &app),
                    Err(_) => break,
                }
            }
        }));
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

    let mut body = String::new();
    if matches!(method.as_str(), "POST" | "PUT" | "PATCH") {
        let _ = req.as_reader().read_to_string(&mut body);
    }

    let (status, value) = route(con, app, &method, &url, &body, auth_header.as_deref());

    let payload = serde_json::to_vec(&value).unwrap_or_else(|_| b"{}".to_vec());
    let header = Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..]).unwrap();
    let response = Response::from_data(payload)
        .with_status_code(status)
        .with_header(header);
    let _ = req.respond(response);
}

fn err(status: u16, exc_type: &str, msg: String) -> (u16, Value) {
    (
        status,
        json!({
            "exception": format!("{exc_type}: {msg}"),
            "exc_type": exc_type,
            "_server_messages": serde_json::to_string(&vec![msg]).unwrap_or_default(),
        }),
    )
}

fn map_orm_err(e: OrmError) -> (u16, Value) {
    match e {
        OrmError::NotFound(m) => err(404, "DoesNotExistError", m),
        OrmError::Validation(m) => err(417, "ValidationError", m),
        OrmError::Db(m) => err(500, "DatabaseError", m.to_string()),
    }
}

fn route(
    con: &Connection,
    app: &App,
    method: &str,
    url: &str,
    body: &str,
    auth_header: Option<&str>,
) -> (u16, Value) {
    let (segments, params) = util::parse_url(url);
    let ident = auth::resolve_user(con, auth_header, &app.default_user);

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
        return err(404, "NotFound", format!("Unknown path /{}", segments.join("/")));
    }

    match segments.get(1).map(|s| s.as_str()) {
        Some("method") => route_method(con, &ident, &segments, &params),
        Some("resource") => route_resource(con, app, &ident, method, &segments, &params, body),
        _ => err(404, "NotFound", format!("Unknown path /{}", segments.join("/"))),
    }
}

fn route_method(
    _con: &Connection,
    ident: &auth::Identity,
    segments: &[String],
    _params: &std::collections::HashMap<String, String>,
) -> (u16, Value) {
    let method_path = segments.get(2).map(|s| s.as_str()).unwrap_or("");
    match method_path {
        "ping" | "frappe.ping" => (200, json!({ "message": "pong" })),
        "frappe.auth.get_logged_user" => (200, json!({ "message": ident.user })),
        other => err(404, "NotFound", format!("Method '{other}' not implemented")),
    }
}

fn route_resource(
    con: &Connection,
    app: &App,
    ident: &auth::Identity,
    method: &str,
    segments: &[String],
    params: &std::collections::HashMap<String, String>,
    body: &str,
) -> (u16, Value) {
    let doctype = match segments.get(2) {
        Some(d) => d.clone(),
        None => return err(404, "NotFound", "No doctype in path".into()),
    };
    let name = segments.get(3).cloned();

    let meta = match app.metas.get(con, &doctype) {
        Ok(m) => m,
        Err(meta::MetaError::NotFound(d)) => {
            return err(404, "DoesNotExistError", format!("DocType {d} not found"))
        }
        Err(meta::MetaError::Db(e)) => return err(500, "DatabaseError", e.to_string()),
    };

    let ptype = auth::ptype_for_method(method);
    if !auth::can(con, &ident.user, &doctype, ptype) {
        return err(
            403,
            "PermissionError",
            format!("No '{ptype}' permission for {} on {doctype}", ident.user),
        );
    }

    match (method, name) {
        ("GET", None) => {
            let q = build_list_query(params);
            match orm::get_list(con, &meta, &q) {
                Ok(data) => (200, json!({ "data": data })),
                Err(e) => map_orm_err(e),
            }
        }
        ("GET", Some(n)) => match orm::get_doc(con, &meta, &n) {
            Ok(data) => (200, json!({ "data": data })),
            Err(e) => map_orm_err(e),
        },
        ("POST", None) => {
            let data = match parse_body_obj(body) {
                Ok(d) => d,
                Err(e) => return err(417, "ValidationError", e),
            };
            match orm::insert(con, &meta, &data, &ident.user) {
                Ok(doc) => (200, json!({ "data": doc })),
                Err(e) => map_orm_err(e),
            }
        }
        ("PUT", Some(n)) | ("PATCH", Some(n)) => {
            let data = match parse_body_obj(body) {
                Ok(d) => d,
                Err(e) => return err(417, "ValidationError", e),
            };
            match orm::update(con, &meta, &n, &data, &ident.user) {
                Ok(doc) => (200, json!({ "data": doc })),
                Err(e) => map_orm_err(e),
            }
        }
        ("DELETE", Some(n)) => match orm::delete(con, &meta, &n) {
            Ok(()) => (202, json!({ "message": "ok" })),
            Err(e) => map_orm_err(e),
        },
        (m, _) => err(405, "MethodNotAllowed", format!("{m} not allowed here")),
    }
}

fn build_list_query(params: &std::collections::HashMap<String, String>) -> ListQuery {
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
    if let Some(v) = params.get("limit_start").and_then(|s| s.parse::<i64>().ok()) {
        q.limit_start = v;
    }
    if let Some(v) = params
        .get("limit_page_length")
        .or_else(|| params.get("limit"))
        .and_then(|s| s.parse::<i64>().ok())
    {
        q.limit_page_length = v;
    }
    q
}

fn parse_body_obj(body: &str) -> Result<Map<String, Value>, String> {
    if body.trim().is_empty() {
        return Ok(Map::new());
    }
    match serde_json::from_str::<Value>(body) {
        Ok(Value::Object(m)) => Ok(m),
        Ok(_) => Err("request body must be a JSON object".into()),
        Err(e) => Err(format!("invalid JSON body: {e}")),
    }
}
