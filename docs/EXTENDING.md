# Extending e

Everything here is a folder you drop somewhere and a **Reload** away from
running. No build step, no install command, no recompile — except for the one
surface that is deliberately Rust.

| surface | what it is | where it lives | language | reload |
|---------|------------|----------------|----------|--------|
| [**Plugins**](#1-plugins) | tools, slash commands, event listeners, guards | `~/.e/plugins/<name>/` | ES module | `/reload` |
| [**Skills**](#2-skills) | prompt packages the model loads on demand | `~/.e/skills/<name>/SKILL.md` | Markdown | live |
| [**MCP servers**](#3-mcp-servers) | tools from an external MCP process | `~/.e/mcp.json` | any | `/reload` |
| [**Built-in tools**](#4-built-in-tools-rust) | tools compiled into the core | `src-tauri/src/engine/tools.rs` | Rust | rebuild |
| [**RPC**](#5-headless--rpc) | drive the engine from another program | `e-rpc` binary | JSONL | — |

Anything global (`~/.e/…`) can also be **project-scoped** (`<project>/.e/…`),
where it applies to that project only and shadows a global one of the same
name. The project folder is the one in the status bar, not wherever the app was
launched from. To share project extensions with a team, commit them — but check
`.gitignore` first: `.e/` is commonly ignored because config lives there too.

Open **Settings (⚙) → Extensions** (or type `/extensions`) to see everything
that was found, what loaded, and what went wrong. Working examples live in
[`examples/`](../examples).

---

## 1. Plugins

A plugin is a folder with a manifest and one ES module:

```
~/.e/plugins/hello/
  plugin.json      # name, version, capabilities
  index.js         # export default function (e) { … }
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

Drop it in, hit **Reload extensions**, and the model has a new tool. That is
the whole mental model.

### Capabilities

The manifest is a request; the host hands out exactly what was asked for and
nothing else. Calling something you did not declare fails loudly — a toast, and
a line under the plugin in Settings → Extensions — instead of silently doing
nothing.

| capability | unlocks |
|------------|---------|
| `tools` | `e.registerTool` |
| `commands` | `e.registerCommand` |
| `events` | `e.on` (including the tool-call veto) |
| `ui` | `e.ui.notify`, `e.ui.confirm` |
| `network` | `e.fetch` |
| `session-read` | `e.session()` |

An unknown capability is an error: the plugin does not load, and the pane says
which word it did not recognise. That way `"net"` never looks like a granted
`"network"`.

Capabilities bound the API, they are not a sandbox. A plugin is code you put in
your own home directory and it runs in the app's webview — read it before you
enable it. Untick any plugin in Settings → Extensions and the choice is
remembered (`disabled_plugins` in `~/.e/config.json`).

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

**Tools.** `parameters` is plain JSON Schema and is sent to the model verbatim.
`run(args)` may be async; return a string, or anything JSON-serialisable.
Throwing marks the call failed and gives the message to the model. Two rules
the engine enforces, reporting anything it refuses:

- a plugin tool may not shadow a built-in (`shell`, `read_file`, `write_file`,
  `list_dir`, `skills`) or another plugin's tool;
- names are `a-z 0-9 _ -` only, because that is what provider APIs accept.

A tool call has 180 seconds to answer before the engine gives up on it.

**Commands.** `e.registerCommand("/checkpoint", fn, "save a checkpoint")` adds
it to the `/` menu in the composer. Built-in command names are refused.

**Events.** The same stream the UI is built on. `e.on("*", …)` sees everything.

| event | payload |
|-------|---------|
| `token` | `{ sid, text }` — streamed assistant text |
| `reasoning` | `{ sid, text }` |
| `tool_call` | `{ sid, id, name, arguments, args }` — `args` is `arguments` already parsed |
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
the call before it runs; the model is told why and carries on. Anything else
lets it through:

```js
e.on("tool_call", (ev) => {
  if (ev.name === "shell" && /rm -rf \//.test(String(ev.args.command || ""))) {
    return { block: true, reason: "refused: that would wipe the disk" };
  }
});
```

The engine waits five seconds at most for an answer and allows the call if none
arrives, so a wedged guard cannot freeze a run. The check runs *before* the
approval prompt, and only when at least one plugin is listening — with no
listeners the hook costs nothing.

### How plugins load

The module is fetched by the backend and imported as a real ES module, so
helpers, top-level constants and normal module syntax all work. It needs a
**default export that is a function**. Loading happens once at startup and on
every reload; a reload drops every tool, command and listener first, so
removing a plugin folder really removes its tools.

Plugins run in the webview, which is why they are the one surface `e --rpc`
does not have.

---

## 2. Skills

A skill is a folder with a `SKILL.md` — the [Agent Skills](https://agentskills.io)
convention, so skills written for other agents work here unchanged.

```
~/.e/skills/commit-style/SKILL.md
~/.agents/skills/<name>/SKILL.md      # shared with your other agents
<project>/.e/skills/<name>/SKILL.md   # this project only
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

`name` and `description` are what the model sees: every skill this project can
reach is listed in the `skills` tool's schema, so the model can pick one by
name instead of guessing. Calling it returns the body of that `SKILL.md`, which
is the only moment any of it enters the context.

Skills are read fresh on every request against the chat's own project folder —
edit a `SKILL.md` and the next message already has it. No reload.

Skills are instructions to a model, not sandboxed code. Read one before you
trust it, exactly as you would a pull request.

---

## 3. MCP servers

Point `e` at any [Model Context Protocol](https://modelcontextprotocol.io)
server and its tools join the registry:

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
`mcp_files_read_file`. Each server starts on its own thread, so a slow one
delays nothing else, and a request that goes unanswered for 60 seconds fails
that tool call instead of hanging the run. Settings → Extensions shows each
server's state, its tools, and the error if it did not start.

Servers restart on reload, not on every project switch: they are shared by
every chat, and pulling tools out from under a run in flight would fail it.
Switch projects, then `/reload` to pick up that project's `.e/mcp.json`.

Servers are subprocesses with their own environment — they never inherit the
app's session state, and anything you put in `env` is a secret you are handing
to that process.

---

## 4. Built-in tools (Rust)

The compiled surface. Use it for something the core should always have; use a
plugin for anything else.

```rust
use crate::engine::tools::{Tool, ToolContext, ToolResult};
use serde_json::{json, Value};

pub struct ShoutTool;
impl Tool for ShoutTool {
    fn name(&self) -> &str { "shout" }
    fn description(&self) -> &str { "Echo a message back in ALL CAPS." }
    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": { "message": { "type": "string", "description": "Text to shout." } },
            "required": ["message"]
        })
    }
    fn run(&self, _ctx: &ToolContext, args: Value) -> ToolResult {
        let msg = args.get("message").and_then(|m| m.as_str()).ok_or("missing 'message'")?;
        Ok(msg.to_uppercase())
    }
}
```

Register it in `ToolRegistry::new()` (`engine/tools.rs`):

```rust
r.register(ShoutTool);
```

`ctx.dir()` is the chat's project folder, already validated — use it instead of
the process's current directory, which is refused on purpose. Anything that can
block needs its own timeout, like `shell`'s 120-second cap.

Tools whose schema depends on the project can override `parameters_for(ctx)`
instead of `parameters()`; that is how `skills` advertises the skills a project
actually has.

---

## 5. Headless / RPC

`e-rpc` is the engine without the window: JSON commands in on stdin, events out
on stdout, one record per line.

```bash
cargo run --bin e-rpc
{"type":"send","id":1,"text":"list the files here"}
{"type":"stop","id":2}
{"type":"reset","id":3}
```

Every line out is either `{"type":"event","event":"token","payload":{…}}` —
the same events plugins see — or `{"type":"response","id":1,"ok":true}`.

MCP servers are started before the first command is answered, so headless runs
have the same tools the GUI does. Plugins are the exception: their modules need
the webview.

Configuration comes from `~/.e/config.json` and the `E_API_KEY`, `E_BASE_URL`,
`E_MODEL` and `E_WORKSPACE` environment variables.

---

## 6. Providers

Any OpenAI-compatible endpoint works — add it in Settings, or point the env
vars at it. See the [README](../README.md#providers-and-models) for how
providers, models and context windows fit together.

To speak a *different* protocol, implement a client returning
[`Completion`](../src-tauri/src/engine/mod.rs) and swap it into
[`Agent`](../src-tauri/src/engine/agent.rs).

---

## Troubleshooting

| symptom | cause |
|---------|-------|
| plugin missing from Settings | no `plugin.json`, or the folder is not directly under `plugins/` |
| "unknown capability" | a typo in `capabilities`; the message lists the valid words |
| "needs the … capability" | the plugin called something its manifest never asked for |
| "no default export" | the entry file must `export default function (e) { … }` |
| "already a built-in tool" | rename the tool; built-ins win |
| tool never called | check it is in the plugin's row in Settings → Extensions, then that its description says *when* to use it |
| skill not offered | no `SKILL.md`, or no `description` in the frontmatter |
| MCP server red | its error is on its row: usually the command is not on `PATH` |
| changed a file, nothing happened | `/reload` (plugins, MCP). Skills need no reload |

Anything you build that reads well as a folder — a manifest, a module, a
Markdown file — is the right shape for this app. If it needs more than that, it
probably wants to be an MCP server.
