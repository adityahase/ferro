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


class Criterion:
    def __init__(self, sql):
        self.sql = sql
    def __and__(self, other):
        return Criterion(f'({self.sql}) AND ({other.sql})')
    def __or__(self, other):
        return Criterion(f'({self.sql}) OR ({other.sql})')
    def __str__(self):
        return self.sql
    def get_sql(self, **k):
        return self.sql


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


def __getattr__(name):
    # functions like Count, Sum, Max, etc. -> aggregate stubs that still compile to a column
    if name in ("Count", "Sum", "Max", "Min", "Avg", "Coalesce", "IfNull", "Abs",
                "Function", "CustomFunction", "Case", "Cast", "Concat", "Date", "Now",
                "Tuple", "Interval", "DatePart", "Extract", "Locate", "GroupConcat"):
        def _fn(*a, **k):
            return Field(name.lower())
        return _fn
    # everything else (JoinType, Order, Criterion helpers, ...) -> permissive Stub type
    from frappe._lazy import stub_attr
    return stub_attr(name)
