# 02 — Failure hierarchy (broad problem → sub-problem)

Every failure surfaced by this audit (empirical HTTP differential + behavioral deep-dives + baseline),
organized two levels deep. Each leaf is tagged **[ferro]** (a real ferro shortfall) or **[env]**
(environment/harness artifact, not ferro), with severity and a pointer.

---

## 1. Authentication & sessions
- **1.1 Guest is granted the `All` role** — [ferro] **HIGH/security**. Guest reads any `All`-permitted
  doctype (ToDo/Note/…). `auth.rs:144`. → G1/D1, FIX-1. (test_api_v2 unauthorized; get_doc_expand)
- **1.2 No session/sid validation** — [ferro] **architectural**. `sid` cookie issued but never stored
  or checked; requests fall to `default_user`. `auth.rs` resolve_user, `main.rs:858`. → G2/D2, FIX-2.
- **1.3 `login` is a non-authenticating stub** — [ferro] **HIGH**. Returns "Logged In" for any
  password; ignores `allow_login_using_*`, `disable_user_pass_login`, `deny_multiple_sessions`.
  `desk.rs:583`. → D3, FIX-2. (test_auth ×5)
- **1.4 Login cookie has no expiry** — [ferro] Med. `attach_login_cookies` sets no `Expires`.
  → test_auth.test_correct_cookie_expiry_set.

## 2. Authorization / permissions
- **2.1 User Permissions not implemented** — [ferro] **Med (over-grants)**. `tabUser Permission` never
  consulted; ferro shows a superset of rows. `auth.rs`/`orm.rs`. → perm-G4.
- **2.2 DocShare not implemented** — [ferro] **Med (under-grants)**. `tabDocShare` ignored; shared docs
  return 403. → perm-G5.
- **2.3 Implicit `Desk User`/`Guest` roles missing on users** — [ferro] Med/Low. Folded into FIX-1.
- **2.4 Write-path permlevel masking absent** — [ferro] Med. Only reads are permlevel-masked; a
  permlevel-0-write user could write a higher-permlevel field. → perm-U1.
- **2.5 Controller `has_permission` hooks** — [ferro→ferrod] Low. No hook dispatch in pure Rust.
- **2.6 `frappe.client.*` read methods bypass the permission gate** — [ferro] **HIGH/security**. The
  desk-method path uses `ReadAcl::all()` + no `auth::permission`; Guest reads User emails via
  `/api/method/frappe.client.get_value`. Active under `--desk`. `desk.rs:760/792/1022/1047/1063`.
  → D8/GAP-12, FIX-9.

## 3. REST API surface
- **3.1 `expand` / `expand_links`** — [ferro] Med. Returns raw link value. `orm.rs` get_list/get_doc.
  → G3, FIX-3.
- **3.2 v2 bulk operations** — [ferro] Med. `/api/v2/.../bulk_delete|bulk_update` 404/405. → G4, FIX-4.
- **3.3 Missing whitelisted methods** — [ferro] Low–Med. `frappe.realtime.get_user_info`→404; app
  Python methods (download_template) → ferrod. → G5/D6, FIX-5.
- **3.4 `debug=1` → `_debug_messages`** — [ferro] Low. Omitted. → G6, FIX-6.
- **3.5 HTTP caching headers** — [ferro] Low. No `Cache-Control` on cacheable responses.
  → test_caching.TestHttpCache.
- **3.6 Corrupt `parent`/`parentfield`/`parenttype` keys** — [ferro] **Med**. 146 doctypes; SQLite
  quoted-identifier→string-literal. `meta.rs:12/156`→`orm.rs:418`. → G7/D7, FIX-8.
- **3.7 Error `exc_type` collapsing** — [ferro] Low. `NameError`→`ValidationError`; no distinct types.
- **3.8 v2 401 returns the V1 error envelope** — [ferro] Med. Auth resolved before version branch.
  `main.rs:987`. → rest-env B-REST-1.
- **3.9 `/api/v2/doctype/<dt>/meta` and `/count` → 404** — [ferro] Med. v2 only branches document/method.
  `main.rs:1033`. → rest-env B-REST-2.
- **3.10 v2 `copy` → 404; read-only-mode 503 not implemented** — [ferro] Low. → rest-env B-REST-3/4.
- **3.11 `frappe.client.*` write methods 404** — [ferro] Med. `set_value`/`delete`/`submit`/`cancel`/
  `bulk_update`/`validate_link_and_fetch`/`get_password`/`insert_many` not implemented. → client-methods G1–G11.
- **3.12 `frappe.client.get` doesn't strip nulls** — [ferro] Med. Frappe returns `as_dict(no_nulls=True)`.

## 4. ORM read (filters / list / db-api)
- **4.1 dict-in-list `filters`/`or_filters` rejected** — [ferro] Med. `orm.rs:222`. → orm-filters GAP-1.
- **4.2 `not in None` returns empty, not all** — [ferro] Med. SQL `NOT IN (NULL)`. `orm.rs:171`. → GAP-2.
- **4.3 `in`/`not in` JSON-encoded list string not parsed** — [ferro] Med. `orm.rs:143`. → GAP-3.
- **4.4 Single doctype in client `get_value`/`get_list`** — [ferro] **High**. Queries nonexistent
  `tab<Single>`. `desk.rs`. → db-api GAP-3.
- **4.5 `get_value` multi-field / `get_single_value` casting** — [ferro] Med. → db-api GAP-1/2.
- **4.6 aggregates/`distinct`/`group_by`/`pluck`/`as_list`/alias** — [ferro] Low. → db-api GAP-4–6.

## 5. ORM write (document lifecycle)
- **5.1 Link validation not performed** — [ferro] Med. Non-existent link target accepted. `orm.rs`
  insert/update. → orm-document G1.
- **5.2 `set_only_once` not enforced** — [ferro] Med. `meta.rs`/`orm.rs` update. → orm-document G2.
- **5.3 Optimistic-lock (`modified`) not checked** — [ferro] Med. Stale `modified` accepted. → G3.
- **5.4 submit / cancel / discard / docstatus transitions** — [ferro] Med (footprint-justified). → G4.
- **5.5 Child-table autoname ignored** — [ferro] **High**. `orm.rs:711` always `random_name()`.
  → naming GAP-child.
- **5.6 Server-side writes 500** — **[env]** SQLite write-lock contention with the in-process test
  txn; identical on the oracle. Not ferro (standalone create=200). → D4.

## 6. Naming
- **6.1 ISO `WW` consecutive week wrong** — [ferro] **High**. `naming.rs:197`. Diverges today.
  → naming GAP-WW.
- **6.2 `revert_series_if_last` missing** — [ferro] Med (txn-atomic counter mitigates). → GAP-revert.
- **6.3 `append_number_if_name_exists` missing** — [ferro] Med. Duplicate → 409 instead of `-1`. → GAP-append.
- **6.4 Amended/cancelled naming** — [ferro] Med (needs submit/cancel). → GAP-amend.
- **6.5 Required-msg label, UUID/int `name` validation, microsecond timestamp** — [ferro] Low.

## 7. Meta / schema
- _Deep-dive `_raw/behavior_meta.md` (pending at time of writing); summary will be folded into `04`._

## 8. Environment / harness (NOT ferro — do not misattribute)
- **8.1 SQLite dual-process write contention** — [env]. All server-write tests 500 on both targets. → D4.
- **8.2 Cookie-session clients masked** — [env+ferro]. FrappeClient password login → whole suite errors;
  partly ferro (1.2) but un-runnable as a differential here.
- **8.3 Missing subsystems in this bench** — [env]. OAuth, read-only mode, PDF/print, background-job
  enqueue, custom-app hooks: fail identically on both (single-app SQLite bench).
- **8.4 SQLite-specific Frappe quirks** — [env]. e.g. `ImplicitCommitError` on `ALTER TABLE … RENAME`
  in a txn; many `test_db` cases `@skip` on non-MariaDB.
- **8.5 Sequential test-data accumulation** — [env]. Baseline (`bench-base`) ran modules against one DB
  without per-module reset → fixture UNIQUE conflicts (e.g. `_Test Blogger`), inflating baseline errors.

---

### How to read the balance
Of the leaves above, the **[ferro]** items in §1–§6 are the actionable compatibility list (≈30
distinct gaps, ~6 High). The **[env]** items in §8 (≈60 test failures) are *not* ferro defects — the
symmetric differential exists precisely to keep them out of ferro's column. The fix plan (`06`) targets
only the [ferro] leaves, led by 1.1 (Guest role), 4.4 (Single doctypes), 5.5 (child naming),
6.1 (WW week), 3.6 (parent-key corruption), and the 1.2/1.3 sessions decision.
