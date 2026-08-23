use crate::engine::agent::ProjectContext;
use crate::engine::Msg;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// The always-present project that holds one-off work belonging to no
/// repository. It is a real folder (see `scratch_workspace`) rather than "no
/// folder", because an unset workspace used to fall through to the global
/// setting — which is how a chat here ended up believing it was working on
/// whatever project was configured last.
pub const DEFAULT_PROJECT: &str = "Tasks";

/// Names earlier versions gave that same bucket. Migrated on load.
const LEGACY_DEFAULT_NAMES: [&str; 2] = ["Default", "Chats"];

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

/// Folder backing the default "Tasks" project: a real directory that exists but
/// is deliberately not a codebase. Chats with no project of their own run here,
/// so they get a valid cwd without borrowing some unrelated project's folder.
pub fn scratch_workspace() -> String {
    let dir = dirs::home_dir().unwrap_or_default().join(".e").join("tasks");
    let _ = std::fs::create_dir_all(&dir);
    dir.to_string_lossy().to_string()
}

pub fn is_scratch(ws: &str) -> bool {
    let s = scratch_workspace();
    !ws.trim().is_empty() && std::path::Path::new(ws.trim()) == std::path::Path::new(&s)
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
            st.project_create(DEFAULT_PROJECT, "");
        }
        st.migrate();
        if st.sessions.is_empty() {
            st.create("Chat 1", "", "", "");
        }
        st
    }

    /// Bring an index written by an older build up to the current invariants.
    ///
    /// Every project must own an absolute, existing-intent folder and every chat
    /// must know which folder it runs in. Anything left blank used to be filled
    /// in at run time from the *global* setting, which silently pointed chats at
    /// an unrelated project.
    fn migrate(&mut self) {
        let scratch = scratch_workspace();
        let default_id = self.projects.first().map(|p| p.id.clone()).unwrap_or_default();
        let mut changed = false;

        for p in self.projects.iter_mut() {
            let folderless = p.workspace.trim().is_empty();
            if folderless {
                // The auto-created bucket: give it the scratch folder and the
                // name it goes by now, so it reads as "not a project" instead of
                // inheriting whatever the global workspace happened to be.
                p.workspace = scratch.clone();
                if p.id == default_id || LEGACY_DEFAULT_NAMES.contains(&p.name.trim()) {
                    p.name = DEFAULT_PROJECT.to_string();
                }
                changed = true;
            } else {
                // Only pin a relative path when it actually resolves to a real
                // folder from here. Otherwise migration would bake the app's
                // launch directory into a path that was already broken, turning
                // "wrong sometimes" into "wrong forever" — leave it, and let the
                // sidebar's missing-folder warning ask the user to repoint it.
                let abs = absolute_workspace(&p.workspace);
                if abs != p.workspace && std::path::Path::new(&abs).is_dir() {
                    p.workspace = abs;
                    changed = true;
                }
            }
        }

        for s in self.sessions.iter_mut() {
            // Chats created before projects existed have no project, so they
            // would never show in the sidebar; adopt them into the default one.
            if s.project.trim().is_empty() && !default_id.is_empty() {
                s.project = default_id.clone();
                changed = true;
            }
            let ws = self
                .projects
                .iter()
                .find(|p| p.id == s.project)
                .map(|p| p.workspace.clone())
                .unwrap_or_default();
            if s.workspace.trim().is_empty() && !ws.is_empty() {
                s.workspace = ws;
                changed = true;
            }
        }

        if changed {
            self.save_index();
        }
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

    /// The folder a chat really runs in: its own, else its project's, else the
    /// scratch area. Deliberately never falls back to the global config
    /// workspace — that fallback is what made a chat in one project run tools
    /// against a different project's checkout.
    pub fn resolved_workspace(&self, id: &str) -> String {
        let sess = self.sessions.iter().find(|s| s.id == id);
        let own = sess.map(|s| s.workspace.trim().to_string()).unwrap_or_default();
        if !own.is_empty() {
            return own;
        }
        let pid = sess
            .map(|s| s.project.clone())
            .filter(|p| !p.trim().is_empty())
            .unwrap_or_else(|| self.current_project.clone());
        let pw = self.project_workspace(&pid);
        if pw.trim().is_empty() {
            scratch_workspace()
        } else {
            pw
        }
    }

    /// What the agent is told about where it is working, so a chat opened under
    /// a project is never left to guess (or inherit) the wrong one.
    pub fn project_context(&self, id: &str) -> ProjectContext {
        let sess = self.sessions.iter().find(|s| s.id == id);
        let pid = sess
            .map(|s| s.project.clone())
            .filter(|p| !p.trim().is_empty())
            .unwrap_or_else(|| self.current_project.clone());
        let proj = self.projects.iter().find(|p| p.id == pid);
        let name = proj.map(|p| p.name.clone()).unwrap_or_default();
        let proj_ws = proj.map(|p| p.workspace.clone()).unwrap_or_default();
        let workspace = self.resolved_workspace(id);
        // Chats survive the deletion of their project: they are refiled under
        // whichever one remains but keep their own folder. Reporting the name of
        // that fallback project would then be a plain lie, so flag the mismatch
        // and let the prompt describe the folder instead of the label.
        let detached = !proj_ws.trim().is_empty()
            && std::path::Path::new(&workspace) != std::path::Path::new(proj_ws.trim());
        ProjectContext {
            scratch: is_scratch(&workspace),
            detached,
            name,
        }
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
    /// Projects for the UI, tagged with whether they are the scratch area. The
    /// flag is derived rather than stored so it can never drift out of sync
    /// with the folder actually on the project.
    pub fn project_list_json(&self) -> Vec<serde_json::Value> {
        self.projects
            .iter()
            .map(|p| {
                serde_json::json!({
                    "id": p.id,
                    "name": p.name,
                    "workspace": p.workspace,
                    "created": p.created,
                    "scratch": is_scratch(&p.workspace),
                })
            })
            .collect()
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
        // No folder means "not a project": use the scratch area rather than the
        // launch directory, which changes depending on how the app was started.
        let ws = if workspace.trim().is_empty() {
            scratch_workspace()
        } else {
            absolute_workspace(workspace)
        };
        let nm = if !name.trim().is_empty() {
            name.trim().to_string()
        } else if is_scratch(&ws) {
            DEFAULT_PROJECT.to_string()
        } else {
            basename_of(&ws)
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
        let ws = if workspace.trim().is_empty() {
            scratch_workspace()
        } else {
            absolute_workspace(workspace)
        };
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
    /// (so nothing is lost) and keep their own folder, which is why they end up
    /// reported as detached rather than as members of that fallback project.
    /// At least one project always remains. Nothing on disk is touched: the
    /// project's folder is the user's, not ours.
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
        // The open chat needs no repair here: chats outlive their project, so
        // whichever one was selected is still there. (This used to compare the
        // selected *chat* id against a *project* id, which never matched.)
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
    /// Create a chat in the current project. It copies the project's folder up
    /// front so the chat is pinned to it: an unresolved workspace would be
    /// filled in later from global config, i.e. from the wrong project.
    pub fn create(&mut self, name: &str, workspace: &str, model: &str, provider: &str) -> SessionMeta {
        let id = format!("s{}", now_ms());
        let ws = if workspace.trim().is_empty() {
            let p = self.current_project_workspace();
            if p.trim().is_empty() { scratch_workspace() } else { p }
        } else {
            absolute_workspace(workspace)
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

    /// Chats for the UI, tagged with whether they are detached. Derived from the
    /// same `project_context` the prompt uses, so the sidebar and the agent can
    /// never disagree about which project a chat really belongs to.
    pub fn session_list_json(&self) -> Vec<serde_json::Value> {
        self.sessions
            .iter()
            .map(|s| {
                serde_json::json!({
                    "id": s.id,
                    "name": s.name,
                    "created": s.created,
                    "workspace": s.workspace,
                    "project": s.project,
                    "model": s.model,
                    "state": s.state,
                    "detached": self.project_context(&s.id).detached,
                })
            })
            .collect()
    }

#[allow(dead_code)]
    pub fn sessions_in(&self, project: &str) -> Vec<SessionMeta> {        self.sessions
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static SEQ: AtomicU64 = AtomicU64::new(0);

    /// A store backed by a throwaway directory, so `migrate` can persist without
    /// touching the developer's real `~/.e` index.
    fn store(projects: Vec<ProjectMeta>, sessions: Vec<SessionMeta>) -> SessionStore {
        let n = SEQ.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!("e-sessions-test-{}-{n}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        SessionStore {
            index_file: dir.join("index.json"),
            dir,
            sessions,
            projects,
            current: String::new(),
            current_project: String::new(),
        }
    }

    fn project(id: &str, name: &str, workspace: &str) -> ProjectMeta {
        ProjectMeta { id: id.into(), name: name.into(), workspace: workspace.into(), created: 0 }
    }

    fn session(id: &str, project: &str, workspace: &str) -> SessionMeta {
        SessionMeta {
            id: id.into(),
            name: id.into(),
            created: 0,
            workspace: workspace.into(),
            project: project.into(),
            model: String::new(),
            provider: String::new(),
            state: String::new(),
        }
    }

    #[test]
    fn folderless_default_becomes_tasks_with_a_scratch_folder() {
        let mut st = store(vec![project("p1", "Default", "")], vec![]);
        st.migrate();
        assert_eq!(st.projects[0].name, DEFAULT_PROJECT);
        assert_eq!(st.projects[0].workspace, scratch_workspace());
    }

    #[test]
    fn a_real_project_named_default_is_left_alone() {
        let mut st = store(
            vec![project("p1", "Tasks", ""), project("p2", "Default", "C:/src/thing")],
            vec![],
        );
        st.migrate();
        assert_eq!(st.projects[1].name, "Default");
        assert_eq!(st.projects[1].workspace, "C:/src/thing");
    }

    /// The reported bug: a chat with no folder of its own used to fall through
    /// to the global config workspace, i.e. some unrelated project's checkout.
    #[test]
    fn a_chat_without_a_folder_resolves_to_its_own_project() {
        let mut st = store(
            vec![project("p1", "Tasks", ""), project("p2", "mascot", "C:/src/mascot")],
            vec![session("s1", "p1", ""), session("s2", "p2", "")],
        );
        st.migrate();
        assert_eq!(st.resolved_workspace("s1"), scratch_workspace());
        assert_eq!(st.resolved_workspace("s2"), "C:/src/mascot");
    }

    #[test]
    fn project_context_distinguishes_scratch_from_a_real_project() {
        let mut st = store(
            vec![project("p1", "Default", ""), project("p2", "mascot", "C:/src/mascot")],
            vec![session("s1", "p1", ""), session("s2", "p2", "")],
        );
        st.migrate();

        let tasks = st.project_context("s1");
        assert_eq!(tasks.name, DEFAULT_PROJECT);
        assert!(tasks.scratch);

        let mascot = st.project_context("s2");
        assert_eq!(mascot.name, "mascot");
        assert!(!mascot.scratch);
    }

    /// A chat predating projects has neither a project nor a folder; it must end
    /// up in the default bucket rather than disappearing from the sidebar.
    #[test]
    fn orphan_chats_are_adopted_by_the_default_project() {
        let mut st = store(vec![project("p1", "Chats", "")], vec![session("s1", "", "")]);
        st.migrate();
        assert_eq!(st.sessions[0].project, "p1");
        assert_eq!(st.resolved_workspace("s1"), scratch_workspace());
    }

    /// Relative folders resolve against the launch directory, so the same index
    /// pointed somewhere different depending on how the app was started. Pin
    /// them — but only when they really resolve, so a path that is already
    /// broken isn't frozen against an arbitrary directory.
    #[test]
    fn resolvable_relative_project_folders_are_pinned_to_absolute_paths() {
        // `cargo test` runs with the package root as cwd, so `src` exists.
        let mut st = store(vec![project("p1", "Tasks", ""), project("p2", "rel", "src")], vec![]);
        st.migrate();
        assert!(std::path::Path::new(&st.projects[1].workspace).is_absolute());
        assert!(std::path::Path::new(&st.projects[1].workspace).is_dir());
    }

    #[test]
    fn a_broken_relative_folder_is_left_for_the_user_to_repoint() {
        let mut st = store(
            vec![project("p1", "Tasks", ""), project("p2", "0xAlpha", "0xAlpha")],
            vec![],
        );
        st.migrate();
        assert_eq!(st.projects[1].workspace, "0xAlpha");
    }

    /// A chat outlives its project, keeping its own folder. Reporting it as a
    /// member of whichever project it was refiled under would be a lie.
    #[test]
    fn a_chat_whose_project_was_deleted_is_reported_as_detached() {
        let mut st = store(
            vec![project("p1", "Tasks", ""), project("p2", "mascot", "C:/src/mascot")],
            vec![session("s1", "p2", "")],
        );
        st.migrate();
        assert!(!st.project_context("s1").detached);

        assert!(st.project_remove("p2"));

        let ctx = st.project_context("s1");
        assert_eq!(st.resolved_workspace("s1"), "C:/src/mascot", "folder must be kept");
        assert!(ctx.detached, "refiled under {:?} but still in mascot", ctx.name);
        assert!(!ctx.scratch);
    }

    #[test]
    fn a_chat_sitting_in_its_own_projects_folder_is_not_detached() {
        let mut st = store(
            vec![project("p1", "Tasks", ""), project("p2", "mascot", "C:/src/mascot")],
            vec![session("s1", "p2", ""), session("s2", "p1", "")],
        );
        st.migrate();
        assert!(!st.project_context("s1").detached);
        assert!(!st.project_context("s2").detached);
    }

    /// Deleting a project must not disturb the open chat; chats survive it.
    #[test]
    fn deleting_a_project_keeps_the_selected_chat() {
        let mut st = store(
            vec![project("p1", "Tasks", ""), project("p2", "mascot", "C:/src/mascot")],
            vec![session("s1", "p2", ""), session("s2", "p1", "")],
        );
        st.migrate();
        st.current = "s1".into();
        st.project_remove("p2");
        assert_eq!(st.current, "s1");
        assert_eq!(st.sessions.len(), 2, "chats are kept, not deleted");
    }

    #[test]
    fn a_new_chat_inherits_the_current_projects_folder() {
        let mut st = store(vec![project("p2", "mascot", "C:/src/mascot")], vec![]);
        st.current_project = "p2".into();
        let meta = st.create("", "", "", "");
        assert_eq!(meta.project, "p2");
        assert_eq!(st.resolved_workspace(&meta.id), "C:/src/mascot");
    }
}
