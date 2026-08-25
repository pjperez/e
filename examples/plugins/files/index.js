// Files — a file tree of the chat's project folder, in e's side pane.
//
// The `fs` capability lists one directory at a time. That shape is the whole
// design: a tree that walks the project up front spends a minute and 200 MB on
// `node_modules` before it can draw a single row, so nothing is read until a
// folder is actually expanded.
//
// Paths are relative and the root is resolved from the chat id on the Rust
// side, so this plugin cannot name a folder outside the project even by
// accident.

/** Folders that are almost never what you opened the tree to look at. */
const NOISE = new Set([".git", "node_modules", "target", "dist", "build", ".venv", "__pycache__", ".next", ".cache"]);

function fmtSize(n) {
  if (n < 1024) return n + " B";
  if (n < 1024 * 1024) return (n / 1024).toFixed(n < 10240 ? 1 : 0) + " K";
  if (n < 1024 * 1024 * 1024) return (n / 1024 / 1024).toFixed(1) + " M";
  return (n / 1024 / 1024 / 1024).toFixed(1) + " G";
}

function baseName(p) {
  const parts = String(p || "").split("/").filter(Boolean);
  return parts.length ? parts[parts.length - 1] : "";
}

export default function (e) {
  e.registerView({
    id: "files",
    title: "Files",
    icon: "▤",
    mount(root, ctx) {
      const head = document.createElement("div");
      head.className = "files-head";
      const pathEl = document.createElement("span");
      pathEl.className = "files-path";
      pathEl.textContent = "/";
      const refresh = document.createElement("button");
      refresh.className = "icon-btn";
      refresh.type = "button";
      refresh.title = "Refresh";
      refresh.textContent = "⟳";
      const hiddenBtn = document.createElement("button");
      hiddenBtn.className = "icon-btn";
      hiddenBtn.type = "button";
      hiddenBtn.title = "Show dotfiles and generated folders";
      hiddenBtn.textContent = "◌";
      head.append(pathEl, hiddenBtn, refresh);

      const tree = document.createElement("div");
      tree.className = "files-tree";

      const preview = document.createElement("div");
      preview.className = "files-preview";
      const pHead = document.createElement("div");
      pHead.className = "files-head";
      const pName = document.createElement("span");
      pName.className = "files-path";
      const pClose = document.createElement("button");
      pClose.className = "icon-btn";
      pClose.type = "button";
      pClose.title = "Close preview";
      pClose.textContent = "×";
      pHead.append(pName, pClose);
      const pBody = document.createElement("pre");
      preview.append(pHead, pBody);

      root.append(head, tree, preview);

      let showHidden = false;
      let selected = "";
      let disposed = false;
      /** Expanded folder paths. Kept across refreshes so reloading the tree
       *  does not collapse everything you just opened. */
      const open = new Set([""]);
      /** path -> entries, so collapsing and re-expanding is instant. */
      const cache = new Map();

      function visible(entries) {
        if (showHidden) return entries;
        return entries.filter((x) => !x.name.startsWith(".") && !(x.dir && NOISE.has(x.name)));
      }

      async function load(path) {
        if (cache.has(path)) return cache.get(path);
        const listing = await e.fs.list(ctx.sid, path);
        cache.set(path, listing);
        return listing;
      }

      function row(entry, depth) {
        const el = document.createElement("div");
        el.className = "files-row" + (entry.dir ? " dir" : "") + (entry.path === selected ? " selected" : "");
        el.style.paddingLeft = 8 + depth * 12 + "px";
        el.title = entry.path + (entry.symlink ? " → (symlink)" : "");

        const caret = document.createElement("span");
        caret.className = "files-caret";
        caret.textContent = entry.dir ? (open.has(entry.path) ? "▾" : "▸") : " ";

        const name = document.createElement("span");
        name.className = "files-name";
        name.textContent = entry.name + (entry.symlink ? " ↗" : "");

        el.append(caret, name);
        if (!entry.dir) {
          const size = document.createElement("span");
          size.className = "files-size";
          size.textContent = fmtSize(entry.size);
          el.appendChild(size);
        }

        el.addEventListener("click", () => {
          if (entry.dir) {
            if (open.has(entry.path)) open.delete(entry.path);
            else open.add(entry.path);
            void render();
          } else {
            selected = entry.path;
            void showFile(entry);
          }
        });
        return el;
      }

      async function renderInto(container, path, depth) {
        let listing;
        try {
          listing = await load(path);
        } catch (err) {
          const bad = document.createElement("div");
          bad.className = "files-error";
          bad.textContent = err instanceof Error ? err.message : String(err);
          container.appendChild(bad);
          return;
        }
        const entries = visible(listing.entries);
        if (!entries.length) {
          const none = document.createElement("div");
          none.className = "files-empty";
          none.style.paddingLeft = 8 + depth * 12 + "px";
          none.textContent = listing.entries.length ? "(only hidden entries)" : "(empty)";
          container.appendChild(none);
        }
        for (const entry of entries) {
          container.appendChild(row(entry, depth));
          if (entry.dir && open.has(entry.path)) {
            await renderInto(container, entry.path, depth + 1);
          }
        }
        if (listing.truncated) {
          const more = document.createElement("div");
          more.className = "files-empty";
          more.style.paddingLeft = 8 + depth * 12 + "px";
          more.textContent = "… too many entries to list";
          container.appendChild(more);
        }
      }

      async function render() {
        // Build off-screen: expanding a deep folder otherwise repaints the tree
        // once per level as each listing resolves.
        const frag = document.createElement("div");
        await renderInto(frag, "", 0);
        if (disposed) return;
        tree.replaceChildren(...frag.childNodes);
      }

      async function showFile(entry) {
        pName.textContent = entry.path;
        pBody.textContent = "…";
        preview.classList.add("open");
        void render();
        try {
          const f = await e.fs.read(ctx.sid, entry.path);
          if (disposed) return;
          if (f.binary) {
            pBody.textContent = `${fmtSize(f.size)} — not text`;
            return;
          }
          pBody.textContent = f.text + (f.truncated ? `\n\n… truncated (${fmtSize(f.size)} total)` : "");
        } catch (err) {
          pBody.textContent = err instanceof Error ? err.message : String(err);
        }
      }

      pClose.addEventListener("click", () => {
        preview.classList.remove("open");
        selected = "";
        void render();
      });
      refresh.addEventListener("click", () => {
        cache.clear();
        void render();
      });
      hiddenBtn.addEventListener("click", () => {
        showHidden = !showHidden;
        hiddenBtn.classList.toggle("on", showHidden);
        void render();
      });

      // The tab says which folder it is looking at, so two Files tabs on two
      // chats are told apart without opening either.
      void (async () => {
        try {
          const listing = await e.fs.list(ctx.sid, "");
          cache.set("", listing);
          const s = e.session && e.session();
          const label = (s && s.workspace && baseName(s.workspace.replace(/\\/g, "/"))) || "";
          pathEl.textContent = label ? label + "/" : "/";
          if (label) ctx.setTitle(label);
        } catch {
          // render() reports it in the tree, where there is room to explain.
        }
        await render();
      })();

      return () => {
        disposed = true;
      };
    },
  });
}
