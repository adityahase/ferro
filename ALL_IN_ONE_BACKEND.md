# ferro all-in-one backend (one process, smaller Procfile)

A stock Frappe bench runs a fleet of backend processes — a web server, a Node `socketio`,
an `rq` worker, a `schedule` ticker, and 1–3 `redis-server` instances tying them together. Because
**ferro is the framework** (pure Rust, no Python, no external clients that need to speak Redis),
all of that collapses into the one `ferro serve` process. The implementations are internal and
invisible to users — only the Procfile gets shorter.

| Stock bench process            | ferro replacement (in-process)                        |
|--------------------------------|-------------------------------------------------------|
| `redis_cache` (redis-server)   | `src/cache.rs` — TTL key/value + hash map             |
| `redis_queue` (redis-server)   | `src/jobs.rs` — in-memory job queue                   |
| `redis_socketio` (redis-server)| `src/realtime.rs` — in-process pub/sub bus            |
| `socketio` (node)              | `src/realtime.rs` — Engine.IO + WebSocket + Socket.IO |
| `worker` (rq)                  | `src/jobs.rs` — worker thread pool + native handlers  |
| `schedule` (bench schedule)    | `src/jobs.rs` — periodic scheduler                    |

The Procfile goes from **5–8 lines to 1** (the `web:` line). `watch` (frontend asset rebuild) is
left untouched — it's orthogonal to the runtime.

## What runs inside `ferro serve --bench-mode`

- **HTTP/REST/Desk** — the existing ferro web server (unchanged).
- **Realtime** — a socket.io v4 server on `socketio_port` (default 9000). Browsers connect exactly
  as before (`io(origin + "/" + sitename)`); ferro does the Engine.IO long-polling handshake, the
  WebSocket upgrade, namespaces, rooms and event delivery itself. Document writes publish the same
  `doc_update` / `list_update` events Frappe's `Document.notify_update` does — so open forms and
  list views refresh live, with no redis hop.
- **Background workers** — a pool of worker threads (count = `background_workers`) draining an
  in-process queue. Jobs are dispatched to native Rust handlers keyed by Frappe-style dotted method
  names; the `python` build (ferrod) can register a fallthrough that runs the real Python callable.
- **Scheduler** — a tick thread that enqueues periodic jobs (`all` ≈ 60s, `hourly`, `daily`, …),
  mirroring Frappe's `scheduler_events`.

A running job holds `Arc`s to the cache and the realtime hub, so it can publish progress straight
to the browser (`JobContext::publish_progress`) — the in-process equivalent of
`publish_realtime` → redis → socketio.

## Flags

`ferro serve --bench-mode` turns the whole backend on by default. Opt out granularly:

- `--no-realtime` — don't run the socket.io server
- `--no-workers` — don't run background workers / scheduler
- `--no-scheduler` — run workers but no periodic scheduler
- `--no-backend` — pure web runtime (the original ferro behaviour)
- `--socketio-port N` — override the realtime port (else `socketio_port` from common_site_config)
- `--workers N` — worker thread count (else `background_workers`)

Outside `--bench-mode` the subsystems stay **off**, so `ferro serve <db>` is the same lightweight
REST runtime as before (no surprise port binding).

## Introspection / control methods

- `GET  /api/method/ferro.status` — runtime + subsystem status (realtime connections, jobs pending,
  registered job methods, cache size, last scheduler heartbeat)
- `POST /api/method/ferro.enqueue?method=<m>&queue=<q>&kwargs=<json>` — enqueue a job (Administrator)
- `GET  /api/method/ferro.job_status?id=<id>` — a job's status / result / error

`FERRO_RT_DEBUG=1` logs the realtime request/transport trace.

## Flip the bench Procfile

`contrib/bench-ferro-switch.sh on` backs up the whole Procfile, replaces the `web:` line with the
all-in-one ferro command, and drops the `socketio`/`schedule`/`worker`/`redis_*` lines. `off`
restores the original Procfile byte-for-byte and resets `web_runtime` to gunicorn. `prod` prints
the supervisor/systemd equivalent.

## Verified (against `/home/frappe/benches/bench-cpython314`, site `mysite.sqlite`)

- **Realtime**: a real `socket.io-client` connects (polling → **websocket upgrade**) and receives
  the `list_update` a REST write broadcasts — 10/10 connects, 5/5 event deliveries.
  Fixed the Engine.IO upgrade race (don't take over the websocket until the client's `5` confirm,
  else the connect-ack queued for polling is flushed to a still-probing socket and lost).
- **Jobs**: `ferro.enqueue` → worker runs the native handler → `Finished` with the result.
- **Scheduler**: the 60s `all` tick enqueues `enqueue_events`, the worker runs it, and
  `scheduler_last_heartbeat` advances — all in the live process, no redis.
- **Web**: REST + Desk shell + desk methods unaffected.
- **Procfile**: collapses 5 → 2 lines; `off` restores it byte-for-byte.
- Unit tests: cache (TTL/hash/glob), jobs (enqueue/worker/unknown-method), SHA-1 (RFC 6455 vector).

## Notes / limits

- Cache and queue are in-process and **not durable** — restarting `ferro serve` clears them. That
  matches a single-node bench where redis is local and mostly a cache anyway; multi-node would need
  a shared backend (out of scope for the one-process goal).
- The pure-Rust worker executes **native** handlers (a registry of common Frappe maintenance jobs);
  arbitrary pickled Python jobs need the ferrod (embedded-CPython) fallthrough handler.
- The realtime auth resolver mirrors ferro's Desk identity (every connection = `default_user`,
  Administrator in Desk mode). A per-sid session lookup can be slotted into the resolver later.
