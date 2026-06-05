//! rewind — hash-addressed store of original payloads for reversible-lossy
//! compression (F1, internal-ref). Read section-extraction drops body content but
//! stashes the full original here; the agent retrieves the exact original (or a
//! line range) by id via the `retrieve` subcommand / MCP tool. Lossy-BUT-
//! recoverable: every dropped byte is fetchable, so aggressive extraction stays
//! safe (the validation that 6/12 aggressive reads drop load-bearing anchors is
//! exactly why this retrieval net is mandatory, not optional).
//!
//! Store: `<data_dir>/rewind/<session>/<id>` — `id` = blake2b-16 hex of the
//! content, so identical content dedups across calls. LRU + TTL capped per
//! session. Fail-open on every I/O: a miss/error returns None and the caller
//! tells the agent to re-read (never a panic, never a block).

use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use blake2::Blake2bVar;
use blake2::digest::{Update, VariableOutput};

const MAX_ENTRIES: usize = 256; // per-session LRU cap
const TTL_SECS: u64 = 24 * 3600;

fn sanitize(s: &str) -> String {
    let out: String = s
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if out.is_empty() {
        "default".to_string()
    } else {
        out
    }
}

fn rewind_dir(session: &str) -> PathBuf {
    let base = match std::env::var("GLYPHDOWN_REWIND_DIR") {
        Ok(v) if !v.trim().is_empty() => PathBuf::from(v),
        _ => crate::data_dir::resolve().join("rewind"),
    };
    base.join(sanitize(session))
}

/// Content-addressed id: blake2b-16 hex of the bytes.
pub fn content_id(content: &str) -> String {
    let mut h = Blake2bVar::new(8).expect("blake2b-8 is a valid size");
    h.update(content.as_bytes());
    let mut out = [0u8; 8];
    h.finalize_variable(&mut out).expect("8-byte buffer");
    out.iter().map(|b| format!("{b:02x}")).collect()
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Stash content, return its id. Idempotent: same content -> same id, written
/// once. Fail-open: None on any I/O error (caller then skips extraction).
pub fn stash(session: &str, content: &str) -> Option<String> {
    if content.is_empty() {
        return None;
    }
    let id = content_id(content);
    let dir = rewind_dir(session);
    std::fs::create_dir_all(&dir).ok()?;
    let path = dir.join(&id);
    if !path.exists() {
        let tmp = dir.join(format!(".{}.{}.tmp", id, std::process::id()));
        std::fs::write(&tmp, content).ok()?;
        // best-effort atomic publish
        let _ = std::fs::rename(&tmp, &path);
    }
    prune(&dir);
    Some(id)
}

/// Retrieve the original, or a 1-based inclusive line range `"A-B"`. None when
/// the id is missing/evicted (the caller tells the agent to re-read).
pub fn retrieve(session: &str, id: &str, range: Option<&str>) -> Option<String> {
    let path = rewind_dir(session).join(sanitize(id));
    let content = std::fs::read_to_string(&path).ok()?;
    match range {
        None => Some(content),
        Some(r) => {
            let (a, b) = parse_range(r)?;
            let lines: Vec<&str> = content.lines().collect();
            let lo = a.saturating_sub(1).min(lines.len());
            let hi = b.min(lines.len());
            if lo >= hi {
                return Some(String::new());
            }
            Some(lines[lo..hi].join("\n"))
        }
    }
}

fn parse_range(r: &str) -> Option<(usize, usize)> {
    let (a, b) = r.split_once('-')?;
    Some((a.trim().parse().ok()?, b.trim().parse().ok()?))
}

/// Evict by TTL, then cap to MAX_ENTRIES newest-by-mtime. Best-effort, silent.
fn prune(dir: &PathBuf) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    let now = now_secs();
    let mut entries: Vec<(PathBuf, u64)> = Vec::new();
    for e in rd.flatten() {
        let p = e.path();
        let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if name.starts_with('.') {
            continue; // skip tmp files
        }
        let mtime = e
            .metadata()
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .unwrap_or(0);
        if now.saturating_sub(mtime) > TTL_SECS {
            let _ = std::fs::remove_file(&p);
        } else {
            entries.push((p, mtime));
        }
    }
    if entries.len() > MAX_ENTRIES {
        entries.sort_by_key(|(_, m)| *m); // oldest first
        let excess = entries.len() - MAX_ENTRIES;
        for (p, _) in entries.into_iter().take(excess) {
            let _ = std::fs::remove_file(&p);
        }
    }
}

/// Serializes env-mutating tests across the rewind + extract modules — both poke
/// `GLYPHDOWN_REWIND_DIR` and the process env is global, so they must not race.
#[cfg(test)]
pub(crate) static TEST_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(test)]
mod tests {
    use super::*;

    fn iso_env(name: &str) -> std::sync::MutexGuard<'static, ()> {
        let g = TEST_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!("glyphdown-rewind-test-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // SAFETY: test-only; the lock above serializes all env-mutating tests.
        unsafe { std::env::set_var("GLYPHDOWN_REWIND_DIR", &dir) };
        g
    }

    #[test]
    fn stash_then_retrieve_is_byte_identical() {
        let _g = iso_env("roundtrip");
        let content = "line one\nline two\nline three\nfile.rs:42 error here\ntail";
        let id = stash("sess", content).expect("stash");
        assert_eq!(retrieve("sess", &id, None).as_deref(), Some(content));
    }

    #[test]
    fn retrieve_range_returns_exact_slice() {
        let _g = iso_env("range");
        let content = "L1\nL2\nL3\nL4\nL5";
        let id = stash("s", content).unwrap();
        assert_eq!(
            retrieve("s", &id, Some("2-4")).as_deref(),
            Some("L2\nL3\nL4")
        );
        assert_eq!(retrieve("s", &id, Some("1-1")).as_deref(), Some("L1"));
    }

    #[test]
    fn same_content_same_id_idempotent() {
        let _g = iso_env("idem");
        let a = stash("s", "hello world payload").unwrap();
        let b = stash("s", "hello world payload").unwrap();
        assert_eq!(a, b, "content-addressed: identical content -> identical id");
    }

    #[test]
    fn missing_id_is_none_fail_open() {
        let _g = iso_env("missing");
        assert!(retrieve("s", "deadbeefdeadbeef", None).is_none());
    }

    #[test]
    fn content_id_is_stable_and_hex16() {
        let id = content_id("anything");
        assert_eq!(id.len(), 16);
        assert!(id.chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(content_id("anything"), id);
    }
}
