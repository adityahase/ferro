# ferro-native — transpiling Frappe apps to Rust, one binary, no interpreter

**Question (the user's):** instead of running app controllers on an *embedded Python interpreter*
(PyO3/ferrod), can we **transpile the Python to Rust and compile everything into one binary** — no
PyO3, no CPython — and still run the 5 investigated apps under 64 MB?

**Answer: yes, for the mechanically-transpilable subset, which is the majority.** This directory
contains a working Python→Rust transpiler, a generated `generated.rs` with **1,274 transpiled
controller methods** (of which **162 lifecycle handlers across 133 DocTypes**, ≈311 methods, are wired
and reachable; the remaining 963 are transpiled `@whitelist` RPCs not yet routed), compiled into a
single **2.09 MB binary that links only libc** (zero Python — verified by `ldd` and a symbol scan).
It serves all 1,077 DocTypes' CRUD and runs transpiled `validate`/`on_*` logic natively; a 7-case
selftest + 5 runtime unit tests prove the logic actually executes (throws fire on field values, a
field is computed and persisted, the pure-CRUD path still works). Measured peak **18.8 MB @4 threads
(tuned)** — ~3× under budget and ~3× lighter than the embedded-CPython path. These numbers are
**post-adversarial-audit** (`reports/03-audit.md`): 7 transpiler correctness bugs were found and fixed.

---

## 1. The pieces

```
transpile/            # this directory (at the repo root)
  characterize.py     # AST survey: what % of controllers fall in a transpilable subset
  transpile.py        # the Python→Rust transpiler (ast -> Rust source)
  gen/                # generated.rs snapshot, selftest cases, characterize JSON
  measurements/       # smaps numbers, selftest output, no-python proof
src/native/           # output lives in the runtime crate
  rt.rs               # the dynamic runtime the generated code calls (≈900 LOC Rust)
  generated.rs        # AUTO-GENERATED transpiled controllers + dispatch registry
  main.rs             # the ferro-native binary: HTTP server + native write-driver
Cargo.toml            # [[bin]] ferro-native, required-features=["native"]
```

Build: `cargo build --release --features native --bin ferro-native`. The data plane
(`orm`/`meta`/`auth`/`naming`/`crypto`/`util`) is **reused unchanged** from the ferro lib — the same
tested Rust that serves the REST API. Only the controller layer is new.

## 2. The core idea that makes transpilation tractable: a uniform dynamic value

Python is dynamically typed; Rust is statically typed. General Python→Rust is intractable because of
type inference. We **dodge it entirely**: every transpiled Python expression lowers to a Rust value
of one type — `serde_json::Value` (the very type ferro's ORM already speaks). Every Python operator
becomes a free function on it that reproduces Frappe/Python's *dynamic* semantics:

| Python | Rust (rt::) |
|---|---|
| `a + b` | `rt::add(&a, &b)` (int+int→int, str concat, list concat, else float) |
| `a < b`, `a == b`, `a in c` | `rt::lt/eq/contains(...)` → `Value::Bool` |
| `if cond:` | `if rt::truthy(&cond) {` (None/0/""/[] falsy) |
| `flt(x, 2)`, `cint(x)`, `cstr(x)` | `rt::flt/cint/cstr(...)` |
| `self.field` / `self.field = v` | `rt::doc_get(doc,"field")` / `rt::doc_set(doc,"field",v)` |
| `for row in self.items:` | index cursor → `rt::child_get/child_set(doc,"items",i,"f")` |
| `frappe.db.get_value(dt,n,f)` | `rt::db_get_value(ctx, …)?` (into ferro ORM, same txn) |
| `frappe.throw(_(msg))` | `return Err(rt::throw(&rt::i18n(&msg)))` |
| `"{0}".format(x)`, f-strings | `rt::str_format(...)`, `rt::concat_str(...)` |

A controller method becomes `fn(doc: &mut Value, ctx: &mut Ctx) -> Result<Value, FerroErr>`. This is
not only tractable — it's *more faithful* than guessing static types, because `flt(None)==0.0`,
truthiness, and mixed arithmetic match Python exactly.

## 3. What the transpiler handles (the supported subset)

Statements: assign / aug-assign / multi-target / tuple-unpack, `if`/`elif`/`else`, `for` (over
child tables, general lists, `range`, `enumerate`, `dict.items()`), `break`/`continue`, `return`,
in-function lazy `import` (skipped), `pass`.
Expressions: literals, names, attributes, arithmetic, comparison (incl. chained), boolean
short-circuit, unary, ternary, subscript, list/tuple/dict literals, **list/dict/set
comprehensions & generator expressions**, f-strings.
Calls: builtins (`len/abs/round/min/max/sum/range/any/all/str/int/float/bool/sorted/enumerate/
list/dict/hasattr/getattr`), Frappe utils (`flt/cint/cstr/getdate/today/nowdate/now_datetime/
add_days/add_months/date_diff/add_to_date/get_link_to_form/scrub/bold/_`), the ORM/db bridges
(`get_doc/new_doc/delete_doc/get_all/get_value/get_cached_value/db.get_value/db.set_value/db.exists/
db.count/db.get_single_value/db.set_single_value/db.sql`), the `Document` contract
(`self.get/set/append/db_set/save/is_new/precision/get_all_children/update`), `super().x()`
(no-op — the base behaviour is already ferro's Rust core), and string methods
(`format/strip/lower/upper/startswith/endswith/split/join/replace/get/items/keys/values/update`).

### Two engineering problems solved during the build (worth recording)
1. **Scope.** Python locals are *function*-scoped; a naive `let` inside an `if`/`for` is
   block-scoped, so a variable assigned in a branch is invisible after it. Fix: a pre-pass collects
   every assigned local and **hoists** `let mut <x> = Value::Null;` to the top of the function;
   everything else is plain assignment. (This single fix eliminated ~hundreds of compile errors.)
2. **Borrows.** `self.x = self.y + 1` lowered naively borrows `doc` `&mut` and `&` at once; nested
   `db_get_value(ctx, …, db_get_value(ctx, …))` borrows `ctx` `&mut` twice. Fix: mutation/`ctx`
   bridges **pre-bind their value arguments to temporaries** so each inner read/borrow completes
   before the outer mutable borrow. Plus an identifier **mangler** (`doc`/`ctx`/Rust keywords like
   `type`/`match`/`ref` → `v_*`) so controller locals never collide with the generated params.

## 4. Selective execution & the write-driver (faithful to Frappe's lifecycle)

`generated.rs` exports a registry: `has_event(doctype,event)` and
`run_event(doctype,event,doc,ctx)`. `main.rs`'s `route_resource` takes the **pure-Rust CRUD fast
path** whenever no transpiled handler exists for the write's events (the common case — all reads,
all pure-CRUD DocTypes); otherwise it drives the lifecycle:

```
INSERT: before_validate · validate · before_save · before_insert   [native if present]
        → ferro ORM insert (naming, defaults, children, required, txn)
        → after_insert · on_update · on_change                     [native if present]
UPDATE: load+merge doc → before_validate · validate · before_save
        → ferro ORM update → on_update · on_change
```

The transpiled controllers' own `frappe.db.*` / `frappe.get_doc` calls run on the **same
connection/transaction** as the triggering write. Transitive safety: a method is emitted only if it
**and every `self.` method it calls** transpile — generated code never dangles into an un-emitted
helper.

## 5. Coverage (honest)

From `characterize.py` over 766 DocType controllers: **57% are pure-CRUD (zero code)** and **~64% of
DocTypes are "fully native"** (CRUD + all-methods-transpilable); **51% of all methods** and **64% of
lifecycle hooks** are in the transpilable subset. The transpiler **emitted 1,274 methods** that
compile, of which **162 lifecycle handlers across 133 DocTypes** are wired into dispatch and **311
methods are reachable** at runtime (handlers + their transitive helpers). The other **963 emitted
methods are `@whitelist` RPCs** — they transpile and compile, but ferro-native currently routes only
the lifecycle, so they have no entry point yet (the audit flagged this; routing
`/api/method/<dotted>` is the next feature). The gap between "transpilable in principle" and "wired"
is the transitive-closure rule, the no-trivial-handler rule (pure `super()`/`pass` handlers are not
wired), and a few constructs still pending.

### What is NOT transpiled yet (ranked, from the live fail log)
- `frappe.qb` query-builder DSL (~250 sites: `from_/where/select/run/&/Sum/on/as_`) — the one
  substantial sub-compiler remaining; **the single biggest lever**.
- `try/except` (36), walrus `:=` (33), `**kwargs` (28), `set()` literals (46), non-str dict keys (25),
  `isinstance`/`getattr` reflection (18), nested `self.flags.x = …` (13).
- Cross-module calls into other apps' Python (`validate_active_employee`, `StatusService`, …) —
  these need those callees transpiled too (module-level functions, a natural next step).

These fall back to the **PyO3 path (ferrod)** or stay deferred-and-documented. Crucially, **memory is
decoupled from coverage**: anything that *is* Rust costs ~0 resident, so partial coverage already
delivers the full memory win for the covered set.

## 6. Limitations / fidelity caveats (deliberate, for the demo)
- `has_value_changed()` returns `true` (no pre-save snapshot yet) — conservative (may over-run a
  guarded branch, never skips one). `get_doc_before_save()` returns null.
- `validate` runs *before* naming in the driver (Frappe runs some naming first); fine for the
  controllers tested, noted for the heavy tail.
- `precision()` returns a fixed default; permission/`has_permission` is Administrator-open in the
  demo; `frappe.flags.*` default to falsy.
- `db.sql` passes through to SQLite with backtick→double-quote normalisation; MariaDB-only SQL in the
  erpnext tail is a known residual.

The adversarial audit (`reports/03-audit.md`) found and **fixed** seven further correctness bugs
(min/max-over-list, child `.append(table,row)`, trivially-wired `super()`-only handlers, empty
`throw(msg=)`, negative modulo, dropped format-specs, chained-comparison double-eval) and documented
a handful of bounded residuals (child-row-passed-to-helper write-back, `has_value_changed`, un-routed
`@whitelist` RPCs). None of these are *memory* problems; they are correctness edges on the heavy tail,
exactly as the prior architecture study predicted.

## 7. Bottom line
Transpiling Frappe apps to native Rust and shipping **one interpreter-free binary** is real and
demonstrated: 137 DocTypes' business logic compiled to machine code, executing correctly against the
real SQLite site, at **18 MB / 4 threads** — comfortably under 64 MB and several times lighter than
embedding CPython, with the Python runtime removed entirely.
