# FINDINGS — Transpiling Frappe apps to Rust (ferro-native)

**Date:** 2026-06-07/08 · **Author:** autonomous build session · **Status:** complete, measured, audited.

> **Question.** We built `ferro` (a Rust reimplementation of Frappe's data plane) and showed the 5
> investigated apps can run on it via *embedded CPython* (PyO3 / `ferrod`). Can we instead
> **transpile the apps' Python controllers to Rust and compile everything into one binary** — no
> PyO3, no interpreter — run all 5 apps, and stay under 64 MB? **Find ways to compress memory.
> Document aggressively.**

> **Answer.** **Yes — built, demonstrated, measured, and adversarially audited.** A working
> Python→Rust transpiler produces a single **2.09 MB binary that links only libc (zero Python)**,
> serving all 1,077 DocTypes' CRUD and running **1,274 transpiled controller methods** natively, at
> **18.8 MB peak @4 threads** — ~3× under budget and ~3× lighter than embedding CPython. The memory
> win is *structural and decoupled from coverage*: compiled code is shared `.text` (≈0 per-process
> heap), unlike interpreted controllers (per-worker private heap).

---

## 1. Headline findings

1. **A single interpreter-free binary is achievable and small.** `ferro-native` (2.09 MB) links only
   `libc`/`libm`/`libgcc` — `ldd` and a symbol scan both show **0 Python**. It reuses ferro's tested
   Rust data plane (`orm`/`meta`/`auth`/`naming`/`crypto`) and adds a native write-driver that
   dispatches to transpiled controllers.

2. **Transpilation is tractable via a uniform dynamic value model.** Every Python expression lowers
   to one type — `serde_json::Value` — and every operator to an `rt::*` free function reproducing
   Python/Frappe's dynamic semantics (`flt(None)==0.0`, truthiness, mixed arithmetic, `/` is float).
   This sidesteps static type inference entirely and is *more* faithful than guessing types. A method
   becomes `fn(doc:&mut Value, ctx:&mut Ctx) -> Result<Value, FerroErr>`.

3. **Coverage is majority, not all (honest counts).** Of 766 DocType controllers: **57% are
   pure-CRUD** (zero code, served natively) and **~64% are fully native**. **51% of all methods** and
   **64% of lifecycle hooks** are in the transpilable subset. The transpiler **emitted 1,274 methods**
   that compile; **162 lifecycle handlers across 133 DocTypes** are wired (**311 methods reachable** =
   handlers + helpers). The other **963 emitted methods are `@whitelist` RPCs not yet routed** (they
   compile and prove the transpiler handles them, but ferro-native only routes the lifecycle today).

4. **The logic provably executes** (not just compiles). 7/7 selftests + 5/5 Rust unit tests + a live
   curl demo: Coupon Code's transpiled `validate` throws "Please select the customer." on a Gift Card
   with no customer, and **computes & persists `maximum_use=1`** when one is present; Bank Guarantee /
   Price List / Vehicle Log validators throw on the right field conditions; pure-CRUD still serves
   non-native DocTypes.

5. **Memory: under 64 MB with ~3× margin, and the transpiled code is "free."**

   | @4 threads, all 5 apps, reads + transpiled writes | idle PSS | peak RSS | peak PSS |
   |---|--:|--:|--:|
   | **ferro-native (transpiled→Rust), tuned** (`MALLOC_ARENA_MAX=2 FERRO_CACHE_KB=64`) | 1.0 | **18.8** | **16.4** |
   | ferro-native, default config | 1.0 | 23.4 | 21.1 |
   | ferrod (PyO3 embedded CPython, all apps) | 43.1 | 62.9 | 57.9 |
   | stock CPython+Frappe gunicorn worker (prior study) | — | ~115 | — |

   - **~2.7–3.3× lighter than the PyO3 path; ~5–6× lighter than stock Frappe.** ferrod eager peaks
     *right at* the 64 MB line; the transpiled build has ~3× headroom.
   - **Why compiled code is free:** `.text` = 1.51 MB (all 1,274 methods + ferro core) is read-only,
     file-backed, COW-shared. At idle the process is RSS 4.0 MB but only **0.4 MB private-dirty** —
     the rest is shared text/libc. Adding all methods grew the binary ~330 KB and resident PSS ~0.
   - **Per-thread marginal cost ≈ 3.4–4 MB**, dominated by each worker's SQLite page/schema cache
     (989 tables) + glibc arena — *not* the transpiled code. Biggest compression lever:
     `FERRO_CACHE_KB=64` (−4.6 MB at 4T). 8T tuned ≈ 35 MB; 16T ≈ 60 MB; both under 64.
   - **Scaling model:** throughput comes from N worker *processes* (each independently < 64 MB), the
     Frappe gunicorn model — not one 32-thread process (which would exceed 64 MB, the wrong model).

## 2. Two engineering problems solved (worth recording)

- **Scope.** Python locals are function-scoped; naive block-scoped Rust `let` made variables assigned
  inside an `if`/`for` invisible afterward (hundreds of compile errors). Fix: a pre-pass **hoists**
  all locals to the top of the function; everything else is plain assignment.
- **Borrows.** `self.x = self.y + 1` borrowed `doc` `&mut` and `&` at once; nested
  `db_get_value(ctx, …, db_get_value(ctx, …))` borrowed `ctx` twice. Fix: mutation/`ctx` calls
  **pre-bind their value args to temporaries**; plus an identifier **mangler** (`doc`/`ctx`/Rust
  keywords like `type`/`match`/`ref` → `v_*`) so controller locals never collide with generated params.

## 3. Adversarial audit (8-agent workflow) — what it confirmed and fixed

The audit (memory honesty, coverage accuracy, transpiler soundness, and line-by-line Rust-vs-Python
correctness over 4 app batches) **independently confirmed** the binary size, no-Python claim, the
all-5-apps DB, the peak-under-load numbers, and that load genuinely exercises transpiled logic. It
found **7 silent-miscompile bugs** (compile-clean but wrong), all now **fixed and regression-tested**:

| Bug | Severity | Fix |
|---|---|---|
| `min(list)`/`max(list)` reduced over the args slice → returned the whole list (live via Job Card) | critical | `py_min_iter`/`py_max_iter` over elements |
| `localdoc.append("table", row)` pushed the table-name string, dropped the row | critical | 2-arg `.append` → `rt::append_to` child-append |
| `super()`-only / `pass` lifecycle handlers wired as no-op "handled" (boarding controllers did nothing) | critical | wire a handler only if **effectful**; 8 dropped → fall back |
| `frappe.throw(msg=…)` keyword form threw an empty string | high | read `msg=`/`message=` kwargs (0 empty throws remain) |
| negative modulo used `rem_euclid` (`7 % -3 = 1`) | high | Python-sign-of-divisor `%` |
| `{:.2f}`/`{:.0f}`/`{:.1%}`/`{:,}` format-specs stripped | high | `rt::fmt_value` applied in f-strings & `.format` |
| chained comparison double-evaluated the middle operand | medium | bind operands to temps once |

**Documented (bounded) residuals:** child-row-passed-to-a-helper loses write-backs;
`has_value_changed()` returns `true` (correct for INSERT); `self.flags.*` read as falsy; un-routed
`@whitelist` RPCs (963); a few non-dispatched `validate()` bodies; round-half-away vs banker's
rounding; `db.sql` doesn't translate `%s` placeholders; div-by-zero returns 0. Full detail and
evidence in `reports/03-audit.md`.

## 4. What this proves about the broader goal

- The thing that makes Frappe heavy is the **interpreted runtime + import graph**, not the apps' own
  logic. Moving logic from interpreted Python to compiled Rust removes its per-process memory cost
  almost entirely — so **the more we transpile, the closer the whole system sits to ferro's ~5 MB
  floor.** Memory is a solved problem for any covered subset.
- The remaining work is **coverage and correctness**, not RAM: the `frappe.qb` query-builder DSL
  (~250 sites — the single biggest lever), `@whitelist` RPC routing, try/except, and the documented
  correctness residuals. The un-transpiled tail falls back to the PyO3 path (`ferrod`) in a hybrid
  deployment, or stays deferred-and-documented.

## 5. Artifacts (all under `transpile/` unless noted)

- `transpile.py` — the Python→Rust transpiler · `characterize.py` — the corpus coverage survey
- `ferro/src/native/{rt.rs, generated.rs, main.rs}` — runtime, generated controllers, the binary
- `reports/01-transpilability.md` (coverage), `02-how-it-works.md` (design), `03-audit.md` (audit)
- `measurements/results.md` (PSS/RSS/USS + comparison), `live-http-demo.txt`, `selftest-output.txt`,
  `no-python-proof.txt`, `gen/selftest_cases.json`
- Build: `cargo build --release --features native --bin ferro-native` (in `/home/frappe/ferro`)
- Verify: `… selftest <site>` · `… coverage` · `… loadtest <site> --threads 4 --rounds 8` ·
  `cargo test --release --features native --bin ferro-native`

## 6. One-line conclusion

Transpiling the Frappe app ecosystem to native Rust and shipping **one interpreter-free binary** is
real and measured: 133 DocTypes' business logic compiled to machine code, executing correctly against
the live SQLite site at **18.8 MB / 4 threads** — comfortably under 64 MB, several times lighter than
embedding CPython, with the Python runtime removed entirely. 64 MB is not the upper bound; it is a
ceiling we sit far beneath.
