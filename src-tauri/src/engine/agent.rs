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
    #[serde(default)]
    pub models: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub providers: Vec<ProviderItem>,
}

/// A saved provider configuration; the active connection is the flat
/// base_url/api_key/model/models fields above.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct ProviderItem {
    pub id: String,
    pub name: String,
    pub base_url: String,
    pub api_key: String,
    pub models: Vec<String>,
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
            providers: vec![ProviderItem {
                id: "aigateway".into(),
                name: "AI Gateway".into(),
                base_url: "https://provider.example/v1".into(),
                api_key: String::new(),
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

    fn ensure_system(&mut self) {
        if self.history.iter().any(|m| m.role == "system") {
            return;
        }
        let hint = format!(
            "{}  Workspace (tools run here + relative paths resolve here): {}",
            platform_hint(),
            self.config.workspace
        );
        let body = if self.config.system.trim().is_empty() {
            hint
        } else {
            format!("{}

{}", hint, self.config.system.trim())
        };
        self.history.insert(0, Msg::text("system", body));
    }

    /// Run one full turn: take a user message, then loop calling the model and
    /// executing tools until the model stops requesting tools (or a cap/stop).
    pub async fn run<F>(&mut self, user_text: &str, images: &[String], emit: &F, cancelled: &AtomicBool) -> RunStats
    where
        F: Emitter,
    {
        self.ensure_system();
        let mut parts: Vec<Part> = vec![Part::Text(user_text.to_string())];
        for url in images {
            if !url.is_empty() {
                parts.push(Part::ImageData(url.clone()));
            }
        }
        self.history.push(Msg { role: "user".into(), parts });
        self.persist();

        let mut stats = RunStats { steps: 0, tool_calls: 0, stopped: false, tokens_in: 0, tokens_out: 0, cost: None, error: None };

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
                if RISKY.contains(&tc.name.as_str()) {
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
