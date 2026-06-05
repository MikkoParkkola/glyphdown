//! gate — F2 State-aware Gate (internal-ref, DESIGN-0.5.0 §F2).
//!
//! Per-session state machine that decides how aggressively the caller should
//! compress a tool output, based on whether the agent is "stuck" on a target.
//!
//! ## State transitions (per `target`, caller-normalised path or command)
//!
//! STANDARD  (default)           -- caller does normal compression
//! ULTRA     (exact repeat:      -- caller collapses to one line
//!            same target AND
//!            same output-hash seen this session)
//! FULL      (stuck:             -- caller backs OFF compression; preserve signal
//!            same target failed
//!            >=2 times AND a
//!            fix was attempted)
//! Priority: FULL > ULTRA > STANDARD
//!
//! ## Key contract
//!
//! Both `decide` and `note_edit` key on the raw `target` string the caller
//! passes in. FULL fires in production only when the failing tool-call and the
//! corrective edit normalise to the **same** `target` value. The gate cannot
//! enforce that normalisation -- it is the caller's load-bearing contract.
//! The `tool` parameter in `decide` is carried for future per-tool statistics;
//! v1 does not include it in the map key.
//!
//! ## Persistence
//!
//! State is written to `<state_dir>/gate-<session>.json` using the same
//! `GLYPHDOWN_STATE_DIR` env override and atomic tmp-rename pattern as
//! `dedup.rs`. Fail-open: any I/O or parse error returns
//! `GateDecision::Standard`.
//!
//! ## Thread-safety
//!
//! Each call is load-mutate-save (not locked). Concurrent processes sharing a
//! session will have a last-writer-wins race -- acceptable for this use case,
//! which is always driven by a single agent thread.

// This module ships as a self-contained integration unit. Dead-code lint fires
// on the pub API until the PostToolUse hook path wires callers into main.rs.
// The pattern is identical to futamura.rs and signed_ccr.rs in this crate.
#![allow(dead_code)]

use std::collections::HashMap;
use std::sync::OnceLock;

use regex_lite::Regex;
use serde::{Deserialize, Serialize};

// ---- public types -----------------------------------------------------------

/// Compression level the caller should apply to the current tool output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GateDecision {
    /// Default: apply normal compression.
    Standard,
    /// Exact repeat (same target + same output-hash): collapse to one line.
    Ultra,
    /// Agent is stuck (>=2 failures + a fix attempt): back off compression.
    Full,
}

// ---- per-target record ------------------------------------------------------

/// State kept for one target string across a session.
#[derive(Serialize, Deserialize, Clone, Default)]
struct TargetRecord {
    /// Number of times this target produced an error response.
    fail_count: u32,
    /// FNV-1a 32-bit hex of the last output seen for this target.
    last_output_hash: String,
    /// Set to true when an Edit/Write touches this target after a failure.
    fix_attempted: bool,
}

// ---- session state ----------------------------------------------------------

#[derive(Serialize, Deserialize, Default)]
struct GateState {
    /// Keyed by the raw `target` string passed by the caller.
    #[serde(default)]
    targets: HashMap<String, TargetRecord>,
}

// ---- helpers ----------------------------------------------------------------

/// Returns the directory used for gate state files.
///
/// Mirrors dedup.rs: GLYPHDOWN_STATE_DIR env var wins; otherwise the
/// glyphdown data dir via crate::data_dir::resolve.
fn state_dir() -> std::path::PathBuf {
    if let Ok(v) = std::env::var("GLYPHDOWN_STATE_DIR") {
        let t = v.trim();
        if !t.is_empty() {
            return std::path::PathBuf::from(t);
        }
    }
    crate::data_dir::resolve()
}

/// Sanitise a session id to a filesystem-safe token (max 128 chars).
///
/// Mirrors the behaviour in dedup.rs exactly: replace any character that is
/// not [A-Za-z0-9_-] with _, truncate to 128, fall back to "default".
fn sanitize_session(session_id: &str) -> String {
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
    state_dir().join(format!("gate-{}.json", sanitize_session(session_id)))
}

/// Load state for session_id. Returns GateState::default on any error (fail-open).
fn load_state(session_id: &str) -> GateState {
    match std::fs::read_to_string(state_path(session_id)) {
        Ok(s) => serde_json::from_str::<GateState>(&s).unwrap_or_default(),
        Err(_) => GateState::default(),
    }
}

/// Atomically save state. Silently swallows every I/O error (fail-open).
fn save_state(session_id: &str, state: &GateState) {
    let path = state_path(session_id);
    let Some(dir) = path.parent() else { return };
    if std::fs::create_dir_all(dir).is_err() {
        return;
    }
    let tmp = dir.join(format!(
        "gate-{}.{}.tmp",
        sanitize_session(session_id),
        std::process::id()
    ));
    let Ok(body) = serde_json::to_string(state) else {
        return;
    };
    if std::fs::write(&tmp, body).is_ok() {
        let _ = std::fs::rename(&tmp, &path);
    }
    // Best-effort cleanup if rename failed.
    let _ = std::fs::remove_file(&tmp);
}

/// FNV-1a 32-bit hash of data as lowercase 8-hex digits.
///
/// Matches the implementation in dedup.rs byte-for-byte. Duplicated here so
/// gate.rs is self-contained and requires no changes to other modules.
fn fnv1a_32(data: &str) -> String {
    let mut h: u32 = 0x811C_9DC5;
    for byte in data.as_bytes() {
        h ^= *byte as u32;
        h = h.wrapping_mul(0x0100_0193);
    }
    format!("{h:08x}")
}

// ---- error-pattern regex ----------------------------------------------------

fn error_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r"(?i)(?:error|fail(?:ed|ure)?|exit\s+code\s+[1-9]\d*|exception|traceback|panic)",
        )
        .expect("static regex is valid")
    })
}

// ---- public API -------------------------------------------------------------

/// Cheap heuristic: returns true when text looks like an error response.
///
/// Covers the patterns: error, fail/failed/failure, exit code N (N >= 1),
/// exception, traceback, panic -- all case-insensitive.
///
/// The caller MAY use this to compute is_error before calling decide,
/// or may have its own detection; both paths are valid.
pub fn looks_like_error(text: &str) -> bool {
    error_re().is_match(text)
}

/// Record that an Edit or Write tool touched `target` within this session.
///
/// `target` must be the **same string** the caller passes to `decide` for the
/// failing command -- this is the load-bearing key contract (see module doc).
/// Arms `fix_attempted` on the target record only if at least one failure has
/// already been seen; otherwise it is a no-op.
///
/// Fail-open: any I/O error is silently swallowed.
pub fn note_edit(session_id: &str, target: &str) {
    let mut state = load_state(session_id);
    let record = state.targets.entry(target.to_string()).or_default();
    // Only mark fix_attempted when there is a prior failure to fix.
    if record.fail_count > 0 {
        record.fix_attempted = true;
        save_state(session_id, &state);
    }
}

/// Core decision function -- call once per tool output.
///
/// Updates the per-target record and returns the appropriate GateDecision.
/// Priority: Full > Ultra > Standard.
///
/// - `session_id`: opaque session identifier (sanitised internally).
/// - `tool`: tool name (e.g. "Bash"). Carried for future per-tool statistics;
///   v1 does not include it in the map key (see module-level key contract).
/// - `target`: caller-normalised path or logical command (e.g. "cargo test").
///   This is the map key; `note_edit` must receive the **same** value for
///   FULL to fire correctly.
/// - `output`: raw tool output text.
/// - `is_error`: caller's error judgement; use `looks_like_error` or own heuristic.
///
/// Fail-open: any I/O error returns Standard.
pub fn decide(
    session_id: &str,
    tool: &str,
    target: &str,
    output: &str,
    is_error: bool,
) -> GateDecision {
    // v1: key on target alone. `tool` is reserved for per-tool stats in v2.
    let _ = tool;
    let hash = fnv1a_32(output);

    let mut state = load_state(session_id);
    let record = state.targets.entry(target.to_string()).or_default();

    // Capture pre-update values for the decision logic.
    let prior_hash = record.last_output_hash.clone();
    let prior_fail_count = record.fail_count;
    let prior_fix_attempted = record.fix_attempted;

    // Mutate: record the new output hash and increment fail_count if needed.
    record.last_output_hash = hash.clone();
    if is_error {
        record.fail_count = record.fail_count.saturating_add(1);
    }
    save_state(session_id, &state);

    // Compute the post-update fail_count for the FULL check.
    let new_fail_count = if is_error {
        prior_fail_count.saturating_add(1)
    } else {
        prior_fail_count
    };

    // Priority: FULL > ULTRA > STANDARD.
    //
    // FULL: >=2 cumulative failures on this target AND a fix was attempted
    // after a prior failure. We check prior_fix_attempted (pre-update value)
    // so that note_edit recorded earlier in the session correctly gates this.
    if new_fail_count >= 2 && prior_fix_attempted {
        return GateDecision::Full;
    }

    // ULTRA: same target, same output hash, already seen this session.
    if !prior_hash.is_empty() && prior_hash == hash {
        return GateDecision::Ultra;
    }

    GateDecision::Standard
}

// ---- tests ------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // All env-mutating tests across the crate share rewind::TEST_ENV_LOCK so gate,
    // dedup, rewind and extract never race on the process-global GLYPHDOWN_STATE_DIR
    // / GLYPHDOWN_REWIND_DIR. A gate-local lock would NOT serialise against dedup's
    // cache-safety test (which also mutates GLYPHDOWN_STATE_DIR) — that cross-module
    // race is what flaked gate::full_priority_beats_ultra on CI.
    use crate::rewind::TEST_ENV_LOCK as ENV_LOCK;

    /// Set up a fresh, unique temp dir for a single test and point
    /// GLYPHDOWN_STATE_DIR at it. Returns the dir path; caller holds the
    /// guard (which keeps ENV_LOCK held) until teardown.
    fn setup_test_dir(label: &str) -> (std::sync::MutexGuard<'static, ()>, std::path::PathBuf) {
        let guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!("glyphdown-gate-test-{label}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // SAFETY: test-only single-threaded section; ENV_LOCK guarantees no
        // other test in this process reads GLYPHDOWN_STATE_DIR concurrently.
        unsafe { std::env::set_var("GLYPHDOWN_STATE_DIR", &dir) };
        (guard, dir)
    }

    fn teardown_test_dir(dir: &std::path::Path) {
        // SAFETY: called while ENV_LOCK is still held by the caller's guard.
        unsafe { std::env::remove_var("GLYPHDOWN_STATE_DIR") };
        let _ = std::fs::remove_dir_all(dir);
    }

    // ---- looks_like_error ---------------------------------------------------

    #[test]
    fn looks_like_error_matches_error_keyword() {
        assert!(looks_like_error("error: build failed"));
    }

    #[test]
    fn looks_like_error_matches_failed_keyword() {
        assert!(looks_like_error("test FAILED after 3 retries"));
    }

    #[test]
    fn looks_like_error_matches_exit_code_nonzero() {
        assert!(looks_like_error("Process exited with exit code 1"));
        assert!(looks_like_error("exit code 127"));
    }

    #[test]
    fn looks_like_error_matches_exception() {
        assert!(looks_like_error("Exception: null pointer"));
    }

    #[test]
    fn looks_like_error_matches_traceback() {
        assert!(looks_like_error("Traceback (most recent call last)"));
    }

    #[test]
    fn looks_like_error_matches_panic() {
        assert!(looks_like_error("thread 'main' panicked at src/main.rs:5"));
    }

    #[test]
    fn looks_like_error_case_insensitive() {
        assert!(looks_like_error("ERROR: something went wrong"));
        assert!(looks_like_error("PANIC: oom"));
    }

    #[test]
    fn looks_like_error_rejects_clean_output() {
        assert!(!looks_like_error("build succeeded in 0.42s"));
        assert!(!looks_like_error("all tests passed"));
        assert!(!looks_like_error("exit code 0"));
    }

    // ---- decide: STANDARD (default) ----------------------------------------

    #[test]
    fn decide_returns_standard_on_first_successful_call() {
        // GIVEN a fresh session with no prior state
        let (_guard, dir) = setup_test_dir("standard-first");

        // WHEN deciding on a first-ever successful output
        let result = decide("sess1", "Bash", "cargo build", "build ok", false);

        // THEN standard compression applies
        assert_eq!(result, GateDecision::Standard);

        teardown_test_dir(&dir);
    }

    // ---- decide: ULTRA (exact repeat) --------------------------------------

    #[test]
    fn decide_returns_ultra_on_exact_output_repeat() {
        // GIVEN a session that already saw a specific output once
        let (_guard, dir) = setup_test_dir("ultra-repeat");
        let output = "test output: 42 passed";
        let _ = decide("sess2", "Bash", "cargo test", output, false);

        // WHEN the identical output appears again for the same target
        let result = decide("sess2", "Bash", "cargo test", output, false);

        // THEN ultra compression (collapse to one line) applies
        assert_eq!(result, GateDecision::Ultra);

        teardown_test_dir(&dir);
    }

    #[test]
    fn decide_returns_standard_when_output_changes() {
        // GIVEN a session that saw an output once
        let (_guard, dir) = setup_test_dir("ultra-changed");
        let _ = decide("sess3", "Bash", "ls", "file_a.txt", false);

        // WHEN the output differs (file list changed)
        let result = decide("sess3", "Bash", "ls", "file_b.txt", false);

        // THEN standard (output is novel, not an exact repeat)
        assert_eq!(result, GateDecision::Standard);

        teardown_test_dir(&dir);
    }

    // ---- decide: FULL (stuck: >=2 failures + fix attempted) ----------------

    #[test]
    fn decide_returns_full_after_two_failures_and_note_edit() {
        // GIVEN a target that has failed twice with a fix attempt in between
        let (_guard, dir) = setup_test_dir("full-stuck");
        let target = "cargo test";
        let err = "error: test failed";

        // Fail 1
        decide("sess4", "Bash", target, err, true);
        // Fix attempt -- note_edit takes the same bare target as decide
        note_edit("sess4", target);
        // Fail 2
        let result = decide("sess4", "Bash", target, err, true);

        // THEN full (back off -- agent is stuck)
        assert_eq!(result, GateDecision::Full);

        teardown_test_dir(&dir);
    }

    #[test]
    fn decide_does_not_return_full_without_fix_attempt() {
        // GIVEN two failures with NO edit/fix in between
        let (_guard, dir) = setup_test_dir("full-no-edit");
        let target = "make build";
        let err = "error: build failed";

        decide("sess5", "Bash", target, err, true);
        let result = decide("sess5", "Bash", target, err, true);

        // THEN NOT full -- fix_attempted was never set
        assert_ne!(result, GateDecision::Full);

        teardown_test_dir(&dir);
    }

    #[test]
    fn decide_returns_full_after_one_failure_then_edit_then_second_failure() {
        // GIVEN: fail 1 -> note_edit -> fail 2 (total 2 failures + fix_attempted)
        let (_guard, dir) = setup_test_dir("full-one-fail-then-edit");
        let target = "pytest";
        let err = "FAILED test_foo";

        decide("sess6", "Bash", target, err, true);
        note_edit("sess6", target);
        // Second failure tips fail_count to 2 with fix_attempted already set.
        let result = decide("sess6", "Bash", target, err, true);

        assert_eq!(result, GateDecision::Full);

        teardown_test_dir(&dir);
    }

    // ---- priority: FULL beats ULTRA ----------------------------------------

    #[test]
    fn full_priority_beats_ultra_when_output_also_repeats() {
        // GIVEN a target that has failed twice + fix attempted, with repeated output
        let (_guard, dir) = setup_test_dir("priority-full-over-ultra");
        let target = "flaky-cmd";
        // Identical error text would be ULTRA candidate without FULL
        let err = "error: connection refused";

        // Fail 1 (first occurrence -- not yet ULTRA)
        decide("sess7", "Bash", target, err, true);
        // Fix
        note_edit("sess7", target);
        // Fail 2 -- same output (ULTRA candidate) + fail_count=2 + fix -> FULL wins
        let result = decide("sess7", "Bash", target, err, true);

        // THEN FULL (priority beats ULTRA)
        assert_eq!(result, GateDecision::Full);

        teardown_test_dir(&dir);
    }

    // ---- note_edit no-ops when no prior failure ----------------------------

    #[test]
    fn note_edit_is_noop_without_prior_failure() {
        // GIVEN a target with zero failures
        let (_guard, dir) = setup_test_dir("noedit-noop");
        let target = "echo hello";

        note_edit("sess8", target);
        // Success after note_edit -- should still be Standard
        let result = decide("sess8", "Bash", target, "hello", false);

        assert_eq!(result, GateDecision::Standard);

        teardown_test_dir(&dir);
    }

    // ---- fail-open on bad state dir ----------------------------------------

    #[test]
    fn decide_fails_open_when_state_dir_is_unwritable() {
        // GIVEN a state dir path that cannot be created (a file blocks it)
        let guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let base = std::env::temp_dir().join("glyphdown-gate-test-failopen");
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        // A regular file where the state dir would live -- mkdir_all will fail.
        let blocker = base.join("gate-state-blocker");
        std::fs::write(&blocker, b"block").unwrap();

        // SAFETY: ENV_LOCK held; no concurrent env access in this process.
        unsafe { std::env::set_var("GLYPHDOWN_STATE_DIR", &blocker) };

        // WHEN decide is called with an unwritable state path
        let result = decide("sess9", "Bash", "cmd", "some output", false);

        // THEN fail-open: Standard (never panic)
        assert_eq!(result, GateDecision::Standard);

        // SAFETY: ENV_LOCK held.
        unsafe { std::env::remove_var("GLYPHDOWN_STATE_DIR") };
        let _ = std::fs::remove_dir_all(&base);
        drop(guard);
    }

    // ---- sanitize_session --------------------------------------------------

    #[test]
    fn sanitize_session_replaces_special_chars() {
        let s = sanitize_session("sess/id:with?special");
        assert!(!s.contains('/'));
        assert!(!s.contains(':'));
        assert!(!s.contains('?'));
    }

    #[test]
    fn sanitize_session_empty_falls_back_to_default() {
        // Only a truly empty string maps to "default"; special chars become underscores.
        assert_eq!(sanitize_session(""), "default");
        assert_eq!(sanitize_session("///"), "___");
    }

    #[test]
    fn sanitize_session_truncates_at_128() {
        let long = "a".repeat(200);
        assert_eq!(sanitize_session(&long).chars().count(), 128);
    }
}
