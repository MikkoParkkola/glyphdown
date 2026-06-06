//! audit — append-only audit log writer (G2 / internal-ref), Rust port of the
//! python codec's `_write_audit`. Restores PRODUCTION OBSERVABILITY for the
//! default Rust codec: without these rows the rust hot path is a measurement
//! black hole, with no record of compaction outcomes (saved tokens, shape,
//! per-tool volume) from rust-served sessions for downstream analysis.
//!
//! Writes each row to TWO independent destinations (each fail-open):
//!   1. GLOBAL  <data_dir>/audit.jsonl            (global outcome log)
//!   2. SESSION ~/.claude/data/glyphdown/audit-<session>.jsonl  (internal-ref)
//! NB the two roots differ: global honors GLYPHDOWN_DATA_DIR / ~/.ultracos;
//! per-session is always under HOME/.claude/data/glyphdown (matching python).
//!
//! SCOPE: load-bearing fields only. Downstream analysis consumes {event,ts,tool,saved_tokens,
//! shape}. The python `cache_class` + `volatile` cohort fields are intentionally
//! NOT emitted here (only audit_cohort.py reads them, and it uses .get()); that
//! is a documented gap, not a correctness issue.

use std::io::Write;

use serde_json::Value;

/// Epoch seconds (matches python time.time()).
fn now_ts() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

/// python `_sanitize_session_id`: non-[A-Za-z0-9_-] -> '_', strip leading
/// '.'/'-', cap 128, fallback "session-unspecified".
fn sanitize_session(session_id: &str) -> String {
    if session_id.is_empty() {
        return "session-unspecified".to_string();
    }
    let cleaned: String = session_id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let cleaned: String = cleaned
        .trim_start_matches(['.', '-'])
        .chars()
        .take(128)
        .collect();
    if cleaned.is_empty() {
        "session-unspecified".to_string()
    } else {
        cleaned
    }
}

fn append_line(path: &std::path::Path, line: &str) {
    // Fast path: try to open+append directly (dir usually already exists);
    // only pay create_dir_all on the first write of a session. Avoids two
    // mkdir syscalls per hook call on the hot path.
    let open = || {
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
    };
    let mut f = match open() {
        Ok(f) => f,
        Err(_) => {
            if let Some(dir) = path.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            match open() {
                Ok(f) => f,
                Err(_) => return,
            }
        }
    };
    let _ = f.write_all(line.as_bytes());
}

/// Append one audit row to the global + per-session logs. Fail-open throughout.
pub fn write_row(session_id: &str, row: &Value) {
    let Ok(mut line) = serde_json::to_string(row) else {
        return;
    };
    line.push('\n');

    // 1) global
    let global = crate::data_dir::resolve().join("audit.jsonl");
    append_line(&global, &line);

    // 2) per-session (HOME/.claude/data/glyphdown/audit-<safe>.jsonl)
    if let Some(home) = std::env::var_os("HOME") {
        let p = std::path::PathBuf::from(home)
            .join(".claude")
            .join("data")
            .join("glyphdown")
            .join(format!("audit-{}.jsonl", sanitize_session(session_id)));
        append_line(&p, &line);
    }
}

/// A "compact" audit row (the only event SIL-1 reads). round(ratio, 4) matches
/// python json serialization of the rounded float.
pub fn compact_row(tool: &str, session_id: &str, report: &crate::codec::CompactReport) -> Value {
    let ratio = if report.original_tokens != 0 {
        (report.compact_tokens as f64 / report.original_tokens as f64 * 10000.0).round() / 10000.0
    } else {
        1.0
    };
    serde_json::json!({
        "ts": now_ts(),
        "event": "compact",
        "tool": tool,
        "session_id": session_id,
        "shape": report.shape,
        "applied": report.applied,
        "original_tokens": report.original_tokens,
        "compact_tokens": report.compact_tokens,
        "saved_tokens": report.saved_tokens(),
        "ratio": ratio,
        "variant": "with-tag",
    })
}

pub fn dedup_row(
    tool: &str,
    session_id: &str,
    mode: &str,
    original_bytes: usize,
    new_bytes: usize,
) -> Value {
    serde_json::json!({
        "ts": now_ts(),
        "event": format!("dedup-{mode}"),
        "tool": tool,
        "session_id": session_id,
        "original_bytes": original_bytes,
        "new_bytes": new_bytes,
    })
}

pub fn simple_event(event: &str, tool: &str, session_id: &str) -> Value {
    serde_json::json!({
        "ts": now_ts(),
        "event": event,
        "tool": tool,
        "session_id": session_id,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_matches_python_rules() {
        assert_eq!(sanitize_session(""), "session-unspecified");
        // sub runs BEFORE lstrip: '.'->'_' (survives lstrip), '-' kept, '/'->'_'.
        assert_eq!(sanitize_session("..--abc/def"), "__--abc_def");
        assert_eq!(sanitize_session("--abc"), "abc"); // leading literal '-' stripped
        assert_eq!(sanitize_session("a/b:c"), "a_b_c");
        assert_eq!(sanitize_session("___"), "___");
    }
}
