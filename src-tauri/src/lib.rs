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
    /// Compaction happens before a chat's run is registered, but it is a
    /// provider call like any other — retries, backoff waits and all — and it
    /// shows on the same activity strip. It needs its own cancel flag or Stop
    /// has nothing to set while a compaction is backing off.
    compactions: Mutex<HashMap<String, Arc<AtomicBool>>>,
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
        let compact = self.compactions.lock().ok().and_then(|c| c.get(sid).cloned());
        let mut hit = false;
        if let Some(f) = flag {
            f.store(true, Ordering::SeqCst);
            hit = true;
        }
        // A chat can be compacting *and* have a run registered (a queued send
        // compacting behind a live run), so stop both rather than either.
        if let Some(f) = compact {
            f.store(true, Ordering::SeqCst);
            hit = true;
        }
        hit
    }

    /// Register a compaction, returning its cancel flag. Unlike a run there is
    /// only ever one per chat, and a second call simply replaces the first.
    fn begin_compaction(&self, sid: &str) -> Arc<AtomicBool> {
        let flag = Arc::new(AtomicBool::new(false));
        if let Ok(mut c) = self.compactions.lock() {
            c.insert(sid.to_string(), flag.clone());
        }
        flag
    }

    fn end_compaction(&self, sid: &str) {
        if let Ok(mut c) = self.compactions.lock() {
            c.remove(sid);
        }
    }

    fn cancel_all(&self) {
        if let Ok(runs) = self.runs.lock() {
            for f in runs.values() {
                f.store(true, Ordering::SeqCst);
            }
        }
        if let Ok(compactions) = self.compactions.lock() {
            for f in compactions.values() {
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
    fn retry(&self, n: &engine::provider::RetryNotice) {
        let _ = self.handle.emit(
            "e:retry",
            serde_json::json!({
                "sid": self.session,
                "attempt": n.attempt,
                "max": n.max_attempts,
                "delayMs": n.delay.as_millis() as u64,
                "status": n.status,
                "reason": n.reason,
            }),
        );
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
    let mut config = config;
    // The connection is derived here, never dictated by the client: it must
    // always be whichever provider serves the selected model. Seed from the
    // live one first so a config that resolves to nothing (every provider
    // deleted) keeps a usable base URL instead of silently blanking it.
    {
        let cur = state.config();
        if config.base_url.trim().is_empty() {
            config.base_url = cur.base_url;
        }
        if config.api_key.trim().is_empty() {
            config.api_key = cur.api_key;
        }
        if config.model.trim().is_empty() {
            config.model = cur.model.clone();
            config.provider_id = cur.provider_id.clone();
        }
        // Which plugins are off is owned by `set_plugin_enabled`, not by this
        // form. Taking it from the payload would clear the list every time
        // Settings was saved, quietly re-enabling a plugin the user rejected.
        config.disabled_plugins = cur.disabled_plugins;
        // Persist provider secrets before replacing the live config. Deleting a
        // provider removes its credential too, so the OS vault cannot collect
        // orphaned keys forever.
        let env_key = std::env::var("E_API_KEY")
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        for provider in &config.providers {
            // `get_config` includes the process-local override so requests and
            // model refreshes use it. A Settings round trip must not turn that
            // temporary value into a permanent credential.
            let is_env_override = provider.id == cur.provider_id
                && env_key.as_deref() == Some(provider.api_key.as_str());
            if !is_env_override {
                engine::credentials::save(&provider.id, &provider.api_key)?;
            }
        }
        for provider in cur
            .providers
            .iter()
            .filter(|old| !config.providers.iter().any(|new| new.id == old.id))
        {
            engine::credentials::delete(&provider.id)?;
        }
    }
    config.normalize();
    if let Ok(mut c) = state.config.lock() {
        *c = config.clone();
    }
    config.save();
    Ok(())
}

/// Every model on offer, across every enabled provider, tagged with the
/// provider it comes from. One flat catalogue means the picker is the single
/// place a model (and therefore a connection) is chosen.
#[tauri::command]
fn list_models(state: tauri::State<AppState>) -> Vec<engine::agent::ModelChoice> {
    state.config().model_catalog()
}

#[tauri::command]
fn read_attachment(state: tauri::State<AppState>, path: String) -> Result<serde_json::Value, String> {
    // Resolve relative to the active chat's own project folder, so `@file`
    // means the same thing as the tools' cwd.
    let ws = state
        .store
        .lock()
        .map(|st| {
            let cur = st.current.clone();
            st.resolved_workspace(&cur)
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
        Ok(st) => serde_json::json!({ "projects": st.project_list_json(), "current": st.current_project }),
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

/// Lets the UI warn about an unusable project folder before a tool fails on it.
///
/// A relative path counts as unusable even when it happens to resolve from
/// here: the tools refuse it (see `ToolContext::dir`), so the sidebar has to
/// flag it too rather than showing a folder that looks fine.
#[tauri::command]
fn path_is_dir(path: String) -> bool {
    let p = path.trim();
    !p.is_empty() && std::path::Path::new(p).is_absolute() && std::path::Path::new(p).is_dir()
}

#[tauri::command]
fn switch_project(state: tauri::State<AppState>, id: String) -> Result<bool, String> {
    let mut st = state.store.lock().map_err(|_| "lock")?;
    Ok(st.project_switch(&id))
}

#[tauri::command]
fn set_session_model(state: tauri::State<AppState>, id: String, model: String, provider: Option<String>) -> Result<(), String> {
    // Resolve here rather than trusting the caller: the chat has to record a
    // provider that really serves this model, or its next run would be sent to
    // whichever provider happened to be active globally.
    let provider = state.config().resolve_provider_id(&model, &provider.unwrap_or_default());
    state.store.lock().map_err(|_| "lock")?.set_model(&id, &model, &provider);
    Ok(())
}

/// Plugins visible from this chat's project, with the ones the user switched
/// off already marked — the frontend never has to re-derive that.
#[tauri::command]
fn list_plugins(state: tauri::State<AppState>, workspace: Option<String>) -> Vec<engine::plugins::PluginInfo> {
    let off = state.config().disabled_plugins;
    plugins::discover(workspace.as_deref())
        .into_iter()
        .map(|mut p| {
            p.enabled = !off.contains(&p.name);
            p
        })
        .collect()
}

#[tauri::command]
fn get_plugin(name: String, workspace: Option<String>) -> Result<serde_json::Value, String> {
    plugins::get_plugin(&name, workspace.as_deref())
        .map(|(info, src)| serde_json::json!({ "manifest": info, "source": src }))
}

/// Turn a plugin on or off. Persisted, because a plugin the user rejected must
/// stay rejected across restarts.
#[tauri::command]
fn set_plugin_enabled(state: tauri::State<AppState>, name: String, enabled: bool) -> Result<(), String> {
    let mut cfg = state.config();
    cfg.disabled_plugins.retain(|n| n != &name);
    if !enabled {
        cfg.disabled_plugins.push(name);
        cfg.disabled_plugins.sort();
    }
    if let Ok(mut c) = state.config.lock() {
        *c = cfg.clone();
    }
    cfg.save();
    Ok(())
}

/// Replace the plugin tool table. Returns what had to be refused (a name that
/// shadows a built-in, a duplicate) so the host can tell the user instead of
/// leaving a tool silently missing.
#[tauri::command]
fn set_plugin_tools(state: tauri::State<AppState>, tools: Vec<engine::plugins::PluginToolDef>) -> Vec<String> {
    state.tools.set_plugin_tools(tools)
}

#[tauri::command]
fn plugin_tool_result(sid: String, ok: bool, output: String) {
    plugins::resolve(&sid, ok, output);
}

/// The host says whether any plugin watches tool calls. While none does, the
/// engine skips the veto round-trip entirely.
#[tauri::command]
fn set_plugin_veto(active: bool) {
    plugins::set_veto_active(active);
}

#[tauri::command]
fn plugin_veto_result(id: String, reason: Option<String>) {
    plugins::veto_resolve(&id, reason);
}

#[tauri::command]
fn list_mcp_servers() -> Vec<engine::mcp::McpStatus> {
    engine::mcp::status()
}

/// Re-scan every extension surface. MCP servers are restarted here; plugins
/// and skills are re-read by the frontend, which owns their runtime.
#[tauri::command]
fn reload_extensions(state: tauri::State<AppState>, workspace: Option<String>) {
    engine::mcp::load(state.tools.clone(), workspace);
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
    let ws = state.store.lock().map_err(|_| "lock")?.resolved_workspace(&id);
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
    let ws = state.store.lock().map_err(|_| "lock")?.resolved_workspace(&id);
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
fn list_skills(workspace: Option<String>) -> Vec<engine::skills::SkillMeta> {
    SkillStore::for_workspace(&workspace.unwrap_or_default()).list()
}

#[tauri::command]
fn get_skill(name: String, workspace: Option<String>) -> Result<String, String> {
    SkillStore::for_workspace(&workspace.unwrap_or_default())
        .get(&name)
        .ok_or_else(|| format!("skill '{name}' not found"))
}

#[tauri::command]
fn list_sessions(state: tauri::State<AppState>) -> serde_json::Value {
    let running: Vec<String> = state.runs.lock().map(|r| r.keys().cloned().collect()).unwrap_or_default();
    match state.store.lock() {
        Ok(st) => serde_json::json!({ "sessions": st.session_list_json(), "current": st.current, "running": running }),
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
fn new_session(state: tauri::State<AppState>, name: Option<String>, workspace: Option<String>, model: Option<String>, provider: Option<String>, project: Option<String>) -> Result<serde_json::Value, String> {
    // Resolve the provider up front so a chat started on a model from a
    // non-default provider keeps that connection on its first run.
    let model = model.unwrap_or_default();
    let provider = if model.is_empty() {
        String::new()
    } else {
        state.config().resolve_provider_id(&model, &provider.unwrap_or_default())
    };
    let mut st = state.store.lock().map_err(|_| "lock")?;
    // Chats are created inside the current project, so aim it at the folder the
    // user clicked "+" on rather than wherever they happened to be last.
    if let Some(p) = project.filter(|p| !p.trim().is_empty()) {
        st.project_switch(p.trim());
    }
    let meta = st.create(&name.unwrap_or_default(), &workspace.unwrap_or_default(), &model, &provider);
    let use_worktree = state.config().task_worktrees;
    drop(st);
    let meta = provision_session_worktree(&state, meta, use_worktree)?;
    serde_json::to_value(&meta).map_err(|e| e.to_string())
}

struct TaskWorktree {
    workspace: String,
    base: String,
    branch: String,
}

fn worktrees_root() -> std::path::PathBuf {
    dirs::home_dir().unwrap_or_default().join(".e").join("worktrees")
}

/// Create a worktree only when the selected project folder belongs to a Git
/// repository. Non-Git projects continue to use their own folder directly.
fn create_task_worktree(base: &str, session_id: &str) -> Result<Option<TaskWorktree>, String> {
    create_task_worktree_in(base, session_id, &worktrees_root())
}

fn create_task_worktree_in(
    base: &str,
    session_id: &str,
    root: &std::path::Path,
) -> Result<Option<TaskWorktree>, String> {
    let base_path = std::path::Path::new(base);
    if !base_path.is_dir() || engine::sessions::is_scratch(base) {
        return Ok(None);
    }
    let repo = std::process::Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(base_path)
        .output()
        .map_err(|e| format!("could not start git while creating the task worktree: {e}"))?;
    if !repo.status.success() {
        return Ok(None);
    }
    let repo = String::from_utf8_lossy(&repo.stdout).trim().to_string();
    if repo.is_empty() {
        return Ok(None);
    }

    std::fs::create_dir_all(root)
        .map_err(|e| format!("could not create the worktree folder '{}': {e}", root.display()))?;
    let path = root.join(session_id);
    if path.exists() {
        return Err(format!("task worktree already exists: {}", path.display()));
    }
    let branch = format!("e/{session_id}");
    let out = std::process::Command::new("git")
        .args(["worktree", "add", "-b", &branch])
        .arg(&path)
        .arg("HEAD")
        .current_dir(&repo)
        .output()
        .map_err(|e| format!("could not start git while creating the task worktree: {e}"))?;
    if !out.status.success() {
        let detail = String::from_utf8_lossy(&out.stderr).trim().to_string();
        let _ = std::fs::remove_dir_all(&path);
        return Err(format!(
            "could not create a Git worktree for this task{}",
            if detail.is_empty() { String::new() } else { format!(": {detail}") }
        ));
    }
    Ok(Some(TaskWorktree {
        workspace: path.to_string_lossy().to_string(),
        base: repo,
        branch,
    }))
}

fn provision_session_worktree(
    state: &tauri::State<AppState>,
    meta: engine::sessions::SessionMeta,
    enabled: bool,
) -> Result<engine::sessions::SessionMeta, String> {
    if !enabled {
        return Ok(meta);
    }
    match create_task_worktree(&meta.workspace, &meta.id) {
        Ok(Some(worktree)) => {
            let mut managed = meta.clone();
            managed.workspace = worktree.workspace.clone();
            managed.managed_worktree = true;
            managed.worktree_base = worktree.base.clone();
            managed.worktree_branch = worktree.branch.clone();
            let saved = state.store.lock().map_err(|_| "lock".to_string()).and_then(|mut store| {
                store.set_managed_worktree(
                    &meta.id,
                    &worktree.workspace,
                    &worktree.base,
                    &worktree.branch,
                )
                .ok_or_else(|| "task disappeared while its worktree was being created".to_string())
            });
            if saved.is_err() {
                let _ = remove_task_worktree(&managed);
            }
            saved
        }
        Ok(None) => Ok(meta),
        Err(e) => {
            if let Ok(mut store) = state.store.lock() {
                store.remove(&meta.id);
            }
            Err(e)
        }
    }
}

fn remove_task_worktree(meta: &engine::sessions::SessionMeta) -> Result<(), String> {
    remove_task_worktree_in(meta, &worktrees_root())
}

fn remove_task_worktree_in(
    meta: &engine::sessions::SessionMeta,
    root: &std::path::Path,
) -> Result<(), String> {
    if !meta.managed_worktree {
        return Ok(());
    }
    let path = std::path::PathBuf::from(&meta.workspace);
    let expected = root.join(&meta.id);
    if path != expected {
        return Err(format!(
            "refusing to delete a worktree whose path does not match its task id (expected '{}', got '{}')",
            expected.display(),
            path.display()
        ));
    }
    let base = std::path::Path::new(&meta.worktree_base);
    if path.exists() {
        if !base.is_dir() {
            std::fs::remove_dir_all(&path)
                .map_err(|e| format!("could not delete task worktree '{}': {e}", path.display()))?;
        } else {
            let out = std::process::Command::new("git")
                .args(["worktree", "remove", "--force"])
                .arg(&path)
                .current_dir(base)
                .output()
                .map_err(|e| format!("could not start git while deleting the task worktree: {e}"))?;
            if !out.status.success() {
                let detail = String::from_utf8_lossy(&out.stderr).trim().to_string();
                return Err(format!(
                    "could not delete task worktree '{}'{}",
                    path.display(),
                    if detail.is_empty() { String::new() } else { format!(": {detail}") }
                ));
            }
        }
    } else if base.is_dir() {
        let _ = std::process::Command::new("git")
            .args(["worktree", "prune"])
            .current_dir(base)
            .output();
    }
    if base.is_dir() && !meta.worktree_branch.is_empty() {
        let _ = std::process::Command::new("git")
            .args(["branch", "-D", &meta.worktree_branch])
            .current_dir(base)
            .output();
    }
    Ok(())
}

/// Drop the snapshot stashes `workspace_snapshot` left in the user's own
/// repository for a chat.
///
/// Nothing else ever reclaims them, so without this every snapshot a chat took
/// stays in `git stash list` forever — in the user's real project, long after
/// the chat is gone. Dropping renumbers the remaining entries, so remove them
/// highest-index first.
fn drop_snapshot_stashes(ws: &str, id: &str) {
    let ws = ws.trim();
    if ws.is_empty() || !std::path::Path::new(ws).is_dir() {
        return;
    }
    let tag = format!("e-snapshot-{id}");
    let Ok(out) = std::process::Command::new("git")
        .args(["stash", "list", "--format=%gd %gs"])
        .current_dir(ws)
        .output()
    else {
        return;
    };
    if !out.status.success() {
        return;
    }
    let listing = String::from_utf8_lossy(&out.stdout);
    let mut refs: Vec<&str> = listing
        .lines()
        // Match the whole tag at the end, so chat `s1` cannot claim `s12`'s.
        .filter(|l| l.trim_end().ends_with(&tag))
        .filter_map(|l| l.split_whitespace().next())
        .collect();
    refs.reverse();
    for r in refs {
        let _ = std::process::Command::new("git")
            .args(["stash", "drop", r])
            .current_dir(ws)
            .output();
    }
}

#[tauri::command]
fn delete_session(state: tauri::State<AppState>, id: String) -> Result<(), String> {
    // Stop the run first: otherwise it keeps streaming into a chat that is gone
    // and re-creates its history file on the next save.
    state.cancel(&id);
    engine::pty::kill_session(&id);
    // Read metadata before the chat goes, and let the lock go before shelling
    // out to git so a slow repo cannot block every other chat.
    let meta = state
        .store
        .lock()
        .map_err(|_| "lock")?
        .session(&id)
        .ok_or_else(|| "session not found".to_string())?;
    drop_snapshot_stashes(&meta.workspace, &id);
    remove_task_worktree(&meta)?;
    state.store.lock().map_err(|_| "lock")?.remove(&id);
    Ok(())
}

#[tauri::command]
fn fork_session(state: tauri::State<AppState>, id: String, name: Option<String>) -> Result<serde_json::Value, String> {
    let mut st = state.store.lock().map_err(|_| "lock")?;
    let meta = st.fork(&id, &name.unwrap_or_default()).ok_or("session not found")?;
    let use_worktree = state.config().task_worktrees;
    drop(st);
    let meta = provision_session_worktree(&state, meta, use_worktree)?;
    serde_json::to_value(&meta).map_err(|e| e.to_string())
}

#[tauri::command]
fn rename_session(state: tauri::State<AppState>, id: String, name: String) -> Result<bool, String> {
    let mut st = state.store.lock().map_err(|_| "lock")?;
    Ok(st.rename_session(&id, &name))
}

#[tauri::command]
async fn compact_session(app: tauri::AppHandle, state: tauri::State<'_, AppState>, id: String) -> Result<serde_json::Value, String> {
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

    let mut client_cfg = state.config();
    // Summarise with the chat's own model, on its own provider — compacting a
    // chat must not silently bill (or fail against) a different connection.
    let (sess_model, sess_prov) = state
        .store
        .lock()
        .map(|st| (st.model(&id), st.provider(&id)))
        .unwrap_or_default();
    if !sess_model.is_empty() {
        client_cfg.use_model(&sess_model, &sess_prov);
    }
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

    // Read before the config is taken apart to build the connection.
    let effort = client_cfg.active_reasoning_effort();
    let provider = engine::provider::ChatProvider::new(
        client_cfg.base_url,
        client_cfg.api_key,
        client_cfg.model,
        client_cfg.temperature,
        // Summarising with the chat's own model means its own reasoning level
        // too, so compaction can't fail against a model that requires one.
        effort,
    );
    let prompt = vec![Msg::text(
        "user",
        format!(
            "Summarize the following conversation so far into a tight summary. Include: tasks completed, decisions made, key facts, files touched, and open questions. Output ONLY the summary text, no preamble.\n\n=== CONVERSATION ===\n{}",
            old_text
        ),
    )];
    // Compaction is a provider call like any other, so it gets the same retry
    // treatment — and the same on-screen explanation while it waits. That wait
    // is exactly when Stop gets pressed, so it is cancellable too.
    let cancelled = state.begin_compaction(&id);
    let emit = AppEmitter { handle: app, session: id.clone() };
    let summary = provider
        .chat(&prompt, &[], |_| {}, |_| {}, |n| emit.retry(n), &cancelled)
        .await
        .map(|c| c.text)
        .unwrap_or_default();
    state.end_compaction(&id);
    if cancelled.load(Ordering::SeqCst) {
        // Stopped mid-summary: leave the history untouched and say so, so the
        // caller drops the send instead of treating it as a failed compaction.
        return Ok(serde_json::json!({ "messages": hist.len(), "compacted": false, "stopped": true }));
    }
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

/// Usable context window (tokens) for a chat's model. Optional `sid` so the
/// budget follows the chat's own model/provider rather than the global one.
#[tauri::command]
fn context_budget(state: tauri::State<AppState>, sid: Option<String>) -> u64 {
    let cfg = state.config();
    let sid = sid.unwrap_or_default();
    if sid.is_empty() {
        return cfg.active_context_window();
    }
    let (model, prov) = state
        .store
        .lock()
        .map(|st| (st.model(&sid), st.provider(&sid)))
        .unwrap_or_default();
    if model.is_empty() {
        cfg.active_context_window()
    } else {
        cfg.context_window_for(&model, &prov)
    }
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
    let provider = st.provider(&id);
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
        "provider": provider,
        "running": state.is_running(&id),
        // Lets the UI reason about context pressure before the first run of
        // this app session reports real usage. Counts tool traffic, which the
        // message list above deliberately omits.
        "context_estimate": h.iter().map(engine::est_tokens).sum::<usize>(),
    }))
}

/// Fetch a provider's model listing and fold it into what that provider
/// already knows. The merge lives here, not in the UI, so the rule that a
/// refresh never overwrites a hand-set window or a chosen level is enforced in
/// one tested place. The updated provider is returned rather than saved:
/// Settings edits a copy, and Cancel has to really cancel.
#[tauri::command]
async fn refresh_models(
    provider: engine::agent::ProviderItem,
) -> Result<engine::agent::ProviderItem, String> {
    let mut provider = provider;
    let client = reqwest::Client::new();
    let url = format!("{}/models", provider.base_url.trim_end_matches('/'));
    let mut req = client.get(&url);
    if !provider.api_key.is_empty() {
        req = req.bearer_auth(&provider.api_key);
    }
    let resp = req.send().await.map_err(|e| format!("request failed: {e}"))?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        let cut: String = body.chars().take(300).collect();
        return Err(format!("provider returned {status}: {cut}"));
    }
    let v: serde_json::Value = resp.json().await.map_err(|e| format!("bad json: {e}"))?;
    let listed = engine::provider::parse_models(&v);
    if listed.is_empty() {
        // Absorbing nothing would blank a working shelf on a malformed reply.
        return Err("the provider listed no models".into());
    }
    provider.absorb(listed);
    Ok(provider)
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
    let (history, session_ws, session_model, session_prov, project) = match state.store.lock() {
        Ok(st) => (
            st.get_history(&session),
            st.resolved_workspace(&session),
            st.model(&session),
            st.provider(&session),
            st.project_context(&session),
        ),
        Err(_) => {
            state.end_run(&session);
            return Err("internal lock".into());
        }
    };
    // The chat's own project decides where tools run. Falling back to the
    // global setting here is what let a chat in one project (or in the
    // project-less Tasks area) operate on an unrelated project's folder.
    if !session_ws.is_empty() {
        config.workspace = session_ws;
    }
    if !session_model.is_empty() {
        // Follow the model to its provider: a chat can be on a model from any
        // enabled provider, so its base URL and key have to move with it.
        config.use_model(&session_model, &session_prov);
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
                agent.project = project;
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

// ---------- right pane: terminals and files ----------

/// Which chat a pane call is acting for, resolved on this side.
///
/// The renderer passes a chat id; everything else — the folder, which terminals
/// it may touch — is derived from it here. A caller that could name its own
/// workspace would make `fs` a browser for the whole disk and `pty` a shell
/// anywhere, so that decision never leaves the backend.
fn pane_sid(state: &tauri::State<'_, AppState>, sid: Option<String>) -> Result<String, String> {
    let store = state.store.lock().map_err(|_| "lock")?;
    Ok(match sid.map(|s| s.trim().to_string()).filter(|s| !s.is_empty()) {
        Some(s) => s,
        None => store.current.clone(),
    })
}

/// Where a pane view is allowed to look and work: the chat's own project
/// folder, refusing the empty, relative and missing cases with the same advice
/// the tools give.
fn pane_root(state: &tauri::State<'_, AppState>, sid: &str) -> Result<std::path::PathBuf, String> {
    let ws = {
        let store = state.store.lock().map_err(|_| "lock")?;
        store.resolved_workspace(sid)
    };
    let ws = ws.trim();
    if ws.is_empty() {
        return Err("this chat has no project folder. Pick one for its project (sidebar → ✎).".into());
    }
    let p = std::path::PathBuf::from(ws);
    if !p.is_absolute() {
        return Err(format!(
            "project folder is a relative path ({ws}), so it would resolve differently depending on where the app was started. Pick a real folder for this project (sidebar → ✎)."
        ));
    }
    if !p.is_dir() {
        return Err(format!("project folder does not exist: {ws}"));
    }
    Ok(p)
}

// Every pane command runs off the UI thread. `pty_write` can block for as long
// as the child ignores its stdin, and a directory listing can block on a cold
// disk or a network share; on the main thread either one freezes the window.

#[tauri::command(async)]
fn fs_list(state: tauri::State<'_, AppState>, sid: Option<String>, path: Option<String>) -> Result<engine::browse::Listing, String> {
    let sid = pane_sid(&state, sid)?;
    let root = pane_root(&state, &sid)?;
    engine::browse::list(&root, &path.unwrap_or_default())
}

#[tauri::command(async)]
fn fs_read(state: tauri::State<'_, AppState>, sid: Option<String>, path: String) -> Result<engine::browse::FileText, String> {
    let sid = pane_sid(&state, sid)?;
    let root = pane_root(&state, &sid)?;
    engine::browse::read_text(&root, &path)
}

#[tauri::command(async)]
fn pty_spawn(state: tauri::State<'_, AppState>, sid: Option<String>, id: String, cols: u16, rows: u16) -> Result<(), String> {
    let sid = pane_sid(&state, sid)?;
    let root = pane_root(&state, &sid)?;
    engine::pty::spawn(&sid, &id, &root, cols, rows)
}

#[tauri::command(async)]
fn pty_write(state: tauri::State<'_, AppState>, sid: Option<String>, id: String, data: String) -> Result<(), String> {
    let sid = pane_sid(&state, sid)?;
    engine::pty::write(&sid, &id, &data)
}

#[tauri::command(async)]
fn pty_resize(state: tauri::State<'_, AppState>, sid: Option<String>, id: String, cols: u16, rows: u16) -> Result<(), String> {
    let sid = pane_sid(&state, sid)?;
    engine::pty::resize(&sid, &id, cols, rows)
}

#[tauri::command(async)]
fn pty_kill(state: tauri::State<'_, AppState>, sid: Option<String>, id: String) -> Result<(), String> {
    let sid = pane_sid(&state, sid)?;
    engine::pty::kill(&sid, &id)
}

#[tauri::command(async)]
fn pty_alive(state: tauri::State<'_, AppState>, sid: Option<String>, id: String) -> bool {
    match pane_sid(&state, sid) {
        Ok(sid) => engine::pty::alive(&sid, &id),
        Err(_) => false,
    }
}

pub fn run() {
    let config = Config::from_env();
    let state = AppState {
        config: Mutex::new(config),
        tools: Arc::new(ToolRegistry::new()),
        store: Mutex::new(engine::sessions::SessionStore::new()),
        runs: Mutex::new(HashMap::new()),
        compactions: Mutex::new(HashMap::new()),
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
            engine::pty::init(app.handle().clone());
            // MCP servers start against the chat's project folder, so a
            // server told to serve "." serves the project rather than
            // wherever the app was launched from.
            {
                let h = app.handle().clone();
                std::thread::spawn(move || {
                    let state = h.state::<AppState>();
                    let ws = state.store.lock().ok().map(|s| s.resolved_workspace(&s.current.clone()));
                    engine::mcp::load(state.tools.clone(), ws.filter(|w| !w.is_empty()));
                });
            }
            Ok(())
        })
        .manage(state)
        .invoke_handler(tauri::generate_handler![
            get_config,
            save_config,
            list_models,
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
            set_plugin_enabled,
            set_plugin_tools,
            plugin_tool_result,
            set_plugin_veto,
            plugin_veto_result,
            list_mcp_servers,
            reload_extensions,
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
            ui_ready,
            fs_list,
            fs_read,
            pty_spawn,
            pty_write,
            pty_resize,
            pty_kill,
            pty_alive
        ])
        .build(tauri::generate_context!())
        .expect("error while building e")
        // Every way out of the app ends here — the confirmed quit, a second
        // click on the close button while the prompt is up, and a close before
        // the UI ever booted. A pty outlives its parent on both platforms, so
        // killing them anywhere less final leaves an abandoned shell holding
        // the project folder open, which is what makes the next `git`
        // operation fail for no visible reason.
        .run(|_app, event| {
            if let tauri::RunEvent::Exit = event {
                engine::pty::shutdown();
            }
        });
}
#[cfg(test)]
mod stash_tests {
    use super::drop_snapshot_stashes;
    use std::path::Path;
    use std::process::Command;

    fn git(dir: &Path, args: &[&str]) -> String {
        let out = Command::new("git").args(args).current_dir(dir).output().expect("git");
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    fn snapshot(dir: &Path, body: &str, tag: &str) {
        std::fs::write(dir.join("f.txt"), body).unwrap();
        let sha = git(dir, &["stash", "create"]);
        assert!(!sha.is_empty(), "stash create produced nothing");
        git(dir, &["stash", "store", "-m", tag, &sha]);
    }

    #[test]
    fn drops_only_the_deleted_chats_snapshots() {
        let dir = std::env::temp_dir().join(format!("e-stash-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        git(&dir, &["init"]);
        git(&dir, &["config", "user.email", "t@e.test"]);
        git(&dir, &["config", "user.name", "t"]);
        std::fs::write(dir.join("f.txt"), "base").unwrap();
        git(&dir, &["add", "-A"]);
        git(&dir, &["commit", "-m", "base"]);

        // Two snapshots for s1, plus lookalikes that must survive: `s12` shares
        // s1's prefix, and a hand-made stash is none of our business.
        snapshot(&dir, "one", "e-snapshot-s1");
        snapshot(&dir, "two", "e-snapshot-s12");
        snapshot(&dir, "three", "e-snapshot-s1");
        snapshot(&dir, "four", "manual-thing");
        assert_eq!(git(&dir, &["stash", "list"]).lines().count(), 4);

        drop_snapshot_stashes(dir.to_str().unwrap(), "s1");

        let left = git(&dir, &["stash", "list", "--format=%gs"]);
        let _ = std::fs::remove_dir_all(&dir);
        let left: Vec<&str> = left.lines().map(|l| l.trim()).collect();
        assert_eq!(left, vec!["manual-thing", "e-snapshot-s12"], "left: {left:?}");
    }

    #[test]
    fn a_missing_folder_is_not_an_error() {
        drop_snapshot_stashes("C:/definitely/not/here", "s1");
        drop_snapshot_stashes("", "s1");
    }
}

#[cfg(test)]
mod worktree_tests {
    use super::{create_task_worktree_in, remove_task_worktree_in};
    use crate::engine::sessions::SessionMeta;
    use std::path::Path;
    use std::process::Command;

    fn git(dir: &Path, args: &[&str]) -> String {
        let out = Command::new("git").args(args).current_dir(dir).output().expect("git");
        assert!(out.status.success(), "git {:?}: {}", args, String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    #[test]
    fn managed_worktree_is_created_and_removed_with_its_branch() {
        let temp = std::env::temp_dir().join(format!(
            "e-worktree-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let repo = temp.join("repo");
        let worktrees = temp.join("worktrees");
        std::fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "--quiet"]);
        git(&repo, &["config", "user.email", "e-tests@example.invalid"]);
        git(&repo, &["config", "user.name", "e tests"]);
        std::fs::write(repo.join("README.md"), "test").unwrap();
        git(&repo, &["add", "README.md"]);
        git(&repo, &["commit", "--quiet", "-m", "initial"]);

        let task = create_task_worktree_in(repo.to_str().unwrap(), "s-test", &worktrees)
            .unwrap()
            .expect("Git project");
        assert!(Path::new(&task.workspace).is_dir());
        assert!(git(&repo, &["branch", "--list", "e/s-test"]).contains("e/s-test"));

        let meta = SessionMeta {
            id: "s-test".into(),
            name: "test".into(),
            created: 0,
            workspace: task.workspace.clone(),
            project: "p1".into(),
            model: String::new(),
            provider: String::new(),
            state: String::new(),
            managed_worktree: true,
            worktree_base: task.base,
            worktree_branch: task.branch,
        };
        remove_task_worktree_in(&meta, &worktrees).unwrap();
        assert!(!Path::new(&task.workspace).exists());
        assert!(git(&repo, &["branch", "--list", "e/s-test"]).is_empty());
        let _ = std::fs::remove_dir_all(temp);
    }
}
