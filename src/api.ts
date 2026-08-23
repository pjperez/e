// e — bridge to the Rust backend via Tauri.

export type ProviderItem = { id: string; name: string; base_url: string; api_key: string; models: string[]; context_window?: number | null };

export type Config = {
  base_url: string;
  api_key: string;
  model: string;
  temperature: number;
  system: string;
  workspace: string;
  /** Auto-approve risky tools (shell, write_file) instead of prompting. */
  yolo: boolean;
  models: string[];
  /** Usable context window in tokens for the active model. */
  context_window: number;
  providers: ProviderItem[];
};


export type EngineEvents =
  | { type: "token"; text: string; sid: string }
  | { type: "tool_call"; sid: string; id: string; name: string; arguments: string }
  | { type: "tool_result"; sid: string; id: string; name: string; success: boolean; output: string }
  | { type: "message_end"; sid: string }
  | { type: "reasoning"; text: string; sid: string }
  | { type: "plugin_tool_call"; sid: string; name: string; arguments: string }
  | { type: "done"; stopped: boolean; sid: string }
  | { type: "activity"; sid: string; phase: string; tool: string | null; step: number }
  | { type: "summary"; sid: string; steps: number; tools: number; stopped: boolean; error: string | null; tokensIn: number; tokensOut: number; contextTokens: number; cost: number | null }
  | { type: "error"; message: string; sid: string }
  | { type: "approval_request"; id: string; sid: string; tool: string; preview: string }
  | { type: "approval_close"; id: string; sid: string }

type Unlisten = () => void;

let inTauri = false;
try {
  inTauri = !!(window as unknown as { __TAURI_INTERNALS__?: unknown }).__TAURI_INTERNALS__;
} catch {
  inTauri = false;
}
export const isTauri = inTauri;

async function invoke<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  const mod = await import("@tauri-apps/api/core");
  return mod.invoke<T>(cmd, args);
}

export async function getConfig(): Promise<Config> {
  if (!inTauri) return { base_url: "(browser preview)", api_key: "", model: "—", temperature: 1, system: "", workspace: ".", yolo: false, models: [], context_window: 1_000_000, providers: [] };
  return invoke<Config>("get_config");
}

export async function saveConfig(cfg: Config): Promise<void> {
  if (!inTauri) return;
  await invoke("save_config", { config: cfg });
}


export async function refreshModels(base_url: string, api_key: string): Promise<string[]> {
  if (!inTauri) return [];
  return invoke<string[]>("refresh_models", { baseUrl: base_url, apiKey: api_key });
}

export async function readAttachment(path: string): Promise<{ path: string; content: string }> {
  if (!inTauri) return { path, content: "" };
  return invoke("read_attachment", { path });
}

export type ProjectMeta = { id: string; name: string; workspace: string; created: number };

export type PluginToolDef = { name: string; description: string; parameters?: unknown };

export async function listPlugins(): Promise<{ name: string; version?: string; description?: string; capabilities?: string[]; entry?: string }[]> {
  if (!inTauri) return [];
  return invoke("list_plugins");
}

export async function getPlugin(name: string): Promise<{ manifest: unknown; source: string }> {
  if (!inTauri) return { manifest: {}, source: "" };
  return invoke("get_plugin", { name });
}

export async function setPluginTools(tools: PluginToolDef[]): Promise<void> {
  if (!inTauri) return;
  await invoke("set_plugin_tools", { tools });
}

export async function approvalResolve(id: string, approved: boolean): Promise<void> {
  if (!inTauri) return;
  await invoke("approval_resolve", { id, approved });
}

export async function pluginToolResult(sid: string, ok: boolean, output: string): Promise<void> {
  if (!inTauri) return;
  await invoke("plugin_tool_result", { sid, ok, output });
}

export async function listProjects(): Promise<{ projects: ProjectMeta[]; current: string }> {
  if (!inTauri) return { projects: [], current: "" };
  return invoke("list_projects");
}

export async function newProject(name?: string, workspace?: string): Promise<ProjectMeta> {
  if (!inTauri) return { id: "", name: name || "Project", workspace: workspace || "", created: 0 };
  return invoke("new_project", { name, workspace });
}

export async function switchProject(id: string): Promise<boolean> {
  if (!inTauri) return false;
  return invoke("switch_project", { id });
}

export async function projectRemove(id: string): Promise<boolean> {
  if (!inTauri) return false;
  return invoke("project_remove", { id });
}

export async function renameProject(id: string, name: string): Promise<boolean> {
  if (!inTauri) return false;
  return invoke("rename_project", { id, name });
}

export async function setProjectWorkspace(id: string, workspace: string): Promise<boolean> {
  if (!inTauri) return false;
  return invoke("set_project_workspace", { id, workspace });
}

export async function pathIsDir(path: string): Promise<boolean> {
  if (!inTauri) return true;
  return invoke<boolean>("path_is_dir", { path }).catch(() => false);
}

export type SessionMetaItem = { id: string; name: string; created: number };

export async function listSessions(): Promise<{ sessions: SessionMetaItem[]; current: string; running: string[] }> {
  if (!inTauri) return { sessions: [], current: "", running: [] };
  const r = await invoke<{ sessions: SessionMetaItem[]; current: string; running?: string[] }>("list_sessions");
  return { ...r, running: r.running || [] };
}

export async function runningSessions(): Promise<string[]> {
  if (!inTauri) return [];
  return invoke<string[]>("running_sessions");
}

export async function newSession(name?: string, workspace?: string, model?: string): Promise<SessionMetaItem> {
  if (!inTauri) return { id: "", name: name || "Chat", created: 0 };
  return invoke("new_session", { name, workspace, model });
}

export async function deleteSession(id: string): Promise<void> {
  if (!inTauri) return;
  await invoke("delete_session", { id });
}

export async function forkSession(id: string, name?: string): Promise<SessionMetaItem> {
  if (!inTauri) return { id: "", name: name || "Fork", created: 0 };
  return invoke("fork_session", { id, name });
}

export async function renameSession(id: string, name: string): Promise<boolean> {
  if (!inTauri) return false;
  return invoke("rename_session", { id, name });
}

export type CompactResult = { messages: number; compacted: boolean; dropped?: number };

export async function compactSession(id: string): Promise<CompactResult> {
  if (!inTauri) return { messages: 0, compacted: false };
  return invoke("compact_session", { id });
}

/// Usable context window (tokens) for the active provider/model.
export async function contextBudget(): Promise<number> {
  if (!inTauri) return 1_000_000;
  return invoke<number>("context_budget");
}

export async function switchSession(id: string): Promise<boolean> {
  if (!inTauri) return false;
  return invoke("switch_session", { id });
}

export async function getSession(id: string): Promise<{ messages: { role: string; content: string; reasoning?: string }[]; model: string; running: boolean; context_estimate: number }> {
  if (!inTauri) return { messages: [], model: "", running: false, context_estimate: 0 };
  return invoke("get_session", { id });
}

export async function setSessionModel(id: string, model: string): Promise<void> {
  if (!inTauri) return;
  await invoke("set_session_model", { id, model });
}

export async function getStatus
(): Promise<{ model: string; tools: number; ready: boolean }> {
  if (!inTauri) return { model: "(preview)", tools: 0, ready: false };
  return invoke("get_status");
}

/// Send a message to a specific chat. `sid` is always explicit so a message
/// typed in one chat can never be delivered to another one.
export async function sendText(sid: string, text: string, images: string[] = []): Promise<string> {
  if (!inTauri) throw new Error("not-running-in-tauri");
  return invoke<string>("send_text", { text, images, sid });
}

export async function cancelRun(sid?: string): Promise<boolean> {
  if (!inTauri) return false;
  return invoke<boolean>("cancel_run", { sid: sid || null }).catch(() => false);
}

export async function clearSession(sid?: string): Promise<void> {
  if (!inTauri) return;
  await invoke("clear_session", { sid: sid || null });
}

export function onEngineEvent(cb: (ev: EngineEvents) => void): Unlisten {
  if (!inTauri) return () => undefined;
  const un: Unlisten[] = [];
  void (async () => {
    const evt = await import("@tauri-apps/api/event");
    un.push(await evt.listen<{ sid: string; text: string }>("e:token", (e) => cb({ type: "token", ...e.payload })));
    un.push(await evt.listen<Record<string, string>>("e:tool_call", (e) => cb({ type: "tool_call", ...e.payload } as never)));
    un.push(await evt.listen<Record<string, unknown>>("e:tool_result", (e) => cb({ type: "tool_result", ...e.payload } as never)));
    un.push(await evt.listen<{ sid: string }>("e:message_end", (e) => cb({ type: "message_end", sid: e.payload.sid })));
    un.push(await evt.listen<{ sid: string; text: string }>("e:reasoning", (e) => cb({ type: "reasoning", ...e.payload })));
    un.push(await evt.listen<{ sid: string; name: string; arguments: string }>("e:plugin_tool_call", (e) => cb({ type: "plugin_tool_call", ...e.payload })));
    un.push(await evt.listen<Record<string, unknown>>("e:summary", (e) => cb({ type: "summary", ...e.payload } as never)));;
    un.push(await evt.listen<Record<string, unknown>>("e:activity", (e) => cb({ type: "activity", ...e.payload } as never)));
    un.push(await evt.listen<Record<string, unknown>>("e:done", (e) => cb({ type: "done", stopped: !!(e.payload as {stopped?:boolean}).stopped, sid: String((e.payload as {sid?:string}).sid || "") })));
    un.push(await evt.listen<{ id: string; sid: string; tool: string; preview: string }>("e:approval_request", (e) => cb({ type: "approval_request", ...e.payload })));
    un.push(await evt.listen<{ id: string; sid: string }>("e:approval_close", (e) => cb({ type: "approval_close", ...e.payload })));
    un.push(await evt.listen<{ sid: string; message: string }>("e:error", (e) => cb({ type: "error", ...e.payload })));
  })();
  return () => {
    un.forEach((f) => f());
  };
}

export async function workspaceSnapshot(id: string): Promise<boolean> {
  if (!inTauri) return false;
  return invoke("workspace_snapshot", { id });
}

export async function workspaceRevert(id: string): Promise<boolean> {
  if (!inTauri) return false;
  return invoke("workspace_revert", { id });
}

export async function searchSessions(query: string): Promise<{ results: { session_id: string; session_name: string; snippet: string; role?: string; project?: string }[] }> {
  if (!inTauri) return { results: [] };
  return invoke("search_sessions", { query });
}
