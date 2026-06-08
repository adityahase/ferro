#!/usr/bin/env python3
"""Find the exact source site that first triggers each heavy import during
`import frappe` + `import frappe.app`. For every target top-level package, record
the first non-importlib caller frame (file:line) so the eager import can be located
and deferred. cwd=sites/, bench env python."""
import os
import sys
import builtins

TARGETS = {"num2words", "whoosh", "oauthlib", "pypdf", "cssutils", "rq",
           "pydantic", "PIL", "bs4", "requests", "pymysql", "sqlparse",
           "babel", "croniter", "redis", "html5lib", "chardet", "lxml",
           "premailer", "openpyxl", "pandas", "markdown2", "docutils",
           "psutil", "click", "jinja2", "werkzeug", "cryptography"}

first_import = {}


def find_importer():
    import traceback
    for fr in reversed(traceback.extract_stack()):
        f = fr.filename
        if ("importlib" in f or "import_tracer" in f or "<frozen" in f
                or f.endswith("ast.py")):
            continue
        return f"{f.rsplit('/apps/',1)[-1].rsplit('site-packages/',1)[-1]}:{fr.lineno}"
    return "?"


_real_import = builtins.__import__


def traced_import(name, globals=None, locals=None, fromlist=(), level=0):
    top = name.split(".")[0]
    if top in TARGETS and top not in first_import:
        first_import[top] = find_importer()
    return _real_import(name, globals, locals, fromlist, level)


builtins.__import__ = traced_import

import frappe          # noqa: E402
import frappe.app      # noqa: E402

builtins.__import__ = _real_import

print(f"# modules loaded after import frappe+app: {len(sys.modules)}")
print(f"{'PACKAGE':20s}  FIRST IMPORTED BY")
for pkg in sorted(first_import):
    print(f"{pkg:20s}  {first_import[pkg]}")
print("\n# targets NOT imported at frappe-import time (already lazy):")
print("  " + ", ".join(sorted(TARGETS - set(first_import))))
