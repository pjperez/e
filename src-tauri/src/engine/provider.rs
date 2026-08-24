use crate::engine::{Completion, Msg, Part, ToolCall};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// How many times a throttled or overloaded request is retried before the
/// failure is surfaced. Retrying is only ever attempted *before* the first
/// token has been streamed, so a retry can never duplicate visible output.
const MAX_RETRIES: u32 = BACKOFF_STEPS.len() as u32;
/// What each successive wait is worth. A rate limit is usually a per-minute
/// window, so the schedule escalates to straddle one rather than doubling from
/// a value too small to outlast it: a quick retry catches a momentary spike,
/// and the later steps wait the window out.
const BACKOFF_STEPS: [Duration; 4] = [
    Duration::from_secs(1),
    Duration::from_secs(15),
    Duration::from_secs(30),
    Duration::from_secs(60),
];
/// Longest we are willing to sit on a retry. A provider asking for more than
/// this via `Retry-After` isn't throttling us for a moment, it's shut for the
/// hour — waiting it out silently would look like a hang, so we surface it.
const BACKOFF_CAP: Duration = Duration::from_secs(60);

/// A throttled attempt that is about to be slept off, reported so the UI can
/// say what is happening and how long the wait is instead of freezing on
/// "thinking…".
pub struct RetryNotice {
    /// Which attempt just failed, 1-based.
    pub attempt: u32,
    /// Total attempts this request gets, including the first.
    pub max_attempts: u32,
    /// How long we are about to wait before trying again.
    pub delay: Duration,
    /// HTTP status that triggered the retry.
    pub status: u16,
    /// Short human-readable cause, e.g. "rate limited".
    pub reason: String,
}

/// A generic OpenAI-compatible chat provider (streaming, with tool calling).
/// Works with OpenAI and any gateway/local server exposing the
/// `/chat/completions` contract (Ollama, LM Studio, vLLM, Together, ...).
#[derive(Clone)]
pub struct ChatProvider {
    pub base_url: String,
    pub api_key: String,
    pub model: String,
    pub temperature: f64,
    /// Reasoning level to ask for. `None` leaves the field out of the request
    /// entirely, so providers that don't take one are never sent it.
    pub reasoning_effort: Option<String>,
    pub client: reqwest::Client,
}

impl ChatProvider {
    pub fn new(
        base_url: String,
        api_key: String,
        model: String,
        temperature: f64,
        reasoning_effort: Option<String>,
    ) -> Self {
        ChatProvider {
            base_url: base_url.trim_end_matches('/').to_string(),
            api_key,
            model,
            temperature,
            reasoning_effort: reasoning_effort.map(|e| e.trim().to_string()).filter(|e| !e.is_empty()),
            client: reqwest::Client::builder()
                .connect_timeout(std::time::Duration::from_secs(30))
                .build()
                .expect("reqwest client"),
        }
    }

    fn url(&self) -> String {
        format!("{}/chat/completions", self.base_url)
    }

    fn to_openai(&self, msgs: &[Msg], tools: &[Value]) -> Value {
        let mut out: Vec<Value> = Vec::new();
        for m in msgs {
            match m.role.as_str() {
                "system" => {
                    let t = m.plain_text_parts();
                    if !t.is_empty() {
                        out.push(json!({ "role": "system", "content": t }));
                    }
                }
                "user" => {
                    let t = m.plain_text_parts();
                    let imgs: Vec<&str> = m
                        .parts
                        .iter()
                        .filter_map(|p| match p {
                            Part::ImageData(u) => Some(u.as_str()),
                            _ => None,
                        })
                        .collect();
                    if imgs.is_empty() {
                        out.push(json!({ "role": "user", "content": t }));
                    } else {
                        let mut content: Vec<serde_json::Value> = Vec::new();
                        if !t.is_empty() {
                            content.push(json!({ "type": "text", "text": t }));
                        }
                        for u in imgs {
                            content.push(json!({
                                "type": "image_url",
                                "image_url": { "url": u }
                            }));
                        }
                        out.push(json!({ "role": "user", "content": content }));
                    }
                }
                "assistant" => {
                    let mut text = String::new();
                    let mut tcs: Vec<Value> = Vec::new();
                    for p in &m.parts {
                        match p {
                            Part::Text(t) => text.push_str(t),
                            Part::ImageData(_) => {}
                            Part::Reasoning(_) => {}
                            Part::ToolCall(tc) => {
                                tcs.push(json!({
                                    "id": tc.id,
                                    "type": "function",
                                    "function": { "name": tc.name, "arguments": tc.arguments.to_string() }
                                }));
                            }
                            Part::ToolResult { .. } => {}
                            // Our own record of a failed run — never replayed
                            // back to the model.
                            Part::Error(_) => {}
                        }
                    }
                    // An assistant turn with no content and no calls is either
                    // reasoning-only or an error marker; providers reject the
                    // empty message, so drop it rather than send it.
                    if text.is_empty() && tcs.is_empty() {
                        continue;
                    }
                    let mut o = json!({ "role": "assistant", "content": text });
                    if !tcs.is_empty() {
                        o["tool_calls"] = Value::Array(tcs);
                    }
                    out.push(o);
                }
                "tool" => {
                    for p in &m.parts {
                        if let Part::ToolResult { id, content, .. } = p {
                            out.push(json!({ "role": "tool", "tool_call_id": id, "content": content }));
                        }
                    }
                }
                _ => {}
            }
        }
        let mut body = json!({
            "model": self.model,
            "messages": out,
            "temperature": self.temperature,
            "stream": true,
        });
        if !tools.is_empty() {
            body["tools"] = Value::Array(tools.to_vec());
        }
        // Only ever sent when a level was actually chosen for this model:
        // providers that don't know the field reject the whole request, so an
        // always-present default would break every non-reasoning model.
        if let Some(effort) = &self.reasoning_effort {
            body["reasoning_effort"] = Value::String(effort.clone());
        }
        body
    }
}

impl ChatProvider {
    /// Stream one completion. `cancelled` is polled between network chunks so a
    /// Stop takes effect mid-response instead of after the model finishes; the
    /// partial text produced so far is still returned.
    ///
    /// A throttled (429) or unavailable (5xx) provider is retried up to
    /// [`MAX_RETRIES`] times on an escalating schedule with jitter, and each
    /// wait is announced through `on_retry` so the caller can tell the user
    /// what is going on rather than showing a stall.
    pub async fn chat<F, G, H>(
        &self,
        msgs: &[Msg],
        tools: &[Value],
        on_delta: F,
        on_reasoning: G,
        on_retry: H,
        cancelled: &AtomicBool,
    ) -> Result<Completion, String>
    where
        F: Fn(&str) + Send + Sync,
        G: Fn(&str) + Send + Sync,
        H: Fn(&RetryNotice) + Send + Sync,
    {
        use futures_util::StreamExt;

        if cancelled.load(Ordering::SeqCst) {
            return Ok(Completion { text: String::new(), tool_calls: Vec::new(), usage: (0, 0), cost: None, reasoning: String::new() });
        }

        // A fresh install has no provider. Say so plainly — the alternative is
        // posting to a relative URL and reporting a transport error nobody can
        // act on.
        if self.base_url.is_empty() {
            return Err("no provider configured".to_string());
        }

        let body = self.to_openai(msgs, tools);
        let max_attempts = MAX_RETRIES + 1;
        let mut attempt: u32 = 0;

        // Retries live here, before a single byte of the response body has been
        // read: once tokens start flowing they have already been shown, and
        // replaying the request would duplicate them.
        let resp = loop {
            attempt += 1;
            let mut req = self.client.post(self.url()).header("Accept", "text/event-stream");
            if !self.api_key.is_empty() {
                req = req.bearer_auth(&self.api_key);
            }

            let resp = req.json(&body).send().await.map_err(|e| format!("request failed: {e}"))?;
            let status = resp.status();
            if status.is_success() {
                break resp;
            }

            let asked = retry_after(resp.headers());
            let text = resp.text().await.unwrap_or_default();
            let code = status.as_u16();
            let fail = || {
                let retried = attempt - 1;
                let note = if retried > 0 {
                    format!(" after {retried} {}", if retried == 1 { "retry" } else { "retries" })
                } else {
                    String::new()
                };
                Err(format!("provider returned {status}{note}: {}", truncate(&text, 400)))
            };

            // 429 is the throttle; 5xx is the same class of transient failure
            // (overloaded/gateway) and clears on its own just as often. Every
            // other 4xx is our fault and repeating it verbatim can't help.
            if !(code == 429 || status.is_server_error()) || attempt >= max_attempts {
                return fail();
            }
            let delay = match asked {
                // Honour the provider's own number when it's a wait worth
                // sitting through, and give up rather than pretend otherwise.
                Some(d) if d > BACKOFF_CAP => return fail(),
                Some(d) => d,
                None => backoff_delay(attempt),
            };

            on_retry(&RetryNotice {
                attempt,
                max_attempts,
                delay,
                status: code,
                reason: if code == 429 { "rate limited".into() } else { "provider unavailable".into() },
            });

            if !sleep_cancellable(delay, cancelled).await {
                return Ok(Completion { text: String::new(), tool_calls: Vec::new(), usage: (0, 0), cost: None, reasoning: String::new() });
            }
        };

        let mut stream = resp.bytes_stream();
        let mut buf: Vec<u8> = Vec::new();
        let mut text_out = String::new();
        let mut reasoning_out = String::new();
        let mut usage_in: u64 = 0;
        let mut usage_out: u64 = 0;
        let mut cost: Option<f64> = None;
        let mut tc_tmp: std::collections::HashMap<usize, (Option<String>, Option<String>, String)> =
            std::collections::HashMap::new();

        while let Some(chunk) = stream.next().await {
            if cancelled.load(Ordering::SeqCst) {
                break;
            }
            let chunk = chunk.map_err(|e| format!("stream error: {e}"))?;
            buf.extend_from_slice(&chunk);
            loop {
                let end = buf.windows(2).position(|w| w == b"\n\n");
                let Some(pos) = end else { break };
                let line: Vec<u8> = buf.drain(..pos + 2).collect();
                let line = String::from_utf8_lossy(&line).to_string();
                for l in line.lines() {
                    let l = l.trim();
                    if !l.starts_with("data:") { continue; }
                    let data = l[5..].trim();
                    if data == "[DONE]" { continue; }
                    let Ok(v) = serde_json::from_str::<Value>(data) else { continue };
                    let Some(choices) = v.get("choices").and_then(|c| c.as_array()) else { continue };
                    let Some(choice) = choices.first() else { continue };
                    let Some(delta) = choice.get("delta") else { continue };
                    if let Some(r) = delta
                        .get("reasoning")
                        .or_else(|| delta.get("reasoning_content"))
                        .and_then(|x| x.as_str())
                    {
                        reasoning_out.push_str(r);
                        on_reasoning(r);
                    }
                    if let Some(t) = delta.get("content").and_then(|c| c.as_str()) {
                        text_out.push_str(t);
                        on_delta(t);
                    }
                    if let Some(u) = v.get("usage") {
                        usage_in = u.get("prompt_tokens").and_then(|x| x.as_u64()).unwrap_or(usage_in);
                        usage_out = u.get("completion_tokens").and_then(|x| x.as_u64()).unwrap_or(usage_out);
                        // Learn cost from the provider when it reports pricing.
                        if cost.is_none() {
                            if let Some(c) = u.get("cost").and_then(|x| x.as_f64()) {
                                cost = Some(c);
                            } else if let (Some(a), Some(b)) = (
                                u.get("input_cost").and_then(|x| x.as_f64()),
                                u.get("output_cost").and_then(|x| x.as_f64()),
                            ) {
                                cost = Some(a + b);
                            }
                        }
                    }
                    if let Some(tcs) = delta.get("tool_calls").and_then(|c| c.as_array()) {
                        for tc in tcs {
                            let idx = tc.get("index").and_then(|i| i.as_u64()).unwrap_or(0) as usize;
                            let e = tc_tmp.entry(idx).or_insert((None, None, String::new()));
                            if let Some(id) = tc.get("id").and_then(|i| i.as_str()) { e.0 = Some(id.to_string()); }
                            if let Some(fn_) = tc.get("function") {
                                if let Some(n) = fn_.get("name").and_then(|n| n.as_str()) { e.1 = Some(n.to_string()); }
                                if let Some(a) = fn_.get("arguments").and_then(|a| a.as_str()) { e.2.push_str(a); }
                            }
                        }
                    }
                }
            }
        }

        let mut tool_calls = Vec::new();
        let mut indices: Vec<usize> = tc_tmp.keys().cloned().collect();
        indices.sort_unstable();
        for i in indices {
            if let Some((id, name, args)) = tc_tmp.remove(&i) {
                let id = id.unwrap_or_else(|| format!("call_{}", i));
                let name = name.unwrap_or_default();
                let parsed: Value = serde_json::from_str(&args).unwrap_or(Value::Null);
                tool_calls.push(ToolCall { id, name, arguments: parsed });
            }
        }
        Ok(Completion { text: text_out, tool_calls, usage: (usage_in, usage_out), cost, reasoning: reasoning_out })
    }
}

/// How long the provider asked us to wait, when it said so.
///
/// Only the delta-seconds form of `Retry-After` is honoured; the HTTP-date form
/// needs a date parser and is vanishingly rare on these APIs, so it falls
/// through to plain backoff rather than pulling in a dependency.
fn retry_after(headers: &reqwest::header::HeaderMap) -> Option<Duration> {
    let raw = headers.get(reqwest::header::RETRY_AFTER)?.to_str().ok()?.trim();
    let secs: f64 = raw.parse().ok()?;
    if !secs.is_finite() || secs < 0.0 {
        return None;
    }
    Some(Duration::from_millis((secs * 1000.0) as u64))
}

/// The wait before `attempt`'s successor, taken from [`BACKOFF_STEPS`] with a
/// little jitter added on top so several clients throttled at the same moment
/// don't all come back on the same tick. The jitter only ever lengthens a wait,
/// so each step is still at least the interval it advertises.
fn backoff_delay(attempt: u32) -> Duration {
    let idx = (attempt.saturating_sub(1) as usize).min(BACKOFF_STEPS.len() - 1);
    let step = BACKOFF_STEPS[idx];
    step + Duration::from_millis(jitter_ms(step.as_millis() as u64 / 10))
}

/// Cheap jitter in `0..=span_ms`. This only has to de-correlate retrying
/// clients, not be unpredictable, so the clock is enough and `rand` stays out
/// of the dependency list.
fn jitter_ms(span_ms: u64) -> u64 {
    if span_ms == 0 {
        return 0;
    }
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as u64)
        .unwrap_or(0);
    // Mixed because the fast-moving low bits are the only entropy here, and
    // taking them modulo a span directly leaves them clustered.
    nanos.wrapping_mul(6364136223846793005).rotate_left(17) % (span_ms + 1)
}

/// Sleep, but stay stoppable: a backoff can run into seconds, and a Stop during
/// one has to take effect now rather than after the wait. Returns false when the
/// run was cancelled, in which case the caller must not retry.
async fn sleep_cancellable(d: Duration, cancelled: &AtomicBool) -> bool {
    let deadline = std::time::Instant::now() + d;
    loop {
        if cancelled.load(Ordering::SeqCst) {
            return false;
        }
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        if left.is_zero() {
            return true;
        }
        tokio::time::sleep(left.min(Duration::from_millis(100))).await;
    }
}

fn truncate(s: &str, n: usize) -> String {
    if s.len() <= n {
        return s.to_string();
    }
    // Never slice mid-codepoint: walk back to the nearest char boundary.
    let mut end = n;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &s[..end])
}

// ---------- what a provider says about its own models ----------

/// One entry of a provider's `/models` listing.
///
/// Every field past the id is optional, and `None` means "the listing didn't
/// say" — never "no". Gateways range from OpenAI's bare `{id, object}` to
/// OpenRouter's fully described entries, and a silent listing must not be read
/// as the model refusing a capability.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ModelInfo {
    pub id: String,
    /// Usable context window in tokens, as advertised.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window: Option<u64>,
    /// Whether this model takes a reasoning level.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<bool>,
    /// The levels it takes, when the listing enumerates them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reasoning_efforts: Vec<String>,
}

/// Keys used for the context window in the wild: OpenRouter (`context_length`),
/// GitHub-style capability blocks (`max_context_window_tokens`), vLLM
/// (`max_model_len`), LiteLLM (`max_input_tokens`).
const WINDOW_KEYS: &[&str] = &[
    "context_window",
    "context_length",
    "max_context_window_tokens",
    "max_context_length",
    "max_model_len",
    "max_input_tokens",
];

/// Keys whose presence says something about reasoning. Values may be a bool, a
/// list of levels, or a nested object, so each is inspected rather than cast.
const REASONING_KEYS: &[&str] = &[
    "reasoning",
    "reasoning_effort",
    "supports_reasoning",
    "supports_reasoning_effort",
    "include_reasoning",
    "thinking",
    "supported_reasoning_efforts",
    "reasoning_efforts",
];

/// Request fields a model accepts. An explicit list that omits reasoning is the
/// one case where we can say a model definitely doesn't take a level.
const PARAM_LIST_KEYS: &[&str] = &["supported_parameters", "supported_params"];

/// How deep to hunt for capability keys. Providers nest them under
/// `top_provider`, `capabilities`, `capabilities.limits` or `model_info`, so
/// searching by key beats hard-coding every vendor's path.
const SEARCH_DEPTH: usize = 3;

/// Pull the model list out of a `/models` response, keeping everything the
/// provider chose to tell us about each model instead of only the id.
pub fn parse_models(v: &Value) -> Vec<ModelInfo> {
    let items = v
        .get("data")
        .and_then(|d| d.as_array())
        .or_else(|| v.get("models").and_then(|d| d.as_array()))
        .or_else(|| v.as_array());
    let Some(items) = items else { return Vec::new() };
    let mut out: Vec<ModelInfo> = Vec::new();
    for m in items {
        let Some(info) = model_info(m) else { continue };
        // A gateway that fronts several upstreams can list the same id twice;
        // the picker treats a model id as unique per provider, so must we.
        if out.iter().any(|x| x.id == info.id) {
            continue;
        }
        out.push(info);
    }
    out
}

fn model_info(m: &Value) -> Option<ModelInfo> {
    let id = ["id", "model", "model_name", "name"]
        .iter()
        .find_map(|k| m.get(*k).and_then(|v| v.as_str()))
        .map(str::trim)
        .filter(|s| !s.is_empty())?
        .to_string();
    let (reasoning, reasoning_efforts) = reasoning_of(m);
    Some(ModelInfo { id, context_window: window_of(m), reasoning, reasoning_efforts })
}

/// Every value filed under one of `keys`, anywhere in the shallow object tree.
fn collect<'a>(v: &'a Value, keys: &[&str], depth: usize, out: &mut Vec<&'a Value>) {
    let Some(obj) = v.as_object() else { return };
    for k in keys {
        if let Some(found) = obj.get(*k) {
            out.push(found);
        }
    }
    if depth == 0 {
        return;
    }
    for val in obj.values() {
        collect(val, keys, depth - 1, out);
    }
}

/// The smallest window the listing mentions.
///
/// Deliberately the smallest rather than the first match: a listing can quote
/// both a headline window and the smaller one actually served (OpenRouter's
/// `top_provider`), and this number is a budget to stay under. Guessing high
/// overflows the model; guessing low only compacts a little early.
fn window_of(m: &Value) -> Option<u64> {
    let mut found = Vec::new();
    collect(m, WINDOW_KEYS, SEARCH_DEPTH, &mut found);
    found
        .iter()
        .filter_map(|v| v.as_u64().or_else(|| v.as_f64().filter(|f| *f > 0.0).map(|f| f as u64)))
        .filter(|n| *n > 0)
        .min()
}

fn reasoning_of(m: &Value) -> (Option<bool>, Vec<String>) {
    let mut efforts: Vec<String> = Vec::new();
    let mut yes = false;
    let mut no = false;

    let mut found = Vec::new();
    collect(m, REASONING_KEYS, SEARCH_DEPTH, &mut found);
    for v in found {
        match v {
            Value::Bool(b) => {
                if *b {
                    yes = true
                } else {
                    no = true
                }
            }
            // A list under one of these keys is the set of levels on offer.
            Value::Array(a) => {
                for e in a.iter().filter_map(|e| e.as_str()) {
                    push_effort(&mut efforts, e);
                }
                yes = true;
            }
            // A described block (`"reasoning": {"effort": [...]}`) is itself
            // the provider saying the model reasons.
            Value::Object(o) => {
                yes = true;
                for e in o.values().filter_map(|v| v.as_array()).flatten().filter_map(|e| e.as_str()) {
                    push_effort(&mut efforts, e);
                }
            }
            Value::String(s) => {
                let low = s.trim().to_ascii_lowercase();
                if low == "false" || low == "none" || low == "off" {
                    no = true;
                } else if !low.is_empty() {
                    yes = true;
                    push_effort(&mut efforts, s);
                }
            }
            _ => {}
        }
    }

    let mut params = Vec::new();
    collect(m, PARAM_LIST_KEYS, SEARCH_DEPTH, &mut params);
    for list in params.iter().filter_map(|v| v.as_array()) {
        if list
            .iter()
            .filter_map(|x| x.as_str())
            .any(|s| s.eq_ignore_ascii_case("reasoning") || s.eq_ignore_ascii_case("reasoning_effort") || s.eq_ignore_ascii_case("include_reasoning") || s.eq_ignore_ascii_case("thinking"))
        {
            yes = true;
        } else if !list.is_empty() {
            // The provider enumerated what this model accepts and reasoning was
            // not on it — the one honest "no" available to us.
            no = true;
        }
    }

    let verdict = if yes {
        Some(true)
    } else if no {
        Some(false)
    } else {
        None
    };
    if verdict != Some(true) {
        efforts.clear();
    }
    (verdict, efforts)
}

fn push_effort(out: &mut Vec<String>, raw: &str) {
    let e = raw.trim().to_ascii_lowercase();
    // Skip the flag names themselves; only real levels belong in the list.
    if e.is_empty() || e == "true" || e == "false" || e == "reasoning" || e == "reasoning_effort" {
        return;
    }
    if !out.iter().any(|x| *x == e) {
        out.push(e);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn one(v: serde_json::Value) -> ModelInfo {
        parse_models(&v).into_iter().next().expect("one model")
    }

    fn header_map(value: &str) -> reqwest::header::HeaderMap {
        let mut h = reqwest::header::HeaderMap::new();
        h.insert(reqwest::header::RETRY_AFTER, value.parse().expect("header value"));
        h
    }

    #[test]
    fn backoff_follows_the_schedule_and_only_ever_rounds_up() {
        // Jitter must never shorten a step: 1s has to mean at least 1s.
        for (attempt, step) in [(1u32, 1000u64), (2, 15_000), (3, 30_000), (4, 60_000)] {
            let ms = backoff_delay(attempt).as_millis() as u64;
            assert!(ms >= step, "attempt {attempt} waited {ms}ms, under its advertised {step}ms");
            assert!(ms <= step + step / 10, "attempt {attempt} waited {ms}ms, more than 10% over {step}ms");
        }
    }

    #[test]
    fn backoff_saturates_at_the_last_step() {
        // An attempt past the end of the table must reuse the final wait rather
        // than index out of bounds. Compared against the schedule, not against
        // a second sample: two calls jitter independently.
        let step = BACKOFF_STEPS[BACKOFF_STEPS.len() - 1].as_millis() as u64;
        let ms = backoff_delay(99).as_millis() as u64;
        assert!(
            ms >= step && ms <= step + step / 10,
            "a runaway attempt count waited {ms}ms, off the final {step}ms step"
        );
    }

    #[test]
    fn every_scheduled_step_is_one_we_are_willing_to_wait() {
        for step in BACKOFF_STEPS {
            assert!(step <= BACKOFF_CAP, "{step:?} exceeds the wait we refuse from a provider");
        }
    }

    #[test]
    fn jitter_stays_within_the_span() {
        for span in [0u64, 1, 500, 30_000] {
            assert!(jitter_ms(span) <= span, "jitter escaped its span of {span}ms");
        }
    }

    #[test]
    fn retry_after_seconds_are_honoured() {
        assert_eq!(retry_after(&header_map("7")), Some(Duration::from_secs(7)));
        assert_eq!(retry_after(&header_map(" 1.5 ")), Some(Duration::from_millis(1500)));
    }

    #[test]
    fn an_unparseable_retry_after_falls_back_to_backoff() {
        // The HTTP-date form and any junk must yield None rather than a wait of
        // zero, which would hammer the provider that just throttled us.
        assert_eq!(retry_after(&header_map("Wed, 21 Oct 2015 07:28:00 GMT")), None);
        assert_eq!(retry_after(&header_map("-3")), None);
        assert_eq!(retry_after(&reqwest::header::HeaderMap::new()), None);
    }

    #[test]
    fn a_bare_openai_listing_still_yields_the_model() {
        let got = one(json!({ "data": [{ "id": "gpt-4o", "object": "model", "owned_by": "openai" }] }));
        assert_eq!(got.id, "gpt-4o");
        assert_eq!(got.context_window, None, "a silent listing must not invent a window");
        assert_eq!(got.reasoning, None, "silence is not the same as 'no reasoning'");
    }

    #[test]
    fn openrouter_style_metadata_is_kept() {
        let got = one(json!({ "data": [{
            "id": "openai/gpt-5",
            "context_length": 400_000,
            "top_provider": { "context_length": 272_000, "max_completion_tokens": 128_000 },
            "supported_parameters": ["tools", "reasoning", "include_reasoning", "max_tokens"],
        }] }));
        assert_eq!(got.context_window, Some(272_000), "the window actually served wins over the headline one");
        assert_eq!(got.reasoning, Some(true));
    }

    #[test]
    fn a_nested_capability_block_is_found() {
        let got = one(json!({ "data": [{
            "id": "gpt-4.1",
            "capabilities": {
                "family": "gpt-4.1",
                "limits": { "max_context_window_tokens": 128_000, "max_output_tokens": 16_384 },
                "supports": { "streaming": true, "tool_calls": true },
            },
        }] }));
        assert_eq!(got.context_window, Some(128_000), "hunting by key beats hard-coding vendor paths");
        assert_eq!(got.reasoning, None);
    }

    #[test]
    fn an_enumerated_parameter_list_without_reasoning_is_a_real_no() {
        let got = one(json!({ "data": [{ "id": "small", "supported_parameters": ["tools", "max_tokens"] }] }));
        assert_eq!(got.reasoning, Some(false), "the provider listed what it takes and reasoning wasn't in it");
    }

    #[test]
    fn advertised_levels_are_captured() {
        let got = one(json!({ "data": [{
            "id": "thinker",
            "supported_reasoning_efforts": ["Low", "medium", "HIGH", "low"],
            "max_model_len": 32_768,
        }] }));
        assert_eq!(got.reasoning, Some(true));
        assert_eq!(got.reasoning_efforts, vec!["low", "medium", "high"], "levels are normalised and de-duplicated");
        assert_eq!(got.context_window, Some(32_768));
    }

    #[test]
    fn an_explicit_false_is_respected_over_a_missing_window() {
        let got = one(json!({ "data": [{ "id": "plain", "supports_reasoning": false }] }));
        assert_eq!(got.reasoning, Some(false));
        assert!(got.reasoning_efforts.is_empty(), "a model that doesn't reason offers no levels");
    }

    #[test]
    fn other_list_shapes_and_duplicates_are_handled() {
        let v = json!({ "models": [
            { "id": "a", "max_input_tokens": 8_000 },
            { "id": "a", "max_input_tokens": 99 },
            { "name": "b" },
            { "object": "model" },
        ] });
        let got = parse_models(&v);
        assert_eq!(got.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(), vec!["a", "b"]);
        assert_eq!(got[0].context_window, Some(8_000), "the first entry for an id wins");
    }

    #[test]
    fn a_reasoning_level_is_sent_only_when_one_was_chosen() {
        let plain = ChatProvider::new("https://x.test/v1".into(), String::new(), "m".into(), 0.7, None);
        let body = plain.to_openai(&[Msg::text("user", "hi")], &[]);
        assert!(body.get("reasoning_effort").is_none(), "a model with no level chosen must not be sent the field");

        let thinking = ChatProvider::new("https://x.test/v1".into(), String::new(), "m".into(), 0.7, Some("high".into()));
        let body = thinking.to_openai(&[Msg::text("user", "hi")], &[]);
        assert_eq!(body["reasoning_effort"], json!("high"));

        let blank = ChatProvider::new("https://x.test/v1".into(), String::new(), "m".into(), 0.7, Some("  ".into()));
        assert!(blank.reasoning_effort.is_none(), "an empty level is no level at all");
    }
}
