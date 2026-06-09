"""
frappe.query_builder.functions — the SQL function constructors app controllers import directly,
e.g. `from frappe.query_builder.functions import Count`. Each returns a `Func` expression that
renders real SQL (`COUNT("name")`, `COALESCE("a", 0)`, …) and flows through select()/.as_() like a
column. Before this module existed the import failed and the aggregate term rendered empty, yielding
`SELECT "owner",  FROM …` (gameplan's get_user_info / unread_notifications hit exactly that).
"""
from frappe.query_builder import Func, _FN_SQL


def _make(name):
    sql = _FN_SQL.get(name, name.upper())

    def _fn(*args, **kwargs):
        return Func(sql, *args)

    return _fn


# The functions seen across frappe + the installed apps' controllers.
Count = _make("Count")
Sum = _make("Sum")
Max = _make("Max")
Min = _make("Min")
Avg = _make("Avg")
Abs = _make("Abs")
Coalesce = _make("Coalesce")
IfNull = _make("IfNull")
Ifnull = _make("Ifnull")
Concat = _make("Concat")
Concat_ws = _make("Concat_ws")
GroupConcat = _make("GroupConcat")
Replace = _make("Replace")
Cast = _make("Cast")
Cast_ = _make("Cast_")
Date = _make("Date")
Now = _make("Now")
Timestamp = _make("Timestamp")
DateFormat = _make("DateFormat")
Locate = _make("Locate")
Match = _make("Match")
Function = _make("Function")

# pypika-style lowercase aggregate aliases some code imports (_avg/_max/_min/_sum).
_avg = Avg
_max = Max
_min = Min
_sum = Sum


def __getattr__(name):
    # Any other function name -> a permissive SQL function constructor (FUNC(args)).
    return _make(name)
