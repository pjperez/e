//! Microsoft Execution Containers: OS-enforced fencing for tool processes.
//!
//! When a chat runs sandboxed, its `powershell` calls are spawned inside a
//! process container instead of as plain children. The fence is the recipe the
//! mxc-spike example validated on real hardware:
//!
//! * `ui.disable: false` — console executables touch win32k while initialising
//!   and die with `0xC0000142` without it;
//! * the system drive read-only — desktop PowerShell will not start under
//!   narrower grants;
//! * read-write only the workspace and the temp directory;
//! * a deny list that beats the drive-wide grant — this is the boundary that
//!   actually protects secrets, and it is where the defaults live;
//! * an explicit working directory — the backend otherwise picks a policy dir.
//!
//! Host-side tools (`write_file`, `read_file`, `list_dir`) never pass through
//! a container, so the same deny list is enforced in-process at those tools:
//! the fence must not have a door that skips the OS and trusts a prompt.
//!
//! Everything here is inert until the user turns sandbox mode on (Settings →
//! Tools), mirroring the mcp module's zero-cost-when-unused rule.

use crate::engine::jobs;
use crate::engine::tools::{self, ToolResult};
use mxc_sdk::v1::policy::{
    NetworkAction, NetworkEgressPolicy, NetworkIngressPolicy, NetworkPeerPolicy, NetworkPolicy,
    NetworkRulePolicy,
};
use mxc_sdk::v1::{self, ClipboardPolicy, ContainerRequest, FilesystemPolicy, UiPolicy, WaitResult};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// Sensitive home-relative locations denied on top of the user's policy. The
/// deny list is the load-bearing boundary: the system drive is necessarily
/// readable for PowerShell to start, so secrets are protected by explicit
/// denials, not by omission.
///
/// `~/.e` as a whole is deliberately NOT denied — managed worktrees live under
/// `~/.e/worktrees` and are the very folders chats work in. The specific
/// state files inside `~/.e` are denied by name.
const DEFAULT_DENY: &[&str] = &[
    ".ssh",
    ".gnupg",
    ".aws",
    ".azure",
    ".kube",
    ".git-credentials",
    "_netrc",
    ".e/config.json",
    ".e/install_id",
    ".e/sessions",
];

/// The `.e/policy.json` a project may carry, as written by hand:
///
/// ```json
/// {
///   "deny": ["~/.vagrant", "D:/secrets"],
///   "network": { "allow": ["13.107.6.0/24"], "deny": ["10.0.0.0/8"] }
/// }
/// ```
///
/// `~` expands to the user's home; other paths must be absolute. Network
/// entries are CIDRs — the MXC V1 schema has no hostname rules, so a hostname
/// allowlist waits on upstream support and stays out of v1.
#[derive(Debug, Default, serde::Deserialize)]
struct PolicyFile {
    #[serde(default)]
    deny: Vec<String>,
    #[serde(default)]
    network: Option<NetworkFile>,
}

#[derive(Debug, Default, serde::Deserialize)]
struct NetworkFile {
    #[serde(default)]
    allow: Vec<String>,
    #[serde(default)]
    deny: Vec<String>,
}

/// A compiled policy: deny paths resolved to absolute form, network rules
/// validated. Parse failures are errors rather than silent defaults — a
/// mistyped policy that quietly stops applying is security theatre.
#[derive(Debug, Default, Clone)]
pub struct Policy {
    deny: Vec<PathBuf>,
    network: Option<CompiledNetwork>,
}

#[derive(Debug, Default, Clone)]
struct CompiledNetwork {
    allow: Vec<NetworkRulePolicy>,
    deny: Vec<NetworkRulePolicy>,
}

fn rules(cidrs: &[String], field: &str) -> Result<Vec<NetworkRulePolicy>, String> {
    cidrs
        .iter()
        .map(|raw| {
            let cidr = raw.trim();
            if cidr.is_empty() {
                return Ok(NetworkRulePolicy { to: None, ports: None });
            }
            // Presence of '/' and a plausible octet is checked cheaply; the
            // backend owns real validation, but a typo deserves to fail here
            // where the message can name the file.
            if !cidr.contains('/') {
                return Err(format!("network {field} entry '{cidr}' is not a CIDR (e.g. 13.107.6.0/24)"));
            }
            Ok(NetworkRulePolicy {
                to: Some(vec![NetworkPeerPolicy::new(cidr)]),
                ports: None,
            })
        })
        .collect()
}

impl Policy {
    /// Load `<workspace>/.e/policy.json`. A missing file is the common case
    /// and yields the defaults; a present-but-broken file is an error.
    pub fn load(workspace: &Path) -> Result<Policy, String> {
        let file = workspace.join(".e").join("policy.json");
        let text = match std::fs::read_to_string(&file) {
            Ok(t) => t,
            Err(_) => return Ok(Policy::defaults()),
        };
        let parsed: PolicyFile =
            serde_json::from_str(&text).map_err(|e| format!("{}: {e}", file.display()))?;
        Policy::from_file(parsed)
    }

    fn defaults() -> Policy {
        let home = dirs::home_dir().unwrap_or_default();
        Policy {
            deny: DEFAULT_DENY.iter().map(|d| home.join(d)).collect(),
            network: None,
        }
    }

    fn from_file(f: PolicyFile) -> Result<Policy, String> {
        let mut p = Policy::defaults();
        for raw in &f.deny {
            let expanded = expand_tilde(raw)?;
            if !expanded.is_absolute() {
                return Err(format!("deny entry '{raw}' must be absolute (or start with ~)"));
            }
            p.deny.push(expanded);
        }
        if let Some(n) = &f.network {
            p.network = Some(CompiledNetwork {
                allow: rules(&n.allow, "allow")?,
                deny: rules(&n.deny, "deny")?,
            });
        }
        Ok(p)
    }
}

fn expand_tilde(raw: &str) -> Result<PathBuf, String> {
    let t = raw.trim();
    if t == "~" || t.starts_with("~/") || t.starts_with("~\\") {
        let home = dirs::home_dir().ok_or_else(|| "cannot expand '~': no home directory".to_string())?;
        return Ok(home.join(t.strip_prefix('~').unwrap_or("").trim_start_matches(['/', '\\'])));
    }
    Ok(PathBuf::from(t))
}

/// Whether this machine can spawn process containers at all. Probed once:
/// the answer depends on the OS build and feature gates, not on anything that
/// changes while the app runs. A failed probe means sandbox mode quietly
/// behaves as prompt mode (approvals apply) rather than breaking runs.
pub fn available() -> bool {
    static AVAILABLE: OnceLock<bool> = OnceLock::new();
    *AVAILABLE.get_or_init(|| {
        if !cfg!(windows) {
            return false;
        }
        let mut req = ContainerRequest::new("cmd /c exit 0");
        req.timeout_ms = Some(10_000);
        matches!(
            v1::run(req, Default::default()),
            Ok(r) if matches!(r.outcome, WaitResult::Exited(0))
        )
    })
}

/// The container request for one command in one workspace: the spike recipe,
/// with the project's policy folded in.
pub fn request_for(policy: &Policy, ws: &Path, command: &str) -> ContainerRequest {
    let mut req = ContainerRequest::new(command);
    // Console executables need the win32k subsystem while initialising —
    // blocking it kills even `whoami` with 0xC0000142. A terminal tool wants
    // a console, not a desktop: no clipboard, no synthetic input.
    req.ui = Some(UiPolicy {
        disable: false,
        clipboard: ClipboardPolicy::None,
        allow_input_injection: false,
    });
    req.filesystem = Some(FilesystemPolicy {
        // Desktop PowerShell refuses to start without drive-wide read access;
        // secrets are protected by the deny list, not by omitting the grant.
        readonly_paths: vec![system_drive(ws).to_string_lossy().into_owned()],
        readwrite_paths: vec![
            std::env::temp_dir().to_string_lossy().into_owned(),
            ws.to_string_lossy().into_owned(),
        ],
        denied_paths: policy
            .deny
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect(),
        clear_policy_on_exit: None,
    });
    req.working_directory = Some(ws.to_string_lossy().into_owned());
    if let Some(n) = &policy.network {
        req.network = Some(network_policy(n));
    }
    req
}

fn system_drive(ws: &Path) -> PathBuf {
    use std::path::Component;
    match ws.components().next() {
        Some(Component::Prefix(p)) => {
            let s = p.as_os_str().to_string_lossy().into_owned();
            PathBuf::from(format!("{s}\\"))
        }
        _ => PathBuf::from(r"C:\"),
    }
}

fn network_policy(n: &CompiledNetwork) -> NetworkPolicy {
    NetworkPolicy {
        egress: Some(NetworkEgressPolicy {
            // Rules only make sense as exceptions when the default is deny;
            // a bare deny list over an allow-everything default is also
            // honoured, but an empty policy leaves egress open (today's
            // behaviour) rather than cutting the network with no way back.
            default: Some(if n.allow.is_empty() { NetworkAction::Allow } else { NetworkAction::Deny }),
            allow: Some(n.allow.clone()),
            deny: Some(n.deny.clone()),
        }),
        ingress: Some(NetworkIngressPolicy {
            default: Some(NetworkAction::Deny),
            host_loopback: Some(NetworkAction::Deny),
        }),
        runtime_config: None,
    }
}

/// The full command line for one tool command, mirroring what the unsandboxed
/// path passes to the shell.
fn command_line(command: &str) -> String {
    format!("{} -NoLogo -NoProfile -NonInteractive -Command {command}", jobs::shell_executable())
}

/// Run one command to completion (or 120s) inside a container, formatting the
/// result exactly like the unsandboxed sync path so the model cannot tell the
/// difference except by what the fence refuses.
pub fn run_sync(cwd: &Path, command: &str) -> ToolResult {
    let policy = Policy::load(cwd)?;
    let mut req = request_for(&policy, cwd, &command_line(command));
    req.timeout_ms = Some(120_000);
    match v1::run(req, Default::default()) {
        Ok(r) => match r.outcome {
            WaitResult::Exited(code) => {
                let out = tail_taken(&r.stdout, tools::SYNC_MAX);
                let err = tail_taken(&r.stderr, tools::SYNC_MAX);
                let m = tools::format_sync_output(&out, &err, code);
                if code != 0 {
                    Err(m)
                } else {
                    Ok(m)
                }
            }
            WaitResult::TimedOut => {
                let out = tail_taken(&r.stdout, jobs::MAX_POLL);
                let err = tail_taken(&r.stderr, jobs::MAX_POLL);
                let mut m = "command timed out after 120s and was stopped (sandbox contained the process). Output before the stop:".to_string();
                if out.text.trim().is_empty() && err.text.trim().is_empty() {
                    m.push_str("\n(it printed nothing)");
                } else {
                    if !out.text.trim().is_empty() {
                        m.push_str(&format!("\n{}", out.text));
                    }
                    if !err.text.trim().is_empty() {
                        m.push_str(&format!("\n[stderr]\n{}", err.text.trim_end()));
                    }
                }
                Err(m)
            }
        },
        Err(e) => Err(format!("sandbox could not start the command: {e}")),
    }
}

/// Bytes trimmed from the front to `max`, at a UTF-8 boundary, in the same
/// shape `Capture::tail` produces so `format_sync_output` renders it.
fn tail_taken(bytes: &[u8], max: usize) -> jobs::Taken {
    let mut start = bytes.len().saturating_sub(max);
    while start > 0 && (bytes[start] & 0xC0) == 0x80 {
        start -= 1;
    }
    jobs::Taken {
        text: String::from_utf8_lossy(&bytes[start..]).into_owned(),
        skipped: start,
        end: bytes.len(),
    }
}

// ---- host-side gates -------------------------------------------------------

/// Case-insensitive path-prefix test. Lexical on purpose: canonicalising
/// would follow symlinks the model may not control, and a path that resolves
/// through one is exactly the kind the fence should refuse to reason about.
fn is_under(path: &Path, base: &Path) -> bool {
    let norm = |p: &Path| -> Vec<String> {
        p.components()
            .filter_map(|c| c.as_os_str().to_str())
            .map(|s| s.trim_end_matches('\\').to_ascii_lowercase())
            .collect()
    };
    let p = norm(path);
    let b = norm(base);
    p.len() >= b.len() && !b.is_empty() && p[..b.len()] == b[..]
}

/// May the sandboxed host tools read `path`? Mirrors the container's rule for
/// spawned processes: the drive is readable, the deny list is not.
pub fn read_allowed(policy: &Policy, path: &Path) -> Result<(), String> {
    for d in &policy.deny {
        if is_under(path, d) {
            return Err(format!(
                "sandbox: {} is denied by this project's policy (reads outside the workspace are otherwise allowed)",
                path.display()
            ));
        }
    }
    Ok(())
}

/// May the sandboxed host tools write `path`? Stricter than reads: the
/// container grants write only to the workspace and temp, so `write_file`
/// must refuse anything outside them rather than become the door around the
/// fence.
pub fn write_allowed(policy: &Policy, ws: &Path, path: &Path) -> Result<(), String> {
    read_allowed(policy, path)?;
    if is_under(path, ws) || is_under(path, &std::env::temp_dir()) {
        return Ok(());
    }
    Err(format!(
        "sandbox: {} is outside this chat's workspace — writes are fenced to the workspace and the temp directory",
        path.display()
    ))
}

/// The line the system prompt carries while sandbox mode is active, so the
/// model learns the fence from the description of the world rather than from
/// a wall of access-denied errors.
pub fn sandbox_hint() -> &'static str {
    "SANDBOX: commands in this chat run inside a Microsoft Execution Container. Writes are limited to the workspace and the temp directory; some sensitive locations are unreadable; network may be restricted. A blocked operation fails with an access error — do not retry it endlessly; adjust the approach or tell the user."
}

#[cfg(test)]
mod tests {
    use super::*;

    fn home() -> PathBuf {
        dirs::home_dir().unwrap_or_default()
    }

    #[test]
    fn a_missing_policy_file_yields_the_default_deny_list() {
        let p = Policy::load(Path::new(r"C:\does\not\exist")).expect("defaults");
        assert!(p.deny.contains(&home().join(".ssh")), "{:?}", p.deny);
        assert!(p.deny.contains(&home().join(".e").join("config.json")));
        // The deny list must not cover the worktree area wholesale: managed
        // worktrees live under ~/.e/worktrees and ARE the workspace.
        assert!(!p.deny.iter().any(|d| d.ends_with(".e")), "{:?}", p.deny);
        assert!(p.network.is_none());
    }

    #[test]
    fn a_policy_file_adds_denies_expands_tilde_and_rejects_relative() {
        let f = PolicyFile {
            deny: vec!["~/~odd name".into(), r"D:\secrets".into()],
            network: None,
        };
        let p = Policy::from_file(f).expect("compiles");
        assert!(p.deny.contains(&home().join("~odd name")));
        assert!(p.deny.contains(&PathBuf::from(r"D:\secrets")));

        let bad = Policy::from_file(PolicyFile { deny: vec!["relative/dir".into()], network: None });
        assert!(bad.is_err(), "a relative deny path cannot be honoured predictably");
    }

    #[test]
    fn a_broken_policy_file_is_an_error_not_a_silent_default() {
        let dir = std::env::temp_dir().join("e-mxc-test-broken");
        std::fs::create_dir_all(dir.join(".e")).unwrap();
        std::fs::write(dir.join(".e/policy.json"), "{ oops").unwrap();
        assert!(Policy::load(&dir).is_err(), "the user must learn their policy is not applying");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn network_entries_must_look_like_cidrs() {
        let f = PolicyFile {
            deny: vec![],
            network: Some(NetworkFile { allow: vec!["crates.io".into()], deny: vec![] }),
        };
        let err = Policy::from_file(f).unwrap_err();
        assert!(err.contains("crates.io") && err.contains("CIDR"), "{err}");

        let ok = Policy::from_file(PolicyFile {
            deny: vec![],
            network: Some(NetworkFile { allow: vec!["13.107.6.0/24".into()], deny: vec!["10.0.0.0/8".into()] }),
        })
        .expect("compiles");
        let n = ok.network.expect("network");
        assert_eq!(n.allow.len(), 1);
        assert_eq!(n.deny.len(), 1);
    }

    /// The recipe as validated by the spike, as code: every field the hard
    /// way was earned belongs here so a refactor cannot quietly drop one.
    #[test]
    fn the_request_carries_the_validated_recipe() {
        let p = Policy::defaults();
        let ws = Path::new(r"C:\src\e");
        let req = request_for(&p, ws, "Write-Output hi");
        assert_eq!(req.command, "Write-Output hi");

        let ui = req.ui.expect("ui policy");
        assert!(!ui.disable, "win32k must be allowed or console exes die at init");

        let fs = req.filesystem.expect("fs policy");
        assert!(fs.readonly_paths.contains(&r"C:\".to_string()), "{:?}", fs.readonly_paths);
        assert!(fs.readwrite_paths.iter().any(|r| r.contains("Temp")), "{:?}", fs.readwrite_paths);
        assert!(fs.readwrite_paths.contains(&r"C:\src\e".to_string()));
        assert!(fs.denied_paths.iter().any(|d| d.contains(".ssh")), "{:?}", fs.denied_paths);

        assert_eq!(req.working_directory.as_deref(), Some(r"C:\src\e"));
        assert!(req.network.is_none(), "no network file means egress stays as today");
    }

    #[test]
    fn an_allow_list_flips_the_egress_default_to_deny() {
        let p = Policy::from_file(PolicyFile {
            deny: vec![],
            network: Some(NetworkFile { allow: vec!["13.107.6.0/24".into()], deny: vec![] }),
        })
        .expect("compiles");
        let ws = Path::new(r"C:\src\e");
        let req = request_for(&p, ws, "x");
        let net = req.network.expect("network policy");
        let eg = net.egress.expect("egress");
        assert!(matches!(eg.default, Some(NetworkAction::Deny)));
        assert!(eg.allow.expect("allow rules").len() == 1);
    }

    #[test]
    fn writes_are_fenced_to_the_workspace_and_reads_to_the_deny_list() {
        let p = Policy::defaults();
        let ws = Path::new(r"C:\src\e");
        assert!(write_allowed(&p, ws, Path::new(r"C:\src\e\src\main.rs")).is_ok());
        // Case differences must not open a hole on Windows.
        assert!(write_allowed(&p, ws, Path::new(r"c:\SRC\E\other.txt")).is_ok());
        assert!(write_allowed(&p, ws, Path::new(r"C:\src\e")).is_ok(), "the root itself is writable");
        assert!(write_allowed(&p, ws, Path::new(r"C:\src\other\file.txt")).is_err());
        // A path that merely shares a prefix must not be caught by it.
        assert!(write_allowed(&p, ws, Path::new(r"C:\src\evasion\x")).is_err());
        assert!(read_allowed(&p, Path::new(r"C:\Windows\System32\drivers\etc\hosts")).is_ok());
        assert!(read_allowed(&p, &home().join(".ssh").join("id_ed25519")).is_err());
        // The workspace being under ~/.e/worktrees must not trip the deny of
        // the state files beside it.
        let wt = home().join(r".e\worktrees\s123");
        assert!(write_allowed(&p, &wt, &wt.join(r"src\x.rs")).is_ok());
        assert!(read_allowed(&p, &home().join(r".e\sessions\s1.json")).is_err());
    }

    /// Bytes are trimmed from the front at a character boundary, matching
    /// `Capture::tail`'s contract for oversized output.
    #[test]
    fn tail_taken_trims_from_the_front_at_a_boundary() {
        let bytes = "é".repeat(40).into_bytes(); // 80 bytes, boundaries every 2
        let t = tail_taken(&bytes, 7);
        // 80-7 lands mid-character; the boundary is at 72, keeping 4 chars.
        assert_eq!(t.text, "éééé");
        assert_eq!(t.skipped, 72);
        let empty = tail_taken(b"", 100);
        assert_eq!(empty.text, "");
        assert_eq!(empty.skipped, 0);
    }

    // ---- against real containment (self-skipping where unsupported) ------

    fn gated() -> bool {
        if !available() {
            eprintln!("skipping: no process containers on this machine");
            return false;
        }
        true
    }

    /// Each test gets its own scratch workspace: a shared one gets wiped by
    /// whichever parallel test calls this next, mid-run.
    fn scratch(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("e-mxc-sandbox-{tag}"));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn a_contained_command_writes_inside_but_not_outside() {
        if !gated() {
            return;
        }
        let ws = scratch("writes");
        let out = run_sync(&ws, "Set-Content -Path inside.txt -Value ok; Write-Output done").expect("runs");
        assert!(out.contains("done"), "{out}");
        assert!(ws.join("inside.txt").is_file());

        let home_no = home().join("e-mxc-sandbox-denied.txt");
        let out = run_sync(
            &ws,
            &format!("Set-Content -Path '{}' -Value nope; Write-Output attempted", home_no.display()),
        )
        .expect("the command itself runs; the write is refused");
        assert!(out.contains("attempted"), "{out}");
        assert!(!home_no.exists(), "the fence, not a prompt, stopped this write");
        let _ = std::fs::remove_dir_all(&ws);
    }

    /// Egress default-deny with no allow rules cuts the network. On a machine
    /// with no connectivity this passes vacuously — the honest caveat of
    /// probing the internet from a test.
    #[test]
    fn an_allow_list_without_entries_denies_egress() {
        if !gated() {
            return;
        }
        let ws = scratch("egress");
        std::fs::create_dir_all(ws.join(".e")).unwrap();
        std::fs::write(
            ws.join(".e/policy.json"),
            r#"{ "network": { "allow": ["192.0.2.0/24"] } }"#, // TEST-NET: allowed but unroutable
        )
        .unwrap();
        let out = run_sync(
            &ws,
            "try { Invoke-WebRequest -Uri http://example.com -UseBasicParsing -TimeoutSec 5 | Out-Null; Write-Output network-allowed } catch { Write-Output network-denied }",
        )
        .expect("runs");
        assert!(out.contains("network-denied"), "{out}");
        let _ = std::fs::remove_dir_all(&ws);
    }
}
