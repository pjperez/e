# Extensibility: how the edges stay open while the core stays small

**The rule that governs everything: the core of `e` is a thin, stable kernel.
Every extension surface is optional, discovered on disk, and never changes the
core.**

This is the design document. For *how to write one*, see
[EXTENDING.md](EXTENDING.md).

```
                e core (Rust) — SMALL, NEVER-GROWING
   engine loop · provider · built-in tools · sessions · event bus · commands
        │  Tauri commands + events (already the contract)
        ▼
              WebView (TS) — SMALL UI + plugin host
```

Extensions plug into the sides — most never touch Rust:

```
Plugins (~/.e/plugins/)   ES modules: tools / events / commands / UI / guards
Skills  (~/.e/skills/)    SKILL.md (agentskills.io standard), loaded on demand
MCP     (~/.e/mcp.json)   stdio servers -> tools merged at runtime
Remote  (e-rpc)           JSONL over stdio; embed in IDEs, scripts, agents
```

Each of those also has a project form: `<project>/.e/plugins`, `.e/skills`,
`.e/mcp.json`. The project folder is the chat's own workspace, never the
process's current directory — where the app was launched from must never decide
which code runs.

---

## 1. Design decisions (why this stays minimal)

1. **The Rust core is closed by default, the edges are open.** Adding a tool to
   the core means compiling Rust. Almost all extension work is TypeScript in
   the webview or an external process — no recompiles, no new core deps.
2. **Discovery instead of registration.** The app scans well-known folders;
   whatever is there is available and reloadable. Nothing is "installed", so
   nothing has to be uninstalled — and a plugin is a folder you can read, diff
   and delete with ordinary tools.
3. **The event bus is the contract.** Plugins observe the same
   `e:token / e:tool_* / e:summary / e:activity` events the UI uses. If an
   event exists in `engine/mod.rs → Emitter`, a plugin can listen to it, and a
   `tool_call` listener can refuse the call.
4. **Capabilities, not trust.** A manifest asks for what it needs and the host
   hands out exactly that. Refusals are loud, in the UI and in the toast — a
   capability that silently does nothing is worse than one that is refused. An
   unrecognised capability stops the plugin loading, so a typo can never look
   like a grant.
5. **Failure is visible, never silent.** A broken `plugin.json`, a missing
   entry file, an MCP server that would not start, a tool name that collided
   with a built-in: all of it shows in Settings → Extensions with the reason.
   The alternative — a plugin that appears fine while doing nothing — is the
   worst outcome for something you are asked to trust.
6. **Nothing pays for a surface it does not use.** Skills are read only when
   the model asks. MCP is inert without an `mcp.json`. The veto hook is skipped
   entirely until a plugin registers a `tool_call` listener.

---

## 2. The five surfaces

### A. Built-in tools (Rust, compile-time)
The `Tool` trait + registry in `src-tauri/src/engine/tools.rs`. Stable and
tiny: `name`, `description`, `parameters`, `run`, plus an optional
`parameters_for(ctx)` for tools whose schema depends on the project.

### B. Plugins (TS, runtime — the main surface)
`plugin.json` + an ES module. Rust discovers them and proxies tool calls; the
module runs in the webview, which is the only JS runtime the app already has.
A plugin can register tools and slash commands, listen to engine events, show
toasts and confirmations, and refuse tool calls. What it may do is exactly what
its manifest declared.

Tool calls round-trip as `e:plugin_tool_call` → `plugin_tool_result`, guarded
by a timeout so a plugin that never answers fails its call rather than the run.

### C. Skills (prompt packages)
`SKILL.md` with `name`/`description` frontmatter. Every skill the project can
see is enumerated in the `skills` tool's schema — a skill the model cannot see
is a skill it will never use — and its body enters the context only when the
model asks for it by name.

### D. MCP servers (external tools)
A small stdio client: spawn, `initialize`, `tools/list`, `tools/call`. Tools
merge in as `mcp_<server>_<tool>`. Servers start in parallel, each with its own
reader thread and request timeouts, so a slow or wedged server degrades to a
failed tool call instead of a frozen app.

### E. Remote / headless
`e-rpc`: the engine with JSONL on stdin/stdout — `send`, `stop`, `reset` in;
events and responses out. MCP tools are available; plugins are not, because
their modules need the webview.

---

## 3. What is built

| surface | state |
|---------|-------|
| **Plugins** | discovery (global + project), manifest validation, capability enforcement, ES-module loading, per-plugin enable/disable persisted in `~/.e/config.json`, live reload, tool-name collision refusal, `tool_call` veto |
| **Skills** | `~/.e/skills`, `~/.agents/skills`, project `.e/skills`; frontmatter parsing; advertised in the `skills` tool schema; resolved per chat with no reload |
| **MCP** | stdio client with `env`, `cwd`, `disabled`; global + project config; parallel start; per-request timeouts; live status and errors; restart on reload |
| **RPC** | `e-rpc` binary, JSONL protocol, MCP tools loaded before the first command |
| **UI** | Settings → Extensions lists all three surfaces with scope, capabilities, tools, commands and failures; `/extensions` and `/reload` |

**Core cost of all of it:** eleven Tauri commands, one hook that is skipped
unless a plugin uses it, and three self-contained modules (`plugins.rs`,
`skills.rs`, `mcp.rs`) that the engine loop knows nothing about. The engine
still runs one pipeline: everything is a tool.

## 4. What is deliberately not built

- **A plugin registry** (`e plugin add <repo>`). Installing code from a URL
  needs an integrity story and a review screen before it needs a downloader.
  Everything else is in place, so it stays a small addition when it is wanted.
- **A heavy plugin runtime.** No Node in Rust, no embedded JS engine: plugins
  run in the webview that is already there.
- **A plugin framework.** A plugin is a folder with JSON and a module. If that
  is not enough, it wants to be an MCP server, not a bigger core.
- **`session-write`.** Reading the current session is enough for everything
  asked of it so far; letting extensions rewrite history needs a much clearer
  story about what a corrupted transcript costs.
- **Per-project tool registries.** Plugin and MCP tools live in one registry
  shared by every chat, which is why reload is explicit rather than automatic
  on project switch: swapping tools out from under a run in flight would fail
  that run.
- **MCP over SSE/HTTP.** stdio covers local servers, which is what the app is
  for. The transport is one module away when a remote server needs one.

## 5. Trust model

- Plugins are **reviewable folders**, not opaque binaries. Settings →
  Extensions shows name, version, scope, capabilities and every failure before
  you decide to keep one enabled; untick it and the choice persists.
- Capabilities **bound the API, they are not a sandbox**. A plugin is code you
  placed in your own home directory and it runs in the app's webview. The
  honest guarantee is: it gets what it declared, refusals are visible, and
  turning it off is one click.
- **Skills are instructions**, and the model may act on them. Read one as you
  would read a pull request.
- **MCP servers are subprocesses** with their own environment. They never
  inherit app state, each can be parked with `"disabled": true`, and anything
  in `env` is a secret handed to that process.
- **Nothing is fetched from the network** to make any of this work. Every
  surface is a file already on your disk.
