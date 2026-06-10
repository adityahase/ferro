# 04 — Behavioral fidelity findings (ferro core domains vs Frappe spec tests)

For the domains ferro actually reimplements, each Frappe spec test was read for the concrete behaviors
it guarantees, then compared **behavior-by-behavior** against ferro's source (with live HTTP probes
where decidable). Full per-domain detail (every behavior, MATCH/PARTIAL/GAP, file:line, fix sketch)
is in `_raw/behavior_<domain>.md`. This file is the synthesis.

Legend: **MATCH** ferro replicates · **PARTIAL** mostly, edge gaps · **GAP** missing/divergent.

## Summary by domain
| Domain | Verdict | Headline gaps (severity) |
|---|---|---|
| **naming** (`naming.rs`) | Strong; core rules match | child-table autoname ignored (H); ISO `WW` week wrong incl. *today* (H); no `revert_series`/`append_number`/amended naming (M); required-msg uses fieldname not label; UUID/int name not validated (L) |
| **orm-filters** (`orm.rs`) | Strong; operators match | dict-in-list `filters`/`or_filters` rejected (M); `not in None`→empty not all (M); `in`/`not in` JSON-encoded list string not parsed (M); minor order_by/qualifier edges |
| **orm-document** (`orm.rs`) | Read/write data path solid | link validation not done (M); `set_only_once` not enforced (M); optimistic-lock (`modified`) not checked (M); submit/cancel/discter unsupported (M, footprint-justified); **+ the parent-key serialization bug, G7/D7** |
| **permissions** (`auth.rs`) | Role gate + if_owner + permlevel-read match | **Guest gets `All` (H, security)**; `Desk User`/`Guest` roles missing on users (M/L); **User Permissions not implemented (M, over-grants)**; **DocShare not implemented (M, under-grants)**; write-path permlevel masking absent (M); controller has_permission hooks = ferrod-only |
| **db-api** (`desk.rs frappe.client.*`) | Common reads match | **Single doctypes query nonexistent `tab<Single>` (H)**; `get_value` multi-field returns only first (M); `get_single_value` no fieldtype cast, returns 0 for None (M); aggregates/`distinct`/`group_by`/`pluck`/alias not honored (L) |
| **meta** (`meta.rs`) | Core meta load matches | **Custom Fields not merged** into `meta.fields` (M); **Property Setters not applied** (M) — ferro ignores site customizations; no meta-cache invalidation API (M, High if DocType edited at runtime); `_seen` missing from STANDARD_COLUMNS, `Image` fieldtype edge (L) |
| **rest-envelopes** (`main.rs`) | 16 MATCH / 2 PARTIAL / 7 GAP | **v2 401 returns the V1 error envelope** (M); **`/api/v2/doctype/<dt>/meta`+`/count`→404** (M); v2 bulk missing (M); v2 `copy`→404 (L); read-only-mode 503 not impl (L); `get_user_info` etc. 404 (L) |
| **client-methods** (`desk.rs`) | Common methods match | **`client.get` doesn't strip nulls** (Frappe `as_dict(no_nulls=True)`) (M); + corroborates the parent-key bug (G7) and Single-doctype gap; _full detail `_raw/behavior_client-methods.md`_ |

## Cross-domain GAP register (de-duplicated, with fix locations)
The detailed fixes are consolidated in `06-fix-plan.md` (FIX-1…FIX-8 + behavioral appendix). Key items:

### High
- **`frappe.client.*` reads bypass permissions** (`desk.rs:760/792/1022/1047/1063`). Guest reads User
  emails. The single most severe finding. → FIX-9.
- **Guest→`All` role** (`auth.rs:144`). Security over-grant. → FIX-1.
- **v2 401 returns V1 error envelope** (`main.rs:987`, auth resolved before version branch) — a
  frappe-ui v2 client reading `response.errors` mishandles auth failures. → new B-REST-1.
- **`/api/v2/doctype/<dt>/meta` and `/count` → 404** (`main.rs:1033`, v2 only branches document/method)
  → new B-REST-2.
- **ISO `WW` week number wrong** (`naming.rs:197`) — `(day_of_year+6)/7` ≠ ISO consecutive week;
  diverges *today* (Frappe 24 / ferro 23), corrupting `tabSeries` keys vs a CPython worker.
- **Single doctypes in client `get_value`/`get_list`** (`desk.rs`) — query `tab<Single>` which doesn't
  exist; must read from `tabSingles` (key/value) like ferro's own `get_single`.
- **Child-table autoname ignored** (`orm.rs:711`) — every child row gets `random_name()` regardless of
  the child DocType's `autoname`.
- **Parent-key serialization corruption** (`meta.rs:12/156` → `orm.rs:418`) — G7/D7, FIX-8.

### Medium
- ORM filter shapes: dict-in-list, `not in None`, JSON-encoded `in`-list string (`orm.rs:222/171/143`).
- Write integrity: link validation, `set_only_once`, optimistic-lock `modified` check (`orm.rs` insert/update).
- Permission completeness: User Permissions (over-grant), DocShare (under-grant), write-path permlevel
  masking (`auth.rs`/`orm.rs`).
- db-api: `get_value` multi-field, `get_single_value` casting (`desk.rs`).
- naming: `revert_series_if_last`, `append_number_if_name_exists`, amended/cancelled naming (`naming.rs`).
- meta: **Custom Fields** + **Property Setters** not applied (`meta.rs` load) — ferro serves the
  vanilla doctype, ignoring per-site customizations; no meta-cache invalidation on DocType change.

### Low / scoped-out
- naming: required-field message label vs fieldname; UUID/int `name` validation; microsecond timestamp.
- db-api: aggregates, `distinct`/`group_by`/`pluck`/`as_list`, alias, `run=False`, `for_update`.
- submit/cancel/docstatus transitions and controller `has_permission` hooks → ferrod tier, not pure ferro.

## Notable UNDOCUMENTED divergences surfaced (full list in `05-discovered-behaviors.md`)
- **Error `exc_type` collapsing:** ferro returns `ValidationError` where Frappe raises `NameError`
  (no separate NameError type); naming Validation → HTTP 417, duplicate → 409 (mostly matches).
- **Required-field message** uses fieldname not the translated label.
- **Write path is not permlevel-masked** (only reads are) — a permlevel-1 scalar could be written by a
  permlevel-0-write user if it reaches the ORM.
- **`only_if_owner` single-doc** returns 404-before-403 ordering differs from Frappe in some paths.
- **`--desk` ⇒ default_user=Administrator** (auth posture flips with a presentation flag) — `05` D5.

<!-- APPEND: meta / rest-envelopes / client-methods summaries when their deep-dives land -->
