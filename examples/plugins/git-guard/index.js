// A guard: refuse a tool call before it runs.
//
// A `tool_call` handler that returns { block: true, reason } stops the call —
// the model is told why and carries on. Everything else it returns (including
// nothing) lets the call through. The engine waits five seconds at most for an
// answer, so keep the check synchronous and cheap.
//
// This needs only the "events" capability: a guard should not be able to
// register tools, reach the network, or read the session.

const FORBIDDEN = [
  /\bgit\s+push\b.*--force(?!-with-lease)/,
  /\bgit\s+reset\s+--hard\b/,
  /\bgit\s+clean\s+-[a-z]*f/,
  /\brm\s+-rf\s+[/~]/,
];

export default function (e) {
  e.on("tool_call", (ev) => {
    if (ev.name !== "shell") return;
    const command = String((ev.args && ev.args.command) || "");
    const hit = FORBIDDEN.find((re) => re.test(command));
    if (hit) {
      return {
        block: true,
        reason: `git-guard refuses "${command}". Ask the user to run it themselves if it is really intended.`,
      };
    }
  });
}
