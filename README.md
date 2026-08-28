<p align="center">
  <img src="design/brand/e-banner.svg" alt="e - agent harness" width="760">
</p>

<h3 align="center">A minimal, expandable agent harness.</h3>

<p align="center">
  Your models. Your keys. Your tools.<br>
  A fast native workspace for agents you can see, steer, and extend.
</p>

<p align="center">
  <a href="https://github.com/pjperez/e/releases/latest"><img alt="Latest release" src="https://img.shields.io/github/v/release/pjperez/e?style=flat-square&label=release&color=8b5cf6"></a>
  <a href="https://eharness.dev"><img alt="Windows x64 and Arm64" src="https://img.shields.io/badge/Windows-x64%20%7C%20Arm64-8b5cf6?style=flat-square"></a>
  <a href="https://eharness.dev"><img alt="eharness.dev" src="https://img.shields.io/badge/%E2%86%92-eharness.dev-8b5cf6?style=flat-square"></a>
</p>

<p align="center">
  <a href="#why-e">Why e</a> ·
  <a href="#install">Install</a> ·
  <a href="#extend-e">Extend</a> ·
  <a href="#trust">Trust</a> ·
  <a href="#develop">Develop</a>
</p>

## Why e

`e` keeps the agent workspace focused: connect a model, open a project, and get
to work.

| | |
|---|---|
| **Bring the model you want** | Use OpenAI, Ollama, LM Studio, vLLM, Together, OpenRouter, or any compatible endpoint. Keep several providers and switch models per chat. |
| **Watch the work happen** | Follow responses, reasoning, tool calls, results, and retries as they happen, with session token, context, and reported-cost counters. |
| **Stay in control** | Approve built-in command and file-writing tools, stop a run at any point, or steer it with a new instruction. Other chats can keep working in the background. |
| **Keep projects clean** | Organize persistent chats by project. Fork and search conversations, and optionally give new Git tasks their own managed worktree. |
| **Make it yours** | Add reusable skills, JavaScript plugins, MCP servers, custom tools, commands, guards, file browsers, terminals, and side-pane views. |

No `e` account is required. Add your provider, choose a folder, and start.

## Install

Windows x64 or Arm64, using the built-in Windows PowerShell 5.1:

```powershell
irm https://eharness.dev/install.ps1 -OutFile $env:TEMP\e.ps1
powershell.exe -NoProfile -ExecutionPolicy Bypass -File $env:TEMP\e.ps1
```

The bootstrapper selects the right installer and verifies it against the signed
release manifest before launch.

> The installer is not yet Authenticode-signed, so Windows shows an unknown
> publisher.

## Get started

1. Open **Settings** and add an OpenAI-compatible provider URL and, if needed,
   its key.
2. Refresh its model list or add a model ID manually.
3. Create a project from any folder and choose a model.
4. Ask `e` to investigate, build, fix, review, or automate.

PowerShell, file reading and writing, directory listing, and skill loading are
ready immediately. Press <kbd>Esc</kbd> to stop, <kbd>Ctrl</kbd>+<kbd>Enter</kbd>
to steer, type `@path` to include a file, paste an image, or use `/help` to see
the available commands.

## Extend e

Start small and add only what your workflow needs:

- **Skills** package repeatable instructions in `SKILL.md` and load only when
  needed.
- **Plugins** add tools, commands, safety guards, notifications, project views,
  file browsers, and terminals.
- **MCP servers** bring external tools into the same model-facing tool set.

Extensions can be global or live with a project. Reload plugins and MCP servers
without restarting `e`; skill edits are available on the next turn.

Working examples include a custom tool, Git guard, project browser, terminal,
skill, and MCP configuration:

- [`examples/`](examples)
- [Extension guide](docs/EXTENDING.md)

`e-rpc` provides a headless JSONL interface for editors, automation, and
embedding.

## Trust

- Chats, projects, settings, and extension configuration stay on your machine.
- Provider keys are stored in Windows Credential Manager.
- Prompts and tool results go to the provider selected for that chat.
- Built-in PowerShell and `write_file` calls ask for approval unless YOLO mode
  is enabled.
- Local tools run with your Windows user permissions.
- Plugins and MCP servers are code you choose to run; install only what you
  trust.

## Develop

<details>
<summary><b>Build from source</b></summary>

Requires Node.js 18 or newer, Rust stable, MSVC build tools, and WebView2.

```powershell
npm ci
npm run tauri dev
```

Create a release build:

```powershell
npm run tauri build
```

</details>

<br>

| Documentation | |
|---|---|
| [Extending e](docs/EXTENDING.md) | Tools, skills, plugins, MCP, and `e-rpc` |
| [Architecture](docs/ARCHITECTURE.md) | Core boundaries and extension surfaces |
| [Design](docs/DESIGN.md) | Visual system and interface principles |
| [Roadmap](docs/ROADMAP.md) | Shipped work and what comes next |
