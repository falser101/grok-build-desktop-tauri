/**
 * Renderer-side adapter for Tauri 2 commands.
 *
 * Mirrors the `DesktopApi` shape that the original Electron preload
 * exposed via `window.desktop.*`. Every method delegates to a Tauri
 * `invoke()` call (real backend) or to a typed stub (not implemented
 * in v1). A Proxy fills in any unmapped method so that, instead of
 * silently doing nothing, the renderer logs a warning and throws.
 *
 * See `docs/MIGRATION_STATUS.md` for the per-method status (✅ / 🟡).
 */
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import { openUrl } from "@tauri-apps/plugin-opener";

import type {
  AccountStatus,
  AccountUiEvent,
  AddMcpServerInput,
  AgentUiEvent,
  AppSnapshot,
  AskUserQuestionResponse,
  DesktopApi,
  DesktopPlatform,
  ExternalEditorDescriptor,
  ExtensionsConfigPaths,
  FetchModelsInput,
  FetchedModelInfo,
  FileEntry,
  FileReadResult,
  FolderTrustOutcome,
  ForkSessionResult,
  HookEntry,
  InstallerChannel,
  InstallerResult,
  InstallerStatus,
  McpServerEntry,
  McpServerScope,
  ModelConfigKeyIndex,
  ModelProviderConfig,
  ModelProviderPreset,
  PathSuggestion,
  PlanApprovalOutcome,
  PluginEntry,
  PromptAttachment,
  PromptPayload,
  ProviderUsageResult,
  RewindExecuteResult,
  RewindMode,
  RewindPointUi,
  SearchSessionsOptions,
  SessionModeId,
  SessionSearchHit,
  SkillCatalogEntry,
  SkillEntry,
  TermHostEvent,
  TermStartResult,
  TrustedFolderEntry,
  UpsertProviderInput,
  UsageInfo,
} from "@shared/types";

type AnyArgs = Record<string, unknown> | undefined;

/**
 * Helper that converts a thrown Error / string into a Promise rejection,
 * so a stubbed command behaves like a real command that failed.
 */
function reject<T>(name: string): Promise<T> {
  const msg = `${name}: not implemented in grok-build-desktop-tauri v1. See docs/MIGRATION_STATUS.md.`;
  // eslint-disable-next-line no-console
  console.warn(`[stub] ${msg}`);
  return Promise.reject(new Error(msg));
}

/**
 * Helper: subscribe to a Tauri event and forward payloads to `cb`.
 * Returns an unsubscribe function (same shape as the Electron preload).
 */
function subscribe<T>(
  event: string,
  cb: (payload: T) => void,
): () => void {
  let unlisten: UnlistenFn | null = null;
  let cancelled = false;
  void listen<T>(event, (e) => {
    if (cancelled) return;
    try {
      cb(e.payload);
    } catch (err) {
      // eslint-disable-next-line no-console
      console.error(`[desktop] listener for ${event} threw`, err);
    }
  }).then((fn) => {
    if (cancelled) fn();
    else unlisten = fn;
  });
  return () => {
    cancelled = true;
    if (unlisten) unlisten();
  };
}

/**
 * The real adapter. Methods are grouped to match the migration table:
 *
 *   Group A — ACP-bridged (live against `grok agent serve`)
 *   Group B — Tauri-native (no agent serve)
 *   Group D — Real, low-cost extras (folder picker, opener)
 *
 * Group C (stubbed) is intentionally absent here; the Proxy catches it.
 */
const real: Partial<DesktopApi> = {
  // ──────── Group A: ACP-bridged session control ────────
  getState: async () => {
    const v = await invoke<any>("agent_get_state");
    // Tauri 2 IPC sometimes wraps Ok values in an envelope.
    // Try common wrappers: { value: X }, { data: X }, { result: X },
    // { __tauri_result: X }. If none match, use v as-is.
    const inner = v?.value ?? v?.data ?? v?.result ?? v?.__tauri_result ?? v;
    // Ensure required fields exist so the renderer doesn't crash.
    if (!inner || typeof inner !== "object") {
      console.error("[desktop] getState returned non-object:", v);
      return defaultSnapshot();
    }
    return inner as AppSnapshot;
  },
  connect: () => invoke<void>("agent_connect"),
  newSession: (workspace) =>
    invoke<void>("agent_new_session", { workspace }),
  prepareNewChat: () => invoke<void>("agent_prepare_new_chat"),
  loadSession: (sessionId, cwd) =>
    invoke<void>("agent_load_session", { sessionId, cwd }),
  refreshHistory: () => invoke<void>("agent_refresh_history"),
  renameSession: (sessionId, title, cwd) =>
    invoke<void>("agent_rename_session", { sessionId, title, cwd }),
  deleteSession: (sessionId, cwd) =>
    invoke<void>("agent_delete_session", { sessionId, cwd }),
  forkSession: (sessionId, cwd) =>
    invoke<ForkSessionResult>("agent_fork_session", { sessionId, cwd }),
  listRewindPoints: () => invoke<RewindPointUi[]>("agent_list_rewind_points"),
  executeRewind: (targetPromptIndex, mode) =>
    invoke<RewindExecuteResult>("agent_execute_rewind", {
      targetPromptIndex,
      mode,
    }),
  searchSessions: (query, options) =>
    invoke<SessionSearchHit[]>("agent_search_sessions", { query, options }),
  stop: () => invoke<void>("agent_stop"),
  sendPrompt: (payload) => invoke<void>("agent_send_prompt", { payload }),
  listPromptHistory: (cwd, filterSessionId) =>
    invoke<string[]>("agent_list_prompt_history", { cwd, filterSessionId }),
  cancel: () => invoke<void>("agent_cancel"),
  cancelSession: (sessionId) =>
    invoke<void>("agent_cancel_session", { sessionId }),
  respondPermission: (requestId, optionId) =>
    invoke<void>("agent_respond_permission", { requestId, optionId }),
  respondAskUserQuestion: (requestId, response) =>
    invoke<void>("agent_respond_ask_user_question", { requestId, response }),
  respondTrustPrompt: (requestId, outcome) =>
    invoke<void>("agent_respond_trust_prompt", { requestId, outcome }),
  respondPlanApproval: (requestId, outcome, feedback) =>
    invoke<void>("agent_respond_plan_approval", {
      requestId,
      outcome,
      feedback,
    }),
  setModel: (modelId, reasoningEffort) =>
    invoke<void>("agent_set_model", { modelId, reasoningEffort }),
  setMode: (modeId) => invoke<void>("agent_set_mode", { modeId }),
  setAlwaysApprove: (enabled) =>
    invoke<void>("agent_set_always_approve", { enabled }),
  setAutoTrustNewSessions: (enabled) =>
    invoke<void>("agent_set_auto_trust_new_sessions", { enabled }),
  refreshPlanContent: () =>
    invoke<string | null>("agent_refresh_plan_content"),

  // ──────── Group A event subscriptions ────────
  onEvent: (cb) => subscribe<AgentUiEvent>("agent:event", cb),
  onAccountEvent: (cb) => subscribe<AccountUiEvent>("account:event", cb),

  // ──────── Group C: real (previously stubbed) ────────
  // File system
  listDir: (relDir) => invoke<FileEntry[]>("fs_list_dir", { relDir }),
  readFile: (relPath) => invoke<FileReadResult>("fs_read_file", { relPath }),
  readSessionImageDataUrl: (absPath) => invoke<string | null>("fs_read_session_image_data_url", { absPath }),
  pathSuggest: (query) => invoke<PathSuggestion[]>("fs_path_suggest", { query }),

  // Attachments
  pickFiles: () => invoke<PromptAttachment[]>("pick_files"),
  attachPaths: (paths) => invoke<PromptAttachment[]>("attach_paths", { paths }),
  getPathForFile: () => "", // no Tauri equivalent

  // Trusted folders
  listTrustedFolders: () => invoke<TrustedFolderEntry[]>("trust_list"),
  revokeTrustedFolder: (path) => invoke<boolean>("trust_revoke", { pathStr: path }),

  // Account
  getAccountStatus: () => invoke<AccountStatus>("account_get_status"),
  login: (method) => invoke<AccountStatus>("account_login", { method }),
  cancelLogin: () => invoke<void>("account_cancel_login"),
  logout: () => invoke<{ message: string; status: AccountStatus }>("account_logout"),
  setApiKey: (key) => invoke<AccountStatus>("account_set_api_key", { key }),
  reconnectAgent: () => invoke<void>("agent_reconnect"),
  refreshUsage: () => invoke<UsageInfo | null>("account_refresh_usage"),

  // Installer
  installAgent: () => invoke<InstallerResult>("agent_install"),
  getInstallerStatus: () => invoke<InstallerStatus>("agent_installer_status"),
  checkForUpdate: () => invoke<{ hasUpdate: boolean; current: string; latest: string }>("agent_check_for_update"),
  upgradeAgent: () => invoke<InstallerResult>("agent_upgrade"),
  getInstallerChannel: () => invoke<InstallerChannel>("agent_get_channel"),
  setInstallerChannel: (channel) => invoke<InstallerChannel>("agent_set_channel", { channel }),

  // Extensions
  listMcpServers: () => invoke("ext_list_mcp"),
  addMcpServer: (input) => invoke("ext_add_mcp", { input }),
  removeMcpServer: (name, scope) => invoke("ext_remove_mcp", { name, scope }),
  setMcpEnabled: (name, enabled, scope) => invoke("ext_set_mcp_enabled", { name, enabled, scope }),
  listSkills: () => invoke("ext_list_skills"),
  setSkillDisabled: (name, disabled) => invoke("ext_set_skill_disabled", { name, disabled }),
  searchSkillCatalog: (query) => invoke<SkillCatalogEntry[]>("ext_search_skill_catalog", { query }),
  installSkill: (input) => invoke<InstallSkillResult>("ext_install_skill", { input }),
  listPlugins: (available) => invoke("ext_list_plugins", { available }),
  installPlugin: (source) => invoke("ext_install_plugin", { source }),
  uninstallPlugin: (name) => invoke("ext_uninstall_plugin", { name }),
  setPluginEnabled: (name, enabled) => invoke("ext_set_plugin_enabled", { name, enabled }),
  listHooks: () => invoke("ext_list_hooks"),
  readHookFile: (path) => invoke<string>("ext_read_hook_file", { path }),
  getExtensionsPaths: () => invoke<ExtensionsConfigPaths>("ext_get_paths"),

  // Models
  listModelPresets: async () => {
    const v = await invoke<any>("models_list_presets");
    // Tauri 2 may wrap arrays in { value: [...] } — normalize.
    if (v && !Array.isArray(v) && Array.isArray(v.value)) return v.value;
    if (v && v.data && Array.isArray(v.data)) return v.data;
    if (v && Array.isArray(v)) return v;
    console.warn("[desktop] listModelPresets returned unexpected shape:", typeof v, v);
    return Array.isArray(v) ? v : [];
  },
  listModelProviders: async () => {
    const v = await invoke<any>("models_list_providers");
    if (v && !Array.isArray(v) && Array.isArray(v.providers)) return v.providers;
    if (v && !Array.isArray(v) && Array.isArray(v.value)) return v.value;
    if (v && v.data && Array.isArray(v.data)) return v.data;
    if (v && Array.isArray(v)) return v;
    console.warn("[desktop] listModelProviders returned unexpected shape:", typeof v, v);
    return Array.isArray(v) ? v : [];
  },
  upsertModelProvider: (input) => invoke<ModelProviderConfig>("models_upsert_provider", { input }),
  deleteModelProvider: (id) => invoke("models_delete_provider", { id }),
  addModelProviderFromPreset: (presetId, overrides) => invoke<ModelProviderConfig>("models_add_from_preset", { presetId, overrides }),
  fetchProviderModels: (input) => invoke<FetchedModelInfo[]>("models_fetch_provider_models", { input }),
  getModelConfigKeyIndex: () => invoke<ModelConfigKeyIndex>("models_get_config_key_index"),
  queryProviderUsage: (providerId) => invoke<ProviderUsageResult>("models_query_provider_usage", { providerId }),
  reloadAgentModels: () => invoke("models_reload_agent"),

  // External editors
  listExternalEditors: () => invoke<ExternalEditorDescriptor[]>("files_list_external_editors"),
  openInEditor: (editorId, filePath) => invoke("files_open_in_editor", { editorId, filePath }),

  // ──────── Group B: window + UI shortcuts ────────
  minimizeWindow: () => invoke<void>("window_minimize"),
  toggleMaximizeWindow: () => invoke<void>("window_toggle_maximize"),
  closeWindow: () => invoke<void>("window_close"),
  isMaximized: () => invoke<boolean>("window_is_maximized"),
  onMaximizeChanged: (cb) => subscribe<boolean>("window:maximize-changed", cb),
  requestReload: () => invoke<void>("ui_request_reload"),
  requestToggleDevTools: () => invoke<void>("ui_request_toggle_devtools"),
  requestAbout: () => invoke<void>("ui_request_about"),
  requestOpenSettings: () => invoke<void>("ui_request_open_settings"),
  requestNewSession: () => invoke<void>("ui_request_new_session"),
  onUiOpenSettings: (cb) => subscribe<void>("ui:openSettings", cb),
  onUiNewSession: (cb) => subscribe<void>("ui:newSession", cb),
  platform: () => invoke<DesktopPlatform>("window_platform"),

  // ──────── Group D: real, low-cost extras ────────
  pickFolder: async () => {
    const result = await openDialog({
      directory: true,
      multiple: false,
      title: "Select workspace",
    });
    if (result == null) return null;
    return Array.isArray(result) ? (result[0] ?? null) : result;
  },
  openExternal: (url) => openUrl(url).then(() => undefined),
};

/**
 * The exported `desktop` object. Anything missing from `real` is
 * intercepted by the Proxy and rejected with a `[stub]` warning,
 * so it surfaces in the console immediately and is recorded in
 * `docs/MIGRATION_STATUS.md`.
 */
const desktopProxy = new Proxy(real, {
  get(target, prop: string | symbol) {
    if (typeof prop === "symbol") return (target as any)[prop];
    if (prop in target) return (target as any)[prop];
    // Fallback stub factory: every method becomes a rejecting function.
    return (..._args: unknown[]) => reject<never>(String(prop));
  },
});
export const desktop: DesktopApi = desktopProxy as DesktopApi;

// Expose `desktop` on `window.desktop` for files that haven't been
// migrated off the legacy global yet. Safe no-op after migration.
if (typeof window !== "undefined") {
  (window as any).desktop = desktop;
}

// Type-checking helpers (compile-time only — not emitted).
type _Unused = AnyArgs;
// Re-export the type so consumers can `import type { DesktopApi }`.
export type { DesktopApi, DesktopPlatform };

// ───────────────────────── default snapshot ─────────────────────────

function defaultSnapshot(): AppSnapshot {
  return {
    connection: "idle",
    timeline: [],
    sessions: [],
    availableModels: [],
    availableCommands: [],
    sessionMode: "default",
    acceptsImages: true,
    activity: "idle",
    alwaysApprove: false,
    autoTrustNewSessions: false,
    installerStatus: { kind: "absent" },
    installerChannel: "stable",
    todos: [],
  } as AppSnapshot;
}