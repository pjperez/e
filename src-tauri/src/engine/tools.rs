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
    ///
    /// Empty and relative paths are refused outright rather than resolved
    /// against the process's current directory. That fallback depends on
    /// wherever the app was launched from, so it silently pointed a project at
    /// a folder inside the app's own source tree — and `write_file` then
    /// created it, leaving stray directories behind. Failing loudly sends the
    /// user to the project's ✎ to pick a real folder.
    pub fn dir(&self) -> Result<PathBuf, String> {
        let ws = self.workspace.as_path();
        if ws.as_os_str().is_empty() {
            return Err(
                "this chat has no project folder. Pick one for its project (sidebar → ✎)."
                    .to_string(),
            );
        }
        if ws.is_relative() {
            return Err(format!(
                "project folder is a relative path ({}), so it would resolve differently depending on where the app was started. Pick a real folder for this project (sidebar → ✎).",
                ws.display()
            ));
        }
        if ws.is_dir() {
            return Ok(ws.to_path_buf());
        }
        Err(format!(
            "workspace folder does not exist: {}. Pick an existing folder for this project (sidebar → + New project), or move the folder back.",
            ws.display()
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

    /// Schema for a specific workspace. Override when what the tool can do
    /// depends on the project — `skills` lists the skill packages this project
    /// actually has, so the model can name one instead of guessing.
    fn parameters_for(&self, _ctx: &ToolContext) -> Value {
        self.parameters()
    }
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

    /// Replace the plugin tool table, returning what had to be refused.
    ///
    /// A plugin tool that shadows a real one is dropped rather than accepted:
    /// the schema sent to the model must not contain the same function name
    /// twice, and the built-in would win at call time anyway, so the plugin's
    /// version would look registered while never running.
    pub fn set_plugin_tools(&self, defs: Vec<crate::engine::plugins::PluginToolDef>) -> Vec<String> {
        let mut kept: Vec<crate::engine::plugins::PluginToolDef> = Vec::new();
        let mut refused: Vec<String> = Vec::new();
        for mut d in defs {
            let raw = d.name.clone();
            d.name = Self::sanitize_tool_name(&d.name);
            let owner = if d.plugin.is_empty() { "a plugin".to_string() } else { format!("'{}'", d.plugin) };
            if self.get(&d.name).is_some() {
                refused.push(format!("{owner}: tool '{}' is already a built-in tool", d.name));
                continue;
            }
            if kept.iter().any(|k| k.name == d.name) {
                refused.push(format!("{owner}: tool '{}' is registered twice", d.name));
                continue;
            }
            if raw != d.name {
                refused.push(format!("{owner}: tool '{raw}' was renamed to '{}' (a-z, 0-9, _ and - only)", d.name));
            }
            kept.push(d);
        }
        if let Ok(mut p) = self.plugin_tools.lock() {
            *p = kept;
        }
        refused
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

    /// Drop every tool whose name starts with `prefix`. Reloading MCP servers
    /// has to retire the previous generation of `mcp:<server>/…` tools, or a
    /// server that went away would keep answering from a dead process.
    pub fn unregister_prefix(&self, prefix: &str) {
        if let Ok(mut map) = self.tools.lock() {
            map.retain(|name, _| !name.starts_with(prefix));
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
    pub fn openai_schema(&self, ctx: &ToolContext) -> Vec<Value> {
        let entries: Vec<(String, Arc<dyn Tool>)> = self
            .tools
            .lock()
            .map(|m| m.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
            .unwrap_or_default();
        let mut schema: Vec<Value> = entries
            .iter()
            .map(|(name, t)| {
                let params = t.parameters_for(ctx);
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
        "Load a skill package (SKILL.md: workflow, setup instructions, reference docs) by name and return its contents. Skills teach specialised workflows on demand — load one before doing the work it describes."
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
    /// The model cannot ask for a skill it has never heard of, so the schema
    /// carries the catalogue: every skill this project can see, with the
    /// description its author wrote.
    fn parameters_for(&self, ctx: &ToolContext) -> Value {
        let skills = crate::engine::skills::SkillStore::for_workspace(&ctx.workspace.to_string_lossy()).list();
        if skills.is_empty() {
            return self.parameters();
        }
        let listing = skills
            .iter()
            .map(|s| {
                if s.description.is_empty() {
                    s.name.clone()
                } else {
                    format!("{} — {}", s.name, s.description)
                }
            })
            .collect::<Vec<_>>()
            .join("; ");
        json!({
            "type": "object",
            "properties": {
                "name": {
                    "type": "string",
                    "enum": skills.iter().map(|s| s.name.clone()).collect::<Vec<_>>(),
                    "description": format!("Skill to load. Available: {listing}")
                }
            },
            "required": ["name"]
        })
    }
    fn run(&self, ctx: &ToolContext, args: Value) -> ToolResult {
        let name = args.get("name").and_then(|x| x.as_str()).ok_or("missing 'name'")?;
        let store = crate::engine::skills::SkillStore::for_workspace(&ctx.workspace.to_string_lossy());
        match store.get(name) {
            Some(t) => Ok(t),
            None => {
                let avail = store.list().iter().map(|s| s.name.clone()).collect::<Vec<_>>().join(", ");
                if avail.is_empty() {
                    Err(format!("skill '{name}' not found: no skills are installed. Add one at ~/.e/skills/<name>/SKILL.md"))
                } else {
                    Err(format!("skill '{name}' not found. Available: {avail}"))
                }
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
        return match crate::engine::plugins::request(name, args) {
            Ok(o) => (true, o),
            Err(e) => (false, e),
        };
    }
    let known = reg.names().join(", ");
    (false, format!("unknown tool: {name}. Available: {known}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(ws: &str) -> ToolContext {
        ToolContext { workspace: PathBuf::from(ws) }
    }

    /// A relative workspace resolves against wherever the app happened to be
    /// launched from. If something once created a matching folder there, the
    /// existence check passes and tools silently operate inside it.
    #[test]
    fn relative_workspace_is_refused_even_when_it_resolves() {
        let cwd = std::env::current_dir().unwrap();
        let stray = cwd.join("e-stray-check");
        std::fs::create_dir_all(&stray).unwrap();

        let got = ctx("e-stray-check").dir();
        let _ = std::fs::remove_dir_all(&stray);

        assert!(got.is_err(), "relative workspace resolved to {got:?}");
    }

    #[test]
    fn an_absolute_existing_workspace_is_accepted() {
        let cwd = std::env::current_dir().unwrap();
        assert_eq!(ctx(cwd.to_str().unwrap()).dir().unwrap(), cwd);
    }

    #[test]
    fn an_empty_workspace_is_refused_rather_than_using_the_launch_dir() {
        assert!(ctx("").dir().is_err());
    }

    fn plugin_tool(plugin: &str, name: &str) -> crate::engine::plugins::PluginToolDef {
        crate::engine::plugins::PluginToolDef {
            name: name.to_string(),
            description: "test".to_string(),
            parameters: json!({ "type": "object" }),
            plugin: plugin.to_string(),
        }
    }

    /// The tools array sent to the provider must not contain a name twice, and
    /// a built-in would win at call time regardless — so the plugin's copy is
    /// refused loudly instead of looking registered and never running.
    #[test]
    fn a_plugin_tool_cannot_shadow_a_built_in() {
        let reg = ToolRegistry::new();
        let refused = reg.set_plugin_tools(vec![plugin_tool("sneaky", "shell"), plugin_tool("ok", "say_hi")]);

        assert_eq!(refused.len(), 1, "{refused:?}");
        assert!(refused[0].contains("shell"), "{refused:?}");
        assert!(!reg.has_plugin_tool("shell"));
        assert!(reg.has_plugin_tool("say_hi"));

        let names: Vec<String> = reg
            .openai_schema(&ctx("/tmp"))
            .iter()
            .map(|t| t["function"]["name"].as_str().unwrap_or("").to_string())
            .collect();
        let shells = names.iter().filter(|n| *n == "shell").count();
        assert_eq!(shells, 1, "duplicate tool name in schema: {names:?}");
    }

    #[test]
    fn two_plugins_registering_the_same_tool_keep_only_the_first() {
        let reg = ToolRegistry::new();
        let refused = reg.set_plugin_tools(vec![plugin_tool("a", "dup"), plugin_tool("b", "dup")]);
        assert_eq!(refused.len(), 1, "{refused:?}");
        assert!(refused[0].contains("'b'"), "{refused:?}");
    }

    /// MCP reload retires the previous generation of tools; leaving them
    /// registered would keep routing calls into a dead subprocess.
    #[test]
    fn tools_can_be_retired_by_prefix() {
        struct Fake;
        impl Tool for Fake {
            fn name(&self) -> &str {
                "mcp:files/read"
            }
            fn description(&self) -> &str {
                "fake"
            }
            fn parameters(&self) -> Value {
                json!({ "type": "object" })
            }
            fn run(&self, _ctx: &ToolContext, _args: Value) -> ToolResult {
                Ok(String::new())
            }
        }
        let reg = ToolRegistry::new();
        reg.register(Fake);
        assert!(reg.names().iter().any(|n| n.starts_with("mcp_")));
        reg.unregister_prefix("mcp_");
        assert!(!reg.names().iter().any(|n| n.starts_with("mcp_")));
        assert!(reg.get("shell").is_some(), "built-ins must survive");
    }
}
