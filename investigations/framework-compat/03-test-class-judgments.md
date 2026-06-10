# 03 — Per-test-class compatibility judgments

Every test **class** across all 276 files was rated for ferro's obligation to match it, using the rubric in `_agent_context.md` (1:1 / mostly / somewhat / shouldn't-care). Raw data: `_raw/classify_*.json`.

## Distribution (400 classes, deduped to 400)

| tier | classes | meaning |
|---|---|---|
| **1:1** | 7 | core contract ferro reimplements — MUST match |
| **mostly** | 15 | same domain, match substantive assertions |
| **somewhat** | 28 | partially in scope; full fidelity optional given footprint |
| **shouldn't-care** | 350 | Python/app subsystem ferro intentionally doesn't implement (→ ferrod), or pure in-process internals |

**Takeaway:** ~88% of Frappe's test surface is *out of scope* for pure ferro by design. ferro's real obligation is the ~50 in-scope classes below — the REST/ORM/auth/perm/naming/meta core.

## In-scope classes (1:1 / mostly / somewhat)

| tier | area | file: class | ferro | rationale |
|---|---|---|---|---|
| 1:1 | Meta-Schema | `tests: api_v2: TestDocTypeAPIV2` | partial | Tests the /api/v2/doctype/<dt>/meta and count endpoints; meta is served by ferro's meta.rs (desk.rs route) and count by orm.rs. These are core ferro-implemented endpoints that must |
| 1:1 | Naming | `tests: naming: TestNaming` | partial | Naming is a core contract ferro reimplements in naming.rs (naming_series, field:, format:, hash, expression); these in-process tests encode the exact behaviors ferro must replicate |
| 1:1 | ORM-read | `tests: db_query: TestDBQuery` | partial | Tests the full DatabaseQuery filter/permission logic that ferro's orm.rs must replicate exactly for get_list; ferro's build_list_query implements the same filter operators, perm-le |
| 1:1 | REST-API | `tests: api: TestResourceAPI` | partial | Directly tests ferro's core /api/resource GET/POST/PUT/DELETE endpoints and response envelope; CRUD/filters/perm must be 1:1. expand and expand_links are confirmed gaps in ferro (e |
| 1:1 | REST-API | `tests: api: TestMethodAPI` | partial | Tests ping (implemented), token auth cycle (implemented in auth.rs), 404 routing, and server message envelope; frappe.realtime.get_user_info is a known ferro gap (returns 404 inste |
| 1:1 | REST-API | `tests: api_v2: TestResourceAPIV2` | partial | Tests ferro's /api/v2/document endpoints (orm.rs + route_v2_document) with the v2 JSON envelope; CRUD, list, and 404 error format are core 1:1 obligations. Write tests are harness- |
| 1:1 | REST-API | `tests: frappe_client: TestFrappeClient` | partial | Makes real HTTP calls via FrappeClient (password session login) and direct requests with token/Basic auth headers to test CRUD and auth. Token/Basic auth and CRUD are fully in ferr |
| mostly | Auth-Perm | `core/doctype/user_permission: user_permission: TestUserPermission` | partial | User permissions directly affect which documents are visible via ferro's permission gate in auth.rs/orm.rs; the tests are in-process but encode the permission filtering contract fe |
| mostly | Auth-Perm | `desk/doctype/todo: todo: TestToDo` | partial | Tests if_owner permission semantics (owner and assigned_by gates), fetch_from fields, and role-based list filtering through DatabaseQuery — all behaviors ferro's orm.rs must replic |
| mostly | Auth-Perm | `tests: password: TestPassword` | partial | Encodes the password storage contract (pbkdf2_sha256 hash in __Auth, Fernet-encrypted passwords) that ferro's crypto.rs and auth.rs must match; ferro implements pbkdf2 verification |
| mostly | Auth-Perm | `tests: permissions: TestPermissions` | partial | Encodes the full Frappe permission model (role perms, user permissions, if_owner, perm levels, strict mode) that ferro's auth.rs and orm.rs partially implement; ferro handles role- |
| mostly | Meta-Schema | `tests: form_load: TestFormLoad` | partial | Tests frappe.desk.form.load.getdoc/getdoctype/get_docinfo which ferro reimplements in desk.rs (frappe.desk.form.load.*). The field-level permlevel masking and docinfo aggregation ( |
| mostly | Naming | `core/doctype/document_naming_rule: document_naming_rule: TestDocumentNamingRule` | partial | Tests verify that Document Naming Rules (condition-based rule selection, prefix+counter series) produce correct names at insert time; ferro's naming.rs implements autoname rules in |
| mostly | Naming | `core/doctype/document_naming_settings: document_naming_settings: TestNamingSeries` | partial | Tests the DocumentNamingSettings singleton that controls naming_series options, counter values, and amendment-naming policy; ferro's naming.rs uses the same tabSeries counter table |
| mostly | ORM-read | `tests: db_query: TestReportView` | partial | Tests reportview.get_count and reportview.get which ferro exposes via desk.rs (frappe.desk.reportview.*); ferro must produce the same count/filter semantics, though the tests drive |
| mostly | ORM-read | `tests: query: TestQuery` | partial | Tests the query builder's SQL generation and filter validation which encode ORM filter semantics ferro must replicate in orm.rs; ferro uses its own filter parsing rather than frapp |
| mostly | ORM-read | `tests: utils: TestFilters` | partial | Tests Frappe's filter parsing logic used by get_list; ferro's orm.rs must handle the same filter operators (=, !=, >, <, in, not in, is, between, timespan) — the behavior is partia |
| mostly | REST-API | `tests: api: FrappeAPITestCase` | partial | This is the base test class that sets up session-based auth (sid cookie) via LoginManager; ferro has no session store so sid auth always falls through to default_user. The harness  |
| mostly | REST-API | `tests: api_v2: TestMethodAPIV2` | partial | Tests the v2 method envelope format (data key instead of message), token auth, ping, and run_doc_method; ferro must match the v2 response envelope for these. shorthand controller m |
| mostly | REST-API | `tests: caching: TestHttpCache` | no | Tests that frappe.client.is_document_amended response includes Cache-Control: max-age=600, private headers; ferro does not currently set cache-control headers on its responses, mak |
| mostly | REST-API | `tests: client: TestClient` | partial | Tests both in-process calls to frappe.client functions AND some HTTP requests (array_values_in_request_args hits /api/method/frappe.client.get_list over HTTP). Core get/insert/dele |
| mostly | REST-API | `tests: cors: TestCORS` | partial | Tests that the CORS headers are correctly added/suppressed based on site config; ferro's HTTP server must also emit correct CORS headers for cross-origin Desk/SPA clients, so the b |
| somewhat | Auth-Perm | `core/doctype/docshare: docshare: TestDocShare` | partial | Tests frappe.share.* permission grants and has_permission checks in-process; not exercised over HTTP. However, ferro's orm.rs permission gate must respect tabDocShare rows when dec |
| somewhat | Auth-Perm | `core/doctype/role: role: TestUser` | partial | Tests in-process role management that encodes behaviors ferro must replicate — disabled roles should not grant access and role membership affects user classification — but the spec |
| somewhat | Auth-Perm | `core/doctype/user: user: TestUser` | partial | User creation/deletion and role assignment are ORM operations ferro must handle; password reset and signup involve session/email flows outside ferro's pure-Rust scope, so only the  |
| somewhat | Auth-Perm | `core/doctype/user: user: TestImpersonation` | no | Impersonation calls frappe.core.doctype.user.user.impersonate via HTTP then checks get_logged_user — this is a session-mutating action that relies on sid session state which ferro  |
| somewhat | Auth-Perm | `core/doctype/user_type: user_type: TestUserType` | partial | User type definitions shape which DocPerms get created and are thus relevant to ferro's permission checks, but the auto-propagation logic itself is Python controller behavior not r |
| somewhat | Auth-Perm | `desk/doctype/dashboard: dashboard: TestDashboard` | partial | Tests that frappe.get_list('Dashboard') respects permission and module-blocking logic in-process; ferro's orm.rs implements permission-gated get_list over HTTP and would need to ho |
| somewhat | Auth-Perm | `desk/doctype/event: event: TestEvent` | partial | Tests if_owner and docshare-based read permissions through frappe.has_permission and frappe.get_list in-process; ferro's orm.rs implements if_owner permission gating and list filte |
| somewhat | Auth-Perm | `tests: auth: TestAuth` | no | Tests System Settings login policy enforcement (allow_login_using_mobile_number, deny_multiple_sessions, etc.) via FrappeClient password-based session auth — all confirmed ferro ga |
| somewhat | Auth-Perm | `tests: auth: TestSessionExpiry` | no | Tests that sessions expire per System Settings configuration using sid-based auth; ferro issues random sid cookies but has no session store or expiry logic (confirmed gap #2), so s |
| somewhat | Auth-Perm | `tests: hooks: TestAPIHooks` | no | TestAPIHooks sends a real HTTP request and expects a custom auth_hook to run (setting the user from a custom token prefix); ferro's auth.rs has no hook dispatch and would ignore th |
| somewhat | DB-layer | `tests: db: TestDB` | partial | Tests Frappe's Python database layer (frappe.db.*) directly in-process; ferro reimplements the same SQLite read/write semantics in orm.rs but uses its own Rust code path — these te |
| somewhat | DB-layer | `tests: db: TestDBSetValue` | partial | Tests the in-process set_value/get_value contract which maps to ferro's orm.rs update path; ferro must produce equivalent behavior for ORM writes over HTTP, but these tests never c |
| somewhat | DB-layer | `tests: query_builder: TestOperatorIn` | partial | Tests how Frappe's query builder handles IN filters with null/empty values (IS NULL expansion); ferro's orm.rs must implement equivalent IN-filter null handling when processing RES |
| somewhat | DocType-feature | `desk/doctype/tag: tag: TestTag` | partial | Tests add_tag() and get_stats() for _user_tags aggregation which maps to frappe.desk.reportview.get_stats — a method ferro's desk.rs partially implements; ferro would need to suppo |
| somewhat | DocType-feature | `tests: document: TestDocument` | partial | Tests the Python Document class lifecycle (insert, save, submit, cancel, naming, child-table handling, hooks); ferro's orm.rs covers the insert/update/read data path but not Python |
| somewhat | Meta-Schema | `tests: db_update: TestDBUpdate` | partial | Tests the Python DDL migration logic (updatedb) that maps DocType field definitions to SQL column types; ferro's schema.rs implements an equivalent mapping for SQLite install/migra |
| somewhat | Meta-Schema | `tests: model_utils: TestModelUtils` | partial | Tests in-process Python model utilities; get_permitted_fields encodes permission-filtered field lists that ferro's auth.rs/meta.rs must replicate for correct ORM masking, but the P |
| somewhat | Meta-Schema | `tests: non_nullable_docfield: TestNonNullableDocfield` | partial | Tests the not_nullable DocField attribute's effect on DB schema (DDL) and insert default values; ferro's schema.rs handles DDL during install/migrate but not_nullable column semant |
| somewhat | Meta-Schema | `tests: utils: TestLinkTitle` | partial | Tests that doctypes with show_title_field_in_link are included in boot_info and that link title fields appear in getdoc; ferro's desk.rs handles getdoc but may not fully implement  |
| somewhat | ORM-read | `desk/form: form: TestForm` | partial | Tests get_linked_docs (which ferro's desk.rs exposes via frappe.desk.form.load methods), sort-field null-value fallback for list navigation, and get_next pagination — ferro impleme |
| somewhat | ORM-read | `tests: document: TestGetDocs` | partial | Tests frappe.get_docs bulk retrieval which parallels ferro's get_list + per-doc child-table fetch; the filter/limit/order_by semantics are relevant to ferro's orm.rs but get_docs i |
| somewhat | ORM-read | `tests: listview: TestListView` | partial | Tests call frappe.db and frappe.desk.reportview.get() in-process; the reportview.get path (get_list with filters, group-by) encodes ORM semantics that ferro reimplements via orm.rs |
| somewhat | ORM-read | `tests: search: TestSearch` | partial | search_link/search_widget are whitelisted desk methods (frappe.desk.search.*) that ferro's desk.rs partially reimplements; the permission-aware query logic and SQL injection guards |
| somewhat | ORM-write | `tests: dynamic_links: TestDynamicLinks` | partial | Tests that deleting a parent document cascades to dynamically-linked children (Email Unsubscribe, Comments). Ferro's orm.rs implements delete but does not handle dynamic-link casca |
| somewhat | REST-API | `tests: api: TestReadOnlyMode` | no | Tests maintenance_mode site config flag that blocks writes with 503 InReadOnlyMode; ferro does not implement maintenance_mode awareness so writes would succeed (not 503). This is a |
| somewhat | REST-API | `tests: api_v2: TestBulkOperationsV2` | no | Tests /api/v2/document/<dt>/bulk_delete and bulk_update endpoints which are confirmed ferro gaps (empirical finding #6 — returns 404/405). Ferro should implement these but currentl |
| somewhat | REST-API | `tests: api_v2: TestReadOnlyMode` | no | Same as v1 TestReadOnlyMode but verifies the v2 errors[] envelope format for 503; ferro does not implement maintenance_mode awareness, making this out-of-scope for pure ferro. |
| somewhat | REST-API | `tests: perf: TestOverheadCalls` | partial | Uses FrappeAPITestCase to count Redis and SQL calls per HTTP request (ping, resource list, get); ferro has no Redis and its SQL call count differs structurally, so exact overhead n |

## Out-of-scope (shouldn't-care), grouped by area

| area | #classes | representative files | why out of scope |
|---|---|---|---|
| Framework-internal | 90 | core/doctype/access_log, core/doctype/api_request_log, core/doctype/audit_trail, core/doctype/domain | pure in-process Python internals never exposed over ferro's HTTP surface |
| DocType-feature | 54 | automation/doctype/assignment_rule, automation/doctype/auto_repeat, automation/doctype/milestone, automation/doctype/milestone_tracker | per-doctype Python controller logic (validations, hooks) — ferrod's job |
| Dev-tooling | 41 | commands, core/doctype/data_export, core/doctype/data_import, core/doctype/data_import_log | build/test/translate/boilerplate tooling — not a runtime concern |
| Web-Website | 28 | search, tests, website/doctype/about_us_settings, website/doctype/color | web pages / web forms / portal — ferro serves SPAs, not the Python web router |
| Auth-Perm | 27 | core/doctype/activity_log, core/doctype/custom_docperm, core/doctype/custom_role, core/doctype/document_share_key | feature-specific auth (2FA, API-key UI, password policy, LDAP) beyond the core gate |
| Email | 25 | core/doctype/communication, core/doctype/sms_log, core/doctype/sms_settings, core/doctype/user_invitation | email accounts / queue / newsletter — no email subsystem in ferro |
| Reports-Print | 21 | contacts/report/addresses_and_contacts, core/doctype/prepared_report, core/doctype/report, core/report/database_storage_usage_by_tables | report engine / print formats / PDF — not implemented |
| Integrations-OAuth-Social | 17 | integrations/doctype/connected_app, integrations/doctype/geolocation_settings, integrations/doctype/google_contacts, integrations/doctype/google_settings | OAuth2/social login/connected apps/webhooks — not implemented |
| DB-layer | 17 | tests | DB internals (DDL, query-builder, transactions) used in-process, not over HTTP |
| Background-jobs | 11 | commands, core/doctype/background_task, core/doctype/rq_job, core/doctype/rq_worker | Python RQ jobs / scheduler events — ferro has its own minimal jobs only |
| Meta-Schema | 8 | core/doctype/doctype, core/doctype/doctype_layout, core/doctype/page, custom/doctype/custom_field | schema/customize-form internals beyond read meta |
| Workflow | 5 | workflow/doctype/workflow, workflow/doctype/workflow_action, workflow/doctype/workflow_state, workflow/doctype/workflow_transition_task | workflow state engine — not implemented |
| ORM-read | 3 | tests | out of pure-ferro scope |
| ORM-write | 2 | desk/doctype/note, tests | out of pure-ferro scope |
| Naming | 1 | core/doctype/document_naming_rule_condition | out of pure-ferro scope |