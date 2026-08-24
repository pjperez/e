# Roadmap

A terminal gives you raw output but no situational awareness. `e` is built on
the bet that an agent's inner loop becomes genuinely useful when it is
**legible, controllable, and continuous** — you can see what it is doing, steer
it, and keep the work.

Everything below is judged against that. Features that don't make the agent
easier to follow or safer to trust don't earn their bytes.

## Shipped

**Legibility — you can see what it's doing**

- Live activity strip: current phase (`thinking…`, `running shell`), step
  counter, and elapsed timer, pinned above the composer so tool cards never push
  it off-screen.
- Streaming tokens and reasoning, with collapsible thinking panels.
- Collapsible tool cards with status rails, and `+`/`−` colouring for diffs.
- Run summary card: steps and tool calls, or the failure.
- Live token and cost counters, budgeted against the current model's context
  window.

**Control — you can steer it**

- Approval gates: `shell` and `write_file` pause the run for an inline
  Approve/Deny, unless YOLO mode is on.
- Stop at any point, mid-stream, without losing what was already produced.
- Revert: restore the workspace to its pre-run state from the summary card.
- Slash commands and `@file` references in the composer.
- Per-chat model selection, reasoning effort, and context-window overrides.

**Continuity — the work persists**

- Named chats that persist, resume, fork and delete.
- Projects: chats grouped under their own workspace folder.
- Full-text search across chat history.
- Automatic context compaction as a chat approaches its model's window.
- Light/dark theming, persisted.

**Reach**

- Any OpenAI-compatible provider, several at once, with automatic retry on
  rate limits.
- Image input by pasting into the composer.
- Skills (`SKILL.md`), TypeScript plugins, and MCP servers — all landing as
  tools in one registry.
- `e-rpc`: the same engine headless over JSONL, embeddable in editors and
  scripts.

## Planned

**Streaming tool output.** Long `shell` runs currently show a spinner until they
finish. The `Emitter::tool_delta` hook exists but nothing calls it; wiring it up
needs the shell tool to read stdout/stderr incrementally.

**Plan preview.** Render the intended sequence of tool calls as an expandable
card with per-step status, so the trajectory is visible before it executes
rather than one call at a time.

**Per-run cost in the summary.** The summary card reports steps and tool calls;
tokens and cost are only in the status bar as running totals. They belong on the
card too.

**Reduced motion.** Several looping animations ignore
`prefers-reduced-motion`. They should stop when it is set.

**Keyboard navigation through the transcript.** Focus rings and composer
shortcuts exist, but there is no `Tab`/`Space` traversal of turns and tool
cards.

**Richer diff rendering.** Diffs get `+`/`−` colouring today; syntax
highlighting inside the hunks would make review far faster.

**Settings organisation.** Provider fields are still a flat list with one
collapsible section. It wants real grouping as the number of providers grows.

## Non-goals

- **Electron or a UI framework.** The native WebView and a small hand-written
  renderer are the identity, not a limitation to grow out of.
- **Cloud sync or accounts.** Everything stays on the machine.
- **A second SDK language.** The `Tool` trait is the only Rust surface; plugins
  are folders.
- **Replacing the terminal.** `e` complements it by making one specific loop
  legible.
