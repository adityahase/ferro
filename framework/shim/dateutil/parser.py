"""Minimal `dateutil.parser` — `parse()` + `ParserError`, the bits Frappe/ERPNext use.

Real python-dateutil's parser is a large heuristic engine; Frappe mostly feeds it ISO-ish strings
(`YYYY-MM-DD[ HH:MM:SS]`) and a few common formats, and catches ParserError on junk. This handles
those deterministically and raises ParserError otherwise (so callers' try/except behave the same).
"""
import datetime

__all__ = ["parse", "ParserError", "ParserWarning"]


class ParserError(ValueError):
    pass


class ParserWarning(UserWarning):
    pass


_FORMATS = (
    "%Y-%m-%d %H:%M:%S.%f",
    "%Y-%m-%d %H:%M:%S",
    "%Y-%m-%d %H:%M",
    "%Y-%m-%d",
    "%Y/%m/%d %H:%M:%S",
    "%Y/%m/%d",
    "%d-%m-%Y %H:%M:%S",
    "%d-%m-%Y",
    "%d/%m/%Y %H:%M:%S",
    "%d/%m/%Y",
    "%m/%d/%Y %H:%M:%S",
    "%m/%d/%Y",
    "%H:%M:%S",
    "%H:%M",
)


def parse(timestr, default=None, dayfirst=False, yearfirst=False, fuzzy=False, **kwargs):
    if isinstance(timestr, (datetime.datetime,)):
        return timestr
    if isinstance(timestr, datetime.date):
        return datetime.datetime.fromordinal(timestr.toordinal())
    if not isinstance(timestr, str):
        raise ParserError("Cannot parse non-string value %r" % (timestr,))
    s = timestr.strip()
    if not s:
        raise ParserError("String does not contain a date: %r" % (timestr,))
    # ISO 8601 fast path (handles fractional seconds, 'T', offsets on 3.11+).
    try:
        return datetime.datetime.fromisoformat(s.replace("Z", "+00:00"))
    except ValueError:
        pass
    for fmt in _FORMATS:
        try:
            dt = datetime.datetime.strptime(s, fmt)
            if default is not None and fmt in ("%H:%M:%S", "%H:%M"):
                dt = dt.replace(year=default.year, month=default.month, day=default.day)
            return dt
        except ValueError:
            continue
    raise ParserError("Unknown string format: %s" % (timestr,))


# dateutil exposes a default parser instance with .parse; a couple of callers use parser.parser().
class parser:
    def parse(self, timestr, **kwargs):
        return parse(timestr, **kwargs)
