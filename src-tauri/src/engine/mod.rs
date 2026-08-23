use serde::{Deserialize, Serialize};

pub mod agent;
pub mod approval;
pub mod mcp;
pub mod plugins;
pub mod provider;
pub mod sessions;
pub mod skills;
pub mod tools;

/// One tool invocation requested by the model.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub arguments: serde_json::Value,
}

/// A piece of a message.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub enum Part {
    Text(String),
    /// A model's thinking/reasoning block (persisted with the message).
    Reasoning(String),
    /// A user-uploaded/pasted image as a data URL: "data:image/png;base64,..."
    ImageData(String),
    ToolCall(ToolCall),
    ToolResult { id: String, #[allow(dead_code)] name: String, content: String },
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct Msg {
    pub role: String, // system | user | assistant | tool
    pub parts: Vec<Part>,
}

impl Msg {
    pub fn text(role: &str, content: impl Into<String>) -> Self {
        Msg { role: role.to_string(), parts: vec![Part::Text(content.into())] }
    }
    pub fn assistant(parts: Vec<Part>) -> Self {
        Msg { role: "assistant".into(), parts }
    }
    pub fn tool_result(id: &str, name: &str, content: String) -> Self {
        Msg {
            role: "tool".into(),
            parts: vec![Part::ToolResult { id: id.into(), name: name.into(), content }],
        }
    }
    pub fn plain_text_parts(&self) -> String {
        self.parts
            .iter()
            .filter_map(|p| match p {
                Part::Text(t) => Some(t.clone()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// A full assistant completion returned by a provider.
#[derive(Clone, Debug)]
pub struct Completion {
    pub text: String,
    pub tool_calls: Vec<ToolCall>,
    /// (prompt, completion) tokens as reported by the provider.
    pub usage: (u64, u64),
    /// Provider-reported cost, if the provider includes pricing.
    pub cost: Option<f64>,
    /// The model's reasoning / thinking text, when the provider streams it.
    pub reasoning: String,
}

/// Event sink. The agent emits lifecycle events here; the GUI layer plugs an
/// implementation that forwards them to the frontend over Tauri events.
/// Summary returned at the end of a run, carrying real token usage.
#[derive(Clone, Debug, Default)]
pub struct RunSummary {
    pub steps: usize,
    pub tool_calls: usize,
    pub stopped: bool,
    pub tokens_in: u64,
    pub tokens_out: u64,
    pub cost: Option<f64>,
    pub error: Option<String>,
}

pub trait Emitter: Send + Sync {
    fn token(&self, _s: &str) {}
    fn activity(&self, _phase: &str, _tool: Option<&str>, _step: usize) {}
    fn tool_call(&self, _tc: &ToolCall) {}
    /// Streamed thinking tokens from the model.
    fn reasoning(&self, _s: &str) {}
    /// Streaming partial output from a running tool (U4). Currently unused until
    /// the Tool trait supports async streaming.
    #[allow(dead_code)]
    fn tool_delta(&self, _id: &str, _text: &str) {}
    fn tool_result(&self, _id: &str, _name: &str, _success: bool, _output: &str) {}
    fn message_end(&self) {}
    fn done(&self, _stopped: bool) {}
    /// Run summary, including real token usage from the provider.
    fn summary(&self, _s: &RunSummary) {}
    fn error(&self, _msg: &str) {}
}
