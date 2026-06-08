//! In-process cache — ferro's internal replacement for the bench's `redis_cache`.
//!
//! Frappe leans on `frappe.cache()` for session data, rate-limit counters, transient values,
//! and assorted key/value + hash bookkeeping. Because ferro is the framework (it doesn't shell
//! out to Python or to a separate `redis-server`), that cache is just an in-process map guarded
//! by a mutex, with lazy TTL expiry. Same semantics ferro's handlers expect, zero extra
//! processes, a few KB of code.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

fn now_ms() -> u128 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0)
}

enum Val {
    Str(Vec<u8>),
    Hash(HashMap<String, Vec<u8>>),
}

struct Entry {
    val: Val,
    /// Absolute expiry in unix-ms; None = no expiry.
    expires_at: Option<u128>,
}

/// Thread-safe in-process cache. Cheap to clone the `Arc` around it.
pub struct Cache {
    map: Mutex<HashMap<String, Entry>>,
}

impl Default for Cache {
    fn default() -> Self {
        Cache { map: Mutex::new(HashMap::new()) }
    }
}

impl Cache {
    pub fn new() -> Self {
        Cache::default()
    }

    fn live<'a>(map: &'a mut HashMap<String, Entry>, key: &str) -> Option<&'a mut Entry> {
        let expired = match map.get(key) {
            Some(e) => e.expires_at.map(|d| now_ms() >= d).unwrap_or(false),
            None => return None,
        };
        if expired {
            map.remove(key);
            return None;
        }
        map.get_mut(key)
    }

    // ---- string ops ----

    pub fn set(&self, key: &str, val: &[u8]) {
        let mut m = self.map.lock().unwrap();
        m.insert(key.to_string(), Entry { val: Val::Str(val.to_vec()), expires_at: None });
    }

    pub fn setex(&self, key: &str, val: &[u8], ttl: Duration) {
        let mut m = self.map.lock().unwrap();
        m.insert(
            key.to_string(),
            Entry { val: Val::Str(val.to_vec()), expires_at: Some(now_ms() + ttl.as_millis()) },
        );
    }

    pub fn get(&self, key: &str) -> Option<Vec<u8>> {
        let mut m = self.map.lock().unwrap();
        match Self::live(&mut m, key) {
            Some(Entry { val: Val::Str(s), .. }) => Some(s.clone()),
            _ => None,
        }
    }

    pub fn get_str(&self, key: &str) -> Option<String> {
        self.get(key).map(|b| String::from_utf8_lossy(&b).into_owned())
    }

    pub fn set_str(&self, key: &str, val: &str) {
        self.set(key, val.as_bytes());
    }

    pub fn delete(&self, key: &str) -> bool {
        let mut m = self.map.lock().unwrap();
        m.remove(key).is_some()
    }

    pub fn exists(&self, key: &str) -> bool {
        let mut m = self.map.lock().unwrap();
        Self::live(&mut m, key).is_some()
    }

    /// Atomic increment-by; treats a missing/blank key as 0. Returns the new value.
    pub fn incr_by(&self, key: &str, by: i64) -> i64 {
        let mut m = self.map.lock().unwrap();
        let cur = match Self::live(&mut m, key) {
            Some(Entry { val: Val::Str(s), .. }) => {
                String::from_utf8_lossy(s).trim().parse::<i64>().unwrap_or(0)
            }
            _ => 0,
        };
        let nv = cur + by;
        m.insert(key.to_string(), Entry { val: Val::Str(nv.to_string().into_bytes()), expires_at: None });
        nv
    }

    /// Set a TTL on an existing key (no-op if absent). Returns true if applied.
    pub fn expire(&self, key: &str, ttl: Duration) -> bool {
        let mut m = self.map.lock().unwrap();
        if let Some(e) = Self::live(&mut m, key) {
            e.expires_at = Some(now_ms() + ttl.as_millis());
            true
        } else {
            false
        }
    }

    // ---- hash ops ----

    pub fn hset(&self, key: &str, field: &str, val: &[u8]) {
        let mut m = self.map.lock().unwrap();
        Self::live(&mut m, key); // drop if expired
        let e = m
            .entry(key.to_string())
            .or_insert_with(|| Entry { val: Val::Hash(HashMap::new()), expires_at: None });
        if let Val::Hash(h) = &mut e.val {
            h.insert(field.to_string(), val.to_vec());
        } else {
            let mut h = HashMap::new();
            h.insert(field.to_string(), val.to_vec());
            e.val = Val::Hash(h);
        }
    }

    pub fn hget(&self, key: &str, field: &str) -> Option<Vec<u8>> {
        let mut m = self.map.lock().unwrap();
        match Self::live(&mut m, key) {
            Some(Entry { val: Val::Hash(h), .. }) => h.get(field).cloned(),
            _ => None,
        }
    }

    pub fn hgetall(&self, key: &str) -> HashMap<String, Vec<u8>> {
        let mut m = self.map.lock().unwrap();
        match Self::live(&mut m, key) {
            Some(Entry { val: Val::Hash(h), .. }) => h.clone(),
            _ => HashMap::new(),
        }
    }

    pub fn hdelete(&self, key: &str, field: &str) -> bool {
        let mut m = self.map.lock().unwrap();
        match Self::live(&mut m, key) {
            Some(Entry { val: Val::Hash(h), .. }) => h.remove(field).is_some(),
            _ => false,
        }
    }

    // ---- introspection / maintenance ----

    /// Keys matching a simple `*`-glob (prefix/suffix/substring). Used by `delete_keys(pattern)`.
    pub fn keys(&self, pattern: &str) -> Vec<String> {
        let mut m = self.map.lock().unwrap();
        let now = now_ms();
        m.retain(|_, e| e.expires_at.map(|d| now < d).unwrap_or(true));
        m.keys().filter(|k| glob(pattern, k)).cloned().collect()
    }

    pub fn delete_keys(&self, pattern: &str) -> usize {
        let to_del = self.keys(pattern);
        let mut m = self.map.lock().unwrap();
        let mut n = 0;
        for k in to_del {
            if m.remove(&k).is_some() {
                n += 1;
            }
        }
        n
    }

    pub fn clear(&self) {
        self.map.lock().unwrap().clear();
    }

    pub fn len(&self) -> usize {
        let mut m = self.map.lock().unwrap();
        let now = now_ms();
        m.retain(|_, e| e.expires_at.map(|d| now < d).unwrap_or(true));
        m.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Minimal `*`-glob: supports a single leading/trailing `*` and bare substrings, which covers
/// Frappe's `cache().delete_keys("prefix*")` usage. Exact match otherwise.
fn glob(pattern: &str, s: &str) -> bool {
    if pattern == "*" {
        return true;
    }
    match (pattern.starts_with('*'), pattern.ends_with('*')) {
        (true, true) => {
            let inner = &pattern[1..pattern.len() - 1];
            s.contains(inner)
        }
        (true, false) => s.ends_with(&pattern[1..]),
        (false, true) => s.starts_with(&pattern[..pattern.len() - 1]),
        (false, false) => pattern == s,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn string_roundtrip_and_ttl() {
        let c = Cache::new();
        c.set_str("a", "1");
        assert_eq!(c.get_str("a").as_deref(), Some("1"));
        assert!(c.exists("a"));
        assert_eq!(c.incr_by("a", 4), 5);
        c.setex("t", b"x", Duration::from_millis(20));
        assert!(c.exists("t"));
        std::thread::sleep(Duration::from_millis(40));
        assert!(!c.exists("t"));
        assert_eq!(c.get("t"), None);
    }

    #[test]
    fn hash_and_keys() {
        let c = Cache::new();
        c.hset("h", "f1", b"v1");
        c.hset("h", "f2", b"v2");
        assert_eq!(c.hget("h", "f1").as_deref(), Some(&b"v1"[..]));
        assert_eq!(c.hgetall("h").len(), 2);
        assert!(c.hdelete("h", "f1"));
        assert_eq!(c.hgetall("h").len(), 1);

        c.set_str("user:1", "a");
        c.set_str("user:2", "b");
        c.set_str("other", "c");
        let mut ks = c.keys("user:*");
        ks.sort();
        assert_eq!(ks, vec!["user:1".to_string(), "user:2".to_string()]);
        assert_eq!(c.delete_keys("user:*"), 2);
        assert!(!c.exists("user:1"));
        assert!(c.exists("other"));
    }
}
