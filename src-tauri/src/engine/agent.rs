use crate::engine::provider::ChatProvider;
use crate::engine::tools::{run_tool, ToolContext, ToolRegistry};
use crate::engine::{Completion, Emitter, Msg, Part};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// Configuration for the harness engine.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct Config {
    pub base_url: String,
    pub api_key: String,
    pub model: String,
    pub temperature: f64,
    pub system: String,
    pub workspace: String,
    /// Auto-approve risky tools (shell, write_file) instead of prompting.
    #[serde(default)]
    pub yolo: bool,
    #[serde(default)]
    pub models: Vec<String>,
    /// Usable context window (tokens) for the active model. Compaction is
    /// triggered off this, so getting it right per provider/model matters.
    #[serde(default = "default_context_window")]
    pub context_window: u64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub providers: Vec<ProviderItem>,
}

pub fn default_context_window() -> u64 {
    1_000_000
}

/// Fraction of the context window at which we compact. Deliberately high: the
/// point is to avoid overflowing, not to keep the window small.
pub const COMPACT_AT: f64 = 0.85;

/// A saved provider configuration; the active connection is the flat
/// base_url/api_key/model/models fields above.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct ProviderItem {
    pub id: String,
    pub name: String,
    pub base_url: String,
    pub api_key: String,
    pub models: Vec<String>,
    /// Context window for this provider's models; falls back to the global
    /// default when unset (older config files won't have it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window: Option<u64>,
}

impl Config {
    pub fn from_env() -> Self {
        let home = dirs::home_dir().unwrap_or_default().join(".e");
        let cfg_file = home.join("config.json");
        let mut base = Config {
            base_url: "https://provider.example/v1".to_string(),
            api_key: String::new(),
            model: "opencode-go/deepseek-v4-flash".to_string(),
            temperature: 0.7,
            system: "You are e, a fast, capable coding agent running in a local harness with a workspace and tools: shell (run commands), read_file, write_file, list_dir.\nTool use policy: use a tool ONLY when it genuinely helps (inspect/read files, run or verify commands, modify the workspace, or when the user asks you to act). For conversational or directly-answerable requests, answer directly yourself and NEVER make a tool call.".to_string(),
            workspace: std::env::current_dir().unwrap_or_default().to_string_lossy().to_string(),
            yolo: false,
            context_window: default_context_window(),
            providers: vec![ProviderItem {
                id: "aigateway".into(),
                name: "AI Gateway".into(),
                base_url: "https://provider.example/v1".into(),
                api_key: String::new(),
                context_window: None,
                models: vec![
                    "zai-coding/glm-5.2".into(),
                    "openrouter/deepseek/deepseek-v4-flash-0731".into(),
                    "opencode-go/deepseek-v4-flash".into(),
                    "opencode-go/deepseek-v4-pro".into(),
                    "openai/gpt-5.6-luna".into(),
                    "command-code/inclusionai/ling-3.0-flash-free".into(),
                ],
            }],
            models: vec![
                "zai-coding/glm-5.2".into(),
                "openrouter/deepseek/deepseek-v4-flash-0731".into(),
                "opencode-go/deepseek-v4-flash".into(),
                "opencode-go/deepseek-v4-pro".into(),
                "openai/gpt-5.6-luna".into(),
                "command-code/inclusionai/ling-3.0-flash-free".into(),
            ],
        };

        if let Ok(text) = std::fs::read_to_string(&cfg_file) {
            if let Ok(c) = serde_json::from_str::<Config>(&text) {
                merge(&mut base, c);
            }
        }
        if let Ok(k) = std::env::var("E_API_KEY") {
            if !k.is_empty() {
                base.api_key = k;
            }
        }
        if let Ok(v) = std::env::var("E_BASE_URL") {
            if !v.is_empty() {
                base.base_url = v;
            }
        }
        if let Ok(v) = std::env::var("E_MODEL") {
            if !v.is_empty() {
                base.model = v;
            }
        }
        if let Ok(v) = std::env::var("E_WORKSPACE") {
            if !v.is_empty() {
                base.workspace = v;
            }
        }
        if let Ok(v) = std::env::var("E_YOLO") {
            if let Some(b) = parse_bool(&v) {
                base.yolo = b;
            }
        }
        // Ensure the first provider reflects the active connection so the key
        // actually carries through (fixes Refresh / picker showing empty key).
        if let Some(p) = base.providers.first_mut() {
            if p.base_url.is_empty() {
                p.base_url = base.base_url.clone();
            }
            if p.api_key.is_empty() {
                p.api_key = base.api_key.clone();
            }
            if p.models.is_empty() {
                p.models = base.models.clone();
            }
        }
        base
    }

    /// Context window to budget against: the active provider's override when it
    /// has one, otherwise the global setting.
    pub fn active_context_window(&self) -> u64 {
        let win = self
            .providers
            .iter()
            .find(|p| p.base_url == self.base_url)
            .and_then(|p| p.context_window)
            .unwrap_or(self.context_window);
        if win == 0 {
            default_context_window()
        } else {
            win
        }
    }

    pub fn save(&self) {
        let home = dirs::home_dir().unwrap_or_default().join(".e");
        if std::fs::create_dir_all(&home).is_ok() {
            let _ = std::fs::write(
                home.join("config.json"),
                serde_json::to_string_pretty(self).unwrap_or_default(),
            );
        }
    }
}

/// Auto-detected platform note, injected dynamically at each startup so it
/// always matches the OS the harness is running on right now.
fn platform_hint() -> String {
    let arch = std::env::consts::ARCH;
    if cfg!(windows) {
        format!(
            "PLATFORM (auto-detected at startup): Windows ({arch}).
The shell tool runs **cmd.exe** — use Windows command syntax (dir, type, copy, del, rmdir /s, where).
Unix utilities (ls, cat, grep -r, rm -rf, chmod, tree, touch) are NOT reliably available.
Prefer the list_dir tool to list files, read_file to read, and write_file to edit. Path separator is backslash."
        )
    } else {
        format!(
            "PLATFORM (auto-detected at startup): {} on {arch}. The shell tool runs POSIX sh — use standard Unix commands. Path separator is forward slash.",
            std::env::consts::OS
        )
    }
}

fn merge(base: &mut Config, c: Config) {
    if !c.base_url.is_empty() {
        base.base_url = c.base_url;
    }
    if !c.api_key.is_empty() {
        base.api_key = c.api_key;
    }
    if !c.model.is_empty() {
        base.model = c.model;
    }
    if (0.0..=2.0).contains(&c.temperature) {
        base.temperature = c.temperature;
    }
    if !c.system.is_empty() {
        base.system = c.system;
    }
    if !c.workspace.is_empty() {
        base.workspace = c.workspace;
    }
    if !c.models.is_empty() {
        base.models = c.models;
    }
    if !c.providers.is_empty() {
        base.providers = c.providers;
    }
    if c.context_window > 0 {
        base.context_window = c.context_window;
    }
    // Copied unconditionally: the "skip empty values" rule used above cannot
    // express a bool the user deliberately turned off.
    base.yolo = c.yolo;
}

/// Lenient bool parsing for env overrides; unrecognised values leave the
/// configured value untouched rather than silently disabling the flag.
fn parse_bool(v: &str) -> Option<bool> {
    match v.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
}

/// What the agent should believe about where it is working.
///
/// Built per run from the chat's own project, so a chat is never told about a
/// folder that belongs to a different one.
#[derive(Clone, Debug, Default)]
pub struct ProjectContext {
    pub name: String,
    /// True for the scratch "Tasks" area: one-off work with no project behind
    /// it. The agent must not assume a codebase in that case.
    pub scratch: bool,
    /// True when the chat's folder is not the folder of the project it is filed
    /// under — its original project was deleted, or it was pointed elsewhere.
    /// The project name is then just a label and must not be presented as fact.
    pub detached: bool,
}

/// The agent: holds provider, tools, config and the running conversation.
pub struct Agent {
    pub provider: ChatProvider,
    /// Shared with every other session, so MCP/plugin tools registered once are
    /// visible to all runs.
    pub tools: Arc<ToolRegistry>,
    pub config: Config,
    /// The session this agent is running for; used to route approval prompts.
    pub session: String,
    /// The project this chat belongs to; drives the system prompt.
    pub project: ProjectContext,
    history: Vec<Msg>,
    /// Optional hook to persist history as it grows (called per step).
    pub save: Option<Box<dyn Fn(&[Msg]) + Send + Sync>>,
}

pub struct RunStats {
    pub steps: usize,
    pub tool_calls: usize,
    pub stopped: bool,
    pub tokens_in: u64,
    pub tokens_out: u64,
    /// Prompt tokens of the most recent provider call — the live context size.
    pub context_tokens: u64,
    pub cost: Option<f64>,
    pub error: Option<String>,
}

impl RunStats {
    pub fn to_summary(&self) -> crate::engine::RunSummary {
        crate::engine::RunSummary {
            steps: self.steps,
            tool_calls: self.tool_calls,
            stopped: self.stopped,
            tokens_in: self.tokens_in,
            tokens_out: self.tokens_out,
            context_tokens: self.context_tokens,
            cost: self.cost,
            error: self.error.clone(),
        }
    }
}

impl Agent {
    pub fn new(config: Config) -> Self {
        Agent::with_tools(config, Arc::new(ToolRegistry::new()))
    }

    /// Build an agent that shares an existing tool registry. Used per run so
    /// concurrent sessions never fight over one mutable agent.
    pub fn with_tools(config: Config, tools: Arc<ToolRegistry>) -> Self {
        let provider = ChatProvider::new(
            config.base_url.clone(),
            config.api_key.clone(),
            config.model.clone(),
            config.temperature,
        );
        Agent {
            provider,
            tools,
            config,
            session: String::new(),
            project: ProjectContext::default(),
            history: Vec::new(),
            save: None,
        }
    }

    pub fn reload(&mut self, config: Config) {
        self.config = config.clone();
        self.provider = ChatProvider::new(
            config.base_url,
            config.api_key,
            config.model,
            config.temperature,
        );
    }

    pub fn reset(&mut self) {
        self.history.clear();
    }

    pub fn history(&self) -> Vec<Msg> {
        self.history.clone()
    }

    pub fn set_history(&mut self, h: Vec<Msg>) {
        self.history = h;
    }

    fn workspace_ctx(&self) -> ToolContext {
        ToolContext { workspace: std::path::PathBuf::from(&self.config.workspace) }
    }

    /// Where this chat is working, spelled out for the model. Being explicit
    /// about "no project" matters as much as naming one: told only a bare path,
    /// a model happily assumes the last project it heard about.
    fn project_block(&self) -> String {
        let ws = self.config.workspace.trim();
        let name = self.project.name.trim();
        if self.project.scratch {
            format!(
                "PROJECT: none. This chat lives in \"{}\", the area for one-off work that does not belong to any project.\n\
                 Working folder (tools run here; relative paths resolve here): {ws}\n\
                 That folder is an empty scratch directory, not a codebase. There is no project, repository or existing code here: do not assume you are continuing any previous work, and do not guess at a project. If the request needs one, ask which folder to use or work from an absolute path the user gives you.",
                if name.is_empty() { "Tasks" } else { name }
            )
        } else if self.project.detached {
            format!(
                "PROJECT: unresolved. This chat is filed under \"{}\", but that is only where it is listed — the folder below is not that project's folder. The project it belonged to was deleted, or it was pointed somewhere else.\n\
                 Working folder (tools run here; relative paths resolve here): {ws}\n\
                 Trust the folder, not the label: the folder is the only reliable statement about where you are working. Look at it to see what it actually is, and do not claim or assume this chat belongs to any particular project.",
                if name.is_empty() { "(unnamed)" } else { name }
            )
        } else {
            format!(
                "PROJECT: {}\n\
                 Working folder (tools run here; relative paths resolve here): {ws}\n\
                 This chat is scoped to that project and only that project. Look in this folder to learn what it is instead of assuming — never work against a different project, repository or folder.",
                if name.is_empty() { "(unnamed)" } else { name }
            )
        }
    }

    /// Put platform + project context at the top of the conversation, replacing
    /// any earlier copy.
    ///
    /// Rebuilt every turn on purpose: the system message is persisted with the
    /// history, so a chat whose project folder was repointed (or which was
    /// written before this context existed) would otherwise keep quoting the
    /// old, wrong location forever.
    fn sync_system(&mut self) {
        let hint = format!("{}\n\n{}", platform_hint(), self.project_block());
        let body = if self.config.system.trim().is_empty() {
            hint
        } else {
            format!("{}\n\n{}", hint, self.config.system.trim())
        };
        match self.history.iter().position(|m| m.role == "system") {
            Some(i) => self.history[i] = Msg::text("system", body),
            None => self.history.insert(0, Msg::text("system", body)),
        }
    }

    /// Run one full turn: take a user message, then loop calling the model and
    /// executing tools until the model stops requesting tools (or a cap/stop).
    pub async fn run<F>(&mut self, user_text: &str, images: &[String], emit: &F, cancelled: &AtomicBool) -> RunStats
    where
        F: Emitter,
    {
        self.sync_system();
        let mut parts: Vec<Part> = vec![Part::Text(user_text.to_string())];
        for url in images {
            if !url.is_empty() {
                parts.push(Part::ImageData(url.clone()));
            }
        }
        self.history.push(Msg { role: "user".into(), parts });
        self.persist();

        let mut stats = RunStats { steps: 0, tool_calls: 0, stopped: false, tokens_in: 0, tokens_out: 0, context_tokens: 0, cost: None, error: None };

        loop {
            if cancelled.load(Ordering::SeqCst) {
                return finish_stopped(stats, emit);
            }

            let schema = self.tools.openai_schema();
            emit.activity("thinking", None, stats.steps + 1);
            let completion: Completion = match self
                .provider
                .chat(&self.history, &schema, |tok| emit.token(tok), |r| emit.reasoning(r), cancelled)
                .await
            {
                Ok(c) => c,
                Err(e) => {
                    stats.error = Some(e.clone());
                    // Record the failure in the transcript, not just as a live
                    // event: an event-only error disappears the moment the user
                    // switches chats or restarts, leaving a chat flagged
                    // "error" with nothing on screen saying what went wrong.
                    self.history.push(Msg::error(e.clone()));
                    self.persist();
                    emit.error(&e);
                    emit.summary(&stats.to_summary());
                    emit.done(false);
                    return stats;
                }
            };

            let mut parts: Vec<Part> = Vec::new();
            if !completion.text.is_empty() {
                parts.push(Part::Text(completion.text.clone()));
            }
            if !completion.reasoning.is_empty() {
                parts.push(Part::Reasoning(completion.reasoning.clone()));
            }
            for tc in &completion.tool_calls {
                parts.push(Part::ToolCall(tc.clone()));
            }
            let produced_anything = !parts.is_empty();
            if produced_anything {
                self.history.push(Msg::assistant(parts));
            }

            stats.steps += 1;
            stats.tokens_in += completion.usage.0;
            stats.tokens_out += completion.usage.1;
            // Overwrite, don't accumulate: this is the size of the window we
            // just sent, which is what context-pressure decisions need.
            if completion.usage.0 > 0 {
                stats.context_tokens = completion.usage.0;
            }
            if let Some(c) = completion.cost {
                stats.cost = Some(stats.cost.unwrap_or(0.0) + c);
            }

            // Stopping mid-stream still keeps whatever the model already said.
            if cancelled.load(Ordering::SeqCst) {
                if produced_anything {
                    emit.message_end();
                    self.persist();
                }
                return finish_stopped(stats, emit);
            }

            for tc in &completion.tool_calls {
                emit.tool_call(tc);
            }
            if produced_anything {
                emit.message_end();
                self.persist();
            }

            if completion.tool_calls.is_empty() {
                emit.summary(&stats.to_summary());
                emit.done(false);
                return stats;
            }

            const RISKY: [&str; 2] = ["shell", "write_file"];
            let ctx = self.workspace_ctx();
            for tc in &completion.tool_calls {
                if cancelled.load(Ordering::SeqCst) {
                    // Every requested call needs a result or the next request is
                    // malformed (providers reject dangling tool_call ids).
                    let msg = "Stopped by user";
                    self.history.push(Msg::tool_result(&tc.id, &tc.name, msg.to_string()));
                    emit.tool_result(&tc.id, &tc.name, false, msg);
                    continue;
                }
                emit.activity("tool", Some(tc.name.as_str()), stats.steps);
                if RISKY.contains(&tc.name.as_str()) && !self.config.yolo {
                    let preview = tool_preview(tc);
                    if !crate::engine::approval::request(&self.session, &tc.name, &preview, cancelled) {
                        let msg = if cancelled.load(Ordering::SeqCst) { "Stopped by user" } else { "Denied by user" };
                        self.history.push(Msg::tool_result(&tc.id, &tc.name, msg.to_string()));
                        emit.tool_result(&tc.id, &tc.name, false, msg);
                        continue;
                    }
                }
                let (ok, output) = run_tool(&self.tools, &ctx, &tc.name, tc.arguments.clone());
                stats.tool_calls += 1;
                self.history.push(Msg::tool_result(&tc.id, &tc.name, output.clone()));
                emit.tool_result(&tc.id, &tc.name, ok, &output);
                self.persist();
            }

            if cancelled.load(Ordering::SeqCst) {
                return finish_stopped(stats, emit);
            }
        }
    }

    fn persist(&self) {
        if let Some(s) = &self.save {
            s(&self.history);
        }
    }
}

fn finish_stopped<F: Emitter>(mut stats: RunStats, emit: &F) -> RunStats {
    stats.stopped = true;
    emit.summary(&stats.to_summary());
    emit.done(true);
    stats
}

/// A short, human-readable description of what a tool call is about to do,
/// shown in the approval prompt so the user can decide without guessing.
fn tool_preview(tc: &crate::engine::ToolCall) -> String {
    let raw = tc
        .arguments
        .get("command")
        .or_else(|| tc.arguments.get("path"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| tc.arguments.to_string());
    let mut s: String = raw.chars().take(300).collect();
    if s.chars().count() < raw.chars().count() {
        s.push('…');
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agent(workspace: &str, project: ProjectContext) -> Agent {
        let mut cfg = Config::from_env();
        cfg.workspace = workspace.to_string();
        cfg.system = "House rules.".into();
        let mut a = Agent::new(cfg);
        a.project = project;
        a
    }

    fn system_of(a: &Agent) -> String {
        a.history()
            .iter()
            .find(|m| m.role == "system")
            .map(|m| m.plain_text_parts())
            .unwrap_or_default()
    }

    #[test]
    fn a_project_chat_is_told_which_project_it_is_in() {
        let mut a = agent(
            "C:/src/mascot",
            ProjectContext { name: "mascot".into(), scratch: false, detached: false },
        );
        a.sync_system();
        let s = system_of(&a);
        assert!(s.contains("PROJECT: mascot"), "{s}");
        assert!(s.contains("C:/src/mascot"), "{s}");
        assert!(s.contains("House rules."), "{s}");
    }

    /// Without this the model is handed a bare folder path and cheerfully
    /// assumes it is still working on the last project it heard about.
    #[test]
    fn a_scratch_chat_is_told_there_is_no_project() {
        let mut a = agent(
            "C:/Users/x/.e/tasks",
            ProjectContext { name: "Tasks".into(), scratch: true, detached: false },
        );
        a.sync_system();
        let s = system_of(&a);
        assert!(s.contains("PROJECT: none"), "{s}");
        assert!(s.contains("Tasks"), "{s}");
        assert!(!s.contains("mascot"), "{s}");
    }

    /// After its project is deleted a chat is refiled elsewhere but keeps its
    /// folder, so the prompt must not present that label as the project.
    #[test]
    fn a_detached_chat_is_told_not_to_trust_its_label() {
        let mut a = agent(
            "C:/src/mascot",
            ProjectContext { name: "Tasks".into(), scratch: false, detached: true },
        );
        a.sync_system();
        let s = system_of(&a);
        assert!(s.contains("PROJECT: unresolved"), "{s}");
        assert!(s.contains("C:/src/mascot"), "{s}");
        assert!(!s.contains("PROJECT: Tasks"), "{s}");
    }

    /// The system message is persisted with the history, so a chat written
    /// before this context existed (or repointed at another folder) must be
    /// corrected on the next turn instead of quoting the old path forever.
    #[test]
    fn a_stale_system_message_is_replaced_not_appended() {
        let mut a = agent(
            "C:/src/mascot",
            ProjectContext { name: "mascot".into(), scratch: false, detached: false },
        );
        a.set_history(vec![
            Msg::text("system", "Workspace: C:/src/somewhere-else"),
            Msg::text("user", "hi"),
        ]);
        a.sync_system();

        let h = a.history();
        assert_eq!(h.iter().filter(|m| m.role == "system").count(), 1);
        assert_eq!(h[0].role, "system");
        assert!(!system_of(&a).contains("somewhere-else"), "{}", system_of(&a));
        assert!(system_of(&a).contains("C:/src/mascot"));
        assert_eq!(h[1].plain_text_parts(), "hi");
    }
}
