# Quickstart

## Prerequisites

- **Rust** 1.77+ (the Electron project uses 1.92; anything ≥ 1.77 works)
- **Node** 22.x with `pnpm`
- **Tauri 2 system deps** for your platform:
  - **Linux**: `webkit2gtk-4.1`, `libayatana-appindicator3`, `librsvg2`, `pkg-config`, `build-essential`
    ```bash
    sudo pacman -S --needed webkit2gtk-4.1 libayatana-appindicator3 librsvg2 pkgconf base-devel
    # or on Debian/Ubuntu:
    sudo apt install -y libwebkit2gtk-4.1-dev libayatana-appindicator3-dev librsvg2-dev pkg-config build-essential
    ```
  - **macOS**: Xcode Command Line Tools (`xcode-select --install`)
  - **Windows**: WebView2 (preinstalled on Win11; manual install on older builds) + MSVC build tools
- **`grok` binary** on `$PATH` or in `~/.grok/bin/grok` — the Rust agent that backs the UI. If absent, the window still renders and the connection state shows an error pointing to where the desktop looked.

## Build & run

```bash
cd grok-build-desktop-tauri
pnpm install
pnpm tauri:dev      # dev: Vite + Tauri, hot reload
pnpm tauri:build    # production bundle for the current platform
```

Other scripts:

| Script | What it does |
| --- | --- |
| `pnpm dev` | Vite alone (rarely useful — the Tauri dev script wraps it) |
| `pnpm typecheck` | `tsc --noEmit -p tsconfig.web.json` (renderer only) |
| `pnpm dev:wayland` | Same as `tauri:dev` but with Wayland-friendly env (mirror of the Electron `scripts/run-wayland.sh`) |
| `pnpm dev:x11` | Force XWayland path |
| `pnpm tauri:build` | Native installer/bundle for the host OS |

## Where the desktop looks for `grok`

`src-tauri/src/binary.rs::resolve_grok` walks, in order:

1. `$GROK_BINARY`
2. `~/.grok/bin/grok` (or `~/.grok/bin/grok.exe`)
3. `~/.grok/bin/agent`
4. Bundled `<exe-dir>/bin/grok` / `…/bin/agent`
5. `$PATH` (`which grok`, `which agent`)

If all five fail, `agent_connect` returns `"grok binary not found. Tried: …"`. The renderer's connection state surfaces this as `connection: "error"` with the message visible in Agent Settings.

## Stubbed features

A handful of the renderer's panels (file tree, terminal, account login, extensions catalog, custom model providers, installer) call into APIs that are **typed but not implemented** in v1. The adapter layer (`src/desktop.ts`) uses a `Proxy` fallback so any missing method throws a clear error:

```
[stub] account.getAccountStatus: not implemented in grok-build-desktop-tauri v1.
       See docs/MIGRATION_STATUS.md.
```

The UI degrades gracefully — empty lists, disabled buttons, friendly placeholder text. See [`MIGRATION_STATUS.md`](./MIGRATION_STATUS.md) for the full list.

## Troubleshooting

### Window opens but is blank

The Vite dev server isn't reachable. Check that port `1420` is free and that the CSP allows the dev URL. The default CSP is in `src-tauri/tauri.conf.json` under `app.security.csp`.

### `grok binary not found`

Run `which grok`. If it's missing, install it (see the Rust agent repo's README). The desktop will not prompt to install — that's a v2 feature (see [`MIGRATION_STATUS.md`](./MIGRATION_STATUS.md) § "Installer / update").

### fcitx5 / IME on Wayland

`pnpm dev:wayland` sets the env vars the WebKitGTK IME needs. For Wayland Chinese input, your compositor (KDE / labwc / Hyprland) must support `text-input-v3`. Same caveat as the Electron build.

### Window opens but the content area is black

This is almost always a **WebKitGTK rendering backend mismatch** with the session. Two common cases:

1. **`GDK_BACKEND=wayland` is set globally but no Wayland compositor is running** (e.g. crashed Hyprland, headless container, SSH without compositor forwarding). WebKitGTK 4.1 fails silently and the WebView stays blank.

   ```bash
   pgrep -a Hyprland sway river kwin_wayland 2>/dev/null
   ```

   If nothing is listed but `XDG_SESSION_TYPE=wayland`, force the X11/XWayland path:

   ```bash
   unset GDK_BACKEND
   GDK_BACKEND=x11 DISPLAY=:0 pnpm tauri:dev
   # or use the bundled script:
   pnpm dev:x11
   ```

2. **DMABUF renderer crashes on NVIDIA / some wlroots setups.** Disable it:

   ```bash
   WEBKIT_DISABLE_DMABUF_RENDERER=1 WEBKIT_DISABLE_COMPOSITING_MODE=1 pnpm tauri:dev
   ```

The `dev:x11` script already sets all of the above for you.

### Distinguishing "WebView blank" vs "React crashed"

Open the WebView devtools (right-click → Inspect, or Ctrl+Shift+I; in this build you can also call `desktop.requestToggleDevTools()` from a future shell menu). What you see tells you where the failure is:

- **WebView devtools not reachable / window never appears** → compositor / WebKit issue (see above). The `Booting…` placeholder in `index.html` is your friend: if you see that text, the WebView is fine; if you don't, the WebView itself failed to load.
- **Devtools shows `React crashed during boot: …` overlay** → the renderer threw. The error stack trace is on screen; file an issue with the message.
- **Devtools console shows nothing and the `Booting…` placeholder is gone but the page is still black** → React mounted and rendered nothing visible (CSS issue, theme broken). Inspect the DOM tree.

### `error: proc macro panicked ... icon … is not RGBA`

The placeholder PNGs in `src-tauri/icons/` are RGBA. If you replace them with your own, make sure the color type is RGBA (not RGB or palette).

### Vite HMR shows errors about `ipc://localhost`

Tauri injects `ipc://localhost` for IPC. The CSP in `tauri.conf.json` already whitelists it. If you customize the CSP, keep the `ipc:` and `http://ipc.localhost` schemes.

## Logs

Rust logs go through `tracing`. Set `RUST_LOG=info,grok_build_desktop_tauri_lib=debug` before `pnpm tauri:dev` to see the bridge lifecycle.

Renderer logs go to the WebView devtools (right-click → Inspect, or `desktop.requestToggleDevTools()`).