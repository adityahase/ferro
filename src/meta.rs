//! DocType metadata: loaded on demand from tabDocType / tabDocField, then cached.
//!
//! This is the Rust analogue of Frappe's `frappe.model.meta` — the subsystem whose
//! first access cost the CPython worker ~49 MB. Here each Meta is a compact struct and
//! the cache is bounded (LRU-ish by insertion order), so the resident cost stays flat.

use rusqlite::Connection;
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex};

/// Frappe `default_fields` (after stripping "doctype"): present on EVERY doc table.
pub const DEFAULT_FIELDS: &[&str] =
    &["name", "owner", "creation", "modified", "modified_by", "docstatus", "idx"];
/// Frappe `optional_fields`: the metadata columns, present when the table was created with them.
pub const OPTIONAL_FIELDS: &[&str] = &["_user_tags", "_comments", "_assign", "_liked_by", "_seen"];
/// Frappe `child_table_fields`: ONLY exist on child tables (istable=1). Selecting these from a
/// non-child table is the parent-key corruption bug (FIX-8) — SQLite's double-quoted-identifier
/// fallback returns the literal column name as a string. So they are synthesized only for singles
/// (PRAGMA is authoritative for real tables and simply won't list them on non-child tables).
pub const CHILD_TABLE_FIELDS: &[&str] = &["parent", "parentfield", "parenttype"];

#[derive(Clone)]
pub struct DocField {
    pub fieldname: String,
    pub fieldtype: String,
    pub options: Option<String>,
    pub reqd: bool,
    pub default: Option<String>,
    pub permlevel: i64,
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
    #[allow(dead_code)]
    pub naming_rule: Option<String>,
    #[allow(dead_code)]
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
    let (issingle, istable, is_virtual, mut autoname, mut naming_rule, mut title_field, mut sort_field, mut sort_order) =
        row;

    let table = format!("tab{doctype}");

    // 2. DocFields, ordered by idx (matches Frappe meta field order).
    let mut fields = Vec::new();
    {
        let mut stmt = con.prepare(
            "SELECT fieldname, COALESCE(fieldtype,'Data'), options, \
             COALESCE(reqd,0), \"default\", COALESCE(permlevel,0) \
             FROM \"tabDocField\" WHERE parent = ?1 ORDER BY idx",
        )?;
        let rows = stmt.query_map([doctype], |r| {
            Ok(DocField {
                fieldname: r.get::<_, Option<String>>(0)?.unwrap_or_default(),
                fieldtype: r.get::<_, String>(1)?,
                options: r.get::<_, Option<String>>(2)?,
                reqd: r.get::<_, Option<i64>>(3)?.unwrap_or(0) != 0,
                default: r.get::<_, Option<String>>(4)?,
                permlevel: r.get::<_, Option<i64>>(5)?.unwrap_or(0),
            })
        })?;
        for f in rows {
            let f = f?;
            if !f.fieldname.is_empty() {
                fields.push(f);
            }
        }
    }

    // 2b. Merge Custom Fields (per-site customizations) into the field list, ordered by idx —
    //     Frappe's Meta.add_custom_fields. Without this the ORM can't see a custom field's
    //     permlevel / Link options / reqd. The physical column already exists (adding a Custom
    //     Field runs a schema migration), so this only adds the field METADATA.
    if let Ok(mut stmt) = con.prepare(
        "SELECT fieldname, COALESCE(fieldtype,'Data'), options, \
         COALESCE(reqd,0), \"default\", COALESCE(permlevel,0) \
         FROM \"tabCustom Field\" WHERE dt = ?1 ORDER BY idx",
    ) {
        if let Ok(rows) = stmt.query_map([doctype], |r| {
            Ok(DocField {
                fieldname: r.get::<_, Option<String>>(0)?.unwrap_or_default(),
                fieldtype: r.get::<_, String>(1)?,
                options: r.get::<_, Option<String>>(2)?,
                reqd: r.get::<_, Option<i64>>(3)?.unwrap_or(0) != 0,
                default: r.get::<_, Option<String>>(4)?,
                permlevel: r.get::<_, Option<i64>>(5)?.unwrap_or(0),
            })
        }) {
            for f in rows.flatten() {
                if !f.fieldname.is_empty() && !fields.iter().any(|e| e.fieldname == f.fieldname) {
                    fields.push(f);
                }
            }
        }
    }

    // 2c. Apply Property Setters (per-site overrides) — Frappe's Meta.apply_property_setters.
    //     Only the properties ferro's data plane consults are honored: doctype-level naming/sort,
    //     and field-level fieldtype/options/reqd/default/permlevel.
    apply_property_setters(
        con, doctype, &mut fields,
        &mut autoname, &mut naming_rule, &mut title_field, &mut sort_field, &mut sort_order,
    );

    // 3. Physical columns. For a real table PRAGMA is the AUTHORITATIVE set (standard + docfield
    //    + custom columns that actually exist) — crucially it lists parent/parentfield/parenttype
    //    only on child tables, so non-child get_doc never selects those phantom columns (FIX-8).
    //    Singles/virtual have no table, so synthesize the valid-column set from meta instead.
    let mut columns: HashSet<String> = HashSet::new();
    if !issingle && !is_virtual {
        if let Ok(mut stmt) = con.prepare(&format!("PRAGMA table_info({})", quote_ident(&table))) {
            if let Ok(rows) = stmt.query_map([], |r| r.get::<_, String>(1)) {
                for c in rows.flatten() {
                    columns.insert(c);
                }
            }
        }
    } else {
        columns.extend(DEFAULT_FIELDS.iter().map(|s| s.to_string()));
        columns.extend(OPTIONAL_FIELDS.iter().map(|s| s.to_string()));
        if istable {
            columns.extend(CHILD_TABLE_FIELDS.iter().map(|s| s.to_string()));
        }
        for f in &fields {
            if !f.is_virtual_column() {
                columns.insert(f.fieldname.clone());
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

/// Apply `tabProperty Setter` overrides for the data-plane properties ferro consults.
/// Mirrors `Meta.apply_property_setters`: a row with `doctype_or_field='DocType'` overrides a
/// doctype property; `='DocField'` overrides the named field's property. Values are strings in the
/// DB; ferro casts the few it uses (reqd/permlevel are int-ish, the rest are strings).
#[allow(clippy::too_many_arguments)]
fn apply_property_setters(
    con: &Connection,
    doctype: &str,
    fields: &mut [DocField],
    autoname: &mut Option<String>,
    naming_rule: &mut Option<String>,
    title_field: &mut Option<String>,
    sort_field: &mut String,
    sort_order: &mut String,
) {
    let mut stmt = match con.prepare(
        "SELECT COALESCE(doctype_or_field,''), field_name, COALESCE(property,''), value \
         FROM \"tabProperty Setter\" WHERE doc_type = ?1",
    ) {
        Ok(s) => s,
        Err(_) => return, // table absent on a minimal site — nothing to apply
    };
    let rows = match stmt.query_map([doctype], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, Option<String>>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, Option<String>>(3)?,
        ))
    }) {
        Ok(r) => r,
        Err(_) => return,
    };
    for row in rows.flatten() {
        let (dof, field_name, property, value) = row;
        match dof.as_str() {
            "DocType" => match property.as_str() {
                "autoname" => *autoname = value,
                "naming_rule" => *naming_rule = value,
                "title_field" => *title_field = value,
                "sort_field" => {
                    if let Some(v) = value {
                        *sort_field = v;
                    }
                }
                "sort_order" => {
                    if let Some(v) = value {
                        *sort_order = v;
                    }
                }
                _ => {}
            },
            "DocField" => {
                let fname = match &field_name {
                    Some(f) if !f.is_empty() => f,
                    _ => continue,
                };
                if let Some(df) = fields.iter_mut().find(|d| &d.fieldname == fname) {
                    match property.as_str() {
                        "fieldtype" => {
                            if let Some(v) = value {
                                df.fieldtype = v;
                            }
                        }
                        "options" => df.options = value,
                        "default" => df.default = value,
                        "reqd" => df.reqd = value.as_deref().map(|v| crate::util::cint(v) != 0).unwrap_or(false),
                        "permlevel" => df.permlevel = value.as_deref().map(crate::util::cint).unwrap_or(0),
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
}

/// Thread-safe, bounded LRU metadata cache. Entries are `Arc<Meta>` so callers clone the
/// handle out and drop the lock immediately. A single mutex guards both the map and the
/// recency order so they can never disagree; bounded size keeps resident meta flat under churn.
struct Inner {
    map: HashMap<String, Arc<Meta>>,
    order: VecDeque<String>, // front = least-recently-used, back = most-recently-used
}

pub struct MetaCache {
    inner: Mutex<Inner>,
    cap: usize,
}

impl MetaCache {
    pub fn new(cap: usize) -> Self {
        MetaCache {
            inner: Mutex::new(Inner {
                map: HashMap::new(),
                order: VecDeque::new(),
            }),
            cap: cap.max(1),
        }
    }

    pub fn get(&self, con: &Connection, doctype: &str) -> Result<Arc<Meta>, MetaError> {
        // Fast path: hit — clone the Arc and bump recency.
        {
            let mut inner = self.inner.lock().unwrap();
            if let Some(m) = inner.map.get(doctype).cloned() {
                touch(&mut inner.order, doctype);
                return Ok(m);
            }
        }
        // Slow path: load OUTSIDE the lock (DB I/O), then insert.
        let meta = Arc::new(load_meta(con, doctype)?);
        let mut inner = self.inner.lock().unwrap();
        if let Some(existing) = inner.map.get(doctype).cloned() {
            // another thread loaded it meanwhile
            touch(&mut inner.order, doctype);
            return Ok(existing);
        }
        while inner.order.len() >= self.cap {
            if let Some(old) = inner.order.pop_front() {
                inner.map.remove(&old);
            } else {
                break;
            }
        }
        inner.map.insert(doctype.to_string(), meta.clone());
        inner.order.push_back(doctype.to_string());
        Ok(meta)
    }

    #[allow(dead_code)]
    pub fn len(&self) -> usize {
        self.inner.lock().unwrap().map.len()
    }
}

/// Move `key` to the most-recently-used position.
fn touch(order: &mut VecDeque<String>, key: &str) {
    if let Some(pos) = order.iter().position(|k| k == key) {
        order.remove(pos);
    }
    order.push_back(key.to_string());
}
