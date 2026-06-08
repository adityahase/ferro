# ferro-native — adversarial audit & fixes

A multi-agent audit (8 parallel auditors → adversarial verify → synthesize) was run against the
transpile demo across four dimensions: **memory honesty, coverage accuracy, transpiler soundness,
and semantic correctness** (generated Rust diffed line-by-line against the original Python, over four
disjoint app batches). Every auditor independently ran the binary and/or re-derived numbers. This
report records what they confirmed, what they found wrong, and exactly what was fixed.

## Verdicts by dimension

| Dimension | Verdict |
|---|---|
| **Memory honesty** | **Holds.** Binary independently confirmed 2.09 MB, `readelf` NEEDED = libgcc_s/libm/libc only, zero Python symbols. Peak-under-load reproduced (1T 7.9, 4T 18.2, 8T 32.1, 16T 60.5 MB). `smaps_rollup` USS=Private_Clean+Private_Dirty is the textbook definition. DB genuinely has all 5 apps (1077 doctypes). Load genuinely exercises transpiled logic (Coupon Code/Price List verified). |
| **Coverage accuracy** | **Holds, with a corrected headline.** The 1276/170/137 emission counts were exactly reproducible, and `characterize.py`'s 57%/64%/51% reproduce to the digit. BUT the audit showed only **~319 of 1276 emitted methods are reachable** (the rest are transpiled `@whitelist` RPCs with no dispatch route). Fixed: the demo now reports reachable (311) vs emitted (1274) separately. |
| **Transpiler soundness** | **Multiple silent-miscompile classes found** (compile-clean but wrong). The serious ones are now fixed; the rest are documented. |
| **Semantic correctness** | **Most dispatched handlers faithful**; a handful of real divergences (data-affecting) found and fixed or documented. |

## Confirmed-and-FIXED (with verification)

Each fix is locked in by a Rust unit test (`rt.rs` `mod tests`, 5/5 green) and/or the 7-case selftest
(still 7/7) and a grep of `generated.rs`.

| # | Severity | Bug (audit evidence) | Fix | Verified |
|---|---|---|---|---|
| 1 | **critical** | `min(list)`/`max(list)` lowered to `py_min(&[list])` which reduced over the *args slice* → returned the whole list. Live via Job Card `before_save` → wrote an array into date/qty fields. | single-arg `min`/`max` → `py_min_iter`/`py_max_iter` (reduce over elements). | unit test `min_max_over_list`; 9 call-sites now `*_iter`. |
| 2 | **critical** | `localdoc.append("table", row)` (2-arg) mistaken for `list.append` → pushed the **table-name string**, dropped the row dict. Verified in helpdesk `hd_team.sync_users`. | 2-arg `.append` on a local → `rt::append_to(&mut local, table, row)`; 1-arg stays `list_push`. | unit test `child_append_to_local`; 2 `append_to` sites. |
| 3 | **critical** | `super()`-only / `pass` lifecycle handlers were lowered to no-ops **but still wired** as `Ok(true)` → e.g. Employee Onboarding/Separation `on_submit`/`on_cancel` silently did nothing *and* blocked fallback. | a handler is wired only if **effectful** (body does more than `let _ = Value::Null;`). 8 trivial handlers / 4 doctypes dropped (now correctly fall back). | coverage 170→162 handlers, 137→133 doctypes; boarding `on_submit` no longer in dispatch. |
| 4 | **high** | `frappe.throw(msg=…, title=…)` keyword form → threw an **empty string** (only `args[0]` was read). Broad pattern (Batch, payment_entry, stock_entry…). | throw/msgprint now read `msg=`/`message=` kwargs when no positional. | **0** empty `rt::throw(&(rt::s("")))` remain (was many); Batch carries its full message. |
| 5 | **high** | Negative modulo used `rem_euclid` → `7 % -3 = 1` (Python: `-2`). | Python-semantics `%` (sign of divisor). | unit test `python_negative_modulo`. |
| 6 | **high** | `str.format`/f-string format-specs (`{:.2f}`, `{:.0f}`, `{:.1%}`, `{:,}`) and `!r/!s` were stripped → raw numbers in user messages. | `rt::fmt_value(val, spec)` implements `.Nf`/`.N%`/`,`/`d`; f-strings & `.format` apply it. | unit test `format_specs`. |
| 7 | medium | Chained comparison `a < b < c` textually duplicated the middle operand → double-evaluated its side effects (e.g. a DB call run twice). | operands bound to temps once, referenced twice. | inspected generated output; no duplicated middle term. |

## Confirmed-and-DOCUMENTED (not fixed in this pass — bounded, lower-risk)

These are real divergences the auditors found; they are honest limitations of the demo, recorded so
they are not mistaken for correctness. None is a memory problem.

- **Reachability gap (coverage):** 963 transpiled methods are `@whitelist` RPCs with no dispatch
  route (ferro-native only routes the lifecycle). They compile (and prove the transpiler handles
  them) but can't be invoked yet. Honestly reported now; routing `/api/method/<dotted>` is the
  natural next feature.
- **Child row passed to a helper loses mutations:** `for d in self.assets: self.fill(d)` materialises
  `d` as a cloned `child_row`; mutations inside `fill` aren't written back (Asset Movement
  `source_location`, BOM `validate_operation_row`). Mutations *directly in the loop body* DO write
  back (child cursor). Bounded pattern; flagged.
- **`has_value_changed()` returns `true`:** correct for INSERT (new doc); on UPDATE it can run a
  guarded branch Frappe would skip (or skip an early-return). Needs a pre-save snapshot to be exact.
- **`self.flags.*` / `frappe.flags.*` read as falsy:** external-caller escape hatches
  (`ignore_mandatory`, …) never fire. Fine for direct REST writes; flagged.
- **A few non-dispatched `validate()` bodies not transpiled** (Item Price, Stock Reposting Settings):
  their `before_save` is wired but the separate `validate` hits an un-transpilable construct, so those
  specific validations are skipped in the native path (would fall back to PyO3).
- **`flt(x, p)` / `round()` use round-half-away-from-zero**, not banker's rounding, differing from
  Python on exact `.5` boundaries; `db.sql` doesn't translate `%s`/`%(name)s` placeholders (those
  queries error loudly rather than mis-bind); division by zero returns 0 instead of raising.
- **Idle baseline caveat:** the loadtest's `post-boot idle = 1.0 MB PSS` is sampled before any DB
  connection/metadata load; the honest "ready-to-serve" idle is the `measure` figure (~6.4 MB PSS).
  The headline **peak-under-load** numbers are unaffected and reproducible.

## What the audit did NOT find

No memory dishonesty, no fake "no-Python" claim, no fabricated coverage percentages, no corruption in
the *pure-CRUD* Rust data plane, and the majority of dispatched lifecycle handlers (validate/on_*)
faithfully reproduce their Python source operator-for-operator and message-for-message (the auditors
explicitly verified Operation, Workstation, Price List, Work Order, BOM, Job Card before_validate,
Item.on_trash, Asset Shift Factor, Stock Entry Type, Payment Order, Journal Entry Template, Attendance,
Leave Type/Ledger/Policy, Shift Assignment, Interview, and more).

## Net effect of the audit

Coverage headline corrected to **133 doctypes / 162 lifecycle handlers / 311 reachable methods**
(1,274 emitted, 963 not-yet-routed). Seven correctness bugs fixed and regression-tested; the binary
still links only libc, still passes 7/7 selftests, and peaks at **18.8 MB @4T (tuned) / 23.4 MB
(default)** under load — unchanged conclusion, higher confidence.
