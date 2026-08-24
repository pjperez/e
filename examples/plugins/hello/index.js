// The smallest useful plugin: one tool, one command, one listener.
//
// Copy this folder to ~/.e/plugins/hello (or <project>/.e/plugins/hello) and
// press "Reload extensions" in Settings → Extensions. Everything it uses is
// declared in plugin.json — asking for something that was not declared is
// refused and shown in that same pane, so keep the two in step.

export default function (e) {
  // A tool. `parameters` is plain JSON Schema and goes straight to the model,
  // so describe the arguments as if you were writing their documentation.
  e.registerTool({
    name: "say_hi",
    description: "Greet someone by name. Use when the user asks for a greeting.",
    parameters: {
      type: "object",
      properties: { name: { type: "string", description: "Who to greet." } },
      required: ["name"],
    },
    // Return a string, or anything JSON-serialisable. Throwing marks the call
    // failed and hands the message back to the model.
    async run(args) {
      return `hi ${args.name || "there"} — from the hello plugin`;
    },
  });

  // A slash command, typed in the composer.
  e.registerCommand("/hi", () => e.ui.notify("hello from a plugin"), "say hello");

  // Engine events — the same ones the UI draws from.
  e.on("tool_result", (ev) => {
    if (!ev.success) e.log("tool failed:", ev.name, ev.output);
  });
}
