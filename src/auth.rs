//! Authentication + a minimal-but-real permission gate.
//!
//! Token auth mirrors Frappe: `Authorization: token <api_key>:<api_secret>`. The key lives
//! in `tabUser.api_key`; the secret in `__Auth` (doctype='User', fieldname='api_secret').
//! Frappe stores the secret Fernet-encrypted (encrypted=1) using the site key; provisioning
//! via `ferro provision-key` stores it as plaintext (encrypted=0) so the full path works
//! without a crypto dependency. Decryption of encrypted=1 secrets is intentionally not done.
//!
//! Permissions are checked against `tabDocPerm` joined with the user's roles
//! (`tabHas Role` + the implicit "All"/"Guest" roles), exactly as Frappe gates at permlevel 0.

use rusqlite::Connection;

pub struct Identity {
    pub user: String,
    pub via: &'static str, // "token" | "guest" | "default"
}

/// Resolve the request's user from the Authorization header.
pub fn resolve_user(con: &Connection, auth_header: Option<&str>, default_user: &str) -> Identity {
    if let Some(h) = auth_header {
        let h = h.trim();
        if let Some(rest) = h.strip_prefix("token ").or_else(|| h.strip_prefix("Token ")) {
            if let Some((key, secret)) = rest.split_once(':') {
                if let Some(user) = verify_token(con, key.trim(), secret.trim()) {
                    return Identity { user, via: "token" };
                }
            }
        }
        // Unrecognized / failed auth -> Guest (Frappe treats bad creds as anonymous for reads).
        return Identity { user: "Guest".to_string(), via: "guest" };
    }
    Identity { user: default_user.to_string(), via: "default" }
}

fn verify_token(con: &Connection, key: &str, secret: &str) -> Option<String> {
    if key.is_empty() {
        return None;
    }
    let user: String = con
        .query_row(
            "SELECT name FROM \"tabUser\" WHERE api_key=?1 AND COALESCE(enabled,1)=1",
            [key],
            |r| r.get(0),
        )
        .ok()?;
    let (stored, encrypted): (String, i64) = con
        .query_row(
            "SELECT password, COALESCE(encrypted,0) FROM \"__Auth\" \
             WHERE doctype='User' AND name=?1 AND fieldname='api_secret'",
            [&user],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .ok()?;
    if encrypted == 0 && constant_time_eq(stored.as_bytes(), secret.as_bytes()) {
        Some(user)
    } else {
        None
    }
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// Permission types mapped to the tabDocPerm columns.
pub fn ptype_for_method(method: &str) -> &'static str {
    match method {
        "GET" | "HEAD" => "read",
        "POST" => "create",
        "PUT" | "PATCH" => "write",
        "DELETE" => "delete",
        _ => "read",
    }
}

/// Does `user` have `ptype` permission on `doctype` at permlevel 0?
pub fn can(con: &Connection, user: &str, doctype: &str, ptype: &str) -> bool {
    if user == "Administrator" {
        return true;
    }
    // ptype must be one of a known set (it is, from ptype_for_method) — safe to interpolate.
    let col = match ptype {
        "read" | "write" | "create" | "delete" | "submit" | "cancel" | "report" | "export" => ptype,
        _ => "read",
    };

    // Collect the user's roles (+ implicit ones).
    let mut roles: Vec<String> = vec!["All".to_string()];
    if user == "Guest" {
        roles.push("Guest".to_string());
    }
    if let Ok(mut stmt) =
        con.prepare("SELECT role FROM \"tabHas Role\" WHERE parent=?1 AND parenttype='User'")
    {
        if let Ok(rows) = stmt.query_map([user], |r| r.get::<_, String>(0)) {
            for r in rows.flatten() {
                roles.push(r);
            }
        }
    }

    let placeholders = vec!["?"; roles.len()].join(",");
    let sql = format!(
        "SELECT COUNT(*) FROM \"tabDocPerm\" \
         WHERE parent=? AND COALESCE(permlevel,0)=0 AND COALESCE(\"{col}\",0)=1 \
         AND role IN ({placeholders})"
    );
    let mut binds: Vec<&dyn rusqlite::ToSql> = Vec::with_capacity(roles.len() + 1);
    binds.push(&doctype);
    for r in &roles {
        binds.push(r);
    }
    con.query_row(&sql, binds.as_slice(), |r| r.get::<_, i64>(0))
        .map(|n| n > 0)
        .unwrap_or(false)
}

/// Provision an API key pair for a user (sets tabUser.api_key, upserts __Auth secret as plaintext).
pub fn provision_key(con: &Connection, user: &str) -> Result<(String, String), rusqlite::Error> {
    let key = crate::util::random_name();
    let secret = format!("{}{}", crate::util::random_name(), crate::util::random_name());
    con.execute("UPDATE \"tabUser\" SET api_key=?1 WHERE name=?2", rusqlite::params![key, user])?;
    con.execute(
        "DELETE FROM \"__Auth\" WHERE doctype='User' AND name=?1 AND fieldname='api_secret'",
        [user],
    )?;
    con.execute(
        "INSERT INTO \"__Auth\" (doctype, name, fieldname, password, encrypted) \
         VALUES ('User', ?1, 'api_secret', ?2, 0)",
        rusqlite::params![user, secret],
    )?;
    Ok((key, secret))
}
