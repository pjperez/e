# e

**e** is a minimalist, fast, extensible **agent harness** with a native GUI —
think the inner loop of an agent coding tool (converse → plan → fork tools →
apply → repeat) without the terminal. A slim Rust core drives the agent and its
tools; a tiny hand-tuned webview renders the conversation.

Built on **Tauri v2** (Rust + the OS WebView) with a vanilla TypeScript
frontend. No electron, no heavy framework — the whole renderer is ~12 KB of JS.

```
.______________________________.
|  e                      4 tools  ⟂ ⚙  gpt-4.1-mini |
|                — — — — — — — — —             |
|   ▸ you                                        |
|   What can we ship?                            |
|                                                |
|   ▸ e                                          |
|   Reading the repo…                            |
|   [list_dir]  [read_file]  [shell]             |
|   ┌──────────────────────────────────────┐    |
|   │ Ask e to do something…          (➤)   │    |
|   └──────────────────────────────────────┘    |
'______________________________'
```

## Highlights

- **Fast & slim** — native WebView, single small Rust binary, ~20 KB static UI.
- **Beautiful & minimal** — dark, calm, system-typography UI with streaming
  tokens, collapsible tool cards, and a clean composer.
- **A real harness, not a wrapper** — iterative agent loop: the model can call
  tools (shell, read/write files, list dir) and the results feed back until it
  finishes.
- **Extensible** — add a tool by implementing one trait (see
  [docs/EXTENDING.md](docs/EXTENDING.md)). Point it at any OpenAI-compatible
  provider (OpenAI, Ollama, LM Studio, vLLM, Together, your gateway).
- **Private** — your key and config live in `~/.e/config.json`, never in the
  repo. Respects `E_API_KEY` / `E_BASE_URL` / `E_MODEL` / `E_WORKSPACE` env vars.

## Prerequisites

- Node.js ≥ 18 and npm
- Rust (stable) with your platform's Tauri prerequisites:
  <https://tauri.app/start/prerequisites/> (on Windows: WebView2 runtime and the
  MSVC toolchain)

## Run it

```bash
npm install
npm run tauri dev        # dev server + native window, hot reload
```

Configure the model once: click the **gear** in the title bar (or set
`E_API_KEY` etc. before launching).

To build a distributable binary:

```bash
npm run tauri build      # single native executable in src-tauri/target/release/
```

## How it works

```
Tauri frontend (TypeScript)          Rust core (src-tauri/src)
────────────────────────────         ──────────────────────────
  composer ──send_text()──▶            engine::agent::Agent.run()
  streaming tokens ◀─e:token──        engine::provider  (OpenAI-compatible, SSE)
  tool cards     ◀─e:tool_call──      engine::tools     (registry + built-ins)
  tool results   ◀─e:tool_result──    loop: model → run tools → model …
  done           ◀─e:done──
```

The engine owns a `Vec<Msg>` conversation, calls the provider, executes any
requested tools, injects the results, and repeats until the model stops
requesting tools (bounded to 25 steps; cancellable with **Esc**).

### Built-in tools

| tool         | purpose                              |
|--------------|--------------------------------------|
| `shell`      | run a command in the workspace (120 s timeout) |
| `read_file`  | read a text file (truncated if huge) |
| `write_file` | write a file, creating parents        |
| `list_dir`   | list a directory                     |

The **workspace** (where `shell` runs and relative paths resolve) is
configured in Settings and defaults to where you launched `e`.

## Configuration

`e` ships pointed at your **AI Gateway** by default — same base URL,
bearer auth, and models Pi uses. The key is never in the repo: set
`E_API_KEY`, or put it in the local (git-ignored) `~/.e/config.json`.
Portable overrides: `E_BASE_URL`, `E_MODEL`, `E_WORKSPACE`. Settings live in
`~/.e/config.json`:

```jsonc
{
  "base_url": "https://provider.example/v1",
  "api_key": "",             // your gateway key here, or set E_API_KEY
  "model": "opencode-go/deepseek-v4-flash",
  "temperature": 0.7,
  "system": "You are e, a fast, capable agent…",
  "workspace": "C:/src/work",
  "models": [
    "zai-coding/glm-5.2",
    "openrouter/deepseek/deepseek-v4-flash-0731",
    "opencode-go/deepseek-v4-flash",
    "opencode-go/deepseek-v4-pro",
    "openai/gpt-5.6-luna",
    "command-code/inclusionai/ling-3.0-flash-free"
  ]
}
```

The `models` list powers the model picker in **Settings** — type to filter or
pick any of the ported models. Any OpenAI-compatible base URL works: point
`base_url` at `http://localhost:11434/v1` for Ollama or your own gateway and set
a matching `model`.

## Project layout

```
src-tauri/src/
  main.rs            desktop entry point
  lib.rs             Tauri app, commands, event bridge
  engine/
    mod.rs           message/tool-call model + Emitter trait
    provider.rs      OpenAI-compatible streaming client
    tools.rs         Tool trait, registry, built-in tools
    agent.rs         config + the agent loop
src/
  main.ts            UI controller
  api.ts             typed bridge to the Rust backend
  markdown.ts        tiny XSS-safe markdown renderer
  style.css          all styling
```

## Status

A clean, working v0. See [docs/EXTENDING.md](docs/EXTENDING.md) for how to add
tools, and the open ideas there for custom models, sessions, and plugins.

## Extensible
See [docs/EXTENSIBILITY.md](docs/EXTENSIBILITY.md) for the roadmap: user-facing
**plugins** (drop-in TS folders), **skills** (SKILL.md, on demand), **MCP**
servers (feature-flagged client that merges external tools), and **remote/RPC**
(headless JSONL mode) — while the Rust core stays a thin, stable kernel.
