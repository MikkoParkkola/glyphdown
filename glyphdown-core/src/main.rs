//! glyphdown-core — local MCP server for the cost-saving plugin.
//!
//! v0.1.0 ships the load-bearing `stats` tool plus signed-CCR
//! attestations. The Python hooks (PostToolUse arc-event ledger,
//! PreCompact Layer-2 consumer) emit data; this binary reads it,
//! surfaces savings, and signs an Ed25519-backed audit chain.
//!
//! Layout:
//!   src/main.rs       — CLI entrypoint, subcommand dispatch
//!   src/stats.rs      — ledger reader + savings estimator
//!   src/data_dir.rs   — resolves GLYPHDOWN_DATA_DIR per the same
//!                       contract the Python hooks use
//!   src/signed_ccr.rs — Ed25519 attestation + chain verifier

use anyhow::Result;

mod anchor;
mod audit;
mod cache;
mod codec;
mod data_dir;
mod dedup;
mod extract;
mod gate;
mod hook;
mod json_sample;
mod mcp;
mod readiness;
mod rewind;
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
        // GLYPHDOWN_RUST=0). dedup + cache-bypass + compact_payload + anchor, all
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
        // Honors GLYPHDOWN_CACHE_AWARE / GLYPHDOWN_DATA_DIR so it shares state with
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
            eprintln!("the proxy: GLYPHDOWN-L1 lossless compression (x-the proxy-transformer=glyphdown-l1)");
            Ok(())
        }
        Some("expand") => {
            use std::io::Read;
            let mut buf = String::new();
            std::io::stdin().read_to_string(&mut buf)?;
            print!("{}", codec::expand(&buf));
            Ok(())
        }
        Some("dialect-export") => {
            // Dump the compiled-in default dialect as an ordered JSON array of
            // [dense, prose] pairs. Edit + point GLYPHDOWN_DIALECT at the result to
            // customize compression with NO rebuild (lossless self-check on load).
            println!("{}", codec::Dialect::bundled_default().to_json());
            Ok(())
        }
        // F1 (internal-ref): retrieve a rewind-stashed original (or a line range) by id.
        // Usage: retrieve <session> <id> [A-B]   -> prints the exact original slice.
        // The agent-facing recovery path for read section-extraction.
        Some("retrieve") => {
            let session = args.get(1).map(|s| s.as_str()).unwrap_or("");
            let id = args.get(2).map(|s| s.as_str()).unwrap_or("");
            if session.is_empty() || id.is_empty() {
                eprintln!("usage: glyphdown-core retrieve <session> <id> [A-B]");
                std::process::exit(2);
            }
            let range = args.get(3).map(|s| s.as_str());
            match rewind::retrieve(session, id, range) {
                Some(text) => {
                    // Audit the retrieval so F1 retrieval-rate (the default-on gate)
                    // is measurable: a retrieve means extraction dropped something
                    // the agent actually needed.
                    audit::write_row(
                        session,
                        &audit::simple_event("rewind-retrieve", "retrieve", session),
                    );
                    print!("{text}");
                }
                None => {
                    eprintln!(
                        "glyphdown: rewind id {id} not found (evicted or expired); re-read the source"
                    );
                    std::process::exit(1);
                }
            }
            Ok(())
        }
        // Measurement harness for the F1/F2 default-on decision: read the audit log
        // and report extraction/retrieval/gate stats + a data-gated readiness verdict.
        Some("feature-readiness") => {
            println!("{}", readiness::from_data_dir().render());
            Ok(())
        }
        // F3 (internal-ref): uniform JSON-array sampling, Rewind-backed. stdin = JSON.
        // Opt-in (GLYPHDOWN_JSON_SAMPLE); declines non-uniform arrays (no backfire);
        // pass-through if disabled / not JSON / no savings.
        Some("sample-json") => {
            use std::io::Read;
            let session = args.get(1).map(|s| s.as_str()).unwrap_or("default");
            let mut buf = String::new();
            std::io::stdin().read_to_string(&mut buf)?;
            match json_sample::sample_json(session, &buf) {
                Some(s) => {
                    print!("{}", s.text);
                    eprintln!(
                        "glyphdown: sampled {}-item array {} -> {} tokens ({} anomalies kept, id={})",
                        s.array_len,
                        s.original_tokens,
                        s.sampled_tokens,
                        s.anomalies_kept,
                        s.rewind_id
                    );
                }
                None => print!("{buf}"),
            }
            Ok(())
        }
        // F1 (internal-ref): read section-extraction. stdin = a Read tool result.
        // Extracts outline + anchors + head, stashes the original to rewind, and
        // prints the extracted form (with retrieve markers). Pass-through if small.
        Some("extract-read") => {
            use std::io::Read;
            let session = args.get(1).map(|s| s.as_str()).unwrap_or("default");
            let mut buf = String::new();
            std::io::stdin().read_to_string(&mut buf)?;
            match extract::extract_read(session, &buf) {
                Some(r) => {
                    print!("{}", r.text);
                    eprintln!(
                        "glyphdown: extracted {} -> {} tokens ({} lines rewound, id={})",
                        r.original_tokens, r.extracted_tokens, r.dropped_lines, r.rewind_id
                    );
                }
                None => print!("{buf}"),
            }
            Ok(())
        }
        // Compress static config files (CLAUDE.md, skills, agent descriptions)
        // with the active dialect — the system prompt ships on EVERY request, so
        // this is the only always-on saving. DRY-RUN by default (never writes).
        // --apply writes, but ONLY files whose round-trip is lossless, and ALWAYS
        // backs up to <file>.glyphdown.bak first. expand the file (or restore the
        // .bak) to reverse.
        Some("compress-config") => {
            let rest: Vec<&str> = args.iter().skip(1).map(|s| s.as_str()).collect();
            let apply = rest.contains(&"--apply");
            let json = rest.contains(&"--json");
            let paths: Vec<&str> = rest
                .iter()
                .copied()
                .filter(|a| !a.starts_with("--"))
                .collect();
            if paths.is_empty() {
                eprintln!("usage: glyphdown-core compress-config [--apply] [--json] <file>...");
                std::process::exit(2);
            }
            let mut total_saved = 0i64;
            for path in paths {
                let content = match std::fs::read_to_string(path) {
                    Ok(c) => c,
                    Err(e) => {
                        eprintln!("skip {path}: {e}");
                        continue;
                    }
                };
                let r = codec::compress_config(&content);
                if json {
                    println!(
                        "{}",
                        serde_json::json!({
                            "path": path,
                            "lossless": r.lossless,
                            "already_dense": r.already_dense,
                            "original_tokens": r.original_tokens,
                            "compressed_tokens": r.compressed_tokens,
                            "saved_tokens": r.saved_tokens(),
                            "savings_pct": (r.savings_pct() * 100.0).round() / 100.0,
                        })
                    );
                } else {
                    let warn_lossless = if r.lossless {
                        ""
                    } else {
                        "  [NOT LOSSLESS — will not write]"
                    };
                    let warn_dense = if r.already_dense {
                        "  [already dense?]"
                    } else {
                        ""
                    };
                    println!(
                        "{path}: {} -> {} tokens ({:.1}% saved){warn_lossless}{warn_dense}",
                        r.original_tokens,
                        r.compressed_tokens,
                        r.savings_pct()
                    );
                }
                if apply {
                    if !r.safe_to_apply() {
                        eprintln!("REFUSE {path}: round-trip not lossless; left untouched");
                        continue;
                    }
                    if r.saved_tokens() <= 0 {
                        eprintln!("skip {path}: no savings; left untouched");
                        continue;
                    }
                    let bak = format!("{path}.glyphdown.bak");
                    if std::path::Path::new(&bak).exists() {
                        eprintln!("REFUSE {path}: backup {bak} exists; remove it first");
                        continue;
                    }
                    if let Err(e) = std::fs::write(&bak, &content) {
                        eprintln!("REFUSE {path}: cannot write backup {bak}: {e}");
                        continue;
                    }
                    if let Err(e) = std::fs::write(path, &r.compressed) {
                        eprintln!("ERROR {path}: write failed after backup: {e}");
                        continue;
                    }
                    total_saved += r.saved_tokens();
                    eprintln!("WROTE {path} (backup: {bak})");
                } else if r.lossless {
                    total_saved += r.saved_tokens().max(0);
                }
            }
            if apply {
                eprintln!("applied: {total_saved} tokens saved across written files.");
            } else {
                eprintln!(
                    "dry-run: {total_saved} tokens would be saved. Re-run with --apply \
                     to write (each file is backed up to <file>.glyphdown.bak first; \
                     restore the .bak or run `glyphdown-core expand` to reverse)."
                );
            }
            Ok(())
        }
        Some("stats") => {
            let report = stats::scan_all_sessions(&data_dir::resolve())?;
            println!("{}", serde_json::to_string_pretty(&report)?);
            Ok(())
        }
        // MCP server (stdio JSON-RPC 2.0). Exposes compress/expand/compress-config/
        // retrieve as MCP tools so ANY MCP client (Cursor, Cline, Zed, claude.ai
        // connectors, …) gets the codec on-demand — the language-agnostic reach
        // surface. Speaks newline-delimited JSON-RPC on stdin/stdout; all
        // diagnostics on stderr. Run as a plugin mcpServers `command`.
        Some("mcp") => mcp::serve(),
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
            println!("glyphdown-core {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        Some(other) => {
            anyhow::bail!(
                "unknown subcommand: {other}; supported: classify, summarize, compact, compact-payload, cache-sig, cache-bypass, anchor-revert, dedup, compress, expand, mcp, stats, attest, verify, version"
            );
        }
        None => {
            eprintln!(
                "usage: glyphdown-core <classify|summarize|compact|compress|expand|mcp|stats|attest|verify|version>"
            );
            std::process::exit(64); // sysexits: EX_USAGE
        }
    }
}
