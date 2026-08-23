use serde_json::json;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, LazyLock, Mutex, OnceLock};
use std::time::{Duration, Instant};
use tauri::Emitter;

static HOST: OnceLock<ApprovalHost> = OnceLock::new();
static PENDING: LazyLock<Mutex<HashMap<String, mpsc::Sender<bool>>>> = LazyLock::new(|| Mutex::new(HashMap::new()));
static SEQ: AtomicU64 = AtomicU64::new(0);

/// How long we wait for a human before giving up and denying.
const TIMEOUT: Duration = Duration::from_secs(300);
/// Poll interval, so a Stop is noticed while we're waiting on the user.
const TICK: Duration = Duration::from_millis(150);

struct ApprovalHost {
    handle: tauri::AppHandle,
}

pub fn init(handle: tauri::AppHandle) {
    let _ = HOST.set(ApprovalHost { handle });
}

fn next_id() -> String {
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    format!("apr_{}_{}", ms, SEQ.fetch_add(1, Ordering::Relaxed))
}

/// Ask the user to approve a risky tool call. Blocks until they respond, the
/// run is cancelled, or we time out. `session` lets the frontend show the
/// prompt on the chat that actually asked for it.
pub fn request(session: &str, tool: &str, preview: &str, cancelled: &AtomicBool) -> bool {
    let host = match HOST.get() {
        Some(h) => h,
        None => return true, // no host (headless/rpc) = auto-approve
    };
    if cancelled.load(Ordering::SeqCst) {
        return false;
    }

    let id = next_id();
    let (tx, rx) = mpsc::channel();
    if let Ok(mut p) = PENDING.lock() {
        p.insert(id.clone(), tx);
    }
    let _ = host.handle.emit(
        "e:approval_request",
        json!({ "id": id, "sid": session, "tool": tool, "preview": preview }),
    );

    let deadline = Instant::now() + TIMEOUT;
    let answer = loop {
        match rx.recv_timeout(TICK) {
            Ok(approved) => break approved,
            Err(mpsc::RecvTimeoutError::Disconnected) => break false,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if cancelled.load(Ordering::SeqCst) || Instant::now() >= deadline {
                    break false;
                }
            }
        }
    };

    if let Ok(mut p) = PENDING.lock() {
        p.remove(&id);
    }
    if !answer {
        // Withdraw the prompt so a stopped/expired request doesn't linger in the UI.
        let _ = host.handle.emit("e:approval_close", json!({ "id": id, "sid": session }));
    }
    answer
}

pub fn resolve(id: &str, approved: bool) {
    let tx = PENDING.lock().ok().and_then(|mut p| p.remove(id));
    if let Some(tx) = tx {
        let _ = tx.send(approved);
    }
}
