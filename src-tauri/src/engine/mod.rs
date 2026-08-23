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

/// Sentinel prefix on the synthetic `user` message that replaces compacted
/// history. The model still sees a plain user turn; the UI uses the marker to
/// collapse it into a "context compacted" chip instead of dumping the whole
/// summary into the transcript.
pub const COMPACTION_MARKER: &str = "\u{2063}[e:compacted]\n";

/// How many trailing messages survive compaction untouched. Generous on
/// purpose — an over-eager window loses the working context the user cares
/// about, and re-summarising is far more expensive than carrying a few turns.
pub const KEEP_RECENT: usize = 40;

/// Only compact when at least this many messages would actually be collapsed,
/// so we never spend a summarisation request to save a handful of turns.
pub const MIN_COMPACT_GAIN: usize = 16;

/// Share of the context window the kept tail is allowed to occupy. Compaction
/// has to actually free space: keeping a fixed message count can retain almost
/// the whole window when individual messages are large.
const KEEP_SHARE: f64 = 0.4;

/// Never strand the model with less than this much recent conversation, even
/// when the trailing messages are individually enormous.
const KEEP_FLOOR: usize = 6;

/// Rough token estimate. Deliberately cheap — this only has to be good enough
/// to pick a split point; the provider's own count drives the actual trigger.
pub fn est_tokens(m: &Msg) -> usize {
    m.parts
        .iter()
        .map(|p| match p {
            Part::Text(t) | Part::Reasoning(t) => t.len(),
            Part::ImageData(_) => 4 * 1_000,
            Part::ToolCall(tc) => tc.name.len() + tc.arguments.to_string().len(),
            Part::ToolResult { content, .. } => content.len(),
        })
        .sum::<usize>()
        / 4
}

/// Index at which history splits into "summarise this" and "keep verbatim".
///
/// Keeps the largest recent tail that fits both `KEEP_RECENT` messages and
/// `KEEP_SHARE` of the window, then walks back off any `tool` message so the
/// kept slice never begins with a tool result whose originating `tool_calls`
/// message was dropped — providers reject that.
pub fn keep_split(hist: &[Msg], context_window: u64) -> usize {
    let budget = ((context_window as f64) * KEEP_SHARE) as usize;
    let mut used = 0usize;
    let mut kept = 0usize;
    for m in hist.iter().rev() {
        if m.role == "system" {
            continue;
        }
        let t = est_tokens(m);
        if kept >= KEEP_FLOOR && (kept >= KEEP_RECENT || used + t > budget) {
            break;
        }
        used += t;
        kept += 1;
    }
    let mut split = hist.len().saturating_sub(kept);
    while split > 0 && hist[split].role == "tool" {
        split -= 1;
    }
    split
}

impl Msg {
    /// The compaction summary text, if this is a compaction marker message.
    pub fn compaction_summary(&self) -> Option<String> {
        let t = self.plain_text_parts();
        t.strip_prefix(COMPACTION_MARKER).map(|s| s.to_string())
    }

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
    /// Prompt tokens of the *last* provider call, i.e. how full the context
    /// window actually is right now. `tokens_in` is a lifetime sum across every
    /// step and must never be used to reason about context pressure.
    pub context_tokens: u64,
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

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(role: &str, chars: usize) -> Msg {
        Msg::text(role, "x".repeat(chars))
    }

    #[test]
    fn keeps_a_generous_tail_when_messages_are_small() {
        let hist: Vec<Msg> = (0..200).map(|_| msg("user", 40)).collect();
        let split = keep_split(&hist, 1_000_000);
        assert_eq!(hist.len() - split, KEEP_RECENT, "small messages should keep the full message cap");
    }

    #[test]
    fn tail_is_capped_by_the_token_budget_not_the_message_count() {
        // 100k chars = ~25k tokens each; 40 of them would be ~1M tokens, far
        // more than 40% of a 200k window, so far fewer must be kept.
        let hist: Vec<Msg> = (0..80).map(|_| msg("user", 100_000)).collect();
        let split = keep_split(&hist, 200_000);
        let kept = hist.len() - split;
        assert!(kept < KEEP_RECENT, "huge messages must not fill the window: kept {kept}");
        assert!(kept >= KEEP_FLOOR, "must never strand the model: kept {kept}");
    }

    #[test]
    fn never_starts_the_kept_slice_with_an_orphaned_tool_result() {
        let mut hist: Vec<Msg> = vec![msg("user", 10)];
        for _ in 0..60 {
            hist.push(msg("assistant", 10));
            hist.push(Msg::tool_result("1", "shell", "out".into()));
            hist.push(Msg::tool_result("2", "shell", "out".into()));
        }
        let split = keep_split(&hist, 1_000_000);
        assert_ne!(hist[split].role, "tool", "kept slice must not begin with a tool result");
    }

    #[test]
    fn compaction_marker_round_trips_and_plain_messages_are_untouched() {
        let m = Msg::text("user", format!("{COMPACTION_MARKER}a summary"));
        assert_eq!(m.compaction_summary().as_deref(), Some("a summary"));
        assert_eq!(Msg::text("user", "hello").compaction_summary(), None);
    }
}
