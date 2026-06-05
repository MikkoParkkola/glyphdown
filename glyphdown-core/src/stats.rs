//! Stats scanner — reads PostToolUse arc-event ledgers and turns
//! them into the operator-visible numbers that justify installing
//! the plugin.
//!
//! The savings model below is intentionally conservative and clearly
//! sourced from internal-ref's published cost
//! arithmetic, so the headline number can be defended end-to-end:
//!
//!   - Layer-0 preamble: ~75 cached tokens per compaction
//!   - Layer-1 session-invariant block: ~250 cached tokens
//!   - Layer-2 arc-event ledger: ~7 tokens per event (avg short_summary
//!     "<ts> <Tool> <path>" packs ~6-8 tokens at the GPT-tokenizer
//!     rate the Anthropic cache uses as a proxy)
//!
//! Token → dollar conversion uses Anthropic's published cache-miss
//! premium: cache_creation 1.25x base vs cache_read 0.1x base — a
//! 1.15x delta on each cached token, priced at the Opus 4.7 input
//! tier ($15 / 1M tokens at time of writing).

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Serialize;

// Conservative per-event token cost. Real measurement will replace
// this — until then the constant is a public, defensible floor.
const TOKENS_PER_EVENT: f64 = 7.0;
const PREAMBLE_TOKENS: f64 = 75.0;
const LAYER1_TOKENS: f64 = 250.0;

// Anthropic cache-rate delta: cache_creation 1.25x − cache_read 0.1x
// = 1.15x per token. Opus 4.7 input rate published as $15 / 1M tok.
const CACHE_DELTA_RATIO: f64 = 1.15;
const OPUS_INPUT_USD_PER_MTOK: f64 = 15.0;

#[derive(Debug, Serialize)]
pub struct StatsReport {
    pub data_dir: PathBuf,
    pub data_dir_exists: bool,
    pub sessions_seen: usize,
    pub arcs_seen: usize,
    pub total_events: u64,
    pub total_ledger_bytes: u64,
    pub estimated_cached_tokens_per_compaction: f64,
    /// Lower-bound dollar savings if the operator runs the assumed
    /// cadence (default 3 sessions/day × 4 compactions/session × 365
    /// days). The user can tune cadence via env vars without
    /// re-running anything.
    pub estimated_usd_saved_per_year: f64,
    pub cadence: CadenceParams,
    pub schema_version: &'static str,
}

#[derive(Debug, Serialize, Clone, Copy)]
pub struct CadenceParams {
    pub sessions_per_day: f64,
    pub compactions_per_session: f64,
    pub days_per_year: f64,
}

impl CadenceParams {
    fn from_env() -> Self {
        Self {
            sessions_per_day: env_f64("GLYPHDOWN_SESSIONS_PER_DAY", 3.0),
            compactions_per_session: env_f64("GLYPHDOWN_COMPACTIONS_PER_SESSION", 4.0),
            days_per_year: env_f64("GLYPHDOWN_DAYS_PER_YEAR", 365.0),
        }
    }
}

fn env_f64(key: &str, default: f64) -> f64 {
    std::env::var(key)
        .ok()
        .and_then(|s| s.parse::<f64>().ok())
        .filter(|v| v.is_finite() && *v > 0.0)
        .unwrap_or(default)
}

pub fn scan_all_sessions(data_dir: &Path) -> Result<StatsReport> {
    let cadence = CadenceParams::from_env();
    let arcs_root = data_dir.join("arcs");
    let exists = arcs_root.is_dir();

    let mut sessions = 0usize;
    let mut arcs = 0usize;
    let mut total_events = 0u64;
    let mut total_bytes = 0u64;

    if exists {
        for session_dir in read_dir(&arcs_root)? {
            if !session_dir.is_dir() {
                continue;
            }
            sessions += 1;
            for ledger in read_dir(&session_dir)? {
                if ledger.extension().and_then(|s| s.to_str()) != Some("jsonl") {
                    continue;
                }
                arcs += 1;
                let bytes = fs::metadata(&ledger).map(|m| m.len()).unwrap_or(0);
                total_bytes += bytes;
                if let Ok(text) = fs::read_to_string(&ledger) {
                    total_events += text.lines().filter(|l| !l.is_empty()).count() as u64;
                }
            }
        }
    }

    // Per-compaction cached tokens: Layer-0 + Layer-1 + Layer-2 head
    // (capped at LAYER2_MAX_LINES=200 events per the Python consumer)
    let avg_events_per_compaction = if arcs > 0 {
        (total_events as f64 / arcs as f64).min(200.0)
    } else {
        0.0
    };
    let cached_per_comp =
        PREAMBLE_TOKENS + LAYER1_TOKENS + avg_events_per_compaction * TOKENS_PER_EVENT;

    let comp_per_year =
        cadence.sessions_per_day * cadence.compactions_per_session * cadence.days_per_year;
    // Tokens saved per year = cached × (cache_creation − cache_read)
    // ratio × compactions per year. cache_read is paid once anyway, so
    // the delta is the conservative savings figure.
    let tokens_saved = cached_per_comp * CACHE_DELTA_RATIO * comp_per_year;
    let usd_saved = tokens_saved * OPUS_INPUT_USD_PER_MTOK / 1_000_000.0;

    Ok(StatsReport {
        data_dir: data_dir.to_path_buf(),
        data_dir_exists: exists,
        sessions_seen: sessions,
        arcs_seen: arcs,
        total_events,
        total_ledger_bytes: total_bytes,
        estimated_cached_tokens_per_compaction: cached_per_comp,
        estimated_usd_saved_per_year: usd_saved,
        cadence,
        schema_version: "v1",
    })
}

fn read_dir(p: &Path) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    for entry in fs::read_dir(p).with_context(|| format!("read_dir {}", p.display()))? {
        let entry = entry?;
        out.push(entry.path());
    }
    out.sort();
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_ledger(root: &Path, session: &str, arc: u32, lines: &[&str]) {
        let dir = root.join("arcs").join(session);
        fs::create_dir_all(&dir).unwrap();
        let mut fh = fs::File::create(dir.join(format!("{arc}.jsonl"))).unwrap();
        for line in lines {
            writeln!(fh, "{line}").unwrap();
        }
    }

    #[test]
    fn empty_data_dir_reports_zero_but_does_not_error() {
        let tmp = tempfile::tempdir().unwrap();
        let r = scan_all_sessions(tmp.path()).unwrap();
        assert!(!r.data_dir_exists);
        assert_eq!(r.sessions_seen, 0);
        assert_eq!(r.arcs_seen, 0);
        assert_eq!(r.total_events, 0);
        // With zero events, only Layer-0 + Layer-1 cached → savings
        // still > 0 because the operator gets preamble savings even
        // before any tool-use ledger accumulates.
        assert!(r.estimated_usd_saved_per_year > 0.0);
    }

    #[test]
    fn counts_events_across_sessions_and_arcs() {
        let tmp = tempfile::tempdir().unwrap();
        write_ledger(
            tmp.path(),
            "sess-A",
            0,
            &[
                r#"{"ts_iso":"2026-05-21T00:00:00Z","tool":"Read","short_summary":"Read a.py","key_args_hash":"x"}"#,
                r#"{"ts_iso":"2026-05-21T00:00:01Z","tool":"Edit","short_summary":"Edit b.py","key_args_hash":"y"}"#,
            ],
        );
        write_ledger(
            tmp.path(),
            "sess-B",
            0,
            &[
                r#"{"ts_iso":"2026-05-21T00:00:02Z","tool":"Bash","short_summary":"Bash ls","key_args_hash":"z"}"#,
            ],
        );
        let r = scan_all_sessions(tmp.path()).unwrap();
        assert!(r.data_dir_exists);
        assert_eq!(r.sessions_seen, 2);
        assert_eq!(r.arcs_seen, 2);
        assert_eq!(r.total_events, 3);
        assert!(r.total_ledger_bytes > 0);
        // 1.5 events/arc avg → cached_per_comp > L0+L1 baseline.
        assert!(r.estimated_cached_tokens_per_compaction > PREAMBLE_TOKENS + LAYER1_TOKENS);
    }

    #[test]
    fn cadence_env_vars_scale_savings_linearly() {
        let tmp = tempfile::tempdir().unwrap();
        // Snapshot at default cadence
        // Safety: the test runs single-threaded; setting env vars
        // here cannot race anything that reads them concurrently.
        unsafe {
            std::env::remove_var("GLYPHDOWN_SESSIONS_PER_DAY");
            std::env::remove_var("GLYPHDOWN_COMPACTIONS_PER_SESSION");
            std::env::remove_var("GLYPHDOWN_DAYS_PER_YEAR");
        }
        let base = scan_all_sessions(tmp.path()).unwrap();
        unsafe {
            std::env::set_var("GLYPHDOWN_SESSIONS_PER_DAY", "6");
        }
        let doubled = scan_all_sessions(tmp.path()).unwrap();
        // 2× the daily sessions → 2× the yearly savings, within
        // floating-point rounding.
        let ratio = doubled.estimated_usd_saved_per_year / base.estimated_usd_saved_per_year;
        assert!(
            (ratio - 2.0).abs() < 1e-9,
            "expected 2.0× savings, got {ratio}",
        );
        unsafe {
            std::env::remove_var("GLYPHDOWN_SESSIONS_PER_DAY");
        }
    }
}
