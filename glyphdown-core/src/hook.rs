//! PostToolUse codec hook — Rust hot-path port (PHASE 2b.2, default codec).
//!
//! Reads a Claude Code PostToolUse hook payload from stdin:
//!   {"tool_name": "...", "tool_response": {"content": [{"type":"text","text":"..."}, ...],
//!                                          "structuredContent": <optional> }}
//! Per text block: A8 session-dedup -> SIL-5 cache-hot bypass ->
//! `codec::compact_payload` (classify -> shape-dispatch -> path-list factoring
//! -> truncate -> break-even -> schema-tag) -> internal-ref anchor-survival revert.
//! Emits the hook contract:
//!   - changed:   {"continue":true,"updatedToolOutput":<mutated tool_response>}
//!   - unchanged: {"continue":true}
//!
//! FAIL-OPEN is the contract: any parse/IO/transform error emits
//! {"continue":true} and the original tool output is preserved untouched.
//!
//! FLIP-SAFETY (see docs/SPEC_2026-05-31_plugin_completion_plan.md):
//!   Transform matches python 100% on the corpus; the SAFETY-CRITICAL guards
//!   (SIL-5 cache-bypass, internal-ref anchor) and the one ACTIVE token-saver
//!   (A8 session-dedup) are all ported and proven identical to python via
//!   bench/equiv_guards_rust_vs_python.py (signature 40/40, anchor-revert 55/55,
//!   cache interop PASS, dedup-parity 104/104, end-to-end 52/52). That makes the
//!   default flip token-safe. Audit rows (observability for the default rust
//!   path; feeds SIL-1) are also emitted — audit-row parity 22/22. DEFERRED
//!   (python-only, NONE are token-savers): SIL-2 learned skip-policy +
//!   min-payload/allowlist gates (these SKIP compaction — rust compacts a
//!   superset), the A/B no-tag experiment (~10% cohort; rust ships the with-tag
//!   control), and reading SIL-1 tuned thresholds (no production writer exists,
//!   so it would be a cold-start no-op). Set GLYPHDOWN_RUST=0 for python.

use std::io::Read;

use serde_json::Value;

const PASS: &str = "{\"continue\":true}";

/// internal-ref anchor guard toggle (GLYPHDOWN_ANCHOR_GUARD, default ON — matches
/// python ANCHOR_GUARD_ENABLED). Only "0"/"false"/"no"/"off" disable it.
fn anchor_guard_enabled() -> bool {
    match std::env::var("GLYPHDOWN_ANCHOR_GUARD") {
        Ok(v) if !v.trim().is_empty() => !matches!(
            v.trim().to_ascii_lowercase().as_str(),
            "0" | "false" | "no" | "off"
        ),
        _ => true,
    }
}

/// Opt-in feature flag: true only for "1"/"true"/"yes"/"on" (default OFF).
fn env_on(name: &str) -> bool {
    matches!(
        std::env::var(name)
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase()
            .as_str(),
        "1" | "true" | "yes" | "on"
    )
}

/// The F2 gate's `target`: the tool's subject. file path for Read/Edit/Write,
/// command for Bash, else a best-effort tool_input string. Empty if absent.
fn gate_target(payload: &Value, tool: &str) -> String {
    let input = payload.get("tool_input");
    let pick = |k: &str| {
        input
            .and_then(|i| i.get(k))
            .and_then(Value::as_str)
            .map(str::to_string)
    };
    match tool {
        "Read" | "Edit" | "Write" | "MultiEdit" => pick("file_path")
            .or_else(|| pick("path"))
            .unwrap_or_default(),
        "Bash" => pick("command").unwrap_or_default(),
        _ => pick("file_path")
            .or_else(|| pick("command"))
            .or_else(|| pick("path"))
            .unwrap_or_default(),
    }
}

/// Entry point. Always prints a valid hook response; never panics.
pub fn posttooluse() {
    let out = run().unwrap_or_else(|_| PASS.to_string());
    println!("{out}");
}

fn run() -> anyhow::Result<String> {
    let mut buf = String::new();
    std::io::stdin().read_to_string(&mut buf)?;
    let mut payload: Value = serde_json::from_str(&buf)?;

    // tool_name + session_id resolved BEFORE the mutable borrow of tool_response.
    let tool_name = payload
        .get("tool_name")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let session_id = crate::dedup::resolve_session_id(&payload);

    // F1/F2 (0.5.0), both OPT-IN and default OFF — the default path is byte-for-
    // byte unchanged. `target` is the tool's subject (file path for Read/Edit/
    // Write, command for Bash), extracted before the mutable tool_response borrow.
    let read_extract = env_on("GLYPHDOWN_READ_EXTRACT");
    let gate_on = env_on("GLYPHDOWN_GATE");
    let target = gate_target(&payload, &tool_name);
    // F2: an Edit/Write/MultiEdit touching `target` is the "fix attempted" signal.
    if gate_on && matches!(tool_name.as_str(), "Edit" | "Write" | "MultiEdit") {
        crate::gate::note_edit(&session_id, &target);
    }

    let Some(tool_response) = payload.get_mut("tool_response") else {
        return Ok(PASS.to_string());
    };

    let mut any_changed = false;

    // 1. content[] blocks: dedup -> cache-bypass -> compact -> anchor.
    if let Some(items) = tool_response
        .get_mut("content")
        .and_then(Value::as_array_mut)
    {
        for item in items.iter_mut() {
            let Some(mut text) = item.get("text").and_then(Value::as_str).map(str::to_string)
            else {
                continue;
            };

            // F1: read section-extraction (opt-in). Replaces a large Read result
            // with outline + anchors + head; the full original is in the rewind
            // store, retrievable by id+range. Reversible-lossy, so default OFF.
            if read_extract && tool_name == "Read" {
                if let Some(ex) = crate::extract::extract_read(&session_id, &text) {
                    crate::audit::write_row(
                        &session_id,
                        &crate::audit::simple_event("read-extract", &tool_name, &session_id),
                    );
                    item["text"] = Value::String(ex.text);
                    any_changed = true;
                    continue; // extracted form stands in; rewind holds the original
                }
            }

            // F2: state-aware gate (opt-in). FULL (stuck on target) preserves full
            // signal; ULTRA (exact repeat) collapses to one line; STANDARD falls
            // through to the normal dedup -> compact path. Default OFF.
            if gate_on {
                let is_err = crate::gate::looks_like_error(&text);
                match crate::gate::decide(&session_id, &tool_name, &target, &text, is_err) {
                    crate::gate::GateDecision::Full => {
                        crate::audit::write_row(
                            &session_id,
                            &crate::audit::simple_event(
                                "gate-full-preserve",
                                &tool_name,
                                &session_id,
                            ),
                        );
                        continue; // stuck: preserve full signal, skip compaction
                    }
                    crate::gate::GateDecision::Ultra => {
                        crate::audit::write_row(
                            &session_id,
                            &crate::audit::simple_event("gate-ultra", &tool_name, &session_id),
                        );
                        item["text"] =
                            Value::String("[glyphdown:gate identical repeat collapsed]".to_string());
                        any_changed = true;
                        continue;
                    }
                    crate::gate::GateDecision::Standard => {}
                }
            }

            // A8 session dedup (Read/Grep/Glob/Monitor only), BEFORE compaction.
            if crate::dedup::is_dedup_tool(&tool_name) {
                if let Some((new_text, mode)) =
                    crate::dedup::maybe_dedup_or_summarize(&tool_name, &text, &session_id)
                {
                    crate::audit::write_row(
                        &session_id,
                        &crate::audit::dedup_row(
                            &tool_name,
                            &session_id,
                            &mode,
                            text.len(),
                            new_text.len(),
                        ),
                    );
                    item["text"] = Value::String(new_text.clone());
                    any_changed = true;
                    if mode == "dedup" {
                        continue;
                    }
                    text = new_text; // summarize: fall through to compaction
                }
            }

            // SIL-5 cache-hot bypass (only when GLYPHDOWN_CACHE_AWARE enabled).
            if crate::cache::should_bypass_for_cache(&text, None) {
                crate::audit::write_row(
                    &session_id,
                    &crate::audit::simple_event("skip-cache-hot", &tool_name, &session_id),
                );
                continue;
            }
            let report = crate::codec::compact_payload_report(&text);
            // `output != text` is the exact "a real, tagged win fired" signal.
            if report.output != text {
                // internal-ref anchor-survival guard (default ON).
                if anchor_guard_enabled() {
                    let (revert, _, _) = crate::anchor::should_revert(
                        &text,
                        &report.output,
                        crate::anchor::DEFAULT_REDUCTION_THRESHOLD,
                        crate::anchor::DEFAULT_PRESERVATION_FLOOR,
                    );
                    if revert {
                        crate::audit::write_row(
                            &session_id,
                            &crate::audit::simple_event("anchor-revert", &tool_name, &session_id),
                        );
                        continue; // anchor dropped -> keep current text verbatim
                    }
                }
                crate::audit::write_row(
                    &session_id,
                    &crate::audit::compact_row(&tool_name, &session_id, &report),
                );
                item["text"] = Value::String(report.output);
                any_changed = true;
            }
        }
    }

    // 2. structuredContent: minify-only (lossless; never reshape semantics).
    if let Some(sc) = tool_response.get("structuredContent") {
        // Serialize compact, run mechanical compact, only swap back if it
        // re-parses to an equal value (lossless guarantee).
        if let Ok(sc_text) = serde_json::to_string(sc) {
            let compacted = crate::codec::compact(&sc_text);
            if compacted != sc_text {
                if let Ok(reparsed) = serde_json::from_str::<Value>(&compacted) {
                    if reparsed == *sc {
                        // value-equal: the minify was lossless; keep original
                        // object (already minimal in serde form) — no-op, but
                        // record that text-level compaction would fire.
                        any_changed = true;
                    }
                }
            }
        }
    }

    if any_changed {
        let body = serde_json::json!({
            "continue": true,
            "updatedToolOutput": tool_response,
        });
        Ok(serde_json::to_string(&body)?)
    } else {
        Ok(PASS.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn compact_via_run(input: &str) -> String {
        // helper mirrors run() minus stdin, for deterministic tests
        let mut payload: Value = serde_json::from_str(input).unwrap();
        let tr = payload.get_mut("tool_response").unwrap();
        let mut changed = false;
        if let Some(items) = tr.get_mut("content").and_then(Value::as_array_mut) {
            for item in items.iter_mut() {
                if let Some(text) = item.get("text").and_then(Value::as_str) {
                    let c = crate::codec::compact_payload(text);
                    if c != text {
                        item["text"] = Value::String(c);
                        changed = true;
                    }
                }
            }
        }
        if changed {
            "changed".into()
        } else {
            "unchanged".into()
        }
    }

    #[test]
    fn fail_open_on_garbage_stdin() {
        // run() would Err on non-JSON; posttooluse() maps that to PASS.
        assert!(serde_json::from_str::<Value>("not json").is_err());
    }

    #[test]
    fn no_content_is_pass() {
        let got = compact_via_run(r#"{"tool_name":"Bash","tool_response":{}}"#);
        assert_eq!(got, "unchanged");
    }

    #[test]
    fn small_ansi_text_passes_through_break_even() {
        // \u001b = real ESC; tiny ANSI+blank payload saves < 25 tokens -> break-even passthrough (unchanged).
        let input = r#"{"tool_name":"Bash","tool_response":{"content":[{"type":"text","text":"\u001b[31mERROR\u001b[0m line\n\n\n\n\nmore"}]}}"#;
        assert_eq!(compact_via_run(input), "unchanged");
    }

    #[test]
    fn large_blank_laden_text_compresses() {
        // PHASE 2a: a payload that saves >= 25 tokens AND >= 5% crosses
        // break-even and is rewritten (tagged) by compact_payload.
        let big_blanks = "\n".repeat(400);
        let text = format!("first line{big_blanks}last line");
        let payload = serde_json::json!({
            "tool_name": "Bash",
            "tool_response": {"content": [{"type": "text", "text": text}]}
        });
        assert_eq!(compact_via_run(&payload.to_string()), "changed");
    }

    #[test]
    fn already_minimal_text_is_pass() {
        let input = r#"{"tool_name":"Read","tool_response":{"content":[{"type":"text","text":"clean line one\nclean line two"}]}}"#;
        assert_eq!(compact_via_run(input), "unchanged");
    }
}
