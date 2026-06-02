//! Real-process MCP stdio round-trip. The unit tests in `src/mcp.rs` call
//! `handle()` directly and cannot see the serve loop, the newline framing, the
//! stdout/stderr split, or notification silence. This drives the actual `mcp`
//! subcommand the way a client does — bytes in on stdin, lines out on stdout —
//! and is the verification that matches how Cursor/Cline/Zed will invoke it.

use std::io::Write;
use std::process::{Command, Stdio};

fn run_session(input: &str) -> String {
    let bin = env!("CARGO_BIN_EXE_ultracos-core");
    let mut child = Command::new(bin)
        .arg("mcp")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null()) // diagnostics live here; never on stdout
        .spawn()
        .expect("spawn mcp server");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    String::from_utf8(out.stdout).unwrap()
}

#[test]
fn full_stdio_session_frames_one_json_per_line_and_stays_clean() {
    let input = concat!(
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18"}}"#,
        "\n",
        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
        "\n",
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#,
        "\n",
        r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"ultracos_compress","arguments":{"text":"hello"}}}"#,
        "\n",
        "this is not json\n",
        r#"{"jsonrpc":"2.0","id":4,"method":"ping"}"#,
        "\n",
    );
    let stdout = run_session(input);

    let lines: Vec<&str> = stdout.lines().filter(|l| !l.trim().is_empty()).collect();
    // 6 inputs, but the notification yields NO line: expect exactly 5 responses.
    assert_eq!(lines.len(), 5, "notification must not produce output:\n{stdout}");

    // Every emitted line is a standalone JSON object (framing intact).
    let msgs: Vec<serde_json::Value> = lines
        .iter()
        .map(|l| serde_json::from_str(l).expect("each stdout line is valid JSON"))
        .collect();

    // initialize echoes the client's protocol version.
    assert_eq!(msgs[0]["id"], 1);
    assert_eq!(msgs[0]["result"]["protocolVersion"], "2025-06-18");

    // tools/list exposes the codec tools.
    assert_eq!(msgs[1]["id"], 2);
    assert_eq!(msgs[1]["result"]["tools"].as_array().unwrap().len(), 5);

    // compress returns a non-error result.
    assert_eq!(msgs[2]["id"], 3);
    assert_eq!(msgs[2]["result"]["isError"], false);

    // The malformed line became a parse error with null id — and the loop SURVIVED
    // it, because the ping after it was answered.
    assert_eq!(msgs[3]["error"]["code"], -32700);
    assert_eq!(msgs[3]["id"], serde_json::Value::Null);
    assert_eq!(msgs[4]["id"], 4);
    assert!(msgs[4]["result"].is_object());
}

#[test]
fn eof_exits_cleanly_with_zero_status() {
    let bin = env!("CARGO_BIN_EXE_ultracos-core");
    let mut child = Command::new(bin)
        .arg("mcp")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    drop(child.stdin.take()); // immediate EOF
    let status = child.wait().unwrap();
    assert!(status.success(), "EOF on stdin must exit 0");
}
