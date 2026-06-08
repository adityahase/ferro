#!/usr/bin/env python3
"""
transpile.py — Python → Rust transpiler for Frappe DocType controllers.

Lowers each controller method to a Rust function operating on the dynamic `Value` model in
`rt.rs`. Every Python expression becomes an owned `serde_json::Value`; every operator becomes an
`rt::*` call — so there is no static type-inference problem and the output is uniform.

A method is emitted only if (a) the characterizer's whitelist accepts every node AND (b) lowering
succeeds with no `Unsupported` raised AND (c) every `self.` method it transitively calls is also
emittable. Output: ferro/src/native/generated.rs (the dispatch registry + the transpiled fns).

Usage: python3 transpile.py [--out PATH] [--apps a,b,...]
"""
import ast, os, sys, json, glob, collections, re

# Paths are derived from this script's location so the transpiler works wherever the
# monorepo is checked out. transpile/ sits at the repo root, so REPO_ROOT is one up.
REPO_ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
# The Frappe app clones are an external, bring-your-own input (not vendored). Clone the
# apps into docs/investigations/apps-64mb/repos/ (gitignored), or point $FERRO_REPOS elsewhere.
REPOS = os.environ.get(
    "FERRO_REPOS", os.path.join(REPO_ROOT, "docs", "investigations", "apps-64mb", "repos"))
OUT = os.path.join(REPO_ROOT, "src", "native", "generated.rs")
APPS = ["crm", "helpdesk", "gameplan", "hrms", "erpnext"]

LIFECYCLE = {
    "validate", "before_save", "before_insert", "after_insert", "on_update", "before_submit",
    "on_submit", "before_cancel", "on_cancel", "on_trash", "after_delete", "on_change",
    "before_validate",
}


class Unsupported(Exception):
    pass


RUST_RESERVED = {
    "as", "break", "const", "continue", "crate", "dyn", "else", "enum", "extern", "false", "fn",
    "for", "if", "impl", "in", "let", "loop", "match", "mod", "move", "mut", "pub", "ref",
    "return", "self", "static", "struct", "super", "trait", "true", "type", "unsafe", "use",
    "where", "while", "async", "await", "box", "do", "final", "macro", "override", "priv",
    "typeof", "unsized", "virtual", "yield", "try", "abstract", "become", "union", "gen",
}


def mangle(name):
    """Python local -> a valid, non-colliding Rust identifier."""
    if name in ("doc", "ctx") or name in RUST_RESERVED:
        return "v_" + name
    if name.startswith("__"):
        return "p" + name  # keep clear of the transpiler's own __t/__i temporaries
    return name


def rstr(s: str) -> str:
    """Python str -> Rust string literal (UTF-8 source, manual escapes)."""
    out = ['"']
    for ch in s:
        if ch == "\\":
            out.append("\\\\")
        elif ch == '"':
            out.append('\\"')
        elif ch == "\n":
            out.append("\\n")
        elif ch == "\r":
            out.append("\\r")
        elif ch == "\t":
            out.append("\\t")
        elif ord(ch) < 0x20:
            out.append("\\u{%x}" % ord(ch))
        else:
            out.append(ch)
    out.append('"')
    return "".join(out)


def dotted(node):
    parts = []
    while isinstance(node, ast.Attribute):
        parts.append(node.attr)
        node = node.value
    if isinstance(node, ast.Name):
        parts.append(node.id)
        return ".".join(reversed(parts))
    return None


def is_document_subclass(cls):
    for b in cls.bases:
        d = dotted(b) or (b.id if isinstance(b, ast.Name) else "")
        if d and ("Document" in d or "Controller" in d or "WebsiteGenerator" in d):
            return True
    return False


# ---------------------------------------------------------------- the lowerer
class Lowerer:
    def __init__(self, methods_by_name, fnprefix):
        self.methods = methods_by_name      # name -> ast.FunctionDef (sibling methods)
        self.prefix = fnprefix              # e.g. "c12"
        self.locals = set()                 # declared local Value vars
        self.cursors = {}                   # name -> (table_expr_literal, idx_var) for child loops
        self.tmp = 0
        self.calls_self = set()             # self-methods this method calls

    def newtmp(self, base="__t"):
        self.tmp += 1
        return f"{base}{self.tmp}"

    # ---- statements -> list of Rust lines
    def lower_body(self, body, indent):
        lines = []
        for stmt in body:
            lines += self.lower_stmt(stmt, indent)
        return lines

    def lower_stmt(self, node, ind):
        pad = "    " * ind
        if isinstance(node, ast.Pass):
            return []
        if isinstance(node, (ast.Import, ast.ImportFrom)):
            return []  # in-function lazy imports: names are resolved structurally
        if isinstance(node, ast.Expr):
            # bare expression statement
            if isinstance(node.value, ast.Call):
                tgt = dotted(node.value.func)
                if tgt in ("frappe.throw", "throw"):
                    msg = self.throw_msg(node.value)
                    return [f"{pad}return Err(rt::throw(&({msg})));"]
            e = self.lower_expr(node.value)
            return [f"{pad}let _ = {e};"]
        if isinstance(node, ast.Return):
            if node.value is None:
                return [f"{pad}return Ok(Value::Null);"]
            return [f"{pad}return Ok({self.lower_expr(node.value)});"]
        if isinstance(node, ast.Assign):
            if len(node.targets) == 1:
                return self.lower_assign(node.targets[0], node.value, pad)
            # a = b = expr  -> stash to a temp, assign each target from it
            tmp = self.newtmp("__ma")
            out = [f"{pad}let {tmp} = {self.lower_expr(node.value)};"]
            for t in node.targets:
                out += self.assign_target_str(t, f"{tmp}.clone()", pad)
            return out
        if isinstance(node, ast.AnnAssign):
            if node.value is None:
                return []  # type-only annotation
            return self.lower_assign(node.target, node.value, pad)
        if isinstance(node, ast.AugAssign):
            return self.lower_augassign(node, pad)
        if isinstance(node, ast.If):
            cond = self.lower_expr(node.test)
            out = [f"{pad}if rt::truthy(&({cond})) {{"]
            out += self.lower_body(node.body, ind + 1)
            if node.orelse:
                # else / elif
                if len(node.orelse) == 1 and isinstance(node.orelse[0], ast.If):
                    inner = self.lower_stmt(node.orelse[0], ind)
                    # splice: replace leading "if" with "} else if"
                    out.append(f"{pad}}} else {inner[0].strip()}")
                    out += inner[1:]
                    return out
                out.append(f"{pad}}} else {{")
                out += self.lower_body(node.orelse, ind + 1)
            out.append(f"{pad}}}")
            return out
        if isinstance(node, ast.For):
            return self.lower_for(node, ind)
        if isinstance(node, ast.Break):
            return [f"{pad}break;"]
        if isinstance(node, ast.Continue):
            return [f"{pad}continue;"]
        raise Unsupported(f"stmt {type(node).__name__}")

    def lower_assign(self, target, valuenode, pad):
        val = self.lower_expr(valuenode)
        if isinstance(target, ast.Name):
            nm = target.id
            self.locals.add(nm)
            return [f"{pad}{mangle(nm)} = {val};"]  # all locals hoisted to fn scope
        if isinstance(target, ast.Attribute):
            return [f"{pad}{self.lower_attr_set(target, val)};"]
        if isinstance(target, ast.Subscript):
            base = self.lower_expr(target.value)
            key = self.lower_expr(target.slice)
            return [f"{pad}rt::set_item(&mut ({base_as_place(target.value)}), &({key}), {val});" ] if False else self._subscript_set(target, val, pad)
        if isinstance(target, (ast.Tuple, ast.List)):
            # a, b = <tuple literal>
            if isinstance(valuenode, (ast.Tuple, ast.List)) and len(valuenode.elts) == len(target.elts):
                out = []
                for t, v in zip(target.elts, valuenode.elts):
                    out += self.lower_assign(t, v, pad)
                return out
            # a, b = <expr returning a list>  -> temp + index
            if all(isinstance(t, ast.Name) for t in target.elts):
                tmp = self.newtmp("__tup")
                out = [f"{pad}let {tmp} = {val};"]
                for i, t in enumerate(target.elts):
                    self.locals.add(t.id)
                    out.append(f"{pad}{mangle(t.id)} = rt::subscript(&{tmp}, &Value::from({i}i64));")
                return out
            raise Unsupported("tuple unpack shape")
        raise Unsupported(f"assign target {type(target).__name__}")

    def assign_target_str(self, target, valstr, pad):
        """Assign an already-lowered Rust value string to a target (Name/Attribute/Subscript)."""
        if isinstance(target, ast.Name):
            nm = target.id
            self.locals.add(nm)
            return [f"{pad}{mangle(nm)} = {valstr};"]
        if isinstance(target, ast.Attribute):
            return [f"{pad}{self.lower_attr_set(target, valstr)};"]
        if isinstance(target, ast.Subscript):
            return self._subscript_set(target, valstr, pad)
        raise Unsupported("multi-assign target shape")

    def _subscript_set(self, target, val, pad):
        # only support self.<field>[k] = v  and  local[k] = v  -> rebuild via rt::set_item on a place
        if isinstance(target.value, ast.Attribute) and self.is_self(target.value.value):
            field = target.value.attr
            key = self.lower_expr(target.slice)
            return [f"{pad}{{ let mut __m = rt::doc_get(doc, {rstr(field)}); rt::set_item(&mut __m, &({key}), {val}); rt::doc_set(doc, {rstr(field)}, __m); }}"]
        if isinstance(target.value, ast.Name) and target.value.id in self.locals:
            key = self.lower_expr(target.slice)
            d, (k, v) = self.with_temps([key, val])
            return [f"{pad}{{ {d} rt::set_item(&mut {mangle(target.value.id)}, &{k}, {v}); }}"]
        raise Unsupported("subscript assign target")

    def lower_augassign(self, node, pad):
        op = self.binop_fn(node.op)
        if isinstance(node.target, ast.Name):
            nm = node.target.id
            if nm not in self.locals:
                raise Unsupported("augassign to undeclared local")
            rhs = self.lower_expr(node.value)
            mn = mangle(nm)
            return [f"{pad}{mn} = rt::{op}(&({mn}.clone()), &({rhs}));"]
        if isinstance(node.target, ast.Attribute):
            cur = self.lower_attr_get(node.target)
            rhs = self.lower_expr(node.value)
            newv = f"rt::{op}(&({cur}), &({rhs}))"
            return [f"{pad}{self.lower_attr_set(node.target, newv)};"]
        raise Unsupported("augassign target")

    def lower_for(self, node, ind):
        pad = "    " * ind
        tgt = node.target
        it = node.iter
        # enumerate(self.<table>) / enumerate(X)
        enum = isinstance(it, ast.Call) and (dotted(it.func) == "enumerate")
        src = it.args[0] if enum else it
        # child-table cursor when iterating self.<attr> or self.get("literal")
        table = self.child_table_name(src)
        idx = self.newtmp("__i")
        out = []
        if table is not None:
            out.append(f'{pad}for {idx} in 0..rt::child_len(doc, {rstr(table)}) {{')
            inner = ind + 1
            ipad = "    " * inner
            # bind targets
            if enum and isinstance(tgt, ast.Tuple) and len(tgt.elts) == 2:
                ivar = tgt.elts[0].id
                rowvar = tgt.elts[1].id
                self.locals.add(ivar)
                out.append(f"{ipad}{mangle(ivar)} = Value::from({idx} as i64);")
                self.cursors[rowvar] = (table, idx)
            elif isinstance(tgt, ast.Name):
                self.cursors[tgt.id] = (table, idx)
            else:
                raise Unsupported("for target shape")
            out += self.lower_body(node.body, inner)
            out.append(f"{pad}}}")
            # cursor scope ends
            for k in list(self.cursors):
                if self.cursors[k][1] == idx:
                    del self.cursors[k]
            return out
        # general iterable
        listv = self.lower_expr(src)
        item = self.newtmp("__item")
        out.append(f"{pad}for {item} in rt::as_list(&({listv})) {{")
        inner = ind + 1
        ipad = "    " * inner
        if enum and isinstance(tgt, ast.Tuple) and len(tgt.elts) == 2 and all(isinstance(e, ast.Name) for e in tgt.elts):
            # enumerate over a general list: index isn't tracked here, fall back to item[0]/item[1]
            raise Unsupported("enumerate over general iterable")
        elif isinstance(tgt, ast.Name):
            self.locals.add(tgt.id)
            out.append(f"{ipad}{mangle(tgt.id)} = {item};")
        elif isinstance(tgt, (ast.Tuple, ast.List)) and all(isinstance(e, ast.Name) for e in tgt.elts):
            # for a, b in pairs:  -> destructure each item by index
            for i, e in enumerate(tgt.elts):
                self.locals.add(e.id)
                out.append(f"{ipad}{mangle(e.id)} = rt::subscript(&{item}, &Value::from({i}i64));")
        else:
            raise Unsupported("for target shape (general)")
        out += self.lower_body(node.body, inner)
        out.append(f"{pad}}}")
        return out

    def child_table_name(self, node):
        # self.<attr>  -> "<attr>"   ;  self.get("literal") -> "literal"
        if isinstance(node, ast.Attribute) and self.is_self(node.value):
            return node.attr
        if (isinstance(node, ast.Call) and isinstance(node.func, ast.Attribute)
                and node.func.attr == "get" and self.is_self(node.func.value)
                and len(node.args) == 1 and isinstance(node.args[0], ast.Constant)
                and isinstance(node.args[0].value, str)):
            return node.args[0].value
        return None

    def is_self(self, node):
        return isinstance(node, ast.Name) and node.id == "self"

    def lower_module_value(self, d):
        # frappe.* / app.* dotted name used as a *value* (not a call target)
        if d in ("frappe.session.user", "frappe.session.user_email"):
            return "rt::s(&ctx.user)"
        if d.startswith("frappe.session"):
            return "rt::s(&ctx.user)"
        if d.startswith("frappe.flags") or d.startswith("frappe.local") or d.startswith("frappe.request"):
            return "Value::Null"  # flags/locals default to falsy on a minimal deployment
        if d == "frappe.utils.nowdate":
            return "rt::today()"
        raise Unsupported(f"module value {d}")

    # ---- attribute get/set
    def lower_attr_get(self, node):
        # node is ast.Attribute
        base = node.value
        attr = node.attr
        if self.is_self(base):
            return f"rt::doc_get(doc, {rstr(attr)})"
        if isinstance(base, ast.Name) and base.id in self.cursors:
            table, idx = self.cursors[base.id]
            return f"rt::child_get(doc, {rstr(table)}, {idx}, {rstr(attr)})"
        # general value attribute
        b = self.lower_expr(base)
        return f"rt::attr(&({b}), {rstr(attr)})"

    def with_temps(self, exprs):
        """Bind value exprs to fresh temps (evaluated before any &mut borrow) to avoid
        borrowing `doc` mutably and immutably in the same call."""
        decls, names = [], []
        for e in exprs:
            n = self.newtmp("__a")
            decls.append(f"let {n} = {e};")
            names.append(n)
        return " ".join(decls), names

    def ctx_bridge(self, rust_fn, ref_args, opt_args=None):
        """Emit a ctx-taking rt call with every argument pre-bound to a temp, so a nested
        ctx-bridge inside an argument is evaluated (and its &mut ctx released) before this call's
        &mut ctx borrow. ref_args -> &temp ; opt_args -> Option<&temp> (or None)."""
        binds, refs = [], []
        for e in ref_args:
            n = self.newtmp("__d")
            binds.append(f"let {n} = {e};")
            refs.append(f"&{n}")
        for e in (opt_args or []):
            if e is None:
                refs.append("None")
            else:
                n = self.newtmp("__d")
                binds.append(f"let {n} = {e};")
                refs.append(f"Some(&{n})")
        return "{ " + " ".join(binds) + f" rt::{rust_fn}(ctx, {', '.join(refs)})? }}"

    def lower_attr_set(self, node, val):
        base = node.value
        attr = node.attr
        if self.is_self(base):
            d, (nv,) = self.with_temps([val])
            return f"{{ {d} rt::doc_set(doc, {rstr(attr)}, {nv}) }}"
        if isinstance(base, ast.Name) and base.id in self.cursors:
            table, idx = self.cursors[base.id]
            d, (nv,) = self.with_temps([val])
            return f"{{ {d} rt::child_set(doc, {rstr(table)}, {idx}, {rstr(attr)}, {nv}) }}"
        if isinstance(base, ast.Name) and base.id in self.locals:
            # set a field on a local doc/dict (e.g. a fetched doc): d.status = "X"
            d, (nv,) = self.with_temps([val])
            return f"{{ {d} rt::doc_set(&mut {mangle(base.id)}, {rstr(attr)}, {nv}) }}"
        raise Unsupported(f"attr set on {ast.dump(base)[:40]}")

    # ---- expressions -> owned Value Rust expr
    def lower_expr(self, node):
        if isinstance(node, ast.Constant):
            v = node.value
            if v is None:
                return "Value::Null"
            if isinstance(v, bool):
                return f"Value::Bool({'true' if v else 'false'})"
            if isinstance(v, int):
                return f"Value::from({v}i64)"
            if isinstance(v, float):
                return f"rt::float({v}f64)"
            if isinstance(v, str):
                return f"rt::s({rstr(v)})"
            raise Unsupported(f"const {type(v)}")
        if isinstance(node, ast.Name):
            if node.id in self.cursors:
                table, idx = self.cursors[node.id]
                return f"rt::child_row(doc, {rstr(table)}, {idx})"
            if node.id in self.locals:
                return f"{mangle(node.id)}.clone()"
            if node.id == "True":
                return "Value::Bool(true)"
            if node.id in ("False",):
                return "Value::Bool(false)"
            if node.id in ("None",):
                return "Value::Null"
            raise Unsupported(f"free name {node.id}")
        if isinstance(node, ast.Attribute):
            d = dotted(node)
            if d and d.split(".")[0] in ("frappe", "erpnext", "hrms"):
                return self.lower_module_value(d)
            return self.lower_attr_get(node)
        if isinstance(node, ast.BinOp):
            op = self.binop_fn(node.op)
            l = self.lower_expr(node.left)
            r = self.lower_expr(node.right)
            return f"rt::{op}(&({l}), &({r}))"
        if isinstance(node, ast.UnaryOp):
            o = self.lower_expr(node.operand)
            if isinstance(node.op, ast.Not):
                return f"Value::Bool(!rt::truthy(&({o})))"
            if isinstance(node.op, ast.USub):
                return f"rt::neg(&({o}))"
            if isinstance(node.op, ast.UAdd):
                return o
            raise Unsupported("unaryop")
        if isinstance(node, ast.BoolOp):
            return self.lower_boolop(node)
        if isinstance(node, ast.Compare):
            return self.lower_compare(node)
        if isinstance(node, ast.IfExp):
            c = self.lower_expr(node.test)
            a = self.lower_expr(node.body)
            b = self.lower_expr(node.orelse)
            return f"(if rt::truthy(&({c})) {{ {a} }} else {{ {b} }})"
        if isinstance(node, ast.Call):
            return self.lower_call(node)
        if isinstance(node, ast.JoinedStr):
            parts = []
            for v in node.values:
                if isinstance(v, ast.Constant):
                    parts.append(f"rt::s({rstr(str(v.value))})")
                elif isinstance(v, ast.FormattedValue):
                    inner = self.lower_expr(v.value)
                    spec = self.const_format_spec(v.format_spec)
                    if spec:
                        parts.append(f"rt::fmt_value(&({inner}), {rstr(spec)})")
                    else:
                        parts.append(inner)
                else:
                    raise Unsupported("fstring part")
            return f"rt::concat_str(&[{', '.join(parts)}])"
        if isinstance(node, ast.List) or isinstance(node, ast.Tuple):
            elts = ", ".join(self.lower_expr(e) for e in node.elts)
            return f"rt::list(vec![{elts}])"
        if isinstance(node, ast.Dict):
            pairs = []
            for k, v in zip(node.keys, node.values):
                if not (isinstance(k, ast.Constant) and isinstance(k.value, str)):
                    raise Unsupported("non-str dict key")
                pairs.append(f"({rstr(k.value)}.to_string(), {self.lower_expr(v)})")
            return f"rt::dict(vec![{', '.join(pairs)}])"
        if isinstance(node, ast.Subscript):
            base = self.lower_expr(node.value)
            if isinstance(node.slice, ast.Slice):
                raise Unsupported("slice")
            key = self.lower_expr(node.slice)
            return f"rt::subscript(&({base}), &({key}))"
        if isinstance(node, ast.ListComp) or isinstance(node, ast.GeneratorExp):
            return self.lower_comp(node, kind="list")
        if isinstance(node, ast.SetComp):
            return self.lower_comp(node, kind="list")
        if isinstance(node, ast.DictComp):
            return self.lower_comp(node, kind="dict")
        raise Unsupported(f"expr {type(node).__name__}")

    def lower_boolop(self, node):
        # short-circuit, returns operand value (Python semantics)
        vals = [self.lower_expr(v) for v in node.values]
        if isinstance(node.op, ast.And):
            # a and b -> { let t=a; if truthy(t) { b } else { t } }
            expr = vals[-1]
            for v in reversed(vals[:-1]):
                t = self.newtmp("__b")
                expr = f"{{ let {t} = {v}; if rt::truthy(&{t}) {{ {expr} }} else {{ {t} }} }}"
            return expr
        else:  # Or
            expr = vals[-1]
            for v in reversed(vals[:-1]):
                t = self.newtmp("__b")
                expr = f"{{ let {t} = {v}; if rt::truthy(&{t}) {{ {t} }} else {{ {expr} }} }}"
            return expr

    def lower_compare(self, node):
        fns = {ast.Lt: "lt", ast.LtE: "le", ast.Gt: "gt", ast.GtE: "ge", ast.Eq: "eq",
               ast.NotEq: "ne", ast.In: "in", ast.NotIn: "notin", ast.Is: "is", ast.IsNot: "isnot"}
        # Bind each operand to a temp so a chained comparison (a < b < c) never evaluates the
        # middle operand twice (which would double-run any DB call / side effect inside it).
        exprs = [self.lower_expr(node.left)] + [self.lower_expr(c) for c in node.comparators]
        decls, names = self.with_temps(exprs)
        terms = []
        for i, op in enumerate(node.ops):
            a, b = names[i], names[i + 1]
            t = type(op)
            if t not in fns:
                raise Unsupported("compare op")
            f = fns[t]
            if f == "in":
                terms.append(f"rt::truthy(&rt::contains(&{a}, &{b}))")
            elif f == "notin":
                terms.append(f"!rt::truthy(&rt::contains(&{a}, &{b}))")
            elif f == "is":
                terms.append(f"rt::truthy(&rt::eq(&{a}, &{b}))")
            elif f == "isnot":
                terms.append(f"!rt::truthy(&rt::eq(&{a}, &{b}))")
            else:
                terms.append(f"rt::truthy(&rt::{f}(&{a}, &{b}))")
        return "{ " + decls + " Value::Bool(" + " && ".join(terms) + ") }"

    def lower_comp(self, node, kind):
        if len(node.generators) != 1:
            raise Unsupported("multi-gen comprehension")
        gen = node.generators[0]
        if gen.is_async:
            raise Unsupported("async comp")
        itexpr = self.lower_expr(gen.iter)
        item = self.newtmp("__c")
        acc = self.newtmp("__acc")
        # bind target
        saved_locals = set(self.locals)
        binds = []
        if isinstance(gen.target, ast.Name):
            self.locals.add(gen.target.id)
            binds.append(f"let mut {mangle(gen.target.id)} = {item};")
        elif isinstance(gen.target, ast.Tuple):
            for i, el in enumerate(gen.target.elts):
                if not isinstance(el, ast.Name):
                    raise Unsupported("comp tuple target")
                self.locals.add(el.id)
                binds.append(f"let mut {mangle(el.id)} = rt::subscript(&{item}, &Value::from({i}i64));")
        else:
            raise Unsupported("comp target")
        conds = " && ".join(f"rt::truthy(&({self.lower_expr(c)}))" for c in gen.ifs) if gen.ifs else "true"
        if kind == "dict":
            kexpr = self.lower_expr(node.key)
            vexpr = self.lower_expr(node.value)
            body = f"if {conds} {{ {acc}.push((rt::as_str(&({kexpr})), {vexpr})); }}"
            self.locals = saved_locals
            return (f"{{ let mut {acc}: Vec<(String, Value)> = Vec::new(); "
                    f"for {item} in rt::as_list(&({itexpr})) {{ {' '.join(binds)} {body} }} rt::dict({acc}) }}")
        else:
            eltexpr = self.lower_expr(node.elt)
            body = f"if {conds} {{ {acc}.push({eltexpr}); }}"
            self.locals = saved_locals
            return (f"{{ let mut {acc}: Vec<Value> = Vec::new(); "
                    f"for {item} in rt::as_list(&({itexpr})) {{ {' '.join(binds)} {body} }} rt::list({acc}) }}")

    def binop_fn(self, op):
        m = {ast.Add: "add", ast.Sub: "sub", ast.Mult: "mul", ast.Div: "div",
             ast.FloorDiv: "floordiv", ast.Mod: "modulo", ast.Pow: "pow"}
        t = type(op)
        if t not in m:
            raise Unsupported(f"binop {t.__name__}")
        return m[t]

    # ---- calls
    def kw(self, node, name):
        for k in node.keywords:
            if k.arg == name:
                return k.value
        return None

    def throw_msg(self, callnode):
        """The message expr for frappe.throw/msgprint: positional arg 0, else msg=/message= kwarg."""
        if callnode.args:
            return self.lower_expr(callnode.args[0])
        for nm in ("msg", "message"):
            kv = self.kw(callnode, nm)
            if kv is not None:
                return self.lower_expr(kv)
        return 'rt::s("")'

    def const_format_spec(self, fs):
        """Extract a literal format-spec string from an f-string FormattedValue.format_spec, or None."""
        if fs is None:
            return None
        if isinstance(fs, ast.JoinedStr) and len(fs.values) == 1 and isinstance(fs.values[0], ast.Constant):
            return str(fs.values[0].value)
        if isinstance(fs, ast.Constant):
            return str(fs.value)
        return None

    def lower_call(self, node):
        for k in node.keywords:
            if k.arg is None:
                raise Unsupported("**kwargs")
        # super().method(...) -> base Document behaviour already handled by ferro's Rust core: no-op.
        if (isinstance(node.func, ast.Attribute) and isinstance(node.func.value, ast.Call)
                and isinstance(node.func.value.func, ast.Name) and node.func.value.func.id == "super"):
            return "Value::Null"
        target = dotted(node.func)
        args = node.args

        def a(i, default="Value::Null"):
            return self.lower_expr(args[i]) if i < len(args) else default

        # ---- builtins / utils (bare or frappe.utils.*)
        bare = target.rsplit(".", 1)[-1] if target else None
        if target in ("list",):
            return f"rt::to_list(&({a(0) if args else 'rt::list(vec![])'}))"
        if target in ("hasattr",):
            return f"rt::has_attr(&({a(0)}), &({a(1)}))"
        if target in ("getattr",):
            d = f"{a(2)}" if len(args) > 2 else "Value::Null"
            return f"rt::dict_get(&({a(0)}), &({a(1)}), Some({d}))"
        if target in ("flt", "frappe.utils.flt"):
            prec = self.kw(node, "precision")
            p = args[1] if len(args) > 1 else prec
            ptxt = f"Some(rt::as_i64(&({self.lower_expr(p)})))" if p is not None else "None"
            return f"rt::flt(&({a(0)}), {ptxt})"
        if target in ("cint", "frappe.utils.cint", "int"):
            return f"rt::cint(&({a(0)}))"
        if target in ("cstr", "frappe.utils.cstr", "str"):
            return f"rt::cstr(&({a(0)}))"
        if target in ("float",):
            return f"rt::flt(&({a(0)}), None)"
        if target == "bool":
            return f"rt::py_bool(&({a(0)}))"
        if target == "len":
            return f"rt::py_len(&({a(0)}))"
        if target == "abs":
            return f"rt::py_abs(&({a(0)}))"
        if target == "round":
            nd = f"Some(rt::as_i64(&({a(1)})))" if len(args) > 1 else "None"
            return f"rt::py_round(&({a(0)}), {nd})"
        if target in ("min", "max"):
            fn = "py_min" if target == "min" else "py_max"
            if len(args) == 1:
                # Python min(iterable) — reduce over the iterable's ELEMENTS
                return f"rt::{fn}_iter(&({self.lower_expr(args[0])}))"
            elts = ", ".join(self.lower_expr(x) for x in args)
            return f"rt::{fn}(&[{elts}])"
        if target == "sum":
            return f"rt::py_sum(&({a(0)}))"
        if target == "range":
            elts = ", ".join(self.lower_expr(x) for x in args)
            return f"rt::py_range(&[{elts}])"
        if target in ("_", "frappe._"):
            return f"rt::i18n(&({a(0)}))"
        if target in ("bold", "frappe.bold"):
            return f"rt::bold(&({a(0)}))"
        if target in ("get_link_to_form", "frappe.utils.get_link_to_form"):
            return f"rt::get_link_to_form(&({a(0)}), &({a(1)}))"
        if target in ("any",):
            return f"rt::py_any(&({a(0)}))"
        if target in ("all",):
            return f"rt::py_all(&({a(0)}))"
        # ---- date / time utils
        if bare in ("getdate",) or target == "frappe.utils.getdate":
            return f"rt::getdate(&({a(0) if args else 'Value::Null'}))"
        if bare in ("get_datetime",) or target == "frappe.utils.get_datetime":
            return f"rt::get_datetime(&({a(0) if args else 'Value::Null'}))"
        if bare in ("today", "nowdate") or target in ("frappe.utils.today", "frappe.utils.nowdate"):
            return "rt::today()"
        if bare in ("now_datetime", "now") or target in ("frappe.utils.now_datetime", "frappe.utils.now"):
            return "rt::now_datetime()"
        if bare in ("add_days",) or target == "frappe.utils.add_days":
            return f"rt::add_days(&({a(0)}), &({a(1)}))"
        if bare in ("add_months",) or target == "frappe.utils.add_months":
            return f"rt::add_months(&({a(0)}), &({a(1)}))"
        if bare in ("date_diff",) or target == "frappe.utils.date_diff":
            return f"rt::date_diff(&({a(0)}), &({a(1)}))"
        if bare in ("formatdate",) or target == "frappe.utils.formatdate":
            return f"rt::formatdate(&({a(0) if args else 'Value::Null'}))"
        if bare in ("add_to_date",) or target == "frappe.utils.add_to_date":
            dd = self.kw(node, "days")
            mm = self.kw(node, "months")
            yy = self.kw(node, "years")
            dtxt = f"rt::as_i64(&({self.lower_expr(dd)}))" if dd is not None else "0"
            mtxt = f"rt::as_i64(&({self.lower_expr(mm)}))" if mm is not None else "0"
            ytxt = f"rt::as_i64(&({self.lower_expr(yy)}))" if yy is not None else "0"
            return f"rt::add_to_date(&({a(0)}), {dtxt}, {mtxt}, {ytxt})"
        if target == "frappe._dict":
            if args and isinstance(args[0], ast.Dict):
                return self.lower_expr(args[0])
            return "rt::dict(vec![])"

        # ---- frappe.* db / orm bridges (return Result -> use ?)
        if target in ("frappe.db.get_value", "frappe.get_value", "frappe.get_cached_value",
                      "frappe.db.get_cached_value"):
            return self.ctx_bridge("db_get_value", [a(0), a(1), a(2)])
        if target in ("frappe.get_doc", "frappe.get_cached_doc"):
            return self.ctx_bridge("get_doc", [a(0), a(1) if len(args) > 1 else "Value::Null"])
        if target == "frappe.new_doc":
            return f"rt::new_doc(&({a(0)}))"
        if target == "frappe.delete_doc":
            return self.ctx_bridge("delete_doc", [a(0), a(1)])
        if target in ("frappe.scrub", "scrub"):
            return f"rt::scrub(&({a(0)}))"
        if target in ("frappe.has_permission",):
            return "Value::Bool(true)"  # Administrator demo: full access
        if target in ("frappe.db.set_single_value",):
            return self.ctx_bridge("db_set_single_value", [a(0), a(1), a(2)])
        if target in ("frappe.db.sql",):
            params_arg = a(1) if len(args) > 1 else "Value::Null"
            as_dict_kw = self.kw(node, "as_dict")
            asd = "true" if (as_dict_kw is not None and isinstance(as_dict_kw, ast.Constant) and as_dict_kw.value) else ("true" if (len(args) > 2) else "false")
            d, (qy, pr) = self.with_temps([a(0), params_arg])
            return f"{{ {d} rt::db_sql(ctx, &{qy}, &{pr}, {asd})? }}"
        if target == "frappe.db.get_single_value":
            return self.ctx_bridge("get_single_value", [a(0), a(1)])
        if target == "frappe.db.exists":
            return self.ctx_bridge("db_exists", [a(0), a(1)])
        if target == "frappe.db.count":
            f = self.kw(node, "filters")
            fexpr = self.lower_expr(f) if f is not None else (a(1) if len(args) > 1 else None)
            return self.ctx_bridge("db_count", [a(0)], [fexpr])
        if target in ("frappe.get_all", "frappe.db.get_all", "frappe.get_list", "frappe.db.get_list"):
            filt = self.kw(node, "filters")
            flds = self.kw(node, "fields")
            fexpr = self.lower_expr(filt) if filt is not None else (a(1) if len(args) > 1 else None)
            flexpr = self.lower_expr(flds) if flds is not None else None
            return self.ctx_bridge("get_all", [a(0)], [fexpr, flexpr])
        if target in ("frappe.db.set_value",):
            return self.ctx_bridge("db_set_value", [a(0), a(1), a(2), a(3)])
        if target in ("frappe.msgprint", "msgprint"):
            d, (m,) = self.with_temps([self.throw_msg(node)])
            return f"{{ {d} rt::msgprint(ctx, &{m}) }}"
        if target in ("frappe.throw", "throw"):
            # throw used as an expression value (rare) — diverge
            return f"{{ return Err(rt::throw(&({self.throw_msg(node)}))); #[allow(unreachable_code)] Value::Null }}"

        # ---- self.<method>(...)  -> sibling fn call
        if target and target.startswith("self.") and target.count(".") == 1:
            mname = target.split(".", 1)[1]
            # special document methods
            if mname == "get":
                return f"rt::dget(doc, &({a(0)}))"
            if mname == "set":
                d, (k, v) = self.with_temps([a(0), a(1)])
                return f"{{ {d} rt::dset(doc, &{k}, {v}) }}"
            if mname == "get_value":
                return f"rt::dget(doc, &({a(0)}))"
            if mname == "append":
                d, (k, v) = self.with_temps([a(0), a(1)])
                return f"{{ {d} rt::append_v(doc, &{k}, {v}) }}"
            if mname == "db_set":
                d, (k, v) = self.with_temps([a(0), a(1)])
                return f"{{ {d} rt::self_db_set(ctx, doc, &{k}, &{v})? }}"
            if mname in ("precision",):
                return f"rt::precision(&({a(0)}))"
            if mname == "get_all_children":
                return f"rt::dget(doc, &({a(0)}))"
            if mname == "update":
                d, (o,) = self.with_temps([a(0)])
                return f"{{ {d} rt::update_into(doc, &{o}) }}"
            if mname == "is_new":
                return "rt::doc_is_new(doc)"
            if mname == "has_value_changed":
                return "Value::Bool(true)"  # conservative: treat as changed (no pre-save snapshot)
            if mname == "get_doc_before_save":
                return "Value::Null"
            if mname in ("save", "insert"):
                return "rt::self_save(ctx, doc)?"
            if mname in ("reload",):
                return "Value::Null"
            if mname in self.methods:
                self.calls_self.add(mname)
                callee = self.methods[mname]
                cparams = [arg.arg for arg in callee.args.args if arg.arg != "self"]
                cdefaults = callee.args.defaults
                ndef, nparams = len(cdefaults), len(cparams)
                provided = [self.lower_expr(x) for x in args]
                kwmap = {k.arg: self.lower_expr(k.value) for k in node.keywords if k.arg}
                callargs = ["doc", "ctx"]
                for i, pname in enumerate(cparams):
                    if i < len(provided):
                        callargs.append(provided[i])
                    elif pname in kwmap:
                        callargs.append(kwmap[pname])
                    else:
                        di = i - (nparams - ndef)
                        callargs.append(self.lower_expr(cdefaults[di]) if 0 <= di < ndef else "Value::Null")
                return f"{self.prefix}_{mname}({', '.join(callargs)})?"
            raise Unsupported(f"self.{mname}")

        # ---- value methods: x.method(...)
        if isinstance(node.func, ast.Attribute):
            recvnode = node.func.value
            m = node.func.attr
            recv = self.lower_expr(recvnode)
            if m == "format":
                pos = ", ".join(self.lower_expr(x) for x in args)
                named = ", ".join(f"({rstr(k.arg)}.to_string(), {self.lower_expr(k.value)})" for k in node.keywords)
                return f"rt::str_format(&({recv}), &[{pos}], &[{named}])"
            if m == "strip":
                return f"rt::str_strip(&({recv}))"
            if m == "lower":
                return f"rt::str_lower(&({recv}))"
            if m == "upper":
                return f"rt::str_upper(&({recv}))"
            if m == "startswith":
                return f"rt::str_startswith(&({recv}), &({a(0)}))"
            if m == "endswith":
                return f"rt::str_endswith(&({recv}), &({a(0)}))"
            if m == "replace":
                return f"rt::str_replace(&({recv}), &({a(0)}), &({a(1)}))"
            if m == "split":
                sep = f"Some(&({a(0)}))" if args else "None"
                return f"rt::str_split(&({recv}), {sep})"
            if m == "join":
                return f"rt::str_join(&({recv}), &({a(0)}))"
            if m == "get":
                d = f"Some({a(1)})" if len(args) > 1 else "None"
                return f"rt::dict_get(&({recv}), &({a(0)}), {d})"
            if m == "precision":
                return f"rt::precision(&({a(0) if args else 'Value::Null'}))"
            if m == "update":
                if isinstance(recvnode, ast.Name) and recvnode.id in self.locals:
                    d, (o,) = self.with_temps([a(0)])
                    return f"{{ {d} rt::update_into(&mut {mangle(recvnode.id)}, &{o}) }}"
                raise Unsupported("update on expr")
            if m == "items":
                return f"rt::items(&({recv}))"
            if m == "keys":
                return f"rt::keys(&({recv}))"
            if m == "values":
                return f"rt::values(&({recv}))"
            if m == "append":
                # Document.append(table, row) is always 2-arg; list.append(x) is 1-arg.
                if len(args) >= 2:
                    if isinstance(recvnode, ast.Attribute) and self.is_self(recvnode.value):
                        d, (t, v) = self.with_temps([a(0), a(1)])
                        return f"{{ {d} rt::append_v(doc, &{t}, {v}) }}"
                    if isinstance(recvnode, ast.Name) and recvnode.id in self.locals:
                        d, (t, v) = self.with_temps([a(0), a(1)])
                        return f"{{ {d} rt::append_to(&mut {mangle(recvnode.id)}, &{t}, {v}) }}"
                    raise Unsupported("append(table,row) on expr")
                # self.<table>.append(row) -> child table; local_list.append(x) -> list push
                if isinstance(recvnode, ast.Attribute) and self.is_self(recvnode.value):
                    d, (v,) = self.with_temps([a(0)])
                    return f"{{ {d} rt::append(doc, {rstr(recvnode.attr)}, {v}) }}"
                if isinstance(recvnode, ast.Name) and recvnode.id in self.locals:
                    d, (v,) = self.with_temps([a(0)])
                    return f"{{ {d} rt::list_push(&mut {mangle(recvnode.id)}, {v}) }}"
                raise Unsupported("append on expr")
            raise Unsupported(f"method .{m}")

        raise Unsupported(f"call {target}")

    def _unsupported(self, why):
        raise Unsupported(why)


def base_as_place(node):
    return "doc"  # unused placeholder


def recv_place(node, low):
    raise Unsupported("place")


# ---------------------------------------------------------------- driver
def collect():
    controllers = []  # (doctype_name, fnprefix, methods_by_name, path)
    seen_prefix = {}
    idx = 0
    for app in APPS:
        root = os.path.join(REPOS, app)
        for path in glob.glob(os.path.join(root, "**", "doctype", "**", "*.py"), recursive=True):
            base = os.path.basename(path)
            parent = os.path.basename(os.path.dirname(path))
            if base == "__init__.py" or base.startswith("test_") or base[:-3] != parent:
                continue
            jsonp = os.path.join(os.path.dirname(path), parent + ".json")
            if not os.path.exists(jsonp):
                continue
            try:
                dt_name = json.load(open(jsonp, encoding="utf-8")).get("name")
            except Exception:
                continue
            if not dt_name:
                continue
            try:
                tree = ast.parse(open(path, encoding="utf-8").read())
            except Exception:
                continue
            classes = [n for n in tree.body if isinstance(n, ast.ClassDef) and is_document_subclass(n)]
            if not classes:
                continue
            methods = {n.name: n for n in classes[0].body if isinstance(n, ast.FunctionDef)}
            if not methods:
                continue
            idx += 1
            controllers.append((dt_name, f"c{idx}", methods, path, app))
    return controllers


def collect_assigned(fn):
    """All simple local names assigned anywhere in the function body (Python is function-scoped)."""
    names = set()

    class V(ast.NodeVisitor):
        def visit_Name(self, n):
            if isinstance(n.ctx, ast.Store):
                names.add(n.id)

        def visit_FunctionDef(self, n):
            pass  # don't descend into nested defs

    v = V()
    for s in fn.body:
        v.visit(s)
    return names


def try_lower_method(fn, methods, prefix):
    low = Lowerer(methods, prefix)
    # params after self
    params = [arg.arg for arg in fn.args.args if arg.arg != "self"]
    for p in params:
        low.locals.add(p)
    # hoist all locals to function scope (Python semantics) so block-scoped Rust `let` never
    # makes a variable assigned inside an `if`/`for` invisible afterwards.
    hoist = sorted(collect_assigned(fn) - set(params))
    for h in hoist:
        low.locals.add(h)
    try:
        body = low.lower_body(fn.body, 2)
    except Unsupported as e:
        return None, str(e), set(), False
    # "effectful" = the body does something other than no-ops. A method whose body is empty or only
    # `let _ = Value::Null;` (i.e. pure `pass` / `super().x()` delegation we cannot fulfil) is NOT
    # wired into the lifecycle dispatch — wiring it would falsely report the event as "handled"
    # while silently doing nothing (and block any fallback). See audit finding (boarding controllers).
    effectful = any(l.strip() and l.strip() != "let _ = Value::Null;" for l in body)
    sig_params = "".join(f", mut {mangle(p)}: Value" for p in params)
    rust = []
    rust.append(f"fn {prefix}_{fn.name}(doc: &mut Value, ctx: &mut Ctx{sig_params}) -> Result<Value, FerroErr> {{")
    for h in hoist:
        rust.append(f"    let mut {mangle(h)}: Value = Value::Null;")
    rust += body
    rust.append("    Ok(Value::Null)")
    rust.append("}")
    return ("\n".join(rust), params, low.calls_self, effectful)


def main():
    out_path = OUT
    if "--out" in sys.argv:
        out_path = sys.argv[sys.argv.index("--out") + 1]
    controllers = collect()
    # First pass: attempt to lower every method; record success + self-call edges + effectful flag.
    per = {}  # prefix -> {mname: (rust|None, params, calls_self, effectful)}
    fail_reasons = collections.Counter()
    for (dt, prefix, methods, path, app) in controllers:
        per[prefix] = {}
        for mname, fn in methods.items():
            res = try_lower_method(fn, methods, prefix)
            if res[0] is None:
                per[prefix][mname] = (None, [], set(), False)
                fail_reasons[res[1]] += 1
            else:
                per[prefix][mname] = res  # (rust, params, calls, effectful)

    # Transitive-green: a method is emittable iff it lowered AND all self-callees are emittable.
    def emittable(prefix, mname, stack):
        rec = per[prefix].get(mname)
        if rec is None or rec[0] is None:
            return False
        if (prefix, mname) in stack:
            return True  # recursion within an emittable cycle
        stack = stack | {(prefix, mname)}
        for callee in rec[2]:
            if not emittable(prefix, callee, stack):
                return False
        return True

    emitted_fns = []
    emitted_keys = []          # (prefix, mname) of every emitted fn
    dispatch = collections.defaultdict(list)  # dt -> [(event, fnname)]
    native_doctypes = []
    total_emitted = total_lifecycle = 0
    for (dt, prefix, methods, path, app) in controllers:
        dt_has = False
        for mname, fn in methods.items():
            if not emittable(prefix, mname, set()):
                continue
            rec = per[prefix][mname]
            emitted_fns.append(rec[0])
            emitted_keys.append((prefix, mname))
            total_emitted += 1
            # Wire a lifecycle handler ONLY if it actually does something (not a pure pass/super()).
            if mname in LIFECYCLE and rec[3]:
                dispatch[dt].append((mname, f"{prefix}_{mname}"))
                total_lifecycle += 1
                dt_has = True
        if dt_has:
            native_doctypes.append(dt)

    # Reachability: BFS from every wired lifecycle handler over self-call edges -> what can actually
    # run at request time (the honest "functional" method count, vs total emitted which includes
    # transpiled-but-not-yet-routed @whitelist RPCs that have no dispatch entry point).
    prefix_of_dt = {}
    for (dt, prefix, methods, path, app) in controllers:
        prefix_of_dt.setdefault(dt, prefix)
    roots = []
    for dt, handlers in dispatch.items():
        p = prefix_of_dt[dt]
        for (event, _fn) in handlers:
            roots.append((p, event))
    reachable = set()
    stack = list(roots)
    while stack:
        key = stack.pop()
        if key in reachable:
            continue
        reachable.add(key)
        p, m = key
        rec = per.get(p, {}).get(m)
        if rec:
            for callee in rec[2]:
                if (p, callee) not in reachable:
                    stack.append((p, callee))
    total_reachable = len(reachable)

    # ---- emit generated.rs
    L = []
    L.append("//! AUTO-GENERATED by transpile.py — Frappe controllers transpiled to Rust.")
    L.append("//! Do not edit by hand. Regenerate with: python3 transpile/transpile.py")
    L.append("#![allow(unused, non_snake_case, unused_parens, clippy::all)]")
    L.append("use crate::rt::{self, Ctx, FerroErr};")
    L.append("use serde_json::Value;")
    L.append("")
    L += emitted_fns
    L.append("")
    # dispatch
    L.append("pub fn run_event(doctype: &str, event: &str, doc: &mut Value, ctx: &mut Ctx) -> Result<bool, FerroErr> {")
    L.append("    match (doctype, event) {")
    for dt in sorted(dispatch):
        for (event, fnname) in dispatch[dt]:
            L.append(f"        ({rstr(dt)}, {rstr(event)}) => {{ {fnname}(doc, ctx)?; Ok(true) }}")
    L.append("        _ => Ok(false),")
    L.append("    }")
    L.append("}")
    L.append("")
    # has_event
    L.append("pub fn has_event(doctype: &str, event: &str) -> bool {")
    L.append("    matches!((doctype, event),")
    pairs = []
    for dt in sorted(dispatch):
        for (event, _fn) in dispatch[dt]:
            pairs.append(f"({rstr(dt)}, {rstr(event)})")
    L.append("        " + " | ".join(pairs) if pairs else "        (\"\\0\", \"\\0\")")
    L.append("    )")
    L.append("}")
    L.append("")
    L.append("pub fn has_any(doctype: &str) -> bool { NATIVE_DOCTYPES.contains(&doctype) }")
    L.append("")
    ndset = sorted(set(native_doctypes))
    nd = ", ".join(rstr(d) for d in ndset)
    L.append(f"pub const NATIVE_DOCTYPES: &[&str] = &[{nd}];")
    # STATS = (native_doctypes, REACHABLE methods (runnable via lifecycle dispatch), lifecycle handlers)
    L.append(f"pub const STATS: (usize, usize, usize) = ({len(ndset)}, {total_reachable}, {total_lifecycle});")
    # EMITTED = total transpiled fns compiled in (includes @whitelist RPCs not yet routed to a dispatch)
    L.append(f"pub const EMITTED_METHODS: usize = {total_emitted};")
    L.append("")
    open(out_path, "w").write("\n".join(L) + "\n")

    # ---- report
    sys.stderr.write("=== TRANSPILE RESULT ===\n")
    sys.stderr.write(f"controllers scanned          : {len(controllers)}\n")
    sys.stderr.write(f"methods emitted (compiled in): {total_emitted}\n")
    sys.stderr.write(f"  of which REACHABLE at runtime (via lifecycle dispatch): {total_reachable}\n")
    sys.stderr.write(f"  not-yet-routed (@whitelist RPCs, no dispatch path)    : {total_emitted - total_reachable}\n")
    sys.stderr.write(f"lifecycle handlers wired     : {total_lifecycle}\n")
    sys.stderr.write(f"doctypes with native logic   : {len(ndset)}\n")
    sys.stderr.write(f"generated.rs lines           : {len(L)}\n")
    sys.stderr.write("top lowering-fail reasons:\n")
    for k, v in fail_reasons.most_common(20):
        sys.stderr.write(f"  {v:5d}  {k}\n")


if __name__ == "__main__":
    main()
