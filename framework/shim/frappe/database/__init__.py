"""frappe.db — the database facade, delegating to the native ferro_rt (ferro's SQLite ORM)."""
import json as _json


class Database:
    def __init__(self, rt):
        self._rt = rt

    # ---- single-value reads ----
    def get_value(self, doctype, filters=None, fieldname="name", as_dict=False,
                  order_by=None, pluck=False, **kwargs):
        rt = self._rt
        # name given directly
        if isinstance(filters, str) or filters is None:
            name = filters
            if name is None:
                # single doctype value
                return self.get_single_value(doctype, fieldname if isinstance(fieldname, str) else "name")
            if isinstance(fieldname, (list, tuple)):
                doc = rt.get_doc(doctype, name)
                if as_dict:
                    from frappe import _dict
                    return _dict({f: doc.get(f) for f in fieldname})
                return [doc.get(f) for f in fieldname]
            return rt.get_value(doctype, name, fieldname)
        # filters dict -> find first match
        flj = _json.dumps(filters)
        rows = rt.get_list(doctype, flj, _json.dumps(["name"]), order_by, 0, 1)
        if not rows:
            return None
        name = rows[0]["name"]
        if isinstance(fieldname, (list, tuple)):
            doc = rt.get_doc(doctype, name)
            if as_dict:
                from frappe import _dict
                return _dict({f: doc.get(f) for f in fieldname})
            return [doc.get(f) for f in fieldname]
        return rt.get_value(doctype, name, fieldname)

    def get_values(self, doctype, filters=None, fieldname="name", as_dict=False, **kwargs):
        v = self.get_value(doctype, filters, fieldname, as_dict=as_dict, **kwargs)
        if v is None:
            return []
        return [v]

    def get_single_value(self, doctype, fieldname):
        try:
            doc = self._rt.get_doc(doctype, doctype)
        except Exception:
            return None
        return doc.get(fieldname)

    def get_all(self, doctype, *args, **kwargs):
        import frappe
        # Frappe's db.get_all takes (doctype, filters, fields, ...) positionally — but a lot of app
        # code (e.g. gameplan.api.unread_notifications) calls db.get_all(dt, FIELDS, FILTERS). The
        # real query layer disambiguates by shape: a list -> fields, a dict/list-of-lists -> filters.
        for a in args:
            if isinstance(a, (list, tuple)) and not (a and isinstance(a[0], (list, tuple))):
                kwargs.setdefault("fields", list(a))
            else:
                kwargs.setdefault("filters", a)
        return frappe.get_all(doctype, **kwargs)

    def get_list(self, doctype, *args, **kwargs):
        return self.get_all(doctype, *args, **kwargs)

    def exists(self, doctype, name=None, cache=False):
        if isinstance(doctype, dict):
            # exists({"doctype": dt, ...filters})
            dt = doctype.get("doctype")
            filters = {k: v for k, v in doctype.items() if k != "doctype"}
            v = self.get_value(dt, filters, "name")
            return v
        if isinstance(name, dict):
            v = self.get_value(doctype, name, "name")
            return v
        if name is None:
            name = doctype
        return name if self._rt.exists(doctype, name) else None

    def count(self, doctype, filters=None, **kwargs):
        flj = _json.dumps(filters) if filters else None
        return self._rt.count(doctype, flj)

    # ---- writes ----
    def set_value(self, doctype, name, fieldname, value=None, **kwargs):
        if isinstance(fieldname, dict):
            for f, v in fieldname.items():
                self._rt.set_value(doctype, name, f, _json.dumps(v))
            return
        return self._rt.set_value(doctype, name, fieldname, _json.dumps(value))

    def set_single_value(self, doctype, fieldname, value=None, **kwargs):
        if isinstance(fieldname, dict):
            for f, v in fieldname.items():
                self._rt.update(doctype, doctype, _json.dumps({f: v}))
            return
        return self._rt.update(doctype, doctype, _json.dumps({fieldname: value}))

    def get_single(self, doctype):
        return self._rt.get_doc(doctype, doctype)

    def delete(self, doctype, filters=None, **kwargs):
        if isinstance(filters, str):
            self._rt.delete(doctype, filters)

    # ---- raw SQL passthrough ----
    def sql(self, query, values=None, as_dict=False, as_list=False, **kwargs):
        if hasattr(query, "get_sql"):
            query = query.get_sql()
        params = None
        if values is not None:
            if isinstance(values, (list, tuple)):
                params = _json.dumps(list(values))
            elif isinstance(values, dict):
                params = _json.dumps(list(values.values()))
            else:
                params = _json.dumps([values])
        rows = self._rt.sql(str(query), params, bool(as_dict))
        if as_dict:
            from frappe import _dict
            return [_dict(r) for r in rows]
        return rows

    def get_descendants(self, doctype, name):
        return []

    def escape(self, s, percent=True):
        return "'" + str(s).replace("'", "''") + "'"

    def commit(self):
        pass

    def rollback(self):
        pass

    def savepoint(self, *a, **k):
        pass

    def begin(self, *a, **k):
        pass

    def has_column(self, doctype, column):
        return True

    def get_table_columns(self, doctype):
        meta = self._rt.get_meta(doctype)
        return [f["fieldname"] for f in meta["fields"]]

    def add_index(self, *a, **k):
        pass

    def field_exists(self, doctype, fieldname):
        return True

    def multisql(self, sql_dict, *a, **k):
        # {"mariadb": "...", "sqlite": "...", "postgres": "..."} — pick sqlite/default
        if isinstance(sql_dict, dict):
            q = sql_dict.get("sqlite") or sql_dict.get("default") or next(iter(sql_dict.values()))
            return self.sql(q, *a, **k)
        return self.sql(sql_dict, *a, **k)
