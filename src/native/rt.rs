//! rt — the dynamic runtime that transpiled Frappe controllers call into.
//!
//! Every transpiled Python expression evaluates to a `serde_json::Value` (the same type ferro's
//! ORM speaks), so there is no static type-inference problem: we reproduce Python/Frappe's
//! *dynamic* semantics directly (`flt(None)==0.0`, truthiness of "" / 0 / [] , mixed-type
//! arithmetic, …). A transpiled controller method has the signature
//!
//!     fn(doc: &mut Value, ctx: &mut Ctx) -> Result<Value, FerroErr>
//!
//! and mutates `doc` in place. Reads/writes of `self.<field>` and child-table rows go through the
//! `doc_*` / `child_*` helpers below. `frappe.*` calls that touch the database take `ctx`, which
//! carries the live SQLite connection + meta cache, so a controller's ORM calls run on the very
//! same transaction as the write that triggered it.

#![allow(dead_code)]

use ferro::meta::MetaCache;
use ferro::orm::{self, ListQuery, OrmError, ReadAcl};
use ferro::util;
use rusqlite::Connection;
use serde_json::{json, Map, Value};
use std::sync::Arc;

// ----------------------------------------------------------------- errors / context
#[derive(Debug)]
pub enum FerroErr {
    /// frappe.throw / ValidationError -> HTTP 417
    Throw(String),
    /// frappe.PermissionError -> 403
    Perm(String),
    /// frappe.DoesNotExistError -> 404
    NotFound(String),
    Db(String),
}
impl From<OrmError> for FerroErr {
    fn from(e: OrmError) -> Self {
        match e {
            OrmError::NotFound(s) => FerroErr::NotFound(s),
            OrmError::Validation(s) => FerroErr::Throw(s),
            OrmError::Duplicate(s) => FerroErr::Throw(s),
            OrmError::Db(e) => FerroErr::Db(e.to_string()),
        }
    }
}

/// Per-request context handed to every transpiled controller. Borrows the request's connection
/// (so ORM calls share the write transaction) and the process-wide meta cache.
pub struct Ctx<'a> {
    pub con: &'a Connection,
    pub metas: &'a Arc<MetaCache>,
    pub user: String,
    pub messages: Vec<String>,
}
impl<'a> Ctx<'a> {
    pub fn new(con: &'a Connection, metas: &'a Arc<MetaCache>, user: &str) -> Self {
        Ctx { con, metas, user: user.to_string(), messages: Vec::new() }
    }
}

// ================================================================= value helpers
#[inline]
pub fn null() -> Value {
    Value::Null
}
pub fn s(x: &str) -> Value {
    Value::from(x.to_string())
}
pub fn int(x: i64) -> Value {
    Value::from(x)
}
pub fn float(x: f64) -> Value {
    json!(x)
}
pub fn boolean(x: bool) -> Value {
    Value::Bool(x)
}

/// Python truthiness: None/False/0/0.0/""/[]/{} are falsy.
pub fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().map(|f| f != 0.0).unwrap_or(false),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

pub fn as_f64(v: &Value) -> f64 {
    match v {
        Value::Null => 0.0,
        Value::Bool(b) => *b as i64 as f64,
        Value::Number(n) => n.as_f64().unwrap_or(0.0),
        Value::String(s) => util::flt(s),
        _ => 0.0,
    }
}
pub fn as_i64(v: &Value) -> i64 {
    match v {
        Value::Null => 0,
        Value::Bool(b) => *b as i64,
        Value::Number(n) => n.as_i64().unwrap_or_else(|| n.as_f64().unwrap_or(0.0) as i64),
        Value::String(s) => util::cint(s),
        _ => 0,
    }
}
pub fn as_str(v: &Value) -> String {
    match v {
        Value::Null => String::new(),
        Value::String(s) => s.clone(),
        Value::Bool(b) => if *b { "1".into() } else { "0".into() },
        Value::Number(n) => n.to_string(),
        other => other.to_string(),
    }
}
/// True if a JSON number is integral (so `5/2` style code knows int vs float).
fn is_int(v: &Value) -> bool {
    matches!(v, Value::Number(n) if n.is_i64() || n.is_u64())
}
fn num(f: f64) -> Value {
    if f.fract() == 0.0 && f.abs() < 9.007e15 {
        Value::from(f as i64)
    } else {
        json!(f)
    }
}

// ----------------------------------------------------------------- arithmetic (Python semantics)
pub fn add(a: &Value, b: &Value) -> Value {
    // string concat, list concat, else numeric
    match (a, b) {
        (Value::String(x), _) => Value::from(format!("{x}{}", as_str(b))),
        (_, Value::String(y)) if matches!(a, Value::String(_)) => Value::from(format!("{}{y}", as_str(a))),
        (Value::Array(x), Value::Array(y)) => {
            let mut v = x.clone();
            v.extend(y.clone());
            Value::Array(v)
        }
        _ => {
            if is_int(a) && is_int(b) {
                Value::from(as_i64(a) + as_i64(b))
            } else {
                num(as_f64(a) + as_f64(b))
            }
        }
    }
}
pub fn sub(a: &Value, b: &Value) -> Value {
    if is_int(a) && is_int(b) {
        Value::from(as_i64(a) - as_i64(b))
    } else {
        num(as_f64(a) - as_f64(b))
    }
}
pub fn mul(a: &Value, b: &Value) -> Value {
    if is_int(a) && is_int(b) {
        Value::from(as_i64(a) * as_i64(b))
    } else {
        num(as_f64(a) * as_f64(b))
    }
}
pub fn div(a: &Value, b: &Value) -> Value {
    // Python `/` is always float
    let d = as_f64(b);
    if d == 0.0 {
        return Value::from(0.0);
    }
    json!(as_f64(a) / d)
}
pub fn floordiv(a: &Value, b: &Value) -> Value {
    let d = as_f64(b);
    if d == 0.0 {
        return Value::from(0);
    }
    num((as_f64(a) / d).floor())
}
pub fn modulo(a: &Value, b: &Value) -> Value {
    // Python `%`: the result takes the sign of the divisor (unlike Rust's truncated `%`).
    if is_int(a) && is_int(b) {
        let (x, d) = (as_i64(a), as_i64(b));
        if d == 0 {
            return Value::from(0);
        }
        let r = x % d;
        Value::from(if r != 0 && (r < 0) != (d < 0) { r + d } else { r })
    } else {
        let (x, d) = (as_f64(a), as_f64(b));
        if d == 0.0 {
            return Value::from(0.0);
        }
        let r = x % d;
        num(if r != 0.0 && (r < 0.0) != (d < 0.0) { r + d } else { r })
    }
}
pub fn pow(a: &Value, b: &Value) -> Value {
    num(as_f64(a).powf(as_f64(b)))
}
pub fn neg(a: &Value) -> Value {
    if is_int(a) {
        Value::from(-as_i64(a))
    } else {
        num(-as_f64(a))
    }
}

// ----------------------------------------------------------------- comparison
fn cmp_num(a: &Value, b: &Value) -> Option<std::cmp::Ordering> {
    as_f64(a).partial_cmp(&as_f64(b))
}
pub fn eq(a: &Value, b: &Value) -> Value {
    let r = match (a, b) {
        (Value::Null, Value::Null) => true,
        (Value::Null, _) | (_, Value::Null) => false,
        (Value::String(x), Value::String(y)) => x == y,
        (Value::Bool(x), Value::Bool(y)) => x == y,
        (Value::Number(_), Value::Number(_)) => as_f64(a) == as_f64(b),
        _ => a == b,
    };
    Value::Bool(r)
}
pub fn ne(a: &Value, b: &Value) -> Value {
    Value::Bool(!truthy(&eq(a, b)))
}
pub fn lt(a: &Value, b: &Value) -> Value {
    if let (Value::String(x), Value::String(y)) = (a, b) {
        return Value::Bool(x < y);
    }
    Value::Bool(cmp_num(a, b) == Some(std::cmp::Ordering::Less))
}
pub fn le(a: &Value, b: &Value) -> Value {
    Value::Bool(truthy(&lt(a, b)) || truthy(&eq(a, b)))
}
pub fn gt(a: &Value, b: &Value) -> Value {
    Value::Bool(!truthy(&le(a, b)))
}
pub fn ge(a: &Value, b: &Value) -> Value {
    Value::Bool(!truthy(&lt(a, b)))
}
pub fn contains(item: &Value, container: &Value) -> Value {
    let r = match container {
        Value::Array(a) => a.iter().any(|e| truthy(&eq(e, item))),
        Value::Object(o) => o.contains_key(&as_str(item)),
        Value::String(s) => s.contains(&as_str(item)),
        _ => false,
    };
    Value::Bool(r)
}
pub fn is_(a: &Value, b: &Value) -> Value {
    // we only ever compare against None in practice
    Value::Bool(matches!((a, b), (Value::Null, Value::Null)) || (truthy(&eq(a, b)) && matches!(a, Value::Bool(_))))
}

// ----------------------------------------------------------------- doc / field access
pub fn doc_get(doc: &Value, field: &str) -> Value {
    doc.get(field).cloned().unwrap_or(Value::Null)
}
pub fn doc_set(doc: &mut Value, field: &str, v: Value) {
    if let Value::Object(m) = doc {
        m.insert(field.to_string(), v);
    }
}
/// number of rows in a child table field
pub fn child_len(doc: &Value, table: &str) -> usize {
    doc.get(table).and_then(|v| v.as_array()).map(|a| a.len()).unwrap_or(0)
}
pub fn child_get(doc: &Value, table: &str, idx: usize, field: &str) -> Value {
    doc.get(table)
        .and_then(|v| v.as_array())
        .and_then(|a| a.get(idx))
        .and_then(|r| r.get(field))
        .cloned()
        .unwrap_or(Value::Null)
}
pub fn child_set(doc: &mut Value, table: &str, idx: usize, field: &str, v: Value) {
    if let Some(Value::Array(a)) = doc.get_mut(table) {
        if let Some(Value::Object(row)) = a.get_mut(idx) {
            row.insert(field.to_string(), v);
        }
    }
}
/// clone a whole child row (used when a row escapes the loop cursor)
pub fn child_row(doc: &Value, table: &str, idx: usize) -> Value {
    doc.get(table)
        .and_then(|v| v.as_array())
        .and_then(|a| a.get(idx))
        .cloned()
        .unwrap_or(Value::Null)
}
/// generic attribute read on any value (a fetched doc, a dict row, …)
pub fn attr(v: &Value, name: &str) -> Value {
    v.get(name).cloned().unwrap_or(Value::Null)
}
/// append a row (dict) to a child table and return the appended row
pub fn append(doc: &mut Value, table: &str, row: Value) -> Value {
    if let Value::Object(m) = doc {
        let arr = m.entry(table.to_string()).or_insert_with(|| Value::Array(vec![]));
        if let Value::Array(a) = arr {
            a.push(row.clone());
        }
    }
    row
}

// ----------------------------------------------------------------- builtins
pub fn flt(v: &Value, prec: Option<i64>) -> Value {
    let f = as_f64(v);
    match prec {
        Some(p) if p >= 0 => {
            let m = 10f64.powi(p as i32);
            json!((f * m).round() / m)
        }
        _ => json!(f),
    }
}
pub fn cint(v: &Value) -> Value {
    Value::from(as_i64(v))
}
pub fn cstr(v: &Value) -> Value {
    Value::from(as_str(v))
}
pub fn py_len(v: &Value) -> Value {
    let n = match v {
        Value::Array(a) => a.len(),
        Value::Object(o) => o.len(),
        Value::String(s) => s.chars().count(),
        _ => 0,
    };
    Value::from(n as i64)
}
pub fn py_abs(v: &Value) -> Value {
    if is_int(v) {
        Value::from(as_i64(v).abs())
    } else {
        num(as_f64(v).abs())
    }
}
pub fn py_round(v: &Value, ndigits: Option<i64>) -> Value {
    flt(v, Some(ndigits.unwrap_or(0)))
}
pub fn py_str(v: &Value) -> Value {
    Value::from(as_str(v))
}
pub fn py_bool(v: &Value) -> Value {
    Value::Bool(truthy(v))
}
pub fn py_min(items: &[Value]) -> Value {
    items.iter().cloned().reduce(|a, b| if truthy(&le(&a, &b)) { a } else { b }).unwrap_or(Value::Null)
}
pub fn py_max(items: &[Value]) -> Value {
    items.iter().cloned().reduce(|a, b| if truthy(&ge(&a, &b)) { a } else { b }).unwrap_or(Value::Null)
}
/// min(iterable)/max(iterable) — Python's single-iterable form: reduce over the *elements*.
pub fn py_min_iter(v: &Value) -> Value {
    py_min(&as_list(v))
}
pub fn py_max_iter(v: &Value) -> Value {
    py_max(&as_list(v))
}
pub fn py_sum(v: &Value) -> Value {
    match v {
        Value::Array(a) => a.iter().fold(Value::from(0), |acc, x| add(&acc, x)),
        _ => Value::from(0),
    }
}
pub fn py_range(args: &[Value]) -> Value {
    let (start, stop, step) = match args.len() {
        1 => (0, as_i64(&args[0]), 1),
        2 => (as_i64(&args[0]), as_i64(&args[1]), 1),
        _ => (as_i64(&args[0]), as_i64(&args[1]), as_i64(&args[2]).max(1)),
    };
    let mut out = vec![];
    let mut i = start;
    while i < stop {
        out.push(Value::from(i));
        i += step;
    }
    Value::Array(out)
}
pub fn as_list(v: &Value) -> Vec<Value> {
    match v {
        Value::Array(a) => a.clone(),
        Value::Null => vec![],
        Value::Object(o) => o.keys().map(|k| Value::from(k.clone())).collect(), // dict iterates keys
        Value::String(s) => s.chars().map(|c| Value::from(c.to_string())).collect(),
        other => vec![other.clone()],
    }
}

// string methods
pub fn str_strip(v: &Value) -> Value {
    Value::from(as_str(v).trim().to_string())
}
pub fn str_lower(v: &Value) -> Value {
    Value::from(as_str(v).to_lowercase())
}
pub fn str_upper(v: &Value) -> Value {
    Value::from(as_str(v).to_uppercase())
}
pub fn str_startswith(v: &Value, p: &Value) -> Value {
    Value::Bool(as_str(v).starts_with(&as_str(p)))
}
pub fn str_endswith(v: &Value, p: &Value) -> Value {
    Value::Bool(as_str(v).ends_with(&as_str(p)))
}
pub fn str_replace(v: &Value, a: &Value, b: &Value) -> Value {
    Value::from(as_str(v).replace(&as_str(a), &as_str(b)))
}
pub fn str_split(v: &Value, sep: Option<&Value>) -> Value {
    let s = as_str(v);
    let parts: Vec<Value> = match sep {
        Some(sp) => s.split(&as_str(sp)).map(|x| Value::from(x.to_string())).collect(),
        None => s.split_whitespace().map(|x| Value::from(x.to_string())).collect(),
    };
    Value::Array(parts)
}
pub fn str_join(sep: &Value, items: &Value) -> Value {
    let parts: Vec<String> = as_list(items).iter().map(as_str).collect();
    Value::from(parts.join(&as_str(sep)))
}

/// Python str.format — supports {}, {0}, {name} (no format spec / conversions).
pub fn str_format(template: &Value, pos: &[Value], named: &[(String, Value)]) -> Value {
    let t = as_str(template);
    let mut out = String::with_capacity(t.len());
    let mut chars = t.chars().peekable();
    let mut auto = 0usize;
    while let Some(c) = chars.next() {
        if c == '{' {
            if chars.peek() == Some(&'{') {
                chars.next();
                out.push('{');
                continue;
            }
            let mut key = String::new();
            while let Some(&n) = chars.peek() {
                if n == '}' {
                    break;
                }
                key.push(n);
                chars.next();
            }
            chars.next(); // consume '}'
            // split "name:spec" — keep the spec so {:.2f} etc. format correctly
            let mut parts = key.splitn(2, ':');
            let name = parts.next().unwrap_or("").trim().to_string();
            let spec = parts.next().unwrap_or("").trim().to_string();
            // strip a !r/!s conversion off the name
            let name = name.split('!').next().unwrap_or("").trim().to_string();
            let val = if name.is_empty() {
                let v = pos.get(auto).cloned().unwrap_or(Value::Null);
                auto += 1;
                v
            } else if let Ok(i) = name.parse::<usize>() {
                pos.get(i).cloned().unwrap_or(Value::Null)
            } else {
                named.iter().find(|(k, _)| *k == name).map(|(_, v)| v.clone()).unwrap_or(Value::Null)
            };
            out.push_str(&as_str(&fmt_value(&val, &spec)));
        } else if c == '}' {
            if chars.peek() == Some(&'}') {
                chars.next();
            }
            out.push('}');
        } else {
            out.push(c);
        }
    }
    Value::from(out)
}

/// Apply a Python format-spec to a value (the common subset: `.Nf`, `.N%`, `,`, `d`, plain).
pub fn fmt_value(v: &Value, spec: &str) -> Value {
    if spec.is_empty() {
        return Value::from(as_str(v));
    }
    let comma = spec.contains(',');
    if let Some(dot) = spec.find('.') {
        let rest = &spec[dot + 1..];
        let digits: usize = rest.chars().take_while(|c| c.is_ascii_digit()).collect::<String>().parse().unwrap_or(6);
        let last = rest.chars().last().unwrap_or('f');
        let f = as_f64(v);
        if last == '%' {
            return Value::from(format!("{:.*}%", digits, f * 100.0));
        }
        let s = format!("{:.*}", digits, f);
        return Value::from(if comma { group_thousands(&s) } else { s });
    }
    if spec.ends_with('f') {
        return Value::from(format!("{:.6}", as_f64(v)));
    }
    if spec.ends_with('d') {
        return Value::from(as_i64(v).to_string());
    }
    if comma {
        return Value::from(group_thousands(&as_str(v)));
    }
    Value::from(as_str(v))
}
fn group_thousands(s: &str) -> String {
    let (sign, body) = if let Some(rest) = s.strip_prefix('-') { ("-", rest) } else { ("", s) };
    let (int_part, frac) = match body.split_once('.') {
        Some((i, f)) => (i, Some(f)),
        None => (body, None),
    };
    let bytes = int_part.as_bytes();
    let mut grouped = String::new();
    for (i, c) in bytes.iter().enumerate() {
        if i > 0 && (bytes.len() - i) % 3 == 0 {
            grouped.push(',');
        }
        grouped.push(*c as char);
    }
    match frac {
        Some(f) => format!("{sign}{grouped}.{f}"),
        None => format!("{sign}{grouped}"),
    }
}

/// f-string / general string build: concatenate already-stringified parts.
pub fn concat_str(parts: &[Value]) -> Value {
    let mut out = String::new();
    for p in parts {
        out.push_str(&as_str(p));
    }
    Value::from(out)
}

pub fn bold(v: &Value) -> Value {
    Value::from(format!("<b>{}</b>", as_str(v)))
}
/// frappe._ — translation is identity on a single-locale deployment.
pub fn i18n(v: &Value) -> Value {
    Value::from(as_str(v))
}
pub fn get_link_to_form(dt: &Value, name: &Value) -> Value {
    Value::from(format!("<a href=\"/app/{}/{}\">{}</a>", as_str(dt), as_str(name), as_str(name)))
}

// dict / list literals
pub fn dict(pairs: Vec<(String, Value)>) -> Value {
    let mut m = Map::new();
    for (k, v) in pairs {
        m.insert(k, v);
    }
    Value::Object(m)
}
pub fn list(items: Vec<Value>) -> Value {
    Value::Array(items)
}
pub fn subscript(v: &Value, key: &Value) -> Value {
    match v {
        Value::Array(a) => {
            let i = as_i64(key);
            let idx = if i < 0 { (a.len() as i64 + i) as usize } else { i as usize };
            a.get(idx).cloned().unwrap_or(Value::Null)
        }
        Value::Object(o) => o.get(&as_str(key)).cloned().unwrap_or(Value::Null),
        Value::String(s) => {
            let i = as_i64(key);
            s.chars().nth(i as usize).map(|c| Value::from(c.to_string())).unwrap_or(Value::Null)
        }
        _ => Value::Null,
    }
}
pub fn dict_get(v: &Value, key: &Value, default: Option<Value>) -> Value {
    match v.get(as_str(key).as_str()) {
        Some(x) => x.clone(),
        None => default.unwrap_or(Value::Null),
    }
}

// ----------------------------------------------------------------- frappe.* db bridges (need ctx)
fn meta_of<'a>(ctx: &'a Ctx, dt: &str) -> Result<Arc<ferro::meta::Meta>, FerroErr> {
    ctx.metas.get(ctx.con, dt).map_err(|e| match e {
        ferro::meta::MetaError::NotFound(d) => FerroErr::NotFound(format!("DocType {d} not found")),
        ferro::meta::MetaError::Db(e) => FerroErr::Db(e.to_string()),
    })
}

/// frappe.db.get_value(dt, name_or_filters, fieldname) — single value (scalar) form.
pub fn db_get_value(ctx: &mut Ctx, dt: &Value, name: &Value, field: &Value) -> Result<Value, FerroErr> {
    let dt = as_str(dt);
    let meta = meta_of(ctx, &dt)?;
    let acl = ReadAcl::all();
    // filters form (dict) vs name form (string)
    if let Value::Object(_) = name {
        let mut q = ListQuery::default();
        q.filters = name.clone();
        q.fields = vec![as_str(field)];
        q.limit_page_length = 1;
        let rows = orm::get_list(ctx.con, &meta, &acl, &q, None)?;
        if let Some(first) = rows.as_array().and_then(|a| a.first()) {
            return Ok(attr(first, &as_str(field)));
        }
        return Ok(Value::Null);
    }
    let nm = as_str(name);
    if nm.is_empty() {
        return Ok(Value::Null);
    }
    match orm::get_doc(ctx.con, &meta, &acl, &nm) {
        Ok(doc) => Ok(attr(&doc, &as_str(field))),
        Err(OrmError::NotFound(_)) => Ok(Value::Null),
        Err(e) => Err(e.into()),
    }
}

pub fn db_exists(ctx: &mut Ctx, dt: &Value, name: &Value) -> Result<Value, FerroErr> {
    let dt = as_str(dt);
    let meta = meta_of(ctx, &dt)?;
    let acl = ReadAcl::all();
    if let Value::Object(_) = name {
        let mut q = ListQuery::default();
        q.filters = name.clone();
        q.fields = vec!["name".into()];
        q.limit_page_length = 1;
        let rows = orm::get_list(ctx.con, &meta, &acl, &q, None)?;
        return Ok(match rows.as_array().and_then(|a| a.first()) {
            Some(r) => attr(r, "name"),
            None => Value::Null,
        });
    }
    let nm = as_str(name);
    match orm::get_doc(ctx.con, &meta, &acl, &nm) {
        Ok(_) => Ok(Value::from(nm)),
        Err(OrmError::NotFound(_)) => Ok(Value::Null),
        Err(e) => Err(e.into()),
    }
}

pub fn db_count(ctx: &mut Ctx, dt: &Value, filters: Option<&Value>) -> Result<Value, FerroErr> {
    let dt = as_str(dt);
    let meta = meta_of(ctx, &dt)?;
    let f = filters.cloned().unwrap_or(Value::Null);
    Ok(Value::from(orm::count(ctx.con, &meta, &f, None)?))
}

/// frappe.get_all / frappe.db.get_all / get_list — returns array of dicts (or names).
pub fn get_all(
    ctx: &mut Ctx,
    dt: &Value,
    filters: Option<&Value>,
    fields: Option<&Value>,
) -> Result<Value, FerroErr> {
    let dt = as_str(dt);
    let meta = meta_of(ctx, &dt)?;
    let acl = ReadAcl::all();
    let mut q = ListQuery::default();
    q.limit_page_length = 0; // unlimited
    if let Some(f) = filters {
        q.filters = f.clone();
    }
    if let Some(Value::Array(fl)) = fields {
        let parsed: Vec<String> = fl.iter().map(as_str).collect();
        if !parsed.is_empty() {
            q.fields = parsed;
        }
    } else {
        q.fields = vec!["name".into()];
    }
    Ok(orm::get_list(ctx.con, &meta, &acl, &q, None)?)
}

/// frappe.get_doc(dt, name) -> a detached doc Value.
pub fn get_doc(ctx: &mut Ctx, dt: &Value, name: &Value) -> Result<Value, FerroErr> {
    let dt = as_str(dt);
    let meta = meta_of(ctx, &dt)?;
    let acl = ReadAcl::all();
    Ok(orm::get_doc(ctx.con, &meta, &acl, &as_str(name))?)
}

/// frappe.db.get_single_value(dt, field)
pub fn get_single_value(ctx: &mut Ctx, dt: &Value, field: &Value) -> Result<Value, FerroErr> {
    let dt = as_str(dt);
    let meta = meta_of(ctx, &dt)?;
    let acl = ReadAcl::all();
    let doc = orm::get_doc(ctx.con, &meta, &acl, &dt)?;
    Ok(attr(&doc, &as_str(field)))
}

/// self.db_set(field, value) / frappe.db.set_value — direct column write on a saved doc.
pub fn db_set_value(ctx: &mut Ctx, dt: &Value, name: &Value, field: &Value, val: &Value) -> Result<Value, FerroErr> {
    let dt = as_str(dt);
    let meta = meta_of(ctx, &dt)?;
    let acl = ReadAcl::all();
    let mut m = Map::new();
    m.insert(as_str(field), val.clone());
    orm::update(ctx.con, &meta, &acl, &as_str(name), &m, &ctx.user.clone())?;
    Ok(Value::Null)
}

pub fn throw(msg: &Value) -> FerroErr {
    FerroErr::Throw(as_str(msg))
}

/// frappe.new_doc(dt) -> a fresh in-memory doc dict.
pub fn new_doc(dt: &Value) -> Value {
    let mut m = Map::new();
    m.insert("doctype".into(), Value::from(as_str(dt)));
    Value::Object(m)
}
/// frappe.delete_doc(dt, name)
pub fn delete_doc(ctx: &mut Ctx, dt: &Value, name: &Value) -> Result<Value, FerroErr> {
    let dt = as_str(dt);
    let meta = meta_of(ctx, &dt)?;
    match orm::delete(ctx.con, &meta, &as_str(name)) {
        Ok(()) => Ok(Value::Null),
        Err(OrmError::NotFound(_)) => Ok(Value::Null),
        Err(e) => Err(e.into()),
    }
}
/// frappe.scrub("Sales Invoice") -> "sales_invoice"
pub fn scrub(v: &Value) -> Value {
    Value::from(as_str(v).trim().to_lowercase().replace([' ', '-'], "_"))
}
pub fn has_attr(v: &Value, name: &Value) -> Value {
    Value::Bool(v.get(as_str(name).as_str()).is_some())
}
/// dict.update(other) / self.update(other) -> merge other's keys into v.
pub fn update_into(v: &mut Value, other: &Value) -> Value {
    if let (Value::Object(m), Value::Object(o)) = (&mut *v, other) {
        for (k, val) in o {
            m.insert(k.clone(), val.clone());
        }
    }
    Value::Null
}
pub fn db_set_single_value(ctx: &mut Ctx, dt: &Value, field: &Value, val: &Value) -> Result<Value, FerroErr> {
    let dt = as_str(dt);
    let meta = meta_of(ctx, &dt)?;
    let acl = ReadAcl::all();
    let mut m = Map::new();
    m.insert(as_str(field), val.clone());
    orm::update(ctx.con, &meta, &acl, &dt, &m, &ctx.user.clone())?;
    Ok(Value::Null)
}
/// list(x) constructor
pub fn to_list(v: &Value) -> Value {
    Value::Array(as_list(v))
}

/// frappe.db.sql(query, params, as_dict) -> rows. Raw SQLite passthrough (read-only safe forms).
/// Returns a list of dicts when as_dict, else a list of lists. Params: scalar, list, or None.
pub fn db_sql(ctx: &mut Ctx, query: &Value, params: &Value, as_dict: bool) -> Result<Value, FerroErr> {
    let q = as_str(query);
    // normalise common MariaDB-isms to SQLite
    let q = q.replace('`', "\"");
    let binds: Vec<rusqlite::types::Value> = match params {
        Value::Null => vec![],
        Value::Array(a) => a.iter().map(json_to_sqlite).collect(),
        other => vec![json_to_sqlite(other)],
    };
    let mut stmt = ctx.con.prepare(&q).map_err(|e| FerroErr::Db(e.to_string()))?;
    let ncol = stmt.column_count();
    let colnames: Vec<String> = stmt.column_names().iter().map(|s| s.to_string()).collect();
    let rows = stmt
        .query_map(rusqlite::params_from_iter(binds.iter()), |row| {
            if as_dict {
                let mut m = Map::new();
                for i in 0..ncol {
                    m.insert(colnames[i].clone(), sqlite_to_json(row.get_ref(i)?));
                }
                Ok(Value::Object(m))
            } else {
                let mut a = Vec::with_capacity(ncol);
                for i in 0..ncol {
                    a.push(sqlite_to_json(row.get_ref(i)?));
                }
                Ok(Value::Array(a))
            }
        })
        .map_err(|e| FerroErr::Db(e.to_string()))?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r.map_err(|e| FerroErr::Db(e.to_string()))?);
    }
    Ok(Value::Array(out))
}
fn json_to_sqlite(v: &Value) -> rusqlite::types::Value {
    use rusqlite::types::Value as S;
    match v {
        Value::Null => S::Null,
        Value::Bool(b) => S::Integer(*b as i64),
        Value::Number(n) => n.as_i64().map(S::Integer).unwrap_or_else(|| S::Real(n.as_f64().unwrap_or(0.0))),
        Value::String(s) => S::Text(s.clone()),
        other => S::Text(other.to_string()),
    }
}
fn sqlite_to_json(v: rusqlite::types::ValueRef) -> Value {
    use rusqlite::types::ValueRef;
    match v {
        ValueRef::Null => Value::Null,
        ValueRef::Integer(i) => Value::from(i),
        ValueRef::Real(f) => json!(f),
        ValueRef::Text(t) => Value::from(String::from_utf8_lossy(t).into_owned()),
        ValueRef::Blob(_) => Value::Null,
    }
}
pub fn msgprint(ctx: &mut Ctx, msg: &Value) -> Value {
    ctx.messages.push(as_str(msg));
    Value::Null
}

/// Frappe DocField precision: default 6 for floats, 3 for currency-less; we use a fixed default
/// (the field-specific precision is a display concern, not a correctness one for the demo).
pub fn precision(_field: &Value) -> Value {
    Value::from(6)
}

// ----------------------------------------------------------------- list / dict mutation
pub fn list_push(v: &mut Value, item: Value) -> Value {
    if !v.is_array() {
        *v = Value::Array(vec![]);
    }
    if let Value::Array(a) = v {
        a.push(item);
    }
    Value::Null
}
pub fn set_item(v: &mut Value, key: &Value, val: Value) {
    match v {
        Value::Array(a) => {
            let i = as_i64(key) as usize;
            if i < a.len() {
                a[i] = val;
            }
        }
        Value::Object(o) => {
            o.insert(as_str(key), val);
        }
        _ => {
            let mut o = Map::new();
            o.insert(as_str(key), val);
            *v = Value::Object(o);
        }
    }
}
/// dict.items() / object entries as a list of [k, v] pairs (for `for k, v in d.items()`).
pub fn items(v: &Value) -> Value {
    match v {
        Value::Object(o) => Value::Array(o.iter().map(|(k, val)| json!([k, val])).collect()),
        _ => Value::Array(vec![]),
    }
}
pub fn keys(v: &Value) -> Value {
    match v {
        Value::Object(o) => Value::Array(o.keys().map(|k| Value::from(k.clone())).collect()),
        _ => Value::Array(vec![]),
    }
}
pub fn values(v: &Value) -> Value {
    match v {
        Value::Object(o) => Value::Array(o.values().cloned().collect()),
        Value::Array(a) => Value::Array(a.clone()),
        _ => Value::Array(vec![]),
    }
}
pub fn py_any(v: &Value) -> Value {
    Value::Bool(as_list(v).iter().any(truthy))
}
pub fn py_all(v: &Value) -> Value {
    Value::Bool(as_list(v).iter().all(truthy))
}

// ----------------------------------------------------------------- self.* document helpers
/// self.get(field) — dynamic field name.
pub fn dget(doc: &Value, key: &Value) -> Value {
    doc.get(as_str(key).as_str()).cloned().unwrap_or(Value::Null)
}
pub fn dset(doc: &mut Value, key: &Value, v: Value) -> Value {
    if let Value::Object(m) = doc {
        m.insert(as_str(key), v);
    }
    Value::Null
}
pub fn append_v(doc: &mut Value, table: &Value, row: Value) -> Value {
    append(doc, &as_str(table), row)
}
/// `<localdoc>.append("table", row)` — append a child row to an arbitrary doc-valued local
/// (e.g. a fetched/new doc), not just the controller's own `doc`.
pub fn append_to(v: &mut Value, table: &Value, row: Value) -> Value {
    if !v.is_object() {
        *v = Value::Object(Map::new());
    }
    append(v, &as_str(table), row)
}
pub fn doc_is_new(doc: &Value) -> Value {
    let has_name = doc.get("name").map(|n| !as_str(n).is_empty()).unwrap_or(false);
    Value::Bool(!has_name || truthy(&doc.get("__islocal").cloned().unwrap_or(Value::Null)))
}

/// self.db_set(field, value) — write one column to this doc's row AND update the in-memory doc.
pub fn self_db_set(ctx: &mut Ctx, doc: &mut Value, field: &Value, val: &Value) -> Result<Value, FerroErr> {
    doc_set(doc, &as_str(field), val.clone());
    let dt = as_str(&doc_get(doc, "doctype"));
    let name = as_str(&doc_get(doc, "name"));
    if dt.is_empty() || name.is_empty() {
        return Ok(Value::Null); // not yet persisted; in-memory set is enough
    }
    db_set_value(ctx, &Value::from(dt), &Value::from(name), field, val)
}

/// self.save() — persist the in-memory doc's writable fields.
pub fn self_save(ctx: &mut Ctx, doc: &mut Value) -> Result<Value, FerroErr> {
    let dt = as_str(&doc_get(doc, "doctype"));
    let name = as_str(&doc_get(doc, "name"));
    if dt.is_empty() || name.is_empty() {
        return Ok(Value::Null);
    }
    let meta = meta_of(ctx, &dt)?;
    let acl = ReadAcl::all();
    let map = doc.as_object().cloned().unwrap_or_default();
    orm::update(ctx.con, &meta, &acl, &name, &map, &ctx.user.clone())?;
    Ok(Value::Null)
}

// ----------------------------------------------------------------- dates (ISO, dependency-free)
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = (y - era * 400) as i64;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}
fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}
/// Parse "YYYY-MM-DD" (optionally followed by time) into epoch-days; None if unparseable.
fn parse_date(s: &str) -> Option<i64> {
    let s = s.trim();
    let datepart = s.split([' ', 'T']).next().unwrap_or(s);
    let mut it = datepart.split('-');
    let y: i64 = it.next()?.parse().ok()?;
    let m: i64 = it.next()?.parse().ok()?;
    let d: i64 = it.next()?.parse().ok()?;
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    Some(days_from_civil(y, m, d))
}
fn iso_from_days(days: i64) -> String {
    let (y, m, d) = civil_from_days(days);
    format!("{:04}-{:02}-{:02}", y, m, d)
}
pub fn today() -> Value {
    Value::from(util::now_date())
}
pub fn now_datetime() -> Value {
    Value::from(util::now_datetime())
}
/// frappe.utils.getdate: normalise to an ISO date string; empty/None -> today.
pub fn getdate(v: &Value) -> Value {
    let s = as_str(v);
    if s.is_empty() {
        return today();
    }
    match parse_date(&s) {
        Some(days) => Value::from(iso_from_days(days)),
        None => Value::from(s),
    }
}
/// frappe.utils.get_datetime: keep the original (datetime-comparable) string, defaulted to now.
pub fn get_datetime(v: &Value) -> Value {
    let s = as_str(v);
    if s.is_empty() {
        now_datetime()
    } else {
        Value::from(s)
    }
}
pub fn add_days(v: &Value, n: &Value) -> Value {
    match parse_date(&as_str(v)) {
        Some(days) => Value::from(iso_from_days(days + as_i64(n))),
        None => v.clone(),
    }
}
pub fn add_months(v: &Value, n: &Value) -> Value {
    let s = as_str(v);
    if let Some(days) = parse_date(&s) {
        let (y, m, d) = civil_from_days(days);
        let total = (y * 12 + (m - 1)) + as_i64(n);
        let (ny, nm) = (total.div_euclid(12), total.rem_euclid(12) + 1);
        // clamp day to month length
        let dim = days_in_month(ny, nm);
        Value::from(iso_from_days(days_from_civil(ny, nm, d.min(dim))))
    } else {
        v.clone()
    }
}
fn days_in_month(y: i64, m: i64) -> i64 {
    match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => if (y % 4 == 0 && y % 100 != 0) || y % 400 == 0 { 29 } else { 28 },
        _ => 30,
    }
}
/// frappe.utils.date_diff(a, b) -> integer days (a - b).
pub fn date_diff(a: &Value, b: &Value) -> Value {
    match (parse_date(&as_str(a)), parse_date(&as_str(b))) {
        (Some(x), Some(y)) => Value::from(x - y),
        _ => Value::from(0),
    }
}
/// frappe.utils.add_to_date(date, days=, months=, years=)
pub fn add_to_date(v: &Value, days: i64, months: i64, years: i64) -> Value {
    let mut out = v.clone();
    if months != 0 || years != 0 {
        out = add_months(&out, &Value::from(months + years * 12));
    }
    if days != 0 {
        out = add_days(&out, &Value::from(days));
    }
    out
}
pub fn formatdate(v: &Value) -> Value {
    getdate(v)
}

// ----------------------------------------------------------------- unit tests (audit fixes)
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn min_max_over_list() {
        // the Job Card bug: min([3,1,2]) must be 1, not the whole list
        assert_eq!(py_min_iter(&json!([3, 1, 2])), json!(1));
        assert_eq!(py_max_iter(&json!([3, 1, 2])), json!(3));
        assert_eq!(py_min_iter(&json!(["2024-03-01", "2024-01-05", "2024-02-09"])), json!("2024-01-05"));
    }

    #[test]
    fn python_negative_modulo() {
        // result takes the sign of the divisor (Python semantics), not rem_euclid
        assert_eq!(modulo(&json!(7), &json!(-3)), json!(-2));
        assert_eq!(modulo(&json!(-7), &json!(3)), json!(2));
        assert_eq!(modulo(&json!(10), &json!(3)), json!(1));
    }

    #[test]
    fn format_specs() {
        assert_eq!(fmt_value(&json!(2.5), ".0f"), json!("2")); // f64 round-half-even at .0f via Rust fmt
        assert_eq!(fmt_value(&json!(3.0), ".2f"), json!("3.00"));
        assert_eq!(fmt_value(&json!(0.1234), ".1%"), json!("12.3%"));
        assert_eq!(fmt_value(&json!(1234567.5), ".2f,"), json!("1,234,567.50"));
        // str.format honours the spec now
        assert_eq!(str_format(&s("amt {0:.2f}"), &[json!(3.0)], &[]), json!("amt 3.00"));
    }

    #[test]
    fn child_append_to_local() {
        // hd_team bug: ar.append("table", {row}) must push the row dict, not the table-name string
        let mut d = json!({});
        append_to(&mut d, &s("weighted_users"), json!({"user": "x", "weight": 2}));
        assert_eq!(d, json!({"weighted_users": [{"user": "x", "weight": 2}]}));
    }

    #[test]
    fn dynamic_value_semantics() {
        assert_eq!(flt(&Value::Null, None), json!(0.0)); // flt(None) == 0.0
        assert!(!truthy(&json!(0)) && !truthy(&json!("")) && !truthy(&json!([])));
        assert!(truthy(&json!("x")) && truthy(&json!(1)));
        assert_eq!(add(&s("a"), &json!(1)), json!("a1")); // str + -> concat
        assert_eq!(div(&json!(5), &json!(2)), json!(2.5)); // Python / is float
    }
}
