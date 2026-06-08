# Can we transpile Frappe apps to Rust? — corpus characterization

**Date:** 2026-06-07  ·  **Corpus:** crm, helpdesk, gameplan, hrms, erpnext (the 5 investigated apps)
**Method:** `characterize.py` walks every DocType controller's AST. A method is **GREEN**
(transpilable to Rust *today*) iff every AST node and every call target falls inside a defined
*transpilable subset*; otherwise **RED**, and we record the exact blocker. Doctype classification:
PURE_CRUD (no business methods → ferro already serves with zero code), FULL_GREEN (all methods
green), PARTIAL, RED.

## Headline numbers (766 doctype controllers, 3650 methods)

| Bucket | Count | Share |
|---|--:|--:|
| **Pure-CRUD doctypes** (zero code) | 434 | **57%** |
| Full-green doctypes (all methods transpilable) | 53 | 7% |
| **→ Fully native today** (CRUD + full-green) | **487** | **64%** |
| Partial doctypes (some methods transpilable) | 243 | 32% |
| Red doctypes (no method transpilable) | 36 | 5% |
| **Methods green** (transpilable) | 1855 | **51%** |
| **Lifecycle methods green** (validate/on_update/on_submit/…) | 484 / 755 | **64%** |

**So ~64% of doctypes can run entirely in compiled Rust with no interpreter at all**, and just
over half of *all* controller methods — including 64% of the lifecycle hooks that carry the
actual business rules — fall inside a mechanically-transpilable subset.

## What blocks the other 49% of methods (ranked — the roadmap)

| Hits | Blocker | Nature | Transpilable? |
|--:|---|---|---|
| ~1000 | `frappe.qb` builder (`from_`/`where`/`select`/`run`/`&`=BitAnd/`Sum`/`on`/`as_`) | pypika query DSL | yes, but it's a whole sub-compiler → **biggest single lever** |
| 118 | `frappe.bold` (bare `bold`) | util | trivial add |
| 103 | `super().method()` | base-class call | yes (map to ferro base impl / no-op) |
| 98 | `frappe.db.sql(...)` | raw SQL | yes (passthrough to ferro SQLite) |
| 61/63 | `try` / `except` | error handling | yes (lower to Rust match on Result) |
| 58 | non-string-keyed dict literal | container | defer |
| 54 | `:=` walrus (`NamedExpr`) | expr | yes (lower to let + use) |
| 50 | `frappe.get_single_value` | ORM | trivial add |
| 37 | `**kwargs` spread | call | defer |
| 31/25 | `getattr` / `isinstance` | dynamic | hard (reflection) — defer |

The long tail after `frappe.qb` is mostly small, mechanical additions. **The query builder is the
one substantial sub-project**; everything else is incremental. For the demo we target the current
green subset (which already needs no qb), then add the cheap wins (`bold`, `db.sql`,
`get_single_value`, in-function imports, `super` no-op, try/except).

## The design decision this drives: a uniform dynamic `Value` model

Python is untyped; Rust is statically typed. The classic transpiler killer is type inference. We
**sidestep it entirely**: every transpiled Python expression lowers to a Rust value of one
dynamic enum —

```
enum Value { Null, Bool(bool), Int(i64), Float(f64), Str(String), List(Vec<Value>),
             Map(IndexMap<String,Value>), Doc(DocRef) }
```

— and every Python operator becomes a method on it (`a.add(&b)`, `a.lt(&b)`, `a.truthy()`),
matching Frappe's *own* dynamic semantics bit-for-bit (e.g. `flt(None)==0.0`). Locals are all
`let mut x: Value`. A controller method lowers to
`fn(doc:&mut Doc, ctx:&mut Ctx) -> Result<Value, FerroErr>`. This makes the lowering mechanical
and uniform, and it's *more* faithful than guessing static types would be.

Transitive-green rule: a method is only emitted if it **and every `self.` method it transitively
calls** are green — so generated code never dangles a call into an un-emitted (red) helper.

## Bottom line

Transpiling Frappe apps to native Rust is **not** all-or-nothing. 64% of doctypes need no
interpreter at all; the transpilable method subset is just over half today and rises with each
roadmap item. The remainder falls back (to the PyO3 path from the prior study, or stays
unimplemented and documented). The memory win is decoupled from coverage: **any** code that is
Rust instead of Python contributes ~zero per-process heap — so the more we transpile, the closer
the whole system sits to ferro's ~5 MB floor.
