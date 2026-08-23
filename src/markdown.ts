// e — minimal, XSS-safe markdown renderer.
// Every piece of user/model/tool text is HTML-escaped before embedding, so
// untrusted output can never inject markup or scripts.

function esc(s: string): string {
  return s
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;")
    .replace(/'/g, "&#39;");
}

// Inline tokens: `code`, **bold**, *italic*, ~~strike~~, [text](url)
function inline(s: string): string {
  const re = /(`[^`]+`)|(\*\*[^*]+\*\*)|(\*[^*\n]+\*)|(~~[^~]+~~)|(\[([^\]]+)\]\(([^)\s]+)\))/g;
  let out = "";
  let last = 0;
  let m: RegExpExecArray | null;
  while ((m = re.exec(s)) !== null) {
    out += esc(s.slice(last, m.index));
    const tok = m[0];
    if (tok.startsWith("`")) out += `<code class="inline">${esc(tok.slice(1, -1))}</code>`;
    else if (tok.startsWith("**")) out += `<strong>${inline(tok.slice(2, -2))}</strong>`;
    else if (tok.startsWith("~~")) out += `<del>${inline(tok.slice(2, -2))}</del>`;
    else if (tok.startsWith("*")) out += `<em>${inline(tok.slice(1, -1))}</em>`;
    else out += `<a href="${esc(m[6])}" target="_blank" rel="noopener">${inline(m[5])}</a>`;
    last = m.index + tok.length;
  }
  return out + esc(s.slice(last));
}

function renderTable(lines: string[], start: number): { html: string; next: number } {
  const headerCols = lines[start].split("|").map((c) => c.trim()).filter(Boolean);
  let idx = start + 1;
  if (idx < lines.length) idx++; // skip separator
  const rows: string[] = [];
  while (idx < lines.length && lines[idx].trim() !== "") {
    const cells = lines[idx].split("|").map((c) => c.trim()).filter(Boolean);
    rows.push(`<tr>${cells.map((c) => `<td>${inline(c)}</td>`).join("")}</tr>`);
    idx++;
  }
  const html =
    `<table><thead><tr>` +
    headerCols.map((c) => `<th>${inline(c)}</th>`).join("") +
    `</tr></thead><tbody>${rows.join("")}</tbody></table>`;
  return { html, next: idx };
}

export function renderMarkdown(src: string): string {
  if (!src) return "";
  const lines = src.split("\n");
  const out: string[] = [];
  const block: string[] = [];
  let i = 0;

  const flush = (): void => {
    if (!block.length) return;
    const joined = block.join("\n");
    const first = block[0].trim();

    const h = /^(#{1,4})\s+(.*)$/.exec(first);
    if (h && block.length === 1) {
      out.push(`<h${h[1].length}>${inline(h[2])}</h${h[1].length}>`);
    } else if (/^---+\s*$/.test(first) && block.length === 1) {
      out.push("<hr/>");
    } else if (/^\s*>/.test(first)) {
      const bq = joined.split("\n").map((l) => l.replace(/^\s*>\s?/, "")).join("\n");
      out.push(`<blockquote>${inline(bq).replace(/\n/g, "<br/>")}</blockquote>`);
    } else if (/^\s*[-*]\s+/.test(first) || /^\s*\d+\.\s+/.test(first)) {
      const ordered = /^\s*\d+\.\s+/.test(first);
      const items = block.map((l) => {
        const t = l.replace(/^\s*([-*]|\d+\.)\s+/, "");
        return `<li>${inline(t)}</li>`;
      });
      out.push(ordered ? `<ol>${items.join("")}</ol>` : `<ul>${items.join("")}</ul>`);
    } else if (
      block.length >= 3 &&
      block[0].includes("|") &&
      /^\s*\|?[\s:|-]+\|?\s*$/.test(block[1].trim())
    ) {
      out.push(renderTable(block, 0).html);
    } else {
      out.push(`<p>${inline(joined).replace(/\n/g, "<br/>")}</p>`);
    }
    block.length = 0;
  };

  while (i < lines.length) {
    const line = lines[i];
    const fence = /^```(\S*)?\s*$/.exec(line.trim());
    if (fence) {
      flush();
      i++;
      const code: string[] = [];
      while (i < lines.length && !/^```\s*$/.test(lines[i].trim())) {
        code.push(lines[i]);
        i++;
      }
      i++;
      const lang = fence[1] || "";
      out.push(
        `<pre><code class="block${lang ? " language-" + esc(lang) : ""}">${esc(code.join("\n"))}</code></pre>`
      );
      continue;
    }
    if (line.trim() === "") {
      flush();
      i++;
      continue;
    }
    block.push(line);
    i++;
  }
  flush();
  return out.join("\n");
}
