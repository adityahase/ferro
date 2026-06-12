//! `ferro_rt` — the native PyO3 module the Python `frappe` shim calls into.
//!
//! It exposes ferro's Rust data plane (orm.rs / meta.rs) to the embedded interpreter, so a
//! real Frappe controller's `frappe.get_doc(...)`, `frappe.db.get_value(...)`, `self.insert()`,
//! etc. execute against the same SQLite site — with NO Python framework underneath.
//!
//! Connection routing: while ferro (Rust) drives a hooked write it stashes a pointer to the
//! worker thread's `Connection` in a thread-local; re-entrant `ferro_rt` calls reuse it, so the
//! controller's reads/writes share the request's connection (and see its own writes). Outside a
//! request (e.g. the boot-time loader) it falls back to a lazily-opened per-thread connection.

use crate::meta::{Meta, MetaCache};
use crate::orm::{self, ListQuery, OrmError, ReadAcl};
use pyo3::exceptions::{PyKeyError, PyRuntimeError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList};
use rusqlite::Connection;
use serde_json::{Map, Value};
use std::cell::{Cell, RefCell};
use std::ptr;
use std::sync::{Arc, OnceLock};

/// Process-global state the module needs to open connections / cache metadata.
pub struct PyState {
    pub db_path: String,
    pub metas: Arc<MetaCache>,
}
static STATE: OnceLock<PyState> = OnceLock::new();

pub fn init_state(db_path: String, metas: Arc<MetaCache>) {
    let _ = STATE.set(PyState { db_path, metas });
}
fn state() -> &'static PyState {
    STATE.get().expect("ferro_rt state not initialised")
}

thread_local! {
    /// Borrowed connection for the in-flight request (set by the Rust write driver).
    static CUR_CON: Cell<*const Connection> = const { Cell::new(ptr::null()) };
    /// Owned per-thread fallback connection (for Python-initiated work outside a request).
    static OWN_CON: RefCell<Option<Connection>> = const { RefCell::new(None) };
    /// Message buffer (frappe.msgprint / _server_messages), drained per request.
    static MSGS: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
    /// Current user for ORM writes initiated from Python.
    static CUR_USER: RefCell<String> = RefCell::new(String::from("Administrator"));
}

/// Borrow the current connection (request-scoped pointer if set, else the per-thread owned one).
fn with_con<R>(f: impl FnOnce(&Connection) -> R) -> R {
    let p = CUR_CON.with(|c| c.get());
    if !p.is_null() {
        // SAFETY: pointer set by `set_request_con` for the duration of a single GIL-held call on
        // this same thread; cleared before the borrow ends. No aliasing: Python runs serialized.
        return unsafe { f(&*p) };
    }
    OWN_CON.with(|cell| {
        let mut b = cell.borrow_mut();
        if b.is_none() {
            let con = Connection::open(&state().db_path).expect("open sqlite (py thread)");
            con.busy_timeout(std::time::Duration::from_secs(5)).ok();
            con.pragma_update(None, "foreign_keys", "OFF").ok();
            con.pragma_update(None, "cache_size", -2048).ok();
            *b = Some(con);
        }
        f(b.as_ref().unwrap())
    })
}

/// RAII-ish hooks the Rust driver uses around a Python call.
pub fn set_request_con(con: &Connection) {
    CUR_CON.with(|c| c.set(con as *const Connection));
}
pub fn clear_request_con() {
    CUR_CON.with(|c| c.set(ptr::null()));
}
pub fn set_user(user: &str) {
    CUR_USER.with(|u| *u.borrow_mut() = user.to_string());
}
pub fn take_messages() -> Vec<String> {
    MSGS.with(|m| std::mem::take(&mut *m.borrow_mut()))
}

fn cur_user() -> String {
    CUR_USER.with(|u| u.borrow().clone())
}

fn meta_of(con: &Connection, doctype: &str) -> Result<Arc<Meta>, PyErr> {
    state()
        .metas
        .get(con, doctype)
        .map_err(|e| match e {
            crate::meta::MetaError::NotFound(d) => PyKeyError::new_err(format!("DocType {d} not found")),
            crate::meta::MetaError::Db(e) => PyRuntimeError::new_err(e.to_string()),
        })
}

fn orm_err(e: OrmError) -> PyErr {
    match e {
        OrmError::NotFound(m) => PyKeyError::new_err(m),
        OrmError::Validation(m) => PyValueError::new_err(m),
        OrmError::Duplicate(m) => PyValueError::new_err(m),
        OrmError::Db(e) => PyRuntimeError::new_err(e.to_string()),
    }
}

// ---------- JSON <-> Python ----------

fn json_to_py(py: Python<'_>, v: &Value) -> PyObject {
    match v {
        Value::Null => py.None(),
        Value::Bool(b) => b.into_pyobject(py).unwrap().to_owned().into_any().unbind(),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                i.into_pyobject(py).unwrap().into_any().unbind()
            } else {
                n.as_f64().unwrap_or(0.0).into_pyobject(py).unwrap().into_any().unbind()
            }
        }
        Value::String(s) => s.into_pyobject(py).unwrap().into_any().unbind(),
        Value::Array(a) => {
            let list = PyList::empty(py);
            for item in a {
                list.append(json_to_py(py, item)).unwrap();
            }
            list.into_any().unbind()
        }
        Value::Object(o) => {
            let d = PyDict::new(py);
            for (k, val) in o {
                d.set_item(k, json_to_py(py, val)).unwrap();
            }
            d.into_any().unbind()
        }
    }
}

fn parse_json(s: &str) -> Result<Value, PyErr> {
    serde_json::from_str(s).map_err(|e| PyValueError::new_err(format!("bad json: {e}")))
}

// ---------- exposed functions ----------

#[pyfunction]
fn get_doc(py: Python<'_>, doctype: &str, name: &str) -> PyResult<PyObject> {
    with_con(|con| {
        let meta = meta_of(con, doctype)?;
        let v = orm::get_doc(con, &meta, &ReadAcl::all(), name).map_err(orm_err)?;
        Ok(json_to_py(py, &v))
    })
}

#[pyfunction]
#[pyo3(signature = (doctype, filters_json=None, fields_json=None, order_by=None, limit_start=0, limit_page_length=0))]
fn get_list(
    py: Python<'_>,
    doctype: &str,
    filters_json: Option<&str>,
    fields_json: Option<&str>,
    order_by: Option<String>,
    limit_start: i64,
    limit_page_length: i64,
) -> PyResult<PyObject> {
    with_con(|con| {
        let meta = meta_of(con, doctype)?;
        let mut q = ListQuery {
            limit_start,
            limit_page_length,
            order_by,
            ..Default::default()
        };
        if let Some(f) = fields_json {
            if let Value::Array(a) = parse_json(f)? {
                let v: Vec<String> = a.iter().filter_map(|x| x.as_str().map(|s| s.to_string())).collect();
                if !v.is_empty() {
                    q.fields = v;
                }
            }
        }
        if let Some(f) = filters_json {
            q.filters = parse_json(f)?;
        }
        let v = orm::get_list(con, &meta, &ReadAcl::all(), &q, None).map_err(orm_err)?;
        Ok(json_to_py(py, &v))
    })
}

#[pyfunction]
fn get_value(py: Python<'_>, doctype: &str, name: &str, fieldname: &str) -> PyResult<PyObject> {
    with_con(|con| {
        let meta = meta_of(con, doctype)?;
        match orm::get_doc(con, &meta, &ReadAcl::all(), name) {
            Ok(Value::Object(o)) => Ok(json_to_py(py, o.get(fieldname).unwrap_or(&Value::Null))),
            Ok(_) => Ok(py.None()),
            Err(OrmError::NotFound(_)) => Ok(py.None()),
            Err(e) => Err(orm_err(e)),
        }
    })
}

#[pyfunction]
fn exists(doctype: &str, name: &str) -> PyResult<bool> {
    with_con(|con| {
        let meta = match meta_of(con, doctype) {
            Ok(m) => m,
            Err(_) => return Ok(false),
        };
        match orm::get_doc(con, &meta, &ReadAcl::all(), name) {
            Ok(_) => Ok(true),
            Err(OrmError::NotFound(_)) => Ok(false),
            Err(e) => Err(orm_err(e)),
        }
    })
}

#[pyfunction]
#[pyo3(signature = (doctype, filters_json=None))]
fn count(doctype: &str, filters_json: Option<&str>) -> PyResult<i64> {
    with_con(|con| {
        let meta = meta_of(con, doctype)?;
        let filters = match filters_json {
            Some(f) => parse_json(f)?,
            None => Value::Null,
        };
        orm::count(con, &meta, &filters, None).map_err(orm_err)
    })
}

#[pyfunction]
#[pyo3(signature = (doctype, data_json, ignore_mandatory=false, ignore_links=false))]
fn insert(
    py: Python<'_>,
    doctype: &str,
    data_json: &str,
    ignore_mandatory: bool,
    ignore_links: bool,
) -> PyResult<PyObject> {
    with_con(|con| {
        let meta = meta_of(con, doctype)?;
        let data = match parse_json(data_json)? {
            Value::Object(m) => m,
            _ => return Err(PyValueError::new_err("insert data must be an object")),
        };
        let v = orm::with_insert_flags(ignore_links, ignore_mandatory, || {
            orm::insert(con, &meta, &ReadAcl::all(), &data, &cur_user())
        })
        .map_err(orm_err)?;
        Ok(json_to_py(py, &v))
    })
}

/// Raw insert — Frappe's `db_insert`: a low-level DB write with NO Document validation (neither
/// link existence nor required fields), used for self-referential bootstrap fixtures.
#[pyfunction]
fn raw_insert(py: Python<'_>, doctype: &str, data_json: &str) -> PyResult<PyObject> {
    with_con(|con| {
        let meta = meta_of(con, doctype)?;
        let data = match parse_json(data_json)? {
            Value::Object(m) => m,
            _ => return Err(PyValueError::new_err("insert data must be an object")),
        };
        let v = orm::with_insert_flags(true, true, || {
            orm::insert(con, &meta, &ReadAcl::all(), &data, &cur_user())
        })
        .map_err(orm_err)?;
        Ok(json_to_py(py, &v))
    })
}

#[pyfunction]
fn update(py: Python<'_>, doctype: &str, name: &str, data_json: &str) -> PyResult<PyObject> {
    with_con(|con| {
        let meta = meta_of(con, doctype)?;
        let data = match parse_json(data_json)? {
            Value::Object(m) => m,
            _ => return Err(PyValueError::new_err("update data must be an object")),
        };
        let v = orm::update(con, &meta, &ReadAcl::all(), name, &data, &cur_user()).map_err(orm_err)?;
        Ok(json_to_py(py, &v))
    })
}

#[pyfunction]
fn set_value(py: Python<'_>, doctype: &str, name: &str, fieldname: &str, value_json: &str) -> PyResult<PyObject> {
    with_con(|con| {
        let meta = meta_of(con, doctype)?;
        let mut m = Map::new();
        m.insert(fieldname.to_string(), parse_json(value_json)?);
        let v = orm::update(con, &meta, &ReadAcl::all(), name, &m, &cur_user()).map_err(orm_err)?;
        Ok(json_to_py(py, &v))
    })
}

#[pyfunction]
fn delete(doctype: &str, name: &str) -> PyResult<()> {
    with_con(|con| {
        let meta = meta_of(con, doctype)?;
        orm::delete(con, &meta, name).map_err(orm_err)
    })
}

/// Raw SQL passthrough (frappe.db.sql). Returns a list of rows; each row is a list when
/// `as_dict` is false, else a dict keyed by column name.
#[pyfunction]
#[pyo3(signature = (query, params_json=None, as_dict=false))]
fn sql(py: Python<'_>, query: &str, params_json: Option<&str>, as_dict: bool) -> PyResult<PyObject> {
    use rusqlite::types::ValueRef;
    with_con(|con| {
        let params: Vec<Value> = match params_json {
            Some(p) => match parse_json(p)? {
                Value::Array(a) => a,
                Value::Null => vec![],
                other => vec![other],
            },
            None => vec![],
        };
        let binds: Vec<rusqlite::types::Value> = params
            .iter()
            .map(|v| match v {
                Value::Null => rusqlite::types::Value::Null,
                Value::Bool(b) => rusqlite::types::Value::Integer(*b as i64),
                Value::Number(n) => n
                    .as_i64()
                    .map(rusqlite::types::Value::Integer)
                    .unwrap_or_else(|| rusqlite::types::Value::Real(n.as_f64().unwrap_or(0.0))),
                Value::String(s) => rusqlite::types::Value::Text(s.clone()),
                other => rusqlite::types::Value::Text(other.to_string()),
            })
            .collect();
        let mut stmt = con.prepare(query).map_err(|e| PyValueError::new_err(e.to_string()))?;
        let ncol = stmt.column_count();
        let col_names: Vec<String> = stmt.column_names().iter().map(|s| s.to_string()).collect();
        let out = PyList::empty(py);
        let mut rows = stmt
            .query(rusqlite::params_from_iter(binds.iter()))
            .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
        while let Some(row) = rows.next().map_err(|e| PyRuntimeError::new_err(e.to_string()))? {
            let cell = |i: usize| -> Value {
                match row.get_ref(i).unwrap_or(ValueRef::Null) {
                    ValueRef::Null => Value::Null,
                    ValueRef::Integer(n) => Value::from(n),
                    ValueRef::Real(f) => Value::from(f),
                    ValueRef::Text(t) => Value::from(String::from_utf8_lossy(t).into_owned()),
                    ValueRef::Blob(b) => Value::from(crate::util::b64(b)),
                }
            };
            if as_dict {
                let d = PyDict::new(py);
                for i in 0..ncol {
                    d.set_item(&col_names[i], json_to_py(py, &cell(i))).unwrap();
                }
                out.append(d).unwrap();
            } else {
                let l = PyList::empty(py);
                for i in 0..ncol {
                    l.append(json_to_py(py, &cell(i))).unwrap();
                }
                out.append(l).unwrap();
            }
        }
        Ok(out.into_any().unbind())
    })
}

/// Faithful-enough DocType meta for the Document proxy: fields + flags.
#[pyfunction]
fn get_meta(py: Python<'_>, doctype: &str) -> PyResult<PyObject> {
    with_con(|con| {
        let meta = meta_of(con, doctype)?;
        let d = PyDict::new(py);
        d.set_item("name", &meta.name)?;
        d.set_item("issingle", meta.issingle)?;
        d.set_item("istable", meta.istable)?;
        d.set_item("is_virtual", meta.is_virtual)?;
        d.set_item("autoname", meta.autoname.clone())?;
        let fields = PyList::empty(py);
        for f in &meta.fields {
            let fd = PyDict::new(py);
            fd.set_item("fieldname", &f.fieldname)?;
            fd.set_item("fieldtype", &f.fieldtype)?;
            fd.set_item("options", f.options.clone())?;
            fd.set_item("reqd", f.reqd)?;
            fd.set_item("default", f.default.clone())?;
            fd.set_item("permlevel", f.permlevel)?;
            fields.append(fd)?;
        }
        d.set_item("fields", fields)?;
        Ok(d.into_any().unbind())
    })
}

#[pyfunction]
fn msgprint(msg: &str) {
    MSGS.with(|m| m.borrow_mut().push(msg.to_string()));
}

#[pyfunction]
fn whoami() -> String {
    cur_user()
}

#[pyfunction]
fn now_datetime() -> String {
    crate::util::now_datetime()
}

#[pyfunction]
fn generate_hash() -> String {
    format!("{}{}", crate::util::random_name(), crate::util::random_name())
}

/// The module registered as `ferro_rt` in the embedded interpreter.
#[pymodule]
pub fn ferro_rt(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(get_doc, m)?)?;
    m.add_function(wrap_pyfunction!(get_list, m)?)?;
    m.add_function(wrap_pyfunction!(get_value, m)?)?;
    m.add_function(wrap_pyfunction!(exists, m)?)?;
    m.add_function(wrap_pyfunction!(count, m)?)?;
    m.add_function(wrap_pyfunction!(insert, m)?)?;
    m.add_function(wrap_pyfunction!(raw_insert, m)?)?;
    m.add_function(wrap_pyfunction!(update, m)?)?;
    m.add_function(wrap_pyfunction!(set_value, m)?)?;
    m.add_function(wrap_pyfunction!(delete, m)?)?;
    m.add_function(wrap_pyfunction!(sql, m)?)?;
    m.add_function(wrap_pyfunction!(get_meta, m)?)?;
    m.add_function(wrap_pyfunction!(msgprint, m)?)?;
    m.add_function(wrap_pyfunction!(whoami, m)?)?;
    m.add_function(wrap_pyfunction!(now_datetime, m)?)?;
    m.add_function(wrap_pyfunction!(generate_hash, m)?)?;
    Ok(())
}
