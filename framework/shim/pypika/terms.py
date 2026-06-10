"""pypika.terms — lower-level term constructors apps import directly (LiteralValue, Bracket,
ValueWrapper, ExistsCriterion, Order). Rendered against ferro's SQLite query builder."""

from frappe.query_builder import Criterion, Field, Order, _lit  # noqa: F401


class LiteralValue(Field):
    """A raw SQL fragment used verbatim (e.g. LiteralValue('1'))."""

    def __init__(self, value):
        super().__init__(str(value))

    def _col(self):
        return self.name


class ValueWrapper(Field):
    """A wrapped python value, rendered as a quoted SQL literal."""

    def __init__(self, value):
        super().__init__(_lit(value))

    def _col(self):
        return self.name


def Bracket(expr):
    s = expr.get_sql() if hasattr(expr, "get_sql") else str(expr)
    return Criterion(f"({s})")


class ExistsCriterion(Criterion):
    """EXISTS (<subquery>) — subquery is anything with get_sql()."""

    def __init__(self, subquery):
        s = subquery.get_sql() if hasattr(subquery, "get_sql") else str(subquery)
        super().__init__(f"EXISTS ({s})")
