//! SPA mode — serve the frappe-ui app frontends (Frappe CRM, Gameplan, Helpdesk, …) from the
//! pure-Rust ferro runtime, at the routes their `hooks.py` declares — mirroring what Frappe's
//! website router does by default.
//!
//! Unlike the admin **Desk** (one SPA at `/app` → `/desk`, handled by `desk.rs`), these apps each
//! mount their own frappe-ui SPA on a separate path. The framework wires that up through
//! `website_route_rules` in the app's `hooks.py`, e.g.
//!
//! ```python
//! website_route_rules = [{"from_route": "/crm/<path:app_path>", "to_route": "crm"}]
//! ```
//!
//! A `bench build` then writes the built SPA shell to `<app>/<app>/www/<to_route>.html` (the vite
//! `indexHtmlPath`) with the hashed bundle `<script>`/`<link>` tags already injected, pointing at
//! `/assets/<app>/...`. Frappe serves that shell (rendered through its www `get_context`) for the
//! base route *and* every `<path:app_path>` sub-route; the SPA then client-routes and drives itself
//! through `/api/...`.
//!
//! This module reproduces that behaviour without Python:
//!   * discover each *installed* app's `website_route_rules` / `website_redirects` from `hooks.py`,
//!   * locate and pre-render its built www shell (the small set of Jinja placeholders these
//!     templates use — `boot`, `site_name`, `csrf_token`, `boot.lang`/`boot.dir` — filled in),
//!   * serve that shell for the base route and any sub-path, and emit the redirects as 301s.
//!
//! Asset requests (`/assets/<app>/...`) are already served by the Desk asset handler, since the
//! built frontend lives in the same `sites/assets` tree.

use crate::desk::RawResp;
use crate::util;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

/// A discovered SPA route: a base path (`/crm`) and its pre-rendered shell HTML.
struct SpaRoute {
    base: String,
    shell: Vec<u8>,
}

/// A `website_redirects` rule reduced to a literal prefix swap (`/teams` -> `/g`).
struct Redirect {
    prefix: String,
    target: String,
}

/// The SPA serving table for one site: the app routes (longest base first) and the redirects.
pub struct Spa {
    routes: Vec<SpaRoute>,
    redirects: Vec<Redirect>,
}

impl Spa {
    /// Discover SPA routes for the apps installed on this site. `apps_dir` is the bench/forge
    /// `apps/` directory; `installed_apps` gates which apps we serve (the forge symlinks *all*
    /// apps via the shared mirror, so we must not serve a route for an app that isn't installed).
    /// Returns None when no installed app ships a (built) SPA shell.
    pub fn discover(apps_dir: &Path, installed_apps: &[String], site_name: &str) -> Option<Spa> {
        let mut routes: Vec<SpaRoute> = Vec::new();
        let mut redirects: Vec<Redirect> = Vec::new();

        for app in installed_apps {
            let hooks = apps_dir.join(app).join(app).join("hooks.py");
            let src = match std::fs::read_to_string(&hooks) {
                Ok(s) => s,
                Err(_) => continue,
            };

            for (from_route, to_route) in parse_pairs(&src, "from_route", "to_route") {
                let base = route_base(&from_route);
                if base.is_empty() || routes.iter().any(|r| r.base == base) {
                    continue;
                }
                // The built shell is required: on an unbuilt checkout there's no `www/<route>.html`
                // and we simply don't register the route (honest — a bare link would 404).
                let shell_path = match resolve_www(apps_dir, app, &to_route) {
                    Some(p) => p,
                    None => continue,
                };
                let raw = match std::fs::read_to_string(&shell_path) {
                    Ok(s) => s,
                    Err(_) => continue,
                };
                routes.push(SpaRoute {
                    shell: render_shell(&raw, &base, site_name).into_bytes(),
                    base,
                });
            }

            for (source, target) in parse_pairs(&src, "source", "target") {
                if let (Some(prefix), Some(target)) = (redirect_prefix(&source), redirect_prefix(&target)) {
                    redirects.push(Redirect { prefix, target });
                }
            }
        }

        if routes.is_empty() {
            return None;
        }
        // Longest base first so e.g. `/crm` is matched before a shorter overlapping prefix.
        routes.sort_by(|a, b| b.base.len().cmp(&a.base.len()));
        Some(Spa { routes, redirects })
    }

    /// Number of registered SPA routes (for the startup log line).
    pub fn route_count(&self) -> usize {
        self.routes.len()
    }

    /// If `url` matches a redirect or an SPA route, return the response to send. The base route and
    /// every `<path:app_path>` sub-route serve the same shell (the SPA client-routes from there).
    pub fn try_route(&self, url: &str) -> Option<RawResp> {
        let path = url.split('?').next().unwrap_or(url);

        for r in &self.redirects {
            if path == r.prefix || path.starts_with(&(r.prefix.clone() + "/")) {
                let rest = &path[r.prefix.len()..];
                return Some(redirect_301(&(r.target.clone() + rest)));
            }
        }
        for rt in &self.routes {
            if path == rt.base || path.starts_with(&(rt.base.clone() + "/")) {
                return Some(spa_html(rt.shell.clone()));
            }
        }
        None
    }
}

// --------------------------------------------------------------------- responses

fn spa_html(body: Vec<u8>) -> RawResp {
    RawResp {
        status: 200,
        content_type: "text/html; charset=utf-8".into(),
        body,
        // SPA www pages are `no_cache=1`; never let a proxy/browser pin a stale boot.
        headers: vec![("Cache-Control".into(), "no-store".into())],
    }
}

fn redirect_301(location: &str) -> RawResp {
    RawResp {
        status: 301,
        content_type: "text/html; charset=utf-8".into(),
        body: Vec::new(),
        headers: vec![("Location".into(), location.into())],
    }
}

// --------------------------------------------------------------------- hooks.py parsing
//
// hooks.py is regular enough that a targeted scan beats pulling in a Python parser (and ferro is
// deliberately std-only). We look for `"<key1>": "<val1>" ... "<key2>": "<val2>"` pairs, skipping
// commented-out example lines.

/// Extract `("<k1>"->v1, "<k2>"->v2)` string pairs where k1 precedes k2 within each entry.
fn parse_pairs(src: &str, k1: &str, k2: &str) -> Vec<(String, String)> {
    let needle1 = format!("\"{k1}\"");
    let needle2 = format!("\"{k2}\"");
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(rel) = src[from..].find(&needle1) {
        let idx = from + rel;
        from = idx + needle1.len();
        if line_is_comment(src, idx) {
            continue;
        }
        let (v1, pos) = match next_string(src, idx + needle1.len()) {
            Some(x) => x,
            None => continue,
        };
        let to_rel = match src[pos..].find(&needle2) {
            Some(r) => r,
            None => continue,
        };
        let (v2, end) = match next_string(src, pos + to_rel + needle2.len()) {
            Some(x) => x,
            None => continue,
        };
        from = end;
        out.push((v1, v2));
    }
    out
}

/// The next double-quoted string at/after `start`. Backslashes are kept literal (these are raw
/// regex strings like `r"/g\1"`, not escaped string literals). Returns (value, idx_after_close).
fn next_string(src: &str, start: usize) -> Option<(String, usize)> {
    let b = src.as_bytes();
    let mut i = start;
    while i < b.len() && b[i] != b'"' {
        i += 1;
    }
    if i >= b.len() {
        return None;
    }
    let val_start = i + 1;
    let mut j = val_start;
    while j < b.len() && b[j] != b'"' {
        j += 1;
    }
    if j >= b.len() {
        return None;
    }
    Some((src[val_start..j].to_string(), j + 1))
}

/// True if the line containing `idx` is a `#` comment.
fn line_is_comment(src: &str, idx: usize) -> bool {
    let b = src.as_bytes();
    let mut s = idx;
    while s > 0 && b[s - 1] != b'\n' {
        s -= 1;
    }
    let mut k = s;
    while k < b.len() && (b[k] == b' ' || b[k] == b'\t') {
        k += 1;
    }
    k < b.len() && b[k] == b'#'
}

/// The literal base of a route pattern: segments up to the first werkzeug converter `<...>`.
/// `/crm/<path:app_path>` -> `/crm`; `/helpdesk/<path:app_path>` -> `/helpdesk`.
fn route_base(from_route: &str) -> String {
    let mut segs: Vec<&str> = Vec::new();
    for seg in from_route.split('/') {
        if seg.is_empty() {
            continue;
        }
        if seg.contains('<') {
            break;
        }
        segs.push(seg);
    }
    if segs.is_empty() {
        String::new()
    } else {
        format!("/{}", segs.join("/"))
    }
}

/// The literal prefix of a redirect regex, up to the first metacharacter.
/// `/teams(/.*)?` -> `/teams`; `/g\1` -> `/g`.
fn redirect_prefix(s: &str) -> Option<String> {
    let mut out = String::new();
    for c in s.chars() {
        if "(<[?*\\.$^+{|)".contains(c) {
            break;
        }
        out.push(c);
    }
    let out = out.trim_end_matches('/');
    if out.is_empty() || !out.starts_with('/') {
        None
    } else {
        Some(out.to_string())
    }
}

/// Locate an app's built www shell for `to_route`: `www/<route>.html` or `www/<route>/index.html`.
fn resolve_www(apps_dir: &Path, app: &str, to_route: &str) -> Option<PathBuf> {
    let www = apps_dir.join(app).join(app).join("www");
    let direct = www.join(format!("{to_route}.html"));
    if direct.is_file() {
        return Some(direct);
    }
    let index = www.join(to_route).join("index.html");
    if index.is_file() {
        return Some(index);
    }
    None
}

// --------------------------------------------------------------------- shell rendering
//
// The built shell is mostly static (hashed asset tags already injected). It carries a handful of
// Jinja placeholders that Frappe fills from the www `get_context`; we fill the ones these frappe-ui
// templates actually use and blank anything else, so no raw `{{ ... }}` reaches the browser.

fn render_shell(raw: &str, base: &str, site_name: &str) -> String {
    let csrf = util::random_name();
    let boot_json = serde_json::to_string(&build_boot(base, site_name, &csrf)).unwrap_or_else(|_| "{}".into());

    // Drop `{% ... %}` control blocks, then substitute `{{ ... }}` expressions.
    let no_blocks = sub_delims(raw, "{%", "%}", &|_| String::new());
    let mut html = sub_delims(&no_blocks, "{{", "}}", &|expr| eval_expr(expr.trim(), &boot_json, site_name, &csrf));

    // Safety net for a shell with no csrf placeholder (frappe-ui's `request` reads window.csrf_token
    // for API calls). ferro doesn't validate CSRF, but the SPA expects the global to exist.
    if !html.contains("csrf_token") {
        let inject = format!("<script>window.csrf_token = \"{csrf}\";</script>");
        if let Some(p) = html.find("</head>") {
            html.insert_str(p, &inject);
        } else {
            html.insert_str(0, &inject);
        }
    }
    html
}

/// A minimal boot blob — enough for the SPA to load and client-route. App-specific fields (lead
/// counts, agent name, …) come from Python `get_context` we don't run; their absence affects only
/// minor in-app states, never whether the route resolves.
fn build_boot(base: &str, site_name: &str, csrf: &str) -> Value {
    json!({
        "site_name": site_name,
        "default_route": base,
        "csrf_token": csrf,
        "read_only_mode": false,
        "setup_complete": 1,
        "sysdefaults": {},
        "lang": "en",
        "system_timezone": "UTC",
        "timezone": { "system": "UTC", "user": "UTC" },
    })
}

/// Evaluate a `{{ ... }}` expression against the known context. Handles `expr | filter`,
/// `expr or 'default'`, and the dotted `boot.lang` / `boot.dir` the helpdesk shell uses.
fn eval_expr(expr: &str, boot_json: &str, site: &str, csrf: &str) -> String {
    let head = expr.split('|').next().unwrap_or("").trim();
    let base = head.split(" or ").next().unwrap_or("").trim();
    match base {
        "boot" => boot_json.to_string(),
        "site_name" => site.to_string(),
        "csrf_token" => csrf.to_string(),
        "boot.lang" => "en".to_string(),
        "boot.dir" => "ltr".to_string(),
        _ => {
            // Unknown var: fall back to its `or '<default>'` literal if it has one, else blank.
            if let Some(default) = expr.split(" or ").nth(1) {
                default.trim().trim_matches(|c| c == '\'' || c == '"').to_string()
            } else {
                String::new()
            }
        }
    }
}

/// Replace each `open ... close` region by `f(inner)`. Unterminated opens are left verbatim.
fn sub_delims(src: &str, open: &str, close: &str, f: &dyn Fn(&str) -> String) -> String {
    let mut out = String::with_capacity(src.len());
    let mut rest = src;
    while let Some(o) = rest.find(open) {
        out.push_str(&rest[..o]);
        let after = &rest[o + open.len()..];
        match after.find(close) {
            Some(c) => {
                out.push_str(&f(&after[..c]));
                rest = &after[c + close.len()..];
            }
            None => {
                out.push_str(open);
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, body: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    }

    /// Build a throwaway apps/ tree with one SPA app, returning its path.
    fn fixture(tag: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("ferro_spa_{}_{}", std::process::id(), tag));
        let _ = std::fs::remove_dir_all(&root);
        let apps = root.join("apps");

        // crm: single-file www shell with the placeholders these templates use.
        write(
            &apps.join("crm/crm/hooks.py"),
            "app_name = \"crm\"\n# website_route_rules = [example]\nwebsite_route_rules = [\n\t{\"from_route\": \"/crm/<path:app_path>\", \"to_route\": \"crm\"},\n]\n",
        );
        write(
            &apps.join("crm/crm/www/crm.html"),
            "<html lang=\"{{ boot.lang or 'en' }}\"><head><title>{{ site_name }}</title>\
             <script>window.boot = {{ boot | tojson }};</script></head>\
             <body><div id=app></div></body></html>",
        );

        // gameplan: a redirect (/teams -> /g) plus its own route.
        write(
            &apps.join("gameplan/gameplan/hooks.py"),
            "website_route_rules = [\n\t{\"from_route\": \"/g/<path:app_path>\", \"to_route\": \"g\"},\n]\nwebsite_redirects = [\n\t{\"source\": r\"/teams(/.*)?\", \"target\": r\"/g\\1\"},\n]\n",
        );
        write(
            &apps.join("gameplan/gameplan/www/g.html"),
            "<html><head></head><body>gameplan</body></html>",
        );

        // helpdesk: directory-style www page (www/<route>/index.html).
        write(
            &apps.join("helpdesk/helpdesk/hooks.py"),
            "website_route_rules = [\n\t{\n\t\t\"from_route\": \"/helpdesk/<path:app_path>\",\n\t\t\"to_route\": \"helpdesk\",\n\t},\n]\n",
        );
        write(
            &apps.join("helpdesk/helpdesk/www/helpdesk/index.html"),
            "<html><head></head><body>helpdesk</body></html>",
        );

        apps
    }

    #[test]
    fn serves_base_and_subroutes_for_installed_apps() {
        let apps = fixture("serve");
        let installed = vec!["crm".to_string(), "gameplan".to_string(), "helpdesk".to_string()];
        let spa = Spa::discover(&apps, &installed, "demo.site").expect("routes discovered");
        assert_eq!(spa.route_count(), 3);

        // Base route serves the shell, with placeholders resolved (no raw Jinja leaks).
        let r = spa.try_route("/crm").expect("base route");
        assert_eq!(r.status, 200);
        let body = String::from_utf8(r.body).unwrap();
        assert!(body.contains("demo.site"), "site_name substituted");
        assert!(body.contains("\"site_name\":\"demo.site\""), "boot json injected");
        assert!(body.contains("lang=\"en\""), "boot.lang default applied");
        assert!(!body.contains("{{"), "no unrendered Jinja: {body}");

        // Deep sub-routes (client-side paths) serve the same shell.
        assert_eq!(spa.try_route("/crm/leads/42").unwrap().status, 200);
        assert_eq!(spa.try_route("/helpdesk/tickets").unwrap().status, 200);
        assert_eq!(spa.try_route("/g/home").unwrap().status, 200);

        // Query strings are ignored for matching.
        assert_eq!(spa.try_route("/crm?view=board").unwrap().status, 200);

        // A path that only shares a prefix must NOT match.
        assert!(spa.try_route("/crmotron").is_none());
        assert!(spa.try_route("/").is_none());
        assert!(spa.try_route("/api/method/foo").is_none());

        let _ = std::fs::remove_dir_all(apps.parent().unwrap());
    }

    #[test]
    fn applies_website_redirects() {
        let apps = fixture("redir");
        let spa = Spa::discover(&apps, &["gameplan".to_string()], "demo.site").unwrap();
        let r = spa.try_route("/teams/abc").expect("redirect matched");
        assert_eq!(r.status, 301);
        let loc = r.headers.iter().find(|(k, _)| k == "Location").map(|(_, v)| v.clone());
        assert_eq!(loc.as_deref(), Some("/g/abc"));
        let _ = std::fs::remove_dir_all(apps.parent().unwrap());
    }

    #[test]
    fn uninstalled_apps_are_not_served() {
        let apps = fixture("gate");
        // crm present on disk but NOT in installed_apps -> no routes.
        let spa = Spa::discover(&apps, &["frappe".to_string()], "demo.site");
        assert!(spa.is_none(), "must not serve routes for uninstalled apps");
        let _ = std::fs::remove_dir_all(apps.parent().unwrap());
    }

    #[test]
    fn csrf_injected_when_shell_lacks_it() {
        // A dev-style shell with no csrf placeholder still gets window.csrf_token.
        let html = render_shell(
            "<html><head></head><body><div id=app></div></body></html>",
            "/crm",
            "demo.site",
        );
        assert!(html.contains("window.csrf_token ="), "csrf injected: {html}");
    }
}
