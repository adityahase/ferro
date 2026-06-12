"""Minimal faithful `dateutil` for ferro's embedded interpreter.

The real python-dateutil isn't installed in the bare CPython ferro embeds. Without this, imports
like `from dateutil.relativedelta import relativedelta` fall through to the permissive Stub finder,
which silently breaks DATE ARITHMETIC (e.g. `date + relativedelta(years=1)` returns a Stub, so
ERPNext's Fiscal Year span check / period-end / due-date math all go wrong). This package provides
correct implementations of the subset Frappe + ERPNext actually use (relativedelta, parser).
"""
__all__ = ["relativedelta", "parser"]
