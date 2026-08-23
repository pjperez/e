use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{LazyLock, mpsc, Mutex, OnceLock};
use tauri::Emitter;

/// A plugin tool definition registered from the frontend.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PluginToolDef {
    pub name: String,
    pub description: String,
    #[serde(default)]
    pub parameters: serde_json::Value,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PluginManifest {
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

static HOST: OnceLock<PluginHost> = OnceLock::new();
static PENDING: LazyLock<Mutex<HashMap<String, mpsc::Sender<(bool, String)>>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

pub struct PluginHost {
    handle: tauri::AppHandle,
}

pub fn init(handle: tauri::AppHandle) {
    let _ = HOST.set(PluginHost { handle });
}

/// Called by a proxy Tool when the model requests a plugin tool. Emits a request
/// to the frontend and blocks until `resolve` answers (or a timeout).
pub fn request(sid: &str, name: &str, args: serde_json::Value) -> Result<String, String> {
    let host = HOST.get().ok_or_else(|| "plugin host not initialized".to_string())?;
    let (tx, rx) = mpsc::channel();
    PENDING.lock().unwrap().insert(sid.to_string(), tx);
    let _ = host.handle.emit(
        "e:plugin_tool_call",
        serde_json::json!({ "sid": sid, "name": name, "arguments": args.to_string() }),
    );
    match rx.recv_timeout(std::time::Duration::from_secs(180)) {
        Ok((ok, out)) => {
            if ok {
                Ok(out)
            } else {
                Err(out)
            }
        }
        Err(_) => {
            PENDING.lock().unwrap().remove(sid);
            Err("plugin tool timed out".to_string())
        }
    }
}

pub fn resolve(sid: &str, ok: bool, output: String) {
    if let Some(tx) = PENDING.lock().unwrap().remove(sid) {
        let _ = tx.send((ok, output));
    }
}

fn plugin_dirs() -> Vec<PathBuf> {
    let home = dirs::home_dir().unwrap_or_default();
    let mut dirs = vec![home.join(".e/plugins")];
    if let Ok(cwd) = std::env::current_dir() {
        dirs.push(cwd.join(".e/plugins"));
    }
    dirs
}

fn read_manifest(dir: &PathBuf) -> Option<PluginManifest> {
    let mf = dir.join("plugin.json");
    let text = std::fs::read_to_string(&mf).ok()?;
    serde_json::from_str(&text).ok()
}

pub fn discover() -> Vec<PluginManifest> {
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for d in plugin_dirs() {
        if let Ok(rd) = std::fs::read_dir(&d) {
            for e in rd.flatten() {
                if e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                    let name = e.file_name().to_string_lossy().to_string();
                    if seen.insert(name.clone()) {
                        if let Some(m) = read_manifest(&e.path()) {
                            out.push(m);
                        }
                    }
                }
            }
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

pub fn get_plugin(name: &str) -> Option<(PluginManifest, String)> {
    for d in plugin_dirs() {
        let dir = d.join(name);
        if !dir.is_dir() {
            continue;
        }
        let mf = read_manifest(&dir)?;
        let source = {
            let entry = if mf.entry.is_empty() { "index.js".to_string() } else { mf.entry.clone() };
            std::fs::read_to_string(dir.join(entry)).ok()?
        };
        return Some((mf, source));
    }
    None
}
