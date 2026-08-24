---
name: Commit style
description: How this project writes commit messages. Load before writing a commit.
---

# Commit style

Copy this folder to `~/.e/skills/commit-style` (yours everywhere) or
`<project>/.e/skills/commit-style` (this project only). The agent sees the
`name` and `description` above in its `skills` tool and loads the rest of this
file when it decides the skill applies.

## Workflow

1. Read the staged diff (`git diff --cached`) before writing anything.
2. Write the subject as an imperative sentence that says what the change does
   for the user, not which files moved: "Stop deleted projects leaving stashes
   behind", never "update sessions.rs".
3. Keep the subject under 72 characters and do not end it with a full stop.
4. Add a body only when the change needs a reason. Explain *why*, and what
   would break without it.
5. Never mention the tool that produced the change.

## Checks

- `git diff --cached --stat` matches what the subject claims.
- The subject reads as a sentence completing "This commit will …".
