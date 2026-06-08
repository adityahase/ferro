#!/usr/bin/env python3
"""
characterize.py — Data-driven transpilability map of the 5 Frappe apps' DocType controllers.

For every controller method we walk the AST and decide whether it falls within a *transpilable
subset* (a whitelist of AST node kinds + a whitelist of call targets). A method is "GREEN"
(transpilable to Rust today) iff every node in its body is in the whitelist. Otherwise it is
"RED" and we record the specific blockers (unsupported node kinds / call targets), ranked, so we
know exactly what to add next to raise coverage.

We also classify each DocType controller:
  - PURE_CRUD   : no lifecycle/business methods at all (`pass`, only type annotations) -> ferro
                  already serves these with ZERO code.
  - FULL_GREEN  : has methods, and ALL of them are transpilable.
  - PARTIAL     : some methods green, some red.
  - RED         : has methods, none transpilable.

Output: JSON to stdout + a human summary to stderr.
"""
import ast, os, sys, json, collections, glob

# Bring-your-own app clones; see transpile.py. Default mirrors that path.
_REPO_ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
REPOS = os.environ.get(
    "FERRO_REPOS", os.path.join(_REPO_ROOT, "docs", "investigations", "apps-64mb", "repos"))
APPS = ["crm", "helpdesk", "gameplan", "hrms", "erpnext"]

# ---- the transpilable subset -------------------------------------------------
# AST node kinds we can lower to Rust. Anything outside this set makes a method RED.
ALLOWED_NODES = {
    # structure
    "Module", "FunctionDef", "arguments", "arg", "Pass", "Return", "Expr",
    "keyword",                       # named call args: frappe.get_all(..., filters={...})
    "Import", "ImportFrom", "alias", # in-function lazy imports (declare names, skipped in lowering)
    # control flow
    "If", "For", "Break", "Continue",
    # statements
    "Assign", "AugAssign", "AnnAssign",
    # expressions
    "Call", "Attribute", "Name", "Constant", "BinOp", "BoolOp", "UnaryOp",
    "Compare", "IfExp", "Subscript",
    # comprehensions (lower to Rust iterator chains / loops)
    "ListComp", "SetComp", "GeneratorExp", "DictComp", "comprehension",
    # operands / ops
    "Load", "Store", "Add", "Sub", "Mult", "Div", "Mod", "FloorDiv", "Pow",
    "Lt", "LtE", "Gt", "GtE", "Eq", "NotEq", "In", "NotIn", "Is", "IsNot",
    "And", "Or", "Not", "USub", "UAdd",
    # literals / small containers (string-keyed dict + list of scalars only; checked separately)
    "List", "Tuple", "Dict", "JoinedStr", "FormattedValue", "Index", "Slice", "Starred",
}

# Call targets we know how to lower. Keyed by the dotted spelling we reconstruct.
ALLOWED_CALL_PREFIXES = (
    "self.",          # self.method(), self.get/set/append/get_all_children, self.db_set, ...
    "frappe.db.get_value", "frappe.db.set_value", "frappe.db.exists", "frappe.db.count",
    "frappe.db.get_single_value", "frappe.db.set_single_value", "frappe.db.get_all",
    "frappe.db.get_list", "frappe.db.delete", "frappe.db.commit", "frappe.db.escape",
    "frappe.get_value", "frappe.get_all", "frappe.get_list", "frappe.db.get_values",
    "frappe.throw", "frappe.msgprint", "frappe.bold", "frappe._",
    "frappe.get_cached_value", "frappe.get_doc", "frappe.new_doc", "frappe.get_cached_doc",
    "frappe.delete_doc", "frappe.rename_doc", "frappe.get_meta", "frappe.scrub",
    "frappe.utils.flt", "frappe.utils.cint", "frappe.utils.cstr", "frappe.utils.getdate",
)
# bare-name calls (after `from frappe.utils import ...` / `from frappe import _`)
ALLOWED_BARE_CALLS = {
    "flt", "cint", "cstr", "_", "len", "range", "abs", "min", "max", "round", "str",
    "int", "float", "bool", "sum", "getdate", "nowdate", "now_datetime", "today",
    "add_days", "add_months", "date_diff", "fmt_money", "format", "enumerate", "sorted",
    "get_link_to_form", "get_datetime", "add_to_date", "formatdate", "now", "nowtime",
    "time_diff_in_hours", "time_diff_in_seconds", "money_in_words", "get_url_to_form",
    "get_fullname", "scrub", "unscrub", "list", "dict", "set", "any", "all", "frappe._dict",
    "fmt_money", "rounded", "ceil", "floor", "cint", "comma_or", "comma_and",
}
# method calls on values (x.method(...)) we can lower
ALLOWED_VALUE_METHODS = {
    "format", "append", "get", "set", "precision", "strip", "lower", "upper",
    "startswith", "endswith", "split", "join", "replace", "insert", "save", "db_set",
    "get_all_children", "as_dict", "is_new", "has_value_changed", "setdefault",
    "get_value_before_save", "get_doc_before_save", "run_method", "reload", "items", "keys", "values",
}


def dotted(node):
    """Reconstruct a dotted name from an Attribute/Name chain; None if not a plain chain."""
    parts = []
    while isinstance(node, ast.Attribute):
        parts.append(node.attr)
        node = node.value
    if isinstance(node, ast.Name):
        parts.append(node.id)
        return ".".join(reversed(parts))
    return None


class MethodScanner(ast.NodeVisitor):
    def __init__(self):
        self.blockers = collections.Counter()

    def block(self, key):
        self.blockers[key] += 1

    def visit_Call(self, node):
        # check the call target
        target = dotted(node.func)
        ok = False
        if target:
            if target.startswith("self."):
                ok = True
            elif any(target == p or target.startswith(p) for p in ALLOWED_CALL_PREFIXES):
                ok = True
            elif "." not in target and target in ALLOWED_BARE_CALLS:
                ok = True
            elif "." in target:
                # value method like x.method  -> last component must be allowed
                last = target.rsplit(".", 1)[1]
                if last in ALLOWED_VALUE_METHODS:
                    ok = True
        else:
            # func is an Attribute on a non-plain expr, e.g. self.foo().bar() or _("x").format()
            if isinstance(node.func, ast.Attribute) and node.func.attr in ALLOWED_VALUE_METHODS:
                ok = True
        if not ok:
            name = target or (node.func.attr if isinstance(node.func, ast.Attribute) else "<dynamic>")
            self.block(f"call:{name}")
        # keyword args with ** are hard
        for kw in node.keywords:
            if kw.arg is None:
                self.block("call:**kwargs")
        self.generic_visit(node)

    def visit_Dict(self, node):
        # only string-keyed dict literals are lowerable to a struct/map
        for k in node.keys:
            if not (isinstance(k, ast.Constant) and isinstance(k.value, str)):
                self.block("dict:non-str-key")
                break
        self.generic_visit(node)

    def generic_visit(self, node):
        kind = type(node).__name__
        if kind not in ALLOWED_NODES:
            self.block(f"node:{kind}")
        super().generic_visit(node)


def scan_method(fn):
    s = MethodScanner()
    for stmt in fn.body:
        s.visit(stmt)
    return s.blockers


LIFECYCLE = {
    "validate", "before_save", "before_insert", "after_insert", "on_update",
    "before_submit", "on_submit", "before_cancel", "on_cancel", "on_trash",
    "after_delete", "on_change", "before_validate", "before_update_after_submit",
    "on_update_after_submit", "autoname", "before_naming",
}


def is_document_subclass(cls):
    for b in cls.bases:
        d = dotted(b) or (b.id if isinstance(b, ast.Name) else "")
        if d and ("Document" in d or "Controller" in d or "WebsiteGenerator" in d):
            return True
    return False


def analyze():
    apps = collections.OrderedDict()
    global_blockers = collections.Counter()
    green_methods_by_name = collections.Counter()
    red_methods_by_name = collections.Counter()

    for app in APPS:
        root = os.path.join(REPOS, app)
        stats = dict(doctypes=0, pure_crud=0, full_green=0, partial=0, red=0,
                     methods=0, methods_green=0, methods_red=0,
                     lifecycle=0, lifecycle_green=0, whitelist=0, whitelist_green=0,
                     parse_errors=0)
        for path in glob.glob(os.path.join(root, "**", "doctype", "**", "*.py"), recursive=True):
            base = os.path.basename(path)
            if base == "__init__.py" or base.startswith("test_"):
                continue
            # controller file lives at doctype/<dt>/<dt>.py
            parent = os.path.basename(os.path.dirname(path))
            if base[:-3] != parent:
                continue
            try:
                tree = ast.parse(open(path, encoding="utf-8").read())
            except Exception:
                stats["parse_errors"] += 1
                continue
            classes = [n for n in tree.body if isinstance(n, ast.ClassDef) and is_document_subclass(n)]
            if not classes:
                continue
            stats["doctypes"] += 1
            cls = classes[0]
            methods = [n for n in cls.body if isinstance(n, ast.FunctionDef)]
            # strip the auto-generated TYPE_CHECKING block (it's inside an `if TYPE_CHECKING`)
            real_methods = [m for m in methods if m.name != "__init__" or m.body]
            if not real_methods:
                stats["pure_crud"] += 1
                continue
            ngreen = nred = 0
            for m in real_methods:
                stats["methods"] += 1
                blk = scan_method(m)
                is_lc = m.name in LIFECYCLE
                if is_lc:
                    stats["lifecycle"] += 1
                if not blk:
                    ngreen += 1
                    stats["methods_green"] += 1
                    green_methods_by_name[m.name] += 1
                    if is_lc:
                        stats["lifecycle_green"] += 1
                else:
                    nred += 1
                    stats["methods_red"] += 1
                    red_methods_by_name[m.name] += 1
                    global_blockers.update(blk)
            if nred == 0:
                stats["full_green"] += 1
            elif ngreen == 0:
                stats["red"] += 1
            else:
                stats["partial"] += 1
        apps[app] = stats

    # totals
    tot = collections.Counter()
    for s in apps.values():
        for k, v in s.items():
            tot[k] += v
    return {
        "apps": apps,
        "total": dict(tot),
        "top_blockers": global_blockers.most_common(40),
        "green_method_names": green_methods_by_name.most_common(25),
        "red_method_names": red_methods_by_name.most_common(25),
    }


if __name__ == "__main__":
    out = analyze()
    print(json.dumps(out, indent=2))
    t = out["total"]
    dt = t["doctypes"]
    crud_or_green = t["pure_crud"] + t["full_green"]
    sys.stderr.write("\n=== TRANSPILABILITY SUMMARY ===\n")
    sys.stderr.write(f"doctype controllers analyzed : {dt}\n")
    sys.stderr.write(f"  pure-CRUD (zero code)      : {t['pure_crud']}  ({100*t['pure_crud']/dt:.0f}%)\n")
    sys.stderr.write(f"  full-green (all methods OK): {t['full_green']}\n")
    sys.stderr.write(f"  partial (some methods OK)  : {t['partial']}\n")
    sys.stderr.write(f"  red (no methods OK)        : {t['red']}\n")
    sys.stderr.write(f"  => fully native (crud+green): {crud_or_green}  ({100*crud_or_green/dt:.0f}%)\n")
    sys.stderr.write(f"methods total                : {t['methods']}\n")
    sys.stderr.write(f"  green                      : {t['methods_green']}  ({100*t['methods_green']/max(1,t['methods']):.0f}%)\n")
    sys.stderr.write(f"  red                        : {t['methods_red']}\n")
    sys.stderr.write(f"lifecycle methods            : {t['lifecycle']}  green {t['lifecycle_green']}  ({100*t['lifecycle_green']/max(1,t['lifecycle']):.0f}%)\n")
    sys.stderr.write("\nTop blockers (what to add next):\n")
    for k, v in out["top_blockers"][:25]:
        sys.stderr.write(f"  {v:5d}  {k}\n")
