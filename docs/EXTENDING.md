# Extending e

`e` is designed around two clean extension surfaces: **tools** (things the agent
can do) and **providers** (where the model comes from).

## 1. Add a tool (the main one)

A tool is just a struct that implements the [`Tool`](../src-tauri/src/engine/tools.rs) trait:

```rust
use crate::engine::tools::{Tool, ToolContext, ToolResult};
use serde_json::{json, Value};

pub struct ShoutTool;
impl Tool for ShoutTool {
    fn name(&self) -> &str { "shout" }
    fn description(&self) -> &str {
        "Echo a message back in ALL CAPS."
    }
    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "message": { "type": "string", "description": "Text to shout." }
            },
            "required": ["message"]
        })
    }
    fn run(&self, _ctx: &ToolContext, args: Value) -> ToolResult {
        let msg = args.get("message")
            .and_then(|m| m.as_str())
            .ok_or("missing 'message'")?
            .to_uppercase();
        Ok(msg)
    }
}
```

Then register it in `ToolRegistry::new()` (`engine/tools.rs`):

```rust
r.register(ShoutTool);   // or any tool you write
r.register(MyHttpTool);
```

That's it. The tool's `parameters()` JSON-Schema is sent to the model, its
`run()` is invoked with the model's arguments, and the result is fed back into
the conversation — the agent loop and UI tool cards just work.

> **Tip:** implement `std::sync::mpsc` + a timeout inside `run()` (like the
> built-in `shell` tool) for anything long-running or blocking.

### Where to put your own tools

Keep the core in `engine/tools.rs` for built-ins, or create
`engine/plugins/mod.rs` and register them from `ToolRegistry::new()`. For a
plugin, expose a simple `pub fn register_all(registry: &mut ToolRegistry)` and
call it from `new()`.

## 2. Change the provider / model

Any OpenAI-compatible endpoint works. The `ChatProvider`
(`engine/provider.rs`) speaks the streaming `/chat/completions` contract with
tool calling. Configure it in the GUI (gear icon) or via env:

| env var        | meaning                     |
|----------------|-----------------------------|
| `E_BASE_URL`   | e.g. `https://api.openai.com/v1`, or `http://localhost:11434/v1` (Ollama) |
| `E_API_KEY`    | bearer key (optional for local servers) |
| `E_MODEL`      | model id                     |
| `E_WORKSPACE`  | working dir for `shell` etc. |

To add a *different* provider protocol, implement a second client that returns
[`Completion`](../src-tauri/src/engine/mod.rs) and swap it into
[`Agent`](../src-tauri/src/engine/agent.rs).

## 3. Customize behavior

- **System prompt** — edit in Settings. The agent seeds each new session with it.
- **Max steps** — `MAX_STEPS` in `engine/agent.rs` bounds the agent loop.
- **Workspace** — change where `shell` / relative file tools operate.

## Ideas (not yet built)

- **Sessions** — persist conversations and resume / fork them.
- **Declarative tools** — define a tool in a JSON manifest (name, schema, and
  an HTTP URL or command) with no recompile.
- **Custom events** — subscribe to `e:token`, `e:tool_*` from the UI to add
  panels (costs, plan preview, git diff).
- **Model picker** — query the server's `/models` and list them in Settings.
- **Streaming to plugins** — expose raw deltas and tool logs on a local event
  bus so external tools can react.

Most of these reduce to: add a command in `lib.rs`, emit an event, render a
panel in `main.ts`.
