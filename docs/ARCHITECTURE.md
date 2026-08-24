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

**Discovery, not installation.** The app scans well-known folders at startup.
Whatever is there is available. Nothing is registered into a database, and
removing a folder removes the extension.

**The event bus is the contract.** Plugins observe the same events the UI
does. If it exists on the `Emitter` trait in `engine/mod.rs`, a plugin can
listen to it.

**Capabilities are declared, not enforced.** A plugin manifest lists the
capabilities it wants, and those are visible before you run anything — but
nothing currently stops a plugin from doing more. Treat a plugin as code you
have chosen to run.

**Loaded on demand.** A skill's `SKILL.md` is read when the model asks for it; a
plugin module loads on first use. Startup cost stays near zero.

## Tools

The `Tool` trait and `ToolRegistry` in
[`engine/tools.rs`](../src-tauri/src/engine/tools.rs). The registry is shared
across concurrently running sessions behind a mutex, and lookups clone the
`Arc<dyn Tool>` out before the tool runs, so a slow tool never blocks another
session.

Built-ins: `shell`, `read_file`, `write_file`, `list_dir`, `skills`. See
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
second runtime. A plugin tool reaches the engine through a registry proxy that
emits `e:plugin_tool_call` and waits for `plugin_tool_result`, guarded by a
timeout, so a wedged plugin cannot hang a run. Plugins can also contribute
`/commands` and raise toasts and confirmations.

## Skills

A skill is a folder containing a `SKILL.md` — YAML front matter with `name` and
`description`, then the instructions. Only the front matter is shown to the
model; the body is loaded when it calls the `skills` tool, so a large skill
costs nothing until it is needed.

Skills follow the Agent Skills convention, and `~/.agents/skills/` is scanned
alongside `~/.e/skills/`, so a skill written for another agent works here
unchanged.

## MCP

Servers listed in `~/.e/mcp.json` are spawned as subprocesses, handshaken,
queried with `tools/list`, and merged into the registry as
`mcp:<server>/<tool>`. Calls round-trip through `tools/call`.

```json
{
  "servers": {
    "files": { "command": "npx", "args": ["-y", "@modelcontextprotocol/server-filesystem", "."] }
  }
}
```

The module is inert without an `mcp.json`, so the cost of not using MCP is zero.

## Headless

The `e-rpc` binary runs the same engine with no GUI, speaking JSONL over stdio —
one JSON command per line in, events and responses out. That makes `e` embeddable
in editors, scripts, and other agents. See [EXTENDING.md](EXTENDING.md#headless).

## Security

- **Plugins are folders, not binaries.** The manifest — name, version, requested
  capabilities — and the module source are all plain files you can read before
  running anything.
- **Requested capabilities are informational today.** They are declared in the
  manifest and can be inspected, but they are not yet enforced at runtime, so a
  plugin is only as trustworthy as its source.
- **Skills are instructions the model may act on.** They are prompt content, not
  sandboxed code, and deserve the same review as any prompt you would paste in.
- **Risky tools are gated.** `shell` and `write_file` require explicit approval
  unless YOLO mode is on.
- **MCP servers are separate processes** with their own stdio and per-server
  enablement.

## Deliberately not built

- **No embedded plugin runtime.** No Node in Rust, no bundled JS engine —
  plugins run in the WebView the app already has.
- **No plugin framework.** A plugin is a folder with a manifest and a module. If
  that is not enough, it becomes a bigger plugin, not a bigger core.
- **No second SDK language.** The `Tool` trait is the only Rust surface;
  everything else is data in, events out.
- **No plugin registry or installer.** Copying a folder is the install step.
