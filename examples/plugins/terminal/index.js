// Terminal — a real shell in e's side pane.
//
// The `pty` capability gives this plugin a byte stream and a size; everything
// that turns those bytes into something readable lives here. That split is
// deliberate: Rust should not have opinions about how a cursor is drawn, and a
// terminal emulator has no business owning a process handle.
//
// It is a single file because a plugin is loaded as one ES module from a blob
// URL — there is no base URL for a relative import to resolve against.
//
// Scope: enough VT100/xterm to make a shell, `git`, `ls --color`, `top` and a
// pager behave. Not a full emulator — no double-width characters, no reflow of
// wrapped lines on resize, no sixel.

const SCROLLBACK = 5000;

// xterm's first 16, in two flavours so the pane is legible against a light
// theme as well as a dark one. Indices 0-7 normal, 8-15 bright.
const PALETTE_DARK = [
  "#1c1f26", "#f87171", "#6ee7b7", "#fbbf24", "#60a5fa", "#c084fc", "#67e8f9", "#d1d5db",
  "#4b5563", "#fca5a5", "#a7f3d0", "#fde68a", "#93c5fd", "#e9d5ff", "#a5f3fc", "#ffffff",
];
const PALETTE_LIGHT = [
  "#1f2328", "#b91c1c", "#047857", "#a16207", "#1d4ed8", "#7e22ce", "#0e7490", "#4b5563",
  "#6b7280", "#dc2626", "#059669", "#ca8a04", "#2563eb", "#9333ea", "#0891b2", "#111827",
];

/** The 256-colour cube and greyscale ramp, for `38;5;n`. */
function xterm256(n, palette) {
  if (n < 16) return palette[n];
  if (n < 232) {
    const i = n - 16;
    const step = (v) => (v === 0 ? 0 : 55 + v * 40);
    const r = step(Math.floor(i / 36) % 6);
    const g = step(Math.floor(i / 6) % 6);
    const b = step(i % 6);
    return `rgb(${r},${g},${b})`;
  }
  const v = 8 + (n - 232) * 10;
  return `rgb(${v},${v},${v})`;
}

const BLANK = { ch: " ", fg: null, bg: null, bold: false, dim: false, ul: false, inv: false, italic: false };

function cell(ch, a) {
  return { ch, fg: a.fg, bg: a.bg, bold: a.bold, dim: a.dim, ul: a.ul, inv: a.inv, italic: a.italic };
}

function sameStyle(a, b) {
  return a.fg === b.fg && a.bg === b.bg && a.bold === b.bold && a.dim === b.dim
    && a.ul === b.ul && a.inv === b.inv && a.italic === b.italic;
}

/**
 * A VT screen: a grid, a cursor, and a parser that mutates both.
 *
 * Lines that scroll off the top move to `scrollback` rather than being dropped,
 * because a terminal you cannot scroll back through is a log viewer that
 * forgets. `onCommit` lets the view append those lines once instead of
 * re-rendering thousands of frozen rows on every keystroke.
 */
class Screen {
  constructor(cols, rows, onCommit, onReply, onUncommit) {
    this.cols = Math.max(1, cols);
    this.rows = Math.max(1, rows);
    this.onCommit = onCommit;
    /** A row taken back out of scrollback by a resize; the view must drop the
     *  copy it already drew, or the line shows up twice. */
    this.onUncommit = onUncommit;
    // Some sequences are *questions*. A shell's line editor asks where the
    // cursor is and blocks until the terminal answers, so a view that never
    // replies gets no prompt at all — the pty is fine, the conversation just
    // never starts.
    this.onReply = onReply;
    this.scrollback = [];
    this.buf = [];
    for (let y = 0; y < this.rows; y++) this.buf.push(this.blankRow());
    this.x = 0;
    this.y = 0;
    this.saved = null;
    this.attr = { ...BLANK };
    this.cursorVisible = true;
    this.wrapNext = false;
    this.autowrap = true;
    this.appCursor = false;
    this.bracketedPaste = false;
    this.top = 0;
    this.bottom = this.rows - 1;
    this.alt = null;
    this.title = "";
    // Parser
    this.state = "ground";
    this.params = "";
    this.osc = "";
    this.pending = "";
  }

  blankRow() {
    const r = new Array(this.cols);
    for (let i = 0; i < this.cols; i++) r[i] = { ...BLANK };
    return r;
  }

  resize(cols, rows) {
    cols = Math.max(1, cols);
    rows = Math.max(1, rows);
    if (cols === this.cols && rows === this.rows) return false;
    // Growing pulls lines back out of scrollback so the prompt does not jump
    // away from the bottom of the window when the pane is widened. Those rows
    // were already drawn into the frozen history, so the view has to un-draw
    // them or the last few lines of output appear twice.
    while (rows > this.rows) {
      const back = this.scrollback.pop();
      if (back) {
        this.buf.unshift(back);
        this.y++;
        if (this.onUncommit) this.onUncommit();
      } else {
        this.buf.push(this.blankRowOf(cols));
      }
      this.rows++;
    }
    while (rows < this.rows) {
      // Prefer trimming blank tail rows; only then push real content up.
      const last = this.buf[this.buf.length - 1];
      if (this.y < this.rows - 1 && last.every((c) => c.ch === " ")) {
        this.buf.pop();
      } else {
        const first = this.buf.shift();
        this.commit(first);
        this.y = Math.max(0, this.y - 1);
      }
      this.rows--;
    }
    this.cols = cols;
    for (const row of this.buf) {
      while (row.length < cols) row.push({ ...BLANK });
      row.length = cols;
    }
    this.x = Math.min(this.x, cols - 1);
    this.y = Math.min(this.y, rows - 1);
    this.top = 0;
    this.bottom = rows - 1;
    this.wrapNext = false;
    return true;
  }

  blankRowOf(cols) {
    const r = new Array(cols);
    for (let i = 0; i < cols; i++) r[i] = { ...BLANK };
    return r;
  }

  commit(row) {
    // The alternate screen is scratch space (that is what `vim` and `less` use
    // it for); committing it would fill scrollback with a dead editor.
    if (this.alt) return;
    this.scrollback.push(row);
    if (this.scrollback.length > SCROLLBACK) this.scrollback.shift();
    if (this.onCommit) this.onCommit(row);
  }

  scrollUp(n = 1) {
    for (let i = 0; i < n; i++) {
      const row = this.buf.splice(this.top, 1)[0];
      // Only lines leaving the top of the *screen* are history; a scroll region
      // is a pager repainting inside a box.
      if (this.top === 0) this.commit(row);
      this.buf.splice(this.bottom, 0, this.blankRow());
    }
  }

  scrollDown(n = 1) {
    for (let i = 0; i < n; i++) {
      this.buf.splice(this.bottom, 1);
      this.buf.splice(this.top, 0, this.blankRow());
    }
  }

  put(ch) {
    if (this.wrapNext && this.autowrap) {
      this.x = 0;
      this.lineFeed();
      this.wrapNext = false;
    }
    this.buf[this.y][this.x] = cell(ch, this.attr);
    if (this.x + 1 >= this.cols) this.wrapNext = true;
    else this.x++;
  }

  lineFeed() {
    if (this.y === this.bottom) this.scrollUp(1);
    else if (this.y < this.rows - 1) this.y++;
  }

  eraseInRow(y, from, to) {
    const row = this.buf[y];
    for (let i = from; i <= to && i < this.cols; i++) row[i] = { ...BLANK, bg: this.attr.bg };
  }

  write(data) {
    const s = this.pending + data;
    this.pending = "";
    for (let i = 0; i < s.length; i++) {
      const ch = s[i];
      const code = s.charCodeAt(i);
      switch (this.state) {
        case "ground":
          if (ch === "\x1b") this.state = "escape";
          else if (ch === "\r") { this.x = 0; this.wrapNext = false; }
          else if (ch === "\n" || ch === "\v" || ch === "\f") { this.lineFeed(); this.wrapNext = false; }
          else if (ch === "\b") { this.x = Math.max(0, this.x - (this.wrapNext ? 0 : 1)); this.wrapNext = false; }
          else if (ch === "\t") { this.x = Math.min(this.cols - 1, (Math.floor(this.x / 8) + 1) * 8); this.wrapNext = false; }
          else if (ch === "\x07") { /* bell: deliberately silent */ }
          else if (code < 0x20 || code === 0x7f) { /* other C0: ignored */ }
          else this.put(ch);
          break;

        case "escape":
          if (ch === "[") { this.state = "csi"; this.params = ""; }
          else if (ch === "]") { this.state = "osc"; this.osc = ""; }
          else if (ch === "(" || ch === ")" || ch === "*" || ch === "+") this.state = "charset";
          else if (ch === "M") { if (this.y === this.top) this.scrollDown(1); else this.y = Math.max(0, this.y - 1); this.state = "ground"; }
          else if (ch === "D") { this.lineFeed(); this.state = "ground"; }
          else if (ch === "E") { this.x = 0; this.lineFeed(); this.state = "ground"; }
          else if (ch === "7") { this.saveCursor(); this.state = "ground"; }
          else if (ch === "8") { this.restoreCursor(); this.state = "ground"; }
          else if (ch === "c") { this.reset(); this.state = "ground"; }
          else this.state = "ground";
          break;

        case "charset":
          this.state = "ground";
          break;

        case "csi":
          // Parameter and intermediate bytes accumulate; the first byte in
          // 0x40-0x7E ends the sequence and says what it was.
          if (code >= 0x40 && code <= 0x7e) {
            this.csi(ch);
            this.state = "ground";
          } else {
            this.params += ch;
            // A runaway sequence must not grow without bound.
            if (this.params.length > 64) this.state = "ground";
          }
          break;

        case "osc":
          if (ch === "\x07") { this.oscEnd(); this.state = "ground"; }
          else if (ch === "\x1b") this.state = "osc-esc";
          else {
            this.osc += ch;
            if (this.osc.length > 512) this.state = "ground";
          }
          break;

        case "osc-esc":
          // ESC \ is the other terminator (ST).
          this.oscEnd();
          this.state = "ground";
          break;
      }
    }
  }

  oscEnd() {
    const m = /^(\d+);([\s\S]*)$/.exec(this.osc);
    // 0 sets icon+title, 2 sets title. Everything else (colours, clipboard) is
    // out of scope and dropping it is better than acting on it half-way.
    if (m && (m[1] === "0" || m[1] === "2")) {
      const t = m[2].trim();
      // ConPTY seeds the title with the command line, so an untouched
      // PowerShell tab would be labelled "C:\Windows\System32\WindowsPowerShe…"
      // — the one string that says nothing about which terminal this is.
      if (t && !/\.(exe|com|bat|cmd)$/i.test(t)) this.title = t;
    }
    this.osc = "";
  }

  nums(def = 0) {
    const raw = this.params.replace(/^[?<>!]/, "").split(";");
    return raw.map((p) => (p === "" ? def : parseInt(p, 10) || 0));
  }

  saveCursor() {
    this.saved = { x: this.x, y: this.y, attr: { ...this.attr } };
  }

  restoreCursor() {
    if (!this.saved) return;
    this.x = Math.min(this.saved.x, this.cols - 1);
    this.y = Math.min(this.saved.y, this.rows - 1);
    this.attr = { ...this.saved.attr };
  }

  reset() {
    this.buf = [];
    for (let y = 0; y < this.rows; y++) this.buf.push(this.blankRow());
    this.x = 0;
    this.y = 0;
    this.attr = { ...BLANK };
    this.top = 0;
    this.bottom = this.rows - 1;
  }

  csi(final) {
    const priv = this.params.startsWith("?");
    const p = this.nums();
    const n = Math.max(1, p[0] || 1);
    switch (final) {
      case "A": this.y = Math.max(this.top, this.y - n); this.wrapNext = false; break;
      case "B": this.y = Math.min(this.bottom, this.y + n); this.wrapNext = false; break;
      case "C": this.x = Math.min(this.cols - 1, this.x + n); this.wrapNext = false; break;
      case "D": this.x = Math.max(0, this.x - n); this.wrapNext = false; break;
      case "E": this.y = Math.min(this.bottom, this.y + n); this.x = 0; break;
      case "F": this.y = Math.max(this.top, this.y - n); this.x = 0; break;
      case "G": case "`": this.x = Math.min(this.cols - 1, n - 1); this.wrapNext = false; break;
      case "d": this.y = Math.min(this.rows - 1, n - 1); break;
      case "H": case "f":
        this.y = Math.min(this.rows - 1, Math.max(1, p[0] || 1) - 1);
        this.x = Math.min(this.cols - 1, Math.max(1, p[1] || 1) - 1);
        this.wrapNext = false;
        break;
      case "J": {
        const mode = p[0] || 0;
        if (mode === 0) {
          this.eraseInRow(this.y, this.x, this.cols - 1);
          for (let y = this.y + 1; y < this.rows; y++) this.eraseInRow(y, 0, this.cols - 1);
        } else if (mode === 1) {
          for (let y = 0; y < this.y; y++) this.eraseInRow(y, 0, this.cols - 1);
          this.eraseInRow(this.y, 0, this.x);
        } else {
          // `clear` sends 2J then homes the cursor. Pushing the old screen into
          // scrollback (rather than dropping it) is what makes the scrollbar
          // still hold what you just cleared.
          for (let y = 0; y < this.rows; y++) {
            if (!this.alt && this.buf[y].some((c) => c.ch !== " ")) this.commit(this.buf[y]);
            this.buf[y] = this.blankRow();
          }
        }
        break;
      }
      case "K": {
        const mode = p[0] || 0;
        if (mode === 0) this.eraseInRow(this.y, this.x, this.cols - 1);
        else if (mode === 1) this.eraseInRow(this.y, 0, this.x);
        else this.eraseInRow(this.y, 0, this.cols - 1);
        break;
      }
      case "L":
        for (let i = 0; i < n; i++) { this.buf.splice(this.bottom, 1); this.buf.splice(this.y, 0, this.blankRow()); }
        break;
      case "M":
        for (let i = 0; i < n; i++) { this.buf.splice(this.y, 1); this.buf.splice(this.bottom, 0, this.blankRow()); }
        break;
      case "P": {
        const row = this.buf[this.y];
        row.splice(this.x, n);
        while (row.length < this.cols) row.push({ ...BLANK });
        break;
      }
      case "@": {
        const row = this.buf[this.y];
        for (let i = 0; i < n; i++) row.splice(this.x, 0, { ...BLANK });
        row.length = this.cols;
        break;
      }
      case "X": this.eraseInRow(this.y, this.x, this.x + n - 1); break;
      case "S": this.scrollUp(n); break;
      case "T": this.scrollDown(n); break;
      case "r":
        this.top = Math.min(this.rows - 1, Math.max(1, p[0] || 1) - 1);
        this.bottom = Math.min(this.rows - 1, (p[1] || this.rows) - 1);
        if (this.bottom <= this.top) { this.top = 0; this.bottom = this.rows - 1; }
        this.x = 0;
        this.y = this.top;
        break;
      case "s": this.saveCursor(); break;
      case "u": this.restoreCursor(); break;
      case "h": case "l": this.mode(priv, p, final === "h"); break;
      case "m": this.sgr(p); break;
      case "n": this.report(priv, p[0] || 0); break;
      case "c": this.identify(); break;
      default: break;
    }
  }

  reply(s) {
    if (this.onReply) this.onReply(s);
  }

  /** Device Status Report. 6 asks for the cursor position; 5 asks whether the
   *  terminal is healthy. PSReadLine and every readline-alike wait on the
   *  former before drawing a prompt. */
  report(priv, which) {
    const row = this.y + 1;
    const col = Math.min(this.x, this.cols - 1) + 1;
    if (which === 6) this.reply(priv ? `\x1b[?${row};${col}R` : `\x1b[${row};${col}R`);
    else if (which === 5) this.reply("\x1b[0n");
  }

  /** Device Attributes — "what kind of terminal are you?". Claiming a VT100
   *  with advanced video is the safe, widely-understood answer. */
  identify() {
    if (this.params.startsWith(">")) this.reply("\x1b[>0;276;0c");
    else this.reply("\x1b[?1;2c");
  }

  mode(priv, p, on) {
    if (!priv) return;
    for (const code of p) {
      if (code === 25) this.cursorVisible = on;
      else if (code === 7) this.autowrap = on;
      else if (code === 1) this.appCursor = on;
      else if (code === 2004) this.bracketedPaste = on;
      else if (code === 1049 || code === 47 || code === 1047) {
        // The alternate screen: a full-screen program borrows the grid and
        // gives it back untouched. Without it, quitting `less` leaves its
        // rendering permanently pasted over your shell history.
        if (on && !this.alt) {
          this.alt = { buf: this.buf, x: this.x, y: this.y, attr: { ...this.attr } };
          this.buf = [];
          for (let y = 0; y < this.rows; y++) this.buf.push(this.blankRow());
          this.x = 0;
          this.y = 0;
        } else if (!on && this.alt) {
          this.buf = this.alt.buf;
          // The saved screen was sized for the old geometry; a resize while a
          // pager was open would otherwise restore ragged rows.
          while (this.buf.length < this.rows) this.buf.push(this.blankRow());
          this.buf.length = this.rows;
          for (const row of this.buf) {
            while (row.length < this.cols) row.push({ ...BLANK });
            row.length = this.cols;
          }
          this.x = Math.min(this.alt.x, this.cols - 1);
          this.y = Math.min(this.alt.y, this.rows - 1);
          this.attr = this.alt.attr;
          this.alt = null;
        }
      }
    }
  }

  sgr(p) {
    if (!p.length) p = [0];
    for (let i = 0; i < p.length; i++) {
      const c = p[i];
      if (c === 0) this.attr = { ...BLANK };
      else if (c === 1) this.attr.bold = true;
      else if (c === 2) this.attr.dim = true;
      else if (c === 3) this.attr.italic = true;
      else if (c === 4) this.attr.ul = true;
      else if (c === 7) this.attr.inv = true;
      else if (c === 22) { this.attr.bold = false; this.attr.dim = false; }
      else if (c === 23) this.attr.italic = false;
      else if (c === 24) this.attr.ul = false;
      else if (c === 27) this.attr.inv = false;
      else if (c >= 30 && c <= 37) this.attr.fg = c - 30;
      else if (c >= 90 && c <= 97) this.attr.fg = c - 90 + 8;
      else if (c >= 40 && c <= 47) this.attr.bg = c - 40;
      else if (c >= 100 && c <= 107) this.attr.bg = c - 100 + 8;
      else if (c === 39) this.attr.fg = null;
      else if (c === 49) this.attr.bg = null;
      else if (c === 38 || c === 48) {
        const key = c === 38 ? "fg" : "bg";
        if (p[i + 1] === 5) { this.attr[key] = p[i + 2]; i += 2; }
        else if (p[i + 1] === 2) { this.attr[key] = `rgb(${p[i + 2] | 0},${p[i + 3] | 0},${p[i + 4] | 0})`; i += 4; }
      }
    }
  }
}

// ---------- rendering ----------

function colourOf(v, palette) {
  if (v === null || v === undefined) return null;
  if (typeof v === "string") return v;
  return xterm256(v, palette);
}

/** One row of cells as a single element, coalescing runs of equal style.
 *
 *  One node per row on purpose: scrollback is trimmed from the front and a
 *  resize hands rows back from the front of the buffer, and both are one
 *  `remove()` when a row is one element rather than a loose run of spans. */
function renderRow(row, palette, cursorX) {
  const line = document.createElement("span");
  line.className = "term-row";
  // Trailing blanks carry no information and make every line the full width,
  // which breaks text selection and copy.
  let end = row.length - 1;
  while (end >= 0 && row[end].ch === " " && !row[end].bg && !row[end].inv) end--;
  const limit = cursorX >= 0 ? Math.max(end, cursorX) : end;

  let i = 0;
  while (i <= limit) {
    const style = row[i];
    let text = "";
    const start = i;
    while (i <= limit && sameStyle(row[i], style) && (cursorX < 0 || (i === cursorX) === (start === cursorX))) {
      text += row[i].ch;
      i++;
    }
    const span = document.createElement("span");
    span.textContent = text;
    const cls = [];
    if (style.bold) cls.push("term-bold");
    if (style.dim) cls.push("term-dim");
    if (style.ul) cls.push("term-underline");
    if (style.inv) cls.push("term-inverse");
    if (start === cursorX) cls.push("cursor");
    if (cls.length) span.className = cls.join(" ");
    if (style.italic) span.style.fontStyle = "italic";
    const fg = colourOf(style.fg, palette);
    const bg = colourOf(style.bg, palette);
    // Inverse is done with a class so it still reads correctly when no explicit
    // colours are set; explicit colours win over it when both are present.
    if (fg && !style.inv) span.style.color = fg;
    if (bg && !style.inv) span.style.background = bg;
    if (style.inv && fg) span.style.background = fg;
    if (style.inv && bg) span.style.color = bg;
    line.appendChild(span);
  }
  line.appendChild(document.createTextNode("\n"));
  return line;
}

// ---------- key handling ----------

function keyToBytes(e, screen) {
  const ctrl = e.ctrlKey;
  const alt = e.altKey;
  const k = e.key;
  // In application cursor mode a shell's line editor expects SS3, not CSI.
  const cur = (letter) => (screen.appCursor ? "\x1bO" + letter : "\x1b[" + letter);

  if (k === "Enter") return "\r";
  if (k === "Backspace") return ctrl ? "\x08" : "\x7f";
  if (k === "Tab") return e.shiftKey ? "\x1b[Z" : "\t";
  if (k === "Escape") return "\x1b";
  if (k === "ArrowUp") return cur("A");
  if (k === "ArrowDown") return cur("B");
  if (k === "ArrowRight") return cur("C");
  if (k === "ArrowLeft") return cur("D");
  if (k === "Home") return cur("H");
  if (k === "End") return cur("F");
  if (k === "PageUp") return "\x1b[5~";
  if (k === "PageDown") return "\x1b[6~";
  if (k === "Insert") return "\x1b[2~";
  if (k === "Delete") return "\x1b[3~";
  if (/^F\d+$/.test(k)) {
    const map = { F1: "\x1bOP", F2: "\x1bOQ", F3: "\x1bOR", F4: "\x1bOS", F5: "\x1b[15~", F6: "\x1b[17~", F7: "\x1b[18~", F8: "\x1b[19~", F9: "\x1b[20~", F10: "\x1b[21~", F11: "\x1b[23~", F12: "\x1b[24~" };
    return map[k] || "";
  }
  if (ctrl && k.length === 1) {
    const c = k.toLowerCase();
    if (c >= "a" && c <= "z") return String.fromCharCode(c.charCodeAt(0) - 96);
    if (c === " ") return "\x00";
    if (c === "[") return "\x1b";
    if (c === "\\") return "\x1c";
    if (c === "]") return "\x1d";
    return "";
  }
  if (alt && k.length === 1) return "\x1b" + k;
  if (k.length === 1) return k;
  return "";
}

// ---------- the view ----------

export default function (e) {
  e.registerView({    id: "terminal",
    title: "Terminal",
    icon: "▪",
    mount(root, ctx) {
      const wrap = document.createElement("div");
      wrap.className = "term-wrap";
      wrap.tabIndex = 0;
      const screenEl = document.createElement("pre");
      screenEl.className = "term-screen";
      // Two nodes: frozen history that is only ever appended to, and the live
      // grid that is rebuilt each frame. Re-rendering 5000 scrollback rows on
      // every keystroke is the difference between usable and unusable.
      const historyEl = document.createElement("span");
      historyEl.className = "term-history";
      const liveEl = document.createElement("span");
      liveEl.className = "term-live";
      screenEl.append(historyEl, liveEl);
      wrap.appendChild(screenEl);

      const note = document.createElement("div");
      note.className = "term-note";
      note.textContent = "starting…";

      root.append(wrap, note);

      let palette = document.documentElement.dataset.theme === "light" ? PALETTE_LIGHT : PALETTE_DARK;
      let disposed = false;
      let dead = false;
      let frame = 0;

      // Measure the font once, from a real span in the real container: hard
      // coding a character width guesses wrong on every machine, and a wrong
      // column count makes a shell wrap its own prompt.
      function metrics() {
        const probe = document.createElement("span");
        probe.className = "term-screen";
        probe.style.position = "absolute";
        probe.style.visibility = "hidden";
        probe.style.whiteSpace = "pre";
        probe.textContent = "M".repeat(50);
        wrap.appendChild(probe);
        const rect = probe.getBoundingClientRect();
        const cw = rect.width / 50;
        const lh = rect.height;
        probe.remove();
        const style = getComputedStyle(wrap);
        const padX = parseFloat(style.paddingLeft) + parseFloat(style.paddingRight);
        const padY = parseFloat(style.paddingTop) + parseFloat(style.paddingBottom);
        const cols = Math.max(20, Math.floor((wrap.clientWidth - padX) / (cw || 7)));
        const rows = Math.max(5, Math.floor((wrap.clientHeight - padY) / (lh || 16)));
        return { cols, rows };
      }

      const first = metrics();
      const screen = new Screen(
        first.cols,
        first.rows,
        (row) => {
          historyEl.appendChild(renderRow(row, palette, -1));
          // Trim the DOM alongside the buffer, or the node count grows forever.
          while (historyEl.childElementCount > SCROLLBACK) historyEl.removeChild(historyEl.firstElementChild);
        },
        (answer) => {
          // Replies go back on the same channel as typing; the shell cannot
          // tell the difference and must not be able to.
          e.pty.write(ctx.sid, ctx.tab, answer).catch(() => undefined);
        },
        () => {
          // A grow handed this row back to the live screen; drop the frozen
          // copy so it is not drawn in both places.
          historyEl.lastElementChild?.remove();
        },
      );

      function draw() {
        frame = 0;
        if (disposed) return;
        const atBottom = wrap.scrollHeight - wrap.scrollTop - wrap.clientHeight < 24;
        const frag = document.createDocumentFragment();
        const showCursor = screen.cursorVisible && !dead && document.activeElement === wrap;
        for (let y = 0; y < screen.rows; y++) {
          frag.appendChild(renderRow(screen.buf[y], palette, showCursor && y === screen.y ? Math.min(screen.x, screen.cols - 1) : -1));
        }
        liveEl.replaceChildren(frag);
        if (screen.title) ctx.setTitle(screen.title);
        // Follow the output only if the user was already at the bottom —
        // yanking the view back mid-scroll makes reading a long output
        // impossible.
        if (atBottom) wrap.scrollTop = wrap.scrollHeight;
      }

      function schedule() {
        if (frame || disposed) return;
        // Coalesce to one repaint per frame: a build tool can emit hundreds of
        // writes a second and each one repainting is pure dropped frames.
        frame = requestAnimationFrame(draw);
      }

      const offData = e.pty.onData(ctx.tab, (data) => {
        screen.write(data);
        schedule();
      });
      const offExit = e.pty.onExit(ctx.tab, (code) => {
        dead = true;
        ctx.setDone(true);
        note.textContent = code === 0 ? "shell exited" : `shell exited (${code})`;
        note.classList.toggle("err", code !== 0);
        schedule();
      });

      // Re-theme in place. The palette is the only thing that changes, so a
      // full redraw is enough — and cheaper than rebuilding scrollback rows
      // one at a time.
      const themeWatch = new MutationObserver(() => {
        const next = document.documentElement.dataset.theme === "light" ? PALETTE_LIGHT : PALETTE_DARK;
        if (next === palette) return;
        palette = next;
        const frag = document.createDocumentFragment();
        for (const row of screen.scrollback) frag.appendChild(renderRow(row, palette, -1));
        historyEl.replaceChildren(frag);
        schedule();
      });
      themeWatch.observe(document.documentElement, { attributes: true, attributeFilter: ["data-theme"] });

      function fit() {
        if (disposed || dead) return;
        // A hidden tab measures 0×0; resizing to that would tell the shell it
        // has no screen and permanently mangle its prompt.
        if (!wrap.clientWidth || !wrap.clientHeight) return;
        const m = metrics();
        if (!screen.resize(m.cols, m.rows)) return;
        e.pty.resize(ctx.sid, ctx.tab, m.cols, m.rows).catch(() => undefined);
        schedule();
      }

      ctx.onShow(() => {
        fit();
        // Focus follows the tab: clicking a terminal tab and then having to
        // click the terminal is one click too many.
        if (ctx.isActive()) wrap.focus({ preventScroll: true });
      });

      wrap.addEventListener("focus", schedule);
      wrap.addEventListener("blur", schedule);

      wrap.addEventListener("keydown", (ev) => {
        if (dead) return;
        // Let copy work: with a selection, Ctrl/Cmd+C means copy, not SIGINT.
        const sel = String(window.getSelection() || "");
        if ((ev.ctrlKey || ev.metaKey) && ev.key.toLowerCase() === "c" && sel) return;
        if ((ev.ctrlKey || ev.metaKey) && ev.key.toLowerCase() === "v") return; // paste handler below
        // The pane toggle stays reachable from inside a terminal.
        if ((ev.ctrlKey || ev.metaKey) && ev.key.toLowerCase() === "b") return;
        const bytes = keyToBytes(ev, screen);
        if (!bytes) return;
        ev.preventDefault();
        ev.stopPropagation();
        e.pty.write(ctx.sid, ctx.tab, bytes).catch(() => undefined);
      });

      wrap.addEventListener("paste", (ev) => {
        if (dead) return;
        ev.preventDefault();
        const text = (ev.clipboardData && ev.clipboardData.getData("text")) || "";
        if (!text) return;
        // Bracketed paste tells the shell "this is pasted data", which is what
        // stops a pasted newline from running a half-typed command.
        const payload = screen.bracketedPaste ? `\x1b[200~${text}\x1b[201~` : text;
        e.pty.write(ctx.sid, ctx.tab, payload).catch(() => undefined);
      });

      // Clicking anywhere in the pane types there, except when the user is
      // selecting text to copy.
      wrap.addEventListener("mouseup", () => {
        if (!String(window.getSelection() || "")) wrap.focus({ preventScroll: true });
      });

      const ro = new ResizeObserver(() => fit());
      ro.observe(wrap);

      void (async () => {
        try {
          await e.pty.spawn(ctx.sid, ctx.tab, screen.cols, screen.rows);
          note.textContent = "";
          note.hidden = true;
          wrap.focus({ preventScroll: true });
        } catch (err) {
          dead = true;
          const msg = err instanceof Error ? err.message : String(err);
          note.textContent = msg;
          note.classList.add("err");
          ctx.setDone(true);
          e.ui.notify(msg, "error");
        }
      })();

      return () => {
        disposed = true;
        if (frame) cancelAnimationFrame(frame);
        ro.disconnect();
        themeWatch.disconnect();
        offData();
        offExit();
        // Kill last: the subscriptions are already gone, so the exit event this
        // provokes has nothing left to deliver to.
        e.pty.kill(ctx.sid, ctx.tab).catch(() => undefined);
      };
    },
  });
}

// The host only needs the default export. `Screen` and `renderRow` are named
// as well so the emulator — the part of this file with real edge cases — can be
// exercised directly rather than only through a live pty.
export { Screen, renderRow };
