# Design — grok-build-desktop-tauri

**Status:** Living design for the Tauri 2 port.  
**Sibling of:** `/home/falser/Projects/grok-build` (Rust agent) and `/home/falser/Projects/grok-build-desktop` (Electron predecessor).  
**Shell:** Tauri 2 (system WebView + Rust core).  
**Backend strategy:** Path B — reuse the existing Rust agent via `grok agent serve`, swap only the host shell.  
**Capability checklist:** [`MIGRATION_STATUS.md`](./MIGRATION_STATUS.md)

---

## 1. Goals

Same as the Electron build (full parity is the eventual goal):

- Chat with Grok coding agent in a windowed UI
- Stream assistant text, tool calls, and file diffs
- Approve/deny sensitive tool actions
- Create and resume local sessions
- Share auth and session store with the CLI (`~/.grok`)

### Non-goals for v1 (this scaffold)

- Rewriting the agent runtime
- Embedding the TUI (ratatui) inside the WebView
- Bundling `grok` binary into the installer
- Auto-update, code signing, notarization
- iOS / Android (Tauri mobile is experimental; not in this plan)

---

## 2. Architecture

```text
┌──────────────────────────────────────────────────────────────┐
│ Tauri Main (Rust)                                            │
│  • Resolve grok binary (binary.rs)                            │
│  • Spawn grok agent serve --bind 127.0.0.1:<port>            │
│  • tokio-tungstenite ACP WS client (agent.rs)                 │
│  • Reader task → emit("agent:event", …) on notifications     │
│  • Child watcher → emits "agent-exited" on subprocess exit    │
│  • Tauri commands ≈ Electron IPC (commands.rs, window_cmds.rs)│
│  • Lazy bridge: window renders even if grok is missing        │
└────────────────────┬─────────────────────────────────────────┘
                     │ Tauri invoke / listen
┌────────────────────▼─────────────────────────────────────────┐
│ Renderer (React + Vite)                                      │
│  • Verbatim copy of grok-build-desktop/src/renderer/*         │
│  • src/desktop.ts mirrors DesktopApi via Tauri invoke/listen  │
│  • Proxy fallback rejects unmapped methods at runtime         │
└────────────────────┬─────────────────────────────────────────┘
                     │ ws://127.0.0.1:<port>/ws?server-key=<secret>
                     ▼
              grok agent serve
```

### Why `agent serve` (unchanged from Electron)

- First-class WebSocket; same wire on macOS / Windows / Linux
- Reuses the Electron build's protocol surface — no work to keep them in sync
- Per-process random secret, loopback only

### Why Tauri (instead of "just" re-running Electron)

- **Smaller bundle**: WebKitGTK / WebView2 / WKWebView is reused; no Chromium download
- **Lower idle memory**: no forked renderer process per window
- **Same Rust ecosystem**: `tokio`, `tokio-tungstenite`, `serde`, `rand` are already in the workspace — no Node runtime needed for the host
- **Same renderer source**: the React/Vite/TypeScript side is portable as-is

The trade-off is the missing Electron-only modules (Linux Ozone flags, `webUtils.getPathForFile`, `node-pty`, `process-tray` icon APIs) — all addressed in v2 if/when needed.

---

## 3. Process & lifecycle

### App startup

1. `tauri::Builder` loads `tauri.conf.json`, registers plugins (`dialog`, `opener`), manages `AppState`.
2. The main window opens immediately with the React renderer. Initial `AppSnapshot` is the renderer's hardcoded `initial` constant — sidebar shows, header bar shows, timeline is empty.
3. `App.tsx` mounts → calls `desktop.connect()` → `agent_connect` Tauri command → Rust spawns `grok agent serve` and connects.
4. `desktop.getState()` returns the sessions list; the renderer hydrates its `AppSnapshot`.

### Renderer → agent round-trip

```
renderer (invoke)
  → Tauri IPC
    → commands::agent_* command
      → state.agent.lock().await → bridge
        → bridge.call(method, params)
          → WS send JSON-RPC frame
        → bridge (writer)
      → bridge (reader task)
        → JSON-RPC frame parsed, response delivered via oneshot
      → bridge.call resolves with result
    → command returns serde_json::Value
  → invoke resolves with T (typed by Tauri command signature)
renderer (await)
```

### Agent → renderer push

```
reader task
  → tokio_tungstenite reads Message::Text
  → serde_json::from_str → Value
  → has method, no id → server-pushed notification
  → app.emit("agent:event", { method, params })
renderer listen("agent:event", cb) fires
```

`account:event` is a parallel channel used by `SettingsView.tsx` — same mechanism.

---

## 4. Security model

1. **Bind 127.0.0.1 only.** `TcpListener::bind("127.0.0.1:0")` in `agent.rs::connect`.
2. **Random 12-byte hex secret per process.** `rand::Rng::gen::<[u8;12]>()`; never logged, never written to disk.
3. **Renderer never sees the secret.** Auth lives only on the Rust side of the IPC boundary.
4. **Auth via query param.** `ws://127.0.0.1:<port>/ws?server-key=<secret>` (server-side `validate_auth` falls back to the header / query param path).
5. **Renderer has no native FS access.** Capabilities (`src-tauri/capabilities/default.json`) grant only `dialog:allow-open`, `opener:allow-open-url`, and the window controls. All filesystem operations live in Rust.
6. **Subprocess lifetime.** `kill_on_drop(true)` on the child Command; watcher task emits `agent:event { type: "agent-exited" }` when the child exits so the renderer can surface a connection-error state.

---

## 5. Protocol contract (ACP + xAI)

Wire format is identical to the Electron build: JSON-RPC 2.0 text frames over WebSocket.

Auth: `Authorization: Bearer <secret>` header **or** `?server-key=<secret>` query param. See `crates/codegen/xai-grok-shell/src/agent/server.rs:94-107`.

ACP methods we use today:

| desktop method | ACP wire call |
| --- | --- |
| `getState` | `x.ai/sessions/list` |
| `newSession` | `new_session` |
| `loadSession` | `load_session` |
| `forkSession` | `x.ai/session/fork` |
| `renameSession` | `x.ai/session/rename` |
| `deleteSession` | `x.ai/session/delete` |
| `listRewindPoints` | `x.ai/session/rewind_points` |
| `executeRewind` | `x.ai/session/rewind` |
| `searchSessions` | `x.ai/session/search` |
| `sendPrompt` | `prompt` |
| `listPromptHistory` | `x.ai/prompt_history` |
| `cancel` | `cancel` |
| `cancelSession` | `x.ai/session/cancel` |
| `respondPermission` | `x.ai/permission/respond` |
| `respondAskUserQuestion` | `x.ai/ask_user_question/respond` |
| `respondTrustPrompt` | `x.ai/folder_trust/respond` |
| `respondPlanApproval` | `x.ai/exit_plan_mode/respond` |
| `setModel` | `set_session_model` |
| `setMode` | `set_session_mode` |
| `setAlwaysApprove` | `x.ai/always_approve` |
| `setAutoTrustNewSessions` | `x.ai/auto_trust_new_sessions` |
| `refreshPlanContent` | `x.ai/plan_content` |

Initialization: `initialize` with `clientInfo.name = "grok-build-desktop-tauri"`. Future versions may pass richer capabilities.

---

## 6. v1 limitations (intentional)

| Area | Limitation | Reason |
| --- | --- | --- |
| PTY | Stubbed (no `portable-pty`) | Out of v1 scope; everything in the renderer still renders. |
| File tree | Stubbed (`fs:listDir`, `fs:readFile`) | Workspace FS module needs design + trust model. |
| Account | Stubbed | OAuth + API key flows need a Rust port of `account-manager.ts`. |
| Extensions | Stubbed | MCP/Skills/Plugins management requires a port of `extensions-manager.ts`. |
| Models | Stubbed | Custom provider CRUD needs a port of `model-providers.ts`. |
| Installer | Stubbed | The shell-script and PowerShell installer runners need porting. |
| Custom title bar | Default decorations | `titleBarOverlay` planned for v2. |

Each limitation is documented with a row in [`MIGRATION_STATUS.md`](./MIGRATION_STATUS.md). When ready to implement, the row moves to "Real".

---

## 7. Roadmap to parity

Suggested order (each step unblocks a chunk of the renderer):

1. **PTY (`portable-pty`)** — wires up the right-side terminal; few other dependents.
2. **File tree (`fs:listDir`, `fs:readFile`, `fs:readSessionImageDataUrl`)** — wires the FileTree / FileViewer.
3. **Account** — unlocks Settings → Account and the login flows.
4. **Extensions** (MCP / Skills / Plugins) — unlocks the Extensions page.
5. **Models** (custom providers + usage) — unlocks Models view.
6. **Installer** — wires Agent Settings page.
7. **External editors** — wires "Open in editor…" actions.

Each step is a self-contained `cargo` + `desktop.ts` change; the renderer is unchanged.