// e — codemode host: runs model-written scripts in sandboxed iframes.
//
// The engine emits a script on `e:codemode_run` and blocks; every tool call
// the script makes round-trips through `codemode_tool_call`, passing the same
// gates (plugin veto, approval, execution) as a call the model made itself,
// and exactly one `codemode_result` hands back the distilled output. Runs are
// keyed by sid because two chats can be streaming — and scripting — at once.
import * as api from "./api";
import type { CodemodeRun } from "./api";

// Subscribed at import time rather than inside bootCodemode: the listener must
// be on its way live before anything announces this window as a host, and
// main.ts imports this module long before init() runs the handshake.
api.onCodemodeRun(runCodemode);

/// Tool calls one script may keep in flight. The engine executes them
/// concurrently, but a loop firing hundreds at once buries each approval
/// prompt under the pile; extras queue FIFO, which is what Promise.all wants.
const MAX_INFLIGHT = 4;

/// Ceiling on what one run may hand back, so a `text()` inside a runaway
/// loop cannot wedge the conversation. Past the cap the middle goes: head
/// and tail are what a model reading its own transcript actually needs.
const MAX_OUTPUT = 100_000;

/// A run in flight. The iframe is the script's whole world; the token is the
/// only identity to check inbound messages against, because a sandboxed
/// srcdoc frame has an opaque origin — there is no origin string to compare.
type Run = {
  sid: string;
  session: string;
  token: string;
  frame: HTMLIFrameElement;
  watchdog: number;
  /// Flipped by the first outcome (cm:done or the watchdog). The backend
  /// no-ops a late second answer, but a host that needed that mercy would be
  /// buggy by design — one run, one result.
  settled: boolean;
  off: () => void;
};

/// What the frame reports on completion. Items are strings already — the
/// frame stringifies — and a failure keeps whatever was collected before the
/// throw, because partial output beats a bare stack trace.
type FrameDone = { ok: boolean; items: string[]; console: string[]; error?: string; ms: number };

const runs = new Map<string, Run>();

/// Start one run: build the frame, arm the watchdog, wait. Nothing here
/// blocks — completion arrives as a message, or as the watchdog firing.
export function runCodemode(run: CodemodeRun): void {
  // The engine mints a fresh sid per run, so a duplicate delivery would be a
  // bug elsewhere; honouring it here would answer one run twice.
  if (runs.has(run.sid)) return;
  const token = newToken();
  const frame = document.createElement("iframe");
  // allow-scripts alone, never allow-same-origin: the frame must stay an
  // opaque origin or its script could reach this window's DOM and storage.
  frame.setAttribute("sandbox", "allow-scripts");
  frame.style.display = "none";
  frame.srcdoc = frameDoc(token);

  const r: Run = { sid: run.sid, session: run.session, token, frame, watchdog: 0, settled: false, off: () => undefined };

  const onMessage = (ev: MessageEvent) => {
    if (ev.source !== frame.contentWindow) return;
    const m = ev.data as Record<string, unknown> | null;
    if (!m || m.id !== token) return;
    if (m.type === "cm:call") {
      const callId = String(m.callId || "");
      const name = String(m.name || "");
      const args = m.args && typeof m.args === "object" ? (m.args as Record<string, unknown>) : {};
      if (callId && name) callTool(r, callId, name, args);
      return;
    }
    if (m.type === "cm:done") {
      const d = doneFrom(m);
      finish(r, d.ok, shape(d));
    }
  };
  window.addEventListener("message", onMessage);
  r.off = () => window.removeEventListener("message", onMessage);

  // The inline script runs before `load` fires, so the frame's listener is
  // guaranteed to exist by the time the code lands.
  frame.addEventListener("load", () => {
    frame.contentWindow?.postMessage({ type: "cm:run", id: token, code: run.code }, "*");
  });

  // The budget is generous on purpose — a tool call parked on a human
  // approval is still running — but past it the script is dead and must not
  // linger as an iframe.
  const budget = run.timeoutMs > 0 ? run.timeoutMs : 600_000;
  r.watchdog = window.setTimeout(() => {
    const s = budget / 1000;
    const secs = Number.isInteger(s) ? String(s) : s.toFixed(1);
    finish(r, false, `Script timed out after ${secs}s — the run budget covers tool calls waiting on approval too`);
  }, budget);

  runs.set(run.sid, r);
  document.body.appendChild(frame);
}

/// Forward one script tool call to the engine and route its answer home. A
/// refusal (veto, denied approval, failed tool) is a result carrying the
/// reason; a rejected invoke means the run is dead engine-side. Both reject
/// the script's promise — with the reason, not a generic transport error.
function callTool(r: Run, callId: string, name: string, args: Record<string, unknown>): void {
  api.codemodeToolCall(r.sid, r.session, name, args).then(
    (res) => reply(r, callId, res.ok, res.output),
    (e) => reply(r, callId, false, String(e)),
  );
}

function reply(r: Run, callId: string, ok: boolean, output: string): void {
  // The watchdog may have torn the frame down while the engine was still
  // gating the call; a reply into a dead frame is dropped, never fatal.
  if (r.settled) return;
  r.frame.contentWindow?.postMessage({ type: "cm:callres", id: r.token, callId, ok, output }, "*");
}

/// The single exit path: exactly one `codemode_result` per run, the frame
/// gone, the listener off, the watchdog disarmed — on every outcome.
function finish(r: Run, ok: boolean, output: string): void {
  if (r.settled) return;
  r.settled = true;
  clearTimeout(r.watchdog);
  r.off();
  runs.delete(r.sid);
  r.frame.remove();
  // A rejection here means the engine is gone (window closing); nothing
  // remains to answer.
  api.codemodeResult(r.sid, ok, output).catch(() => undefined);
}

function doneFrom(m: Record<string, unknown>): FrameDone {
  return {
    ok: m.ok === true,
    items: Array.isArray(m.items) ? m.items.map(String) : [],
    console: Array.isArray(m.console) ? m.console.map(String) : [],
    error: typeof m.error === "string" ? m.error : undefined,
    ms: typeof m.ms === "number" ? m.ms : 0,
  };
}

/// Collapse a run into the one string the model sees: verdict and cost first,
/// the script's own output next (bare when singular, framed when not, so a
/// multi-item run counts honestly), diagnostics fenced off at the end, and a
/// failure's reason last — where an error belongs in a transcript.
function shape(d: FrameDone): string {
  const lines = [`${d.ok ? "Script completed" : "Script failed"} in ${(d.ms / 1000).toFixed(1)}s`];
  if (d.items.length === 1) lines.push(d.items[0]);
  else d.items.forEach((it, i) => lines.push(`==> text ${i + 1}/${d.items.length} <==`, it));
  if (d.console.length) lines.push("", "<console_output>", ...d.console, "</console_output>");
  if (!d.ok && d.error) lines.push("Script error: " + d.error);
  const out = lines.join("\n");
  if (out.length <= MAX_OUTPUT) return out;
  const half = MAX_OUTPUT / 2;
  return out.slice(0, half) + `\n[… truncated ${out.length - MAX_OUTPUT} chars …]\n` + out.slice(-half);
}

function newToken(): string {
  const bytes = new Uint8Array(16);
  crypto.getRandomValues(bytes);
  return Array.from(bytes, (b) => b.toString(16).padStart(2, "0")).join("");
}

/// The frame's document. The CSP strips network, storage and every origin the
/// parent might vouch for; the sandbox attribute is what actually isolates
/// the frame, so `'unsafe-eval'` (the Function constructor the runner needs —
/// `'unsafe-inline'` alone would block it) adds nothing the script could use:
/// there is no origin to reach and no network to reach it through.
function frameDoc(token: string): string {
  return `<!doctype html><html><head><meta http-equiv="Content-Security-Policy" content="default-src 'none'; script-src 'unsafe-inline' 'unsafe-eval'"></head><body><script>
(function () {
  "use strict";
  var ID = ${JSON.stringify(token)};
  var LIMIT = ${MAX_INFLIGHT};
  var calls = {};
  var waiting = [];
  var inflight = 0;
  var seq = 0;
  var items = [];
  var lines = [];
  var t0 = 0;
  // Private sentinel: exit() unwinds via an exception only this wrapper ever
  // matches, so stopping early can never be confused with a script error.
  var EXIT = {};

  function post(msg) { msg.id = ID; parent.postMessage(msg, "*"); }

  function dispatch(e) {
    inflight++;
    post({ type: "cm:call", callId: e.callId, name: e.name, args: e.args });
  }

  function call(name, args) {
    return new Promise(function (resolve, reject) {
      var e = { callId: "c" + (++seq), name: name, args: args, resolve: resolve, reject: reject };
      calls[e.callId] = e;
      if (inflight < LIMIT) dispatch(e);
      else waiting.push(e);
    });
  }

  function fmt(v) {
    if (typeof v === "string") return v;
    if (v instanceof Error) return String(v && v.stack || v);
    try { return JSON.stringify(v); } catch (x) { return String(v); }
  }

  function text(v) { items.push(fmt(v)); }

  function line() {
    var parts = [];
    for (var i = 0; i < arguments.length; i++) parts.push(fmt(arguments[i]));
    lines.push(parts.join(" "));
  }
  var con = { log: line, info: line, warn: line, error: line, debug: line };

  function exit() { throw EXIT; }

  // Any tool name works without a list here: the engine is the authority on
  // what exists, so a tool registered tomorrow is callable by today's code.
  // "then" must stay undefined or "await tools" would try to call it, and
  // symbol keys (Symbol.toPrimitive and friends) are not tool names either.
  var tools = new Proxy({}, {
    get: function (_t, prop) {
      if (typeof prop !== "string" || prop === "then") return undefined;
      return function (args) { return call(prop, args || {}); };
    }
  });

  function done(ok, error) {
    post({ type: "cm:done", ok: ok, items: items, console: lines, error: error, ms: Date.now() - t0 });
  }

  function run(code) {
    t0 = Date.now();
    var fn;
    try {
      // The script is the body of an async function — top-level await works —
      // and the harness names are parameters, so nothing under those names
      // can be reached on the frame's real globals.
      fn = new Function("tools", "text", "console", "exit", '"use strict";return (async ()=>{\\n' + code + "\\n})();");
    } catch (e) {
      done(false, String(e && e.stack || e).slice(0, 2000));
      return;
    }
    window.console = con;
    fn(tools, text, con, exit).then(
      function () { done(true); },
      function (err) {
        if (err === EXIT) done(true);
        // Partial output survives the failure: what the script had already
        // said is the context for the stack that killed it.
        else done(false, String(err && err.stack || err).slice(0, 2000));
      }
    );
  }

  window.addEventListener("message", function (ev) {
    if (ev.source !== parent) return;
    var m = ev.data;
    if (!m || m.id !== ID) return;
    if (m.type === "cm:run") run(m.code);
    else if (m.type === "cm:callres") {
      var e = calls[m.callId];
      if (!e) return;
      delete calls[m.callId];
      inflight--;
      if (waiting.length) dispatch(waiting.shift());
      if (m.ok === true) e.resolve(String(m.output));
      else e.reject(new Error(String(m.output)));
    }
  });
})();
</script></body></html>`;
}

/// Announce this window as a codemode host. The engine refuses the codemode
/// tool outright until this lands, so it must never fire before the run
/// listener above is registered — a script emitted at an unhosted window
/// would block a whole run budget on an event nobody answers.
export async function bootCodemode(): Promise<void> {
  if (!api.isTauri) return;
  await api.setCodemodeActive(true);
}
