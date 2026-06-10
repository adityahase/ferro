#!/usr/bin/env python3
"""
ferro-control — the control plane for the Ferro signup / self-serve flow.

A single zero-dependency (Python stdlib) service that, depending on the request Host:

  * apex (ferro.x.frappe.dev)          -> the signup app + provisioning API
  * <sub>.ferro.x.frappe.dev           -> a transparent reverse-proxy in front of that
                                          tenant's Frappe Desk (`ferro serve --desk`):
                                          / -> /app, and /app,/desk,/assets,/api,... are
                                          proxied to the backend port

It also answers Caddy's on-demand-TLS "ask" endpoint so certificates are only ever
minted for the apex or a genuinely provisioned tenant (protecting the shared
*.frappe.dev ACME quota).

Provisioning a tenant = one `ferro` forge whose apps/ is a symlink to a shared,
read-only app-mirror + a fresh frappe-core seed site + `install-app` for each chosen
app + workspace import, served as Frappe Desk by a systemd template unit
`ferro-instance@<sub>`.

Run as the `ferro-control.service` systemd unit (see systemd/).
"""
import html
import json
import os
import re
import shutil
import socket
import subprocess
import threading
import time
import urllib.error
import urllib.parse
import urllib.request
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

# --------------------------------------------------------------------------- config
def envv(k, d):
    return os.environ.get(k, d)

BASE_DOMAIN = envv("FERRO_BASE_DOMAIN", "ferro.x.frappe.dev")
APEX        = BASE_DOMAIN
SUFFIX      = "." + BASE_DOMAIN
LISTEN_HOST = envv("FERRO_CONTROL_HOST", "127.0.0.1")
LISTEN_PORT = int(envv("FERRO_CONTROL_PORT", "8080"))

FERRO_HOME  = envv("FERRO_HOME", "/opt/ferro/stack")
FERRO_CLI   = os.path.join(FERRO_HOME, "bin", "ferro")
APP_MIRROR  = envv("FERRO_APP_MIRROR", "/opt/ferro/app-mirror")
TENANTS_DIR = envv("FERRO_TENANTS", "/opt/ferro/tenants")
CONTROL_DIR = envv("FERRO_CONTROL_DIR", "/opt/ferro/control")
STATIC_DIR  = os.path.join(CONTROL_DIR, "static")
STATE_PATH  = os.path.join(CONTROL_DIR, "instances.json")
LOG_DIR     = envv("FERRO_LOG_DIR", "/opt/ferro/logs")

# Desk mode: tenants serve the full Frappe Desk SPA. The shared, read-only built asset tree
# (one `bench build` output) is pointed at by every tenant's `ferro serve --assets`; the
# workspace importer materialises each installed app's Workspace fixtures so the Desk sidebar
# reflects the apps the user chose (build_db.py imports DocType *schema* only).
ASSETS_DIR  = envv("FERRO_ASSETS", "/opt/ferro/assets")
IMPORTER    = envv("FERRO_IMPORTER", "/opt/ferro/bin/import-workspaces.py")
DESK_ENABLED = os.path.isdir(ASSETS_DIR)

# Fire-and-forget Desk methods with no meaningful return value that the pure-Rust runtime
# doesn't implement; we answer them with a no-op success so Desk doesn't surface a "Not found"
# error dialog on routine navigation. Keep this list to genuinely side-effect-only / telemetry
# calls — never anything that returns data the UI renders.
DESK_NOOP_METHODS = {
    "frappe.desk.doctype.route_history.route_history.deferred_insert",  # records visited routes
    "frappe.core.doctype.access_log.access_log.make_access_log",        # access/download logging
    "frappe.client.is_document_amended",                               # null == not amended
    "frappe.desk.doctype.notification_log.notification_log.mark_all_as_read",
}

PORT_LO, PORT_HI = 9000, 9600
MAX_INSTANCES    = int(envv("FERRO_MAX_INSTANCES", "60"))
POPULATE_ROWS    = int(envv("FERRO_POPULATE_ROWS", "120"))
PROVISION_TIMEOUT = 240
MAX_BODY         = int(envv("FERRO_MAX_BODY", str(12 * 1024 * 1024)))   # proxy/provision body cap
PROVISION_WINDOW = int(envv("FERRO_PROVISION_WINDOW", "600"))           # per-IP rate window (s)
PROVISION_MAX_IP = int(envv("FERRO_PROVISION_MAX_IP", "6"))             # provisions / IP / window

# hop-by-hop + identity-leaking headers never forwarded in either direction
HOP_HEADERS = {"connection", "keep-alive", "proxy-authenticate", "proxy-authorization",
               "te", "trailer", "transfer-encoding", "upgrade", "content-length"}
IDLE_STOP   = int(envv("FERRO_IDLE_STOP", str(2 * 3600)))     # stop (free RAM) after 2h idle
IDLE_DELETE = int(envv("FERRO_IDLE_DELETE", str(24 * 3600)))  # deprovision after 24h idle

def _reap_token():
    t = os.environ.get("FERRO_REAP_TOKEN", "")
    if t:
        return t
    try:
        with open(os.path.join(CONTROL_DIR, "reap.token")) as f:
            return f.read().strip()
    except OSError:
        return ""
REAP_TOKEN = _reap_token()

# Reverse-proxy opener that does NOT follow redirects, so the backend's /app -> /desk 301
# is relayed to the browser (real Frappe behaviour) instead of being swallowed server-side.
class _NoRedirectHandler(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, *a, **k):
        return None  # -> urllib raises HTTPError for 3xx, which _proxy relays verbatim
NoRedirectProxy = urllib.request.build_opener(_NoRedirectHandler)

# reserved / blocked subdomains
BLOCKLIST = {
    "www", "api", "app", "admin", "administrator", "root", "ferro", "mail", "smtp",
    "imap", "pop", "ns", "ns1", "ns2", "dns", "ftp", "ssh", "vpn", "test", "staging",
    "dev", "prod", "production", "status", "dashboard", "control", "panel", "console",
    "billing", "pay", "payment", "auth", "login", "signup", "register", "account",
    "static", "assets", "cdn", "img", "images", "media", "files", "download", "docs",
    "doc", "blog", "support", "help", "internal", "localhost", "frappe", "x",
}
SUB_RE = re.compile(r"^[a-z0-9](?:[a-z0-9-]{0,30}[a-z0-9])$")  # 2–32 chars, no leading/trailing hyphen

# --------------------------------------------------------------------------- registry
def load_json(path, default):
    try:
        with open(path) as f:
            return json.load(f)
    except (FileNotFoundError, json.JSONDecodeError):
        return default

# Desk-oriented blurbs: under ferro Desk every app's doctypes are browsable/editable in the
# Frappe admin UI; apps that ship Desk Workspaces also add sidebar sections.
DESK_DESC = {
    "erpnext":  "Full ERP — Selling, Buying, Stock, Accounts, Manufacturing & more, native in Desk.",
    "hrms":     "HR & Payroll — employees, leaves, attendance, salary, with HR workspaces in Desk.",
    "crm":      "Sales CRM — leads, deals & contacts as Desk doctypes with a CRM workspace.",
    "helpdesk": "Support desk — tickets, teams & SLAs as Desk doctypes with a Helpdesk workspace.",
    "gameplan": "Team communication — projects, discussions & pages as Desk doctypes.",
}

# Apps whose real UI is a frappe-ui SPA on its own route (not Desk workspaces). Installing one flips
# the tenant to the "ferrod" runtime tier, where ferro embeds CPython and auto-routes that app's
# whitelisted /api/method/<app>.* calls into real controller Python — so the SPA actually works.
SPA_APPS = {"crm", "gameplan", "helpdesk"}
# Each SPA app's entry route (its hooks.py website_route_rules base), for post-signup deep links.
APP_ROUTES = {"crm": "/crm", "gameplan": "/g", "helpdesk": "/helpdesk"}

# Canonical display names — acronyms / mixed case the UI can't infer from the bare app name.
APP_LABEL = {"erpnext": "ERPNext", "hrms": "HRMS", "crm": "CRM",
             "helpdesk": "Helpdesk", "gameplan": "Gameplan"}

def app_label(name):
    return APP_LABEL.get(name, name.capitalize())


def _count_jsons(appdir, segment):
    n = 0
    seg = os.sep + segment + os.sep
    for root, _dirs, files in os.walk(appdir):
        if seg in root + os.sep:
            n += sum(1 for f in files if f.endswith(".json") and not f.startswith("test_"))
    return n

def app_registry():
    reg = load_json(os.path.join(FERRO_HOME, "ferro.json"), {}).get("apps", {})
    out = {}
    for name, ent in reg.items():
        appdir = os.path.join(APP_MIRROR, name)
        avail = os.path.isdir(appdir)
        dt = _count_jsons(appdir, "doctype") if avail else 0
        ws = _count_jsons(appdir, "workspace") if avail else 0
        out[name] = {
            "name": name,
            "desc": DESK_DESC.get(name, ent.get("desc", "")),
            "doctypes": dt,
            "workspaces": ws,
            "available": avail,
        }
    return out

REGISTRY = app_registry()

# --------------------------------------------------------------------------- state
STATE_LOCK = threading.RLock()
PROVISION_LOCK = threading.Lock()
JOBS = {}            # sub -> {sub, host, status, steps:[{name,status,detail}], error, url}

_PROV_HITS = {}      # ip -> [timestamps]  (per-IP provision rate limit)
_PROV_HITS_LOCK = threading.Lock()

def provision_rate_ok(ip):
    now = time.time()
    with _PROV_HITS_LOCK:
        hits = [t for t in _PROV_HITS.get(ip, []) if now - t < PROVISION_WINDOW]
        if len(hits) >= PROVISION_MAX_IP:
            _PROV_HITS[ip] = hits
            return False
        hits.append(now)
        _PROV_HITS[ip] = hits
        return True

def read_state():
    with STATE_LOCK:
        return load_json(STATE_PATH, {})

def write_state(state):
    with STATE_LOCK:
        os.makedirs(os.path.dirname(STATE_PATH), exist_ok=True)
        tmp = STATE_PATH + ".tmp"
        with open(tmp, "w") as f:
            json.dump(state, f, indent=1)
        os.replace(tmp, STATE_PATH)

def upsert_instance(sub, **fields):
    with STATE_LOCK:
        st = read_state()
        rec = st.get(sub, {})
        rec["sub"] = sub
        rec.update(fields)
        st[sub] = rec
        write_state(st)
        return rec

def remove_instance(sub):
    with STATE_LOCK:
        st = read_state()
        st.pop(sub, None)
        write_state(st)

def known_host(host):
    host = (host or "").lower().split(":")[0]
    if host == APEX:
        return True
    if not host.endswith(SUFFIX):
        return False
    sub = host[: -len(SUFFIX)]
    if sub in read_state():
        return True
    j = JOBS.get(sub)
    return bool(j and j.get("status") in ("provisioning", "ready"))

# --------------------------------------------------------------------------- ports
def port_in_use(port):
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as s:
        s.settimeout(0.2)
        return s.connect_ex(("127.0.0.1", port)) == 0

def alloc_port():
    with STATE_LOCK:
        used = {r.get("port") for r in read_state().values() if r.get("port")}
        used |= {j.get("port") for j in JOBS.values() if j.get("port")}
        for p in range(PORT_LO, PORT_HI):
            if p in used:
                continue
            if port_in_use(p):
                continue
            return p
    return None

# --------------------------------------------------------------------------- ferro shell
def ferro_env(forge=None):
    env = dict(os.environ)
    env["FERRO_HOME"] = FERRO_HOME
    if forge:
        env["FERRO_FORGE"] = forge
    return env

def sh(cmd, env=None, cwd=None, timeout=180):
    """Run a command, return (rc, combined_output)."""
    try:
        p = subprocess.run(cmd, env=env, cwd=cwd, timeout=timeout,
                           stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
        return p.returncode, p.stdout
    except subprocess.TimeoutExpired as e:
        return 124, (e.output or "") + "\n[timed out]"

def ferro(args, **kw):
    return sh(["python3", FERRO_CLI] + args, **kw)

# --------------------------------------------------------------------------- systemd
def systemctl(*args, timeout=60):
    return sh(["systemctl"] + list(args), timeout=timeout)

def instance_unit(sub):
    return f"ferro-instance@{sub}.service"

def instance_pid(sub):
    rc, out = systemctl("show", instance_unit(sub), "-p", "MainPID", "--value")
    out = out.strip()
    if rc == 0 and out.isdigit() and int(out) > 0:
        return int(out)
    return None

def instance_active(sub):
    rc, out = systemctl("is-active", instance_unit(sub))
    return out.strip() == "active"

def proc_mem_kb(pid):
    """Return (rss_kb, pss_kb) for a pid, best-effort."""
    rss = pss = 0
    try:
        with open(f"/proc/{pid}/smaps_rollup") as f:
            for line in f:
                if line.startswith("Rss:"):
                    rss = int(line.split()[1])
                elif line.startswith("Pss:"):
                    pss = int(line.split()[1])
    except OSError:
        try:
            with open(f"/proc/{pid}/status") as f:
                for line in f:
                    if line.startswith("VmRSS:"):
                        rss = int(line.split()[1])
        except OSError:
            pass
    return rss, pss

# --------------------------------------------------------------------------- provisioning
def validate_sub(sub):
    sub = (sub or "").strip().lower()
    if not sub:
        return None, "Pick a subdomain."
    if not SUB_RE.match(sub):
        return None, "Use 2-32 chars: lowercase letters, numbers, hyphens (not at the ends)."
    if sub.startswith("xn--"):
        return None, "Unicode / punycode subdomains aren't allowed."
    if sub in BLOCKLIST:
        return None, "That subdomain is reserved."
    if sub in read_state():
        return None, "That subdomain is already taken."
    j = JOBS.get(sub)
    if j and j.get("status") in ("provisioning", "ready"):
        return None, "That subdomain is already taken."
    return sub, None

def clean_apps(apps):
    out = []
    for a in apps or []:
        a = str(a).strip().lower()
        if a in REGISTRY and REGISTRY[a]["available"] and a not in out:
            out.append(a)
    return out

# apps that reuse another app's doctypes (hrms is built on erpnext masters).
APP_DEPS = {"hrms": ["erpnext"]}

def expand_apps(apps):
    """Add required dependency apps, ordered so a dependency installs before its dependent."""
    out = []
    def add(a):
        for dep in APP_DEPS.get(a, []):
            if dep in REGISTRY and REGISTRY[dep]["available"]:
                add(dep)
        if a not in out:
            out.append(a)
    for a in apps:
        add(a)
    return out

def site_db_path(forge, host):
    """Locate the SQLite db for a tenant site (forge/sites/<host>/db/<name>.db)."""
    dbdir = os.path.join(forge, "sites", host, "db")
    try:
        for f in os.listdir(dbdir):
            if f.endswith(".db"):
                return os.path.join(dbdir, f)
    except OSError:
        pass
    return None

def import_workspaces(forge, host, apps):
    """Materialise the installed apps' Desk Workspace fixtures into the tenant site so the
    Desk sidebar shows each app's sections. Best-effort: a failure here only means a sparser
    sidebar, never a failed provision."""
    if not os.path.exists(IMPORTER):
        return False
    db = site_db_path(forge, host)
    if not db:
        return False
    cmd = ["python3", IMPORTER, "--db", db]
    for a in apps:
        appdir = os.path.join(APP_MIRROR, a)
        if os.path.isdir(appdir):
            cmd += ["--app-dir", appdir]
    rc, _out = sh(cmd, timeout=120)
    return rc == 0

def set_site_config(forge, host, **keys):
    """Merge keys into the tenant's site_config.json (best-effort)."""
    cfgp = os.path.join(forge, "sites", host, "site_config.json")
    try:
        cfg = load_json(cfgp, {})
        cfg.update(keys)
        with open(cfgp, "w") as f:
            json.dump(cfg, f, indent=1)
        return True
    except OSError:
        return False


def _step(job, name, detail=""):
    s = {"name": name, "status": "running", "detail": detail}
    job["steps"].append(s)
    return s

def _ok(s, detail=None):
    s["status"] = "ok"
    if detail is not None:
        s["detail"] = detail

def provision(sub, apps):
    """Run the full provisioning recipe. Updates JOBS[sub] in place."""
    host = f"{sub}{SUFFIX}"
    forge = os.path.join(TENANTS_DIR, sub)
    job = JOBS[sub]
    env = ferro_env(forge=forge)
    port = None
    try:
        with PROVISION_LOCK:
            if len(read_state()) >= MAX_INSTANCES:
                raise RuntimeError("The demo is at capacity right now — please try again later.")
            port = alloc_port()
            if not port:
                raise RuntimeError("No free ports available.")
            job["port"] = port
            # reserve immediately so concurrent ports/subs don't collide
            upsert_instance(sub, host=host, port=port, apps=apps,
                            status="provisioning",
                            created=int(time.time()), last_seen=int(time.time()))

            s = _step(job, "workspace", "creating forge")
            if os.path.exists(forge):
                shutil.rmtree(forge, ignore_errors=True)
            rc, out = ferro(["init", forge, "--port", str(port)], env=env)
            if rc != 0:
                raise RuntimeError(f"forge init failed:\n{out}")
            # share the read-only app mirror instead of cloning per tenant
            appslink = os.path.join(forge, "apps")
            shutil.rmtree(appslink, ignore_errors=True)
            os.symlink(APP_MIRROR, appslink)
            cfgp = os.path.join(forge, "ferro.json")
            cfg = load_json(cfgp, {})
            cfg["default_site"] = host
            with open(cfgp, "w") as f:
                json.dump(cfg, f, indent=1)
            _ok(s, "forge ready")

            s = _step(job, "site", "decompressing frappe-core seed")
            rc, out = ferro(["new-site", host], env=env, cwd=forge)
            if rc != 0:
                raise RuntimeError(f"new-site failed:\n{out}")
            _ok(s, "278 frappe-core doctypes")

            for app in apps:
                lbl = app_label(app)
                s = _step(job, f"app:{lbl}", f"materialising {lbl} schema")
                rc, out = ferro(["install-app", app, "--site", host], env=env, cwd=forge, timeout=180)
                if rc != 0:
                    raise RuntimeError(f"install-app {app} failed:\n{out[-1500:]}")
                _ok(s, f"{lbl} installed ({REGISTRY.get(app,{}).get('doctypes','?')} doctypes)")

            # Run each app's after_install hook (its seed/master data — CRM statuses, fields
            # layouts, industries, …) via the python-enabled runtime. Native install-app materialises
            # DocType *schema* only, and the app frontends crash on the missing seed data (e.g. CRM's
            # status badges). Best-effort: a failure leaves a schema-only site, which still serves.
            seed_apps = [a for a in apps if a != "frappe"]
            if seed_apps:
                s = _step(job, "seed", "seeding app data")
                site_dir = os.path.join(forge, "sites", host)
                py_bin = envv("FERRO_RUNTIME_PY_BIN", "/opt/ferro/runtime/ferro-py")
                pyhome = envv("FERRO_PYHOME", "/opt/ferro/python")
                henv = dict(env)
                henv["PYTHONHOME"] = pyhome
                henv["LD_LIBRARY_PATH"] = f"{pyhome}/lib:" + henv.get("LD_LIBRARY_PATH", "")
                henv["FERRO_SHIM"] = os.path.join(FERRO_HOME, "framework", "shim")
                henv["FERRO_REPOS"] = APP_MIRROR
                if os.path.exists(py_bin):
                    rc, out = sh([py_bin, "install-hooks", site_dir] + seed_apps,
                                 env=henv, cwd=forge, timeout=180)  # best-effort
                    _ok(s, "app data seeded" if rc == 0 else "schema-only (seed incomplete)")
                else:
                    _ok(s, "schema-only (no python runtime)")

            s = _step(job, "workspaces", "building the Desk sidebar")
            if import_workspaces(forge, host, apps):
                _ok(s, "app workspaces added to Desk")
            else:
                _ok(s, "core Desk workspaces ready")

            # SPA apps (crm/gameplan/helpdesk) need their real Python API. Flip the tenant to the
            # ferrod runtime tier so /api/method/<app>.* runs real controllers; the launcher uses the
            # python-enabled ferro (and falls back to pure-Rust if it isn't staged).
            if set(apps) & SPA_APPS:
                s = _step(job, "runtime", "enabling real app methods")
                if set_site_config(forge, host, web_runtime="ferrod"):
                    _ok(s, "ferrod tier (live app APIs)")
                else:
                    _ok(s, "default runtime")

            s = _step(job, "data", "adding demo rows")
            ferro(["populate", "--site", host, "--rows", str(POPULATE_ROWS)],
                  env=env, cwd=forge, timeout=120)  # best-effort
            _ok(s, "sample data ready")

            s = _step(job, "launch", "starting Desk")
            write_instance_env(sub, host, port, apps)
            rc, out = systemctl("enable", "--now", instance_unit(sub))
            if rc != 0:
                raise RuntimeError(f"service start failed:\n{out}")
            _ok(s, "service up")

        # health wait (outside the global lock)
        s = _step(job, "health", "waiting for first response")
        if not wait_health(port, timeout=40):
            raise RuntimeError("backend did not become healthy in time")
        _ok(s, "responding")

        upsert_instance(sub, status="running", last_seen=int(time.time()))
        job["status"] = "ready"
        job["url"] = f"https://{host}/app"          # land straight in Desk
        job["api"] = f"https://{host}/api/resource/DocType"
        job["desk"] = DESK_ENABLED
        # Per-app deep links for the success screen (only the installed SPA apps).
        job["app_routes"] = {a: APP_ROUTES[a] for a in apps if a in APP_ROUTES}
    except Exception as e:  # noqa: BLE001
        job["status"] = "error"
        job["error"] = str(e)
        # best-effort cleanup so the subdomain can be retried
        try:
            systemctl("disable", "--now", instance_unit(sub))
        except Exception:
            pass
        shutil.rmtree(forge, ignore_errors=True)
        remove_instance(sub)

def write_instance_env(sub, host, port, apps):
    forge = os.path.join(TENANTS_DIR, sub)
    lines = [
        f"SUB={sub}",
        f"SITE={host}",
        f"PORT={port}",
        f"APPS={','.join(apps)}",
    ]
    with open(os.path.join(forge, "instance.env"), "w") as f:
        f.write("\n".join(lines) + "\n")

def wait_health(port, timeout=40):
    deadline = time.time() + timeout
    url = f"http://127.0.0.1:{port}/api/resource/DocType?limit_page_length=1"
    while time.time() < deadline:
        try:
            with urllib.request.urlopen(url, timeout=4) as r:
                if r.status == 200:
                    return True
        except Exception:
            time.sleep(0.8)
    return False

_LAST_SEEN_BUMP = {}          # sub -> ts of last persisted last_seen (throttle disk writes)
_LAST_SEEN_LOCK = threading.Lock()
LAST_SEEN_THROTTLE = 30       # persist last_seen at most once per N seconds per tenant

def _touch_last_seen(sub):
    """Persist last_seen, throttled — Desk fires dozens of requests per page load and we must
    not rewrite instances.json on every one."""
    now = int(time.time())
    with _LAST_SEEN_LOCK:
        if now - _LAST_SEEN_BUMP.get(sub, 0) < LAST_SEEN_THROTTLE:
            return
        _LAST_SEEN_BUMP[sub] = now
    upsert_instance(sub, last_seen=now)

def ensure_awake(sub):
    """Return a running instance's port, waking a slept one on demand. Hot-path friendly: a
    cheap localhost socket check first; only a sleeping bench pays the systemctl/health cost."""
    rec = read_state().get(sub)
    if not rec:
        return None
    port = rec.get("port")
    # Fast path: the port is live -> it's up. Skip the per-request `systemctl is-active`
    # subprocess entirely (Desk would spawn one per asset otherwise).
    if port and port_in_use(port):
        _touch_last_seen(sub)
        return port
    # Slow path: wake a slept/stopped bench.
    systemctl("start", instance_unit(sub))
    if port and wait_health(port, timeout=25):
        upsert_instance(sub, status="running", last_seen=int(time.time()))
        with _LAST_SEEN_LOCK:
            _LAST_SEEN_BUMP[sub] = int(time.time())
        return port
    return port if port and port_in_use(port) else None

# --------------------------------------------------------------------------- HTTP
def read_static(name):
    path = os.path.join(STATIC_DIR, name)
    try:
        with open(path, "rb") as f:
            return f.read()
    except OSError:
        return None

CTYPE = {
    ".html": "text/html; charset=utf-8", ".css": "text/css", ".js": "application/javascript",
    ".svg": "image/svg+xml", ".json": "application/json", ".ico": "image/x-icon",
}

class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"
    server_version = "ferro-control"

    def log_message(self, *a):
        pass

    # ---- helpers
    def _host(self):
        h = self.headers.get("X-Forwarded-Host") or self.headers.get("Host") or ""
        return h.lower().split(",")[0].strip().split(":")[0]

    def _sub(self, host):
        if host.endswith(SUFFIX):
            return host[: -len(SUFFIX)]
        return None

    def _send(self, code, body=b"", ctype="text/plain; charset=utf-8", extra=None):
        if isinstance(body, str):
            body = body.encode("utf-8")
        self.send_response(code)
        self.send_header("Content-Type", ctype)
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Cache-Control", "no-store")
        for k, v in (extra or {}).items():
            self.send_header(k, v)
        self.end_headers()
        if self.command != "HEAD":
            self.wfile.write(body)

    def _json(self, code, obj):
        self._send(code, json.dumps(obj), "application/json")

    def _read_body(self):
        n = int(self.headers.get("Content-Length", 0) or 0)
        if n > MAX_BODY:
            return None   # caller responds 413
        return self.rfile.read(n) if n else b""

    def _client_ip(self):
        xff = self.headers.get("X-Forwarded-For", "")
        return (xff.split(",")[0].strip() if xff else "") or self.client_address[0]

    # ---- dispatch
    def do_GET(self):
        try:
            self._route("GET")
        except BrokenPipeError:
            pass
        except Exception as e:  # noqa: BLE001
            try:
                self._send(500, f"control error: {e}")
            except Exception:
                pass

    do_HEAD = do_GET

    def do_POST(self):
        try:
            self._route("POST")
        except BrokenPipeError:
            pass
        except Exception as e:  # noqa: BLE001
            try:
                self._json(500, {"error": str(e)})
            except Exception:
                pass

    do_PUT = do_DELETE = do_PATCH = do_POST

    def _route(self, method):
        path = urllib.parse.urlparse(self.path).path
        # /internal/* is reachable only from direct localhost callers (Caddy ask, reaper);
        # any request that arrived via the reverse proxy carries X-Forwarded-For -> reject.
        if path.startswith("/internal/"):
            if self.headers.get("X-Forwarded-For"):
                return self._send(403, "forbidden")
            if path == "/internal/tls-check":
                qs = urllib.parse.parse_qs(urllib.parse.urlparse(self.path).query)
                dom = (qs.get("domain", [""])[0]).lower()
                ok = known_host(dom)
                return self._send(200 if ok else 404, "ok" if ok else "no")
            if path == "/internal/reap" and method == "POST":
                if REAP_TOKEN and self.headers.get("X-Ferro-Admin") != REAP_TOKEN:
                    return self._send(403, "forbidden")
                return self._json(200, reap())
            if path == "/internal/deprovision" and method == "POST":
                if REAP_TOKEN and self.headers.get("X-Ferro-Admin") != REAP_TOKEN:
                    return self._send(403, "forbidden")
                qs = urllib.parse.parse_qs(urllib.parse.urlparse(self.path).query)
                sub = (qs.get("sub", [""])[0]).strip().lower()
                if not sub or sub not in read_state():
                    return self._json(404, {"error": "unknown sub"})
                deprovision(sub)
                return self._json(200, {"deprovisioned": sub})
            return self._send(404, "no")
        if path == "/healthz":
            return self._send(200, "ok")

        host = self._host()
        sub = self._sub(host)
        is_apex = host == APEX or host in ("", "localhost", LISTEN_HOST) or sub is None

        if is_apex:
            return self._apex(method, path)
        return self._tenant(method, path, sub, host)

    # ---- apex (signup app + API)
    def _apex(self, method, path):
        if method == "GET" and path in ("/", "/index.html"):
            body = read_static("signup.html") or b"signup app not deployed"
            return self._send(200, body, CTYPE[".html"])
        if method == "GET" and path == "/api/apps":
            return self._json(200, {"apps": list(REGISTRY.values()),
                                    "base_domain": BASE_DOMAIN,
                                    "desk": DESK_ENABLED})
        if method == "GET" and path == "/api/check":
            qs = urllib.parse.parse_qs(urllib.parse.urlparse(self.path).query)
            sub, err = validate_sub(qs.get("sub", [""])[0])
            return self._json(200, {"available": sub is not None,
                                    "reason": err or "available",
                                    "host": f"{sub}{SUFFIX}" if sub else None})
        if method == "GET" and path == "/api/instances":
            return self._json(200, {"instances": public_instances(), "max": MAX_INSTANCES})
        if method == "GET" and path == "/api/status":
            qs = urllib.parse.parse_qs(urllib.parse.urlparse(self.path).query)
            sub = qs.get("sub", [""])[0]
            j = JOBS.get(sub)
            if not j:
                rec = read_state().get(sub)
                if rec:
                    return self._json(200, {"status": "ready", "host": rec["host"],
                                            "url": f"https://{rec['host']}/", "steps": []})
                return self._json(404, {"error": "unknown job"})
            return self._json(200, j)
        if method == "POST" and path == "/api/provision":
            return self._provision()
        if method == "GET" and path.startswith("/static/"):
            return self._static(path[len("/static/"):])
        # unknown apex path
        if path.startswith("/api/"):
            return self._json(404, {"error": "not found"})
        body = read_static("signup.html") or b"not found"
        return self._send(200, body, CTYPE[".html"])

    def _static(self, rel):
        # confine strictly to STATIC_DIR — resolve symlinks/.. and verify containment,
        # never trust string-stripping (an absolute or ../-laden path must not escape).
        base = os.path.realpath(STATIC_DIR)
        full = os.path.realpath(os.path.join(base, rel.lstrip("/")))
        if full != base and not full.startswith(base + os.sep):
            return self._send(404, "not found")
        if not os.path.isfile(full):
            return self._send(404, "not found")
        try:
            with open(full, "rb") as f:
                body = f.read()
        except OSError:
            return self._send(404, "not found")
        ext = os.path.splitext(full)[1]
        return self._send(200, body, CTYPE.get(ext, "application/octet-stream"))

    def _provision(self):
        raw = self._read_body()
        if raw is None:
            return self._json(413, {"error": "request too large"})
        try:
            data = json.loads(raw or b"{}")
        except json.JSONDecodeError:
            return self._json(400, {"error": "invalid json"})
        if not provision_rate_ok(self._client_ip()):
            return self._json(429, {"error": "Too many benches from your network — give it a minute."})
        sub, err = validate_sub(data.get("sub"))
        if err:
            return self._json(400, {"error": err})
        apps = clean_apps(data.get("apps"))
        if not apps:
            return self._json(400, {"error": "Choose at least one app."})
        apps = expand_apps(apps)   # pull in required deps (e.g. hrms -> erpnext)
        if len(read_state()) >= MAX_INSTANCES:
            return self._json(503, {"error": "Demo at capacity; try again later."})
        host = f"{sub}{SUFFIX}"
        JOBS[sub] = {"sub": sub, "host": host, "status": "provisioning",
                     "steps": [], "error": None, "url": None, "apps": apps}
        threading.Thread(target=provision, args=(sub, apps), daemon=True).start()
        return self._json(202, {"sub": sub, "host": host, "status": "provisioning"})

    # ---- tenant (Frappe Desk, served transparently)
    #
    # A ready tenant *is* Frappe Desk: every request except the control-plane `/_ferro/*`
    # endpoints is reverse-proxied to that tenant's desk-mode `ferro serve` backend, which
    # serves the Desk shell (/desk), the /app->/desk redirect, the shared assets and the
    # REST/desk API. The bare host redirects to /app so visitors land straight in Desk.
    def _tenant(self, method, path, sub, host):
        rec = read_state().get(sub)
        job = JOBS.get(sub)
        if not rec and not job:
            return self._send(404, f"No bench named '{html.escape(sub)}'.", CTYPE[".html"])

        # control-plane endpoints (never proxied to the Desk backend)
        if path == "/_ferro/info":
            if not rec or rec.get("status") == "provisioning":
                return self._json(200, {"sub": sub, "host": host, "status": "provisioning",
                                        "job": job})
            return self._tenant_info(sub, rec)
        if path.startswith("/_ferro/"):
            return self._send(404, "not found")

        # still provisioning -> the self-contained waiting page (polls /_ferro/info, then
        # bounces to /app). Don't proxy: the backend isn't up yet.
        if not rec or rec.get("status") == "provisioning":
            return self._send(200, read_static("instance.html") or b"provisioning...", CTYPE[".html"])

        # land visitors of the bare host straight in Desk
        if method == "GET" and path in ("/", "/index.html"):
            return self._send(302, b"", CTYPE[".html"], extra={"Location": "/app"})

        # Desk fire-and-forget telemetry methods the pure-Rust runtime doesn't implement would
        # otherwise 404 and pop a "Not found" dialog on every navigation. Short-circuit the
        # benign, return-value-less ones with a no-op success so Desk feels seamless. (When the
        # runtime grows these natively this just becomes a redundant fast-path.)
        if path.startswith("/api/method/"):
            m = path[len("/api/method/"):].split("?", 1)[0]
            if m in DESK_NOOP_METHODS:
                return self._json(200, {"message": None})

        # everything else is Desk -> proxy to the tenant backend
        return self._proxy(sub, rec)

    def _tenant_info(self, sub, rec):
        pid = instance_pid(sub)
        rss = pss = 0
        if pid:
            rss, pss = proc_mem_kb(pid)
        # NB: do NOT bump last_seen here — the waiting page's mem-polling must not pin an idle
        # bench awake forever. Only real data-plane traffic (_proxy -> ensure_awake) counts as use.
        return self._json(200, {
            "sub": sub, "host": rec["host"], "status": rec.get("status"),
            "apps": rec.get("apps", []),
            "created": rec.get("created"),
            "rss_kb": rss, "pss_kb": pss,
            "registry": {a: REGISTRY.get(a, {}) for a in rec.get("apps", [])},
            "base_domain": BASE_DOMAIN,
        })

    def _proxy(self, sub, rec):
        port = ensure_awake(sub)
        if not port:
            return self._send(503, "bench is starting, retry in a moment")
        body = self._read_body()
        if body is None:
            return self._send(413, "request too large")
        url = f"http://127.0.0.1:{port}{self.path}"
        req = urllib.request.Request(url, data=body if body else None, method=self.command)
        skip_up = HOP_HEADERS | {"host", "x-forwarded-host", "x-forwarded-proto",
                                 "x-forwarded-for", "accept-encoding"}
        for k, v in self.headers.items():
            if k.lower() in skip_up:
                continue
            req.add_header(k, v)
        skip_down = HOP_HEADERS | {"content-encoding", "server", "date"}

        def relay(status, headers, data):
            self.send_response(status)
            for k, v in headers.items():
                if k.lower() in skip_down or k.lower() == "content-length":
                    continue
                self.send_header(k, v)   # forwards Location (/app->/desk), Set-Cookie, Cache-Control…
            self.send_header("Content-Length", str(len(data)))
            self.end_headers()
            if self.command != "HEAD":
                self.wfile.write(data)

        try:
            # Don't auto-follow redirects: the backend 301s /app -> /desk and the *browser*
            # must see that, exactly as in a real Frappe bench.
            with NoRedirectProxy.open(req, timeout=40) as r:
                relay(r.status, r.headers, r.read())
        except urllib.error.HTTPError as e:
            # 3xx/4xx/5xx from the backend — forward verbatim (with its Location/headers).
            relay(e.code, e.headers, e.read())
        except Exception as e:  # noqa: BLE001
            self._send(502, f"backend error: {e}")

# --------------------------------------------------------------------------- misc helpers
def deprovision(sub):
    systemctl("disable", "--now", instance_unit(sub))
    shutil.rmtree(os.path.join(TENANTS_DIR, sub), ignore_errors=True)
    remove_instance(sub)
    JOBS.pop(sub, None)   # free the name + clear stale /api/status, known_host, tls-check

def reap():
    """Stop benches idle past IDLE_STOP (freeing RAM; they wake on next visit);
    fully deprovision those idle past IDLE_DELETE."""
    now = int(time.time())
    stopped, deleted = [], []
    for sub, rec in list(read_state().items()):
        if rec.get("status") == "provisioning":
            continue
        last = rec.get("last_seen") or rec.get("created") or now
        idle = now - last
        if idle > IDLE_DELETE:
            deprovision(sub)
            deleted.append(sub)
        elif idle > IDLE_STOP and instance_active(sub):
            systemctl("stop", instance_unit(sub))
            upsert_instance(sub, status="sleeping")
            stopped.append(sub)
    return {"stopped": stopped, "deleted": deleted, "ts": now}

def public_instances():
    out = []
    for sub, rec in read_state().items():
        out.append({"sub": sub, "apps": rec.get("apps", []),
                    "created": rec.get("created"),
                    "status": rec.get("status")})
    out.sort(key=lambda r: r.get("created") or 0, reverse=True)
    return out[:50]

# --------------------------------------------------------------------------- main
def main():
    os.makedirs(LOG_DIR, exist_ok=True)
    os.makedirs(TENANTS_DIR, exist_ok=True)
    httpd = ThreadingHTTPServer((LISTEN_HOST, LISTEN_PORT), Handler)
    httpd.daemon_threads = True
    print(f"ferro-control on {LISTEN_HOST}:{LISTEN_PORT}  base={BASE_DOMAIN}  "
          f"apps={list(REGISTRY)}  desk={DESK_ENABLED}", flush=True)
    httpd.serve_forever()

if __name__ == "__main__":
    main()
