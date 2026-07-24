// Suppress the additional console window on Windows in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    grok_build_desktop_tauri_lib::run();
}