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
    /// Which provider `model` belongs to. Stored explicitly because two
    /// providers can share a base URL or serve the same model id, so matching
    /// on either one cannot say which connection the user actually picked.
    #[serde(default)]
    pub provider_id: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub providers: Vec<ProviderItem>,
}

pub fn default_context_window() -> u64 {
    1_000_000
}

/// Fraction of the context window at which we compact. Deliberately high: the
/// point is to avoid overflowing, not to keep the window small.
pub const COMPACT_AT: f64 = 0.85;

/// A saved provider configuration. Every enabled provider contributes its
/// enabled models to one flat catalogue the user picks from; the flat
/// base_url/api_key/model fields above are just the resulting connection.
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
    /// Turn the whole provider off without deleting it (and losing its key).
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Models hidden from the picker. An opt-out list rather than an opt-in
    /// one so a `/models` refresh surfaces newly added models by default
    /// instead of silently hiding them.
    #[serde(default)]
    pub disabled_models: Vec<String>,
}

pub fn default_true() -> bool {
    true
}

impl ProviderItem {
    pub fn is_usable(&self) -> bool {
        self.enabled && !self.base_url.trim().is_empty()
    }

    /// Models this provider offers to the picker.
    pub fn enabled_models(&self) -> Vec<String> {
        self.models
            .iter()
            .filter(|m| !self.disabled_models.iter().any(|d| d == *m))
            .cloned()
            .collect()
    }

    pub fn serves(&self, model: &str) -> bool {
        self.is_usable()
            && self.models.iter().any(|m| m == model)
            && !self.disabled_models.iter().any(|d| d == model)
    }
}

/// One entry in the flat, cross-provider model catalogue the picker shows.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct ModelChoice {
    pub model: String,
    pub provider_id: String,
    pub provider_name: String,
    pub context_window: u64,
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
            provider_id: "aigateway".into(),
            providers: vec![ProviderItem {
                id: "aigateway".into(),
                name: "AI Gateway".into(),
                base_url: "https://provider.example/v1".into(),
                api_key: String::new(),
                context_window: None,
                enabled: true,
                disabled_models: Vec::new(),
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
        if let Some(v) = env_var("E_API_KEY") {
            base.api_key = v;
        }
        if let Some(v) = env_var("E_BASE_URL") {
            base.base_url = v;
        }
        if let Some(v) = env_var("E_MODEL") {
            base.model = v;
        }
        if let Some(v) = env_var("E_WORKSPACE") {
            base.workspace = v;
        }
        if let Ok(v) = std::env::var("E_YOLO") {
            if let Some(b) = parse_bool(&v) {
                base.yolo = b;
            }
        }
        // A config written before providers existed (or one whose list was
        // emptied) still has to yield one provider, otherwise there is nothing
        // to enable, disable or pick models from.
        if base.providers.is_empty() {
            base.providers.push(ProviderItem {
                id: "default".into(),
                name: host_of(&base.base_url),
                base_url: base.base_url.clone(),
                api_key: base.api_key.clone(),
                models: base.models.clone(),
                context_window: None,
                enabled: true,
                disabled_models: Vec::new(),
            });
        }
        // Env overrides land on the provider the connection points at, not just
        // the flat fields: the provider list is the source of truth now, so a
        // key set only on the flat field would be dropped the moment anything
        // re-derived the connection.
        let active_id = base.resolve_provider_id(&base.model, &base.provider_id);
        if let Some(p) = base.providers.iter_mut().find(|p| p.id == active_id) {
            if p.base_url.trim().is_empty() {
                p.base_url = base.base_url.clone();
            }
            if p.api_key.trim().is_empty() {
                p.api_key = base.api_key.clone();
            }
            if p.models.is_empty() {
                p.models = base.models.clone();
            }
            if let Some(v) = env_var("E_BASE_URL") {
                p.base_url = v;
            }
            if let Some(v) = env_var("E_API_KEY") {
                p.api_key = v;
            }
            if let Some(v) = env_var("E_MODEL") {
                if !p.models.contains(&v) {
                    p.models.push(v.clone());
                }
                p.disabled_models.retain(|m| *m != v);
                p.enabled = true;
            }
        }
        base.use_model(&base.model.clone(), &active_id);
        base
    }

    /// Id of the provider that should serve `model`. The hint (the previously
    /// active provider, or a chat's remembered one) wins whenever it still
    /// offers the model, so a model several providers happen to share keeps
    /// using the connection it was picked from.
    pub fn resolve_provider_id(&self, model: &str, hint: &str) -> String {
        if let Some(p) = self.providers.iter().find(|p| p.id == hint && p.serves(model)) {
            return p.id.clone();
        }
        if let Some(p) = self.providers.iter().find(|p| p.serves(model)) {
            return p.id.clone();
        }
        // Nothing serves it (unknown model, or every owner was disabled): fall
        // back to the hint, then to any usable provider, so runs still have a
        // connection instead of silently pointing at nothing.
        if let Some(p) = self.providers.iter().find(|p| p.id == hint && p.is_usable()) {
            return p.id.clone();
        }
        self.providers
            .iter()
            .find(|p| p.is_usable())
            .or_else(|| self.providers.first())
            .map(|p| p.id.clone())
            .unwrap_or_default()
    }

    pub fn provider(&self, id: &str) -> Option<&ProviderItem> {
        self.providers.iter().find(|p| p.id == id)
    }

    /// Point the connection at `model`, following it to whichever provider
    /// owns it. This is the only way the flat base_url/api_key should change:
    /// picking a model *is* picking a provider.
    pub fn use_model(&mut self, model: &str, provider_hint: &str) {
        let id = self.resolve_provider_id(model, provider_hint);
        let Some((base_url, api_key, models)) = self
            .provider(&id)
            .map(|p| (p.base_url.clone(), p.api_key.clone(), p.enabled_models()))
        else {
            return;
        };
        self.provider_id = id;
        self.base_url = base_url;
        self.api_key = api_key;
        let wanted = if model.trim().is_empty() { self.model.clone() } else { model.to_string() };
        // A model that is no longer on offer (turned off, or dropped by a
        // refresh) would just be rejected by the provider, so fall back to one
        // it actually serves rather than failing every run.
        self.model = if models.contains(&wanted) {
            wanted
        } else {
            models.first().cloned().unwrap_or(wanted)
        };
        self.models = models;
    }

    /// Re-derive the connection from the provider list. Call after any edit to
    /// providers or the selected model so base_url/api_key/models can never
    /// drift from the provider that actually serves it.
    pub fn normalize(&mut self) {
        let model = self.model.clone();
        let hint = self.provider_id.clone();
        self.use_model(&model, &hint);
    }

    /// Every enabled model across every enabled provider — the flat catalogue
    /// the picker offers, with the provider each entry belongs to.
    pub fn model_catalog(&self) -> Vec<ModelChoice> {
        let mut out = Vec::new();
        for p in self.providers.iter().filter(|p| p.is_usable()) {
            let win = p.context_window.filter(|w| *w > 0).unwrap_or(self.context_window);
            for m in p.enabled_models() {
                out.push(ModelChoice {
                    model: m,
                    provider_id: p.id.clone(),
                    provider_name: if p.name.trim().is_empty() { p.id.clone() } else { p.name.clone() },
                    context_window: if win == 0 { default_context_window() } else { win },
                });
            }
        }
        out
    }

    /// Context window to budget against for `model`: its provider's override
    /// when it has one, otherwise the global setting.
    pub fn context_window_for(&self, model: &str, provider_hint: &str) -> u64 {
        let id = self.resolve_provider_id(model, provider_hint);
        let win = self
            .provider(&id)
            .and_then(|p| p.context_window)
            .filter(|w| *w > 0)
            .unwrap_or(self.context_window);
        if win == 0 {
            default_context_window()
        } else {
            win
        }
    }

    /// Context window for the connection as it stands.
    pub fn active_context_window(&self) -> u64 {
        self.context_window_for(&self.model, &self.provider_id)
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
    if !c.provider_id.is_empty() {
        base.provider_id = c.provider_id;
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

/// An env override only counts when it actually carries a value; an empty one
/// must not blank out a configured setting.
fn env_var(key: &str) -> Option<String> {
    std::env::var(key).ok().map(|v| v.trim().to_string()).filter(|v| !v.is_empty())
}

/// Host part of a base URL, used to name a provider migrated from a config
/// that predates the provider list.
fn host_of(url: &str) -> String {
    let s = url.split_once("://").map(|(_, rest)| rest).unwrap_or(url);
    let host = s.split('/').next().unwrap_or(s).trim();
    if host.is_empty() {
        "Provider".to_string()
    } else {
        host.to_string()
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

    fn provider(id: &str, models: &[&str]) -> ProviderItem {
        ProviderItem {
            id: id.into(),
            name: id.to_uppercase(),
            base_url: format!("https://{id}.test/v1"),
            api_key: format!("key-{id}"),
            models: models.iter().map(|m| m.to_string()).collect(),
            context_window: None,
            enabled: true,
            disabled_models: Vec::new(),
        }
    }

    fn cfg(providers: Vec<ProviderItem>) -> Config {
        Config {
            base_url: String::new(),
            api_key: String::new(),
            model: String::new(),
            temperature: 0.7,
            system: String::new(),
            workspace: String::new(),
            yolo: false,
            models: Vec::new(),
            context_window: 1_000_000,
            provider_id: String::new(),
            providers,
        }
    }

    #[test]
    fn catalog_spans_every_enabled_provider() {
        let c = cfg(vec![provider("a", &["m1", "m2"]), provider("b", &["m3"])]);
        let got: Vec<String> = c.model_catalog().into_iter().map(|x| x.model).collect();
        assert_eq!(got, vec!["m1", "m2", "m3"], "all enabled providers contribute");
    }

    #[test]
    fn disabling_a_provider_hides_its_models_but_keeps_it() {
        let mut c = cfg(vec![provider("a", &["m1"]), provider("b", &["m3"])]);
        c.providers[0].enabled = false;
        let got: Vec<String> = c.model_catalog().into_iter().map(|x| x.model).collect();
        assert_eq!(got, vec!["m3"]);
        assert_eq!(c.providers.len(), 2, "disabling must not delete the provider or its key");
        assert_eq!(c.providers[0].api_key, "key-a");
    }

    #[test]
    fn disabling_one_model_leaves_the_rest_of_the_provider_on() {
        let mut c = cfg(vec![provider("a", &["m1", "m2"])]);
        c.providers[0].disabled_models = vec!["m1".into()];
        let got: Vec<String> = c.model_catalog().into_iter().map(|x| x.model).collect();
        assert_eq!(got, vec!["m2"]);
    }

    #[test]
    fn picking_a_model_switches_the_whole_connection() {
        let mut c = cfg(vec![provider("a", &["m1"]), provider("b", &["m3"])]);
        c.use_model("m1", "");
        assert_eq!((c.provider_id.as_str(), c.base_url.as_str(), c.api_key.as_str()), ("a", "https://a.test/v1", "key-a"));
        c.use_model("m3", "");
        assert_eq!((c.provider_id.as_str(), c.base_url.as_str(), c.api_key.as_str()), ("b", "https://b.test/v1", "key-b"));
    }

    #[test]
    fn a_shared_model_id_stays_on_the_provider_it_was_picked_from() {
        let c = cfg(vec![provider("a", &["shared"]), provider("b", &["shared"])]);
        assert_eq!(c.resolve_provider_id("shared", "b"), "b", "the hint wins when it still serves the model");
        assert_eq!(c.resolve_provider_id("shared", ""), "a", "no hint falls back to the first that serves it");
        assert_eq!(c.resolve_provider_id("shared", "gone"), "a", "a stale hint must not strand the run");
    }

    #[test]
    fn turning_off_the_selected_model_moves_the_selection() {
        let mut c = cfg(vec![provider("a", &["m1", "m2"])]);
        c.use_model("m1", "");
        c.providers[0].disabled_models = vec!["m1".into()];
        c.normalize();
        assert_eq!(c.model, "m2", "a model the provider no longer offers would just be rejected");
    }

    #[test]
    fn turning_off_a_providers_last_model_falls_through_to_another() {
        let mut c = cfg(vec![provider("a", &["m1"]), provider("b", &["m3"])]);
        c.use_model("m1", "");
        c.providers[0].enabled = false;
        c.normalize();
        assert_eq!((c.provider_id.as_str(), c.model.as_str()), ("b", "m3"));
    }

    #[test]
    fn context_window_follows_the_model_to_its_own_provider() {
        let mut c = cfg(vec![provider("a", &["m1"]), provider("b", &["m3"])]);
        c.providers[1].context_window = Some(128_000);
        assert_eq!(c.context_window_for("m1", ""), 1_000_000, "no override falls back to the global window");
        assert_eq!(c.context_window_for("m3", ""), 128_000, "the owning provider's override wins");
    }

    /// A config written before providers could be switched off has no
    /// `enabled`, `disabled_models` or `provider_id`. It must still load, keep
    /// every provider on, and land on the provider that serves its model.
    #[test]
    fn a_config_from_before_this_feature_still_loads() {
        let legacy = r#"{
            "base_url": "https://gw.test/v1",
            "api_key": "gw-key",
            "model": "local/qwen",
            "temperature": 0.7,
            "system": "hi",
            "workspace": "/tmp",
            "yolo": false,
            "models": ["gw/one"],
            "context_window": 900000,
            "providers": [
              { "id": "gw", "name": "Gateway", "base_url": "https://gw.test/v1", "api_key": "gw-key", "models": ["gw/one"] },
              { "id": "local", "name": "Local", "base_url": "http://localhost:11434/v1", "api_key": "", "models": ["local/qwen"] }
            ]
        }"#;
        let mut c: Config = serde_json::from_str(legacy).expect("legacy config must still parse");
        assert!(c.providers.iter().all(|p| p.enabled), "providers default to on, never silently off");
        assert!(c.providers.iter().all(|p| p.disabled_models.is_empty()), "no model is hidden by default");
        assert_eq!(c.model_catalog().len(), 2, "both providers' models are on offer");
        c.normalize();
        assert_eq!(c.provider_id, "local", "an empty provider_id resolves from the model");
        assert_eq!(c.base_url, "http://localhost:11434/v1", "the connection follows the model, not the stale flat field");
    }

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
