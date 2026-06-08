//! Desk mode — serve the Frappe **Desk** SPA (the admin UI) from the pure-Rust ferro runtime,
//! with no Python at all.
//!
//! Desk is a single-page app. Loading `/app` (which Frappe v17 redirects to `/desk`) returns an
//! HTML shell that embeds a large `frappe.boot` JSON blob and pulls the built JS/CSS bundles from
//! `/assets/...`. The SPA then drives itself through a set of whitelisted `/api/method/frappe.*`
//! endpoints (list data, form data, doctype meta, a handful of boot/toolbar calls).
//!
//! This module provides everything Desk needs that the bare REST runtime did not:
//!   * static asset serving from the built `sites/assets` tree,
//!   * the `/desk` HTML render (boot + csrf + bundle includes), with `/app` -> `/desk` redirect,
//!   * a bootinfo built from a captured baseline snapshot patched with live values,
//!   * the `frappe.desk.*` / `frappe.client.*` data methods, mapped onto ferro's ORM,
//!   * stubs for the boot/toolbar methods and a graceful 404 for socket.io (realtime degrades).
//!
//! The data plane (reads, perms, meta) is the same Rust code the REST API uses.

use rusqlite::Connection;
use serde_json::{json, Map, Value};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::meta::{Meta, MetaCache, MetaError};
use crate::orm::{self, ListQuery, OrmError, ReadAcl};
use crate::util;

/// A response that can carry HTML / binary bodies and custom headers — the API path only ever
/// needs JSON, but Desk needs to serve assets, the HTML shell, and Set-Cookie on login.
pub struct RawResp {
    pub status: u16,
    pub content_type: String,
    pub body: Vec<u8>,
    pub headers: Vec<(String, String)>,
}

impl RawResp {
    pub fn json(status: u16, v: &Value) -> RawResp {
        RawResp {
            status,
            content_type: "application/json; charset=utf-8".into(),
            body: serde_json::to_vec(v).unwrap_or_else(|_| b"{}".to_vec()),
            headers: Vec::new(),
        }
    }
    fn html(status: u16, body: String) -> RawResp {
        RawResp {
            status,
            content_type: "text/html; charset=utf-8".into(),
            body: body.into_bytes(),
            headers: Vec::new(),
        }
    }
    fn bytes(content_type: String, body: Vec<u8>) -> RawResp {
        RawResp { status: 200, content_type, body, headers: Vec::new() }
    }
    fn redirect(location: &str) -> RawResp {
        RawResp {
            status: 301,
            content_type: "text/html; charset=utf-8".into(),
            body: Vec::new(),
            headers: vec![("Location".into(), location.into())],
        }
    }
    fn not_found() -> RawResp {
        RawResp { status: 404, content_type: "text/plain".into(), body: b"Not Found".to_vec(), headers: Vec::new() }
    }
}

/// Desk runtime state: where assets live, the bundle map, and the boot baseline.
pub struct Desk {
    /// Root of the built asset tree (`<bench>/sites/assets`). `/assets/X` -> `<assets_dir>/X`.
    assets_dir: PathBuf,
    /// Baseline bootinfo snapshot (captured once from a real site), patched per request.
    boot_template: Value,
    /// Precomputed `<head>`/`<body>` HTML fragments for the shell.
    html_shell: HtmlShell,
}

struct HtmlShell {
    css_tags: String,
    js_tags: String,
    icon_tags: String,
    build_version: String,
}

// The app's stylesheet/script/icon include lists, from frappe/hooks.py (app_include_*).
const APP_INCLUDE_JS: &[&str] = &[
    "libs.bundle.js", "billing.bundle.js", "desk.bundle.js", "list.bundle.js",
    "form.bundle.js", "controls.bundle.js", "report.bundle.js", "telemetry.bundle.js",
];
const APP_INCLUDE_CSS: &[&str] = &["desk.bundle.css", "report.bundle.css"];
const APP_INCLUDE_ICONS: &[&str] = &[
    "/assets/frappe/icons/lucide/icons.svg",
    "/assets/frappe/icons/timeless/icons.svg",
    "/assets/frappe/icons/espresso/icons.svg",
    "/assets/frappe/icons/desktop_icons/alphabets.svg",
];

impl Desk {
    /// Build Desk state. `assets_dir` is `<bench>/sites/assets`. Falls back to vendored
    /// snapshots of `assets.json` and the bootinfo if the live files are unavailable.
    pub fn new(assets_dir: PathBuf, boot_override: Option<PathBuf>) -> Desk {
        // assets.json: prefer the live file (matches the bundles actually on disk), else vendored.
        let asset_map: HashMap<String, String> = std::fs::read_to_string(assets_dir.join("assets.json"))
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_else(|| {
                serde_json::from_str(include_str!("desk_assets.json")).unwrap_or_default()
            });

        let boot_template: Value = boot_override
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_else(|| {
                serde_json::from_str(include_str!("desk_boot.json")).unwrap_or(Value::Null)
            });

        let resolve = |name: &str| -> String {
            if name.starts_with("/assets") {
                name.to_string()
            } else {
                asset_map.get(name).cloned().unwrap_or_else(|| format!("/assets/frappe/dist/js/{name}"))
            }
        };

        let css_tags = APP_INCLUDE_CSS
            .iter()
            .map(|c| format!("<link type=\"text/css\" rel=\"stylesheet\" href=\"{}\">", resolve(c)))
            .collect::<Vec<_>>()
            .join("\n");
        let js_tags = APP_INCLUDE_JS
            .iter()
            .map(|c| format!("<script type=\"text/javascript\" src=\"{}\"></script>", resolve(c)))
            .collect::<Vec<_>>()
            .join("\n");
        // include_icons emits a fetch() that injects the SVG sprite into #all-symbols.
        let icon_tags = APP_INCLUDE_ICONS
            .iter()
            .map(|p| {
                format!(
                    "<script type=\"text/javascript\">fetch(`{p}?v=${{window._version_number}}`, \
                     {{credentials: \"same-origin\"}}).then((r) => r.text()).then((svg) => {{\
                     let c = document.getElementById(\"all-symbols\"); \
                     c.insertAdjacentHTML(\"beforeend\", svg);}});</script>"
                )
            })
            .collect::<Vec<_>>()
            .join("\n");

        Desk {
            assets_dir,
            boot_template,
            html_shell: HtmlShell {
                css_tags,
                js_tags,
                icon_tags,
                build_version: "ferro-desk".into(),
            },
        }
    }

    /// Build the bootinfo for `user`: clone the baseline and patch the live/dynamic bits.
    fn build_boot(&self, user: &str) -> Value {
        let mut boot = self.boot_template.clone();
        if let Value::Object(ref mut o) = boot {
            // Make sure the SPA does not drop into the setup wizard.
            if let Some(Value::Object(sd)) = o.get_mut("sysdefaults") {
                sd.insert("setup_complete".into(), json!("1"));
            }
            o.insert("setup_complete".into(), json!(1));
            o.insert("server_date".into(), json!(util::now_date()));
            o.insert("time_zone".into(), o.get("time_zone").cloned().unwrap_or(json!("UTC")));
            // A `home_page` of "setup-wizard" makes the initial route bounce to the (now complete)
            // setup wizard page, which redirects back -> infinite reload. Point it at the
            // "Workspaces" standard page: pageview.show("") loads it, and its show() then redirects
            // the empty route to the default workspace (and client-side "home" navigation works too).
            let cur = o.get("home_page").and_then(|h| h.as_str()).unwrap_or("");
            if cur.is_empty() || cur == "setup-wizard" {
                o.insert("home_page".into(), json!("Workspaces"));
            }
            if let Some(Value::Object(ad)) = o.get_mut("apps_data") {
                let dp = ad.get("default_path").and_then(|d| d.as_str()).unwrap_or("");
                if dp.is_empty() {
                    ad.insert("default_path".into(), json!("/app"));
                }
            }
            // Reflect the actual logged-in user in the boot (snapshot is Administrator).
            if user != "Administrator" {
                if let Some(Value::Object(u)) = o.get_mut("user") {
                    u.insert("name".into(), json!(user));
                }
            }
            o.insert("read_only".into(), json!(0));
        }
        boot
    }

    /// Render the `/desk` HTML shell with the boot blob + csrf token.
    fn render_html(&self, user: &str, csrf: &str) -> String {
        let boot = self.build_boot(user);
        let boot_json = serde_json::to_string(&boot).unwrap_or_else(|_| "{}".into());
        let s = &self.html_shell;
        format!(
            r##"<!DOCTYPE html>
<html data-theme-mode="light" data-theme="light" dir="ltr" lang="en">
<head>
<meta charset="UTF-8">
<meta name="theme-color" content="#0089FF">
<meta content="text/html;charset=utf-8" http-equiv="Content-Type">
<meta name="viewport" content="width=device-width, initial-scale=1.0, maximum-scale=1.0, minimum-scale=1.0, user-scalable=no, minimal-ui">
<meta name="apple-mobile-web-app-capable" content="yes">
<meta name="mobile-web-app-capable" content="yes">
<title>Frappe</title>
<link rel="shortcut icon" href="/assets/frappe/images/frappe-favicon.svg" type="image/x-icon">
<link rel="icon" href="/assets/frappe/images/frappe-favicon.svg" type="image/x-icon">
{css}
</head>
<body>
<div class="centered splash"><img src="/assets/frappe/images/frappe-framework-logo.svg" style="width: auto; max-width: 200px;"></div>
<div class="main-section"><header></header><div id="body"></div><footer></footer></div>
<div id="all-symbols" style="display:none"></div>
<div id="build-events-overlay"></div>
<script type="text/javascript">
window._version_number = "{ver}";
window.app = true;
window.dev_server = 0;
if (!window.frappe) window.frappe = {{}};
frappe.boot = {boot};
frappe._messages = {{}};
frappe.csrf_token = "{csrf}";
frappe._translations_loaded = fetch(`/api/method/frappe.translate.get_boot_translations?v=${{frappe.boot.translations_version}}&lang=${{frappe.boot.lang}}`, {{credentials: "same-origin", headers: {{"X-Frappe-CSRF-Token": frappe.csrf_token, "Accept": "application/json"}}}}).then(r => r.json()).then(data => {{ frappe._messages = data.message || {{}}; }}).catch(() => {{}});
</script>
{icons}
{js}
</body>
</html>"##,
            css = s.css_tags,
            ver = s.build_version,
            boot = boot_json,
            csrf = csrf,
            icons = s.icon_tags,
            js = s.js_tags,
        )
    }

    /// Serve a static file under `/assets/...`. Guards against path traversal.
    fn serve_asset(&self, url_path: &str) -> RawResp {
        // url_path is "/assets/frappe/dist/js/desk.bundle.XXXX.js"
        let rel = url_path.trim_start_matches("/assets/");
        // reject traversal
        if rel.split('/').any(|c| c == ".." || c == ".") {
            return RawResp::not_found();
        }
        let full = self.assets_dir.join(rel);
        match std::fs::read(&full) {
            Ok(bytes) => {
                let mut r = RawResp::bytes(mime_for(&full).to_string(), bytes);
                // assets are content-hashed; allow long caching
                r.headers.push(("Cache-Control".into(), "public, max-age=31536000".into()));
                r
            }
            Err(_) => RawResp::not_found(),
        }
    }
}

/// Try to handle a "raw" route (asset / HTML shell / socket.io / redirect). Returns None for
/// anything that should fall through to the JSON API router.
pub fn try_raw(desk: &Desk, user: &str, method: &str, url: &str) -> Option<RawResp> {
    let path = url.split('?').next().unwrap_or(url);

    // Static assets (the bulk of Desk's requests).
    if path.starts_with("/assets/") {
        return Some(desk.serve_asset(path));
    }

    // Frappe v17 serves the desk SPA at /desk and 301s /app -> /desk.
    if path == "/app" || path.starts_with("/app/") {
        let target = path.replacen("/app", "/desk", 1);
        return Some(RawResp::redirect(&target));
    }
    if path == "/desk" || path.starts_with("/desk/") {
        let csrf = util::random_name(); // demo CSRF token (not validated server-side)
        let _ = method;
        return Some(RawResp::html(200, desk.render_html(user, &csrf)));
    }

    // Realtime is not served; let it 404 quietly so Desk degrades to no-realtime.
    if path.starts_with("/socket.io") {
        return Some(RawResp::not_found());
    }

    // The favicon / framework logo etc. live under /assets already; bare "/" is the API banner.
    None
}

// ----------------------------------------------------------------- whitelisted method dispatch

/// Combined argument map for a method call: query params first, then form/JSON body overrides.
fn collect_args(params: &HashMap<String, String>, body: &str, content_type: Option<&str>) -> HashMap<String, String> {
    let mut args: HashMap<String, String> = params.clone();
    let trimmed = body.trim();
    if trimmed.is_empty() {
        return args;
    }
    let is_json = content_type.map(|c| c.contains("application/json")).unwrap_or(false)
        || trimmed.starts_with('{');
    if is_json {
        if let Ok(Value::Object(m)) = serde_json::from_str::<Value>(trimmed) {
            for (k, v) in m {
                let s = match v {
                    Value::String(s) => s,
                    other => other.to_string(),
                };
                args.insert(k, s);
            }
        }
    } else {
        for pair in trimmed.split('&') {
            if pair.is_empty() {
                continue;
            }
            let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
            args.insert(util::percent_decode(k, true), util::percent_decode(v, true));
        }
    }
    args
}

/// Wrap a method result in Frappe's `{"message": ...}` envelope.
fn message(v: Value) -> (u16, Value) {
    (200, json!({ "message": v }))
}

/// Dispatch a `frappe.*` whitelisted method used by Desk. Returns None if the method is not a
/// Desk method (so the caller can fall back to the builtin ping/get_logged_user handlers).
#[allow(clippy::too_many_arguments)]
pub fn route_method(
    con: &Connection,
    metas: &MetaCache,
    user: &str,
    name: &str,
    params: &HashMap<String, String>,
    body: &str,
    content_type: Option<&str>,
    http_method: &str,
) -> Option<(u16, Value)> {
    let args = collect_args(params, body, content_type);

    let result: (u16, Value) = match name {
        // ---- login / session (cookies are set by the caller) ----
        "login" => (200, json!({
            "message": "Logged In",
            "home_page": "/app",
            "full_name": user,
        })),
        "logout" | "frappe.auth.logout" => message(Value::Null),

        // ---- boot-time / toolbar methods (stubs are sufficient for Desk to run) ----
        "frappe.translate.get_boot_translations" => message(json!({})),
        "frappe.apps.get_apps" => message(json!([])),
        "frappe.core.doctype.session_default_settings.session_default_settings.get_session_default_values" => {
            message(json!("[]"))
        }
        "frappe.core.doctype.background_task.background_task.get_recent_tasks" => message(json!([])),
        "frappe.desk.notifications.get_open_count" => {
            message(json!({"open_count_doctype": {}, "open_count_other": {}, "targets": {}}))
        }
        "frappe.desk.notifications.get_notifications" => message(json!({
            "open_count_doctype": {}, "open_count_other": {}, "targets": {},
            "new_messages": [], "notification_count": {}, "notification_percent": {}
        })),
        "frappe.core.doctype.user.user.get_all_roles" => message(get_all_roles(con)),
        "frappe.core.doctype.user.user.get_timezones" => message(json!({"timezones": TIMEZONES})),
        "frappe.client.get_count" | "frappe.desk.reportview.get_count" => {
            method_get_count(con, metas, &args)
        }

        // ---- list view ----
        "frappe.desk.reportview.get" => method_reportview_get(con, metas, user, &args),
        "frappe.desk.reportview.get_list" | "frappe.client.get_list" => {
            method_get_list(con, metas, user, &args)
        }
        "frappe.desk.listview.get_list_settings"
        | "frappe.desk.doctype.list_view_settings.list_view_settings.get" => (200, json!({})),
        "frappe.desk.listview.get_group_by_count" => message(json!([])),
        "frappe.model.utils.user_settings.get" => message(json!("{}")),
        "frappe.model.utils.user_settings.save" => message(Value::Null),

        // ---- dashboards (workspaces with charts) ----
        "frappe.desk.doctype.dashboard_settings.dashboard_settings.get_dashboard_settings" => message(Value::Null),
        "frappe.desk.doctype.dashboard_settings.dashboard_settings.create_dashboard_settings" => {
            message(json!({ "name": user, "user": user, "chart_config": "{}" }))
        }
        "frappe.desk.doctype.number_card.number_card.get_result" => method_number_card_result(con, metas, &args),
        "frappe.desk.doctype.number_card.number_card.get_percentage_difference" => message(Value::Null),
        "frappe.desk.doctype.dashboard_chart.dashboard_chart.get"
        | "frappe.desk.doctype.dashboard_chart.dashboard_chart.get_data" => message(json!({ "labels": [], "datasets": [] })),

        // ---- workspace (desktop) page content ----
        "frappe.desk.desktop.get_desktop_page" => method_get_desktop_page(con, &args),
        "frappe.desk.desktop.get_workspace_sidebar_items" => method_workspace_sidebar(con),
        "frappe.desk.desk_page.getpage" => method_getpage(con, &args),

        // ---- form view ----
        "frappe.desk.form.load.getdoctype" => method_getdoctype(con, metas, &args),
        "frappe.desk.form.load.getdoc" => method_getdoc(con, metas, user, &args),
        "frappe.desk.form.save.savedocs" => method_savedocs(con, metas, user, &args),
        "frappe.client.save" | "frappe.client.insert" => method_client_save(con, metas, user, &args),
        "frappe.client.get" => method_client_get(con, metas, user, &args),
        "frappe.client.get_value" => method_get_value(con, metas, &args),
        "frappe.client.get_single_value" => method_get_single_value(con, metas, &args),
        "frappe.desk.form.load.get_docinfo" => message(empty_docinfo()),

        _ => {
            let _ = http_method;
            return None;
        }
    };
    Some(result)
}

// ----------------------------------------------------------------- method implementations

fn get_meta(metas: &MetaCache, con: &Connection, doctype: &str) -> Result<Arc<Meta>, (u16, Value)> {
    metas.get(con, doctype).map_err(|e| match e {
        MetaError::NotFound(d) => (404, json!({"exc_type": "DoesNotExistError", "message": format!("DocType {d} not found")})),
        MetaError::Db(e) => (500, json!({"exc_type": "DatabaseError", "message": e.to_string()})),
    })
}

/// Strip a `` `tabDocType`.`field` `` qualifier (or `field as alias`) down to the bare fieldname.
fn bare_field(raw: &str) -> String {
    let mut s = raw.trim();
    // drop "as alias"
    if let Some(idx) = s.to_lowercase().find(" as ") {
        s = s[..idx].trim();
    }
    // take the part after the last '.'
    let s = s.rsplit('.').next().unwrap_or(s);
    s.trim().trim_matches('`').trim_matches('"').to_string()
}

fn parse_fields_arg(raw: Option<&String>) -> Vec<String> {
    match raw {
        Some(s) => {
            if let Ok(Value::Array(a)) = serde_json::from_str::<Value>(s) {
                a.iter().filter_map(|v| v.as_str().map(bare_field)).collect()
            } else {
                vec![]
            }
        }
        None => vec![],
    }
}

fn build_query(meta: &Meta, args: &HashMap<String, String>) -> ListQuery {
    let mut q = ListQuery::default();
    // fields: keep only those that are real columns on this doctype (drop computed/_seen etc.)
    let requested = parse_fields_arg(args.get("fields"));
    let mut fields: Vec<String> = requested.into_iter().filter(|f| meta.has_column(f)).collect();
    if !fields.iter().any(|f| f == "name") {
        fields.insert(0, "name".to_string());
    }
    if !fields.is_empty() {
        q.fields = fields;
    }
    if let Some(f) = args.get("filters") {
        if let Ok(v) = serde_json::from_str::<Value>(f) {
            q.filters = v;
        }
    }
    if let Some(f) = args.get("or_filters") {
        if let Ok(v) = serde_json::from_str::<Value>(f) {
            q.or_filters = v;
        }
    }
    if let Some(o) = args.get("order_by").filter(|s| !s.is_empty()) {
        // strip table qualifiers from "`tabToDo`.`modified` desc"
        let parts: Vec<String> = o
            .split(',')
            .map(|seg| {
                let seg = seg.trim();
                let (col, dir) = match seg.rsplit_once(' ') {
                    Some((c, d)) if matches!(d.to_lowercase().as_str(), "asc" | "desc") => (c, d),
                    _ => (seg, ""),
                };
                let bare = bare_field(col);
                if dir.is_empty() { bare } else { format!("{bare} {dir}") }
            })
            .collect();
        q.order_by = Some(parts.join(", "));
    }
    if let Some(v) = args.get("limit_start").or_else(|| args.get("start")).and_then(|s| s.parse().ok()) {
        q.limit_start = v;
    }
    if let Some(v) = args
        .get("limit_page_length")
        .or_else(|| args.get("page_length"))
        .or_else(|| args.get("limit"))
        .and_then(|s| s.parse().ok())
    {
        q.limit_page_length = v;
    }
    q
}

fn map_orm_err(e: OrmError) -> (u16, Value) {
    match e {
        OrmError::NotFound(m) => (404, json!({"exc_type": "DoesNotExistError", "message": m})),
        OrmError::Validation(m) => (417, json!({"exc_type": "ValidationError", "message": m})),
        OrmError::Duplicate(m) => (409, json!({"exc_type": "DuplicateEntryError", "message": m})),
        OrmError::Db(e) => (500, json!({"exc_type": "DatabaseError", "message": e.to_string()})),
    }
}

/// `frappe.desk.reportview.get` — list view data in Frappe's compressed `{keys, values}` form.
fn method_reportview_get(con: &Connection, metas: &MetaCache, _user: &str, args: &HashMap<String, String>) -> (u16, Value) {
    let doctype = match args.get("doctype") {
        Some(d) => d.clone(),
        None => return (417, json!({"exc_type": "ValidationError", "message": "doctype required"})),
    };
    let meta = match get_meta(metas, con, &doctype) {
        Ok(m) => m,
        Err(e) => return e,
    };
    let q = build_query(&meta, args);
    let keys = q.fields.clone();
    let acl = ReadAcl::all();
    match orm::get_list(con, &meta, &acl, &q, None) {
        Ok(Value::Array(rows)) => {
            // project each row object into a values array in `keys` order
            let values: Vec<Value> = rows
                .iter()
                .map(|row| {
                    Value::Array(
                        keys.iter()
                            .map(|k| row.get(k).cloned().unwrap_or(Value::Null))
                            .collect(),
                    )
                })
                .collect();
            message(json!({ "keys": keys, "values": values, "user_info": {} }))
        }
        Ok(other) => message(other),
        Err(e) => map_orm_err(e),
    }
}

/// `frappe.desk.reportview.get_list` / `frappe.client.get_list` — list of dicts.
fn method_get_list(con: &Connection, metas: &MetaCache, _user: &str, args: &HashMap<String, String>) -> (u16, Value) {
    let doctype = match args.get("doctype") {
        Some(d) => d.clone(),
        None => return (417, json!({"exc_type": "ValidationError", "message": "doctype required"})),
    };
    let meta = match get_meta(metas, con, &doctype) {
        Ok(m) => m,
        Err(e) => return e,
    };
    let q = build_query(&meta, args);
    let acl = ReadAcl::all();
    match orm::get_list(con, &meta, &acl, &q, None) {
        Ok(v) => message(v),
        Err(e) => map_orm_err(e),
    }
}

/// `frappe.client.get_count` / `frappe.desk.reportview.get_count`.
fn method_get_count(con: &Connection, metas: &MetaCache, args: &HashMap<String, String>) -> (u16, Value) {
    let doctype = match args.get("doctype") {
        Some(d) => d.clone(),
        None => return (417, json!({"exc_type": "ValidationError", "message": "doctype required"})),
    };
    let meta = match get_meta(metas, con, &doctype) {
        Ok(m) => m,
        Err(e) => return e,
    };
    let filters = args
        .get("filters")
        .and_then(|f| serde_json::from_str::<Value>(f).ok())
        .unwrap_or(Value::Null);
    match orm::count(con, &meta, &filters) {
        Ok(n) => message(json!(n)),
        Err(e) => map_orm_err(e),
    }
}

/// `frappe.desk.doctype.number_card.number_card.get_result` — the value shown on a workspace
/// number card. We support the common "Count" function (document_type + filters -> row count).
fn method_number_card_result(con: &Connection, metas: &MetaCache, args: &HashMap<String, String>) -> (u16, Value) {
    let card = args.get("doc").and_then(|d| serde_json::from_str::<Value>(d).ok()).unwrap_or(Value::Null);
    let doctype = card
        .get("document_type")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .or_else(|| args.get("document_type").cloned());
    let doctype = match doctype {
        Some(d) if !d.is_empty() => d,
        _ => return message(json!(0)),
    };
    let meta = match get_meta(metas, con, &doctype) {
        Ok(m) => m,
        Err(_) => return message(json!(0)),
    };
    // filters: prefer the card's filters_json, fall back to the explicit `filters` arg.
    let filters = card
        .get("filters_json")
        .and_then(|v| v.as_str())
        .and_then(|s| serde_json::from_str::<Value>(s).ok())
        .or_else(|| args.get("filters").and_then(|s| serde_json::from_str::<Value>(s).ok()))
        .unwrap_or(Value::Null);
    match orm::count(con, &meta, &filters) {
        Ok(n) => message(json!(n)),
        Err(_) => message(json!(0)),
    }
}

/// `frappe.desk.form.load.getdoctype` — the doctype meta the form renderer needs.
fn method_getdoctype(con: &Connection, metas: &MetaCache, args: &HashMap<String, String>) -> (u16, Value) {
    let doctype = match args.get("doctype") {
        Some(d) => d.clone(),
        None => return (417, json!({"exc_type": "ValidationError", "message": "doctype required"})),
    };
    // ensure it exists / is cacheable
    if let Err(e) = get_meta(metas, con, &doctype) {
        return e;
    }
    // Build a meta bundle: the DocType plus the meta of every child-table doctype its fields
    // reference (Table / Table MultiSelect). The form's grid + Table MultiSelect controls need
    // these or rendering aborts ("Table MultiSelect requires a Table with atleast one Link field").
    let main = match build_meta_doc(con, &doctype) {
        Ok(d) => d,
        Err(e) => return map_orm_err(e),
    };
    let mut docs = vec![main.clone()];
    let mut seen: HashSet<String> = HashSet::new();
    seen.insert(doctype.clone());
    if let Some(fields) = main.get("fields").and_then(|f| f.as_array()) {
        for f in fields {
            let ft = f.get("fieldtype").and_then(|v| v.as_str()).unwrap_or("");
            if ft == "Table" || ft == "Table MultiSelect" {
                if let Some(child) = f.get("options").and_then(|v| v.as_str()) {
                    if !child.is_empty() && seen.insert(child.to_string()) {
                        if let Ok(cdoc) = build_meta_doc(con, child) {
                            docs.push(cdoc);
                        }
                    }
                }
            }
        }
    }
    (200, json!({ "docs": docs, "user_settings": "{}" }))
}

/// `frappe.desk.form.load.getdoc` — a single document plus (empty) docinfo.
fn method_getdoc(con: &Connection, metas: &MetaCache, _user: &str, args: &HashMap<String, String>) -> (u16, Value) {
    let doctype = match args.get("doctype") {
        Some(d) => d.clone(),
        None => return (417, json!({"exc_type": "ValidationError", "message": "doctype required"})),
    };
    let name = match args.get("name") {
        Some(n) => n.clone(),
        None => return (417, json!({"exc_type": "ValidationError", "message": "name required"})),
    };
    let meta = match get_meta(metas, con, &doctype) {
        Ok(m) => m,
        Err(e) => return e,
    };
    let acl = ReadAcl::all();
    match orm::get_doc(con, &meta, &acl, &name) {
        Ok(mut doc) => {
            if let Value::Object(ref mut o) = doc {
                o.insert("doctype".into(), json!(doctype));
            }
            // docinfo MUST carry doctype+name: frappe.model.sync_docinfo keys
            // frappe.model.docinfo[doctype][name] off them (the form timeline reads it back).
            let mut docinfo = empty_docinfo();
            if let Value::Object(ref mut o) = docinfo {
                o.insert("doctype".into(), json!(doctype));
                o.insert("name".into(), json!(name));
                o.insert("user_info".into(), json!({}));
            }
            (200, json!({ "docs": [doc], "docinfo": docinfo, "_link_titles": {} }))
        }
        // A missing doc (the form's "new-<doctype>-xxx" placeholder) must return an empty list with
        // HTTP 200 — matching frappe's getdoc — so the new-document form proceeds instead of erroring.
        Err(OrmError::NotFound(_)) => message(json!([])),
        Err(e) => map_orm_err(e),
    }
}

/// `frappe.desk.form.save.savedocs` — persist a document edited in a Desk form.
/// Maps to `orm::insert` (new docs) / `orm::update` (existing). NOTE: the pure-Rust ORM persists
/// the parent doc's own fields; child-table rows are not written here (pure-CRUD scope).
fn method_savedocs(con: &Connection, metas: &MetaCache, user: &str, args: &HashMap<String, String>) -> (u16, Value) {
    let doc: Value = match args.get("doc").and_then(|d| serde_json::from_str(d).ok()) {
        Some(v) => v,
        None => return (417, json!({"exc_type": "ValidationError", "message": "doc required"})),
    };
    persist_doc(con, metas, user, &doc)
}

/// `frappe.client.save` / `frappe.client.insert` — same idea, doc passed as `doc`.
fn method_client_save(con: &Connection, metas: &MetaCache, user: &str, args: &HashMap<String, String>) -> (u16, Value) {
    let doc: Value = match args.get("doc").and_then(|d| serde_json::from_str(d).ok()) {
        Some(v) => v,
        None => return (417, json!({"exc_type": "ValidationError", "message": "doc required"})),
    };
    match persist_one(con, metas, user, &doc) {
        Ok(saved) => message(saved),
        Err(e) => e,
    }
}

/// Insert/update a single doc object; returns the saved doc or an error envelope tuple.
fn persist_one(con: &Connection, metas: &MetaCache, user: &str, doc: &Value) -> Result<Value, (u16, Value)> {
    let obj = doc.as_object().ok_or((417, json!({"exc_type": "ValidationError", "message": "doc must be an object"})))?;
    let doctype = obj.get("doctype").and_then(|v| v.as_str()).ok_or((417, json!({"exc_type": "ValidationError", "message": "doctype required"})))?.to_string();
    let meta = get_meta(metas, con, &doctype)?;
    let acl = ReadAcl::all();
    let name = obj.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let is_new = obj.get("__islocal").map(is_truthy).unwrap_or(false)
        || name.is_empty()
        || name.starts_with("new-");
    let mut data: Map<String, Value> = obj.clone();
    let result = if is_new {
        // Drop the client's temporary "new-<doctype>-xxx" name so naming autogenerates a real one.
        data.remove("name");
        orm::insert(con, &meta, &acl, &data, user)
    } else {
        orm::update(con, &meta, &acl, &name, &data, user)
    };
    result.map_err(map_orm_err)
}

fn persist_doc(con: &Connection, metas: &MetaCache, user: &str, doc: &Value) -> (u16, Value) {
    // The client's temporary name, if this is a new doc — echoed back as `localname` so
    // frappe.model.sync renames the local form doc to the server-assigned name.
    let temp_name = doc.get("name").and_then(|v| v.as_str()).unwrap_or("");
    let was_new = doc.get("__islocal").map(is_truthy).unwrap_or(false)
        || temp_name.is_empty()
        || temp_name.starts_with("new-");
    let localname = if was_new && !temp_name.is_empty() { Some(temp_name.to_string()) } else { None };
    match persist_one(con, metas, user, doc) {
        Ok(mut saved) => {
            let doctype = doc.get("doctype").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let name = saved.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string();
            if let Value::Object(ref mut o) = saved {
                o.insert("doctype".into(), json!(doctype));
                o.insert("__islocal".into(), json!(0));
                o.insert("__unsaved".into(), json!(0));
                if let Some(ln) = &localname {
                    o.insert("localname".into(), json!(ln));
                }
            }
            let mut docinfo = empty_docinfo();
            if let Value::Object(ref mut o) = docinfo {
                o.insert("doctype".into(), json!(doctype));
                o.insert("name".into(), json!(name));
                o.insert("user_info".into(), json!({}));
            }
            let server_messages = serde_json::to_string(&vec![
                serde_json::to_string(&json!({"message": "Saved", "title": "Message", "indicator": "green"})).unwrap_or_default()
            ]).unwrap_or_default();
            (200, json!({ "docs": [saved], "docinfo": docinfo, "_link_titles": {}, "_server_messages": server_messages }))
        }
        Err(e) => e,
    }
}

fn is_truthy(v: &Value) -> bool {
    match v {
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().map(|f| f != 0.0).unwrap_or(false),
        Value::String(s) => !s.is_empty() && s != "0" && s != "false",
        _ => false,
    }
}

/// `frappe.client.get` — one document (no docinfo wrapper).
fn method_client_get(con: &Connection, metas: &MetaCache, _user: &str, args: &HashMap<String, String>) -> (u16, Value) {
    let doctype = match args.get("doctype") {
        Some(d) => d.clone(),
        None => return (417, json!({"exc_type": "ValidationError", "message": "doctype required"})),
    };
    let meta = match get_meta(metas, con, &doctype) {
        Ok(m) => m,
        Err(e) => return e,
    };
    let name = args.get("name").cloned().unwrap_or_else(|| doctype.clone());
    let acl = ReadAcl::all();
    match orm::get_doc(con, &meta, &acl, &name) {
        Ok(doc) => message(doc),
        Err(e) => map_orm_err(e),
    }
}

/// `frappe.client.get_value` — selected fields of the first matching doc.
fn method_get_value(con: &Connection, metas: &MetaCache, args: &HashMap<String, String>) -> (u16, Value) {
    let doctype = match args.get("doctype") {
        Some(d) => d.clone(),
        None => return (417, json!({"exc_type": "ValidationError", "message": "doctype required"})),
    };
    let meta = match get_meta(metas, con, &doctype) {
        Ok(m) => m,
        Err(e) => return e,
    };
    let mut q = build_query(&meta, args);
    if let Some(fieldname) = args.get("fieldname") {
        let f = bare_field(fieldname);
        if meta.has_column(&f) {
            q.fields = vec![f];
        }
    }
    q.limit_page_length = 1;
    let acl = ReadAcl::all();
    match orm::get_list(con, &meta, &acl, &q, None) {
        Ok(Value::Array(rows)) => message(rows.into_iter().next().unwrap_or(json!({}))),
        Ok(v) => message(v),
        Err(e) => map_orm_err(e),
    }
}

/// `frappe.client.get_single_value` — one field of a Single doctype.
fn method_get_single_value(con: &Connection, metas: &MetaCache, args: &HashMap<String, String>) -> (u16, Value) {
    let doctype = match args.get("doctype") {
        Some(d) => d.clone(),
        None => return (417, json!({"exc_type": "ValidationError", "message": "doctype required"})),
    };
    let field = args.get("field").cloned().unwrap_or_default();
    // tabSingles stores single-doctype values as (doctype, field, value) rows.
    let val: Option<String> = con
        .query_row(
            "SELECT value FROM \"tabSingles\" WHERE doctype = ?1 AND field = ?2",
            rusqlite::params![doctype, field],
            |r| r.get::<_, Option<String>>(0),
        )
        .ok()
        .flatten();
    let _ = metas;
    match val {
        Some(v) => message(Value::from(v)),
        None => message(json!(0)),
    }
}

/// `frappe.desk.desktop.get_desktop_page` — resolve a workspace's widgets (cards/charts/etc).
fn method_get_desktop_page(con: &Connection, args: &HashMap<String, String>) -> (u16, Value) {
    // `page` is a JSON object describing the workspace; we only need its name.
    let name = args
        .get("page")
        .and_then(|p| serde_json::from_str::<Value>(p).ok())
        .and_then(|v| v.get("name").and_then(|n| n.as_str()).map(|s| s.to_string()))
        .or_else(|| args.get("name").cloned())
        .unwrap_or_default();

    let items = |table: &str| -> Value {
        json!({ "items": child_rows(con, table, &name).unwrap_or(json!([])) })
    };
    (200, json!({ "message": {
        "charts": items("tabWorkspace Chart"),
        "shortcuts": items("tabWorkspace Shortcut"),
        "cards": { "items": build_card_groups(con, &name) },
        "onboardings": { "items": [] },
        "quick_lists": items("tabWorkspace Quick List"),
        "number_cards": items("tabWorkspace Number Card"),
        "custom_blocks": items("tabWorkspace Custom Block"),
    }}))
}

/// Group a workspace's `Workspace Link` rows into cards: each "Card Break" row starts a card and
/// the following non-break links become its `links` (mirrors Workspace.get_link_groups).
fn build_card_groups(con: &Connection, workspace: &str) -> Value {
    let links = match child_rows(con, "tabWorkspace Link", workspace) {
        Ok(Value::Array(a)) => a,
        _ => return json!([]),
    };
    let mut cards: Vec<Value> = Vec::new();
    let mut current: Option<Map<String, Value>> = None;
    let mut card_links: Vec<Value> = Vec::new();
    let flush = |cards: &mut Vec<Value>, cur: &mut Option<Map<String, Value>>, links: &mut Vec<Value>| {
        if let Some(mut c) = cur.take() {
            if !links.is_empty() {
                c.insert("links".into(), Value::Array(std::mem::take(links)));
                cards.push(Value::Object(c));
            }
        }
        links.clear();
    };
    for link in links {
        let ty = link.get("type").and_then(|v| v.as_str()).unwrap_or("");
        if ty == "Card Break" {
            flush(&mut cards, &mut current, &mut card_links);
            current = link.as_object().cloned();
        } else if current.is_some() {
            card_links.push(link);
        }
    }
    flush(&mut cards, &mut current, &mut card_links);
    Value::Array(cards)
}

/// `frappe.desk.desktop.get_workspace_sidebar_items` — the list of workspace pages for the sidebar.
fn method_workspace_sidebar(con: &Connection) -> (u16, Value) {
    let pages = child_rows_query(
        con,
        "SELECT * FROM \"tabWorkspace\" WHERE COALESCE(public,0)=1 AND COALESCE(is_hidden,0)=0 ORDER BY sequence_id, name",
    )
    .unwrap_or(json!([]));
    message(json!({ "pages": pages, "has_access": true, "has_create_access": true }))
}

/// `frappe.desk.desk_page.getpage` — a single workspace page document.
fn method_getpage(con: &Connection, args: &HashMap<String, String>) -> (u16, Value) {
    let name = args.get("name").cloned().unwrap_or_default();
    match row_as_object(con, "tabWorkspace", "name", &name) {
        Ok(Some(doc)) => message(doc),
        _ => message(json!({})),
    }
}

/// Run a SELECT with no params and return the rows as a JSON array of objects.
fn child_rows_query(con: &Connection, sql: &str) -> Result<Value, OrmError> {
    let mut stmt = con.prepare(sql).map_err(OrmError::Db)?;
    let cols: Vec<String> = stmt.column_names().iter().map(|s| s.to_string()).collect();
    let mut rows = stmt.query([]).map_err(OrmError::Db)?;
    let mut out = Vec::new();
    while let Some(r) = rows.next().map_err(OrmError::Db)? {
        out.push(sqlite_row_to_object(r, &cols));
    }
    Ok(Value::Array(out))
}

/// Assemble the full DocType meta doc (tabDocType row + its fields + permissions) the form needs.
fn build_meta_doc(con: &Connection, doctype: &str) -> Result<Value, OrmError> {
    let dt = row_as_object(con, "tabDocType", "name", doctype)?
        .ok_or_else(|| OrmError::NotFound(format!("DocType {doctype} not found")))?;
    let mut doc = dt;
    if let Value::Object(ref mut o) = doc {
        o.insert("doctype".into(), json!("DocType"));
        o.insert("fields".into(), tagged_child_rows(con, "tabDocField", doctype, "DocField")?);
        o.insert("permissions".into(), tagged_child_rows(con, "tabDocPerm", doctype, "DocPerm")?);
        o.insert("actions".into(), tagged_child_rows(con, "tabDocType Action", doctype, "DocType Action").unwrap_or(json!([])));
        o.insert("links".into(), tagged_child_rows(con, "tabDocType Link", doctype, "DocType Link").unwrap_or(json!([])));
        o.insert("states".into(), tagged_child_rows(con, "tabDocType State", doctype, "DocType State").unwrap_or(json!([])));
        o.insert("__js".into(), Value::Null);
        o.insert("__css".into(), Value::Null);
    }
    Ok(doc)
}

/// Read a single row from `table` where `key = val`, as a JSON object keyed by column name.
fn row_as_object(con: &Connection, table: &str, key: &str, val: &str) -> Result<Option<Value>, OrmError> {
    let sql = format!("SELECT * FROM \"{table}\" WHERE \"{key}\" = ?1 LIMIT 1");
    let mut stmt = con.prepare(&sql).map_err(OrmError::Db)?;
    let cols: Vec<String> = stmt.column_names().iter().map(|s| s.to_string()).collect();
    let mut rows = stmt.query(rusqlite::params![val]).map_err(OrmError::Db)?;
    if let Some(r) = rows.next().map_err(OrmError::Db)? {
        Ok(Some(sqlite_row_to_object(r, &cols)))
    } else {
        Ok(None)
    }
}

/// Like `child_rows`, but stamps each row with `doctype` (e.g. "DocField") as Frappe's as_dict does.
fn tagged_child_rows(con: &Connection, table: &str, parent: &str, child_dt: &str) -> Result<Value, OrmError> {
    let rows = child_rows(con, table, parent)?;
    if let Value::Array(arr) = rows {
        Ok(Value::Array(
            arr.into_iter()
                .map(|mut r| {
                    if let Value::Object(ref mut o) = r {
                        o.insert("doctype".into(), json!(child_dt));
                    }
                    r
                })
                .collect(),
        ))
    } else {
        Ok(json!([]))
    }
}

/// Read all child rows from `table` where parent = `parent`, ordered by idx, as a JSON array.
fn child_rows(con: &Connection, table: &str, parent: &str) -> Result<Value, OrmError> {
    let sql = format!("SELECT * FROM \"{table}\" WHERE parent = ?1 ORDER BY idx");
    let mut stmt = con.prepare(&sql).map_err(OrmError::Db)?;
    let cols: Vec<String> = stmt.column_names().iter().map(|s| s.to_string()).collect();
    let mut rows = stmt.query(rusqlite::params![parent]).map_err(OrmError::Db)?;
    let mut out = Vec::new();
    while let Some(r) = rows.next().map_err(OrmError::Db)? {
        out.push(sqlite_row_to_object(r, &cols));
    }
    Ok(Value::Array(out))
}

/// Convert a rusqlite row into a JSON object using the given column names.
fn sqlite_row_to_object(r: &rusqlite::Row, cols: &[String]) -> Value {
    let mut o = Map::new();
    for (i, name) in cols.iter().enumerate() {
        let v: Value = match r.get_ref(i) {
            Ok(rusqlite::types::ValueRef::Null) => Value::Null,
            Ok(rusqlite::types::ValueRef::Integer(n)) => Value::from(n),
            Ok(rusqlite::types::ValueRef::Real(f)) => Value::from(f),
            Ok(rusqlite::types::ValueRef::Text(t)) => Value::from(String::from_utf8_lossy(t).into_owned()),
            Ok(rusqlite::types::ValueRef::Blob(b)) => Value::from(util::b64(b)),
            Err(_) => Value::Null,
        };
        o.insert(name.clone(), v);
    }
    Value::Object(o)
}

/// All distinct role names (for the toolbar's role list).
fn get_all_roles(con: &Connection) -> Value {
    let mut roles = Vec::new();
    if let Ok(mut stmt) = con.prepare("SELECT name FROM \"tabRole\" WHERE disabled = 0 ORDER BY name") {
        if let Ok(rows) = stmt.query_map([], |r| r.get::<_, String>(0)) {
            for r in rows.flatten() {
                roles.push(Value::from(r));
            }
        }
    }
    Value::Array(roles)
}

/// An empty-but-well-formed docinfo so the form's sidebar/timeline render without crashing.
fn empty_docinfo() -> Value {
    json!({
        "attachments": [], "communications": [], "automated_messages": [], "comments": [],
        "total_comments": 0, "versions": [], "assignments": [], "permissions": {},
        "shared": [], "views": [], "energy_point_logs": [], "milestones": [],
        "is_document_followed": false, "tags": "", "document_email": null,
        "rating": 0, "additional_timeline_content": [], "since_last_visit": null
    })
}

fn mime_for(path: &Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()).unwrap_or("") {
        "js" | "mjs" => "application/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" | "map" => "application/json; charset=utf-8",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "ico" => "image/x-icon",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "ttf" => "font/ttf",
        "eot" => "application/vnd.ms-fontobject",
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        "html" => "text/html; charset=utf-8",
        "txt" => "text/plain; charset=utf-8",
        "wasm" => "application/wasm",
        _ => "application/octet-stream",
    }
}

// A static IANA timezone list (subset is fine; Desk only needs a non-empty list for the dropdown).
const TIMEZONES: &[&str] = &[
    "Africa/Cairo", "Africa/Johannesburg", "Africa/Lagos", "America/Chicago", "America/Denver",
    "America/Los_Angeles", "America/New_York", "America/Sao_Paulo", "America/Toronto",
    "Asia/Dubai", "Asia/Hong_Kong", "Asia/Kolkata", "Asia/Shanghai", "Asia/Singapore",
    "Asia/Tokyo", "Australia/Sydney", "Europe/Amsterdam", "Europe/Berlin", "Europe/London",
    "Europe/Moscow", "Europe/Paris", "Pacific/Auckland", "UTC",
];
