//! Document naming — the Rust analogue of `frappe.model.naming`.
//!
//! Frappe builds a document's `name` from the DocType's `autoname` rule. ferro must follow the
//! SAME rules so names are identical to what a CPython worker would produce AND so the persistent
//! `tabSeries` counters stay in sync (a random hash would corrupt the sequence and risk collisions
//! with rows a CPython worker later inserts). Implemented rules (mirroring naming.py):
//!   field:<f> · naming_series:<...> · format:<...> · Expression (literal series w/ `#`) ·
//!   hash · UUID · autoincrement · prompt · null→hash (final fallback).

use crate::meta::{quote_ident, Meta};
use crate::orm::OrmError;
use crate::util;
use rusqlite::Connection;
use serde_json::{Map, Value};

/// Stringify a JSON value the way Frappe's `cstr` would for naming purposes.
fn cstr(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => (if *b { "1" } else { "0" }).to_string(),
        Some(other) => other.to_string(),
    }
}

/// Resolve the `name` for a new document, applying the DocType's autoname rule.
pub fn resolve_name(con: &Connection, meta: &Meta, data: &Map<String, Value>) -> Result<String, OrmError> {
    // Singles are always named after the doctype.
    if meta.issingle {
        return Ok(meta.name.clone());
    }

    // Explicit name from the client wins (covers prompt/Set-by-user and overrides).
    if let Some(n) = data.get("name").and_then(|v| v.as_str()) {
        if !n.trim().is_empty() {
            return validate_name(meta, n.trim());
        }
    }

    let autoname = meta.autoname.as_deref().unwrap_or("").trim().to_string();
    let lower = autoname.to_lowercase();

    let name = if autoname.is_empty() {
        // No rule: Frappe falls back to hash.
        util::random_name()
    } else if lower == "autoincrement" {
        next_autoincrement(con, meta)?
    } else if lower == "uuid" {
        util::uuid7()
    } else if lower == "hash" || lower == "autoname" {
        util::random_name()
    } else if lower.starts_with("field:") {
        let field = &autoname[6..];
        let v = cstr(data.get(field));
        let v = v.trim();
        if v.is_empty() {
            return Err(OrmError::Validation(format!("{field} is required")));
        }
        v.to_string()
    } else if lower.starts_with("prompt") {
        // prompt naming requires the client to send a name; none was provided above.
        return Err(OrmError::Validation("Please set the document name".into()));
    } else if lower.starts_with("naming_series:") {
        let series = naming_series_value(meta, data)?;
        let key = if series.contains('#') { series } else { format!("{series}.#####") };
        parse_naming_series(con, &key, data)?
    } else if lower.starts_with("format:") {
        format_autoname(con, &autoname, data)?
    } else if autoname.contains('#') {
        // Expression rule: substitute {braced} params, normalize, then run series.
        let substituted = substitute_braces(con, &autoname, data)?;
        let normalized = normalize_dash_series(&substituted);
        parse_naming_series(con, &normalized, data)?
    } else {
        // Unknown autoname literal → hash fallback (matches Frappe's final fallback).
        util::random_name()
    };

    validate_name(meta, name.trim())
}

/// Determine the naming series string for a `naming_series:`-ruled doctype:
/// the payload's `naming_series`, else the first option of the `naming_series` docfield.
fn naming_series_value(meta: &Meta, data: &Map<String, Value>) -> Result<String, OrmError> {
    if let Some(s) = data.get("naming_series").and_then(|v| v.as_str()) {
        if !s.trim().is_empty() {
            return Ok(s.trim().to_string());
        }
    }
    if let Some(f) = meta.field("naming_series") {
        if let Some(opts) = &f.options {
            if let Some(first) = opts.lines().map(|l| l.trim()).find(|l| !l.is_empty()) {
                return Ok(first.to_string());
            }
        }
    }
    Err(OrmError::Validation("Naming Series mandatory".into()))
}

/// `format:`-rule naming. Replace every `{param}` then, if a bare `#` series remains, run it.
fn format_autoname(con: &Connection, autoname: &str, data: &Map<String, Value>) -> Result<String, OrmError> {
    let value = match autoname.find(':') {
        Some(i) => &autoname[i + 1..],
        None => autoname,
    };
    let value = normalize_dash_series(value);
    let substituted = substitute_braces(con, &value, data)?;
    if substituted.contains('#') && !substituted.contains('{') {
        parse_naming_series(con, &substituted, data)
    } else {
        Ok(substituted)
    }
}

/// Normalize `-.####` → `.-.####` (only when not already dot-preceded), matching Frappe's regex.
fn normalize_dash_series(s: &str) -> String {
    // Find occurrences of "-." followed by one or more '#', where the '-' is not preceded by '.'.
    let bytes = s.as_bytes();
    let mut out = String::with_capacity(s.len() + 2);
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'-'
            && i + 1 < bytes.len()
            && bytes[i + 1] == b'.'
            && i + 2 < bytes.len()
            && bytes[i + 2] == b'#'
            && (i == 0 || bytes[i - 1] != b'.')
        {
            out.push('.'); // insert separating dot before the dash
        }
        out.push(bytes[i] as char);
        i += 1;
    }
    out
}

/// Replace each `{token}` by evaluating the token through the series engine (single-part).
fn substitute_braces(con: &Connection, s: &str, data: &Map<String, Value>) -> Result<String, OrmError> {
    let mut out = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'{' {
            if let Some(close) = s[i..].find('}') {
                let inner = &s[i + 1..i + close];
                out.push_str(&eval_part(con, inner, "", data)?);
                i += close + 1;
                continue;
            }
        }
        out.push(bytes[i] as char);
        i += 1;
    }
    Ok(out)
}

/// Parse a naming series like `SINV-.YYYY.-.#####` by splitting on '.', evaluating each part,
/// and replacing the first run of `#` with the zero-padded series counter keyed by the prefix.
fn parse_naming_series(con: &Connection, series: &str, data: &Map<String, Value>) -> Result<String, OrmError> {
    let mut name = String::new();
    let mut series_set = false;
    for part in series.split('.') {
        if part.is_empty() {
            continue;
        }
        if part.starts_with('#') {
            if !series_set {
                let digits = part.len();
                let counter = getseries(con, &name, digits)?;
                name.push_str(&counter);
                series_set = true;
            }
            // additional '#'-parts after the first are ignored (Frappe sets only once)
            continue;
        }
        let prefix = name.clone();
        let v = eval_part(con, part, &prefix, data)?;
        name.push_str(&v);
    }
    Ok(name)
}

/// Evaluate a single naming-series token against date params / doc fields / `#` series.
fn eval_part(con: &Connection, token: &str, prefix: &str, data: &Map<String, Value>) -> Result<String, OrmError> {
    let (y, mo, d, _, _, _) = util::now_civil();
    let t = token.trim();
    // A braced token may itself contain a series, e.g. {#####}
    if t.starts_with('#') {
        return getseries(con, prefix, t.len());
    }
    Ok(match t {
        "YY" => format!("{:02}", y % 100),
        "YYYY" => format!("{y:04}"),
        "MM" => format!("{mo:02}"),
        "DD" => format!("{d:02}"),
        "JJJ" => format!("{:03}", util::day_of_year(y, mo, d)),
        // Frappe's determine_consecutive_week_number: ISO %V with Jan/Dec boundary fixups so the
        // first/last days of a year stay consecutive ("00" for an early-Jan ISO wk 52/53, "53" for
        // a late-Dec ISO wk 0/1).
        "WW" => {
            let w = util::iso_week(y, mo, d);
            let w = if mo == 1 && w >= 52 {
                0
            } else if mo == 12 && w <= 1 {
                53
            } else {
                w
            };
            format!("{w:02}")
        }
        // Frappe's {timestamp} is frappe.utils.now() — microsecond precision (keeps format-named
        // docs created within the same second distinct, matching CPython).
        "timestamp" => util::now_datetime(),
        other => {
            // strip surrounding braces if any, then treat as a doc field or literal
            let key = other.trim_matches(|c| c == '{' || c == '}');
            match data.get(key) {
                Some(_) => cstr(data.get(key)),
                None => other.to_string(), // literal separator/prefix
            }
        }
    })
}

/// Atomic series counter, mirroring `frappe.model.naming.getseries`.
/// Uses SQLite UPSERT…RETURNING so concurrent workers never collide on a counter value.
fn getseries(con: &Connection, key: &str, digits: usize) -> Result<String, OrmError> {
    let current: i64 = con.query_row(
        "INSERT INTO \"tabSeries\" (name, current) VALUES (?1, 1) \
         ON CONFLICT(name) DO UPDATE SET current = current + 1 RETURNING current",
        [key],
        |r| r.get(0),
    )?;
    Ok(format!("{current:0width$}", width = digits))
}

/// `autoincrement` naming: next integer (max(name)+1) within the current transaction.
fn next_autoincrement(con: &Connection, meta: &Meta) -> Result<String, OrmError> {
    let next: i64 = con
        .query_row(
            &format!(
                "SELECT COALESCE(MAX(CAST({} AS INTEGER)), 0) + 1 FROM {}",
                quote_ident("name"),
                quote_ident(&meta.table)
            ),
            [],
            |r| r.get(0),
        )
        .unwrap_or(1);
    Ok(next.to_string())
}

/// Validate a chosen name, mirroring `frappe.model.naming.validate_name`.
pub fn validate_name(meta: &Meta, name: &str) -> Result<String, OrmError> {
    let name = name.trim();
    if name.is_empty() {
        return Err(OrmError::Validation(format!("No Name Specified for {}", meta.name)));
    }
    if name.starts_with(&format!("New {}", meta.name)) {
        return Err(OrmError::Validation(
            "There were some errors setting the name, please contact the administrator".into(),
        ));
    }
    if !meta.issingle && name == meta.name && meta.name != "DocType" {
        return Err(OrmError::Validation(format!(
            "Name of {} cannot be {}",
            meta.name, name
        )));
    }
    if name.contains('<') || name.contains('>') {
        return Err(OrmError::Validation(
            "Name cannot contain special characters like '<', '>'".into(),
        ));
    }
    Ok(name.to_string())
}
