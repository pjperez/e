# Extending e

Five surfaces. Four of them are folders you drop somewhere and a reload away
from running; only the first needs Rust.

| surface | where | reload |
|---------|-------|--------|
| [Tools](#tools) | `engine/tools.rs` (Rust) | rebuild |
| [Providers](#providers) | Settings (⚙) | live |
| [Skills](#skills) | `~/.e/skills/<name>/SKILL.md` | live |
| [Plugins](#plugins) | `~/.e/plugins/<name>/` | `/reload` |
| [MCP](#mcp) | `~/.e/mcp.json` | `/reload` |

Everything global (`~/.e/…`) has a project form (`<project>/.e/…`) that applies
to that project only and shadows a global one of the same name. The project is
the chat's own workspace, never wherever the app was launched from.

**Settings (⚙) → Extensions** — or `/extensions` — lists everything the app
found, what it registered, and why anything failed. Runnable starting points
live in [`examples/`](../examples).

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

    // Optional: schema that depends on the project. `skills` uses it to list
    // the skill packages this workspace actually has.
    fn parameters_for(&self, ctx: &ToolContext) -> Value { self.parameters() }
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
throttled provider on an escalating schedule (1s, 15s, 30s, 60s) so a
per-minute rate limit is waited out rather than expired inside.

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

A skill is a folder with a `SKILL.md` — instructions loaded only when the model
asks for them, so they cost nothing until used.

```
~/.e/skills/<name>/SKILL.md          # global
~/.agents/skills/<name>/SKILL.md     # global, shared with other agent tools
<project>/.e/skills/<name>/SKILL.md  # project-local
```

```markdown
---
name: Commit style
description: How this project writes commit messages. Load before writing a commit.
---

# Commit style

1. Read the staged diff before writing anything.
…
```

The front matter's `name` and `description` are what the model sees: every
skill the project can reach is enumerated in the `skills` tool's schema, so it
can name one instead of guessing. Calling the tool returns the body, which is
the only moment any of it enters the context.

Skills are read fresh against the chat's own project on every request — edit a
`SKILL.md` and the next message already has it. No reload. They are
instructions a model may act on, not sandboxed code, so read one as you would
read a pull request. See [`engine/skills.rs`](../src-tauri/src/engine/skills.rs).

## Plugins

A plugin is a folder with a manifest and one ES module, and it can contribute
tools, slash commands, event listeners and guards without touching Rust.

```
~/.e/plugins/hello/plugin.json       # global
<project>/.e/plugins/hello/index.js  # project-local
```

```jsonc
{
  "name": "Hello",                 // display name; the folder name is the id
  "version": "0.1.0",
  "description": "What it does.",  // shown in Settings → Extensions
  "capabilities": ["tools", "ui"], // everything it is allowed to touch
  "entry": "index.js"              // optional, defaults to index.js
}
```

```js
export default function (e) {
  e.registerTool({
    name: "say_hi",
    description: "Greet someone by name.",
    parameters: { type: "object", properties: { name: { type: "string" } } },
    async run(args) { return "hi " + (args.name || "there"); },
  });
}
```

### Capabilities

The manifest is a request; the host hands out exactly what was asked for and
nothing else. Calling something you did not declare fails loudly — a toast, and
a line under the plugin in Settings → Extensions — instead of silently doing
nothing.

| capability | unlocks |
|------------|---------|
| `tools` | `e.registerTool` |
| `commands` | `e.registerCommand` |
| `events` | `e.on`, including the tool-call guard |
| `ui` | `e.ui.notify`, `e.ui.confirm` |
| `network` | `e.fetch` |
| `session-read` | `e.session()` |

An unknown capability stops the plugin loading, and the pane says which word it
did not recognise — that way `"net"` never looks like a granted `"network"`.

Capabilities bound the API; they are not a sandbox. A plugin is code you put in
your own home directory and it runs in the app's WebView, so read it before you
enable it. Untick one in Settings → Extensions and the choice persists
(`disabled_plugins` in `~/.e/config.json`).

### The `e` API

```ts
e.name                      // this plugin's folder name

e.registerTool({ name, description, parameters, run })   // needs "tools"
e.registerCommand("/name", fn, "description")            // needs "commands"
e.on(event, handler)                                     // needs "events"
e.ui.notify(message, kind?)                              // needs "ui"
await e.ui.confirm(message) -> boolean                   // needs "ui"
await e.fetch(url, init?) -> Response                    // needs "network"
e.session() -> { id, name, workspace, model, provider }  // needs "session-read"
e.log(...args)                                           // always
```

**Tools.** `parameters` is JSON Schema, sent to the model verbatim. `run(args)`
may be async; return a string or anything JSON-serialisable, and throwing marks
the call failed with the message handed to the model. A plugin tool may not
shadow a built-in or another plugin's tool — the engine refuses it and says so
rather than putting the same name in the schema twice. Calls have 180 seconds
to answer.

**Events.** The same stream the UI is built on; `e.on("*", …)` sees everything.

| event | payload |
|-------|---------|
| `token` | `{ sid, text }` — streamed assistant text |
| `reasoning` | `{ sid, text }` |
| `tool_call` | `{ sid, id, name, arguments, args }` — `args` is `arguments` parsed |
| `tool_result` | `{ sid, id, name, success, output }` |
| `message_end` | `{ sid }` |
| `activity` | `{ sid, phase, tool, step }` |
| `retry` | `{ sid, attempt, max, delayMs, status, reason }` |
| `summary` | `{ sid, steps, tools, stopped, tokensIn, tokensOut, contextTokens, cost, error }` |
| `done` | `{ sid, stopped }` |
| `error` | `{ sid, message }` |

`sid` is the chat the event belongs to — a background chat streams while you
look at another one, so use it rather than assuming "the current chat".

**Guards.** A `tool_call` handler that returns `{ block: true, reason }` stops
the call before it runs; the model is told why and carries on.

```js
e.on("tool_call", (ev) => {
  if (ev.name === "shell" && /rm -rf \//.test(String(ev.args.command || ""))) {
    return { block: true, reason: "refused: that would wipe the disk" };
  }
});
```

The engine waits five seconds at most and allows the call if no answer arrives,
so a wedged guard cannot freeze a run. The check runs *before* the approval
prompt, and only when at least one plugin is listening — with no listeners the
hook costs nothing.

**Loading.** The module is imported as a real ES module, so helpers, top-level
constants and normal module syntax work; it needs a default export that is a
function. A reload drops every tool, command and listener first, so removing a
folder really removes its tools. Plugins run in the WebView, which is why they
are the one surface `e --rpc` does not have. See
[`engine/plugins.rs`](../src-tauri/src/engine/plugins.rs).

## MCP

[`engine/mcp.rs`](../src-tauri/src/engine/mcp.rs) merges tools from external
[Model Context Protocol](https://modelcontextprotocol.io) servers into the same
registry, so they reach the agent exactly like built-ins.

```jsonc
{
  "servers": {
    "files": {
      "command": "npx",
      "args": ["-y", "@modelcontextprotocol/server-filesystem", "."]
    },
    "github": {
      "command": "npx",
      "args": ["-y", "@modelcontextprotocol/server-github"],
      "env": { "GITHUB_PERSONAL_ACCESS_TOKEN": "…" }
    },
    "parked": { "command": "npx", "args": ["-y", "some-server"], "disabled": true }
  }
}
```

| field | meaning |
|-------|---------|
| `command`, `args` | the process to run (stdio transport) |
| `env` | extra environment variables for that process |
| `cwd` | working directory; defaults to the project folder |
| `disabled` / `enabled` | park a server without deleting its config |

Tools arrive as `mcp_<server>_<tool>` — `files` + `read_file` becomes
`mcp_files_read_file`. Servers start in parallel, so a slow one delays nothing
else, and a request left unanswered for 60 seconds fails that tool call instead
of hanging the run. Settings → Extensions shows each server's state, its tools,
and the error if it did not start.

Servers restart on reload, not on every project switch: they are shared by
every chat, and pulling tools out from under a run in flight would fail it.
Switch projects, then `/reload`.

Servers are subprocesses with their own environment — they never inherit app
state, and anything in `env` is a secret you are handing to that process.

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

MCP servers are started before the first command is answered, so headless runs
have the same tools the GUI does.

## Troubleshooting

| symptom | cause |
|---------|-------|
| plugin missing from Settings | no `plugin.json`, or the folder is not directly under `plugins/` |
| "unknown capability" | a typo in `capabilities`; the message lists the valid words |
| "needs the … capability" | the plugin called something its manifest never asked for |
| "no default export" | the entry file must `export default function (e) { … }` |
| "already a built-in tool" | rename the tool; built-ins win |
| tool never called | check the plugin's row in Settings → Extensions, then that the description says *when* to use it |
| skill not offered | no `SKILL.md`, or no `description` in the front matter |
| MCP server red | its error is on its row: usually the command is not on `PATH` |
| changed a file, nothing happened | `/reload` (plugins, MCP). Skills need no reload |

