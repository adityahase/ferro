//! Dependency-free crypto needed to verify Frappe API tokens.
//!
//! Frappe stores `User.api_secret` as a Password field, encrypted at rest with **Fernet**
//! (symmetric, AES-128-CBC + HMAC-SHA256) keyed by the *single* site-wide `encryption_key`
//! from `site_config.json` (see frappe/utils/password.py). Auth decrypts it and compares.
//! To be a drop-in for a real site we must do the same — so this module implements exactly
//! what Fernet decryption needs and nothing more, with no external crates.
//!
//! Fernet token layout (url-safe base64 of):
//!   0x80 (version) | 8-byte BE timestamp | 16-byte IV | AES-128-CBC ciphertext | 32-byte HMAC-SHA256
//! The 32-byte key splits as: [0..16] = HMAC signing key, [16..32] = AES key.
//! HMAC is computed over (version|timestamp|IV|ciphertext) and must be verified first.

// ----------------------------- base64 (url-safe + standard) -----------------------------

fn b64_decode(input: &str, url_safe: bool) -> Option<Vec<u8>> {
    let mut table = [255u8; 256];
    let alphabet: &[u8; 64] = if url_safe {
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_"
    } else {
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/"
    };
    for (i, &c) in alphabet.iter().enumerate() {
        table[c as usize] = i as u8;
    }
    let mut out = Vec::with_capacity(input.len() / 4 * 3 + 3);
    let mut buf = 0u32;
    let mut bits = 0u32;
    for &c in input.as_bytes() {
        if c == b'=' || c == b'\n' || c == b'\r' {
            continue;
        }
        let v = table[c as usize];
        if v == 255 {
            return None; // invalid char
        }
        buf = (buf << 6) | v as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buf >> bits) as u8);
        }
    }
    Some(out)
}

/// Decode a url-safe base64 string (Fernet key/token), padding optional.
pub fn b64url_decode(s: &str) -> Option<Vec<u8>> {
    b64_decode(s.trim(), true)
}

/// Decode a standard base64 string (e.g. HTTP Basic auth credentials), padding optional.
pub fn b64url_decode_std(s: &str) -> Option<Vec<u8>> {
    b64_decode(s.trim(), false)
}

/// Encode bytes as url-safe base64 *with* padding (matches Python urlsafe_b64encode), for tests/provisioning.
#[allow(dead_code)]
pub fn b64url_encode(data: &[u8]) -> String {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::with_capacity((data.len() + 2) / 3 * 4);
    for chunk in data.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        out.push(A[((n >> 18) & 63) as usize] as char);
        out.push(A[((n >> 12) & 63) as usize] as char);
        out.push(if chunk.len() > 1 { A[((n >> 6) & 63) as usize] as char } else { '=' });
        out.push(if chunk.len() > 2 { A[(n & 63) as usize] as char } else { '=' });
    }
    out
}

/// Encode bytes as standard base64 *with* padding (alphabet `+/`). Used for the WebSocket
/// `Sec-WebSocket-Accept` handshake response.
pub fn b64_encode_std(data: &[u8]) -> String {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity((data.len() + 2) / 3 * 4);
    for chunk in data.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        out.push(A[((n >> 18) & 63) as usize] as char);
        out.push(A[((n >> 12) & 63) as usize] as char);
        out.push(if chunk.len() > 1 { A[((n >> 6) & 63) as usize] as char } else { '=' });
        out.push(if chunk.len() > 2 { A[(n & 63) as usize] as char } else { '=' });
    }
    out
}

// ----------------------------------- SHA-1 ------------------------------------
// SHA-1 is cryptographically broken for collision resistance, but the WebSocket (RFC 6455)
// opening handshake mandates exactly `base64(sha1(key + magic))` — it's a framing token, not a
// security primitive. This is the only place ferro uses SHA-1.

pub fn sha1(msg: &[u8]) -> [u8; 20] {
    let mut h: [u32; 5] = [0x67452301, 0xEFCDAB89, 0x98BADCFE, 0x10325476, 0xC3D2E1F0];
    let bitlen = (msg.len() as u64) * 8;
    let mut data = msg.to_vec();
    data.push(0x80);
    while data.len() % 64 != 56 {
        data.push(0);
    }
    data.extend_from_slice(&bitlen.to_be_bytes());

    let mut w = [0u32; 80];
    for block in data.chunks_exact(64) {
        for i in 0..16 {
            w[i] = u32::from_be_bytes([block[i * 4], block[i * 4 + 1], block[i * 4 + 2], block[i * 4 + 3]]);
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }
        let (mut a, mut b, mut c, mut d, mut e) = (h[0], h[1], h[2], h[3], h[4]);
        for (i, &wi) in w.iter().enumerate() {
            let (f, k) = match i {
                0..=19 => ((b & c) | ((!b) & d), 0x5A827999u32),
                20..=39 => (b ^ c ^ d, 0x6ED9EBA1),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1BBCDC),
                _ => (b ^ c ^ d, 0xCA62C1D6),
            };
            let tmp = a.rotate_left(5).wrapping_add(f).wrapping_add(e).wrapping_add(k).wrapping_add(wi);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = tmp;
        }
        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
    }
    let mut out = [0u8; 20];
    for (i, word) in h.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&word.to_be_bytes());
    }
    out
}

// ----------------------------------- SHA-256 ------------------------------------

const SHA256_K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

pub fn sha256(msg: &[u8]) -> [u8; 32] {
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19,
    ];
    // pad
    let bitlen = (msg.len() as u64) * 8;
    let mut data = msg.to_vec();
    data.push(0x80);
    while data.len() % 64 != 56 {
        data.push(0);
    }
    data.extend_from_slice(&bitlen.to_be_bytes());

    let mut w = [0u32; 64];
    for block in data.chunks_exact(64) {
        for i in 0..16 {
            w[i] = u32::from_be_bytes([block[i * 4], block[i * 4 + 1], block[i * 4 + 2], block[i * 4 + 3]]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16].wrapping_add(s0).wrapping_add(w[i - 7]).wrapping_add(s1);
        }
        let (mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh) =
            (h[0], h[1], h[2], h[3], h[4], h[5], h[6], h[7]);
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let t1 = hh.wrapping_add(s1).wrapping_add(ch).wrapping_add(SHA256_K[i]).wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);
            hh = g; g = f; f = e;
            e = d.wrapping_add(t1);
            d = c; c = b; b = a;
            a = t1.wrapping_add(t2);
        }
        h[0] = h[0].wrapping_add(a); h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c); h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e); h[5] = h[5].wrapping_add(f);
        h[6] = h[6].wrapping_add(g); h[7] = h[7].wrapping_add(hh);
    }
    let mut out = [0u8; 32];
    for i in 0..8 {
        out[i * 4..i * 4 + 4].copy_from_slice(&h[i].to_be_bytes());
    }
    out
}

pub fn hmac_sha256(key: &[u8], msg: &[u8]) -> [u8; 32] {
    let mut k = [0u8; 64];
    if key.len() > 64 {
        k[..32].copy_from_slice(&sha256(key));
    } else {
        k[..key.len()].copy_from_slice(key);
    }
    let mut ipad = [0x36u8; 64];
    let mut opad = [0x5cu8; 64];
    for i in 0..64 {
        ipad[i] ^= k[i];
        opad[i] ^= k[i];
    }
    let mut inner = Vec::with_capacity(64 + msg.len());
    inner.extend_from_slice(&ipad);
    inner.extend_from_slice(msg);
    let ih = sha256(&inner);
    let mut outer = Vec::with_capacity(96);
    outer.extend_from_slice(&opad);
    outer.extend_from_slice(&ih);
    sha256(&outer)
}

/// Constant-time byte comparison.
pub fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

// ------------------------------ PBKDF2 / passlib --------------------------------
// Frappe stores `User.password` (in `__Auth`) as a passlib pbkdf2_sha256 modular-crypt hash:
//   $pbkdf2-sha256$<rounds>$<ab64(salt)>$<ab64(checksum)>
// where checksum = PBKDF2-HMAC-SHA256(password, salt, rounds, dklen=32), default rounds=29000,
// salt = 16 random bytes, and ab64 is passlib's "adapted base64" (standard base64 with '+'->'.'
// and '=' padding stripped; '/' is kept). We must produce byte-identical hashes so Frappe's
// passlib (and ferro's own login path) can verify a password ferro set.

pub const PASSLIB_DEFAULT_ROUNDS: u32 = 29000;

/// PBKDF2-HMAC-SHA256, built on the existing `hmac_sha256` primitive (RFC 2898).
pub fn pbkdf2_hmac_sha256(password: &[u8], salt: &[u8], rounds: u32, dklen: usize) -> Vec<u8> {
    const HLEN: usize = 32;
    let blocks = dklen.div_ceil(HLEN);
    let mut out = Vec::with_capacity(blocks * HLEN);
    for i in 1..=blocks as u32 {
        // U1 = PRF(password, salt || INT_32_BE(i))
        let mut msg = Vec::with_capacity(salt.len() + 4);
        msg.extend_from_slice(salt);
        msg.extend_from_slice(&i.to_be_bytes());
        let mut u = hmac_sha256(password, &msg);
        let mut t = u;
        for _ in 1..rounds {
            u = hmac_sha256(password, &u);
            for k in 0..HLEN {
                t[k] ^= u[k];
            }
        }
        out.extend_from_slice(&t);
    }
    out.truncate(dklen);
    out
}

/// passlib "adapted base64": standard base64, '+' -> '.', '=' padding stripped ('/' kept).
pub fn ab64_encode(data: &[u8]) -> String {
    let mut s = b64_encode_std(data);
    while s.ends_with('=') {
        s.pop();
    }
    s.replace('+', ".")
}

fn ab64_decode(s: &str) -> Option<Vec<u8>> {
    // reverse: '.' -> '+', then pad to a multiple of 4 with '='.
    let mut t = s.replace('.', "+");
    while t.len() % 4 != 0 {
        t.push('=');
    }
    b64_decode(&t, false)
}

/// Produce a passlib `$pbkdf2-sha256$...` hash with the given rounds and salt.
pub fn passlib_pbkdf2_sha256_with(password: &str, salt: &[u8], rounds: u32) -> String {
    let cksum = pbkdf2_hmac_sha256(password.as_bytes(), salt, rounds, 32);
    format!(
        "$pbkdf2-sha256${}${}${}",
        rounds,
        ab64_encode(salt),
        ab64_encode(&cksum)
    )
}

/// Hash a password the way Frappe's `update_password` does: pbkdf2_sha256, 29000 rounds,
/// fresh 16-byte random salt. The result is verifiable by Frappe's passlib.
pub fn passlib_pbkdf2_sha256_hash(password: &str) -> String {
    let salt = crate::util::random_bytes(16);
    passlib_pbkdf2_sha256_with(password, &salt, PASSLIB_DEFAULT_ROUNDS)
}

/// Verify a password against a passlib `$pbkdf2-sha256$rounds$salt$checksum` hash.
pub fn passlib_pbkdf2_sha256_verify(password: &str, hash: &str) -> bool {
    let parts: Vec<&str> = hash.split('$').collect();
    // ["", "pbkdf2-sha256", rounds, salt, checksum]
    if parts.len() != 5 || parts[1] != "pbkdf2-sha256" {
        return false;
    }
    let rounds: u32 = match parts[2].parse() {
        Ok(r) => r,
        Err(_) => return false,
    };
    let (salt, expect) = match (ab64_decode(parts[3]), ab64_decode(parts[4])) {
        (Some(s), Some(e)) => (s, e),
        _ => return false,
    };
    let got = pbkdf2_hmac_sha256(password.as_bytes(), &salt, rounds, expect.len());
    ct_eq(&got, &expect)
}

// ----------------------------------- AES-128 ------------------------------------

#[rustfmt::skip]
const INV_SBOX: [u8; 256] = [
    0x52,0x09,0x6a,0xd5,0x30,0x36,0xa5,0x38,0xbf,0x40,0xa3,0x9e,0x81,0xf3,0xd7,0xfb,
    0x7c,0xe3,0x39,0x82,0x9b,0x2f,0xff,0x87,0x34,0x8e,0x43,0x44,0xc4,0xde,0xe9,0xcb,
    0x54,0x7b,0x94,0x32,0xa6,0xc2,0x23,0x3d,0xee,0x4c,0x95,0x0b,0x42,0xfa,0xc3,0x4e,
    0x08,0x2e,0xa1,0x66,0x28,0xd9,0x24,0xb2,0x76,0x5b,0xa2,0x49,0x6d,0x8b,0xd1,0x25,
    0x72,0xf8,0xf6,0x64,0x86,0x68,0x98,0x16,0xd4,0xa4,0x5c,0xcc,0x5d,0x65,0xb6,0x92,
    0x6c,0x70,0x48,0x50,0xfd,0xed,0xb9,0xda,0x5e,0x15,0x46,0x57,0xa7,0x8d,0x9d,0x84,
    0x90,0xd8,0xab,0x00,0x8c,0xbc,0xd3,0x0a,0xf7,0xe4,0x58,0x05,0xb8,0xb3,0x45,0x06,
    0xd0,0x2c,0x1e,0x8f,0xca,0x3f,0x0f,0x02,0xc1,0xaf,0xbd,0x03,0x01,0x13,0x8a,0x6b,
    0x3a,0x91,0x11,0x41,0x4f,0x67,0xdc,0xea,0x97,0xf2,0xcf,0xce,0xf0,0xb4,0xe6,0x73,
    0x96,0xac,0x74,0x22,0xe7,0xad,0x35,0x85,0xe2,0xf9,0x37,0xe8,0x1c,0x75,0xdf,0x6e,
    0x47,0xf1,0x1a,0x71,0x1d,0x29,0xc5,0x89,0x6f,0xb7,0x62,0x0e,0xaa,0x18,0xbe,0x1b,
    0xfc,0x56,0x3e,0x4b,0xc6,0xd2,0x79,0x20,0x9a,0xdb,0xc0,0xfe,0x78,0xcd,0x5a,0xf4,
    0x1f,0xdd,0xa8,0x33,0x88,0x07,0xc7,0x31,0xb1,0x12,0x10,0x59,0x27,0x80,0xec,0x5f,
    0x60,0x51,0x7f,0xa9,0x19,0xb5,0x4a,0x0d,0x2d,0xe5,0x7a,0x9f,0x93,0xc9,0x9c,0xef,
    0xa0,0xe0,0x3b,0x4d,0xae,0x2a,0xf5,0xb0,0xc8,0xeb,0xbb,0x3c,0x83,0x53,0x99,0x61,
    0x17,0x2b,0x04,0x7e,0xba,0x77,0xd6,0x26,0xe1,0x69,0x14,0x63,0x55,0x21,0x0c,0x7d,
];

#[rustfmt::skip]
const SBOX: [u8; 256] = [
    0x63,0x7c,0x77,0x7b,0xf2,0x6b,0x6f,0xc5,0x30,0x01,0x67,0x2b,0xfe,0xd7,0xab,0x76,
    0xca,0x82,0xc9,0x7d,0xfa,0x59,0x47,0xf0,0xad,0xd4,0xa2,0xaf,0x9c,0xa4,0x72,0xc0,
    0xb7,0xfd,0x93,0x26,0x36,0x3f,0xf7,0xcc,0x34,0xa5,0xe5,0xf1,0x71,0xd8,0x31,0x15,
    0x04,0xc7,0x23,0xc3,0x18,0x96,0x05,0x9a,0x07,0x12,0x80,0xe2,0xeb,0x27,0xb2,0x75,
    0x09,0x83,0x2c,0x1a,0x1b,0x6e,0x5a,0xa0,0x52,0x3b,0xd6,0xb3,0x29,0xe3,0x2f,0x84,
    0x53,0xd1,0x00,0xed,0x20,0xfc,0xb1,0x5b,0x6a,0xcb,0xbe,0x39,0x4a,0x4c,0x58,0xcf,
    0xd0,0xef,0xaa,0xfb,0x43,0x4d,0x33,0x85,0x45,0xf9,0x02,0x7f,0x50,0x3c,0x9f,0xa8,
    0x51,0xa3,0x40,0x8f,0x92,0x9d,0x38,0xf5,0xbc,0xb6,0xda,0x21,0x10,0xff,0xf3,0xd2,
    0xcd,0x0c,0x13,0xec,0x5f,0x97,0x44,0x17,0xc4,0xa7,0x7e,0x3d,0x64,0x5d,0x19,0x73,
    0x60,0x81,0x4f,0xdc,0x22,0x2a,0x90,0x88,0x46,0xee,0xb8,0x14,0xde,0x5e,0x0b,0xdb,
    0xe0,0x32,0x3a,0x0a,0x49,0x06,0x24,0x5c,0xc2,0xd3,0xac,0x62,0x91,0x95,0xe4,0x79,
    0xe7,0xc8,0x37,0x6d,0x8d,0xd5,0x4e,0xa9,0x6c,0x56,0xf4,0xea,0x65,0x7a,0xae,0x08,
    0xba,0x78,0x25,0x2e,0x1c,0xa6,0xb4,0xc6,0xe8,0xdd,0x74,0x1f,0x4b,0xbd,0x8b,0x8a,
    0x70,0x3e,0xb5,0x66,0x48,0x03,0xf6,0x0e,0x61,0x35,0x57,0xb9,0x86,0xc1,0x1d,0x9e,
    0xe1,0xf8,0x98,0x11,0x69,0xd9,0x8e,0x94,0x9b,0x1e,0x87,0xe9,0xce,0x55,0x28,0xdf,
    0x8c,0xa1,0x89,0x0d,0xbf,0xe6,0x42,0x68,0x41,0x99,0x2d,0x0f,0xb0,0x54,0xbb,0x16,
];

const RCON: [u8; 10] = [0x01, 0x02, 0x04, 0x08, 0x10, 0x20, 0x40, 0x80, 0x1b, 0x36];

/// Expand a 16-byte AES-128 key into 11 round keys (44 words).
fn key_expansion(key: &[u8; 16]) -> [[u8; 16]; 11] {
    let mut w = [[0u8; 4]; 44];
    for i in 0..4 {
        w[i] = [key[4 * i], key[4 * i + 1], key[4 * i + 2], key[4 * i + 3]];
    }
    for i in 4..44 {
        let mut t = w[i - 1];
        if i % 4 == 0 {
            // RotWord
            t = [t[1], t[2], t[3], t[0]];
            // SubWord
            for b in t.iter_mut() {
                *b = SBOX[*b as usize];
            }
            t[0] ^= RCON[i / 4 - 1];
        }
        for j in 0..4 {
            w[i][j] = w[i - 4][j] ^ t[j];
        }
    }
    let mut rks = [[0u8; 16]; 11];
    for r in 0..11 {
        for c in 0..4 {
            for j in 0..4 {
                rks[r][4 * c + j] = w[4 * r + c][j];
            }
        }
    }
    rks
}

/// GF(2^8) multiply.
fn gmul(mut a: u8, mut b: u8) -> u8 {
    let mut p = 0u8;
    for _ in 0..8 {
        if b & 1 != 0 {
            p ^= a;
        }
        let hi = a & 0x80;
        a <<= 1;
        if hi != 0 {
            a ^= 0x1b;
        }
        b >>= 1;
    }
    p
}

fn add_round_key(state: &mut [u8; 16], rk: &[u8; 16]) {
    for i in 0..16 {
        state[i] ^= rk[i];
    }
}

fn inv_sub_bytes(state: &mut [u8; 16]) {
    for b in state.iter_mut() {
        *b = INV_SBOX[*b as usize];
    }
}

fn inv_shift_rows(state: &mut [u8; 16]) {
    // state is column-major: index = row + 4*col. InvShiftRows rotates row r right by r.
    let s = *state;
    for r in 1..4 {
        for c in 0..4 {
            state[r + 4 * c] = s[r + 4 * ((c + 4 - r) % 4)];
        }
    }
}

fn inv_mix_columns(state: &mut [u8; 16]) {
    for c in 0..4 {
        let i = 4 * c;
        let a0 = state[i];
        let a1 = state[i + 1];
        let a2 = state[i + 2];
        let a3 = state[i + 3];
        state[i] = gmul(a0, 14) ^ gmul(a1, 11) ^ gmul(a2, 13) ^ gmul(a3, 9);
        state[i + 1] = gmul(a0, 9) ^ gmul(a1, 14) ^ gmul(a2, 11) ^ gmul(a3, 13);
        state[i + 2] = gmul(a0, 13) ^ gmul(a1, 9) ^ gmul(a2, 14) ^ gmul(a3, 11);
        state[i + 3] = gmul(a0, 11) ^ gmul(a1, 13) ^ gmul(a2, 9) ^ gmul(a3, 14);
    }
}

/// Decrypt one 16-byte block with AES-128.
fn aes128_decrypt_block(block: &[u8; 16], rks: &[[u8; 16]; 11]) -> [u8; 16] {
    let mut state = *block;
    add_round_key(&mut state, &rks[10]);
    for round in (1..10).rev() {
        inv_shift_rows(&mut state);
        inv_sub_bytes(&mut state);
        add_round_key(&mut state, &rks[round]);
        inv_mix_columns(&mut state);
    }
    inv_shift_rows(&mut state);
    inv_sub_bytes(&mut state);
    add_round_key(&mut state, &rks[0]);
    state
}

/// AES-128-CBC decrypt with PKCS7 unpadding. `iv` and each block are 16 bytes.
fn aes128_cbc_decrypt(key: &[u8; 16], iv: &[u8; 16], ct: &[u8]) -> Option<Vec<u8>> {
    if ct.is_empty() || ct.len() % 16 != 0 {
        return None;
    }
    let rks = key_expansion(key);
    let mut prev = *iv;
    let mut out = Vec::with_capacity(ct.len());
    for chunk in ct.chunks_exact(16) {
        let mut blk = [0u8; 16];
        blk.copy_from_slice(chunk);
        let dec = aes128_decrypt_block(&blk, &rks);
        for i in 0..16 {
            out.push(dec[i] ^ prev[i]);
        }
        prev = blk;
    }
    // PKCS7 unpad
    let pad = *out.last()? as usize;
    if pad == 0 || pad > 16 || pad > out.len() {
        return None;
    }
    if !out[out.len() - pad..].iter().all(|&b| b as usize == pad) {
        return None;
    }
    out.truncate(out.len() - pad);
    Some(out)
}

// ----------------------------------- Fernet ------------------------------------

/// Decrypt a Fernet token using a url-safe-base64 32-byte key.
/// Returns the plaintext on success (HMAC verified). TTL is intentionally not enforced —
/// Frappe's get_decrypted_password does not pass a ttl either.
pub fn fernet_decrypt(key_b64: &str, token_b64: &str) -> Option<Vec<u8>> {
    let key = b64url_decode(key_b64)?;
    if key.len() != 32 {
        return None;
    }
    let sign_key = &key[0..16];
    let mut enc_key = [0u8; 16];
    enc_key.copy_from_slice(&key[16..32]);

    let tok = b64url_decode(token_b64)?;
    // version(1) + ts(8) + iv(16) + ct(>=16) + hmac(32)
    if tok.len() < 1 + 8 + 16 + 16 + 32 {
        return None;
    }
    if tok[0] != 0x80 {
        return None;
    }
    let hmac_start = tok.len() - 32;
    let signed = &tok[..hmac_start];
    let mac = &tok[hmac_start..];
    let expect = hmac_sha256(sign_key, signed);
    if !ct_eq(&expect, mac) {
        return None; // tampered / wrong key
    }
    let mut iv = [0u8; 16];
    iv.copy_from_slice(&tok[9..25]);
    let ct = &tok[25..hmac_start];
    aes128_cbc_decrypt(&enc_key, &iv, ct)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha1_known() {
        // SHA1("abc") = a9993e364706816aba3e25717850c26c9cd0d89d
        let h = sha1(b"abc");
        assert_eq!(
            h.iter().map(|b| format!("{b:02x}")).collect::<String>(),
            "a9993e364706816aba3e25717850c26c9cd0d89d"
        );
        // RFC 6455 WebSocket handshake vector.
        let key = "dGhlIHNhbXBsZSBub25jZQ==";
        let magic = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";
        let accept = b64_encode_std(&sha1(format!("{key}{magic}").as_bytes()));
        assert_eq!(accept, "s3pPLMBiTxaQ9kYGzzhZRbK+xOo=");
    }

    #[test]
    fn sha256_known() {
        // SHA256("abc")
        let h = sha256(b"abc");
        assert_eq!(
            h.iter().map(|b| format!("{b:02x}")).collect::<String>(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn hmac_known() {
        // RFC 4231 test case 2: key="Jefe", data="what do ya want for nothing?"
        let h = hmac_sha256(b"Jefe", b"what do ya want for nothing?");
        assert_eq!(
            h.iter().map(|b| format!("{b:02x}")).collect::<String>(),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
    }

    #[test]
    fn fernet_roundtrip_vector() {
        // Vector generated by python cryptography.Fernet with key = bytes(0..32).
        let key = "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8=";
        let token = "gAAAAABqJEB45tQaGY_5JjkQClP-a2caKrAz38UIvVSXHr-ipVb5oYGs-Quk_LCDJ8bZGjDTAOKgoFf3OxGly-AKbC8sLvuWMvihUhEFbbMrabOC4O-hA4oflvsz3-7z8scyhWEZKwvK";
        let pt = fernet_decrypt(key, token).expect("decrypt");
        assert_eq!(pt, b"my-super-secret-api-secret-1234567890");
    }
}
