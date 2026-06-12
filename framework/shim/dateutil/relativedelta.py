"""A correct, minimal `dateutil.relativedelta` — the subset Frappe/ERPNext rely on.

Implements the relative fields (years/months/weeks/days/hours/minutes/seconds/microseconds +
leapdays), the absolute fields (year/month/day/hour/minute/second/microsecond) and weekday jumps
(MO..SU, with an optional nth like MO(2)). Arithmetic mirrors python-dateutil: absolute fields are
applied first, then year/month with month-end clamping, then the weekday jump, then the relative
timedelta. This is what makes `date + relativedelta(years=1) - relativedelta(days=1)` land on the
right day.
"""
import calendar
import datetime

__all__ = ["relativedelta", "MO", "TU", "WE", "TH", "FR", "SA", "SU", "weekday", "weekdays"]


class weekday:
    __slots__ = ["weekday", "n"]

    def __init__(self, wkday, n=None):
        self.weekday = wkday
        self.n = n

    def __call__(self, n):
        return self if n == self.n else self.__class__(self.weekday, n)

    def __eq__(self, other):
        try:
            return self.weekday == other.weekday and self.n == other.n
        except AttributeError:
            return False

    def __hash__(self):
        return hash((self.weekday, self.n))

    def __repr__(self):
        s = ("MO", "TU", "WE", "TH", "FR", "SA", "SU")[self.weekday]
        return s if not self.n else "%s(%+d)" % (s, self.n)


MO, TU, WE, TH, FR, SA, SU = weekdays = tuple(weekday(i) for i in range(7))


class relativedelta:
    def __init__(self, dt1=None, dt2=None,
                 years=0, months=0, days=0, leapdays=0, weeks=0,
                 hours=0, minutes=0, seconds=0, microseconds=0,
                 year=None, month=None, day=None, weekday=None,
                 yearday=None, nlyearday=None,
                 hour=None, minute=None, second=None, microsecond=None):
        if dt1 is not None and dt2 is not None:
            # Difference between two dates. Frappe rarely uses this form; compute a normalized
            # (years, months, days) + time delta so `relativedelta(a, b)` reads back sensibly.
            if not (isinstance(dt1, datetime.date) and isinstance(dt2, datetime.date)):
                raise TypeError("relativedelta only diffs date/datetime")
            if isinstance(dt1, datetime.datetime) or isinstance(dt2, datetime.datetime):
                d1 = dt1 if isinstance(dt1, datetime.datetime) else datetime.datetime.fromordinal(dt1.toordinal())
                d2 = dt2 if isinstance(dt2, datetime.datetime) else datetime.datetime.fromordinal(dt2.toordinal())
            else:
                d1, d2 = dt1, dt2
            months_total = (d1.year - d2.year) * 12 + (d1.month - d2.month)
            self._set_zero()
            self.months = months_total
            self._normalize_months()
            anchor = self.__radd__(d2)
            if (months_total > 0 and anchor > d1) or (months_total < 0 and anchor < d1):
                self.months += (-1 if months_total > 0 else 1)
                self._normalize_months()
                anchor = self.__radd__(d2)
            delta = d1 - anchor
            self.days = delta.days
            self.seconds = delta.seconds
            self.microseconds = delta.microseconds
            self.leapdays = 0
            self.year = self.month = self.day = None
            self.hour = self.minute = self.second = self.microsecond = None
            self.weekday = None
            self.hours = self.minutes = 0
        else:
            self.years = years
            self.months = months
            self.days = days + weeks * 7
            self.leapdays = leapdays
            self.hours = hours
            self.minutes = minutes
            self.seconds = seconds
            self.microseconds = microseconds
            self.year = year
            self.month = month
            self.day = day
            self.hour = hour
            self.minute = minute
            self.second = second
            self.microsecond = microsecond
            if weekday is None:
                self.weekday = None
            elif isinstance(weekday, int):
                self.weekday = weekdays[weekday]
            else:
                self.weekday = weekday
            self._normalize_months()

    def _set_zero(self):
        self.years = self.months = self.days = self.leapdays = 0
        self.hours = self.minutes = self.seconds = self.microseconds = 0

    def _normalize_months(self):
        m = self.months
        if abs(m) > 11:
            s = -1 if m < 0 else 1
            div, mod = divmod(m * s, 12)
            self.months = mod * s
            self.years += div * s

    @property
    def weeks(self):
        return int(self.days / 7.0)

    def __radd__(self, other):
        if not isinstance(other, datetime.date):
            return NotImplemented
        year = (self.year if self.year is not None else other.year) + self.years
        month = self.month if self.month is not None else other.month
        if self.months:
            month += self.months
            if month > 12:
                year += 1
                month -= 12
            elif month < 1:
                year -= 1
                month += 12
        day = min(calendar.monthrange(year, month)[1],
                  self.day if self.day is not None else other.day)
        repl = {"year": year, "month": month, "day": day}
        if isinstance(other, datetime.datetime):
            for attr in ("hour", "minute", "second", "microsecond"):
                v = getattr(self, attr)
                if v is not None:
                    repl[attr] = v
        ret = other.replace(**repl)

        if self.weekday:
            wday = self.weekday.weekday
            nth = self.weekday.n or 1
            jumpdays = (abs(nth) - 1) * 7
            if nth > 0:
                jumpdays += (7 - ret.weekday() + wday) % 7
            else:
                jumpdays += -((ret.weekday() - wday) % 7)
            ret = ret + datetime.timedelta(days=jumpdays)

        extra_days = self.days
        if self.leapdays and month > 2 and calendar.isleap(year):
            extra_days += self.leapdays
        ret = ret + datetime.timedelta(days=extra_days, hours=self.hours,
                                       minutes=self.minutes, seconds=self.seconds,
                                       microseconds=self.microseconds)
        return ret

    __add__ = __radd__

    def __rsub__(self, other):
        return self.__neg__().__radd__(other)

    def __neg__(self):
        return relativedelta(years=-self.years, months=-self.months, days=-self.days,
                             hours=-self.hours, minutes=-self.minutes, seconds=-self.seconds,
                             microseconds=-self.microseconds, leapdays=self.leapdays,
                             year=self.year, month=self.month, day=self.day,
                             weekday=self.weekday, hour=self.hour, minute=self.minute,
                             second=self.second, microsecond=self.microsecond)

    def __sub__(self, other):
        if not isinstance(other, relativedelta):
            return NotImplemented
        return relativedelta(years=self.years - other.years, months=self.months - other.months,
                             days=self.days - other.days, hours=self.hours - other.hours,
                             minutes=self.minutes - other.minutes, seconds=self.seconds - other.seconds,
                             microseconds=self.microseconds - other.microseconds,
                             leapdays=self.leapdays or other.leapdays,
                             year=self.year, month=self.month, day=self.day,
                             weekday=self.weekday, hour=self.hour, minute=self.minute,
                             second=self.second, microsecond=self.microsecond)

    def __add__rd(self, other):  # pragma: no cover - placeholder to keep name table tidy
        return NotImplemented

    def __bool__(self):
        return bool(self.years or self.months or self.days or self.hours or self.minutes
                    or self.seconds or self.microseconds or self.leapdays
                    or self.year is not None or self.month is not None or self.day is not None
                    or self.hour is not None or self.minute is not None or self.second is not None
                    or self.microsecond is not None or self.weekday is not None)

    def __repr__(self):
        attrs = []
        for k in ("years", "months", "days", "leapdays", "hours", "minutes", "seconds",
                  "microseconds", "year", "month", "day", "weekday", "hour", "minute",
                  "second", "microsecond"):
            v = getattr(self, k)
            if v:
                attrs.append("%s=%s" % (k, repr(v)))
        return "relativedelta(%s)" % ", ".join(attrs)
