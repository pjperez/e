//! Minimal MCP (Model Context Protocol) stdio client.
//!
//! Reads `~/.e/mcp.json` (and `<workspace>/.e/mcp.json`), spawns the servers
//! it lists, discovers their tools and merges them into the running registry
//! as `mcp_<server>_<tool>`. Everything here is inert without an `mcp.json`:
//! the core never learns MCP exists unless the user asks for it.

use crate::engine::tools::{Tool, ToolContext, ToolRegistry, ToolResult};
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

/// Prefix every MCP tool carries once registered. Reloading retires the whole
/// generation by this prefix.
pub const PREFIX: &str = "mcp_";
/// How long a server gets to answer one request. A server that goes quiet must
/// surface as a failed tool call, not as a frozen agent.
const RPC_TIMEOUT: Duration = Duration::from_secs(60);
/// The handshake and tool listing happen at startup and should be quick.
const START_TIMEOUT: Duration = Duration::from_secs(20);

/// What one configured server is doing, for the Extensions pane.
#[derive(Clone, Debug, Serialize)]
pub struct McpStatus {
    pub name: String,
    pub command: String,
    /// "global" (`~/.e/mcp.json`) or "project" (`<workspace>/.e/mcp.json`).
    pub scope: String,
    /// "ready", "disabled" or "error".
    pub state: String,
    pub tools: Vec<String>,
    pub error: String,
}

#[derive(Clone, Debug)]
struct ServerConf {
    name: String,
    command: String,
    args: Vec<String>,
    env: BTreeMap<String, String>,
    cwd: String,
    disabled: bool,
    scope: String,
}

pub struct McpServer {
    pub name: String,
    child: Mutex<Child>,
    stdin: Mutex<ChildStdin>,
    /// Lines from a reader thread. Reading the pipe directly would block
    /// forever when a server stops answering, taking the agent loop with it.
    lines: Mutex<mpsc::Receiver<String>>,
    next_id: AtomicU64,
}

impl McpServer {
    fn rpc(&self, method: &str, params: Value, timeout: Duration) -> Result<Value, String> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst) + 1;
        let mut line = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }).to_string();
        line.push('\n');
        {
            let mut sin = self.stdin.lock().map_err(|_| "mcp stdin lock")?;
            sin.write_all(line.as_bytes()).map_err(|e| format!("write: {e}"))?;
            sin.flush().map_err(|e| format!("flush: {e}"))?;
        }
        let rx = self.lines.lock().map_err(|_| "mcp reader lock")?;
        let deadline = Instant::now() + timeout;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(format!("'{}' did not answer {method} within {}s", self.name, timeout.as_secs()));
            }
            let text = match rx.recv_timeout(left) {
                Ok(t) => t,
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    return Err(format!("'{}' did not answer {method} within {}s", self.name, timeout.as_secs()))
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return Err(format!("'{}' exited", self.name))
                }
            };
            let t = text.trim();
            if t.is_empty() {
                continue;
            }
            let Ok(v) = serde_json::from_str::<Value>(t) else { continue };
            // Notifications and log lines carry no matching id; skip them.
            if v.get("id").and_then(|x| x.as_u64()) != Some(id) {
                continue;
            }
            if let Some(err) = v.get("error") {
                return Err(err.get("message").and_then(|m| m.as_str()).unwrap_or("mcp error").to_string());
            }
            return Ok(v.get("result").cloned().unwrap_or(Value::Null));
        }
    }

    fn handshake(&self) -> Result<(), String> {
        self.rpc(
            "initialize",
            json!({
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": { "name": "e", "version": env!("CARGO_PKG_VERSION") }
            }),
            START_TIMEOUT,
        )?;
        let mut l = json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }).to_string();
        l.push('\n');
        let mut sin = self.stdin.lock().map_err(|_| "lock")?;
        let _ = sin.write_all(l.as_bytes());
        let _ = sin.flush();
        Ok(())
    }

    pub fn list_tools(&self) -> Result<Vec<McpToolInfo>, String> {
        let res = self.rpc("tools/list", json!({}), START_TIMEOUT)?;
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
        let res = self.rpc("tools/call", json!({ "name": tool, "arguments": args }), RPC_TIMEOUT)?;
        let mut text = String::new();
        if let Some(content) = res.get("content").and_then(|c| c.as_array()) {
            for item in content {
                let ty = item.get("type").and_then(|x| x.as_str()).unwrap_or("");
                if ty == "text" {
                    if let Some(t) = item.get("text").and_then(|x| x.as_str()) {
                        text.push_str(t);
                    }
                }
            }
        }
        if text.trim().is_empty() {
            text = "(no text content)".to_string();
        }
        if res.get("isError").and_then(|x| x.as_bool()).unwrap_or(false) {
            Err(text)
        } else {
            Ok(text)
        }
    }

    fn shutdown(&self) {
        if let Ok(mut c) = self.child.lock() {
            let _ = c.kill();
            let _ = c.wait();
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
        // Provider APIs only accept [a-zA-Z0-9_-], so the qualified name is
        // built in that alphabet: what the model is told is then exactly what
        // the registry, the tool card and the docs call it.
        let qualified = format!("{PREFIX}{}_{}", slug(&server.name), slug(&info.name));
        McpTool {
            server,
            tool: info.name,
            description: info.description,
            parameters: info.parameters,
            qualified,
        }
    }
}

fn slug(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' { c } else { '_' })
        .collect()
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

/// Live servers, so a reload can stop the previous generation instead of
/// leaking subprocesses that still hold their stdio.
static LIVE: LazyLock<Mutex<Vec<Arc<McpServer>>>> = LazyLock::new(|| Mutex::new(Vec::new()));
static STATUS: LazyLock<Mutex<Vec<McpStatus>>> = LazyLock::new(|| Mutex::new(Vec::new()));

pub fn status() -> Vec<McpStatus> {
    STATUS.lock().map(|s| s.clone()).unwrap_or_default()
}

fn config_files(workspace: Option<&str>) -> Vec<(String, PathBuf)> {
    let mut out = Vec::new();
    if let Some(home) = dirs::home_dir() {
        out.push(("global".to_string(), home.join(".e/mcp.json")));
    }
    if let Some(ws) = workspace.map(str::trim).filter(|w| !w.is_empty()) {
        let p = Path::new(ws);
        if p.is_absolute() {
            out.push(("project".to_string(), p.join(".e/mcp.json")));
        }
    }
    out
}

/// Servers from every config file, project entries shadowing global ones of
/// the same name.
fn read_config(workspace: Option<&str>) -> Vec<ServerConf> {
    let mut by_name: BTreeMap<String, ServerConf> = BTreeMap::new();
    for (scope, file) in config_files(workspace) {
        let Ok(text) = std::fs::read_to_string(&file) else { continue };
        let Ok(cfg) = serde_json::from_str::<Value>(&text) else { continue };
        let Some(map) = cfg.get("servers").and_then(|s| s.as_object()) else { continue };
        for (name, conf) in map {
            let env = conf
                .get("env")
                .and_then(|e| e.as_object())
                .map(|o| {
                    o.iter()
                        .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                        .collect::<BTreeMap<String, String>>()
                })
                .unwrap_or_default();
            by_name.insert(
                name.clone(),
                ServerConf {
                    name: name.clone(),
                    command: conf.get("command").and_then(|c| c.as_str()).unwrap_or("").to_string(),
                    args: conf
                        .get("args")
                        .and_then(|a| a.as_array())
                        .map(|a| a.iter().filter_map(|x| x.as_str().map(|s| s.to_string())).collect())
                        .unwrap_or_default(),
                    env,
                    cwd: conf.get("cwd").and_then(|c| c.as_str()).unwrap_or("").to_string(),
                    // Either spelling works: `"disabled": true` or `"enabled": false`.
                    disabled: conf.get("disabled").and_then(|d| d.as_bool()).unwrap_or(false)
                        || !conf.get("enabled").and_then(|e| e.as_bool()).unwrap_or(true),
                    scope: scope.clone(),
                },
            );
        }
    }
    by_name.into_values().collect()
}

fn spawn(conf: &ServerConf, workspace: Option<&str>) -> Result<Arc<McpServer>, String> {
    if conf.command.trim().is_empty() {
        return Err("no \"command\" in mcp.json".to_string());
    }
    let mut cmd = Command::new(&conf.command);
    cmd.args(&conf.args).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null());
    for (k, v) in &conf.env {
        cmd.env(k, v);
    }
    // A server that reads files ("." in its args) has to start somewhere
    // meaningful: its own `cwd`, else the project folder. Never the directory
    // the app happened to be launched from.
    let dir = if !conf.cwd.trim().is_empty() {
        Some(PathBuf::from(conf.cwd.trim()))
    } else {
        workspace.map(str::trim).filter(|w| !w.is_empty()).map(PathBuf::from).filter(|p| p.is_absolute())
    };
    if let Some(d) = dir.filter(|d| d.is_dir()) {
        cmd.current_dir(d);
    }
    let mut child = cmd.spawn().map_err(|e| format!("cannot start '{}': {e}", conf.command))?;
    let stdin = child.stdin.take().ok_or("no stdin")?;
    let stdout = child.stdout.take().ok_or("no stdout")?;
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    let server = Arc::new(McpServer {
        name: conf.name.clone(),
        child: Mutex::new(child),
        stdin: Mutex::new(stdin),
        lines: Mutex::new(rx),
        next_id: AtomicU64::new(1),
    });
    server.handshake().map_err(|e| format!("handshake failed: {e}"))?;
    Ok(server)
}

/// Stop the running servers and forget their tools.
pub fn shutdown(tools: &ToolRegistry) {
    tools.unregister_prefix(PREFIX);
    let live: Vec<Arc<McpServer>> = LIVE.lock().map(|mut l| std::mem::take(&mut *l)).unwrap_or_default();
    for s in live {
        s.shutdown();
    }
}

/// (Re)start every configured server and register its tools. Each server gets
/// its own thread, so one that hangs during startup cannot keep the others —
/// or the app — waiting. The handles let a headless caller wait for the first
/// generation of tools before it starts answering requests.
pub fn load(tools: Arc<ToolRegistry>, workspace: Option<String>) -> Vec<std::thread::JoinHandle<()>> {
    shutdown(&tools);
    let confs = read_config(workspace.as_deref());
    if let Ok(mut s) = STATUS.lock() {
        *s = confs
            .iter()
            .map(|c| McpStatus {
                name: c.name.clone(),
                command: format!("{} {}", c.command, c.args.join(" ")).trim().to_string(),
                scope: c.scope.clone(),
                state: if c.disabled { "disabled".into() } else { "starting".into() },
                tools: Vec::new(),
                error: String::new(),
            })
            .collect();
    }
    let mut handles = Vec::new();
    for conf in confs.into_iter().filter(|c| !c.disabled) {
        let tools = tools.clone();
        let ws = workspace.clone();
        handles.push(std::thread::spawn(move || {
            let (state, names, error) = match spawn(&conf, ws.as_deref()) {
                Ok(server) => match server.list_tools() {
                    Ok(list) => {
                        let mut names = Vec::new();
                        for info in list {
                            let t = McpTool::new(server.clone(), info);
                            names.push(t.qualified.clone());
                            tools.register_boxed(Box::new(t));
                        }
                        if let Ok(mut l) = LIVE.lock() {
                            l.push(server);
                        }
                        ("ready".to_string(), names, String::new())
                    }
                    Err(e) => {
                        server.shutdown();
                        ("error".to_string(), Vec::new(), format!("tools/list failed: {e}"))
                    }
                },
                Err(e) => ("error".to_string(), Vec::new(), e),
            };
            if let Ok(mut all) = STATUS.lock() {
                if let Some(s) = all.iter_mut().find(|s| s.name == conf.name) {
                    s.state = state;
                    s.tools = names;
                    s.error = error;
                }
            }
        }));
    }
    handles
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("e-mcp-test-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(p.join(".e")).unwrap();
        p
    }

    #[test]
    fn a_project_config_overrides_a_disabled_flag_and_carries_env() {
        let ws = temp("conf");
        std::fs::write(
            ws.join(".e/mcp.json"),
            r#"{ "servers": {
                   "files": { "command": "npx", "args": ["-y", "server-filesystem", "."],
                              "env": { "TOKEN": "abc" }, "cwd": "/tmp" },
                   "off":   { "command": "noop", "disabled": true },
                   "off2":  { "command": "noop", "enabled": false }
                 } }"#,
        )
        .unwrap();

        let confs = read_config(Some(ws.to_str().unwrap()));
        let files = confs.iter().find(|c| c.name == "files").expect("files");
        assert_eq!(files.args, vec!["-y", "server-filesystem", "."]);
        assert_eq!(files.env.get("TOKEN").map(String::as_str), Some("abc"));
        assert_eq!(files.cwd, "/tmp");
        assert_eq!(files.scope, "project");
        assert!(!files.disabled);
        assert!(confs.iter().find(|c| c.name == "off").unwrap().disabled);
        assert!(confs.iter().find(|c| c.name == "off2").unwrap().disabled);
        let _ = std::fs::remove_dir_all(&ws);
    }

    /// The name the model is given has to survive the provider's tool-name
    /// rules unchanged, or it cannot call back into the right tool.
    #[test]
    fn qualified_names_use_the_alphabet_providers_accept() {
        let name = format!("{PREFIX}{}_{}", slug("file system"), slug("read/file"));
        assert_eq!(name, "mcp_file_system_read_file");
        assert!(name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-'));
    }
}
