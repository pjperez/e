# Extending e

Four surfaces, in order of how often you'll reach for them: **tools**,
**providers**, **skills**, and **plugins**.

## Tools

A tool is a struct implementing the
[`Tool`](../src-tauri/src/engine/tools.rs) trait, registered in
`ToolRegistry::new()`. The [README](../README.md#add-a-tool) has a complete
worked example; this is the reference.

```rust
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn parameters(&self) -> Value;   // JSON Schema for `arguments`
    fn run(&self, ctx: &ToolContext, args: Value) -> ToolResult;
}
```

- `name()` is sanitised to `^[a-zA-Z0-9_-]{1,64}$` before it reaches the model.
- `description()` and `parameters()` are the whole prompt the model gets — a
  vague description is the usual reason a tool never gets called.
- `run()` returns `Result<String, String>`; both arms are fed back into the
  conversation, so an error message is a chance to tell the model how to retry.
- `ctx.dir()` resolves the chat's workspace, refusing empty or relative paths
  rather than silently falling back to the process's current directory.

`run()` is synchronous, on a worker thread. Bound anything long-running
yourself — `ShellTool` shows the `mpsc::recv_timeout` pattern.

### Approval

`shell` and `write_file` prompt the user before running unless YOLO mode is on.
The list is `RISKY` in [`engine/agent.rs`](../src-tauri/src/engine/agent.rs) —
add your tool's name there to require the same confirmation. The prompt
mechanism itself is in
[`engine/approval.rs`](../src-tauri/src/engine/approval.rs); with no GUI host
(headless/RPC) it auto-approves.

## Providers

Any endpoint speaking the OpenAI streaming `/chat/completions` contract works —
add it in Settings (⚙), no code required. `ChatProvider`
([`engine/provider.rs`](../src-tauri/src/engine/provider.rs)) handles SSE
streaming, tool calls, reasoning deltas, usage/cost reporting, and retries a
throttled provider with exponential backoff.

Environment variables override the active provider for one launch:

| env var        | meaning                                                     |
|----------------|-------------------------------------------------------------|
| `E_BASE_URL`   | e.g. `https://api.openai.com/v1`, `http://localhost:11434/v1` |
| `E_API_KEY`    | bearer key (optional for local servers)                      |
| `E_MODEL`      | model id                                                     |
| `E_WORKSPACE`  | working dir for `shell` and relative paths                   |

For a different *protocol*, implement a client returning
[`Completion`](../src-tauri/src/engine/mod.rs) and swap it into
[`Agent`](../src-tauri/src/engine/agent.rs).

## Skills

A skill is a folder with a `SKILL.md` — instructions injected only when
relevant, so they cost nothing until used.

```
~/.e/skills/<name>/SKILL.md          # global
~/.agents/skills/<name>/SKILL.md     # global, shared with other agent tools
<project>/.e/skills/<name>/SKILL.md  # project-local
```

The front matter's `name` and `description` are listed to the model; the body is
loaded when it calls the `skills` tool. See
[`engine/skills.rs`](../src-tauri/src/engine/skills.rs).

## Plugins

A plugin is a drop-in TypeScript folder that can contribute tools without
touching Rust:

```
~/.e/plugins/<name>/            # global
<project>/.e/plugins/<name>/    # project-local
```

The manifest declares tools; calls are dispatched to the webview as
`e:plugin_tool_call` and answered with `plugin_tool_result`. See
[`engine/plugins.rs`](../src-tauri/src/engine/plugins.rs) and
[ARCHITECTURE.md](ARCHITECTURE.md) for the full model.

## MCP

[`engine/mcp.rs`](../src-tauri/src/engine/mcp.rs) merges tools from external
Model Context Protocol servers into the same registry, so they appear to the
agent exactly like built-ins.

## Headless

The `e-rpc` binary drives the same engine over JSONL on stdio — commands in,
events out — for scripts, IDEs, and other agents.

```bash
echo '{"id":1,"type":"send","text":"list the repo"}' | e-rpc
```

Commands are `send`, `reset` and `stop`. Events mirror the `Emitter` trait in
[`engine/mod.rs`](../src-tauri/src/engine/mod.rs): `token`, `reasoning`,
`activity`, `retry`, `tool_call`, `tool_result`, `message_end`, `summary`,
`done`, `error`. Anything you add to `Emitter` is available to the GUI and the
RPC transport at once.
