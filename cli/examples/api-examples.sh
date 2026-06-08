#!/usr/bin/env bash
#
# api-examples.sh — runnable curl examples against a running ferro REST runtime.
#
# ---------------------------------------------------------------------------
# WHAT THIS IS
# ---------------------------------------------------------------------------
# ferro serves the Frappe *v1* REST API directly against a SQLite site:
#
#     /api/method/ping                         -> {"message":"pong"}
#     /api/method/frappe.auth.get_logged_user  -> {"message":"<user>"}
#     /api/resource/<DocType>            (GET list, POST create)
#     /api/resource/<DocType>/<name>     (GET one, PUT update, DELETE)
#
# NOTE: a DocType name with a space (e.g. "CRM Deal") must be %20-encoded in the
# URL path — an unencoded space breaks the HTTP request line. The /api/method
# endpoints return a {"message": ...} body; resource endpoints return {"data": ...}.
#
# Successful resource responses are wrapped in a {"data": ...} envelope. DELETE returns
# {"data":"ok"} with HTTP 202. Errors use the Frappe v1 error shape (a JSON body
# carrying an "exc_type" and "_server_messages", with the matching HTTP status:
# 404 not found, 417 validation, 403 permission, 401 auth, 409 duplicate,
# 413 body-too-large, 500 server). Source: REPORT.md.
#
# ---------------------------------------------------------------------------
# WHAT IS *NOT* SERVED (so these examples don't pretend otherwise)
# ---------------------------------------------------------------------------
#   * No /api/v2 namespace — only v1 (/api/resource, /api/method).
#   * /api/method is allow-listed to exactly `ping` (alias `frappe.ping`) and
#     `frappe.auth.get_logged_user`; any other dotted method returns 404
#     (arbitrary methods are Python). Note the get_logged_user form is the FULL
#     dotted name — the bare `get_logged_user` 404s.
#   * POST /api/resource/<dt>/<name> (run_method / submit / cancel) returns 405:
#     no controller methods, no submit/cancel over REST.
#   * No controller business logic on the pure-Rust `ferro` runtime: validate /
#     before_save / on_submit hooks and computed/fetched fields do not run.
#   * Query filters/fields/order_by operate on the doctype's OWN physical columns
#     only — no joined/child-field filters (tabChild.field), no group_by/aggregate.
#   * Supported operators: = != > < >= <= <>, like / not like, in / not in
#     (incl. comma-strings), between, is set / is not set. NOT supported:
#     timespan/previous/next, "descendants of"/"ancestors of".
# Source: docs/LIMITATIONS.md.
#
# ---------------------------------------------------------------------------
# SETUP (assumed before running this script)
# ---------------------------------------------------------------------------
#   ferro new-site dev.localhost      # create the SQLite site from the seed
#   ferro install-app crm             # REQUIRED for the "CRM Deal" examples below
#                                     #   (materialises that app's DocType schema)
#   ferro serve --user Administrator  # serve on :8000 as the default user
#
# NOTE ON AUTH (local default): serving with `--user Administrator` makes
# Administrator the *default user* — requests with no Authorization header run as
# Administrator and bypass token auth entirely. So all the calls below work with
# no credentials in this local setup. The token form is shown at the end for
# completeness (what you'd send against a normally-secured site).
#
# Examples that depend on `ferro install-app crm` are marked: [requires: crm].
# ---------------------------------------------------------------------------

set -e

# Parameterized base URL — override with: BASE_URL=http://host:port ./api-examples.sh
BASE_URL="${BASE_URL:-http://127.0.0.1:8000}"

# Pretty-print JSON if `jq` is present; otherwise pass through unchanged.
pp() { if command -v jq >/dev/null 2>&1; then jq .; else cat; fi; }

section() { printf '\n========== %s ==========\n' "$1"; }


# ---------------------------------------------------------------------------
section "1. ping — liveness check (/api/method/ping)"
# ---------------------------------------------------------------------------
# One of only two allow-listed /api/method endpoints. Returns {"message":"pong"}.
curl -sS "${BASE_URL}/api/method/ping" | pp


# ---------------------------------------------------------------------------
section "2. get_logged_user — who am I (/api/method/frappe.auth.get_logged_user)"
# ---------------------------------------------------------------------------
# The other allow-listed method (note the full dotted name). With --user
# Administrator and no auth header, this reports the default user:
# {"message":"Administrator"}.
curl -sS "${BASE_URL}/api/method/frappe.auth.get_logged_user" | pp


# ---------------------------------------------------------------------------
section "3. list with filters / fields / limit_page_length / order_by"
# ---------------------------------------------------------------------------
# GET /api/resource/<DocType> with the standard Frappe v1 list query params.
#   filters            JSON array of [field, operator, value] triples
#   fields             JSON array of column names to return
#   limit_page_length  page size (0 = unlimited)
#   order_by           "<field> asc|desc"
# All params must be URL-encoded; --data-urlencode + -G does that for us.
#
# This lists enabled, non-Administrator users — a doctype present in every site.
curl -sS -G "${BASE_URL}/api/resource/User" \
  --data-urlencode 'filters=[["enabled","=",1],["name","!=","Administrator"]]' \
  --data-urlencode 'fields=["name","email","enabled"]' \
  --data-urlencode 'order_by=creation desc' \
  --data-urlencode 'limit_page_length=5' | pp

# limit_page_length=0 means "no limit" (returns every matching row).
# Example with an `in` operator (comma-string form is also accepted by ferro):
curl -sS -G "${BASE_URL}/api/resource/User" \
  --data-urlencode 'filters=[["name","in",["Administrator","Guest"]]]' \
  --data-urlencode 'fields=["name"]' \
  --data-urlencode 'limit_page_length=0' | pp


# ---------------------------------------------------------------------------
section "4. get one document (/api/resource/<DocType>/<name>)"
# ---------------------------------------------------------------------------
# Returns the full document under {"data": {...}}. A missing name returns 404
# with the v1 error envelope (exc_type set). Names containing slashes are fine.
curl -sS "${BASE_URL}/api/resource/User/Administrator" | pp


# ---------------------------------------------------------------------------
section "5. create with a child table (/api/resource/CRM Deal)   [requires: crm]"
# ---------------------------------------------------------------------------
# POST /api/resource/<DocType> with a JSON body creates a document. Child-table
# fields are passed as arrays of row objects. The server assigns name/owner/
# creation/modified (clients cannot override these); naming follows the doctype's
# naming_series / format rule, backed by the atomic tabSeries counter.
#
# "CRM Deal" only exists after `ferro install-app crm` has materialised its
# schema into the site — otherwise this 404s on an unknown doctype.
#
# (Field names below are illustrative; adjust to the CRM Deal schema in your app.
#  The shape — a parent doc plus a "contacts" child-row array — is the point.)
# Capture the full response, print it, then extract data.name. We use python3 (always
# present in a Ferro install) rather than jq so the name capture works without jq.
DEAL_RESP=$(curl -sS -X POST "${BASE_URL}/api/resource/CRM%20Deal" \
  -H 'Content-Type: application/json' \
  -d '{
        "organization": "Acme Corp",
        "status": "Qualification",
        "annual_revenue": 50000,
        "contacts": [
          {"contact": "jane@acme.example", "is_primary": 1},
          {"contact": "joe@acme.example",  "is_primary": 0}
        ]
      }')
printf '%s' "${DEAL_RESP}" | pp
DEAL_NAME=$(printf '%s' "${DEAL_RESP}" | python3 -c \
  'import sys,json; print((json.load(sys.stdin).get("data") or {}).get("name","") if sys.stdin else "")' 2>/dev/null || true)

echo "created CRM Deal: ${DEAL_NAME}"


# ---------------------------------------------------------------------------
section "6. update a document (PUT /api/resource/CRM Deal/<name>)   [requires: crm]"
# ---------------------------------------------------------------------------
# PUT replaces the supplied fields. Note (LIMITATIONS.md): a child-table update is
# a full-array REPLACE (delete + reinsert), so child row names change — fine
# unless something external references those child names. System fields
# (owner/creation/modified/docstatus/idx/parent*) are server-authoritative and
# cannot be set by the client.
if [ -n "${DEAL_NAME}" ] && [ "${DEAL_NAME}" != "null" ]; then
  curl -sS -X PUT "${BASE_URL}/api/resource/CRM%20Deal/${DEAL_NAME}" \
    -H 'Content-Type: application/json' \
    -d '{"status": "Negotiation", "annual_revenue": 75000}' | pp
else
  echo "skipping update — no CRM Deal name captured (is crm installed?)"
fi


# ---------------------------------------------------------------------------
section "7. delete a document (DELETE /api/resource/CRM Deal/<name>)   [requires: crm]"
# ---------------------------------------------------------------------------
# DELETE returns {"data":"ok"} with HTTP 202. Submitted (docstatus=1) docs are
# blocked from deletion. There is NO link-integrity check on delete (ferro does
# not reimplement Frappe's LinkExistsError), so the row is removed even if other
# docs reference it. -w prints the status code on its own line.
if [ -n "${DEAL_NAME}" ] && [ "${DEAL_NAME}" != "null" ]; then
  curl -sS -X DELETE "${BASE_URL}/api/resource/CRM%20Deal/${DEAL_NAME}" \
    -w '\nHTTP %{http_code}\n' | pp
else
  echo "skipping delete — no CRM Deal name captured (is crm installed?)"
fi


# ---------------------------------------------------------------------------
section "8. token auth (Authorization header) — for a secured site"
# ---------------------------------------------------------------------------
# On a NORMALLY-SECURED site (i.e. NOT serving with --user Administrator), every
# request must carry an API key/secret. ferro accepts the Frappe forms:
#
#     Authorization: token <api_key>:<api_secret>
#     Authorization: Basic base64(<api_key>:<api_secret>)
#
# It verifies against tabUser.api_key + the secret in __Auth — including real
# Frappe Fernet-encrypted (encrypted=1) secrets, decrypted with the site key.
# Bad/unknown credentials return HTTP 401 (no silent Guest downgrade).
#
# Issue a local key/secret with:
#
#     ferro provision-key <site-dir-or-db> <user>
#
# which prints a ready-to-use header line, e.g.:
#
#     Authorization: token 1a2b3c4d5e6f7a8:9b8c7d6e5f4a3b2
#
# Plug that key:secret pair into FERRO_TOKEN below to authenticate explicitly.
# (Left empty here because the local Administrator default already authorizes
#  these calls; this block only runs if you set FERRO_TOKEN.)
FERRO_TOKEN="${FERRO_TOKEN:-}"   # e.g. export FERRO_TOKEN='1a2b3c4d5e6f7a8:9b8c7d6e5f4a3b2'

if [ -n "${FERRO_TOKEN}" ]; then
  echo "# token form:"
  curl -sS "${BASE_URL}/api/method/frappe.auth.get_logged_user" \
    -H "Authorization: token ${FERRO_TOKEN}" | pp

  echo "# Basic form (same credentials, base64-encoded):"
  curl -sS "${BASE_URL}/api/method/frappe.auth.get_logged_user" \
    -u "${FERRO_TOKEN}" | pp
else
  echo "FERRO_TOKEN not set — skipping the authenticated calls."
  echo "Provision one with:  ferro provision-key <site-dir-or-db> <user>"
  echo "then:  export FERRO_TOKEN='<api_key>:<api_secret>'  and re-run."
fi

echo
echo "Done. (All endpoints above are what ferro actually serves — v1 only.)"
