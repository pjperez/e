// e — copy-to-clipboard affordances.
// One implementation for every "copy" in the UI: message turns, code blocks,
// tool output, and error details.

const COPY_ICON = `<svg class="copy-glyph" viewBox="0 0 16 16" width="12" height="12" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><rect x="5.75" y="5.75" width="8.5" height="8.5" rx="2"/><path d="M11 4V3.75A2 2 0 0 0 9 1.75H3.75A2 2 0 0 0 1.75 3.75V9a2 2 0 0 0 2 2H4"/></svg>`;
const CHECK_ICON = `<svg class="copy-glyph" viewBox="0 0 16 16" width="12" height="12" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><path d="M3 8.5l3.3 3.3L13 5"/></svg>`;

/// `navigator.clipboard` is the happy path, but the OS WebViews Tauri sits on
/// refuse it whenever the document isn't focused — clicking a background window
/// is enough. The textarea + execCommand dance is the escape hatch.
export async function copyText(text: string): Promise<boolean> {
  if (!text) return false;
  try {
    await navigator.clipboard.writeText(text);
    return true;
  } catch {
    /* fall through to the legacy path */
  }
  try {
    const ta = document.createElement("textarea");
    ta.value = text;
    ta.setAttribute("readonly", "");
    ta.style.cssText = "position:fixed;top:0;left:-9999px;opacity:0";
    document.body.appendChild(ta);
    ta.select();
    const ok = document.execCommand("copy");
    ta.remove();
    return ok;
  } catch {
    return false;
  }
}

/// Turn any button into a copy button. `get` is read at click time, so a turn
/// that is still streaming copies what it has arrived at rather than the empty
/// string it started as. The pristine label is captured once, so hammering the
/// button can't leave "copied" stuck as the permanent caption.
export function wireCopy(btn: HTMLElement, get: () => string): void {
  const original = btn.innerHTML;
  let timer = 0;
  btn.addEventListener("click", (e) => {
    e.stopPropagation();
    void copyText(get()).then((ok) => {
      window.clearTimeout(timer);
      btn.classList.remove("copied", "copy-failed");
      btn.classList.add(ok ? "copied" : "copy-failed");
      btn.innerHTML = ok
        ? `${CHECK_ICON}<span class="copy-label">copied</span>`
        : `${COPY_ICON}<span class="copy-label">failed</span>`;
      timer = window.setTimeout(() => {
        btn.classList.remove("copied", "copy-failed");
        btn.innerHTML = original;
      }, 1400);
    });
  });
}

export function copyButton(label: string, get: () => string): HTMLButtonElement {
  const b = document.createElement("button");
  b.type = "button";
  b.className = "copy-btn";
  b.title = `Copy ${label}`;
  b.setAttribute("aria-label", `Copy ${label}`);
  b.innerHTML = `${COPY_ICON}<span class="copy-label">copy</span>`;
  wireCopy(b, get);
  return b;
}

/// Hover-revealed copy in a turn's role row — out of the text flow, so it can
/// never sit on top of the message it belongs to.
export function attachTurnCopy(el: HTMLElement, label: string, get: () => string): void {
  const role = el.querySelector(".role");
  if (role) role.appendChild(copyButton(label, get));
}

/// Markdown is re-rendered wholesale on every update, which throws away any
/// buttons hung on the previous nodes — so decoration re-runs after each render
/// and skips blocks it has already wrapped.
///
/// The wrapper (rather than the `<pre>` itself) anchors the button: `pre`
/// scrolls horizontally, and an absolutely positioned child would slide out of
/// its corner along with the code.
export function decorateCodeBlocks(root: HTMLElement): void {
  for (const pre of Array.from(root.querySelectorAll("pre"))) {
    if (pre.parentElement?.classList.contains("codeblock")) continue;
    const code = pre.querySelector("code");
    const wrap = document.createElement("div");
    wrap.className = "codeblock";
    pre.replaceWith(wrap);
    wrap.appendChild(pre);
    wrap.appendChild(copyButton("code", () => (code ?? pre).textContent ?? ""));
  }
}
