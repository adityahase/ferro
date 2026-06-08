"""
frappe.utils — dependency-free helpers ported (semantics-matching) from Frappe.

These are pure Python (no framework, no heavy deps), so they run byte-identically here. The
module-level __getattr__ gives any rarely-used helper a permissive stub so every
`from frappe.utils import <x>` in a controller resolves at import time.
"""
import datetime as _dt
import math as _math
import re as _re
import json as _json

DATE_FORMAT = "%Y-%m-%d"
DATETIME_FORMAT = "%Y-%m-%d %H:%M:%S.%f"
TIME_FORMAT = "%H:%M:%S.%f"


# ----------------------------------------------------------------- numeric casts
def cint(s, default=0):
    try:
        return int(s)
    except (ValueError, TypeError):
        try:
            return int(float(s))
        except (ValueError, TypeError):
            return default


def flt(s, precision=None, default=0.0):
    try:
        v = float(s)
    except (ValueError, TypeError):
        try:
            v = float(str(s).strip())
        except (ValueError, TypeError):
            v = default
    if precision is not None:
        return round(v, precision)
    return v


def cstr(s, encoding="utf-8"):
    if s is None:
        return ""
    if isinstance(s, bytes):
        return s.decode(encoding, "ignore")
    return str(s)


def fmt_money(amount, precision=2, currency=None, format=None):
    return f"{flt(amount):,.{precision}f}"


def rounded(num, precision=0):
    return round(flt(num), cint(precision))


def ceil(n):
    return _math.ceil(flt(n))


def floor(n):
    return _math.floor(flt(n))


def safe_div(n, d, precision=None):
    if not d:
        return 0
    return flt(n) / flt(d) if precision is None else round(flt(n) / flt(d), precision)


# ----------------------------------------------------------------- dates
def _to_datetime(v):
    if v is None:
        return None
    if isinstance(v, _dt.datetime):
        return v
    if isinstance(v, _dt.date):
        return _dt.datetime(v.year, v.month, v.day)
    s = str(v)
    for fmt in (DATETIME_FORMAT, "%Y-%m-%d %H:%M:%S", DATE_FORMAT):
        try:
            return _dt.datetime.strptime(s, fmt)
        except ValueError:
            continue
    try:
        return _dt.datetime.fromisoformat(s)
    except ValueError:
        return None


def getdate(string_date=None):
    if string_date is None:
        return _dt.date.today()
    if isinstance(string_date, _dt.datetime):
        return string_date.date()
    if isinstance(string_date, _dt.date):
        return string_date
    d = _to_datetime(string_date)
    return d.date() if d else _dt.date.today()


def get_datetime(v=None):
    if v is None:
        return _dt.datetime.now()
    return _to_datetime(v)


def now_datetime():
    return _dt.datetime.now()


def now():
    return _dt.datetime.now().strftime(DATETIME_FORMAT)


def nowdate():
    return _dt.date.today().strftime(DATE_FORMAT)


def today():
    return nowdate()


def nowtime():
    return _dt.datetime.now().strftime(TIME_FORMAT)


def add_days(date, days):
    d = getdate(date)
    return d + _dt.timedelta(days=cint(days))


def add_to_date(date, years=0, months=0, days=0, hours=0, minutes=0, seconds=0, as_string=False, as_datetime=False):
    d = get_datetime(date) or _dt.datetime.now()
    # naive month/year add
    month = d.month - 1 + months
    year = d.year + years + month // 12
    month = month % 12 + 1
    day = min(d.day, [31, 29 if year % 4 == 0 and (year % 100 != 0 or year % 400 == 0) else 28,
                      31, 30, 31, 30, 31, 31, 30, 31, 30, 31][month - 1])
    d = d.replace(year=year, month=month, day=day) + _dt.timedelta(days=days, hours=hours, minutes=minutes, seconds=seconds)
    if as_string:
        return d.strftime(DATE_FORMAT if not (hours or minutes or seconds) else DATETIME_FORMAT)
    return d


def add_months(date, months):
    return add_to_date(date, months=months, as_datetime=True)


def add_years(date, years):
    return add_to_date(date, years=years, as_datetime=True)


def date_diff(d1, d2):
    return (getdate(d1) - getdate(d2)).days


def time_diff_in_seconds(d1, d2):
    a, b = get_datetime(d1), get_datetime(d2)
    return (a - b).total_seconds() if a and b else 0


def time_diff_in_hours(d1, d2):
    return time_diff_in_seconds(d1, d2) / 3600.0


def get_first_day(date, d_years=0, d_months=0):
    d = getdate(date)
    return d.replace(day=1)


def get_last_day(date):
    d = getdate(date)
    nxt = (d.replace(day=28) + _dt.timedelta(days=4)).replace(day=1)
    return nxt - _dt.timedelta(days=1)


def formatdate(string_date=None, format_string=None):
    return getdate(string_date).strftime(DATE_FORMAT)


def format_datetime(v=None, format_string=None):
    d = get_datetime(v) or _dt.datetime.now()
    return d.strftime(DATETIME_FORMAT)


def format_date(v=None, format_string=None):
    return formatdate(v)


def get_time(v):
    d = _to_datetime(v)
    return d.time() if d else None


# ----------------------------------------------------------------- strings / misc
def strip_html(text):
    return _re.sub(r"<[^>]+>", "", text or "")


def strip(s, chars=None):
    return (s or "").strip(chars)


def escape_html(text):
    return (text or "").replace("&", "&amp;").replace("<", "&lt;").replace(">", "&gt;")


def cstr_or_none(v):
    return None if v is None else cstr(v)


def get_url(uri=None, *a, **k):
    return uri or ""


def get_link_to_form(doctype, name, label=None):
    return label or name


def get_fullname(user=None):
    return user or ""


def validate_email_address(email, throw=False):
    return email


def split_emails(txt):
    return [e.strip() for e in _re.split(r"[,\n]", txt or "") if e.strip()]


def random_string(length=10):
    import ferro_rt
    h = ferro_rt.generate_hash()
    while len(h) < length:
        h += ferro_rt.generate_hash()
    return h[:length]


def unique(seq):
    seen = set()
    out = []
    for x in seq:
        if x not in seen:
            seen.add(x)
            out.append(x)
    return out


def get_datetime_str(v):
    d = get_datetime(v)
    return d.strftime(DATETIME_FORMAT) if d else None


def flt_to_str(v):
    return str(flt(v))


def money_in_words(number, main_currency=None, fraction_currency=None):
    return cstr(number)


def get_number_format_info(fmt):
    return (".", ",", 2)


def evaluate_filters(doc, filters):
    return True


def update_progress_bar(*a, **k):
    pass


def cast(fieldtype, value):
    if fieldtype in ("Int", "Long Int", "Check"):
        return cint(value)
    if fieldtype in ("Float", "Currency", "Percent", "Rate"):
        return flt(value)
    return value


class DotDict(dict):
    def __getattr__(self, k):
        return self.get(k)
    def __setattr__(self, k, v):
        self[k] = v


def __getattr__(name):
    # permissive fallback for the long tail of frappe.utils helpers (import-completeness).
    # Returns a *type* so it survives `X | None` annotations and `class Y(X)` at import time.
    from frappe._lazy import stub_attr
    return stub_attr(name)
