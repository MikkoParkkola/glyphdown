//! cache — SIL-5 cache-hot bypass (internal-ref), Rust port of ultracos_cache.py.
//!
//! Backs off compression on cache-hot prefixes so the Anthropic native prompt
//! cache key stays stable — compressing a payload whose long stable prefix is
//! already cached upstream mutates the cache key and forces a fresh fetch
//! (the prompt-cache-busting class). Heuristic: a prefix observed >= hot_hits
//! times within TTL is treated as cache-hot and bypasses compaction.
//!
//! Default OFF (`ULTRACOS_CACHE_AWARE`), so in the flipped-default rust path
//! this is a NO-OP with ZERO disk I/O — identical to python — until an operator
//! opts in. When enabled, the signature is blake2b-16 of the first prefix_bytes
//! so py and rust share `<data_dir>/cache_state.json` byte-for-byte on the hash.
//!
//! Fail-open on every I/O: any error reverts to the historical always-compress
//! behaviour (caller treats a panic/None as "do not bypass").

use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

use blake2::Blake2bVar;
use blake2::digest::{Update, VariableOutput};
use serde::{Deserialize, Serialize};

const POLICY_VERSION: i64 = 1;

struct Config {
    enabled: bool,
    prefix_bytes: usize,
    hot_hits: i64,
    ttl_seconds: f64,
    max_entries: usize,
}

fn int_env(name: &str, default: i64) -> i64 {
    std::env::var(name)
        .ok()
        .and_then(|v| {
            let t = v.trim().to_string();
            if t.is_empty() { None } else { t.parse().ok() }
        })
        .unwrap_or(default)
}

fn bool_env(name: &str, default: bool) -> bool {
    match std::env::var(name) {
        Ok(v) if !v.trim().is_empty() => {
            matches!(
                v.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        }
        _ => default,
    }
}

fn config() -> Config {
    Config {
        enabled: bool_env("ULTRACOS_CACHE_AWARE", false),
        prefix_bytes: int_env("ULTRACOS_CACHE_PREFIX_BYTES", 1024).max(64) as usize,
        hot_hits: int_env("ULTRACOS_CACHE_HOT_HITS", 2).max(2),
        ttl_seconds: int_env("ULTRACOS_CACHE_TTL_SECONDS", 7 * 86400).max(60) as f64,
        max_entries: int_env("ULTRACOS_CACHE_MAX_ENTRIES", 2048).max(16) as usize,
    }
}

#[derive(Serialize, Deserialize, Clone)]
struct Entry {
    hits: i64,
    first_seen: f64,
    last_seen: f64,
}

#[derive(Serialize, Deserialize)]
struct State {
    version: i64,
    prefixes: HashMap<String, Entry>,
}

impl Default for State {
    fn default() -> Self {
        State {
            version: POLICY_VERSION,
            prefixes: HashMap::new(),
        }
    }
}

fn now_secs() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

fn state_file() -> std::path::PathBuf {
    crate::data_dir::resolve().join("cache_state.json")
}

/// blake2b-16 hex of the first `prefix_bytes` UTF-8 bytes. None if text is
/// shorter than the limit (python: too short to participate in cache reuse).
pub fn prefix_signature(text: &str) -> Option<String> {
    if text.is_empty() {
        return None;
    }
    let limit = config().prefix_bytes;
    let raw = text.as_bytes();
    if raw.len() < limit {
        return None;
    }
    let head = &raw[..limit];
    let mut h = Blake2bVar::new(16).ok()?;
    h.update(head);
    let mut out = [0u8; 16];
    h.finalize_variable(&mut out).ok()?;
    Some(hex::encode(out))
}

fn load_state() -> State {
    match std::fs::read_to_string(state_file()) {
        Ok(s) => match serde_json::from_str::<State>(&s) {
            Ok(st) if st.version == POLICY_VERSION => st,
            _ => State::default(),
        },
        Err(_) => State::default(),
    }
}

fn save_state(state: &State) {
    let dir = crate::data_dir::resolve();
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let path = state_file();
    let tmp = dir.join(format!(".cache_state.{}.tmp", std::process::id()));
    let Ok(body) = serde_json::to_string(state) else {
        return;
    };
    if std::fs::write(&tmp, body).is_ok() {
        let _ = std::fs::rename(&tmp, &path);
    }
    let _ = std::fs::remove_file(&tmp); // best-effort cleanup if rename failed
}

fn prune(state: &mut State, now: f64, ttl: f64, max_entries: usize) {
    let cutoff = now - ttl;
    state.prefixes.retain(|_, v| v.last_seen >= cutoff);
    if state.prefixes.len() > max_entries {
        let mut kv: Vec<(String, f64)> = state
            .prefixes
            .iter()
            .map(|(k, v)| (k.clone(), v.last_seen))
            .collect();
        kv.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
        let overflow = state.prefixes.len() - max_entries;
        for (k, _) in kv.into_iter().take(overflow) {
            state.prefixes.remove(&k);
        }
    }
}

/// Record one sighting. Returns the signature. Zero disk I/O when disabled.
pub fn observe(text: &str, now: Option<f64>) -> Option<String> {
    let sig = prefix_signature(text)?;
    let cfg = config();
    if !cfg.enabled {
        return Some(sig); // default-off: NO state read/write
    }
    let ts = now.unwrap_or_else(now_secs);
    let mut state = load_state();
    let entry = state.prefixes.entry(sig.clone()).or_insert(Entry {
        hits: 0,
        first_seen: ts,
        last_seen: ts,
    });
    entry.hits += 1;
    entry.last_seen = ts;
    prune(&mut state, ts, cfg.ttl_seconds, cfg.max_entries);
    save_state(&state);
    Some(sig)
}

/// True when `text`'s prefix has been seen >= hot_hits within TTL.
pub fn is_cache_hot(text: &str, now: Option<f64>) -> bool {
    let cfg = config();
    if !cfg.enabled {
        return false;
    }
    let Some(sig) = prefix_signature(text) else {
        return false;
    };
    let ts = now.unwrap_or_else(now_secs);
    let state = load_state();
    let Some(entry) = state.prefixes.get(&sig) else {
        return false;
    };
    if (ts - entry.last_seen) > cfg.ttl_seconds {
        return false;
    }
    entry.hits >= cfg.hot_hits
}

/// Codec convenience: record THEN probe. First sighting -> hits=1 (below
/// default hot_hits=2, so compaction proceeds); second -> hits=2 -> bypass.
pub fn should_bypass_for_cache(text: &str, now: Option<f64>) -> bool {
    let _ = observe(text, now);
    is_cache_hot(text, now)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_text_has_no_signature() {
        assert_eq!(prefix_signature("tiny"), None);
    }

    #[test]
    fn signature_is_blake2b16_hex() {
        let text = "x".repeat(2000);
        let sig = prefix_signature(&text).unwrap();
        assert_eq!(sig.len(), 32); // 16 bytes -> 32 hex chars
        assert!(sig.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn disabled_is_zero_io_noop() {
        // ULTRACOS_CACHE_AWARE unset -> never bypass, no panic.
        let text = "y".repeat(2000);
        assert!(!should_bypass_for_cache(&text, Some(1000.0)));
        assert!(!is_cache_hot(&text, Some(1000.0)));
    }
}
