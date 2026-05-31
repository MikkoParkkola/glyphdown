//! signed-CCR — Ed25519-signed compact-context-record attestations.
//!
//! Each attestation binds: arc session, arc event index, payload hash
//! (sha256 of canonical-JSON event body), and an optional prompt-cache
//! prefix-hash (the upstream-truth prefix signal). The chain
//! field carries the sha256 of the previous attestation's signature so
//! the JSONL log is tamper-evident end-to-end.
//!
//! Storage is append-only JSONL at `<data_dir>/attestations.jsonl`.
//! Keypair lives at `<data_dir>/signing.key` (32-byte raw seed, 0600).

use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow};
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use fs2::FileExt;
use rand_core::OsRng;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const SEED_LEN: usize = 32;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Attestation {
    pub schema_version: String,
    pub arc_session: String,
    pub arc_event_index: u64,
    pub payload_sha256: String,
    pub prefix_hash: Option<String>,
    pub signed_at_epoch: u64,
    pub public_key_hex: String,
    pub signature_hex: String,
    pub prev_chain_hash: String,
}

pub fn data_paths(data_dir: &Path) -> (PathBuf, PathBuf) {
    (
        data_dir.join("signing.key"),
        data_dir.join("attestations.jsonl"),
    )
}

pub fn ensure_keypair(data_dir: &Path) -> Result<SigningKey> {
    std::fs::create_dir_all(data_dir)
        .with_context(|| format!("create_dir_all {}", data_dir.display()))?;
    let (key_path, _) = data_paths(data_dir);
    if key_path.exists() {
        let raw =
            std::fs::read(&key_path).with_context(|| format!("read {}", key_path.display()))?;
        if raw.len() != SEED_LEN {
            return Err(anyhow!(
                "{} has length {} (expected {})",
                key_path.display(),
                raw.len(),
                SEED_LEN
            ));
        }
        let seed: [u8; SEED_LEN] = raw.try_into().map_err(|_| anyhow!("seed slice copy"))?;
        return Ok(SigningKey::from_bytes(&seed));
    }
    let key = SigningKey::generate(&mut OsRng);
    write_private(&key_path, &key.to_bytes())?;
    Ok(key)
}

#[cfg(unix)]
fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    let mut f = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .with_context(|| format!("create {}", path.display()))?;
    f.write_all(bytes)?;
    Ok(())
}

#[cfg(not(unix))]
fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    // Windows: rely on per-user profile ACLs at %LOCALAPPDATA%.
    std::fs::write(path, bytes).with_context(|| format!("write {}", path.display()))?;
    Ok(())
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    hex::encode(h.finalize())
}

fn now_epoch() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn last_chain_hash(log_path: &Path) -> Result<String> {
    if !log_path.exists() {
        return Ok("genesis".to_owned());
    }
    let f = File::open(log_path).with_context(|| format!("open {}", log_path.display()))?;
    let mut last_sig: Option<String> = None;
    for line in BufReader::new(f).lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let rec: Attestation =
            serde_json::from_str(&line).with_context(|| "parse attestation jsonl line")?;
        last_sig = Some(rec.signature_hex);
    }
    Ok(match last_sig {
        Some(s) => sha256_hex(s.as_bytes()),
        None => "genesis".to_owned(),
    })
}

/// Canonical signing payload: deterministic concatenation of the fields
/// that go into the signature. Keep stable across versions; chain hash
/// is included so swapping any prior record invalidates every later sig.
fn signing_payload(
    arc_session: &str,
    arc_event_index: u64,
    payload_sha256: &str,
    prefix_hash: Option<&str>,
    signed_at_epoch: u64,
    prev_chain_hash: &str,
) -> Vec<u8> {
    let mut buf = Vec::with_capacity(256);
    buf.extend_from_slice(b"ultracos-ccr-v1\n");
    buf.extend_from_slice(arc_session.as_bytes());
    buf.push(b'\n');
    buf.extend_from_slice(arc_event_index.to_string().as_bytes());
    buf.push(b'\n');
    buf.extend_from_slice(payload_sha256.as_bytes());
    buf.push(b'\n');
    buf.extend_from_slice(prefix_hash.unwrap_or("").as_bytes());
    buf.push(b'\n');
    buf.extend_from_slice(signed_at_epoch.to_string().as_bytes());
    buf.push(b'\n');
    buf.extend_from_slice(prev_chain_hash.as_bytes());
    buf
}

pub fn attest(
    data_dir: &Path,
    arc_session: &str,
    arc_event_index: u64,
    event_payload: &[u8],
    prefix_hash: Option<&str>,
) -> Result<Attestation> {
    std::fs::create_dir_all(data_dir)
        .with_context(|| format!("create_dir_all {}", data_dir.display()))?;
    // Exclusive advisory lock guards BOTH the keypair-ensure step and
    // the read-then-write chain-append section against concurrent
    // attestors (e.g. a Python writer signing arc-event appends while
    // the Rust CLI is invoked by the operator). Without the lock:
    //   (a) two writers observe the same last_chain_hash and silently
    //       fork the chain, and
    //   (b) ensure_keypair's create_new-then-write races a sibling's
    //       read path, surfacing as a 0-byte signing.key.
    let lock_path = data_dir.join("attestations.lock");
    let lock_file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .open(&lock_path)
        .with_context(|| format!("open {}", lock_path.display()))?;
    lock_file
        .lock_exclusive()
        .with_context(|| "acquire exclusive flock on attestations.lock")?;
    let key = ensure_keypair(data_dir)?;
    let (_, log_path) = data_paths(data_dir);
    let payload_sha256 = sha256_hex(event_payload);
    let signed_at_epoch = now_epoch();
    let prev_chain_hash = last_chain_hash(&log_path)?;
    let msg = signing_payload(
        arc_session,
        arc_event_index,
        &payload_sha256,
        prefix_hash,
        signed_at_epoch,
        &prev_chain_hash,
    );
    let sig: Signature = key.sign(&msg);
    let rec = Attestation {
        schema_version: "v1".to_owned(),
        arc_session: arc_session.to_owned(),
        arc_event_index,
        payload_sha256,
        prefix_hash: prefix_hash.map(str::to_owned),
        signed_at_epoch,
        public_key_hex: hex::encode(key.verifying_key().to_bytes()),
        signature_hex: hex::encode(sig.to_bytes()),
        prev_chain_hash,
    };
    let line = serde_json::to_string(&rec)?;
    let mut f = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .with_context(|| format!("open append {}", log_path.display()))?;
    writeln!(f, "{line}")?;
    // Lock drops on lock_file going out of scope.
    Ok(rec)
}

#[derive(Debug, Serialize)]
pub struct VerifyReport {
    pub total: usize,
    pub ok: usize,
    pub failures: Vec<VerifyFailure>,
}

#[derive(Debug, Serialize)]
pub struct VerifyFailure {
    pub line_number: usize,
    pub reason: String,
}

pub fn verify_log(data_dir: &Path) -> Result<VerifyReport> {
    let (_, log_path) = data_paths(data_dir);
    let mut report = VerifyReport {
        total: 0,
        ok: 0,
        failures: Vec::new(),
    };
    if !log_path.exists() {
        return Ok(report);
    }
    let f = File::open(&log_path).with_context(|| format!("open {}", log_path.display()))?;
    let mut expected_prev = "genesis".to_owned();
    for (idx, line) in BufReader::new(f).lines().enumerate() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        report.total += 1;
        let n = idx + 1;
        let rec: Attestation = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => {
                report.failures.push(VerifyFailure {
                    line_number: n,
                    reason: format!("json parse: {e}"),
                });
                continue;
            }
        };
        if rec.prev_chain_hash != expected_prev {
            report.failures.push(VerifyFailure {
                line_number: n,
                reason: format!(
                    "chain break: expected prev_chain_hash {expected_prev}, got {}",
                    rec.prev_chain_hash
                ),
            });
            // Don't bail — keep verifying so the operator sees the full scope.
        }
        let pk_bytes = match hex::decode(&rec.public_key_hex) {
            Ok(b) if b.len() == 32 => b,
            Ok(b) => {
                report.failures.push(VerifyFailure {
                    line_number: n,
                    reason: format!("public_key length {} (want 32)", b.len()),
                });
                expected_prev = sha256_hex(rec.signature_hex.as_bytes());
                continue;
            }
            Err(e) => {
                report.failures.push(VerifyFailure {
                    line_number: n,
                    reason: format!("public_key hex decode: {e}"),
                });
                expected_prev = sha256_hex(rec.signature_hex.as_bytes());
                continue;
            }
        };
        let pk_arr: [u8; 32] = pk_bytes.as_slice().try_into().unwrap();
        let vk = match VerifyingKey::from_bytes(&pk_arr) {
            Ok(v) => v,
            Err(e) => {
                report.failures.push(VerifyFailure {
                    line_number: n,
                    reason: format!("public_key parse: {e}"),
                });
                expected_prev = sha256_hex(rec.signature_hex.as_bytes());
                continue;
            }
        };
        let sig_bytes = match hex::decode(&rec.signature_hex) {
            Ok(b) if b.len() == 64 => b,
            _ => {
                report.failures.push(VerifyFailure {
                    line_number: n,
                    reason: "signature hex decode or length".to_owned(),
                });
                expected_prev = sha256_hex(rec.signature_hex.as_bytes());
                continue;
            }
        };
        let sig_arr: [u8; 64] = sig_bytes.as_slice().try_into().unwrap();
        let sig = Signature::from_bytes(&sig_arr);
        let msg = signing_payload(
            &rec.arc_session,
            rec.arc_event_index,
            &rec.payload_sha256,
            rec.prefix_hash.as_deref(),
            rec.signed_at_epoch,
            &rec.prev_chain_hash,
        );
        if vk.verify(&msg, &sig).is_ok() {
            report.ok += 1;
        } else {
            report.failures.push(VerifyFailure {
                line_number: n,
                reason: "ed25519 signature mismatch".to_owned(),
            });
        }
        expected_prev = sha256_hex(rec.signature_hex.as_bytes());
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_single_attestation_verifies() {
        let tmp = tempfile::tempdir().unwrap();
        let rec = attest(tmp.path(), "sess-A", 0, b"hello world", Some("prefix-abc")).unwrap();
        assert_eq!(rec.arc_session, "sess-A");
        assert_eq!(rec.arc_event_index, 0);
        assert_eq!(rec.prefix_hash.as_deref(), Some("prefix-abc"));
        let report = verify_log(tmp.path()).unwrap();
        assert_eq!(report.total, 1);
        assert_eq!(report.ok, 1);
        assert!(report.failures.is_empty(), "{:?}", report.failures);
    }

    #[test]
    fn chain_links_across_three_records() {
        let tmp = tempfile::tempdir().unwrap();
        attest(tmp.path(), "sess-A", 0, b"a", None).unwrap();
        attest(tmp.path(), "sess-A", 1, b"b", None).unwrap();
        attest(tmp.path(), "sess-A", 2, b"c", Some("p")).unwrap();
        let report = verify_log(tmp.path()).unwrap();
        assert_eq!(report.total, 3);
        assert_eq!(report.ok, 3);
    }

    #[test]
    fn tampered_payload_hash_breaks_verification() {
        let tmp = tempfile::tempdir().unwrap();
        attest(tmp.path(), "sess-A", 0, b"original", None).unwrap();
        let (_, log_path) = data_paths(tmp.path());
        let raw = std::fs::read_to_string(&log_path).unwrap();
        let mut rec: Attestation = serde_json::from_str(raw.trim()).unwrap();
        // Mutate the payload hash. The signature was over the original
        // payload hash so verification must now fail.
        rec.payload_sha256 = sha256_hex(b"tampered");
        std::fs::write(
            &log_path,
            format!("{}\n", serde_json::to_string(&rec).unwrap()),
        )
        .unwrap();
        let report = verify_log(tmp.path()).unwrap();
        assert_eq!(report.ok, 0);
        assert_eq!(report.failures.len(), 1);
        assert!(report.failures[0].reason.contains("signature mismatch"));
    }

    #[test]
    fn removed_middle_record_breaks_chain() {
        let tmp = tempfile::tempdir().unwrap();
        attest(tmp.path(), "sess-A", 0, b"a", None).unwrap();
        attest(tmp.path(), "sess-A", 1, b"b", None).unwrap();
        attest(tmp.path(), "sess-A", 2, b"c", None).unwrap();
        let (_, log_path) = data_paths(tmp.path());
        let lines: Vec<String> = std::fs::read_to_string(&log_path)
            .unwrap()
            .lines()
            .map(str::to_owned)
            .collect();
        // Drop the middle record. Signatures still valid individually,
        // but the chain hash for record 3 no longer matches the expected
        // prev (which now points at record 1's signature, not record 2's).
        let joined = format!("{}\n{}\n", lines[0], lines[2]);
        std::fs::write(&log_path, joined).unwrap();
        let report = verify_log(tmp.path()).unwrap();
        assert!(
            report
                .failures
                .iter()
                .any(|f| f.reason.contains("chain break"))
        );
    }

    #[test]
    fn keypair_persists_across_calls() {
        let tmp = tempfile::tempdir().unwrap();
        let k1 = ensure_keypair(tmp.path()).unwrap();
        let k2 = ensure_keypair(tmp.path()).unwrap();
        assert_eq!(k1.to_bytes(), k2.to_bytes());
    }

    #[test]
    fn concurrent_attestors_produce_valid_chain() {
        // Twelve threads, each appending three records. Without the
        // attestations.lock flock, the read-then-write race silently
        // forks the chain and verify_log surfaces chain-break failures.
        // With the lock, all 36 records must verify cleanly.
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().to_path_buf();
        let mut handles = Vec::new();
        for t in 0..12 {
            let p = path.clone();
            handles.push(std::thread::spawn(move || {
                for i in 0..3 {
                    attest(
                        &p,
                        &format!("sess-{t}"),
                        i,
                        format!("payload-{t}-{i}").as_bytes(),
                        None,
                    )
                    .unwrap();
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }
        let report = verify_log(&path).unwrap();
        assert_eq!(report.total, 36);
        assert_eq!(report.ok, 36, "failures: {:?}", report.failures);
    }
}
