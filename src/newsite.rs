//! new-site — create a fresh SQLite Frappe site, 100% native (no Python interpreter).
//!
//! For SQLite the doctype-sync work of install_app("frappe") is frozen into a pre-baked seed DB
//! (`framework/seed/core.db.gz`: all core tables/doctypes/singles/roles/Admin+Guest). So new-site =
//! make dirs + write site_config + decompress the seed + reset THIS site's Administrator password
//! and API key. No DDL generation, no doctype JSON parsing, no controllers. (Installing non-frappe
//! apps afterwards is the separate `install-app` path.)

use crate::util::random_name;
use rusqlite::{params, Connection};
use std::path::{Path, PathBuf};
use std::process::Command;

pub struct NewSiteOpts {
    pub admin_password: String,
    pub force: bool,
    pub set_default: bool,
    pub db_name: Option<String>,
}

/// Locate the frappe-core seed: $FERRO_SEED, else next to the binary, else repo-relative candidates.
fn find_seed() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("FERRO_SEED") {
        let p = PathBuf::from(p);
        if p.is_file() {
            return Some(p);
        }
    }
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        // target/release/ferro -> repo root is two levels up
        if let Some(rel) = exe.parent().and_then(|d| d.parent()).and_then(|d| d.parent()) {
            candidates.push(rel.join("framework/seed/core.db.gz"));
        }
        if let Some(dir) = exe.parent() {
            candidates.push(dir.join("core.db.gz"));
        }
    }
    candidates.push(PathBuf::from("framework/seed/core.db.gz"));
    candidates.into_iter().find(|p| p.is_file())
}

/// A fresh 44-char Fernet-shaped key (urlsafe-base64 of 32 random bytes), like Fernet.generate_key().
fn fresh_encryption_key() -> String {
    crate::crypto::b64url_encode(&crate::util::random_bytes(32))
}

pub fn new_site(sites_dir: &Path, site: &str, opts: &NewSiteOpts) -> Result<(), String> {
    let site_dir = sites_dir.join(site);
    if site_dir.exists() && !opts.force {
        return Err(format!("site {site} already exists (use --force to overwrite)"));
    }
    let seed = find_seed().ok_or_else(|| {
        "frappe-core seed not found (set FERRO_SEED=/path/to/core.db.gz)".to_string()
    })?;

    // 1. directory tree
    for sub in ["db", "public/files", "private/backups", "private/files", "locks", "logs"] {
        std::fs::create_dir_all(site_dir.join(sub)).map_err(|e| e.to_string())?;
    }

    // 2. db name + site_config.json
    let db_name = opts.db_name.clone().unwrap_or_else(|| format!("_{}{}", random_name(), random_name()));
    let db_name = db_name.chars().take(17).collect::<String>(); // "_" + 16 hex
    let enc_key = fresh_encryption_key();
    let cfg = serde_json::json!({
        "db_type": "sqlite",
        "db_name": db_name,
        "encryption_key": enc_key,
        "installed_apps": ["frappe"],
    });
    let mut cfg_str = String::new();
    crate::bench::write_config_json(&cfg, &mut cfg_str);
    std::fs::write(site_dir.join("site_config.json"), cfg_str).map_err(|e| e.to_string())?;

    // 3. materialise the DB by decompressing the seed (this is the whole bootstrap_database step)
    let db_path = site_dir.join("db").join(format!("{db_name}.db"));
    gunzip_to(&seed, &db_path)?;

    // 4. reset THIS site's Administrator credentials (seed ships shared baked ones)
    let con = Connection::open(&db_path).map_err(|e| e.to_string())?;
    let hash = crate::crypto::passlib_pbkdf2_sha256_hash(&opts.admin_password);
    con.execute(
        "INSERT OR REPLACE INTO \"__Auth\" (doctype, name, fieldname, password, encrypted) \
         VALUES ('User', 'Administrator', 'password', ?1, 0)",
        params![hash],
    )
    .map_err(|e| e.to_string())?;
    // fresh API key/secret so sites don't share the seed's baked pair
    crate::auth::provision_key(&con, "Administrator").map_err(|e| e.to_string())?;
    drop(con);

    // 5. optional default site
    if opts.set_default {
        let common = sites_dir.join("common_site_config.json");
        let mut map: serde_json::Map<String, serde_json::Value> = std::fs::read_to_string(&common)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default();
        map.insert("default_site".to_string(), serde_json::Value::String(site.to_string()));
        let mut s = String::new();
        crate::bench::write_config_json(&serde_json::Value::Object(map), &mut s);
        let _ = std::fs::write(&common, s);
    }

    Ok(())
}

fn gunzip_to(src: &Path, dst: &Path) -> Result<(), String> {
    let out = std::fs::File::create(dst).map_err(|e| e.to_string())?;
    let status = Command::new("gzip")
        .arg("-dc")
        .arg(src)
        .stdout(out)
        .status()
        .map_err(|e| format!("gzip: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err("seed decompression failed".into())
    }
}
