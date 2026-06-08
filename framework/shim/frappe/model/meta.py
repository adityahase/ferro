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
        self._by_name = {f["fieldname"]: f for f in self._fields}

    @property
    def fields(self):
        return self._fields

    def get(self, key, default=None):
        return getattr(self, key, default)

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
