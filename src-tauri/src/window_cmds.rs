//! Group B: window controls + UI shortcuts.
//!
//! These commands map to Tauri 2's window APIs and event channels.
//! They don't talk to the agent; they're purely the desktop chrome.

use tauri::{Emitter, WebviewWindow};

#[tauri::command]
pub fn window_platform() -> String {
    // Tauri 2's `tauri::process` is process-singleton; we want a
    // `NodeJS.Platform`-shaped string ("linux" | "darwin" | "win32"
    // | "freebsd" | ...). Tauri's `tauri_plugin_os::platform()`
    // returns "linux"/"macos"/"windows"; we map to the NodeJS shape.
    let p = tauri_plugin_opener_internal_platform();
    match p.as_str() {
        "macos" => "darwin".to_string(),
        "windows" => "win32".to_string(),
        other => other.to_string(),
    }
}

#[tauri::command]
pub fn window_minimize(window: WebviewWindow) -> Result<(), String> {
    window.minimize().map_err(|e| e.to_string())
}

#[tauri::command]
pub fn window_toggle_maximize(window: WebviewWindow) -> Result<(), String> {
    if window.is_maximized().unwrap_or(false) {
        window.unmaximize().map_err(|e| e.to_string())
    } else {
        window.maximize().map_err(|e| e.to_string())
    }
}

#[tauri::command]
pub fn window_close(window: WebviewWindow) -> Result<(), String> {
    window.close().map_err(|e| e.to_string())
}

#[tauri::command]
pub fn window_is_maximized(window: WebviewWindow) -> bool {
    window.is_maximized().unwrap_or(false)
}

#[tauri::command]
pub fn ui_request_reload(window: WebviewWindow) -> Result<(), String> {
    // Reload by navigating to the current URL.
    window
        .eval("location.reload()")
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn ui_request_toggle_devtools(window: WebviewWindow) -> Result<(), String> {
    if window.is_devtools_open() {
        window.close_devtools();
    } else {
        window.open_devtools();
    }
    Ok(())
}

#[tauri::command]
pub fn ui_request_about(app: tauri::AppHandle) -> Result<(), String> {
    app.emit("ui:about", ()).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn ui_request_open_settings(app: tauri::AppHandle) -> Result<(), String> {
    app.emit("ui:openSettings", ()).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn ui_request_new_session(app: tauri::AppHandle) -> Result<(), String> {
    app.emit("ui:newSession", ()).map_err(|e| e.to_string())
}

// ─────────────────────────── helpers ───────────────────────────

/// Use Tauri's built-in platform detection without pulling in the
/// `tauri-plugin-os` plugin (which would add another capability).
fn tauri_plugin_opener_internal_platform() -> String {
    if cfg!(target_os = "macos") {
        "macos".into()
    } else if cfg!(target_os = "windows") {
        "windows".into()
    } else if cfg!(target_os = "linux") {
        "linux".into()
    } else {
        std::env::consts::OS.to_string()
    }
}