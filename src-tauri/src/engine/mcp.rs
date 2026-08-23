// Minimal MCP (Model Context Protocol) stdio client.
// Reads ~/.e/mcp.json, spawns servers, discovers tools, and bridges calls.
// Active only when mcp.json exists; the core is otherwise unaware of MCP.

use crate::engine::tools::{Tool, ToolContext, ToolResult};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

pub struct McpServer {
    pub name: String,
    _child: Child,
    stdin: Mutex<ChildStdin>,
    stdout: Mutex<BufReader<ChildStdout>>,
    next_id: AtomicU64,
}

impl McpServer {
    fn rpc(&self, method: &str, params: Value) -> Result<Value, String> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst) + 1;
        let req = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }).to_string();
        let mut line = req;
        line.push('\n');
        let mut sin = self.stdin.lock().map_err(|_| "mcp stdin lock")?;
        sin.write_all(line.as_bytes()).map_err(|e| format!("write: {e}"))?;
        sin.flush().map_err(|e| format!("flush: {e}"))?;
        drop(sin);
        let mut sout = self.stdout.lock().map_err(|_| "mcp stdout lock")?;
        let mut buf = String::new();
        loop {
            buf.clear();
            if sout.read_line(&mut buf).map_err(|e| format!("read: {e}"))? == 0 {
                return Err("mcp server closed".into());
            }
            let t = buf.trim();
            if t.is_empty() {
                continue;
            }
            let v: Value = serde_json::from_str(t).map_err(|e| format!("bad json: {e}"))?;
            if v.get("id").and_then(|x| x.as_u64()) == Some(id) {
                if let Some(err) = v.get("error") {
                    return Err(err.get("message").and_then(|m| m.as_str()).unwrap_or("mcp error").to_string());
                }
                return Ok(v.get("result").cloned().unwrap_or(Value::Null));
            }
        }
    }

    fn handshake(&self) -> Result<(), String> {
        self.rpc(
            "initialize",
            json!({
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": { "name": "e", "version": "0.1.0" }
            }),
        )?;
        let notify = json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }).to_string();
        let mut l = notify;
        l.push('\n');
        let mut sin = self.stdin.lock().map_err(|_| "lock")?;
        let _ = sin.write_all(l.as_bytes());
        let _ = sin.flush();
        Ok(())
    }

    pub fn list_tools(&self) -> Result<Vec<McpToolInfo>, String> {
        let res = self.rpc("tools/list", json!({}))?;
        let mut out = Vec::new();
        if let Some(tools) = res.get("tools").and_then(|t| t.as_array()) {
            for t in tools {
                let name = t.get("name").and_then(|n| n.as_str()).unwrap_or("").to_string();
                let description = t.get("description").and_then(|d| d.as_str()).unwrap_or("").to_string();
                let parameters = t.get("inputSchema").cloned().unwrap_or(json!({ "type": "object" }));
                if !name.is_empty() {
                    out.push(McpToolInfo { name, description, parameters });
                }
            }
        }
        Ok(out)
    }

    pub fn call(&self, tool: &str, args: Value) -> Result<String, String> {
        if let Value::Object(mut m) = args.clone() {
            m.insert("_meta".into(), json!({}));
        }
        let res = self.rpc("tools/call", json!({ "name": tool, "arguments": args }))?;
        let mut text = String::new();
        let mut is_error = false;
        if let Some(content) = res.get("content").and_then(|c| c.as_array()) {
            for item in content {
                let ty = item.get("type").and_then(|x| x.as_str()).unwrap_or("");
                let txt = item.get("text").and_then(|x| x.as_str());
                if ty == "text" {
                    if let Some(t) = txt {
                        text.push_str(t);
                    }
                }
            }
        }
        if res.get("isError").and_then(|x| x.as_bool()).unwrap_or(false) {
            is_error = true;
        }
        if text.trim().is_empty() {
            text = "(no text content)".to_string();
        }
        if is_error {
            Err(text)
        } else {
            Ok(text)
        }
    }
}

pub struct McpToolInfo {
    pub name: String,
    pub description: String,
    pub parameters: Value,
}

pub struct McpTool {
    pub server: Arc<McpServer>,
    pub tool: String,
    pub description: String,
    pub parameters: Value,
    /// Precomputed exposed name. Building it in `name()` leaked a `String` on
    /// every call, because the trait returns a borrowed `&str`.
    pub qualified: String,
}

impl McpTool {
    pub fn new(server: Arc<McpServer>, info: McpToolInfo) -> Self {
        let qualified = format!("mcp:{}/{}", server.name, info.name);
        McpTool {
            server,
            tool: info.name,
            description: info.description,
            parameters: info.parameters,
            qualified,
        }
    }
}

impl Tool for McpTool {
    fn name(&self) -> &str {
        &self.qualified
    }
    fn description(&self) -> &str {
        &self.description
    }
    fn parameters(&self) -> Value {
        self.parameters.clone()
    }
    fn run(&self, _ctx: &ToolContext, args: Value) -> ToolResult {
        self.server.call(&self.tool, args)
    }
}

/// Read ~/.e/mcp.json, spawn + handshake servers, return running servers.
pub fn load_servers() -> Vec<Arc<McpServer>> {
    let home = dirs::home_dir().unwrap_or_default();
    let cfg: Value = std::fs::read_to_string(home.join(".e/mcp.json"))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or(Value::Null);
    let mut servers = Vec::new();
    let Some(map) = cfg.get("servers").and_then(|s| s.as_object()) else {
        return servers;
    };
    for (name, conf) in map {
        let command = conf.get("command").and_then(|c| c.as_str()).unwrap_or("");
        let args: Vec<String> = conf
            .get("args")
            .and_then(|a| a.as_array())
            .map(|a| a.iter().filter_map(|x| x.as_str().map(|s| s.to_string())).collect())
            .unwrap_or_default();
        if command.is_empty() {
            continue;
        }
        let mut cmd = Command::new(command);
        cmd.args(&args).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null());
        let Ok(mut child) = cmd.spawn() else { continue };
        let Some(stdin) = child.stdin.take() else { continue };
        let Some(stdout) = child.stdout.take() else { continue };
        let server = Arc::new(McpServer {
            name: name.clone(),
            _child: child,
            stdin: Mutex::new(stdin),
            stdout: Mutex::new(BufReader::new(stdout)),
            next_id: AtomicU64::new(1),
        });
        if server.handshake().is_err() {
            continue;
        }
        servers.push(server);
    }
    servers
}
