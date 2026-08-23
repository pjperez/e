# e — Product & UX Roadmap (second pass)

*Product lens: the whole point of `e` is that the agent's inner loop is more
**legible, controllable, and trustworthy** than doing it in a terminal. Every
feature below is judged by one question: does it make the user feel more in
control and more informed?*

---

## The core UX thesis

A terminal gives you raw output but no *situational awareness*. `e` wins by
giving the user a **live, glanceable picture of what the agent is doing, what it
plans to do, and what it changed** — while staying fast and zero-clutter.

Three pillars:

1. **Legibility** — you always know what's happening, next, and why.
2. **Control** — you can steer, approve, and stop at the right moments.
3. **Continuity** — your work persists, resumes, and forks like a real tool.

---

## Current UX audit (what's good, what's broken)

### Good — keep
- Dark, calm, single-column, zero chrome. Focused.
- Streaming tokens + caret = the product feels alive.
- Collapsible tool cards = good progressive disclosure.
- Empty state with suggestions = decent onboarding.
- Model pill + autocomplete picker.

### Broken — fix first (these are the real problems)
1. **Zero situational awareness during a run.** Status bar is just a pulsing
   dot. You can't tell *thinking vs. running a tool vs. how many steps vs.
   what's next*. This is the #1 trust killer.
2. **Tool cards bury the current action.** They pile up inline and push your
   prompt off-screen mid-run. There's no persistent "activity" strip.
3. **No plan preview.** Tool calls stream one at a time; you can't see the
   intended sequence or intervene early.
4. **Stop is undiscoverable.** `Esc` works but there's no visible pause/stop
   affordance, and no approve moment for risky actions.
5. **Composer is the only surface.** No slash commands, no file references, no
   way to set scope. For a coding harness this is a big miss.
6. **No conversation management.** One linear thread; "+" silently wipes
   everything. No resume, fork, or history.
7. **Settings is a wall of fields.** Overwhelming; model picker is inconsistent
   with settings.
8. **No "what changed" summary.** After a run you see raw tool cards, not an
   aggregated diff/impact view.
9. **Empty state doesn't teach.** Cute copy, but no capability signal.
10. **Accessibility gaps.** Keyboard nav, focus states, reduced motion.

---

## Restructured roadmap — organized by user journey, not engineering

### Journey 1: "I understand what's happening" (Legibility)
*The foundation. Without this, nothing else matters.*

**U1. Live activity strip**
- A slim, always-visible strip above the composer (or pinned in the header)
  showing the *current* step: `thinking…`, `running shell`, `step 3/25`, with a
  subtle spinner. Replaces the meaningless pulsing dot.
- Tool cards stay in the conversation, but the *live* action lives in the
  strip so the prompt never gets pushed off-screen.

**U2. Plan preview**
- Before a multi-tool run, render an expandable "plan" card at the top of the
  response: the intended sequence of tool calls, each with a status
  (queued → running → done/error). Click any step to jump to its result.
- Lets the user *see the trajectory* and intervene early.

**U3. Step counter & progress**
- Show `step N / 25` and elapsed time in the activity strip. Removes the "is it
  stuck?" anxiety.

**U4. Streaming tool output**
- Long `shell` runs stream stdout/stderr into the card live instead of a frozen
  spinner. The strip shows the tail while it runs.

### Journey 2: "I'm in control" (Control & trust)

**U5. Visible stop + approve**
- Replace the send button with a **Stop** button (already exists) but make it
  prominent and labeled, not just an icon swap.
- Add **approval gates**: risky tools (`shell`, `write_file`) pause the loop and
  surface an inline **Approve / Deny** prompt in the activity strip. Configurable
  per tool and per workspace.

**U6. Slash commands & scope**
- `/` opens a command menu: `/new`, `/model`, `/workspace`, `/diff`, `/undo`,
  `/help`. Keyboard-first.
- **File references**: type `@` to attach a workspace file into context (reads
  it into the prompt). This is the killer feature for a coding harness — "fix
  `@src/main.ts`" just works.

**U7. Undo / revert**
- After a run, an inline **"Revert changes"** action on the summary that
  restores files from the pre-run state (via git or a snapshot). The safety net
  that makes users comfortable letting the agent act.

### Journey 3: "My work persists and builds" (Continuity)

**U8. Sessions**
- A lightweight session switcher (sidebar or dropdown): name, resume, fork,
  delete. The "+" button starts a *new named session* instead of silently
  wiping the current one.
- Auto-save on `done`; crash-safe.

**U9. History search**
- `Cmd/Ctrl+K` command palette with full-text search across sessions and tool
  results. Jump back to any past action.

**U10. "What changed" summary**
- At the end of a run, a compact summary card: files written/modified, commands
  run, errors, tokens used. One glance = full impact. Links to the git diff
  view.

### Journey 4: "It feels like mine" (Polish & trust)

**U11. Better empty state**
- Replace generic copy with capability chips ("I can run commands, read/write
  files, list dirs") and 2–3 *scenario* starters ("Refactor this module",
  "Explain this repo", "Fix the failing test"). Teach, don't just greet.

**U12. Settings redesign**
- Split the wall into tabs: **Connection** (provider/base/key/model) and
  **Behavior** (workspace, system prompt, temperature, tool permissions).
  Move model switching into the picker only; settings is for the rest.

**U13. Keyboard-first navigation**
- `Tab`/`Shift+Tab` between turns and cards, `Space` to expand, `/` composer,
  `Cmd/Ctrl+K` palette, `Esc` everywhere. Visible focus rings. Reduced-motion
  support.

**U14. Diff rendering**
- When a tool result is a diff, render it with +/- coloring and syntax
  highlighting instead of raw text. This is the single most "wow" visual for a
  coding harness.

**U15. Theming**
- Light/dark + accent presets. System font stack stays. Low effort, high
  perceived polish.

---

## UX-first sequencing (what to ship, in order of user value)

| Milestone | Focus | Items | Why this order |
|-----------|-------|-------|----------------|
| **M1** | Awareness | U1, U3, U4 | Nothing matters until users trust what's happening. Small, high-impact. |
| **M2** | Control | U5, U6, U7 | Give users the wheel before they go deep. Approval + undo = trust. |
| **M3** | Continuity | U8, U9, U10 | Persistence turns `e` into a real tool, not a toy. |
| **M4** | Legibility depth | U2, U14 | Plan preview + diff rendering make it *feel* like a pro tool. |
| **M5** | Polish | U11, U12, U13, U15 | The last 20% that makes it feel finished. |

**M1 is the recommended first sprint.** It directly attacks the #1 product
problem (situational awareness) with the smallest, safest changes — and it's the
prerequisite for everything else to feel trustworthy.

---

## UX principles to hold (non-negotiables)

1. **One glance, one answer.** The user should understand the agent's state in
   under a second, without reading.
2. **Progressive disclosure.** Default view is clean; detail is one click away.
   Never dump everything at once.
3. **Never silently destroy.** No action (clear, delete, overwrite) happens
   without a visible, reversible path.
4. **Keyboard and mouse both first-class.** Power users live on the keyboard;
   the tool must not punish either.
5. **Stay fast and slim.** Every feature must justify its bytes. If it adds
   chrome, it must add more clarity than clutter.
6. **Privacy is a feature.** Everything stays local; never imply otherwise.

---

## Non-goals (deliberately out of scope)

- Electron / heavy frameworks — native WebView + slim JS is the identity.
- Cloud sync / accounts — local-first is a selling point, not a limitation.
- A plugin SDK in a new language — keep the one `Tool` trait.
- Replacing the terminal — we *complement* it with legibility.

---

## Status log

**Sprint: M1 (Awareness) — in progress**

- **U1 Live activity strip — DONE.** Added `e:activity` event (phase: `thinking` | `tool`, tool name, step). The strip sits above the composer and shows: spinner + `thinking…` / `running <tool>` + `step N/25` + a live elapsed timer. It replaces the pulsing status dot; tool cards stay in the conversation so the prompt never gets pushed off-screen. (`engine/mod.rs` Emitter::activity, `agent.rs` emits, `lib.rs` AppEmitter, `index.html`/`main.ts`/`style.css` strip.)
- **U3 Step counter & elapsed — DONE.** `step N/25` and `mm:ss` elapsed shown in the strip; timer starts on send, stops on done/error.
- **U4 Streaming tool output — SCAFFOLDED.** `Emitter::tool_delta` exists but is unused; requires making the shell tool async (read stdout/stderr incrementally). Deferred from this pass.

**Pending M1:** U4 (finish streaming). **Next after M1:** M2 (Control): visible Stop, approval gates for risky tools, slash commands + `@file` refs, undo/revert.

**Progress since last log:**
- **Pasted images — DONE.** Any image pasted into the composer (clipboard) is captured as a data URL, shown as a removable thumbnail, and sent to the model. The engine's message model gained `Part::ImageData` and the OpenAI conversion emits `image_url` content parts for the active user turn. (supports any model with image input, e.g. GPT-5.6 Luna.)
- **U10 "what changed" summary — DONE.** The engine now emits `e:summary` (steps, tool calls, stopped, error) at the end of every run; the UI renders a compact `done — N steps · M tool calls` (or ⚠ error) card under the final turn.
- **Removed the 25-step cap.** The loop now runs until the model stops requesting tools; Stop/Esc is the escape hatch.
- M1 U1/U3 (activity strip) and M2 U6 (slash commands + @file) remain done from earlier.
- **Deferred:** U4 (async streaming tool output), U5 (approval gates), U7 (undo/revert), U8 sessions, U9 history search, U11 empty-state chips, U12 settings tabs, U13 keyboard nav, U14 diff rendering, U15 theming.

**Progress (U8, U12, U15 done):**
- **U8 Sessions/multi-chat — DONE.** File-backed session store in `~/.e/sessions/` (per-session history + index, auto-saved after every run). Top-bar `☰` dropdown: create, resume/switch, fork, delete; history loads and renders on switch. The `+` button now starts a new named session instead of wiping the thread. Commands: `list_sessions`/`new_session`/`delete_session`/`fork_session`/`switch_session`/`get_session`.
- **U12 Settings redesign + Refresh fix — DONE.** Settings is now provider-centric: one provider selector with per-provider `name / base_url / api_key / model`, a collapsible **Behavior** section (workspace, temperature, system prompt). The Refresh failure was because the seeded provider carried an empty key; the provider list now inherits the active key/base/models at load, so Refresh actually authenticates.
- **U15 Theming — DONE.** Light/dark toggle (top-bar `☾`/`☀`), persisted in localStorage, no recompile.

**Progress (workspace per-chat, OS hint, multi-chat fix):**
- **Per-chat workspace — DONE.** `SessionMeta` now carries `workspace`; creating a new chat asks for a workspace folder (top-bar `+` opens the sessions menu with a workspace field), and `send_text` uses that session's workspace for tools. Workspace removed from global settings.
- **Auto OS detection — DONE.** `platform_hint()` runs at startup and is injected into the system prompt (Windows → cmd syntax / use list_dir/read_file instead of Unix tools; otherwise POSIX sh). Re-detected each restart.
- **Multi-chat bug — FIXED.** Root cause: `SessionMeta.workspace` was added without `#[serde(default)]`, so loading an existing `index.json` failed deserialization and reset sessions to a fresh "Chat 1" — dropping prior chats on every load. Added `#[serde(default)]` and guarded empty-id history writes (the orphan `.json` file is cleaned up).
