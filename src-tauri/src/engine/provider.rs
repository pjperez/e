use crate::engine::{Completion, Msg, Part, ToolCall};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicBool, Ordering};

/// A generic OpenAI-compatible chat provider (streaming, with tool calling).
/// Works with OpenAI and any gateway/local server exposing the
/// `/chat/completions` contract (Ollama, LM Studio, vLLM, Together, ...).
#[derive(Clone)]
pub struct ChatProvider {
    pub base_url: String,
    pub api_key: String,
    pub model: String,
    pub temperature: f64,
    pub client: reqwest::Client,
}

impl ChatProvider {
    pub fn new(base_url: String, api_key: String, model: String, temperature: f64) -> Self {
        ChatProvider {
            base_url: base_url.trim_end_matches('/').to_string(),
            api_key,
            model,
            temperature,
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
                        }
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
        body
    }
}

impl ChatProvider {
    /// Stream one completion. `cancelled` is polled between network chunks so a
    /// Stop takes effect mid-response instead of after the model finishes; the
    /// partial text produced so far is still returned.
    pub async fn chat<F, G>(
        &self,
        msgs: &[Msg],
        tools: &[Value],
        on_delta: F,
        on_reasoning: G,
        cancelled: &AtomicBool,
    ) -> Result<Completion, String>
    where
        F: Fn(&str) + Send + Sync,
        G: Fn(&str) + Send + Sync,
    {
        use futures_util::StreamExt;

        if cancelled.load(Ordering::SeqCst) {
            return Ok(Completion { text: String::new(), tool_calls: Vec::new(), usage: (0, 0), cost: None, reasoning: String::new() });
        }

        let body = self.to_openai(msgs, tools);
        let mut req = self.client.post(self.url()).header("Accept", "text/event-stream");
        if !self.api_key.is_empty() {
            req = req.bearer_auth(&self.api_key);
        }

        let resp = req.json(&body).send().await.map_err(|e| format!("request failed: {e}"))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(format!("provider returned {status}: {}", truncate(&text, 400)));
        }

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
