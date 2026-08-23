use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SkillMeta {
    pub name: String,
    pub description: String,
    pub path: String,
}

/// Scans standard skill directories for <name>/SKILL.md packages
/// (Global: ~/.e/skills, ~/.agents/skills · Project: .e/skills).
pub struct SkillStore {
    dirs: Vec<PathBuf>,
}

fn frontmatter(body: &str, key: &str) -> Option<String> {
    let b = body.strip_prefix("---")?;
    let end = b.find("\n---").or_else(|| b.find("\r\n---"))?;
    let fm = &b[..end];
    for line in fm.lines() {
        let line = line.trim();
        if let Some(v) = line.strip_prefix(&format!("{key}:")) {
            return Some(v.trim().to_string());
        }
    }
    None
}

impl SkillStore {
    pub fn new() -> Self {
        let home = dirs::home_dir().unwrap_or_default();
        let mut dirs = vec![home.join(".e/skills"), home.join(".agents/skills")];
        if let Ok(cwd) = std::env::current_dir() {
            dirs.push(cwd.join(".e/skills"));
        }
        SkillStore { dirs }
    }

    pub fn list(&self) -> Vec<SkillMeta> {
        let mut out: Vec<SkillMeta> = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for d in &self.dirs {
            if let Ok(rd) = std::fs::read_dir(d) {
                for e in rd.flatten() {
                    if e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                        let name = e.file_name().to_string_lossy().to_string();
                        let sf = e.path().join("SKILL.md");
                        if sf.exists() && seen.insert(name.clone()) {
                            if let Some(meta) = read_skill_meta(&name, &sf) {
                                out.push(meta);
                            }
                        }
                    }
                }
            }
        }
        out.sort_by(|a, b| a.name.cmp(&b.name));
        out
    }

    pub fn get(&self, name: &str) -> Option<String> {
        for d in &self.dirs {
            let sf = d.join(name).join("SKILL.md");
            if sf.exists() {
                if let Ok(t) = std::fs::read_to_string(&sf) {
                    return Some(t);
                }
            }
        }
        None
    }
}

fn read_skill_meta(name: &str, path: &Path) -> Option<SkillMeta> {
    let text = std::fs::read_to_string(path).ok()?;
    let display = frontmatter(&text, "name").filter(|s| !s.is_empty()).unwrap_or_else(|| name.to_string());
    let description = frontmatter(&text, "description").unwrap_or_default();
    Some(SkillMeta {
        name: display,
        description,
        path: path.to_string_lossy().to_string(),
    })
}

impl Default for SkillStore {
    fn default() -> Self {
        Self::new()
    }
}
