<p align="center">
  <img src="design/brand/e-banner.svg" alt="e — agent harness" width="760">
</p>

<h3 align="center">The agent loop, made visible.</h3>

<p align="center">
  A minimalist, fast, extensible <b>agent harness</b> with a native GUI.<br>
  A Rust core drives your model and runs its tools on your machine;<br>
  a hand-written ~20&nbsp;KB webview shows every step as it happens.
</p>

<p align="center">
  <a href="https://github.com/pjperez/e/releases/latest"><img alt="Latest release" src="https://img.shields.io/github/v/release/pjperez/e?style=flat-square&label=release&color=8b5cf6"></a>
  <a href="https://eharness.dev"><img alt="Windows x64 and Arm64" src="https://img.shields.io/badge/Windows-x64%20%7C%20Arm64-8b5cf6?style=flat-square"></a>
  <a href="https://tauri.app"><img alt="Rust and Tauri 2" src="https://img.shields.io/badge/Rust-Tauri%202-8b5cf6?style=flat-square"></a>
  <a href="https://eharness.dev"><img alt="eharness.dev" src="https://img.shields.io/badge/%E2%86%92-eharness.dev-8b5cf6?style=flat-square"></a>
</p>

<p align="center">
  <a href="#install">Install</a> ·
  <a href="#first-run">First run</a> ·
  <a href="#built-in-tools">Tools</a> ·
  <a href="#add-a-tool">Add a tool</a> ·
  <a href="#extensions">Extensions</a> ·
  <a href="#how-it-works">How it works</a> ·
  <a href="#configuration">Configuration</a>
</p>

```
┌────────────────────────────────────────────────────────────────┐
│  e   ~/src/acme  ·  main             gpt-4.1-mini  ·  5 tools  │
├────────────────────────────────────────────────────────────────┤
│                                                                │
│  you                                                           │
│  Cut the 0.2 release notes from the log since 0.1.             │
│                                                                │
│  e                                                             │
│  Reading the repository...                                     │
│                                                                │
│    [ list_dir ]      12 entries                                │
│    [ powershell ]    git log 0.1..HEAD   exit 0                │
│    [ write_file ]    CHANGELOG.md        +38 lines             │
│                                                                │
│  Done. 4 steps, 3.2 s, 1,284 tokens, $0.004.                   │
│                                                                │
├────────────────────────────────────────────────────────────────┤
│  Ask e to do something...                 Esc stops  [ Send ]  │
└────────────────────────────────────────────────────────────────┘
```

`e` is the inner loop of an agent coding tool — converse, call tools, apply,
repeat — without the terminal. It is built on **Tauri 2**: a Rust core and the
operating system's own WebView, with a vanilla TypeScript frontend. No Electron,
no framework.

## Why e

|  | |
|---|---|
| **A harness, not a wrapper** | The model calls real tools — PowerShell, read, write, list, skills — and reads their real output until the task is done. Bounded to 25 steps, cancellable with <kbd>Esc</kbd>. |
| **Your models, your keys** | OpenAI, Ollama, LM Studio, vLLM, Together, OpenRouter or your own gateway. Keep several configured and switch per chat. |
| **Chats and projects that hold** | Conversations persist and fork, each carrying its own model, workspace and token budget. History summarises itself as a chat approaches the context window. |
| **Tasks stay isolated** | Git projects get an app-managed worktree per task by default, checked out the first time the task runs rather than when you open it. Delete the task and the worktree goes with it. |
| **Nothing is hidden** | Tokens, reasoning, tool cards and live token and cost counters all stream as the run happens. |
| **Yours** | Settings live in `~/.e/config.json`. API keys go to Windows Credential Manager, never into the repo. |

## Install

Windows releases need no Node.js, Rust or build toolchain. Run this from
PowerShell:

```powershell
irm https://eharness.dev/install.ps1 -OutFile $env:TEMP\e.ps1; pwsh -NoProfile -File $env:TEMP\e.ps1
```

The bootstrapper picks the x64 or Arm64 build, checks its SHA-256 against a
separately signed release manifest, and runs the installer. `e` installs to
`C:\Program Files\e`, so Windows shows the usual approval prompt. Pass `-Quiet`
for an unattended install.

> Releases are not yet Authenticode-signed, so Windows will name the publisher
> as unknown. The signed manifest is what establishes that the download is the
> artifact this repository built.

<details>
<summary><b>Build from source instead</b></summary>

<br>

Requires Node.js ≥ 18, Rust (stable) and your platform's
[Tauri prerequisites](https://tauri.app/start/prerequisites/) — on Windows the
WebView2 runtime and the MSVC toolchain.

```bash
npm install
npm run tauri dev        # dev server + native window, hot reload
npm run tauri build      # single native executable in src-tauri/target/release/
```

</details>

## First run

`e` ships with **no provider configured**. Open **Settings (⚙)** and add one:

1. **+ Add provider** — a name, a base URL (`https://api.openai.com/v1`, or
   `http://localhost:11434/v1` for Ollama) and a key if it needs one.
2. **Refresh** pulls the model list from `<base_url>/models`. Gateways that
   don't advertise everything they serve can have model ids added by hand.
3. Pick a model from the title bar. A model carries its provider with it, so
   choosing one selects the base URL, key and context window too.

Environment variables override the active provider for a single launch:
`E_BASE_URL`, `E_API_KEY`, `E_MODEL`, `E_WORKSPACE`.

## Built-in tools

| tool | purpose |
|------|---------|
| `powershell` | run PowerShell in the workspace (120 s timeout) |
| `read_file` | read a text file (truncated if huge) |
| `write_file` | write a file, creating parents |
| `list_dir` | list a directory |
| `skills` | load a `SKILL.md` on demand |

The **workspace** — where `powershell` runs and relative paths resolve — belongs
to the chat's project and is set in the sidebar (✎).

## Add a tool

A tool is a struct implementing one trait. Here is a complete, working one:

```rust
// src-tauri/src/engine/tools.rs
use serde_json::{json, Value};

pub struct GitStatusTool;

impl Tool for GitStatusTool {
    fn name(&self) -> &str {
        "git_status"
    }

    fn description(&self) -> &str {
        "Show the working tree status of the workspace's git repository."
    }

    /// JSON Schema for `arguments`. This is what the model is shown.
    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "short": { "type": "boolean", "description": "Use --short output." }
            }
        })
    }

    fn run(&self, ctx: &ToolContext, args: Value) -> ToolResult {
        let short = args.get("short").and_then(|s| s.as_bool()).unwrap_or(true);

        let mut cmd = std::process::Command::new("git");
        cmd.current_dir(ctx.dir()?).arg("status");
        if short {
            cmd.arg("--short");
        }

        let out = cmd.output().map_err(|e| format!("git: {e}"))?;
        if !out.status.success() {
            return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
        }
        Ok(String::from_utf8_lossy(&out.stdout).to_string())
    }
}
```

Register it in `ToolRegistry::new()`:

```rust
r.register(GitStatusTool);
```

That is the whole surface. `parameters()` is sent to the model, `run()` receives
whatever the model passed, and the returned `Ok`/`Err` string is fed back into
the conversation — the agent loop, the tool card in the UI and the approval
prompt all work automatically.

Two things worth knowing:

- `ctx.dir()?` resolves the chat's workspace and returns a readable error if it
  is unset or missing, so a tool never runs somewhere unexpected.
- `run()` is synchronous and runs on a worker thread. For anything long-running,
  apply your own timeout — the `powershell` tool uses `mpsc::recv_timeout`.

## Extensions

Three drop-in folders. No rebuild, no restart — `/reload`, or
**Settings (⚙) → Extensions → Reload extensions**:

| drop this in | and you get |
|---|---|
| `~/.e/plugins/<name>/plugin.json` + `index.js` | tools, `/commands`, event guards |
| `~/.e/skills/<name>/SKILL.md` | prompt packages, loaded on demand |
| `~/.e/mcp.json` | MCP servers; their tools merge in |

Each has a project-scoped form — `<project>/.e/plugins`, `.e/skills`,
`.e/mcp.json` — that applies to that project only and shadows a global one of
the same name.

```js
// ~/.e/plugins/hello/index.js
export default function (e) {
  e.registerTool({
    name: "say_hi",
    description: "Greet someone by name.",
    parameters: { type: "object", properties: { name: { type: "string" } } },
    async run(args) { return "hi " + (args.name || "there"); },
  });
}
```

A plugin declares what it may touch — `tools`, `commands`, `events`, `ui`,
`network`, `session-read` — and gets exactly that. Anything it did not declare
is refused out loud. A plugin listening for `tool_call` can also refuse a call
before it runs, which is how a guard stops `git push --force` reaching the
PowerShell tool.

**Settings (⚙) → Extensions**, or `/extensions`, lists everything found: scope,
capabilities, the tools and commands each plugin registered, MCP server state,
and why anything failed. Untick a plugin to keep it off for good.

Copy-paste starting points live in [`examples/`](examples); the full reference is
[EXTENDING.md](docs/EXTENDING.md).

## How it works

```
frontend (TypeScript)                    Rust core (src-tauri/src)
─────────────────────                    ─────────────────────────
composer          --send_text()------->  engine::agent      the loop
tokens            <--e:token-----------  engine::provider   SSE, OpenAI-compatible
tool cards        <--e:tool_call-------  engine::tools      registry + built-ins
tool results      <--e:tool_result-----  engine::sessions   chats and projects
done              <--e:done------------  engine::approval   human gate for risky tools
```

The engine owns a `Vec<Msg>` conversation, calls the provider, executes any
requested tools, injects the results, and repeats until the model stops asking
for tools.

```
src-tauri/src/
  main.rs            desktop entry point
  lib.rs             Tauri app, commands, event bridge
  bin/e-rpc.rs       the same engine, headless over JSONL
  engine/
    mod.rs           message/tool-call model + Emitter trait
    provider.rs      OpenAI-compatible streaming client
    tools.rs         Tool trait, registry, built-in tools
    agent.rs         config + the agent loop
    sessions.rs      chats, projects, persistence
    approval.rs      human approval for risky tools
    skills.rs        SKILL.md discovery
    mcp.rs           MCP client
    plugins.rs       plugin discovery + the bridge to the host
src/
  main.ts            UI controller + plugin host
  api.ts             typed bridge to the Rust backend
  markdown.ts        tiny XSS-safe markdown renderer
  copy.ts            copy-to-clipboard buttons
  style.css          all styling
```

## Configuration

Settings live in `~/.e/config.json` and are written by the GUI — edit by hand
only if you want to.

```jsonc
{
  "temperature": 0.7,
  "system": "You are e, a fast, capable agent…",
  "workspace": "C:/src/work",
  "model": "gpt-4.1-mini",     // the picked model…
  "provider_id": "openai",     // …and who serves it
  "providers": [
    {
      "id": "openai",
      "name": "OpenAI",
      "base_url": "https://api.openai.com/v1",
      "enabled": true,            // off hides its models but keeps the key
      "context_window": null,     // provider-wide fallback; null = global
      "models": ["gpt-4.1-mini", "gpt-4.1"],
      "model_meta": {             // per model: learned on Refresh, tuned by you
        "gpt-4.1": {
          "advertised_window": 272000,   // what /models said
          "window_override": null,       // your number; beats the above
          "reasoning": true,             // takes a reasoning level
          "reasoning_efforts": ["low", "medium", "high"],
          "reasoning_effort": "high"     // the level to ask for
        }
      },
      "disabled_models": ["gpt-4.1"]     // hidden from the picker
    },
    {
      "id": "ollama",
      "name": "Ollama",
      "base_url": "http://localhost:11434/v1",
      "enabled": true,
      "models": ["qwen3-coder:30b"],
      "disabled_models": []
    }
  ],
  "task_worktrees": true,               // isolate each new Git task by default
  "disabled_plugins": ["noisy-plugin"]  // unticked in Settings → Extensions
}
```

Top-level `base_url` and `models` are derived — the connection `e` uses is always
the one belonging to the provider serving the selected model. API keys never go
in this file: on Windows they are stored in Windows Credential Manager, and any
existing plaintext keys are migrated out of `config.json` on startup.

**Context window** resolves per model, then the provider's fallback, then the
global default; it is what compaction is budgeted against. **Reasoning level**
is `auto · min · low · med · high`, or exactly the levels the provider
enumerated — `auto` sends no `reasoning_effort` field at all.

## Documentation

| doc | what's in it |
|-----|--------------|
| [EXTENDING.md](docs/EXTENDING.md) | adding tools, providers, skills, plugins, MCP servers; the headless RPC binary |
| [ARCHITECTURE.md](docs/ARCHITECTURE.md) | how the core and its extension surfaces fit together |
| [DESIGN.md](docs/DESIGN.md) | design tokens, themes, layout and typography |
| [ROADMAP.md](docs/ROADMAP.md) | what's shipped, what's planned, what's deliberately out of scope |

## Brand

<p align="center">
  <img src="design/brand/e-construction.svg" alt="the mark's construction" width="440">
</p>

The mark is a lowercase **e** whose bowl is a true logarithmic spiral,
`r(θ) = r₀·e^(bθ)`, tuned so the radius grows by exactly φ across the sweep. It
is generated, not drawn: [`design/logo.py`](design/logo.py) emits every asset
from the same equations.

```bash
python design/logo.py            # writes design/brand/* and public/e.svg
python design/logo.py --review   # contact sheet of the alternate cuts
npm run tauri -- icon design/brand/e-tile.svg   # platform icon set
```

`public/e.svg` is the single source of truth for the in-app mark: the title bar
and empty state tint it with a CSS mask, so it follows the theme for free.
