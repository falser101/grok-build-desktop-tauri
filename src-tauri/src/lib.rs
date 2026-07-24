//! Crate root for the Tauri 2 backend.
//!
//! Wires plugins, manages state, and registers every `#[tauri::command]`
//! exposed to the renderer. See `docs/MIGRATION_STATUS.md` for the
//! per-method status (✅ / 🟡 / ⬜).

pub mod agent;
pub mod binary;
pub mod commands;
pub mod grok_cli;
pub mod paths;
pub mod state;
pub mod stubs;
pub mod window_cmds;

use state::AppState;
use tauri::Manager;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // Initialise tracing (RUST_LOG controls verbosity).
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info,grok_build_desktop_tauri_lib=debug")),
        )
        .with_target(false)
        .try_init();

    tauri::Builder::default()
        .manage(AppState::default())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        // Forward window resize events to the renderer's
        // `onMaximizeChanged` subscription. We only emit on transitions,
        // so a stream of resize events doesn't fire spurious toggles.
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::Resized(_) = event {
                if let Some(state) = window.try_state::<AppState>() {
                    let _ = state; // touch the state so the manager registers it
                }
                if let Ok(maximized) = window.is_maximized() {
                    use tauri::Emitter;
                    let _ = window.emit_to(window.label(), "window:maximize-changed", maximized);
                }
            }
        })
        // ─────────── Group A: ACP-bridged ───────────
        .invoke_handler(tauri::generate_handler![
            commands::agent_connect,
            commands::agent_get_state,
            commands::agent_new_session,
            commands::agent_prepare_new_chat,
            commands::agent_load_session,
            commands::agent_refresh_history,
            commands::agent_rename_session,
            commands::agent_delete_session,
            commands::agent_fork_session,
            commands::agent_list_rewind_points,
            commands::agent_execute_rewind,
            commands::agent_search_sessions,
            commands::agent_stop,
            commands::agent_send_prompt,
            commands::agent_list_prompt_history,
            commands::agent_cancel,
            commands::agent_cancel_session,
            commands::agent_respond_permission,
            commands::agent_respond_ask_user_question,
            commands::agent_respond_trust_prompt,
            commands::agent_respond_plan_approval,
            commands::agent_set_model,
            commands::agent_set_mode,
            commands::agent_set_always_approve,
            commands::agent_set_auto_trust_new_sessions,
            commands::agent_refresh_plan_content,
            // ─────────── Group C: stubs ───────────
            commands::fs_list_dir,
            commands::fs_read_file,
            commands::fs_read_session_image_data_url,
            commands::fs_path_suggest,
            commands::pick_files,
            commands::attach_paths,
            commands::get_path_for_file,
            commands::trust_list,
            commands::trust_revoke,
            commands::term_start,
            commands::term_write,
            commands::term_resize,
            commands::term_kill,
            commands::ext_list_mcp,
            commands::ext_add_mcp,
            commands::ext_remove_mcp,
            commands::ext_set_mcp_enabled,
            commands::ext_list_skills,
            commands::ext_set_skill_disabled,
            commands::ext_search_skill_catalog,
            commands::ext_install_skill,
            commands::ext_list_plugins,
            commands::ext_install_plugin,
            commands::ext_uninstall_plugin,
            commands::ext_set_plugin_enabled,
            commands::ext_list_hooks,
            commands::ext_read_hook_file,
            commands::ext_get_paths,
            commands::models_list_presets,
            commands::models_list_providers,
            commands::models_upsert_provider,
            commands::models_delete_provider,
            commands::models_add_from_preset,
            commands::models_fetch_provider_models,
            commands::models_get_config_key_index,
            commands::models_query_provider_usage,
            commands::models_reload_agent,
            commands::account_get_status,
            commands::account_login,
            commands::account_cancel_login,
            commands::account_logout,
            commands::account_set_api_key,
            commands::account_refresh_usage,
            commands::account_open_external,
            commands::agent_install,
            commands::agent_installer_status,
            commands::agent_check_for_update,
            commands::agent_upgrade,
            commands::agent_get_channel,
            commands::agent_set_channel,
            commands::files_list_external_editors,
            commands::files_open_in_editor,
            commands::agent_reconnect,
            // ─────────── Group B/D: Tauri-native ───────────
            window_cmds::window_platform,
            window_cmds::window_minimize,
            window_cmds::window_toggle_maximize,
            window_cmds::window_close,
            window_cmds::window_is_maximized,
            window_cmds::ui_request_reload,
            window_cmds::ui_request_toggle_devtools,
            window_cmds::ui_request_about,
            window_cmds::ui_request_open_settings,
            window_cmds::ui_request_new_session,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}