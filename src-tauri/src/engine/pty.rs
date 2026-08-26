//! Long-lived pseudo-terminals, one per terminal tab in the right pane.
//!
//! This is deliberately not `ShellTool`. That tool runs one command, captures
//! what it printed and returns — the shape a *model* needs. A terminal is the
//! opposite shape: one process that outlives many commands, a byte stream in
//! both directions, and a size the program is told about so `less`, `top` and
//! a shell's own line editor lay themselves out correctly.
//!
//! Rust owns the pty and nothing else: it opens the pair, pumps bytes to the
//! webview as `e:pty_data`, and reports the exit as `e:pty_exit`. Deciding what
//! those bytes *mean* — cursor motion, colour, wrapping — belongs to whatever
//! draws them, which keeps this file to plumbing.
//!
//! The working directory is never taken from the caller. It is resolved from
//! the chat's own workspace by [`crate::pty_spawn`], so a plugin cannot open a
//! shell somewhere the chat has no business being.

use portable_pty::{native_pty_system, ChildKiller, CommandBuilder, MasterPty, PtySize};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::Path;
use std::sync::{Arc, LazyLock, Mutex, OnceLock};
use tauri::Emitter;

/// How many terminals may exist at once, across every chat. A pty is a real
/// process plus two threads; an accidental loop calling `spawn` should hit a
/// message rather than the OS handle limit.
const MAX_PTYS: usize = 24;

/// Bounds for a size we accept from the renderer. A zero or absurd dimension
/// reaches the OS as an invalid `TIOCSWINSZ`/`ResizePseudoConsole` argument.
const MIN_DIM: u16 = 1;
const MAX_DIM: u16 = 1000;

/// Read buffer. Big enough that a `cat` of a large file does not turn into
/// thousands of tiny events, small enough to stay responsive.
const READ_CHUNK: usize = 8 * 1024;

struct Pty {
    /// The chat this terminal belongs to. Every later call has to name the same
    /// chat, so an id guessed or reused elsewhere cannot reach into another
    /// project's shell.
    sid: String,
    /// Behind its own lock, and never written to while the registry is held.
    /// A pty's input buffer fills as soon as the child stops reading stdin — a
    /// long `ping`, a build — and `write_all` then blocks until it drains.
    /// Holding the registry across that would take every other terminal down
    /// with it, and the command itself runs off the UI thread for the same
    /// reason: a paste into a busy shell must not freeze the window.
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    master: Box<dyn MasterPty + Send>,
    killer: Box<dyn ChildKiller + Send + Sync>,
}

static PTYS: LazyLock<Mutex<HashMap<String, Pty>>> = LazyLock::new(|| Mutex::new(HashMap::new()));
static HOST: OnceLock<tauri::AppHandle> = OnceLock::new();

pub fn init(handle: tauri::AppHandle) {
    let _ = HOST.set(handle);
}

fn emit(event: &str, payload: serde_json::Value) {
    if let Some(h) = HOST.get() {
        let _ = h.emit(event, payload);
    }
}

fn clamp(v: u16) -> u16 {
    v.clamp(MIN_DIM, MAX_DIM)
}

/// The program a bare terminal tab starts.
///
/// `E_SHELL` wins so a user can pick one without a rebuild. Otherwise: on
/// Windows PowerShell, which is what a Windows user's muscle memory expects and
/// what `ShellTool`'s `cmd` is not; elsewhere `$SHELL`, falling back to `sh`
/// because that is the one thing guaranteed to exist.
fn default_shell() -> (String, Vec<String>) {
    if let Ok(s) = std::env::var("E_SHELL") {
        let s = s.trim().to_string();
        if !s.is_empty() {
            return (s, Vec::new());
        }
    }
    if cfg!(windows) {
        ("powershell.exe".to_string(), vec!["-NoLogo".to_string()])
    } else {
        let sh = std::env::var("SHELL").unwrap_or_default();
        let sh = if sh.trim().is_empty() { "/bin/sh".to_string() } else { sh };
        // A login-ish interactive shell, so the user's prompt and aliases are
        // there rather than a bare `$`.
        (sh, vec!["-i".to_string()])
    }
}

/// Open a pty running a shell in `cwd`, streaming its output under `id`.
///
/// Re-spawning an id that is already live is refused rather than silently
/// replacing it: the old process would be orphaned with its output still
/// arriving under the same id, and the pane would show two shells interleaved.
pub fn spawn(sid: &str, id: &str, cwd: &Path, cols: u16, rows: u16) -> Result<(), String> {
    let id = id.trim();
    if id.is_empty() {
        return Err("terminal id is empty".into());
    }
    {
        let map = PTYS.lock().map_err(|_| "terminal registry is poisoned")?;
        if map.contains_key(id) {
            return Err(format!("terminal '{id}' is already running"));
        }
        if map.len() >= MAX_PTYS {
            return Err(format!("too many terminals open ({MAX_PTYS}); close one first"));
        }
    }

    let size = PtySize {
        rows: clamp(rows),
        cols: clamp(cols),
        pixel_width: 0,
        pixel_height: 0,
    };
    let pair = native_pty_system()
        .openpty(size)
        .map_err(|e| format!("could not open a terminal: {e}"))?;

    let (prog, args) = default_shell();
    let mut cmd = CommandBuilder::new(&prog);
    for a in &args {
        cmd.arg(a);
    }
    cmd.cwd(cwd);
    // Programs decide whether to emit colour and how to address the cursor from
    // this. Without it they assume a dumb terminal and the pane looks dead.
    cmd.env("TERM", "xterm-256color");
    cmd.env("COLORTERM", "truecolor");

    let mut child = pair.slave.spawn_command(cmd).map_err(|e| {
        format!("could not start '{prog}': {e}. Set E_SHELL to a shell that exists.")
    })?;
    // The slave handle must go before the reader can ever see EOF: while this
    // process holds it open, a shell that exits leaves the read blocking
    // forever and the tab never reports that it closed.
    drop(pair.slave);

    let reader = pair
        .master
        .try_clone_reader()
        .map_err(|e| format!("could not read from the terminal: {e}"))?;
    let writer = pair
        .master
        .take_writer()
        .map_err(|e| format!("could not write to the terminal: {e}"))?;
    let killer = child.clone_killer();

    if let Ok(mut map) = PTYS.lock() {
        map.insert(
            id.to_string(),
            Pty {
                sid: sid.to_string(),
                writer: Arc::new(Mutex::new(writer)),
                master: pair.master,
                killer,
            },
        );
    }

    pump(id.to_string(), reader);

    let wait_id = id.to_string();
    std::thread::spawn(move || {
        let code = child.wait().map(|s| s.exit_code()).unwrap_or(0);
        // Drop the handles first, so a tab that respawns the same id after an
        // exit is not refused by a corpse still sitting in the registry.
        remove(&wait_id);
        emit("e:pty_exit", serde_json::json!({ "id": wait_id, "code": code }));
    });

    Ok(())
}

/// Forward the pty's output to the webview as text.
///
/// Bytes are decoded here rather than shipped raw because the event payload is
/// JSON. A chunk boundary can fall inside a multi-byte character, so an
/// incomplete tail is carried into the next read instead of being replaced with
/// `U+FFFD` — otherwise any accented character had a one-in-eight chance of
/// arriving as garbage.
fn pump(id: String, reader: Box<dyn Read + Send>) {
    std::thread::spawn(move || {
        drain(reader, |text| {
            emit("e:pty_data", serde_json::json!({ "id": id, "data": text }));
        });
    });
}

/// The read-and-decode loop, split out from the thread and the event bus so it
/// can be tested against a real pty without a running app.
fn drain(mut reader: Box<dyn Read + Send>, mut sink: impl FnMut(String)) {
    let mut buf = [0u8; READ_CHUNK];
    let mut carry: Vec<u8> = Vec::new();
    loop {
        match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                carry.extend_from_slice(&buf[..n]);
                let text = match std::str::from_utf8(&carry) {
                    Ok(s) => {
                        let s = s.to_string();
                        carry.clear();
                        s
                    }
                    Err(e) => {
                        let good = e.valid_up_to();
                        // An error past the last few bytes is a real encoding
                        // fault, not a split character: take it lossily so the
                        // stream cannot wedge.
                        if e.error_len().is_some() {
                            let s = String::from_utf8_lossy(&carry).into_owned();
                            carry.clear();
                            s
                        } else {
                            let s = String::from_utf8_lossy(&carry[..good]).into_owned();
                            carry.drain(..good);
                            s
                        }
                    }
                };
                if !text.is_empty() {
                    sink(text);
                }
            }
            Err(_) => break,
        }
    }
}

/// Look a terminal up, refusing one that belongs to a different chat.
///
/// Ownership is checked here rather than trusted from the renderer: the id is
/// caller-supplied, so without this a stray (or guessed) id would let one
/// chat's tab type into another chat's shell.
fn owned<'a>(map: &'a mut HashMap<String, Pty>, sid: &str, id: &str) -> Result<&'a mut Pty, String> {
    match map.get_mut(id) {
        None => Err(format!("terminal '{id}' is not running")),
        Some(p) if p.sid != sid => Err(format!("terminal '{id}' belongs to another chat")),
        Some(p) => Ok(p),
    }
}

pub fn write(sid: &str, id: &str, data: &str) -> Result<(), String> {
    // Take the writer out and release the registry before any I/O: the write
    // below can block for as long as the child ignores its stdin.
    let writer = {
        let mut map = PTYS.lock().map_err(|_| "terminal registry is poisoned")?;
        owned(&mut map, sid, id)?.writer.clone()
    };
    let mut w = writer.lock().map_err(|_| "terminal writer is poisoned")?;
    w.write_all(data.as_bytes())
        .and_then(|_| w.flush())
        .map_err(|e| format!("could not write to terminal '{id}': {e}"))
}

pub fn resize(sid: &str, id: &str, cols: u16, rows: u16) -> Result<(), String> {
    let mut map = PTYS.lock().map_err(|_| "terminal registry is poisoned")?;
    let pty = owned(&mut map, sid, id)?;
    pty.master
        .resize(PtySize { rows: clamp(rows), cols: clamp(cols), pixel_width: 0, pixel_height: 0 })
        .map_err(|e| format!("could not resize terminal '{id}': {e}"))
}

/// Kill a terminal's process. The exit event still comes from the waiter
/// thread, so closing a tab and a shell typing `exit` end up on the same path.
pub fn kill(sid: &str, id: &str) -> Result<(), String> {
    let mut map = PTYS.lock().map_err(|_| "terminal registry is poisoned")?;
    match map.get_mut(id) {
        // Already gone is success: closing a tab whose shell just exited is
        // the common case, not an error worth showing anyone.
        None => Ok(()),
        Some(p) if p.sid != sid => Err(format!("terminal '{id}' belongs to another chat")),
        Some(p) => p.killer.kill().map_err(|e| format!("could not stop terminal '{id}': {e}")),
    }
}

/// Stop and forget every terminal owned by one chat before its managed
/// worktree is removed. On Windows, even an idle PowerShell process keeps its
/// working directory open and makes `git worktree remove` fail.
pub fn kill_session(sid: &str) {
    let ids = {
        let mut map = match PTYS.lock() {
            Ok(m) => m,
            Err(e) => e.into_inner(),
        };
        let ids: Vec<String> = map
            .iter()
            .filter(|(_, pty)| pty.sid == sid)
            .map(|(id, _)| id.clone())
            .collect();
        for id in &ids {
            if let Some(pty) = map.get_mut(id) {
                let _ = pty.killer.kill();
            }
        }
        ids
    };
    // Child termination and release of its current-directory handle are
    // asynchronous on Windows. The waiter removes each registry entry only
    // after `child.wait()` completes, which gives deletion a reliable barrier.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    while std::time::Instant::now() < deadline {
        let alive = PTYS
            .lock()
            .map(|map| ids.iter().any(|id| map.contains_key(id)))
            .unwrap_or(false);
        if !alive {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
    if let Ok(mut map) = PTYS.lock() {
        for id in ids {
            map.remove(&id);
        }
    }
}

fn remove(id: &str) {
    if let Ok(mut map) = PTYS.lock() {
        map.remove(id);
    }
}

pub fn alive(sid: &str, id: &str) -> bool {
    PTYS.lock().map(|m| m.get(id).is_some_and(|p| p.sid == sid)).unwrap_or(false)
}

/// Stop every terminal, whoever owns it. Called on exit: a pty survives its
/// parent on both platforms, and an abandoned shell holding the project folder
/// open is what makes a later `git` operation fail for no visible reason.
pub fn shutdown() {
    let mut map = match PTYS.lock() {
        Ok(m) => m,
        Err(e) => e.into_inner(),
    };
    for (_, pty) in map.iter_mut() {
        let _ = pty.killer.kill();
    }
    map.clear();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_size_is_clamped_into_what_the_os_will_accept() {
        assert_eq!(clamp(0), MIN_DIM);
        assert_eq!(clamp(9000), MAX_DIM);
        assert_eq!(clamp(80), 80);
    }

    #[test]
    fn a_shell_is_always_named() {
        let (prog, _) = default_shell();
        assert!(!prog.trim().is_empty());
    }

    /// Writing to an id that was never spawned has to say so. It used to be
    /// possible for a pane to keep typing into nothing after its shell exited.
    #[test]
    fn talking_to_a_terminal_that_is_not_there_is_an_error() {
        assert!(write("chat", "no-such-terminal", "ls\r").is_err());
        assert!(resize("chat", "no-such-terminal", 80, 24).is_err());
        assert!(!alive("chat", "no-such-terminal"));
    }

    /// Closing a tab whose shell already exited is the normal case, so `kill`
    /// treats a missing terminal as success rather than surfacing a scary
    /// message every time a terminal is closed the ordinary way.
    #[test]
    fn killing_a_terminal_that_already_exited_is_not_an_error() {
        assert!(kill("chat", "no-such-terminal").is_ok());
    }

    /// The id is chosen by the caller, so ownership has to be checked on this
    /// side. Without it one chat's tab could type into — or kill — a shell
    /// running in a different project's folder.
    #[test]
    fn a_terminal_belonging_to_another_chat_is_refused() {
        let id = format!("owned-{}", std::process::id());
        let size = PtySize { rows: 24, cols: 80, pixel_width: 0, pixel_height: 0 };
        let pair = native_pty_system().openpty(size).expect("openpty");
        let (prog, args) = default_shell();
        let mut cmd = CommandBuilder::new(&prog);
        for a in &args {
            cmd.arg(a);
        }
        cmd.cwd(std::env::temp_dir());
        let mut child = pair.slave.spawn_command(cmd).expect("spawn shell");
        let writer = pair.master.take_writer().expect("writer");
        let killer = child.clone_killer();
        drop(pair.slave);
        PTYS.lock().unwrap().insert(
            id.clone(),
            Pty { sid: "chat-a".into(), writer: Arc::new(Mutex::new(writer)), master: pair.master, killer },
        );

        assert!(write("chat-b", &id, "whoami\r").is_err(), "another chat could type into it");
        assert!(resize("chat-b", &id, 100, 30).is_err(), "another chat could resize it");
        assert!(kill("chat-b", &id).is_err(), "another chat could kill it");
        assert!(!alive("chat-b", &id));
        // Its own chat still reaches it.
        assert!(alive("chat-a", &id));
        assert!(resize("chat-a", &id, 100, 30).is_ok());

        let _ = kill("chat-a", &id);
        let _ = child.kill();
        remove(&id);
    }

    /// The end-to-end plumbing check, against a real ConPTY/openpty: a command
    /// typed in has to come back out. Everything else in this module is
    /// bookkeeping around this one path, and it is the part that silently does
    /// nothing when the handles are opened in the wrong order.
    #[test]
    fn a_shell_echoes_what_is_typed_into_it() {
        use std::sync::mpsc;

        let size = PtySize { rows: 24, cols: 80, pixel_width: 0, pixel_height: 0 };
        let pair = native_pty_system().openpty(size).expect("openpty");
        let (prog, args) = default_shell();
        let mut cmd = CommandBuilder::new(&prog);
        for a in &args {
            cmd.arg(a);
        }
        cmd.cwd(std::env::temp_dir());
        cmd.env("TERM", "xterm-256color");

        let mut child = pair.slave.spawn_command(cmd).expect("spawn shell");
        let reader = pair.master.try_clone_reader().expect("reader");
        let mut writer = pair.master.take_writer().expect("writer");
        drop(pair.slave);

        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            drain(reader, |text| {
                let _ = tx.send(text);
            });
        });

        // Give the shell a moment to draw its prompt before typing at it.
        std::thread::sleep(std::time::Duration::from_millis(1500));
        writer.write_all(b"echo E_PTY_MARKER\r").expect("write");
        writer.flush().expect("flush");

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        let mut seen = String::new();
        // The echo of the typed line arrives first, so wait for the marker to
        // appear twice — the command, then its output.
        while std::time::Instant::now() < deadline && seen.matches("E_PTY_MARKER").count() < 2 {
            if let Ok(chunk) = rx.recv_timeout(std::time::Duration::from_millis(500)) {
                seen.push_str(&chunk);
                // A shell's line editor asks the terminal where the cursor is
                // (DSR) and *blocks until it answers*. This is the reply a real
                // view has to send; without it PowerShell never draws a prompt
                // and the pane looks broken while the pty is perfectly fine.
                if chunk.contains("\u{1b}[6n") {
                    let _ = writer.write_all(b"\x1b[1;1R");
                    let _ = writer.flush();
                }
            }
        }
        let _ = child.kill();
        assert!(
            seen.matches("E_PTY_MARKER").count() >= 2,
            "shell never echoed the command back; got {} bytes: {:?}",
            seen.len(),
            seen.chars().take(400).collect::<String>()
        );
    }
}
