//! `ferro bench <command> [...]` — the native reimplementation of Frappe's `bench_helper.py`
//! passthrough commands.
//!
//! `bench <cmd>` execs `python -m frappe.utils.bench_helper frappe <cmd>`, which parses a set of
//! GLOBAL options (`--site`, `--force`, `--verbose`, `--profile`) BEFORE the subcommand, resolves
//! the target site list, and dispatches to a click command. The `contrib/ferro_bench_shim.py`
//! router intercepts the supported subset and re-execs this binary as `ferro bench <cmd> ...`;
//! anything we don't implement falls through to the real Python helper (here and in the shim).
//!
//! This module implements the pure-native commands (config, auth, scheduler toggles, read-only
//! introspection, backup/restore/teardown). The schema/install lifecycle (install-app, migrate,
//! new-site) lands in dedicated modules; until then those names fall through to Python.

use rusqlite::{params, Connection};
use serde_json::{json, Map, Value};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::{open_conn, resolve_db_path};

/// Global options mirrored from bench_helper.py's `app_group` (parsed before the subcommand).
#[derive(Default, Clone)]
struct GlobalOpts {
    site: Option<String>,
    sites_path: Option<String>,
    force: bool,
    verbose: bool,
    profile: bool,
}

/// Entry point: `args` is everything after the literal `bench` token.
pub fn dispatch(args: &[String]) -> ! {
    let mut g = GlobalOpts::default();
    let mut subcmd: Option<String> = None;
    let mut rest: Vec<String> = Vec::new();
    let mut i = 0;
    // Global opts come before the subcommand (click group options); once we hit the first bare
    // word that is the subcommand and everything after it is forwarded verbatim.
    while i < args.len() {
        if subcmd.is_none() {
            match args[i].as_str() {
                "--site" | "-s" => {
                    g.site = args.get(i + 1).cloned();
                    i += 2;
                    continue;
                }
                "--sites-path" => {
                    g.sites_path = args.get(i + 1).cloned();
                    i += 2;
                    continue;
                }
                "--force" => {
                    g.force = true;
                    i += 1;
                    continue;
                }
                "--verbose" | "-v" => {
                    g.verbose = true;
                    i += 1;
                    continue;
                }
                "--profile" => {
                    g.profile = true;
                    i += 1;
                    continue;
                }
                s if !s.starts_with('-') => {
                    subcmd = Some(s.to_string());
                    i += 1;
                    continue;
                }
                _ => {
                    // unknown global flag: keep it for fallthrough
                    rest.push(args[i].clone());
                    i += 1;
                    continue;
                }
            }
        } else {
            rest.push(args[i].clone());
            i += 1;
        }
    }

    let cmd = match subcmd.as_deref() {
        Some(c) => c,
        None => {
            eprintln!("ferro bench: no command given");
            std::process::exit(2);
        }
    };

    let code = match cmd {
        // ---- Wave 1: config + site meta -------------------------------------------------
        "set-config" => cmd_set_config(&g, &rest),
        "use" => cmd_use(&g, &rest),
        "set-maintenance-mode" => cmd_set_maintenance_mode(&g, &rest),
        "list-sites" => cmd_list_sites(&g, &rest),
        "show-config" => cmd_show_config(&g, &rest),
        "list-apps" => cmd_list_apps(&g, &rest),
        "version" => cmd_version(&g, &rest),
        // ---- Wave 1: auth + sessions ----------------------------------------------------
        "set-admin-password" => cmd_set_password(&g, "Administrator", &rest),
        "set-password" => {
            let user = rest.first().cloned().unwrap_or_default();
            cmd_set_password(&g, &user, rest.get(1..).unwrap_or(&[]))
        }
        "destroy-all-sessions" => cmd_destroy_all_sessions(&g),
        // ---- Wave 1: scheduler ----------------------------------------------------------
        "enable-scheduler" => cmd_toggle_scheduler(&g, true),
        "disable-scheduler" => cmd_toggle_scheduler(&g, false),
        "scheduler" => cmd_scheduler(&g, &rest),
        "clear-cache" => cmd_clear_cache(&g),
        "clear-website-cache" => cmd_clear_cache(&g),
        // ---- Wave 2: backup / restore / teardown ----------------------------------------
        "backup" => cmd_backup(&g, &rest),
        "restore" => cmd_restore(&g, &rest),
        "drop-site" => cmd_drop_site(&g, &rest),
        "db-console" | "sqlite" => cmd_db_console(&g),
        "bypass-patch" => cmd_bypass_patch(&g, &rest),
        // ---- Wave 4: schema sync + install lifecycle -------------------------------------
        "reload-doctype" => cmd_reload_doctype(&g, &rest),
        "reload-doc" => cmd_reload_doc(&g, &rest),
        "install-app" => cmd_install_app(&g, &rest),
        "migrate" => cmd_migrate(&g, &rest),
        "new-site" => cmd_new_site(&g, &rest),
        "remove-from-installed-apps" => cmd_remove_from_installed_apps(&g, &rest),
        // ---- Wave 5: db ops + users ------------------------------------------------------
        "disable-user" => cmd_disable_user(&g, &rest),
        "describe-database-table" => cmd_describe_table(&g, &rest),
        "add-database-index" => cmd_add_database_index(&g, &rest),
        "trim-database" => cmd_trim_database(&g, &rest),
        "ready-for-migration" => cmd_ready_for_migration(&g),
        // ---- not yet native: delegate to the real Python helper -------------------------
        // fallthrough_to_python diverges (-> !), so this arm never falls through to exit().
        _ => fallthrough_to_python(args),
    };
    std::process::exit(code);
}

// =====================================================================================
// site resolution (mirrors bench_helper.py get_sites: all / --site / FRAPPE_SITE /
// default_site / currentsite.txt)
// =====================================================================================

fn sites_dir(g: &GlobalOpts) -> PathBuf {
    if let Some(s) = &g.sites_path {
        PathBuf::from(s)
    } else if Path::new("sites").is_dir() {
        PathBuf::from("sites")
    } else {
        PathBuf::from(".")
    }
}

fn is_site_dir(p: &Path) -> bool {
    p.is_dir() && p.join("site_config.json").is_file()
}

fn all_site_names(sd: &Path) -> Vec<String> {
    let mut v = Vec::new();
    if let Ok(rd) = fs::read_dir(sd) {
        for e in rd.flatten() {
            let p = e.path();
            if is_site_dir(&p) {
                if let Some(n) = p.file_name() {
                    let n = n.to_string_lossy();
                    if !n.starts_with('.') {
                        v.push(n.into_owned());
                    }
                }
            }
        }
    }
    v.sort();
    v
}

fn default_site_name(sd: &Path) -> Option<String> {
    // common_site_config.json default_site
    if let Ok(t) = fs::read_to_string(sd.join("common_site_config.json")) {
        if let Ok(v) = serde_json::from_str::<Value>(&t) {
            if let Some(s) = v.get("default_site").and_then(|x| x.as_str()) {
                if !s.is_empty() {
                    return Some(s.to_string());
                }
            }
        }
    }
    // currentsite.txt (deprecated upstream, harmless single-site fallback here)
    if let Ok(s) = fs::read_to_string(sd.join("currentsite.txt")) {
        let s = s.trim().to_string();
        if !s.is_empty() {
            return Some(s);
        }
    }
    // ferro extension: a lone site is unambiguous
    let names = all_site_names(sd);
    if names.len() == 1 {
        return names.into_iter().next();
    }
    None
}

/// Resolve the global `--site` selector to a list of site directories.
fn resolve_sites(g: &GlobalOpts) -> Vec<PathBuf> {
    let sd = sites_dir(g);
    let names: Vec<String> = if g.site.as_deref() == Some("all") {
        all_site_names(&sd)
    } else if let Some(s) = &g.site {
        vec![s.clone()]
    } else if let Ok(env) = std::env::var("FRAPPE_SITE") {
        if !env.is_empty() {
            vec![env]
        } else {
            default_site_name(&sd).into_iter().collect()
        }
    } else {
        default_site_name(&sd).into_iter().collect()
    };
    names
        .into_iter()
        .map(|n| sd.join(n))
        .filter(|p| p.join("site_config.json").is_file())
        .collect()
}

/// Resolve to exactly one site dir or print SiteNotSpecified and signal exit-1.
fn require_sites(g: &GlobalOpts) -> Option<Vec<PathBuf>> {
    let sites = resolve_sites(g);
    if sites.is_empty() {
        eprintln!("Error: Please specify --site sitename (or set a default site with `bench use`)");
    }
    Some(sites).filter(|s| !s.is_empty())
}

fn site_name(dir: &Path) -> String {
    dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

fn read_site_config(dir: &Path) -> Map<String, Value> {
    fs::read_to_string(dir.join("site_config.json"))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

fn bench_root(sd: &Path) -> PathBuf {
    sd.parent().map(|p| p.to_path_buf()).unwrap_or_else(|| PathBuf::from("."))
}

// =====================================================================================
// JSON config writing — mirrors json.dumps(indent=1, sort_keys=True) + the coercion rules
// from installer._update_config_file
// =====================================================================================

/// Serialize a Value the way Frappe writes config files: 1-space indent, keys sorted.
fn json_frappe(v: &Value, indent: usize, out: &mut String) {
    match v {
        Value::Object(m) => {
            if m.is_empty() {
                out.push_str("{}");
                return;
            }
            out.push_str("{\n");
            let mut keys: Vec<&String> = m.keys().collect();
            keys.sort();
            for (idx, k) in keys.iter().enumerate() {
                if idx > 0 {
                    out.push_str(",\n");
                }
                out.extend(std::iter::repeat(' ').take(indent + 1));
                out.push_str(&serde_json::to_string(k).unwrap());
                out.push_str(": ");
                json_frappe(&m[*k], indent + 1, out);
            }
            out.push('\n');
            out.extend(std::iter::repeat(' ').take(indent));
            out.push('}');
        }
        Value::Array(a) => {
            if a.is_empty() {
                out.push_str("[]");
                return;
            }
            out.push_str("[\n");
            for (idx, item) in a.iter().enumerate() {
                if idx > 0 {
                    out.push_str(",\n");
                }
                out.extend(std::iter::repeat(' ').take(indent + 1));
                json_frappe(item, indent + 1, out);
            }
            out.push('\n');
            out.extend(std::iter::repeat(' ').take(indent));
            out.push(']');
        }
        scalar => out.push_str(&serde_json::to_string(scalar).unwrap()),
    }
}

/// Read-modify-write a config file: set `key` to `value`, or delete it if `value` is None.
fn update_config_file(path: &Path, key: &str, value: Option<Value>) -> std::io::Result<()> {
    let mut map: Map<String, Value> = fs::read_to_string(path)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default();
    match value {
        Some(v) => {
            map.insert(key.to_string(), v);
        }
        None => {
            map.remove(key);
        }
    }
    let mut s = String::new();
    json_frappe(&Value::Object(map), 0, &mut s);
    fs::write(path, s)
}

/// Coerce a CLI string value the way installer._update_config_file does. None => delete key.
fn coerce_config_value(raw: &str, parse: bool) -> Option<Value> {
    if parse {
        // -p/--parse: best-effort literal (JSON subset). Frappe uses ast.literal_eval; we do not
        // run Python, so accept JSON literals and fall back to the raw string.
        return Some(serde_json::from_str(raw).unwrap_or_else(|_| Value::String(raw.to_string())));
    }
    match raw {
        "0" => Some(json!(0)),
        "1" => Some(json!(1)),
        "true" => Some(json!(true)),
        "false" => Some(json!(false)),
        "None" => None,
        other => Some(Value::String(other.to_string())),
    }
}

// =====================================================================================
// Wave 1 — config + site meta
// =====================================================================================

fn cmd_set_config(g: &GlobalOpts, rest: &[String]) -> i32 {
    // set-config KEY VALUE [-g/--global] [-p/--parse]
    let mut positional = Vec::new();
    let mut global = false;
    let mut parse = false;
    for a in rest {
        match a.as_str() {
            "-g" | "--global" => global = true,
            "-p" | "--parse" => parse = true,
            other => positional.push(other.to_string()),
        }
    }
    if positional.len() < 2 {
        eprintln!("usage: bench set-config KEY VALUE [-g] [-p]");
        return 2;
    }
    let (key, raw) = (&positional[0], &positional[1]);
    let value = coerce_config_value(raw, parse);

    if global {
        let sd = sites_dir(g);
        let path = sd.join("common_site_config.json");
        if let Err(e) = update_config_file(&path, key, value) {
            eprintln!("set-config: {e}");
            return 1;
        }
        return 0;
    }
    let sites = match require_sites(g) {
        Some(s) => s,
        None => return 1,
    };
    for site in &sites {
        let path = site.join("site_config.json");
        if let Err(e) = update_config_file(&path, key, value.clone()) {
            eprintln!("set-config ({}): {e}", site_name(site));
            return 1;
        }
    }
    0
}

fn cmd_use(g: &GlobalOpts, rest: &[String]) -> i32 {
    let site = match rest.first() {
        Some(s) => s,
        None => {
            eprintln!("usage: bench use SITE");
            return 2;
        }
    };
    let sd = sites_dir(g);
    if !is_site_dir(&sd.join(site)) {
        println!("Site {site} does not exist");
        return 1;
    }
    let path = sd.join("common_site_config.json");
    if let Err(e) = update_config_file(&path, "default_site", Some(Value::String(site.clone()))) {
        eprintln!("use: {e}");
        return 1;
    }
    println!("Current Site set to {site}");
    0
}

fn cmd_set_maintenance_mode(g: &GlobalOpts, rest: &[String]) -> i32 {
    // set-maintenance-mode <on|off>
    let state = match rest.first().map(|s| s.as_str()) {
        Some("on") => 1,
        Some("off") => 0,
        _ => {
            eprintln!("usage: bench set-maintenance-mode <on|off>");
            return 2;
        }
    };
    let sites = match require_sites(g) {
        Some(s) => s,
        None => return 1,
    };
    for site in &sites {
        let path = site.join("site_config.json");
        if let Err(e) = update_config_file(&path, "maintenance_mode", Some(json!(state))) {
            eprintln!("set-maintenance-mode ({}): {e}", site_name(site));
            return 1;
        }
    }
    0
}

fn cmd_list_sites(g: &GlobalOpts, rest: &[String]) -> i32 {
    let as_json = rest.iter().any(|a| a == "--json");
    let sd = sites_dir(g);
    let names = all_site_names(&sd);
    let default = default_site_name(&sd);
    if as_json {
        println!("{}", serde_json::to_string(&names).unwrap());
    } else if names.is_empty() {
        println!("No sites found");
    } else {
        println!("Available sites:");
        for n in &names {
            if Some(n) == default.as_ref() {
                println!("* {n}");
            } else {
                println!("  {n}");
            }
        }
    }
    0
}

fn cmd_show_config(g: &GlobalOpts, rest: &[String]) -> i32 {
    let fmt = opt_value(rest, &["-f", "--format"]).unwrap_or_else(|| "text".to_string());
    let sites = match require_sites(g) {
        Some(s) => s,
        None => return 1,
    };
    let sd = sites_dir(g);
    let common: Map<String, Value> = fs::read_to_string(sd.join("common_site_config.json"))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default();
    let mut out = Map::new();
    for site in &sites {
        let mut merged = common.clone();
        for (k, v) in read_site_config(site) {
            merged.insert(k, v);
        }
        out.insert(site_name(site), Value::Object(merged));
    }
    if fmt == "json" {
        let mut s = String::new();
        json_frappe(&Value::Object(out), 0, &mut s);
        println!("{s}");
    } else {
        for (site, cfg) in &out {
            if sites.len() > 1 {
                println!("\n{site}:");
            }
            if let Value::Object(m) = cfg {
                let mut keys: Vec<&String> = m.keys().collect();
                keys.sort();
                for k in keys {
                    let v = &m[k];
                    let rendered = match v {
                        Value::String(s) => s.clone(),
                        other => serde_json::to_string(other).unwrap(),
                    };
                    println!("{k} {rendered}");
                }
            }
        }
    }
    0
}

fn cmd_list_apps(g: &GlobalOpts, rest: &[String]) -> i32 {
    let fmt = opt_value(rest, &["-f", "--format"]).unwrap_or_else(|| "text".to_string());
    let sites = match require_sites(g) {
        Some(s) => s,
        None => return 1,
    };
    let mut summary: Map<String, Value> = Map::new();
    for site in &sites {
        let con = open_conn(&resolve_db_path(&site.to_string_lossy()));
        let apps = installed_apps(&con);
        if fmt == "text" {
            if sites.len() > 1 {
                println!("{}", site_name(site));
            }
            for a in &apps {
                println!("{a}");
            }
            println!();
        }
        summary.insert(site_name(site), json!(apps));
    }
    if fmt == "json" {
        let mut s = String::new();
        json_frappe(&Value::Object(summary), 0, &mut s);
        println!("{s}");
    }
    0
}

/// Serialize a config Value the Frappe way (1-space indent, sorted keys) — used by install.rs.
pub(crate) fn write_config_json(v: &Value, out: &mut String) {
    json_frappe(v, 0, out);
}

/// installed_apps for use by other modules (install/migrate).
pub(crate) fn installed_apps_of(con: &Connection) -> Vec<String> {
    installed_apps(con)
}

/// installed_apps as stored by Frappe: a JSON list in tabDefaultValue(defkey='installed_apps').
fn installed_apps(con: &Connection) -> Vec<String> {
    let raw: Option<String> = con
        .query_row(
            "SELECT defvalue FROM tabDefaultValue WHERE defkey='installed_apps' AND parent='__global'",
            [],
            |r| r.get(0),
        )
        .ok();
    raw.and_then(|s| serde_json::from_str::<Vec<String>>(&s).ok()).unwrap_or_default()
}

fn cmd_version(g: &GlobalOpts, rest: &[String]) -> i32 {
    let fmt = opt_value(rest, &["-f", "--format"]).unwrap_or_else(|| "plain".to_string());
    let sd = sites_dir(g);
    let root = bench_root(&sd);
    let apps_txt = sd.join("apps.txt");
    let apps: Vec<String> = fs::read_to_string(&apps_txt)
        .map(|t| t.split_whitespace().map(str::to_string).collect())
        .unwrap_or_else(|_| {
            // fall back to the apps/ directory listing
            fs::read_dir(root.join("apps"))
                .map(|rd| {
                    rd.flatten()
                        .filter(|e| e.path().is_dir())
                        .filter_map(|e| e.file_name().to_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default()
        });
    let mut data = Vec::new();
    for app in &apps {
        let app_path = root.join("apps").join(app);
        let version = read_app_version(&app_path, app).unwrap_or_default();
        let branch = git_out(&app_path, &["rev-parse", "--abbrev-ref", "HEAD"]).unwrap_or_default();
        let commit = git_out(&app_path, &["rev-parse", "--short", "HEAD"]).unwrap_or_default();
        data.push((app.clone(), version, branch, commit));
    }
    match fmt.as_str() {
        "json" => {
            let arr: Vec<Value> = data
                .iter()
                .map(|(a, v, b, c)| json!({"app": a, "version": v, "branch": b, "commit": c}))
                .collect();
            println!("{}", serde_json::to_string_pretty(&arr).unwrap());
        }
        "legacy" => {
            for (a, v, _, _) in &data {
                println!("{a} {v}");
            }
        }
        _ => {
            for (a, v, b, c) in &data {
                println!("{a} {v} {b} ({c})");
            }
        }
    }
    0
}

/// Read `__version__ = "x.y.z"` from an app's package __init__.py.
fn read_app_version(app_path: &Path, app: &str) -> Option<String> {
    let init = app_path.join(app).join("__init__.py");
    let text = fs::read_to_string(init).ok()?;
    for line in text.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("__version__") {
            if let Some(eq) = rest.split_once('=') {
                let v = eq.1.trim().trim_matches(|c| c == '"' || c == '\'' || c == ' ');
                if !v.is_empty() {
                    return Some(v.to_string());
                }
            }
        }
    }
    None
}

fn git_out(repo: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git").arg("-C").arg(repo).args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

// =====================================================================================
// Wave 1 — auth + sessions + scheduler
// =====================================================================================

fn cmd_set_password(g: &GlobalOpts, user: &str, rest: &[String]) -> i32 {
    if user.is_empty() {
        eprintln!("usage: bench set-password USER [PASSWORD]");
        return 2;
    }
    let logout_all = rest.iter().any(|a| a == "--logout-all-sessions");
    let password: Option<String> = rest.iter().find(|a| !a.starts_with('-')).cloned();
    let sites = match require_sites(g) {
        Some(s) => s,
        None => return 1,
    };
    for site in &sites {
        let con = open_conn(&resolve_db_path(&site.to_string_lossy()));
        // user must exist
        let exists: bool = con
            .query_row("SELECT 1 FROM tabUser WHERE name=?1", params![user], |_| Ok(()))
            .is_ok();
        if !exists {
            println!("User {user} does not exist");
            return 1;
        }
        let pwd = match &password {
            Some(p) => p.clone(),
            None => prompt_password(&format!("{user}'s password for {}: ", site_name(site))),
        };
        if pwd.is_empty() {
            eprintln!("set-password: empty password");
            return 1;
        }
        let hash = crate::crypto::passlib_pbkdf2_sha256_hash(&pwd);
        // INSERT OR REPLACE into __Auth (sqlite branch of update_password)
        if let Err(e) = con.execute(
            "INSERT OR REPLACE INTO \"__Auth\" (doctype, name, fieldname, password, encrypted) \
             VALUES ('User', ?1, 'password', ?2, 0)",
            params![user, hash],
        ) {
            eprintln!("set-password: {e}");
            return 1;
        }
        if logout_all {
            con.execute("DELETE FROM tabSessions WHERE user=?1", params![user]).ok();
        }
        println!("Password set for {user} on {}", site_name(site));
    }
    0
}

fn prompt_password(prompt: &str) -> String {
    use std::io::Write;
    eprint!("{prompt}");
    std::io::stderr().flush().ok();
    let mut s = String::new();
    std::io::stdin().read_line(&mut s).ok();
    s.trim_end_matches(['\n', '\r']).to_string()
}

fn cmd_destroy_all_sessions(g: &GlobalOpts) -> i32 {
    let sites = match require_sites(g) {
        Some(s) => s,
        None => return 1,
    };
    for site in &sites {
        let con = open_conn(&resolve_db_path(&site.to_string_lossy()));
        match con.execute("DELETE FROM tabSessions", []) {
            Ok(n) => println!("Cleared {n} session(s) on {}", site_name(site)),
            Err(e) => {
                eprintln!("destroy-all-sessions: {e}");
                return 1;
            }
        }
    }
    0
}

fn set_single_value(con: &Connection, doctype: &str, field: &str, value: &str) -> rusqlite::Result<()> {
    let n = con.execute(
        "UPDATE tabSingles SET value=?3 WHERE doctype=?1 AND field=?2",
        params![doctype, field, value],
    )?;
    if n == 0 {
        con.execute(
            "INSERT INTO tabSingles (doctype, field, value) VALUES (?1, ?2, ?3)",
            params![doctype, field, value],
        )?;
    }
    Ok(())
}

fn get_single_value(con: &Connection, doctype: &str, field: &str) -> Option<String> {
    con.query_row(
        "SELECT value FROM tabSingles WHERE doctype=?1 AND field=?2",
        params![doctype, field],
        |r| r.get(0),
    )
    .ok()
}

fn cmd_toggle_scheduler(g: &GlobalOpts, enable: bool) -> i32 {
    let sites = match require_sites(g) {
        Some(s) => s,
        None => return 1,
    };
    for site in &sites {
        let con = open_conn(&resolve_db_path(&site.to_string_lossy()));
        if let Err(e) = set_single_value(&con, "System Settings", "enable_scheduler", if enable { "1" } else { "0" }) {
            eprintln!("scheduler toggle: {e}");
            return 1;
        }
        println!("{} for {}", if enable { "Enabled" } else { "Disabled" }, site_name(site));
    }
    0
}

fn cmd_scheduler(g: &GlobalOpts, rest: &[String]) -> i32 {
    // scheduler <pause|resume|disable|enable|status>
    let state = match rest.iter().find(|a| !a.starts_with('-')) {
        Some(s) => s.as_str(),
        None => {
            eprintln!("usage: bench scheduler <pause|resume|disable|enable|status>");
            return 2;
        }
    };
    let sites = match require_sites(g) {
        Some(s) => s,
        None => return 1,
    };
    for site in &sites {
        match state {
            "status" => {
                let con = open_conn(&resolve_db_path(&site.to_string_lossy()));
                let enabled = get_single_value(&con, "System Settings", "enable_scheduler")
                    .map(|v| v == "1")
                    .unwrap_or(false);
                let paused = read_site_config(site)
                    .get("pause_scheduler")
                    .and_then(|v| v.as_i64().map(|i| i == 1).or_else(|| v.as_bool()))
                    .unwrap_or(false);
                let status = if !enabled || paused { "disabled" } else { "enabled" };
                println!("Scheduler is {status} for site {}", site_name(site));
            }
            "pause" | "resume" => {
                let path = site.join("site_config.json");
                let v = json!(if state == "pause" { 1 } else { 0 });
                if let Err(e) = update_config_file(&path, "pause_scheduler", Some(v)) {
                    eprintln!("scheduler: {e}");
                    return 1;
                }
                println!("Scheduler is {state}d for site {}", site_name(site));
            }
            "enable" | "disable" => {
                let con = open_conn(&resolve_db_path(&site.to_string_lossy()));
                set_single_value(&con, "System Settings", "enable_scheduler", if state == "enable" { "1" } else { "0" }).ok();
                println!("Scheduler is {state}d for site {}", site_name(site));
            }
            other => {
                eprintln!("scheduler: unknown state '{other}'");
                return 2;
            }
        }
    }
    0
}

fn cmd_clear_cache(g: &GlobalOpts) -> i32 {
    // ferro's cache is in-process (cache.rs), owned by the running `ferro serve`. A separate CLI
    // process holds no shared/persistent cache, so there is nothing on disk or in the DB to purge;
    // we validate the site and report success. The serving process rebuilds meta lazily and
    // flushes fully on restart (`bench restart`).
    let sites = match require_sites(g) {
        Some(s) => s,
        None => return 1,
    };
    for site in &sites {
        // touch a sentinel a future `ferro serve` can watch to flush its in-process cache.
        let _ = fs::write(site.join("locks").join("ferro_cache_flush"), crate::util::now_datetime());
        println!("Cleared cache for {}", site_name(site));
    }
    0
}

// =====================================================================================
// Wave 2 — backup / restore / teardown
// =====================================================================================

struct BackupArtifacts {
    files: Vec<PathBuf>,
}

fn cmd_backup(g: &GlobalOpts, rest: &[String]) -> i32 {
    let with_files = rest.iter().any(|a| a == "--with-files");
    let compress = rest.iter().any(|a| a == "--compress");
    let sites = match require_sites(g) {
        Some(s) => s,
        None => return 1,
    };
    let mut code = 0;
    for site in &sites {
        match do_backup(site, with_files, compress) {
            Ok(art) => {
                println!("Backup for Site {} has been successfully completed", site_name(site));
                for f in &art.files {
                    let size = fs::metadata(f).map(|m| m.len()).unwrap_or(0);
                    println!("  {}  ({})", f.display(), human_size(size));
                }
            }
            Err(e) => {
                eprintln!("Backup failed for Site {}: {e}", site_name(site));
                code = 1;
            }
        }
    }
    code
}

fn do_backup(site: &Path, with_files: bool, compress: bool) -> Result<BackupArtifacts, String> {
    let cfg = read_site_config(site);
    let db_name = cfg.get("db_name").and_then(|v| v.as_str()).ok_or("no db_name in site_config")?;
    let db_file = site.join("db").join(format!("{db_name}.db"));
    if !db_file.is_file() {
        // fall back to ferro's resolver (handles single-db dirs)
        let resolved = resolve_db_path(&site.to_string_lossy());
        if !Path::new(&resolved).is_file() {
            return Err(format!("database file not found: {}", db_file.display()));
        }
    }
    let backups_dir = site.join("private").join("backups");
    fs::create_dir_all(&backups_dir).map_err(|e| e.to_string())?;

    // retention: prune our own old backup artifacts past keep_backups_for_hours (default 24)
    let keep_hours = cfg.get("keep_backups_for_hours").and_then(|v| v.as_u64()).unwrap_or(24);
    prune_old_backups(&backups_dir, keep_hours);

    let (y, mo, d, hh, mm, ss) = crate::util::now_civil();
    let ts = format!("{y:04}{mo:02}{d:02}_{hh:02}{mm:02}{ss:02}");
    let slug = site_name(site).replace('.', "_");
    let prefix = format!("{ts}-{slug}");
    let mut artifacts = Vec::new();

    // ---- database: a consistent snapshot, gzipped (raw db bytes, NOT a SQL dump) -------------
    let db_out = backups_dir.join(format!("{prefix}-database.sql.gz"));
    let snapshot = backups_dir.join(format!(".snap-{ts}.db"));
    {
        let resolved = resolve_db_path(&site.to_string_lossy());
        let con = Connection::open(&resolved).map_err(|e| e.to_string())?;
        // VACUUM INTO yields a consistent snapshot even under a live writer. It cannot run in a
        // transaction and does not accept bound parameters, so inline an escaped literal path.
        let snap_lit = snapshot.to_string_lossy().replace('\'', "''");
        con.execute_batch(&format!("VACUUM INTO '{snap_lit}'"))
            .map_err(|e| format!("snapshot failed: {e}"))?;
    }
    gzip_file(&snapshot, &db_out)?;
    fs::remove_file(&snapshot).ok();
    artifacts.push(db_out);

    // ---- site_config.json: byte-exact copy (contains secrets; do NOT re-serialize) ----------
    let conf_out = backups_dir.join(format!("{prefix}-site_config_backup.json"));
    fs::copy(site.join("site_config.json"), &conf_out).map_err(|e| e.to_string())?;
    artifacts.push(conf_out);

    // ---- files (optional) --------------------------------------------------------------------
    if with_files {
        for (sub, label) in [("public/files", "files"), ("private/files", "private-files")] {
            let src = site.join(sub);
            if src.is_dir() {
                let ext = if compress { "tar.gz" } else { "tar" };
                let out = backups_dir.join(format!("{prefix}-{label}.{ext}"));
                tar_dir(site, sub, &out, compress)?;
                artifacts.push(out);
            }
        }
    }

    Ok(BackupArtifacts { files: artifacts })
}

fn gzip_file(src: &Path, dst: &Path) -> Result<(), String> {
    let out = fs::File::create(dst).map_err(|e| e.to_string())?;
    let status = Command::new("gzip")
        .arg("-c")
        .arg(src)
        .stdout(out)
        .status()
        .map_err(|e| format!("gzip: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err("gzip failed".into())
    }
}

fn tar_dir(cwd: &Path, sub: &str, out: &Path, compress: bool) -> Result<(), String> {
    let flags = if compress { "-czf" } else { "-cf" };
    let status = Command::new("tar")
        .arg(flags)
        .arg(out)
        .arg("-C")
        .arg(cwd)
        .arg(sub)
        .status()
        .map_err(|e| format!("tar: {e}"))?;
    // tar returns 1 on "file changed as we read it" — tolerate non-fatal warnings.
    if status.code().map(|c| c <= 1).unwrap_or(false) {
        Ok(())
    } else {
        Err("tar failed".into())
    }
}

fn prune_old_backups(dir: &Path, keep_hours: u64) {
    let cutoff = std::time::SystemTime::now()
        .checked_sub(std::time::Duration::from_secs(keep_hours * 3600));
    let cutoff = match cutoff {
        Some(c) => c,
        None => return,
    };
    let is_backup = |name: &str| {
        name.ends_with("-database.sql.gz")
            || name.ends_with("-site_config_backup.json")
            || name.contains("-files.tar")
            || name.contains("-private-files.tar")
    };
    if let Ok(rd) = fs::read_dir(dir) {
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if !is_backup(&name) {
                continue;
            }
            if let Ok(meta) = e.metadata() {
                if let Ok(modified) = meta.modified() {
                    if modified < cutoff {
                        fs::remove_file(e.path()).ok();
                    }
                }
            }
        }
    }
}

fn human_size(bytes: u64) -> String {
    const U: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut b = bytes as f64;
    let mut i = 0;
    while b >= 1024.0 && i < U.len() - 1 {
        b /= 1024.0;
        i += 1;
    }
    format!("{b:.1}{}", U[i])
}

fn cmd_restore(g: &GlobalOpts, rest: &[String]) -> i32 {
    let backup_path = match rest.iter().find(|a| !a.starts_with('-')) {
        Some(p) => PathBuf::from(p),
        None => {
            eprintln!("usage: bench --site SITE restore <backup-db.sql.gz>");
            return 2;
        }
    };
    if !backup_path.is_file() {
        eprintln!("restore: file not found: {}", backup_path.display());
        return 1;
    }
    let sites = match require_sites(g) {
        Some(s) => s,
        None => return 1,
    };
    if sites.len() != 1 {
        eprintln!("restore: specify exactly one --site");
        return 2;
    }
    let site = &sites[0];
    let target = PathBuf::from(resolve_db_path(&site.to_string_lossy()));

    // validate the gunzipped stream begins with the SQLite magic header
    match gunzip_head(&backup_path, 16) {
        Ok(head) if head.starts_with(b"SQLite format 3\0") => {}
        Ok(_) => {
            eprintln!("restore: backup does not look like a SQLite database (bad header)");
            return 1;
        }
        Err(e) => {
            eprintln!("restore: {e}");
            return 1;
        }
    }

    // back up the current db, then gunzip the backup into place
    if target.is_file() {
        let bak = target.with_extension("db.pre-restore");
        fs::rename(&target, &bak).ok();
        println!("Existing database moved to {}", bak.display());
    }
    if let Err(e) = gunzip_to(&backup_path, &target) {
        eprintln!("restore: {e}");
        return 1;
    }
    // SQLite WAL/SHM sidecars of the old db are stale now
    fs::remove_file(target.with_extension("db-wal")).ok();
    fs::remove_file(target.with_extension("db-shm")).ok();
    println!("Restored {} -> {}", backup_path.display(), target.display());
    println!("Note: app reinstall + admin-password steps (Frappe's restore tail) are not run; the database is restored as-is.");
    0
}

fn gunzip_head(src: &Path, n: usize) -> Result<Vec<u8>, String> {
    use std::io::Read;
    let child = Command::new("gzip")
        .arg("-dc")
        .arg(src)
        .stdout(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("gzip: {e}"))?;
    let mut out = child.stdout.ok_or("no stdout")?;
    let mut buf = vec![0u8; n];
    let mut read = 0;
    while read < n {
        match out.read(&mut buf[read..]) {
            Ok(0) => break,
            Ok(k) => read += k,
            Err(e) => return Err(e.to_string()),
        }
    }
    buf.truncate(read);
    Ok(buf)
}

fn gunzip_to(src: &Path, dst: &Path) -> Result<(), String> {
    let out = fs::File::create(dst).map_err(|e| e.to_string())?;
    let status = Command::new("gzip")
        .arg("-dc")
        .arg(src)
        .stdout(out)
        .status()
        .map_err(|e| format!("gzip: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err("gunzip failed".into())
    }
}

fn cmd_drop_site(g: &GlobalOpts, rest: &[String]) -> i32 {
    let positional: Vec<&String> = rest.iter().filter(|a| !a.starts_with('-')).collect();
    let no_backup = rest.iter().any(|a| a == "--no-backup");
    let archived_path = opt_value(rest, &["--archived-sites-path"]);
    let site_name_arg = match positional.first() {
        Some(s) => (*s).clone(),
        None => {
            eprintln!("usage: bench drop-site SITE [--no-backup] [--force]");
            return 2;
        }
    };
    let sd = sites_dir(g);
    let site_dir = sd.join(&site_name_arg);
    if !is_site_dir(&site_dir) {
        eprintln!("drop-site: site {site_name_arg} does not exist");
        return 1;
    }
    if !no_backup {
        println!("Taking backup of {site_name_arg}");
        if let Err(e) = do_backup(&site_dir, true, false) {
            if g.force {
                eprintln!("Backup failed (continuing due to --force): {e}");
            } else {
                eprintln!("drop-site stopped: backup failed: {e}");
                eprintln!("Hint: use `bench drop-site {site_name_arg} --force` to force removal");
                return 1;
            }
        }
    }
    // archive: move the site dir under <bench>/archived/sites/<site>[count]
    let archived_dir = archived_path
        .map(PathBuf::from)
        .unwrap_or_else(|| bench_root(&sd).join("archived").join("sites"));
    if let Err(e) = fs::create_dir_all(&archived_dir) {
        eprintln!("drop-site: {e}");
        return 1;
    }
    let mut dest = archived_dir.join(&site_name_arg);
    let mut count = 0;
    while dest.exists() {
        count += 1;
        dest = archived_dir.join(format!("{site_name_arg}{count}"));
    }
    if let Err(e) = fs::rename(&site_dir, &dest) {
        eprintln!("drop-site: failed to archive: {e}");
        return 1;
    }
    println!("Site {site_name_arg} moved to archive: {}", dest.display());
    0
}

fn cmd_db_console(g: &GlobalOpts) -> i32 {
    let sites = match require_sites(g) {
        Some(s) => s,
        None => return 1,
    };
    let db = resolve_db_path(&sites[0].to_string_lossy());
    let status = Command::new("sqlite3").arg(&db).status();
    match status {
        Ok(s) => s.code().unwrap_or(0),
        Err(e) => {
            eprintln!("db-console: cannot launch sqlite3: {e}");
            1
        }
    }
}

fn cmd_bypass_patch(g: &GlobalOpts, rest: &[String]) -> i32 {
    let patch = match rest.iter().find(|a| !a.starts_with('-')) {
        Some(p) => p.clone(),
        None => {
            eprintln!("usage: bench --site SITE bypass-patch <patch.module.path>");
            return 2;
        }
    };
    let sites = match require_sites(g) {
        Some(s) => s,
        None => return 1,
    };
    for site in &sites {
        let con = open_conn(&resolve_db_path(&site.to_string_lossy()));
        // already logged?
        let logged: bool = con
            .query_row("SELECT 1 FROM \"tabPatch Log\" WHERE patch=?1", params![patch], |_| Ok(()))
            .is_ok();
        if logged {
            println!("Patch {patch} already in Patch Log for {}", site_name(site));
            continue;
        }
        let now = crate::util::now_datetime();
        let name = crate::util::random_name();
        if let Err(e) = con.execute(
            "INSERT INTO \"tabPatch Log\" (name, creation, modified, modified_by, owner, docstatus, idx, patch, skipped) \
             VALUES (?1, ?2, ?2, 'Administrator', 'Administrator', 0, 0, ?3, 0)",
            params![name, now, patch],
        ) {
            eprintln!("bypass-patch: {e}");
            return 1;
        }
        println!("Marked patch {patch} as run (bypassed) for {}", site_name(site));
    }
    0
}

// =====================================================================================
// Wave 4 — schema sync (reload-doctype / reload-doc)
// =====================================================================================

fn cmd_reload_doctype(g: &GlobalOpts, rest: &[String]) -> i32 {
    let doctype = match rest.iter().find(|a| !a.starts_with('-')) {
        Some(d) => d.clone(),
        None => {
            eprintln!("usage: bench --site SITE reload-doctype <DocType>");
            return 2;
        }
    };
    let sites = match require_sites(g) {
        Some(s) => s,
        None => return 1,
    };
    let root = bench_root(&sites_dir(g));
    for site in &sites {
        let con = open_conn(&resolve_db_path(&site.to_string_lossy()));
        match crate::schema::reload_doctype(&con, &root, &doctype) {
            Ok(()) => println!("Reloaded DocType {doctype} for {}", site_name(site)),
            Err(e) => {
                eprintln!("reload-doctype ({}): {e}", site_name(site));
                return 1;
            }
        }
    }
    0
}

fn cmd_reload_doc(g: &GlobalOpts, rest: &[String]) -> i32 {
    // reload-doc <module> <doctype> <docname>. The common case is `reload-doc <m> doctype <name>`,
    // i.e. reload a DocType definition — identical to reload-doctype <name>.
    let pos: Vec<&String> = rest.iter().filter(|a| !a.starts_with('-')).collect();
    if pos.len() < 3 {
        eprintln!("usage: bench --site SITE reload-doc <module> <doctype> <docname>");
        return 2;
    }
    let (doctype, docname) = (pos[1].as_str(), pos[2].clone());
    if doctype.eq_ignore_ascii_case("doctype") {
        return cmd_reload_doctype(g, std::slice::from_ref(&docname));
    }
    // reloading a non-DocType record (page/report/workspace/…) — delegate to Python for now.
    eprintln!("reload-doc for '{doctype}' records is not native yet; delegating.");
    fallthrough_to_python(&["reload-doc".to_string()]);
}

fn cmd_install_app(g: &GlobalOpts, rest: &[String]) -> i32 {
    let apps: Vec<String> = rest.iter().filter(|a| !a.starts_with('-')).cloned().collect();
    if apps.is_empty() {
        eprintln!("usage: bench --site SITE install-app APP [APP...] [--force]");
        return 2;
    }
    let force = g.force || rest.iter().any(|a| a == "--force");
    let sites = match require_sites(g) {
        Some(s) => s,
        None => return 1,
    };
    let root = bench_root(&sites_dir(g));
    let mut code = 0;
    for site in &sites {
        let con = open_conn(&resolve_db_path(&site.to_string_lossy()));
        for app in &apps {
            match crate::install::install_app(&con, site, &root, app, force) {
                Ok(crate::install::InstallOutcome::Installed(r)) => {
                    println!(
                        "Installed {app} on {}: {} doctype(s) synced, {} unchanged",
                        site_name(site),
                        r.synced,
                        r.skipped
                    );
                }
                Ok(crate::install::InstallOutcome::AlreadyInstalled) => {
                    println!("App {app} already installed on {}", site_name(site));
                }
                Ok(crate::install::InstallOutcome::NotFound) => {
                    eprintln!("App {app} not found under apps/");
                    code = 1;
                }
                Ok(crate::install::InstallOutcome::NeedsPython) => {
                    println!("App {app} declares install hooks — delegating install to Python");
                    let c = python_bench_status(&[
                        "--site".into(),
                        site_name(site),
                        "install-app".into(),
                        app.clone(),
                    ]);
                    if c != 0 {
                        code = c;
                    }
                }
                Err(e) => {
                    eprintln!("install-app {app} ({}): {e}", site_name(site));
                    code = 1;
                }
            }
        }
    }
    code
}

fn cmd_migrate(g: &GlobalOpts, rest: &[String]) -> i32 {
    let native_only = rest.iter().any(|a| a == "--native-only");
    let sites = match require_sites(g) {
        Some(s) => s,
        None => return 1,
    };
    let root = bench_root(&sites_dir(g));
    let mut code = 0;
    for site in &sites {
        println!("Migrating {}", site_name(site));
        let cfg = site.join("site_config.json");
        update_config_file(&cfg, "maintenance_mode", Some(json!(1))).ok();

        let con = open_conn(&resolve_db_path(&site.to_string_lossy()));
        match crate::install::migrate_site(&con, &root) {
            Ok(rep) => {
                let synced: usize = rep.per_app.iter().map(|(_, r)| r.synced).sum();
                let skipped: usize = rep.per_app.iter().map(|(_, r)| r.skipped).sum();
                println!(
                    "  schema: {synced} doctype(s) synced, {skipped} unchanged across {} app(s)",
                    rep.per_app.len()
                );
                if !rep.python_patches.is_empty() {
                    if native_only || !have_bench_python() {
                        println!(
                            "  {} data patch(es) NOT run (need a Python pass):",
                            rep.python_patches.len()
                        );
                        for p in rep.python_patches.iter().take(10) {
                            println!("    - {p}");
                        }
                    } else {
                        println!(
                            "  {} data patch(es) need Python — running `bench migrate` to finish (schema already synced, will be skipped)...",
                            rep.python_patches.len()
                        );
                        // ferro already updated migration_hash, so Python's schema sync is a no-op;
                        // it just runs the patches + after_migrate hooks.
                        let c = python_bench_status(&["--site".into(), site_name(site), "migrate".into()]);
                        if c != 0 {
                            code = c;
                        }
                    }
                }
            }
            Err(e) => {
                eprintln!("migrate ({}): {e}", site_name(site));
                code = 1;
            }
        }

        let _ = fs::write(site.join("locks").join("ferro_cache_flush"), crate::util::now_datetime());
        update_config_file(&cfg, "maintenance_mode", Some(json!(0))).ok();
        println!();
    }
    code
}

fn cmd_new_site(g: &GlobalOpts, rest: &[String]) -> i32 {
    let positional: Vec<&String> = rest.iter().filter(|a| !a.starts_with('-')).collect();
    let site = match positional.first() {
        Some(s) => (*s).clone(),
        None => {
            eprintln!("usage: bench new-site SITE [--admin-password PW] [--db-name NAME] [--set-default] [--force]");
            return 2;
        }
    };
    let admin_password = opt_value(rest, &["--admin-password"]).unwrap_or_else(|| "admin".to_string());
    let db_name = opt_value(rest, &["--db-name"]);
    let set_default = rest.iter().any(|a| a == "--set-default");
    let force = g.force || rest.iter().any(|a| a == "--force");

    let sd = sites_dir(g);
    let opts = crate::newsite::NewSiteOpts { admin_password, force, set_default, db_name };
    match crate::newsite::new_site(&sd, &site, &opts) {
        Ok(()) => {
            println!("Site {site} created.");
            println!("  Administrator password set; site available at sites/{site}");
            0
        }
        Err(e) => {
            eprintln!("new-site: {e}");
            1
        }
    }
}

// =====================================================================================
// Wave 5 — db ops + user management (native)
// =====================================================================================

fn cmd_disable_user(g: &GlobalOpts, rest: &[String]) -> i32 {
    let users: Vec<String> = rest.iter().filter(|a| !a.starts_with('-')).cloned().collect();
    if users.is_empty() {
        eprintln!("usage: bench --site SITE disable-user USER [USER...]");
        return 2;
    }
    let sites = match require_sites(g) {
        Some(s) => s,
        None => return 1,
    };
    for site in &sites {
        let con = open_conn(&resolve_db_path(&site.to_string_lossy()));
        for user in &users {
            if user == "Administrator" {
                eprintln!("refusing to disable Administrator");
                continue;
            }
            match con.execute("UPDATE tabUser SET enabled=0 WHERE name=?1", params![user]) {
                Ok(1) => println!("Disabled {user} on {}", site_name(site)),
                Ok(_) => println!("User {user} not found on {}", site_name(site)),
                Err(e) => {
                    eprintln!("disable-user: {e}");
                    return 1;
                }
            }
        }
    }
    0
}

fn cmd_describe_table(g: &GlobalOpts, rest: &[String]) -> i32 {
    let doctype = match opt_value(rest, &["--doctype"]).or_else(|| rest.iter().find(|a| !a.starts_with('-')).cloned()) {
        Some(d) => d,
        None => {
            eprintln!("usage: bench --site SITE describe-database-table --doctype DOCTYPE");
            return 2;
        }
    };
    let sites = match require_sites(g) {
        Some(s) => s,
        None => return 1,
    };
    let table = format!("tab{doctype}");
    for site in &sites {
        let con = open_conn(&resolve_db_path(&site.to_string_lossy()));
        println!("Table: {table}");
        let mut stmt = match con.prepare(&format!("PRAGMA table_info({})", crate::meta::quote_ident(&table))) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("describe-database-table: {e}");
                return 1;
            }
        };
        let rows = stmt.query_map([], |r| {
            Ok((r.get::<_, String>(1)?, r.get::<_, String>(2)?, r.get::<_, i64>(3)?, r.get::<_, Option<String>>(4)?, r.get::<_, i64>(5)?))
        });
        if let Ok(rows) = rows {
            println!("  columns:");
            for row in rows.flatten() {
                let (name, ty, notnull, dflt, pk) = row;
                println!(
                    "    {name:24} {ty:10}{}{}{}",
                    if notnull != 0 { " NOT NULL" } else { "" },
                    dflt.map(|d| format!(" DEFAULT {d}")).unwrap_or_default(),
                    if pk != 0 { " PRIMARY KEY" } else { "" },
                );
            }
        }
        let index_stmt = con.prepare(&format!("PRAGMA index_list({})", crate::meta::quote_ident(&table)));
        if let Ok(mut s2) = index_stmt {
            if let Ok(rows) = s2.query_map([], |r| Ok((r.get::<_, String>(1)?, r.get::<_, i64>(2)?))) {
                println!("  indexes:");
                for (idx, uniq) in rows.flatten() {
                    println!("    {idx}{}", if uniq != 0 { " (unique)" } else { "" });
                }
            }
        }
    }
    0
}

fn cmd_add_database_index(g: &GlobalOpts, rest: &[String]) -> i32 {
    let doctype = match opt_value(rest, &["--doctype"]) {
        Some(d) => d,
        None => {
            eprintln!("usage: bench --site SITE add-database-index --doctype DOCTYPE --column col[,col]");
            return 2;
        }
    };
    let columns = match opt_value(rest, &["--column", "--columns"]) {
        Some(c) => c,
        None => {
            eprintln!("add-database-index: --column required");
            return 2;
        }
    };
    let cols: Vec<&str> = columns.split(',').map(|s| s.trim()).filter(|s| !s.is_empty()).collect();
    let sites = match require_sites(g) {
        Some(s) => s,
        None => return 1,
    };
    let table = format!("tab{doctype}");
    let idx_name = format!("{}_index", cols.join("_"));
    let quoted_cols: Vec<String> = cols.iter().map(|c| crate::meta::quote_ident(c)).collect();
    for site in &sites {
        let con = open_conn(&resolve_db_path(&site.to_string_lossy()));
        let sql = format!(
            "CREATE INDEX IF NOT EXISTS {idx} ON {tbl} ({cols})",
            idx = crate::meta::quote_ident(&idx_name),
            tbl = crate::meta::quote_ident(&table),
            cols = quoted_cols.join(", "),
        );
        match con.execute_batch(&sql) {
            Ok(()) => println!("Index {idx_name} on {table}({}) ensured for {}", cols.join(", "), site_name(site)),
            Err(e) => {
                eprintln!("add-database-index: {e}");
                return 1;
            }
        }
    }
    0
}

fn cmd_trim_database(g: &GlobalOpts, rest: &[String]) -> i32 {
    // Drop orphan tab<X> tables that have no DocType row. Safe-by-default: lists unless --yes.
    let apply = rest.iter().any(|a| a == "--yes" || a == "--no-dry-run");
    // tables we must never drop even though they have no DocType row
    const KEEP: &[&str] = &["tabSingles", "tabSeries", "tabDeferred Insert", "tabSessions"];
    let sites = match require_sites(g) {
        Some(s) => s,
        None => return 1,
    };
    for site in &sites {
        let con = open_conn(&resolve_db_path(&site.to_string_lossy()));
        let doctypes: std::collections::HashSet<String> = {
            let mut set = std::collections::HashSet::new();
            if let Ok(mut s) = con.prepare("SELECT name FROM \"tabDocType\"") {
                if let Ok(rows) = s.query_map([], |r| r.get::<_, String>(0)) {
                    for n in rows.flatten() {
                        set.insert(format!("tab{n}"));
                    }
                }
            }
            set
        };
        let mut orphans = Vec::new();
        if let Ok(mut s) = con.prepare("SELECT name FROM sqlite_master WHERE type='table' AND name LIKE 'tab%'") {
            if let Ok(rows) = s.query_map([], |r| r.get::<_, String>(0)) {
                for t in rows.flatten() {
                    if !doctypes.contains(&t) && !KEEP.contains(&t.as_str()) {
                        orphans.push(t);
                    }
                }
            }
        }
        if orphans.is_empty() {
            println!("trim-database: no orphan tables on {}", site_name(site));
            continue;
        }
        if !apply {
            println!("trim-database (DRY RUN — pass --yes to drop) — {} orphan table(s) on {}:", orphans.len(), site_name(site));
            for t in &orphans {
                println!("  {t}");
            }
        } else {
            for t in &orphans {
                if let Err(e) = con.execute_batch(&format!("DROP TABLE {}", crate::meta::quote_ident(t))) {
                    eprintln!("trim-database: failed to drop {t}: {e}");
                } else {
                    println!("dropped {t}");
                }
            }
        }
    }
    0
}

fn cmd_remove_from_installed_apps(g: &GlobalOpts, rest: &[String]) -> i32 {
    let app = match rest.iter().find(|a| !a.starts_with('-')) {
        Some(a) => a.clone(),
        None => {
            eprintln!("usage: bench --site SITE remove-from-installed-apps APP");
            return 2;
        }
    };
    let sites = match require_sites(g) {
        Some(s) => s,
        None => return 1,
    };
    for site in &sites {
        let con = open_conn(&resolve_db_path(&site.to_string_lossy()));
        let mut apps = installed_apps(&con);
        let before = apps.len();
        apps.retain(|a| a != &app);
        if apps.len() == before {
            println!("App {app} is not in installed_apps on {}", site_name(site));
            continue;
        }
        let json = serde_json::to_string(&apps).unwrap();
        con.execute(
            "UPDATE tabDefaultValue SET defvalue=?1 WHERE defkey='installed_apps' AND parent='__global'",
            params![json],
        )
        .ok();
        // mirror to site_config
        let cfg_path = site.join("site_config.json");
        if let Ok(text) = fs::read_to_string(&cfg_path) {
            if let Ok(mut cfg) = serde_json::from_str::<Map<String, Value>>(&text) {
                cfg.insert("installed_apps".to_string(), json!(apps));
                let mut s = String::new();
                json_frappe(&Value::Object(cfg), 0, &mut s);
                let _ = fs::write(&cfg_path, s);
            }
        }
        println!("Removed {app} from installed apps on {} (tables left intact)", site_name(site));
    }
    0
}

fn cmd_ready_for_migration(g: &GlobalOpts) -> i32 {
    // ferro hosts background jobs in-process inside `ferro serve`; there is no separate durable
    // queue for a CLI to drain. A ferro deploy gates migrate on the switch/restart, so report ready.
    let sites = resolve_sites(g);
    let name = sites.first().map(|s| site_name(s)).unwrap_or_default();
    println!("READY for migration: site {name} (ferro runs jobs in-process; no external queue)");
    0
}

// =====================================================================================
// fallthrough — anything ferro does not implement natively runs through the real Python helper
// =====================================================================================

fn fallthrough_to_python(args: &[String]) -> ! {
    eprintln!("ferro bench: '{}' not implemented natively — delegating to Python", args.join(" "));
    std::process::exit(python_bench_status(args));
}

/// The bench's python: `<bench>/env/bin/python`, else `python3` on PATH.
fn bench_python() -> String {
    let sd = if Path::new("sites").is_dir() {
        PathBuf::from("sites")
    } else {
        PathBuf::from(".")
    };
    let py = bench_root(&sd).join("env").join("bin").join("python");
    if py.is_file() {
        py.to_string_lossy().into_owned()
    } else {
        "python3".to_string()
    }
}

fn have_bench_python() -> bool {
    let sd = if Path::new("sites").is_dir() {
        PathBuf::from("sites")
    } else {
        PathBuf::from(".")
    };
    bench_root(&sd).join("env").join("bin").join("python").is_file()
}

/// Run `python -m frappe.utils.bench_helper frappe <args>` and return its exit code (does NOT exit).
fn python_bench_status(args: &[String]) -> i32 {
    let py = bench_python();
    match Command::new(&py)
        .arg("-m")
        .arg("frappe.utils.bench_helper")
        .arg("frappe")
        .args(args)
        .status()
    {
        Ok(s) => s.code().unwrap_or(1),
        Err(e) => {
            eprintln!("python delegate failed: {e}");
            127
        }
    }
}

// =====================================================================================
// small helpers
// =====================================================================================

/// Find the value following any of `names` in `rest` (e.g. `-f json` / `--format json`).
fn opt_value(rest: &[String], names: &[&str]) -> Option<String> {
    let mut it = rest.iter();
    while let Some(a) = it.next() {
        if names.contains(&a.as_str()) {
            return it.next().cloned();
        }
        // also accept --format=json
        for n in names {
            if let Some(v) = a.strip_prefix(&format!("{n}=")) {
                return Some(v.to_string());
            }
        }
    }
    None
}
