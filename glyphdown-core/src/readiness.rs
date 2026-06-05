//! readiness — F1/F2 default-on readiness from the audit log.
//!
//! F1 (read section-extraction) and F2 (state-aware gate) ship opt-in
//! (`GLYPHDOWN_READ_EXTRACT`, `GLYPHDOWN_GATE`). The decision to flip either to
//! default-on is DATA-gated, not a guess: run them on real traffic, then read the
//! audit events here. F1 is safe to default-on when the agent rarely has to
//! retrieve the content extraction dropped (retrieval rate below the floor); F2 is
//! informational (how often ULTRA/FULL fire). Pure tally over audit `event` names.

#[derive(Default, Debug, PartialEq, Eq)]
pub struct Readiness {
    pub read_extracts: u64,
    pub rewind_retrieves: u64,
    pub gate_ultra: u64,
    pub gate_full: u64,
    pub compact_events: u64,
    pub total_rows: u64,
}

/// Verdict for the F1 default-on flip.
#[derive(Debug, PartialEq, Eq)]
pub enum Verdict {
    /// Not enough extraction samples yet — keep gathering.
    NeedMoreData,
    /// Retrieval rate below the floor — safe to default-on.
    Ready,
    /// Retrieval rate too high — loosen the keep-ratio before default-on.
    TuneFirst,
}

/// Default-on is safe only with enough samples AND a low retrieval rate.
const F1_MIN_SAMPLES: u64 = 50;
const F1_RETRIEVAL_FLOOR: f64 = 0.15;

impl Readiness {
    /// Fraction of extracted reads the agent had to retrieve the original for.
    /// 0.0 when nothing was extracted.
    pub fn f1_retrieval_rate(&self) -> f64 {
        if self.read_extracts == 0 {
            0.0
        } else {
            self.rewind_retrieves as f64 / self.read_extracts as f64
        }
    }

    pub fn f1_verdict(&self) -> Verdict {
        if self.read_extracts < F1_MIN_SAMPLES {
            Verdict::NeedMoreData
        } else if self.f1_retrieval_rate() < F1_RETRIEVAL_FLOOR {
            Verdict::Ready
        } else {
            Verdict::TuneFirst
        }
    }

    /// Fraction of audit rows where the gate fired (ULTRA or FULL).
    pub fn f2_fire_rate(&self) -> f64 {
        if self.total_rows == 0 {
            0.0
        } else {
            (self.gate_ultra + self.gate_full) as f64 / self.total_rows as f64
        }
    }

    /// Tally a single audit `event` name.
    pub fn add_event(&mut self, event: &str) {
        self.total_rows += 1;
        match event {
            "read-extract" => self.read_extracts += 1,
            "rewind-retrieve" => self.rewind_retrieves += 1,
            "gate-ultra" => self.gate_ultra += 1,
            "gate-full-preserve" => self.gate_full += 1,
            "compact" => self.compact_events += 1,
            _ => {}
        }
    }

    /// Build from the audit log text (one JSON object per line).
    pub fn from_audit(text: &str) -> Self {
        let mut r = Readiness::default();
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(line) {
                if let Some(ev) = v.get("event").and_then(|x| x.as_str()) {
                    r.add_event(ev);
                }
            }
        }
        r
    }

    /// Human-readable readiness report.
    pub fn render(&self) -> String {
        let v = match self.f1_verdict() {
            Verdict::NeedMoreData => format!(
                "NEED MORE DATA ({} extracts < {} samples)",
                self.read_extracts, F1_MIN_SAMPLES
            ),
            Verdict::Ready => "READY to default-on (retrieval rate below floor)".to_string(),
            Verdict::TuneFirst => {
                "TUNE FIRST (retrieval rate >= 15%; loosen keep-ratio)".to_string()
            }
        };
        format!(
            "glyphdown feature readiness (from {} audit rows)\n\
             F1 read-extraction:\n\
             \x20 extracts        : {}\n\
             \x20 retrieves       : {}\n\
             \x20 retrieval rate  : {:.1}% (floor {:.0}%)\n\
             \x20 default-on       : {v}\n\
             F2 state-aware gate:\n\
             \x20 ULTRA collapses : {}\n\
             \x20 FULL preserves  : {}\n\
             \x20 fire rate       : {:.1}% of rows\n\
             baseline compactions: {}",
            self.total_rows,
            self.read_extracts,
            self.rewind_retrieves,
            self.f1_retrieval_rate() * 100.0,
            F1_RETRIEVAL_FLOOR * 100.0,
            self.gate_ultra,
            self.gate_full,
            self.f2_fire_rate() * 100.0,
            self.compact_events,
        )
    }
}

/// Read the global audit log and build the readiness report. Empty when absent.
pub fn from_data_dir() -> Readiness {
    let path = crate::data_dir::resolve().join("audit.jsonl");
    match std::fs::read_to_string(&path) {
        Ok(text) => Readiness::from_audit(&text),
        Err(_) => Readiness::default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tallies_events_from_audit_lines() {
        let log = concat!(
            r#"{"event":"read-extract","tool":"Read"}"#,
            "\n",
            r#"{"event":"read-extract","tool":"Read"}"#,
            "\n",
            r#"{"event":"rewind-retrieve","tool":"retrieve"}"#,
            "\n",
            r#"{"event":"gate-ultra","tool":"Bash"}"#,
            "\n",
            r#"{"event":"gate-full-preserve","tool":"Bash"}"#,
            "\n",
            r#"{"event":"compact","tool":"Read"}"#,
            "\n",
            "garbage line that is not json\n",
        );
        let r = Readiness::from_audit(log);
        assert_eq!(r.read_extracts, 2);
        assert_eq!(r.rewind_retrieves, 1);
        assert_eq!(r.gate_ultra, 1);
        assert_eq!(r.gate_full, 1);
        assert_eq!(r.compact_events, 1);
        // garbage line is skipped, not counted
        assert_eq!(r.total_rows, 6);
    }

    #[test]
    fn f1_retrieval_rate_and_verdict() {
        let mut r = Readiness {
            read_extracts: 100,
            rewind_retrieves: 8,
            ..Default::default()
        };
        assert!((r.f1_retrieval_rate() - 0.08).abs() < 1e-9);
        assert_eq!(r.f1_verdict(), Verdict::Ready); // 8% < 15%, 100 >= 50

        r.rewind_retrieves = 30; // 30%
        assert_eq!(r.f1_verdict(), Verdict::TuneFirst);

        r.read_extracts = 10; // too few samples
        assert_eq!(r.f1_verdict(), Verdict::NeedMoreData);
    }

    #[test]
    fn empty_audit_is_zero_not_panic() {
        let r = Readiness::from_audit("");
        assert_eq!(r, Readiness::default());
        assert_eq!(r.f1_retrieval_rate(), 0.0);
        assert_eq!(r.f2_fire_rate(), 0.0);
        assert_eq!(r.f1_verdict(), Verdict::NeedMoreData);
    }

    #[test]
    fn render_contains_verdict_and_rates() {
        let r = Readiness {
            read_extracts: 80,
            rewind_retrieves: 4,
            gate_ultra: 12,
            gate_full: 3,
            total_rows: 200,
            compact_events: 150,
        };
        let out = r.render();
        assert!(out.contains("READY to default-on"));
        assert!(out.contains("retrieval rate  : 5.0%"));
    }
}
