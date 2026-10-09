//! Codemode: the model writes JavaScript, the harness runs it.
//!
//! The pattern is Pi's (pi.dev): instead of one LLM round-trip per tool call,
//! the model emits a small program that orchestrates the calls itself — loops,
//! retries, filtering, `Promise.all` — and only the distilled result returns
//! to the conversation.
//!
//! The script never runs in Rust. The webview is the app's only JavaScript
//! runtime (the same rule plugins follow), so the tool ships the source across
//! as an event and blocks until the frontend answers. The script's own tool
//! calls come back the other way, through `codemode_tool_call`, and pass the
//! same gates a model-made call would: plugin veto, approval for risky tools,
//! then execution. A sandboxed script therefore cannot do anything the model
//! could not have done one call at a time — it can only do it with fewer
//! round-trips.

use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, LazyLock, Mutex, OnceLock};
use tauri::Emitter;

use crate::engine::tools::{Tool, ToolContext, ToolResult};

/// How long one script may run, engine side. Generous on purpose: each tool
/// call a script makes can legitimately wait minutes on a human approval, so
/// a wall clock measured in minutes is the honest bound, not seconds.
pub const RUN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(600);

static HOST: OnceLock<tauri::AppHandle> = OnceLock::new();
static PENDING: LazyLock<Mutex<HashMap<String, mpsc::Sender<(bool, String)>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
/// Run ids currently executing. `codemode_tool_call` is only honoured for one
/// of these: without the check, anything that can reach the frontend could
/// invoke gated tools at any time by quoting a made-up id.
static RUNS: LazyLock<Mutex<HashSet<String>>> = LazyLock::new(|| Mutex::new(HashSet::new()));
/// Set by the frontend once its codemode host is listening. Until then the
/// tool refuses fast instead of blocking for the whole run timeout on an
/// event nobody will answer.
static HOSTED: AtomicBool = AtomicBool::new(false);
static SEQ: AtomicU64 = AtomicU64::new(0);

pub fn init(handle: tauri::AppHandle) {
    let _ = HOST.set(handle);
}

pub fn set_hosted(active: bool) {
    HOSTED.store(active, Ordering::SeqCst);
}

pub fn hosted() -> bool {
    HOSTED.load(Ordering::SeqCst)
}

/// Whether `sid` is a codemode run in flight — the authorisation behind
/// `codemode_tool_call`.
pub fn run_active(sid: &str) -> bool {
    RUNS.lock().map(|r| r.contains(sid)).unwrap_or(false)
}

fn next_id() -> String {
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    format!("cm{}_{}", ms, SEQ.fetch_add(1, Ordering::Relaxed))
}

/// Ship one script to the frontend host and wait out its execution. The
/// frontend answers via `codemode_result`; everything else (timeout, no host)
/// is an error the model sees as the tool's result.
fn request(session: &str, code: &str) -> Result<String, String> {
    let host = HOST.get().ok_or_else(|| {
        "codemode needs the desktop app: this process has no JavaScript runtime".to_string()
    })?;
    if !hosted() {
        return Err("codemode is not ready in this window".to_string());
    }
    let sid = next_id();
    let (tx, rx) = mpsc::channel();
    if let Ok(mut p) = PENDING.lock() {
        p.insert(sid.clone(), tx);
    }
    if let Ok(mut r) = RUNS.lock() {
        r.insert(sid.clone());
    }
    let _ = host.emit(
        "e:codemode_run",
        json!({
            "sid": sid,
            "session": session,
            "code": code,
            "timeoutMs": RUN_TIMEOUT.as_millis() as u64,
        }),
    );
    let outcome = rx.recv_timeout(RUN_TIMEOUT);
    if let Ok(mut r) = RUNS.lock() {
        r.remove(&sid);
    }
    if let Ok(mut p) = PENDING.lock() {
        p.remove(&sid);
    }
    match outcome {
        Ok((true, out)) => Ok(out),
        Ok((false, err)) => Err(err),
        Err(_) => Err(format!(
            "codemode script did not finish within {}s (tool calls waiting on approval count toward this)",
            RUN_TIMEOUT.as_secs()
        )),
    }
}

/// The frontend's answer for `sid`: the shaped transcript, or the failure.
/// A stale id (already timed out engine-side) resolves nothing, which is
/// exactly what a late answer deserves.
pub fn resolve(sid: &str, ok: bool, output: String) {
    let tx = PENDING.lock().ok().and_then(|mut p| p.remove(sid));
    if let Some(tx) = tx {
        let _ = tx.send((ok, output));
    }
    if let Ok(mut r) = RUNS.lock() {
        r.remove(sid);
    }
}

/// What the model is told about writing scripts. The callable names are not
/// spelled out here on purpose: the model already sees every tool's schema in
/// each request, and this text cannot go stale the moment a plugin or MCP
/// server registers something new.
const DESCRIPTION: &str = "Write a JavaScript program that runs harness-side and calls e's other tools directly, replacing a whole sequence of LLM round-trips with one. The script runs as the body of an async function: top-level await works. Every tool in this chat's tool list is callable as `await tools.<name>(args)` with the same name and arguments (use bracket access for names that are not identifiers, e.g. `tools[\"mcp-files-list\"]`); calls are async, may run concurrently via Promise.all, and reject on failure — a denied approval or a blocked call rejects with its reason. Use `text(value)` to append output for the model; console.log and friends are captured too. There is no filesystem, network, timers or imports: scripts act only through tools. Risky tools (powershell, write_file) still ask the user for approval mid-script. Prefer this whenever work is iterative: loops, retries, searching or filtering large outputs, cross-referencing several results — anything where only the distilled result belongs in the conversation.";

pub struct CodeModeTool;
impl Tool for CodeModeTool {
    fn name(&self) -> &str {
        "codemode"
    }
    fn description(&self) -> &str {
        DESCRIPTION
    }
    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "code": {
                    "type": "string",
                    "description": "JavaScript source. Runs as an async function body; call tools via `await tools.<name>(args)` and produce output with text(value)."
                }
            },
            "required": ["code"]
        })
    }
    fn run(&self, ctx: &ToolContext, args: Value) -> ToolResult {
        let code = args
            .get("code")
            .and_then(|c| c.as_str())
            .map(str::trim)
            .filter(|c| !c.is_empty())
            .ok_or_else(|| "missing 'code'".to_string())?;
        request(&ctx.session, code)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tool_call_without_code_is_refused_before_anything_runs() {
        let ctx = ToolContext { workspace: std::path::PathBuf::from("C:/x"), session: "s1".into(), ..Default::default() };
        assert_eq!(CodeModeTool.run(&ctx, json!({})).unwrap_err(), "missing 'code'");
        assert_eq!(
            CodeModeTool.run(&ctx, json!({ "code": "   " })).unwrap_err(),
            "missing 'code'",
            "blank code is missing code"
        );
    }

    /// No host is initialised in a test binary, so this is also the headless
    /// `e-rpc` case: the refusal must be immediate, not a wait on an event no
    /// one will answer.
    #[test]
    fn without_a_javascript_runtime_the_tool_refuses_fast() {
        set_hosted(true);
        let ctx = ToolContext { workspace: std::path::PathBuf::from("C:/x"), session: "s1".into(), ..Default::default() };
        let err = CodeModeTool.run(&ctx, json!({ "code": "text(1)" })).unwrap_err();
        assert!(err.contains("no JavaScript runtime"), "{err}");
        set_hosted(false);
    }

    #[test]
    fn an_unhosted_window_is_refused_even_with_a_host_present() {
        // HOST is process-wide and absent in tests; hosted() alone is the
        // frontend handshake, and it must gate independently.
        set_hosted(false);
        assert!(!hosted());
        set_hosted(true);
        assert!(hosted());
        set_hosted(false);
    }

    /// The authorisation behind `codemode_tool_call`: a made-up id must never
    /// authorize anything, and a resolved run must not linger as active.
    #[test]
    fn a_made_up_run_id_authorizes_nothing() {
        assert!(!run_active("cm_totally-made-up"));
        // Resolving an unknown run is a no-op, not a panic.
        resolve("cm_nope", true, "late answer".into());
        assert!(!run_active("cm_nope"));
    }

    #[test]
    fn the_description_teaches_the_contract_without_naming_tools() {
        for phrase in ["await tools.<name>(args)", "text(value)", "Promise.all", "powershell"] {
            assert!(DESCRIPTION.contains(phrase), "description must mention {phrase}");
        }
        assert!(!DESCRIPTION.contains("read_file"), "naming tools here would go stale the moment a plugin registers one");
    }
}
