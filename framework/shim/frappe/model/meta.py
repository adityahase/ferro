"""frappe.model.meta — DocType meta, backed by the native ferro_rt.get_meta."""
try:
    import ferro_rt as _rt
except ImportError:
    _rt = None


class DocField(dict):
    def __getattr__(self, k):
        return self.get(k)
    def __setattr__(self, k, v):
        self[k] = v
    def as_dict(self, *a, **k):
        # Return an attribute-accessible _dict: callers do field.as_dict() then access .fieldname /
        # .fieldtype etc. (a plain dict would AttributeError).
        import frappe
        return frappe._dict(self)


class Meta:
    _cache = {}

    def __init__(self, doctype):
        if isinstance(doctype, Meta):
            self.__dict__.update(doctype.__dict__)
            return
        self.name = doctype
        raw = Meta._cache.get(doctype)
        if raw is None and _rt is not None and doctype:
            try:
                raw = _rt.get_meta(doctype)
            except Exception:
                raw = None
            Meta._cache[doctype] = raw
        raw = raw or {"fields": [], "issingle": False, "istable": False}
        self.issingle = raw.get("issingle", False)
        self.istable = raw.get("istable", False)
        self.is_virtual = raw.get("is_virtual", False)
        self.autoname = raw.get("autoname")
        self._fields = [DocField(f) for f in raw.get("fields", [])]
        # Frappe auto-derives a field label from its fieldname when unset; mirror it so app code
        # that does "..." + field.label + "..." (crm fields-layout) never hits None.
        for f in self._fields:
            if not f.get("label") and f.get("fieldname"):
                f["label"] = str(f["fieldname"]).replace("_", " ").title()
        self._by_name = {f["fieldname"]: f for f in self._fields}

    @property
    def fields(self):
        return self._fields

    def get(self, key, default=None, filters=None):
        # Frappe semantics: meta.get("<child-table>", {filters}) returns the matching child rows
        # (e.g. meta.get("fields", {"fieldname": "enabled"}) -> [] if there is no such field). The
        # 2nd positional arg is the filter dict in that form, NOT a default. Callers rely on the
        # empty-list-is-falsy result (crm.api.contact.search_emails).
        flt = filters if isinstance(filters, dict) else (default if isinstance(default, dict) else None)
        val = getattr(self, key, None)
        if flt is not None and isinstance(val, list):
            return [r for r in val if all((r.get(k) if hasattr(r, "get") else None) == v for k, v in flt.items())]
        return val if val is not None else default

    def as_dict(self, no_nulls=False, *a, **k):
        # Return COPIES of the field dicts: callers (e.g. crm.api.doc.get_filterable_fields) mutate
        # the returned fields in place, which would otherwise corrupt the shared meta cache.
        return {
            "name": self.name,
            "issingle": self.issingle,
            "istable": self.istable,
            "is_virtual": self.is_virtual,
            "autoname": self.autoname,
            "fields": [dict(f) for f in self._fields],
        }

    def get_field(self, fieldname):
        return self._by_name.get(fieldname)

    def has_field(self, fieldname):
        return fieldname in self._by_name

    def get_label(self, fieldname):
        f = self._by_name.get(fieldname)
        return (f.get("label") if f else None) or fieldname

    def get_options(self, fieldname):
        f = self._by_name.get(fieldname)
        return f.get("options") if f else None

    def get_valid_columns(self):
        return [f["fieldname"] for f in self._fields]

    def get_table_fields(self):
        return [f for f in self._fields if f.get("fieldtype") in ("Table", "Table MultiSelect")]

    def get_link_fields(self):
        return [f for f in self._fields if f.get("fieldtype") == "Link"]

    def get_dynamic_link_fields(self):
        return [f for f in self._fields if f.get("fieldtype") == "Dynamic Link"]

    def get_fields_to_fetch(self, *a, **k):
        return []

    def get_set_only_once_fields(self):
        return []

    def get_high_permlevel_fields(self):
        return [f for f in self._fields if (f.get("permlevel") or 0) > 0]

    def get_permlevel_access(self, *a, **k):
        return [0]

    def get_field_precision(self, *a, **k):
        return 2

    def get_default_currency(self):
        return None


def get_field_precision(df, doc=None, currency=None):
    return 2


def get_default_df(fieldname):
    return None


def __getattr__(name):
    from frappe._lazy import stub_attr
    return stub_attr(name)
