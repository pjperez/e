//! Skills: prompt packages the model loads on demand.
//!
//! A skill is a folder containing `SKILL.md` — YAML-ish frontmatter (`name`,
//! `description`) plus a body of instructions, per the Agent Skills
//! convention. They are found in `~/.e/skills` and `~/.agents/skills`
//! (global, shared with other agents) and `<workspace>/.e/skills` (project).
//!
//! Nothing is injected until the model asks: the `skills` tool advertises what
//! exists and returns the body of the one it names.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SkillMeta {
    /// Folder name — the id the `skills` tool is called with.
    pub name: String,
    /// The frontmatter's `name`, for display. Falls back to the folder name.
    pub display: String,
    pub description: String,
    pub path: String,
    /// "global" (home) or "project" (the chat's workspace).
    pub scope: String,
}

/// Scans the standard skill directories for `<name>/SKILL.md` packages.
pub struct SkillStore {
    dirs: Vec<(String, PathBuf)>,
}

fn frontmatter(body: &str, key: &str) -> Option<String> {
    let b = body.strip_prefix("---")?;
    let end = b.find("\n---").or_else(|| b.find("\r\n---"))?;
    let fm = &b[..end];
    for line in fm.lines() {
        let line = line.trim();
        if let Some(v) = line.strip_prefix(&format!("{key}:")) {
            return Some(v.trim().trim_matches(['"', '\'']).to_string());
        }
    }
    None
}

impl SkillStore {
    /// Global skills only. Used where no chat (and so no project) is in play.
    pub fn new() -> Self {
        Self::for_workspace("")
    }

    /// Global skills plus the ones belonging to this project. A relative or
    /// empty workspace contributes nothing: resolving it would depend on where
    /// the app was launched from.
    pub fn for_workspace(workspace: &str) -> Self {
        let mut dirs = Vec::new();
        if let Some(home) = dirs::home_dir() {
            dirs.push(("global".to_string(), home.join(".e/skills")));
            dirs.push(("global".to_string(), home.join(".agents/skills")));
        }
        let ws = workspace.trim();
        if !ws.is_empty() {
            let p = Path::new(ws);
            if p.is_absolute() {
                dirs.push(("project".to_string(), p.join(".e/skills")));
            }
        }
        SkillStore { dirs }
    }

    pub fn list(&self) -> Vec<SkillMeta> {
        let mut out: Vec<SkillMeta> = Vec::new();
        let mut seen = std::collections::HashSet::new();
        // Reversed so a project skill shadows a global one of the same name.
        for (scope, d) in self.dirs.iter().rev() {
            let Ok(rd) = std::fs::read_dir(d) else { continue };
            for e in rd.flatten() {
                if !e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                    continue;
                }
                let name = e.file_name().to_string_lossy().to_string();
                let sf = e.path().join("SKILL.md");
                if sf.exists() && seen.insert(name.clone()) {
                    if let Some(meta) = read_skill_meta(&name, &sf, scope) {
                        out.push(meta);
                    }
                }
            }
        }
        out.sort_by(|a, b| a.name.cmp(&b.name));
        out
    }

    pub fn get(&self, name: &str) -> Option<String> {
        for (_, d) in self.dirs.iter().rev() {
            let sf = d.join(name).join("SKILL.md");
            if sf.exists() {
                if let Ok(t) = std::fs::read_to_string(&sf) {
                    return Some(t);
                }
            }
        }
        // Frontmatter names are what a user sees, so accept one here too
        // rather than answering "not found" for a skill listed by that name.
        let by_display = self.list().into_iter().find(|s| s.display == name)?;
        std::fs::read_to_string(&by_display.path).ok()
    }
}

fn read_skill_meta(name: &str, path: &Path, scope: &str) -> Option<SkillMeta> {
    let text = std::fs::read_to_string(path).ok()?;
    let display = frontmatter(&text, "name").filter(|s| !s.is_empty()).unwrap_or_else(|| name.to_string());
    let description = frontmatter(&text, "description").unwrap_or_default();
    Some(SkillMeta {
        name: name.to_string(),
        display,
        description,
        path: path.to_string_lossy().to_string(),
        scope: scope.to_string(),
    })
}

impl Default for SkillStore {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("e-skill-test-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    fn skill(ws: &Path, name: &str, body: &str) {
        let d = ws.join(".e/skills").join(name);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("SKILL.md"), body).unwrap();
    }

    #[test]
    fn a_project_skill_is_found_with_its_frontmatter() {
        let ws = temp("project");
        skill(&ws, "release", "---\nname: Release checklist\ndescription: How we ship\n---\n\nStep one.\n");

        let store = SkillStore::for_workspace(ws.to_str().unwrap());
        let found = store.list().into_iter().find(|s| s.name == "release").expect("listed");
        assert_eq!(found.display, "Release checklist");
        assert_eq!(found.description, "How we ship");
        assert_eq!(found.scope, "project");
        assert!(store.get("release").unwrap().contains("Step one."));
        // The name a user reads in the list resolves too.
        assert!(store.get("Release checklist").is_some());
        let _ = std::fs::remove_dir_all(&ws);
    }

    #[test]
    fn a_relative_workspace_contributes_no_skills() {
        let store = SkillStore::for_workspace("some/relative/dir");
        assert!(store.dirs.iter().all(|(scope, _)| scope == "global"));
    }
}
