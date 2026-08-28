<p align="center">
  <img src="design/brand/e-banner.svg" alt="e - agent harness" width="760">
</p>

<h3 align="center">The agent loop, made visible.</h3>

<p align="center">
  <a href="https://github.com/pjperez/e/releases/latest"><img alt="Latest release" src="https://img.shields.io/github/v/release/pjperez/e?style=flat-square&label=release&color=8b5cf6"></a>
  <a href="https://eharness.dev"><img alt="Windows x64 and Arm64" src="https://img.shields.io/badge/Windows-x64%20%7C%20Arm64-8b5cf6?style=flat-square"></a>
  <a href="https://tauri.app"><img alt="Rust and Tauri 2" src="https://img.shields.io/badge/Rust-Tauri%202-8b5cf6?style=flat-square"></a>
</p>

<p align="center">
  Bring an OpenAI-compatible model. Give it a project and real tools.<br>
  Watch it reason, act, and respond from one local Windows app.
</p>

## What is e?

`e` is a desktop agent harness: the part between a model and your machine that
turns a chat response into an actual working loop.

Choose a model from any configured OpenAI-compatible provider, attach the chat
to a project, and ask for work. The model can inspect files, run PowerShell,
write changes, load skills, and call plugin or MCP tools. `e` feeds every result
back to the model and keeps going until the model is finished.

That loop is visible and controllable. Text and reasoning stream as they arrive;
tool calls show their inputs, progress, and results; risky built-in tools pause
for approval; and a run can be stopped or steered without leaving the chat.
Projects, conversations, configuration, and tool execution are managed locally.
Prompts and tool results are sent to the provider you choose.

## At a glance

| Area | Current behavior |
|---|---|
| Agent loop | Streaming chat completions, repeated model-requested tool calls, and cancellation during generation or provider backoff |
| Providers | Multiple saved providers, a combined model picker, `/models` refresh, manually added model IDs, per-model context windows, and reasoning-effort settings |
| Conversations | Persistent named chats, per-chat model/provider selection, forking, search over names and transcript text, background runs, and model-backed context compaction |
| Projects | Chats grouped by project folder, plus a separate scratch `Tasks` area |
| Git isolation | Optional app-managed branch and worktree for each new chat in a Git-backed project, prepared in the background when the chat opens |
| Inputs | Text, `@file` references, and pasted images |
| Visibility | Streamed text and reasoning, tool cards, retry state, run summaries, and token/context/cost counters when the provider reports usage |
| Control | Stop, `Ctrl+Enter` steering, approval prompts for risky built-ins, and optional YOLO auto-approval |
| Extensions | Rust tools, `SKILL.md` packages, WebView plugins, and stdio MCP servers |

## Install

Official releases currently target Windows x64 and Arm64. They are packaged as
per-machine NSIS installers. The bootstrap script runs in Windows PowerShell 5.1,
which is included with Windows 11.

```powershell
irm https://eharness.dev/install.ps1 -OutFile $env:TEMP\e.ps1
powershell.exe -NoProfile -ExecutionPolicy Bypass -File $env:TEMP\e.ps1
```

The bootstrapper selects the installer for the current architecture, verifies a
signed release manifest and the installer's SHA-256 digest, then starts the
installer. Use `-Quiet` for an unattended install.

The installer itself is not yet Authenticode-signed, so Windows reports an
unknown publisher. Release integrity is checked by the signed manifest before
the installer runs.

### Build from source

Requirements:

- Node.js 18 or newer
- Rust stable
- The Windows Tauri prerequisites: MSVC build tools and WebView2

```powershell
npm ci
npm run tauri dev
```

Create a release build with:

```powershell
npm run tauri build
```

The Windows installer is written under
`src-tauri\target\release\bundle\nsis\`. The unpackaged executable is
`src-tauri\target\release\e.exe`.

## First run and providers

`e` ships without a configured provider. Open **Settings** and add:

1. A display name.
2. An OpenAI-compatible base URL, such as
   `https://api.openai.com/v1` or `http://localhost:11434/v1`.
3. A bearer key if the endpoint requires one.

**Refresh** requests `<base_url>/models`. Models missing from that response can
be added manually. Selecting a model also selects the provider that owns it.

When advertised by the provider, `e` records model context windows and supported
reasoning levels. Both can be overridden in Settings. Context compaction uses
the selected model's effective window.

Temporary process-level overrides are available:

| Variable | Meaning |
|---|---|
| `E_BASE_URL` | Provider base URL |
| `E_API_KEY` | Bearer key |
| `E_MODEL` | Model ID |
| `E_WORKSPACE` | Fallback workspace; used directly by `e-rpc` |
| `E_YOLO` | Override approval mode (`true`/`false`, `1`/`0`, `yes`/`no`, or `on`/`off`) |
| `E_SHELL` | Shell executable used by plugin PTY sessions |

Provider requests use the streaming OpenAI `/chat/completions` shape. HTTP 429
and 5xx responses are retried before output starts, using bounded backoff. Other
provider protocols require a different client implementation.

## Built-in tools

| Tool | Behavior |
|---|---|
| `powershell` | Runs Windows PowerShell non-interactively with the chat workspace as its current directory; returns stdout, stderr, and the exit code; times out after 120 seconds |
| `read_file` | Reads a text file and truncates output after 200,000 bytes |
| `write_file` | Replaces a file with supplied text and creates parent directories |
| `list_dir` | Lists one directory |
| `skills` | Lists available skills in its schema and loads one `SKILL.md` on demand |

Relative file paths resolve from the chat's workspace. The file tools also
accept absolute paths, and PowerShell can access anything allowed by the current
Windows user. These tools are **not a sandbox**.

`powershell` and `write_file` require approval before execution. YOLO mode,
available in Settings or through `/yolo`, skips those two prompts. Plugin and MCP
tools do not automatically inherit this approval gate.

## Projects, chats, and worktrees

The built-in `Tasks` project is backed by `%USERPROFILE%\.e\tasks` and is meant
for work that does not belong to a project. A normal project points at an
absolute folder selected by the user.

With `task_worktrees` enabled, creating a chat in a folder inside a Git
repository creates:

- branch `e/<session-id>` from the repository's current `HEAD`
- worktree `%USERPROFILE%\.e\worktrees\<session-id>`

Deleting that chat stops its run, closes its terminals, and removes only the
worktree and branch that `e` created. User-selected project folders are never
deleted. Non-Git projects use their selected folder directly.

Each chat persists its transcript, selected model, provider, and workspace.
Forking copies the transcript into a new chat; in a Git project, the fork gets a
new managed worktree when worktree isolation is enabled. Chats may continue
running while another chat is visible.

As a conversation approaches its configured context window, `e` asks that
chat's own model to summarize older messages and keeps the recent tail intact.
If summarization fails, the original history is retained.

## Composer controls

| Input | Action |
|---|---|
| `Enter` | Send |
| `Shift+Enter` | Insert a newline |
| `Esc` | Stop the visible chat's run |
| `Ctrl+Enter` during a run | Queue the current text, stop the run, then send the queued text |
| `@path` | Read a text file and include it with the message |
| Paste an image | Attach it as image data |

Built-in slash commands:

| Command | Action |
|---|---|
| `/new` | Clear the current chat's transcript |
| `/model` | Open the model picker |
| `/settings` | Open Settings |
| `/extensions` | Open the extension inventory |
| `/reload` | Reload plugins, skills, and MCP servers |
| `/yolo [on\|off]` | Read or change auto-approval |
| `/help` | Show command help |

## Extensions

Extension discovery is global or project-scoped:

| Surface | Global location | Project location | Activation |
|---|---|---|---|
| Skills | `%USERPROFILE%\.e\skills\<name>\SKILL.md` and `%USERPROFILE%\.agents\skills\<name>\SKILL.md` | `<project>\.e\skills\<name>\SKILL.md` | Read fresh when called |
| Plugins | `%USERPROFILE%\.e\plugins\<name>\` | `<project>\.e\plugins\<name>\` | `/reload` |
| MCP | `%USERPROFILE%\.e\mcp.json` | `<project>\.e\mcp.json` | `/reload` |

A project extension with the same name shadows its global counterpart.
**Settings -> Extensions** shows discovered skills, plugins, and MCP servers,
including plugin and MCP load failures.

### Plugins

A plugin is a `plugin.json` manifest and an ES module. It runs inside the
application WebView and may register tools, slash commands, event listeners,
tool-call guards, and side-pane views.

Supported manifest capabilities are:

| Capability | API |
|---|---|
| `tools` | Register model-callable tools |
| `events` | Observe app events and guard tool calls |
| `commands` | Register slash commands |
| `ui` | Show notifications and confirmation dialogs |
| `network` | Use the plugin fetch wrapper |
| `session-read` | Read current chat metadata |
| `views` | Add side-pane tabs |
| `fs` | Read files inside a chat's project through the fenced file API |
| `pty` | Open and control a terminal owned by a chat |

Unknown capabilities prevent the plugin from loading. The host refuses API calls
that were not declared, but the manifest is **not a security sandbox**: plugin
code shares the app's WebView and must be treated as trusted code. The Rust
backend still checks project boundaries for the `fs` API and chat ownership for
PTY operations.

Plugin tools time out after 180 seconds. A `tool_call` guard has five seconds to
block a call; if the guard does not answer, the call proceeds.

See [`examples/plugins`](examples/plugins) and
[`docs/EXTENDING.md`](docs/EXTENDING.md) for the plugin API.

### Skills

A skill is a folder containing `SKILL.md` with `name` and `description` front
matter. The built-in `skills` tool advertises the available names and
descriptions to the model, then loads the selected file only when requested.
Skills are prompt content, not executable code or a security boundary.

### MCP

`mcp.json` defines stdio MCP subprocesses. `e` starts enabled servers, requests
their tool list, and registers the tools as
`mcp_<server-name>_<tool-name>`. MCP servers are independent programs with the
permissions of the current user and should be treated accordingly.

## Configuration and local data

Configuration is stored in `%USERPROFILE%\.e\config.json`. Provider API keys are
stored separately in Windows Credential Manager; plaintext keys left by older
versions are migrated on startup.

Chats and their index are stored under `%USERPROFILE%\.e\sessions`. Managed
worktrees live under `%USERPROFILE%\.e\worktrees`.

The main configuration fields are:

```jsonc
{
  "temperature": 0.7,
  "system": "You are e, a fast, capable coding agent...",
  "model": "gpt-4.1-mini",
  "provider_id": "openai",
  "context_window": 1000000,
  "task_worktrees": true,
  "providers": [
    {
      "id": "openai",
      "name": "OpenAI",
      "base_url": "https://api.openai.com/v1",
      "enabled": true,
      "models": ["gpt-4.1-mini"],
      "context_window": null,
      "model_meta": {},
      "disabled_models": []
    }
  ],
  "disabled_plugins": []
}
```

The top-level `base_url`, `api_key`, and `models` fields are retained for
compatibility and normalized from the selected provider. API keys are removed
from the persisted JSON after migration.

## How the loop is wired

```text
TypeScript UI
  |
  | Tauri commands and events
  v
Rust core
  agent.rs      conversation and tool loop
  provider.rs   OpenAI-compatible HTTP/SSE client
  tools.rs      tool trait, registry, and built-ins
  sessions.rs   projects, chats, and file-backed persistence
  plugins.rs    plugin discovery and tool/guard bridge
  skills.rs     SKILL.md discovery
  mcp.rs        stdio MCP client
  pty.rs        chat-owned pseudo-terminals
```

The frontend receives streamed tokens, reasoning, tool calls, tool results,
retry notices, activity state, summaries, and completion events. Histories are
persisted after user messages, assistant output, and tool results so switching
chats or restarting the application does not discard completed work.

## Headless engine

The source tree also defines `e-rpc`, a JSONL-over-stdio binary using the same
agent, provider, and tool code:

```powershell
cargo build --manifest-path src-tauri\Cargo.toml --bin e-rpc
'{"id":1,"type":"send","text":"list the repository"}' |
  src-tauri\target\debug\e-rpc.exe
```

It accepts `send`, `reset`, and `stop` commands. MCP tools are available.
WebView plugins are not, because the headless process has no plugin host.

## Known limitations

- Official release artifacts are Windows-only.
- Provider integration currently expects the OpenAI-compatible
  `/chat/completions` protocol.
- PowerShell output is returned when the command finishes; it is not streamed
  incrementally.
- Built-in file and command tools are not sandboxed.
- Plugin capability declarations limit the provided API but do not isolate
  plugin code.
- Cost is displayed only when the provider reports it.
- Release installers are not yet Authenticode-signed.

## Development

```powershell
npm run build
cargo test --manifest-path src-tauri\Cargo.toml
```

More detail:

| Document | Contents |
|---|---|
| [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) | Core and extension architecture |
| [`docs/EXTENDING.md`](docs/EXTENDING.md) | Tool, skill, plugin, MCP, and RPC reference |
| [`docs/DESIGN.md`](docs/DESIGN.md) | UI design system |
| [`docs/ROADMAP.md`](docs/ROADMAP.md) | Shipped, planned, and out-of-scope work |
