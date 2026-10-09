//! MXC spike: validate, on this machine, the containment primitives `e` would
//! build on. Not part of the test suite — it needs Windows 11 and real OS
//! containment, and its findings are meant to be read, not asserted on CI.
//!
//! Run: `cargo run --example mxc-spike`
//!
//! The recipe under test (established through the diagnostics that became
//! this spike — see the conversation that produced it):
//!   - ui.disable = false           — console exes touch win32k while
//!                                    initialising; blocking it is instant
//!                                    0xC0000142 (even whoami dies)
//!   - readonly  C:\                — what desktop PowerShell needs to start
//!                                    (MSIX/packaged pwsh cannot run at all:
//!                                    CreateProcess error 5)
//!   - readwrite temp + workspace   — the only places writes may land
//!   - denied    ~/.e               — carve-outs beat the drive-wide grant
//!   - working_directory set        — or the backend picks a policy dir
//!   - BOTH pipes drained, each on its own thread — a child blocked on a full
//!     stderr pipe stops writing stdout entirely

use mxc_sdk::v1::{self, ClipboardPolicy, ContainerRequest, FilesystemPolicy, UiPolicy, WaitResult};
use std::io::Read;

fn ps(cmd: &str) -> String {
    format!("powershell -NoLogo -NoProfile -NonInteractive -Command {cmd}")
}

fn mark(name: &str, ok: bool, detail: &str) {
    println!("[{}] {name}: {detail}", if ok { "PASS" } else { "FAIL" });
    use std::io::Write;
    let _ = std::io::stdout().flush();
}

fn console_ui() -> UiPolicy {
    UiPolicy {
        // Console executables need the win32k subsystem during init; a
        // terminal tool wants a console, not a desktop — no clipboard, no
        // synthetic input.
        disable: false,
        clipboard: ClipboardPolicy::None,
        allow_input_injection: false,
    }
}

fn request(ws: &str, cmd: &str) -> ContainerRequest {
    let home = dirs::home_dir().expect("home");
    let mut req = ContainerRequest::new(cmd);
    req.ui = Some(console_ui());
    req.filesystem = Some(FilesystemPolicy {
        readonly_paths: vec![r"C:\".to_string()],
        readwrite_paths: vec![std::env::temp_dir().to_string_lossy().to_string(), ws.to_string()],
        denied_paths: vec![home.join(".e").to_string_lossy().to_string()],
        clear_policy_on_exit: None,
    });
    req.working_directory = Some(ws.to_string());
    req
}

fn main() {
    // Never hang a runner: every path below is bounded, but a regression in
    // the SDK's pipe handling once blocked a read forever.
    std::thread::spawn(|| {
        std::thread::sleep(std::time::Duration::from_secs(120));
        eprintln!("[watchdog] aborting");
        std::process::exit(2);
    });

    let ws_dir = std::env::temp_dir().join("mxc-spike-ws");
    std::fs::create_dir_all(&ws_dir).expect("create workspace");
    let ws = ws_dir.to_string_lossy().to_string();
    let home = dirs::home_dir().expect("home");

    // ---- P1: hello world through pipes ----
    match v1::run(request(&ws, &ps("Write-Output hello-from-container")), Default::default()) {
        Ok(r) => {
            let out = String::from_utf8_lossy(&r.stdout).trim().to_string();
            mark(
                "P1 pipes+exit",
                matches!(r.outcome, WaitResult::Exited(0)) && out == "hello-from-container",
                &format!("exit={:?} stdout={out:?}", r.outcome),
            );
        }
        Err(e) => mark("P1 pipes+exit", false, &format!("spawn error: {e}")),
    }

    // ---- P2: write inside the granted workspace ----
    match v1::run(request(&ws, &ps("Set-Content -Path inside.txt -Value ok; Write-Output wrote-inside")), Default::default()) {
        Ok(r) => {
            let inside_ok = ws_dir.join("inside.txt").is_file();
            mark("P2 write inside grant", inside_ok, &format!("file exists: {inside_ok}, exit={:?}", r.outcome));
        }
        Err(e) => mark("P2 write inside grant", false, &format!("spawn error: {e}")),
    }

    // ---- P3: a write outside every grant ($HOME is read-only under C:\) ----
    match v1::run(request(&ws, &ps("Set-Content -Path $HOME\\mxc-spike-denied.txt -Value nope; Write-Output attempted")), Default::default()) {
        Ok(r) => {
            let blocked = !home.join("mxc-spike-denied.txt").exists();
            mark(
                "P3 write outside denied",
                blocked,
                &format!("file absent: {blocked}, exit={:?} (PS continues past the non-terminating error)", r.outcome),
            );
        }
        Err(e) => mark("P3 write outside denied", false, &format!("spawn error: {e}")),
    }

    // ---- P3b: the deny carve-out beats the drive-wide readonly ----
    match v1::run(request(&ws, &ps("if (Test-Path $HOME\\.e\\install_id) { Write-Output secrets-readable } else { Write-Output secrets-denied }")), Default::default()) {
        Ok(r) => {
            let out = String::from_utf8_lossy(&r.stdout).trim().to_string();
            mark("P3b deny carve-out", out == "secrets-denied", &format!("stdout={out:?}"));
        }
        Err(e) => mark("P3b deny carve-out", false, &format!("spawn error: {e}")),
    }

    // ---- P4: timeout ----
    let mut req = request(&ws, &ps("Start-Sleep -Seconds 60"));
    req.timeout_ms = Some(2_000);
    let t0 = std::time::Instant::now();
    match v1::run(req, Default::default()) {
        Ok(r) => mark(
            "P4 timeout",
            matches!(r.outcome, WaitResult::TimedOut) && t0.elapsed().as_secs() < 30,
            &format!("outcome={:?} after {:?}", r.outcome, t0.elapsed()),
        ),
        Err(e) => mark("P4 timeout", false, &format!("spawn error: {e}")),
    }

    // ---- P5: spawn, stream, kill mid-run (the background-jobs path) ----
    match v1::spawn(
        request(&ws, &ps("foreach ($i in 1..100) { Write-Output ('line-' + $i); Start-Sleep -Milliseconds 100 }")),
        Default::default(),
    ) {
        Ok(mut p) => {
            let mut stdout = p.take_stdout().expect("stdout pipe");
            let mut stderr = p.take_stderr().expect("stderr pipe");
            // Drain stderr on its own thread or a full pipe there freezes
            // stdout — the exact hang this spike started as.
            let err_thread = std::thread::spawn(move || {
                let mut buf = String::new();
                let _ = stderr.read_to_string(&mut buf);
                buf
            });
            let mut buf = [0u8; 512];
            let n = stdout.read(&mut buf).unwrap_or(0);
            let first = String::from_utf8_lossy(&buf[..n]).trim().to_string();
            let alive_before_kill = matches!(p.try_wait(), Ok(None));
            let killed = p.kill().is_ok();
            let outcome = p.wait().ok();
            mark(
                "P5 stream+kill",
                first.contains("line-1") && alive_before_kill && killed && outcome.is_some(),
                &format!("first chunk={first:?} alive-before-kill={alive_before_kill} killed={killed} final={outcome:?} stderr={:?}", err_thread.join().unwrap_or_default()),
            );
        }
        Err(e) => mark("P5 stream+kill", false, &format!("spawn error: {e}")),
    }

    // ---- P6: cwd + layered environment ----
    let mut req = request(&ws, &ps("Write-Output ((Get-Location).Path + ' env=' + $env:E_SPIKE)"));
    req.environment = Some(vec![("E_SPIKE".to_string(), "seen".to_string())]);
    req.inherit_default_environment = Some(true);
    match v1::run(req, Default::default()) {
        Ok(r) => {
            let out = String::from_utf8_lossy(&r.stdout).trim().to_string();
            // Case-insensitive: PowerShell echoes the path with whatever case
            // the registry gave $env:TEMP, which need not match temp_dir().
            // temp_dir() can return the 8.3 short form (PEDROP~1) while
            // PowerShell echoes the long one, so compare past the username.
            let tail = r"\appdata\local\temp\mxc-spike-ws env=seen";
            let hit = out.to_lowercase().ends_with(tail);
            mark("P6 cwd+env", hit, &format!("stdout={out:?}"));
        }
        Err(e) => mark("P6 cwd+env", false, &format!("spawn error: {e}")),
    }

    let _ = std::fs::remove_file(ws_dir.join("inside.txt"));
    println!(
        "spike done — outside file existed: {}",
        home.join("mxc-spike-denied.txt").exists()
    );
}
