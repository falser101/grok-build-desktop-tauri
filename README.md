# grok-build-desktop-tauri

> **Status:** v0.1.0 — minimum-viable port of [`grok-build-desktop`](../grok-build-desktop/) from Electron to Tauri 2.

Tauri 2 desktop client for [Grok Build](https://x.ai/cli). Reuses the existing React renderer verbatim and replaces the Node/Electron main process with a small Rust core that spawns `grok agent serve` and bridges JSON-RPC over loopback WebSocket.

```text
React renderer (verbatim from grok-build-desktop)  →
   Tauri invoke / listen  →
     Rust core (src-tauri)  →
       grok agent serve (loopback WS, random per-process secret)
```

## Why Tauri?

Smaller bundle, lower idle memory, no Chromium download. The renderer is the same — only the host shell changes.

| | Electron | Tauri 2 |
| --- | --- | --- |
| Idle RAM (Hello World) | ~150–300 MB | ~30–80 MB |
| Install size | ~85 MB (incl. Chromium) | ~3–10 MB (uses system WebView) |
| Native deps in renderer | none | none |
| Renderer reuse | 100% (verbatim) | 100% (verbatim) |

## Layout

```
grok-build-desktop-tauri/
├── src/                    # React renderer (copied from grok-build-desktop)
│   ├── desktop.ts          # Tauri adapter (typed DesktopApi → invoke/listen)
│   ├── shims.d.ts          # ambient `window.desktop` declaration
│   └── shared/types.ts     # ACP + desktop types
├── src-tauri/              # Rust core
│   ├── src/agent.rs        # AgentBridge: spawn serve + WS client
│   ├── src/commands.rs     # Group A + C (real + stubbed)
│   ├── src/window_cmds.rs  # Group B (Tauri-native)
│   ├── src/binary.rs       # Resolve `grok` binary
│   └── src/stubs.rs        # Shared "not_implemented" helper
└── docs/
    ├── DESIGN.md
    ├── MIGRATION_STATUS.md
    └── QUICKSTART.md
```

## What's working in v1

- ✅ Full UI shell (sidebar, title bar, settings, extensions page, models page, etc.)
- ✅ Session list, prompt streaming, cancel, rewind, fork, rename, delete
- ✅ Permission / ask-user-question / trust / plan approval dialogs
- ✅ Window controls, devtools toggle, reload, About
- ✅ Folder picker (`pickFolder`), external URL opener (`openExternal`)

## What's stubbed

- 🟡 File tree (`listDir`/`readFile`/`readSessionImageDataUrl`)
- 🟡 PTY terminal (`termStart`/`termWrite`/`termKill`/`onTermEvent`)
- 🟡 Account (`getAccountStatus`/`login`/`logout`/`setApiKey`)
- 🟡 MCP / Skills / Plugins / Hooks (14 methods)
- 🟡 Custom model providers (9 methods)
- 🟡 Installer (`installAgent`/`upgradeAgent`/`checkForUpdate`)
- 🟡 External editors

See [`docs/MIGRATION_STATUS.md`](./docs/MIGRATION_STATUS.md) for the full per-method table.

## Build

```bash
pnpm install
pnpm tauri:dev          # development
pnpm tauri:build        # production bundle
```

See [`docs/QUICKSTART.md`](./docs/QUICKSTART.md) for prerequisites, troubleshooting, and Wayland/IME notes.

## Related

- [`grok-build-desktop`](../grok-build-desktop/) — the Electron build this is porting from
- [`grok-build`](../grok-build/) — the Rust agent that `grok agent serve` is part of