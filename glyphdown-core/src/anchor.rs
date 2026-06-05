//! anchor — preservation-floor guard (internal-ref), Rust port of anchor_guard.py.
//!
//! When codec output drops >= REDUCTION_THRESHOLD of original CHARACTERS,
//! verify high-value "anchor" strings (file:line, error codes, test verdicts,
//! panics, exit/signal) survive at >= PRESERVATION_FLOOR. If not, the caller
//! reverts the compression. This is the guard that runs in PRODUCTION by
//! default (ANCHOR_GUARD_ENABLED defaults true) and it interacts with the
//! `truncate_with_marker` transform shipped in phase 2a — a huge anchor-bearing
//! payload that truncates below the floor MUST be reverted, exactly as python
//! does, or the rust path silently loses the one error code that mattered.
//!
//! Parity with python is validated empirically by
//! bench/equiv_guards_rust_vs_python.py (anchor-revert subcommand), NOT by
//! inspection — `regex-lite` vs python `re` divergence would be caught there.

use std::sync::OnceLock;

use regex_lite::Regex;

pub const DEFAULT_REDUCTION_THRESHOLD: f64 = 0.90;
pub const DEFAULT_PRESERVATION_FLOOR: f64 = 0.70;

/// The six anchor patterns, compiled once per process. Order is irrelevant
/// (results union into a set), but kept identical to python for clarity.
fn patterns() -> &'static [Regex] {
    static PATS: OnceLock<Vec<Regex>> = OnceLock::new();
    PATS.get_or_init(|| {
        [
            // file:line  e.g. src/main.rs:42, lib/foo.py:128:8, ./tests/a.ts:7
            r"(?:[A-Za-z0-9_./\-]+/)?[A-Za-z0-9_\-]+\.[A-Za-z0-9]{1,5}:\d+(?::\d+)?",
            // Rust compiler errors: error[E0308]
            r"error\[E\d{3,5}\]",
            // TS/JS compiler errors: TS2345
            r"\bTS\d{3,5}\b",
            // Test verdicts: 12 passed, 3 failed, 1 error, 2 skipped
            r"(?i)\b\d+\s+(?:passed|failed|errored?|skipped)\b",
            // Bare UPPERCASE per-test verdicts: pytest `test_x FAILED`, cargo
            // `test foo ... FAILED`. Uppercase only — `passed`/`ok` in prose are
            // too noisy, but ALL-CAPS verdicts are near-exclusive to test output,
            // so this catches the mid-line per-test verdict the counted pattern
            // misses (it requires a leading number) without backfiring on prose.
            r"\b(?:PASSED|FAILED|ERRORED?|SKIPPED)\b",
            // panic / FAIL / FATAL markers (line-anchored)
            r"(?m)^(?:thread\s+'\w+'\s+panicked|FAIL(?:URE)?\b|FATAL\b|PANIC\b)",
            // exit/abort codes
            r"(?i)\b(?:exit\s+status|signal|SIG(?:KILL|TERM|SEGV|ABRT))\s+\d+\b",
        ]
        .iter()
        .filter_map(|p| Regex::new(p).ok())
        .collect()
    })
}

/// Distinct anchor strings found in `text` (group-0 matches, deduplicated).
pub fn extract_anchors(text: &str) -> std::collections::HashSet<String> {
    let mut set = std::collections::HashSet::new();
    for re in patterns() {
        for m in re.find_iter(text) {
            set.insert(m.as_str().to_string());
        }
    }
    set
}

/// Fraction of `original` anchors that still appear verbatim in `compact`.
/// 1.0 when original has no anchors (vacuous — nothing to preserve).
pub fn survival_ratio(original: &str, compact: &str) -> f64 {
    let orig = extract_anchors(original);
    if orig.is_empty() {
        return 1.0;
    }
    let comp = extract_anchors(compact);
    let surviving = orig.intersection(&comp).count();
    surviving as f64 / orig.len() as f64
}

/// python `should_revert`. Returns (revert, reduction, survival).
/// Lengths are CHARACTER counts (python `len`), not tokens.
pub fn should_revert(
    original: &str,
    compact: &str,
    reduction_threshold: f64,
    preservation_floor: f64,
) -> (bool, f64, f64) {
    let orig_len = original.chars().count();
    if orig_len == 0 {
        return (false, 0.0, 1.0);
    }
    let reduction = 1.0 - (compact.chars().count() as f64 / orig_len as f64);
    if reduction < reduction_threshold {
        return (false, reduction, 1.0);
    }
    let survival = survival_ratio(original, compact);
    (survival < preservation_floor, reduction, survival)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_anchors_never_reverts() {
        let orig = "x".repeat(1000);
        let compact = "x".repeat(10); // 99% reduction, but no anchors
        let (revert, _, surv) = should_revert(&orig, &compact, 0.90, 0.70);
        assert!(!revert);
        assert_eq!(surv, 1.0);
    }

    #[test]
    fn small_compression_never_reverts() {
        let orig = format!("error[E0308] {}", "x".repeat(100));
        let compact = "y".repeat(90); // dropped the anchor but only ~20% reduction
        let (revert, _, _) = should_revert(&orig, &compact, 0.90, 0.70);
        assert!(!revert); // not aggressive enough to check
    }

    #[test]
    fn aggressive_drop_of_anchor_reverts() {
        // big payload carrying an error code; truncated to a tiny tail that
        // drops the anchor -> reduction >= 90% AND survival 0 -> revert.
        let orig = format!(
            "error[E0432]: unresolved import\n{}",
            "noise line\n".repeat(500)
        );
        let compact = "noise line\nnoise line\n[truncated: 1 lines / 999 bytes hidden]".to_string();
        let (revert, reduction, survival) = should_revert(&orig, &compact, 0.90, 0.70);
        assert!(reduction >= 0.90, "reduction={reduction}");
        assert_eq!(survival, 0.0);
        assert!(revert);
    }

    #[test]
    fn aggressive_but_anchor_survives_no_revert() {
        let orig = format!("src/main.rs:42:7 error[E0308]\n{}", "noise\n".repeat(500));
        // compact keeps the anchors despite heavy reduction
        let compact = "src/main.rs:42:7 error[E0308]\n[truncated]".to_string();
        let (revert, reduction, survival) = should_revert(&orig, &compact, 0.90, 0.70);
        assert!(reduction >= 0.90);
        assert_eq!(survival, 1.0);
        assert!(!revert);
    }

    #[test]
    fn extracts_common_anchors() {
        let a = extract_anchors("see src/lib.rs:128:8 and error[E0432] plus 12 passed, 3 failed");
        assert!(a.contains("src/lib.rs:128:8"));
        assert!(a.contains("error[E0432]"));
        assert!(a.contains("12 passed"));
        assert!(a.contains("3 failed"));
    }

    #[test]
    fn extracts_bare_uppercase_test_verdicts() {
        // pytest / cargo emit mid-line UPPERCASE per-test verdicts with no leading
        // count — the counted pattern misses these; the uppercase pattern catches
        // them so extract never drops the "which test failed" line.
        let a = extract_anchors("tests/test_pay.py::test_refund FAILED");
        assert!(a.contains("FAILED"), "pytest mid-line FAILED must anchor");
        let b = extract_anchors("test result: ok. 12 PASSED; 0 SKIPPED");
        assert!(b.contains("PASSED"));
        assert!(b.contains("SKIPPED"));
    }

    #[test]
    fn lowercase_verdict_words_in_prose_do_not_anchor() {
        // The bias is deliberate: ALL-CAPS only. Prose like "the test passed and
        // everything is ok" must NOT anchor, or extraction would never collapse
        // ordinary text (the lowercase-substring backfire the project already learned).
        let a = extract_anchors("the migration passed review and the build looks ok to me now");
        assert!(a.is_empty(), "lowercase prose verdicts must not anchor: {a:?}");
    }
}
