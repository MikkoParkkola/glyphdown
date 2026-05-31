//! ultracos-core — local MCP server for the cost-saving plugin.
//!
//! v0.1.0 ships the load-bearing `stats` tool plus signed-CCR
//! attestations. The Python hooks (PostToolUse arc-event ledger,
//! PreCompact Layer-2 consumer) emit data; this binary reads it,
//! surfaces savings, and signs an Ed25519-backed audit chain.
//!
//! Layout:
//!   src/main.rs       — CLI entrypoint, subcommand dispatch
//!   src/stats.rs      — ledger reader + savings estimator
//!   src/data_dir.rs   — resolves ULTRACOS_DATA_DIR per the same
//!                       contract the Python hooks use
//!   src/signed_ccr.rs — Ed25519 attestation + chain verifier

use anyhow::Result;

mod anchor;
mod audit;
mod cache;
mod codec;
mod data_dir;
mod dedup;
mod hook;
mod signed_ccr;
mod stats;

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(|s| s.as_str()) {
        Some("compact") => {
            use std::io::Read;
            let mut buf = String::new();
            std::io::stdin().read_to_string(&mut buf)?;
            print!("{}", codec::compact(&buf));
            Ok(())
        }
        // PHASE 2a: full semantic-equivalent port of python `compact_payload`
        // (classify -> shape-dispatch -> path-list -> truncate -> break-even ->
        // schema-tag). Tokenizer-free (len/4). stdin -> stdout.
        Some("compact-payload") => {
            use std::io::Read;
            let mut buf = String::new();
            std::io::stdin().read_to_string(&mut buf)?;
            print!("{}", codec::compact_payload(&buf));
            Ok(())
        }
        // PostToolUse codec hook (DEFAULT codec; dispatcher routes here unless
        // ULTRACOS_RUST=0). dedup + cache-bypass + compact_payload + anchor, all
        // proven py-equivalent (see bench/equiv_guards_rust_vs_python.py).
        Some("posttooluse") => {
            hook::posttooluse();
            Ok(())
        }
        // PHASE 2b parity probes (used by bench/equiv_rust_vs_python.py to
        // prove py<->rust agreement on the safety guards, not just by inspection).
        // cache-sig: stdin text -> blake2b-16 hex signature (empty line if None).
        Some("cache-sig") => {
            use std::io::Read;
            let mut buf = String::new();
            std::io::stdin().read_to_string(&mut buf)?;
            match cache::prefix_signature(&buf) {
                Some(sig) => println!("{sig}"),
                None => println!(),
            }
            Ok(())
        }
        // anchor-revert: stdin JSON {"orig":..,"compact":..} -> {"revert":bool,..}.
        Some("anchor-revert") => {
            use std::io::Read;
            let mut buf = String::new();
            std::io::stdin().read_to_string(&mut buf)?;
            let v: serde_json::Value = serde_json::from_str(&buf)?;
            let orig = v.get("orig").and_then(|x| x.as_str()).unwrap_or("");
            let compact = v.get("compact").and_then(|x| x.as_str()).unwrap_or("");
            let (revert, reduction, survival) = anchor::should_revert(
                orig,
                compact,
                anchor::DEFAULT_REDUCTION_THRESHOLD,
                anchor::DEFAULT_PRESERVATION_FLOOR,
            );
            println!(
                "{}",
                serde_json::json!({
                    "revert": revert,
                    "reduction": reduction,
                    "survival": survival,
                })
            );
            Ok(())
        }
        // cache-bypass: stdin text -> "1" if should bypass (observe+probe), else "0".
        // Honors ULTRACOS_CACHE_AWARE / ULTRACOS_DATA_DIR so it shares state with
        // python for the interop fixture.
        Some("cache-bypass") => {
            use std::io::Read;
            let mut buf = String::new();
            std::io::stdin().read_to_string(&mut buf)?;
            let bypass = cache::should_bypass_for_cache(&buf, None);
            println!("{}", if bypass { "1" } else { "0" });
            Ok(())
        }
        // dedup: stdin JSON {"tool":..,"text":..,"session":..} -> {"mode":..,"text":..}
        // or {"mode":null} for no rewrite. Drives the dedup-parity harness and
        // shares <state_dir>/dedup-<session>.json with python.
        Some("dedup") => {
            use std::io::Read;
            let mut buf = String::new();
            std::io::stdin().read_to_string(&mut buf)?;
            let v: serde_json::Value = serde_json::from_str(&buf)?;
            let tool = v.get("tool").and_then(|x| x.as_str()).unwrap_or("");
            let text = v.get("text").and_then(|x| x.as_str()).unwrap_or("");
            let session = v
                .get("session")
                .and_then(|x| x.as_str())
                .unwrap_or("default");
            match dedup::maybe_dedup_or_summarize(tool, text, session) {
                Some((new_text, mode)) => {
                    println!("{}", serde_json::json!({"mode": mode, "text": new_text}))
                }
                None => println!("{}", serde_json::json!({"mode": serde_json::Value::Null})),
            }
            Ok(())
        }
        Some("compress") => {
            use std::io::Read;
            let mut buf = String::new();
            std::io::stdin().read_to_string(&mut buf)?;
            print!("{}", codec::compress(&buf));
            eprintln!("ultracos: ULTRACOS-L1 lossless compression (x-ultracos-transformer=ultracos-l1)");
            Ok(())
        }
        Some("expand") => {
            use std::io::Read;
            let mut buf = String::new();
            std::io::stdin().read_to_string(&mut buf)?;
            print!("{}", codec::expand(&buf));
            Ok(())
        }
        Some("stats") => {
            let report = stats::scan_all_sessions(&data_dir::resolve())?;
            println!("{}", serde_json::to_string_pretty(&report)?);
            Ok(())
        }
        Some("attest") => {
            // attest ARC_SESSION ARC_EVENT_INDEX PAYLOAD [PREFIX_HASH]
            let session = args
                .get(1)
                .ok_or_else(|| anyhow::anyhow!("attest: missing arc_session argument"))?;
            let idx: u64 = args
                .get(2)
                .ok_or_else(|| anyhow::anyhow!("attest: missing arc_event_index argument"))?
                .parse()?;
            let payload = args
                .get(3)
                .ok_or_else(|| anyhow::anyhow!("attest: missing payload argument"))?;
            let prefix = args.get(4).map(String::as_str);
            let rec = signed_ccr::attest(
                &data_dir::resolve(),
                session,
                idx,
                payload.as_bytes(),
                prefix,
            )?;
            println!("{}", serde_json::to_string_pretty(&rec)?);
            Ok(())
        }
        Some("verify") => {
            let report = signed_ccr::verify_log(&data_dir::resolve())?;
            println!("{}", serde_json::to_string_pretty(&report)?);
            if !report.failures.is_empty() {
                std::process::exit(1);
            }
            Ok(())
        }
        Some("version") | Some("--version") => {
            println!("ultracos-core {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        Some(other) => {
            anyhow::bail!(
                "unknown subcommand: {other}; supported: compact, compact-payload, cache-sig, cache-bypass, anchor-revert, dedup, compress, expand, stats, attest, verify, version"
            );
        }
        None => {
            eprintln!(
                "usage: ultracos-core <classify|summarize|compact|compress|expand|stats|attest|verify|version>"
            );
            std::process::exit(64); // sysexits: EX_USAGE
        }
    }
}
