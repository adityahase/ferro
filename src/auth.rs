//! Authentication + permission gate, mirroring Frappe's REST data-plane behavior.
//!
//! Token auth: `Authorization: token <api_key>:<api_secret>` (also accepted as
//! `Authorization: Basic base64(api_key:api_secret)`). The key lives in `tabUser.api_key`;
//! the secret in `__Auth` (doctype='User', fieldname='api_secret'). Frappe stores that secret
//! Fernet-encrypted (encrypted=1) with the site `encryption_key`; ferro decrypts it (see crypto.rs)
//! and constant-time compares — so tokens issued by a normal Frappe site work. Plaintext secrets
//! (encrypted=0, as written by `ferro provision-key`) are also accepted.
//!
//! Permissions mirror `frappe.permissions` for the REST surface: doctype-level grants from
//! `tabDocPerm` (or `tabCustom DocPerm` when the doctype is customized) joined with the user's
//! roles (+ implicit "All"/"Guest"), with `if_owner` row scoping and `permlevel` field scoping.

use crate::crypto;
use crate::meta::Meta;
use crate::util;
use rusqlite::Connection;
use std::collections::HashSet;

pub struct Identity {
    pub user: String,
    #[allow(dead_code)]
    pub via: &'static str, // "token" | "basic" | "default"
}

/// Result of resolving the request's identity.
pub enum AuthOutcome {
    /// A concrete identity (authenticated token/basic, or the unauthenticated default user).
    Ok(Identity),
    /// Credentials were supplied but are invalid — caller must return HTTP 401.
    Unauthorized,
}

/// Resolve the request's user from the Authorization header.
/// No header  -> default user (Guest for the server).
/// Valid token/basic -> that user.
/// Present-but-invalid credentials -> Unauthorized (401), matching Frappe's AuthenticationError.
pub fn resolve_user(
    con: &Connection,
    auth_header: Option<&str>,
    default_user: &str,
    enc_key: Option<&str>,
) -> AuthOutcome {
    resolve_user_session(con, auth_header, None, default_user, enc_key)
}

/// Like `resolve_user`, but also honors a `sid` session cookie (FIX-2): with no Authorization
/// header, a valid non-expired `tabSessions` row resolves to its user; an absent/invalid/expired
/// sid falls through to `default_user` (an unauthenticated request, not a 401).
pub fn resolve_user_session(
    con: &Connection,
    auth_header: Option<&str>,
    sid: Option<&str>,
    default_user: &str,
    enc_key: Option<&str>,
) -> AuthOutcome {
    let h = match auth_header {
        Some(h) if !h.trim().is_empty() => h.trim(),
        _ => {
            // No token/basic header: try the session cookie before the default user.
            if let Some(sid) = sid.map(str::trim).filter(|s| !s.is_empty() && *s != "Guest") {
                if let Some(user) = user_for_sid(con, sid) {
                    return AuthOutcome::Ok(Identity { user, via: "session" });
                }
            }
            return AuthOutcome::Ok(Identity {
                user: default_user.to_string(),
                via: "default",
            });
        }
    };

    // token <key>:<secret>
    if let Some(rest) = h.strip_prefix("token ").or_else(|| h.strip_prefix("Token ")) {
        if let Some((key, secret)) = rest.split_once(':') {
            if let Some(user) = verify_token(con, key.trim(), secret.trim(), enc_key) {
                return AuthOutcome::Ok(Identity { user, via: "token" });
            }
        }
        return AuthOutcome::Unauthorized;
    }

    // Basic base64(key:secret) — the API-key-over-Basic form Frappe accepts.
    if let Some(rest) = h.strip_prefix("Basic ").or_else(|| h.strip_prefix("basic ")) {
        if let Some(decoded) = crypto::b64url_decode_std(rest.trim()) {
            if let Ok(s) = String::from_utf8(decoded) {
                if let Some((key, secret)) = s.split_once(':') {
                    if let Some(user) = verify_token(con, key.trim(), secret.trim(), enc_key) {
                        return AuthOutcome::Ok(Identity { user, via: "basic" });
                    }
                }
            }
        }
        return AuthOutcome::Unauthorized;
    }

    // Unknown auth scheme presented.
    AuthOutcome::Unauthorized
}

fn verify_token(con: &Connection, key: &str, secret: &str, enc_key: Option<&str>) -> Option<String> {
    if key.is_empty() || secret.is_empty() {
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

    let ok = if encrypted == 0 {
        crypto::ct_eq(stored.as_bytes(), secret.as_bytes())
    } else if let Some(k) = enc_key {
        match crypto::fernet_decrypt(k, &stored) {
            Some(plain) => crypto::ct_eq(&plain, secret.as_bytes()),
            None => false,
        }
    } else {
        false
    };
    if ok {
        Some(user)
    } else {
        None
    }
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

/// Outcome of a doctype-level permission check.
pub struct Perm {
    /// Any grant (owner-scoped or not) exists for this (user, ptype).
    pub allowed: bool,
    /// All grants are `if_owner` — access must be restricted to rows the user owns.
    pub only_if_owner: bool,
}

/// The 4 metadata doctypes Frappe never lets Custom DocPerm override (meta.py set_custom_permissions).
fn custom_docperm_excluded(name: &str) -> bool {
    matches!(name, "DocType" | "DocField" | "DocPerm" | "Custom DocPerm")
}

/// Collect the user's roles plus the implicit ones Frappe always grants.
///
/// Mirrors `frappe/permissions.py get_roles`: Guest gets only `["Guest"]` (never `All`);
/// a normal user gets their `Has Role` rows (minus the AUTOMATIC_ROLES) plus the implicit
/// `["All","Guest"]`, plus `["Desk User"]` iff `User.user_type == "System User"`.
/// (Administrator is special-cased by the callers and never reaches here.)
fn user_roles(con: &Connection, user: &str) -> Vec<String> {
    // Guest never gets the "All" role (Frappe: get_roles("Guest") == ["Guest"]).
    if user == "Guest" {
        return vec!["Guest".to_string()];
    }
    let mut roles: Vec<String> = Vec::new();
    if let Ok(mut stmt) = con.prepare(
        "SELECT role FROM \"tabHas Role\" \
         WHERE parent=?1 AND parenttype='User' \
           AND role NOT IN ('All','Guest','Desk User','Administrator')",
    ) {
        if let Ok(rows) = stmt.query_map([user], |r| r.get::<_, String>(0)) {
            roles.extend(rows.flatten());
        }
    }
    roles.push("All".to_string());
    roles.push("Guest".to_string());
    // "Desk User" only for System Users (relevant for desk-only doctype perms).
    let is_system: bool = con
        .query_row(
            "SELECT 1 FROM \"tabUser\" WHERE name=?1 AND user_type='System User'",
            [user],
            |_| Ok(true),
        )
        .unwrap_or(false);
    if is_system {
        roles.push("Desk User".to_string());
    }
    roles
}

/// Which permission table applies to this doctype: Custom DocPerm (if customized) else DocPerm.
fn perm_table(con: &Connection, meta: &Meta) -> &'static str {
    if meta.istable || custom_docperm_excluded(&meta.name) {
        return "tabDocPerm";
    }
    let has_custom: bool = con
        .query_row(
            "SELECT 1 FROM \"tabCustom DocPerm\" WHERE parent=?1 LIMIT 1",
            [&meta.name],
            |_| Ok(true),
        )
        .unwrap_or(false);
    if has_custom {
        "tabCustom DocPerm"
    } else {
        "tabDocPerm"
    }
}

/// Doctype-level permission, with if_owner detection. Administrator is unconditionally allowed.
pub fn permission(con: &Connection, meta: &Meta, user: &str, ptype: &str) -> Perm {
    if user == "Administrator" {
        return Perm {
            allowed: true,
            only_if_owner: false,
        };
    }
    let col = match ptype {
        "read" | "write" | "create" | "delete" | "submit" | "cancel" | "report" | "export" => ptype,
        _ => "read",
    };
    let roles = user_roles(con, user);
    let placeholders = vec!["?"; roles.len()].join(",");
    let table = perm_table(con, meta);
    // Count unconditional vs owner-only grants in one query.
    let sql = format!(
        "SELECT \
           COALESCE(SUM(CASE WHEN COALESCE(if_owner,0)=0 THEN 1 ELSE 0 END),0), \
           COALESCE(SUM(CASE WHEN COALESCE(if_owner,0)=1 THEN 1 ELSE 0 END),0) \
         FROM \"{table}\" \
         WHERE parent=? AND COALESCE(permlevel,0)=0 AND COALESCE(\"{col}\",0)=1 \
           AND role IN ({placeholders})"
    );
    let mut binds: Vec<&dyn rusqlite::ToSql> = Vec::with_capacity(roles.len() + 1);
    let dt = &meta.name;
    binds.push(dt);
    for r in &roles {
        binds.push(r);
    }
    let (uncond, owner_only): (i64, i64) = con
        .query_row(&sql, binds.as_slice(), |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap_or((0, 0));

    Perm {
        allowed: uncond > 0 || owner_only > 0,
        // create is never owner-scoped in Frappe
        only_if_owner: uncond == 0 && owner_only > 0 && col != "create",
    }
}

/// Lowercased owner-equality check used for if_owner scoping (Frappe lowercases both sides).
pub fn owns(owner: &str, user: &str) -> bool {
    owner.to_lowercase() == user.to_lowercase()
}

/// The set of permlevels the user may READ for this doctype. `None` ⇒ all (Administrator).
pub fn readable_permlevels(con: &Connection, meta: &Meta, user: &str) -> Option<HashSet<i64>> {
    if user == "Administrator" {
        return None;
    }
    let roles = user_roles(con, user);
    let placeholders = vec!["?"; roles.len()].join(",");
    let table = perm_table(con, meta);
    let sql = format!(
        "SELECT DISTINCT COALESCE(permlevel,0) FROM \"{table}\" \
         WHERE parent=? AND COALESCE(read,0)=1 AND role IN ({placeholders})"
    );
    let mut binds: Vec<&dyn rusqlite::ToSql> = Vec::with_capacity(roles.len() + 1);
    let dt = &meta.name;
    binds.push(dt);
    for r in &roles {
        binds.push(r);
    }
    let mut set = HashSet::new();
    if let Ok(mut stmt) = con.prepare(&sql) {
        if let Ok(rows) = stmt.query_map(binds.as_slice(), |r| r.get::<_, i64>(0)) {
            for r in rows.flatten() {
                set.insert(r);
            }
        }
    }
    set.insert(0); // permlevel-0 fields are always readable when read is granted at all
    Some(set)
}

// ----------------------------------------------------------------- sessions (FIX-2)

/// Resolve the user behind a session id, honoring `status` and `session_expiry`. None if the
/// session is missing, logged out, or expired.
pub fn user_for_sid(con: &Connection, sid: &str) -> Option<String> {
    let (user, lastupdate, status): (String, Option<String>, Option<String>) = con
        .query_row(
            "SELECT user, lastupdate, status FROM \"tabSessions\" WHERE sid=?1",
            [sid],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .ok()?;
    if status.as_deref() == Some("Logged Out") {
        return None;
    }
    if let Some(lu) = lastupdate.as_deref().and_then(util::parse_civil_to_unix) {
        if util::now_unix() - lu > session_expiry_secs(con) {
            return None;
        }
    }
    if user.is_empty() || user == "Guest" {
        return None;
    }
    // A still-valid sid must not outlive the account: a disabled/deleted user gets no access.
    let enabled: bool = con
        .query_row(
            "SELECT 1 FROM \"tabUser\" WHERE name=?1 AND COALESCE(enabled,1)=1",
            [&user],
            |_| Ok(true),
        )
        .unwrap_or(false);
    if enabled {
        Some(user)
    } else {
        None
    }
}

/// `System Settings.session_expiry` ("HH:MM:SS", hours may exceed 24) in seconds; default 6h.
fn session_expiry_secs(con: &Connection) -> i64 {
    let raw: Option<String> = con
        .query_row(
            "SELECT value FROM \"tabSingles\" WHERE doctype='System Settings' AND field='session_expiry'",
            [],
            |r| r.get::<_, Option<String>>(0),
        )
        .ok()
        .flatten();
    let def = 6 * 3600;
    let s = match raw.as_deref() {
        Some(s) if !s.trim().is_empty() => s.trim().to_string(),
        _ => return def,
    };
    let mut it = s.split(':');
    let h: i64 = it.next().and_then(|x| x.parse().ok()).unwrap_or(6);
    let m: i64 = it.next().and_then(|x| x.parse().ok()).unwrap_or(0);
    let sec: i64 = it.next().and_then(|x| x.parse().ok()).unwrap_or(0);
    let total = h * 3600 + m * 60 + sec;
    if total <= 0 {
        def
    } else {
        total
    }
}

/// Verify a username/password login: resolve `usr` (by name, email, or username) to an enabled user
/// and check `pwd` against `__Auth(fieldname='password')` (passlib pbkdf2_sha256). Returns the
/// resolved user name on success — the honest replacement for the old accept-anything login stub.
pub fn verify_login(con: &Connection, usr: &str, pwd: &str) -> Option<String> {
    let usr = usr.trim();
    if usr.is_empty() || pwd.is_empty() {
        return None;
    }
    let user: String = con
        .query_row(
            "SELECT name FROM \"tabUser\" \
             WHERE (name=?1 OR email=?1 OR username=?1) AND COALESCE(enabled,1)=1 LIMIT 1",
            [usr],
            |r| r.get(0),
        )
        .ok()?;
    let hash: String = con
        .query_row(
            "SELECT password FROM \"__Auth\" WHERE doctype='User' AND name=?1 AND fieldname='password'",
            [&user],
            |r| r.get(0),
        )
        .ok()?;
    if crypto::passlib_pbkdf2_sha256_verify(pwd, &hash) {
        Some(user)
    } else {
        None
    }
}

/// Mint a new session row for `user`, returning its sid.
pub fn create_session(con: &Connection, user: &str) -> Option<String> {
    let sid = format!("{}{}", util::random_name(), util::random_name());
    let now = util::now_datetime();
    con.execute(
        "INSERT INTO \"tabSessions\" (sid, user, lastupdate, status, sessiondata) \
         VALUES (?1, ?2, ?3, 'Active', '')",
        rusqlite::params![sid, user, now],
    )
    .ok()?;
    Some(sid)
}

/// Invalidate a session (logout).
pub fn delete_session(con: &Connection, sid: &str) {
    let _ = con.execute("DELETE FROM \"tabSessions\" WHERE sid=?1", [sid]);
}

/// The set of permlevels the user may WRITE for this doctype. `None` ⇒ all (Administrator).
/// Mirrors `readable_permlevels` but on the `write` column; used to drop fields a user can't write
/// at their permlevel before they reach the ORM (write-path permlevel masking).
pub fn writable_permlevels(con: &Connection, meta: &Meta, user: &str) -> Option<HashSet<i64>> {
    if user == "Administrator" {
        return None;
    }
    let roles = user_roles(con, user);
    let placeholders = vec!["?"; roles.len()].join(",");
    let table = perm_table(con, meta);
    let sql = format!(
        "SELECT DISTINCT COALESCE(permlevel,0) FROM \"{table}\" \
         WHERE parent=? AND COALESCE(\"write\",0)=1 AND role IN ({placeholders})"
    );
    let mut binds: Vec<&dyn rusqlite::ToSql> = Vec::with_capacity(roles.len() + 1);
    let dt = &meta.name;
    binds.push(dt);
    for r in &roles {
        binds.push(r);
    }
    let mut set = HashSet::new();
    if let Ok(mut stmt) = con.prepare(&sql) {
        if let Ok(rows) = stmt.query_map(binds.as_slice(), |r| r.get::<_, i64>(0)) {
            for r in rows.flatten() {
                set.insert(r);
            }
        }
    }
    set.insert(0); // permlevel-0 fields are writable when write is granted at all
    Some(set)
}

/// True iff `user` has a DocShare row granting `ptype` ("read"/"write") on (doctype, name).
/// DocShare grants per-document access beyond role perms (Frappe under-grant fix).
pub fn doc_shared(con: &Connection, doctype: &str, name: &str, user: &str, ptype: &str) -> bool {
    if user == "Guest" {
        return false;
    }
    let col = match ptype {
        "write" | "share" | "submit" => ptype,
        _ => "read",
    };
    con.query_row(
        &format!(
            "SELECT 1 FROM \"tabDocShare\" \
             WHERE user=?1 AND share_doctype=?2 AND share_name=?3 AND COALESCE(\"{col}\",0)=1 LIMIT 1"
        ),
        rusqlite::params![user, doctype, name],
        |_| Ok(true),
    )
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
