# Ferro Signup — self-serve **Frappe Desk** on `*.ferro.x.frappe.dev`

A tiny, zero-dependency control plane that lets anyone spin up their own **Frappe site** in
seconds: pick a subdomain, pick the apps, and land in the real **Frappe Desk** admin UI —
workspaces, list & form views, search, CRUD writes — served by the pure-Rust `ferro` runtime in
**~15–25 MB of RAM** per site (verified: a fresh ERPNext Desk tenant idles at ~18 MB PSS).

Each tenant runs `ferro serve <site> --desk`: the desk-capable runtime serves the Desk SPA shell,
the `/app`→`/desk` redirect, the shared built assets and the `frappe.desk.*` API straight off the
tenant's SQLite site. The control plane is a transparent reverse proxy in front of it — visiting
`<sub>.ferro.x.frappe.dev` drops you straight into Desk.

Provisioning uses the [`ferro` CLI](../../cli) as a black box (`init / new-site /
install-app`); the desk runtime binary and the shared asset tree are shipped out-of-band (see
**Desk assets** below). The Desk sidebar reflects the apps you chose because each installed app's
**Workspace fixtures are imported** at provision time (the CLI materialises DocType *schema* only).

Live: **https://ferro.x.frappe.dev**

```
            Internet (v4 + v6)
                  │  443 / 80
        ┌─────────▼──────────┐   one WILDCARD cert (DNS-01 via Route53);
        │       Caddy        │   certbot obtains+renews, Caddy serves it
        │  ferro.x.frappe.dev│
        │ *.ferro.x.frappe.dev│
        └─────────┬──────────┘
                  │  reverse_proxy → 127.0.0.1:8080  (Host preserved)
        ┌─────────▼───────────────────────────────────────────────┐
        │            control plane  (control/server.py)            │
        │  Host = apex      →  signup app + provisioning API        │
        │  Host = <sub>.*   →  transparent reverse-proxy → Desk     │
        │                       ( / → /app ; /_ferro/* = control )   │
        └───────┬───────────────────────────────┬──────────────────┘
       provision│ (ferro CLI)      reverse-proxy │ everything (/app /desk /assets /api …)
        ┌───────▼────────┐          ┌────────────▼─────────────────┐
        │ systemd unit   │ ferro    │  ferro serve <site> --desk    │  one per tenant,
        │ ferro-instance@<sub> ────▶│  --assets /opt/ferro/assets   │  127.0.0.1:9000–9600
        └───────┬────────┘          │  127.0.0.1:<port>             │
   /opt/ferro/tenants/<sub>/        └──────┬─────────────────┬──────┘
        apps/ ─symlink→ app-mirror         │ SQLite site     │ shared, read-only
        sites/<host>/db/<...>.db  ◀────────┘    /opt/ferro/assets (built Desk bundles)
```

## Why it stays cheap

* **One shared asset tree.** Frappe Desk's JS/CSS bundles (~50 MB built) are **not** per-tenant.
  A single read-only `/opt/ferro/assets` tree (one `bench build` output, dereferenced) is served
  by every tenant via `ferro serve --assets`. Provisioning never builds or copies assets.
* **Shared app mirror.** Every tenant forge's `apps/` is a *symlink* to one read-only
  `/opt/ferro/app-mirror` (the app repos, cloned once). Provisioning never clones — it
  decompresses the ~600 KB frappe-core seed, materialises the chosen apps' DocType schemas, and
  imports their Workspace fixtures into a fresh SQLite site. A tenant costs a few MB of disk.
* **Pure-`ferro` runtime.** The interpreter-free Rust runtime serves the Desk shell, assets, the
  `frappe.desk.*` API and CRUD/auth/permissions for every installed DocType with **no Python in
  the hot path** — a full ERPNext Desk tenant idles at **~18 MB PSS**.
* **Sleep / wake / reap.** Idle tenants are stopped (RAM freed) after 2 h and woken on the next
  request; tenants idle > 24 h are fully deprovisioned. See the reaper timer.

## Desk assets

The desk-capable `ferro` binary and the built asset tree are shipped to the droplet out-of-band
(they are large / not source, so they don't live in this repo):

```bash
# 1. build the desk-capable runtime (has --desk/--assets) and ship it
(cd ../.. && cargo build --release --bin ferro)
rsync -az ../../target/release/ferro root@ferro.x.frappe.dev:/opt/ferro/runtime/ferro

# 2. stage a self-contained Desk asset tree from any built bench, then ship it
bin/stage-assets.sh /path/to/bench/sites/assets /tmp/ferro-assets
rsync -az --delete /tmp/ferro-assets/ root@ferro.x.frappe.dev:/opt/ferro/assets/
```

`deploy.sh` prints a preflight check for both. Without them, tenants fall back to serving the REST
API only (the Desk shell's bundles would 404). The Desk SPA loads only the **frappe** bundles, so
frappe's built assets are the must-have; app-specific bundles aren't required for the Desk admin UI
of an app's doctypes. A short allowlist of fire-and-forget Desk telemetry methods
(`route_history.deferred_insert`, …) is answered with a no-op success by the control plane so Desk
never pops a "Not found" dialog for methods the pure-Rust runtime doesn't implement.

## On the droplet

```
/opt/ferro/
  runtime/ferro          # the DESK-capable runtime binary (--desk/--assets), shipped out-of-band
  assets/                # the shared, read-only built Desk asset tree (one bench build; ~54 MB)
  stack/                 # the ferro CLI + framework (rsync of the monorepo cli/ + framework/) — used for provisioning
  app-mirror/            # the app repos, cloned once, shared read-only by every tenant
  tenants/<sub>/         # one forge per tenant (apps/ -> symlink to app-mirror) + instance.env
  control/
    server.py            # the control plane (stdlib http.server) — provision + Desk gateway
    static/{signup,instance}.html
    instances.json       # tenant registry (single writer: the control plane)
    reap.token           # admin token for /internal/*
  bin/{run-instance.sh, import-workspaces.py, stage-assets.sh, reap.sh}
  logs/
  signup-src/            # this repo, synced; run deploy.sh from here
```

Systemd units (`/etc/systemd/system/`):

| unit | role |
|---|---|
| `caddy.service` | TLS edge → control plane |
| `ferro-control.service` | the control plane (root; drives `systemctl`) |
| `ferro-instance@<sub>.service` | one tenant bench (`MemoryMax=320M`) |
| `ferro-reaper.timer` → `ferro-reaper.service` | every 15 min, idle policy |

## Endpoints

Apex (`ferro.x.frappe.dev`):

| | |
|---|---|
| `GET /` | the signup SPA |
| `GET /api/apps` | available apps + DocType counts |
| `GET /api/check?sub=` | subdomain availability / validity |
| `POST /api/provision` | `{sub, apps[]}` → starts provisioning a Desk tenant (202) |
| `GET /api/status?sub=` | provisioning progress (step list) |
| `GET /api/instances` | public list of live benches |

Tenant (`<sub>.ferro.x.frappe.dev`) — this **is** Frappe Desk:

| | |
|---|---|
| `GET /` | 302 → `/app` (land in Desk) |
| `GET /_ferro/info` | tenant metadata + **live RSS/PSS** (control plane; never proxied) |
| `/app` `/desk` `/assets/*` `/api/*` `/files/*` … | reverse-proxied to the tenant's `ferro serve --desk` |
| (while provisioning) | a self-contained waiting page that polls `/_ferro/info`, then bounces to `/app` |

Internal (loopback only; rejected if `X-Forwarded-For` is present; token via `reap.token`):
`GET /internal/tls-check?domain=` (Caddy ask-gate), `POST /internal/reap`,
`POST /internal/deprovision?sub=`.

## Operate

```bash
# deploy / refresh (from your workstation)
rsync -az deploy/signup/ root@ferro.x.frappe.dev:/opt/ferro/signup-src/
ssh root@ferro.x.frappe.dev bash /opt/ferro/signup-src/deploy.sh

# logs
ssh root@… journalctl -u ferro-control -f
ssh root@… tail -f /opt/ferro/logs/instance-<sub>.log

# provision / tear down by hand
curl -X POST https://ferro.x.frappe.dev/api/provision -H 'content-type: application/json' \
     -d '{"sub":"acme","apps":["crm","helpdesk"]}'
TOK=$(ssh root@… cat /opt/ferro/control/reap.token)
ssh root@… "curl -s -X POST -H 'X-Ferro-Admin: $TOK' \
     'http://127.0.0.1:8080/internal/deprovision?sub=acme'"
```

Tunables (env on `ferro-control.service`): `FERRO_ASSETS`, `FERRO_MAX_INSTANCES`,
`FERRO_POPULATE_ROWS`, `FERRO_IDLE_STOP`, `FERRO_IDLE_DELETE`, `FERRO_BASE_DOMAIN`.

## Security model

* Each bench is an intentionally-open **sandbox**: it pins the default request user to Administrator
  (`ferro serve --default-user Administrator`), so the Desk and REST API are reachable without logging
  in. The runtime itself now enforces **real Frappe auth** — password login, `tabSessions`, and row +
  field-level permissions (permlevel masking + DocShare) — so this open posture is a deliberate,
  explicit opt-in (after the framework-compat audit, bare `--desk` defaults to **Guest** and warns),
  **not** a missing-auth gap. It is by design for a demo; it is **not** multi-user.
* Tenants are isolated by process + port + their own SQLite site. The proxy only ever dials
  `127.0.0.1:<that tenant's port>`; it cannot be steered to another host.
* `/internal/*` is loopback + `X-Forwarded-For`-gated + token-gated, so the reaper/teardown/ask
  endpoints are unreachable through the public edge.
* TLS is **one wildcard certificate** for `ferro.x.frappe.dev` + `*.ferro.x.frappe.dev`, so there is
  **zero per-subdomain issuance** and no exposure to the shared `frappe.dev` Let's Encrypt rate limit
  no matter how many benches exist. It is obtained via the ACME **DNS-01** challenge through Route53.
  Caddy serves it as a static cert (`tls /etc/caddy/wildcard/{fullchain,privkey}.pem`).

Hardening applied after an adversarial audit of the live deployment:

* **Static serving is path-confined** by `realpath`, not string-stripping — the only file-read surface
  (`/static/<x>`) cannot escape `STATIC_DIR` (a prior `rel.replace("..","")` filter was bypassable
  and is fixed).
* `/api/provision` is **rate-limited per client IP** (`FERRO_PROVISION_MAX_IP` / window) on top of the
  global `FERRO_MAX_INSTANCES` cap, and request bodies are capped (`FERRO_MAX_BODY`).
* The proxy strips hop-by-hop + identity headers both ways (no `Transfer-Encoding` smuggling, no
  duplicate `Server`/`Date`). Subdomains are 2–32 chars and punycode (`xn--`) is rejected.
* Deprovisioning clears in-memory job state (a removed subdomain is immediately reclaimable), and the
  reaper's idle clock is only advanced by real data-plane traffic, not the waiting page's memory-polling.

## TLS / the wildcard certificate

A single Let's Encrypt **wildcard** cert (`*.ferro.x.frappe.dev` + `ferro.x.frappe.dev`) covers every
tenant. It is obtained by **certbot** via the **DNS-01** challenge through **Route53** — *not* Caddy's
own route53 plugin, because the scoped IAM user (`Atlas-Aditya-x.frappe.dev`) can **change** records
but cannot **list** them (`ListResourceRecordSets`/`ListHostedZonesByName` are denied), and Caddy's
libdns-route53 reads before writing. certbot's plugin does a blind `UPSERT`, which the policy allows.

```
creds:        /root/.aws/{credentials,config}              # boto3 default chain (renewal runs headless)
issue:        certbot certonly --dns-route53 --cert-name ferro -d ferro.x.frappe.dev -d '*.ferro.x.frappe.dev'
renewal:      certbot.timer (twice daily)  →  deploy hook /etc/letsencrypt/renewal-hooks/deploy/10-caddy.sh
deploy hook:  /opt/ferro/bin/install-wildcard-cert.sh  → copies cert to /etc/caddy/wildcard (caddy-owned) + reloads Caddy
serve:        Caddyfile  tls /etc/caddy/wildcard/{fullchain,privkey}.pem
```

`certbot renew --dry-run` is green, so renewal is unattended. (If the zone changes, re-pin the hosted
zone id; today it is `Z09074453NDASXA598485` for `x.frappe.dev`.)

## Relationship to the runtime & CLI

This system **consumes** the monorepo's CLI + framework — [`cli/`](../../cli) and
[`framework/`](../../framework), formerly the standalone `ferro-stack` — synced to
`/opt/ferro/stack` for provisioning, and the desk-capable runtime built from the repo root for
serving. To adopt runtime improvements, rebuild + rsync `/opt/ferro/runtime/ferro`; to adopt
CLI/seed improvements, re-sync `stack/` + `app-mirror/`.
The Desk caveats (no realtime, deferred `/api/method/*` tail, no submit/cancel/workflow, frappe-only
assets) are inherited from Ferro and surfaced honestly in the signup copy.
