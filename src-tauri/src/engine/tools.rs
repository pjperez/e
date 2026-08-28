use serde_json::{json, Value};
use std::time::Duration;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};

use crate::engine::jobs;

/// Upper bound on how much captured output one synchronous command returns.
/// The old implementation returned everything it printed; a chatty build
/// could drown the transcript. 100k chars matches the read_file cap's order
/// of magnitude, and anything bigger genuinely needs the background path.
const SYNC_MAX: usize = 100_000;

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
        r.register(PowerShellTool);
        r.register(ProcessPollTool);
        r.register(ProcessKillTool);
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
            // `shell` stays reserved even though it is no longer a built-in:
            // otherwise a plugin could quietly put a Bash-capable generic shell
            // back into the agent's advertised tool set.
            if self.get(&d.name).is_some() || d.name == "shell" {
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

/// Format captured stdout/stderr for a synchronous command result, keeping
/// the shape the model already knows: stdout, an optional [stderr] section,
/// then the exit code. Oversized output is trimmed at the front with a note.
fn format_sync_output(out: &jobs::Taken, err: &jobs::Taken, code: i32) -> String {
    let mut m = String::new();
    if !out.text.trim().is_empty() {
        m.push_str(&out.text);
    }
    if !err.text.trim().is_empty() {
        m.push_str(&format!("\n[stderr]\n{}", err.text.trim_end()));
    }
    if m.trim().is_empty() {
        m = "(no output)".to_string();
    }
    if out.skipped > 0 {
        m = format!("[… truncated {} earlier bytes of stdout]\n{}", out.skipped, m);
    }
    if err.skipped > 0 {
        m.push_str(&format!("\n[… {} earlier bytes of stderr were truncated]", err.skipped));
    }
    m.push_str(&format!("\n[exit code {code}]"));
    m
}

pub struct PowerShellTool;
impl Tool for PowerShellTool {
    fn name(&self) -> &str {
        "powershell"
    }
    fn description(&self) -> &str {
        "Run a PowerShell command in the session workspace and return stdout, stderr and exit code. Blocks at most 120s; pass background:true for anything that may run longer (builds, deploys, dev servers) to start it as a background job and get a job id back immediately — then poll it with process_poll. Bash, cmd.exe, and POSIX shell syntax are not supported."
    }
    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "command": { "type": "string", "description": "The PowerShell command to execute." },
                "background": { "type": "boolean", "description": "Run as a background job: return a job id immediately instead of blocking, then poll with process_poll." }
            },
            "required": ["command"]
        })
    }
    fn run(&self, ctx: &ToolContext, args: Value) -> ToolResult {
        let cmd = args
            .get("command")
            .and_then(|c| c.as_str())
            .ok_or_else(|| "missing 'command'".to_string())?
            .trim()
            .to_string();
        if cmd.is_empty() {
            return Err("empty command".to_string());
        }
        let cwd = ctx.dir()?;
        if args.get("background").and_then(|b| b.as_bool()).unwrap_or(false) {
            let id = jobs::start(&cwd, &cmd)?;
            return Ok(format!(
                "[job {id}] started in the background; the command keeps running.\nPoll with process_poll(id=\"{id}\", wait_ms=20000) for progress — its wait blocks so one long poll beats many short ones — and stop it with process_kill(id=\"{id}\") if needed."
            ));
        }
        run_sync(&cwd, &cmd)
    }
}

/// Run a command to completion (or 120s), capturing output as it prints so a
/// timeout can report what the command managed to say before dying — and
/// kill it, instead of leaving an orphan no one can account for.
fn run_sync(cwd: &std::path::Path, cmd: &str) -> ToolResult {
    use std::process::Stdio;

    let mut c = crate::engine::quiet_command(jobs::shell_executable());
    c.args(["-NoLogo", "-NoProfile", "-NonInteractive", "-Command", cmd]);
    c.current_dir(cwd);
    c.stdin(Stdio::null());
    c.stdout(Stdio::piped());
    c.stderr(Stdio::piped());
    let mut child = c.spawn().map_err(|e| format!("failed to start: {e}"))?;
    let stdout = child.stdout.take().ok_or("command has no stdout pipe")?;
    let stderr = child.stderr.take().ok_or("command has no stderr pipe")?;
    let pid = child.id();

    let out_cap = Arc::new(Mutex::new(jobs::Capture::new()));
    let err_cap = Arc::new(Mutex::new(jobs::Capture::new()));
    let r1 = jobs::spawn_reader(stdout, out_cap.clone());
    let r2 = jobs::spawn_reader(stderr, err_cap.clone());

    // The child is waited on in a thread so this thread can bound the wait;
    // the pid stays here so a timeout can actually kill the tree.
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let code = child.wait().map(|s| s.code().unwrap_or(-1)).unwrap_or(-1);
        let _ = tx.send(code);
    });

    const TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);
    match rx.recv_timeout(TIMEOUT) {
        Ok(code) => {
            // Join the readers with a budget: on the happy path the pipes are
            // already at EOF and this returns at once, but a grandchild that
            // inherited a write end must not hang the tool forever.
            jobs::drain(vec![r1, r2], Duration::from_secs(5));
            let out = out_cap.lock().unwrap_or_else(|e| e.into_inner()).tail(SYNC_MAX);
            let err = err_cap.lock().unwrap_or_else(|e| e.into_inner()).tail(SYNC_MAX);
            let m = format_sync_output(&out, &err, code);
            if code != 0 {
                Err(m)
            } else {
                Ok(m)
            }
        }
        Err(_) => {
            jobs::kill_tree(pid);
            // Give the pipes a moment to finish draining after the kill so
            // the tail carries the command's last words, not everything but.
            jobs::drain(vec![r1, r2], Duration::from_secs(5));
            let out = out_cap.lock().unwrap_or_else(|e| e.into_inner()).tail(jobs::MAX_POLL);
            let err = err_cap.lock().unwrap_or_else(|e| e.into_inner()).tail(jobs::MAX_POLL);
            let mut m = format!(
                "command timed out after 120s and was killed (process tree terminated). Output before the kill:"
            );
            if out.text.trim().is_empty() && err.text.trim().is_empty() {
                m.push_str("\n(it printed nothing)");
            } else {
                if !out.text.trim().is_empty() {
                    m.push_str(&format!("\n{}", out.text));
                }
                if !err.text.trim().is_empty() {
                    m.push_str(&format!("\n[stderr]\n{}", err.text.trim_end()));
                }
            }
            Err(m)
        }
    }
}

/// Long-poll one background job: block up to `wait_ms` for new output or
/// exit, then report status plus everything printed since the last poll.
pub struct ProcessPollTool;
impl Tool for ProcessPollTool {
    fn name(&self) -> &str {
        "process_poll"
    }
    fn description(&self) -> &str {
        "Poll a background job started with powershell background:true. Blocks up to wait_ms (default 20000, max 25000) waiting for new output or exit, then returns the job's status and everything it printed since the previous poll. Prefer one long poll over repeated immediate ones, and do other work between polls instead of spinning."
    }
    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "id": { "type": "string", "description": "Job id returned by powershell background:true." },
                "wait_ms": { "type": "integer", "description": "How long to wait for new output or exit before reporting (ms). Default 20000, max 25000." }
            },
            "required": ["id"]
        })
    }
    fn run(&self, _ctx: &ToolContext, args: Value) -> ToolResult {
        let id = args.get("id").and_then(|i| i.as_str()).ok_or("missing 'id'")?;
        let wait_ms = args.get("wait_ms").and_then(|w| w.as_u64()).unwrap_or(20_000);
        let wait = std::time::Duration::from_millis(wait_ms.clamp(0, jobs::MAX_WAIT.as_millis() as u64));
        jobs::poll(id, wait)
    }
}

/// Stop a background job's whole process tree.
pub struct ProcessKillTool;
impl Tool for ProcessKillTool {
    fn name(&self) -> &str {
        "process_kill"
    }
    fn description(&self) -> &str {
        "Kill a background job started with powershell background:true — the process and everything it spawned. Use when a job is stuck, failed, or no longer needed. Poll the id afterwards to see its final output."
    }
    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "id": { "type": "string", "description": "Job id returned by powershell background:true." }
            },
            "required": ["id"]
        })
    }
    fn run(&self, _ctx: &ToolContext, args: Value) -> ToolResult {
        let id = args.get("id").and_then(|i| i.as_str()).ok_or("missing 'id'")?;
        jobs::kill(id)
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

    use std::time::Duration;

    fn ctx(ws: &str) -> ToolContext {
        ToolContext { workspace: PathBuf::from(ws) }
    }

    /// Pull the job id out of the powershell background:true message. The id
    /// sits inside "[job bg-…] started in the background", so a naive
    /// split_whitespace picks up the trailing ']'.
    fn extract_id(out: &str) -> String {
        out.split_whitespace()
            .find(|w| w.starts_with("bg-"))
            .map(|w| w.trim_end_matches(']').to_string())
            .expect("job id in output")
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
        let refused = reg.set_plugin_tools(vec![
            plugin_tool("sneaky", "powershell"),
            plugin_tool("bash-backdoor", "shell"),
            plugin_tool("ok", "say_hi"),
        ]);

        assert_eq!(refused.len(), 2, "{refused:?}");
        assert!(refused.iter().any(|r| r.contains("powershell")), "{refused:?}");
        assert!(refused.iter().any(|r| r.contains("shell")), "{refused:?}");
        assert!(!reg.has_plugin_tool("shell"));
        assert!(!reg.has_plugin_tool("powershell"));
        assert!(reg.has_plugin_tool("say_hi"));

        let names: Vec<String> = reg
            .openai_schema(&ctx("/tmp"))
            .iter()
            .map(|t| t["function"]["name"].as_str().unwrap_or("").to_string())
            .collect();
        assert!(!names.iter().any(|n| n == "shell"), "generic shell leaked into schema: {names:?}");
        assert_eq!(names.iter().filter(|n| *n == "powershell").count(), 1, "duplicate tool name: {names:?}");
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
        assert!(reg.get("powershell").is_some(), "built-ins must survive");
    }

    // -- background flag / poll / kill ---------------------------------------

    #[test]
    fn the_process_tools_are_registered() {
        let reg = ToolRegistry::new();
        assert!(reg.get("process_poll").is_some());
        assert!(reg.get("process_kill").is_some());
    }

    #[test]
    fn background_true_returns_a_job_id_without_running_the_command_to_completion() {
        let dir = std::env::temp_dir();
        let started = std::time::Instant::now();
        let (ok, out) = run_tool(
            &ToolRegistry::new(),
            &ctx(dir.to_str().unwrap()),
            "powershell",
            json!({ "command": "Start-Sleep -Seconds 30", "background": true }),
        );
        assert!(ok, "{out}");
        assert!(out.contains("started in the background"), "{out}");
        let id = extract_id(&out);
        assert!(started.elapsed() < Duration::from_secs(10), "must not block: {out}");
        // Clean up so the test run does not leave a sleeping shell behind.
        assert!(jobs::kill(&id).is_ok());
    }

    #[test]
    fn the_full_background_cycle_works_end_to_end() {
        let reg = ToolRegistry::new();
        let dir = std::env::temp_dir();
        let (ok, out) = run_tool(
            &reg,
            &ctx(dir.to_str().unwrap()),
            "powershell",
            json!({ "command": "Write-Output cycle-marker", "background": true }),
        );
        assert!(ok, "{out}");
        let id = extract_id(&out);

        // Poll blocks for new output / exit and reports both.
        let (ok, poll_out) = run_tool(&reg, &ctx(dir.to_str().unwrap()), "process_poll", json!({ "id": id, "wait_ms": 20000 }));
        assert!(ok, "{poll_out}");
        assert!(poll_out.contains("exited with code 0"), "{poll_out}");
        assert!(poll_out.contains("cycle-marker"), "{poll_out}");
    }

    #[test]
    fn polling_a_made_up_id_fails_with_a_readable_error() {
        let (ok, out) = run_tool(
            &ToolRegistry::new(),
            &ctx(std::env::temp_dir().to_str().unwrap()),
            "process_poll",
            json!({ "id": "bg-nope" }),
        );
        assert!(!ok);
        assert!(out.contains("no such background job"), "{out}");
    }

    /// The synchronous path keeps its contract: real output, exit code, and
    /// a non-zero exit surfaced as an error.
    #[test]
    fn sync_commands_still_return_output_and_exit_code() {
        let dir = std::env::temp_dir();
        let (ok, out) = run_tool(
            &ToolRegistry::new(),
            &ctx(dir.to_str().unwrap()),
            "powershell",
            json!({ "command": "Write-Output sync-marker; Write-Error boom -ErrorAction Continue" }),
        );
        assert!(!ok, "a failing command must be an Err: {out}");
        assert!(out.contains("sync-marker"), "{out}");
        assert!(out.contains("boom"), "stderr must be included: {out}");
        assert!(out.contains("[exit code 1]") || out.contains("[exit code"), "{out}");
    }

    /// Output is captured as it prints, so a command that is killed at the
    /// cap still reports what it said before dying. (Uses the same machinery
    /// with a short timeout by going through jobs directly.)
        /// Output that has already been read into the capture survives a kill.
    /// A force-kill can lose output still buffered in the dying process, so
    /// the command holds the marker long enough for the reader to drain it
    /// before the kill lands.
        /// Output that has already been read into the capture survives a kill.
    /// A force-kill can lose output still buffered in the dying process, so
    /// the command holds the marker long enough for the reader to drain it
    /// before the kill lands. The single post-kill poll blocks until the
    /// status flips, then renders the unconsumed marker in the same call —
    /// a polling loop would consume the marker on an early "running" poll.
        /// Output that has already been read into the capture survives a kill.
    /// A force-kill can lose output still buffered in the dying process, so
    /// the command holds the marker long enough for the reader to drain it
    /// before the kill lands. Polls accumulate across calls, because an early
    /// poll can consume the marker while the kill is still settling.
    #[test]
    fn output_printed_before_a_kill_is_not_lost() {
        let dir = std::env::temp_dir();
        let id = jobs::start(
            &dir,
            "Write-Output dying-words; Start-Sleep -Milliseconds 800; Start-Sleep -Seconds 60",
        )
        .expect("start");
        // Let the reader thread drain the marker into the capture before the
        // kill, so the assertion is on the drained tail, not a race with it.
        std::thread::sleep(Duration::from_millis(2000));
        let _ = jobs::kill(&id);
        let deadline = std::time::Instant::now() + Duration::from_secs(15);
        let mut seen = String::new();
        loop {
            let out = jobs::poll(&id, Duration::from_millis(500)).expect("poll");
            seen.push_str(&out);
            if out.contains("killed by request") {
                break;
            }
            assert!(std::time::Instant::now() < deadline, "never killed: {out}");
        }
        assert!(seen.contains("dying-words"), "the tail must survive the kill: {seen}");
    }
}
