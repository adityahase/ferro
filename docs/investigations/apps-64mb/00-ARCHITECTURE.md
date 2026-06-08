# Running Frappe apps on ferro in 64 MB — architecture

**Question.** ferro is a Rust reimplementation of the Frappe framework's data plane. Frappe's
value is its *app ecosystem* (erpnext, hrms, crm, helpdesk, gameplan, …) — Python codebases
that call `frappe.*` and subclass `frappe.model.document.Document`. How do we run those apps on
ferro **without** paying Frappe's memory cost, fitting framework + apps in **64 MB**?

**Answer, in one line.** *Make the apps import a thin native `frappe` shim instead of the real
framework.* Frappe's memory is almost entirely the framework's own import graph (measured
+106 MB); the apps' own code is small (measured 5–54 MB). When **ferro is the framework**, the
Python side loads only the interpreter + a shim + the app controllers it actually touches — and
that fits 64 MB with room to spare. ferro serves the ~80% of requests that are pure CRUD with
**zero Python**, and invokes Python only for the doctypes that carry server-side logic.

This document is grounded in direct measurement on this box (CPython 3.14.4, ferro release) and
in reading the Frappe 17 source + the five apps. See `01-MEASURED-FLOOR.md` for raw numbers.

---

## 1. Why this is even possible — the cost is the framework, not the apps

The prior runtime-memory study (`runtime-memory-investigation/`) established, by measurement:

| | RSS | modules |
|---|--:|--:|
| bare CPython 3.14 | 12 MB | 64 |
| `import frappe` + `frappe.app` | **118 MB** | 1620 |
| full warm worker | 154 MB | 1730 |

The jump from 12 → 118 MB is a **+106 MB cliff** the moment you import the framework. ~43 MB of
it is the marshalled **code-object graph** of frappe + its dependency stack (werkzeug, jinja2,
redis, pymysql, requests, num2words, whoosh, pydantic, cryptography, lxml, …). None of that is
*app* code; it's the framework and its web/db/email/PDF machinery.

**ferro already replaces every one of those subsystems in Rust** (`orm.rs`, `meta.rs`,
`naming.rs`, `auth.rs`, `crypto.rs`, `main.rs` — HTTP, routing, ORM, query building, metadata,
naming, auth, permissions). So the apps don't need the framework loaded in Python at all. They
need an *interface* — the `frappe.*` names and the `Document` base class — that delegates into
ferro.

I measured the app side directly, by loading every app controller against a thin `frappe` stub
(no real framework, no heavy deps):

| Loaded (thin shim) | RSS |
|---|--:|
| bare interpreter + shim | 11 MB |
| crm / gameplan / helpdesk (all controllers) | 14–16 MB |
| hrms (all 152 controllers) | 20 MB |
| **erpnext (all 514 doctype controllers)** | **38 MB** |
| erpnext — *every* non-test module eager (2268) | 54 MB |
| erpnext — hot working set (lazy) | ~22 MB |

ferro itself serves the data plane in **4–8 MB RSS** (1.67 MB binary). So:

```
ferro (Rust framework)            6–8 MB
CPython interpreter + frappe shim  11 MB
app controllers (erpnext, lazy)  10–27 MB
                                 ─────────
TOTAL resident                   ~28–46 MB   →  18–36 MB headroom under 64 MB
```

Even the pathological "every erpnext module eagerly resident" ceiling is ~54 MB Python + 8 MB
ferro ≈ 62 MB — still under 64. The budget works. The rest of this document is *how*.

---

## 2. The shape of the integration

### 2.1 Process model — embed CPython in ferro (PyO3)

ferro embeds a CPython 3.14 interpreter **in-process** via PyO3 (link `libpython`, or
`pyo3` + `auto-initialize`). One address space holds both the Rust framework and the Python
app layer. This is the most memory-efficient option (no IPC buffers, no doc serialization
across a socket, and the Document proxy can hold a raw pointer to Rust-owned field data).

Rationale vs alternatives (full scoring is in §7, decided by the workflow judge panel):
- **Embedded (PyO3)** — chosen. Lowest memory; zero-copy doc access; Python invoked as a
  function call under the GIL only when needed. (PyO3 0.27+ supports Python 3.14 on the full
  native API; 0.28 supports the free-threaded build — see the §7 maturity note.)
- *Sidecar process* — viable and simpler to sandbox, but doubles the base RSS (two heaps),
  needs doc (de)serialization per call, and an IPC hop on every hooked write.
- *Sub-interpreters (PEP 684)* — interesting for concurrency; evaluated in §6.
- *Alt runtime (RustPython/WASM)* — breaks C-extension deps (cryptography, lxml, PIL) and app
  compatibility; rejected for the general case.

### 2.2 The `frappe` shim — a native module, not a Python package

The apps do `import frappe`. We make that resolve to a **native PyO3 module** (a Rust
`#[pymodule]` registered in `sys.modules['frappe']` before any app import), plus a *small*
amount of pure-Python glue for the genuinely-Python bits (the `Document` base class body, the
`frappe.utils` helpers). Crucially, the shim pulls in **none** of the framework's heavy deps.

The shim's surface is dictated by what the apps actually use. Measured frequency across all five
apps (top of a long tail):

```
3257 frappe.get_doc        2299 frappe.db.get_value   1525 frappe.qb.DocType
2330 frappe.throw          1268 frappe.get_all        1264 frappe.whitelist
1564 frappe._dict          1162 frappe.db.exists       976 frappe.qb.from_
 848 frappe.db.sql          822 frappe.bold            804 frappe.db.set_value
 762 frappe.utils.*         746 frappe.get_cached_value 672 frappe.new_doc
 567 frappe.ValidationError 438 frappe.db.set_single_value  367 frappe.db.get_single_value
 361 frappe.msgprint        358 frappe.session.user    355 frappe.delete_doc
 ... has_permission, get_meta, get_roles, scrub, parse_json, generate_hash, enqueue, log_error
```

Every one of these maps to a category:

| shim symbol(s) | backed by | notes |
|---|---|---|
| `get_doc`, `get_all`, `get_list`, `new_doc`, `get_value`, `delete_doc`, `get_last_doc`, `get_single`, `get_cached_*`, `rename_doc`, `copy_doc` | **ferro ORM** (`orm.rs`) | direct call into Rust; returns a Document proxy or `_dict` |
| `db.get_value`, `db.set_value`, `db.exists`, `db.count`, `db.get_all`, `db.get_single_value`, `db.set_single_value`, `db.get_values`, `db.delete`, `db.commit`, `db.rollback`, `db.escape`, `db.has_column` | **ferro ORM / SQLite** | thin Rust methods on a `frappe.db` object; `commit`/`rollback` map to ferro's `with_txn` boundary |
| `db.sql(...)` | **ferro SQLite passthrough** | the hard case — raw SQL; see §5 |
| `qb.*` (pypika query builder) | **ferro query builder** | re-expose a builder that compiles to ferro's filter/SQL layer; see §5 |
| `throw`, `msgprint`, `bold`, `ValidationError`, `PermissionError`, `log_error` | **pure Rust/Python** | exceptions + message buffer threaded into ferro's response envelope |
| `_dict`, `parse_json`, `scrub`, `generate_hash`, `utils.*` | **pure Python** in the shim | port Frappe's small pure-python helpers (date math, cstr/cint/flt, etc.) — no deps |
| `session`, `flags`, `local` | **request context** | ferro fills per-request; exposed as attributes |
| `whitelist` | **decorator** | marks a function callable via `/api/method/...`; ferro routes to it |
| `has_permission`, `get_roles`, `get_meta`, `get_precision` | **ferro auth/meta** | call into `auth.rs` / `meta.rs` |
| `enqueue`, `publish_realtime`, webhooks, notifications | **ferro queue/no-op** | background path; see §8 |

The `frappe.utils` namespace (762 hits) is the one chunk of *real Python* worth porting verbatim
— it's dependency-free helper code (`cint`, `flt`, `cstr`, `getdate`, `add_days`, `nowdate`,
`fmt_money`, …) and it's small. Everything else is a Rust-backed thunk.

### 2.3 The `Document` base class — a Rust-backed lazy proxy

Apps subclass `frappe.model.document.Document` and lean on a large contract. The methods app
controllers actually call (from reading the 2685-LOC base class + grepping the apps):

- **Field access**: `self.<field>`, `self.get(f)`, `self.set(f, v)`, `self.append(table, row)`,
  `self.as_dict()`, `self.get_all_children()` — served by a proxy over **Rust-held field data**
  (the doc is materialized once in Rust; Python reads/writes through `__getattr__`/`__setattr__`).
- **State queries**: `self.is_new()`, `self.has_value_changed(f)`, `self.get_doc_before_save()`,
  `self.get_value_before_save(f)`, `self.docstatus`, `self.flags`, `self.meta` — `is_new`/`flags`
  are pure in-memory; `has_value_changed`/`get_doc_before_save` need the pre-save DB snapshot
  (one Rust read).
- **DB writes**: `self.db_set(f, v)`, `self.save()`, `self.insert()`, `self.submit()`,
  `self.delete()`, `self.reload()` — delegate to ferro ORM.
- **Triggers**: `self.run_method(m)` — the hook dispatcher (§3).

The proxy means a Document is **not** a fat Python dict per request; it's a small Python object
wrapping a Rust handle, so per-request Python allocation stays tiny — which is what keeps the
headroom in §1 from being eaten by request churn.

---

## 3. The controller-event protocol — when ferro calls Python

This is the precise contract, read from Frappe 17 `document.py`. ferro must drive this sequence
for **writes to doctypes that have Python**, running the framework steps in Rust and calling into
Python at the named trigger points. (Read paths — get/list — never touch Python.)

**INSERT** (`Document.insert`, lines 647–736):
```
_set_defaults · set_user_and_timestamp · set_docstatus        [Rust]
check_permission("create") · check_if_latest · _validate_links [Rust]
run_method("before_insert")                                    →PY (if present)
set_new_name (naming) · set_parent_in_children                 [Rust]
in_insert=1
run_before_save_methods:
    run_method("before_validate")                              →PY
    run_method("validate")                                     →PY
    run_method("before_save")                                  →PY
_validate (field/mandatory/options validation)                 [Rust]
db_insert (parent) + children db_insert                        [Rust]
run_method("after_insert")                                     →PY
run_post_save_methods:
    run_method("on_update")                                    →PY
    notify_update · save_version                               [Rust/no-op]
    run_method("on_change")                                    →PY
```

**SAVE** (update): `before_validate · validate · before_save` → `[Rust UPDATE]` →
`on_update · on_change`.
**SUBMIT**: `validate · before_submit` → `[docstatus=1 write]` → `on_update · on_submit`.
**CANCEL**: `before_cancel` → `[docstatus=2 write]` → `on_cancel`.

`run_method(m)` (line 1579) does three things ferro must replicate:
1. call the controller's own `m` if defined;
2. via the `Document.hook` decorator, call every **hooks.py `doc_events`** function registered
   for `(doctype, m)` — *this is why a doctype with a pure-CRUD controller can still need
   Python* (e.g. crm's `hooks.py` attaches `validate`/`on_update` to `Contact`, `ToDo`, `Item`);
3. run notifications / webhooks / server-scripts (mostly no-ops on a minimal deployment).

**Design consequence — the needs-Python registry.** At boot (or lazily), ferro builds, per
doctype, the set of lifecycle events that have *any* Python attached = `{events the controller
defines}` ∪ `{events any app's hooks.py doc_events registers}`. For a write:
- if the set is empty for the relevant events → **ferro runs the whole write in Rust, no GIL,
  no Python** (the common case);
- otherwise ferro drives the sequence above, acquiring the GIL and calling Python only at the
  populated trigger points, with the doc proxy shared.

---

## 4. The selective-Python dividend — most requests need no Python

An AST classifier over all five apps (reproduced by the verifier, off by 1 vs the first pass)
gives the **pure‑CRUD vs needs‑Python split per app**:

| app | doctypes | needs‑Python | pure‑CRUD | @whitelist RPCs |
|---|--:|--:|--:|--:|
| crm | 44 | 19 | 18 | 108 |
| helpdesk | 40 | 21 | 19 | 81 |
| gameplan | 27 | 15 | 12 | 73 |
| hrms | 160 | 86 | 74 | 214 |
| erpnext | 527 | 221 | 300 | 788 |
| **total** | **798** | **362** | **423 (≈53%)** | **1264** |

So **~53% of doctype controllers are pure‑CRUD** — ferro serves them, and *all reads*, in Rust
with zero Python. The needs‑Python tail is concentrated in transactional doctypes (314 `validate`,
106 `on_submit`, 100 `on_cancel`, …) and the 1264 `@frappe.whitelist` RPCs.

**Two verified caveats sharpen this:**
- The registry in §3 must count `hooks.py` `doc_events` (incl. `'*'` wildcards), not just
  controller methods — so the effective needs‑Python set is a bit larger than the table's column.
- **Reads are *not* universally Python‑free:** doctypes with `permission_query_conditions`
  (crm Lead/Deal, helpdesk Ticket, gameplan) return a Python‑computed SQL `WHERE` that ferro must
  splice into the list query (risk #2). For everything else, reads are pure Rust.

This is the throughput story under the GIL: the GIL only ever serializes the needs‑Python tail;
all reads (bar the `permission_query_conditions` doctypes) and all pure‑CRUD writes run in Rust.

---

## 5. The hard parts — raw SQL and the query builder

Two parts of the surface don't reduce to structured ORM calls:

- **`frappe.db.sql(...)`** — 848 call sites. Raw SQL strings, some with joins/subqueries/
  GROUP BY. ferro is SQLite-backed, so a `db.sql` shim can **pass the string through to SQLite**
  and return rows (as tuples or `_dict`s, matching Frappe's `as_dict`/`as_list`). The risk is
  MariaDB-only SQL (backtick quoting, vendor functions) — but a Frappe *SQLite* site already
  requires SQLite-compatible SQL, so apps that target SQLite are fine, and ferro can normalize
  the common dialect differences (backticks → double-quotes, `IFNULL`/`UTC_TIMESTAMP`, etc.).
- **`frappe.qb`** (pypika, 1525+ hits) — a fluent query builder. The shim re-implements the
  small builder surface the apps use (`qb.from_(DocType).select(...).where(...).run()`),
  compiling to SQLite. pypika itself is pure-Python and not huge, so a fallback is to ship a
  trimmed pypika in the shim and point its execution at ferro's connection.

These are the two areas to validate hardest (the workflow's db-surface agent + verifier quantify
what fraction needs raw passthrough vs structured calls, and what breaks).

---

## 6. Concurrency & the GIL

- **Reads / pure-CRUD writes**: handled by ferro's Rust threads against SQLite (WAL), no GIL.
- **Needs-Python writes**: serialize on the single GIL. For a write-storm on a logic-heavy
  doctype (e.g. erpnext Sales Invoice submit), throughput is bounded by one Python core. Options,
  in increasing complexity: (a) accept it — write-heavy bursts on logic doctypes are the minority
  of traffic; (b) **pre-fork worker processes** each with its own interpreter + COW-shared shim
  and code graph (the prior study measured `--preload` COW at −48% pool PSS — the shim+app code
  graph is shared, only per-request pages are private); (c) **PEP 684 sub-interpreters** with
  per-interpreter GIL for true in-process parallelism — evaluated by the workflow; the cost is
  per-subinterpreter duplication of mutable module state, partly mitigated on 3.14.
- **Memory under concurrency**: option (b) is the sweet spot — N small workers, each ~the §1
  budget, COW-sharing the code graph, so the marginal worker is mostly per-request private pages.

---

## 7. Architecture options — scored

A judge panel designed and scored five approaches against the verified findings; each verdict
was then adversarially attacked. Scores 0–100.

| Approach | Score | Fits 64 MB | Unmod. compat | Verdict |
|---|--:|---|---|---|
| **Embedded CPython (PyO3) + native shim** | **74** | **yes** (~36 MB realistic) | ~80–88% | **RECOMMENDED** |
| Sidecar process (one warm interpreter, UDS/shm) | 62 | yes | ~70–80% | viable — choose **only** for fault isolation; pays IPC + GIL tax |
| Sub-interpreters (PEP 684, per-interp GIL) | 38 | **no** | ~70–80% | reject — **~14 MB each, no code sharing on 3.14**; N=2 > 64 MB |
| Alt runtime (RustPython / WASM) | 38 | yes (box) | ~50–65% | reject — **last-ULP financial divergence** (no CPython `_decimal`) + C‑ext cliff |
| AOT declarative extraction | 34 | tight | ~98% (via tail) | reject standalone — ferro **already** does declarative validation; net gain single‑digit % |

**Winner: a single embedded CPython 3.14 interpreter inside ferro (PyO3), one GIL, serving the
needs‑Python tail behind ferro's Rust data plane**, with scheduler/queue work in a separate
out‑of‑budget process. Measured embed cost: `Py_Initialize` = 11.7 MB RSS / 37 modules (≈ bare
`python -c`); full ~2 kLOC shim +1.4 MB. Realistic disciplined footprint **~36 MB**
(interp 11 + Rust 8 + shim 1.4 + lazy app code 6 + per‑request 4 + fragmentation 6) — real
headroom under 64 MB.

Why not the others, in one line each: **sub‑interpreters** don't share code objects on 3.14 so
memory scales linearly (decisive); **alt‑runtimes** can't reproduce CPython's accounting math
bit‑for‑bit and kill every C extension (disqualifying for erpnext/hrms); **AOT** re‑derives work
ferro already does and adds a dual‑implementation correctness hazard; **sidecar** is memory‑fine
but turns every in‑process call into a 33–79‑hop IPC fan‑out (~2 ms/write) and risks holding a
SQLite write txn open across the wire.

> **Stale‑risk correction (verified mid‑2026):** earlier PyO3 couldn't build against 3.14.
> **PyO3 0.27+ now supports Python 3.14 final on the full native API**, and **0.28 supports the
> free‑threaded build (3.14t)** — so native `#[pyclass]` Document proxies and a `#[pymodule]`
> `ferro_rt` are first‑class, and free‑threading is a real (if memory‑costed) path out of the
> GIL serialization in §6.

## 8. Out‑of‑request work — scheduler, jobs, reports, print

Not everything is on the hot request path; push it out of the 64 MB serving budget:

- **`scheduler_events` + `frappe.enqueue`** (erpnext alone has 50+ `daily_maintenance` entries,
  plus hourly/weekly/monthly/cron) run in a **separate background worker process**, not the
  serving process. They never need to be resident while serving HTTP, so they don't count
  against the request budget. ferro exposes an `enqueue` shim that drops jobs onto a queue the
  worker drains.
- **Reports / dashboards** are the heaviest raw‑SQL/`frappe.qb` consumers — route them through
  the `db.sql` passthrough (§5) and, if needed, run them in the background worker so a heavy
  aggregate never blocks the serving interpreter.
- **Jinja / email / `safe_eval`** are real subsystems a minority of features need (hd_ticket
  agent reply uses bs4 + Jinja + sendmail; salary_slip and SLA use `safe_eval`). Provide them in
  the shim **lazily**: a Jinja env (~1 MB) loaded on first `render_template`; `sendmail` handed
  to the background worker; `safe_eval` as a restricted‑builtins `eval` (CPython‑native, so
  semantics match). These are deliberately deferred, not stubbed‑and‑forgotten.

## 9. Risk register (all confirmed against source)

1. **Wildcard `*` doc_events make selective‑Python data‑dependent.** `document.py:1982` dispatches
   `doc_events['*'][method]`; erpnext registers `'*'.validate` (SLA apply + deletion‑lock),
   gameplan `'*'.on_trash` (cascade delete). **ferro cannot prove any write/delete is Python‑free
   without consulting the merged hook table** — a naive Rust fast path silently drops SLA/cascade
   logic and **corrupts data**. *Mitigation:* build the merged hook registry at boot; gate the
   Rust fast path on "no controller method **and** no `(doctype | '*')` hook for this event."
2. **`permission_query_conditions` means some READS need Python too.** crm (Lead/Deal), helpdesk
   (Ticket), gameplan return a SQL `WHERE` fragment to splice into list queries. So the "reads are
   always Python‑free" simplification is false for these doctypes. *Mitigation:* the list path
   calls Python for doctypes in this registry and injects the returned predicate.
3. **Raw‑SQL / `frappe.qb` passthrough is a from‑scratch subsystem.** ferro's `get_list` is
   single‑table (orm.rs:288); JOINs (727 qb + ~157 raw), GROUP BY/aggregates (199 + 225),
   aliased/computed columns (1022 `.as_`), subqueries (84), window funcs (8) have no ORM analog.
   erpnext is 718/848 of raw `db.sql`. *Mitigation:* add `db.sql(string)` passthrough to ferro's
   SQLite; `frappe.qb` already materializes a SQL string via `frappe.local.db.sql(q.get_sql())`
   and is **dialect‑aware** (emits SQLite on a SQLite site), so most of it "just executes." The
   residual is hand‑written MariaDB‑isms in raw `db.sql` (a correctness long tail, erpnext‑heavy).
4. **Eager‑import trap — lazy discipline is mandatory.** One module‑level `import requests` =
   **+25 MB / 435 modules**; found eager in `hrms/utils/__init__.py`, `gameplan/.../gp_project.py`
   (+bs4), `crm/api/__init__.py` (+bs4). *Mitigation:* the same lazy‑import refactor the prior
   study applied to the framework (move feature imports into functions); without it a single hot
   controller can push past 64 MB.
5. **SQLite single‑writer bounds write throughput regardless of model.** Every write is
   `BEGIN IMMEDIATE` (orm.rs:51); SQLite allows one writer. *Mitigation:* keep Python‑driven
   transactions short; per‑tenant DB files give per‑tenant write parallelism. (This — not the GIL —
   is the real write ceiling; document it.)
6. **`self.meta` is the largest contract surface.** Pervasive `self.meta.get_field/get_label/
   precision/get_table_fields` + ~15 engine‑only methods. The Rust meta layer must expose faithful
   `DocField` objects (fieldtype/options/permlevel/reqd/fetch_from/default/…) to Python or
   validation/precision/serialization break.
7. **`_doc_before_save` loads a full second Document per non‑new save** (`has_value_changed`,
   `validate_set_only_once`, `db_set` depend on it) — an extra `get_doc` round‑trip and a doubled
   in‑memory doc graph. *Mitigation:* a lighter Rust‑side "values‑before‑save" fetch that still
   quacks like a doc for `.get(field)`.
8. **`override_doctype_class` / `extend_doctype_class`** (crm Contact/Email Template; hrms
   Employee→EmployeeMaster, Payment Entry→EmployeePaymentEntry, …) require instantiating the
   **Python subclass**, which drags the heavy base‑controller import closure when touched (memory
   on first use). ferro must resolve doctype→Python class via the merged registry.
9. **hrms requires erpnext co‑residency** (`required_apps=[erpnext]`, 176 module‑level erpnext
   imports, `Employee` lives in erpnext) — "run hrms" means hosting erpnext's Python too. Still
   fits under lazy loading, but budget for the combined controller set.
10. **Side‑effect subsystems** (`notify_update`, `save_version`, `update_global_search`,
    webhooks, notifications) are safe to no‑op for CRUD correctness but silently disable features
    (audit history, search index, alerts). Flag as **deliberately deferred**, wire to Rust later.
11. **The GIL serializes the Python tier** (measured 1.00× across 4 threads). See §6 — throughput
    comes from pre‑fork COW workers or the free‑threaded 3.14t build, not from one interpreter.

## 10. Phased implementation plan

**Phase 0 — Land the easy 3 apps first.** crm / helpdesk / gameplan are nearly raw‑SQL‑free,
have light dynamic dispatch, and score ~90% compat. Prove the model end‑to‑end here before
erpnext's long tail.

**Phase 1 — The `frappe` shim + embed.** Embed CPython 3.14 via PyO3 0.27. Ship: native
`#[pymodule] ferro_rt` (get_doc/get_all/get_value/exists/set_value/count/insert/update/delete/
naming/has_permission/get_roles/get_meta → ferro Rust); pure‑Python `Document`/`BaseDocument`
(the lifecycle state machine + field proxy, ~a few hundred LOC) backed by Rust leaves; `_dict`,
the exception hierarchy, `frappe.flags`/`session`/`local`, `whitelist`, and a verbatim port of
`frappe.utils` numerics (`flt`/`cint`/`cstr`/`getdate`/… — CPython‑native, byte‑identical).

**Phase 2 — The merged hook registry + selective dispatch.** Parse every installed app's
`hooks.py` into one table: `doc_events` (incl. `'*'` and tuple keys), `override_doctype_class`/
`extend_doctype_class`, `permission_query_conditions`, `has_permission`,
`override_whitelisted_methods`. ferro drives the §3 write protocol, taking the **Rust fast path
only when the registry proves the event is Python‑free**, and calls Python at populated triggers.

**Phase 3 — `db.sql` passthrough + `frappe.qb` execution.** Expose `frappe.db.sql(string, params)`
executing against ferro's SQLite connection, returning rows as tuples / `_dict`s
(`as_list`/`as_dict`). `frappe.qb` runs unmodified (it emits the string). This unlocks
erpnext/hrms reports & dashboards.

**Phase 4 — Lazy‑import discipline.** Defer the eager heavy imports (requests/bs4/twilio/…) per
risk #4. Establishes the budget guarantee under real controllers.

**Phase 5 — Out‑of‑budget worker.** Separate process for `scheduler_events` + `enqueue` + heavy
reports + email; ferro's `enqueue` shim feeds it.

**Phase 6 — erpnext/hrms heavy tail.** Regional overrides (`@allow_regional` via
`get_hooks('regional_overrides')` at call time), the `safe_eval` salary/SLA engines, Jinja/email
for ticket replies, and the MariaDB‑dialect long tail in hand‑written `db.sql`.

**Throughput track (parallel to all phases).** Run **N pre‑forked workers** (master imports the
shim + hot controllers once; workers COW‑share the code graph — the prior study measured `--preload`
at −48% pool PSS), or evaluate the **free‑threaded 3.14t** build (PyO3 0.28) to get true in‑process
Python parallelism, trading some per‑object memory for escaping the single GIL.

---

### Bottom line
Running the Frappe app ecosystem on ferro in 64 MB is **feasible and measured**, because the cost
that makes Frappe heavy is the *framework's* import graph (+106 MB), which a Rust framework + thin
shim eliminates. The apps' own code is 5–54 MB and loads lazily. ferro serves ~53% of doctypes
(all reads, all pure‑CRUD writes) with **zero Python**; the embedded interpreter runs only the
needs‑Python tail, against a Rust‑backed shim, with the doc materialized once in Rust. The work
is real — a hook registry, a `db.sql` passthrough, lazy‑import hygiene, and a faithful `Document`
contract — but none of it is a memory problem. The remaining hard edges are **correctness**
(wildcard hooks, SQL dialect) and **write throughput** (SQLite's single writer + the GIL), not RAM.
