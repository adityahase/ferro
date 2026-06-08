//! install-app / migrate — the app lifecycle, built on the `schema` DDL engine.
//!
//! The schema spine (module defs + doctype CREATE/ALTER + installed_apps + patch log) is pure Rust
//! and runs natively for any app. The Python tail — before/after_install hooks, data patches,
//! fixtures with controllers, after_migrate hooks — cannot run without an interpreter, so:
//!   - `install-app`: if the app's hooks.py declares install-time hooks, the whole command is
//!     delegated to the real Python helper (correct + safe). Pure-CRUD apps install natively.
//!   - `migrate`: the native schema sync (which subsumes every `reload_doc` patch) runs for all
//!     installed apps; data patches / after_migrate hooks are reported for a Python pass.

use crate::schema::{self, SchemaError};
use crate::util::now_datetime;
use rusqlite::{params, Connection};
use serde_json::Value;
use std::path::{Path, PathBuf};

pub struct SyncReport {
    pub synced: usize,
    pub skipped: usize,
}

fn app_root(bench_root: &Path, app: &str) -> PathBuf {
    bench_root.join("apps").join(app).join(app)
}

/// Modules listed in the app's modules.txt.
fn app_modules(bench_root: &Path, app: &str) -> Vec<String> {
    std::fs::read_to_string(app_root(bench_root, app).join("modules.txt"))
        .map(|t| t.lines().map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).collect())
        .unwrap_or_default()
}

/// All `<module>/doctype/<slug>/<slug>.json` DocType definition files under the app.
fn app_doctype_files(bench_root: &Path, app: &str) -> Vec<PathBuf> {
    let mut out = Vec::new();
    collect_doctype_files(&app_root(bench_root, app), &mut out, 6);
    out.sort();
    out
}

fn collect_doctype_files(dir: &Path, out: &mut Vec<PathBuf>, depth: usize) {
    if depth == 0 {
        return;
    }
    let rd = match std::fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(_) => return,
    };
    for e in rd.flatten() {
        let p = e.path();
        if !p.is_dir() {
            continue;
        }
        if p.file_name().map(|n| n == "doctype").unwrap_or(false) {
            // each <slug>/<slug>.json under a doctype/ dir is a DocType definition
            if let Ok(slugs) = std::fs::read_dir(&p) {
                for s in slugs.flatten() {
                    let sp = s.path();
                    if sp.is_dir() {
                        if let Some(name) = sp.file_name().and_then(|n| n.to_str()) {
                            let json = sp.join(format!("{name}.json"));
                            if json.is_file() {
                                out.push(json);
                            }
                        }
                    }
                }
            }
        } else {
            collect_doctype_files(&p, out, depth - 1);
        }
    }
}

fn ensure_module_def(con: &Connection, module: &str, app: &str) -> Result<(), SchemaError> {
    let exists: bool = con
        .query_row("SELECT 1 FROM \"tabModule Def\" WHERE name=?1", params![module], |_| Ok(()))
        .is_ok();
    if !exists {
        let now = now_datetime();
        con.execute(
            "INSERT INTO \"tabModule Def\" (name, creation, modified, modified_by, owner, docstatus, idx, module_name, app_name) \
             VALUES (?1, ?2, ?2, 'Administrator', 'Administrator', 0, 0, ?1, ?3)",
            params![module, now, app],
        )?;
    }
    Ok(())
}

/// Ensure a Role exists (referenced by DocPerm). Minimal row; desk_access defaulted by the table.
fn ensure_role(con: &Connection, role: &str) -> Result<(), SchemaError> {
    if role.is_empty() {
        return Ok(());
    }
    let exists: bool = con
        .query_row("SELECT 1 FROM \"tabRole\" WHERE name=?1", params![role], |_| Ok(()))
        .is_ok();
    if !exists {
        let now = now_datetime();
        con.execute(
            "INSERT INTO \"tabRole\" (name, creation, modified, modified_by, owner, docstatus, idx, role_name) \
             VALUES (?1, ?2, ?2, 'Administrator', 'Administrator', 0, 0, ?1)",
            params![role, now],
        )?;
    }
    Ok(())
}

/// Sync one app's schema: module defs + every DocType (md5-gated import + table sync). This is the
/// shared core of install-app and migrate. `reset_permissions` controls whether DocPerm rows are
/// replaced from JSON (install) or preserved (migrate).
pub fn sync_app(
    con: &Connection,
    bench_root: &Path,
    app: &str,
    reset_permissions: bool,
    force: bool,
) -> Result<SyncReport, SchemaError> {
    for m in app_modules(bench_root, app) {
        ensure_module_def(con, &m, app)?;
    }
    let mut synced = 0usize;
    let mut skipped = 0usize;
    for file in app_doctype_files(bench_root, app) {
        let bytes = match std::fs::read(&file) {
            Ok(b) => b,
            Err(_) => continue,
        };
        let doc: Value = match serde_json::from_slice(&bytes) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let name = match doc.get("name").and_then(|v| v.as_str()) {
            Some(n) => n.to_string(),
            None => continue,
        };
        let file_hash = crate::crypto::md5_hex(&bytes);

        // md5 gate: skip an unchanged doctype whose table already exists.
        if !force {
            let stored: Option<String> = con
                .query_row(
                    "SELECT migration_hash FROM \"tabDocType\" WHERE name=?1",
                    params![name],
                    |r| r.get::<_, Option<String>>(0),
                )
                .ok()
                .flatten();
            if stored.as_deref() == Some(file_hash.as_str()) {
                skipped += 1;
                continue;
            }
        }

        schema::import_doctype(con, &doc, reset_permissions)?;
        // ensure roles referenced by the doctype's permissions exist
        if let Some(perms) = doc.get("permissions").and_then(|v| v.as_array()) {
            for perm in perms {
                if let Some(role) = perm.get("role").and_then(|v| v.as_str()) {
                    ensure_role(con, role)?;
                }
            }
        }
        schema::sync_table(con, &name)?;
        con.execute(
            "UPDATE \"tabDocType\" SET migration_hash=?2 WHERE name=?1",
            params![name, file_hash],
        )?;
        synced += 1;
    }
    Ok(SyncReport { synced, skipped })
}

/// Does the app's hooks.py declare any install-time hook that needs Python to run?
pub fn hooks_need_python(bench_root: &Path, app: &str) -> bool {
    let hooks = app_root(bench_root, app).join("hooks.py");
    let text = match std::fs::read_to_string(hooks) {
        Ok(t) => t,
        Err(_) => return false,
    };
    const KEYS: &[&str] = &[
        "before_install",
        "after_install",
        "after_sync",
        "before_app_install",
        "after_app_install",
        "required_apps",
        "fixtures",
    ];
    for line in text.lines() {
        let l = line.trim_start();
        for k in KEYS {
            // an assignment like `after_install = "..."` (ignore comments / hook *names* in strings)
            if l.starts_with(k) {
                let rest = l[k.len()..].trim_start();
                if rest.starts_with('=') && !rest.starts_with("==") {
                    return true;
                }
            }
        }
    }
    false
}

/// Append `app` to installed_apps (tabDefaultValue/__global JSON list) and to site_config.json.
fn add_to_installed_apps(con: &Connection, site_dir: &Path, app: &str) -> Result<(), SchemaError> {
    // DB: tabDefaultValue(defkey='installed_apps', parent='__global')
    let raw: Option<String> = con
        .query_row(
            "SELECT defvalue FROM tabDefaultValue WHERE defkey='installed_apps' AND parent='__global'",
            [],
            |r| r.get(0),
        )
        .ok();
    let mut apps: Vec<String> = raw
        .as_deref()
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or_default();
    if !apps.iter().any(|a| a == app) {
        apps.push(app.to_string());
    }
    let json = serde_json::to_string(&apps).unwrap();
    let updated = con.execute(
        "UPDATE tabDefaultValue SET defvalue=?1 WHERE defkey='installed_apps' AND parent='__global'",
        params![json],
    )?;
    if updated == 0 {
        let now = now_datetime();
        con.execute(
            "INSERT INTO tabDefaultValue (name, creation, modified, modified_by, owner, docstatus, idx, parent, parenttype, parentfield, defkey, defvalue) \
             VALUES (?1, ?2, ?2, 'Administrator', 'Administrator', 0, 0, '__global', 'DefaultValue', 'system_defaults', 'installed_apps', ?3)",
            params![format!("__global_installed_apps"), now, json],
        )?;
    }

    // site_config.json installed_apps (what `ferro serve` reads) — mirror the full DB list.
    let cfg_path = site_dir.join("site_config.json");
    if let Ok(text) = std::fs::read_to_string(&cfg_path) {
        if let Ok(mut cfg) = serde_json::from_str::<serde_json::Map<String, Value>>(&text) {
            cfg.insert("installed_apps".to_string(), serde_json::json!(apps));
            let mut s = String::new();
            crate::bench::write_config_json(&Value::Object(cfg), &mut s);
            let _ = std::fs::write(&cfg_path, s);
        }
    }
    Ok(())
}

/// Mark every patch in the app's patches.txt as completed (without running them) — the install-app
/// contract (set_all_patches_as_completed), so a fresh install doesn't re-run historical patches.
fn set_all_patches_as_completed(con: &Connection, bench_root: &Path, app: &str) -> Result<(), SchemaError> {
    for patch in app_patches(bench_root, app) {
        let logged: bool = con
            .query_row("SELECT 1 FROM \"tabPatch Log\" WHERE patch=?1", params![patch], |_| Ok(()))
            .is_ok();
        if !logged {
            let now = now_datetime();
            con.execute(
                "INSERT INTO \"tabPatch Log\" (name, creation, modified, modified_by, owner, docstatus, idx, patch, skipped) \
                 VALUES (?1, ?2, ?2, 'Administrator', 'Administrator', 0, 0, ?3, 0)",
                params![crate::util::random_name(), now, patch],
            )?;
        }
    }
    Ok(())
}

/// Parse patches.txt into patch identifiers (drop section headers, comments, blanks).
fn app_patches(bench_root: &Path, app: &str) -> Vec<String> {
    let text = match std::fs::read_to_string(app_root(bench_root, app).join("patches.txt")) {
        Ok(t) => t,
        Err(_) => return Vec::new(),
    };
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with('[') {
            continue;
        }
        out.push(line.to_string());
    }
    out
}

pub enum InstallOutcome {
    Installed(SyncReport),
    AlreadyInstalled,
    NeedsPython,
    NotFound,
}

/// Native install-app. Returns NeedsPython if the app declares install-time hooks (caller delegates).
pub fn install_app(
    con: &Connection,
    site_dir: &Path,
    bench_root: &Path,
    app: &str,
    force: bool,
) -> Result<InstallOutcome, SchemaError> {
    if !app_root(bench_root, app).join("hooks.py").exists()
        && !app_root(bench_root, app).is_dir()
    {
        return Ok(InstallOutcome::NotFound);
    }
    // already installed?
    let installed = crate::bench::installed_apps_of(con);
    if !force && installed.iter().any(|a| a == app) {
        return Ok(InstallOutcome::AlreadyInstalled);
    }
    if hooks_need_python(bench_root, app) {
        return Ok(InstallOutcome::NeedsPython);
    }
    let report = sync_app(con, bench_root, app, true, force)?;
    add_to_installed_apps(con, site_dir, app)?;
    set_all_patches_as_completed(con, bench_root, app)?;
    Ok(InstallOutcome::Installed(report))
}

pub struct MigrateReport {
    pub per_app: Vec<(String, SyncReport)>,
    pub python_patches: Vec<String>,
}

/// Native migrate: schema-sync every installed app (md5-gated). The full schema sync subsumes every
/// `reload_doc` patch; remaining data patches / after_migrate hooks are returned for a Python pass.
pub fn migrate_site(con: &Connection, bench_root: &Path) -> Result<MigrateReport, SchemaError> {
    let apps = crate::bench::installed_apps_of(con);
    let mut per_app = Vec::new();
    let mut python_patches = Vec::new();
    for app in &apps {
        let report = sync_app(con, bench_root, app, false, false)?;
        per_app.push((app.clone(), report));
        // classify unrun patches: reload_doc/reload_doctype are covered by the schema sync above;
        // anything else is a data patch that needs Python.
        for patch in app_patches(bench_root, app) {
            let logged: bool = con
                .query_row("SELECT 1 FROM \"tabPatch Log\" WHERE patch=?1", params![patch], |_| Ok(()))
                .is_ok();
            if logged {
                continue;
            }
            let is_reload = patch.contains("reload_doc") || patch.contains("reload_doctype");
            if !is_reload {
                python_patches.push(patch);
            }
        }
    }
    Ok(MigrateReport { per_app, python_patches })
}
