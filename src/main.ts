// e — UI controller.
import { renderMarkdown } from "./markdown";
import { attachTurnCopy, decorateCodeBlocks, wireCopy } from "./copy";
import * as api from "./api";
import type { Config, ProviderItem } from "./api";

const conv = document.getElementById("conversation") as HTMLElement;
const empty = document.getElementById("empty") as HTMLElement;
const input = document.getElementById("input") as HTMLTextAreaElement;
const sendBtn = document.getElementById("send") as HTMLButtonElement;
const statusWrap = document.getElementById("status") as HTMLElement;
const statusText = document.getElementById("status-text") as HTMLElement;
const modelPill = document.getElementById("model-pill") as HTMLElement;
const chatTitle = document.getElementById("chat-title") as HTMLElement;
const activity = document.getElementById("activity") as HTMLElement;
const actText = document.getElementById("act-text") as HTMLElement;
const actStep = document.getElementById("act-step") as HTMLElement;
const actTime = document.getElementById("act-time") as HTMLElement;
const themeBtn = document.getElementById("btn-theme") as HTMLElement;

function applyTheme(t: string): void {
  document.documentElement.dataset.theme = t;
  themeBtn.textContent = t === "light" ? "☀" : "☾";
}
const savedTheme = localStorage.getItem("e:theme") || "dark";
applyTheme(savedTheme);
themeBtn.addEventListener("click", () => {
  const next = document.documentElement.dataset.theme === "light" ? "dark" : "light";
  localStorage.setItem("e:theme", next);
  applyTheme(next);
});

// ---------- status bar ----------
const sbWs = document.getElementById("sb-ws") as HTMLElement;
const sbProv = document.getElementById("sb-prov") as HTMLElement;
const sbModel = document.getElementById("sb-model") as HTMLElement;
const sbArrows = document.getElementById("sb-arrows") as HTMLElement;
const sbCtx = document.getElementById("sb-context") as HTMLElement;
const sbCost = document.getElementById("sb-cost") as HTMLElement;
const sbYolo = document.getElementById("sb-yolo") as HTMLElement;
const actSteer = document.getElementById("act-steer") as HTMLButtonElement;

function hostOf(u: string): string { try { return new URL(u).host; } catch { return u; } }

/// YOLO disables the approval prompt for shell/write_file, so it gets a
/// permanent badge rather than living only in the settings modal.
function setYoloIndicator(on: boolean): void {
  sbYolo.hidden = !on;
}

// ---------- per-chat run state ----------
// Everything about "is something happening" is scoped to a chat id. These used
// to be module-level singletons, so switching chats carried the previous chat's
// spinner, timer, token counters and queued message into the new one — leaving
// the new chat stuck in a fake busy state.
type Approval = { id: string; tool: string; preview: string };
type ChatUI = {
  running: boolean;
  /** Message typed while this chat was busy; sent to *this* chat when it frees up. */
  queued: string;
  startedAt: number;
  activityText: string;
  activityStep: string;
  /** Streamed text/reasoning not yet persisted, so switching away and back mid-run keeps it. */
  liveText: string;
  liveReason: string;
  baseIn: number;
  baseOut: number;
  liveIn: number;
  liveOut: number;
  /**
   * Prompt tokens of the last provider call — the *actual* size of the context
   * window. Distinct from `baseIn`, which is a lifetime sum across every run
   * and every step and so grows far faster than the context does.
   */
  ctxIn: number;
  money: number;
  costKnown: boolean;
  approval: Approval | null;
  errored: boolean;
  /** True while a send_text call is in flight and the backend hasn't registered the run yet. */
  sending: boolean;
  /**
   * Stop was pressed. The button goes live the moment the UI says "running",
   * which is before the backend has a run to cancel — attachments, the context
   * check and a compaction pass all happen first. Without this the press hit an
   * unknown session, was reported as a stale flag, and the run started anyway:
   * Stop looked like it did nothing. `startRun` checks it at the hand-off.
   */
  stopping: boolean;
  /** True while history is being summarised, so the UI can say so. */
  compacting: boolean;
  /**
   * A throttled provider call being backed off. Held as a deadline rather than
   * a formatted string so the strip can count the wait down live: a silent
   * multi-second pause is indistinguishable from a hang.
   */
  retry: { until: number; attempt: number; max: number; reason: string } | null;
};

const chatUI = new Map<string, ChatUI>();

function ui(sid: string): ChatUI {
  let s = chatUI.get(sid);
  if (!s) {
    s = {
      running: false, queued: "", startedAt: 0, activityText: "", activityStep: "",
      liveText: "", liveReason: "",
      baseIn: 0, baseOut: 0, liveIn: 0, liveOut: 0, ctxIn: 0, money: 0, costKnown: false,
      approval: null, errored: false, sending: false, stopping: false, compacting: false, retry: null,
    };
    chatUI.set(sid, s);
  }
  return s;
}
const cur = (): ChatUI => ui(currentSession);
const isRunning = (): boolean => !!currentSession && cur().running;

/** The global setting, used until a chat's own model reports something better. */
let defaultCtxWindow = 1_000_000;
/**
 * Usable context window per chat. Per chat rather than one global number
 * because a chat can sit on a model from any provider: a queued run in a
 * background chat has to budget against *its* model, not whichever one the
 * visible chat happens to be on.
 */
const ctxWindows = new Map<string, number>();
const winOf = (sid: string): number => ctxWindows.get(sid) || defaultCtxWindow;

/**
 * Re-read a chat's context window from the backend, following its own model.
 * Deliberately never compacts: picking a smaller model is often a misclick, and
 * summarising history the moment it happens cannot be undone. The budget moves
 * straight away; the history is only collapsed by the next send.
 */
async function refreshCtxWindow(sid: string): Promise<number> {
  const win = await api.contextBudget(sid || undefined).catch(() => 0);
  if (win > 0) ctxWindows.set(sid, win);
  return winOf(sid);
}

/** Compact once the live context passes this share of the window. */
const COMPACT_AT = 0.85;

function fmtTokens(n: number): string {
  // A round million reads as "1M", not "1.0M".
  if (n >= 1_000_000) return (n / 1_000_000).toFixed(1).replace(/\.0$/, "") + "M";
  if (n >= 1_000) return Math.round(n / 1_000) + "k";
  return String(n);
}

function sbUpdate(): void {
  const s = cur();
  const win = winOf(currentSession);
  const inT = Math.round(s.baseIn + s.liveIn);
  const outT = Math.round(s.baseOut + s.liveOut);
  // Context usage is the live window (last prompt + what we're about to add),
  // never the lifetime token total.
  const ctx = Math.round(s.ctxIn + s.liveIn);
  const pct = win > 0 ? Math.min(100, (ctx * 100) / win) : 0;
  sbArrows.textContent = "↑" + inT.toLocaleString() + " ↓" + outT.toLocaleString();
  sbCtx.textContent = s.compacting
    ? "compacting…"
    : "CH " + fmtTokens(ctx) + "/" + fmtTokens(win) + " · " + pct.toFixed(1) + "%";
  // Landing over the line by switching model is a state worth showing rather
  // than acting on, so the user can switch back before any history is lost.
  const over = !s.compacting && win > 0 && ctx >= win * COMPACT_AT;
  sbCtx.classList.toggle("over", over);
  sbCtx.title = over ? "Over the compaction threshold — history is summarised on your next message" : "";
  sbCost.textContent = s.costKnown ? "$" + s.money.toFixed(3) : "$-";
}
/** Name of the provider serving the current model, for the status bar. */
function providerLabel(): string {
  const hit = catalog.find((c) => c.provider_id === currentProviderId);
  if (hit) return hit.provider_name;
  const p = providers.find((x) => x.id === currentProviderId) || providers[0];
  if (p) return p.name || p.id;
  return "none";
}

const setBusy = (b: boolean) => {
  statusWrap.classList.toggle("busy", b);
  if (b) statusWrap.classList.remove("error");
};

function prettyArgs(args: string): string {
  try {
    return JSON.stringify(JSON.parse(args), null, 2);
  } catch {
    return args;
  }
}

function clampOut(out: string, max = 6000): string {
  if (out.length <= max) return out;
  return out.slice(0, max) + `\n… (truncated ${out.length - max} chars)`;
}

/// Markdown and its copy buttons always go up together — a bare `innerHTML =
/// renderMarkdown(...)` would drop the buttons on the floor.
function renderInto(el: HTMLElement, md: string): void {
  el.innerHTML = renderMarkdown(md);
  decorateCodeBlocks(el);
}

// ---------- conversation rendering ----------
type ToolBlock = { el: HTMLElement; stateEl: HTMLElement; outputEl: HTMLElement };
type Turn = {
  el: HTMLElement;
  body: HTMLElement;
  textEl: HTMLElement;
  /** Whose turn this is. A user turn has `textEl === body`, so anything the
   *  run appends to it is destroyed the moment its text is re-rendered. */
  role: "user" | "assistant";
  raw: string;
  hasCaret: boolean;
  tools: Map<string, ToolBlock>;
  thinkEl?: HTMLElement;
  thinkOpen?: boolean;
};

let turns: Turn[] = [];
const lastTurn = (): Turn | undefined => turns[turns.length - 1];

/// The turn a run's own output belongs in. Never the user's bubble: a run that
/// fails before the model says anything (a 429 on send, say) would otherwise
/// hang its error card inside the message that triggered it, where the `done`
/// re-render then wipes it — a failure that left nothing on screen at all.
function outputTurn(): Turn {
  const t = lastTurn();
  return t && t.role === "assistant" ? t : addAssistantTurn();
}

/// A quiet divider standing in for history that was summarised away. The
/// summary text itself is deliberately not shown — compaction is plumbing, not
/// conversation.
function compactionMark(): HTMLElement {
  const el = document.createElement("div");
  el.className = "compacted";
  el.innerHTML = `<span class="compacted-rule"></span><span class="compacted-label">context compacted</span><span class="compacted-rule"></span>`;
  el.title = "Earlier messages were summarised to free up context";
  return el;
}

function addUserTurn(text: string): void {
  const el = document.createElement("div");
  el.className = "turn user";
  el.innerHTML = `<div class="role">you</div><div class="body user"></div>`;
  const body = el.querySelector(".body") as HTMLElement;
  body.textContent = text;
  attachTurnCopy(el, "message", () => text);
  conv.appendChild(el);
  turns.push({ el, body, textEl: body, role: "user", raw: text, hasCaret: false, tools: new Map() });
  scrollBottom();
}

function addAssistantTurn(): Turn {
  const el = document.createElement("div");
  el.className = "turn assistant";
  el.innerHTML = `<div class="role">e</div><div class="body assistant"><div class="text"></div></div>`;
  const body = el.querySelector(".body") as HTMLElement;
  const textEl = el.querySelector(".text") as HTMLElement;
  conv.appendChild(el);
  const t: Turn = { el, body, textEl, role: "assistant", raw: "", hasCaret: true, tools: new Map() };
  attachTurnCopy(el, "reply", () => t.raw);
  turns.push(t);
  scrollBottom();
  return t;
}

function removeCaret(t: Turn): void {
  t.hasCaret = false;
  const c = t.textEl.querySelector(".caret");
  if (c) c.remove();
}
/// Streaming path: plain text + caret (cheap, O(1) per token).
/// Full markdown is rendered once, on message_end / done.
function setText(t: Turn): void {
  t.textEl.textContent = t.raw;
  const c = document.createElement("span");
  c.className = "caret";
  t.textEl.appendChild(c);
  t.hasCaret = true;
}

/// The collapsible "Thinking" panel for a turn, created on demand. Uses the
/// stylesheet's `.think` rules instead of the three copies of inline CSS this
/// used to carry around.
///
/// Always starts collapsed, including while reasoning is still streaming: an
/// open panel shoves the actual answer off screen for as long as the model
/// thinks. The summary carries a one-line preview so a collapsed panel still
/// shows movement.
function thinkBody(t: Turn): HTMLElement {
  if (!t.thinkEl) {
    const d = document.createElement("details");
    d.className = "think live";
    d.innerHTML = `<summary>Thinking<span class="think-preview"></span></summary><div class="think-body"></div>`;
    t.body.insertBefore(d, t.textEl);
    t.thinkEl = d;
  }
  return t.thinkEl.querySelector(".think-body") as HTMLElement;
}

/// Mirror the tail of the reasoning into the collapsed summary, and keep an
/// expanded panel pinned to the newest text — but only when the user is already
/// at the bottom, so scrolling back to read isn't yanked away.
function thinkTouch(t: Turn): void {
  const body = thinkBody(t);
  const prev = t.thinkEl?.querySelector(".think-preview") as HTMLElement | null;
  if (prev) {
    const line = body.textContent!.replace(/\s+/g, " ").trimEnd();
    prev.textContent = line.length > 160 ? "…" + line.slice(-160) : line;
  }
  if (body.scrollHeight - body.scrollTop - body.clientHeight < 40) {
    body.scrollTop = body.scrollHeight;
  }
}

function addToolCard(t: Turn, id: string, name: string, args: string): void {
  const card = document.createElement("div");
  card.className = "tool pending";
  Object.assign(card.style, {
    margin: "14px 0", border: "1px solid var(--edge)", borderLeft: "3px solid var(--warn)",
    borderRadius: "12px", background: "var(--bg-2)", overflow: "hidden",
    fontFamily: "var(--font)",
  });
  card.innerHTML = `
    <div class="tool-head" style="display:flex;align-items:center;gap:12px;padding:14px 18px 14px 20px;min-height:52px;">
      <span class="tool-ico" style="width:28px;height:28px;background:var(--bg-3);border-radius:8px;display:inline-flex;align-items:center;justify-content:center;color:var(--accent);flex:none;font-size:14px;">&#9881;</span>
      <span class="tool-name" style="font-family:var(--mono);font-size:13px;font-weight:600;color:var(--text);flex:none;"></span>
      <span class="tool-preview" style="flex:1;min-width:0;overflow:hidden;text-overflow:ellipsis;white-space:nowrap;font-family:var(--mono);font-size:12px;color:var(--text-faint);"></span>
      <span class="tool-state" style="font-family:var(--mono);font-size:10px;font-weight:700;letter-spacing:0.1em;text-transform:uppercase;padding:3px 10px;border-radius:999px;background:rgba(251,191,36,0.1);color:var(--warn);border:1px solid rgba(251,191,36,0.3);flex:none;">running</span>
      <span class="chev"><svg viewBox="0 0 16 16" width="12" height="12" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"><path d="M6 3l5 5-5 5"/></svg></span>
    </div>
    <div class="tool-body">
      <div class="label">input</div>
      <pre class="in"></pre>
      <div class="tool-out-row"><span class="label" style="padding:0">output</span><button class="tool-copy" type="button">&#10697; copy</button></div>
      <pre class="out"></pre>
    </div>`;
  card.querySelector(".tool-name")!.textContent = name;
  const argsStr = typeof args === "string" ? args : JSON.stringify(args);
  const preview = argsStr.length > 90 ? argsStr.slice(0, 90) + "…" : argsStr;
  (card.querySelector(".tool-preview") as HTMLElement).textContent = preview;
  (card.querySelector(".in") as HTMLElement).textContent = prettyArgs(args);
  const stateEl = card.querySelector(".tool-state") as HTMLElement;
  const outputEl = card.querySelector(".out") as HTMLElement;
  const head = card.querySelector(".tool-head") as HTMLElement;
  head.addEventListener("click", () => {
    card.classList.toggle("open");
    if (card.classList.contains("open")) scrollBottom();
  });
  t.body.appendChild(card);
  t.tools.set(id, { el: card, stateEl, outputEl });
  scrollBottom();
}

function resolveTool(id: string, success: boolean, output: string): void {
  for (const t of turns) {
    const b = t.tools.get(id);
    if (b) {
      b.el.classList.remove("pending");
      // Update inline styles (they override CSS classes)
      b.el.style.borderLeft = success ? "3px solid var(--accent-2)" : "3px solid var(--err)";
      const chip = b.stateEl;
      if (success) {
        chip.style.background = "rgba(110,231,183,0.08)";
        chip.style.color = "var(--accent-2)";
        chip.style.border = "1px solid rgba(110,231,183,0.25)";
      } else {
        chip.style.background = "rgba(248,113,113,0.08)";
        chip.style.color = "var(--err)";
        chip.style.border = "1px solid rgba(248,113,113,0.3)";
      }
      b.el.classList.add(success ? "ok" : "err");
      b.stateEl.textContent = success ? "done" : "error";
      const copyBtn = b.el.querySelector(".tool-copy") as HTMLButtonElement;
      wireCopy(copyBtn, () => output);
      const inTxt = ((b.el.querySelector(".in") as HTMLElement).textContent || "");
      const diffy = output.split("\n").filter((l) => /^[+-]/.test(l)).length > 3;
      if (diffy || /^(git (diff|apply))/.test(inTxt.trim())) {
        b.outputEl.classList.add("diff");
        // `+` was tested before `-`, so removals were painted as additions.
        b.outputEl.innerHTML = output.split("\n").map((l) => {
          const esc = l.replace(/&/g, "&amp;").replace(/</g, "&lt;");
          if (/^\+/.test(l)) return '<span class="dl-add">' + esc + "</span>";
          if (/^-/.test(l)) return '<span class="dl-del">' + esc + "</span>";
          return esc;
        }).join("\n");
      } else {
        b.outputEl.textContent = clampOut(output);
      }
      if (output.length > 2400) {
        b.outputEl.classList.add("clamped");
        b.outputEl.addEventListener("click", () => {
          b.outputEl.classList.toggle("clamped");
        });
      }
      return;
    }
  }
}

// ---------- empty state & helpers ----------
const suggestions = [
  "Summarize the current directory and suggest next steps",
  "Write a tiny CLI in Rust and test it",
  "Explain what's in this repo at a glance",
  "Draft a bullet list of risks in this codebase",
];

function renderSuggestions(): void {
  const box = document.getElementById("suggestions")!;
  suggestions.forEach((s) => {
    const b = document.createElement("button");
    b.textContent = s;
    b.addEventListener("click", () => {
      input.value = s;
      doSend();
    });
    box.appendChild(b);
  });
}

function updateEmpty(): void {
  empty.classList.toggle("hidden", turns.length > 0 || isRunning());
}

function scrollBottom(): void {
  conv.scrollTop = conv.scrollHeight;
}

function autoGrow(): void {
  input.style.height = "0";
  input.style.height = Math.min(input.scrollHeight, 200) + "px";
}

const SEND_ICON = `<svg viewBox="0 0 24 24" width="18" height="18"><path d="M4 12l16-7-6 16-2.5-6.5L4 12z" fill="currentColor"/></svg>`;
const STOP_ICON = `<svg viewBox="0 0 24 24" width="16" height="16"><rect x="7" y="7" width="10" height="10" rx="2" fill="currentColor"/></svg>`;

function updateInputState(): void {
  const running = isRunning();
  // While running this is the Stop button, so it must stay clickable — it used
  // to be disabled by the same flag that turned it into Stop, which is why
  // Stop appeared to do nothing.
  sendBtn.disabled = !running && input.value.trim() === "";
  sendBtn.classList.toggle("stop", running);
  sendBtn.title = running ? "Stop" : "Send";
  sendBtn.setAttribute("aria-label", running ? "Stop" : "Send");
  sendBtn.innerHTML = running ? STOP_ICON : SEND_ICON;
  input.placeholder = running ? "Reply — sent when this chat finishes…" : "Ask e to do something…";
}

/// Turn a raw engine/provider error into something a human can act on.
/// Provider failures arrive as `provider returned 429 …: {json}`, which is
/// unreadable dumped verbatim into the transcript.
type ErrInfo = { title: string; hint: string; code: string; tags: string[]; raw: string };

function explainError(raw: string): ErrInfo {
  const code = /returned (\d{3})/.exec(raw)?.[1] || "";
  const brace = raw.indexOf("{");
  let data: Record<string, unknown> | null = null;
  if (brace >= 0) {
    try { data = JSON.parse(raw.slice(brace)) as Record<string, unknown>; } catch { data = null; }
  }
  const pick = (o: unknown, k: string): unknown => (o && typeof o === "object" ? (o as Record<string, unknown>)[k] : undefined);
  const extra = pick(data, "extra_fields");
  const routing = pick(extra, "routing_info");
  const provider = (pick(routing, "provider") ?? pick(extra, "provider")) as string | undefined;
  const model = (pick(routing, "model") ?? pick(extra, "resolved_model_used")) as string | undefined;
  const message = (pick(pick(data, "error"), "message") ?? pick(data, "message")) as string | undefined;

  let title = "Run failed";
  let hint = message || "";
  if (/^no provider configured/i.test(raw)) {
    title = "No provider yet";
    hint = "Open Settings (⚙) to add an OpenAI-compatible provider — a base URL, and a key if it needs one — then pick a model from the title bar.";
  } else if (/^workspace folder does not exist/i.test(raw)) {
    title = "Workspace folder is missing";
    hint = raw.replace(/^workspace folder does not exist:\s*/i, "Folder: ");
  } else if (/insufficient tool messages|must be followed by tool messages|tool_call_id/i.test(raw)) {
    // History with a tool call that never got a result — the app was closed or
    // crashed mid-tool. Every later send replays it and fails identically, so
    // say what happened instead of showing a raw 400.
    title = "Interrupted tool call";
    hint = "This chat was closed while a tool was still running, leaving its history incomplete. It has been repaired — send again.";
  } else if (/^request failed/i.test(raw)) {
    title = "Can't reach the provider";
    hint = "Check your connection and the base URL in Settings.";
  } else if (/timed out/i.test(raw)) {
    title = "Timed out";
    hint = hint || "The provider took too long to respond.";
  } else if (code === "429") {
    title = "Rate limited";
    hint = "The provider is still throttling after several automatic retries — wait a bit before sending again, or switch model.";
  } else if (code === "401" || code === "403") {
    title = "Authentication failed";
    hint = "Check this provider's API key in Settings (⚙).";
  } else if (code === "404") {
    title = "Model not available";
    hint = "The provider doesn't serve this model. Pick another from the model picker.";
  } else if (code.startsWith("5")) {
    title = "Provider error";
    hint = hint || "The provider failed server-side, and automatic retries didn't clear it.";
  } else if (code === "400") {
    title = "Request rejected";
  }

  const tags = [provider, model].filter((t): t is string => !!t);
  return { title, hint, code, tags, raw };
}

function errorCard(raw: string): HTMLElement {
  const info = explainError(raw);
  const el = document.createElement("div");
  el.className = "errcard";

  const head = document.createElement("div");
  head.className = "errcard-head";
  const ico = document.createElement("span");
  ico.className = "errcard-ico";
  ico.textContent = "⚠";
  const title = document.createElement("span");
  title.className = "errcard-title";
  title.textContent = info.title;
  head.append(ico, title);
  if (info.code) {
    const code = document.createElement("span");
    code.className = "errcard-code";
    code.textContent = info.code;
    head.appendChild(code);
  }
  el.appendChild(head);

  if (info.hint) {
    const hint = document.createElement("p");
    hint.className = "errcard-hint";
    hint.textContent = info.hint;
    el.appendChild(hint);
  }
  if (info.tags.length) {
    const meta = document.createElement("div");
    meta.className = "errcard-meta";
    info.tags.forEach((t) => {
      const s = document.createElement("span");
      s.textContent = t;
      meta.appendChild(s);
    });
    el.appendChild(meta);
  }

  const det = document.createElement("details");
  det.className = "errcard-raw";
  const sum = document.createElement("summary");
  sum.textContent = "Details";
  const pre = document.createElement("pre");
  pre.textContent = info.raw;
  const copy = document.createElement("button");
  copy.className = "errcard-copy";
  copy.textContent = "copy";
  wireCopy(copy, () => info.raw);
  det.append(sum, pre, copy);
  el.appendChild(det);
  return el;
}

function setError(msg: string): void {
  if (currentSession) {
    chatState[currentSession] = "error";
    cur().errored = true;
    renderSessions();
  }
  statusWrap.classList.add("error");
  statusText.textContent = "error";
  hideActivity();
  // The card lives in the turn body, not in `textEl`: `done` re-renders the
  // text from `raw`, which would otherwise wipe the message a moment later.
  const t = outputTurn();
  removeCaret(t);
  t.body.appendChild(errorCard(msg));
  if (currentSession) cur().running = false;
  setBusy(false);
  updateInputState();
  updateEmpty();
  scrollBottom();
}

// ---------- send / events ----------
/** Last-saved provider list; Settings edits a copy so Cancel really cancels. */
let providers: ProviderItem[] = [];
let currentWs = "";
/** Provider open in the Settings editor. Purely an editing cursor — there is
 *  no "active provider" to choose any more; picking a model picks one. */
let editingProviderId = "";
/** Every model on offer across all enabled providers — what the picker shows. */
let catalog: api.ModelChoice[] = [];
let currentModel = "";
/** Provider serving the current model; the same id can exist in several. */
let currentProviderId = "";

// ---------- queued-message chip ----------
// A message typed while a chat is busy belongs to *that* chat and is shown
// explicitly, so it is never a mystery where it went.
const queuedChip = document.createElement("div");
queuedChip.id = "queued";
queuedChip.hidden = true;
queuedChip.innerHTML = `<span class="q-tag">queued</span><span class="q-text"></span><button class="q-x" type="button" title="Cancel queued message">×</button>`;
const queuedText = queuedChip.querySelector(".q-text") as HTMLElement;
(queuedChip.querySelector(".q-x") as HTMLButtonElement).addEventListener("click", () => {
  const s = cur();
  input.value = s.queued;
  s.queued = "";
  renderQueued();
  autoGrow();
  updateInputState();
  input.focus();
});
document.getElementById("composer-wrap")!.insertBefore(queuedChip, document.getElementById("composer"));

function renderQueued(): void {
  const q = currentSession ? cur().queued : "";
  queuedChip.hidden = !q;
  queuedText.textContent = q;
  queuedChip.title = q ? "Will be sent to this chat when the current run finishes" : "";
}

// ---------- activity strip ----------
const actApprove = document.getElementById("act-approve") as HTMLButtonElement;
const actDeny = document.getElementById("act-deny") as HTMLButtonElement;
let actTimer = 0;

function fmtElapsed(ms: number): string {
  const s = Math.max(0, Math.round(ms / 1000));
  return String(Math.floor(s / 60)).padStart(2, "0") + ":" + String(s % 60).padStart(2, "0");
}

function tickTimer(): void {
  const s = currentSession ? cur() : null;
  actTime.textContent = s && s.running && s.startedAt ? fmtElapsed(Date.now() - s.startedAt) : "";
  // The retry countdown rides the same tick, so the remaining wait visibly
  // shrinks instead of being a single number frozen on screen.
  if (s && s.retry && !s.approval) actText.textContent = retryLabel(s.retry);
}

/// What a backoff looks like to the user: the cause, the time left, and which
/// attempt is coming. Recomputed per tick so it counts down.
function retryLabel(r: NonNullable<ChatUI["retry"]>): string {
  const left = Math.max(0, r.until - Date.now());
  const when = left > 0 ? `retrying in ${Math.ceil(left / 1000)}s` : "retrying…";
  return `${r.reason} — ${when} (attempt ${r.attempt + 1} of ${r.max})`;
}

/// Repaint the whole strip from the *current* chat's state. Called on every
/// state change and after switching chats, so the strip can never show
/// leftovers from another conversation.
function renderActivity(): void {
  const s = currentSession ? cur() : null;
  const running = !!s && s.running;
  const approval = s ? s.approval : null;
  const retry = s && !approval ? s.retry : null;

  activity.hidden = !running && !approval;
  if (approval) {
    actText.textContent = `allow ${approval.tool}?`;
    actStep.textContent = approval.preview || "";
    actStep.title = approval.preview || "";
  } else {
    actText.textContent = retry ? retryLabel(retry) : s ? s.activityText || "thinking…" : "";
    actStep.textContent = s ? s.activityStep : "";
    actStep.title = "";
  }
  actApprove.hidden = !approval;
  actDeny.hidden = !approval;
  actSteer.hidden = !running;
  activity.classList.toggle("awaiting", !!approval);
  activity.classList.toggle("retrying", !!retry);

  tickTimer();
  if (running && !actTimer) actTimer = window.setInterval(tickTimer, 1000);
  if (!running && actTimer) {
    window.clearInterval(actTimer);
    actTimer = 0;
  }
}

function hideActivity(): void {
  if (currentSession) {
    const s = cur();
    s.activityText = "";
    s.activityStep = "";
    s.retry = null;
    s.startedAt = 0;
  }
  renderActivity();
}

/// Restore every piece of chrome for whichever chat is on screen now.
function applyChatUI(): void {
  renderActivity();
  renderQueued();
  updateInputState();
  updateEmpty();
  setBusy(isRunning());
  const s = currentSession ? cur() : null;
  statusWrap.classList.toggle("error", !!s && s.errored && !s.running);
  statusText.textContent = !s ? "ready" : s.running ? "working" : s.errored ? "error" : "ready";
  sbUpdate();
}

actApprove.addEventListener("click", () => resolveApproval(true));
actDeny.addEventListener("click", () => resolveApproval(false));

function resolveApproval(ok: boolean): void {
  const s = currentSession ? cur() : null;
  if (!s || !s.approval) return;
  const id = s.approval.id;
  s.approval = null;
  renderActivity();
  void api.approvalResolve(id, ok);
}

// ---------- slash commands + @file references (M2: control) ----------
// Group 2 is the path; the original pattern had a single group, so every
// reference resolved to `undefined` and @file silently never worked.
const ATTR_RE = String.raw`(^|\s)@([^\s@,.;:!?"'()]+)`;

async function expandAttachments(text: string): Promise<string> {
  const paths: string[] = [];
  const re = new RegExp(ATTR_RE, "g");
  let m: RegExpExecArray | null;
  while ((m = re.exec(text)) !== null) {
    const p = m[2];
    if (p && !paths.includes(p)) paths.push(p);
  }
  let ctx = "";
  for (const p of paths) {
    try {
      const att = await api.readAttachment(p);
      if (!att.content) continue;
      ctx += "```" + p + "\n" + att.content + "\n```\n\n";
    } catch {
      /* not a file; ignore */
    }
  }
  return ctx;
}

const HELP = `**Commands**\n- \`/new\` — new conversation\n- \`/model\` — switch model\n- \`/settings\` — open settings\n- \`/extensions\` — plugins, skills and MCP servers\n- \`/reload\` — re-read extensions (no restart)\n- \`/yolo [on|off]\` — auto-approve risky tools (shell, write_file)\n- \`/help\` — this help\n\n**File references**\nType \`@path\` (e.g. \`fix @src/main.ts\`) to include a file in context.`;

/// Single source of truth for YOLO: the settings checkbox reads the same saved
/// config, so the two controls cannot drift apart.
async function toggleYolo(arg: string): Promise<void> {
  try {
    const cfg = await api.getConfig();
    const next = arg === "on" ? true : arg === "off" ? false : !cfg.yolo;
    cfg.yolo = next;
    await api.saveConfig(cfg);
    setYoloIndicator(next);
    notify(next ? "YOLO mode ON — shell & write_file run without asking" : "YOLO mode OFF — risky tools ask first", next ? "error" : "info");
  } catch (e) {
    notify(String(e), "error");
  }
}

function runSlash(cmd: string): void {
  const parts = cmd.trim().split(/\s+/);
  const c = parts[0];
  const arg = (parts[1] || "").toLowerCase();
  const plugin = pluginCommands[c];
  if (plugin) {
    plugin.run();
    return;
  }
  const show = (md: string) => {
    const t = addAssistantTurn();
    removeCaret(t);
    t.raw = md;
    renderInto(t.textEl, md);
  };
  switch (c) {
    case "/new":
      // Clear only once the backend agrees. It refuses while a run is in
      // flight, and since slash commands now skip the queue this is reachable
      // mid-run: wiping first would leave the live run streaming into a
      // transcript that no longer matches the stored history.
      void (async () => {
        try {
          await api.clearSession(currentSession);
        } catch (e) {
          notify(String(e), "error");
          return;
        }
        conv.innerHTML = "";
        turns = [];
        empty.classList.remove("hidden");
        if (currentSession) {
          const s = cur();
          s.liveText = "";
          s.liveReason = "";
        }
        resetSB();
        applyChatUI();
      })();
      break;
    case "/model":
      closePicker();
      openPicker();
      break;
    case "/settings":
      closePicker();
      void openSettings();
      break;
    case "/extensions":
      closePicker();
      void openSettings(true);
      break;
    case "/reload":
      void reloadExtensions();
      break;
    case "/yolo":
      void toggleYolo(arg);
      break;
    case "/help":
      show(HELP);
      break;
    default:
      show("Unknown command `" + c + "`. Try `/help`.");
      break;
  }
}

function sessionName(sid: string): string {
  return sessions.find((x) => x.id === sid)?.name || "chat";
}

async function doSend(): Promise<void> {
  const text = input.value.trim();
  if (!text) return;

  // Slash commands are UI actions, not model input: they need no chat and are
  // never queued behind an in-flight run.
  if (text.startsWith("/")) {
    input.value = "";
    autoGrow();
    updateInputState();
    closeSlash();
    runSlash(text);
    return;
  }

  if (!currentSession) {
    notify("No chat selected", "error");
    return;
  }
  const sid = currentSession;
  const s = ui(sid);

  if (s.running) {
    // Queue against *this* chat, not a global slot, and show it.
    s.queued = s.queued ? s.queued + "\n\n" + text : text;
    input.value = "";
    autoGrow();
    renderQueued();
    updateInputState();
    notify("Queued for “" + sessionName(sid) + "” — sends when this run finishes", "info");
    return;
  }

  input.value = "";
  autoGrow();
  updateInputState();
  await startRun(sid, text);
}

/// Summarise old history when the *live context* is close to the model's
/// window. Deliberately a no-op most of the time: the old check compared a
/// lifetime token counter against a fixed number, so it fired constantly on
/// long-lived chats whose actual context was nowhere near full.
///
/// This is the *only* place compaction is triggered, which is what makes
/// switching to a smaller model safe: the budget on screen shrinks at once, but
/// nothing is summarised until the user actually sends a turn to that model.
async function maybeCompact(sid: string): Promise<void> {
  const s = ui(sid);
  // Re-read rather than trust the cache. This chat may have been moved to a
  // smaller model since its last run, and a queued chat is by definition not
  // the one on screen, so the visible chat's window is the wrong budget for it.
  const win = await refreshCtxWindow(sid);
  const used = s.ctxIn + s.liveIn;
  if (!win || used < win * COMPACT_AT) return;

  s.compacting = true;
  s.activityText = "compacting context…";
  if (sid === currentSession) {
    sbUpdate();
    renderActivity();
  }
  try {
    const r = await api.compactSession(sid);
    // The backend stops a compaction mid-summary when Stop is pressed during
    // its backoff wait. Treat that as the stop it was, not as a failure.
    if (r.stopped) s.stopping = true;
    if (r.compacted) {
      // The next run reports the real size; treat the window as reset until then.
      s.ctxIn = 0;
      // Re-rendering history here would drop the user turn that startRun has
      // already put on screen but not yet persisted, so splice the marker in
      // ahead of it instead.
      if (sid === currentSession) {
        const pending = turns[turns.length - 1];
        if (pending) conv.insertBefore(compactionMark(), pending.el);
        else conv.appendChild(compactionMark());
      }
    }
  } catch {
    // Compaction is best-effort: a failed summary must never block the send.
  } finally {
    s.compacting = false;
    // Don't talk over a stop the user asked for while we were compacting.
    s.activityText = s.stopping ? "stopping…" : "thinking…";
    // A backoff during compaction ends with it; leaving it would strand a dead
    // countdown over the run that follows.
    s.retry = null;
    if (sid === currentSession) {
      sbUpdate();
      renderActivity();
    }
  }
}

/// Start a run against an explicit chat. Safe to call for a chat that is not
/// on screen (that's how a queued message is flushed).
async function startRun(sid: string, text: string): Promise<void> {
  const s = ui(sid);
  const onScreen = sid === currentSession;

  s.liveText = "";
  s.liveReason = "";
  s.running = true;
  s.stopping = false;
  s.errored = false;
  s.startedAt = Date.now();
  s.activityText = "thinking…";
  s.activityStep = "";
  s.retry = null;
  chatState[sid] = "busy";

  if (onScreen) {
    addUserTurn(text);
    empty.classList.add("hidden");
    applyChatUI();
  }
  renderSessions();

  const meta = sessions.find((x) => x.id === sid);
  if (meta && /^(New task|Chat)/.test(meta.name)) {
    const nm = (text.length > 40 ? text.slice(0, 40) + "…" : text).trim();
    if (nm) {
      void api.renameSession(sid, nm).then(async () => {
        await refreshSessions();
        updateChatTitle();
        updateChatBanner();
      });
    }
  }

  try {
    s.sending = true;
    const ctx = await expandAttachments(text);
    const urls = onScreen ? attachments.map((a) => a.url).filter(Boolean) : [];
    s.liveIn = Math.floor((text.length + ctx.length) / 4);
    if (sid === currentSession) sbUpdate();
    await maybeCompact(sid);
    // Stop pressed while we were still preparing (reading @files, checking the
    // context, compacting): there was no run to cancel, so honour it here
    // instead of starting the one the user just stopped.
    if (s.stopping) {
      abortBeforeStart(sid, text);
      return;
    }
    await api.sendText(sid, ctx ? text + "\n\n## Referenced files\n" + ctx : text, urls);
    // Stop landed while the send itself was in flight, before the backend had
    // registered the run. There is a flag to set now, so re-issue it.
    if (s.stopping) void api.cancelRun(sid);
    if (onScreen) {
      attachments = [];
      renderAttachments();
    }
  } catch (e) {
    s.running = false;
    s.stopping = false;
    s.startedAt = 0;
    s.errored = true;
    chatState[sid] = "error";
    renderSessions();
    if (sid === currentSession) setError(String(e));
    else notify("“" + sessionName(sid) + "”: " + String(e), "error");
  } finally {
    s.sending = false;
  }
}

api.onEngineEvent((ev) => {
  dispatchToPlugins(ev);

  // Plugin tool calls carry a correlation id in `sid`, not a chat id.
  if (ev.type === "plugin_tool_call") {
    const fn = pluginReg.get(ev.name);
    if (!fn) {
      void api.pluginToolResult(ev.sid, false, "unknown plugin tool: " + ev.name);
      return;
    }
    let args: Record<string, unknown> = {};
    try { args = JSON.parse(ev.arguments); } catch { /* ignore */ }
    void (async () => {
      try {
        const out = await fn(args);
        void api.pluginToolResult(ev.sid, true, typeof out === "string" ? out : JSON.stringify(out));
      } catch (e) {
        void api.pluginToolResult(ev.sid, false, String(e));
      }
    })();
    return;
  }

  // A guard plugin gets to refuse a tool call before it runs.
  if (ev.type === "plugin_veto_request") {
    void answerVeto(ev);
    return;
  }

  const sid = (ev as unknown as { sid?: string }).sid || "";
  if (!sid) return;
  const chat = ui(sid);
  const onScreen = sid === currentSession;

  // Any sign of forward progress ends a backoff countdown — the wait is over
  // the moment the retried request starts answering (or finally gives up).
  if (ev.type !== "retry" && chat.retry) {
    chat.retry = null;
    // Repaint now rather than leaving a dead countdown on the strip: the
    // token/tool events that follow a successful retry don't touch it.
    if (onScreen) renderActivity();
  }

  // ---- bookkeeping: happens for every chat, on screen or not ----
  let sidebarDirty = false;
  switch (ev.type) {
    case "retry":
      // A backoff announced after the user pressed Stop is one we are about to
      // cut short; showing its countdown would bury the "stopping…" that says
      // the press landed.
      if (chat.stopping) break;
      // Held as a deadline; the activity strip counts it down every second.
      chat.retry = { until: Date.now() + (ev.delayMs || 0), attempt: ev.attempt, max: ev.max, reason: ev.reason };
      break;
    case "token":
      chat.liveText += ev.text;
      chat.liveOut += ev.text.length / 4;
      break;
    case "reasoning":
      chat.liveReason += ev.text;
      break;
    case "message_end":
      chat.liveText = "";
      chat.liveReason = "";
      break;
    case "activity":
      chat.activityText = ev.tool ? "running " + ev.tool : ev.phase === "thinking" ? "thinking…" : ev.phase;
      chat.activityStep = ev.step ? "step " + ev.step : "";
      break;
    case "approval_request":
      chat.approval = { id: ev.id, tool: ev.tool, preview: ev.preview || "" };
      if (!onScreen) notify("“" + sessionName(sid) + "” needs approval for " + ev.tool, "info");
      break;
    case "approval_close":
      if (chat.approval && chat.approval.id === ev.id) chat.approval = null;
      break;
    case "summary":
      chat.baseIn += ev.tokensIn || 0;
      chat.baseOut += ev.tokensOut || 0;
      if (ev.contextTokens) chat.ctxIn = ev.contextTokens;
      chat.liveIn = 0;
      chat.liveOut = 0;
      if (typeof ev.cost === "number" && !isNaN(ev.cost)) {
        chat.costKnown = true;
        chat.money += ev.cost;
      }
      break;
    case "error":
      chat.errored = true;
      chat.running = false;
      chat.stopping = false;
      chat.retry = null;
      chat.startedAt = 0;
      chat.approval = null;
      chatState[sid] = "error";
      sidebarDirty = true;
      // A background chat used to fail with nothing but a dot in the sidebar.
      if (!onScreen) notify("“" + sessionName(sid) + "” failed — " + explainError(ev.message).title, "error");
      break;
    case "done":
      chat.running = false;
      chat.stopping = false;
      // A run that ends during a backoff leaves the deadline behind; the next
      // run would open under a countdown that expired minutes ago.
      chat.retry = null;
      chat.startedAt = 0;
      chat.activityText = "";
      chat.activityStep = "";
      chat.approval = null;
      chat.liveText = "";
      chat.liveReason = "";
      if (!chat.errored) chatState[sid] = "idle";
      sidebarDirty = true;
      break;
  }
  if (ev.type === "token" || ev.type === "tool_call" || ev.type === "reasoning") {
    if (!chat.running) chat.running = true;
    if (chatState[sid] !== "busy") {
      chatState[sid] = "busy";
      sidebarDirty = true;
    }
  }
  if (sidebarDirty) renderSessions();

  // ---- DOM: only for the chat currently on screen ----
  if (onScreen) {
    switch (ev.type) {
      case "token": {
        let t = lastTurn();
        if (!t || !t.hasCaret) t = addAssistantTurn();
        t.raw += ev.text;
        setText(t);
        sbUpdate();
        scrollBottom();
        break;
      }
      case "reasoning": {
        let t = lastTurn();
        if (!t || !t.hasCaret) t = addAssistantTurn();
        thinkBody(t).textContent += ev.text;
        thinkTouch(t);
        scrollBottom();
        break;
      }
      case "tool_call": {
        let t = lastTurn();
        if (!t || !t.hasCaret) t = addAssistantTurn();
        addToolCard(t, ev.id, ev.name, ev.arguments);
        break;
      }
      case "tool_result":
        resolveTool(ev.id, ev.success, ev.output);
        break;
      case "message_end": {
        const t = lastTurn();
        if (t && t.role === "assistant") {
          removeCaret(t);
          if (t.thinkEl) t.thinkEl.classList.remove("live");
          renderInto(t.textEl, t.raw);
        }
        break;
      }
      case "activity":
      case "retry":
        renderActivity();
        break;
      case "approval_request":
      case "approval_close":
        renderActivity();
        break;
      case "summary":
        sbUpdate();
        addSummaryCard(ev);
        break;
      case "done": {
        // Only ever re-render an assistant turn: a user turn's text element
        // *is* its body, so re-rendering one drops the error and summary cards
        // a failed run just put there.
        const t = lastTurn();
        if (t && t.role === "assistant") {
          removeCaret(t);
          renderInto(t.textEl, t.raw);
        }
        applyChatUI();
        break;
      }
      case "error":
        setError(ev.message);
        break;
    }
  }

  // ---- flush this chat's queued message once it's free ----
  if (ev.type === "done" && chat.queued) {
    const q = chat.queued;
    chat.queued = "";
    if (onScreen) renderQueued();
    setTimeout(() => void startRun(sid, q), 10);
  }
});

// ---------- model picker (every enabled model, grouped by provider) ----------
const pickerMenu = document.getElementById("picker-menu") as HTMLElement;
const pickerInput = document.getElementById("picker-input") as HTMLInputElement;
const pickerList = document.getElementById("picker-list") as HTMLElement;

/** Levels to offer when a provider says a model reasons but not with which
 *  levels. The OpenAI-compatible set, which is what `reasoning_effort` means. */
const DEFAULT_EFFORTS = ["minimal", "low", "medium", "high"];
const EFFORT_LABEL: Record<string, string> = { minimal: "min", medium: "med" };

/** Levels to show for a model: the ones its provider named, or the standard
 *  set when it only said "this one reasons". */
function effortsFor(c: api.ModelChoice): string[] {
  const named = (c.reasoning_efforts || []).filter((e) => e.trim());
  return named.length ? named : DEFAULT_EFFORTS;
}

/** Whether the level chips belong on this row. A provider that explicitly said
 *  the model takes no level hides them; one that said nothing does not, since
 *  plenty of gateways describe nothing at all — but then only the model in use
 *  shows them, so a long list stays readable. */
function showEfforts(c: api.ModelChoice, active: boolean): boolean {
  if (c.reasoning === false) return false;
  return c.reasoning === true || active;
}

/** What the title-bar pill says. A level that was dialled in belongs there:
 *  asking a model to think hard is a standing choice, not a hidden one. */
function pillText(): string {
  if (!currentModel) return "no model";
  const c = catalog.find((x) => x.model === currentModel && x.provider_id === currentProviderId);
  const e = (c?.reasoning_effort || "").trim();
  return e ? `${currentModel} · ${EFFORT_LABEL[e] || e}` : currentModel;
}

function renderPicker(): void {
  const q = pickerInput.value.trim().toLowerCase();
  // Matching the provider name too means "openai" finds that provider's whole
  // shelf, not just models that happen to spell it out in their id.
  const list = q
    ? catalog.filter((c) => c.model.toLowerCase().includes(q) || c.provider_name.toLowerCase().includes(q))
    : catalog;
  pickerList.innerHTML = "";
  if (!list.length) {
    const e = document.createElement("div");
    e.className = "picker-empty";
    e.textContent = catalog.length
      ? "No matching models"
      : "No models enabled — turn a provider on in Settings (⚙)";
    pickerList.appendChild(e);
    return;
  }
  let group = "";
  list.forEach((c) => {
    if (c.provider_id !== group) {
      group = c.provider_id;
      const h = document.createElement("div");
      h.className = "picker-group";
      h.textContent = c.provider_name;
      pickerList.appendChild(h);
    }
    const active = c.model === currentModel && c.provider_id === currentProviderId;
    const it = document.createElement("div");
    it.className = "picker-item mdl" + (active ? " active" : "");

    const top = document.createElement("div");
    top.className = "mdl-top";
    const name = document.createElement("span");
    name.className = "mdl-name";
    name.textContent = c.model;
    const win = document.createElement("span");
    // A guessed window is dimmed: it is the global fallback, not something the
    // provider actually promised, and compaction fires off the number shown.
    win.className = "mdl-win" + (c.window_known ? "" : " guess");
    win.textContent = fmtTokens(c.context_window);
    win.title = c.window_known
      ? `Context window: ${c.context_window.toLocaleString()} tokens`
      : `No window advertised for this model — using the ${c.context_window.toLocaleString()} token default. Set one in Settings.`;
    top.append(name, win);
    it.appendChild(top);

    if (showEfforts(c, active)) {
      const row = document.createElement("div");
      row.className = "mdl-efforts";
      const chosen = (c.reasoning_effort || "").trim();
      [""].concat(effortsFor(c)).forEach((e) => {
        const b = document.createElement("button");
        b.type = "button";
        b.className = "eff" + (e === chosen ? " on" : "");
        b.textContent = e ? EFFORT_LABEL[e] || e : "auto";
        b.title = e
          ? `Ask this model to think at "${e}"`
          : "Send no reasoning level — whatever the provider defaults to";
        // Tuning the level keeps the picker open: dialling effort and picking
        // a model are different intentions.
        b.addEventListener("click", (ev) => {
          ev.stopPropagation();
          void selectModel(c, e, true);
        });
        row.appendChild(b);
      });
      it.appendChild(row);
    }

    it.addEventListener("click", () => void selectModel(c));
    pickerList.appendChild(it);
  });
}
function openPicker(): void {
  pickerInput.value = "";
  renderPicker();
  pickerMenu.hidden = false;
  pickerInput.focus();
}
function closePicker(): void {
  pickerMenu.hidden = true;
}
/// Point the app at a model, optionally setting the reasoning level to ask it
/// for. `effort` is only written when passed, so plain model switching never
/// disturbs a level that was already dialled in.
async function selectModel(c: api.ModelChoice, effort?: string, keepOpen = false): Promise<void> {
  try {
    const cfg = await api.getConfig();
    cfg.model = c.model;
    // Carry the provider: the same model id can be served by more than one, so
    // the backend must not have to guess which connection was meant.
    cfg.provider_id = c.provider_id;
    if (effort !== undefined) {
      const p = (cfg.providers || []).find((x) => x.id === c.provider_id);
      if (p) metaOf(p, c.model).reasoning_effort = effort || null;
    }
    await api.saveConfig(cfg);
    currentModel = c.model;
    currentProviderId = c.provider_id;
    // Re-read rather than patching the row: the backend is the arbiter of what
    // actually stuck, here as everywhere else.
    catalog = await api.listModels().catch(() => catalog);
    modelPill.textContent = pillText();
    sbModel.textContent = c.model;
    sbProv.textContent = "[" + c.provider_name + "]";
    if (currentSession) await api.setSessionModel(currentSession, c.model, c.provider_id);
    // The budget moves with the model immediately. Compaction does not: if this
    // model's window is smaller than what the chat is already carrying, the
    // status bar says so and the next send deals with it — a mis-picked model
    // must be undoable by picking the right one back.
    await refreshCtxWindow(currentSession);
    sbUpdate();
    if (keepOpen) {
      renderPicker();
      return;
    }
  } catch (e) {
    statusText.textContent = "save failed";
    console.error(e);
  }
  closePicker();
}
pickerInput.addEventListener("input", renderPicker);
modelPill.addEventListener("click", (e) => {
  e.stopPropagation();
  openPicker();
});
document.addEventListener("click", (e) => {
  const t = e.target as HTMLElement;
  if (!pickerMenu.hidden && !t.closest("#picker")) closePicker();
  if (!slashMenu.hidden && !t.closest("#composer")) closeSlash();
});

// ---------- image paste + run summary ----------
let attachments: { url: string; name: string }[] = [];

function renderAttachments(): void {
  const box = document.getElementById("attachments") as HTMLElement;
  box.innerHTML = "";
  attachments.forEach((a, i) => {
    const t = document.createElement("div");
    t.className = "att";
    t.style.backgroundImage = `url(${a.url})`;
    t.title = a.name;
    const x = document.createElement("button");
    x.className = "att-x";
    x.textContent = "×";
    x.addEventListener("click", () => {
      attachments.splice(i, 1);
      renderAttachments();
    });
    t.appendChild(x);
    box.appendChild(t);
  });
  box.style.display = attachments.length ? "flex" : "none";
}

input.addEventListener("paste", (e: ClipboardEvent) => {
  const items = e.clipboardData?.items;
  if (!items) return;
  let handled = false;
  for (const it of items) {
    if (it.kind !== "file") continue;
    const file = it.getAsFile();
    if (!file) continue;
    if (file.size > 500_000) {
      // Large file: save to workspace and reference
      const reader = new FileReader();
      reader.onload = () => {
        const data = String(reader.result);
        const isImg = file.type.startsWith("image/");
        attachments.push({
          url: isImg ? data : "",
          name: file.name || "pasted file",
        });
        renderAttachments();
      };
      reader.readAsDataURL(file);
      handled = true;
      e.preventDefault();
      break;
    } else if (it.type.startsWith("image/")) {
      const reader = new FileReader();
      reader.onload = () => {
        attachments.push({ url: String(reader.result), name: file.name || "pasted image" });
        renderAttachments();
      };
      reader.readAsDataURL(file);
      handled = true;
      e.preventDefault();
      break;
    } else if (it.type === "text/plain") {
      // large text paste -> dump to file

      // let text through normally for small pastes
      break;
    }
  }
  if (handled) e.preventDefault();
});

function addSummaryCard(s: { steps: number; tools: number; stopped: boolean; error: string | null }): void {
  const t = outputTurn();
  const bits: string[] = [`${s.steps} step${s.steps === 1 ? "" : "s"}`];
  if (s.tools) bits.push(`${s.tools} tool call${s.tools === 1 ? "" : "s"}`);
  const div = document.createElement("div");
  div.className = "summary" + (s.error ? " err" : "");
  // The error itself is already shown as a card by the `error` event; repeating
  // the raw provider payload here is what made failures look like a wall of JSON.
  div.textContent = s.error
    ? `failed — ${bits.join(" · ")}`
    : `${s.stopped ? "stopped" : "done"} — ${bits.join(" · ")}`;
  if (!s.error && s.tools > 0) {
    const sid = currentSession;
    const rv = document.createElement("button");
    rv.className = "revert-btn";
    rv.textContent = "↩ revert";
    rv.title = "Restore files to pre-run state";
    rv.addEventListener("click", () => {
      void (async () => {
        rv.disabled = true;
        rv.textContent = "reverting…";
        const ok = await api.workspaceRevert(sid);
        rv.textContent = ok ? "✓ reverted" : "↩ revert";
        rv.disabled = ok;
        notify(ok ? "Workspace reverted" : "Revert failed", ok ? "info" : "error");
      })();
    });
    div.appendChild(rv);
  }
  t.body.appendChild(div);
  scrollBottom();
}

// ---------- projects + sessions (left sidebar) ----------
/// Name of the built-in bucket for work that belongs to no project. Chats there
/// run in a scratch folder, not in whatever project was opened last.
const DEFAULT_PROJECT = "Tasks";
const sidebar = document.getElementById("sidebar") as HTMLElement;
const sessList = document.getElementById("sess-list") as HTMLElement;
const projAdd = document.getElementById("proj-add") as HTMLButtonElement;
const pinBtn = document.getElementById("sess-pin") as HTMLButtonElement;
let projects: api.ProjectMeta[] = [];
let currentProject = "";
let sessions: api.SessionMetaItem[] = [];
let currentSession = "";
/// Chats already announced with `chat_open`. A plugin should hear "this chat
/// appeared" once, not again every time you switch back to it.
const seenChats = new Set<string>();
const chatState: Record<string, string> = {};
const openProjs = new Set<string>();

async function pickFolder(): Promise<string | null> {
  try {
    const { open } = await import("@tauri-apps/plugin-dialog");
    const r: unknown = await open({ directory: true, multiple: false, title: "Choose project folder" });
    return typeof r === "string" ? r : null;
  } catch {
    return null;
  }
}
let pinned = localStorage.getItem("e:pin") === "1";
pinBtn.classList.toggle("pinned", pinned);
sidebar.classList.toggle("pinned", pinned);
pinBtn.title = pinned ? "Unpin sidebar" : "Pin sidebar";

function addStaticAssistant(content: string, reasoning = ""): Turn {
  const el = document.createElement("div");
  el.className = "turn assistant";
  el.innerHTML = `<div class="role">e</div><div class="body assistant"><div class="text"></div></div>`;
  const textEl = el.querySelector(".text") as HTMLElement;
  const bodyEl = el.querySelector(".body") as HTMLElement;
  renderInto(textEl, content);
  conv.appendChild(el);
  const t: Turn = { el, body: bodyEl, textEl, role: "assistant", raw: content, hasCaret: false, tools: new Map() };
  attachTurnCopy(el, "reply", () => t.raw);
  if (reasoning) {
    // Persisted thinking starts collapsed; live reasoning appends into it.
    thinkBody(t).textContent = reasoning;
    thinkTouch(t);
    t.thinkEl?.classList.remove("live");
  }
  turns.push(t);
  return t;
}

function updateChatTitle(): void {
  const p = projects.find((x) => x.id === currentProject);
  const s = sessions.find((x) => x.id === currentSession);
  chatTitle.textContent = ((p ? p.name : DEFAULT_PROJECT) + " / " + (s ? s.name : "New task"));
}

function updateChatBanner(): void {
  const p = projects.find((x) => x.id === currentProject);
  const sess = sessions.find((x) => x.id === currentSession);
  const el = document.getElementById("chat-banner");
  if (el) el.textContent = ((p ? p.name : DEFAULT_PROJECT) + " / " + (sess ? sess.name : "New task"));
}

/// Seed context size for a chat we have never run in this app session. The
/// provider's real count replaces this after the first run; without it a large
/// restored history would sail past the window before we ever measured it.
function seedContextEstimate(sid: string, estimate: number): void {
  const s = ui(sid);
  if (!s.ctxIn) s.ctxIn = estimate || 0;
}

function renderHistory(messages: { role: string; content: string; reasoning?: string; error?: string }[]): void {
  conv.innerHTML = "";
  turns = [];
  for (const m of messages) {
    if (m.role === "compaction") conv.appendChild(compactionMark());
    else if (m.role === "user" && m.content) addUserTurn(m.content);
    else if (m.role === "assistant" && (m.content || m.reasoning || m.error)) {
      const t = addStaticAssistant(m.content, m.reasoning || "");
      // Failures are part of the transcript, so a chat that died stays
      // explained after a switch or restart instead of just showing a red dot.
      if (m.error) t.body.appendChild(errorCard(m.error));
    }
  }
  updateEmpty();
  scrollBottom();
}

/// Row overflow menu (the `⋯` on projects and chats). Lives in `body` because
/// the sidebar's list scrolls with `overflow` set, which would clip a menu
/// anchored inside a row.
type RowMenuItem = { label: string; danger?: boolean; run: () => void };
let rowMenu: HTMLElement | null = null;
let rowMenuOwner: HTMLElement | null = null;

function closeRowMenu(): void {
  rowMenu?.remove();
  rowMenu = null;
  rowMenuOwner?.classList.remove("menu-open");
  rowMenuOwner = null;
}

function openRowMenu(anchor: HTMLElement, owner: HTMLElement, items: RowMenuItem[]): void {
  closeRowMenu();
  const menu = document.createElement("div");
  menu.className = "row-menu";
  for (const it of items) {
    const b = document.createElement("button");
    b.type = "button";
    b.className = "row-menu-item" + (it.danger ? " danger" : "");
    b.textContent = it.label;
    b.addEventListener("click", (e) => {
      e.stopPropagation();
      closeRowMenu();
      it.run();
    });
    menu.appendChild(b);
  }
  document.body.appendChild(menu);
  rowMenu = menu;
  rowMenuOwner = owner;
  owner.classList.add("menu-open");
  // Right-align under the button, then flip up / pull inside the viewport so a
  // row near the bottom of the list doesn't push the menu off screen.
  const r = anchor.getBoundingClientRect();
  const w = menu.offsetWidth;
  const h = menu.offsetHeight;
  menu.style.left = Math.max(8, Math.min(r.right - w, window.innerWidth - w - 8)) + "px";
  menu.style.top = (r.bottom + 4 + h > window.innerHeight ? Math.max(8, r.top - 4 - h) : r.bottom + 4) + "px";
}

document.addEventListener("click", (e) => {
  if (rowMenu && !(e.target as HTMLElement).closest(".row-menu")) closeRowMenu();
});
// Capture, so Escape dismisses the menu instead of reaching the composer and
// stopping the run behind it.
document.addEventListener("keydown", (e) => {
  if (e.key === "Escape" && rowMenu) {
    e.preventDefault();
    e.stopPropagation();
    closeRowMenu();
  }
}, true);
window.addEventListener("resize", closeRowMenu);
sessList.addEventListener("scroll", closeRowMenu);

function renderSessions(): void {
  closeRowMenu();
  sessList.innerHTML = "";
  projects.forEach((p) => {
    const open = openProjs.has(p.id);
    const row = document.createElement("div");
    row.className = "sess-item proj-folder" + (p.id === currentProject ? " active" : "");
    const lab = document.createElement("span");
    lab.className = "sess-label";
    lab.textContent = (open ? "▾ " : "▸ ") + (p.name || p.id);
    const padd = document.createElement("button");
    padd.className = "sess-act add";
    padd.textContent = "+";
    padd.title = "New chat in this project";
    const pmore = document.createElement("button");
    pmore.className = "sess-act more";
    pmore.textContent = "⋯";
    pmore.title = "More";
    row.append(lab, padd, pmore);
    padd.addEventListener("click", (e) => {
      e.stopPropagation();
      void (async () => {
        openProjs.add(p.id);
        const meta = await api.newSession("", undefined, currentModel, currentProviderId, p.id);
        await refreshSessions();
        if (meta.id) await loadSession(meta.id);
      })();
    });
    pmore.addEventListener("click", (e) => {
      e.stopPropagation();
      openRowMenu(pmore, row, [
        { label: "Rename", run: () => openRenameModal(p.id, p.name || p.id, p.workspace || "") },
        // Chats orphaned by any other deletion fall back to the scratch project,
        // so it gets no Close entry at all rather than one that quietly fails.
        ...(p.scratch
          ? []
          : [{
              label: "Close",
              danger: true,
              run: () => void (async () => {
                if (!(await confirmModal("Close project \"" + p.name + "\"? Its chats are kept and move to the remaining folder."))) return;
                await api.projectRemove(p.id);
                await refreshSessions();
                if (currentSession) await loadSession(currentSession);
              })(),
            }]),
      ]);
    });
    row.addEventListener("click", () => {
      void (async () => {
        if (openProjs.has(p.id)) openProjs.delete(p.id);
        else openProjs.add(p.id);
        if (p.id !== currentProject) {
          currentProject = p.id;
          // Awaited: a new chat is created in whatever project the backend
          // thinks is current, so the switch has to land before that can run.
          await api.switchProject(p.id);
        }
        renderSessions();
        await updateWorkspaceLabel();
      })();
    });
    sessList.appendChild(row);
    if (!open) return;
    sessions
      .filter((s) => s.project === p.id)
      .forEach((s) => {
        const c = document.createElement("div");
        c.className = "sess-item sub" + (s.id === currentSession ? " active" : "");
        const st = chatState[s.id] || s.state || "idle";
        const dot = document.createElement("span");
        dot.className = "sess-dot " + st;
        dot.title = st;
        const label = document.createElement("span");
        label.className = "sess-label";
        label.textContent = s.name;
        // A detached chat sits under this project but runs somewhere else, so
        // say where rather than letting the folder it is filed under imply it.
        label.title = s.detached
          ? `Open: ${s.name}\nRuns in ${s.workspace} — not this project's folder (its original project was deleted).`
          : "Open: " + s.name;
        const more = document.createElement("button");
        more.className = "sess-act more";
        more.textContent = "⋯";
        more.title = "More";
        c.append(dot, label, more);
        label.addEventListener("click", () => void loadSession(s.id));
        more.addEventListener("click", (e) => {
          e.stopPropagation();
          openRowMenu(more, c, [
            { label: "Rename", run: () => openChatRename(s.id, s.name) },
            { label: "Fork", run: () => void (async () => { await api.forkSession(s.id); await refreshSessions(); })() },
            {
              label: "Close",
              danger: true,
              run: () => void (async () => {
                if (!(await confirmModal("Close chat \"" + s.name + "\"? Its history is deleted."))) return;
                const wasCurrent = s.id === currentSession;
                await api.deleteSession(s.id);
                chatUI.delete(s.id);
                delete chatState[s.id];
                // A chat's terminals must not outlive it: the shells would keep
                // running against a project folder nothing on screen refers to.
                closePaneTabsFor(s.id);
                await refreshSessions();
                if (wasCurrent && currentSession) await loadSession(currentSession);
                else if (wasCurrent) {
                  renderHistory([]);
                  applyChatUI();
                }
              })(),
            },
          ]);
        });
        sessList.appendChild(c);
      });
  });
}

async function refreshSessions(): Promise<void> {
  const [r, pr] = await Promise.all([api.listSessions(), api.listProjects()]);
  sessions = r.sessions;
  const previous = currentSession;
  currentSession = r.current;
  projects = pr.projects;
  currentProject = pr.current;
  if (currentProject) openProjs.add(currentProject);
  // This is the other place the visible chat can change (boot, or a chat being
  // deleted out from under the current one), so the pane has to follow it here
  // as well as in `loadSession` — otherwise the tab strip keeps showing the
  // previous chat's tabs.
  if (previous !== currentSession) {
    renderPaneTabs();
    notifyPaneShown();
    if (currentSession) emitChatEvent({ type: "chat_switch", sid: currentSession, previous });
  }
  if (currentSession && !seenChats.has(currentSession)) {
    seenChats.add(currentSession);
    emitChatEvent({ type: "chat_open", sid: currentSession });
  }

  // The backend is the source of truth for "is this chat running". Persisted
  // `state` in the index can be stale (e.g. app killed mid-run), which used to
  // leave chats showing a spinner forever.
  const live = new Set(r.running);
  for (const s of sessions) {
    const u = ui(s.id);
    if (u.sending) continue; // send in flight, not registered yet
    u.running = live.has(s.id);
    if (!u.running && u.startedAt) {
      u.startedAt = 0;
      u.activityText = "";
      u.activityStep = "";
      u.retry = null;
      u.approval = null;
    }
    // A chat that died in a previous app session has its failure persisted but
    // no in-memory flag, so adopt the stored state — otherwise a restart makes
    // every past failure look like a clean idle chat.
    if (!u.running && s.state === "error") u.errored = true;
    chatState[s.id] = u.running ? "busy" : u.errored ? "error" : "idle";
  }
  // Drop UI state for chats that no longer exist.
  const alive = new Set(sessions.map((s) => s.id));
  for (const id of [...chatUI.keys()]) if (!alive.has(id)) chatUI.delete(id);

  renderSessions();
  await updateWorkspaceLabel();
}

/// Footer line telling the user where the current project's chats run. Kept
/// separate from `refreshSessions` so switching projects can update it without
/// also re-expanding folders the user just collapsed.
async function updateWorkspaceLabel(): Promise<void> {
  const chat = sessions.find((x) => x.id === currentSession);
  // A detached chat runs outside its project's folder, so showing the project's
  // path here would answer "where does my work happen" with the wrong folder.
  if (chat && chat.detached && chat.workspace) {
    sbWs.textContent = chat.workspace;
    sbWs.classList.toggle("missing", !(await api.pathIsDir(chat.workspace)));
    sbWs.title = `This chat runs here, outside any project: ${chat.workspace}`;
    return;
  }
  const p0 = projects.find((x) => x.id === currentProject);
  const ws = p0 && p0.workspace ? p0.workspace : "";
  // The scratch area does have a folder, but showing its path would imply a
  // project that isn't there. Say what it means and keep the path in the title.
  const scratch = !!(p0 && p0.scratch);
  sbWs.textContent = scratch ? "no project folder" : ws || ".";
  // Flag a missing folder here rather than letting every tool call fail on it.
  const ok = ws ? await api.pathIsDir(ws) : true;
  sbWs.classList.toggle("missing", !ok && !scratch);
  sbWs.title = scratch
    ? `One-off work, outside any project. Scratch folder: ${ws}`
    : ok
      ? ws
      : `Folder not found: ${ws} — open the project's ⋯ menu and rename it to pick another`;
}

async function loadSession(id: string): Promise<void> {
  const s0 = sessions.find((x) => x.id === id);
  if (s0) {
    const proj = s0.project;
    if (proj) {
      openProjs.add(proj);
      if (proj !== currentProject) {
        currentProject = proj;
        void api.switchProject(proj);
      }
    }
  }
  await api.switchSession(id);
  const previous = currentSession;
  currentSession = id;
  // The pane belongs to the chat, so it has to swap before anything else can
  // render against it — a tab left from the previous chat is a terminal
  // pointing at the previous project's folder.
  renderPaneTabs();
  notifyPaneShown();
  if (previous !== id) emitChatEvent({ type: "chat_switch", sid: id, previous });
  if (!seenChats.has(id)) {
    seenChats.add(id);
    emitChatEvent({ type: "chat_open", sid: id });
  }
  const g = await api.getSession(id);
  await applySessionModel(g.model, g.provider);
  renderHistory(g.messages);
  seedContextEstimate(id, g.context_estimate);

  // Trust the backend about whether this chat is actually running; the UI's own
  // flag can be stale if a run finished while we were looking elsewhere.
  const s = ui(id);
  s.running = !!g.running;
  if (!s.running) {
    s.startedAt = 0;
    s.activityText = "";
    s.activityStep = "";
    s.retry = null;
    s.liveText = "";
    s.liveReason = "";
    s.approval = null;
  } else if (s.liveText || s.liveReason) {
    // Re-attach the in-flight message so switching away mid-run loses nothing.
    const t = addAssistantTurn();
    if (s.liveReason) {
      thinkBody(t).textContent = s.liveReason;
      thinkTouch(t);
    }
    t.raw = s.liveText;
    setText(t);
    scrollBottom();
  }
  chatState[id] = s.running ? "busy" : chatState[id] === "error" || s.errored ? "error" : "idle";

  // Input carries the draft of the chat you're leaving, not the one you enter.
  input.value = "";
  autoGrow();
  closeSessions();
  await refreshSessions();
  applyChatUI();
  updateChatTitle();
  updateChatBanner();
}

function openSessions(): void {
  void refreshSessions();
  sidebar.classList.add("open");
}
function closeSessions(): void {
  // A pinned sidebar stays put — switching chats, starting a new one or opening
  // a project used to close it regardless of the pin.
  if (pinned) return;
  sidebar.classList.remove("open");
}

document.getElementById("mark")!.addEventListener("click", (e) => {
  e.stopPropagation();
  if (sidebar.classList.contains("open")) closeSessions();
  else openSessions();
});
pinBtn.addEventListener("click", () => {
  pinned = !pinned;
  localStorage.setItem("e:pin", pinned ? "1" : "0");
  pinBtn.classList.toggle("pinned", pinned);
  sidebar.classList.toggle("pinned", pinned);
  pinBtn.title = pinned ? "Unpin sidebar" : "Pin sidebar";
  if (pinned) openSessions();
  else closeSessions();
});
sidebar.addEventListener("mouseleave", () => {
  // Reaching for a row's `⋯` menu means leaving the sidebar, which used to
  // slam it shut under the menu that was just opened.
  if (!pinned && !rowMenu) closeSessions();
});
projAdd.addEventListener("click", () => openProjModal());

// ---------- slash command popup ----------
type SlashCmd = { name: string; desc: string };

const BUILTIN_SLASH: SlashCmd[] = [
  { name: "/new", desc: "new conversation" },
  { name: "/model", desc: "switch model" },
  { name: "/settings", desc: "open settings" },
  { name: "/extensions", desc: "plugins, skills and MCP servers" },
  { name: "/reload", desc: "re-read plugins, skills and MCP servers" },
  { name: "/yolo", desc: "toggle auto-approval of risky tools" },
  { name: "/help", desc: "list commands" },
];

function slashCommands(): SlashCmd[] {
  const plugins = Object.entries(pluginCommands).map(([name, c]) => ({ name, desc: c.desc }));
  return [...BUILTIN_SLASH, ...plugins];
}

const slashMenu = document.createElement("div");
slashMenu.className = "picker-menu";
slashMenu.id = "slash-menu";
slashMenu.hidden = true;
slashMenu.innerHTML = `<div class="picker-list" id="slash-list"></div>`;
(document.getElementById("composer") as HTMLElement).appendChild(slashMenu);
const slashList = slashMenu.querySelector("#slash-list") as HTMLElement;
let slashMatches: SlashCmd[] = [];
let slashSel = 0;

function closeSlash(): void {
  slashMenu.hidden = true;
  slashMatches = [];
  slashSel = 0;
}

function renderSlash(): void {
  slashList.innerHTML = "";
  slashMatches.forEach((c, i) => {
    const it = document.createElement("div");
    it.className = "picker-item" + (i === slashSel ? " active" : "");
    const name = document.createElement("span");
    name.className = "slash-name";
    name.textContent = c.name;
    const desc = document.createElement("span");
    desc.className = "slash-desc";
    desc.textContent = c.desc;
    it.append(name, desc);
    it.addEventListener("mouseenter", () => {
      slashSel = i;
      renderSlash();
    });
    it.addEventListener("click", () => pickSlash());
    slashList.appendChild(it);
  });
  slashList.children[slashSel]?.scrollIntoView({ block: "nearest" });
}

/// The popup only lives while the input is a bare command token — as soon as an
/// argument or ordinary prose is typed it gets out of the way.
function refreshSlash(): void {
  const m = /^\/(\S*)$/.exec(input.value);
  if (!m) {
    closeSlash();
    return;
  }
  const q = "/" + m[1].toLowerCase();
  slashMatches = slashCommands().filter((c) => c.name.toLowerCase().startsWith(q));
  if (!slashMatches.length) {
    closeSlash();
    return;
  }
  slashSel = Math.min(slashSel, slashMatches.length - 1);
  slashMenu.hidden = false;
  renderSlash();
}

function moveSlash(delta: number): void {
  const n = slashMatches.length;
  slashSel = (slashSel + delta + n) % n;
  renderSlash();
}

function completeSlash(): void {
  const c = slashMatches[slashSel];
  if (!c) return;
  input.value = c.name;
  closeSlash();
  autoGrow();
  updateInputState();
}

function pickSlash(): void {
  const c = slashMatches[slashSel];
  if (!c) return;
  input.value = "";
  closeSlash();
  autoGrow();
  updateInputState();
  runSlash(c.name);
}

// ---------- input wiring ----------
input.addEventListener("input", () => {
  autoGrow();
  updateInputState();
  refreshSlash();
});
input.addEventListener("keydown", (e) => {
  // The popup claims its keys first: otherwise Enter would send the raw "/text"
  // as a message and Escape would cancel the run instead of closing the list.
  if (!slashMenu.hidden) {
    if (e.key === "ArrowDown" || e.key === "ArrowUp") {
      e.preventDefault();
      moveSlash(e.key === "ArrowDown" ? 1 : -1);
      return;
    }
    if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      pickSlash();
      return;
    }
    if (e.key === "Tab" || e.key === " ") {
      e.preventDefault();
      completeSlash();
      return;
    }
    if (e.key === "Escape") {
      e.preventDefault();
      closeSlash();
      return;
    }
  }
  // Ctrl+Enter = steer: stop the current run, then send this text to the same
  // chat as soon as it winds down.
  if (e.key === "Enter" && e.ctrlKey && isRunning()) {
    e.preventDefault();
    const t = input.value.trim();
    if (!t) return;
    const s = cur();
    s.queued = t;
    input.value = "";
    autoGrow();
    renderQueued();
    void stopCurrent();
    return;
  }
  if (e.key === "Enter" && !e.shiftKey) {
    e.preventDefault();
    void doSend();
    return;
  }
  if (e.key === "Escape" && isRunning()) {
    e.preventDefault();
    void stopCurrent();
  }
});

async function stopCurrent(): Promise<void> {
  if (!currentSession) return;
  const sid = currentSession;
  const s = ui(sid);
  if (!s.running) return;
  if (s.approval) {
    // A run parked on an approval prompt is waiting on the user, not the model.
    resolveApproval(false);
  }
  // Record the intent first: the run may not have reached the backend yet, and
  // this is what stops `startRun` handing it over after the fact.
  s.stopping = true;
  s.activityText = "stopping…";
  s.activityStep = "";
  // Drop any backoff countdown. It is recomputed on every tick and takes
  // priority over the activity text, so leaving it up means a press during a
  // retry wait shows nothing at all — the button looks broken while it works.
  s.retry = null;
  renderActivity();
  const ok = await api.cancelRun(sid);
  if (ok || s.sending || !s.stopping) return;
  // Backend has no run for this chat and none is on its way: our flag was stale.
  s.stopping = false;
  s.running = false;
  s.startedAt = 0;
  chatState[sid] = "idle";
  renderSessions();
  if (sid === currentSession) applyChatUI();
}

/// Undo a run the user stopped before the backend ever accepted it. No `done`
/// event is coming for a run that never started, so everything that event would
/// have settled has to be settled here.
function abortBeforeStart(sid: string, text: string): void {
  const s = ui(sid);
  s.stopping = false;
  s.running = false;
  s.startedAt = 0;
  s.activityText = "";
  s.activityStep = "";
  s.retry = null;
  s.liveIn = 0;
  chatState[sid] = "idle";
  renderSessions();

  const onScreen = sid === currentSession;
  if (onScreen) {
    // The message never reached the model or the stored history, so leaving its
    // bubble on screen would show a turn that vanishes on the next reload.
    const t = lastTurn();
    if (t && t.role === "user" && t.raw === text) {
      t.el.remove();
      turns.pop();
    }
    // Hand the text back rather than swallowing it — unless something is
    // already waiting to be typed or sent.
    if (!s.queued && !input.value.trim()) {
      input.value = text;
      autoGrow();
    }
    updateEmpty();
  }

  // Ctrl+Enter steers by queueing the replacement and stopping the run; with no
  // `done` event to flush it, that message would sit in the chip forever.
  const q = s.queued;
  if (q) {
    s.queued = "";
    if (onScreen) renderQueued();
    setTimeout(() => void startRun(sid, q), 10);
  } else if (onScreen) {
    applyChatUI();
  }
}

sendBtn.addEventListener("click", () => {
  if (isRunning()) void stopCurrent();
  else void doSend();
});
document.getElementById("btn-clear")!.addEventListener("click", (e) => {
  e.stopPropagation();
  openSessions();
});

// ---------- settings (a shelf of providers, not one "active" one) ----------
// Settings decides what is *available*: which providers are on, and which of
// their models are on. Which one a chat runs is picked from the model pill,
// so there is nothing here to mistake for "the active provider".
const overlay = document.createElement("div");
overlay.className = "overlay";
overlay.innerHTML = `
  <div class="modal">
    <h3>Settings</h3>
    <div class="field">
      <label>Providers</label>
      <p class="note prov-help">Every enabled model from every enabled provider shows up together in the model picker — click the model name in the title bar to switch.</p>
      <div id="provlist"></div>
      <div class="prov-row"><button id="cfg-addprov" type="button">+ New provider</button></div>
    </div>
    <div id="prov-edit" class="prov-edit" hidden>
      <div class="field"><label>Name</label><input id="cfg-pname" spellcheck="false"/></div>
      <div class="field"><label>API base URL</label><input id="cfg-base" spellcheck="false" placeholder="https://host/v1"/></div>
      <div class="field"><label>API key</label><input id="cfg-key" type="password" spellcheck="false"/></div>
      <div class="field"><label>Context window (tokens)</label><input id="cfg-ctxwin" type="number" step="1000" min="0" placeholder="1000000"/></div>
      <div class="field">
        <label>Models <span id="cfg-mcount" class="prov-count"></span></label>
        <div class="model-tools">
          <input id="cfg-mfilter" placeholder="Filter…" spellcheck="false"/>
          <button id="cfg-refresh" type="button" title="Fetch &lt;base&gt;/models">Refresh</button>
          <button id="cfg-mall" type="button" title="Enable every model">All</button>
          <button id="cfg-mnone" type="button" title="Disable every model">None</button>
        </div>
        <div id="cfg-models" class="model-list"></div>
        <div class="model-tools">
          <input id="cfg-madd" placeholder="Add a model id by hand…" spellcheck="false"/>
          <button id="cfg-maddbtn" type="button">Add</button>
        </div>
      </div>
      <div class="prov-row"><button id="cfg-delprov" type="button">Delete provider</button></div>
    </div>
    <details class="field bhr">
      <summary>Behavior</summary>
            <label class="lbl">Temperature</label><input id="cfg-temp" type="number" step="0.1" min="0" max="2"/>
      <label class="lbl">System prompt</label><textarea id="cfg-sys" rows="3"></textarea>
      <label class="lbl cfg-check"><input id="cfg-yolo" type="checkbox"/> YOLO mode — run shell &amp; write_file without asking</label>
    </details>
    <details class="field bhr" id="cfg-ext">
      <summary>Extensions</summary>
      <p class="note">Plugins, skills and MCP servers found in <code>~/.e/</code> and in this project's <code>.e/</code>. Changes apply on <b>Reload</b> — no restart.</p>
      <div id="ext-body"></div>
      <div class="prov-row"><button id="cfg-extreload" type="button">Reload extensions</button></div>
    </details>
    <p class="note">Stored in <code>~/.e/config.json</code>. &ldquo;Refresh&rdquo; fetches <code>&lt;base&gt;/models</code>.</p>
    <div class="modal-actions">
      <button id="cfg-cancel">Cancel</button>
      <button id="cfg-save" class="primary">Save</button>
    </div>
  </div>`;
document.body.appendChild(overlay);

/** Settings works on a copy, so Cancel discards edits instead of half-applying. */
let draft: ProviderItem[] = [];

const el = <T extends HTMLElement>(sel: string): T => overlay.querySelector(sel) as T;
const editing = (): ProviderItem | undefined => draft.find((p) => p.id === editingProviderId);

function modelOn(p: ProviderItem, m: string): boolean {
  return !(p.disabled_models || []).includes(m);
}

function setModelOn(p: ProviderItem, m: string, on: boolean): void {
  const off = new Set(p.disabled_models || []);
  if (on) off.delete(m);
  else off.add(m);
  p.disabled_models = [...off];
}

/** The metadata slot for one model, created on demand so a window can be set
 *  for a model the provider never described. */
function metaOf(p: ProviderItem, m: string): api.ModelMeta {
  const all = (p.model_meta = p.model_meta || {});
  return (all[m] = all[m] || {});
}

/** The window a model falls back to when nothing is set for it: what its
 *  provider advertised, then the provider-wide number, then the global one. */
function fallbackWindow(p: ProviderItem, m: string): number {
  return (p.model_meta || {})[m]?.advertised_window || p.context_window || defaultCtxWindow;
}

/** Read the open editor's text fields back into the draft. Called before any
 *  re-render or provider switch so typing is never silently thrown away. */
function commitProviderForm(): void {
  const p = editing();
  if (!p) return;
  p.name = el<HTMLInputElement>("#cfg-pname").value.trim() || p.id;
  p.base_url = el<HTMLInputElement>("#cfg-base").value.trim();
  p.api_key = el<HTMLInputElement>("#cfg-key").value.trim();
  const win = parseInt(el<HTMLInputElement>("#cfg-ctxwin").value, 10);
  p.context_window = win > 0 ? win : null;
  // Per-model windows commit on blur, which Save would otherwise race.
  overlay.querySelectorAll<HTMLInputElement>("#cfg-models .model-win").forEach((box) => {
    const m = box.dataset.model;
    if (m) setModelWindow(p, m, box.value);
  });
}

/** Store a hand-set window for one model. Blank clears the override and hands
 *  the model back to whatever its provider advertised. */
function setModelWindow(p: ProviderItem, m: string, raw: string): void {
  const n = parseInt(raw, 10);
  metaOf(p, m).window_override = n > 0 ? n : null;
}

function renderModelList(): void {
  const p = editing();
  const box = el<HTMLElement>("#cfg-models");
  const count = el<HTMLElement>("#cfg-mcount");
  box.innerHTML = "";
  if (!p) return;
  const on = p.models.filter((m) => modelOn(p, m)).length;
  count.textContent = p.models.length ? `${on}/${p.models.length} on` : "none yet — Refresh or add one";
  const q = el<HTMLInputElement>("#cfg-mfilter").value.trim().toLowerCase();
  const list = q ? p.models.filter((m) => m.toLowerCase().includes(q)) : p.models;
  if (!list.length) {
    const e = document.createElement("div");
    e.className = "picker-empty";
    e.textContent = p.models.length ? "No matching models" : "No models — hit Refresh, or add one by hand";
    box.appendChild(e);
    return;
  }
  list.forEach((m) => {
    const meta = (p.model_meta || {})[m] || {};
    const row = document.createElement("label");
    row.className = "model-row";
    const cb = document.createElement("input");
    cb.type = "checkbox";
    cb.checked = modelOn(p, m);
    const name = document.createElement("span");
    name.className = "model-name";
    name.textContent = m;
    if (meta.reasoning === true) {
      const tag = document.createElement("span");
      tag.className = "model-tag";
      tag.textContent = "thinks";
      tag.title = meta.reasoning_efforts?.length
        ? "Takes a reasoning level: " + meta.reasoning_efforts.join(", ")
        : "Takes a reasoning level — set it in the model picker";
      name.appendChild(tag);
    }
    // Per-model window: the provider's own figure is the placeholder, so the
    // box shows what will be used without pretending the user typed it.
    const win = document.createElement("input");
    win.type = "number";
    win.className = "model-win";
    win.min = "0";
    win.step = "1000";
    win.dataset.model = m;
    win.value = meta.window_override ? String(meta.window_override) : "";
    win.placeholder = fmtTokens(fallbackWindow(p, m));
    win.title = meta.advertised_window
      ? `Provider advertises ${meta.advertised_window.toLocaleString()} tokens. Type a number to override it.`
      : `Nothing advertised for this model — using ${fallbackWindow(p, m).toLocaleString()} tokens. Type a number to set one.`;
    const drop = document.createElement("button");
    drop.className = "sess-act";
    drop.type = "button";
    drop.textContent = "×";
    drop.title = "Remove from the list";
    cb.addEventListener("change", () => {
      setModelOn(p, m, cb.checked);
      renderModelList();
      renderProviderList();
    });
    win.addEventListener("click", (e) => e.stopPropagation());
    win.addEventListener("change", () => {
      setModelWindow(p, m, win.value);
      win.placeholder = fmtTokens(fallbackWindow(p, m));
    });
    drop.addEventListener("click", (e) => {
      e.preventDefault();
      p.models = p.models.filter((x) => x !== m);
      p.disabled_models = (p.disabled_models || []).filter((x) => x !== m);
      delete (p.model_meta || {})[m];
      renderModelList();
      renderProviderList();
    });
    row.append(cb, name, win, drop);
    box.appendChild(row);
  });
}

function loadProviderToForm(p?: ProviderItem): void {
  if (!p) return;
  editingProviderId = p.id;
  el<HTMLInputElement>("#cfg-pname").value = p.name || p.id;
  el<HTMLInputElement>("#cfg-base").value = p.base_url;
  el<HTMLInputElement>("#cfg-key").value = p.api_key;
  el<HTMLInputElement>("#cfg-ctxwin").value = p.context_window ? String(p.context_window) : "";
  el<HTMLInputElement>("#cfg-ctxwin").placeholder = String(defaultCtxWindow);
  el<HTMLInputElement>("#cfg-mfilter").value = "";
  el<HTMLInputElement>("#cfg-madd").value = "";
  renderModelList();
  el<HTMLElement>("#prov-edit").hidden = false;
}

function closeProviderForm(): void {
  editingProviderId = "";
  el<HTMLElement>("#prov-edit").hidden = true;
  renderProviderList();
}

function renderProviderList(): void {
  const box = el<HTMLElement>("#provlist");
  box.innerHTML = "";
  if (!draft.length) {
    const e = document.createElement("div");
    e.className = "picker-empty";
    e.textContent = "No providers yet";
    box.appendChild(e);
    return;
  }
  draft.forEach((p) => {
    const row = document.createElement("div");
    row.className = "prov-item" + (p.id === editingProviderId ? " editing" : "") + (p.enabled ? "" : " off");
    const toggle = document.createElement("input");
    toggle.type = "checkbox";
    toggle.checked = p.enabled;
    toggle.title = p.enabled ? "Disable this provider" : "Enable this provider";
    const lab = document.createElement("span");
    lab.className = "prov-name";
    lab.textContent = (p.name || p.id) + (p.base_url ? "  ·  " + hostOf(p.base_url) : "  ·  no URL");
    const count = document.createElement("span");
    count.className = "prov-count";
    const on = p.models.filter((m) => modelOn(p, m)).length;
    count.textContent = p.models.length ? `${on}/${p.models.length}` : "0";
    count.title = "Models enabled";
    const edit = document.createElement("button");
    edit.className = "sess-act";
    edit.type = "button";
    edit.textContent = "✎";
    edit.title = "Edit provider";
    const del = document.createElement("button");
    del.className = "sess-act";
    del.type = "button";
    del.textContent = "×";
    del.title = "Delete provider";
    row.append(toggle, lab, count, edit, del);
    toggle.addEventListener("change", () => {
      p.enabled = toggle.checked;
      renderProviderList();
    });
    lab.addEventListener("click", () => {
      commitProviderForm();
      if (p.id === editingProviderId) closeProviderForm();
      else {
        loadProviderToForm(p);
        renderProviderList();
      }
    });
    edit.addEventListener("click", () => {
      commitProviderForm();
      loadProviderToForm(p);
      renderProviderList();
    });
    del.addEventListener("click", async () => {
      if (!(await confirmModal('Delete provider "' + (p.name || p.id) + '"?'))) return;
      draft = draft.filter((x) => x.id !== p.id);
      if (editingProviderId === p.id) editingProviderId = "";
      renderProviderList();
      el<HTMLElement>("#prov-edit").hidden = !editing();
    });
    box.appendChild(row);
  });
}

el<HTMLInputElement>("#cfg-mfilter").addEventListener("input", renderModelList);

el<HTMLButtonElement>("#cfg-refresh").addEventListener("click", async () => {
  commitProviderForm();
  const p = editing();
  if (!p) return;
  if (!p.base_url) {
    statusText.textContent = "set a base URL first";
    return;
  }
  statusText.textContent = "refreshing…";
  try {
    // The backend does the folding-in: what the provider advertises is
    // refreshed, what the user set by hand is left alone. Assigning in place
    // keeps this the same draft object the editor is pointed at.
    Object.assign(p, await api.refreshModels(p));
    renderModelList();
    renderProviderList();
    const described = p.models.filter((m) => (p.model_meta || {})[m]?.advertised_window).length;
    statusText.textContent = described
      ? `${p.models.length} models · ${described} with a window`
      : `${p.models.length} models`;
  } catch (e) {
    statusText.textContent = "refresh failed";
    console.error(e);
  }
});

function addModelByHand(): void {
  const p = editing();
  const input = el<HTMLInputElement>("#cfg-madd");
  const m = input.value.trim();
  if (!p || !m) return;
  // Not every gateway lists everything it serves, so a model can be named by
  // hand instead of only through /models.
  if (!p.models.includes(m)) p.models.push(m);
  p.disabled_models = (p.disabled_models || []).filter((x) => x !== m);
  input.value = "";
  renderModelList();
  renderProviderList();
}
el<HTMLButtonElement>("#cfg-maddbtn").addEventListener("click", addModelByHand);
el<HTMLInputElement>("#cfg-madd").addEventListener("keydown", (e) => {
  if (e.key === "Enter") {
    e.preventDefault();
    addModelByHand();
  }
});

el<HTMLButtonElement>("#cfg-mall").addEventListener("click", () => {
  const p = editing();
  if (!p) return;
  p.disabled_models = [];
  renderModelList();
  renderProviderList();
});
el<HTMLButtonElement>("#cfg-mnone").addEventListener("click", () => {
  const p = editing();
  if (!p) return;
  p.disabled_models = [...p.models];
  renderModelList();
  renderProviderList();
});

el<HTMLButtonElement>("#cfg-addprov").addEventListener("click", () => {
  commitProviderForm();
  const p: ProviderItem = {
    id: "p" + Date.now().toString(36),
    name: "Provider " + (draft.length + 1),
    base_url: "",
    api_key: "",
    models: [],
    context_window: null,
    model_meta: {},
    enabled: true,
    disabled_models: [],
  };
  draft.push(p);
  loadProviderToForm(p);
  renderProviderList();
  el<HTMLInputElement>("#cfg-pname").focus();
});

el<HTMLButtonElement>("#cfg-delprov").addEventListener("click", async () => {
  const p = editing();
  if (!p) return;
  if (!(await confirmModal('Delete provider "' + (p.name || p.id) + '"?'))) return;
  draft = draft.filter((x) => x.id !== p.id);
  editingProviderId = "";
  el<HTMLElement>("#prov-edit").hidden = true;
  renderProviderList();
});

async function openSettings(extensions = false): Promise<void> {
  const cfg = await api.getConfig();
  providers = cfg.providers || [];
  draft = providers.map((p) => ({
    ...p,
    models: [...(p.models || [])],
    disabled_models: [...(p.disabled_models || [])],
    // Deep enough that editing a window in the draft can't reach through to
    // the saved config and make Cancel a lie.
    model_meta: Object.fromEntries(
      Object.entries(p.model_meta || {}).map(([m, meta]) => [m, { ...meta }]),
    ),
  }));
  currentWs = cfg.workspace;
  defaultCtxWindow = cfg.context_window || 1_000_000;
  editingProviderId = "";
  el<HTMLElement>("#prov-edit").hidden = true;
  el<HTMLInputElement>("#cfg-temp").value = String(cfg.temperature);
  el<HTMLTextAreaElement>("#cfg-sys").value = cfg.system;
  el<HTMLInputElement>("#cfg-yolo").checked = !!cfg.yolo;
  renderProviderList();
  const ext = el<HTMLDetailsElement>("#cfg-ext");
  ext.open = extensions;
  void renderExtensions().then(() => {
    if (extensions) ext.scrollIntoView({ block: "nearest" });
  });
  overlay.classList.add("open");
}

/// Everything the app found on disk, in one place: what loaded, what did not,
/// and why. A plugin that is broken or was refused a capability has to be
/// visible here — that is the whole review surface.
async function renderExtensions(): Promise<void> {
  const body = el<HTMLElement>("#ext-body");
  const ws = activeWorkspace();
  const [skills, servers] = await Promise.all([
    api.listSkills(ws || undefined).catch(() => [] as api.SkillMeta[]),
    api.listMcpServers().catch(() => [] as api.McpStatus[]),
  ]);
  body.textContent = "";
  const scope = document.createElement("p");
  scope.className = "note";
  scope.textContent = ws ? `This project: ${ws}` : "No project folder — global extensions only.";
  body.appendChild(scope);

  const section = (title: string, count: number, hint: string): HTMLElement => {
    const wrap = document.createElement("div");
    wrap.className = "ext-group";
    const h = document.createElement("div");
    h.className = "ext-head";
    h.innerHTML = `<span>${title}</span><span class="prov-count">${count}</span>`;
    wrap.appendChild(h);
    if (!count) {
      const p = document.createElement("p");
      p.className = "note";
      p.textContent = hint;
      wrap.appendChild(p);
    }
    body.appendChild(wrap);
    return wrap;
  };

  const chip = (text: string, cls = ""): HTMLElement => {
    const s = document.createElement("span");
    s.className = "ext-chip " + cls;
    s.textContent = text;
    return s;
  };

  const plugins = section("Plugins", pluginStatus.length, "Nothing in ~/.e/plugins yet — see docs/EXTENDING.md.");
  for (const p of pluginStatus) {
    const row = document.createElement("div");
    row.className = "ext-row";
    const top = document.createElement("div");
    top.className = "ext-top";
    // Only the box and the name toggle the plugin: a label wrapping the whole
    // row would turn a click on a capability chip into "switch this off".
    const toggle = document.createElement("label");
    toggle.className = "ext-toggle";
    const box = document.createElement("input");
    box.type = "checkbox";
    box.checked = p.enabled;
    box.addEventListener("change", () => {
      void (async () => {
        await api.setPluginEnabled(p.name, box.checked);
        await loadPlugins();
        await renderExtensions();
      })();
    });
    const title = document.createElement("span");
    title.className = "ext-name";
    title.textContent = p.display + (p.version ? " " + p.version : "");
    toggle.appendChild(box);
    toggle.appendChild(title);
    top.appendChild(toggle);
    top.appendChild(chip(p.scope, "scope"));
    for (const c of p.capabilities) top.appendChild(chip(c, "cap"));
    if (p.tools.length) top.appendChild(chip(p.tools.length + (p.tools.length === 1 ? " tool" : " tools")));
    if (p.commands.length) top.appendChild(chip(p.commands.join(" ")));
    // Name the views, not just how many: "2 views" tells you nothing about
    // which tab in the pane came from which folder on disk.
    for (const key of p.views) {
      const v = paneViews.get(key);
      top.appendChild(chip((v ? (v.icon ? v.icon + " " : "") + v.title : key) + " view"));
    }
    row.appendChild(top);

    const detail = document.createElement("p");
    detail.className = "note ext-note";
    detail.textContent = p.description || p.dir;
    row.appendChild(detail);

    const problems = [p.failure, ...p.notes].filter(Boolean);
    for (const problem of problems) {
      const err = document.createElement("p");
      err.className = "note ext-err";
      err.textContent = problem;
      row.appendChild(err);
    }
    if (!problems.length && p.enabled && !p.loaded) {
      const err = document.createElement("p");
      err.className = "note ext-err";
      err.textContent = "not loaded yet — hit Reload extensions";
      row.appendChild(err);
    }
    plugins.appendChild(row);
  }

  const skillGroup = section("Skills", skills.length, "Nothing in ~/.e/skills — a skill is a folder with a SKILL.md.");
  for (const s of skills) {
    const row = document.createElement("div");
    row.className = "ext-row";
    const top = document.createElement("div");
    top.className = "ext-top";
    const name = document.createElement("span");
    name.className = "ext-name";
    name.textContent = s.display;
    top.appendChild(name);
    top.appendChild(chip(s.scope, "scope"));
    top.appendChild(chip(s.name, "cap"));
    row.appendChild(top);
    const detail = document.createElement("p");
    detail.className = "note ext-note";
    detail.textContent = s.description || s.path;
    row.appendChild(detail);
    skillGroup.appendChild(row);
  }

  const mcp = section("MCP servers", servers.length, "No ~/.e/mcp.json — add one to merge an MCP server's tools.");
  for (const m of servers) {
    const row = document.createElement("div");
    row.className = "ext-row";
    const top = document.createElement("div");
    top.className = "ext-top";
    const name = document.createElement("span");
    name.className = "ext-name";
    name.textContent = m.name;
    top.appendChild(name);
    top.appendChild(chip(m.scope, "scope"));
    top.appendChild(chip(m.state, m.state === "error" ? "bad" : m.state === "ready" ? "ok" : ""));
    if (m.tools.length) top.appendChild(chip(m.tools.length + (m.tools.length === 1 ? " tool" : " tools")));
    row.appendChild(top);
    const detail = document.createElement("p");
    detail.className = "note ext-note";
    detail.textContent = m.command;
    row.appendChild(detail);
    if (m.error) {
      const err = document.createElement("p");
      err.className = "note ext-err";
      err.textContent = m.error;
      row.appendChild(err);
    }
    mcp.appendChild(row);
  }

  // Servers start in the background, so a row that says "starting" is a
  // promise to come back — otherwise Reload leaves the pane stuck on it.
  if (servers.some((m) => m.state === "starting") && overlay.classList.contains("open")) {
    setTimeout(() => {
      if (overlay.classList.contains("open")) void renderExtensions();
    }, 700);
  }
}

function closeSettings(): void {
  overlay.classList.remove("open");
}
overlay.querySelector("#cfg-cancel")!.addEventListener("click", closeSettings);
overlay.querySelector("#cfg-save")!.addEventListener("click", async () => {
  commitProviderForm();
  const workspace = currentWs;
  const temperature = parseFloat(el<HTMLInputElement>("#cfg-temp").value) || 1;
  const system = el<HTMLTextAreaElement>("#cfg-sys").value;
  const yolo = el<HTMLInputElement>("#cfg-yolo").checked;
  const cfg: Config = {
    // Settings decides what is *available*; the picker decides what is in use.
    // Leaving the connection and the selection empty tells the backend to keep
    // its own — saving Settings must never move a chat to another model.
    base_url: "",
    api_key: "",
    model: "",
    provider_id: "",
    workspace,
    system,
    temperature,
    yolo,
    models: [],
    context_window: defaultCtxWindow,
    providers: draft,
  };
  await api.saveConfig(cfg);
  providers = draft;
  await syncModelState();
  setYoloIndicator(yolo);
  closeSettings();
});
overlay.addEventListener("click", (e) => {
  if (e.target === overlay) closeSettings();
});
document.getElementById("btn-settings")!.addEventListener("click", () => void openSettings());
overlay.querySelector("#cfg-extreload")!.addEventListener("click", () => {
  void (async () => {
    await reloadExtensions();
    await renderExtensions();
  })();
});

/// Pull the catalogue and the resolved selection back from the backend. It is
/// the arbiter: turning a provider or model off can move the selection, and
/// the UI must show where it actually landed rather than what was clicked.
async function syncModelState(): Promise<void> {
  catalog = await api.listModels().catch(() => [] as api.ModelChoice[]);
  const cfg = await api.getConfig();
  // Editing providers can move any chat's window, so no cached budget survives
  // a sync. Each chat re-reads its own on its next render or send.
  ctxWindows.clear();
  providers = cfg.providers || providers;
  // Leave a chat on its own model while that model is still on offer. Only
  // when it has been turned off (or removed) does the chat move — and then it
  // moves for real, so its next run doesn't hit a model nothing serves.
  const stillOffered = catalog.some(
    (c) => c.model === currentModel && c.provider_id === currentProviderId,
  );
  if (!stillOffered) {
    currentModel = cfg.model || "";
    currentProviderId = cfg.provider_id || "";
    if (currentSession && currentModel) {
      await api.setSessionModel(currentSession, currentModel, currentProviderId);
    }
  }
  modelPill.textContent = pillText();
  sbModel.textContent = currentModel || "?";
  sbProv.textContent = "[" + providerLabel() + "]";
  await refreshCtxWindow(currentSession);
  sbUpdate();
}

/// Point the UI at the model a chat is on. A chat can sit on a model from any
/// provider, so the provider label and the context budget move with it.
async function applySessionModel(model: string, provider: string): Promise<void> {
  if (model) {
    currentModel = model;
    currentProviderId =
      provider || catalog.find((c) => c.model === model)?.provider_id || currentProviderId;
  }
  modelPill.textContent = pillText();
  sbModel.textContent = currentModel || "?";
  sbProv.textContent = "[" + providerLabel() + "]";
  await refreshCtxWindow(currentSession);
  sbUpdate();
}


// ---------- init ----------
async function init(): Promise<void> {
  renderSuggestions();
  empty.classList.remove("hidden");
  updateInputState();

  if (!api.isTauri) {
    statusText.textContent = "browser preview";
    modelPill.textContent = "—";
    input.placeholder = "Run with `npm run tauri dev` for the real harness";
    return;
  }

  try {
    const cfg = await api.getConfig();
    defaultCtxWindow = cfg.context_window || 1_000_000;
    await syncModelState();
    setYoloIndicator(!!cfg.yolo);
    await refreshSessions();
    if (pinned) openSessions();
    if (currentSession) {
      const g = await api.getSession(currentSession);
      renderHistory(g.messages);
      seedContextEstimate(currentSession, g.context_estimate);
      ui(currentSession).running = !!g.running;
      await applySessionModel(g.model, g.provider);
    }
    updateChatTitle();
    updateChatBanner();
    applyChatUI();
    // Plugins load once projects are known, so a project's own .e/plugins is
    // in scope rather than only the global folder.
    await loadPlugins();
  } catch (e) {
    statusWrap.classList.add("error");
    statusText.textContent = "unreachable";
    modelPill.textContent = "error";
    console.error(e);
  }
}

/// Reset the cost/token readout for a chat that has no history to account for.
function resetSB(sid = currentSession): void {
  if (!sid) return;
  const s = ui(sid);
  s.baseIn = s.baseOut = s.liveIn = s.liveOut = s.ctxIn = s.money = 0;
  s.costKnown = false;
  s.errored = false;
  if (sid === currentSession) sbUpdate();
}

// ---------- project modals ----------
const pm = document.createElement("div");
pm.className = "overlay";
pm.innerHTML = `
  <div class="modal">
    <h3>New project</h3>
    <p class="note">Pick a local folder &mdash; its name becomes the project name.</p>
    <div class="modal-actions">
      <button id="pm-cancel">Cancel</button>
      <button id="pm-go" class="primary">Choose folder&hellip;</button>
    </div>
  </div>`;
document.body.appendChild(pm);
pm.querySelector("#pm-cancel")!.addEventListener("click", () => pm.classList.remove("open"));
pm.addEventListener("click", (e) => { if (e.target === pm) pm.classList.remove("open"); });
pm.querySelector("#pm-go")!.addEventListener("click", async () => {
  const dir = await pickFolder();
  if (!dir) return;
  await api.newProject("", dir);
  const meta = await api.newSession("Chat 1", dir, currentModel, currentProviderId);
  pm.classList.remove("open");
  await refreshSessions();
  if (meta.id) await loadSession(meta.id);
});
function openProjModal(): void { pm.classList.add("open"); }

const rm = document.createElement("div");
rm.className = "overlay";
rm.innerHTML = `
  <div class="modal">
    <h3>Project</h3>
    <div class="field"><label class="lbl">Name</label><input id="rm-input" spellcheck="false"/></div>
    <div class="field">
      <label class="lbl">Folder</label>
      <div class="rm-folder"><code id="rm-ws"></code><button id="rm-pick" type="button">Choose&hellip;</button></div>
      <p class="note rm-warn" id="rm-warn" hidden>&#9888; This folder doesn&rsquo;t exist. Tools will fail until you pick another one.</p>
    </div>
    <div class="modal-actions">
      <button id="rm-cancel">Cancel</button>
      <button id="rm-save" class="primary">Save</button>
    </div>
  </div>`;
document.body.appendChild(rm);
const rmInput = rm.querySelector("#rm-input") as HTMLInputElement;
const rmWs = rm.querySelector("#rm-ws") as HTMLElement;
const rmWarn = rm.querySelector("#rm-warn") as HTMLElement;
let renameProjId = "";
let renameProjWs = "";
let renameProjOrigWs = "";

async function showRenameWorkspace(ws: string): Promise<void> {
  renameProjWs = ws;
  // A scratch project is deliberately folder-less from the user's point of
  // view; naming its internal path would suggest a codebase that isn't there.
  // Picking a real folder turns it into an ordinary project, so re-check.
  const isScratch =
    !!projects.find((p) => p.id === renameProjId)?.scratch && ws === renameProjOrigWs;
  rmWs.textContent = isScratch ? "none — one-off work" : ws || "(not set)";
  rmWarn.hidden = isScratch ? true : ws ? await api.pathIsDir(ws) : false;
}

rm.querySelector("#rm-pick")!.addEventListener("click", () => {
  void (async () => {
    const dir = await pickFolder();
    if (dir) await showRenameWorkspace(dir);
  })();
});
rm.querySelector("#rm-cancel")!.addEventListener("click", () => rm.classList.remove("open"));
rm.addEventListener("click", (e) => { if (e.target === rm) rm.classList.remove("open"); });
rm.querySelector("#rm-save")!.addEventListener("click", async () => {
  const v = rmInput.value.trim();
  if (renameProjId) {
    if (v) await api.renameProject(renameProjId, v);
    if (renameProjWs && renameProjWs !== renameProjOrigWs) await api.setProjectWorkspace(renameProjId, renameProjWs);
    await refreshSessions();
  }
  rm.classList.remove("open");
});
function openRenameModal(id: string, name: string, workspace: string): void {
  renameProjId = id;
  renameProjOrigWs = workspace;
  rmInput.value = name;
  void showRenameWorkspace(workspace);
  rm.classList.add("open");
  rmInput.focus();
}

// ---------- right pane ----------
// A third column beside the conversation, holding tabs contributed by plugins.
// Two rules shape everything below.
//
// Tabs belong to a chat, not to the window. Switching chats has to swap the
// whole tab strip, or a terminal opened in one project would be sitting in
// another project's chat pointing at the wrong folder.
//
// A view is mounted once and then only hidden. Tearing the DOM down on tab
// switch loses a terminal's scrollback and every folder a tree had expanded,
// which makes tabs feel like they reset themselves for no reason.

/** What a plugin hands to `e.registerView`. */
type PaneViewDef = {
  /** Unique within the plugin; the pane namespaces it as `<plugin>:<id>`. */
  id: string;
  /** Menu entry, and the default tab label. */
  title: string;
  /** Short glyph shown before the tab label. */
  icon?: string;
  /** Build the view into `el`. Return a cleanup function to release resources
   *  (a pty, a watcher) when the tab is closed. */
  mount: (el: HTMLElement, ctx: PaneViewContext) => void | (() => void) | Promise<void | (() => void)>;
};

/** What a mounted view is told about where it lives. */
type PaneViewContext = {
  /** This tab's instance id — unique per tab, stable while the tab exists. */
  tab: string;
  /** The chat the tab belongs to. Views must use this rather than "the current
   *  chat": a tab keeps running while you read another conversation. */
  sid: string;
  /** Rename the tab, so a terminal can show its cwd and a tree its folder. */
  setTitle: (t: string) => void;
  /** Mark the tab spent (a shell that exited) without closing it. */
  setDone: (done: boolean) => void;
  /** True while this tab is the visible one. */
  isActive: () => boolean;
  /** Called when the tab becomes visible or is resized — the moment a view
   *  that measures itself (a terminal working out its column count) can
   *  actually get a non-zero box. */
  onShow: (fn: () => void) => void;
  /** Close this tab from inside the view. */
  close: () => void;
};

type RegisteredView = PaneViewDef & { plugin: string; key: string };

type PaneTab = {
  tab: string;
  sid: string;
  key: string;
  title: string;
  icon: string;
  done: boolean;
  el: HTMLElement;
  cleanup: (() => void) | null;
  onShow: (() => void)[];
};

const paneViews = new Map<string, RegisteredView>();
/** Tabs per chat id. A chat with no entry has simply never opened one. */
const paneTabs = new Map<string, PaneTab[]>();
const paneActive = new Map<string, string>();

const rightPane = document.getElementById("rightpane") as HTMLElement;
const paneGrip = document.getElementById("pane-grip") as HTMLElement;
const paneTabList = document.getElementById("pane-tablist") as HTMLElement;
const paneBody = document.getElementById("pane-body") as HTMLElement;
const paneEmpty = document.getElementById("pane-empty") as HTMLElement;
const paneAdd = document.getElementById("pane-add") as HTMLButtonElement;
const paneCloseBtn = document.getElementById("pane-close") as HTMLButtonElement;
const paneMenu = document.getElementById("pane-menu") as HTMLElement;
const paneMenuList = document.getElementById("pane-menu-list") as HTMLElement;
const paneBtn = document.getElementById("btn-pane") as HTMLButtonElement;

const PANE_MIN = 260;
/// Leave the conversation a readable column no matter how far the grip is
/// dragged: a 90%-wide pane technically works and makes the app useless.
const PANE_MAX_SHARE = 0.62;
let paneOpen = localStorage.getItem("e:pane") === "1";
let paneWidth = Math.max(PANE_MIN, Number(localStorage.getItem("e:pane-w")) || 380);
let paneSeq = 0;

function paneMaxWidth(): number {
  return Math.max(PANE_MIN, Math.round(window.innerWidth * PANE_MAX_SHARE));
}

function applyPaneWidth(w: number): void {
  paneWidth = Math.min(paneMaxWidth(), Math.max(PANE_MIN, Math.round(w)));
  document.documentElement.style.setProperty("--pane-w", paneWidth + "px");
}

function setPaneOpen(open: boolean): void {
  paneOpen = open;
  rightPane.hidden = !open;
  paneGrip.hidden = !open;
  paneBtn.classList.toggle("on", open);
  paneBtn.setAttribute("aria-pressed", open ? "true" : "false");
  localStorage.setItem("e:pane", open ? "1" : "0");
  if (open) {
    applyPaneWidth(paneWidth);
    renderPaneTabs();
    // The pane was 0×0 while hidden, so anything that measures itself has to
    // be told to look again now that it has a box.
    notifyPaneShown();
  }
}

function tabsFor(sid: string): PaneTab[] {
  let list = paneTabs.get(sid);
  if (!list) {
    list = [];
    paneTabs.set(sid, list);
  }
  return list;
}

/// Views a plugin registered, in a stable order so the menu does not reshuffle
/// itself between reloads.
function paneViewList(): RegisteredView[] {
  return [...paneViews.values()].sort((a, b) => a.plugin.localeCompare(b.plugin) || a.title.localeCompare(b.title));
}

function renderPaneTabs(): void {
  const sid = currentSession;
  const list = tabsFor(sid);
  const active = paneActive.get(sid) || "";
  paneTabList.replaceChildren();
  for (const t of list) {
    const el = document.createElement("div");
    el.className = "pane-tab" + (t.tab === active ? " active" : "") + (t.done ? " exited" : "");
    el.setAttribute("role", "tab");
    el.setAttribute("aria-selected", t.tab === active ? "true" : "false");
    el.title = t.title;
    const label = document.createElement("span");
    label.className = "pane-tab-label";
    label.textContent = (t.icon ? t.icon + " " : "") + t.title;
    const x = document.createElement("button");
    x.className = "pane-tab-x";
    x.type = "button";
    x.textContent = "×";
    x.title = "Close tab";
    x.setAttribute("aria-label", `Close ${t.title}`);
    x.addEventListener("click", (e) => {
      e.stopPropagation();
      closePaneTab(t.tab);
    });
    el.append(label, x);
    el.addEventListener("click", () => activatePaneTab(t.tab));
    // Middle-click closes, the way it does in every other tab strip.
    el.addEventListener("auxclick", (e) => {
      if ((e as MouseEvent).button === 1) {
        e.preventDefault();
        closePaneTab(t.tab);
      }
    });
    paneTabList.appendChild(el);
  }
  paneEmpty.hidden = list.length > 0;
  // Only the current chat's views may be on screen. A tab left visible after a
  // chat switch is a terminal in the wrong project's folder.
  for (const [, all] of paneTabs) {
    for (const t of all) {
      t.el.classList.toggle("active", t.sid === sid && t.tab === active);
    }
  }
}

function notifyPaneShown(): void {
  const sid = currentSession;
  const active = paneActive.get(sid);
  const t = tabsFor(sid).find((x) => x.tab === active);
  if (!t) return;
  for (const fn of t.onShow) {
    try {
      fn();
    } catch (e) {
      console.error("pane view onShow", t.key, e);
    }
  }
}

function activatePaneTab(tab: string): void {
  const sid = currentSession;
  if (!tabsFor(sid).some((t) => t.tab === tab)) return;
  paneActive.set(sid, tab);
  renderPaneTabs();
  notifyPaneShown();
}

/// Open a view as a new tab in a chat. Returns the tab id, or "" when the view
/// is not registered — a plugin can be disabled between a menu being drawn and
/// a click landing on it.
async function openPaneTab(key: string, sid = currentSession): Promise<string> {
  const view = paneViews.get(key);
  if (!view) {
    notify(`No such view: ${key}`, "error");
    return "";
  }
  const tab = `t${++paneSeq}_${Date.now().toString(36)}`;
  const el = document.createElement("div");
  el.className = "pane-view";
  paneBody.appendChild(el);

  const entry: PaneTab = {
    tab,
    sid,
    key,
    title: view.title,
    icon: view.icon || "",
    done: false,
    el,
    cleanup: null,
    onShow: [],
  };
  tabsFor(sid).push(entry);
  paneActive.set(sid, tab);
  if (!paneOpen) setPaneOpen(true);
  renderPaneTabs();

  const ctx: PaneViewContext = {
    tab,
    sid,
    setTitle: (t) => {
      entry.title = String(t || view.title).slice(0, 80);
      if (sid === currentSession) renderPaneTabs();
    },
    setDone: (done) => {
      entry.done = !!done;
      if (sid === currentSession) renderPaneTabs();
    },
    isActive: () => paneActive.get(entry.sid) === tab && entry.sid === currentSession && paneOpen,
    onShow: (fn) => {
      entry.onShow.push(fn);
    },
    close: () => closePaneTab(tab),
  };

  try {
    const cleanup = await view.mount(el, ctx);
    entry.cleanup = typeof cleanup === "function" ? cleanup : null;
  } catch (e) {
    console.error("pane view", key, e);
    el.innerHTML = `<div class="files-error"></div>`;
    (el.querySelector(".files-error") as HTMLElement).textContent = `${view.title} failed to open: ${e instanceof Error ? e.message : String(e)}`;
  }
  // Mounting can take an await; the tab may already be the visible one by now.
  notifyPaneShown();
  return tab;
}

function closePaneTab(tab: string): void {
  for (const [sid, list] of paneTabs) {
    const i = list.findIndex((t) => t.tab === tab);
    if (i < 0) continue;
    const [t] = list.splice(i, 1);
    // Cleanup first: it is what kills the pty, and a throw here must not leave
    // the tab on screen with nothing behind it.
    try {
      t.cleanup?.();
    } catch (e) {
      console.error("pane view cleanup", t.key, e);
    }
    t.el.remove();
    if (paneActive.get(sid) === tab) {
      const next = list[Math.min(i, list.length - 1)];
      if (next) paneActive.set(sid, next.tab);
      else paneActive.delete(sid);
    }
    if (sid === currentSession) {
      renderPaneTabs();
      notifyPaneShown();
    }
    return;
  }
}

/// Drop every tab belonging to a chat — used when that chat is deleted, so its
/// shells do not outlive it.
function closePaneTabsFor(sid: string): void {
  for (const t of [...tabsFor(sid)]) closePaneTab(t.tab);
  paneTabs.delete(sid);
  paneActive.delete(sid);
}

function openPaneMenu(): void {
  const views = paneViewList();
  paneMenuList.replaceChildren();
  if (!views.length) {
    const row = document.createElement("div");
    row.className = "picker-item dim";
    row.textContent = "No views installed";
    paneMenuList.appendChild(row);
  }
  for (const v of views) {
    const row = document.createElement("div");
    row.className = "picker-item";
    row.innerHTML = `<span class="pi-name"></span><span class="pi-sub"></span>`;
    (row.querySelector(".pi-name") as HTMLElement).textContent = (v.icon ? v.icon + " " : "") + v.title;
    (row.querySelector(".pi-sub") as HTMLElement).textContent = v.plugin;
    row.addEventListener("click", () => {
      paneMenu.hidden = true;
      void openPaneTab(v.key);
    });
    paneMenuList.appendChild(row);
  }
  paneMenu.hidden = false;
}

paneAdd.addEventListener("click", (e) => {
  e.stopPropagation();
  // Toggle rather than always-open, so a second click (or an impatient
  // double-click) closes the menu instead of reopening it under the cursor.
  if (!paneMenu.hidden) {
    paneMenu.hidden = true;
    return;
  }
  const views = paneViewList();
  // One view installed makes a menu of one an obstacle, not a choice.
  if (views.length === 1) void openPaneTab(views[0].key);
  else openPaneMenu();
});
paneCloseBtn.addEventListener("click", () => setPaneOpen(false));
document.addEventListener("click", (e) => {
  if (!paneMenu.hidden && !paneMenu.contains(e.target as Node)) paneMenu.hidden = true;
});
// Escape closes the menu before anything else can read it — without this it is
// the one popup in the app you can only dismiss with the mouse.
document.addEventListener("keydown", (e) => {
  if (e.key !== "Escape" || paneMenu.hidden) return;
  e.preventDefault();
  e.stopPropagation();
  paneMenu.hidden = true;
}, true);
paneBtn.addEventListener("click", () => setPaneOpen(!paneOpen));

// Ctrl/Cmd+B is the near-universal "toggle the side panel" binding.
document.addEventListener("keydown", (e) => {
  if ((e.ctrlKey || e.metaKey) && !e.shiftKey && !e.altKey && e.key.toLowerCase() === "b") {
    e.preventDefault();
    setPaneOpen(!paneOpen);
  }
});

// The grip drags on pointer events rather than mouse events so a trackpad or
// pen behaves the same, and captures the pointer so leaving the 5px strip
// mid-drag does not drop it.
paneGrip.addEventListener("pointerdown", (e) => {
  e.preventDefault();
  paneGrip.setPointerCapture(e.pointerId);
  paneGrip.classList.add("dragging");
  document.body.classList.add("pane-dragging");
  const startX = e.clientX;
  const startW = paneWidth;
  const move = (ev: PointerEvent): void => {
    // The pane is on the right, so dragging left widens it.
    applyPaneWidth(startW - (ev.clientX - startX));
  };
  const up = (): void => {
    paneGrip.releasePointerCapture(e.pointerId);
    paneGrip.classList.remove("dragging");
    document.body.classList.remove("pane-dragging");
    paneGrip.removeEventListener("pointermove", move);
    paneGrip.removeEventListener("pointerup", up);
    paneGrip.removeEventListener("pointercancel", up);
    localStorage.setItem("e:pane-w", String(paneWidth));
    notifyPaneShown();
  };
  paneGrip.addEventListener("pointermove", move);
  paneGrip.addEventListener("pointerup", up);
  paneGrip.addEventListener("pointercancel", up);
});
// Keyboard resize, so the grip is not mouse-only.
paneGrip.addEventListener("keydown", (e) => {
  const step = e.shiftKey ? 60 : 16;
  if (e.key === "ArrowLeft") applyPaneWidth(paneWidth + step);
  else if (e.key === "ArrowRight") applyPaneWidth(paneWidth - step);
  else return;
  e.preventDefault();
  localStorage.setItem("e:pane-w", String(paneWidth));
  notifyPaneShown();
});

let paneResizeTimer = 0;
window.addEventListener("resize", () => {
  applyPaneWidth(paneWidth);
  // Views re-measure on a trailing edge: a terminal reflowing on every frame
  // of a window drag is thousands of pointless resize round-trips to the pty.
  clearTimeout(paneResizeTimer);
  paneResizeTimer = window.setTimeout(notifyPaneShown, 120);
});

applyPaneWidth(paneWidth);
setPaneOpen(paneOpen);

// ---------- pty stream fan-out ----------
// Two listeners for the whole app, demultiplexed by terminal id. A listener per
// terminal would have to be unregistered on every tab close, and one missed
// teardown keeps that tab's entire scrollback (and its closure) alive forever.
const ptyDataSubs = new Map<string, Set<(data: string) => void>>();
const ptyExitSubs = new Map<string, Set<(code: number) => void>>();

function subscribe<T>(map: Map<string, Set<T>>, id: string, fn: T): () => void {
  let set = map.get(id);
  if (!set) {
    set = new Set();
    map.set(id, set);
  }
  set.add(fn);
  return () => {
    const s = map.get(id);
    if (!s) return;
    s.delete(fn);
    if (!s.size) map.delete(id);
  };
}

api.onPtyEvent((ev) => {
  if (ev.type === "data") {
    for (const fn of ptyDataSubs.get(ev.id) || []) {
      try {
        fn(ev.data);
      } catch (e) {
        console.error("pty data handler", ev.id, e);
      }
    }
    return;
  }
  for (const fn of ptyExitSubs.get(ev.id) || []) {
    try {
      fn(ev.code);
    } catch (e) {
      console.error("pty exit handler", ev.id, e);
    }
  }
  // The process is gone; nothing more can arrive under this id.
  ptyDataSubs.delete(ev.id);
  ptyExitSubs.delete(ev.id);
});

// ---------- plugins ----------
// A plugin is a folder the user put in ~/.e/plugins (or <project>/.e/plugins):
// a manifest and an ES module. The module runs here, in the webview, and only
// ever sees the API its manifest asked for — a plugin that declares "events"
// cannot quietly register a tool or reach the network.
const pluginTools: api.PluginToolDef[] = [];
const pluginReg = new Map<string, (args: Record<string, unknown>) => unknown>();
const pluginHandlers: { plugin: string; event: string; handler: (ev: api.EngineEvents) => unknown }[] = [];
const pluginCommands: Record<string, { run: () => void; desc: string; plugin: string }> = {};

/** What a plugin folder turned into: the manifest, plus what it did with it. */
type PluginStatus = api.PluginInfo & {
  loaded: boolean;
  failure: string;
  tools: string[];
  commands: string[];
  views: string[];
  notes: string[];
};
let pluginStatus: PluginStatus[] = [];
/// True while `loadPlugins` is running, so registrations made during the load
/// are published once at the end instead of once each.
let loadingPlugins = false;

interface PluginAPIHost {
  /** The plugin's own folder name. */
  name: string;
  registerTool(def: api.PluginToolDef & { run: (args: Record<string, unknown>) => unknown }): void;
  on(event: string, handler: (ev: api.EngineEvents) => unknown): void;
  registerCommand(name: string, fn: () => void, desc?: string): void;
  /** Contribute a tab to the right pane. Needs "views". */
  registerView(def: PaneViewDef): void;
  ui: { notify: (msg: string, kind?: string) => void; confirm: (msg: string) => Promise<boolean> };
  /** Read-only browsing of a chat's project folder. Needs "fs". */
  fs: {
    list: (sid: string, path?: string) => Promise<api.FsListing>;
    read: (sid: string, path: string) => Promise<api.FsFile>;
  };
  /** Real terminals in a chat's project folder. Needs "pty". */
  pty: {
    spawn: (sid: string, id: string, cols: number, rows: number) => Promise<void>;
    write: (sid: string, id: string, data: string) => Promise<void>;
    resize: (sid: string, id: string, cols: number, rows: number) => Promise<void>;
    kill: (sid: string, id: string) => Promise<void>;
    alive: (sid: string, id: string) => Promise<boolean>;
    onData: (id: string, fn: (data: string) => void) => () => void;
    onExit: (id: string, fn: (code: number) => void) => () => void;
  };
  fetch: (input: string, init?: RequestInit) => Promise<Response>;
  session: () => { id: string; name: string; workspace: string; model: string; provider: string } | null;
  log: (...args: unknown[]) => void;
}

function notify(msg: string, kind = "info"): void {
  const t = document.createElement("div");
  t.className = "toast" + (kind === "error" ? " err" : "");
  t.textContent = msg;
  document.body.appendChild(t);
  setTimeout(() => t.classList.add("out"), 2600);
  setTimeout(() => t.remove(), 3000);
}

function confirmModal(msg: string, title = "Confirm", yesLabel = "Yes", noLabel = "No"): Promise<boolean> {
  return new Promise((resolve) => {
    const ov = document.createElement("div");
    ov.className = "overlay";
    ov.innerHTML = `<div class="modal"><h3></h3><p class="note"></p><div class="modal-actions"><button id="cm-no"></button><button id="cm-yes" class="primary"></button></div></div>`;
    (ov.querySelector("h3") as HTMLElement).textContent = title;
    (ov.querySelector(".note") as HTMLElement).textContent = msg;
    const yes = ov.querySelector("#cm-yes") as HTMLButtonElement;
    const no = ov.querySelector("#cm-no") as HTMLButtonElement;
    yes.textContent = yesLabel;
    no.textContent = noLabel;
    const done = (v: boolean): void => {
      document.removeEventListener("keydown", onKey, true);
      ov.remove();
      resolve(v);
    };
    // Capture, so Escape closes this dialog instead of reaching the composer's
    // "Escape stops the run" binding underneath.
    const onKey = (e: KeyboardEvent): void => {
      if (e.key === "Escape") { e.preventDefault(); e.stopPropagation(); done(false); }
      else if (e.key === "Enter") { e.preventDefault(); e.stopPropagation(); done(true); }
    };
    document.addEventListener("keydown", onKey, true);
    document.body.appendChild(ov);
    ov.classList.add("open");
    yes.focus();
    yes.addEventListener("click", () => done(true));
    no.addEventListener("click", () => done(false));
    ov.addEventListener("click", (e) => { if (e.target === ov) done(false); });
  });
}

/// The window's close button is intercepted in Rust so quitting always asks
/// first — closing mid-run loses the work in flight and used to leave that
/// chat's history in a state the provider rejects outright.
let closePrompt = false;
api.onCloseRequested((running) => {
  if (closePrompt) return;
  closePrompt = true;
  void (async () => {
    try {
      const names = running.map(sessionName);
      const msg = names.length
        ? `${names.length === 1 ? "“" + names[0] + "” is" : names.length + " chats are"} still running. Quitting now interrupts ${names.length === 1 ? "it" : "them"} and loses the work in flight.`
        : "Quit e?";
      if (await confirmModal(msg, names.length ? "Quit while running?" : "Quit e", "Quit", "Stay")) {
        await api.confirmClose();
      } else {
        await api.closeDismissed();
      }
    } finally {
      closePrompt = false;
    }
  })();
});

function dispatchToPlugins(ev: api.EngineEvents): void {
  // `tool_call` carries its arguments as a JSON string. Parsing it once here
  // means every handler sees the same shape — including the veto path, which
  // builds the same event — instead of each plugin re-parsing it.
  const payload = ev.type === "tool_call" ? withParsedArgs(ev) : ev;
  for (const h of pluginHandlers) {
    if (h.event !== ev.type && h.event !== "*") continue;
    try {
      h.handler(payload);
    } catch (e) {
      console.error("plugin handler", h.plugin, e);
    }
  }
}

/// Chat lifecycle, which the engine knows nothing about — it only ever sees a
/// run. A pane view needs it: a tab is bound to one chat, so "which chat is on
/// screen" is the difference between a file tree showing this project and one
/// showing whichever project you looked at last.
function emitChatEvent(ev: Extract<api.EngineEvents, { type: "chat_open" | "chat_switch" }>): void {
  dispatchToPlugins(ev);
}

function withParsedArgs(ev: api.EngineEvents & { arguments?: string }): api.EngineEvents {
  let args: Record<string, unknown> = {};
  try { args = JSON.parse(ev.arguments || "{}"); } catch { /* leave empty */ }
  return { ...ev, args } as api.EngineEvents;
}

/// Ask every `tool_call` handler whether this call may proceed. The engine
/// waits five seconds at most, so a handler that hangs is treated as "allow"
/// rather than being allowed to freeze the run.
async function answerVeto(ev: { id: string; sid: string; tool: string; arguments: string }): Promise<void> {
  const call = withParsedArgs({
    type: "tool_call",
    sid: ev.sid,
    id: "",
    name: ev.tool,
    arguments: ev.arguments,
  } as unknown as api.EngineEvents);
  let reason: string | null = null;
  for (const h of pluginHandlers) {
    if (h.event !== "tool_call" && h.event !== "*") continue;
    try {
      const out = (await h.handler(call)) as { block?: boolean; reason?: string } | undefined;
      if (out && out.block) {
        reason = out.reason || `blocked by ${h.plugin}`;
        break;
      }
    } catch (e) {
      console.error("plugin veto handler", h.plugin, e);
    }
  }
  await api.pluginVetoResult(ev.id, reason);
}

/// Provider APIs only accept `a-z A-Z 0-9 _ -` in a tool name, so the engine
/// sanitises what it is given. The host applies the same rule *before*
/// registering: otherwise the model would be told about `say_hi` while the
/// plugin's handler was filed under `say hi`, and every call would miss.
function toolName(raw: string): string {
  const clean = raw.replace(/[^a-zA-Z0-9_-]/g, "_").slice(0, 64);
  return clean || "tool";
}

/// Terminal ids are namespaced by plugin. Without this, one plugin could write
/// into — or kill — another's shell just by guessing the id it used.
function ptyId(plugin: string, id: string): string {
  return `${plugin}::${String(id || "")}`;
}

/// Capabilities are a contract, not a suggestion: everything a plugin can
/// reach is handed out here, and only if its manifest asked for it. A refusal
/// is loud — a silently missing tool is the worst possible failure mode.
function buildPluginApi(info: api.PluginInfo, status: PluginStatus): PluginAPIHost {
  const has = (c: string): boolean => info.capabilities.includes(c);
  const deny = (cap: string, what: string): void => {
    const msg = `${info.name}: ${what} needs the "${cap}" capability — add it to plugin.json`;
    status.notes.push(msg);
    notify(msg, "error");
  };
  return {
    name: info.name,
    registerTool(def) {
      if (!has("tools")) return deny("tools", `registerTool("${def.name}")`);
      const name = toolName(def.name);
      if (name !== def.name) {
        status.notes.push(`tool "${def.name}" is exposed as "${name}" (a-z, 0-9, _ and - only)`);
      }
      pluginReg.set(name, def.run);
      pluginTools.push({
        name,
        description: def.description,
        parameters: def.parameters || { type: "object" },
        plugin: info.name,
      });
      status.tools.push(name);
      // A plugin may register late (after an await of its own). During the
      // initial load one publish at the end covers everything; afterwards each
      // registration has to reach the engine on its own.
      if (!loadingPlugins) void publishPluginTools();
    },
    on(event, handler) {
      if (!has("events")) return deny("events", `on("${event}")`);
      pluginHandlers.push({ plugin: info.name, event, handler });
      // Same late-registration case as tools: the engine has to learn that
      // someone is watching tool calls, or the guard would never be consulted.
      if (!loadingPlugins && (event === "tool_call" || event === "*")) void api.setPluginVeto(true);
    },
    registerCommand(name, fn, desc) {
      if (!has("commands")) return deny("commands", `registerCommand("${name}")`);
      const cmd = name.startsWith("/") ? name : "/" + name;
      if (BUILTIN_SLASH.some((c) => c.name === cmd)) {
        const msg = `${info.name}: ${cmd} is a built-in command`;
        status.notes.push(msg);
        notify(msg, "error");
        return;
      }
      pluginCommands[cmd] = { run: fn, desc: desc || "plugin command", plugin: info.name };
      status.commands.push(cmd);
    },
    registerView(def) {
      if (!has("views")) return deny("views", `registerView("${def && def.id}")`);
      const id = String((def && def.id) || "").trim();
      if (!id) {
        const msg = `${info.name}: a view needs an id`;
        status.notes.push(msg);
        notify(msg, "error");
        return;
      }
      if (typeof def.mount !== "function") {
        const msg = `${info.name}: view "${id}" has no mount function`;
        status.notes.push(msg);
        notify(msg, "error");
        return;
      }
      // Namespaced by plugin, so two plugins may both contribute a "terminal"
      // without one silently replacing the other.
      const key = `${info.name}:${id}`;
      if (paneViews.has(key)) {
        const msg = `${info.name}: view "${id}" is registered twice`;
        status.notes.push(msg);
        notify(msg, "error");
        return;
      }
      paneViews.set(key, {
        ...def,
        id,
        title: String(def.title || id),
        icon: def.icon ? String(def.icon).slice(0, 2) : "",
        plugin: info.name,
        key,
      });
      status.views.push(key);
    },
    ui: {
      notify: (msg, kind) => {
        if (!has("ui")) return deny("ui", "ui.notify");
        notify(`${info.display}: ${msg}`, kind);
      },
      confirm: async (msg) => {
        if (!has("ui")) {
          deny("ui", "ui.confirm");
          return false;
        }
        return confirmModal(msg, info.display);
      },
    },
    // The chat id is the argument, not a path: the backend turns it into a
    // folder. A plugin naming its own root would make this a file browser for
    // the whole disk, which is the one thing the capability must not be.
    fs: {
      list: (sid, path) => {
        if (!has("fs")) {
          deny("fs", "fs.list");
          return Promise.reject(new Error(`${info.name}: fs.list needs the "fs" capability`));
        }
        return api.fsList(sid, path || "");
      },
      read: (sid, path) => {
        if (!has("fs")) {
          deny("fs", "fs.read");
          return Promise.reject(new Error(`${info.name}: fs.read needs the "fs" capability`));
        }
        return api.fsRead(sid, path);
      },
    },
    pty: {
      spawn: (sid, id, cols, rows) => {
        if (!has("pty")) {
          deny("pty", "pty.spawn");
          return Promise.reject(new Error(`${info.name}: pty.spawn needs the "pty" capability`));
        }
        return api.ptySpawn(sid, ptyId(info.name, id), cols, rows);
      },
      write: (sid, id, data) => {
        if (!has("pty")) {
          deny("pty", "pty.write");
          return Promise.reject(new Error(`${info.name}: pty.write needs the "pty" capability`));
        }
        return api.ptyWrite(sid, ptyId(info.name, id), data);
      },
      resize: (sid, id, cols, rows) => {
        if (!has("pty")) {
          deny("pty", "pty.resize");
          return Promise.reject(new Error(`${info.name}: pty.resize needs the "pty" capability`));
        }
        return api.ptyResize(sid, ptyId(info.name, id), cols, rows);
      },
      kill: (sid, id) => {
        if (!has("pty")) {
          deny("pty", "pty.kill");
          return Promise.reject(new Error(`${info.name}: pty.kill needs the "pty" capability`));
        }
        return api.ptyKill(sid, ptyId(info.name, id));
      },
      alive: (sid, id) => {
        if (!has("pty")) {
          deny("pty", "pty.alive");
          return Promise.resolve(false);
        }
        return api.ptyAlive(sid, ptyId(info.name, id));
      },
      onData: (id, fn) => {
        if (!has("pty")) {
          deny("pty", "pty.onData");
          return () => undefined;
        }
        return subscribe(ptyDataSubs, ptyId(info.name, id), fn);
      },
      onExit: (id, fn) => {
        if (!has("pty")) {
          deny("pty", "pty.onExit");
          return () => undefined;
        }
        return subscribe(ptyExitSubs, ptyId(info.name, id), fn);
      },
    },
    fetch: (input, init) => {
      if (!has("network")) {
        deny("network", "fetch");
        return Promise.reject(new Error(`${info.name}: fetch needs the "network" capability`));
      }
      return fetch(input, init);
    },
    session: () => {
      if (!has("session-read")) {
        deny("session-read", "session()");
        return null;
      }
      if (!currentSession) return null;
      return {
        id: currentSession,
        name: sessionName(currentSession),
        workspace: activeWorkspace(),
        model: currentModel,
        provider: currentProviderId,
      };
    },
    log: (...args) => console.log(`[${info.name}]`, ...args),
  };
}

/// Plugins are ES modules with a default export. They are loaded through a
/// blob URL rather than `eval` so ordinary module syntax — helpers above the
/// export, top-level constants — works the way the author wrote it.
async function loadPluginModule(source: string): Promise<(host: PluginAPIHost) => unknown> {
  const url = URL.createObjectURL(new Blob([source], { type: "text/javascript" }));
  try {
    const mod = (await import(/* @vite-ignore */ url)) as { default?: unknown };
    if (typeof mod.default !== "function") {
      throw new Error("no default export — a plugin is `export default function (e) { … }`");
    }
    return mod.default as (host: PluginAPIHost) => unknown;
  } finally {
    URL.revokeObjectURL(url);
  }
}

function resetPluginRuntime(): void {
  pluginTools.length = 0;
  pluginReg.clear();
  pluginHandlers.length = 0;
  for (const k of Object.keys(pluginCommands)) delete pluginCommands[k];
  // Open tabs go with the code that drew them. Keeping a view mounted after
  // its module was dropped leaves a pane nobody owns: its buttons still call
  // into the old closure, and a reload that was meant to pick up an edit would
  // instead run the previous version until the tab happened to be closed.
  for (const sid of [...paneTabs.keys()]) closePaneTabsFor(sid);
  paneViews.clear();
  pluginStatus = [];
}

/// Where the current chat actually runs. Project extensions live under this
/// folder's `.e/`, so it has to be the same directory the tools use — the
/// global default would point a project at someone else's plugins.
function activeWorkspace(): string {
  const chat = sessions.find((x) => x.id === currentSession);
  if (chat && chat.detached && chat.workspace) return chat.workspace;
  const p = projects.find((x) => x.id === currentProject);
  return (p && p.workspace) || currentWs || "";
}

/// Discover, load and register every enabled plugin for the current project.
/// Safe to call again: everything registered is dropped first, so a reload
/// cannot leave a removed plugin's tools behind.
async function loadPlugins(): Promise<void> {
  if (!api.isTauri) return;
  resetPluginRuntime();
  loadingPlugins = true;
  const ws = activeWorkspace();
  let found: api.PluginInfo[] = [];
  try {
    found = await api.listPlugins(ws || undefined);
  } catch (e) {
    console.error("plugin discovery", e);
    loadingPlugins = false;
    return;
  }
  for (const info of found) {
    const status: PluginStatus = { ...info, loaded: false, failure: info.error, tools: [], commands: [], views: [], notes: [] };
    pluginStatus.push(status);
    if (!info.enabled || info.error) continue;
    try {
      const g = await api.getPlugin(info.name, ws || undefined);
      const factory = await loadPluginModule(g.source);
      await factory(buildPluginApi(info, status));
      status.loaded = true;
    } catch (e) {
      status.failure = String(e instanceof Error ? e.message : e);
      console.error("plugin", info.name, e);
      notify(`Plugin “${info.display}” failed to load`, "error");
    }
  }
  loadingPlugins = false;
  await publishPluginTools();
  // The engine only pays for the veto hook when something is actually
  // listening for tool calls.
  await api.setPluginVeto(pluginHandlers.some((h) => h.event === "tool_call" || h.event === "*"));
}

async function publishPluginTools(): Promise<void> {
  try {
    const refused = await api.setPluginTools(pluginTools);
    for (const r of refused) {
      // Name the tool the engine dropped: the plugin looks fine from the
      // outside, but the model would never see that tool.
      const owner = pluginStatus.find((p) => r.startsWith(`'${p.name}'`));
      if (owner) owner.notes.push(r);
      notify(r, "error");
    }
  } catch (e) {
    console.error("register plugin tools", e);
  }
}

/// Re-read every extension surface: plugin folders, skills, MCP servers. This
/// is deliberately explicit rather than automatic on project switch — MCP
/// servers and plugin tools live in one registry shared by every chat, and
/// pulling them out from under a run in flight would fail that run's tools.
async function reloadExtensions(): Promise<void> {
  const ws = activeWorkspace();
  await api.reloadExtensions(ws || undefined);
  await loadPlugins();
  notify("Extensions reloaded");
}
// ---------- chat rename ----------
const cr = document.createElement("div");
cr.className = "overlay";
cr.innerHTML = `<div class="modal"><h3>Rename chat</h3><div class="field"><input id="cr-input" spellcheck="false"/></div><div class="modal-actions"><button id="cr-cancel">Cancel</button><button id="cr-save" class="primary">Save</button></div></div>`;
document.body.appendChild(cr);
const crInput = cr.querySelector("#cr-input") as HTMLInputElement;
let crTarget = "";
cr.querySelector("#cr-cancel")!.addEventListener("click", () => cr.classList.remove("open"));
cr.addEventListener("click", (e) => { if (e.target === cr) cr.classList.remove("open"); });
cr.querySelector("#cr-save")!.addEventListener("click", async () => {
  const v = crInput.value.trim();
  if (v && crTarget) {
    await api.renameSession(crTarget, v);
    await refreshSessions();
  }
  cr.classList.remove("open");
});
function openChatRename(id: string, name: string): void {
  crTarget = id;
  crInput.value = name;
  cr.classList.add("open");
  crInput.focus();
}

// ---------- history search (U9) ----------
const kpal = document.createElement("div");
kpal.className = "overlay kpalette";
kpal.innerHTML = `
  <div class="modal kpal-modal">
    <input id="kp-input" placeholder="Search chats and messages…" spellcheck="false" autocomplete="off"/>
    <div class="picker-list" id="kp-results"></div>
  </div>`;
document.body.appendChild(kpal);
let kpOpen = false;
function toggleKpal(force?: boolean): void {
  kpOpen = force !== undefined ? force : !kpOpen;
  kpal.classList.toggle("open", kpOpen);
  if (kpOpen) (document.getElementById("kp-input") as HTMLInputElement).focus();
  else void refreshSessions();
}
document.getElementById("kp-input")?.addEventListener("input", async (e) => {
  const q = (e.target as HTMLInputElement).value.trim();
  const box = document.getElementById("kp-results") as HTMLElement;
  box.innerHTML = "";
  if (!q || q.length < 2) return;
  try {
    const r = await api.searchSessions(q);
    r.results.slice(0, 15).forEach((m) => {
      const it = document.createElement("div");
      it.className = "picker-item";
      const label = document.createElement("span");
      label.textContent = "[" + (m.session_name || "chat") + "] " + m.snippet;
      it.appendChild(label);
      it.addEventListener("click", () => {
        void loadSession(m.session_id);
        toggleKpal(false);
      });
      box.appendChild(it);
    });
    if (!r.results.length) {
      const e2 = document.createElement("div");
      e2.className = "picker-empty";
      e2.textContent = "No matches";
      box.appendChild(e2);
    }
  } catch { /* ignore */ }
});
document.getElementById("kp-input")?.addEventListener("keydown", (e) => {
  if (e.key === "Escape") toggleKpal(false);
});
kpal.addEventListener("click", (e) => { if (e.target === kpal) toggleKpal(false); });
document.addEventListener("keydown", (e) => {
  if ((e.metaKey || e.ctrlKey) && e.key === "k") { e.preventDefault(); toggleKpal(); }
});

// Stop button in the activity strip: scoped to the chat on screen, not global.
actSteer.addEventListener("click", () => void stopCurrent());

void init();
