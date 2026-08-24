//! End-to-end MCP: spawn a real stdio server, discover its tools, call one.
//!
//! The MCP client is the surface with the most moving parts — a subprocess,
//! a hand-rolled JSON-RPC framing and a registry that has to be re-entrant —
//! so it is exercised against an actual process rather than a mock.

use e_lib::engine::mcp;
use e_lib::engine::tools::{run_tool, ToolContext, ToolRegistry};
use std::path::{Path, PathBuf};
use std::sync::Arc;

const SERVER_JS: &str = r#"
// Minimal MCP stdio server: initialize, tools/list, tools/call.
let buf = "";
process.stdin.on("data", (chunk) => {
  buf += chunk;
  let i;
  while ((i = buf.indexOf("\n")) >= 0) {
    const line = buf.slice(0, i).trim();
    buf = buf.slice(i + 1);
    if (!line) continue;
    const req = JSON.parse(line);
    if (req.method === "notifications/initialized") continue;
    let result;
    if (req.method === "initialize") {
      result = { protocolVersion: "2024-11-05", capabilities: {}, serverInfo: { name: "fake", version: "1" } };
    } else if (req.method === "tools/list") {
      result = { tools: [{
        name: "echo",
        description: "Echo the text back.",
        inputSchema: { type: "object", properties: { text: { type: "string" } }, required: ["text"] },
      }] };
    } else if (req.method === "tools/call") {
      const text = (req.params && req.params.arguments && req.params.arguments.text) || "";
      result = { content: [{ type: "text", text: "echo:" + text + ":" + (process.env.E_TEST_TOKEN || "no-token") }] };
    } else {
      result = {};
    }
    // A log line with no id: the client has to skip it rather than mistake it
    // for the answer it is waiting for.
    process.stdout.write(JSON.stringify({ jsonrpc: "2.0", method: "log", params: { level: "info" } }) + "\n");
    process.stdout.write(JSON.stringify({ jsonrpc: "2.0", id: req.id, result }) + "\n");
  }
});
"#;

fn node_available() -> bool {
    std::process::Command::new("node")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn workspace(tag: &str, servers: &str) -> PathBuf {
    let ws = std::env::temp_dir().join(format!("e-mcp-e2e-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&ws);
    std::fs::create_dir_all(ws.join(".e")).unwrap();
    std::fs::write(ws.join("server.js"), SERVER_JS).unwrap();
    std::fs::write(ws.join(".e/mcp.json"), servers).unwrap();
    ws
}

fn ctx(ws: &Path) -> ToolContext {
    ToolContext { workspace: ws.to_path_buf() }
}

/// Server status and the live-server list are process-wide, so these tests
/// take turns rather than reading each other's servers.
static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());
fn serialized() -> std::sync::MutexGuard<'static, ()> {
    SERIAL.lock().unwrap_or_else(|e| e.into_inner())
}

#[test]
fn a_servers_tools_are_discovered_registered_and_callable() {
    let _turn = serialized();
    if !node_available() {
        eprintln!("skipping: node is not on PATH");
        return;
    }
    let ws = workspace(
        "ok",
        r#"{ "servers": { "fake": {
               "command": "node", "args": ["server.js"],
               "env": { "E_TEST_TOKEN": "from-env" } } } }"#,
    );
    let reg = Arc::new(ToolRegistry::new());
    for h in mcp::load(reg.clone(), Some(ws.to_string_lossy().to_string())) {
        let _ = h.join();
    }

    // Registered under the name the model is actually given.
    assert!(reg.get("mcp_fake_echo").is_some(), "tools: {:?}", reg.names());

    let (ok, out) = run_tool(&reg, &ctx(&ws), "mcp_fake_echo", serde_json::json!({ "text": "hi" }));
    assert!(ok, "{out}");
    // `env` reached the process, and `cwd` defaulted to the project folder —
    // "server.js" is relative and only resolves there.
    assert_eq!(out, "echo:hi:from-env");

    let status = mcp::status();
    let fake = status.iter().find(|s| s.name == "fake").expect("status");
    assert_eq!(fake.state, "ready", "{fake:?}");
    assert_eq!(fake.scope, "project");
    assert_eq!(fake.tools, vec!["mcp_fake_echo".to_string()]);

    // A reload retires the previous generation instead of leaving tools that
    // point at a subprocess that is gone.
    mcp::shutdown(&reg);
    assert!(reg.get("mcp_fake_echo").is_none());
    assert!(reg.get("shell").is_some(), "built-ins survive a reload");
    let _ = std::fs::remove_dir_all(&ws);
}

#[test]
fn a_server_that_cannot_start_is_reported_not_hidden() {
    let _turn = serialized();
    let ws = workspace(
        "bad",
        r#"{ "servers": { "nope": { "command": "definitely-not-a-real-command-xyz" } } }"#,
    );
    let reg = Arc::new(ToolRegistry::new());
    for h in mcp::load(reg.clone(), Some(ws.to_string_lossy().to_string())) {
        let _ = h.join();
    }

    let status = mcp::status();
    let nope = status.iter().find(|s| s.name == "nope").expect("status");
    assert_eq!(nope.state, "error");
    assert!(nope.error.contains("cannot start"), "{}", nope.error);

    mcp::shutdown(&reg);
    let _ = std::fs::remove_dir_all(&ws);
}

#[test]
fn a_disabled_server_is_listed_but_never_spawned() {
    let _turn = serialized();
    let ws = workspace(
        "off",
        r#"{ "servers": { "parked": { "command": "node", "args": ["server.js"], "disabled": true } } }"#,
    );
    let reg = Arc::new(ToolRegistry::new());
    for h in mcp::load(reg.clone(), Some(ws.to_string_lossy().to_string())) {
        let _ = h.join();
    }

    let status = mcp::status();
    let parked = status.iter().find(|s| s.name == "parked").expect("status");
    assert_eq!(parked.state, "disabled");
    assert!(reg.names().iter().all(|n| !n.starts_with("mcp_")), "{:?}", reg.names());

    mcp::shutdown(&reg);
    let _ = std::fs::remove_dir_all(&ws);
}
