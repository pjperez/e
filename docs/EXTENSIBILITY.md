# e — extensibility plan: plugins, skills, MCP, remote (minimal core)

**Rule that governs everything: the core of `e` is a thin, stable kernel. Every
extension surface is optional, loaded on demand, and never changes the core.**

```
                e core (Rust) — SMALL, NEVER-GROWING
   engine loop · provider · built-in tools · sessions · event bus · commands
        │  Tauri commands + events (already the contract)
        ▼
              WebView (TS) — SMALL UI + plugin/skill host
```

Extensions plug into the sides — most never touch Rust:

```
Plugins (~/.e/plugins/)   TS modules: tools / events / UI / commands
Skills  (~/.e/skills/)    SKILL.md (agentskills.io standard) injected on demand
MCP     (external)        stdio/SSE servers -> tools merged at runtime
Remote  (headless)        JSONL over stdio / socket; embed in IDEs, agents
```

---

## 1. Design decisions (why this stays minimal)

1. **The Rust core is closed (by default), the edges are open.** Adding a tool
   to the core requires compiling Rust (the `Tool` trait). Almost all user
   extension work happens in TypeScript in the webview or in external
   processes — no recompiles, no new Rust deps in core.
2. **Discovery instead of registration.** The app scans well-known folders
   (`~/.e/…` global, `.e/…` project) at startup; whatever is there is
   available, reloadable. Nothing is "installed" into the app.
3. **The event bus is the contract.** Plugins observe the same
   `e:token / e:tool_* / e:summary / e:activity` events the UI uses, plus their
   own lifecycle hooks. If an event exists in `engine/mod.rs → Emitter`, a
   plugin can listen to it (and in some cases intercept / block it).
4. **Capabilities, not trust.** Extensions are third-party code. Default is a
   per-plugin capability set (e.g. `tools`, `events`, `ui`, `network`,
   `session-read`, `session-write`); risky surfaces are opt-in and surfaced in
   the UI.
5. **On-demand loading.** Nothing is parsed/executed until it is actually
   needed (a skill's SKILL.md is only injected when relevant; a plugin's module
   loads on first use). Startup cost stays ~zero.

---

## 2. The five surfaces

### A. Built-in tools (Rust, compile-time) — exists today
The `Tool` trait + registry in `src-tauri/src/engine/tools.rs`. Stable, tiny,
documented in EXTENDING.md. Not the primary surface for users.

### B. Plugins (TS, runtime, the main surface) — mirrors pi's extension model
- Locations: `~/.e/plugins/<name>/` (global) and `.e/plugins/<name>/`
  (project). Each dir contains `plugin.json` (manifest) + `index.js`/`.ts`.
- Manifest (`plugin.json`):
  ```json
  {
    "name": "git-guard",
    "version": "0.1.0",
    "capabilities": ["tools", "events", "session-read", "ui"],
    "entry": "index.js"
  }
  ```
- Runtime: a small TS host in the frontend loads the module and gives it an `e`
  API object (thin, mirroring what we already expose):
  ```ts
  export default function (e: PluginAPI) {
    e.registerTool({ name, description, parameters, async run(args, ctx) {…} });
    e.on("tool_call", (ev) => { if (dangerous) return { block: true, reason: "…" }; });
    e.on("session_end", (ev) => {…});
    e.registerCommand("/checkpoint", async (ctx) => {…});
    e.ui.notify("…");            // toast
    e.ui.confirm("…");           // modal dialog
    e.setSessionMeta(key, val);  // persisted in the session
    e.fetch(url);                // requires "network" capability
  }
  ```
- Bridging:
  - `list_plugins` / `load_plugin` scan + serve manifests/modules.
  - Plugin tools reach the engine by extending `ToolRegistry` with a proxy tool
    whose `run` calls `plugin_tool_run` (engine loop unchanged).
  - `on(…)`: the backend event bus is mirrored into the frontend via `e:*`
    events; a listener can veto by calling `plugin_veto` which the engine
    consults before executing a tool (an optional no-op `VetoHook` in the tool
    pipeline; default = off, zero cost).

### C. Skills (prompt packages) — the Agent Skills standard
- Locations: `~/.e/skills/`, `~/.agents/skills/` (global); `.e/skills/`
  (project). Each skill is a `SKILL.md` per the agentskills.io spec —
  frontmatter (name, description, license) + workflow/setup body + optional
  `scripts/`.
- How it reaches the model: skills become a `#skills` tool exposed to the
  model (like pi). Invoking a skill injects its SKILL.md into context before
  the next `provider.chat` call (a pre-call hook in `agent.rs`).
- Implementation (small): `list_skills` (scan + frontmatter parse),
  `get_skill(name)`, and an inject step. ~100 lines, no new deps.

### D. MCP servers (external tools) — runtime tool merging
- Config `~/.e/mcp.json`: `{ "servers": { "filesystem": { "command": "npx", "args": ["-y", "@modelcontextprotocol/server-filesystem", "."] } } }`.
- Client: a small Rust module gated behind the `mcp` cargo feature (default
  off), or an `e-mcp` sidecar; implements MCP client (initialize,
  tools/list, tools/call) over stdio (SSE later).
- Merge: listed servers -> `tools/list` -> register as `mcp:<server>/<tool>` in
  the running `ToolRegistry`; calls round-trip via `tools/call`.
- Core stays clean: the protocol code is feature-flagged and separate; the core
  never knows MCP exists when the feature is off.

### E. Remote / headless — drive `e` from anything
- RPC mode (`e --rpc`): headless engine loop, JSONL protocol on stdin/stdout
  (pi's proven framing: LF-only records, `id`-correlated responses, events as
  JSON lines). Enables IDEs, other GUIs, scripts, agents to embed `e`.
- Remote daemon (later): `e --serve :port` exposes the same protocol over a
  socket with a session token; the desktop app can attach to a remote `e`. The
  provider/gateway stays the network path for the LLM itself (already supported
  via `E_BASE_URL`).

---

## 3. Phased roadmap (each phase ships independently; core stays minimal)

| Phase | What | Core cost | User-visible |
|------|------|-----------|--------------|
| **P0 — Plugin foundation** | `~/.e/plugins` discovery + manifest + safe loader + `PluginAPI` (`registerTool`, `on(events)`, `registerCommand`, `ui.confirm/notify`) | +1 command, ~120 lines TS host | drop a folder → it works |
| **P1 — Tools & commands** | plugin tools reach the engine (proxied `ToolRegistry` entry) + veto hook for tool interception; `/commands` from plugins | +2 commands, default no-op veto hook | permission gates, git guards, stateful tools |
| **P2 — Skills** | `~/.e/skills` + `#skills` tool + inject hook | ~100 lines backend | skill packages land like pi |
| **P3 — MCP** | `mcp` cargo feature: stdio client, `mcp.json`, tool merging, reconnect | feature-flagged module | any MCP server's tools appear |
| **P4 — Remote/RPC** | `e --rpc` JSONL headless; later `--serve` socket | new bin/target | embeds in IDEs/tools |
| **P5 — Registry** | `plugin.json` → `e plugin add <repo>` fetches a bundle (folder/tarball) into `~/.e/plugins`; integrity hash + capabilities review screen | small | install from repo, no compile |

**Recommended order: P0 → P2 → P4** (P0 unlocks everything; skills are the
cheapest high-value; RPC enables remote/IDE use). P1 and P3 slot in anytime.

---

## 4. Security & trust model (non-negotiable)

- Plugins are **reviewable folders**, not opaque binaries: show
  name/version/capabilities + the module source in Settings → Plugins before
  enabling anything broader than `events`.
- Capabilities default **deny**: a plugin that only wants `events` never
  silently gains `network` or `session-write`.
- **Skills** are instructions: the model may act on them — surface a
  "reviewed?" hint and agent-skills style warnings like pi.
- **MCP** servers run as subprocesses with their own stdio; token scoping and
  per-server enablement. Never inherit the app's session token.
- No remote access without the session token; RPC/socket binds localhost by
  default.

## 5. What is deliberately NOT built (to stay minimal)

- No heavy plugin runtime (no Node-in-Rust, no QJSEngine): TS plugins run in
  the existing WebView, which is already there.
- No framework for plugins: a plugin is a folder with JSON + a module. If a
  user needs "more", it folds into a bigger plugin, not a bigger core.
- No compile-time plugin SDK in a new language: the `Tool` trait stays the only
  Rust surface; everything else is data-in / events-out.
- The core adds at most ~5 commands and one no-op hook across all phases.

## 6. The first surface in sketch (P0)

```
~/.e/plugins/my-tool/plugin.json      # manifest
~/.e/plugins/my-tool/index.js         # plain JS module
list_plugins -> load_plugin(name) -> eval(index.js) with allowlist API
engine tool loop sees "my-tool/foo" -> ToolRegistry proxy -> plugin_tool_run
```
The plugin author writes:
```js
export default function (e) {
  e.registerTool({
    name: "say-hi", description: "Echo a greeting",
    parameters: { type: "object", properties: { name: { type: "string" } } },
    async run(args) { return "hi " + (args.name || "there"); }
  });
}
```
That is the entire mental model. It composes with everything else — skills and
MCP are also just tools, so there is one pipeline in the engine.

---

## Status — all phases implemented

- **P2 Skills — DONE.** `~/.e/skills` + `~/.agents/skills` + project `.e/skills`
  scanned; `list_skills`/`get_skill` commands; a `#skills` tool returns the
  SKILL.md contents to the model (on-demand injection).
- **P0+P1 Plugins — DONE.** `~/.e/plugins/<name>/plugin.json` + `index.js`
  discovered via `list_plugins`/`get_plugin`; frontend host exposes `PluginAPI`
  (`registerTool`, `on(event)`, `registerCommand("/x")`, `ui.notify`,
  `ui.confirm`); plugin tools reach the engine through a proxy that emits
  `e:plugin_tool_call` and waits on `plugin_tool_result` (timeout-guarded);
  `/commands` from plugins are checked in `runSlash`; toast + confirm UI built.
  (Vetos / `block: true` are reserved for a follow-up: the hook point exists.)
- **P3 MCP — DONE.** `~/.e/mcp.json` servers are spawned (stdio), handshaken
  (initialize), tools discovered via `tools/list` and merged as
  `mcp:<server>/<tool>` into the ToolRegistry; calls round-trip through
  `tools/call`. Verified end-to-end with a live server.
- **P4 Remote/RPC — DONE.** `e-rpc` binary (built) = headless JSONL protocol
  over stdio: `{"type":"send|stop|reset","id":…}` in, `{"type":"event"…}`
  + `{"type":"response"…}` out. Verified streaming reasoning/tokens/summary
  with real usage. `default-run = "e"` keeps `tauri dev` on the GUI.
- **P5 Registry — NOT built** (deliberately deferred): an `e plugin add <repo>`
  installer. Everything else is in place so it's a small add-on.

**Core cost of all phases:** ~8 commands, one feature-flag-free MCP module that
is inert without `mcp.json`, one no-op hook, `pub mod engine`, and
`default-run`. The UI stayed a single small WebView bundle.

## Usage cheatsheet
- **Skill**: create `~/.e/skills/my-skill/SKILL.md` (with `name:`/`description:`
  frontmatter); ask the agent about it — it calls `#skills`.
- **Plugin**: `~/.e/plugins/hello/plugin.json` + `index.js` with
  `export default (e) => { e.registerTool({…}); }`; restart (or reload).
- **MCP**: write `~/.e/mcp.json` with `{ "servers": { "files": { "command": …,
  "args": […] } } }`; tools appear as `mcp:files/…` automatically.
- **RPC**: `target/debug/e-rpc.exe`; echo a JSON command per line; events stream
  out.
