//! dedup — A8 session dedup + summarize (internal-ref), Rust port of glyphdown_dedup.py.
//!
//! For Read/Grep/Glob/Monitor outputs: hash normalized content (FNV-1a 32-bit),
//! replace a repeat with `[seen earlier this session: <ref>]`, and summarize
//! oversize first-occurrences (head + error lines + tail). Session state lives
//! at <state_dir>/dedup-<session>.json (GLYPHDOWN_STATE_DIR override, else the
//! glyphdown data dir) — same path + JSON schema + FNV digest as python, so the
//! state files interoperate. Fail-open: any error returns None (caller
//! untouched). dedup is an ACTIVE token-saver, so porting it is what lets the
//! Rust path become the default (vs the phase-3 opt-in).

use std::sync::OnceLock;

use regex_lite::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const DEDUP_TOOLS: &[&str] = &["Read", "Grep", "Glob", "Monitor"];
const DEFAULT_SUMMARIZE_BYTES: usize = 8 * 1024;
const SUMMARIZE_HEAD_LINES: usize = 40;
const SUMMARIZE_TAIL_LINES: usize = 10;
const SUMMARIZE_MAX_ERROR_LINES: usize = 20;

pub fn is_dedup_tool(tool: &str) -> bool {
    DEDUP_TOOLS.contains(&tool)
}

struct Norms {
    ansi: Regex,
    iso_ts: Regex,
    syslog_ts: Regex,
    bracket_time: Regex,
    epoch_ms: Regex,
    pid_kv: Regex,
    pid_bracket: Regex,
    process_bracket: Regex,
    ws: Regex,
    error: Regex,
}

fn norms() -> &'static Norms {
    static N: OnceLock<Norms> = OnceLock::new();
    N.get_or_init(|| Norms {
        ansi: Regex::new(r"\x1b\[[0-9;?]*[A-Za-z]|\x1b\][^\x07]*\x07").unwrap(),
        iso_ts: Regex::new(
            r"\d{4}-\d{2}-\d{2}[T ]\d{2}:\d{2}:\d{2}(?:[.,]\d+)?(?:Z|[+-]\d{2}:?\d{2})?",
        )
        .unwrap(),
        syslog_ts: Regex::new(
            r"\b(?:Jan|Feb|Mar|Apr|May|Jun|Jul|Aug|Sep|Oct|Nov|Dec)\s+\d{1,2}\s+\d{2}:\d{2}:\d{2}(?:[.,]\d+)?\b",
        )
        .unwrap(),
        bracket_time: Regex::new(r"\[\d{2}:\d{2}:\d{2}(?:[.,]\d+)?\]").unwrap(),
        epoch_ms: Regex::new(r"\b1[5-9]\d{8}\d{3}\b|\b2[0-3]\d{8}\d{3}\b").unwrap(),
        pid_kv: Regex::new(r"(?i)\bpid[=:]\s*\d+\b").unwrap(),
        pid_bracket: Regex::new(r"(?i)\[pid[:\s]*\d+\]").unwrap(),
        process_bracket: Regex::new(r"\[\d{3,7}\]").unwrap(),
        ws: Regex::new(r"[ \t]+").unwrap(),
        error: Regex::new(r"(?i)\b(error|warning|warn|fatal|exception|traceback|panic|failed|fail)\b").unwrap(),
    })
}

/// python `_normalize`: ANSI strip, LF-normalize, volatile-field sentinels,
/// whitespace collapse, trim. Order matters (must match python exactly).
pub fn normalize(text: &str) -> String {
    let n = norms();
    let t = n.ansi.replace_all(text, "");
    let t = t.replace("\r\n", "\n").replace('\r', "\n");
    let t = n.iso_ts.replace_all(&t, "<TS>");
    let t = n.syslog_ts.replace_all(&t, "<TS>");
    let t = n.bracket_time.replace_all(&t, "<TS>");
    let t = n.epoch_ms.replace_all(&t, "<TS>");
    let t = n.pid_kv.replace_all(&t, "pid=<PID>");
    let t = n.pid_bracket.replace_all(&t, "[pid=<PID>]");
    let t = n.process_bracket.replace_all(&t, "[<PID>]");
    let t = n.ws.replace_all(&t, " ");
    t.trim().to_string()
}

/// FNV-1a 32-bit over UTF-8 bytes, lowercase 8-hex (python `fnv1a_32`).
pub fn fnv1a_32(data: &str) -> String {
    let mut h: u32 = 0x811C_9DC5;
    for byte in data.as_bytes() {
        h ^= *byte as u32;
        h = h.wrapping_mul(0x0100_0193);
    }
    format!("{h:08x}")
}

/// python `summarize_large`: head + middle error lines + tail. (out, changed).
pub fn summarize_large(text: &str) -> (String, bool) {
    let lines: Vec<&str> = text.split('\n').collect();
    // python str.splitlines() drops a single trailing newline's empty line;
    // `split('\n')` keeps it. Mirror splitlines: drop one trailing "" if the
    // text ended with '\n'.
    let lines: Vec<&str> = if text.ends_with('\n') {
        lines[..lines.len() - 1].to_vec()
    } else {
        lines
    };
    if lines.len() <= SUMMARIZE_HEAD_LINES + SUMMARIZE_TAIL_LINES + 5 {
        return (text.to_string(), false);
    }
    let head = &lines[..SUMMARIZE_HEAD_LINES];
    let tail = &lines[lines.len() - SUMMARIZE_TAIL_LINES..];
    let n = norms();
    let mut middle_errors: Vec<(usize, &str)> = Vec::new();
    for idx in SUMMARIZE_HEAD_LINES..lines.len() - SUMMARIZE_TAIL_LINES {
        if n.error.is_match(lines[idx]) {
            middle_errors.push((idx, lines[idx]));
            if middle_errors.len() >= SUMMARIZE_MAX_ERROR_LINES {
                break;
            }
        }
    }
    let hidden = lines.len() - head.len() - tail.len() - middle_errors.len();
    let mut pieces: Vec<String> = head.iter().map(|s| s.to_string()).collect();
    if !middle_errors.is_empty() {
        pieces.push(format!(
            "[glyphdown:summarize-v1 kept={} error-lines hidden={hidden} lines]",
            middle_errors.len()
        ));
        for (idx, ln) in &middle_errors {
            pieces.push(format!("L{}: {ln}", idx + 1));
        }
    } else {
        pieces.push(format!(
            "[glyphdown:summarize-v1 hidden={hidden} lines, no error pattern]"
        ));
    }
    pieces.extend(tail.iter().map(|s| s.to_string()));
    (pieces.join("\n"), true)
}

// ── session state ───────────────────────────────────────────────────────────

#[derive(Serialize, Deserialize, Clone)]
struct SeenEntry {
    #[serde(rename = "ref")]
    reference: String,
    bytes: u64,
}

#[derive(Serialize, Deserialize, Default)]
struct State {
    #[serde(default)]
    seen: std::collections::HashMap<String, SeenEntry>,
    #[serde(default)]
    counters: std::collections::HashMap<String, i64>,
}

fn state_dir() -> std::path::PathBuf {
    if let Ok(v) = std::env::var("GLYPHDOWN_STATE_DIR") {
        let t = v.trim();
        if !t.is_empty() {
            return std::path::PathBuf::from(t);
        }
    }
    crate::data_dir::resolve()
}

fn sanitize_session(session_id: &str) -> String {
    // python: re.sub(r"[^A-Za-z0-9_-]", "_", session_id)[:128] or "default"
    let mut s: String = session_id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if s.chars().count() > 128 {
        s = s.chars().take(128).collect();
    }
    if s.is_empty() {
        "default".to_string()
    } else {
        s
    }
}

fn state_path(session_id: &str) -> std::path::PathBuf {
    state_dir().join(format!("dedup-{}.json", sanitize_session(session_id)))
}

fn load_state(session_id: &str) -> State {
    match std::fs::read_to_string(state_path(session_id)) {
        Ok(s) => serde_json::from_str::<State>(&s).unwrap_or_default(),
        Err(_) => State::default(),
    }
}

fn save_state(session_id: &str, state: &State) {
    let path = state_path(session_id);
    let Some(dir) = path.parent() else { return };
    if std::fs::create_dir_all(dir).is_err() {
        return;
    }
    let tmp = dir.join(format!(
        "dedup-{}.{}.tmp",
        sanitize_session(session_id),
        std::process::id()
    ));
    let Ok(body) = serde_json::to_string(state) else {
        return;
    };
    if std::fs::write(&tmp, body).is_ok() {
        let _ = std::fs::rename(&tmp, &path);
    }
    let _ = std::fs::remove_file(&tmp);
}

/// python `maybe_dedup_or_summarize`. Returns (new_text, mode) where mode is
/// "dedup" or "summarize", or None for no rewrite. Fail-open.
pub fn maybe_dedup_or_summarize(
    tool_name: &str,
    text: &str,
    session_id: &str,
) -> Option<(String, String)> {
    if !is_dedup_tool(tool_name) || text.is_empty() {
        return None;
    }
    let mut state = load_state(session_id);
    let norm = normalize(text);
    if norm.is_empty() {
        return None;
    }
    let digest = fnv1a_32(&norm);

    if let Some(prior) = state.seen.get(&digest) {
        // dedup hit — full replacement, NO state write.
        return Some((
            format!("[seen earlier this session: {}]", prior.reference),
            "dedup".to_string(),
        ));
    }

    // first occurrence: assign ref, persist.
    let next_n = state.counters.get(tool_name).copied().unwrap_or(0) + 1;
    state.counters.insert(tool_name.to_string(), next_n);
    let reference = format!("{tool_name}#{next_n}");
    state.seen.insert(
        digest,
        SeenEntry {
            reference: reference.clone(),
            bytes: text.len() as u64,
        },
    );
    save_state(session_id, &state);

    // oversize -> summarize.
    if text.len() > DEFAULT_SUMMARIZE_BYTES {
        let (out, changed) = summarize_large(text);
        if changed {
            let tag = format!("[glyphdown:dedup-ref ref={reference}]\n");
            return Some((format!("{tag}{out}"), "summarize".to_string()));
        }
    }
    None
}

/// Resolve the session id the way python main() does: payload.session_id, else
/// $CLAUDE_SESSION_ID, else pid-<pid>.
pub fn resolve_session_id(payload: &Value) -> String {
    if let Some(s) = payload.get("session_id").and_then(Value::as_str) {
        if !s.is_empty() {
            return s.to_string();
        }
    }
    if let Ok(s) = std::env::var("CLAUDE_SESSION_ID") {
        if !s.is_empty() {
            return s;
        }
    }
    format!("pid-{}", std::process::id())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fnv1a_known_vector() {
        // FNV-1a 32-bit of "" is the offset basis 0x811c9dc5.
        assert_eq!(fnv1a_32(""), "811c9dc5");
    }

    #[test]
    fn normalize_strips_volatile_fields() {
        let a = normalize("2026-05-31T10:00:00Z  pid=123  hello");
        let b = normalize("2026-01-01T00:00:00Z  pid=999  hello");
        assert_eq!(a, b); // same logical line -> same normalized form
        assert!(a.contains("<TS>") && a.contains("pid=<PID>"));
    }

    #[test]
    fn non_dedup_tool_is_none() {
        assert!(maybe_dedup_or_summarize("Bash", "x", "s").is_none());
    }

    #[test]
    fn cache_safe_dedup_back_references_only_the_repeat() {
        // CACHE-SAFETY INVARIANT: dedup replaces the REPEAT with a back-reference and
        // never rewrites the earlier occurrence. The first occurrence is what may sit
        // in a cached prefix; leaving it untouched is why the codec can't bust the
        // Anthropic prompt cache. Isolated to a temp state dir.
        let dir = std::env::temp_dir().join("glyphdown-cachesafe-dedup-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // SAFETY: test-only; no other test reads GLYPHDOWN_STATE_DIR concurrently
        // (the other dedup tests touch no state), so this env write does not race.
        unsafe { std::env::set_var("GLYPHDOWN_STATE_DIR", &dir) };

        let sid = "cache-safe-sid";
        let content = "Read result body that repeats within the session";
        // first occurrence: passes through unchanged (None) — the cacheable copy.
        assert!(
            maybe_dedup_or_summarize("Read", content, sid).is_none(),
            "first occurrence must pass through unchanged (cacheable copy untouched)"
        );
        // second occurrence: only the CURRENT payload is back-referenced.
        let (out, mode) =
            maybe_dedup_or_summarize("Read", content, sid).expect("repeat must dedup");
        assert_eq!(mode, "dedup");
        assert!(
            out.starts_with("[seen earlier this session:"),
            "repeat -> back-ref to the earlier copy, which is never rewritten"
        );

        unsafe { std::env::remove_var("GLYPHDOWN_STATE_DIR") };
        let _ = std::fs::remove_dir_all(&dir);
    }
}
