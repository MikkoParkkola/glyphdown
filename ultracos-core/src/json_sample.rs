//! json_sample — F3 (internal-ref): uniform JSON-array sampling, Rewind-backed.
//!
//! Large homogeneous JSON arrays in tool output (cargo metadata, issue lists,
//! search hits) can be sampled — keep a head + tail + every anomaly + the schema,
//! drop the uniform middle — and the full original is stashed in the `rewind`
//! store so it is recoverable by id. Lossy-BUT-recoverable.
//!
//! The hard lesson the fail-fast taught (see DESIGN-0.5.0 §F3): real agent arrays
//! are often NON-uniform (cargo packages vary wildly in size) and naive sampling
//! BACKFIRES. So this gates on uniformity first (consistent schema + low size
//! variance) and DECLINES otherwise, and detects anomalies STRUCTURALLY (explicit
//! failure-status field or schema deviation) — never by substring, which flagged
//! every real record. Opt-in (`ULTRACOS_JSON_SAMPLE`), default OFF.

use std::collections::BTreeSet;

use serde_json::Value;

const MIN_ARRAY: usize = 8;
const KEEP_HEAD: usize = 3;
const KEEP_TAIL: usize = 2;
/// Gate thresholds (validated on real gh/cargo output).
const MIN_SCHEMA_CONSISTENCY: f64 = 0.8;
const MAX_SIZE_CV: f64 = 0.6;

pub struct Sampled {
    pub text: String,
    pub rewind_id: String,
    pub original_tokens: i64,
    pub sampled_tokens: i64,
    pub array_len: usize,
    pub anomalies_kept: usize,
}

fn enabled() -> bool {
    matches!(
        std::env::var("ULTRACOS_JSON_SAMPLE")
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase()
            .as_str(),
        "1" | "true" | "yes" | "on"
    )
}

/// The biggest homogeneous (all-object) array, length >= MIN_ARRAY, anywhere in
/// the value, with the JSON-pointer-ish path that locates it.
fn biggest_array(v: &Value) -> Option<Vec<Value>> {
    let mut best: Option<&Vec<Value>> = None;
    fn walk<'a>(v: &'a Value, best: &mut Option<&'a Vec<Value>>) {
        match v {
            Value::Array(a) => {
                if a.len() >= MIN_ARRAY && a.iter().all(|x| x.is_object()) {
                    if best.map(|b| a.len() > b.len()).unwrap_or(true) {
                        *best = Some(a);
                    }
                }
                for x in a {
                    walk(x, best);
                }
            }
            Value::Object(o) => {
                for x in o.values() {
                    walk(x, best);
                }
            }
            _ => {}
        }
    }
    walk(v, &mut best);
    best.cloned()
}

fn key_set(rec: &Value) -> BTreeSet<String> {
    rec.as_object()
        .map(|o| o.keys().cloned().collect())
        .unwrap_or_default()
}

/// (schema_consistency, size_cv, modal_keys).
fn uniformity(arr: &[Value]) -> (f64, f64, BTreeSet<String>) {
    use std::collections::HashMap;
    let mut counts: HashMap<BTreeSet<String>, usize> = HashMap::new();
    for r in arr {
        *counts.entry(key_set(r)).or_insert(0) += 1;
    }
    let (modal, modal_n) = counts
        .into_iter()
        .max_by_key(|(_, n)| *n)
        .unwrap_or((BTreeSet::new(), 0));
    let schema_consistency = modal_n as f64 / arr.len().max(1) as f64;
    let sizes: Vec<f64> = arr.iter().map(|r| r.to_string().len() as f64).collect();
    let mean = sizes.iter().sum::<f64>() / sizes.len().max(1) as f64;
    let var = sizes.iter().map(|s| (s - mean).powi(2)).sum::<f64>() / sizes.len().max(1) as f64;
    let cv = if mean > 0.0 { var.sqrt() / mean } else { 0.0 };
    (schema_consistency, cv, modal)
}

/// STRUCTURAL anomaly: an explicit failure-status field, OR a key-set deviating
/// from the modal schema. Never substring — that backfires on real data.
fn is_anomalous(rec: &Value, modal: &BTreeSet<String>) -> bool {
    if let Some(o) = rec.as_object() {
        for k in ["status", "state", "result", "outcome", "conclusion"] {
            if let Some(Value::String(s)) = o.get(k) {
                if matches!(
                    s.to_ascii_lowercase().as_str(),
                    "failed" | "failure" | "error" | "fail" | "broken" | "timed_out"
                ) {
                    return true;
                }
            }
        }
    }
    &key_set(rec) != modal
}

/// Sample the biggest uniform array in `text` (must be valid JSON). Stashes the
/// original to rewind. None when disabled, not JSON, no uniform array, non-uniform
/// (gate declines), or it would not save tokens.
pub fn sample_json(session: &str, text: &str) -> Option<Sampled> {
    if !enabled() {
        return None;
    }
    let v: Value = serde_json::from_str(text).ok()?;
    let arr = biggest_array(&v)?;
    let (sc, cv, modal) = uniformity(&arr);
    if sc < MIN_SCHEMA_CONSISTENCY || cv > MAX_SIZE_CV {
        return None; // non-uniform: declining is what stops the backfire
    }
    let anomalies: Vec<&Value> = arr.iter().filter(|r| is_anomalous(r, &modal)).collect();
    let mut kept: Vec<Value> = Vec::new();
    kept.extend(arr.iter().take(KEEP_HEAD).cloned());
    kept.extend(anomalies.iter().map(|r| (*r).clone()));
    if arr.len() > KEEP_TAIL {
        kept.extend(arr[arr.len() - KEEP_TAIL..].iter().cloned());
    }
    let id = rewind::stash(session, text)?;
    let schema: Vec<String> = modal.iter().cloned().collect();
    let marker = serde_json::json!({
        "__ultracos_sampled__": {
            "total": arr.len(),
            "kept": kept.len(),
            "schema": schema,
            "anomalies_preserved": anomalies.len(),
            "retrieve": format!("ultracos-core retrieve {session} {id}"),
        }
    });
    let mut out_arr = kept;
    out_arr.push(marker);
    let sampled_text = serde_json::to_string(&Value::Array(out_arr)).ok()?;

    let original_tokens = crate::codec::estimate_tokens(text);
    let sampled_tokens = crate::codec::estimate_tokens(&sampled_text);
    if sampled_tokens >= original_tokens {
        return None; // marker overhead exceeded the drop — pass through
    }
    Some(Sampled {
        text: sampled_text,
        rewind_id: id,
        original_tokens,
        sampled_tokens,
        array_len: arr.len(),
        anomalies_kept: anomalies.len(),
    })
}

use crate::rewind;

#[cfg(test)]
mod tests {
    use super::*;

    fn on(name: &str) {
        let dir = std::env::temp_dir().join(format!("ultracos-jsonsample-test-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // SAFETY: serialized via the shared lock below.
        unsafe {
            std::env::set_var("ULTRACOS_REWIND_DIR", &dir);
            std::env::set_var("ULTRACOS_JSON_SAMPLE", "1");
        }
    }
    fn off() {
        unsafe {
            std::env::remove_var("ULTRACOS_REWIND_DIR");
            std::env::remove_var("ULTRACOS_JSON_SAMPLE");
        }
    }

    fn uniform(n: usize) -> String {
        let arr: Vec<Value> = (0..n)
            .map(|i| serde_json::json!({"id": i, "name": format!("item_{i}"), "status": "ok"}))
            .collect();
        serde_json::to_string(&arr).unwrap()
    }

    #[test]
    fn samples_uniform_array_reversibly() {
        let _g = crate::rewind::TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        on("uniform");
        let text = uniform(200);
        let s = sample_json("s", &text).expect("uniform array should sample");
        assert!(s.sampled_tokens < s.original_tokens, "must save tokens");
        assert_eq!(s.array_len, 200);
        // FULL original recoverable byte-for-byte from rewind
        assert_eq!(
            rewind::retrieve("s", &s.rewind_id, None).as_deref(),
            Some(text.as_str())
        );
        off();
    }

    #[test]
    fn off_by_default() {
        let _g = crate::rewind::TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        // env not set -> None even on a perfect array
        unsafe { std::env::remove_var("ULTRACOS_JSON_SAMPLE") };
        assert!(sample_json("s", &uniform(50)).is_none());
    }

    #[test]
    fn declines_non_uniform_array() {
        let _g = crate::rewind::TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        on("nonuniform");
        // a few huge records among many tiny -> high CV -> gate declines (this is
        // exactly the cargo-metadata backfire the uniformity gate prevents).
        let arr: Vec<Value> = (0..20)
            .map(|i| {
                let pad = if i % 5 == 0 {
                    "x".repeat(2000)
                } else {
                    "y".repeat(10)
                };
                serde_json::json!({"id": i, "blob": pad})
            })
            .collect();
        let text = serde_json::to_string(&arr).unwrap();
        assert!(
            sample_json("s", &text).is_none(),
            "non-uniform must decline"
        );
        off();
    }

    #[test]
    fn preserves_anomalies() {
        let _g = crate::rewind::TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        on("anom");
        let arr: Vec<Value> = (0..100)
            .map(|i| {
                if i == 57 {
                    serde_json::json!({"id": i, "name": format!("t{i}"), "status": "failed"})
                } else {
                    serde_json::json!({"id": i, "name": format!("t{i}"), "status": "ok"})
                }
            })
            .collect();
        let text = serde_json::to_string(&arr).unwrap();
        let s = sample_json("s", &text).unwrap();
        assert!(s.anomalies_kept >= 1, "the failed record must be preserved");
        assert!(s.text.contains("\"failed\""), "anomaly survives in output");
        off();
    }
}
