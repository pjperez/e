//! e --rpc: headless JSONL protocol over stdio. Drive the engine from any
//! IDE/script/agent. Commands on stdin (one JSON per line), events on stdout.

use e_lib::engine::agent::{Agent, Config};
use e_lib::engine::tools::ToolRegistry;
use e_lib::engine::{Emitter, RunSummary, ToolCall};
use serde_json::json;
use std::io::{BufRead, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::mpsc;

fn emit_line(kind: &str, payload: serde_json::Value) {
    let mut line = json!({ "type": "event", "event": kind, "payload": payload }).to_string();
    line.push('\n');
    let mut out = std::io::stdout().lock();
    let _ = out.write_all(line.as_bytes());
    let _ = out.flush();
}

struct RpcEmitter;
impl Emitter for RpcEmitter {
    fn token(&self, s: &str) { emit_line("token", json!({ "text": s })); }
    fn reasoning(&self, s: &str) { emit_line("reasoning", json!({ "text": s })); }
    fn activity(&self, phase: &str, tool: Option<&str>, step: usize) {
        emit_line("activity", json!({ "phase": phase, "tool": tool, "step": step }));
    }
    fn retry(&self, n: &e_lib::engine::provider::RetryNotice) {
        emit_line(
            "retry",
            json!({ "attempt": n.attempt, "max": n.max_attempts, "delayMs": n.delay.as_millis() as u64, "status": n.status, "reason": n.reason }),
        );
    }
    fn tool_call(&self, tc: &ToolCall) {
        emit_line("tool_call", json!({ "id": tc.id, "name": tc.name, "arguments": tc.arguments.to_string() }));
    }
    fn tool_result(&self, id: &str, name: &str, ok: bool, output: &str) {
        emit_line("tool_result", json!({ "id": id, "name": name, "ok": ok, "output": output }));
    }
    fn summary(&self, s: &RunSummary) {
        emit_line(
            "summary",
            json!({ "steps": s.steps, "tools": s.tool_calls, "stopped": s.stopped, "tokensIn": s.tokens_in, "tokensOut": s.tokens_out, "contextTokens": s.context_tokens, "cost": s.cost, "error": s.error }),
        );
    }
    fn message_end(&self) { emit_line("message_end", json!({})); }
    fn done(&self, stopped: bool) { emit_line("done", json!({ "stopped": stopped })); }
    fn error(&self, msg: &str) { emit_line("error", json!({ "message": msg })); }
}

fn response(id: &serde_json::Value, ok: bool, error: Option<&str>) {
    let line = json!({ "type": "response", "id": id, "ok": ok, "error": error }).to_string();
    let mut out = std::io::stdout().lock();
    let _ = out.write_all(line.as_bytes());
    let _ = out.write_all(b"\n");
    let _ = out.flush();
}

#[tokio::main]
async fn main() {
    let cfg = Config::from_env();
    // MCP servers work headless — they are subprocesses, not UI. Start them
    // before the first command is answered so their tools are in the schema.
    // (Plugins are not available here: their modules run in the webview.)
    let tools = Arc::new(ToolRegistry::new());
    for h in e_lib::engine::mcp::load(tools.clone(), Some(cfg.workspace.clone())) {
        let _ = h.join();
    }
    let mut agent = Agent::with_tools(cfg, tools);
    let cancelled = Arc::new(AtomicBool::new(false));

    // stdin is read on its own thread so `stop` is observed *during* a run.
    // Handling it inline meant the loop was blocked on the run and stop could
    // only ever arrive after the run it was meant to interrupt had finished.
    let (tx, mut rx) = mpsc::unbounded_channel::<serde_json::Value>();
    let stop_flag = cancelled.clone();
    std::thread::spawn(move || {
        let stdin = std::io::stdin();
        for line in stdin.lock().lines() {
            let Ok(line) = line else { break };
            let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) else { continue };
            if v.get("type").and_then(|t| t.as_str()) == Some("stop") {
                stop_flag.store(true, Ordering::SeqCst);
                response(&v.get("id").cloned().unwrap_or(json!(null)), true, None);
                continue;
            }
            if tx.send(v).is_err() {
                break;
            }
        }
    });

    while let Some(v) = rx.recv().await {
        let id = v.get("id").cloned().unwrap_or(json!(null));
        match v.get("type").and_then(|t| t.as_str()).unwrap_or("") {
            "send" => {
                let text = v.get("text").and_then(|t| t.as_str()).unwrap_or("").to_string();
                let images: Vec<String> = v
                    .get("images")
                    .and_then(|a| a.as_array())
                    .map(|a| a.iter().filter_map(|x| x.as_str().map(|s| s.to_string())).collect())
                    .unwrap_or_default();
                cancelled.store(false, Ordering::SeqCst);
                let em = RpcEmitter;
                let _ = agent.run(&text, &images, &em, &cancelled).await;
                response(&id, true, None);
            }
            "reset" => {
                agent.reset();
                response(&id, true, None);
            }
            _ => response(&id, false, Some("unknown command")),
        }
    }
}
