//! mcp — a minimal, dependency-free MCP (Model Context Protocol) server that
//! exposes the GLYPHDOWN codec to ANY MCP-speaking client (Cursor, Cline, Zed,
//! Continue, claude.ai connectors, …), not just the Claude Code PostToolUse hook.
//!
//! This is the **reach** surface: the codec is text->text and language-agnostic,
//! so the highest-leverage distribution is "any MCP client gets compress/expand/
//! compress-config/retrieve on-demand" without a per-language SDK. The binary is
//! shared; this module just speaks the wire protocol over stdio.
//!
//! Transport: newline-delimited JSON-RPC 2.0 over stdin/stdout (the MCP stdio
//! transport — NOT LSP-style Content-Length framing). One JSON message per line.
//!
//! Discipline (load-bearing — a stdio MCP server lives or dies on these):
//!   * stdout carries the protocol ONLY. Every diagnostic goes to stderr. A
//!     stray byte on stdout desyncs the client. `println!` is safe because the
//!     std stdout handle is a LineWriter that flushes on '\n'; do NOT wrap it in
//!     a BufWriter without flushing.
//!   * `initialize` ECHOES the client's protocolVersion (codec is version-
//!     agnostic) and falls back to a recent default only when none is sent.
//!   * A tool-execution failure (retrieve miss) is a normal `tools/call` result
//!     with `isError:true` — NOT a JSON-RPC error. JSON-RPC errors are reserved
//!     for protocol faults (parse error, unknown method, unknown tool, bad args).
//!   * Notifications (no `id`) produce ZERO output.
//!   * Fail-open: a malformed line never panics the loop; EOF exits cleanly.

use serde_json::{Value, json};
use std::io::{BufRead, Write};

use crate::{codec, extract, rewind};

/// Most recent protocol version we default to when a client omits one. We echo
/// the client's version when present, so this only matters for non-negotiating
/// callers.
const DEFAULT_PROTOCOL_VERSION: &str = "2025-06-18";

/// Run the stdio MCP loop until EOF. Always returns Ok — the loop is fail-open.
pub fn serve() -> anyhow::Result<()> {
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break }; // stdin closed / unreadable -> exit
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        match serde_json::from_str::<Value>(trimmed) {
            Ok(req) => {
                if let Some(resp) = handle(&req) {
                    write_line(&mut out, &resp);
                }
            }
            Err(_) => {
                // Parse error: id is unknowable -> null per JSON-RPC 2.0.
                write_line(&mut out, &rpc_error(Value::Null, -32700, "parse error"));
            }
        }
    }
    Ok(())
}

fn write_line(out: &mut impl Write, v: &Value) {
    // Compact, single line, explicit flush. Ignore write errors (client gone).
    let _ = writeln!(out, "{v}");
    let _ = out.flush();
}

/// Handle one parsed JSON-RPC message. Returns None for notifications (no `id`)
/// and for anything that must stay silent.
fn handle(req: &Value) -> Option<Value> {
    let method = req.get("method").and_then(|m| m.as_str()).unwrap_or("");
    let id = req.get("id").cloned();

    // No id => notification => never respond (even on error).
    if id.is_none() {
        return None;
    }
    let id = id.unwrap();

    match method {
        "initialize" => Some(rpc_ok(id, initialize_result(req))),
        "tools/list" => Some(rpc_ok(id, json!({ "tools": tool_specs() }))),
        "tools/call" => Some(handle_tools_call(id, req)),
        "ping" => Some(rpc_ok(id, json!({}))),
        other => Some(rpc_error(
            id,
            -32601,
            &format!("method not found: {other}"),
        )),
    }
}

fn initialize_result(req: &Value) -> Value {
    // Echo the client's protocolVersion; fall back only when absent.
    let proto = req
        .get("params")
        .and_then(|p| p.get("protocolVersion"))
        .and_then(|v| v.as_str())
        .unwrap_or(DEFAULT_PROTOCOL_VERSION);
    json!({
        "protocolVersion": proto,
        "capabilities": { "tools": {} },
        "serverInfo": {
            "name": "glyphdown-core",
            "version": env!("CARGO_PKG_VERSION"),
        },
    })
}

/// The four codec tools — the exact reach surface (compress / expand /
/// compress-config / retrieve). Schemas are validated client-side, so keep them
/// honest and complete.
fn tool_specs() -> Value {
    json!([
        {
            "name": "glyphdown_compress",
            "description": "Compress prose to the GLYPHDOWN-L1 dense register (lossless; \
                            unrecognized text passes through). The target LLM reads the \
                            dense form natively. Reverse with glyphdown_expand.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "text": { "type": "string", "description": "Prose to compress." }
                },
                "required": ["text"]
            }
        },
        {
            "name": "glyphdown_expand",
            "description": "Expand GLYPHDOWN-L1 dense text back to prose (exact inverse of \
                            glyphdown_compress).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "text": { "type": "string", "description": "Dense text to expand." }
                },
                "required": ["text"]
            }
        },
        {
            "name": "glyphdown_compress_config",
            "description": "Preview compressing a config/system-prompt file (CLAUDE.md, a \
                            skill, an agent description) with the active dialect. Returns the \
                            compressed text plus token savings and a `lossless` flag. \
                            Read-only — never writes a file.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "text": { "type": "string", "description": "File content to preview-compress." }
                },
                "required": ["text"]
            }
        },
        {
            "name": "glyphdown_retrieve",
            "description": "Recover a rewind-stashed original (or a line range A-B) by id — \
                            the recovery path for extracted/sampled tool output. Resolves only \
                            when this server shares the rewind store with whatever stashed the \
                            content (co-located); inert for a remote client.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "session": { "type": "string", "description": "Session id the content was stashed under." },
                    "id": { "type": "string", "description": "Rewind id to retrieve." },
                    "range": { "type": "string", "description": "Optional line range 'A-B' (1-based, inclusive)." }
                },
                "required": ["session", "id"]
            }
        },
        {
            "name": "glyphdown_extract",
            "description": "Shrink a large Read-class tool result: keep the head, every \
                            structural landmark (headings, declarations, keys), and every \
                            load-bearing anchor (file:line, error code, test verdict, hash); \
                            collapse the uniform middle into retrieve markers; stash the full \
                            original to the rewind store. The producer half of the \
                            extract/retrieve loop — the returned text embeds `id`/`range` \
                            markers that glyphdown_retrieve resolves. Lossy-but-recoverable; \
                            passes the text through unchanged when it is too small to save \
                            tokens. Targets the fat tail (a few huge results dominate volume).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "session": { "type": "string", "description": "Session id to stash the original under (use the same id with glyphdown_retrieve)." },
                    "text": { "type": "string", "description": "The large tool result to extract." }
                },
                "required": ["session", "text"]
            }
        }
    ])
}

fn handle_tools_call(id: Value, req: &Value) -> Value {
    let params = req.get("params");
    let name = params
        .and_then(|p| p.get("name"))
        .and_then(|n| n.as_str())
        .unwrap_or("");
    let args = params
        .and_then(|p| p.get("arguments"))
        .cloned()
        .unwrap_or_else(|| json!({}));

    let s = |k: &str| args.get(k).and_then(|v| v.as_str());

    match name {
        "glyphdown_compress" => match s("text") {
            Some(t) => rpc_ok(id, tool_text(codec::compress(t))),
            None => rpc_error(id, -32602, "glyphdown_compress: missing required 'text'"),
        },
        "glyphdown_expand" => match s("text") {
            Some(t) => rpc_ok(id, tool_text(codec::expand(t))),
            None => rpc_error(id, -32602, "glyphdown_expand: missing required 'text'"),
        },
        "glyphdown_compress_config" => match s("text") {
            Some(t) => {
                let r = codec::compress_config(t);
                let payload = json!({
                    "compressed": r.compressed,
                    "lossless": r.lossless,
                    "already_dense": r.already_dense,
                    "original_tokens": r.original_tokens,
                    "compressed_tokens": r.compressed_tokens,
                    "saved_tokens": r.saved_tokens(),
                    "savings_pct": (r.savings_pct() * 100.0).round() / 100.0,
                    "safe_to_apply": r.safe_to_apply(),
                });
                rpc_ok(
                    id,
                    tool_text(
                        serde_json::to_string_pretty(&payload)
                            .unwrap_or_else(|_| "{}".to_string()),
                    ),
                )
            }
            None => rpc_error(id, -32602, "glyphdown_compress_config: missing required 'text'"),
        },
        "glyphdown_retrieve" => {
            let (Some(session), Some(rid)) = (s("session"), s("id")) else {
                return rpc_error(
                    id,
                    -32602,
                    "glyphdown_retrieve: missing required 'session' and/or 'id'",
                );
            };
            let range = s("range");
            match rewind::retrieve(session, rid, range) {
                Some(text) => rpc_ok(id, tool_text(text)),
                // Execution failure, NOT a protocol error: isError result.
                None => rpc_ok(
                    id,
                    tool_error(format!(
                        "rewind id '{rid}' not found in session '{session}' \
                         (evicted, expired, or not co-located); re-read the source"
                    )),
                ),
            }
        }
        "glyphdown_extract" => {
            let (Some(session), Some(text)) = (s("session"), s("text")) else {
                return rpc_error(
                    id,
                    -32602,
                    "glyphdown_extract: missing required 'session' and/or 'text'",
                );
            };
            match extract::extract_read(session, text) {
                // Extracted form embeds the retrieve markers (id + range).
                Some(r) => rpc_ok(id, tool_text(r.text)),
                // Too small / no savings: pass the original through unchanged.
                // Not an error — extraction simply declined.
                None => rpc_ok(id, tool_text(text.to_string())),
            }
        }
        other => rpc_error(id, -32602, &format!("unknown tool: {other}")),
    }
}

/// A successful tools/call result carrying one text content block.
fn tool_text(text: String) -> Value {
    json!({ "content": [ { "type": "text", "text": text } ], "isError": false })
}

/// A tools/call result that ran but failed at the tool level (isError:true).
fn tool_error(text: String) -> Value {
    json!({ "content": [ { "type": "text", "text": text } ], "isError": true })
}

fn rpc_ok(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

fn rpc_error(id: Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(s: &str) -> Value {
        serde_json::from_str(s).unwrap()
    }

    #[test]
    fn initialize_echoes_client_protocol_version() {
        let r = handle(&req(
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05"}}"#,
        ))
        .unwrap();
        assert_eq!(r["result"]["protocolVersion"], "2024-11-05");
        assert_eq!(r["result"]["serverInfo"]["name"], "glyphdown-core");
        assert!(r["result"]["capabilities"]["tools"].is_object());
    }

    #[test]
    fn initialize_falls_back_when_no_version() {
        let r = handle(&req(r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#)).unwrap();
        assert_eq!(r["result"]["protocolVersion"], DEFAULT_PROTOCOL_VERSION);
    }

    #[test]
    fn notification_produces_no_output() {
        // No id => notification => silent.
        assert!(handle(&req(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#)).is_none());
    }

    #[test]
    fn tools_list_has_the_codec_tools() {
        let r = handle(&req(r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#)).unwrap();
        let names: Vec<&str> = r["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap())
            .collect();
        assert_eq!(
            names,
            vec![
                "glyphdown_compress",
                "glyphdown_expand",
                "glyphdown_compress_config",
                "glyphdown_retrieve",
                "glyphdown_extract"
            ]
        );
        // Every tool carries a valid object inputSchema.
        for t in r["result"]["tools"].as_array().unwrap() {
            assert_eq!(t["inputSchema"]["type"], "object");
        }
    }

    #[test]
    fn compress_roundtrips_via_tools_call() {
        let probe = "decision verify implementation analyze";
        let call = json!({
            "jsonrpc":"2.0","id":3,"method":"tools/call",
            "params":{"name":"glyphdown_compress","arguments":{"text":probe}}
        });
        let r = handle(&call).unwrap();
        assert_eq!(r["result"]["isError"], false);
        let dense = r["result"]["content"][0]["text"].as_str().unwrap().to_string();

        let back = json!({
            "jsonrpc":"2.0","id":4,"method":"tools/call",
            "params":{"name":"glyphdown_expand","arguments":{"text":dense}}
        });
        let r2 = handle(&back).unwrap();
        assert_eq!(r2["result"]["content"][0]["text"], probe);
    }

    #[test]
    fn compress_config_reports_savings_json() {
        let call = json!({
            "jsonrpc":"2.0","id":5,"method":"tools/call",
            "params":{"name":"glyphdown_compress_config","arguments":{"text":"decision and implementation"}}
        });
        let r = handle(&call).unwrap();
        let text = r["result"]["content"][0]["text"].as_str().unwrap();
        let parsed: Value = serde_json::from_str(text).unwrap();
        assert!(parsed["lossless"].as_bool().unwrap());
        assert!(parsed.get("saved_tokens").is_some());
    }

    #[test]
    fn retrieve_miss_is_iserror_not_protocol_error() {
        let call = json!({
            "jsonrpc":"2.0","id":6,"method":"tools/call",
            "params":{"name":"glyphdown_retrieve","arguments":{"session":"nope","id":"deadbeef"}}
        });
        let r = handle(&call).unwrap();
        // Must be a normal result, isError:true — NOT a top-level JSON-RPC error.
        assert!(r.get("error").is_none());
        assert_eq!(r["result"]["isError"], true);
    }

    #[test]
    fn missing_required_arg_is_protocol_error() {
        let call = json!({
            "jsonrpc":"2.0","id":7,"method":"tools/call",
            "params":{"name":"glyphdown_compress","arguments":{}}
        });
        let r = handle(&call).unwrap();
        assert_eq!(r["error"]["code"], -32602);
    }

    #[test]
    fn unknown_method_is_method_not_found() {
        let r = handle(&req(r#"{"jsonrpc":"2.0","id":8,"method":"frobnicate"}"#)).unwrap();
        assert_eq!(r["error"]["code"], -32601);
    }

    #[test]
    fn unknown_tool_is_invalid_params() {
        let call = json!({
            "jsonrpc":"2.0","id":9,"method":"tools/call",
            "params":{"name":"glyphdown_nonexistent","arguments":{}}
        });
        let r = handle(&call).unwrap();
        assert_eq!(r["error"]["code"], -32602);
    }

    #[test]
    fn extract_then_retrieve_round_trips_the_full_original() {
        let _g = crate::rewind::TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join("glyphdown-mcp-extract-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // SAFETY: serialized by the shared TEST_ENV_LOCK above.
        unsafe { std::env::set_var("GLYPHDOWN_REWIND_DIR", &dir) };

        // A big doc with a uniform, droppable middle (plain prose: no colons,
        // headings, declarations, or anchors -> collapsible).
        let mut doc = String::from("intro paragraph orienting the reader here now\n");
        for i in 0..60 {
            doc.push_str(&format!("this is filler prose line number {i} of the body text\n"));
        }
        let call = json!({
            "jsonrpc":"2.0","id":10,"method":"tools/call",
            "params":{"name":"glyphdown_extract","arguments":{"session":"sx","text":doc}}
        });
        let r = handle(&call).unwrap();
        assert_eq!(r["result"]["isError"], false);
        let extracted = r["result"]["content"][0]["text"].as_str().unwrap();
        assert!(
            extracted.contains("glyphdown:extracted"),
            "extraction must emit a retrieve marker:\n{extracted}"
        );

        // Pull the rewind id out of the marker and retrieve the FULL original.
        let id = extracted
            .split("id=")
            .nth(1)
            .and_then(|s| s.split([' ', ']']).next())
            .expect("marker carries id=");
        let back = json!({
            "jsonrpc":"2.0","id":11,"method":"tools/call",
            "params":{"name":"glyphdown_retrieve","arguments":{"session":"sx","id":id}}
        });
        let r2 = handle(&back).unwrap();
        assert_eq!(r2["result"]["isError"], false);
        assert_eq!(
            r2["result"]["content"][0]["text"], doc,
            "retrieve must reproduce the original byte-for-byte"
        );

        unsafe { std::env::remove_var("GLYPHDOWN_REWIND_DIR") };
    }

    #[test]
    fn extract_passes_small_input_through_unchanged() {
        let call = json!({
            "jsonrpc":"2.0","id":12,"method":"tools/call",
            "params":{"name":"glyphdown_extract","arguments":{"session":"sy","text":"tiny"}}
        });
        let r = handle(&call).unwrap();
        assert_eq!(r["result"]["isError"], false);
        assert_eq!(r["result"]["content"][0]["text"], "tiny");
    }
}
