# ferro ↔ Frappe framework compatibility — test-driven audit

**Goal:** measure how well pure-Rust **ferro** matches the Frappe framework, by running Frappe's own
test suite against it, judging per test class how strictly ferro *should* match, and producing a
precise fix plan — while keeping ferro minimal and low-footprint.

> **Status (applied):** the fix plan in `06-fix-plan.md` (FIX-1..FIX-9 + the behavioral appendix)
> has been **implemented**. See `08-implementation-status.md` for the per-fix landing record,
> regression locks, and the deliberately scoped-out items. `_specs/` holds the transcribed
> Frappe-source specs the behavioral fixes were built against. Regression suite:
> `measurements/verify.py` — **105 passing, 0 failing**, deterministic.

**Date:** 2026-06-10 · **Frappe:** v17 (17.0.0-dev) · **Bench:** `bench-cpython314`, SQLite,
only `frappe` installed · **ferro:** working tree (default pure-Rust build).

## Read in this order
| File | What |
|---|---|
| `00-methodology.md` | How the suite was run against ferro (HTTP differential vs a CPython oracle), harness, benches, limits, reproduce steps. |
| `01-http-differential-results.md` | Empirical per-test results: the **15 real ferro gaps** vs 59 environment/harness artifacts. |
| `02-failure-hierarchy.md` | All failures as a 2-level **broad → sub** problem tree. |
| `03-test-class-judgments.md` | Every test **class** rated **1:1 / mostly / somewhat / shouldn't-care** (50 in-scope, 350 out). |
| `04-behavioral-findings.md` | Behavior-by-behavior MATCH/PARTIAL/GAP for ferro's core domains (ORM filters, document lifecycle, naming, perms, meta, DB API, REST envelopes, client methods) with file:line. |
| `05-discovered-behaviors.md` | Cross-cutting divergences / sharp edges (role bug, no sessions, login stub, desk auth posture). |
| `06-fix-plan.md` | Prioritized fixes with code samples, line numbers, function names, regression locks. |
| `07-baseline-and-apps.md` | In-process CPython baseline (env noise) + why app-level suites are N/A here. |
| `_raw/` | Raw machine outputs: `classify_*.json` (per-class judgments), `behavior_*.md` (deep-dives). |

## Executive summary

**Method.** Frappe's test client was redirected (a `get_test_client` shim with live token-auth
bridging) to hit a live **ferro** server AND a live **CPython Frappe** server (the *oracle*) with the
**identical** harness, on isolated DB copies (the real bench kept byte-for-byte pristine). A test that
passes on the oracle but fails on ferro is a genuine ferro shortfall; one that fails on both is an
environment/harness artifact. The 276-file suite was also classified for relevance, and ferro's core
domains were audited behavior-by-behavior against the spec tests + ferro source.

**ferro's scope is deliberately narrow, and that's correct.** ~87% of Frappe's test surface
(350/400 classes) tests Python subsystems ferro intentionally doesn't implement (email, workflow,
reports/print, OAuth/social, server scripts, web forms, background-Python, dev tooling) or pure
in-process internals never exposed over HTTP. ferro's real obligation is the **~50 in-scope classes**:
the REST/ORM/auth/perm/naming/meta core.

**On the core, ferro is largely faithful but has a focused set of real gaps.** Of the HTTP-testable
surface: **49 pass on both**, **15 are real ferro gaps**, the rest are environment/harness artifacts
(notably SQLite dual-process write contention, which 500s on the oracle too).

### The headline findings (severity)
0. **🔴🔴 `frappe.client.*` reads bypass the permission gate (P0/security, most severe).** The
   `/api/method/frappe.client.get_list|get|get_value|…` desk path reads with `ReadAcl::all()` and never
   calls `auth::permission` — so any user, including unauthenticated **Guest, reads User email
   addresses** (confirmed: 403 on `/api/resource/User` but 200 on `/api/method/frappe.client.get_value`).
   Active whenever `--desk` is on (Desk/SPA/signup). The `/api/resource` + `/api/v2` CRUD paths DO
   enforce perms; only this desk-method path doesn't. Fix FIX-9 (thread the perm context into
   `desk::route_method`).
1. **🔴 Guest over-permission (P0/security).** `auth.rs:144` grants the `All` role to *Guest*, so
   Guest can read any doctype whose DocPerm grants `All` (e.g. ToDo). Also logged-in users wrongly
   miss the implicit `Guest`/`Desk User` roles. Small, clear fix (FIX-1).
2. **🟠 No session/sid auth + non-authenticating `login` stub (architectural).** ferro has no session
   store; `login` returns "Logged In" for *any* password and issues a meaningless `sid` cookie.
   Cookie/session clients (FrappeClient password login, non-admin Desk) can't work. Decision: build a
   minimal `tabSessions` validator (FIX-2A) or formally scope sessions out and harden the stub (FIX-2B).
3. **🟡 REST feature gaps.** `expand`/`expand_links` (FIX-3), v2 bulk operations (FIX-4), a few missing
   whitelisted methods incl. `frappe.realtime.get_user_info` (FIX-5), `debug`→`_debug_messages` (FIX-6).
4. **🟡 ORM fidelity gaps** (from the behavioral deep-dive): `filters`/`or_filters` as dict-lists,
   `in/not in` with JSON-encoded lists and with `None`, child-table autoname, ISO `WW` week number,
   link validation, `set_only_once`, optimistic-lock (`modified`) check. All small, localized fixes.
5. **🔵 Guardrail:** `ferro serve --desk` silently runs every request as Administrator (FIX-7).

**Write path is fine.** The create/update/delete 500s in the differential are SQLite write-lock
contention from the in-process test harness (identical on the oracle), not ferro — confirmed by a
standalone create returning 200 and by `measurements/verify.py`.

**Bottom line.** ferro matches the Frappe REST/ORM/perm/naming contract on the large majority of
in-scope behaviors. Closing the small, well-localized list in `06-fix-plan.md` — led by the Guest-role
fix and a sessions decision — would make ferro indistinguishable from gunicorn for the token-auth CRUD
+ permission + naming surface that real clients and frappe-ui frontends depend on.
