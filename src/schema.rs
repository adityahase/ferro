//! The schema-sync DDL engine: doctype JSON ⇄ SQLite tables.
//!
//! A faithful Rust port of Frappe's `database/schema.py` + `database/sqlite/schema.py` +
//! `database/sqlite/database.py` (the type_map). It is the shared substrate of `reload-doctype`,
//! `reload-doc`, `install-app` and `migrate`:
//!   - `import_doctype` writes a DocType JSON into tabDocType + its child tables (DocField/DocPerm/…)
//!   - `sync_table` brings the physical `tab<DocType>` table in line with that meta (CREATE / ALTER)
//!   - `reload_doctype` ties them together for one doctype found on disk.
//!
//! Pure Rust, no controller Python. For a pure-CRUD app this is the entire install/migrate schema
//! step; apps with controllers/hooks/patches need ferrod for the Python tail (handled elsewhere).

use crate::meta::quote_ident;
use crate::util::{cint, flt, now_datetime};
use rusqlite::{params, Connection};
use serde_json::{Map, Value};
use std::path::{Path, PathBuf};

const DEFAULT_SHORTCUTS: &[&str] = &["_Login", "__user", "_Full Name", "Today", "__today", "now", "Now"];
const OPTIONAL_COLUMNS: &[&str] = &["_user_tags", "_comments", "_assign", "_liked_by"];
const NOT_NULL_TYPES: &[&str] = &["Check", "Int", "Currency", "Float", "Percent"];

/// Fieldtypes that carry no value → no column (mirrors model.no_value_fields).
fn no_value_field(ft: &str) -> bool {
    matches!(
        ft,
        "Section Break" | "Column Break" | "Tab Break" | "HTML" | "Table" | "Table MultiSelect"
            | "Button" | "Image" | "Fold" | "Heading"
    )
}

/// SQLite base column type for a fieldtype (port of sqlite/database.py type_map). `None` = the
/// fieldtype is not in the type_map (no column), which also covers all no_value fieldtypes.
fn sqlite_base_type(ft: &str) -> Option<&'static str> {
    Some(match ft {
        "Currency" | "Float" | "Percent" | "Rating" | "Duration" => "REAL",
        "Int" | "Long Int" | "Check" => "INTEGER",
        "Date" => "DATE",
        "Datetime" => "TIMESTAMP",
        "Time" => "TIME",
        "Small Text" | "Long Text" | "Code" | "Text Editor" | "Markdown Editor" | "HTML Editor"
        | "Text" | "Data" | "Link" | "Dynamic Link" | "Password" | "Select" | "Read Only"
        | "Attach" | "Attach Image" | "Signature" | "Color" | "Barcode" | "Geolocation" | "Icon"
        | "Phone" | "Autocomplete" | "JSON" => "TEXT",
        _ => return None,
    })
}

/// A docfield as it matters to the DDL engine.
struct SField {
    fieldname: String,
    fieldtype: String,
    options: Option<String>,
    default: Option<String>,
    search_index: bool,
    unique: bool,
    not_nullable: bool,
}

/// Top-level DocType attributes that shape the table.
struct DocTypeInfo {
    issingle: bool,
    istable: bool,
    is_virtual: bool,
    autoname: Option<String>,
    sort_field: String,
    track_seen: bool,
}

#[derive(Debug)]
pub enum SchemaError {
    Db(rusqlite::Error),
    NotFound(String),
    Io(String),
}
impl From<rusqlite::Error> for SchemaError {
    fn from(e: rusqlite::Error) -> Self {
        SchemaError::Db(e)
    }
}
impl std::fmt::Display for SchemaError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SchemaError::Db(e) => write!(f, "db: {e}"),
            SchemaError::NotFound(s) => write!(f, "not found: {s}"),
            SchemaError::Io(s) => write!(f, "io: {s}"),
        }
    }
}

fn escape_sql_str(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

/// Format an f64 the way Python's str(float) does (so DEFAULT values match Frappe byte-for-byte):
/// integral floats get a trailing ".0".
fn fmt_flt(x: f64) -> String {
    let s = format!("{x}");
    if s.contains('.') || s.contains('e') || s.contains("inf") || s.contains("NaN") {
        s
    } else {
        format!("{s}.0")
    }
}

fn not_null_default(ft: &str) -> String {
    match ft {
        "Check" | "Int" | "Long Int" | "Duration" => "0".to_string(),
        "Currency" | "Float" | "Percent" | "Rating" => "0.0".to_string(),
        _ => "''".to_string(),
    }
}

fn doctype_autoname_is_uuid(con: &Connection, doctype: &str) -> bool {
    con.query_row(
        "SELECT autoname FROM \"tabDocType\" WHERE name=?1",
        params![doctype],
        |r| r.get::<_, Option<String>>(0),
    )
    .ok()
    .flatten()
    .map(|a| a == "UUID")
    .unwrap_or(false)
}

/// Full SQLite column definition for one field (port of DbColumn.get_definition for SQLite).
/// Returns None when the fieldtype has no column.
fn column_definition(con: &Connection, f: &SField, for_modification: bool) -> Option<String> {
    let mut coltype = sqlite_base_type(&f.fieldtype)?.to_string();
    if f.fieldtype == "Link" {
        if let Some(opt) = &f.options {
            if !opt.is_empty() && doctype_autoname_is_uuid(con, opt) {
                coltype = "uuid".to_string();
            }
        }
    }

    let mut null = true;
    let mut default: Option<String> = None;
    let mut unique = false;

    if NOT_NULL_TYPES.contains(&f.fieldtype.as_str()) {
        null = false;
    }
    if f.fieldtype == "Check" || f.fieldtype == "Int" {
        default = Some(cint(f.default.as_deref().unwrap_or("")).to_string());
    } else if matches!(f.fieldtype.as_str(), "Currency" | "Float" | "Percent") {
        default = Some(fmt_flt(flt(f.default.as_deref().unwrap_or(""))));
    } else if let Some(d) = &f.default {
        if !d.is_empty() && !DEFAULT_SHORTCUTS.contains(&d.as_str()) && !d.starts_with(':') {
            default = Some(escape_sql_str(d));
        }
    }

    if f.not_nullable && null {
        if default.is_none() {
            default = Some(not_null_default(&f.fieldtype));
        }
        null = false;
    }

    if f.unique && !for_modification && coltype != "text" && coltype != "longtext" {
        unique = true;
    }

    let mut s = coltype;
    if !null {
        s.push_str(" NOT NULL");
    }
    if let Some(d) = default {
        s.push_str(&format!(" DEFAULT {d}"));
    }
    if unique {
        s.push_str(" UNIQUE");
    }
    Some(s)
}

/// Read the DocType's table-shaping attributes from tabDocType.
fn load_doctype_info(con: &Connection, doctype: &str) -> Result<DocTypeInfo, SchemaError> {
    con.query_row(
        "SELECT COALESCE(issingle,0), COALESCE(istable,0), COALESCE(is_virtual,0), autoname, \
         COALESCE(sort_field,'modified'), COALESCE(track_seen,0) \
         FROM \"tabDocType\" WHERE name=?1",
        params![doctype],
        |r| {
            Ok(DocTypeInfo {
                issingle: r.get::<_, i64>(0)? != 0,
                istable: r.get::<_, i64>(1)? != 0,
                is_virtual: r.get::<_, i64>(2)? != 0,
                autoname: r.get::<_, Option<String>>(3)?,
                sort_field: r.get::<_, String>(4)?,
                track_seen: r.get::<_, i64>(5)? != 0,
            })
        },
    )
    .map_err(|e| match e {
        rusqlite::Error::QueryReturnedNoRows => SchemaError::NotFound(doctype.to_string()),
        other => SchemaError::Db(other),
    })
}

/// The doctype's value-bearing fields, in idx order: standard docfields then custom fields.
fn schema_fields(con: &Connection, doctype: &str) -> Result<Vec<SField>, SchemaError> {
    let mut out = Vec::new();
    {
        let mut stmt = con.prepare(
            "SELECT fieldname, COALESCE(fieldtype,'Data'), options, \"default\", \
             COALESCE(search_index,0), COALESCE(\"unique\",0), COALESCE(not_nullable,0) \
             FROM \"tabDocField\" WHERE parent=?1 ORDER BY idx, creation",
        )?;
        let rows = stmt.query_map(params![doctype], |r| {
            Ok(SField {
                fieldname: r.get::<_, Option<String>>(0)?.unwrap_or_default(),
                fieldtype: r.get::<_, String>(1)?,
                options: r.get::<_, Option<String>>(2)?,
                default: r.get::<_, Option<String>>(3)?,
                search_index: r.get::<_, i64>(4)? != 0,
                unique: r.get::<_, i64>(5)? != 0,
                not_nullable: r.get::<_, i64>(6)? != 0,
            })
        })?;
        for f in rows.flatten() {
            if !f.fieldname.is_empty() && !no_value_field(&f.fieldtype) {
                out.push(f);
            }
        }
    }
    // custom fields (tabCustom Field) — best effort; the table/columns exist on frappe sites.
    if table_exists(con, "tabCustom Field") {
        if let Ok(mut stmt) = con.prepare(
            "SELECT fieldname, COALESCE(fieldtype,'Data'), options, \"default\", \
             COALESCE(search_index,0), COALESCE(\"unique\",0) \
             FROM \"tabCustom Field\" WHERE dt=?1 ORDER BY idx, creation",
        ) {
            if let Ok(rows) = stmt.query_map(params![doctype], |r| {
                Ok(SField {
                    fieldname: r.get::<_, Option<String>>(0)?.unwrap_or_default(),
                    fieldtype: r.get::<_, String>(1)?,
                    options: r.get::<_, Option<String>>(2)?,
                    default: r.get::<_, Option<String>>(3)?,
                    search_index: r.get::<_, i64>(4)? != 0,
                    unique: r.get::<_, i64>(5)? != 0,
                    not_nullable: false,
                })
            }) {
                for f in rows.flatten() {
                    if !f.fieldname.is_empty() && !no_value_field(&f.fieldtype) {
                        out.push(f);
                    }
                }
            }
        }
    }
    Ok(out)
}

fn table_exists(con: &Connection, table: &str) -> bool {
    con.query_row(
        "SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1",
        params![table],
        |_| Ok(()),
    )
    .is_ok()
}

fn column_set(con: &Connection, table: &str) -> Vec<String> {
    let mut cols = Vec::new();
    if let Ok(mut stmt) = con.prepare(&format!("PRAGMA table_info({})", quote_ident(table))) {
        if let Ok(rows) = stmt.query_map([], |r| r.get::<_, String>(1)) {
            cols.extend(rows.flatten());
        }
    }
    cols
}

/// Sync the physical `tab<DocType>` table to the doctype meta. CREATE if absent, else ALTER to add
/// any missing columns/indexes. (Singles/virtual doctypes have no table.) Returns whether it acted.
pub fn sync_table(con: &Connection, doctype: &str) -> Result<bool, SchemaError> {
    let info = load_doctype_info(con, doctype)?;
    if info.is_virtual || info.issingle {
        return Ok(false); // singles live in tabSingles; virtual has no storage
    }
    let table = format!("tab{doctype}");
    let fields = schema_fields(con, doctype)?;

    // desired docfield columns, in order, with their definitions
    let mut docfield_cols: Vec<(String, String)> = Vec::new();
    for f in &fields {
        if let Some(def) = column_definition(con, f, false) {
            docfield_cols.push((f.fieldname.clone(), def));
        }
    }
    // optional columns (non-child) + _seen
    if !info.istable {
        for c in OPTIONAL_COLUMNS {
            docfield_cols.push((c.to_string(), "TEXT".to_string()));
        }
        if info.track_seen {
            docfield_cols.push(("_seen".to_string(), "TEXT".to_string()));
        }
    }

    if !table_exists(con, &table) {
        create_table(con, &table, &info, &docfield_cols)?;
    } else {
        alter_table(con, &table, &docfield_cols)?;
    }
    // search-index field indexes (frappe defers these to alter; we add them in one pass).
    create_field_indexes(con, &table, &fields);
    Ok(true)
}

fn create_table(
    con: &Connection,
    table: &str,
    info: &DocTypeInfo,
    docfield_cols: &[(String, String)],
) -> Result<(), SchemaError> {
    let name_column = match info.autoname.as_deref() {
        Some("autoincrement") if !info.issingle => "name INTEGER PRIMARY KEY AUTOINCREMENT",
        _ => "name TEXT PRIMARY KEY",
    };
    let mut defs: Vec<String> = docfield_cols
        .iter()
        .map(|(n, d)| format!("{} {}", quote_ident(n), d))
        .collect();
    if info.istable {
        defs.push("parent TEXT".to_string());
        defs.push("parentfield TEXT".to_string());
        defs.push("parenttype TEXT".to_string());
    }
    let additional = if defs.is_empty() {
        String::new()
    } else {
        format!(",\n{}", defs.join(",\n"))
    };
    let create_sql = format!(
        "CREATE TABLE {tbl} (\n\
         {name_column},\n\
         creation DATETIME,\n\
         modified DATETIME,\n\
         modified_by TEXT,\n\
         owner TEXT,\n\
         docstatus INTEGER NOT NULL DEFAULT 0,\n\
         idx INTEGER NOT NULL DEFAULT 0{additional})",
        tbl = quote_ident(table),
    );
    con.execute_batch(&create_sql)?;

    // indexes
    if info.istable {
        con.execute_batch(&format!(
            "CREATE INDEX {idx} ON {tbl}(parent)",
            idx = quote_ident(&format!("{table}_parent_idx")),
            tbl = quote_ident(table)
        ))?;
    } else {
        con.execute_batch(&format!(
            "CREATE INDEX {idx} ON {tbl}(creation)",
            idx = quote_ident(&format!("{table}_creation_idx")),
            tbl = quote_ident(table)
        ))?;
        if info.sort_field == "modified" {
            con.execute_batch(&format!(
                "CREATE INDEX {idx} ON {tbl}(modified)",
                idx = quote_ident(&format!("{table}_modified_idx")),
                tbl = quote_ident(table)
            ))?;
        }
    }
    Ok(())
}

/// ALTER: add any desired columns missing from the live table. (Type/default/nullability changes
/// would require SQLite's table-rebuild dance — not done here; we add columns, the 99% migrate case.)
fn alter_table(con: &Connection, table: &str, docfield_cols: &[(String, String)]) -> Result<(), SchemaError> {
    let existing: Vec<String> = column_set(con, table).iter().map(|c| c.to_lowercase()).collect();
    for (name, def) in docfield_cols {
        if !existing.contains(&name.to_lowercase()) {
            con.execute_batch(&format!(
                "ALTER TABLE {tbl} ADD COLUMN {col} {def}",
                tbl = quote_ident(table),
                col = quote_ident(name),
            ))?;
        }
    }
    Ok(())
}

fn create_field_indexes(con: &Connection, table: &str, fields: &[SField]) {
    for f in fields {
        if f.search_index && sqlite_base_type(&f.fieldtype).is_some() {
            let idx = format!("{}_index", f.fieldname);
            let _ = con.execute_batch(&format!(
                "CREATE INDEX IF NOT EXISTS {idx} ON {tbl} ({col})",
                idx = quote_ident(&idx),
                tbl = quote_ident(table),
                col = quote_ident(&f.fieldname),
            ));
        }
    }
}

// =====================================================================================
// importer: DocType JSON -> tabDocType + child tables
// =====================================================================================

fn scrub(name: &str) -> String {
    name.replace([' ', '-'], "_").to_lowercase()
}

/// The child arrays of a DocType JSON and the tables/parentfields they map to.
const DOCTYPE_CHILDREN: &[(&str, &str)] = &[
    ("fields", "tabDocField"),
    ("permissions", "tabDocPerm"),
    ("actions", "tabDocType Action"),
    ("links", "tabDocType Link"),
    ("states", "tabDocType State"),
];

/// Import a DocType JSON into tabDocType + its child tables (DocField/DocPerm/…). When
/// `reset_permissions` is false, existing tabDocPerm rows are preserved (migrate semantics);
/// when true they are replaced (install / reload-doctype semantics).
pub fn import_doctype(con: &Connection, doc: &Value, reset_permissions: bool) -> Result<(), SchemaError> {
    let obj = doc.as_object().ok_or_else(|| SchemaError::Io("doctype json is not an object".into()))?;
    let name = obj
        .get("name")
        .and_then(|v| v.as_str())
        .ok_or_else(|| SchemaError::Io("doctype json has no name".into()))?
        .to_string();

    // upsert the parent row (tabDocType)
    upsert_doc_row(con, "tabDocType", &name, obj, None)?;

    // child tables
    for (key, table) in DOCTYPE_CHILDREN {
        if *key == "permissions" && !reset_permissions {
            // migrate: only seed perms if none exist
            let have: bool = con
                .query_row(
                    "SELECT 1 FROM \"tabDocPerm\" WHERE parent=?1 LIMIT 1",
                    params![name],
                    |_| Ok(()),
                )
                .is_ok();
            if have {
                continue;
            }
        }
        // replace child rows for this parent
        con.execute(&format!("DELETE FROM {} WHERE parent=?1", quote_ident(table)), params![name])?;
        if let Some(arr) = obj.get(*key).and_then(|v| v.as_array()) {
            for (i, child) in arr.iter().enumerate() {
                if let Some(cobj) = child.as_object() {
                    insert_child_row(con, table, &name, "DocType", key, i + 1, cobj)?;
                }
            }
        }
    }
    Ok(())
}

/// INSERT OR REPLACE a top-level doc row: intersect the JSON's scalar keys with the table's columns.
fn upsert_doc_row(
    con: &Connection,
    table: &str,
    name: &str,
    obj: &Map<String, Value>,
    parent: Option<(&str, &str, &str, usize)>, // (parent, parenttype, parentfield, idx)
) -> Result<(), SchemaError> {
    let cols = column_set(con, table);
    let now = now_datetime();
    let mut col_names: Vec<String> = Vec::new();
    let mut values: Vec<Value> = Vec::new();

    let put = |k: &str, v: Value, col_names: &mut Vec<String>, values: &mut Vec<Value>| {
        if cols.iter().any(|c| c == k) && !col_names.iter().any(|c| c == k) {
            col_names.push(k.to_string());
            values.push(v);
        }
    };

    put("name", Value::String(name.to_string()), &mut col_names, &mut values);
    let creation = obj.get("creation").and_then(|v| v.as_str()).unwrap_or(&now).to_string();
    let modified = obj.get("modified").and_then(|v| v.as_str()).unwrap_or(&now).to_string();
    let owner = obj.get("owner").and_then(|v| v.as_str()).unwrap_or("Administrator").to_string();
    put("creation", Value::String(creation), &mut col_names, &mut values);
    put("modified", Value::String(modified), &mut col_names, &mut values);
    put("modified_by", Value::String(owner.clone()), &mut col_names, &mut values);
    put("owner", Value::String(owner), &mut col_names, &mut values);
    put("docstatus", Value::from(obj.get("docstatus").and_then(|v| v.as_i64()).unwrap_or(0)), &mut col_names, &mut values);
    let idx_val = parent.map(|p| p.3 as i64).or_else(|| obj.get("idx").and_then(|v| v.as_i64())).unwrap_or(0);
    put("idx", Value::from(idx_val), &mut col_names, &mut values);
    if let Some((p, pt, pf, _)) = parent {
        put("parent", Value::String(p.to_string()), &mut col_names, &mut values);
        put("parenttype", Value::String(pt.to_string()), &mut col_names, &mut values);
        put("parentfield", Value::String(pf.to_string()), &mut col_names, &mut values);
    }

    // every other scalar JSON key that is a real column
    for (k, v) in obj {
        if matches!(k.as_str(), "name" | "creation" | "modified" | "modified_by" | "owner" | "docstatus" | "idx") {
            continue;
        }
        if v.is_object() || v.is_array() {
            continue; // child tables / nested handled separately
        }
        put(k, v.clone(), &mut col_names, &mut values);
    }

    // The frozen frappe-core seed tables (tabDocType, tabDocField, tabDocPerm, …) were carved WITHOUT
    // a PRIMARY KEY on `name` (only secondary indexes), so "INSERT OR REPLACE" has no conflict target
    // and would *append* a duplicate row when a record is re-imported (e.g. `migrate` re-syncs every
    // app's doctypes). Delete any existing row(s) for this name first, making the write a true upsert
    // on every table — PK or not — and self-healing any duplicates a prior sync already created.
    con.execute(&format!("DELETE FROM {} WHERE name=?1", quote_ident(table)), params![name])?;

    let placeholders: Vec<String> = (1..=col_names.len()).map(|i| format!("?{i}")).collect();
    let quoted: Vec<String> = col_names.iter().map(|c| quote_ident(c)).collect();
    let sql = format!(
        "INSERT OR REPLACE INTO {tbl} ({cols}) VALUES ({ph})",
        tbl = quote_ident(table),
        cols = quoted.join(", "),
        ph = placeholders.join(", "),
    );
    let sql_values: Vec<rusqlite::types::Value> = values.iter().map(json_to_sql).collect();
    con.execute(&sql, rusqlite::params_from_iter(sql_values))?;
    Ok(())
}

/// Convert a serde_json scalar to a rusqlite storage value (arrays/objects never reach here).
fn json_to_sql(v: &Value) -> rusqlite::types::Value {
    use rusqlite::types::Value as V;
    match v {
        Value::Null => V::Null,
        Value::Bool(b) => V::Integer(if *b { 1 } else { 0 }),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                V::Integer(i)
            } else if let Some(f) = n.as_f64() {
                V::Real(f)
            } else {
                V::Null
            }
        }
        Value::String(s) => V::Text(s.clone()),
        other => V::Text(other.to_string()),
    }
}

fn insert_child_row(
    con: &Connection,
    table: &str,
    parent: &str,
    parenttype: &str,
    parentfield: &str,
    idx: usize,
    cobj: &Map<String, Value>,
) -> Result<(), SchemaError> {
    let name = cobj
        .get("name")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .unwrap_or_else(|| format!("{}_{}", scrub(parent), idx));
    upsert_doc_row(con, table, &name, cobj, Some((parent, parenttype, parentfield, idx)))
}

/// A doctype's child-table fields: (fieldname, child doctype) for every Table / Table MultiSelect
/// field, read from the already-synced tabDocField rows.
fn child_table_fields(con: &Connection, doctype: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    if let Ok(mut stmt) = con.prepare(
        "SELECT fieldname, options FROM \"tabDocField\" WHERE parent=?1 \
         AND fieldtype IN ('Table','Table MultiSelect') AND options IS NOT NULL AND options<>''",
    ) {
        if let Ok(rows) = stmt.query_map(params![doctype], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        }) {
            for row in rows.flatten() {
                out.push(row);
            }
        }
    }
    out
}

/// Import a single non-DocType record fixture (Report, Print Format, Number Card, Web Form, …) into
/// its table + child tables, idempotently (INSERT OR REPLACE). The target doctype is taken from the
/// JSON's `doctype`; its child-table fields are discovered from the synced DocFields. Records whose
/// table (or a child table) isn't installed are skipped. Returns Ok(true) if the main row was
/// written. This is the file-fixture half of Frappe's `sync_for` — the part that needs no Python.
pub fn import_record(con: &Connection, doc: &Value) -> Result<bool, SchemaError> {
    let obj = match doc.as_object() {
        Some(o) => o,
        None => return Ok(false),
    };
    let doctype = match obj.get("doctype").and_then(|v| v.as_str()) {
        Some(d) => d,
        None => return Ok(false),
    };
    if doctype == "DocType" {
        return Ok(false); // DocTypes go through import_doctype (creates the table too)
    }
    let name = match obj.get("name").and_then(|v| v.as_str()) {
        Some(n) => n.to_string(),
        None => return Ok(false),
    };
    let table = format!("tab{doctype}");
    if !table_exists(con, &table) {
        return Ok(false); // the record's doctype isn't installed on this site
    }
    upsert_doc_row(con, &table, &name, obj, None)?;
    for (fieldname, child_dt) in child_table_fields(con, doctype) {
        let child_table = format!("tab{child_dt}");
        if !table_exists(con, &child_table) {
            continue;
        }
        con.execute(
            &format!(
                "DELETE FROM {} WHERE parent=?1 AND parentfield=?2 AND parenttype=?3",
                quote_ident(&child_table)
            ),
            params![name, fieldname, doctype],
        )?;
        if let Some(arr) = obj.get(&fieldname).and_then(|v| v.as_array()) {
            for (i, child) in arr.iter().enumerate() {
                if let Some(cobj) = child.as_object() {
                    insert_child_row(con, &child_table, &name, doctype, &fieldname, i + 1, cobj)?;
                }
            }
        }
    }
    Ok(true)
}

// rusqlite ToSql for serde_json::Value (text/int/real/bool/null) — used by the importer.
// (Implemented inline via a wrapper since serde_json::Value isn't ToSql by default.)

// =====================================================================================
// reload one doctype from disk
// =====================================================================================

/// Locate `<app>/.../doctype/<slug>/<slug>.json` for a doctype under the bench's apps/ dir.
pub fn find_doctype_json(bench_root: &Path, doctype: &str) -> Option<PathBuf> {
    let slug = scrub(doctype);
    let target = format!("{slug}/{slug}.json");
    let apps = bench_root.join("apps");
    let app_dirs = std::fs::read_dir(&apps).ok()?;
    for app in app_dirs.flatten() {
        let app_path = app.path();
        if !app_path.is_dir() {
            continue;
        }
        // search up to a bounded depth for */doctype/<slug>/<slug>.json
        if let Some(p) = walk_for(&app_path, "doctype", &target, 5) {
            return Some(p);
        }
    }
    None
}

fn walk_for(dir: &Path, anchor: &str, tail: &str, depth: usize) -> Option<PathBuf> {
    if depth == 0 {
        return None;
    }
    let rd = std::fs::read_dir(dir).ok()?;
    let mut subdirs = Vec::new();
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            if p.file_name().map(|n| n == anchor).unwrap_or(false) {
                let candidate = p.join(tail);
                if candidate.is_file() {
                    return Some(candidate);
                }
            }
            subdirs.push(p);
        }
    }
    for s in subdirs {
        if let Some(found) = walk_for(&s, anchor, tail, depth - 1) {
            return Some(found);
        }
    }
    None
}

/// reload-doctype: re-import a doctype's JSON from disk and re-sync its table.
pub fn reload_doctype(con: &Connection, bench_root: &Path, doctype: &str) -> Result<(), SchemaError> {
    let path = find_doctype_json(bench_root, doctype)
        .ok_or_else(|| SchemaError::NotFound(format!("doctype json for {doctype}")))?;
    let text = std::fs::read_to_string(&path).map_err(|e| SchemaError::Io(e.to_string()))?;
    let doc: Value = serde_json::from_str(&text).map_err(|e| SchemaError::Io(e.to_string()))?;
    import_doctype(con, &doc, true)?;
    sync_table(con, doctype)?;
    Ok(())
}
