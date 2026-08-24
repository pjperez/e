//! Plugin discovery and the bridge between the engine and the TypeScript host.
//!
//! A plugin is a folder — `plugin.json` plus an ES module — under
//! `~/.e/plugins` (global) or `<workspace>/.e/plugins` (project). Nothing is
//! installed: the folder *is* the installation, so a plugin can be read,
//! diffed and deleted with ordinary tools.
//!
//! Rust only ever *finds* plugins and routes tool calls to them. The module
//! itself runs in the webview, which is the only JavaScript runtime in the
//! app; that is what keeps this file (and the core) small.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, LazyLock, Mutex, OnceLock};
use tauri::Emitter;

/// Everything a plugin may ask for. A manifest naming anything else is a
/// mistake we surface rather than ignore — silently dropping an unknown
/// capability would let a typo ("net" for "network") look like a granted one.
pub const CAPABILITIES: [&str; 6] = ["tools", "events", "commands", "ui", "network", "session-read"];

/// A plugin tool definition, registered from the frontend after the module
/// asked for it.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PluginToolDef {
    pub name: String,
    pub description: String,
    #[serde(default)]
    pub parameters: serde_json::Value,
    /// Which plugin registered it, for error messages and the Extensions list.
    #[serde(default)]
    pub plugin: String,
}

/// The on-disk `plugin.json`.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct PluginManifest {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub capabilities: Vec<String>,
    #[serde(default)]
    pub entry: String,
}

/// What the UI and the loader see: the manifest plus where it came from and
/// whether it can run at all.
#[derive(Clone, Debug, Serialize)]
pub struct PluginInfo {
    /// Folder name. This is the identity everywhere — `get_plugin`, the
    /// disabled list — because it is the one thing guaranteed unique and
    /// unable to drift from what is on disk.
    pub name: String,
    /// The manifest's own name, for display. Falls back to the folder name.
    pub display: String,
    pub version: String,
    pub description: String,
    pub capabilities: Vec<String>,
    pub entry: String,
    pub dir: String,
    /// "global" (`~/.e/plugins`) or "project" (`<workspace>/.e/plugins`).
    pub scope: String,
    pub enabled: bool,
    /// Why this plugin cannot load. Empty when it is fine.
    pub error: String,
}

static HOST: OnceLock<PluginHost> = OnceLock::new();
static PENDING: LazyLock<Mutex<HashMap<String, mpsc::Sender<(bool, String)>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static VETO_PENDING: LazyLock<Mutex<HashMap<String, mpsc::Sender<Option<String>>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
/// Set by the host once at least one plugin listens for `tool_call`. While it
/// is false the engine never asks, so the hook costs nothing.
static VETO_ACTIVE: AtomicBool = AtomicBool::new(false);
static SEQ: AtomicU64 = AtomicU64::new(0);

/// How long a plugin tool may take before the engine gives up on it.
const TOOL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(180);
/// How long the veto round-trip may take. Short on purpose: it runs before
/// *every* tool call, and a wedged handler must not stall the agent loop.
const VETO_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

pub struct PluginHost {
    handle: tauri::AppHandle,
}

pub fn init(handle: tauri::AppHandle) {
    let _ = HOST.set(PluginHost { handle });
}

fn next_id(prefix: &str) -> String {
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    // Millisecond timestamps collide when two calls happen in the same tick,
    // which used to make one call steal the other's reply.
    format!("{prefix}{}_{}", ms, SEQ.fetch_add(1, Ordering::Relaxed))
}

/// Called by the proxy tool when the model requests a plugin tool. Emits a
/// request to the frontend and blocks until `resolve` answers (or a timeout).
pub fn request(name: &str, args: serde_json::Value) -> Result<String, String> {
    let host = HOST.get().ok_or_else(|| {
        "plugin tools need the desktop app: there is no plugin host in this process".to_string()
    })?;
    let sid = next_id("pt");
    let (tx, rx) = mpsc::channel();
    if let Ok(mut p) = PENDING.lock() {
        p.insert(sid.clone(), tx);
    }
    let _ = host.handle.emit(
        "e:plugin_tool_call",
        serde_json::json!({ "sid": sid, "name": name, "arguments": args.to_string() }),
    );
    match rx.recv_timeout(TOOL_TIMEOUT) {
        Ok((true, out)) => Ok(out),
        Ok((false, err)) => Err(err),
        Err(_) => {
            if let Ok(mut p) = PENDING.lock() {
                p.remove(&sid);
            }
            Err(format!("plugin tool '{name}' timed out after {}s", TOOL_TIMEOUT.as_secs()))
        }
    }
}

pub fn resolve(sid: &str, ok: bool, output: String) {
    let tx = PENDING.lock().ok().and_then(|mut p| p.remove(sid));
    if let Some(tx) = tx {
        let _ = tx.send((ok, output));
    }
}

/// The frontend tells us whether any plugin is actually watching tool calls.
pub fn set_veto_active(active: bool) {
    VETO_ACTIVE.store(active, Ordering::SeqCst);
}

pub fn veto_active() -> bool {
    VETO_ACTIVE.load(Ordering::SeqCst)
}

/// Ask the plugin host whether this tool call may proceed. `Some(reason)`
/// blocks it. Anything that goes wrong — no host, no answer, a slow handler —
/// allows the call: a plugin that fails must not be able to freeze the agent.
pub fn veto(session: &str, tool: &str, args: &serde_json::Value) -> Option<String> {
    if !veto_active() {
        return None;
    }
    let host = HOST.get()?;
    let id = next_id("veto");
    let (tx, rx) = mpsc::channel();
    VETO_PENDING.lock().ok()?.insert(id.clone(), tx);
    let _ = host.handle.emit(
        "e:plugin_veto_request",
        serde_json::json!({ "id": id, "sid": session, "tool": tool, "arguments": args.to_string() }),
    );
    let answer = rx.recv_timeout(VETO_TIMEOUT).unwrap_or(None);
    if let Ok(mut p) = VETO_PENDING.lock() {
        p.remove(&id);
    }
    answer
}

pub fn veto_resolve(id: &str, reason: Option<String>) {
    let tx = VETO_PENDING.lock().ok().and_then(|mut p| p.remove(id));
    if let Some(tx) = tx {
        let _ = tx.send(reason.filter(|r| !r.trim().is_empty()));
    }
}

/// Global first, project second: a project plugin with the same folder name
/// deliberately shadows the global one, the way project config shadows global
/// config. `workspace` is the chat's project folder, never the process's
/// current directory — where the app was launched from must not decide which
/// code runs.
fn plugin_dirs(workspace: Option<&str>) -> Vec<(String, PathBuf)> {
    let mut dirs = Vec::new();
    if let Some(home) = dirs::home_dir() {
        dirs.push(("global".to_string(), home.join(".e/plugins")));
    }
    if let Some(ws) = workspace.map(str::trim).filter(|w| !w.is_empty()) {
        let p = Path::new(ws);
        if p.is_absolute() {
            dirs.push(("project".to_string(), p.join(".e/plugins")));
        }
    }
    dirs
}

fn read_info(folder: &str, dir: &Path, scope: &str) -> PluginInfo {
    let mut info = PluginInfo {
        name: folder.to_string(),
        display: folder.to_string(),
        version: String::new(),
        description: String::new(),
        capabilities: Vec::new(),
        entry: "index.js".to_string(),
        dir: dir.to_string_lossy().to_string(),
        scope: scope.to_string(),
        enabled: true,
        error: String::new(),
    };
    let text = match std::fs::read_to_string(dir.join("plugin.json")) {
        Ok(t) => t,
        Err(e) => {
            info.error = format!("cannot read plugin.json: {e}");
            return info;
        }
    };
    let mf: PluginManifest = match serde_json::from_str(&text) {
        Ok(m) => m,
        Err(e) => {
            info.error = format!("plugin.json is not valid JSON: {e}");
            return info;
        }
    };
    if !mf.name.trim().is_empty() {
        info.display = mf.name.trim().to_string();
    }
    info.version = mf.version;
    info.description = mf.description;
    info.capabilities = mf.capabilities.iter().map(|c| c.trim().to_string()).collect();
    if !mf.entry.trim().is_empty() {
        info.entry = mf.entry.trim().to_string();
    }

    let unknown: Vec<&str> = info
        .capabilities
        .iter()
        .filter(|c| !CAPABILITIES.contains(&c.as_str()))
        .map(|c| c.as_str())
        .collect();
    if !unknown.is_empty() {
        info.error = format!(
            "unknown capabilit{}: {} (known: {})",
            if unknown.len() == 1 { "y" } else { "ies" },
            unknown.join(", "),
            CAPABILITIES.join(", ")
        );
        return info;
    }
    if !dir.join(&info.entry).is_file() {
        info.error = format!("entry file '{}' is missing", info.entry);
    }
    info
}

/// Every plugin folder found, broken ones included — a plugin that cannot load
/// is something to show the user, not something to hide.
pub fn discover(workspace: Option<&str>) -> Vec<PluginInfo> {
    let mut found: HashMap<String, PluginInfo> = HashMap::new();
    for (scope, d) in plugin_dirs(workspace) {
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            if !e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                continue;
            }
            let folder = e.file_name().to_string_lossy().to_string();
            if folder.starts_with('.') {
                continue;
            }
            let info = read_info(&folder, &e.path(), &scope);
            found.insert(folder, info);
        }
    }
    let mut out: Vec<PluginInfo> = found.into_values().collect();
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// The manifest plus the module source, so the host can load it and Settings
/// can show exactly what it is about to run.
pub fn get_plugin(name: &str, workspace: Option<&str>) -> Result<(PluginInfo, String), String> {
    let mut dirs = plugin_dirs(workspace);
    // Project wins, so look there first.
    dirs.reverse();
    for (scope, d) in dirs {
        let dir = d.join(name);
        if !dir.is_dir() {
            continue;
        }
        let info = read_info(name, &dir, &scope);
        if !info.error.is_empty() {
            return Err(format!("plugin '{name}': {}", info.error));
        }
        let source = std::fs::read_to_string(dir.join(&info.entry))
            .map_err(|e| format!("plugin '{name}': cannot read {}: {e}", info.entry))?;
        return Ok((info, source));
    }
    Err(format!("plugin '{name}' not found"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, name: &str, body: &str) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join(name), body).unwrap();
    }

    fn temp(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("e-plugin-test-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn a_plugin_is_identified_by_its_folder_not_its_manifest_name() {
        let ws = temp("folder-id");
        let dir = ws.join(".e/plugins/git-guard");
        write(&dir, "plugin.json", r#"{"name":"Git Guard","version":"0.2.0"}"#);
        write(&dir, "index.js", "export default () => {};");

        let found = discover(Some(ws.to_str().unwrap()));
        let p = found.iter().find(|p| p.name == "git-guard").expect("discovered");
        assert_eq!(p.display, "Git Guard");
        assert_eq!(p.scope, "project");
        assert!(p.error.is_empty(), "{}", p.error);
        // Lookup uses the folder name, which is what the UI hands back.
        assert!(get_plugin("git-guard", Some(ws.to_str().unwrap())).is_ok());
        let _ = std::fs::remove_dir_all(&ws);
    }

    #[test]
    fn an_unknown_capability_is_an_error_not_a_silent_grant() {
        let ws = temp("bad-cap");
        let dir = ws.join(".e/plugins/typo");
        write(&dir, "plugin.json", r#"{"capabilities":["net"]}"#);
        write(&dir, "index.js", "export default () => {};");

        let found = discover(Some(ws.to_str().unwrap()));
        let p = found.iter().find(|p| p.name == "typo").expect("discovered");
        assert!(p.error.contains("unknown capabilit"), "{}", p.error);
        assert!(get_plugin("typo", Some(ws.to_str().unwrap())).is_err());
        let _ = std::fs::remove_dir_all(&ws);
    }

    #[test]
    fn a_broken_plugin_is_reported_rather_than_hidden() {
        let ws = temp("broken");
        write(&ws.join(".e/plugins/half-written"), "plugin.json", "{ oops");
        write(&ws.join(".e/plugins/no-entry"), "plugin.json", r#"{"entry":"main.js"}"#);

        let found = discover(Some(ws.to_str().unwrap()));
        let broken = found.iter().find(|p| p.name == "half-written").expect("discovered");
        assert!(broken.error.contains("not valid JSON"), "{}", broken.error);
        let missing = found.iter().find(|p| p.name == "no-entry").expect("discovered");
        assert!(missing.error.contains("missing"), "{}", missing.error);
        let _ = std::fs::remove_dir_all(&ws);
    }

    /// A relative workspace resolves against wherever the app was launched
    /// from, so it must never contribute plugin folders.
    #[test]
    fn a_relative_workspace_contributes_nothing() {
        let dirs = plugin_dirs(Some("some/relative/dir"));
        assert!(dirs.iter().all(|(scope, _)| scope == "global"));
    }

    /// The guard hook must fail open. A plugin that never answers — or a
    /// process with no plugin host at all, like `e-rpc` — cannot be allowed to
    /// block every tool call in the app.
    #[test]
    fn the_veto_hook_allows_the_call_when_nothing_can_answer() {
        set_veto_active(false);
        assert_eq!(veto("chat", "shell", &serde_json::json!({})), None);

        set_veto_active(true);
        // No host is initialised in a test binary, so this is the headless case.
        assert_eq!(veto("chat", "shell", &serde_json::json!({})), None);
        set_veto_active(false);
    }
}
