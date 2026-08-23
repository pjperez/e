pub mod engine;

use engine::agent::{Agent, Config};
use engine::approval;
use engine::plugins;
use engine::skills::SkillStore;
use engine::tools::ToolRegistry;
use engine::{Emitter, Part, RunSummary, ToolCall, COMPACTION_MARKER, KEEP_RECENT, MIN_COMPACT_GAIN};
use serde::Serialize;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tauri::{Emitter as _, Manager as _};

/// Application state.
///
/// Runs are tracked per session id: each has its own cancellation flag, created
/// fresh when the run starts. A single shared flag used to mean that stopping
/// one chat permanently poisoned every later run, and that one chat's Stop hit
/// whichever chat happened to be running.
struct AppState {
    config: Mutex<Config>,
    /// Shared by every run, so MCP + plugin tools registered once apply to all.
    tools: Arc<ToolRegistry>,
    store: Mutex<engine::sessions::SessionStore>,
    runs: Mutex<HashMap<String, Arc<AtomicBool>>>,
    /// Set once the frontend has registered its close handler. Until then the
    /// close button must behave normally: preventing close while nothing can
    /// draw the confirmation would leave the app impossible to quit.
    ui_ready: AtomicBool,
    /// A confirmation prompt is on screen. A second close attempt while it is
    /// up means the user really wants out, so it is allowed through — the
    /// escape hatch if the webview stops responding.
    close_pending: AtomicBool,
}

impl AppState {
    fn config(&self) -> Config {
        self.config.lock().map(|c| c.clone()).unwrap_or_else(|e| e.into_inner().clone())
    }

    fn is_running(&self, sid: &str) -> bool {
        self.runs.lock().map(|r| r.contains_key(sid)).unwrap_or(false)
    }

    /// Register a run, returning its cancel flag, or `None` if that session is
    /// already running.
    fn begin_run(&self, sid: &str) -> Option<Arc<AtomicBool>> {
        let mut runs = self.runs.lock().ok()?;
        if runs.contains_key(sid) {
            return None;
        }
        let flag = Arc::new(AtomicBool::new(false));
        runs.insert(sid.to_string(), flag.clone());
        Some(flag)
    }

    fn end_run(&self, sid: &str) {
        if let Ok(mut runs) = self.runs.lock() {
            runs.remove(sid);
        }
    }

    fn cancel(&self, sid: &str) -> bool {
        let flag = self.runs.lock().ok().and_then(|r| r.get(sid).cloned());
        match flag {
            Some(f) => {
                f.store(true, Ordering::SeqCst);
                true
            }
            None => false,
        }
    }

    fn cancel_all(&self) {
        if let Ok(runs) = self.runs.lock() {
            for f in runs.values() {
                f.store(true, Ordering::SeqCst);
            }
        }
    }
}

struct AppEmitter {
    handle: tauri::AppHandle,
    session: String,
}

impl Emitter for AppEmitter {
    fn token(&self, s: &str) {
        let _ = self.handle.emit("e:token", serde_json::json!({ "sid": self.session, "text": s }));
    }
    fn reasoning(&self, s: &str) {
        let _ = self.handle.emit("e:reasoning", serde_json::json!({ "sid": self.session, "text": s }));
    }
    fn activity(&self, phase: &str, tool: Option<&str>, step: usize) {
        let _ = self.handle.emit("e:activity", serde_json::json!({ "sid": self.session, "phase": phase, "tool": tool, "step": step }));
    }
    fn tool_call(&self, tc: &ToolCall) {
        let _ = self.handle.emit("e:tool_call", serde_json::json!({ "sid": self.session, "id": tc.id, "name": tc.name, "arguments": tc.arguments.to_string() }));
    }
    fn tool_result(&self, id: &str, name: &str, ok: bool, output: &str) {
        let _ = self.handle.emit("e:tool_result", serde_json::json!({ "sid": self.session, "id": id, "name": name, "success": ok, "output": output }));
    }
    fn summary(&self, s: &RunSummary) {
        let _ = self.handle.emit(
            "e:summary",
            serde_json::json!({
                "sid": self.session,
                "steps": s.steps,
                "tools": s.tool_calls,
                "stopped": s.stopped,
                "tokensIn": s.tokens_in,
                "tokensOut": s.tokens_out,
                "contextTokens": s.context_tokens,
                "cost": s.cost,
                "error": s.error,
            }),
        );
    }
    fn message_end(&self) {
        let _ = self.handle.emit("e:message_end", serde_json::json!({ "sid": self.session }));
    }
    fn done(&self, stopped: bool) {
        let _ = self.handle.emit("e:done", serde_json::json!({ "sid": self.session, "stopped": stopped }));
    }
    fn error(&self, msg: &str) {
        let _ = self.handle.emit("e:error", serde_json::json!({ "sid": self.session, "message": msg }));
    }
}

#[derive(Serialize)]
struct Status {
    model: String,
    tools: usize,
    base_url: String,
    ready: bool,
}

#[tauri::command]
fn get_config(state: tauri::State<AppState>) -> Config {
    state.config()
}

#[tauri::command]
fn save_config(state: tauri::State<AppState>, config: Config) -> Result<(), String> {
    if let Ok(mut c) = state.config.lock() {
        *c = config.clone();
    }
    config.save();
    Ok(())
}

#[tauri::command]
fn read_attachment(state: tauri::State<AppState>, path: String) -> Result<serde_json::Value, String> {
    // Resolve relative to the active chat's workspace, falling back to the
    // global one, so `@file` means the same thing as the tools' cwd.
    let ws = state
        .store
        .lock()
        .map(|st| {
            let w = st.workspace(&st.current);
            if w.is_empty() { st.current_project_workspace() } else { w }
        })
        .unwrap_or_default();
    let ws = if ws.is_empty() { state.config().workspace } else { ws };
    let p = std::path::Path::new(&path);
    let full = if p.is_absolute() { p.to_path_buf() } else { std::path::PathBuf::from(&ws).join(p) };
    let data = std::fs::read(&full).map_err(|e| format!("{path}: {e}"))?;
    let mut content = String::from_utf8_lossy(&data).to_string();
    if content.len() > 200_000 {
        let mut end = 200_000;
        while end > 0 && !content.is_char_boundary(end) {
            end -= 1;
        }
        content.truncate(end);
        content.push_str("\n… (truncated)");
    }
    Ok(serde_json::json!({ "path": path, "content": content }))
}

#[tauri::command]
fn list_projects(state: tauri::State<AppState>) -> serde_json::Value {
    match state.store.lock() {
        Ok(st) => serde_json::json!({ "projects": st.project_list(), "current": st.current_project }),
        Err(_) => serde_json::json!({ "projects": [], "current": "" }),
    }
}

#[tauri::command]
fn new_project(state: tauri::State<AppState>, name: Option<String>, workspace: Option<String>) -> Result<serde_json::Value, String> {
    let mut st = state.store.lock().map_err(|_| "lock")?;
    let meta = st.project_create(&name.unwrap_or_default(), &workspace.unwrap_or_default());
    serde_json::to_value(&meta).map_err(|e| e.to_string())
}

#[tauri::command]
fn rename_project(state: tauri::State<AppState>, id: String, name: String) -> Result<bool, String> {
    let mut st = state.store.lock().map_err(|_| "lock")?;
    Ok(st.project_rename(&id, &name))
}

#[tauri::command]
fn set_project_workspace(state: tauri::State<AppState>, id: String, workspace: String) -> Result<bool, String> {
    let mut st = state.store.lock().map_err(|_| "lock")?;
    Ok(st.project_set_workspace(&id, &workspace))
}

/// Lets the UI warn about a missing project folder before a tool fails on it.
#[tauri::command]
fn path_is_dir(path: String) -> bool {
    !path.trim().is_empty() && std::path::Path::new(path.trim()).is_dir()
}

#[tauri::command]
fn switch_project(state: tauri::State<AppState>, id: String) -> Result<bool, String> {
    let mut st = state.store.lock().map_err(|_| "lock")?;
    Ok(st.project_switch(&id))
}

#[tauri::command]
fn set_session_model(state: tauri::State<AppState>, id: String, model: String) -> Result<(), String> {
    state.store.lock().map_err(|_| "lock")?.set_model(&id, &model);
    Ok(())
}

#[tauri::command]
fn list_plugins() -> Vec<engine::plugins::PluginManifest> {
    plugins::discover()
}

#[tauri::command]
fn get_plugin(name: String) -> Result<serde_json::Value, String> {
    plugins::get_plugin(&name)
        .map(|(mf, src)| serde_json::json!({ "manifest": mf, "source": src }))
        .ok_or_else(|| format!("plugin '{}' not found", name))
}

#[tauri::command]
fn set_plugin_tools(state: tauri::State<AppState>, tools: Vec<engine::plugins::PluginToolDef>) {
    state.tools.set_plugin_tools(tools);
}

#[tauri::command]
fn plugin_tool_result(sid: String, ok: bool, output: String) {
    plugins::resolve(&sid, ok, output);
}

#[tauri::command]
fn approval_resolve(id: String, approved: bool) {
    approval::resolve(&id, approved);
}

#[tauri::command]
fn project_remove(state: tauri::State<AppState>, id: String) -> Result<bool, String> {
    let mut st = state.store.lock().map_err(|_| "lock")?;
    Ok(st.project_remove(&id))
}

#[tauri::command]
fn workspace_snapshot(state: tauri::State<AppState>, id: String) -> Result<bool, String> {
    let ws = state.store.lock().map_err(|_| "lock")?.workspace(&id);
    if ws.is_empty() {
        return Ok(false);
    }
    // Use git if available, otherwise skip
    let out = std::process::Command::new("git")
        .args(["stash", "create"])
        .current_dir(&ws)
        .output();
    if let Ok(o) = out {
        if o.status.success() {
            let sha = String::from_utf8_lossy(&o.stdout).trim().to_string();
            if !sha.is_empty() {
                let _ = std::process::Command::new("git")
                    .args(["stash", "store", "-m", &format!("e-snapshot-{id}"), &sha])
                    .current_dir(&ws)
                    .output();
                return Ok(true);
            }
        }
    }
    Ok(false)
}

#[tauri::command]
fn workspace_revert(state: tauri::State<AppState>, id: String) -> Result<bool, String> {
    let ws = state.store.lock().map_err(|_| "lock")?.workspace(&id);
    if ws.is_empty() {
        return Ok(false);
    }
    let out = std::process::Command::new("git")
        .args(["checkout", "--", "."])
        .current_dir(&ws)
        .output();
    Ok(out.map(|o| o.status.success()).unwrap_or(false))
}

/// Byte-safe substring around a match: slicing a UTF-8 string at arbitrary
/// byte offsets panics, which used to crash search on any non-ASCII message.
fn snippet_around(text: &str, pos: usize, len: usize) -> String {
    let mut start = pos.saturating_sub(40);
    while start > 0 && !text.is_char_boundary(start) {
        start -= 1;
    }
    let mut end = (pos + len + 80).min(text.len());
    while end < text.len() && !text.is_char_boundary(end) {
        end += 1;
    }
    if end < start {
        return String::new();
    }
    text[start..end].to_string()
}

#[tauri::command]
fn search_sessions(state: tauri::State<AppState>, query: String) -> Result<serde_json::Value, String> {
    let st = state.store.lock().map_err(|_| "lock")?;
    let q = query.trim().to_lowercase();
    let mut results: Vec<serde_json::Value> = Vec::new();
    if q.is_empty() {
        return Ok(serde_json::json!({ "results": results }));
    }
    for meta in &st.sessions {
        if meta.name.to_lowercase().contains(&q) {
            results.push(serde_json::json!({ "session_id": meta.id, "session_name": meta.name, "snippet": meta.name, "project": meta.project }));
            continue;
        }
        for m in &st.get_history(&meta.id) {
            if m.role == "system" {
                continue;
            }
            let text = m.plain_text_parts();
            let text = text.strip_prefix(engine::COMPACTION_MARKER).map(str::to_string).unwrap_or(text);
            let lower = text.to_lowercase();
            if let Some(pos) = lower.find(&q) {
                results.push(serde_json::json!({
                    "session_id": meta.id,
                    "session_name": meta.name,
                    "snippet": snippet_around(&text, pos, q.len()),
                    "role": m.role,
                    "project": meta.project
                }));
                break; // one match per session is enough
            }
        }
    }
    Ok(serde_json::json!({ "results": results }))
}

#[tauri::command]
fn list_skills() -> Vec<engine::skills::SkillMeta> {
    SkillStore::new().list()
}

#[tauri::command]
fn get_skill(name: String) -> Result<String, String> {
    SkillStore::new().get(&name).ok_or_else(|| format!("skill '{name}' not found"))
}

#[tauri::command]
fn list_sessions(state: tauri::State<AppState>) -> serde_json::Value {
    let running: Vec<String> = state.runs.lock().map(|r| r.keys().cloned().collect()).unwrap_or_default();
    match state.store.lock() {
        Ok(st) => serde_json::json!({ "sessions": st.sessions, "current": st.current, "running": running }),
        Err(_) => serde_json::json!({ "sessions": [], "current": "", "running": running }),
    }
}

/// Session ids with a run in flight. The UI asks for this on startup and after
/// switching chats so it can show the right state instead of guessing.
#[tauri::command]
fn running_sessions(state: tauri::State<AppState>) -> Vec<String> {
    state.runs.lock().map(|r| r.keys().cloned().collect()).unwrap_or_default()
}

#[tauri::command]
fn new_session(state: tauri::State<AppState>, name: Option<String>, workspace: Option<String>, model: Option<String>, project: Option<String>) -> Result<serde_json::Value, String> {
    let mut st = state.store.lock().map_err(|_| "lock")?;
    // Chats are created inside the current project, so aim it at the folder the
    // user clicked "+" on rather than wherever they happened to be last.
    if let Some(p) = project.filter(|p| !p.trim().is_empty()) {
        st.project_switch(p.trim());
    }
    let meta = st.create(&name.unwrap_or_default(), &workspace.unwrap_or_default(), &model.unwrap_or_default());
    serde_json::to_value(&meta).map_err(|e| e.to_string())
}

#[tauri::command]
fn delete_session(state: tauri::State<AppState>, id: String) -> Result<(), String> {
    // Stop the run first: otherwise it keeps streaming into a chat that is gone
    // and re-creates its history file on the next save.
    state.cancel(&id);
    state.store.lock().map_err(|_| "lock")?.remove(&id);
    Ok(())
}

#[tauri::command]
fn fork_session(state: tauri::State<AppState>, id: String, name: Option<String>) -> Result<serde_json::Value, String> {
    let mut st = state.store.lock().map_err(|_| "lock")?;
    let meta = st.fork(&id, &name.unwrap_or_default()).ok_or("session not found")?;
    serde_json::to_value(&meta).map_err(|e| e.to_string())
}

#[tauri::command]
fn rename_session(state: tauri::State<AppState>, id: String, name: String) -> Result<bool, String> {
    let mut st = state.store.lock().map_err(|_| "lock")?;
    Ok(st.rename_session(&id, &name))
}

#[tauri::command]
async fn compact_session(state: tauri::State<'_, AppState>, id: String) -> Result<serde_json::Value, String> {
    use engine::Msg;
    // Rewriting history under a live run would race with the agent's own saves.
    if state.is_running(&id) {
        return Err("chat is running".into());
    }
    let hist: Vec<engine::Msg> = state.store.lock().map(|st| st.get_history(&id)).unwrap_or_default();
    let non_system = hist.iter().filter(|m| m.role != "system").count();
    // Nothing worth compacting: keeping the tail already covers the whole
    // conversation, so summarising would burn a request and lose fidelity.
    if non_system <= KEEP_RECENT + MIN_COMPACT_GAIN {
        return Ok(serde_json::json!({ "messages": hist.len(), "compacted": false }));
    }

    let client_cfg = state.config();
    let split = engine::keep_split(&hist, client_cfg.active_context_window());
    if split == 0 {
        return Ok(serde_json::json!({ "messages": hist.len(), "compacted": false }));
    }
    let keep = hist[split..].to_vec();
    let old = &hist[..split];
    let old_text: String = old
        .iter()
        .map(|m| {
            if m.role == "system" {
                String::new()
            } else {
                format!("[{}]\n{}\n\n", m.role, m.plain_text_parts())
            }
        })
        .collect();

    let provider = engine::provider::ChatProvider::new(
        client_cfg.base_url,
        client_cfg.api_key,
        client_cfg.model,
        client_cfg.temperature,
    );
    let prompt = vec![Msg::text(
        "user",
        format!(
            "Summarize the following conversation so far into a tight summary. Include: tasks completed, decisions made, key facts, files touched, and open questions. Output ONLY the summary text, no preamble.\n\n=== CONVERSATION ===\n{}",
            old_text
        ),
    )];
    let never = AtomicBool::new(false);
    let summary = provider
        .chat(&prompt, &[], |_| {}, |_| {}, &never)
        .await
        .map(|c| c.text)
        .unwrap_or_default();
    if summary.trim().is_empty() {
        // Don't destroy history when summarization failed.
        return Err("could not summarize".into());
    }
    // A run may have started while we were waiting on the provider; writing now
    // would clobber the messages it has since appended.
    if state.is_running(&id) {
        return Err("chat is running".into());
    }
    let mut new_hist: Vec<engine::Msg> = hist.iter().filter(|m| m.role == "system").cloned().collect();
    new_hist.push(Msg::text("user", format!("{COMPACTION_MARKER}{summary}")));
    let n = new_hist.len();
    let kept = keep.len();
    new_hist.extend(keep);
    if let Ok(st) = state.store.lock() {
        st.set_history(&id, new_hist);
    }
    Ok(serde_json::json!({ "messages": n + kept, "compacted": true, "dropped": split.saturating_sub(n) }))
}

/// Usable context window (tokens) for the active provider/model. The frontend
/// budgets compaction against this instead of guessing.
#[tauri::command]
fn context_budget(state: tauri::State<AppState>) -> u64 {
    state.config().active_context_window()
}

#[tauri::command]
fn switch_session(state: tauri::State<AppState>, id: String) -> Result<bool, String> {
    let mut st = state.store.lock().map_err(|_| "lock")?;
    Ok(st.switch(&id))
}

#[tauri::command]
fn get_session(state: tauri::State<AppState>, id: String) -> Result<serde_json::Value, String> {
    let st = state.store.lock().map_err(|_| "lock")?;
    let model = st.model(&id);
    let h = st.get_history(&id);
    let list: Vec<serde_json::Value> = h
        .iter()
        .filter(|m| m.role != "system")
        .map(|m| {
            let compacted = m.compaction_summary();
            let role = if compacted.is_some() {
                "compaction"
            } else if m.role == "assistant" {
                "assistant"
            } else if m.role == "tool" {
                "tool"
            } else {
                "user"
            };
            let reasoning = m
                .parts
                .iter()
                .filter_map(|p| if let Part::Reasoning(r) = p { Some(r.clone()) } else { None })
                .collect::<Vec<_>>()
                .join("\n");
            let content = compacted.unwrap_or_else(|| m.plain_text_parts());
            let error = m
                .parts
                .iter()
                .filter_map(|p| if let Part::Error(e) = p { Some(e.clone()) } else { None })
                .collect::<Vec<_>>()
                .join("\n");
            serde_json::json!({ "role": role, "content": content, "reasoning": reasoning, "error": error })
        })
        .filter(|v| {
            !v["content"].as_str().unwrap_or("").is_empty()
                || !v["reasoning"].as_str().unwrap_or("").is_empty()
                || !v["error"].as_str().unwrap_or("").is_empty()
        })
        .collect();
    Ok(serde_json::json!({
        "messages": list,
        "model": model,
        "running": state.is_running(&id),
        // Lets the UI reason about context pressure before the first run of
        // this app session reports real usage. Counts tool traffic, which the
        // message list above deliberately omits.
        "context_estimate": h.iter().map(engine::est_tokens).sum::<usize>(),
    }))
}

#[tauri::command]
async fn refresh_models(base_url: String, api_key: String) -> Result<Vec<String>, String> {
    let client = reqwest::Client::new();
    let url = format!("{}/models", base_url.trim_end_matches('/'));
    let mut req = client.get(&url);
    if !api_key.is_empty() {
        req = req.bearer_auth(&api_key);
    }
    let resp = req.send().await.map_err(|e| format!("request failed: {e}"))?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        let cut: String = body.chars().take(300).collect();
        return Err(format!("provider returned {status}: {cut}"));
    }
    let v: serde_json::Value = resp.json().await.map_err(|e| format!("bad json: {e}"))?;
    let mut out = Vec::new();
    if let Some(data) = v.get("data").and_then(|d| d.as_array()) {
        for m in data {
            if let Some(id) = m.get("id").and_then(|i| i.as_str()) {
                out.push(id.to_string());
            }
        }
    }
    Ok(out)
}

#[tauri::command]
fn get_status(state: tauri::State<AppState>) -> Status {
    let cfg = state.config();
    let busy = state.runs.lock().map(|r| !r.is_empty()).unwrap_or(false);
    Status {
        model: cfg.model,
        tools: state.tools.names().len(),
        base_url: cfg.base_url,
        ready: !busy,
    }
}

#[tauri::command]
async fn send_text(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    text: String,
    images: Option<Vec<String>>,
    sid: Option<String>,
) -> Result<String, String> {
    // The target chat is explicit: a message typed in one chat must never land
    // in another just because the user switched tabs before it was sent.
    let session = match sid.map(|s| s.trim().to_string()).filter(|s| !s.is_empty()) {
        Some(s) => s,
        None => state.store.lock().map(|st| st.current.clone()).unwrap_or_default(),
    };
    if session.is_empty() {
        return Err("no chat selected".into());
    }

    let text = text.trim_start_matches('\u{feff}').trim().to_string();
    if text.is_empty() {
        return Err("empty message".into());
    }

    let cancelled = state.begin_run(&session).ok_or("this chat is already running")?;

    let mut config = state.config();
    let (history, session_ws, session_model) = match state.store.lock() {
        Ok(st) => (st.get_history(&session), st.workspace(&session), st.model(&session)),
        Err(_) => {
            state.end_run(&session);
            return Err("internal lock".into());
        }
    };
    if !session_ws.is_empty() {
        config.workspace = session_ws;
    }
    if !session_model.is_empty() {
        config.model = session_model;
    }

    if let Ok(mut st) = state.store.lock() {
        st.set_state(&session, "busy");
    }

    let images = images.unwrap_or_default();
    let tools = state.tools.clone();
    let handle = app.clone();
    let save_handle = app.clone();
    let save_sess = session.clone();
    let run_sess = session.clone();

    // Each run gets its own thread and single-threaded runtime. The agent loop
    // calls blocking code (shell tools, approval prompts), which would otherwise
    // occupy shared runtime workers and stall every other chat.
    std::thread::Builder::new()
        .name(format!("e-run-{session}"))
        .spawn(move || {
            let rt = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
                Ok(rt) => rt,
                Err(e) => {
                    let state = handle.state::<AppState>();
                    state.end_run(&run_sess);
                    let emit = AppEmitter { handle: handle.clone(), session: run_sess.clone() };
                    emit.error(&format!("could not start run: {e}"));
                    emit.done(false);
                    return;
                }
            };
            rt.block_on(async move {
                let mut agent = Agent::with_tools(config, tools);
                agent.session = run_sess.clone();
                agent.set_history(history);
                agent.save = Some(Box::new(move |msgs: &[crate::engine::Msg]| {
                    if let Ok(st) = save_handle.state::<AppState>().store.lock() {
                        st.set_history(&save_sess, msgs.to_vec());
                    }
                }));

                let emit = AppEmitter { handle: handle.clone(), session: run_sess.clone() };
                let stats = agent.run(&text, &images, &emit, &cancelled).await;

                let state = handle.state::<AppState>();
                state.end_run(&run_sess);
                let sess_state = if stats.error.is_some() { "error" } else { "idle" };
                let history = agent.history();
                if let Ok(mut st) = state.store.lock() {
                    st.set_state(&run_sess, sess_state);
                    st.set_history(&run_sess, history);
                };
            });
        })
        .map_err(|e| {
            state.end_run(&session);
            format!("could not start run: {e}")
        })?;

    Ok(session)
}

#[tauri::command]
fn cancel_run(state: tauri::State<AppState>, sid: Option<String>) -> bool {
    match sid.map(|s| s.trim().to_string()).filter(|s| !s.is_empty()) {
        Some(s) => state.cancel(&s),
        None => {
            state.cancel_all();
            true
        }
    }
}

#[tauri::command]
fn clear_session(state: tauri::State<AppState>, sid: Option<String>) -> Result<(), String> {
    let session = match sid.map(|s| s.trim().to_string()).filter(|s| !s.is_empty()) {
        Some(s) => s,
        None => state.store.lock().map_err(|_| "lock")?.current.clone(),
    };
    if session.is_empty() {
        return Ok(());
    }
    if state.is_running(&session) {
        return Err("chat is running".into());
    }
    state.store.lock().map_err(|_| "lock")?.set_history(&session, Vec::new());
    Ok(())
}

/// Close the app for real. The window's own close is intercepted (see
/// `on_window_event`) so the UI can confirm first; this is the path it calls
/// once the user says yes. Going through `exit` rather than closing the window
/// avoids re-entering the intercept.
#[tauri::command]
fn confirm_close(app: tauri::AppHandle, state: tauri::State<AppState>) {
    // Signal in-flight runs to stop so they get the chance to write their
    // tool results; a run killed mid-tool leaves history a provider rejects.
    state.cancel_all();
    app.exit(0);
}

/// The user chose to stay, so the next close attempt gets a fresh prompt
/// instead of being waved straight through.
#[tauri::command]
fn close_dismissed(state: tauri::State<AppState>) {
    state.close_pending.store(false, Ordering::SeqCst);
}

/// The frontend has booted and registered its close handler. Only from this
/// point is it safe to intercept the close button.
#[tauri::command]
fn ui_ready(state: tauri::State<AppState>) {
    state.ui_ready.store(true, Ordering::SeqCst);
}

pub fn run() {
    let config = Config::from_env();
    let state = AppState {
        config: Mutex::new(config),
        tools: Arc::new(ToolRegistry::new()),
        store: Mutex::new(engine::sessions::SessionStore::new()),
        runs: Mutex::new(HashMap::new()),
        ui_ready: AtomicBool::new(false),
        close_pending: AtomicBool::new(false),
    };

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        // Closing is intercepted so the UI can confirm first — a run killed
        // mid-tool loses work and leaves history the provider rejects.
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                let app = window.app_handle();
                let state = app.state::<AppState>();
                // Never trap the user: if the UI hasn't booted it cannot draw
                // the prompt, and a second attempt while the prompt is already
                // up is taken as "yes, really". Both fall through to a normal
                // close instead of preventing it.
                if !state.ui_ready.load(Ordering::SeqCst) {
                    return;
                }
                if state.close_pending.swap(true, Ordering::SeqCst) {
                    return;
                }
                api.prevent_close();
                let running: Vec<String> =
                    state.runs.lock().map(|r| r.keys().cloned().collect()).unwrap_or_default();
                let _ = window.emit("e:close_requested", serde_json::json!({ "running": running }));
            }
        })
        .setup(|app| {
            plugins::init(app.handle().clone());
            approval::init(app.handle().clone());
            {
                let h = app.handle().clone();
                std::thread::spawn(move || {
                    let tools = h.state::<AppState>().tools.clone();
                    for srv in engine::mcp::load_servers() {
                        if let Ok(list) = srv.list_tools() {
                            for t in list {
                                tools.register_boxed(Box::new(engine::mcp::McpTool::new(srv.clone(), t)));
                            }
                        }
                    }
                });
            }
            Ok(())
        })
        .manage(state)
        .invoke_handler(tauri::generate_handler![
            get_config,
            save_config,
            get_status,
            send_text,
            cancel_run,
            clear_session,
            read_attachment,
            refresh_models,
            list_skills,
            get_skill,
            list_sessions,
            running_sessions,
            new_session,
            delete_session,
            fork_session,
            switch_session,
            rename_session,
            compact_session,
            context_budget,
            get_session,
            set_session_model,
            list_plugins,
            get_plugin,
            set_plugin_tools,
            plugin_tool_result,
            list_projects,
            new_project,
            switch_project,
            rename_project,
            set_project_workspace,
            path_is_dir,
            approval_resolve,
            project_remove,
            workspace_snapshot,
            workspace_revert,
            search_sessions,
            confirm_close,
            close_dismissed,
            ui_ready
        ])
        .run(tauri::generate_context!())
        .expect("error while running e");
}