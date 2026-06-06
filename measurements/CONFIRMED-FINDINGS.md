# Ferro fidelity — findings I confirmed directly against Frappe source (17.0.0-dev)

Independent of the audit workflow. Each verified by reading the cited Frappe code.

## CRITICAL (already fixed)
- **C1 OOM in random_name()** — `std::fs::read("/dev/urandom")` reads a never-EOF device → unbounded
  alloc → SIGKILL. Broke ALL write paths (provision-key, hash/field inserts, child rows).
  FIXED in util.rs: `File::open` + `read_exact(&mut [u8;5])`. Verified provision + 200 CRUD cycles OK.

## HIGH
- **H1 limit_page_length=0 must mean UNLIMITED, not zero rows.**
  Frappe: db_query.py:185 `self.limit_page_length = cint(x) if x else None`; :1288 `if self.limit_page_length:`
  → falsy ⇒ no LIMIT clause ⇒ all rows. ferro emits `LIMIT 0` ⇒ returns nothing. Very common client call
  (fetch all). Fix: in get_list, if limit_page_length <= 0 omit LIMIT (keep OFFSET if start>0).
  Default-when-absent stays 20 (v1 document_list sets default 20). ferro ListQuery default already 20. ✓

- **H2 naming_series / format: / expression naming downgraded to random hash.**
  Distribution in site: ~6 doctypes `X.####` series, 7 `format:{...}`, 1 autoincrement, plus "Expression".
  Frappe naming.py: make_autoname/parse_naming_series with `#`→getseries (tabSeries counter, leading-zeros),
  date parts YY/MM/DD/YYYY/JJJ/WW/timestamp, `{field}` substitution; `format:` via _format_autoname;
  `field:` (ferro ✓), `hash` (ferro ✓), null→hash (ferro ✓). autoincrement→DB sequence.
  Fix: implement series (`#` run + tabSeries upsert + date parts + {field}) and format: substitution.
  tabSeries schema: (name TEXT pk, current INT). getseries: SELECT current WHERE name=key; +1 or INSERT 1.

## MEDIUM
- **M1 "Prompt" (capital) not handled** — resolve_name uses case-sensitive `strip_prefix("prompt")`.
  Frappe lowercases (set_name_from_naming_options `_autoname = autoname.lower()`). 21 doctypes use "Prompt".
  When client supplies name it's fine (resolve_name returns it first); only matters when name omitted.
  Fix: lowercase compare for prompt/uuid/autoincrement/naming_series/format/field.

- **M2 DELETE envelope** — Frappe v1 delete_doc returns `"ok"` with http 202 ⇒ body `{"data":"ok"}`.
  ferro returns `{"message":"ok"}` @202. Fix: return `{"data":"ok"}`.

- **M3 _server_messages format** — Frappe v1 (_make_logs_v1): value is JSON-array-string whose elements are
  each a JSON-string of a dict, e.g. `"[\"{\\\"message\\\": \\\"X\\\"}\"]"`. frappe-js-sdk does JSON.parse twice
  then reads `.message`. ferro emits array of plain strings ⇒ inner parse fails in JS SDK. Fix: wrap each msg as
  `{"message": msg}` then json-encode element then array then json-encode whole.

- **M4 UUID autoname** — Frappe supports `autoname="UUID"` (uuid7). Not in this site's distribution; low priority.

## KNOWN LIMITATIONS (document, likely won't fully implement — out of REST-data-plane scope or huge)
- **L1 Row-level permissions / user permissions / if_owner / permlevel** — ferro gates at doctype level via
  tabDocPerm role-join (permlevel 0). Frappe get_list also injects match conditions (user permissions, if_owner)
  filtering ROWS, and field-level (permlevel>0) read masking. ferro is over-permissive for restricted users on
  row visibility. Admin/System Manager unaffected. Reimplementing user-permission system is large.
- **L2 Encrypted api_secret (Fernet)** — production sites store api_secret encrypted=1 (Fernet w/ site
  encryption_key). ferro verifies only plaintext (encrypted=0); provision-key writes plaintext. Fernet needs AES;
  would add a crypto dep to the deliberately-minimal build. Decide based on audit severity.
- **L3 Type coercion** — ferro returns raw SQLite affinity (int/real/text). Frappe casts via fieldtype in as_dict
  (e.g. Check→int, Float→float, Date→str). Mostly aligned because SQLite stores Frappe's written types; edge cases
  (Link int vs str — v2 cstr's it; v1 does not) minor.
- **L4 Unbounded result set memory** — get_list with unlimited builds full Vec<Value>+serializes in RAM. Mirrors
  Frappe (also non-streaming). Acceptable; note it.

## ENVELOPE / STATUS-CODE GROUND TRUTH (Frappe v1, confirmed)
- success: return wrapped as `{"data": <value>}`.
- exceptions.py: ValidationError 417, DoesNotExistError 404, PermissionError 403, AuthenticationError 401,
  NameError(duplicate) 409, UnsupportedMediaType 415. ferro maps duplicate→417; should be 409.
- error body v1: `exc_type=<ClassName>`, `exception=<last traceback line, dev-mode only>`, `_server_messages`.
