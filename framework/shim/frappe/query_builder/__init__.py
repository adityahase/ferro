"""
frappe.qb — a small, SQLite-compiling stand-in for Frappe's pypika query builder.

Implements the fluent subset app controllers use (from_/select/where/orderby/limit/run),
compiling to SQL that executes via the native ferro_rt.sql. Unsupported corners degrade to a
permissive no-op so imports never fail; what runs, runs against the real DB.
"""
import json as _json

try:
    import ferro_rt as _rt
except ImportError:
    _rt = None


class Field:
    def __init__(self, name, table=None):
        self.name = name
        self.table = table

    def _col(self):
        return f'"{self.name}"'

    def __eq__(self, other):  return Criterion(f'{self._col()} = {_lit(other)}')
    def __ne__(self, other):  return Criterion(f'{self._col()} != {_lit(other)}')
    def __gt__(self, other):  return Criterion(f'{self._col()} > {_lit(other)}')
    def __lt__(self, other):  return Criterion(f'{self._col()} < {_lit(other)}')
    def __ge__(self, other):  return Criterion(f'{self._col()} >= {_lit(other)}')
    def __le__(self, other):  return Criterion(f'{self._col()} <= {_lit(other)}')

    def isin(self, vals):
        vals = list(vals)
        inner = ", ".join(_lit(v) for v in vals) if vals else "NULL"
        return Criterion(f'{self._col()} IN ({inner})')

    def notin(self, vals):
        vals = list(vals)
        inner = ", ".join(_lit(v) for v in vals) if vals else "NULL"
        return Criterion(f'{self._col()} NOT IN ({inner})')

    def like(self, pat):
        return Criterion(f'{self._col()} LIKE {_lit(pat)}')

    def isnull(self):
        return Criterion(f'{self._col()} IS NULL')

    def isnotnull(self):
        return Criterion(f'{self._col()} IS NOT NULL')

    def as_(self, alias):
        self.alias = alias
        return self

    def __str__(self):
        return self._col()


class Func(Field):
    """A SQL function expression, e.g. COUNT("name") or COALESCE("a", 0). Subclasses Field so it
    flows through select()/_colsql() (alias via .as_()) exactly like a column. This is what
    `frappe.query_builder.functions.Count/Sum/Max/...` return — without it the aggregate term
    rendered empty and produced `SELECT "owner", FROM ...` (dangling comma)."""

    def __init__(self, fname, *args):
        super().__init__(fname)
        self.fname = fname
        self.args = args
        self._distinct = False

    def distinct(self):
        # pypika's Count(x).distinct() -> COUNT(DISTINCT x). Chainable.
        self._distinct = True
        return self

    def _col(self):
        inner = ", ".join(_arg_sql(a) for a in self.args) if self.args else ""
        if self._distinct:
            inner = f"DISTINCT {inner}"
        return f'{self.fname}({inner})'


def _arg_sql(a):
    # A function argument may be a column (Field), a nested Func, or a literal value.
    if isinstance(a, Field):
        return a._col()
    if a == "*":
        return "*"
    return _lit(a)


def _lit(v):
    if v is None:
        return "NULL"
    if isinstance(v, bool):
        return "1" if v else "0"
    if isinstance(v, (int, float)):
        return str(v)
    if isinstance(v, Field):
        return v._col()
    return "'" + str(v).replace("'", "''") + "'"


def _crit_sql(c):
    if c is None:
        return None
    if hasattr(c, "get_sql"):
        s = c.get_sql()
    else:
        s = str(c)
    s = (s or "").strip()
    return s or None


class Criterion:
    def __init__(self, sql):
        self.sql = sql
    def __and__(self, other):
        return Criterion(f'({self.sql}) AND ({other.sql})')
    def __or__(self, other):
        return Criterion(f'({self.sql}) OR ({other.sql})')
    def __invert__(self):
        return Criterion(f'NOT ({self.sql})')
    def __str__(self):
        return self.sql
    def get_sql(self, **k):
        return self.sql

    # pypika combinators: Criterion.any([...]) -> OR, Criterion.all([...]) -> AND. App controllers
    # (e.g. crm.api.views.get_views) import these from pypika; without a real implementation the
    # `from pypika import Criterion` stub made them no-ops and the WHERE rendered an empty "()".
    @staticmethod
    def any(criteria=None):
        parts = [s for s in (_crit_sql(c) for c in (criteria or [])) if s]
        if not parts:
            return None
        return Criterion(" OR ".join(f'({p})' for p in parts))

    @staticmethod
    def all(criteria=None):
        parts = [s for s in (_crit_sql(c) for c in (criteria or [])) if s]
        if not parts:
            return None
        return Criterion(" AND ".join(f'({p})' for p in parts))


class _OrderVal:
    """pypika.Order member stand-in: carries a SQL direction in `.value` (Query.orderby reads
    getattr(order, 'value', order))."""
    def __init__(self, value):
        self.value = value
    def __str__(self):
        return self.value


class Order:
    asc = _OrderVal("ASC")
    desc = _OrderVal("DESC")


class Table:
    def __init__(self, doctype):
        self.doctype = doctype
        self._table = "tab" + doctype
    def __getattr__(self, name):
        if name.startswith("_"):
            raise AttributeError(name)
        return Field(name, self)
    def __getitem__(self, name):
        return Field(name, self)
    def star(self):
        return Field("*", self)


class Query:
    def __init__(self, table):
        self._table = table
        self._select = []
        self._where = None
        self._orderby = None
        self._limit = None
        self._offset = None
        self._groupby = None

    def select(self, *cols):
        for c in cols:
            self._select.append(c)
        return self

    def where(self, cond):
        if cond is None:
            return self
        c = cond.get_sql() if hasattr(cond, "get_sql") else str(cond)
        self._where = c if self._where is None else f'({self._where}) AND ({c})'
        return self

    def orderby(self, *cols, order=None):
        parts = []
        for c in cols:
            name = c._col() if isinstance(c, Field) else str(c)
            parts.append(name)
        direction = ""
        if order is not None:
            direction = " DESC" if str(getattr(order, "value", order)).lower().startswith("desc") else " ASC"
        self._orderby = ", ".join(parts) + direction
        return self

    def groupby(self, *cols):
        self._groupby = ", ".join(c._col() if isinstance(c, Field) else str(c) for c in cols)
        return self

    def limit(self, n):
        self._limit = n
        return self

    def offset(self, n):
        self._offset = n
        return self

    def _colsql(self, c):
        if isinstance(c, Field):
            s = c._col()
            if getattr(c, "alias", None):
                s += f' AS "{c.alias}"'
            return s
        return str(c)

    def get_sql(self, **k):
        cols = ", ".join(self._colsql(c) for c in self._select) if self._select else "*"
        sql = f'SELECT {cols} FROM "{self._table._table}"'
        if self._where:
            sql += f' WHERE {self._where}'
        if self._groupby:
            sql += f' GROUP BY {self._groupby}'
        if self._orderby:
            sql += f' ORDER BY {self._orderby}'
        if self._limit is not None:
            sql += f' LIMIT {int(self._limit)}'
        if self._offset is not None:
            sql += f' OFFSET {int(self._offset)}'
        return sql

    def run(self, as_dict=False, as_list=False, pluck=None):
        if _rt is None:
            return []
        rows = _rt.sql(self.get_sql(), None, bool(as_dict))
        if as_dict:
            from frappe import _dict
            rows = [_dict(r) for r in rows]
        if pluck:
            return [r[pluck] if as_dict else r[0] for r in rows]
        return rows


class _Builder:
    def from_(self, table):
        if isinstance(table, str):
            table = Table(table)
        return Query(table)

    def into(self, table):
        return Query(table if not isinstance(table, str) else Table(table))

    DocType = staticmethod(lambda name: Table(name))
    Table = staticmethod(lambda name: Table(name))


# frappe.qb is an instance with .from_, .DocType, etc.
import sys as _sys
_self = _sys.modules[__name__]
from_ = _Builder().from_
DocType = lambda name: Table(name)


def get_query(doctype, fields=None, filters=None, order_by=None, distinct=False,
              group_by=None, limit=None, limit_start=0, offset=None, **kwargs):
    """frappe.qb.get_query(...) — Frappe's high-level query helper (used by app controllers like
    crm.api.session.get_users). We don't reimplement its full permission/link machinery; we route to
    the same native get_list path `frappe.get_all` uses, which is what these read queries need.
    Returns a runnable wrapper so the usual `.run(as_dict=1)` works."""
    return _GetQuery(doctype, fields, filters, order_by,
                     0 if limit is None else limit, limit_start or offset or 0)


class _GetQuery:
    def __init__(self, doctype, fields, filters, order_by, limit, limit_start):
        self._doctype, self._fields, self._filters = doctype, fields, filters
        self._order_by, self._limit, self._limit_start = order_by, limit, limit_start

    # Callers chain refinements before .run(). We can't faithfully execute joins/extra selects over
    # the native get_list path, so these degrade to no-ops (the base doctype query still runs) rather
    # than raising AttributeError — what got_discussions et al. need to not 500. on()/get_sql() let a
    # `.left_join(X).on(...)` chain keep flowing.
    def where(self, *a, **k):
        return self
    def orderby(self, *a, **k):
        return self
    def select(self, *a, **k):
        return self
    def groupby(self, *a, **k):
        return self
    def distinct(self, *a, **k):
        return self
    def join(self, *a, **k):
        return self
    def left_join(self, *a, **k):
        return self
    def inner_join(self, *a, **k):
        return self
    def right_join(self, *a, **k):
        return self
    def on(self, *a, **k):
        return self
    def offset(self, n):
        self._limit_start = n
        return self
    def limit(self, n):
        self._limit = n
        return self

    def run(self, as_dict=False, as_list=False, pluck=None):
        import frappe
        rows = frappe.get_all(self._doctype, filters=self._filters, fields=self._fields,
                              order_by=self._order_by, limit=self._limit,
                              limit_start=self._limit_start, pluck=pluck)
        if pluck:
            return rows
        if as_dict:
            return rows  # already attribute-accessible _dict rows
        flds = self._fields or ["name"]
        return [tuple(r.get(f) for f in flds) for r in rows]


# SQL-name overrides for functions whose Python name differs from the SQL keyword.
_FN_SQL = {"Count": "COUNT", "Sum": "SUM", "Max": "MAX", "Min": "MIN", "Avg": "AVG",
           "Coalesce": "COALESCE", "IfNull": "IFNULL", "Ifnull": "IFNULL", "Abs": "ABS",
           "Concat": "CONCAT", "Concat_ws": "CONCAT_WS", "GroupConcat": "GROUP_CONCAT",
           "Now": "DATETIME", "Replace": "REPLACE", "Cast": "CAST", "Cast_": "CAST",
           "Date": "DATE", "Timestamp": "DATETIME", "DateFormat": "STRFTIME", "Locate": "INSTR"}


def _make_fn(name):
    sql = _FN_SQL.get(name, name.upper())

    def _fn(*a, **k):
        return Func(sql, *a)

    return _fn


def __getattr__(name):
    # functions like Count, Sum, Max, etc. -> real SQL function expressions (render FUNC(args)).
    if name in _FN_SQL or name in ("Function", "CustomFunction", "Case", "Extract", "DatePart",
                                   "Tuple", "Interval", "Match", "Now"):
        return _make_fn(name)
    # everything else (JoinType, Order, Criterion helpers, ...) -> permissive Stub type
    from frappe._lazy import stub_attr
    return stub_attr(name)
