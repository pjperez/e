use crate::engine::Msg;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProjectMeta {
    pub id: String,
    pub name: String,
    pub workspace: String,
    pub created: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SessionMeta {
    pub id: String,
    pub name: String,
    pub created: u64,
    #[serde(default)]
    pub workspace: String,
    #[serde(default)]
    pub project: String,
    #[serde(default)]
    pub model: String,
    /// Provider that serves `model`. Models can be picked from any enabled
    /// provider, so the chat has to remember which connection it chose or the
    /// next run would send its model to whatever provider happened to be
    /// active globally.
    #[serde(default)]
    pub provider: String,
    #[serde(default)]
    pub state: String,
}

/// File-backed store: ~/.e/sessions/<id>.json (history) + index.json.
/// Projects are the top-level unit; each owns a workspace, sessions live under
/// the current project.
pub struct SessionStore {
    dir: PathBuf,
    index_file: PathBuf,
    pub sessions: Vec<SessionMeta>,
    pub projects: Vec<ProjectMeta>,
    pub current: String,
    pub current_project: String,
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn basename_of(path: &str) -> String {
    std::path::Path::new(path)
        .file_name()
        .map(|n| n.to_string_lossy().trim().to_string())
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| "Project".to_string())
}

/// Workspaces are always stored absolute. A relative path silently resolves
/// against whatever directory the app happened to be launched from, so tools
/// would run somewhere different (or nowhere) on the next start.
fn absolute_workspace(ws: &str) -> String {
    let t = ws.trim();
    let cwd = std::env::current_dir().unwrap_or_default();
    let p = std::path::Path::new(t);
    let abs = if t.is_empty() {
        cwd
    } else if p.is_absolute() {
        p.to_path_buf()
    } else {
        cwd.join(p)
    };
    abs.to_string_lossy().to_string()
}

impl SessionStore {
    pub fn new() -> Self {
        let dir = dirs::home_dir().unwrap_or_default().join(".e/sessions");
        let _ = std::fs::create_dir_all(&dir);
        let index_file = dir.join("index.json");
        let mut st = SessionStore {
            dir,
            index_file,
            sessions: Vec::new(),
            projects: Vec::new(),
            current: String::new(),
            current_project: String::new(),
        };
        st.load();
        if st.projects.is_empty() {
            st.project_create("Chats", "");
        }
        // Migrate: sessions created before projects existed have an empty
        // project -> they would never show in the sidebar. Adopt them into
        // the first project (the general "Chats" folder).
        let fallback = st.projects.first().map(|p| p.id.clone()).unwrap_or_default();
        let mut changed = false;
        for s in st.sessions.iter_mut() {
            if s.project.is_empty() && !fallback.is_empty() {
                s.project = fallback.clone();
                changed = true;
            }
        }
        if changed {
            st.save_index();
        }
        if st.sessions.is_empty() {
            st.create("Chat 1", "", "", "");
        }
        st
    }

    fn load(&mut self) {
        if let Ok(text) = std::fs::read_to_string(&self.index_file) {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) {
                self.current = v.get("current").and_then(|c| c.as_str()).unwrap_or("").to_string();
                self.current_project = v
                    .get("current_project")
                    .and_then(|c| c.as_str())
                    .unwrap_or("")
                    .to_string();
                self.sessions = v
                    .get("sessions")
                    .and_then(|a| a.as_array())
                    .map(|a| a.iter().filter_map(|m| serde_json::from_value(m.clone()).ok()).collect())
                    .unwrap_or_default();
                self.projects = v
                    .get("projects")
                    .and_then(|a| a.as_array())
                    .map(|a| a.iter().filter_map(|m| serde_json::from_value(m.clone()).ok()).collect())
                    .unwrap_or_default();
            }
        }
    }

    fn save_index(&self) {
        let idx = serde_json::json!({
            "current": self.current,
            "current_project": self.current_project,
            "sessions": self.sessions,
            "projects": self.projects,
        });
        let _ = std::fs::write(&self.index_file, serde_json::to_string_pretty(&idx).unwrap_or_default());
    }

    fn file(&self, id: &str) -> PathBuf {
        self.dir.join(format!("{id}.json"))
    }

    /// Read a chat's history, repairing it if a previous run left it in a state
    /// providers reject (see `repair_tool_calls`). The repair is written back
    /// so the chat is fixed once rather than re-patched on every read.
    pub fn get_history(&self, id: &str) -> Vec<Msg> {
        if id.is_empty() {
            return Vec::new();
        }
        let mut msgs: Vec<Msg> = std::fs::read_to_string(self.file(id))
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default();
        if crate::engine::repair_tool_calls(&mut msgs) > 0 {
            self.set_history(id, msgs.clone());
        }
        msgs
    }

    pub fn set_history(&self, id: &str, msgs: Vec<Msg>) {
        if id.is_empty() {
            return;
        }
        if let Ok(t) = serde_json::to_string(&msgs) {
            let _ = std::fs::write(self.file(id), t);
        }
    }

    pub fn workspace(&self, id: &str) -> String {
        self.sessions
            .iter()
            .find(|s| s.id == id)
            .map(|s| s.workspace.clone())
            .unwrap_or_default()
    }

    pub fn model(&self, id: &str) -> String {
        self.sessions
            .iter()
            .find(|s| s.id == id)
            .map(|s| s.model.clone())
            .unwrap_or_default()
    }

    pub fn set_model(&mut self, id: &str, model: &str, provider: &str) {
        if let Some(s) = self.sessions.iter_mut().find(|x| x.id == id) {
            s.model = model.to_string();
            s.provider = provider.to_string();
            self.save_index();
        }
    }

    /// Provider this chat picked its model from; empty means "resolve it".
    pub fn provider(&self, id: &str) -> String {
        self.sessions
            .iter()
            .find(|s| s.id == id)
            .map(|s| s.provider.clone())
            .unwrap_or_default()
    }

    pub fn set_state(&mut self, id: &str, state: &str) {
        if let Some(s) = self.sessions.iter_mut().find(|x| x.id == id) {
            s.state = state.to_string();
            self.save_index();
        }
    }

    // ---- projects ----
    pub fn project_list(&self) -> Vec<ProjectMeta> {
        self.projects.clone()
    }

    pub fn project_workspace(&self, id: &str) -> String {
        self.projects
            .iter()
            .find(|p| p.id == id)
            .map(|p| p.workspace.clone())
            .unwrap_or_default()
    }

    pub fn current_project_workspace(&self) -> String {
        self.project_workspace(&self.current_project)
    }

    pub fn project_create(&mut self, name: &str, workspace: &str) -> ProjectMeta {
        let id = format!("p{}", now_ms());
        let ws = absolute_workspace(workspace);
        let nm = if name.trim().is_empty() {
            basename_of(&ws)
        } else {
            name.trim().to_string()
        };
        let meta = ProjectMeta {
            id: id.clone(),
            name: nm,
            workspace: ws,
            created: now_ms(),
        };
        self.projects.push(meta.clone());
        self.current_project = id;
        self.save_index();
        meta
    }

    /// Repoint a project (and its chats) at another folder. Chats copy the
    /// workspace when they are created, so they have to move too or they keep
    /// running against the old, possibly missing, directory.
    pub fn project_set_workspace(&mut self, id: &str, workspace: &str) -> bool {
        let ws = absolute_workspace(workspace);
        let Some(p) = self.projects.iter_mut().find(|p| p.id == id) else {
            return false;
        };
        p.workspace = ws.clone();
        for s in self.sessions.iter_mut().filter(|s| s.project == id) {
            s.workspace = ws.clone();
        }
        self.save_index();
        true
    }

    pub fn project_rename(&mut self, id: &str, name: &str) -> bool {
        let nm = name.trim();
        if !nm.is_empty() {
            if let Some(p) = self.projects.iter_mut().find(|p| p.id == id) {
                p.name = nm.to_string();
                self.save_index();
                return true;
            }
        }
        false
    }

    /// Delete a project; its chats are moved to the first remaining project
    /// (so nothing is lost). At least one project always remains.
    pub fn project_remove(&mut self, id: &str) -> bool {
        if self.projects.len() <= 1 {
            return false;
        }
        let fallback = self.projects.iter().find(|p| p.id != id).map(|p| p.id.clone());
        self.projects.retain(|p| p.id != id);
        if let Some(fb) = fallback {
            for s in self.sessions.iter_mut() {
                if s.project == id {
                    s.project = fb.clone();
                }
            }
            if self.current_project == id {
                self.current_project = fb;
            }
        }
        if self.current == id {
            self.current = self.sessions.first().map(|s| s.id.clone()).unwrap_or_default();
        }
        self.save_index();
        true
    }

    pub fn project_switch(&mut self, id: &str) -> bool {
        if self.projects.iter().any(|p| p.id == id) {
            self.current_project = id.to_string();
            self.save_index();
            true
        } else {
            false
        }
    }

    // ---- sessions ----
    pub fn create(&mut self, name: &str, workspace: &str, model: &str, provider: &str) -> SessionMeta {        let id = format!("s{}", now_ms());
        let ws = if workspace.trim().is_empty() {
            self.current_project_workspace()
        } else {
            workspace.trim().to_string()
        };
        let n = self.sessions.iter().filter(|s| s.project == self.current_project).count() + 1;
        let meta = SessionMeta {
            id: id.clone(),
            name: if name.trim().is_empty() {
                if n == 1 { "New task".to_string() } else { format!("New task {n}") }
            } else {
                name.trim().to_string()
            },
            created: now_ms(),
            workspace: ws,
            project: self.current_project.clone(),
            model: model.trim().to_string(),
            provider: provider.trim().to_string(),
            state: String::new(),
        };
        self.set_history(&id, Vec::new());
        self.sessions.push(meta.clone());
        self.current = id;
        self.save_index();
        meta
    }

#[allow(dead_code)]
    pub fn sessions_in(&self, project: &str) -> Vec<SessionMeta> {
        self.sessions
            .iter()
            .filter(|s| s.project == project)
            .cloned()
            .collect()
    }

    pub fn rename_session(&mut self, id: &str, name: &str) -> bool {
        let nm = name.trim();
        if !nm.is_empty() {
            if let Some(s) = self.sessions.iter_mut().find(|x| x.id == id) {
                s.name = nm.to_string();
                self.save_index();
                return true;
            }
        }
        false
    }

    /// Non-summarising fallback: keep system + the most recent messages. Only a
    /// safety net for when the model-backed `compact_session` is unavailable.
    pub fn compact(&self, id: &str) -> usize {
        let h = self.get_history(id);
        let non_system = h.iter().filter(|m| m.role != "system").count();
        if non_system > crate::engine::KEEP_RECENT + crate::engine::MIN_COMPACT_GAIN {
            let split = crate::engine::keep_split(&h, crate::engine::agent::default_context_window());
            if split == 0 {
                return h.len();
            }
            let system: Vec<Msg> = h.iter().filter(|m| m.role == "system").cloned().collect();
            let kept = h[split..].to_vec();
            let mut out = system;
            out.extend(kept);
            let n = out.len(); self.set_history(id, out); n
        } else {
            h.len()
        }
    }

    pub fn switch(&mut self, id: &str) -> bool {
        if self.sessions.iter().any(|s| s.id == id) {
            self.current = id.to_string();
            self.save_index();
            true
        } else {
            false
        }
    }

    pub fn remove(&mut self, id: &str) {
        self.sessions.retain(|s| s.id != id);
        let _ = std::fs::remove_file(self.file(id));
        if self.current == id {
            self.current = self.sessions.first().map(|s| s.id.clone()).unwrap_or_default();
        }
        self.save_index();
    }

    pub fn fork(&mut self, id: &str, name: &str) -> Option<SessionMeta> {
        let src = self.sessions.iter().find(|s| s.id == id).cloned()?;
        let history = self.get_history(id);
        let meta = SessionMeta {
            id: format!("s{}", now_ms()),
            name: if name.trim().is_empty() {
                format!("{} (fork)", src.name)
            } else {
                name.trim().to_string()
            },
            created: now_ms(),
            workspace: src.workspace,
            project: src.project,
            model: src.model,
            provider: src.provider,
            state: src.state,
        };
        self.set_history(&meta.id, history);
        self.sessions.push(meta.clone());
        self.current = meta.id.clone();
        self.save_index();
        Some(meta)
    }
}

impl Default for SessionStore {
    fn default() -> Self {
        Self::new()
    }
}
