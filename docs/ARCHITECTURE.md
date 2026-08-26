# Architecture

`e` is a thin Rust kernel with open edges. The core owns the agent loop, the
provider client, the built-in tools, sessions, and the event bus — and stays
small on purpose. Everything else plugs into the sides, and most of it never
touches Rust.

```
                         e core (Rust)
   agent loop · provider · built-in tools · sessions · event bus · commands
                              │
                   Tauri commands + events
                              ▼
                    WebView (TypeScript)
                  UI + plugin host + commands
```

Four extension surfaces sit around that core:

| surface | lives in | reaches the agent as |
|---------|----------|----------------------|
| **Tools** | `engine/tools.rs` (Rust) | a registry entry |
| **Plugins** | `~/.e/plugins/`, `<project>/.e/plugins/` (TypeScript) | a proxied registry entry |
| **Skills** | `~/.e/skills/`, `~/.agents/skills/`, `<project>/.e/skills/` | prompt text, on demand |
| **MCP servers** | `~/.e/mcp.json` (external processes) | merged registry entries |

They converge deliberately: skills, plugins and MCP servers all end up as tools,
so the engine has exactly one pipeline to reason about.

## Principles

**The core is closed by default; the edges are open.** Adding a built-in tool
means compiling Rust. Everything a user is likely to want — a custom tool, a
command, a prompt package, an external server — happens in TypeScript or in a
separate process, with no recompile.

**Discovery, not installation.** The app scans well-known folders at startup and
on reload. Whatever is there is available. Nothing is registered into a
database, and removing a folder removes the extension.

**The event bus is the contract.** Plugins observe the same events the UI
does. If it exists on the `Emitter` trait in `engine/mod.rs`, a plugin can
listen to it.

**Capabilities are declared and enforced.** A plugin manifest lists what it
wants to touch — `tools`, `commands`, `events`, `ui`, `network`,
`session-read` — and the host hands out exactly that. A call to something the
manifest never asked for is refused and surfaced in Settings → Extensions, and
a capability nobody recognises stops the plugin loading. This bounds the API,
not the runtime: a plugin is still code you have chosen to run.

**Loaded on demand, reloadable.** A skill's `SKILL.md` is read when the model
asks for it; plugin modules and MCP servers reload without a restart
(`/reload`). Startup cost stays near zero.

## Tools

The `Tool` trait and `ToolRegistry` in
[`engine/tools.rs`](../src-tauri/src/engine/tools.rs). The registry is shared
across concurrently running sessions behind a mutex, and lookups clone the
`Arc<dyn Tool>` out before the tool runs, so a slow tool never blocks another
session.

Built-ins: `powershell`, `read_file`, `write_file`, `list_dir`, `skills`. See
[EXTENDING.md](EXTENDING.md) for the trait and a worked example.

## Plugins

A plugin is a reviewable folder — a manifest and a module:

```
~/.e/plugins/hello/plugin.json
~/.e/plugins/hello/index.js
```

```json
{
  "name": "hello",
  "version": "0.1.0",
  "capabilities": ["tools", "events", "ui"],
  "entry": "index.js"
}
```

```js
export default function (e) {
  e.registerTool({
    name: "say_hi",
    description: "Echo a greeting.",
    parameters: { type: "object", properties: { name: { type: "string" } } },
    async run(args) { return "hi " + (args.name || "there"); }
  });
}
```

The module runs in the WebView that is already there — no embedded JS engine, no
second runtime. It is imported as a real ES module, so a plugin is written the
way any module is written. A plugin tool reaches the engine through a registry
proxy that emits `e:plugin_tool_call` and waits for `plugin_tool_result`,
guarded by a timeout, so a wedged plugin cannot hang a run; a tool whose name
would shadow a built-in is refused rather than silently ignored. Plugins can
also contribute `/commands`, raise toasts and confirmations, and refuse a tool
call outright: a `tool_call` listener returning `{ block: true, reason }` stops
it before it runs, and the hook is skipped entirely while no plugin is
listening.

## Skills

A skill is a folder containing a `SKILL.md` — YAML front matter with `name` and
`description`, then the instructions. Only the front matter is shown to the
model — every skill the project can see is enumerated in the `skills` tool's
schema, so it can name one instead of guessing — and the body is loaded when it
calls that tool, so a large skill costs nothing until it is needed.

Skills follow the Agent Skills convention, and `~/.agents/skills/` is scanned
alongside `~/.e/skills/`, so a skill written for another agent works here
unchanged. They are resolved against the chat's own project on every request,
which is why editing one needs no reload.

## MCP

Servers listed in `~/.e/mcp.json` (or a project's `.e/mcp.json`) are spawned as
subprocesses, handshaken, queried with `tools/list`, and merged into the
registry as `mcp_<server>_<tool>`. Calls round-trip through `tools/call`.

```json
{
  "servers": {
    "files": { "command": "npx", "args": ["-y", "@modelcontextprotocol/server-filesystem", "."] }
  }
}
```

Each server starts on its own thread and is read by another, with per-request
timeouts, so one that is slow to start delays nothing else and one that goes
quiet fails a single tool call rather than the run. The module is inert without
an `mcp.json`, so the cost of not using MCP is zero.

## Headless

The `e-rpc` binary runs the same engine with no GUI, speaking JSONL over stdio —
one JSON command per line in, events and responses out. That makes `e` embeddable
in editors, scripts, and other agents. MCP servers start before the first command
is answered, so a headless run has the same tools the GUI does; plugins are the
exception, since their modules need the WebView. See
[EXTENDING.md](EXTENDING.md#headless).

## Security

- **Plugins are folders, not binaries.** The manifest — name, version, requested
  capabilities — and the module source are all plain files you can read before
  running anything, and Settings → Extensions shows them, what each plugin
  registered, and anything that failed.
- **Capabilities are enforced, but they are not a sandbox.** The host hands a
  plugin exactly what its manifest declared and refuses the rest out loud. The
  module still runs in the app's WebView, so a plugin is only as trustworthy as
  its source; unticking it is one click and is remembered.
- **Skills are instructions the model may act on.** They are prompt content, not
  sandboxed code, and deserve the same review as any prompt you would paste in.
- **Risky tools are gated.** `powershell` and `write_file` require explicit approval
  unless YOLO mode is on, and a plugin guard can refuse a call before the prompt
  is even shown.
- **MCP servers are separate processes** with their own stdio, their own `env`,
  and per-server enablement.

## Deliberately not built

- **No embedded plugin runtime.** No Node in Rust, no bundled JS engine —
  plugins run in the WebView the app already has.
- **No plugin framework.** A plugin is a folder with a manifest and a module. If
  that is not enough, it becomes a bigger plugin, not a bigger core.
- **No second SDK language.** The `Tool` trait is the only Rust surface;
  everything else is data in, events out.
- **No plugin registry or installer.** Copying a folder is the install step.
