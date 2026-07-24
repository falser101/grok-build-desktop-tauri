/**
 * Global ambient declarations that complete the Tauri shim.
 *
 * The original Electron preload declared `window.desktop` globally in
 * `src/shared/types.ts`. We mirror that here so legacy renderer files
 * still type-check while we migrate them off `window.desktop.*` one
 * by one.
 *
 * The runtime value is supplied by `src/desktop.ts` (which assigns
 * onto `window.desktop` on first import).
 */
import type { DesktopApi } from "@shared/types";

declare global {
  interface Window {
    desktop: DesktopApi;
  }
}

export {};