//! DocType metadata: loaded on demand from tabDocType / tabDocField, then cached.
//!
//! This is the Rust analogue of Frappe's `frappe.model.meta` — the subsystem whose
//! first access cost the CPython worker ~49 MB. Here each Meta is a compact struct and
//! the cache is bounded (LRU-ish by insertion order), so the resident cost stays flat.

use rusqlite::Connection;
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex};

/// Columns Frappe guarantees on every doc table.
pub const STANDARD_COLUMNS: &[&str] = &[
    "name", "creation", "modified", "modified_by", "owner", "docstatus", "idx",
    "parent", "parentfield", "parenttype", "_user_tags", "_comments", "_assign", "_liked_by",
];

#[derive(Clone)]
pub struct DocField {
    pub fieldname: String,
    pub fieldtype: String,
    pub options: Option<String>,
    pub reqd: bool,
    pub default: Option<String>,
}

impl DocField {
    pub fn is_child_table(&self) -> bool {
        self.fieldtype == "Table" || self.fieldtype == "Table MultiSelect"
    }
    /// Fieldtypes with no column in the parent table.
    pub fn is_virtual_column(&self) -> bool {
        matches!(
            self.fieldtype.as_str(),
            "Table" | "Table MultiSelect" | "Section Break" | "Column Break" | "Tab Break"
                | "HTML" | "Heading" | "Button" | "Fold"
        )
    }
}

pub struct Meta {
    pub name: String,
    pub table: String, // "tab<DocType>"
    pub issingle: bool,
    pub istable: bool,
    pub is_virtual: bool,
    pub autoname: Option<String>,
    pub naming_rule: Option<String>,
    pub title_field: Option<String>,
    pub sort_field: String,
    pub sort_order: String,
    pub fields: Vec<DocField>,
    /// Physical columns present in the table (standard + docfield + custom). Used to
    /// validate every fieldname touched by a query — the SQL-injection guard.
    pub columns: HashSet<String>,
}

impl Meta {
    pub fn has_column(&self, c: &str) -> bool {
        self.columns.contains(c)
    }
    pub fn field(&self, name: &str) -> Option<&DocField> {
        self.fields.iter().find(|f| f.fieldname == name)
    }
    pub fn child_tables(&self) -> impl Iterator<Item = &DocField> {
        self.fields.iter().filter(|f| f.is_child_table())
    }
}

/// Quote a SQL identifier for SQLite (double-quote, escape embedded quotes).
pub fn quote_ident(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for ch in s.chars() {
        if ch == '"' {
            out.push('"');
        }
        out.push(ch);
    }
    out.push('"');
    out
}

#[derive(Debug)]
pub enum MetaError {
    NotFound(String),
    Db(rusqlite::Error),
}
impl From<rusqlite::Error> for MetaError {
    fn from(e: rusqlite::Error) -> Self {
        MetaError::Db(e)
    }
}

/// Load a DocType's metadata directly from the site DB.
fn load_meta(con: &Connection, doctype: &str) -> Result<Meta, MetaError> {
    // 1. DocType row.
    let row = con
        .query_row(
            "SELECT issingle, istable, is_virtual, autoname, naming_rule, title_field, \
             COALESCE(sort_field,'modified'), COALESCE(sort_order,'DESC') \
             FROM \"tabDocType\" WHERE name = ?1",
            [doctype],
            |r| {
                Ok((
                    r.get::<_, Option<i64>>(0)?.unwrap_or(0) != 0,
                    r.get::<_, Option<i64>>(1)?.unwrap_or(0) != 0,
                    r.get::<_, Option<i64>>(2)?.unwrap_or(0) != 0,
                    r.get::<_, Option<String>>(3)?,
                    r.get::<_, Option<String>>(4)?,
                    r.get::<_, Option<String>>(5)?,
                    r.get::<_, String>(6)?,
                    r.get::<_, String>(7)?,
                ))
            },
        )
        .map_err(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => MetaError::NotFound(doctype.to_string()),
            other => MetaError::Db(other),
        })?;
    let (issingle, istable, is_virtual, autoname, naming_rule, title_field, sort_field, sort_order) =
        row;

    let table = format!("tab{doctype}");

    // 2. DocFields, ordered by idx (matches Frappe meta field order).
    let mut fields = Vec::new();
    {
        let mut stmt = con.prepare(
            "SELECT fieldname, COALESCE(fieldtype,'Data'), options, \
             COALESCE(reqd,0), \"default\" \
             FROM \"tabDocField\" WHERE parent = ?1 ORDER BY idx",
        )?;
        let rows = stmt.query_map([doctype], |r| {
            Ok(DocField {
                fieldname: r.get::<_, Option<String>>(0)?.unwrap_or_default(),
                fieldtype: r.get::<_, String>(1)?,
                options: r.get::<_, Option<String>>(2)?,
                reqd: r.get::<_, Option<i64>>(3)?.unwrap_or(0) != 0,
                default: r.get::<_, Option<String>>(4)?,
            })
        })?;
        for f in rows {
            let f = f?;
            if !f.fieldname.is_empty() {
                fields.push(f);
            }
        }
    }

    // 3. Physical columns. For non-single tables PRAGMA gives the authoritative set
    //    (includes custom fields). Singles/virtual have no table, so synthesize from meta.
    let mut columns: HashSet<String> = STANDARD_COLUMNS.iter().map(|s| s.to_string()).collect();
    for f in &fields {
        if !f.is_virtual_column() {
            columns.insert(f.fieldname.clone());
        }
    }
    if !issingle && !is_virtual {
        if let Ok(mut stmt) = con.prepare(&format!("PRAGMA table_info({})", quote_ident(&table))) {
            if let Ok(rows) = stmt.query_map([], |r| r.get::<_, String>(1)) {
                for c in rows.flatten() {
                    columns.insert(c);
                }
            }
        }
    }

    Ok(Meta {
        name: doctype.to_string(),
        table,
        issingle,
        istable,
        is_virtual,
        autoname,
        naming_rule,
        title_field,
        sort_field,
        sort_order,
        fields,
        columns,
    })
}

/// Thread-safe, bounded metadata cache. Entries are `Arc<Meta>` so callers clone the
/// handle out and drop the lock immediately.
pub struct MetaCache {
    map: Mutex<HashMap<String, Arc<Meta>>>,
    order: Mutex<VecDeque<String>>,
    cap: usize,
}

impl MetaCache {
    pub fn new(cap: usize) -> Self {
        MetaCache {
            map: Mutex::new(HashMap::new()),
            order: Mutex::new(VecDeque::new()),
            cap,
        }
    }

    pub fn get(&self, con: &Connection, doctype: &str) -> Result<Arc<Meta>, MetaError> {
        if let Some(m) = self.map.lock().unwrap().get(doctype) {
            return Ok(m.clone());
        }
        let meta = Arc::new(load_meta(con, doctype)?);
        let mut map = self.map.lock().unwrap();
        let mut order = self.order.lock().unwrap();
        if !map.contains_key(doctype) {
            // bounded eviction (FIFO) keeps resident meta flat under churn
            while order.len() >= self.cap {
                if let Some(old) = order.pop_front() {
                    map.remove(&old);
                } else {
                    break;
                }
            }
            map.insert(doctype.to_string(), meta.clone());
            order.push_back(doctype.to_string());
        }
        Ok(meta)
    }

    pub fn len(&self) -> usize {
        self.map.lock().unwrap().len()
    }
}
