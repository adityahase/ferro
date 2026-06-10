"""pypika.functions — SQL function constructors (Coalesce, Sum, Count, Replace, ...). Delegates to
frappe.query_builder's Func so they render as real `FUNC(args)` expressions and flow through
select()/.as_() like a column."""

from frappe.query_builder import Func, _FN_SQL


def _make(name):
    sql = _FN_SQL.get(name, name.upper())

    def _fn(*args, **kwargs):
        return Func(sql, *args)

    return _fn


Coalesce = _make("Coalesce")
Sum = _make("Sum")
Count = _make("Count")
Max = _make("Max")
Min = _make("Min")
Avg = _make("Avg")
Replace = _make("Replace")
Abs = _make("Abs")
Concat = _make("Concat")
Function = _make("Function")
Cast = _make("Cast")
Date = _make("Date")
IfNull = _make("IfNull")


def __getattr__(name):
    # Any other SQL function name -> render FUNC(args).
    if name[:1].isupper():
        return _make(name)
    from frappe._lazy import stub_attr
    return stub_attr(name)
