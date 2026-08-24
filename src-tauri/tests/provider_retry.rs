//! Throttling behaviour against a real socket. Backoff is the kind of thing
//! that looks right in isolation and still never fires in practice, so these
//! drive `ChatProvider::chat` through an actual HTTP conversation and count the
//! requests the provider receives.

use e_lib::engine::provider::{ChatProvider, RetryNotice};
use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

fn too_many(retry_after: Option<&str>) -> String {
    let body = r#"{"error":{"message":"slow down"}}"#;
    let extra = retry_after.map(|v| format!("Retry-After: {v}\r\n")).unwrap_or_default();
    format!(
        "HTTP/1.1 429 Too Many Requests\r\nContent-Type: application/json\r\nContent-Length: {}\r\n{extra}Connection: close\r\n\r\n{body}",
        body.len()
    )
}

fn streamed(text: &str) -> String {
    let body = format!("data: {{\"choices\":[{{\"delta\":{{\"content\":\"{text}\"}}}}]}}\n\ndata: [DONE]\n\n");
    format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

/// Serve `responses` in order, reusing the last one once they run out, and
/// report how many requests actually arrived.
fn spawn_provider(responses: Vec<String>) -> (String, Arc<AtomicUsize>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    let hits = Arc::new(AtomicUsize::new(0));
    let counter = hits.clone();

    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut sock) = stream else { break };
            let n = counter.fetch_add(1, Ordering::SeqCst);

            // Consume the whole request before replying: closing on a half-sent
            // body would surface as a transport error and hide what's being
            // tested.
            let mut seen: Vec<u8> = Vec::new();
            let mut buf = [0u8; 4096];
            loop {
                if let Some(h) = seen.windows(4).position(|w| w == b"\r\n\r\n") {
                    let headers = String::from_utf8_lossy(&seen[..h]).to_ascii_lowercase();
                    let len: usize = headers
                        .lines()
                        .find_map(|l| l.strip_prefix("content-length:"))
                        .and_then(|v| v.trim().parse().ok())
                        .unwrap_or(0);
                    if seen.len() >= h + 4 + len {
                        break;
                    }
                }
                match sock.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(k) => seen.extend_from_slice(&buf[..k]),
                }
            }

            let reply = responses.get(n).or_else(|| responses.last()).cloned().unwrap_or_default();
            let _ = sock.write_all(reply.as_bytes());
            let _ = sock.flush();
        }
    });

    (format!("http://127.0.0.1:{port}"), hits)
}

fn provider(base: &str) -> ChatProvider {
    ChatProvider::new(base.to_string(), String::new(), "test-model".into(), 1.0, None)
}

fn prompt() -> Vec<e_lib::engine::Msg> {
    vec![e_lib::engine::Msg::text("user", "hello")]
}

type Notice = (u32, u32, u64, u16, String);

/// Every notice the run reported, flattened so assertions read plainly.
fn recorder() -> (Arc<Mutex<Vec<Notice>>>, impl Fn(&RetryNotice) + Send + Sync) {
    let log: Arc<Mutex<Vec<Notice>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = log.clone();
    (log, move |n: &RetryNotice| {
        sink.lock()
            .expect("lock")
            .push((n.attempt, n.max_attempts, n.delay.as_millis() as u64, n.status, n.reason.clone()));
    })
}

#[tokio::test]
async fn a_throttled_request_backs_off_and_then_succeeds() {
    let (base, hits) = spawn_provider(vec![too_many(None), too_many(None), streamed("hi")]);
    let (log, on_retry) = recorder();
    let never = AtomicBool::new(false);

    let started = Instant::now();
    let got = provider(&base)
        .chat(&prompt(), &[], |_| {}, |_| {}, on_retry, &never)
        .await
        .expect("the third attempt answers");

    assert_eq!(got.text, "hi", "the retried request's own output is what comes back");
    assert_eq!(hits.load(Ordering::SeqCst), 3, "two throttles should cost exactly two extra requests");

    let notices = log.lock().expect("lock").clone();
    assert_eq!(notices.len(), 2, "the user has to be told about every wait, not just the first");
    assert_eq!(notices[0].0, 1, "notices are 1-based on the attempt that failed");
    assert_eq!(notices[1].0, 2);
    assert_eq!(notices[0].1, 4, "three retries means four attempts in total");
    assert_eq!(notices[0].3, 429);
    assert_eq!(notices[0].4, "rate limited");

    // Equal jitter: [half, full] of a doubling window, so waits grow but no two
    // clients come back at the same instant.
    assert!((500..=1000).contains(&notices[0].2), "first wait was {}ms", notices[0].2);
    assert!((1000..=2000).contains(&notices[1].2), "second wait was {}ms", notices[1].2);

    let waited = started.elapsed();
    assert!(waited >= Duration::from_millis(1500), "backoff was announced but not actually served: {waited:?}");
}

#[tokio::test]
async fn retries_run_out_and_the_error_says_so() {
    // Retry-After: 0 keeps the test quick while exercising the header path.
    let (base, hits) = spawn_provider(vec![too_many(Some("0"))]);
    let (log, on_retry) = recorder();
    let never = AtomicBool::new(false);

    let err = provider(&base)
        .chat(&prompt(), &[], |_| {}, |_| {}, on_retry, &never)
        .await
        .expect_err("a provider that never lets up must surface the failure");

    assert_eq!(hits.load(Ordering::SeqCst), 4, "one original attempt plus three retries, and no more");
    assert_eq!(log.lock().expect("lock").len(), 3);
    assert!(err.contains("429"), "the status has to survive for the UI to explain it: {err}");
    assert!(err.contains("after 3 retries"), "the error should own up to the retries: {err}");
    assert!(err.contains("slow down"), "the provider's own words are still the most useful part: {err}");
}

#[tokio::test]
async fn a_provider_asking_for_an_absurd_wait_is_not_waited_out() {
    // An hour-long Retry-After isn't throttling, it's closed. Sitting on it
    // would be indistinguishable from a hang.
    let (base, hits) = spawn_provider(vec![too_many(Some("3600"))]);
    let (log, on_retry) = recorder();
    let never = AtomicBool::new(false);

    let started = Instant::now();
    let err = provider(&base)
        .chat(&prompt(), &[], |_| {}, |_| {}, on_retry, &never)
        .await
        .expect_err("giving up beats a silent hour");

    assert_eq!(hits.load(Ordering::SeqCst), 1, "no point retrying a wait we refused to serve");
    assert!(log.lock().expect("lock").is_empty(), "a wait that never happens must not be announced");
    assert!(started.elapsed() < Duration::from_secs(5), "the caller was made to wait anyway");
    assert!(err.contains("429"), "{err}");
}

#[tokio::test]
async fn stop_during_a_backoff_takes_effect_immediately() {
    let (base, hits) = spawn_provider(vec![too_many(None)]);
    let (_log, on_retry) = recorder();
    let cancelled = Arc::new(AtomicBool::new(false));

    let flag = cancelled.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(150));
        flag.store(true, Ordering::SeqCst);
    });

    let started = Instant::now();
    let got = provider(&base)
        .chat(&prompt(), &[], |_| {}, |_| {}, on_retry, &cancelled)
        .await
        .expect("a stopped run is not a failed run");

    assert!(got.text.is_empty());
    assert_eq!(hits.load(Ordering::SeqCst), 1, "Stop must not be followed by another request");
    assert!(
        started.elapsed() < Duration::from_millis(600),
        "Stop waited out the backoff instead of interrupting it: {:?}",
        started.elapsed()
    );
}
