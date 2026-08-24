# Examples

Working starting points for each extension surface. Copy a folder, edit it,
reload (Settings → Extensions → **Reload extensions**, or `/reload`).

| example | copy to | what it shows |
|---------|---------|---------------|
| `plugins/hello/` | `~/.e/plugins/hello/` | a tool, a `/hi` command, an event listener |
| `plugins/git-guard/` | `~/.e/plugins/git-guard/` | blocking a tool call before it runs |
| `skills/commit-style/` | `~/.e/skills/commit-style/` | a SKILL.md the model loads on demand |
| `mcp.json` | `~/.e/mcp.json` | MCP servers, env vars, and one parked server |

Anything global (`~/.e/…`) can also live in a project (`<project>/.e/…`),
where it applies to that project only and shadows a global one of the same
name. The full reference is [docs/EXTENDING.md](../docs/EXTENDING.md).
