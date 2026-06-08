"""frappe.utils.data — controllers import many helpers from here; re-export from the package."""
from frappe.utils import *  # noqa: F401,F403
from frappe.utils import (  # noqa: F401
    cint, flt, cstr, getdate, get_datetime, now, nowdate, today, nowtime,
    add_days, add_to_date, add_months, add_years, date_diff, time_diff_in_seconds,
    time_diff_in_hours, get_first_day, get_last_day, formatdate, format_datetime,
    flt as _flt, fmt_money, rounded, strip_html, escape_html, get_url, cast,
    DATE_FORMAT, DATETIME_FORMAT, TIME_FORMAT,
)


def __getattr__(name):
    from frappe import utils as _u
    return getattr(_u, name)
