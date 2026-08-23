use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{mpsc, Arc, Mutex};

/// Minimal context handed to every tool when it runs.
#[derive(Clone)]
pub struct ToolContext {
    pub workspace: PathBuf,
}

impl ToolContext {
    /// The directory tools actually operate in.
    ///
    /// A stored workspace can be empty, relative, or point at a folder that no
    /// longer exists. Handing such a path to `Command::current_dir` fails with
    /// an opaque OS error ("The directory name is invalid. (os error 267)"),
    /// so resolve it up front and explain what to fix instead.
    pub fn dir(&self) -> Result<PathBuf, String> {
        let ws = self.workspace.as_path();
        let abs = if ws.as_os_str().is_empty() {
            std::env::current_dir().map_err(|e| format!("no working directory available: {e}"))?
        } else if ws.is_absolute() {
            ws.to_path_buf()
        } else {
            std::env::current_dir()
                .map_err(|e| format!("no working directory available: {e}"))?
                .join(ws)
        };
        if abs.is_dir() {
            return Ok(abs);
        }
        Err(format!(
            "workspace folder does not exist: {}. Pick an existing folder for this project (sidebar → + New project), or move the folder back.",
            abs.display()
        ))
    }
}

pub type ToolResult = Result<String, String>;

/// The extension point of `e`. Implement this trait and register the instance
/// in `ToolRegistry::register` to add a tool — the entire surface a plugin
/// author needs.
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn parameters(&self) -> Value; // JSON Schema object for `arguments`
    fn run(&self, ctx: &ToolContext, args: Value) -> ToolResult;
}

/// Shared, internally synchronised tool table.
///
/// Every method takes `&self` so a single registry can be shared (via `Arc`)
/// by concurrently running sessions. Lookups clone the `Arc<dyn Tool>` out and
/// release the lock before the tool runs, so a slow tool never blocks
/// registration or another session.
pub struct ToolRegistry {
    tools: Mutex<HashMap<String, Arc<dyn Tool>>>,
    plugin_tools: Mutex<Vec<crate::engine::plugins::PluginToolDef>>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        let r = ToolRegistry { tools: Mutex::new(HashMap::new()), plugin_tools: Mutex::new(Vec::new()) };
        r.register(ShellTool);
        r.register(ReadFileTool);
        r.register(WriteFileTool);
        r.register(ListDirTool);
        r.register(SkillsTool);
        r
    }

    pub fn set_plugin_tools(&self, defs: Vec<crate::engine::plugins::PluginToolDef>) {
        let mapped = defs
            .into_iter()
            .map(|mut d| {
                d.name = Self::sanitize_tool_name(&d.name);
                d
            })
            .collect();
        if let Ok(mut p) = self.plugin_tools.lock() {
            *p = mapped;
        }
    }

    pub fn register(&self, t: impl Tool + 'static) {
        self.register_boxed(Box::new(t));
    }

    /// OpenAI-compatible APIs require ^[a-zA-Z0-9_-]{1,64}$ for tool names.
    fn sanitize_tool_name(name: &str) -> String {
        let mut s: String = name
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' { c } else { '_' })
            .collect();
        if s.is_empty() {
            s = "tool".to_string();
        }
        s.truncate(64);
        s
    }

    pub fn register_boxed(&self, t: Box<dyn Tool>) {
        let n = Self::sanitize_tool_name(t.name());
        if let Ok(mut map) = self.tools.lock() {
            map.insert(n, Arc::from(t));
        }
    }

    pub fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.tools.lock().ok()?.get(name).cloned()
    }

    pub fn has_plugin_tool(&self, name: &str) -> bool {
        self.plugin_tools.lock().map(|p| p.iter().any(|d| d.name == name)).unwrap_or(false)
    }

    pub fn names(&self) -> Vec<String> {
        let mut v: Vec<String> = self.tools.lock().map(|m| m.keys().cloned().collect()).unwrap_or_default();
        v.sort();
        v
    }

    /// OpenAI `tools` schema describing every registered tool with its params.
    pub fn openai_schema(&self) -> Vec<Value> {
        let entries: Vec<(String, Arc<dyn Tool>)> = self
            .tools
            .lock()
            .map(|m| m.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
            .unwrap_or_default();
        let mut schema: Vec<Value> = entries
            .iter()
            .map(|(name, t)| {
                let params = t.parameters();
                let mut s = json!({
                    "type": "function",
                    "function": {
                        "name": name,
                        "description": t.description(),
                        "parameters": { "type": "object" }
                    }
                });
                if let Some(p) = params.get("properties") {
                    s["function"]["parameters"]["properties"] = p.clone();
                }
                if let Some(req) = params.get("required") {
                    s["function"]["parameters"]["required"] = req.clone();
                }
                s
            })
            .collect();
        // Stable ordering: HashMap iteration order is not deterministic and the
        // schema is part of every request (and of provider prompt caching).
        schema.sort_by(|a, b| {
            a["function"]["name"].as_str().unwrap_or("").cmp(b["function"]["name"].as_str().unwrap_or(""))
        });
        let plugins: Vec<crate::engine::plugins::PluginToolDef> =
            self.plugin_tools.lock().map(|p| p.clone()).unwrap_or_default();
        for def in &plugins {
            let params = if def.parameters.is_null() {
                json!({ "type": "object" })
            } else {
                def.parameters.clone()
            };
            let mut t = json!({
                "type": "function",
                "function": {
                    "name": def.name,
                    "description": def.description,
                    "parameters": { "type": "object" }
                }
            });
            if let Some(p) = params.get("properties") {
                t["function"]["parameters"]["properties"] = p.clone();
            }
            if let Some(r) = params.get("required") {
                t["function"]["parameters"]["required"] = r.clone();
            }
            schema.push(t);
        }
        schema
    }
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Built-in tools
// ---------------------------------------------------------------------------

pub struct ShellTool;
impl Tool for ShellTool {
    fn name(&self) -> &str {
        "shell"
    }
    fn description(&self) -> &str {
        "Run a shell command in the session workspace and return stdout, stderr and exit code."
    }
    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "command": { "type": "string", "description": "The shell command to execute." }
            },
            "required": ["command"]
        })
    }
    fn run(&self, ctx: &ToolContext, args: Value) -> ToolResult {
        use std::process::Command;
        let cmd = args
            .get("command")
            .and_then(|c| c.as_str())
            .ok_or_else(|| "missing 'command'".to_string())?
            .trim()
            .to_string();
        if cmd.is_empty() {
            return Err("empty command".to_string());
        }
        let (tx, rx) = mpsc::channel();
        let cwd = ctx.dir()?;
        std::thread::spawn(move || {
            let mut c = if cfg!(windows) {
                let mut c = Command::new("cmd");
                c.args(["/C", &cmd]);
                c
            } else {
                let mut c = Command::new("sh");
                c.arg("-c").arg(&cmd);
                c
            };
            c.current_dir(&cwd);
            let _ = tx.send(c.output());
        });
        const TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);
        let out = rx
            .recv_timeout(TIMEOUT)
            .map_err(|_| "command timed out after 120s".to_string())?
            .map_err(|e| format!("failed to start: {e}"))?;

        let stdout = String::from_utf8_lossy(&out.stdout).to_string();
        let stderr = String::from_utf8_lossy(&out.stderr).to_string();
        let code = out.status.code().unwrap_or(-1);
        let mut m = String::new();
        if !stdout.trim().is_empty() {
            m.push_str(&stdout);
        }
        if !stderr.trim().is_empty() {
            m.push_str(&format!("\n[stderr]\n{}", stderr.trim_end()));
        }
        if m.trim().is_empty() {
            m = "(no output)".to_string();
        }
        m.push_str(&format!("\n[exit code {}]", code));
        if code != 0 {
            Err(m)
        } else {
            Ok(m)
        }
    }
}

fn resolve(ctx: &ToolContext, path: &str) -> Result<PathBuf, String> {
    let p = std::path::Path::new(path);
    Ok(if p.is_absolute() { p.to_path_buf() } else { ctx.dir()?.join(p) })
}

const MAX_FILE: u64 = 200_000;

pub struct ReadFileTool;
impl Tool for ReadFileTool {
    fn name(&self) -> &str {
        "read_file"
    }
    fn description(&self) -> &str {
        "Read a text file from the workspace and return its contents (truncated if huge)."
    }
    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": { "path": { "type": "string" } },
            "required": ["path"]
        })
    }
    fn run(&self, ctx: &ToolContext, args: Value) -> ToolResult {
        let path = args.get("path").and_then(|p| p.as_str()).ok_or("missing 'path'")?;
        let full = resolve(ctx, path)?;
        let meta = std::fs::metadata(&full).map_err(|e| format!("{path}: {e}"))?;
        if meta.is_dir() {
            return Err(format!("{path} is a directory, use list_dir"));
        }
        let data = std::fs::read(&full).map_err(|e| format!("{path}: {e}"))?;
        let mut text = String::from_utf8_lossy(&data).to_string();
        if data.len() as u64 > MAX_FILE {
            let keep = MAX_FILE as usize;
            text.truncate(keep);
            text.push_str(&format!("\n… (truncated {} bytes)", data.len() - keep));
        }
        Ok(text)
    }
}

pub struct WriteFileTool;
impl Tool for WriteFileTool {
    fn name(&self) -> &str {
        "write_file"
    }
    fn description(&self) -> &str {
        "Write text content to a file in the workspace, creating parent directories."
    }
    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string" },
                "content": { "type": "string" }
            },
            "required": ["path", "content"]
        })
    }
    fn run(&self, ctx: &ToolContext, args: Value) -> ToolResult {
        let path = args.get("path").and_then(|p| p.as_str()).ok_or("missing 'path'")?.to_string();
        let content = args.get("content").and_then(|c| c.as_str()).ok_or("missing 'content'")?.to_string();
        let full = resolve(ctx, &path)?;
        if let Some(parent) = full.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("mkdir: {e}"))?;
        }
        std::fs::write(&full, content.as_bytes()).map_err(|e| format!("{path}: {e}"))?;
        Ok(format!("wrote {} bytes to {}", content.len(), path))
    }
}

pub struct ListDirTool;
impl Tool for ListDirTool {
    fn name(&self) -> &str {
        "list_dir"
    }
    fn description(&self) -> &str {
        "List the entries of a directory in the workspace."
    }
    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": { "path": { "type": "string", "description": "Directory path, defaults to workspace." } }
        })
    }
    fn run(&self, ctx: &ToolContext, args: Value) -> ToolResult {
        let p = args.get("path").and_then(|p| p.as_str()).unwrap_or(".");
        let full = resolve(ctx, p)?;
        let rd = std::fs::read_dir(&full).map_err(|e| format!("{p}: {e}"))?;
        let mut lines: Vec<String> = Vec::new();
        for e in rd.flatten() {
            let is_dir = e.file_type().map(|t| t.is_dir()).unwrap_or(false);
            let name = e.file_name().to_string_lossy().to_string();
            lines.push(if is_dir { format!("{name}/") } else { name });
        }
        lines.sort();
        Ok(lines.join("\n"))
    }
}

pub struct SkillsTool;
impl Tool for SkillsTool {
    fn name(&self) -> &str {
        "skills"
    }
    fn description(&self) -> &str {
        "Load a skill package (SKILL.md: workflow, setup instructions, reference docs) by name and return its contents. Skills teach specialised workflows on demand."
    }
    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "name": { "type": "string", "description": "Skill name" }
            },
            "required": ["name"]
        })
    }
    fn run(&self, _ctx: &ToolContext, args: Value) -> ToolResult {
        let name = args.get("name").and_then(|x| x.as_str()).ok_or("missing 'name'")?;
        let store = crate::engine::skills::SkillStore::new();
        match store.get(name) {
            Some(t) => Ok(t),
            None => {
                let avail = store.list().iter().map(|s| s.name.as_str()).collect::<Vec<_>>().join(", ");
                Err(format!("skill '{name}' not found. Available: {avail}"))
            }
        }
    }
}

/// Run a tool, returning (success, output). Shared helper for plugin authors.
pub fn run_tool(reg: &ToolRegistry, ctx: &ToolContext, name: &str, args: Value) -> (bool, String) {
    // Clone the handle out and drop the registry lock before running: tools can
    // block for a long time (shell has a 120s cap) and must not stall the app.
    if let Some(t) = reg.get(name) {
        return match t.run(ctx, args) {
            Ok(o) => (true, o),
            Err(e) => (false, e),
        };
    }
    if reg.has_plugin_tool(name) {
        let sid = format!(
            "pt{}_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis())
                .unwrap_or(0),
            PLUGIN_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        );
        return match crate::engine::plugins::request(&sid, name, args) {
            Ok(o) => (true, o),
            Err(e) => (false, e),
        };
    }
    (false, format!("unknown tool: {name}"))
}

/// Millisecond timestamps collide when two plugin tools are called in the same
/// tick, which used to make one call steal the other's reply.
static PLUGIN_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
