//! Background jobs: processes the agent starts now and polls later.
//!
//! The `powershell` tool blocks for at most 120s and answers once, which is
//! the wrong shape for work that runs longer — a deployment, a build, a dev
//! server. The standard pattern (CI platforms, `--no-wait` + `wait` pairs,
//! every background-shell tool in every serious agent) is fire-and-poll:
//! start detached, hand back a handle immediately, then poll for *new*
//! output until the process exits. Each poll is a millisecond-scale tool
//! call, so no timeout can ever bite, and progress is visible while the work
//! runs instead of only at the end.
//!
//! Everything here is intentionally unsophisticated: output is captured into
//! a capped in-memory buffer (the model polls deltas), a waiter thread turns
//! process exit into a status, and kill walks the process tree because
//! killing only the direct child would leave `npm run deploy`'s deploy
//! script running. Jobs live for the app's lifetime and are killed on exit —
//! same contract as the pty table.

use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

/// Bytes of output kept per job. Output past this is dropped from the front
/// (oldest first) rather than growing without bound — a runaway build must
/// not eat memory all afternoon. `total()` still reports how much the
/// process actually printed, so the model can tell that capture was capped.
pub const MAX_CAPTURE: usize = 1_000_000;

/// Newest bytes one poll will return. A job that printed 300 KB between
/// polls would otherwise pour a whole context window into the transcript in
/// one swallow; the model can poll more often if it wants finer grain.
pub const MAX_POLL: usize = 8_000;

/// How many jobs may exist at once, running or recently finished. Each is a
/// real process plus three threads; an accidental loop calling start should
/// hit a message rather than the OS handle limit.
pub const MAX_JOBS: usize = 32;

/// Finished jobs are forgotten after this long. Their processes are gone;
/// only the captured output is being kept, and an hour is generous for
/// "check what that deploy printed".
pub const RETAIN_FINISHED: Duration = Duration::from_secs(3600);

/// Longest one poll may block waiting for new output. Well under the
/// powershell tool's own 120s cap, so a poll can never be the call that
/// times out.
pub const MAX_WAIT: Duration = Duration::from_secs(25);

/// Told to the model once per system prompt; the tool descriptions carry the
/// details, this carries the policy: when to detach, and how to behave
/// while a job runs.
pub const AGENT_HINT: &str = "LONG-RUNNING WORK: the powershell tool blocks for at most 120s and then the process is killed. For anything that may run longer — builds, deployments, dev servers, watch tasks, installs — pass background:true to powershell: it returns a job id immediately and the command keeps running. Poll with process_poll (its wait_ms blocks up to 25s per call for new output or exit, so prefer one wait_ms=20000 poll over ten immediate ones), and stop a job with process_kill. Between polls, do other useful work rather than spin.";

static JOBS: LazyLock<Mutex<HashMap<String, Arc<Job>>>> = LazyLock::new(|| Mutex::new(HashMap::new()));
static SEQ: AtomicU64 = AtomicU64::new(0);

/// Capture state for one output stream (job polls merge stdout+stderr; the
/// sync shell tool keeps them apart).
///
/// `cursor`/`dropped`/`total` are absolute byte positions into the stream
/// the process has printed; `buf` only holds the newest `MAX_CAPTURE` bytes.
/// Absolute positions keep the model's poll cursor meaningful even after old
/// bytes are evicted.
pub struct Capture {
    buf: Vec<u8>,
    /// How far polls have read (absolute).
    cursor: usize,
    /// Absolute position of `buf[0]`: everything before it was evicted.
    dropped: usize,
    total: usize,
}

/// What one poll/tail call extracted: the text, plus how many bytes it had
/// to skip to stay within budget, so the model knows it is seeing an excerpt.
pub struct Taken {
    pub text: String,
    pub skipped: usize,
    /// Absolute position just past the returned text — where the next poll
    /// continues from.
    pub end: usize,
}

impl Capture {
    pub fn new() -> Self {
        Capture { buf: Vec::new(), cursor: 0, dropped: 0, total: 0 }
    }

    /// Append output, evicting the oldest bytes beyond the capture cap.
    pub fn push(&mut self, data: &[u8]) {
        self.total += data.len();
        self.buf.extend_from_slice(data);
        if self.buf.len() > MAX_CAPTURE {
            let excess = self.buf.len() - MAX_CAPTURE;
            self.buf.drain(..excess);
            self.dropped += excess;
        }
    }

    pub fn total(&self) -> usize {
        self.total
    }

    fn has_new(&self) -> bool {
        self.cursor < self.total
    }

    /// Everything printed since the cursor (i.e. since the last poll),
    /// trimmed to the newest `max` bytes at a character boundary. Advances
    /// the cursor: the next poll returns only what came after.
    pub fn take_new(&mut self, max: usize) -> Taken {
        let from = self.cursor.min(self.total);
        let taken = self.take_from(from, max);
        self.cursor = taken.end;
        taken
    }

    /// The newest `max` bytes of everything captured, without advancing the
    /// cursor — used to report what a killed/timed-out command printed.
    /// `skipped` counts from the very start of the stream, so evicted bytes
    /// are honestly reported as unseen.
    pub fn tail(&self, max: usize) -> Taken {
        self.take_from(0, max)
    }

    /// Read [from, total) — or whatever of it survives eviction — trimmed to
    /// the newest `max` bytes at a UTF-8 boundary. `skipped` is measured
    /// against `from` for take_new, against the stream start (0) for tail.
    fn take_from(&self, from: usize, max: usize) -> Taken {
        if from >= self.total {
            return Taken { text: String::new(), skipped: 0, end: from.max(self.total) };
        }
        let end_in_buf = self.buf.len().min(self.total - self.dropped);
        let begin_abs = from.max(self.dropped);
        let start_in_buf = begin_abs - self.dropped;
        let slice = &self.buf[start_in_buf..end_in_buf];

        // Keep only the newest `max` bytes, stepped back to a UTF-8 boundary.
        // A boundary is any byte that does not continue a character, i.e. one
        // whose top bits are not 10.
        let mut start = slice.len().saturating_sub(max);
        while start > 0 && (slice[start] & 0xC0) == 0x80 {
            start -= 1;
        }
        let (text, consumed) = decode_from(slice, start);
        let text_abs_start = begin_abs + start;
        Taken { text, skipped: text_abs_start.saturating_sub(from), end: begin_abs + consumed }
    }
}

impl Default for Capture {
    fn default() -> Self {
        Self::new()
    }
}

/// Decode bytes from `from`, tolerating a read that stops mid-character.
///
/// A poll can land between the bytes of a multi-byte UTF-8 character (the
/// same split-chunk problem the pty decoder solves). An incomplete trailing
/// sequence is left for the next poll rather than replaced with U+FFFD, so
/// no character is ever corrupted by where the poll happened to land;
/// genuinely invalid bytes are consumed lossily so the cursor can never
/// wedge. Returns the text and how many bytes were consumed.
fn decode_from(buf: &[u8], from: usize) -> (String, usize) {
    if from >= buf.len() {
        return (String::new(), from);
    }
    match std::str::from_utf8(&buf[from..]) {
        Ok(s) => (s.to_string(), buf.len()),
        Err(e) => {
            let mut end = from + e.valid_up_to();
            if let Some(len) = e.error_len() {
                // A genuinely invalid byte: consume it as U+FFFD rather than
                // letting the cursor stall on it forever.
                end += len;
            }
            let text = String::from_utf8_lossy(&buf[from..end]).into_owned();
            (text, end)
        }
    }
}

#[derive(Clone)]
pub enum Status {
    Running,
    Done { code: i32, at: Instant },
}

pub struct Job {
    pub id: String,
    pub command: String,
    pub cwd: PathBuf,
    pub pid: u32,
    pub started_at: Instant,
    /// Shared with the two reader threads, so polls see what they captured.
    pub cap: Arc<Mutex<Capture>>,
    pub status: Mutex<Status>,
    pub killed: AtomicBool,
}

impl Job {
    fn elapsed(&self) -> Duration {
        match &*self.status.lock().unwrap_or_else(|e| e.into_inner()) {
            Status::Running => self.started_at.elapsed(),
            Status::Done { at, .. } => at.saturating_duration_since(self.started_at),
        }
    }

    fn done(&self) -> bool {
        matches!(&*self.status.lock().unwrap_or_else(|e| e.into_inner()), Status::Done { .. })
    }

    /// The poll report: status line, then whatever the model has not seen
    /// yet. Always non-empty, so a poll never returns a blank tool result.
    fn render(&self) -> String {
        let status = self.status.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let mut cap = self.cap.lock().unwrap_or_else(|e| e.into_inner());
        let secs = self.elapsed().as_secs();
        let mut head = match status {
            Status::Running => format!(
                "[job {}] running, {secs}s elapsed, {} bytes of output so far",
                self.id,
                cap.total()
            ),
            Status::Done { code, .. } if self.killed.load(Ordering::SeqCst) => format!(
                "[job {}] killed by request after {secs}s (process tree terminated, exit code {code})",
                self.id
            ),
            Status::Done { code, .. } => format!(
                "[job {}] exited with code {code} after {secs}s (total output {} bytes)",
                self.id,
                cap.total()
            ),
        };
        let taken = cap.take_new(MAX_POLL);
        if !taken.text.trim().is_empty() {
            head.push('\n');
            head.push_str(&taken.text);
        } else if !self.done() {
            head.push_str("\n(no new output since the last poll)");
        } else if cap.total() == 0 {
            head.push_str("\n(the command printed nothing)");
        }
        if taken.skipped > 0 {
            head.push_str(&format!(
                "\n[… skipped {} bytes printed between polls — poll more often, or have long jobs write to a log file and read that]",
                taken.skipped
            ));
        }
        head
    }
}

/// The shell every command runs under — same choice as the powershell tool.
pub(crate) fn shell_executable() -> &'static str {
    if cfg!(windows) {
        "powershell.exe"
    } else {
        "pwsh"
    }
}

/// Kill `pid` and everything it spawned.
///
/// `Child::kill()` would only take the direct child — the powershell host —
/// while the deploy script it ran carried on. `taskkill /T` walks the tree
/// on Windows; on unix there is no process-group plumbing here, so a plain
/// `kill -9` on the pid is the best-effort equivalent.
pub(crate) fn kill_tree(pid: u32) {
    if cfg!(windows) {
        let mut c = crate::engine::quiet_command("taskkill");
        c.args(["/PID", &pid.to_string(), "/T", "/F"]);
        let _ = c.output();
    } else {
        let _ = Command::new("kill").args(["-9", &pid.to_string()]).output();
    }
}

/// Pump one output pipe into a capture buffer until EOF. Returns the
/// thread's handle so a waiter can be sure the last bytes landed before
/// declaring the work done.
pub(crate) fn spawn_reader(
    mut pipe: impl Read + Send + 'static,
    cap: Arc<Mutex<Capture>>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let mut buf = [0u8; 8 * 1024];
        loop {
            match pipe.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if let Ok(mut c) = cap.lock() {
                        c.push(&buf[..n]);
                    }
                }
            }
        }
    })
}

/// Wait for reader threads to finish, but no longer than `budget` total.
///
/// On the happy path the pipes hit EOF when the process exits and this
/// returns at once. A grandchild that inherited a write end keeps the pipe
/// open, though, so an unbounded join could hang the caller forever; after
/// the budget the remaining bytes simply arrive too late to be shown.
pub(crate) fn drain(handles: Vec<std::thread::JoinHandle<()>>, budget: Duration) {
    let deadline = Instant::now() + budget;
    for h in handles {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            break;
        }
        // `JoinHandle::join` has no timeout, so the wait itself goes on a
        // throwaway thread that can be abandoned.
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = h.join();
            let _ = tx.send(());
        });
        let _ = rx.recv_timeout(left);
    }
}

/// Start `command` in `cwd` as a background job and return its id at once.
///
/// stdin is null: a background command that reads stdin would otherwise wait
/// forever on a pipe nobody writes. stdout and stderr are merged into one
/// capture in print order, which is what "show me the progress" wants.
pub fn start(cwd: &Path, command: &str) -> Result<String, String> {
    {
        let mut map = JOBS.lock().map_err(|_| "job registry is poisoned")?;
        // Prune to one below the limit so a finished job always makes room.
        prune_locked(&mut map, MAX_JOBS - 1, RETAIN_FINISHED);
        if map.len() >= MAX_JOBS {
            return Err(format!(
                "too many background jobs ({MAX_JOBS}); poll or kill the existing ones first"
            ));
        }
    }

    let mut c = crate::engine::quiet_command(shell_executable());
    c.args(["-NoLogo", "-NoProfile", "-NonInteractive", "-Command", command]);
    c.current_dir(cwd);
    c.stdin(Stdio::null());
    c.stdout(Stdio::piped());
    c.stderr(Stdio::piped());
    let mut child = c.spawn().map_err(|e| format!("failed to start: {e}"))?;
    let pid = child.id();
    let stdout = child.stdout.take().ok_or("job has no stdout pipe")?;
    let stderr = child.stderr.take().ok_or("job has no stderr pipe")?;

    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let id = format!("bg-{:x}-{}", millis, SEQ.fetch_add(1, Ordering::SeqCst) + 1);
    let cap: Arc<Mutex<Capture>> = Arc::new(Mutex::new(Capture::new()));
    let job = Arc::new(Job {
        id: id.clone(),
        command: command.to_string(),
        cwd: cwd.to_path_buf(),
        pid,
        started_at: Instant::now(),
        cap: cap.clone(),
        status: Mutex::new(Status::Running),
        killed: AtomicBool::new(false),
    });

    {
        let mut map = JOBS.lock().map_err(|_| "job registry is poisoned")?;
        if map.len() >= MAX_JOBS {
            // A concurrent start slipped in first. Kill the child we just
            // spawned rather than leave it running unregistered and
            // unreachable by kill.
            kill_tree(pid);
            return Err(format!(
                "too many background jobs ({MAX_JOBS}); poll or kill the existing ones first"
            ));
        }
        map.insert(id.clone(), job.clone());
    }

    let j1 = spawn_reader(stdout, cap.clone());
    let j2 = spawn_reader(stderr, cap);

    let waiter_job = job.clone();
    std::thread::spawn(move || {
        let code = child.wait().map(|s| s.code().unwrap_or(-1)).unwrap_or(-1);
        // Let the readers drain what the pipes still held after exit, so the
        // final poll sees the command's last words, not everything but them.
        let _ = j1.join();
        let _ = j2.join();
        *waiter_job.status.lock().unwrap_or_else(|e| e.into_inner()) =
            Status::Done { code, at: Instant::now() };
    });

    Ok(id)
}

/// Poll a job: block up to `wait` for new output or exit, then report
/// status plus everything printed since the previous poll.
pub fn poll(id: &str, wait: Duration) -> Result<String, String> {
    let job = {
        let map = JOBS.lock().map_err(|_| "job registry is poisoned")?;
        map.get(id).cloned().ok_or_else(|| {
            format!(
                "no such background job: {id}. Only ids that powershell background:true returned are valid, and finished jobs are forgotten after an hour."
            )
        })?
    };
    let deadline = Instant::now() + wait;
    loop {
        {
            let running =
                matches!(&*job.status.lock().unwrap_or_else(|e| e.into_inner()), Status::Running);
            let has_new = job.cap.lock().unwrap_or_else(|e| e.into_inner()).has_new();
            if !running || has_new {
                break;
            }
        }
        if Instant::now() >= deadline {
            break;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    Ok(job.render())
}

/// Kill a job's whole process tree and mark it killed, so the next poll says
/// so rather than presenting taskkill's exit code as the command's own.
pub fn kill(id: &str) -> Result<String, String> {
    let job = {
        let map = JOBS.lock().map_err(|_| "job registry is poisoned")?;
        map.get(id).cloned().ok_or_else(|| format!("no such background job: {id}"))?
    };
    if job.done() {
        return Ok(format!("[job {id}] already exited; nothing to kill"));
    }
    job.killed.store(true, Ordering::SeqCst);
    kill_tree(job.pid);
    Ok(format!(
        "[job {id}] kill signalled (process tree terminated, pid {pid}); poll to see its last output",
        pid = job.pid
    ))
}

/// Kill and forget every job. Called on app exit: a detached deploy outliving
/// its harness is exactly the surprise nobody asked for, and on Windows the
/// same orphan would hold the project folder open and break the next `git`
/// operation (see the pty module's shutdown note).
pub fn shutdown_all() {
    let jobs: Vec<Arc<Job>> = {
        let mut map = match JOBS.lock() {
            Ok(m) => m,
            Err(e) => e.into_inner(),
        };
        map.drain().map(|(_, j)| j).collect()
    };
    for j in jobs {
        if !j.done() {
            kill_tree(j.pid);
        }
    }
}

/// Forget finished jobs past `retain`, then the oldest finished ones beyond
/// `max`. Running jobs are never dropped — a registry that discards live
/// processes is worse than one that refuses new work.
fn prune_locked(map: &mut HashMap<String, Arc<Job>>, max: usize, retain: Duration) {
    let expired: Vec<String> = map
        .iter()
        .filter(|(_, j)| {
            if let Status::Done { at, .. } = &*j.status.lock().unwrap_or_else(|e| e.into_inner()) {
                at.elapsed() > retain
            } else {
                false
            }
        })
        .map(|(id, _)| id.clone())
        .collect();
    for id in expired {
        map.remove(&id);
    }
    if map.len() <= max {
        return;
    }
    let mut finished: Vec<(Instant, String)> = map
        .iter()
        .filter_map(|(id, j)| match &*j.status.lock().unwrap_or_else(|e| e.into_inner()) {
            Status::Done { at, .. } => Some((*at, id.clone())),
            Status::Running => None,
        })
        .collect();
    finished.sort();
    for (_, id) in finished {
        if map.len() <= max {
            break;
        }
        map.remove(&id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp() -> PathBuf {
        std::env::temp_dir()
    }

    // -- capture / decoding -------------------------------------------------

    /// A poll that lands between the bytes of a multi-byte character must
    /// carry the orphan bytes to the next poll, not turn them into U+FFFD.
    #[test]
    fn a_poll_split_inside_a_character_loses_nothing() {
        let bytes = "héllo".as_bytes(); // 'é' is 0xc3 0xa9
        let (first, pos) = decode_from(&bytes[..2], 0);
        assert_eq!(first, "h");
        assert_eq!(pos, 1, "the incomplete 'é' must be left for the next poll");
        let (rest, end) = decode_from(bytes, pos);
        assert_eq!(rest, "éllo");
        assert_eq!(end, bytes.len());
    }

    /// Truly invalid bytes are consumed as replacement characters, across as
    /// many decode calls as it takes, so the cursor can never stall on them.
    #[test]
    fn invalid_bytes_are_consumed_not_stuck_on() {
        let (text, pos) = decode_from(&[b'a', 0xff, b'b'], 0);
        assert_eq!(pos, 2, "the invalid byte must be consumed");
        assert!(text.contains('a'), "{text}");
        let (text2, pos2) = decode_from(&[b'a', 0xff, b'b'], pos);
        assert_eq!(text2, "b", "the rest is decoded on the next call");
        assert_eq!(pos2, 3, "every byte is eventually consumed");
    }

    #[test]
    fn tail_keeps_the_newest_bytes_at_a_character_boundary() {
        let mut cap = Capture::new();
        cap.push("é".as_bytes());
        cap.push("é".as_bytes());
        cap.push("é".as_bytes()); // 6 bytes, boundaries at 0/2/4/6
        let t = cap.tail(2);
        assert_eq!(t.text, "é", "2 bytes is exactly the last character");
        assert_eq!(t.skipped, 4);

        let t = cap.tail(3);
        assert_eq!(t.text, "éé", "3 bytes must step back to the boundary at 2");
        assert_eq!(t.skipped, 2);
    }

    #[test]
    fn take_new_advances_and_tail_does_not() {
        let mut cap = Capture::new();
        cap.push(b"one ");
        let t = cap.take_new(1000);
        assert_eq!(t.text, "one ");
        cap.push(b"two");
        let t = cap.take_new(1000);
        assert_eq!(t.text, "two", "the first poll's bytes must not be returned twice");
        let t = cap.tail(1000);
        assert_eq!(t.text, "one two", "tail must still see everything");
    }

    #[test]
    fn take_new_respects_the_byte_budget_and_reports_the_skip() {
        let mut cap = Capture::new();
        cap.push(b"0123456789");
        let t = cap.take_new(4);
        assert_eq!(t.text, "6789", "take_new keeps the newest bytes");
        assert_eq!(t.skipped, 6);
        assert_eq!(t.end, 10, "the cursor lands past everything printed");
        let t = cap.take_new(4);
        assert_eq!(t.text, "", "nothing new after the budget was consumed");
        assert_eq!(t.end, 10);
    }

    #[test]
    fn capture_is_capped_at_the_front_without_losing_the_count() {
        let mut cap = Capture::new();
        cap.push(&vec![b'x'; 3000]);
        let t = cap.tail(1000);
        assert_eq!(t.text.len(), 1000, "tail is bounded");
        assert_eq!(t.skipped, 2000, "skipped counts evicted bytes as unseen");
        assert_eq!(cap.total(), 3000, "total reflects what was actually printed");
    }

    /// A poll whose cursor fell behind an eviction window reports the gap.
    #[test]
    fn a_poll_after_a_big_backlog_reports_what_it_skipped() {
        let mut cap = Capture::new();
        cap.push(b"01234");
        let _ = cap.take_new(1000); // cursor at 5
        cap.push(&vec![b'y'; MAX_CAPTURE + 500]); // evicts far past the cursor
        let t = cap.take_new(100);
        assert_eq!(t.text.len(), 100);
        assert!(
            t.skipped >= MAX_CAPTURE,
            "the skipped count must include the bytes evicted past the cursor: {}",
            t.skipped
        );
    }

    // -- registry lifecycle --------------------------------------------------

    fn done_job(id: &str, finished_ago: Duration) -> Arc<Job> {
        Arc::new(Job {
            id: id.into(),
            command: "echo".into(),
            cwd: tmp(),
            pid: 1,
            started_at: Instant::now() - finished_ago - Duration::from_secs(1),
            cap: Arc::new(Mutex::new(Capture::new())),
            status: Mutex::new(Status::Done { code: 0, at: Instant::now() - finished_ago }),
            killed: AtomicBool::new(false),
        })
    }

    #[test]
    fn pruning_drops_expired_finished_jobs_but_never_running_ones() {
        let mut map = HashMap::new();
        map.insert("old".into(), done_job("old", RETAIN_FINISHED + Duration::from_secs(10)));
        map.insert("fresh".into(), done_job("fresh", Duration::from_secs(1)));
        map.insert(
            "live".into(),
            Arc::new(Job {
                id: "live".into(),
                command: "echo".into(),
                cwd: tmp(),
                pid: 1,
                started_at: Instant::now(),
                cap: Arc::new(Mutex::new(Capture::new())),
                status: Mutex::new(Status::Running),
                killed: AtomicBool::new(false),
            }),
        );

        prune_locked(&mut map, 3, RETAIN_FINISHED);
        assert!(!map.contains_key("old"), "expired finished jobs must go");
        assert!(map.contains_key("fresh") && map.contains_key("live"));

        // Squeezing below max drops the *oldest finished* first, never a run.
        prune_locked(&mut map, 1, RETAIN_FINISHED);
        assert!(!map.contains_key("fresh"), "oldest finished goes before a live job");
        assert!(map.contains_key("live"), "a running job is never pruned");
    }

    // -- end-to-end against a real shell ------------------------------------

    fn wait_done(id: &str, timeout: Duration) -> String {
        let deadline = Instant::now() + timeout;
        loop {
            let out = poll(id, Duration::from_millis(500)).expect("poll");
            if out.contains("exited with code") || out.contains("killed by request") {
                return out;
            }
            assert!(Instant::now() < deadline, "job never finished: {out}");
        }
    }

    #[test]
    fn a_background_command_runs_to_completion_and_polls_cleanly() {
        let id = start(&tmp(), "Write-Output e-job-marker; Write-Output second-line").expect("start");
        let out = wait_done(&id, Duration::from_secs(30));
        assert!(out.contains("e-job-marker"), "{out}");
        assert!(out.contains("second-line"), "{out}");
        assert!(out.contains("exited with code 0"), "{out}");

        // The final poll consumed everything; a later poll must not replay it.
        let again = poll(&id, Duration::from_millis(0)).expect("poll");
        assert!(again.contains("exited with code 0"), "{again}");
        assert!(!again.contains("e-job-marker"), "output must not be returned twice: {again}");
    }

    #[test]
    fn a_failing_command_reports_its_exit_code_and_output() {
        let id = start(&tmp(), "Write-Output before-fail; exit 3").expect("start");
        let out = wait_done(&id, Duration::from_secs(30));
        assert!(out.contains("exited with code 3"), "{out}");
        assert!(out.contains("before-fail"), "{out}");
    }

    #[test]
    fn killing_a_job_stops_it_and_says_so() {
        let id = start(&tmp(), "Start-Sleep -Seconds 60").expect("start");
        std::thread::sleep(Duration::from_millis(700));
        let msg = kill(&id).expect("kill");
        assert!(msg.contains("kill"), "{msg}");
        let out = wait_done(&id, Duration::from_secs(15));
        assert!(out.contains("killed by request"), "{out}");
    }

    #[test]
    fn polling_an_unknown_id_is_an_error_not_a_panic() {
        assert!(poll("bg-does-not-exist", Duration::from_millis(0)).is_err());
        assert!(kill("bg-does-not-exist").is_err());
    }

    /// The poll-while-running report has to carry the status line even when
    /// nothing new printed, so a poll never returns an empty tool result.
    #[test]
    fn a_running_job_with_no_new_output_still_reports_status() {
        let id = start(&tmp(), "Start-Sleep -Seconds 5").expect("start");
        let out = poll(&id, Duration::from_millis(0)).expect("poll");
        assert!(out.contains("running"), "{out}");
        let _ = kill(&id);
    }
}
