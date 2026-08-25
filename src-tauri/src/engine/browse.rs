//! Read-only directory listing and text preview, fenced to one workspace.
//!
//! The file tools the *model* uses take a path and a `ToolContext`. A file
//! explorer needs something different: a cheap listing of one directory at a
//! time (a tree that stats an entire monorepo to draw one row is unusable), and
//! a bounded read for the preview pane.
//!
//! Every path is relative to a root the caller does not choose. [`resolve`]
//! canonicalises before comparing, so `..`, an absolute path and a symlink
//! pointing out of the tree are all the same rejection rather than three
//! separate holes.

use serde::Serialize;
use std::path::{Path, PathBuf};

/// Entries returned for one directory. A generated folder (`node_modules`,
/// `target`) can hold hundreds of thousands of files; past this the listing is
/// truncated and says so, rather than freezing the pane building DOM for all
/// of them.
const MAX_ENTRIES: usize = 5_000;

/// How much of a file the preview reads. Enough for any source file, small
/// enough that clicking a database dump does not swallow a gigabyte.
pub const MAX_PREVIEW: usize = 512 * 1024;

/// How far into a file we look for a NUL before calling it binary.
const SNIFF: usize = 8_000;

#[derive(Debug, Serialize)]
pub struct Entry {
    pub name: String,
    /// Path relative to the root, with `/` separators on every platform so the
    /// renderer can treat it as an opaque key.
    pub path: String,
    pub dir: bool,
    pub size: u64,
    /// Unix seconds, or 0 when the platform would not say.
    pub modified: u64,
    pub symlink: bool,
}

#[derive(Debug, Serialize)]
pub struct Listing {
    pub path: String,
    pub entries: Vec<Entry>,
    /// True when the directory had more than [`MAX_ENTRIES`] children.
    pub truncated: bool,
}

#[derive(Debug, Serialize)]
pub struct FileText {
    pub path: String,
    pub text: String,
    pub size: u64,
    /// The file is longer than what `text` holds.
    pub truncated: bool,
    /// It is not text at all; `text` is empty and the pane should say so
    /// instead of painting the screen with control characters.
    pub binary: bool,
}

/// Turn a caller-supplied relative path into a real one inside `root`.
///
/// Containment is checked *after* canonicalising both sides. Comparing the
/// joined path textually would pass for a symlink whose target is elsewhere,
/// which is exactly the case worth stopping: a project containing a link to
/// `~/.ssh` must not become a file browser for it.
fn resolve(root: &Path, rel: &str) -> Result<PathBuf, String> {
    if root.as_os_str().is_empty() {
        return Err("this chat has no project folder. Pick one for its project (sidebar → ✎).".into());
    }
    let root = root
        .canonicalize()
        .map_err(|e| format!("project folder is unreadable: {} ({e})", root.display()))?;

    let rel = rel.trim().replace('\\', "/");
    let rel = rel.trim_matches('/');
    if rel.is_empty() || rel == "." {
        return Ok(root);
    }
    if Path::new(rel).is_absolute() || rel.split('/').any(|c| c == "..") {
        return Err(format!("'{rel}' is outside the project folder"));
    }

    let full = root
        .join(rel)
        .canonicalize()
        .map_err(|e| format!("cannot open '{rel}': {e}"))?;
    if !full.starts_with(&root) {
        return Err(format!("'{rel}' resolves outside the project folder"));
    }
    Ok(full)
}

fn rel_of(root: &Path, full: &Path) -> String {
    full.strip_prefix(root)
        .map(|p| p.to_string_lossy().replace('\\', "/"))
        .unwrap_or_default()
}

fn mtime(md: &std::fs::Metadata) -> u64 {
    md.modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// One directory's children, folders first then files, each case-insensitively
/// by name — the order a person scans, not the order the filesystem happens to
/// hand back.
pub fn list(root: &Path, rel: &str) -> Result<Listing, String> {
    let canon_root = root
        .canonicalize()
        .map_err(|e| format!("project folder is unreadable: {} ({e})", root.display()))?;
    let dir = resolve(root, rel)?;
    if !dir.is_dir() {
        return Err(format!("'{}' is not a folder", rel_of(&canon_root, &dir)));
    }

    let rd = std::fs::read_dir(&dir).map_err(|e| format!("cannot list '{}': {e}", dir.display()))?;
    let mut entries: Vec<Entry> = Vec::new();
    let mut truncated = false;
    for item in rd.flatten() {
        if entries.len() >= MAX_ENTRIES {
            truncated = true;
            break;
        }
        let path = item.path();
        // `symlink_metadata` first: a link to a folder should be listed as what
        // it is, and a broken link must not drop the whole listing.
        let link_md = match item.path().symlink_metadata() {
            Ok(m) => m,
            Err(_) => continue,
        };
        let symlink = link_md.file_type().is_symlink();
        let md = if symlink { std::fs::metadata(&path).unwrap_or(link_md) } else { link_md };
        entries.push(Entry {
            name: item.file_name().to_string_lossy().to_string(),
            path: rel_of(&canon_root, &path),
            dir: md.is_dir(),
            size: if md.is_dir() { 0 } else { md.len() },
            modified: mtime(&md),
            symlink,
        });
    }
    entries.sort_by(|a, b| {
        b.dir
            .cmp(&a.dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
            .then_with(|| a.name.cmp(&b.name))
    });

    Ok(Listing { path: rel_of(&canon_root, &dir), entries, truncated })
}

/// A bounded text read for the preview pane.
///
/// A binary file comes back flagged rather than as replacement characters:
/// "12 MB, not text" is a useful answer, and half a megabyte of mojibake is
/// not.
pub fn read_text(root: &Path, rel: &str) -> Result<FileText, String> {
    let canon_root = root
        .canonicalize()
        .map_err(|e| format!("project folder is unreadable: {} ({e})", root.display()))?;
    let path = resolve(root, rel)?;
    let md = std::fs::metadata(&path).map_err(|e| format!("cannot open '{rel}': {e}"))?;
    if md.is_dir() {
        return Err(format!("'{rel}' is a folder"));
    }
    let size = md.len();
    let out = rel_of(&canon_root, &path);

    let bytes = read_capped(&path, MAX_PREVIEW)?;
    let sniff = bytes.len().min(SNIFF);
    if bytes[..sniff].contains(&0) {
        return Ok(FileText { path: out, text: String::new(), size, truncated: false, binary: true });
    }
    // The cap can land inside a multi-byte character; lossy conversion turns
    // only that tail into one replacement char rather than failing the read.
    Ok(FileText {
        path: out,
        text: String::from_utf8_lossy(&bytes).into_owned(),
        size,
        truncated: size > bytes.len() as u64,
        binary: false,
    })
}

fn read_capped(path: &Path, cap: usize) -> Result<Vec<u8>, String> {
    use std::io::Read;
    let f = std::fs::File::open(path).map_err(|e| format!("cannot read '{}': {e}", path.display()))?;
    let mut buf = Vec::new();
    f.take(cap as u64)
        .read_to_end(&mut buf)
        .map_err(|e| format!("cannot read '{}': {e}", path.display()))?;
    Ok(buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("e-browse-test-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn a_listing_puts_folders_first_and_paths_are_relative() {
        let root = temp("order");
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("a.txt"), "a").unwrap();
        std::fs::write(root.join("src/main.rs"), "fn main() {}").unwrap();

        let top = list(&root, "").unwrap();
        assert_eq!(top.entries[0].name, "src");
        assert!(top.entries[0].dir);
        assert_eq!(top.entries[1].name, "a.txt");

        let sub = list(&root, "src").unwrap();
        assert_eq!(sub.path, "src");
        assert_eq!(sub.entries[0].path, "src/main.rs");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The whole point of the module. A pane that can be talked into listing
    /// `..` is a file browser for the user's home directory.
    #[test]
    fn nothing_outside_the_root_can_be_reached() {
        let root = temp("escape");
        std::fs::create_dir_all(root.join("inside")).unwrap();
        std::fs::write(root.join("inside/ok.txt"), "fine").unwrap();

        assert!(list(&root, "..").is_err());
        assert!(list(&root, "inside/../..").is_err());
        assert!(read_text(&root, "../../etc/passwd").is_err());
        #[cfg(windows)]
        assert!(read_text(&root, "C:/Windows/System32/drivers/etc/hosts").is_err());
        #[cfg(not(windows))]
        assert!(read_text(&root, "/etc/passwd").is_err());
        // …while an ordinary path still works.
        assert_eq!(read_text(&root, "inside/ok.txt").unwrap().text, "fine");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Backslashes are what a Windows user pastes, and `\` is not a separator
    /// on Unix — normalising means one code path instead of two.
    #[test]
    fn a_windows_style_separator_resolves_the_same_way() {
        let root = temp("seps");
        std::fs::create_dir_all(root.join("a/b")).unwrap();
        std::fs::write(root.join("a/b/c.txt"), "c").unwrap();
        assert_eq!(read_text(&root, "a\\b\\c.txt").unwrap().text, "c");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_binary_file_is_flagged_rather_than_rendered() {
        let root = temp("binary");
        std::fs::write(root.join("blob.bin"), [0x00, 0x01, 0x02, 0xff]).unwrap();
        let f = read_text(&root, "blob.bin").unwrap();
        assert!(f.binary);
        assert!(f.text.is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_large_file_is_truncated_and_says_so() {
        let root = temp("large");
        let big = "x".repeat(MAX_PREVIEW + 1024);
        std::fs::write(root.join("big.txt"), &big).unwrap();
        let f = read_text(&root, "big.txt").unwrap();
        assert!(f.truncated);
        assert_eq!(f.text.len(), MAX_PREVIEW);
        assert_eq!(f.size, big.len() as u64);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_folder_is_not_a_preview() {
        let root = temp("isdir");
        std::fs::create_dir_all(root.join("d")).unwrap();
        assert!(read_text(&root, "d").is_err());
        let _ = std::fs::remove_dir_all(&root);
    }
}
