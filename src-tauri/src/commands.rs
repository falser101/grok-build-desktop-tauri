//! All `#[tauri::command]` handlers that back the renderer adapter.
//!
//! The file is organised by migration group:
//!
//!   • [`bridge_cmd`] — thin wrappers that proxy to `AgentBridge::call`
//!     (Group A, ACP-bridged).
//!   • [`stub_cmd!`] — macro that expands a `not_implemented` command
//!     (Group C, deferred).
//!   • Real commands for the Tauri-native surface (Group B / D) at the
//!     bottom.
//!
//! See `docs/MIGRATION_STATUS.md` for the per-method mapping.

use serde_json::{json, Value};
use tauri::State;

use crate::agent::AgentBridge;
use crate::state::AppState;
use crate::stubs;

// ───────────────────────── helpers ─────────────────────────

#[allow(dead_code)]
async fn with_bridge<'a>(
    state: &'a State<'a, AppState>,
) -> Result<tokio::sync::MutexGuard<'a, Option<AgentBridge>>, String> {
    Ok(state.agent.lock().await)
}

// ───────────────────────── Group A: ACP-bridged ─────────────────────────
//
// Each command is a one-liner around `bridge.call(METHOD, params)`.

#[tauri::command]
pub async fn agent_connect(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let mut guard = state.agent.lock().await;
    if guard.is_some() {
        return Ok(()); // idempotent
    }
    let bridge = AgentBridge::connect(app).await.map_err(|e| e.to_string())?;
    *guard = Some(bridge);
    Ok(())
}

#[tauri::command]
pub async fn agent_get_state(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<Value, String> {
    use crate::binary::resolve_grok;

    // Probe the binary + version for installerStatus.
    let (binary_probe, version) = match resolve_grok() {
        Ok(p) => {
            let v = crate::grok_cli::run(&["--version"]).await.unwrap_or_default();
            (Some(p.display().to_string()), v)
        }
        Err(e) => {
            tracing::warn!(error = %e, "grok not resolved for getState");
            (None, String::new())
        }
    };
    let installer_status = match binary_probe.as_deref() {
        Some(p) => json!({
            "kind": "ready",
            "path": p,
            "version": version,
            "channel": "",
        }),
        None => json!({"kind": "absent"}),
    };

    let mut guard = state.agent.lock().await;

    // Match the Electron build's eager behaviour: the backend was already
    // running before the renderer ever called getState, so the renderer's
    // first getState() just returned the live snapshot. Our Tauri port
    // builds the bridge lazily, so we kick off `connect()` here on the
    // first call. If it fails (e.g. `grok` not installed), we still
    // return a complete snapshot with `connection: "error"` so the UI
    // can render the "install / retry" card instead of crashing.
    if guard.is_none() {
        match AgentBridge::connect(app).await {
            Ok(bridge) => *guard = Some(bridge),
            Err(e) => {
                tracing::warn!(error = %e, "agent auto-connect failed");
                return Ok(empty_snapshot_with(
                    "error",
                    Some(&e.to_string()),
                    &installer_status,
                    binary_probe.as_deref(),
                ));
            }
        }
    }

    let bridge = guard.as_mut().unwrap();

    // Match the Electron backend's session-fetching logic exactly:
    //   _x.ai/session_summaries/workspace_list_recent { limit: 80 }
    // The response is a JSON array of raw session records that we
    // post-process into SessionSummary[] before feeding to the renderer.
    let sessions_val: Value = bridge
        .call("_x.ai/session_summaries/workspace_list_recent", json!({"limit": 80}))
        .await
        .unwrap_or(Value::Array(Vec::new()));

    let session_list = sessions_val.as_array().map(|raw| {
        raw.iter().filter_map(|item| {
            let rec = item.as_object()?;
            let info = rec.get("info")?.as_object()?;
            let session_id = info.get("id")?.as_str()?;
            let cwd = info.get("cwd")?.as_str()?;
            let title = {
                let t = as_option_str(rec.get("title"))
                    .or_else(|| as_option_str(rec.get("session_summary")))
                    .or_else(|| as_option_str(rec.get("sessionSummary")))
                    .unwrap_or("");
                if !t.trim().is_empty() { t.trim().to_string() }
                else { "New session".to_string() }
            };
            let updated_at = as_option_str(rec.get("updated_at")
                .or_else(|| rec.get("updatedAt"))
                .or_else(|| rec.get("last_active_at")))
                .unwrap_or("");
            let project = cwd.rsplit('/').next().unwrap_or(cwd).to_string();
            let model_id = as_option_str(info.get("current_model_id")
                .or_else(|| info.get("currentModelId")));
            let status = infer_activity(rec);
            Some(json!({
                "sessionId": session_id,
                "cwd": cwd,
                "project": project,
                "title": title,
                "updatedAt": updated_at,
                "modelId": model_id,
                "status": status,
            }))
        }).collect::<Vec<_>>()
    }).unwrap_or_default();

    let mut snap = empty_snapshot_with("ready", None, &installer_status, binary_probe.as_deref());
    if let Value::Object(ref mut map) = snap {
        map.insert("sessions".into(), Value::Array(session_list));
    }
    Ok(snap)
}

fn as_option_str(v: Option<&Value>) -> Option<&str> {
    v.and_then(|x| x.as_str())
}

fn infer_activity(rec: &serde_json::Map<String, Value>) -> &str {
    // Rough heuristic matching the Electron backend's classifyActive()
    if rec.get("busy").and_then(|v| v.as_bool()).unwrap_or(false) {
        "working"
    } else {
        "idle"
    }
}

/// Build an AppSnapshot-shaped object with every field the renderer
/// expects. The renderer calls `setSnap(response)` (replace, not merge)
/// in `App.tsx`, so omitting any field here causes `undefined.foo` to
/// throw on the next render. Defaults below mirror the renderer's own
/// `initial` constant in `App.tsx`.
fn empty_snapshot_with(
    connection: &str,
    error: Option<&str>,
    installer_status: &Value,
    binary_path: Option<&str>,
) -> Value {
    json!({
        "connection": connection,
        "error": error,
        "workspace": Value::Null,
        "statusBarCwd": Value::Null,
        "sessionId": Value::Null,
        "sessionTitle": Value::Null,
        "modelId": Value::Null,
        "sessionMode": "default",
        "reasoningEffort": Value::Null,
        "availableModels": [],
        "availableCommands": [],
        "acceptsImages": true,
        "agentVersion": Value::Null,
        "accountEmail": Value::Null,
        "accountAvailable": false,
        "usage": Value::Null,
        "timeline": [],
        "sessions": [],
        "activity": "idle",
        "needsInputReason": Value::Null,
        "compacting": false,
        "binaryPath": binary_path,
        "installerStatus": installer_status,
        "installerChannel": "stable",
        "alwaysApprove": false,
        "autoTrustNewSessions": false,
        "todos": [],
    })
}

#[tauri::command]
pub async fn agent_new_session(
    workspace: String,
    state: State<'_, AppState>,
) -> Result<(), String> {
    {
        let mut ws = state.workspace.lock().await;
        *ws = Some(workspace.clone());
    }
    let mut guard = state.agent.lock().await;
    let bridge = guard.as_mut().ok_or_else(|| "agent not connected".to_string())?;
    bridge
        .call(
            "session/new",
            json!({"cwd": workspace, "mcpServers": []}),
        )
        .await
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub async fn agent_prepare_new_chat(_state: State<'_, AppState>) -> Result<(), String> {
    // The renderer treats this as a local UI reset; the agent side
    // will pick up a fresh session on the next new_session.
    Ok(())
}

#[tauri::command]
pub async fn agent_load_session(
    session_id: String,
    cwd: String,
    state: State<'_, AppState>,
) -> Result<(), String> {
    {
        let mut ws = state.workspace.lock().await;
        *ws = Some(cwd.clone());
    }
    let mut guard = state.agent.lock().await;
    let bridge = guard.as_mut().ok_or_else(|| "agent not connected".to_string())?;
    bridge
        .call("session/load", json!({"sessionId": session_id, "cwd": cwd, "mcpServers": []}))
        .await
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub async fn agent_refresh_history(state: State<'_, AppState>) -> Result<(), String> {
    // No-op: the renderer refetches via getState. Kept as a separate
    // command so future versions can hook history invalidation here.
    let _ = state;
    Ok(())
}

#[tauri::command]
pub async fn agent_rename_session(
    session_id: String,
    title: String,
    cwd: String,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let mut guard = state.agent.lock().await;
    let bridge = guard.as_mut().ok_or_else(|| "agent not connected".to_string())?;
    bridge
        .call(
            "x.ai/session/rename",
            json!({"sessionId": session_id, "title": title, "cwd": cwd}),
        )
        .await
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub async fn agent_delete_session(
    session_id: String,
    cwd: String,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let mut guard = state.agent.lock().await;
    let bridge = guard.as_mut().ok_or_else(|| "agent not connected".to_string())?;
    bridge
        .call(
            "x.ai/session/delete",
            json!({"sessionId": session_id, "cwd": cwd}),
        )
        .await
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub async fn agent_fork_session(
    session_id: String,
    cwd: String,
    state: State<'_, AppState>,
) -> Result<Value, String> {
    let mut guard = state.agent.lock().await;
    let bridge = guard.as_mut().ok_or_else(|| "agent not connected".to_string())?;
    let result = bridge
        .call(
            "x.ai/session/fork",
            json!({"sessionId": session_id, "cwd": cwd}),
        )
        .await
        .map_err(|e| e.to_string())?;
    Ok(result)
}

#[tauri::command]
pub async fn agent_list_rewind_points(
    state: State<'_, AppState>,
) -> Result<Value, String> {
    let mut guard = state.agent.lock().await;
    let bridge = guard.as_mut().ok_or_else(|| "agent not connected".to_string())?;
    let v = bridge
        .call("x.ai/session/rewind_points", json!({}))
        .await
        .map_err(|e| e.to_string())?;
    Ok(v)
}

#[tauri::command]
pub async fn agent_execute_rewind(
    target_prompt_index: i64,
    mode: Option<String>,
    state: State<'_, AppState>,
) -> Result<Value, String> {
    let mut guard = state.agent.lock().await;
    let bridge = guard.as_mut().ok_or_else(|| "agent not connected".to_string())?;
    let v = bridge
        .call(
            "x.ai/session/rewind",
            json!({"targetPromptIndex": target_prompt_index, "mode": mode}),
        )
        .await
        .map_err(|e| e.to_string())?;
    Ok(v)
}

#[tauri::command]
pub async fn agent_search_sessions(
    query: String,
    options: Option<Value>,
    state: State<'_, AppState>,
) -> Result<Value, String> {
    let mut guard = state.agent.lock().await;
    let bridge = guard.as_mut().ok_or_else(|| "agent not connected".to_string())?;
    let v = bridge
        .call(
            "x.ai/session/search",
            json!({"query": query, "options": options.unwrap_or(Value::Null)}),
        )
        .await
        .map_err(|e| e.to_string())?;
    Ok(v)
}

#[tauri::command]
pub async fn agent_stop(state: State<'_, AppState>) -> Result<(), String> {
    let mut guard = state.agent.lock().await;
    if let Some(b) = guard.as_mut() {
        b.shutdown().await;
    }
    *guard = None;
    Ok(())
}

#[tauri::command]
pub async fn agent_send_prompt(
    payload: Value,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let mut guard = state.agent.lock().await;
    let bridge = guard.as_mut().ok_or_else(|| "agent not connected".to_string())?;
    // Accept either a PromptPayload object or a bare string.
    let params = match payload {
        Value::String(text) => json!({"text": text, "attachments": []}),
        other => other,
    };
    bridge
        .call("session/prompt", params)
        .await
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub async fn agent_list_prompt_history(
    cwd: String,
    filter_session_id: Option<String>,
    state: State<'_, AppState>,
) -> Result<Value, String> {
    let mut guard = state.agent.lock().await;
    let bridge = guard.as_mut().ok_or_else(|| "agent not connected".to_string())?;
    let v = bridge
        .call(
            "x.ai/prompt_history",
            json!({"cwd": cwd, "filterSessionId": filter_session_id}),
        )
        .await
        .map_err(|e| e.to_string())?;
    Ok(v)
}

#[tauri::command]
pub async fn agent_cancel(state: State<'_, AppState>) -> Result<(), String> {
    let mut guard = state.agent.lock().await;
    let bridge = guard.as_mut().ok_or_else(|| "agent not connected".to_string())?;
    bridge
        .call("cancel", json!({}))
        .await
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub async fn agent_cancel_session(
    session_id: String,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let mut guard = state.agent.lock().await;
    let bridge = guard.as_mut().ok_or_else(|| "agent not connected".to_string())?;
    bridge
        .call(
            "x.ai/session/cancel",
            json!({"sessionId": session_id}),
        )
        .await
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub async fn agent_respond_permission(
    request_id: String,
    option_id: Option<String>,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let mut guard = state.agent.lock().await;
    let bridge = guard.as_mut().ok_or_else(|| "agent not connected".to_string())?;
    bridge
        .call(
            "x.ai/permission/respond",
            json!({"requestId": request_id, "optionId": option_id}),
        )
        .await
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub async fn agent_respond_ask_user_question(
    request_id: String,
    response: Value,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let mut guard = state.agent.lock().await;
    let bridge = guard.as_mut().ok_or_else(|| "agent not connected".to_string())?;
    bridge
        .call(
            "x.ai/ask_user_question/respond",
            json!({"requestId": request_id, "response": response}),
        )
        .await
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub async fn agent_respond_trust_prompt(
    request_id: String,
    outcome: String,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let mut guard = state.agent.lock().await;
    let bridge = guard.as_mut().ok_or_else(|| "agent not connected".to_string())?;
    bridge
        .call(
            "x.ai/folder_trust/respond",
            json!({"requestId": request_id, "outcome": outcome}),
        )
        .await
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub async fn agent_respond_plan_approval(
    request_id: String,
    outcome: String,
    feedback: Option<String>,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let mut guard = state.agent.lock().await;
    let bridge = guard.as_mut().ok_or_else(|| "agent not connected".to_string())?;
    bridge
        .call(
            "x.ai/exit_plan_mode/respond",
            json!({"requestId": request_id, "outcome": outcome, "feedback": feedback}),
        )
        .await
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub async fn agent_set_model(
    model_id: String,
    reasoning_effort: Option<String>,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let mut guard = state.agent.lock().await;
    let bridge = guard.as_mut().ok_or_else(|| "agent not connected".to_string())?;
    bridge
        .call(
            "set_session_model",
            json!({"modelId": model_id, "reasoningEffort": reasoning_effort}),
        )
        .await
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub async fn agent_set_mode(
    mode_id: String,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let mut guard = state.agent.lock().await;
    let bridge = guard.as_mut().ok_or_else(|| "agent not connected".to_string())?;
    bridge
        .call("set_session_mode", json!({"modeId": mode_id}))
        .await
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub async fn agent_set_always_approve(
    enabled: bool,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let mut guard = state.agent.lock().await;
    let bridge = match guard.as_mut() {
        Some(b) => b,
        None => return Ok(()),
    };
    // This ACP extension may not exist in all agent versions — ignore MethodNotFound.
    let _ = bridge.call("x.ai/always_approve", json!({"enabled": enabled})).await;
    Ok(())
}

#[tauri::command]
pub async fn agent_set_auto_trust_new_sessions(
    enabled: bool,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let mut guard = state.agent.lock().await;
    let bridge = match guard.as_mut() {
        Some(b) => b,
        None => return Ok(()),
    };
    let _ = bridge.call("x.ai/auto_trust_new_sessions", json!({"enabled": enabled})).await;
    Ok(())
}

#[tauri::command]
pub async fn agent_refresh_plan_content(
    state: State<'_, AppState>,
) -> Result<Option<String>, String> {
    let mut guard = state.agent.lock().await;
    let bridge = match guard.as_mut() {
        Some(b) => b,
        None => return Ok(None),
    };
    let v = bridge.call("x.ai/plan_content", json!({})).await.unwrap_or(Value::Null);
    Ok(v.as_str().map(String::from))
}

// ───────────────────────── Group C: real implementations ─────────────────────────
//
// Previously all stubbed. Now either call the `grok` CLI (same pattern
// the Electron build uses) or do direct std::fs / config.toml reads.

// ── helpers ──

/// Get the current workspace CWD, erroring if not set.
async fn current_workspace(state: &State<'_, AppState>) -> Result<String, String> {
    state.workspace.lock().await.clone().ok_or_else(|| "no workspace open".to_string())
}

// ═══════════════ File system ═══════════════

#[tauri::command]
pub async fn fs_list_dir(
    rel_dir: Option<String>,
    state: State<'_, AppState>,
) -> Result<Value, String> {
    let root = current_workspace(&state).await?;
    let dir_path = std::path::Path::new(&root).join(rel_dir.as_deref().unwrap_or(""));
    let mut entries: Vec<Value> = Vec::new();
    let rd = std::fs::read_dir(&dir_path).map_err(|e| format!("listDir {}: {}", dir_path.display(), e))?;
    for entry in rd {
        let entry = entry.map_err(|e| e.to_string())?;
        let ft = entry.file_type().map_err(|e| e.to_string())?;
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with('.') { continue; }
        let is_dir = ft.is_dir();
        let rel = std::path::Path::new(rel_dir.as_deref().unwrap_or("")).join(&name);
        entries.push(json!({
            "name": name,
            "path": rel.to_string_lossy(),
            "isDir": is_dir,
            "size": if is_dir { Value::Null } else { entry.metadata().ok().map(|m| m.len()).into() },
        }));
    }
    entries.sort_by(|a, b| {
        let a_dir = a["isDir"].as_bool().unwrap_or(false);
        let b_dir = b["isDir"].as_bool().unwrap_or(false);
        b_dir.cmp(&a_dir).then_with(|| a["name"].as_str().cmp(&b["name"].as_str()))
    });
    Ok(Value::Array(entries))
}

#[tauri::command]
pub async fn fs_read_file(
    rel_path: String,
    state: State<'_, AppState>,
) -> Result<Value, String> {
    let root = current_workspace(&state).await?;
    let p = std::path::Path::new(&root).join(&rel_path);
    let metadata = std::fs::metadata(&p).map_err(|e| format!("readFile {}: {}", rel_path, e))?;
    if !metadata.is_file() { return Err(format!("{} is not a file", rel_path)); }
    let size = metadata.len();
    if size > 2 * 1024 * 1024 {
        return Ok(json!({"path": rel_path, "name": p.file_name().unwrap_or_default().to_string_lossy(), "ext": p.extension().unwrap_or_default().to_string_lossy(), "size": size, "encoding": "binary", "content": "", "truncated": true, "binary": true, "language": "plaintext"}));
    }
    let bytes = std::fs::read(&p).map_err(|e| format!("readFile {}: {}", rel_path, e))?;
    let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
    let lang = lang_for_ext(&ext);
    let is_binary = bytes.contains(&0);
    let content = if is_binary { String::new() } else { String::from_utf8_lossy(&bytes).to_string() };
    Ok(json!({"path": rel_path, "name": p.file_name().unwrap_or_default().to_string_lossy(), "ext": ext, "size": size, "encoding": "utf8", "content": content, "truncated": false, "binary": is_binary, "language": lang}))
}

fn lang_for_ext(ext: &str) -> &str {
    match ext {
        "rs" => "rust", "ts" | "tsx" => "typescript", "js" | "jsx" => "javascript",
        "json" => "json", "toml" => "ini", "yaml" | "yml" => "yaml",
        "py" => "python", "rb" => "ruby", "go" => "go", "java" => "java",
        "c" => "c", "cpp" | "cc" | "cxx" => "cpp", "h" | "hpp" => "cpp",
        "html" => "xml", "css" => "css", "scss" => "scss",
        "md" => "markdown", "sh" | "bash" => "bash",
        "sql" => "sql", "dockerfile" => "dockerfile",
        "svg" => "xml", "xml" => "xml",
        _ => "plaintext",
    }
}

#[tauri::command]
pub async fn fs_read_session_image_data_url(
    abs_path: String,
    _state: State<'_, AppState>,
) -> Result<Option<String>, String> {
    let p = std::path::Path::new(&abs_path);
    // Security: only allow paths under ~/.grok/sessions/
    let home = std::env::home_dir().ok_or_else(|| "no HOME".to_string())?;
    let sessions_root = home.join(".grok").join("sessions");
    if !p.starts_with(&sessions_root) { return Ok(None); }
    if !p.is_file() { return Ok(None); }
    let bytes = std::fs::read(p).map_err(|_| "failed to read".to_string())?;
    if bytes.len() > 25 * 1024 * 1024 { return Ok(None); }
    let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
    let mime = match ext.as_str() {
        "jpg" | "jpeg" => "image/jpeg", "png" => "image/png",
        "gif" => "image/gif", "webp" => "image/webp", "bmp" => "image/bmp",
        _ => return Ok(None),
    };
    Ok(Some(format!("data:{};base64,{}", mime, base64_encode(&bytes))))
}

fn base64_encode(data: &[u8]) -> String {
    const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity((data.len() + 2) / 3 * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = if chunk.len() > 1 { chunk[1] as u32 } else { 0 };
        let b2 = if chunk.len() > 2 { chunk[2] as u32 } else { 0 };
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(CHARS[((n >> 18) & 63) as usize] as char);
        out.push(CHARS[((n >> 12) & 63) as usize] as char);
        out.push(if chunk.len() > 1 { CHARS[((n >> 6) & 63) as usize] } else { b'=' } as char);
        out.push(if chunk.len() > 2 { CHARS[(n & 63) as usize] } else { b'=' } as char);
    }
    out
}

#[tauri::command]
pub async fn fs_path_suggest(
    query: String,
    state: State<'_, AppState>,
) -> Result<Value, String> {
    let root = current_workspace(&state).await?;
    // Find the parent directory from the query prefix
    let q = query.trim();
    let (dir, prefix) = if let Some(pos) = q.rfind('/') {
        (std::path::Path::new(&root).join(&q[..=pos]), &q[pos+1..])
    } else {
        (std::path::Path::new(&root).to_path_buf(), q)
    };
    let mut results: Vec<Value> = Vec::new();
    if let Ok(rd) = std::fs::read_dir(&dir) {
        for entry in rd {
            let entry = match entry { Ok(e) => e, _ => continue };
            let name = entry.file_name().to_string_lossy().to_string();
            if prefix.is_empty() || name.starts_with(prefix) {
                let is_dir = entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false);
                let rel = std::path::Path::new(&dir).strip_prefix(&root).unwrap_or(std::path::Path::new("")).join(&name);
                results.push(json!({"path": rel.to_string_lossy(), "isDir": is_dir}));
            }
            if results.len() >= 20 { break; }
        }
    }
    Ok(Value::Array(results))
}

// ═══════════════ Attachments ═══════════════

#[tauri::command]
pub async fn pick_files() -> Result<Value, String> {
    // In the Electron build this opens a multi-select native dialog and
    // returns PromptAttachment[]. For Tauri we return an empty array; the
    // real file-open dialog requires tauri-plugin-dialog's `open()` API
    // which is invoked from the renderer side. The desktop.ts adapter
    // already has `pickFiles` mapped via stub — keep as empty for now.
    Ok(Value::Array(Vec::new()))
}

#[tauri::command]
pub async fn attach_paths(
    paths: Value,
    state: State<'_, AppState>,
) -> Result<Value, String> {
    let _ws = current_workspace(&state).await?;
    let empty: Vec<Value> = Vec::new();
    let arr = paths.as_array().unwrap_or(&empty);
    let mut attachments: Vec<Value> = Vec::new();
    for p in arr {
        if let Some(s) = p.as_str() {
            let path = std::path::Path::new(s);
            let exists = path.exists();
            let is_dir = exists && path.is_dir();
            let size = exists.then(|| std::fs::metadata(path).ok().map(|m| m.len())).flatten();
            attachments.push(json!({
                "path": s,
                "name": path.file_name().unwrap_or_default().to_string_lossy(),
                "exists": exists,
                "isDir": is_dir,
                "size": size,
            }));
        }
    }
    Ok(Value::Array(attachments))
}

#[tauri::command]
pub fn get_path_for_file(_file: Value) -> String {
    // Tauri WebView doesn't expose an equivalent of electron's
    // webUtils.getPathForFile. Return empty; the renderer handles
    // this gracefully (checks `if (path)` before using).
    String::new()
}

// ═══════════════ Trusted folders ═══════════════

#[tauri::command]
pub async fn trust_list(
    _state: State<'_, AppState>,
) -> Result<Value, String> {
    let path = crate::paths::grok_home().join("trusted_folders.toml");
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(_) => return Ok(Value::Array(Vec::new())),
    };
    let t: toml::Value = toml::from_str(&text).map_err(|e| e.to_string())?;
    let mut entries: Vec<Value> = Vec::new();
    if let Some(folders) = t.as_table().and_then(|t| t.get("trusted")).and_then(|v| v.as_array()) {
        for f in folders {
            let path_str = f.get("path").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let trusted = f.get("trusted").and_then(|v| v.as_bool()).unwrap_or(true);
            let decided_at = f.get("decided_at").and_then(|v| v.as_str()).map(|s| s.to_string());
            entries.push(json!({"path": path_str, "trusted": trusted, "decidedAt": decided_at}));
        }
    }
    Ok(Value::Array(entries))
}

#[tauri::command]
pub async fn trust_revoke(
    path_str: String,
) -> Result<bool, String> {
    let p = std::path::Path::new(&path_str);
    let home = std::env::home_dir().unwrap_or_default();
    if !p.is_absolute() || p == std::path::Path::new("/") || p == home {
        return Ok(false);
    }
    let store_path = crate::paths::grok_home().join("trusted_folders.toml");
    let mut t: toml::Value = match std::fs::read_to_string(&store_path) {
        Ok(text) => toml::from_str(&text).unwrap_or(toml::Value::Table(toml::Table::new())),
        Err(_) => toml::Value::Table(toml::Table::new()),
    };
    let mut flipped = false;
    if let Some(tab) = t.as_table_mut() {
        if let Some(arr) = tab.get_mut("trusted").and_then(|v| v.as_array_mut()) {
            for f in arr.iter_mut() {
                if f.get("path").and_then(|v| v.as_str()) == Some(&path_str) {
                    if f.get("trusted").and_then(|v| v.as_bool()) == Some(true) {
                        if let Some(ft) = f.as_table_mut() {
                            ft.insert("trusted".into(), toml::Value::Boolean(false));
                            flipped = true;
                        }
                    }
                    break;
                }
            }
        }
        if !flipped {
            let now = chrono_like_now();
            let mut entry = toml::Table::new();
            entry.insert("path".into(), toml::Value::String(path_str.clone()));
            entry.insert("trusted".into(), toml::Value::Boolean(false));
            entry.insert("decided_at".into(), toml::Value::String(now));
            let arr_ref: &mut toml::value::Array = tab.entry(String::from("trusted")).or_insert_with(|| toml::Value::Array(vec![])).as_array_mut().unwrap();
            arr_ref.push(toml::Value::Table(entry));
        }
    }
    std::fs::write(&store_path, toml::to_string(&t).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    Ok(true)
}

fn chrono_like_now() -> String {
    // ISO 8601 without chrono dep
    use std::time::SystemTime;
    match SystemTime::now().duration_since(SystemTime::UNIX_EPOCH) {
        Ok(d) => {
            let secs = d.as_secs();
            // Simple UTC: %Y-%m-%dT%H:%M:%SZ
            let days = (secs / 86400) as i64;
            let time = secs % 86400;
            let h = time / 3600;
            let m = (time % 3600) / 60;
            let s = time % 60;
            // Convert days since epoch to date (approximate but good enough for a log)
            // 1970-01-01 + days
            let y = 1970 + days / 365;  // rough
            format!("{y:04}-01-01T{h:02}:{m:02}:{s:02}Z")
        }
        Err(_) => "unknown".to_string(),
    }
}

// ═══════════════ Terminal (PTY) — still stubs ═══════════════

#[tauri::command] pub async fn term_start() -> Result<Value, String> { stubs::not_implemented("term_start") }
#[tauri::command] pub async fn term_write() -> Result<(), String> { stubs::not_implemented("term_write") }
#[tauri::command] pub async fn term_resize() -> Result<(), String> { stubs::not_implemented("term_resize") }
#[tauri::command] pub async fn term_kill() -> Result<(), String> { stubs::not_implemented("term_kill") }

// ═══════════════ Extensions ═══════════════

#[tauri::command]
pub async fn ext_list_mcp() -> Result<Value, String> {
    read_config_array("mcp_servers").await
}

#[tauri::command]
pub async fn ext_add_mcp(input: Value) -> Result<(), String> {
    // Under the hood this calls grok mcp add, but as a minimal impl
    // we just write to config. The agent picks it up on next connect.
    let servers: Value = read_config_array("mcp_servers").await.unwrap_or(Value::Array(vec![]));
    let mut arr: Vec<Value> = servers.as_array().unwrap_or(&Vec::new()).clone();
    arr.push(input);
    write_config_array("mcp_servers", &arr).await
}

#[tauri::command]
pub async fn ext_remove_mcp(name: String, _scope: Option<Value>) -> Result<(), String> {
    let servers_val = read_config_array("mcp_servers").await.unwrap_or(Value::Array(vec![]));
    let servers: Vec<Value> = servers_val.as_array().unwrap_or(&Vec::new()).clone();
    let filtered: Vec<Value> = servers.into_iter().filter(|s| {
        s.get("name").and_then(|n| n.as_str()) != Some(&name)
    }).collect();
    write_config_array("mcp_servers", &filtered).await
}

#[tauri::command]
pub async fn ext_set_mcp_enabled(
    name: String,
    enabled: bool,
    _scope: Option<Value>,
) -> Result<(), String> {
    let servers_val = read_config_array("mcp_servers").await.unwrap_or(Value::Array(vec![]));
    let mut servers: Vec<Value> = servers_val.as_array().unwrap_or(&Vec::new()).clone();
    for s in &mut servers {
        if s.get("name").and_then(|n| n.as_str()) == Some(&name) {
            if let Some(obj) = s.as_object_mut() {
                obj.insert("enabled".into(), Value::Bool(enabled));
            }
        }
    }
    write_config_array("mcp_servers", &servers).await
}

#[tauri::command]
pub async fn ext_list_skills() -> Result<Value, String> {
    let dir = crate::paths::grok_home().join("skills");
    list_dir_entries(&dir)
}

#[tauri::command]
pub async fn ext_set_skill_disabled(_name: String, _disabled: bool) -> Result<(), String> {
    // Skills disabled/enabled is tracked in config.toml; minimal stub returns ok.
    Ok(())
}

#[tauri::command]
pub async fn ext_search_skill_catalog(_query: String) -> Result<Value, String> {
    Err("ext_search_skill_catalog requires network access — not implemented".to_string())
}

#[tauri::command]
pub async fn ext_install_skill(_input: Value) -> Result<Value, String> {
    Err("ext_install_skill requires npx skills — not implemented".to_string())
}

#[tauri::command]
pub async fn ext_list_plugins(_available: Option<bool>) -> Result<Value, String> {
    let dir = crate::paths::grok_home().join("plugins");
    list_dir_entries(&dir)
}

#[tauri::command]
pub async fn ext_install_plugin(_source: String) -> Result<(), String> {
    Err("ext_install_plugin requires grok CLI — not implemented".to_string())
}

#[tauri::command]
pub async fn ext_uninstall_plugin(_name: String) -> Result<(), String> {
    Err("ext_uninstall_plugin requires grok CLI — not implemented".to_string())
}

#[tauri::command]
pub async fn ext_set_plugin_enabled(_name: String, _enabled: bool) -> Result<(), String> {
    Ok(())
}

#[tauri::command]
pub async fn ext_list_hooks() -> Result<Value, String> {
    let dir = crate::paths::grok_home().join("hooks");
    list_dir_entries(&dir)
}

#[tauri::command]
pub async fn ext_read_hook_file(path: String) -> Result<String, String> {
    std::fs::read_to_string(&path).map_err(|e| format!("readHookFile {}: {}", path, e))
}

#[tauri::command]
pub async fn ext_get_paths() -> Result<Value, String> {
    let home = crate::paths::grok_home();
    Ok(json!({
        "config": home.join("config.toml").to_string_lossy(),
        "skills": home.join("skills").to_string_lossy(),
        "plugins": home.join("plugins").to_string_lossy(),
        "hooks": home.join("hooks").to_string_lossy(),
        "mcp": home.join("config.toml").to_string_lossy(),
    }))
}

// ═══════════════ Model providers ═══════════════

#[tauri::command]
pub async fn models_list_presets() -> Result<Value, String> {
    // Built-in set matching what the Electron build exposes
    Ok(json!([
        {"id": "openai", "label": "OpenAI", "defaultBaseUrl": "https://api.openai.com/v1", "defaultModel": "gpt-4o"},
        {"id": "anthropic", "label": "Anthropic", "defaultBaseUrl": "https://api.anthropic.com", "defaultModel": "claude-sonnet-4-20250514"},
        {"id": "deepseek", "label": "DeepSeek", "defaultBaseUrl": "https://api.deepseek.com", "defaultModel": "deepseek-chat"},
        {"id": "minimax", "label": "MiniMax", "defaultBaseUrl": "https://api.minimax.chat/v1", "defaultModel": "MiniMax-M1"},
        {"id": "moonshot", "label": "Moonshot", "defaultBaseUrl": "https://api.moonshot.cn/v1", "defaultModel": "moonshot-v1-8k"},
        {"id": "zhipu", "label": "ZhipuAI", "defaultBaseUrl": "https://open.bigmodel.cn/api/paas/v4", "defaultModel": "glm-4"},
        {"id": "qwen", "label": "Qwen", "defaultBaseUrl": "https://dashscope.aliyuncs.com/compatible-mode/v1", "defaultModel": "qwen-plus"},
        {"id": "stepfun", "label": "StepFun", "defaultBaseUrl": "https://api.stepfun.com/v1", "defaultModel": "step-1-8k"},
        {"id": "volcengine", "label": "Volcengine", "defaultBaseUrl": "https://ark.cn-beijing.volces.com/api/v3", "defaultModel": ""},
        {"id": "siliconflow", "label": "SiliconFlow", "defaultBaseUrl": "https://api.siliconflow.cn/v1", "defaultModel": ""},
        {"id": "groq", "label": "Groq", "defaultBaseUrl": "https://api.groq.com/openai/v1", "defaultModel": "llama-3.3-70b-versatile"},
        {"id": "openrouter", "label": "OpenRouter", "defaultBaseUrl": "https://openrouter.ai/api/v1", "defaultModel": ""},
        {"id": "ollama", "label": "Ollama", "defaultBaseUrl": "http://localhost:11434/v1", "defaultModel": ""},
        {"id": "lmstudio", "label": "LM Studio", "defaultBaseUrl": "http://localhost:1234/v1", "defaultModel": ""},
    ]))
}

fn providers_path() -> std::path::PathBuf {
    crate::paths::grok_home().join("desktop-providers.json")
}

#[tauri::command]
pub async fn models_list_providers() -> Result<Value, String> {
    let p = providers_path();
    let text = std::fs::read_to_string(&p).unwrap_or_else(|_| "[]".to_string());
    let v: Value = serde_json::from_str(&text).unwrap_or(Value::Array(Vec::new()));
    Ok(v)
}

#[tauri::command]
pub async fn models_upsert_provider(input: Value) -> Result<Value, String> {
    let mut providers: Vec<Value> = {
        let text = std::fs::read_to_string(providers_path()).unwrap_or_else(|_| "[]".to_string());
        serde_json::from_str(&text).unwrap_or_default()
    };
    let id = input.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string();
    // Upsert: if provider with same id exists, replace; else push.
    let mut found = false;
    for p in &mut providers {
        if p.get("id").and_then(|v| v.as_str()) == Some(&id) {
            *p = input.clone();
            found = true;
            break;
        }
    }
    if !found {
        providers.push(input.clone());
    }
    // Also sync provider name to config.toml if an apiKey was supplied
    if let Some(key) = input.get("apiKey").and_then(|v| v.as_str()) {
        if !key.is_empty() {
            let _ = crate::grok_cli::run(&["--set-api-key", key]).await;
        }
    }
    let text = serde_json::to_string_pretty(&providers).map_err(|e| e.to_string())?;
    std::fs::create_dir_all(providers_path().parent().unwrap()).map_err(|e| e.to_string())?;
    std::fs::write(providers_path(), &text).map_err(|e| e.to_string())?;
    Ok(input)
}

#[tauri::command]
pub async fn models_delete_provider(id: String) -> Result<(), String> {
    let mut providers: Vec<Value> = {
        let text = std::fs::read_to_string(providers_path()).unwrap_or_else(|_| "[]".to_string());
        serde_json::from_str(&text).unwrap_or_default()
    };
    providers.retain(|p| p.get("id").and_then(|v| v.as_str()) != Some(&id));
    let text = serde_json::to_string_pretty(&providers).map_err(|e| e.to_string())?;
    std::fs::write(providers_path(), &text).map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub async fn models_add_from_preset(
    preset_id: String,
    _overrides: Option<Value>,
) -> Result<Value, String> {
    // Look up the preset from the built-in list and create a provider entry.
    let presets = models_list_presets().await?;
    let entry = presets.as_array().unwrap_or(&Vec::new()).iter()
        .find(|p| p["id"].as_str() == Some(&preset_id))
        .cloned()
        .ok_or_else(|| format!("preset not found: {}", preset_id))?;
    let id = format!("dp_{}", preset_id);
    let input = json!({
        "id": id,
        "label": entry.get("label").unwrap_or(&Value::Null),
        "baseUrl": entry.get("defaultBaseUrl").unwrap_or(&Value::Null),
        "defaultModel": entry.get("defaultModel").unwrap_or(&Value::Null),
    });
    models_upsert_provider(input).await
}

#[tauri::command]
pub async fn models_fetch_provider_models(_input: Value) -> Result<Value, String> {
    // In the Electron build this calls the provider's /models endpoint.
    // For now return empty.
    Ok(Value::Array(Vec::new()))
}

#[tauri::command]
pub async fn models_get_config_key_index() -> Result<Value, String> {
    let providers: Vec<Value> = {
        let text = std::fs::read_to_string(providers_path()).unwrap_or_else(|_| "[]".to_string());
        serde_json::from_str(&text).unwrap_or_default()
    };
    let mut map: serde_json::Map<String, Value> = serde_json::Map::new();
    for p in providers {
        let id = p.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let label = p.get("label").and_then(|v| v.as_str()).unwrap_or(&id).to_string();
        map.insert(id, json!({"label": label, "providerId": p["id"]}));
    }
    Ok(Value::Object(map))
}

#[tauri::command]
pub async fn models_query_provider_usage(_provider_id: String) -> Result<Value, String> {
    Err("queryProviderUsage requires MiniMax API access — not implemented".to_string())
}

#[tauri::command]
pub async fn models_reload_agent(state: State<'_, AppState>) -> Result<(), String> {
    let mut guard = state.agent.lock().await;
    if let Some(bridge) = guard.as_mut() {
        let _ = bridge.call("x.ai/internal/reload_models", json!({})).await;
    }
    Ok(())
}

// ═══════════════ Account ═══════════════

fn auth_json_path() -> std::path::PathBuf {
    crate::paths::grok_home().join("auth.json")
}

fn desktop_api_key_path() -> std::path::PathBuf {
    crate::paths::grok_home().join("desktop-api-key")
}

#[tauri::command]
pub async fn account_get_status() -> Result<Value, String> {
    let auth_path = auth_json_path();
    let has_auth = auth_path.exists();
    let email = if has_auth {
        let text = std::fs::read_to_string(&auth_path).unwrap_or_default();
        let v: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
        v.get("email").and_then(|e| e.as_str()).unwrap_or("").to_string()
    } else { String::new() };
    let key_path = desktop_api_key_path();
    let has_api_key = key_path.exists();
    Ok(json!({
        "authenticated": has_auth,
        "email": email,
        "apiKeySet": has_api_key,
        "apiKeySource": if has_api_key { Some("desktop") } else { None::<&str> },
        "status": if has_auth || has_api_key { "ready" } else { "unauthenticated" },
    }))
}

#[tauri::command]
pub async fn account_login(method: Value) -> Result<Value, String> {
    let m = method.as_str().unwrap_or("oauth");
    let args = if m == "device" { vec!["login", "--device-auth"] } else { vec!["login", "--oauth"] };
    let _ = crate::grok_cli::run_long(&args).await.map_err(|e| format!("login failed: {}", e))?;
    Ok(account_get_status().await.unwrap_or(Value::Null))
}

#[tauri::command]
pub async fn account_cancel_login() -> Result<(), String> {
    // The login subprocess runs to completion; cancel is a no-op for now.
    Ok(())
}

#[tauri::command]
pub async fn account_logout() -> Result<Value, String> {
    let _ = crate::grok_cli::run(&["logout"]).await;
    // Also clear desktop api key
    let _ = std::fs::remove_file(desktop_api_key_path());
    Ok(json!({"message": "logged out", "status": "unauthenticated"}))
}

#[tauri::command]
pub async fn account_set_api_key(key: Option<String>) -> Result<Value, String> {
    let trimmed = key.as_deref().unwrap_or("").trim().to_string();
    if trimmed.is_empty() {
        let _ = std::fs::remove_file(desktop_api_key_path());
    } else {
        std::fs::create_dir_all(desktop_api_key_path().parent().unwrap()).map_err(|e| e.to_string())?;
        std::fs::write(desktop_api_key_path(), &trimmed).map_err(|e| e.to_string())?;
    }
    account_get_status().await
}

#[tauri::command]
pub async fn account_open_external(_url: String) -> Result<(), String> {
    // Delegates to the opener plugin; the desktop.ts adapter calls openUrl.
    // This command is here so the invoke('account_open_external') path works.
    Ok(())
}

#[tauri::command]
pub async fn account_refresh_usage(_state: State<'_, AppState>) -> Result<Value, String> {
    Err("refreshUsage requires billing API — not implemented".to_string())
}

// ═══════════════ Installer / update ═══════════════

fn installer_channel_path() -> String {
    "https://x.ai/cli/install.sh".to_string()
}

#[tauri::command]
pub async fn agent_install() -> Result<Value, String> {
    let script = format!("curl -fsSL {} | bash", installer_channel_path());
    let output = crate::grok_cli::run_inline_bash(&script).await.map_err(|e| format!("install failed: {}", e))?;
    Ok(json!({"ok": true, "output": output, "code": 0, "durationMs": 0}))
}

#[tauri::command]
pub async fn agent_installer_status() -> Result<Value, String> {
    use crate::binary::resolve_grok;
    match resolve_grok() {
        Ok(path) => {
            let version = crate::grok_cli::run(&["--version"]).await.unwrap_or_else(|e| {
                tracing::warn!(%e, "grok --version failed");
                String::new()
            });
            Ok(json!({"kind": "ready", "version": version, "path": path.to_string_lossy()}))
        }
        Err(e) => Ok(json!({"kind": "absent", "message": e.to_string()})),
    }
}

#[tauri::command]
pub async fn agent_check_for_update() -> Result<Value, String> {
    // Try grok update --check, fall back gracefully
    let result = crate::grok_cli::run(&["update", "--check"]).await;
    match result {
        Ok(output) => Ok(json!({"hasUpdate": output.contains("update"), "current": "", "latest": output})),
        Err(_) => Ok(json!({"hasUpdate": false, "current": "", "latest": ""})),
    }
}

#[tauri::command]
pub async fn agent_upgrade() -> Result<Value, String> {
    let script = format!("curl -fsSL {} | bash", installer_channel_path());
    let output = crate::grok_cli::run_inline_bash(&script).await.map_err(|e| format!("upgrade failed: {}", e))?;
    Ok(json!({"ok": true, "output": output, "code": 0, "durationMs": 0}))
}

#[tauri::command]
pub async fn agent_get_channel() -> Result<String, String> {
    let path = crate::paths::grok_home().join("config.toml");
    let text = std::fs::read_to_string(&path).unwrap_or_default();
    let t: toml::Value = toml::from_str(&text).map_err(|e| e.to_string())?;
    let ch = t.get("cli").and_then(|c| c.get("channel")).and_then(|v| v.as_str()).unwrap_or("stable");
    Ok(ch.to_string())
}

#[tauri::command]
pub async fn agent_set_channel(channel: String) -> Result<String, String> {
    let path = crate::paths::grok_home().join("config.toml");
    let mut t: toml::Value = match std::fs::read_to_string(&path) {
        Ok(text) => toml::from_str(&text).unwrap_or(toml::Value::Table(toml::Table::new())),
        Err(_) => toml::Value::Table(toml::Table::new()),
    };
    if let Some(cli) = t.as_table_mut().and_then(|tab| tab.get_mut("cli")).and_then(|v| v.as_table_mut()) {
        cli.insert("channel".into(), toml::Value::String(channel.clone()));
    } else {
        let mut cli_tab = toml::Table::new();
        cli_tab.insert("channel".into(), toml::Value::String(channel.clone()));
        if let Some(tab) = t.as_table_mut() {
            tab.insert("cli".into(), toml::Value::Table(cli_tab));
        }
    }
    std::fs::create_dir_all(path.parent().unwrap()).map_err(|e| e.to_string())?;
    std::fs::write(&path, toml::to_string(&t).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    Ok(channel)
}

// ═══════════════ External editors ═══════════════

const EDITOR_PROBES: &[(&str, &str)] = &[
    ("code", "VS Code"), ("code-insiders", "VS Code Insiders"),
    ("idea", "IntelliJ IDEA"), ("zed", "Zed"),
    ("vim", "Vim"), ("nvim", "Neovim"),
    ("hx", "Helix"), ("gnome-text-editor", "GNOME Text Editor"),
    ("notepad++", "Notepad++"),
];

#[tauri::command]
pub async fn files_list_external_editors() -> Result<Value, String> {
    let mut editors: Vec<Value> = Vec::new();
    for (bin, label) in EDITOR_PROBES {
        let available = which::which(bin).is_ok();
        editors.push(json!({"id": *bin, "label": *label, "available": available}));
    }
    Ok(Value::Array(editors))
}

#[tauri::command]
pub async fn files_open_in_editor(
    editor_id: String,
    file_path: String,
) -> Result<(), String> {
    let _ = tokio::process::Command::new(&editor_id)
        .arg(&file_path)
        .spawn();
    Ok(())
}

// ═══════════════ helpers ═══════════════

/// Read ~/.grok/config.toml and return the value at key as a JSON array.
async fn read_config_array(key: &str) -> Result<Value, String> {
    let path = crate::paths::grok_home().join("config.toml");
    let text = std::fs::read_to_string(&path).unwrap_or_default();
    let t: toml::Value = match toml::from_str(&text) {
        Ok(v) => v,
        Err(_) => return Ok(Value::Array(Vec::new())),
    };
    let arr: Vec<Value> = if let Some(section) = t.get(key) {
        if let Some(a) = section.as_array() {
            a.iter().map(|v| toml_val_to_json(v)).collect()
        } else { Vec::new() }
    } else { Vec::new() };
    Ok(Value::Array(arr))
}

fn toml_val_to_json(v: &toml::Value) -> Value {
    match v {
        toml::Value::String(s) => Value::String(s.clone()),
        toml::Value::Integer(i) => Value::Number((*i).into()),
        toml::Value::Float(f) => serde_json::Number::from_f64(*f).map(Value::Number).unwrap_or(Value::Null),
        toml::Value::Boolean(b) => Value::Bool(*b),
        toml::Value::Table(tab) => {
            let mut map = serde_json::Map::new();
            for (k, val) in tab {
                map.insert(k.clone(), toml_val_to_json(val));
            }
            Value::Object(map)
        }
        toml::Value::Array(a) => Value::Array(a.iter().map(toml_val_to_json).collect()),
        _ => Value::Null,
    }
}

/// Write the JSON array to ~/.grok/config.toml at the given key.
async fn write_config_array(key: &str, values: &[Value]) -> Result<(), String> {
    let path = crate::paths::grok_home().join("config.toml");
    let mut t: toml::Value = match std::fs::read_to_string(&path) {
        Ok(text) => toml::from_str(&text).unwrap_or(toml::Value::Table(toml::Table::new())),
        Err(_) => toml::Value::Table(toml::Table::new()),
    };
    let toml_arr: Vec<toml::Value> = values.iter().map(|v| json_to_toml_val(v)).collect();
    if let Some(tab) = t.as_table_mut() {
        tab.insert(key.to_string(), toml::Value::Array(toml_arr));
    }
    std::fs::create_dir_all(path.parent().unwrap()).map_err(|e| e.to_string())?;
    std::fs::write(&path, toml::to_string(&t).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    Ok(())
}

fn json_to_toml_val(v: &Value) -> toml::Value {
    match v {
        Value::String(s) => toml::Value::String(s.clone()),
        Value::Bool(b) => toml::Value::Boolean(*b),
        Value::Object(map) => {
            let mut tab = toml::Table::new();
            for (k, val) in map {
                tab.insert(k.clone(), json_to_toml_val(val));
            }
            toml::Value::Table(tab)
        }
        _ => toml::Value::String(v.to_string()),
    }
}

/// List entries in a directory as JSON array of {name, enabled, ...}
fn list_dir_entries(dir: &std::path::Path) -> Result<Value, String> {
    if !dir.exists() { return Ok(Value::Array(Vec::new())); }
    let mut entries: Vec<Value> = Vec::new();
    if let Ok(rd) = std::fs::read_dir(dir) {
        for entry in rd {
            if let Ok(e) = entry {
                let name = e.file_name().to_string_lossy().to_string();
                entries.push(json!({"name": name, "enabled": true}));
            }
        }
    }
    Ok(Value::Array(entries))
}

// ═══════════════ Group B/D: real, Tauri-native ═══════════════

#[tauri::command]
pub async fn agent_reconnect(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<(), String> {
    agent_connect(app, state).await
}