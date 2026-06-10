//! Document ORM: the Rust analogue of frappe.client / frappe.model.db_query.
//! Reads and writes the *same* `tab<DocType>` tables Frappe created.

use crate::meta::{quote_ident, DocField, Meta};
use crate::naming;
use crate::util;
use rusqlite::types::{Value as SqlValue, ValueRef};
use rusqlite::Connection;
use serde_json::{Map, Value};
use std::collections::HashSet;

#[derive(Debug)]
pub enum OrmError {
    NotFound(String),   // -> 404
    Validation(String), // -> 417
    Duplicate(String),  // -> 409
    Db(rusqlite::Error),
}
impl From<rusqlite::Error> for OrmError {
    fn from(e: rusqlite::Error) -> Self {
        OrmError::Db(e)
    }
}

/// Field-level read access control derived from the user's DocPerm permlevels.
/// `None` permlevels ⇒ read everything (Administrator). Otherwise only fields whose
/// docfield permlevel is in the set are returned/queryable (permlevel-0 always allowed).
pub struct ReadAcl {
    pub permlevels: Option<HashSet<i64>>,
}
impl ReadAcl {
    pub fn all() -> Self {
        ReadAcl { permlevels: None }
    }
    /// May the user read this physical column? Standard/custom columns with no docfield are
    /// permlevel 0 (always readable when any read is granted).
    pub fn can_read(&self, meta: &Meta, field: &str) -> bool {
        match &self.permlevels {
            None => true,
            Some(set) => {
                let pl = meta.field(field).map(|f| f.permlevel).unwrap_or(0);
                set.contains(&pl)
            }
        }
    }
}

/// Run a closure inside an IMMEDIATE transaction; commit on Ok, rollback on Err.
/// All multi-statement writes use this so a mid-sequence failure never leaves a partial doc,
/// and the naming-series counter advances atomically with the row it names.
fn with_txn<T>(con: &Connection, f: impl FnOnce() -> Result<T, OrmError>) -> Result<T, OrmError> {
    con.execute_batch("BEGIN IMMEDIATE")?;
    match f() {
        Ok(v) => {
            con.execute_batch("COMMIT")?;
            Ok(v)
        }
        Err(e) => {
            let _ = con.execute_batch("ROLLBACK");
            Err(e)
        }
    }
}

/// rusqlite cell -> JSON. SQLite is dynamically typed and Frappe relies on that:
/// Check/Int/docstatus/idx come back as integers, Float/Currency as reals, the rest as text.
fn cell_to_json(v: ValueRef) -> Value {
    match v {
        ValueRef::Null => Value::Null,
        ValueRef::Integer(i) => Value::from(i),
        ValueRef::Real(f) => Value::from(f),
        ValueRef::Text(t) => Value::from(String::from_utf8_lossy(t).into_owned()),
        ValueRef::Blob(b) => Value::from(util::b64(b)),
    }
}

/// JSON scalar -> SQLite bind value.
fn json_to_sql(v: &Value) -> SqlValue {
    match v {
        Value::Null => SqlValue::Null,
        Value::Bool(b) => SqlValue::Integer(*b as i64),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                SqlValue::Integer(i)
            } else {
                SqlValue::Real(n.as_f64().unwrap_or(0.0))
            }
        }
        Value::String(s) => SqlValue::Text(s.clone()),
        other => SqlValue::Text(other.to_string()),
    }
}

/// Cast a stored single-value string to JSON per its fieldtype (mirrors Frappe as_dict casting).
pub fn cast_by_fieldtype(field: Option<&DocField>, raw: Option<String>) -> Value {
    let raw = match raw {
        Some(s) => s,
        None => return Value::Null,
    };
    let ft = field.map(|f| f.fieldtype.as_str()).unwrap_or("Data");
    match ft {
        "Check" => Value::from(if util::cint(&raw) != 0 { 1 } else { 0 }),
        "Int" | "Long Int" => Value::from(util::cint(&raw)),
        "Float" | "Currency" | "Percent" => Value::from(util::flt(&raw)),
        _ => Value::from(raw),
    }
}

/// Parsed list query (from /api/resource/<doctype> query string).
pub struct ListQuery {
    pub fields: Vec<String>,
    pub filters: Value, // raw JSON (array-of-arrays or object), or Null
    pub or_filters: Value,
    pub order_by: Option<String>,
    pub limit_start: i64,
    pub limit_page_length: i64,
}

impl Default for ListQuery {
    fn default() -> Self {
        ListQuery {
            fields: vec!["name".to_string()],
            filters: Value::Null,
            or_filters: Value::Null,
            order_by: None,
            limit_start: 0,
            limit_page_length: 20,
        }
    }
}

/// One WHERE clause fragment plus its bound parameters.
struct Clause {
    sql: String,
    binds: Vec<SqlValue>,
}

/// Split an `in`/`not in` value into a list. Arrays pass through; a string is parsed as a
/// JSON-encoded list FIRST (so '["a","b,c"]' keeps its embedded commas — B-FIL-3), falling back
/// to a comma-split (Frappe's db_query behavior for a plain "a,b" string).
fn in_values(val: &Value) -> Vec<Value> {
    match val {
        Value::Array(a) => a.clone(),
        Value::String(s) => match serde_json::from_str::<Value>(s) {
            Ok(Value::Array(a)) => a,
            _ => s.split(',').map(|p| Value::from(p.trim().to_string())).collect(),
        },
        other => vec![other.clone()],
    }
}

fn op_clause(meta: &Meta, acl: &ReadAcl, field: &str, op: &str, val: &Value) -> Result<Clause, OrmError> {
    if !meta.has_column(field) || !acl.can_read(meta, field) {
        return Err(OrmError::Validation(format!(
            "Unknown field '{field}' in filters for {}",
            meta.name
        )));
    }
    let col = quote_ident(field);
    let op_l = op.to_lowercase();
    let mut binds = Vec::new();
    let sql = match op_l.as_str() {
        "=" | "!=" | ">" | "<" | ">=" | "<=" | "<>" => {
            binds.push(json_to_sql(val));
            format!("{col} {op} ?")
        }
        "like" => {
            binds.push(json_to_sql(val));
            format!("{col} LIKE ?")
        }
        "not like" => {
            binds.push(json_to_sql(val));
            format!("{col} NOT LIKE ?")
        }
        "in" => {
            // Strip None elements (B-FIL-2): `IN` of nothing matches nothing.
            let arr: Vec<Value> = in_values(val).into_iter().filter(|v| !v.is_null()).collect();
            if arr.is_empty() {
                return Ok(Clause { sql: "0".into(), binds });
            }
            let qs = vec!["?"; arr.len()].join(",");
            for v in arr {
                binds.push(json_to_sql(&v));
            }
            format!("{col} IN ({qs})")
        }
        "not in" => {
            // Strip None elements (B-FIL-2): `NOT IN` of nothing matches all rows.
            let arr: Vec<Value> = in_values(val).into_iter().filter(|v| !v.is_null()).collect();
            if arr.is_empty() {
                return Ok(Clause { sql: "1".into(), binds });
            }
            let qs = vec!["?"; arr.len()].join(",");
            for v in arr {
                binds.push(json_to_sql(&v));
            }
            format!("{col} NOT IN ({qs})")
        }
        "between" => {
            let arr = val.as_array().cloned().unwrap_or_default();
            if arr.len() != 2 {
                return Err(OrmError::Validation("between needs [a, b]".into()));
            }
            binds.push(json_to_sql(&arr[0]));
            binds.push(json_to_sql(&arr[1]));
            format!("{col} BETWEEN ? AND ?")
        }
        "is" => {
            let want = val.as_str().unwrap_or("set");
            if want == "set" {
                format!("({col} IS NOT NULL AND {col} != '')")
            } else {
                format!("({col} IS NULL OR {col} = '')")
            }
        }
        other => return Err(OrmError::Validation(format!("Unsupported operator '{other}'"))),
    };
    Ok(Clause { sql, binds })
}

/// AND-combine a set of clauses into one parenthesized clause (used to fold a dict filter element
/// into a single condition). Returns None for an empty input.
fn combine_and(clauses: Vec<Clause>) -> Option<Clause> {
    if clauses.is_empty() {
        return None;
    }
    if clauses.len() == 1 {
        return clauses.into_iter().next();
    }
    let mut binds = Vec::new();
    let sql = clauses
        .into_iter()
        .map(|c| {
            binds.extend(c.binds);
            c.sql
        })
        .collect::<Vec<_>>()
        .join(" AND ");
    Some(Clause { sql: format!("({sql})"), binds })
}

/// Turn a Frappe filters value into clauses. Supports:
///   [["field","op","value"], ...]  and  [["DocType","field","op","value"], ...]
///   {"field": value}  and  {"field": ["op", value]}
///   and dict elements inside the array (or_filters=[{...}, {...}]).
fn parse_filters(meta: &Meta, acl: &ReadAcl, filters: &Value) -> Result<Vec<Clause>, OrmError> {
    let mut clauses = Vec::new();
    match filters {
        Value::Null => {}
        Value::Array(arr) => {
            for cond in arr {
                // A dict element inside the array (B-FIL-1) — e.g. or_filters=[{...},{...}] or a
                // dict-in-list filters entry. Expand it to its own conditions, AND-combined into a
                // single clause so the surrounding AND/OR join keeps Frappe semantics:
                // or_filters=[{a:1,b:2},{c:3}] -> (a=1 AND b=2) OR (c=3).
                if cond.is_object() {
                    let inner = parse_filters(meta, acl, cond)?;
                    if let Some(combined) = combine_and(inner) {
                        clauses.push(combined);
                    }
                    continue;
                }
                let c = cond
                    .as_array()
                    .ok_or_else(|| OrmError::Validation("each filter must be a list".into()))?;
                let (field, op, val) = match c.len() {
                    2 => (c[0].as_str().unwrap_or_default(), "=", c[1].clone()),
                    3 => (
                        c[0].as_str().unwrap_or_default(),
                        c[1].as_str().unwrap_or("="),
                        c[2].clone(),
                    ),
                    4 => (
                        // [doctype, field, op, value] — doctype assumed to be this one
                        c[1].as_str().unwrap_or_default(),
                        c[2].as_str().unwrap_or("="),
                        c[3].clone(),
                    ),
                    _ => return Err(OrmError::Validation("bad filter arity".into())),
                };
                clauses.push(op_clause(meta, acl, field, op, &val)?);
            }
        }
        Value::Object(obj) => {
            for (field, v) in obj {
                if let Some(pair) = v.as_array() {
                    if pair.len() == 2 {
                        let op = pair[0].as_str().unwrap_or("=");
                        clauses.push(op_clause(meta, acl, field, op, &pair[1])?);
                        continue;
                    }
                }
                clauses.push(op_clause(meta, acl, field, "=", v)?);
            }
        }
        _ => return Err(OrmError::Validation("filters must be a list or object".into())),
    }
    Ok(clauses)
}

fn validate_order_by(meta: &Meta, acl: &ReadAcl, order_by: &str) -> Result<String, OrmError> {
    let mut parts = Vec::new();
    for term in order_by.split(',') {
        let term = term.trim();
        if term.is_empty() {
            continue;
        }
        let mut it = term.split_whitespace();
        let field = it.next().unwrap_or("");
        let field = field.rsplit('.').next().unwrap_or(field).trim_matches('`');
        let dir = it.next().unwrap_or("asc").to_lowercase();
        let dir = if dir == "desc" { "DESC" } else { "ASC" };
        if !meta.has_column(field) || !acl.can_read(meta, field) {
            return Err(OrmError::Validation(format!("Unknown order_by field '{field}'")));
        }
        parts.push(format!("{} {}", quote_ident(field), dir));
    }
    if parts.is_empty() {
        Ok(format!("{} DESC", quote_ident("modified")))
    } else {
        Ok(parts.join(", "))
    }
}

/// GET /api/resource/<doctype> — returns an array of row objects.
/// `owner_scope` (Some(user)) ANDs an `owner = user` constraint, for if_owner permission scoping.
pub fn get_list(
    con: &Connection,
    meta: &Meta,
    acl: &ReadAcl,
    q: &ListQuery,
    owner_scope: Option<&str>,
) -> Result<Value, OrmError> {
    // Virtual doctypes have no table; without a controller we can only return an empty set.
    if meta.is_virtual {
        return Ok(Value::Array(vec![]));
    }

    // Singles have no `tab<Single>` table — their values live in tabSingles (B-DB-1). Return the
    // one doc as a single-row list, projected to the requested fields and ACL-masked via get_single.
    if meta.issingle {
        let doc = get_single(con, meta, acl)?;
        let src = doc.as_object().cloned().unwrap_or_default();
        let wants_all = q.fields.iter().any(|f| f.trim().trim_matches('`') == "*");
        let projected = if wants_all || q.fields.is_empty() {
            Value::Object(src)
        } else {
            let mut o = Map::new();
            for f in &q.fields {
                let f = f.trim().trim_matches('`');
                let f = f.rsplit('.').next().unwrap_or(f);
                // Match the non-Single path: an unknown field is a validation error, not a silent null.
                if !meta.has_column(f) {
                    return Err(OrmError::Validation(format!("Unknown field '{f}' for {}", meta.name)));
                }
                o.insert(f.to_string(), src.get(f).cloned().unwrap_or(Value::Null));
            }
            Value::Object(o)
        };
        return Ok(Value::Array(vec![projected]));
    }

    // Validate & quote selected fields, honoring permlevel.
    let mut select_cols = Vec::new();
    for f in &q.fields {
        let f = f.trim().trim_matches('`');
        let f = f.rsplit('.').next().unwrap_or(f);
        if f == "*" {
            for c in &meta.columns {
                if acl.can_read(meta, c) {
                    select_cols.push(quote_ident(c));
                }
            }
            continue;
        }
        if !meta.has_column(f) {
            return Err(OrmError::Validation(format!("Unknown field '{f}' for {}", meta.name)));
        }
        if !acl.can_read(meta, f) {
            continue; // silently drop fields the user can't read at this permlevel (Frappe behavior)
        }
        select_cols.push(quote_ident(f));
    }
    if select_cols.is_empty() {
        select_cols.push(quote_ident("name"));
    }

    let mut where_parts = Vec::new();
    let mut binds: Vec<SqlValue> = Vec::new();
    if let Some(owner) = owner_scope {
        if meta.has_column("owner") {
            where_parts.push(format!("{} = ?", quote_ident("owner")));
            binds.push(SqlValue::Text(owner.to_string()));
        }
    }
    for c in parse_filters(meta, acl, &q.filters)? {
        where_parts.push(c.sql);
        binds.extend(c.binds);
    }
    let and_sql = where_parts.join(" AND ");

    let mut or_parts = Vec::new();
    for c in parse_filters(meta, acl, &q.or_filters)? {
        or_parts.push(c.sql);
        binds.extend(c.binds);
    }
    let or_sql = or_parts.join(" OR ");

    let mut where_clause = String::new();
    match (and_sql.is_empty(), or_sql.is_empty()) {
        (true, true) => {}
        (false, true) => where_clause = format!(" WHERE {and_sql}"),
        (true, false) => where_clause = format!(" WHERE ({or_sql})"),
        (false, false) => where_clause = format!(" WHERE {and_sql} AND ({or_sql})"),
    }

    let order = match &q.order_by {
        Some(o) => validate_order_by(meta, acl, o)?,
        None => format!(
            "{} {}",
            quote_ident(&meta.sort_field),
            if meta.sort_order.eq_ignore_ascii_case("asc") { "ASC" } else { "DESC" }
        ),
    };

    // limit_page_length <= 0 means UNLIMITED in Frappe (db_query: falsy -> no LIMIT clause).
    // SQLite expresses "no limit, with offset" as `LIMIT -1 OFFSET n`.
    let (limit_sql, lim_bind) = if q.limit_page_length <= 0 {
        ("LIMIT -1 OFFSET ?".to_string(), None)
    } else {
        ("LIMIT ? OFFSET ?".to_string(), Some(q.limit_page_length))
    };

    let sql = format!(
        "SELECT {} FROM {}{} ORDER BY {} {}",
        select_cols.join(", "),
        quote_ident(&meta.table),
        where_clause,
        order,
        limit_sql
    );
    if let Some(l) = lim_bind {
        binds.push(SqlValue::Integer(l));
    }
    binds.push(SqlValue::Integer(q.limit_start.max(0)));

    let mut stmt = con.prepare(&sql)?;
    let col_names: Vec<String> = stmt.column_names().iter().map(|s| s.to_string()).collect();
    let rows = stmt.query_map(rusqlite::params_from_iter(binds.iter()), |row| {
        let mut obj = Map::new();
        for (i, name) in col_names.iter().enumerate() {
            obj.insert(name.clone(), cell_to_json(row.get_ref(i)?));
        }
        Ok(Value::Object(obj))
    })?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(Value::Array(out))
}

/// Read one full document (all readable columns + child tables). For singles, reads tabSingles.
pub fn get_doc(con: &Connection, meta: &Meta, acl: &ReadAcl, name: &str) -> Result<Value, OrmError> {
    if meta.issingle {
        return get_single(con, meta, acl);
    }
    if meta.is_virtual {
        return Err(OrmError::NotFound(format!("{} {}", meta.name, name)));
    }

    // Explicit readable column list (replaces SELECT * so higher-permlevel columns never leak).
    let readable: Vec<String> = {
        let mut v: Vec<String> = meta.columns.iter().filter(|c| acl.can_read(meta, c)).cloned().collect();
        v.sort();
        if v.is_empty() {
            v.push("name".to_string());
        }
        v
    };
    let select = readable.iter().map(|c| quote_ident(c)).collect::<Vec<_>>().join(", ");
    let sql = format!(
        "SELECT {} FROM {} WHERE {} = ?1",
        select,
        quote_ident(&meta.table),
        quote_ident("name")
    );
    let mut stmt = con.prepare(&sql)?;
    let col_names: Vec<String> = stmt.column_names().iter().map(|s| s.to_string()).collect();
    let mut rows = stmt.query([name])?;
    let row = match rows.next()? {
        Some(r) => r,
        None => return Err(OrmError::NotFound(format!("{} {}", meta.name, name))),
    };
    let mut obj = Map::new();
    obj.insert("doctype".into(), Value::from(meta.name.clone()));
    for (i, cn) in col_names.iter().enumerate() {
        obj.insert(cn.clone(), cell_to_json(row.get_ref(i)?));
    }
    drop(rows);
    drop(stmt);

    // Child tables (only those the user can read).
    for cf in meta.child_tables() {
        if !acl.can_read(meta, &cf.fieldname) {
            continue;
        }
        let child_dt = match &cf.options {
            Some(o) if !o.is_empty() => o,
            _ => continue,
        };
        let child_table = format!("tab{child_dt}");
        let csql = format!(
            "SELECT * FROM {} WHERE {}=?1 AND {}=?2 AND {}=?3 ORDER BY idx",
            quote_ident(&child_table),
            quote_ident("parent"),
            quote_ident("parenttype"),
            quote_ident("parentfield"),
        );
        let mut cstmt = match con.prepare(&csql) {
            Ok(s) => s,
            Err(_) => continue, // child table may not exist; skip gracefully
        };
        let cnames: Vec<String> = cstmt.column_names().iter().map(|s| s.to_string()).collect();
        let crows = cstmt.query_map(rusqlite::params![name, meta.name, cf.fieldname], |row| {
            let mut o = Map::new();
            o.insert("doctype".into(), Value::from(child_dt.clone()));
            for (i, cn) in cnames.iter().enumerate() {
                o.insert(cn.clone(), cell_to_json(row.get_ref(i)?));
            }
            Ok(Value::Object(o))
        })?;
        let mut arr = Vec::new();
        for r in crows {
            arr.push(r?);
        }
        obj.insert(cf.fieldname.clone(), Value::Array(arr));
    }

    Ok(Value::Object(obj))
}

fn get_single(con: &Connection, meta: &Meta, acl: &ReadAcl) -> Result<Value, OrmError> {
    let mut stmt = con.prepare("SELECT field, value FROM \"tabSingles\" WHERE doctype = ?1")?;
    let rows = stmt.query_map([&meta.name], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?))
    })?;
    let mut obj = Map::new();
    obj.insert("doctype".into(), Value::from(meta.name.clone()));
    obj.insert("name".into(), Value::from(meta.name.clone()));
    for r in rows {
        let (field, value) = r?;
        if field == "name" || field == "doctype" {
            continue;
        }
        if !acl.can_read(meta, &field) {
            continue;
        }
        obj.insert(field.clone(), cast_by_fieldtype(meta.field(&field), value));
    }
    // Standard fields Frappe always exposes on a single, defaulted when unset.
    for (k, dv) in [("docstatus", Value::from(0)), ("idx", Value::from(0))] {
        obj.entry(k.to_string()).or_insert(dv);
    }
    Ok(Value::Object(obj))
}

/// Compute a docfield's default value for insert (mirrors create_new._set_default_values intent).
fn default_for(df: &DocField, user: &str) -> Option<SqlValue> {
    if let Some(def) = &df.default {
        let d = def.trim();
        if !d.is_empty() {
            if d.starts_with(':') {
                return None; // cross-document default (e.g. ":Company") — out of REST scope
            }
            let v = match d {
                "__user" => user.to_string(),
                "Today" => util::now_date(),
                "now" | "Now" => util::now_datetime(),
                _ => d.to_string(),
            };
            return Some(match df.fieldtype.as_str() {
                "Check" => SqlValue::Integer(if util::cint(&v) != 0 { 1 } else { 0 }),
                "Int" | "Long Int" => SqlValue::Integer(util::cint(&v)),
                _ => SqlValue::Text(v),
            });
        }
    }
    // Select: Frappe defaults to the first option line if it is a real value.
    if df.fieldtype == "Select" {
        if let Some(opts) = &df.options {
            if let Some(first) = opts.lines().next() {
                let first = first.trim();
                if !first.is_empty() && first != "[Select]" && first != "Loading..." {
                    return Some(SqlValue::Text(first.to_string()));
                }
            }
        }
    }
    None
}

/// True when a bound value counts as "present" for required-field validation.
fn is_nonempty(v: &SqlValue) -> bool {
    match v {
        SqlValue::Null => false,
        SqlValue::Text(s) => !s.is_empty(),
        _ => true,
    }
}

const SYSTEM_INSERT_FIELDS: &[&str] = &["owner", "creation", "modified", "modified_by", "docstatus", "idx"];

/// Render a SQL bind value as the text used for set_only_once equality comparison.
fn sqlval_text(v: SqlValue) -> String {
    match v {
        SqlValue::Null => String::new(),
        SqlValue::Integer(i) => i.to_string(),
        SqlValue::Real(f) => f.to_string(),
        SqlValue::Text(s) => s,
        SqlValue::Blob(_) => String::new(),
    }
}

/// Validate one Link / Dynamic Link value points at an existing document (Frappe's
/// get_invalid_links → LinkValidationError, HTTP 417). Conservative: skips empty values, the
/// "[Select]" pseudo-option, and targets whose table doesn't exist (Single/virtual/unknown).
fn validate_link(
    con: &Connection,
    df: &DocField,
    value: &Value,
    siblings: &Map<String, Value>,
) -> Result<(), OrmError> {
    let v = match value {
        Value::String(s) if !s.trim().is_empty() => s.trim().to_string(),
        _ => return Ok(()),
    };
    let target = match df.fieldtype.as_str() {
        "Link" => match df.options.as_deref() {
            Some(o) if !o.is_empty() && o != "[Select]" => o.to_string(),
            _ => return Ok(()),
        },
        // Dynamic Link: `options` names a sibling field holding the target doctype.
        "Dynamic Link" => {
            let key = match df.options.as_deref() {
                Some(k) if !k.is_empty() => k,
                _ => return Ok(()),
            };
            match siblings.get(key).and_then(|x| x.as_str()) {
                Some(dt) if !dt.is_empty() => dt.to_string(),
                _ => return Ok(()),
            }
        }
        _ => return Ok(()),
    };
    let table = format!("tab{target}");
    let table_exists: bool = con
        .query_row("SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1", [&table], |_| Ok(true))
        .unwrap_or(false);
    if !table_exists {
        return Ok(());
    }
    let found: bool = con
        .query_row(
            &format!("SELECT 1 FROM {} WHERE {}=?1 LIMIT 1", quote_ident(&table), quote_ident("name")),
            [&v],
            |_| Ok(true),
        )
        .unwrap_or(false);
    if found {
        Ok(())
    } else {
        Err(OrmError::Validation(format!("Could not find {target}: {v}")))
    }
}

/// Validate every Link / Dynamic Link field present in `data` against `meta`.
fn validate_links(con: &Connection, meta: &Meta, data: &Map<String, Value>) -> Result<(), OrmError> {
    for f in &meta.fields {
        if matches!(f.fieldtype.as_str(), "Link" | "Dynamic Link") {
            if let Some(v) = data.get(&f.fieldname) {
                validate_link(con, f, v, data)?;
            }
        }
    }
    Ok(())
}

/// INSERT a new document. Returns the created doc.
pub fn insert(
    con: &Connection,
    meta: &Meta,
    acl: &ReadAcl,
    data: &Map<String, Value>,
    user: &str,
) -> Result<Value, OrmError> {
    if meta.issingle {
        return insert_single(con, meta, acl, data, user);
    }
    if meta.is_virtual {
        return Err(OrmError::Validation(format!("Cannot create virtual DocType {}", meta.name)));
    }

    let name = with_txn(con, || {
        let now = util::now_datetime();
        let name = naming::resolve_name(con, meta, data)?;

        let mut cols: Vec<String> = Vec::new();
        let mut binds: Vec<SqlValue> = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        // track values for required-field validation
        let mut values: std::collections::HashMap<String, SqlValue> = std::collections::HashMap::new();

        // standalone helper so it doesn't capture `seen`/`values` (we read them below)
        fn put(
            cols: &mut Vec<String>,
            binds: &mut Vec<SqlValue>,
            seen: &mut HashSet<String>,
            values: &mut std::collections::HashMap<String, SqlValue>,
            k: &str,
            v: SqlValue,
        ) {
            if seen.insert(k.to_string()) {
                cols.push(quote_ident(k));
                values.insert(k.to_string(), v.clone());
                binds.push(v);
            }
        }

        put(&mut cols, &mut binds, &mut seen, &mut values, "name", SqlValue::Text(name.clone()));

        // Payload columns (client cannot set system fields — Frappe overrides them).
        for (k, v) in data {
            if k == "doctype" || k == "name" || SYSTEM_INSERT_FIELDS.contains(&k.as_str()) {
                continue;
            }
            if let Some(f) = meta.field(k) {
                if f.is_child_table() {
                    continue;
                }
            }
            if meta.has_column(k) {
                put(&mut cols, &mut binds, &mut seen, &mut values, k, json_to_sql(v));
            }
        }

        // Docfield defaults for any column not provided.
        for f in &meta.fields {
            if f.is_virtual_column() || seen.contains(&f.fieldname) || !meta.has_column(&f.fieldname) {
                continue;
            }
            if let Some(dv) = default_for(f, user) {
                put(&mut cols, &mut binds, &mut seen, &mut values, &f.fieldname, dv);
            }
        }

        // System fields (server-authoritative).
        if meta.has_column("owner") {
            put(&mut cols, &mut binds, &mut seen, &mut values, "owner", SqlValue::Text(user.to_string()));
        }
        if meta.has_column("modified_by") {
            put(&mut cols, &mut binds, &mut seen, &mut values, "modified_by", SqlValue::Text(user.to_string()));
        }
        if meta.has_column("creation") {
            put(&mut cols, &mut binds, &mut seen, &mut values, "creation", SqlValue::Text(now.clone()));
        }
        if meta.has_column("modified") {
            put(&mut cols, &mut binds, &mut seen, &mut values, "modified", SqlValue::Text(now.clone()));
        }
        if meta.has_column("docstatus") {
            put(&mut cols, &mut binds, &mut seen, &mut values, "docstatus", SqlValue::Integer(0));
        }
        if meta.has_column("idx") {
            put(&mut cols, &mut binds, &mut seen, &mut values, "idx", SqlValue::Integer(0));
        }

        // Required-field validation (after defaults).
        for f in &meta.fields {
            if !f.reqd || f.is_child_table() || f.is_virtual_column() || !meta.has_column(&f.fieldname) {
                continue;
            }
            let present = values.get(&f.fieldname).map(is_nonempty).unwrap_or(false);
            if !present {
                return Err(OrmError::Validation(format!(
                    "Value missing for {}: {}",
                    meta.name, f.fieldname
                )));
            }
        }

        // Link / Dynamic Link target existence (B-DOC-1).
        validate_links(con, meta, data)?;

        let placeholders = vec!["?"; cols.len()].join(", ");
        let sql = format!(
            "INSERT INTO {} ({}) VALUES ({})",
            quote_ident(&meta.table),
            cols.join(", "),
            placeholders
        );
        con.execute(&sql, rusqlite::params_from_iter(binds.iter()))
            .map_err(|e| match e {
                rusqlite::Error::SqliteFailure(ref err, _)
                    if err.code == rusqlite::ErrorCode::ConstraintViolation =>
                {
                    OrmError::Duplicate(format!("{} {} already exists", meta.name, name))
                }
                other => OrmError::Db(other),
            })?;

        insert_children(con, meta, &name, data, user, &now)?;
        Ok(name)
    })?;

    get_doc(con, meta, acl, &name)
}

fn insert_children(
    con: &Connection,
    meta: &Meta,
    parent: &str,
    data: &Map<String, Value>,
    user: &str,
    now: &str,
) -> Result<(), OrmError> {
    for cf in meta.child_tables() {
        let child_dt = match &cf.options {
            Some(o) if !o.is_empty() => o,
            _ => continue,
        };
        let rows = match data.get(&cf.fieldname).and_then(|v| v.as_array()) {
            Some(r) => r,
            None => continue,
        };
        let child_table = format!("tab{child_dt}");
        let mut child_cols = HashSet::new();
        if let Ok(mut s) = con.prepare(&format!("PRAGMA table_info({})", quote_ident(&child_table))) {
            if let Ok(rs) = s.query_map([], |r| r.get::<_, String>(1)) {
                for c in rs.flatten() {
                    child_cols.insert(c);
                }
            }
        }
        if child_cols.is_empty() {
            continue;
        }
        // The child DocType's meta, loaded once per table — drives child autoname (B-NAM-2) and
        // child Link validation (B-DOC-1). Optional: a missing child meta degrades to hash naming.
        let child_meta = crate::meta::load_uncached(con, child_dt).ok();
        for (i, item) in rows.iter().enumerate() {
            let obj = match item.as_object() {
                Some(o) => o,
                None => continue,
            };
            // Validate this child row's Link fields against the child meta.
            if let Some(cm) = &child_meta {
                validate_links(con, cm, obj)?;
            }
            // Keep an existing/client-supplied child name (no rename-on-edit); otherwise apply the
            // child DocType's autoname rule (Frappe's set_new_name), falling back to a hash.
            let cname = match obj.get("name").and_then(|v| v.as_str()).filter(|s| !s.trim().is_empty()) {
                Some(n) => n.trim().to_string(),
                None => match &child_meta {
                    Some(cm) => naming::resolve_name(con, cm, obj).unwrap_or_else(|_| util::random_name()),
                    None => util::random_name(),
                },
            };
            let mut cols = vec![quote_ident("name")];
            let mut binds = vec![SqlValue::Text(cname)];
            let mut seen: HashSet<String> = ["name".to_string()].into_iter().collect();
            let put = |cols: &mut Vec<String>, binds: &mut Vec<SqlValue>, seen: &mut HashSet<String>, k: &str, v: SqlValue| {
                if child_cols.contains(k) && seen.insert(k.to_string()) {
                    cols.push(quote_ident(k));
                    binds.push(v);
                }
            };
            for (k, v) in obj {
                if k == "name" || k == "doctype" {
                    continue;
                }
                put(&mut cols, &mut binds, &mut seen, k, json_to_sql(v));
            }
            put(&mut cols, &mut binds, &mut seen, "parent", SqlValue::Text(parent.to_string()));
            put(&mut cols, &mut binds, &mut seen, "parenttype", SqlValue::Text(meta.name.clone()));
            put(&mut cols, &mut binds, &mut seen, "parentfield", SqlValue::Text(cf.fieldname.clone()));
            put(&mut cols, &mut binds, &mut seen, "idx", SqlValue::Integer((i + 1) as i64));
            put(&mut cols, &mut binds, &mut seen, "owner", SqlValue::Text(user.to_string()));
            put(&mut cols, &mut binds, &mut seen, "modified_by", SqlValue::Text(user.to_string()));
            put(&mut cols, &mut binds, &mut seen, "creation", SqlValue::Text(now.to_string()));
            put(&mut cols, &mut binds, &mut seen, "modified", SqlValue::Text(now.to_string()));
            put(&mut cols, &mut binds, &mut seen, "docstatus", SqlValue::Integer(0));
            let ph = vec!["?"; cols.len()].join(", ");
            let sql = format!(
                "INSERT INTO {} ({}) VALUES ({})",
                quote_ident(&child_table),
                cols.join(", "),
                ph
            );
            con.execute(&sql, rusqlite::params_from_iter(binds.iter()))?;
        }
    }
    Ok(())
}

fn insert_single(
    con: &Connection,
    meta: &Meta,
    acl: &ReadAcl,
    data: &Map<String, Value>,
    user: &str,
) -> Result<Value, OrmError> {
    with_txn(con, || update_single(con, meta, data, user))?;
    get_single(con, meta, acl)
}

/// Fields a client may never write through update.
const PROTECTED_UPDATE_FIELDS: &[&str] = &[
    "doctype", "name", "creation", "owner", "docstatus", "idx", "parent", "parenttype", "parentfield",
];

/// UPDATE an existing document. Returns the updated doc.
pub fn update(
    con: &Connection,
    meta: &Meta,
    acl: &ReadAcl,
    name: &str,
    data: &Map<String, Value>,
    user: &str,
) -> Result<Value, OrmError> {
    if meta.issingle {
        with_txn(con, || update_single(con, meta, data, user))?;
        return get_single(con, meta, acl);
    }
    if meta.is_virtual {
        return Err(OrmError::NotFound(format!("{} {}", meta.name, name)));
    }

    with_txn(con, || {
        let exists: i64 = con.query_row(
            &format!(
                "SELECT COUNT(*) FROM {} WHERE {}=?1",
                quote_ident(&meta.table),
                quote_ident("name")
            ),
            [name],
            |r| r.get(0),
        )?;
        if exists == 0 {
            return Err(OrmError::NotFound(format!("{} {}", meta.name, name)));
        }

        // Optimistic concurrency (B-DOC-3): if the client sent `modified`, it must match the DB's,
        // else the doc was changed after the client read it (Frappe's TimestampMismatchError, 417).
        if let Some(client_mod) = data.get("modified").and_then(|v| v.as_str()).filter(|s| !s.trim().is_empty()) {
            let db_mod: Option<String> = con
                .query_row(
                    &format!("SELECT {} FROM {} WHERE {}=?1", quote_ident("modified"), quote_ident(&meta.table), quote_ident("name")),
                    [name],
                    |r| r.get(0),
                )
                .ok();
            if let Some(dbm) = db_mod {
                if dbm.trim() != client_mod.trim() {
                    return Err(OrmError::Validation(format!(
                        "Document has been modified after you have opened it ({} vs {}). Please refresh to get the latest document.",
                        dbm.trim(), client_mod.trim()
                    )));
                }
            }
        }

        // set_only_once (B-DOC-2): such a field's value may be set on insert but not changed.
        for f in &meta.fields {
            if !f.set_only_once || !meta.has_column(&f.fieldname) {
                continue;
            }
            if let Some(new_v) = data.get(&f.fieldname) {
                let cur: Option<String> = con
                    .query_row(
                        &format!("SELECT {} FROM {} WHERE {}=?1", quote_ident(&f.fieldname), quote_ident(&meta.table), quote_ident("name")),
                        [name],
                        |r| r.get::<_, Option<String>>(0),
                    )
                    .ok()
                    .flatten();
                if sqlval_text(json_to_sql(new_v)) != cur.unwrap_or_default() {
                    return Err(OrmError::Validation(format!("Value cannot be changed for {}", f.fieldname)));
                }
            }
        }

        // Link / Dynamic Link target existence (B-DOC-1).
        validate_links(con, meta, data)?;

        let now = util::now_datetime();
        let mut sets = Vec::new();
        let mut binds: Vec<SqlValue> = Vec::new();
        let mut child_fields_in_payload = Vec::new();
        for (k, v) in data {
            if PROTECTED_UPDATE_FIELDS.contains(&k.as_str()) {
                continue;
            }
            if let Some(f) = meta.field(k) {
                if f.is_child_table() {
                    child_fields_in_payload.push((k.clone(), v.clone()));
                    continue;
                }
            }
            if meta.has_column(k) {
                sets.push(format!("{}=?", quote_ident(k)));
                binds.push(json_to_sql(v));
            }
        }
        if meta.has_column("modified") {
            sets.push(format!("{}=?", quote_ident("modified")));
            binds.push(SqlValue::Text(now.clone()));
        }
        if meta.has_column("modified_by") {
            sets.push(format!("{}=?", quote_ident("modified_by")));
            binds.push(SqlValue::Text(user.to_string()));
        }
        if !sets.is_empty() {
            binds.push(SqlValue::Text(name.to_string()));
            let sql = format!(
                "UPDATE {} SET {} WHERE {}=?",
                quote_ident(&meta.table),
                sets.join(", "),
                quote_ident("name")
            );
            con.execute(&sql, rusqlite::params_from_iter(binds.iter()))?;
        }

        // Replace child rows that were supplied (full-array replace, matching the REST contract).
        for (fieldname, value) in child_fields_in_payload {
            if let Some(cf) = meta.field(&fieldname) {
                if let Some(child_dt) = &cf.options {
                    let child_table = format!("tab{child_dt}");
                    let _ = con.execute(
                        &format!(
                            "DELETE FROM {} WHERE {}=?1 AND {}=?2 AND {}=?3",
                            quote_ident(&child_table),
                            quote_ident("parent"),
                            quote_ident("parenttype"),
                            quote_ident("parentfield")
                        ),
                        rusqlite::params![name, meta.name, fieldname],
                    );
                }
            }
            let mut one = Map::new();
            one.insert(fieldname.clone(), value);
            insert_children(con, meta, name, &one, user, &now)?;
        }
        Ok(())
    })?;

    get_doc(con, meta, acl, name)
}

fn update_single(con: &Connection, meta: &Meta, data: &Map<String, Value>, user: &str) -> Result<(), OrmError> {
    // Single doctypes go through this path for both insert and update; validate their Link fields
    // here too (B-DOC-1) so a Single can't store a dangling reference.
    validate_links(con, meta, data)?;
    let now = util::now_datetime();
    let set_field = |field: &str, value: &str| -> Result<(), OrmError> {
        con.execute(
            "DELETE FROM \"tabSingles\" WHERE doctype=?1 AND field=?2",
            rusqlite::params![meta.name, field],
        )?;
        con.execute(
            "INSERT INTO \"tabSingles\" (doctype, field, value) VALUES (?1, ?2, ?3)",
            rusqlite::params![meta.name, field, value],
        )?;
        Ok(())
    };
    for (k, v) in data {
        if k == "doctype" || k == "name" {
            continue;
        }
        let s = match v {
            Value::Null => continue,
            Value::String(s) => s.clone(),
            Value::Bool(b) => (*b as i64).to_string(),
            other => other.to_string(),
        };
        set_field(k, &s)?;
    }
    set_field("modified", &now)?;
    set_field("modified_by", user)?;
    Ok(())
}

/// DELETE a document (and its child rows). Returns Ok(()).
pub fn delete(con: &Connection, meta: &Meta, name: &str) -> Result<(), OrmError> {
    if meta.issingle {
        con.execute("DELETE FROM \"tabSingles\" WHERE doctype=?1", [&meta.name])?;
        return Ok(());
    }
    if meta.is_virtual {
        return Err(OrmError::NotFound(format!("{} {}", meta.name, name)));
    }

    with_txn(con, || {
        // Existence + submitted-state check.
        let docstatus: Option<i64> = con
            .query_row(
                &format!(
                    "SELECT {} FROM {} WHERE {}=?1",
                    if meta.has_column("docstatus") { quote_ident("docstatus") } else { "0".to_string() },
                    quote_ident(&meta.table),
                    quote_ident("name")
                ),
                [name],
                |r| r.get::<_, Option<i64>>(0),
            )
            .map_err(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => OrmError::NotFound(format!("{} {}", meta.name, name)),
                other => OrmError::Db(other),
            })?;
        if docstatus == Some(1) {
            return Err(OrmError::Validation(format!(
                "Submitted Document {} {} cannot be deleted",
                meta.name, name
            )));
        }

        for cf in meta.child_tables() {
            if let Some(child_dt) = &cf.options {
                let child_table = format!("tab{child_dt}");
                let _ = con.execute(
                    &format!(
                        "DELETE FROM {} WHERE {}=?1 AND {}=?2",
                        quote_ident(&child_table),
                        quote_ident("parent"),
                        quote_ident("parenttype")
                    ),
                    rusqlite::params![name, meta.name],
                );
            }
        }
        con.execute(
            &format!(
                "DELETE FROM {} WHERE {}=?1",
                quote_ident(&meta.table),
                quote_ident("name")
            ),
            [name],
        )?;
        Ok(())
    })
}

/// Read the `owner` of a document (for if_owner permission scoping). None if not found.
pub fn doc_owner(con: &Connection, meta: &Meta, name: &str) -> Option<String> {
    if !meta.has_column("owner") {
        return None;
    }
    con.query_row(
        &format!(
            "SELECT {} FROM {} WHERE {}=?1",
            quote_ident("owner"),
            quote_ident(&meta.table),
            quote_ident("name")
        ),
        [name],
        |r| r.get::<_, Option<String>>(0),
    )
    .ok()
    .flatten()
}

/// COUNT for list metadata.
#[allow(dead_code)]
pub fn count(
    con: &Connection,
    meta: &Meta,
    filters: &Value,
    owner_scope: Option<&str>,
) -> Result<i64, OrmError> {
    let acl = ReadAcl::all();
    let mut where_parts = Vec::new();
    let mut binds: Vec<SqlValue> = Vec::new();
    // if_owner permission scoping: only count rows the user owns.
    if let Some(owner) = owner_scope {
        if meta.has_column("owner") {
            where_parts.push(format!("{} = ?", quote_ident("owner")));
            binds.push(SqlValue::Text(owner.to_string()));
        }
    }
    for c in parse_filters(meta, &acl, filters)? {
        where_parts.push(c.sql);
        binds.extend(c.binds);
    }
    let where_clause = if where_parts.is_empty() {
        String::new()
    } else {
        format!(" WHERE {}", where_parts.join(" AND "))
    };
    let sql = format!("SELECT COUNT(*) FROM {}{}", quote_ident(&meta.table), where_clause);
    let n: i64 = con.query_row(&sql, rusqlite::params_from_iter(binds.iter()), |r| r.get(0))?;
    Ok(n)
}
