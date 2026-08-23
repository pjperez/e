// e — UI controller.
import { renderMarkdown } from "./markdown";
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
  /** True while history is being summarised, so the UI can say so. */
  compacting: boolean;
};

const chatUI = new Map<string, ChatUI>();

function ui(sid: string): ChatUI {
  let s = chatUI.get(sid);
  if (!s) {
    s = {
      running: false, queued: "", startedAt: 0, activityText: "", activityStep: "",
      liveText: "", liveReason: "",
      baseIn: 0, baseOut: 0, liveIn: 0, liveOut: 0, ctxIn: 0, money: 0, costKnown: false,
      approval: null, errored: false, sending: false, compacting: false,
    };
    chatUI.set(sid, s);
  }
  return s;
}
const cur = (): ChatUI => ui(currentSession);
const isRunning = (): boolean => !!currentSession && cur().running;

/** Usable context window for the active model; refreshed from the backend. */
let ctxWindow = 1_000_000;
/** Compact once the live context passes this share of the window. */
const COMPACT_AT = 0.85;

function fmtTokens(n: number): string {
  if (n >= 1_000_000) return (n / 1_000_000).toFixed(1) + "M";
  if (n >= 1_000) return Math.round(n / 1_000) + "k";
  return String(n);
}

function sbUpdate(): void {
  const s = cur();
  const inT = Math.round(s.baseIn + s.liveIn);
  const outT = Math.round(s.baseOut + s.liveOut);
  // Context usage is the live window (last prompt + what we're about to add),
  // never the lifetime token total.
  const ctx = Math.round(s.ctxIn + s.liveIn);
  const pct = ctxWindow > 0 ? Math.min(100, (ctx * 100) / ctxWindow) : 0;
  sbArrows.textContent = "↑" + inT.toLocaleString() + " ↓" + outT.toLocaleString();
  sbCtx.textContent = s.compacting
    ? "compacting…"
    : "CH " + fmtTokens(ctx) + "/" + fmtTokens(ctxWindow) + " · " + pct.toFixed(1) + "%";
  sbCost.textContent = s.costKnown ? "$" + s.money.toFixed(3) : "$-";
}
function activeProviderName(cfg: api.Config): string {
  if (cfg.providers && cfg.providers.length) {
    const p = cfg.providers.find((x) => x.base_url === cfg.base_url) || cfg.providers[0];
    return p.name || p.id;
  }
  return hostOf(cfg.base_url);
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

// ---------- conversation rendering ----------
type ToolBlock = { el: HTMLElement; stateEl: HTMLElement; outputEl: HTMLElement };
type Turn = {
  el: HTMLElement;
  body: HTMLElement;
  textEl: HTMLElement;
  raw: string;
  hasCaret: boolean;
  tools: Map<string, ToolBlock>;
  thinkEl?: HTMLElement;
  thinkOpen?: boolean;
};

let turns: Turn[] = [];
const lastTurn = (): Turn | undefined => turns[turns.length - 1];

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
  conv.appendChild(el);
  turns.push({ el, body, textEl: body, raw: text, hasCaret: false, tools: new Map() });
  scrollBottom();
}

function addAssistantTurn(): Turn {
  const el = document.createElement("div");
  el.className = "turn assistant";
  el.innerHTML = `<div class="role">e</div><div class="body assistant"><div class="text"></div></div>`;
  const body = el.querySelector(".body") as HTMLElement;
  const textEl = el.querySelector(".text") as HTMLElement;
  conv.appendChild(el);
  const t: Turn = { el, body, textEl, raw: "", hasCaret: true, tools: new Map() };
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
      copyBtn.addEventListener("click", () => {
        void navigator.clipboard.writeText(output).then(() => notify("Copied to clipboard"));
      });
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
  if (/^workspace folder does not exist/i.test(raw)) {
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
    hint = "The provider is throttling requests — wait a few seconds and retry, or switch model.";
  } else if (code === "401" || code === "403") {
    title = "Authentication failed";
    hint = "Check this provider's API key in Settings (⚙).";
  } else if (code === "404") {
    title = "Model not available";
    hint = "The provider doesn't serve this model. Pick another from the model picker.";
  } else if (code.startsWith("5")) {
    title = "Provider error";
    hint = hint || "The provider failed server-side. Retrying usually helps.";
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
  copy.addEventListener("click", () => {
    void navigator.clipboard.writeText(info.raw).then(
      () => { copy.textContent = "copied"; setTimeout(() => (copy.textContent = "copy"), 1400); },
      () => notify("Copy failed", "error"),
    );
  });
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
  const t = lastTurn() || addAssistantTurn();
  removeCaret(t);
  t.body.appendChild(errorCard(msg));
  if (currentSession) cur().running = false;
  setBusy(false);
  updateInputState();
  updateEmpty();
  scrollBottom();
}

// ---------- send / events ----------
let availableModels: string[] = [];
let providers: ProviderItem[] = [];
let currentWs = "";
let activeProviderId = "";
/** Global fallback context window, used when a provider has no override. */
let defaultCtxWindow = 1_000_000;
let currentModels: string[] = [];
let currentModel = "";

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
}

/// Repaint the whole strip from the *current* chat's state. Called on every
/// state change and after switching chats, so the strip can never show
/// leftovers from another conversation.
function renderActivity(): void {
  const s = currentSession ? cur() : null;
  const running = !!s && s.running;
  const approval = s ? s.approval : null;

  activity.hidden = !running && !approval;
  if (approval) {
    actText.textContent = `allow ${approval.tool}?`;
    actStep.textContent = approval.preview || "";
    actStep.title = approval.preview || "";
  } else {
    actText.textContent = s ? s.activityText || "thinking…" : "";
    actStep.textContent = s ? s.activityStep : "";
    actStep.title = "";
  }
  actApprove.hidden = !approval;
  actDeny.hidden = !approval;
  actSteer.hidden = !running;
  activity.classList.toggle("awaiting", !!approval);

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

const HELP = `**Commands**\n- \`/new\` — new conversation\n- \`/model\` — switch model\n- \`/settings\` — open settings\n- \`/yolo [on|off]\` — auto-approve risky tools (shell, write_file)\n- \`/help\` — this help\n\n**File references**\nType \`@path\` (e.g. \`fix @src/main.ts\`) to include a file in context.`;

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
    t.textEl.innerHTML = renderMarkdown(md);
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
async function maybeCompact(sid: string): Promise<void> {
  const s = ui(sid);
  const used = s.ctxIn + s.liveIn;
  if (!ctxWindow || used < ctxWindow * COMPACT_AT) return;

  s.compacting = true;
  s.activityText = "compacting context…";
  if (sid === currentSession) {
    sbUpdate();
    renderActivity();
  }
  try {
    const r = await api.compactSession(sid);
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
    s.activityText = "thinking…";
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
  s.errored = false;
  s.startedAt = Date.now();
  s.activityText = "thinking…";
  s.activityStep = "";
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
    await api.sendText(sid, ctx ? text + "\n\n## Referenced files\n" + ctx : text, urls);
    if (onScreen) {
      attachments = [];
      renderAttachments();
    }
  } catch (e) {
    s.running = false;
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
  pluginHandlers.forEach((h) => {
    if (h.event === ev.type || h.event === "*") {
      try { h.handler(ev); } catch (e) { console.error("plugin handler", e); }
    }
  });

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

  const sid = (ev as unknown as { sid?: string }).sid || "";
  if (!sid) return;
  const chat = ui(sid);
  const onScreen = sid === currentSession;

  // ---- bookkeeping: happens for every chat, on screen or not ----
  let sidebarDirty = false;
  switch (ev.type) {
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
      chat.startedAt = 0;
      chat.approval = null;
      chatState[sid] = "error";
      sidebarDirty = true;
      // A background chat used to fail with nothing but a dot in the sidebar.
      if (!onScreen) notify("“" + sessionName(sid) + "” failed — " + explainError(ev.message).title, "error");
      break;
    case "done":
      chat.running = false;
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
        if (t) {
          removeCaret(t);
          if (t.thinkEl) t.thinkEl.classList.remove("live");
          t.textEl.innerHTML = renderMarkdown(t.raw);
        }
        break;
      }
      case "activity":
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
        const t = lastTurn();
        if (t) {
          removeCaret(t);
          t.textEl.innerHTML = renderMarkdown(t.raw);
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

// ---------- model picker (dropdown + autocomplete) ----------
const pickerMenu = document.getElementById("picker-menu") as HTMLElement;
const pickerInput = document.getElementById("picker-input") as HTMLInputElement;
const pickerList = document.getElementById("picker-list") as HTMLElement;

function renderPicker(): void {
  const q = pickerInput.value.trim().toLowerCase();
  const list = q ? currentModels.filter((m) => m.toLowerCase().includes(q)) : currentModels;
  pickerList.innerHTML = "";
  if (!list.length) {
    const e = document.createElement("div");
    e.className = "picker-empty";
    e.textContent = "No matching models";
    pickerList.appendChild(e);
    return;
  }
  list.forEach((m) => {
    const it = document.createElement("div");
    it.className = "picker-item" + (m === currentModel ? " active" : "");
    it.textContent = m;
    it.addEventListener("click", () => void selectModel(m));
    pickerList.appendChild(it);
  });
}
function openPicker(): void {
  renderPicker();
  pickerMenu.hidden = false;
  pickerInput.value = "";
  pickerInput.focus();
}
function closePicker(): void {
  pickerMenu.hidden = true;
}
async function selectModel(m: string): Promise<void> {
  try {
    const cfg = await api.getConfig();
    cfg.model = m;
    await api.saveConfig(cfg);
    currentModel = m;
    modelPill.textContent = m;
    sbModel.textContent = m;
    if (currentSession) await api.setSessionModel(currentSession, m);
    ctxWindow = await api.contextBudget().catch(() => ctxWindow);
    sbUpdate();
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
  const t = lastTurn();
  if (!t) return;
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
const sidebar = document.getElementById("sidebar") as HTMLElement;
const sessList = document.getElementById("sess-list") as HTMLElement;
const projAdd = document.getElementById("proj-add") as HTMLButtonElement;
const pinBtn = document.getElementById("sess-pin") as HTMLButtonElement;
let projects: api.ProjectMeta[] = [];
let currentProject = "";
let sessions: api.SessionMetaItem[] = [];
let currentSession = "";
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
  textEl.innerHTML = renderMarkdown(content);
  conv.appendChild(el);
  const t: Turn = { el, body: bodyEl, textEl, raw: content, hasCaret: false, tools: new Map() };
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
  chatTitle.textContent = ((p ? p.name : "Chats") + " / " + (s ? s.name : "New task"));
}

function updateChatBanner(): void {
  const p = projects.find((x) => x.id === currentProject);
  const sess = sessions.find((x) => x.id === currentSession);
  const el = document.getElementById("chat-banner");
  if (el) el.textContent = ((p ? p.name : "Chats") + " / " + (sess ? sess.name : "New task"));
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

function renderSessions(): void {
  sessList.innerHTML = "";
  projects.forEach((p) => {
    const open = openProjs.has(p.id);
    const row = document.createElement("div");
    row.className = "sess-item proj-folder" + (p.id === currentProject ? " active" : "");
    const lab = document.createElement("span");
    lab.className = "sess-label";
    lab.textContent = (open ? "▾ " : "▸ ") + (p.name || p.id);
    const pren = document.createElement("button");
    pren.className = "sess-act";
    pren.textContent = "✎";
    pren.title = "Rename project";
    const pdel = document.createElement("button");
    pdel.className = "sess-act";
    pdel.textContent = "×";
    pdel.title = "Delete project (chats are kept)";
    row.append(lab, pren, pdel);
    pren.addEventListener("click", (e) => {
      e.stopPropagation();
      openRenameModal(p.id, p.name || p.id, p.workspace || "");
    });
    pdel.addEventListener("click", (e) => {
      e.stopPropagation();
      void (async () => {
        if (!(await confirmModal("Delete project \"" + p.name + "\"? Its chats are kept and move to the remaining folder."))) return;
        await api.projectRemove(p.id);
        await refreshSessions();
        if (currentSession) await loadSession(currentSession);
      })();
    });
    row.addEventListener("click", () => {
      if (p.id !== currentProject) {
        currentProject = p.id;
        void api.switchProject(p.id);
      }
      if (openProjs.has(p.id)) openProjs.delete(p.id);
      else openProjs.add(p.id);
      renderSessions();
    });
    sessList.appendChild(row);
    if (!open) return;
    sessions
      .filter((s) => (s as unknown as { project?: string }).project === p.id)
      .forEach((s) => {
        const c = document.createElement("div");
        c.className = "sess-item sub" + (s.id === currentSession ? " active" : "");
        const st = chatState[s.id] || (s as unknown as { state?: string }).state || "idle";
        const dot = document.createElement("span");
        dot.className = "sess-dot " + st;
        dot.title = st;
        const label = document.createElement("span");
        label.className = "sess-label";
        label.textContent = s.name;
        label.title = "Open: " + s.name;
        const ren = document.createElement("button");
        ren.className = "sess-act";
        ren.textContent = "✎";
        ren.title = "Rename chat";
        const fork = document.createElement("button");
        fork.className = "sess-act";
        fork.textContent = "⧉";
        fork.title = "Fork";
        const del = document.createElement("button");
        del.className = "sess-act";
        del.textContent = "×";
        del.title = "Delete";
        c.append(dot, label, ren, fork, del);
        ren.addEventListener("click", (e) => {
          e.stopPropagation();
          openChatRename(s.id, s.name);
        });
        label.addEventListener("click", () => void loadSession(s.id));
        fork.addEventListener("click", (e) => {
          e.stopPropagation();
          void (async () => { await api.forkSession(s.id); await refreshSessions(); })();
        });
        del.addEventListener("click", (e) => {
          e.stopPropagation();
          void (async () => {
            if (!(await confirmModal("Delete chat \"" + s.name + "\"?"))) return;
            const wasCurrent = s.id === currentSession;
            await api.deleteSession(s.id);
            chatUI.delete(s.id);
            delete chatState[s.id];
            await refreshSessions();
            if (wasCurrent && currentSession) await loadSession(currentSession);
            else if (wasCurrent) {
              renderHistory([]);
              applyChatUI();
            }
          })();
        });
        sessList.appendChild(c);
      });
  });
}

async function refreshSessions(): Promise<void> {
  const [r, pr] = await Promise.all([api.listSessions(), api.listProjects()]);
  sessions = r.sessions;
  currentSession = r.current;
  projects = pr.projects;
  currentProject = pr.current;
  if (currentProject) openProjs.add(currentProject);

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
  const p0 = projects.find((x) => x.id === currentProject);
  const ws = p0 && p0.workspace ? p0.workspace : "";
  sbWs.textContent = ws || ".";
  // Flag a missing folder here rather than letting every tool call fail on it.
  const ok = ws ? await api.pathIsDir(ws) : true;
  sbWs.classList.toggle("missing", !ok);
  sbWs.title = ok ? ws : `Folder not found: ${ws} — open the project's ✎ to pick another`;
}

async function loadSession(id: string): Promise<void> {
  const s0 = sessions.find((x) => x.id === id);
  if (s0) {
    const proj = (s0 as unknown as { project?: string }).project;
    if (proj) {
      openProjs.add(proj);
      if (proj !== currentProject) {
        currentProject = proj;
        void api.switchProject(proj);
      }
    }
  }
  await api.switchSession(id);
  currentSession = id;
  const g = await api.getSession(id);
  if (g.model) {
    currentModel = g.model;
    modelPill.textContent = g.model;
    sbModel.textContent = g.model;
  }
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
  if (!pinned) closeSessions();
});
document.getElementById("sess-new")!.addEventListener("click", async () => {
  const meta = await api.newSession("", undefined, currentModel);
  await refreshSessions();
  if (meta.id) await loadSession(meta.id);
  closeSessions();
});
projAdd.addEventListener("click", () => openProjModal());

// ---------- slash command popup ----------
type SlashCmd = { name: string; desc: string };

const BUILTIN_SLASH: SlashCmd[] = [
  { name: "/new", desc: "new conversation" },
  { name: "/model", desc: "switch model" },
  { name: "/settings", desc: "open settings" },
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
    void api.cancelRun(currentSession);
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
  const s = cur();
  if (s.approval) {
    // A run parked on an approval prompt is waiting on the user, not the model.
    resolveApproval(false);
  }
  s.activityText = "stopping…";
  s.activityStep = "";
  renderActivity();
  const ok = await api.cancelRun(currentSession);
  if (!ok) {
    // Backend has no run for this chat: our flag was stale.
    s.running = false;
    s.startedAt = 0;
    chatState[currentSession] = "idle";
    renderSessions();
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

// ---------- settings (provider-centric) ----------
const overlay = document.createElement("div");
overlay.className = "overlay";
overlay.innerHTML = `
  <div class="modal">
    <h3>Settings</h3>
    <div class="field"><label>Providers</label><div id="provlist"></div></div>
    <div id="prov-edit" class="prov-edit" hidden>
      <div class="field"><label>Name</label><input id="cfg-pname" spellcheck="false"/></div>
      <div class="field"><label>API base URL</label><input id="cfg-base" spellcheck="false"/></div>
      <div class="field"><label>API key</label><input id="cfg-key" type="password" spellcheck="false"/></div>
      <div class="field"><label>Model</label><input id="cfg-model" list="model-list" spellcheck="false"/><datalist id="model-list"></datalist></div>
      <div class="prov-row"><button id="cfg-refresh" type="button" title="Fetch /models">Refresh</button><button id="cfg-delprov" type="button">Delete</button></div>
      <label class="lbl">Context window (tokens)</label><input id="cfg-ctxwin" type="number" step="1000" min="0" placeholder="1000000"/>
    </div>
    <div class="prov-row"><button id="cfg-addprov" type="button">+ New provider</button></div>
    <details class="field bhr">
      <summary>Behavior</summary>
            <label class="lbl">Temperature</label><input id="cfg-temp" type="number" step="0.1" min="0" max="2"/>
      <label class="lbl">System prompt</label><textarea id="cfg-sys" rows="3"></textarea>
      <label class="lbl cfg-check"><input id="cfg-yolo" type="checkbox"/> YOLO mode — run shell &amp; write_file without asking</label>
    </details>
    <p class="note">Stored in <code>~/.e/config.json</code>. &ldquo;Refresh&rdquo; fetches <code>&lt;base&gt;/models</code>.</p>
    <div class="modal-actions">
      <button id="cfg-cancel">Cancel</button>
      <button id="cfg-save" class="primary">Save</button>
    </div>
  </div>`;
document.body.appendChild(overlay);

function populateModelList(): void {
  const dl = overlay.querySelector("#model-list") as HTMLDataListElement;
  dl.innerHTML = "";
  availableModels.forEach((m) => {
    const o = document.createElement("option");
    o.value = m;
    dl.appendChild(o);
  });
  const mi = overlay.querySelector("#cfg-model") as HTMLInputElement;
  mi.value = currentModel && availableModels.includes(currentModel) ? currentModel : availableModels[0] || "";
}

function loadProviderToForm(p?: ProviderItem): void {
  if (!p) return;
  activeProviderId = p.id;
  (overlay.querySelector("#cfg-pname") as HTMLInputElement).value = p.name || p.id;
  (overlay.querySelector("#cfg-base") as HTMLInputElement).value = p.base_url;
  (overlay.querySelector("#cfg-key") as HTMLInputElement).value = p.api_key;
  (overlay.querySelector("#cfg-ctxwin") as HTMLInputElement).value = String(p.context_window || defaultCtxWindow);
  availableModels = p.models || [];
  currentModel = availableModels.includes(currentModel) ? currentModel : availableModels[0] || "";
  populateModelList();
  const pane = overlay.querySelector("#prov-edit") as HTMLElement;
  if (pane) pane.hidden = false;
}

function renderProviderSelect(): void {
  const box = overlay.querySelector("#provlist") as HTMLElement;
  box.innerHTML = "";
  providers.forEach((p) => {
    const row = document.createElement("div");
    row.className = "prov-item" + (p.id === activeProviderId ? " active" : "");
    const lab = document.createElement("span");
    lab.className = "prov-name";
    lab.textContent = (p.name || p.id) + "  ·  " + hostOf(p.base_url);
    const edit = document.createElement("button");
    edit.className = "sess-act";
    edit.textContent = "✎";
    edit.title = "Edit provider";
    const del = document.createElement("button");
    del.className = "sess-act";
    del.textContent = "×";
    del.title = "Delete provider";
    row.append(lab, edit, del);
    edit.addEventListener("click", () => {
      activeProviderId = p.id;
      loadProviderToForm(p);
    });
    del.addEventListener("click", async () => {
      if (!(await confirmModal("Delete provider \"" + p.name + "\"?"))) return;
      const i = providers.findIndex((x) => x.id === p.id);
      if (i >= 0) providers.splice(i, 1);
      if (activeProviderId === p.id) activeProviderId = providers[0]?.id || "";
      renderProviderSelect();
      if (providers.length) loadProviderToForm(providers.find((x) => x.id === activeProviderId) || providers[0]);
      else (overlay.querySelector("#prov-edit") as HTMLElement).hidden = true;
    });
    box.appendChild(row);
  });
}

(overlay.querySelector("#cfg-refresh") as HTMLButtonElement).addEventListener("click", async () => {
  const p = providers.find((x) => x.id === activeProviderId);
  if (!p) return;
  statusText.textContent = "refreshing…";
  try {
    const list = await api.refreshModels(p.base_url, p.api_key);
    p.models = list;
    availableModels = list;
    currentModel = list[0] || currentModel;
    populateModelList();
    statusText.textContent = list.length + " models";
  } catch (e) {
    statusText.textContent = "refresh failed";
    console.error(e);
  }
});

(overlay.querySelector("#cfg-addprov") as HTMLButtonElement).addEventListener("click", async () => {
  const n = providers.length + 1;
  const p: ProviderItem = { id: "p" + Date.now().toString(36), name: "Provider " + n, base_url: "", api_key: "", models: [], context_window: null };
  providers.push(p);
  activeProviderId = p.id;
  renderProviderSelect();
  loadProviderToForm(p);
  (overlay.querySelector("#cfg-pname") as HTMLInputElement).focus();
});

(overlay.querySelector("#cfg-delprov") as HTMLButtonElement).addEventListener("click", async () => {
  if (!(await confirmModal("Delete this provider?"))) return;
  const i = providers.findIndex((x) => x.id === activeProviderId);
  if (i >= 0) providers.splice(i, 1);
  activeProviderId = providers[0]?.id || "";
  renderProviderSelect();
  if (providers.length) loadProviderToForm(providers.find((x) => x.id === activeProviderId) || providers[0]);
  else (overlay.querySelector("#prov-edit") as HTMLElement).hidden = true;
});

async function openSettings(): Promise<void> {
  const cfg = await api.getConfig();
  providers = cfg.providers || [];
  availableModels = cfg.models || [];
  currentModel = cfg.model || "";
  // Align the edit target with the connection actually in use, so saving
  // without switching providers can't write the field values onto a different
  // provider than the one the form was populated from.
  const activeProv = cfg.providers?.find((p) => p.base_url === cfg.base_url) || cfg.providers?.[0];
  activeProviderId = activeProv?.id || "";
  currentWs = cfg.workspace;
  (overlay.querySelector("#cfg-temp") as HTMLInputElement).value = String(cfg.temperature);
  defaultCtxWindow = cfg.context_window || 1_000_000;
  (overlay.querySelector("#cfg-ctxwin") as HTMLInputElement).value = String(activeProv?.context_window || defaultCtxWindow);
  (overlay.querySelector("#cfg-sys") as HTMLTextAreaElement).value = cfg.system;
  (overlay.querySelector("#cfg-yolo") as HTMLInputElement).checked = !!cfg.yolo;
  renderProviderSelect();
  overlay.classList.add("open");
}

function closeSettings(): void {
  overlay.classList.remove("open");
}
overlay.querySelector("#cfg-cancel")!.addEventListener("click", closeSettings);
overlay.querySelector("#cfg-save")!.addEventListener("click", async () => {
  const active = providers.find((p) => p.id === activeProviderId) || providers[0];
  if (!active) return;
  active.name = (overlay.querySelector("#cfg-pname") as HTMLInputElement).value.trim() || active.id;
  active.base_url = (overlay.querySelector("#cfg-base") as HTMLInputElement).value.trim();
  active.api_key = (overlay.querySelector("#cfg-key") as HTMLInputElement).value.trim();
  active.models = availableModels;
  const model = (overlay.querySelector("#cfg-model") as HTMLInputElement).value.trim() || availableModels[0] || "";
  const workspace = currentWs;
  const temperature = parseFloat((overlay.querySelector("#cfg-temp") as HTMLInputElement).value) || 1;
  const system = (overlay.querySelector("#cfg-sys") as HTMLTextAreaElement).value;
  const yolo = (overlay.querySelector("#cfg-yolo") as HTMLInputElement).checked;
  const ctxWin = parseInt((overlay.querySelector("#cfg-ctxwin") as HTMLInputElement).value, 10);
  active.context_window = ctxWin > 0 ? ctxWin : null;
  const cfg: Config = {
    base_url: active.base_url,
    api_key: active.api_key,
    model,
    workspace,
    system,
    temperature,
    yolo,
    models: availableModels,
    context_window: defaultCtxWindow,
    providers,
  };
  await api.saveConfig(cfg);
  ctxWindow = await api.contextBudget().catch(() => ctxWindow);
  sbUpdate();
  setYoloIndicator(yolo);
  currentModel = model;
  currentModels = availableModels;
  modelPill.textContent = model || "(none)";
  sbModel.textContent = model || "?";
  sbProv.textContent = "[" + activeProviderName(cfg) + "]";
  closeSettings();
});
overlay.addEventListener("click", (e) => {
  if (e.target === overlay) closeSettings();
});
document.getElementById("btn-settings")!.addEventListener("click", () => void openSettings());


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
    ctxWindow = await api.contextBudget().catch(() => defaultCtxWindow);
    modelPill.textContent = cfg.model || "configure model";
    currentModel = cfg.model || "";
    currentModels = cfg.models || [];
    sbModel.textContent = cfg.model || "?";
    sbProv.textContent = "[" + activeProviderName(cfg) + "]";
    setYoloIndicator(!!cfg.yolo);
    await refreshSessions();
    if (pinned) openSessions();
    if (currentSession) {
      const g = await api.getSession(currentSession);
      renderHistory(g.messages);
      seedContextEstimate(currentSession, g.context_estimate);
      ui(currentSession).running = !!g.running;
      if (g.model) {
        currentModel = g.model;
        modelPill.textContent = g.model;
        sbModel.textContent = g.model;
      }
    }
    updateChatTitle();
    updateChatBanner();
    applyChatUI();
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
  const meta = await api.newSession("Chat 1", dir, currentModel);
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
  rmWs.textContent = ws || "(not set)";
  rmWarn.hidden = ws ? await api.pathIsDir(ws) : false;
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

// ---------- plugins (P0/P1) ----------
const pluginTools: api.PluginToolDef[] = [];
const pluginReg = new Map<string, (args: Record<string, unknown>) => unknown>();
const pluginHandlers: { event: string; handler: (ev: api.EngineEvents) => unknown }[] = [];
const pluginCommands: Record<string, { run: () => void; desc: string }> = {};

interface PluginAPIHost {
  registerTool(def: api.PluginToolDef & { run: (args: Record<string, unknown>) => unknown }): void;
  on(event: string, handler: (ev: api.EngineEvents) => unknown): void;
  registerCommand(name: string, fn: () => void, desc?: string): void;
  ui: { notify: (msg: string, kind?: string) => void; confirm: (msg: string) => Promise<boolean> };
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

function buildPluginApi(): PluginAPIHost {
  return {
    registerTool(def) {
      pluginReg.set(def.name, def.run);
      pluginTools.push({ name: def.name, description: def.description, parameters: def.parameters || { type: "object" } });
      void api.setPluginTools(pluginTools);
    },
    on(event, handler) {
      pluginHandlers.push({ event, handler });
    },
    registerCommand(name, fn, desc) {
      pluginCommands[name] = { run: fn, desc: desc || "plugin command" };
    },
    ui: { notify, confirm: (msg: string) => confirmModal(msg, "Plugin") },
  };
}

function loadPluginSource(source: string, apiObj: PluginAPIHost): void {
  try {
    const body = source.replace(/^\s*export\s+default\s+/m, "");
    const factory = (0, eval)("(" + body + ")");
    if (typeof factory === "function") factory(apiObj);
  } catch (e) {
    console.error("plugin load error", e);
    notify("Plugin failed to load", "error");
  }
}

async function loadPlugins(): Promise<void> {
  try {
    const plugs = await api.listPlugins();
    for (const p of plugs) {
      try {
        const g = await api.getPlugin(p.name);
        loadPluginSource(g.source, buildPluginApi());
      } catch (e) {
        console.error("plugin", p.name, e);
      }
    }
    if (pluginTools.length) void api.setPluginTools(pluginTools);
  } catch {
    /* not running in tauri */
  }
}

void loadPlugins();
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
