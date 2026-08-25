# Examples

Working starting points for each extension surface. Copy a folder, edit it,
reload (Settings → Extensions → **Reload extensions**, or `/reload`).

| example | copy to | what it shows |
|---------|---------|---------------|
| `plugins/hello/` | `~/.e/plugins/hello/` | a tool, a `/hi` command, an event listener |
| `plugins/git-guard/` | `~/.e/plugins/git-guard/` | blocking a tool call before it runs |
| `plugins/terminal/` | `~/.e/plugins/terminal/` | a side-pane view driving a real shell over `pty` |
| `plugins/files/` | `~/.e/plugins/files/` | a side-pane view browsing the project over `fs` |
| `skills/commit-style/` | `~/.e/skills/commit-style/` | a SKILL.md the model loads on demand |
| `mcp.json` | `~/.e/mcp.json` | MCP servers, env vars, and one parked server |

`terminal` and `files` contribute tabs to the side pane (⧉ in the top bar, or
`Ctrl`/`Cmd` `B`). Between them they use every pane capability: `views` for the
tab itself, `pty` for a live shell, `fs` for read-only browsing. Both are bound
to the chat they were opened in, so a terminal keeps running in that project's
folder while you read another conversation.

The terminal is also the honest measure of what a view can be: `pty` hands it a
byte stream and a size, and the ~700 lines in `plugins/terminal/index.js` are
the VT parser and grid renderer that turn those bytes into a screen. Nothing
about it is privileged — a plugin you write has the same API.

Anything global (`~/.e/…`) can also live in a project (`<project>/.e/…`),
where it applies to that project only and shadows a global one of the same
name. The full reference is [docs/EXTENDING.md](../docs/EXTENDING.md).
