use crate::engine::Msg;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProjectMeta {
    pub id: String,
    pub name: String,
    pub workspace: String,
    pub created: u64,
    /// The built-in project. It always exists and can never be deleted, so a
    /// chat always has somewhere to live even with no folder opened.
    #[serde(default)]
    pub locked: bool,
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
/// Name of the built-in project. Created on first run and adopted from the
/// older auto-created folder on upgrade.
const TASKS_NAME: &str = "Tasks";

/// Home is the built-in project's folder: general chores aren't tied to a repo,
/// but tools still need a real directory to run in.
fn home_workspace() -> String {
    dirs::home_dir()
        .map(|p| p.to_string_lossy().to_string())
        .filter(|p| !p.trim().is_empty())
        .unwrap_or_else(|| absolute_workspace(""))
}

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
        st.ensure_tasks_project();
        // Migrate: sessions created before projects existed have an empty
        // project -> they would never show in the sidebar. Adopt them into
        // the first project (the built-in "Tasks" folder).
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
            st.create("Chat 1", "", "");
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

    pub fn set_model(&mut self, id: &str, model: &str) {
        if let Some(s) = self.sessions.iter_mut().find(|x| x.id == id) {
            s.model = model.to_string();
            self.save_index();
        }
    }

    pub fn set_state(&mut self, id: &str, state: &str) {
        if let Some(s) = self.sessions.iter_mut().find(|x| x.id == id) {
            s.state = state.to_string();
            self.save_index();
        }
    }

    // ---- projects ----
    /// Guarantee the built-in project: named "Tasks", rooted at the user's home
    /// directory, always first in the list and never deletable.
    ///
    /// Installs made before it existed carry an auto-created "Default"/"Chats"
    /// folder. Adopt that one instead of adding a second general folder, so the
    /// chats already in it stay where the user left them. It is always the first
    /// project and always carries a name the app generated rather than a folder
    /// basename, so match on position and name — its workspace is unreliable
    /// (older builds left it empty, newer ones filled it with the launch
    /// directory).
    fn ensure_tasks_project(&mut self) {
        if self.projects.iter().any(|p| p.locked) {
            return;
        }
        let home = home_workspace();
        let adopt = matches!(
            self.projects.first().map(|p| p.name.trim()),
            Some("Default") | Some("Chats") | Some(TASKS_NAME)
        );
        if adopt {
            let p = &mut self.projects[0];
            p.name = TASKS_NAME.to_string();
            p.workspace = home.clone();
            p.locked = true;
            let id = p.id.clone();
            // Chats copy their project's workspace, so they have to move too or
            // they keep running wherever the app was launched from.
            for s in self.sessions.iter_mut().filter(|s| s.project == id) {
                s.workspace = home.clone();
            }
        } else {
            self.projects.insert(
                0,
                ProjectMeta {
                    id: format!("p{}", now_ms()),
                    name: TASKS_NAME.to_string(),
                    workspace: home,
                    created: now_ms(),
                    locked: true,
                },
            );
        }
        if !self.projects.iter().any(|p| p.id == self.current_project) {
            self.current_project = self.projects[0].id.clone();
        }
        self.save_index();
    }

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
            locked: false,
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
    /// (so nothing is lost). The built-in project can never be removed.
    pub fn project_remove(&mut self, id: &str) -> bool {
        if self.projects.iter().any(|p| p.id == id && p.locked) {
            return false;
        }
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
    pub fn create(&mut self, name: &str, workspace: &str, model: &str) -> SessionMeta {
        let id = format!("s{}", now_ms());
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static SEQ: AtomicU64 = AtomicU64::new(0);

    /// A store rooted in a throwaway directory, so tests never touch the real
    /// `~/.e/sessions` index.
    fn store() -> SessionStore {
        let dir = std::env::temp_dir().join(format!(
            "e-sessions-test-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::create_dir_all(&dir);
        let index_file = dir.join("index.json");
        SessionStore {
            dir,
            index_file,
            sessions: Vec::new(),
            projects: Vec::new(),
            current: String::new(),
            current_project: String::new(),
        }
    }

    fn project(id: &str, name: &str, workspace: &str) -> ProjectMeta {
        ProjectMeta {
            id: id.to_string(),
            name: name.to_string(),
            workspace: workspace.to_string(),
            created: 0,
            locked: false,
        }
    }

    fn session(id: &str, project: &str, workspace: &str) -> SessionMeta {
        SessionMeta {
            id: id.to_string(),
            name: id.to_string(),
            created: 0,
            workspace: workspace.to_string(),
            project: project.to_string(),
            model: String::new(),
            state: String::new(),
        }
    }

    #[test]
    fn upgrade_adopts_the_old_default_folder_instead_of_duplicating_it() {
        let mut st = store();
        st.projects = vec![project("p1", "Default", "")];
        st.sessions = vec![session("s1", "p1", "")];
        st.ensure_tasks_project();

        assert_eq!(st.projects.len(), 1, "must not add a second general folder");
        assert_eq!(st.projects[0].id, "p1", "existing chats must stay in place");
        assert_eq!(st.projects[0].name, "Tasks");
        assert!(st.projects[0].locked);
        assert_eq!(st.projects[0].workspace, home_workspace());
        assert_eq!(st.sessions[0].workspace, home_workspace(), "chats follow their project's folder");
    }

    #[test]
    fn the_auto_created_folder_is_adopted_even_with_a_workspace_already_set() {
        // Newer builds seeded it with the launch directory rather than "",
        // which must not be mistaken for a real project.
        let mut st = store();
        st.projects = vec![project("p1", "Chats", "C:/some/launch/dir"), project("p2", "GenieX", "C:/src/GenieX")];
        st.ensure_tasks_project();

        assert_eq!(st.projects.len(), 2, "must not add a second general folder");
        assert_eq!(st.projects[0].id, "p1");
        assert_eq!(st.projects[0].name, "Tasks");
        assert_eq!(st.projects[0].workspace, home_workspace());
        assert!(st.projects[0].locked);
    }

    #[test]
    fn a_real_project_is_never_repurposed_as_the_built_in_one() {
        let mut st = store();
        st.projects = vec![project("p1", "GenieX", "C:/src/GenieX")];
        st.current_project = "p1".to_string();
        st.ensure_tasks_project();

        assert_eq!(st.projects.len(), 2);
        assert_eq!(st.projects[0].name, "Tasks", "the built-in project sorts first");
        assert!(st.projects[0].locked);
        assert_eq!(st.projects[1].name, "GenieX", "a real project keeps its name");
        assert_eq!(st.projects[1].workspace, "C:/src/GenieX", "and its folder");
        assert_eq!(st.current_project, "p1", "a valid selection is left alone");
    }

    #[test]
    fn ensuring_twice_does_not_stack_up_built_in_projects() {
        let mut st = store();
        st.ensure_tasks_project();
        let id = st.projects[0].id.clone();
        st.ensure_tasks_project();

        assert_eq!(st.projects.len(), 1);
        assert_eq!(st.projects[0].id, id);
        assert_eq!(st.current_project, id, "an empty selection falls back to the built-in project");
    }

    #[test]
    fn the_built_in_project_cannot_be_removed_but_others_can() {
        let mut st = store();
        st.projects = vec![project("p1", "GenieX", "C:/src/GenieX")];
        st.ensure_tasks_project();
        let tasks = st.projects[0].id.clone();

        assert!(!st.project_remove(&tasks), "the built-in project must survive delete");
        assert!(st.projects.iter().any(|p| p.id == tasks));
        assert!(st.project_remove("p1"), "ordinary projects are still removable");
        assert_eq!(st.projects.len(), 1);
    }
}
