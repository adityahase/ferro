"""pypika — minimal stand-in re-exporting ferro's frappe.query_builder primitives.

Frappe's real query builder IS pypika; app controllers freely `from pypika import Criterion, Order,
Case, functions as fn` and combine those with `frappe.qb` Fields. ferro ships its own tiny SQLite
query builder in frappe.query_builder, so the only way `from pypika import X` works (rather than
resolving to the permissive lazy-finder stub, which silently turns `Criterion.any([...])` into a
no-op and renders an empty `WHERE ()`) is to provide a real pypika package that hands back the same
working primitives. Anything not modelled degrades to a permissive stub so imports never fail.
"""

from frappe.query_builder import (  # noqa: F401
    Criterion,
    Field,
    Func,
    Order,
    Query,
    Table,
    _lit,
)
from . import functions  # noqa: F401
from . import terms  # noqa: F401


class Case:
    """CASE WHEN <cond> THEN <val> [WHEN ...] [ELSE <val>] END — renders as a Field-like term so it
    flows through select()/Func args (crm dashboard wraps it in Count(Case()...))."""

    def __init__(self):
        self._whens = []
        self._else = None

    def when(self, cond, val):
        self._whens.append((cond, val))
        return self

    def else_(self, val):
        self._else = val
        return self

    def _col(self):
        from frappe.query_builder import _arg_sql
        parts = ["CASE"]
        for cond, val in self._whens:
            cs = cond.get_sql() if hasattr(cond, "get_sql") else str(cond)
            parts.append(f"WHEN {cs} THEN {_arg_sql(val)}")
        if self._else is not None or self._whens:
            parts.append(f"ELSE {_arg_sql(self._else)}")
        parts.append("END")
        return " ".join(parts)

    def as_(self, alias):
        self.alias = alias
        return self

    def get_sql(self, **k):
        return self._col()

    def __str__(self):
        return self._col()


class JoinType:
    inner = "INNER"
    left = "LEFT"
    right = "RIGHT"
    outer = "FULL OUTER"


def __getattr__(name):
    # Long tail of pypika names a controller may import (MySQLQuery, Dialects, ...). Keep imports
    # from failing; what's actually exercised has a real impl above / in terms.py / functions.py.
    from frappe._lazy import stub_attr
    return stub_attr(name)
