//! Small dependency-free helpers: datetime, random names, base64, URL/query parsing.

use std::time::{SystemTime, UNIX_EPOCH};

/// Current UTC time as Frappe stores it: "YYYY-MM-DD HH:MM:SS.ffffff".
/// (Frappe uses naive datetimes in the system timezone; a fresh site defaults to UTC.)
pub fn now_datetime() -> String {
    let dur = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
    let secs = dur.as_secs() as i64;
    let micros = dur.subsec_micros();
    let (y, mo, d, h, mi, s) = civil_from_unix(secs);
    format!("{y:04}-{mo:02}-{d:02} {h:02}:{mi:02}:{s:02}.{micros:06}")
}

/// Current UTC date only: "YYYY-MM-DD".
pub fn now_date() -> String {
    let (y, mo, d, _, _, _) = now_civil();
    format!("{y:04}-{mo:02}-{d:02}")
}

/// Current UTC broken-down time (Y, M, D, h, m, s).
pub fn now_civil() -> (i64, u32, u32, u32, u32, u32) {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    civil_from_unix(secs)
}

/// Day-of-year [1,366] for a civil date.
pub fn day_of_year(y: i64, m: u32, d: u32) -> u32 {
    let cum = [0u32, 31, 59, 90, 120, 151, 181, 212, 243, 273, 304, 334];
    let leap = (y % 4 == 0 && y % 100 != 0) || (y % 400 == 0);
    let mut doy = cum[(m as usize).saturating_sub(1).min(11)] + d;
    if leap && m > 2 {
        doy += 1;
    }
    doy
}

/// Frappe `cint`: parse an int, falling back through float, default 0. Used for Check/Int casts.
pub fn cint(s: &str) -> i64 {
    let t = s.trim();
    if let Ok(i) = t.parse::<i64>() {
        return i;
    }
    if let Ok(f) = t.parse::<f64>() {
        return f.trunc() as i64;
    }
    0
}

/// Frappe `flt`: parse a leading float, default 0.0.
pub fn flt(s: &str) -> f64 {
    s.trim().parse::<f64>().unwrap_or(0.0)
}

/// A UUIDv7-ish string: 48-bit unix-millis timestamp + random, formatted 8-4-4-4-12.
/// Frappe uses uuid7 for `autoname = "UUID"`; we match the textual shape and version/variant bits.
pub fn uuid7() -> String {
    let dur = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
    let ms = dur.as_millis() as u64 & 0xFFFF_FFFF_FFFF;
    let mut r = [0u8; 10];
    if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
        use std::io::Read;
        let _ = f.read_exact(&mut r);
    }
    let mut b = [0u8; 16];
    b[0] = (ms >> 40) as u8;
    b[1] = (ms >> 32) as u8;
    b[2] = (ms >> 24) as u8;
    b[3] = (ms >> 16) as u8;
    b[4] = (ms >> 8) as u8;
    b[5] = ms as u8;
    b[6..16].copy_from_slice(&r);
    b[6] = 0x70 | (b[6] & 0x0F); // version 7
    b[8] = 0x80 | (b[8] & 0x3F); // variant
    let h: String = b.iter().map(|x| format!("{x:02x}")).collect();
    format!("{}-{}-{}-{}-{}", &h[0..8], &h[8..12], &h[12..16], &h[16..20], &h[20..32])
}

/// Days-from-civil inverse (Howard Hinnant's algorithm) — unix seconds -> Y/M/D h:m:s (UTC).
fn civil_from_unix(secs: i64) -> (i64, u32, u32, u32, u32, u32) {
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let h = (rem / 3600) as u32;
    let mi = ((rem % 3600) / 60) as u32;
    let s = (rem % 60) as u32;

    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d, h, mi, s)
}

/// 10 lowercase hex chars (5 random bytes) — matches Frappe's `generate_hash` name width.
///
/// NOTE: `/dev/urandom` is a never-EOF character device, so `std::fs::read` (which reads
/// to EOF) would allocate without bound and OOM the process. We MUST read a fixed count.
pub fn random_name() -> String {
    use std::io::Read;
    let mut buf = [0u8; 5]; // 5 bytes -> exactly 10 hex chars
    let ok = std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut buf))
        .is_ok();
    if !ok {
        // fallback: time-derived (nanos give us enough spread for a fallback)
        let n = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos() as u64;
        buf.copy_from_slice(&n.to_le_bytes()[..5]);
    }
    let mut s = String::with_capacity(10);
    for b in buf {
        // hex of a u8 is always 2 ASCII chars; push directly to avoid format! allocation
        const HEX: &[u8; 16] = b"0123456789abcdef";
        s.push(HEX[(b >> 4) as usize] as char);
        s.push(HEX[(b & 0xf) as usize] as char);
    }
    s
}

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Standard base64 (for the rare BLOB cell).
pub fn b64(data: &[u8]) -> String {
    let mut out = String::with_capacity((data.len() + 2) / 3 * 4);
    for chunk in data.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        out.push(B64[((n >> 18) & 63) as usize] as char);
        out.push(B64[((n >> 12) & 63) as usize] as char);
        out.push(if chunk.len() > 1 { B64[((n >> 6) & 63) as usize] as char } else { '=' });
        out.push(if chunk.len() > 2 { B64[(n & 63) as usize] as char } else { '=' });
    }
    out
}

/// Percent-decode (RFC 3986). `plus_as_space` for query components.
pub fn percent_decode(s: &str, plus_as_space: bool) -> String {
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                let hi = hexval(bytes[i + 1]);
                let lo = hexval(bytes[i + 2]);
                if let (Some(hi), Some(lo)) = (hi, lo) {
                    out.push(hi << 4 | lo);
                    i += 3;
                    continue;
                }
                out.push(bytes[i]);
                i += 1;
            }
            b'+' if plus_as_space => {
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

fn hexval(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// Split a request URL into (decoded path segments, query map).
pub fn parse_url(url: &str) -> (Vec<String>, std::collections::HashMap<String, String>) {
    let (path, query) = match url.split_once('?') {
        Some((p, q)) => (p, q),
        None => (url, ""),
    };
    let segments: Vec<String> = path
        .split('/')
        .filter(|s| !s.is_empty())
        .map(|s| percent_decode(s, false))
        .collect();
    let mut params = std::collections::HashMap::new();
    for pair in query.split('&') {
        if pair.is_empty() {
            continue;
        }
        let (k, v) = match pair.split_once('=') {
            Some((k, v)) => (k, v),
            None => (pair, ""),
        };
        params.insert(percent_decode(k, true), percent_decode(v, true));
    }
    (segments, params)
}
