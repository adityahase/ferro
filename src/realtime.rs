//! Internal realtime server — ferro's replacement for the Node `socketio.js` process **and** the
//! `redis_socketio` pub/sub it relied on.
//!
//! Frappe's Desk connects a socket.io v4 client to `http://host:<socketio_port>/<sitename>`. In a
//! stock bench that client talks to a separate Node process which, in turn, subscribes to a redis
//! channel that Python publishes to. ferro folds all of that into one process: this module speaks
//! Engine.IO v4 (long-polling handshake + WebSocket upgrade) and Socket.IO v4 (namespaces, rooms,
//! events) directly, and exposes an in-process [`Realtime::emit`] that ferro's request handlers and
//! background worker call to push events to browsers — no Node, no redis, no extra hop.
//!
//! Dependency-free: std sockets/threads + serde_json, plus SHA-1/base64 from `crate::crypto` for
//! the WebSocket opening handshake.

use crate::crypto;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet, VecDeque};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};

const WS_GUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";
const PING_INTERVAL_MS: u64 = 25000;
const PING_TIMEOUT_MS: u64 = 20000;
/// How long a polling GET will hold open waiting for data before returning a noop.
const POLL_HOLD: Duration = Duration::from_secs(20);

/// Resolves the authenticated user for a realtime connection from the cookie / auth header.
/// `(site, sid_cookie, authorization_header) -> (user, user_type)`.
pub type AuthResolver = Arc<dyn Fn(&str, Option<&str>, Option<&str>) -> (String, String) + Send + Sync>;

// ----------------------------------------------------------------------------------------------
// Session
// ----------------------------------------------------------------------------------------------

struct Session {
    eio_sid: String,
    /// Socket.IO namespace without the leading '/'. Provisionally set from the Host header at
    /// handshake, then overwritten with the authoritative namespace from the CONNECT packet.
    site: Mutex<String>,
    user: Mutex<String>,
    user_type: Mutex<String>,
    /// Rooms this socket has joined (namespace-local names like "doctype:User").
    rooms: Mutex<HashSet<String>>,
    /// Engine.IO packets queued for the polling transport (drained by GET).
    out: Mutex<VecDeque<String>>,
    out_signal: Condvar,
    /// Set once the connection upgrades to (or opens directly on) WebSocket.
    ws: Mutex<Option<Arc<Mutex<TcpStream>>>>,
    /// Whether the Socket.IO namespace CONNECT has completed.
    ns_connected: AtomicBool,
    alive: AtomicBool,
    last_pong: Mutex<Instant>,
}

impl Session {
    /// Deliver one already-framed Engine.IO packet (e.g. "42/site,[...]") to this socket,
    /// over WebSocket if upgraded, otherwise via the polling queue.
    fn send_engine(&self, pkt: &str) {
        if let Some(w) = self.ws.lock().unwrap().as_ref() {
            let mut s = w.lock().unwrap();
            let _ = write_ws_text(&mut s, pkt.as_bytes());
            return;
        }
        self.out.lock().unwrap().push_back(pkt.to_string());
        self.out_signal.notify_all();
    }

    /// Emit a Socket.IO EVENT to just this socket (namespace-scoped).
    fn emit_self(&self, event: &str, message: &Value) {
        self.send_engine(&encode_event(&self.site(), event, message));
    }

    fn site(&self) -> String {
        self.site.lock().unwrap().clone()
    }
}

fn room_key(site: &str, room: &str) -> String {
    format!("{}\u{0}{}", site, room)
}

/// Build the Engine.IO+Socket.IO frame for an EVENT: `4` (msg) + `2` (event) + `/<ns>,` + JSON.
fn encode_event(site: &str, event: &str, message: &Value) -> String {
    let arr = json!([event, message]);
    format!("42/{},{}", site, arr)
}

// ----------------------------------------------------------------------------------------------
// Realtime hub
// ----------------------------------------------------------------------------------------------

pub struct Realtime {
    sessions: Mutex<HashMap<String, Arc<Session>>>,
    /// room_key(site, room) -> set of eio_sids.
    rooms: Mutex<HashMap<String, HashSet<String>>>,
    next_id: AtomicU64,
    resolver: AuthResolver,
}

impl Realtime {
    pub fn new(resolver: AuthResolver) -> Arc<Realtime> {
        Arc::new(Realtime {
            sessions: Mutex::new(HashMap::new()),
            rooms: Mutex::new(HashMap::new()),
            next_id: AtomicU64::new(1),
            resolver,
        })
    }

    fn new_sid(&self) -> String {
        // Engine.IO sids are opaque; uniqueness is all that matters.
        let n = self.next_id.fetch_add(1, Ordering::SeqCst);
        let t = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        format!("ferro{}_{:x}", n, t)
    }

    fn join(&self, sess: &Session, room: &str) {
        sess.rooms.lock().unwrap().insert(room.to_string());
        self.rooms
            .lock()
            .unwrap()
            .entry(room_key(&sess.site(), room))
            .or_default()
            .insert(sess.eio_sid.clone());
    }

    fn leave(&self, sess: &Session, room: &str) {
        sess.rooms.lock().unwrap().remove(room);
        if let Some(set) = self.rooms.lock().unwrap().get_mut(&room_key(&sess.site(), room)) {
            set.remove(&sess.eio_sid);
        }
    }

    fn drop_session(&self, sid: &str) {
        let sess = self.sessions.lock().unwrap().remove(sid);
        if let Some(sess) = sess {
            sess.alive.store(false, Ordering::SeqCst);
            let site = sess.site();
            let rooms: Vec<String> = sess.rooms.lock().unwrap().iter().cloned().collect();
            let mut all = self.rooms.lock().unwrap();
            for r in rooms {
                if let Some(set) = all.get_mut(&room_key(&site, &r)) {
                    set.remove(sid);
                }
            }
        }
    }

    // ---- public emit API (called by ferro request handlers + background worker) ----

    /// Emit an event to every socket in `room` on `site`'s namespace. This is the in-process
    /// equivalent of Frappe's `publish_realtime(...)` → redis → socketio chain.
    pub fn emit(&self, site: &str, room: &str, event: &str, message: &Value) {
        let key = room_key(site, room);
        let targets: Vec<Arc<Session>> = {
            let rooms = self.rooms.lock().unwrap();
            let sessions = self.sessions.lock().unwrap();
            match rooms.get(&key) {
                Some(sids) => sids.iter().filter_map(|s| sessions.get(s).cloned()).collect(),
                None => Vec::new(),
            }
        };
        let frame = encode_event(site, event, message);
        for s in targets {
            s.send_engine(&frame);
        }
    }

    /// Broadcast to the entire namespace (used for site-wide events like asset rebuilds).
    pub fn emit_all(&self, site: &str, event: &str, message: &Value) {
        let targets: Vec<Arc<Session>> = {
            let sessions = self.sessions.lock().unwrap();
            sessions.values().filter(|s| s.site() == site).cloned().collect()
        };
        let frame = encode_event(site, event, message);
        for s in targets {
            s.send_engine(&frame);
        }
    }

    /// Convenience: emit to a single user's room.
    pub fn emit_to_user(&self, site: &str, user: &str, event: &str, message: &Value) {
        self.emit(site, &format!("user:{}", user), event, message);
    }

    pub fn connected_count(&self) -> usize {
        self.sessions.lock().unwrap().len()
    }
}

// ----------------------------------------------------------------------------------------------
// Engine.IO / Socket.IO packet handling
// ----------------------------------------------------------------------------------------------

/// Handle one inbound Engine.IO packet (already a String). Returns false if the connection
/// should close.
fn handle_engine_packet(hub: &Arc<Realtime>, sess: &Arc<Session>, pkt: &str) -> bool {
    let mut chars = pkt.chars();
    let t = match chars.next() {
        Some(c) => c,
        None => return true,
    };
    let rest = &pkt[t.len_utf8()..];
    match t {
        '2' => {
            // ping (may carry "probe" during upgrade) -> reply pong
            sess.send_engine(&format!("3{}", rest));
            true
        }
        '3' => {
            *sess.last_pong.lock().unwrap() = Instant::now();
            true
        }
        '4' => {
            handle_sio_packet(hub, sess, rest);
            true
        }
        '1' => false, // close
        '5' => true,  // upgrade ack (handled in ws loop)
        _ => true,
    }
}

/// Handle one Socket.IO packet (the part after the Engine.IO '4').
fn handle_sio_packet(hub: &Arc<Realtime>, sess: &Arc<Session>, sio: &str) {
    let mut idx = 0usize;
    let bytes = sio.as_bytes();
    let sio_type = if idx < bytes.len() { bytes[idx] as char } else { return };
    idx += 1;

    // optional namespace: starts with '/' up to ','
    let mut namespace = String::from("/");
    if idx < bytes.len() && bytes[idx] == b'/' {
        let start = idx;
        while idx < bytes.len() && bytes[idx] != b',' {
            idx += 1;
        }
        namespace = sio[start..idx].to_string();
        if idx < bytes.len() {
            idx += 1; // skip comma
        }
    }
    // optional ack id (digits) — skip
    while idx < bytes.len() && bytes[idx].is_ascii_digit() {
        idx += 1;
    }
    let payload = &sio[idx..];

    match sio_type {
        '0' => {
            // CONNECT to namespace
            connect_namespace(hub, sess, &namespace);
        }
        '1' => {
            // DISCONNECT from namespace
            hub.drop_session(&sess.eio_sid);
        }
        '2' => {
            // EVENT: payload is a JSON array ["event", arg1, arg2, ...]
            if let Ok(Value::Array(items)) = serde_json::from_str::<Value>(payload) {
                if let Some(event) = items.get(0).and_then(|v| v.as_str()) {
                    handle_client_event(hub, sess, event, &items[1..]);
                }
            }
        }
        _ => {}
    }
}

fn connect_namespace(hub: &Arc<Realtime>, sess: &Arc<Session>, namespace: &str) {
    sess.ns_connected.store(true, Ordering::SeqCst);
    // The namespace IS the Frappe sitename (the client connects to `host/<sitename>`); adopt it as
    // the authoritative site so rooms line up with what ferro emits to.
    if namespace != "/" && !namespace.is_empty() {
        *sess.site.lock().unwrap() = namespace.trim_start_matches('/').to_string();
    }
    // Socket.IO CONNECT ack: `40<ns>,{"sid":"..."}` (ns_prefix already supplies the comma).
    let ack = format!("40{}{}", ns_prefix(namespace), json!({"sid": sess.eio_sid}));
    sess.send_engine(&ack);

    // Auto-join the rooms Frappe's frappe_handlers join on connect.
    let user = sess.user.lock().unwrap().clone();
    let user_type = sess.user_type.lock().unwrap().clone();
    hub.join(sess, &format!("user:{}", user));
    hub.join(sess, "website");
    if user_type == "System User" {
        hub.join(sess, "all");
    }
}

/// `/` (default ns) encodes with no prefix; a named ns like `/mysite` encodes as `/mysite,`.
fn ns_prefix(namespace: &str) -> String {
    if namespace == "/" || namespace.is_empty() {
        String::new()
    } else {
        format!("{},", namespace)
    }
}

/// Mirror of Frappe's realtime `handlers.js`: room (un)subscription and ping.
fn handle_client_event(hub: &Arc<Realtime>, sess: &Arc<Session>, event: &str, args: &[Value]) {
    let s = |i: usize| args.get(i).and_then(|v| v.as_str()).unwrap_or("").to_string();
    match event {
        "ping" => sess.emit_self("pong", &Value::Null),
        "doctype_subscribe" => hub.join(sess, &format!("doctype:{}", s(0))),
        "doctype_unsubscribe" => hub.leave(sess, &format!("doctype:{}", s(0))),
        "doc_subscribe" => hub.join(sess, &format!("doc:{}/{}", s(0), s(1))),
        "doc_unsubscribe" => hub.leave(sess, &format!("doc:{}/{}", s(0), s(1))),
        "doc_open" => hub.join(sess, &format!("open_doc:{}/{}", s(0), s(1))),
        "doc_close" => hub.leave(sess, &format!("open_doc:{}/{}", s(0), s(1))),
        "task_subscribe" | "progress_subscribe" => hub.join(sess, &format!("task_progress:{}", s(0))),
        "task_unsubscribe" => hub.leave(sess, &format!("task_progress:{}", s(0))),
        _ => { /* unknown client event — ignore, matching Node's permissive handling */ }
    }
}

// ----------------------------------------------------------------------------------------------
// HTTP request parsing (for the polling transport + the WS upgrade request line)
// ----------------------------------------------------------------------------------------------

struct HttpReq {
    method: String,
    target: String,
    headers: HashMap<String, String>,
    body: Vec<u8>,
}

fn read_http_request(stream: &mut TcpStream) -> Option<HttpReq> {
    // read headers up to \r\n\r\n
    let mut buf = Vec::with_capacity(1024);
    let mut one = [0u8; 1];
    loop {
        match stream.read(&mut one) {
            Ok(0) => return None,
            Ok(_) => {
                buf.push(one[0]);
                if buf.len() >= 4 && &buf[buf.len() - 4..] == b"\r\n\r\n" {
                    break;
                }
                if buf.len() > 64 * 1024 {
                    return None; // header too large
                }
            }
            Err(_) => return None,
        }
    }
    let head = String::from_utf8_lossy(&buf);
    let mut lines = head.split("\r\n");
    let req_line = lines.next()?;
    let mut parts = req_line.split_whitespace();
    let method = parts.next()?.to_string();
    let target = parts.next()?.to_string();
    let mut headers = HashMap::new();
    for line in lines {
        if line.is_empty() {
            break;
        }
        if let Some((k, v)) = line.split_once(':') {
            headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_string());
        }
    }
    let mut body = Vec::new();
    if let Some(cl) = headers.get("content-length").and_then(|v| v.parse::<usize>().ok()) {
        body = vec![0u8; cl];
        if cl > 0 && stream.read_exact(&mut body).is_err() {
            return None;
        }
    }
    Some(HttpReq { method, target, headers, body })
}

fn query_params(target: &str) -> HashMap<String, String> {
    let mut m = HashMap::new();
    if let Some(q) = target.split('?').nth(1) {
        for kv in q.split('&') {
            if let Some((k, v)) = kv.split_once('=') {
                m.insert(k.to_string(), url_decode(v));
            } else {
                m.insert(kv.to_string(), String::new());
            }
        }
    }
    m
}

fn url_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                let h = hex_val(bytes[i + 1]);
                let l = hex_val(bytes[i + 2]);
                if let (Some(h), Some(l)) = (h, l) {
                    out.push(h * 16 + l);
                    i += 3;
                    continue;
                }
                out.push(bytes[i]);
                i += 1;
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}
fn hex_val(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

/// The Socket.IO namespace the connecting client wants. The client connects to
/// `http://host:port/<sitename>`; socket.io-client derives the *path* (`/socket.io/`) from the
/// engine config and sends the sitename later as the Socket.IO namespace. So we cannot read the
/// site from the URL path here — it arrives in the CONNECT packet's namespace. We capture it from
/// the `Origin`/`Host` only as a fallback; `connect_namespace` stores the authoritative one.
fn cors_headers(origin: Option<&String>) -> String {
    match origin {
        Some(o) => format!(
            "Access-Control-Allow-Origin: {}\r\nAccess-Control-Allow-Credentials: true\r\n",
            o
        ),
        None => "Access-Control-Allow-Origin: *\r\n".to_string(),
    }
}

fn http_respond(stream: &mut TcpStream, status: &str, ctype: &str, cors: &str, body: &[u8]) {
    let head = format!(
        "HTTP/1.1 {}\r\nContent-Type: {}\r\nContent-Length: {}\r\n{}Connection: close\r\n\r\n",
        status,
        ctype,
        body.len(),
        cors
    );
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(body);
    let _ = stream.flush();
}

// ----------------------------------------------------------------------------------------------
// Connection dispatch
// ----------------------------------------------------------------------------------------------

fn rt_debug() -> bool {
    std::env::var("FERRO_RT_DEBUG").is_ok()
}

fn handle_connection(hub: Arc<Realtime>, mut stream: TcpStream) {
    let _ = stream.set_nodelay(true);
    let req = match read_http_request(&mut stream) {
        Some(r) => r,
        None => return,
    };
    let origin = req.headers.get("origin").cloned();
    let q = query_params(&req.target);
    if rt_debug() {
        eprintln!(
            "[rt] {} transport={} sid={} upgrade={}",
            req.method,
            q.get("transport").map(|s| s.as_str()).unwrap_or("-"),
            q.get("sid").map(|s| &s[..s.len().min(10)]).unwrap_or("-"),
            req.headers.get("upgrade").map(|s| s.as_str()).unwrap_or("-"),
        );
    }

    // CORS preflight
    if req.method == "OPTIONS" {
        let cors = format!(
            "{}Access-Control-Allow-Methods: GET,POST,OPTIONS\r\nAccess-Control-Allow-Headers: {}\r\n",
            cors_headers(origin.as_ref()),
            req.headers.get("access-control-request-headers").cloned().unwrap_or_else(|| "*".into())
        );
        http_respond(&mut stream, "204 No Content", "text/plain", &cors, b"");
        return;
    }

    // WebSocket upgrade?
    let is_ws = req
        .headers
        .get("upgrade")
        .map(|u| u.eq_ignore_ascii_case("websocket"))
        .unwrap_or(false);
    if is_ws {
        handle_ws(hub, stream, &req, &q);
        return;
    }

    // Engine.IO polling transport
    let cors = cors_headers(origin.as_ref());
    let transport = q.get("transport").cloned().unwrap_or_default();
    if transport != "polling" {
        http_respond(&mut stream, "400 Bad Request", "text/plain", &cors, b"unsupported transport");
        return;
    }

    match (req.method.as_str(), q.get("sid")) {
        ("GET", None) => {
            // Engine.IO open handshake — create a session.
            let site = derive_site(&req, &q);
            let (user, user_type) =
                (hub.resolver)(&site, req.headers.get("cookie").map(|s| s.as_str()),
                    req.headers.get("authorization").map(|s| s.as_str()));
            let sid = hub.new_sid();
            let sess = Arc::new(Session {
                eio_sid: sid.clone(),
                site: Mutex::new(site),
                user: Mutex::new(user),
                user_type: Mutex::new(user_type),
                rooms: Mutex::new(HashSet::new()),
                out: Mutex::new(VecDeque::new()),
                out_signal: Condvar::new(),
                ws: Mutex::new(None),
                ns_connected: AtomicBool::new(false),
                alive: AtomicBool::new(true),
                last_pong: Mutex::new(Instant::now()),
            });
            hub.sessions.lock().unwrap().insert(sid.clone(), sess);
            let open = json!({
                "sid": sid,
                "upgrades": ["websocket"],
                "pingInterval": PING_INTERVAL_MS,
                "pingTimeout": PING_TIMEOUT_MS,
                "maxPayload": 1000000,
            });
            let body = format!("0{}", open);
            http_respond(&mut stream, "200 OK", "text/plain; charset=UTF-8", &cors, body.as_bytes());
        }
        ("GET", Some(sid)) => {
            // Poll: drain (long-poll) the outbound queue.
            let sess = hub.sessions.lock().unwrap().get(sid).cloned();
            let sess = match sess {
                Some(s) => s,
                None => {
                    // Unknown sid: tell the client the session is gone so it reopens.
                    http_respond(&mut stream, "400 Bad Request", "text/plain", &cors,
                        b"{\"code\":1,\"message\":\"Session ID unknown\"}");
                    return;
                }
            };
            let packets = drain_polling(&sess, POLL_HOLD);
            // Engine.IO payload: packets joined by the record separator \x1e. Empty -> a noop ping.
            let body = if packets.is_empty() {
                "2".to_string() // server ping keeps the client polling loop alive
            } else {
                packets.join("\u{1e}")
            };
            if rt_debug() {
                eprintln!("[rt] GET poll -> {:?}", &body[..body.len().min(60)]);
            }
            http_respond(&mut stream, "200 OK", "text/plain; charset=UTF-8", &cors, body.as_bytes());
        }
        ("POST", Some(sid)) => {
            let sess = hub.sessions.lock().unwrap().get(sid).cloned();
            if let Some(sess) = sess {
                let body = String::from_utf8_lossy(&req.body);
                if rt_debug() {
                    eprintln!("[rt] POST body -> {:?}", &body[..body.len().min(60)]);
                }
                for pkt in body.split('\u{1e}') {
                    if !pkt.is_empty() {
                        handle_engine_packet(&hub, &sess, pkt);
                    }
                }
            }
            http_respond(&mut stream, "200 OK", "text/plain; charset=UTF-8", &cors, b"ok");
        }
        _ => {
            http_respond(&mut stream, "400 Bad Request", "text/plain", &cors, b"bad request");
        }
    }
}

/// Drain queued packets; if none, block up to `hold` for the first one (Engine.IO long-poll).
fn drain_polling(sess: &Session, hold: Duration) -> Vec<String> {
    let mut q = sess.out.lock().unwrap();
    if q.is_empty() {
        // wait for data, but wake periodically so we never hang shutdown.
        let deadline = Instant::now() + hold;
        while q.is_empty() {
            let now = Instant::now();
            if now >= deadline || !sess.alive.load(Ordering::SeqCst) {
                break;
            }
            let wait = (deadline - now).min(Duration::from_secs(1));
            let (nq, _) = sess.out_signal.wait_timeout(q, wait).unwrap();
            q = nq;
            // If we upgraded to websocket while waiting, stop holding the poll.
            if sess.ws.lock().unwrap().is_some() {
                break;
            }
        }
    }
    q.drain(..).collect()
}

/// Best-effort site/namespace derivation for the open handshake. The authoritative namespace
/// comes later in the Socket.IO CONNECT packet; this is only a placeholder used before then.
fn derive_site(req: &HttpReq, _q: &HashMap<String, String>) -> String {
    // Prefer the explicit x-frappe-site-name header if present, else the Host without port.
    if let Some(s) = req.headers.get("x-frappe-site-name") {
        return s.split(':').next().unwrap_or(s).to_string();
    }
    if let Some(h) = req.headers.get("host") {
        return h.split(':').next().unwrap_or(h).to_string();
    }
    String::new()
}

// ----------------------------------------------------------------------------------------------
// WebSocket
// ----------------------------------------------------------------------------------------------

fn handle_ws(hub: Arc<Realtime>, mut stream: TcpStream, req: &HttpReq, q: &HashMap<String, String>) {
    let key = match req.headers.get("sec-websocket-key") {
        Some(k) => k.clone(),
        None => return,
    };
    let accept = crypto::b64_encode_std(&crypto::sha1(format!("{}{}", key, WS_GUID).as_bytes()));
    let resp = format!(
        "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {}\r\n\r\n",
        accept
    );
    if stream.write_all(resp.as_bytes()).is_err() {
        return;
    }
    let _ = stream.flush();

    let writer = Arc::new(Mutex::new(match stream.try_clone() {
        Ok(s) => s,
        Err(_) => return,
    }));
    // `active` = this websocket is the live transport. For an UPGRADE (sid present) we must NOT
    // take over delivery until the client confirms with a `5` (upgrade) packet — until then the
    // client is still on polling and anything written to this (probing) socket is dropped. For a
    // DIRECT websocket open (no prior session) the socket is the transport from the first byte.
    let (sess, mut active) = match q.get("sid").and_then(|sid| hub.sessions.lock().unwrap().get(sid).cloned()) {
        Some(s) => (s, false),
        None => {
            // direct websocket open: create the session and send the Engine.IO open frame.
            let site = derive_site(req, q);
            let (user, user_type) = (hub.resolver)(
                &site,
                req.headers.get("cookie").map(|s| s.as_str()),
                req.headers.get("authorization").map(|s| s.as_str()),
            );
            let sid = hub.new_sid();
            let sess = Arc::new(Session {
                eio_sid: sid.clone(),
                site: Mutex::new(site),
                user: Mutex::new(user),
                user_type: Mutex::new(user_type),
                rooms: Mutex::new(HashSet::new()),
                out: Mutex::new(VecDeque::new()),
                out_signal: Condvar::new(),
                ws: Mutex::new(None),
                ns_connected: AtomicBool::new(false),
                alive: AtomicBool::new(true),
                last_pong: Mutex::new(Instant::now()),
            });
            hub.sessions.lock().unwrap().insert(sid.clone(), sess.clone());
            let open = json!({
                "sid": sid,
                "upgrades": [],
                "pingInterval": PING_INTERVAL_MS,
                "pingTimeout": PING_TIMEOUT_MS,
                "maxPayload": 1000000,
            });
            let _ = write_ws_text(&mut writer.lock().unwrap(), format!("0{}", open).as_bytes());
            (sess, true)
        }
    };
    if active {
        attach_ws(&sess, &writer);
    }

    // Read loop
    let mut rd = stream;
    let _ = rd.set_read_timeout(Some(Duration::from_secs(
        (PING_INTERVAL_MS + PING_TIMEOUT_MS) / 1000 + 5,
    )));
    loop {
        match read_ws_frame(&mut rd) {
            Ok(Some((opcode, payload))) => match opcode {
                0x1 | 0x0 => {
                    let pkt = String::from_utf8_lossy(&payload).into_owned();
                    if pkt == "2probe" {
                        // Engine.IO upgrade probe: reply over THIS socket, but stay on polling.
                        let _ = write_ws_text(&mut writer.lock().unwrap(), b"3probe");
                    } else if pkt == "5" {
                        // upgrade confirmed: now take over delivery from polling.
                        if !active {
                            attach_ws(&sess, &writer);
                            active = true;
                        }
                    } else if !handle_engine_packet(&hub, &sess, &pkt) {
                        break;
                    }
                }
                0x8 => break, // close
                0x9 => {
                    let _ = write_ws_frame(&mut writer.lock().unwrap(), 0xA, &payload);
                }
                0xA => {
                    *sess.last_pong.lock().unwrap() = Instant::now();
                }
                _ => {}
            },
            Ok(None) => break,
            Err(_) => break,
        }
    }
    let _ = write_ws_frame(&mut writer.lock().unwrap(), 0x8, &[]);
    hub.drop_session(&sess.eio_sid);
}

/// Promote this websocket to the session's live transport: set it as the writer, flush any
/// packets that queued on the polling transport, and start the Engine.IO ping heartbeat.
fn attach_ws(sess: &Arc<Session>, writer: &Arc<Mutex<TcpStream>>) {
    // Set ws first so concurrent emits go straight to the socket; then drain anything that was
    // queued before the switch (no packet ends up both queued and sent).
    *sess.ws.lock().unwrap() = Some(writer.clone());
    let queued: Vec<String> = sess.out.lock().unwrap().drain(..).collect();
    for p in queued {
        let _ = write_ws_text(&mut writer.lock().unwrap(), p.as_bytes());
    }
    sess.out_signal.notify_all();

    let sess_hb = sess.clone();
    let writer_hb = writer.clone();
    thread::spawn(move || loop {
        thread::sleep(Duration::from_millis(PING_INTERVAL_MS));
        if !sess_hb.alive.load(Ordering::SeqCst) {
            break;
        }
        if write_ws_text(&mut writer_hb.lock().unwrap(), b"2").is_err() {
            break;
        }
    });
}

/// Write a server->client text frame (unmasked, FIN set).
fn write_ws_text(stream: &mut TcpStream, payload: &[u8]) -> std::io::Result<()> {
    write_ws_frame(stream, 0x1, payload)
}

fn write_ws_frame(stream: &mut TcpStream, opcode: u8, payload: &[u8]) -> std::io::Result<()> {
    let mut frame = Vec::with_capacity(payload.len() + 10);
    frame.push(0x80 | (opcode & 0x0F)); // FIN + opcode
    let len = payload.len();
    if len < 126 {
        frame.push(len as u8);
    } else if len < 65536 {
        frame.push(126);
        frame.extend_from_slice(&(len as u16).to_be_bytes());
    } else {
        frame.push(127);
        frame.extend_from_slice(&(len as u64).to_be_bytes());
    }
    frame.extend_from_slice(payload);
    stream.write_all(&frame)?;
    stream.flush()
}

/// Read one client->server frame, unmasking the payload. Returns Ok(None) on clean EOF.
/// Handles fragmentation by concatenating continuation frames.
fn read_ws_frame(stream: &mut TcpStream) -> std::io::Result<Option<(u8, Vec<u8>)>> {
    let mut acc: Vec<u8> = Vec::new();
    let mut first_opcode = 0u8;
    loop {
        let mut h = [0u8; 2];
        match stream.read_exact(&mut h) {
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
            Err(e) => return Err(e),
        }
        let fin = h[0] & 0x80 != 0;
        let opcode = h[0] & 0x0F;
        let masked = h[1] & 0x80 != 0;
        let mut len = (h[1] & 0x7F) as usize;
        if len == 126 {
            let mut e = [0u8; 2];
            stream.read_exact(&mut e)?;
            len = u16::from_be_bytes(e) as usize;
        } else if len == 127 {
            let mut e = [0u8; 8];
            stream.read_exact(&mut e)?;
            len = u64::from_be_bytes(e) as usize;
        }
        let mask = if masked {
            let mut m = [0u8; 4];
            stream.read_exact(&mut m)?;
            Some(m)
        } else {
            None
        };
        let mut payload = vec![0u8; len];
        if len > 0 {
            stream.read_exact(&mut payload)?;
        }
        if let Some(m) = mask {
            for (i, b) in payload.iter_mut().enumerate() {
                *b ^= m[i % 4];
            }
        }
        // control frames (>=0x8) are never fragmented
        if opcode >= 0x8 {
            return Ok(Some((opcode, payload)));
        }
        if first_opcode == 0 {
            first_opcode = opcode;
        }
        acc.extend_from_slice(&payload);
        if fin {
            return Ok(Some((first_opcode, acc)));
        }
    }
}

// ----------------------------------------------------------------------------------------------
// Listener
// ----------------------------------------------------------------------------------------------

/// Bind the realtime server on `addr` (e.g. "0.0.0.0:9000") and serve forever (thread per conn).
pub fn serve(hub: Arc<Realtime>, addr: &str) -> std::io::Result<thread::JoinHandle<()>> {
    let listener = TcpListener::bind(addr)?;
    let addr_owned = addr.to_string();
    let handle = thread::Builder::new().name("ferro-realtime".into()).spawn(move || {
        eprintln!("ferro realtime: socket.io listening on {}", addr_owned);
        for stream in listener.incoming() {
            match stream {
                Ok(s) => {
                    let hub = hub.clone();
                    let _ = thread::Builder::new()
                        .stack_size(256 * 1024)
                        .spawn(move || handle_connection(hub, s));
                }
                Err(_) => break,
            }
        }
    })?;
    Ok(handle)
}
