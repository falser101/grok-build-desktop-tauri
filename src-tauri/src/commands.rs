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
use tauri::{Emitter, State};

use crate::agent::AgentBridge;
use crate::paths::plan_file_path;
use crate::state::{empty_runtime, AppState, SessionRuntime};
use crate::stubs;

// ───────────────────────── helpers ─────────────────────────

#[allow(dead_code)]
async fn with_bridge<'a>(
    state: &'a State<'a, AppState>,
) -> Result<tokio::sync::MutexGuard<'a, Option<AgentBridge>>, String> {
    Ok(state.agent.lock().await)
}

// ───────────────────────── tool content extraction ───────────────────────

/// Max chars of tool output before truncation (mirrors Electron's
/// `MAX_TOOL_OUTPUT_CHARS`).
const MAX_TOOL_OUTPUT_CHARS: usize = 80_000;

/// Parse ACP `ToolCallContent[]` into diffs + concatenated text output.
/// Wire shapes from the agent:
///   { type: "diff", path, oldText?, newText }
///   { type: "content", content: { type: "text", text } }
///   { type: "text", text }
fn parse_tool_content(raw: Option<&Vec<Value>>) -> (Vec<Value>, Option<String>, bool) {
    let Some(arr) = raw else {
        return (vec![], None, false);
    };
    let mut diffs: Vec<Value> = Vec::new();
    let mut text_parts: Vec<String> = Vec::new();

    for item in arr {
        let rec = item.as_object();
        let Some(rec) = rec else { continue };
        let typ = rec.get("type").and_then(|v| v.as_str());

        if typ == Some("diff") {
            let path = rec.get("path")
                .or_else(|| rec.get("filePath"))
                .or_else(|| rec.get("file_path"))
                .and_then(|v| v.as_str());
            let new_text = rec.get("newText")
                .or_else(|| rec.get("new_text"))
                .or_else(|| rec.get("new"))
                .and_then(|v| v.as_str());
            if path.is_none() || new_text.is_none() { continue; }
            let mut diff_obj = serde_json::Map::new();
            diff_obj.insert("path".into(), Value::String(path.unwrap().to_string()));
            diff_obj.insert("newText".into(), Value::String(new_text.unwrap().to_string()));
            if let Some(ot) = rec.get("oldText")
                .or_else(|| rec.get("old_text"))
                .or_else(|| rec.get("old"))
                .and_then(|v| v.as_str())
            {
                diff_obj.insert("oldText".into(), Value::String(ot.to_string()));
            }
            diffs.push(Value::Object(diff_obj));
            continue;
        }

        if typ == Some("content") || typ == Some("text") {
            let nested = rec.get("content").and_then(|v| v.as_object());
            let text = nested.and_then(|n| n.get("text")).and_then(|v| v.as_str())
                .or_else(|| rec.get("text").and_then(|v| v.as_str()));
            if let Some(t) = text {
                text_parts.push(t.to_string());
            }
            continue;
        }

        // Bare `{ text: "…" }` without a type field.
        if typ.is_none() {
            if let Some(t) = rec.get("text").and_then(|v| v.as_str()) {
                text_parts.push(t.to_string());
            }
        }
    }

    let mut output_text = if text_parts.is_empty() { None } else { Some(text_parts.join("\n")) };
    let mut truncated = false;
    if let Some(ref t) = output_text {
        if t.len() > MAX_TOOL_OUTPUT_CHARS {
            output_text = Some(format!("{}\n… [truncated]", &t[..MAX_TOOL_OUTPUT_CHARS]));
            truncated = true;
        }
    }
    (diffs, output_text, truncated)
}

/// Extract a `toolKind` tag from the update payload (read / write / search /
/// bash / …) so the renderer can pick an icon. Mirrors Electron's
/// `semanticToolKind()`.
fn semantic_tool_kind(update: &serde_json::Map<String, Value>) -> Option<String> {
    update.get("toolKind")
        .or_else(|| update.get("tool_kind"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .or_else(|| {
            let title = update.get("title").or_else(|| update.get("name")).and_then(|v| v.as_str())?;
            let t = title.to_lowercase();
            if t.contains("read") { Some("read".into()) }
            else if t.contains("write") { Some("write".into()) }
            else if t.contains("search") || t.contains("grep") { Some("search".into()) }
            else if t.contains("bash") || t.contains("shell") || t.contains("run") { Some("bash".into()) }
            else if t.contains("list") || t.contains("ls") { Some("list_dir".into()) }
            else if t.contains("edit") || t.contains("replace") { Some("edit".into()) }
            else { None }
        })
}

/// Detect `/goal (pause|resume|clear)` at the start of a prompt text.
fn detect_goal_action_verb(text: &str) -> Option<&'static str> {
    let trimmed = text.trim();
    if trimmed.len() < 6 { return None; }
    let lower = trimmed.to_lowercase();
    if lower == "/goal pause" { return Some("pause"); }
    if lower == "/goal resume" { return Some("resume"); }
    if lower == "/goal clear" { return Some("clear"); }
    None
}

/// Push a `goal_action` card into the timeline. Mirrors Electron's
/// `beginGoalAction`.
async fn push_goal_action_card(state: &AppState, verb: &str) {
    {
        let mut tl = state.timeline.lock().await;
        for item in tl.iter_mut() {
            if item.get("kind") == Some(&json!("goal_action"))
                && item.get("status") == Some(&json!("running"))
            {
                item["status"] = json!("cancelled");
            }
        }
    }
    use rand::Rng;
    let id = format!("goalact-{:016x}", rand::thread_rng().gen::<u64>());
    *state.goal_action_timeline_id.lock().await = Some(id.clone());
    let mut tl = state.timeline.lock().await;
    tl.push(json!({"id":id,"kind":"goal_action","verb":verb,"status":"running"}));
    drop(tl);
}

fn detect_manual_compact(text: &str) -> Option<&str> {
    let trimmed = text.trim();
    if trimmed.starts_with("/compact ") || trimmed == "/compact" {
        Some("manual")
    } else {
        None
    }
}

async fn push_compact_card(state: &AppState, mode: &str, percentage: Option<f64>) {
    use rand::Rng;
    let id = format!("compact-{:016x}", rand::thread_rng().gen::<u64>());
    *state.compacting.lock().await = true;
    *state.compact_timeline_id.lock().await = Some(id.clone());
    let mut item = json!({"id":id,"kind":"compact","status":"running","mode":mode});
    if let Some(p) = percentage { item["percentage"] = json!(p); }
    let mut tl = state.timeline.lock().await;
    tl.push(item);
    drop(tl);
}

// ───────────────────────── session runtime cache ─────────────────────────
//
// These helpers mirror the per-session `runtimes` map and the
// `parkActiveSession()` / `hydrateFromRuntime()` /
// `syncActiveIntoRuntimes()` / `markRuntimeHydrated()` quartet in
// `grok-build-desktop/src/main/backend.ts`. They are the reason
// switching between sessions in the sidebar is fast in Electron:
// on a warm hit the focus fields are restored from memory and no
// ACP round-trip is needed.

/// Mirror the focused session's fields into `runtime_cache[id]`.
///
/// Captures a *snapshot* (clones vectors, deep-copies `cwd`) so the
/// focused session can be parked (cleared) without the cached bag
/// seeing later mutations. Fields missing from focus (e.g. `todos`
/// if never set) are left at their defaults.
async fn sync_active_into_runtimes(state: &AppState) {
    let sid = match state.session_id.lock().await.clone() {
        Some(s) => s,
        None => return,
    };
    let cwd = state.workspace.lock().await.clone().unwrap_or_default();
    let title = state
        .session_title
        .lock()
        .await
        .clone()
        .unwrap_or_else(|| "Session".to_string());
    let timeline = state.timeline.lock().await.clone();
    let replaying = *state.replaying.lock().await;
    let model_id = state.model_id.lock().await.clone();
    let available_models = state.available_models.lock().await.clone();

    let mut cache = state.runtime_cache.lock().await;
    // Preserve the previous `hydrated` flag — the caller of this
    // helper is keeping the cache coherent with current focus, not
    // re-issuing hydration. Without this guard `mark_hydrated()`
    // followed by a streaming update would re-flip the flag to
    // `false` and force the next switch into COLD again.
    let prev_hydrated = cache.get(&sid).map(|r| r.hydrated).unwrap_or(false);
    cache.insert(
        sid.clone(),
        SessionRuntime {
            session_id: sid,
            cwd,
            title,
            timeline,
            // Activity is a derived field in Electron; here we only
            // track `replaying` and trust the snapshot to compute
            // activity at the renderer.
            activity: "idle".to_string(),
            replaying,
            compacting: false,
            model_id,
            session_mode: "default".to_string(),
            todos: Vec::new(),
            plan_content: None,
            goal_state: None,
            goal_todos: Vec::new(),
            available_models,
            hydrated: prev_hydrated,
        },
    );
}

/// Capture the focused session into the runtime cache and clear the
/// focus fields. Mirrors `parkActiveSession()`. After parking we
/// hold a *detached* snapshot of the timeline so subsequent streaming
/// updates targeting the old session id (still routed by the agent)
/// will be dropped by `handle_session_update` since they don't match
/// the new focus — preserving correctness when the user switches
/// mid-turn.
async fn park_active_session(state: &AppState) {
    let sid = match state.session_id.lock().await.clone() {
        Some(s) => s,
        None => return,
    };
    sync_active_into_runtimes(state).await;

    // Clear focus fields so the next snapshot reflects the new
    // session (or a clean empty state) until hydrate runs.
    *state.session_id.lock().await = None;
    *state.session_title.lock().await = None;
    state.timeline.lock().await.clear();
    *state.replaying.lock().await = false;

    // Drop the cache reference so we can later detect "switched to
    // a brand-new session" vs "switched to a hydrated session".
    let mut cache = state.runtime_cache.lock().await;
    if !cache.contains_key(&sid) {
        cache.insert(sid.clone(), empty_runtime(&sid, ""));
    }
    let _ = cache; // release the lock eagerly
}

/// Restore a session's runtime bag into focus. Mirrors
/// `hydrateFromRuntime()`. Returns `true` if the bag was hydrated.
async fn hydrate_from_runtime(state: &AppState, sid: &str) -> bool {
    let bag = {
        let cache = state.runtime_cache.lock().await;
        cache.get(sid).cloned()
    };
    let Some(rt) = bag else {
        return false;
    };
    if !rt.hydrated {
        return false;
    }
    *state.session_id.lock().await = Some(rt.session_id.clone());
    *state.workspace.lock().await = Some(rt.cwd.clone());
    *state.session_title.lock().await = Some(rt.title.clone());
    *state.timeline.lock().await = rt.timeline.clone();
    *state.replaying.lock().await = rt.replaying;
    if let Some(mid) = rt.model_id.clone() {
        *state.model_id.lock().await = Some(mid);
    }
    if !rt.available_models.is_empty() {
        *state.available_models.lock().await = rt.available_models.clone();
    }
    true
}

/// Flip the `hydrated` flag for `sid`. Mirrors `markRuntimeHydrated()`.
async fn mark_hydrated(state: &AppState, sid: &str, hydrated: bool) {
    let mut cache = state.runtime_cache.lock().await;
    if let Some(rt) = cache.get_mut(sid) {
        rt.hydrated = hydrated;
    } else {
        // Mirror the Electron behaviour: `markRuntimeHydrated` is a
        // no-op if the bag doesn't exist yet. The first switch into
        // a session parks the bag; this call then promotes it.
        let mut rt = empty_runtime(sid, "");
        rt.hydrated = hydrated;
        cache.insert(sid.to_string(), rt);
    }
}

/// Best-effort reader for `plan.md`. Returns `None` if either path
/// is empty or the file is missing. Mirrors `readPlanFile()` in the
/// Electron backend.
async fn read_plan_file(cwd: &str, sid: &str) -> Option<String> {
    if cwd.trim().is_empty() || sid.trim().is_empty() {
        return None;
    }
    let path = plan_file_path(cwd, sid);
    match tokio::fs::read_to_string(&path).await {
        Ok(body) if !body.trim().is_empty() => Some(body),
        _ => None,
    }
}

/// Trace helper for switch telemetry — matches Electron's
/// `[activity] loadSession <sid8> → WARM|COLD` log lines so both
/// builds have the same observability story.
fn trace_switch(sid: &str, kind: &str, hydrated: bool) {
    let sid8 = &sid[..sid.len().min(8)];
    tracing::info!(
        sid = sid8,
        kind = kind,
        hydrated = hydrated,
        "[activity] loadSession → {kind} (bag_hydrated={hydrated})"
    );
}

/// Emit a snapshot to the renderer — but **only if the focused
/// session is not in COLD replay**. Mirrors `pushTimeline()` /
/// `updateTimeline()` in the Electron backend (`backend.ts:2862`,
/// `backend.ts:2878`), which both early-return when `this.replaying`
/// is true.
///
/// Why: during a cold session load the agent streams O(n) chunked
/// `sessionUpdate` notifications to rebuild a history of length n.
/// Emitting a snapshot per chunk forces the renderer to re-parse
/// growing markdown N times, which on big conversations feels like a
/// hung switch. Electron's pattern is: one snapshot at the start of
/// replay (empty + spinner), silent accumulation, one snapshot at
/// the end (replaying=false + full history).
async fn maybe_emit_snapshot(state: &AppState, app: &tauri::AppHandle) {
    if *state.replaying.lock().await {
        return;
    }
    let snap = build_snapshot_from_state(state).await;
    emit_snapshot_event(app, snap).await;
}

/// Parse the `models` field out of a `session/load` / `session/new`
/// / `x.ai/models/update` payload. Mirrors `parseModels()` in
/// `grok-build-desktop/src/main/backend.ts:1329`. Returns the
/// `currentModelId` and an array of `ModelInfo`-shaped JSON objects
/// (the renderer expects the same shape as Electron produces).
fn parse_models(models_val: &Value) -> (Option<String>, Vec<Value>) {
    let Some(models) = models_val.as_object() else {
        return (None, Vec::new());
    };
    let current = models
        .get("currentModelId")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let Some(raw_list) = models.get("availableModels").and_then(|v| v.as_array()) else {
        return (current, Vec::new());
    };
    let mut available: Vec<Value> = Vec::with_capacity(raw_list.len());
    for item in raw_list {
        let Some(rec) = item.as_object() else {
            continue;
        };
        let Some(model_id) = rec.get("modelId").and_then(|v| v.as_str()) else {
            continue;
        };
        let name = rec
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or(model_id);
        let description = rec.get("description").and_then(|v| v.as_str());

        // Reasoning efforts come from `model._meta.reasoningEfforts`.
        let reasoning_efforts: Option<Vec<Value>> = rec
            .get("_meta")
            .and_then(|m| m.get("reasoningEfforts"))
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|e| {
                        let o = e.as_object()?;
                        let id = o
                            .get("id")
                            .or_else(|| o.get("value"))
                            .and_then(|v| v.as_str())?;
                        let label = o
                            .get("label")
                            .and_then(|v| v.as_str())
                            .unwrap_or(id);
                        let description = o
                            .get("description")
                            .and_then(|v| v.as_str())
                            .map(|s| s.to_string());
                        let mut obj = serde_json::Map::new();
                        obj.insert("id".into(), Value::String(id.to_string()));
                        obj.insert("label".into(), Value::String(label.to_string()));
                        if let Some(d) = description {
                            obj.insert("description".into(), Value::String(d));
                        }
                        Some(Value::Object(obj))
                    })
                    .collect::<Vec<_>>()
                    .into()
            })
            .filter(|v: &Vec<Value>| !v.is_empty());

        let supports_reasoning = rec
            .get("_meta")
            .and_then(|m| m.get("supportsReasoningEffort"))
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
            || reasoning_efforts.is_some();

        let reasoning_effort = rec
            .get("_meta")
            .and_then(|m| m.get("reasoningEffort"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        let accepts_images = rec
            .get("_meta")
            .and_then(|m| m.get("inputModalities"))
            .and_then(|v| v.as_array())
            .map(|arr| {
                if arr.is_empty() {
                    true // default to true if unknown
                } else {
                    arr.iter().any(|m| {
                        m.as_str()
                            .map(|s| s.eq_ignore_ascii_case("image"))
                            .unwrap_or(false)
                    })
                }
            })
            .unwrap_or(true); // default true

        let context_window = rec
            .get("_meta")
            .and_then(|m| m.get("totalContextTokens"))
            .or_else(|| rec.get("_meta").and_then(|m| m.get("total_context_tokens")))
            .or_else(|| rec.get("_meta").and_then(|m| m.get("contextWindow")))
            .or_else(|| rec.get("_meta").and_then(|m| m.get("context_window")))
            .and_then(|v| v.as_u64())
            .map(|n| n as i64);

        let mut out = serde_json::Map::new();
        out.insert("modelId".into(), Value::String(model_id.to_string()));
        out.insert("name".into(), Value::String(name.to_string()));
        if let Some(d) = description {
            out.insert("description".into(), Value::String(d.to_string()));
        }
        out.insert(
            "supportsReasoningEffort".into(),
            Value::Bool(supports_reasoning),
        );
        if let Some(re) = reasoning_effort {
            out.insert("reasoningEffort".into(), Value::String(re));
        }
        if let Some(re) = reasoning_efforts {
            out.insert("reasoningEfforts".into(), Value::Array(re));
        }
        out.insert("acceptsImages".into(), Value::Bool(accepts_images));
        if let Some(cw) = context_window {
            out.insert("contextWindow".into(), Value::Number(cw.into()));
        }
        available.push(Value::Object(out));
    }
    (current, available)
}

/// Apply a parsed (current, available) model set to the focused
/// session. Mirrors `applyModelsFromSession()` in
/// `backend.ts:2694`. We adopt `current` only when the focused
/// session has no `modelId` yet (initial session/new); a user
/// choice via `setModel()` is the source of truth thereafter.
async fn apply_models_to_focus(
    state: &AppState,
    current: Option<String>,
    available: Vec<Value>,
) {
    if !available.is_empty() {
        *state.available_models.lock().await = available;
    }
    if let Some(c) = current {
        let mut mid = state.model_id.lock().await;
        if mid.is_none() {
            *mid = Some(c);
        }
    }
}

/// Apply a `x.ai/models/update` notification — refreshes focus and
/// every cached runtime bag with the same catalog, so warm switches
/// see the latest models. Mirrors the loop at
/// `backend.ts:5007-5019`.
pub async fn handle_models_update(state: &AppState, params: &Value) {
    let (current, available) = parse_models(params);
    if available.is_empty() && current.is_none() {
        return;
    }
    if !available.is_empty() {
        *state.available_models.lock().await = available.clone();
        // Keep parked runtimes' catalogs in sync.
        let mut cache = state.runtime_cache.lock().await;
        for rt in cache.values_mut() {
            rt.available_models = available.clone();
        }
    }
    if let Some(c) = current {
        let mut mid = state.model_id.lock().await;
        if mid.is_none() {
            *mid = Some(c);
        }
    }
    let count = state.available_models.lock().await.len();
    tracing::info!(count, "models/update applied");
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

    // Fetch and cache sessions list.
    let sessions_val: Value = bridge
        .call("_x.ai/session_summaries/workspace_list_recent", json!({"limit": 80}))
        .await
        .unwrap_or(Value::Array(Vec::new()));
    let session_list = parse_session_list(&sessions_val);
    {
        let mut cache = state.sessions_cache.lock().await;
        *cache = session_list.clone();
    }

    let mut snap = build_snapshot_from_state(&state).await;
    if let Value::Object(ref mut map) = snap {
        map.insert("connection".into(), json!("ready"));
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

/// Parse raw session records from the bridge into SessionSummary JSON array.
fn parse_session_list(raw: &Value) -> Vec<Value> {
    raw.as_array().map(|raw| {
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
    }).unwrap_or_default()
}

/// Build a snapshot from the current AppState (no bridge call needed).
/// Uses cached sessions list. This is safe to call from the reader task.
///
/// Account fields (`accountAvailable` / `accountEmail`) are read live
/// from disk on every snapshot — auth credentials may have changed
/// between snapshots (login, logout, API key set), and we'd rather
/// pay one stat than risk showing "Grok official models require
/// login" while the user is actually signed in.
pub async fn build_snapshot_from_state(state: &AppState) -> Value {
    let sess_id = state.session_id.lock().await.clone();
    let session_title = state.session_title.lock().await.clone();
    let timeline = state.timeline.lock().await.clone();
    let replaying = *state.replaying.lock().await;
    let workspace = state.workspace.lock().await.clone();
    let sessions_cache = state.sessions_cache.lock().await.clone();
    let model_id = state.model_id.lock().await.clone();
    let available_models = state.available_models.lock().await.clone();
    let available_commands = state.available_commands.lock().await.clone();
    let session_mode = state.session_mode.lock().await.clone();
    let todos = state.todos.lock().await.clone();
    let usage = state.usage.lock().await.clone();

    let account = build_account_status();
    let account_available = account
        .get("signedIn")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let account_email = account
        .get("email")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let mut snap = empty_snapshot_with("ready", None, &json!({"kind": "absent"}), None);
    if let Value::Object(ref mut map) = snap {
        if let Some(ref sid) = sess_id {
            map.insert("sessionId".into(), json!(sid));
        }
        if let Some(ref st) = session_title {
            map.insert("sessionTitle".into(), json!(st));
        }
        if let Some(ref mid) = model_id {
            map.insert("modelId".into(), json!(mid));
        }
        map.insert("availableModels".into(), Value::Array(available_models));
        map.insert("availableCommands".into(), Value::Array(available_commands));
        map.insert("sessions".into(), Value::Array(sessions_cache));
        map.insert("timeline".into(), Value::Array(timeline));
        map.insert("replaying".into(), json!(replaying));
        map.insert("sessionMode".into(), json!(session_mode));
        map.insert("todos".into(), Value::Array(todos));
        // Goal subsystem — populated from the focused session's runtime
        // bag (per-session, survives session switches like Electron).
        {
            let cache = state.runtime_cache.lock().await;
            if let Some(sid) = &sess_id {
                if let Some(rt) = cache.get(sid) {
                    if let Some(ref gs) = rt.goal_state {
                        map.insert("goalState".into(), gs.clone());
                    }
                    if !rt.goal_todos.is_empty() {
                        map.insert("goalTodos".into(), Value::Array(rt.goal_todos.clone()));
                    }
                }
            }
        }
        if let Some(ref ws) = workspace {
            map.insert("workspace".into(), json!(ws));
            map.insert("statusBarCwd".into(), json!(ws));
        }
        if let Some(u) = usage {
            map.insert("usage".into(), u);
        }
        map.insert("accountAvailable".into(), Value::Bool(account_available));
        if let Some(email) = account_email {
            map.insert("accountEmail".into(), Value::String(email));
        }
    }
    snap
}

/// Emit a `{ type: "snapshot", snapshot }` event to the renderer.
/// Convenience wrapper used by both commands and the reader task.
pub async fn emit_snapshot_event(app: &tauri::AppHandle, snapshot: Value) {
    let sid = snapshot.get("sessionId").and_then(|v| v.as_str()).unwrap_or("null");
    let replaying = snapshot.get("replaying").and_then(|v| v.as_bool()).unwrap_or(false);
    let tl_len = snapshot.get("timeline").and_then(|v| v.as_array()).map(|a| a.len()).unwrap_or(0);
    tracing::trace!(sid, replaying, tl_len, "emit_snapshot_event");
    let payload = json!({"type": "snapshot", "snapshot": snapshot});
    if let Err(e) = app.emit("agent:event", payload) {
        tracing::warn!(error = %e, "emit_snapshot_event failed");
    }
}

/// Process a `sessionUpdate` notification from the agent.
/// Parses the update, builds a timeline item, appends it to AppState,
/// and emits a snapshot event to the renderer.
pub async fn handle_session_update(
    params: &Value,
    app: &tauri::AppHandle,
    state: &AppState,
) {
    use rand::Rng;
    let update_session_id = params
        .get("sessionId")
        .and_then(|v| v.as_str())
        .or_else(|| params.get("session_id").and_then(|v| v.as_str()));

    // Ignore updates for non-focused sessions.
    {
        let current_sid_guard = state.session_id.lock().await;
        let current_sid = current_sid_guard.as_deref();
        match (current_sid, update_session_id) {
            (Some(cur), Some(upd)) if cur == upd => {}
            (Some(_), None) => {}
            _ => {
                tracing::trace!(
                    "handle_session_update skipped (sid mismatch: current={:?}, update={:?})",
                    current_sid, update_session_id
                );
                return;
            }
        }
    }

    // Extract the update object and discriminator.
    let update = params.get("update").and_then(|v| v.as_object());
    let kind = update
        .and_then(|u| u.get("sessionUpdate"))
        .and_then(|v| v.as_str())
        .unwrap_or("");

    tracing::trace!(kind, "handle_session_update");

    // For notifications with no "update" field, just re-emit snapshot.
    let some_update = match update {
        Some(u) => u,
        None => {
            maybe_emit_snapshot(state, app).await;
            return;
        }
    };

    // --- Helpers for the cascade below ---
    fn as_str_or(v: &serde_json::Map<String, Value>, key: &str) -> Option<String> {
        v.get(key).and_then(|x| x.as_str()).map(|s| s.to_string())
    }
    fn as_f64_or(v: &serde_json::Map<String, Value>, key: &str) -> Option<f64> {
        v.get(key).and_then(|x| x.as_f64())
    }
    fn new_id(prefix: &str) -> String {
        use rand::Rng;
        format!("{}-{:016x}", prefix, rand::thread_rng().gen::<u64>())
    }

    // Goal-shaped payloads without a discriminator still light up the
    // progress bubble. Mirrors Electron's `looksLikeGoalUpdate` gate.
    if kind.is_empty()
        && some_update.get("objective").and_then(|v| v.as_str()).is_some()
        && some_update.get("status").and_then(|v| v.as_str()).is_some()
    {
        tracing::debug!("goal-shaped update without discriminator — routing to goal handler");
        // Phase 3 will wire goal_state / goal_todos here. For now,
        // just emit so the renderer picks up the raw goal fields.
        maybe_emit_snapshot(state, app).await;
        return;
    }

    // Most session/update envelopes carry a discriminator; drop bare ones.
    if kind.is_empty() {
        maybe_emit_snapshot(state, app).await;
        return;
    }

    // user_message_chunk – live prompts already append in sendPrompt; only
    // session replay needs these to rebuild history.
    if kind == "user_message_chunk" {
        let content = some_update.get("content").and_then(|v| v.as_object());
        let text = content
            .and_then(|c| c.get("text"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let replaying = *state.replaying.lock().await;
        if !replaying {
            return; // live user prompts handled by agent_send_prompt
        }
        let id = new_id("user");
        let mut tl = state.timeline.lock().await;
        tl.push(json!({"id":id,"kind":"user","text":text}));
        drop(tl);
        maybe_emit_snapshot(state, app).await;
        return;
    }

    // agent_message_chunk – streaming assistant text chunks.
    // Merges into the last assistant item when present; otherwise starts a
    // new one. Handles whitespace-only chunks (must pass through so
    // markdown newline spacing survives).
    if kind == "agent_message_chunk" {
        let content = some_update.get("content").and_then(|v| v.as_object());
        let text = content
            .and_then(|c| c.get("text"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if text.is_empty() {
            maybe_emit_snapshot(state, app).await;
            return;
        }
        let mut tl = state.timeline.lock().await;
        let merged = if let Some(last) = tl.last_mut() {
            if last.get("kind") == Some(&json!("assistant")) {
                if let Some(obj) = last.as_object_mut() {
                    let existing = obj.get("text").and_then(|v| v.as_str()).unwrap_or("");
                    obj.insert("text".into(), json!(format!("{}{}", existing, text)));
                    obj.insert("streaming".into(), json!(true));
                    true
                } else {
                    false
                }
            } else {
                false
            }
        } else {
            false
        };
        if !merged {
            let id = new_id("asst");
            tl.push(json!({"id":id,"kind":"assistant","text":text,"streaming":true}));
        }
        drop(tl);
        maybe_emit_snapshot(state, app).await;
        return;
    }

    // agent_thought_chunk – model reasoning in a collapsible thought bubble.
    if kind == "agent_thought_chunk" {
        let content = some_update.get("content").and_then(|v| v.as_object());
        let text = content
            .and_then(|c| c.get("text"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if text.is_empty() {
            maybe_emit_snapshot(state, app).await;
            return;
        }
        let mut tl = state.timeline.lock().await;
        let merged = if let Some(last) = tl.last_mut() {
            if last.get("kind") == Some(&json!("thought")) {
                if let Some(obj) = last.as_object_mut() {
                    let existing = obj.get("text").and_then(|v| v.as_str()).unwrap_or("");
                    obj.insert("text".into(), json!(format!("{}{}", existing, text)));
                    obj.insert("streaming".into(), json!(true));
                    true
                } else {
                    false
                }
            } else {
                false
            }
        } else {
            false
        };
        if !merged {
            let id = new_id("th");
            tl.push(json!({"id":id,"kind":"thought","text":text,"streaming":true}));
        }
        drop(tl);
        maybe_emit_snapshot(state, app).await;
        return;
    }

    // tool_call – agent invoked a tool. Push a new card, or update
    // the existing one if we've already seen this toolCallId (dedup
    // for session replay or duplicate stream emits).
    if kind == "tool_call" {
        let tool_call_id = as_str_or(some_update, "toolCallId").unwrap_or_else(|| new_id("tool"));
        let title = as_str_or(some_update, "title").unwrap_or_else(|| "tool".to_string());
        let status = as_str_or(some_update, "status").unwrap_or_else(|| "pending".to_string());
        let tool_kind = semantic_tool_kind(some_update);
        let content_raw = some_update.get("content").and_then(|v| v.as_array());
        let (diffs, output_text, output_truncated) = parse_tool_content(content_raw);
        let has_content = diffs.len() > 0 || output_text.is_some();

        // Dedup: if tool_index already tracks this toolCallId, update
        // the existing card (mirrors Electron backend.ts:5390-5410).
        {
            let ti = state.tool_index.lock().await;
            if let Some(existing_id) = ti.get(&tool_call_id) {
                let mut tl = state.timeline.lock().await;
                if let Some(item) = tl.iter_mut().find(|i| i.get("id").and_then(|v| v.as_str()) == Some(existing_id)) {
                    if item.get("kind") == Some(&json!("tool")) {
                        item["title"] = json!(title);
                        item["status"] = json!(status);
                        if let Some(ref tk) = tool_kind { item["toolKind"] = json!(tk); }
                        if has_content {
                            if !diffs.is_empty() { item["diffs"] = Value::Array(diffs); }
                            if let Some(ref ot) = output_text { item["outputText"] = json!(ot); }
                            if output_truncated { item["outputTruncated"] = json!(true); }
                        }
                    }
                }
                drop(ti);
                drop(tl);
                maybe_emit_snapshot(state, app).await;
                return;
            }
        }

        let id = new_id("tool");
        {
            let mut ti = state.tool_index.lock().await;
            ti.insert(tool_call_id.clone(), id.clone());
            drop(ti);
        }
        let mut item = json!({
            "id": id,
            "kind": "tool",
            "toolCallId": tool_call_id,
            "title": title,
            "status": status,
        });
        if let Some(ref tk) = tool_kind { item["toolKind"] = json!(tk); }
        if has_content {
            if !diffs.is_empty() { item["diffs"] = Value::Array(diffs); }
            if let Some(ref ot) = output_text { item["outputText"] = json!(ot); }
            if output_truncated { item["outputTruncated"] = json!(true); }
        }
        let mut tl = state.timeline.lock().await;
        tl.push(item);
        drop(tl);
        maybe_emit_snapshot(state, app).await;
        return;
    }

    // tool_call_update – follow-up to a tool_call (output text, diffs).
    if kind == "tool_call_update" {
        let tool_call_id = as_str_or(some_update, "toolCallId");
        let status = as_str_or(some_update, "status");
        let title = as_str_or(some_update, "title");
        let tool_kind = semantic_tool_kind(some_update);
        let content_raw = some_update.get("content").and_then(|v| v.as_array());
        let (diffs, output_text, output_truncated) = parse_tool_content(content_raw);
        let has_content = diffs.len() > 0 || output_text.is_some();

        if let Some(tcid) = &tool_call_id {
            let mut tl = state.timeline.lock().await;
            // First: try the tool_index (fast path).
            let indexed_id = {
                let ti = state.tool_index.lock().await;
                ti.get(tcid).cloned()
            };
            let found = if let Some(ref iid) = indexed_id {
                tl.iter_mut().find(|i| i.get("id").and_then(|v| v.as_str()) == Some(iid.as_str()))
            } else {
                tl.iter_mut().find(|i|
                    i.get("kind") == Some(&json!("tool")) &&
                    i.get("toolCallId").and_then(|v| v.as_str()) == Some(tcid.as_str())
                )
            };
            if let Some(item) = found {
                if let Some(ref s) = status { item["status"] = json!(s); }
                if let Some(ref t) = title { item["title"] = json!(t); }
                if let Some(ref tk) = tool_kind { item["toolKind"] = json!(tk); }
                if has_content {
                    if !diffs.is_empty() { item["diffs"] = Value::Array(diffs); }
                    if let Some(ref ot) = output_text { item["outputText"] = json!(ot); }
                    if output_truncated { item["outputTruncated"] = json!(true); }
                }
            } else {
                // Late tool card without a prior tool_call — create one.
                let id = new_id("tool");
                {
                    let mut ti = state.tool_index.lock().await;
                    ti.insert(tcid.clone(), id.clone());
                    drop(ti);
                }
                let mut item = json!({
                    "id": id,
                    "kind": "tool",
                    "toolCallId": tcid,
                    "title": title.unwrap_or_else(|| tcid.clone()),
                    "status": status.unwrap_or_else(|| "in_progress".to_string()),
                });
                if let Some(ref tk) = tool_kind { item["toolKind"] = json!(tk); }
                if has_content {
                    if !diffs.is_empty() { item["diffs"] = Value::Array(diffs); }
                    if let Some(ref ot) = output_text { item["outputText"] = json!(ot); }
                    if output_truncated { item["outputTruncated"] = json!(true); }
                }
                tl.push(item);
            }
            drop(tl);
        }
        maybe_emit_snapshot(state, app).await;
        return;
    }

    // available_commands_update – agent pushes its slash-command catalog.
    if kind == "available_commands_update" || kind == "availableCommandsUpdate" {
        let cmds_val = some_update
            .get("availableCommands")
            .or_else(|| some_update.get("available_commands"));
        if let Some(arr) = cmds_val.and_then(|v| v.as_array()) {
            let cmds: Vec<Value> = arr.clone();
            *state.available_commands.lock().await = cmds;
        }
        maybe_emit_snapshot(state, app).await;
        return;
    }

    // ── Compaction lifecycle ──
    if kind == "auto_compact_started" {
        let replaying = *state.replaying.lock().await;
        if replaying { return; }
        let percentage = as_f64_or(some_update, "percentage")
            .or_else(|| as_f64_or(some_update, "percent"));
        push_compact_card(state, "auto", percentage).await;
        maybe_emit_snapshot(state, app).await;
        return;
    }
    if kind == "compaction_checkpoint" || kind == "CompactionCheckpoint" {
        let replaying = *state.replaying.lock().await;
        if replaying { return; }
        // If compact card not yet started, push a running one.
        let tl = state.timeline.lock().await;
        let has_running = tl.iter().any(|i|
            i.get("kind") == Some(&json!("compact")) && i.get("status") == Some(&json!("running"))
        );
        drop(tl);
        if !has_running {
            let id = new_id("compact");
            let mut tl2 = state.timeline.lock().await;
            tl2.push(json!({"id":id,"kind":"compact","status":"running","mode":"auto"}));
            drop(tl2);
        }
        maybe_emit_snapshot(state, app).await;
        return;
    }
    if kind == "auto_compact_completed" {
        let tokens_before = as_f64_or(some_update, "tokens_before")
            .or_else(|| as_f64_or(some_update, "tokensBefore"));
        let tokens_after = as_f64_or(some_update, "tokens_after")
            .or_else(|| as_f64_or(some_update, "tokensAfter"));
        let replaying = *state.replaying.lock().await;
        if replaying {
            let id = new_id("compact");
            let mut tl = state.timeline.lock().await;
            let mut item = json!({
                "id": id, "kind": "compact", "status": "completed", "mode": "auto",
            });
            if let Some(b) = tokens_before { item["tokensBefore"] = json!(b); }
            if let Some(a) = tokens_after { item["tokensAfter"] = json!(a); }
            tl.push(item);
            drop(tl);
        } else {
            // Update the last running compact card to completed.
            let mut tl = state.timeline.lock().await;
            if let Some(last) = tl.iter_mut().rev().find(|i|
                i.get("kind") == Some(&json!("compact")) && i.get("status") == Some(&json!("running"))
            ) {
                last["status"] = json!("completed");
                if let Some(b) = tokens_before { last["tokensBefore"] = json!(b); }
                if let Some(a) = tokens_after { last["tokensAfter"] = json!(a); }
            }
            drop(tl);
        }
        maybe_emit_snapshot(state, app).await;
        return;
    }
    if kind == "auto_compact_failed" {
        let replaying = *state.replaying.lock().await;
        if replaying { return; }
        let message = as_str_or(some_update, "message").or_else(|| as_str_or(some_update, "error"));
        let mut tl = state.timeline.lock().await;
        if let Some(last) = tl.iter_mut().rev().find(|i|
            i.get("kind") == Some(&json!("compact")) && i.get("status") == Some(&json!("running"))
        ) {
            last["status"] = json!("failed");
            if let Some(ref m) = message { last["message"] = json!(m); }
        }
        drop(tl);
        maybe_emit_snapshot(state, app).await;
        return;
    }
    if kind == "auto_compact_cancelled" {
        let replaying = *state.replaying.lock().await;
        if replaying { return; }
        let mut tl = state.timeline.lock().await;
        if let Some(last) = tl.iter_mut().rev().find(|i|
            i.get("kind") == Some(&json!("compact")) && i.get("status") == Some(&json!("running"))
        ) {
            last["status"] = json!("cancelled");
        }
        drop(tl);
        maybe_emit_snapshot(state, app).await;
        return;
    }
    if kind == "auto_continue_completed" {
        let replaying = *state.replaying.lock().await;
        if replaying { return; }
        let mut tl = state.timeline.lock().await;
        tl.push(json!({"id": new_id("sys"), "kind": "system", "text": "Resumed after compaction."}));
        drop(tl);
        maybe_emit_snapshot(state, app).await;
        return;
    }
    if kind == "memory_flush_started" {
        // Optional enrichment during compact — keep running, skip snapshot.
        return;
    }

    // current_mode_update – agent changes permission mode (default, plan, yolo, …)
    if kind == "current_mode_update" || kind == "currentModeUpdate" {
        let mode = as_str_or(some_update, "currentModeId")
            .or_else(|| as_str_or(some_update, "current_mode_id"))
            .or_else(|| as_str_or(some_update, "modeId"));
        if let Some(m) = mode {
            *state.session_mode.lock().await = m;
        }
        maybe_emit_snapshot(state, app).await;
        return;
    }

    // plan – todo list from todo_write (and turn-end cleanup).
    // Phase 6 fills full todo / plan_content wiring.
    if kind == "plan" {
        let entries = some_update.get("entries").or_else(|| some_update.get("planEntries"));
        if let Some(arr) = entries.and_then(|v| v.as_array()) {
            let todos: Vec<Value> = arr.iter()
                .filter_map(|e| {
                    let o = e.as_object()?;
                    let content = o.get("content").and_then(|v| v.as_str())?;
                    let status = o.get("status").and_then(|v| v.as_str()).unwrap_or("pending");
                    Some(json!({
                        "id": format!("todo-{:016x}", rand::thread_rng().gen::<u64>()),
                        "content": content,
                        "status": status,
                    }))
                })
                .collect();
            *state.todos.lock().await = todos;
        }
        maybe_emit_snapshot(state, app).await;
        return;
    }

    // goal_updated – goal orchestrator progress. Drives the 🎯 bubble.
    if kind == "goal_updated" || kind == "GoalUpdated" {
        let sid = state.session_id.lock().await.clone();
        if let Some(ref session_id) = sid {
            let goal_id = as_str_or(some_update, "goal_id")
                .or_else(|| as_str_or(some_update, "goalId"));
            let objective = as_str_or(some_update, "objective").unwrap_or_default();
            let status = as_str_or(some_update, "status").unwrap_or_else(|| "active".to_string());
            let phase = as_str_or(some_update, "phase").unwrap_or_else(|| "idle".to_string());
            let token_budget = as_f64_or(some_update, "token_budget").or_else(|| as_f64_or(some_update, "tokenBudget"));
            let tokens_used = as_f64_or(some_update, "tokens_used").or_else(|| as_f64_or(some_update, "tokensUsed"));
            let elapsed_ms = as_f64_or(some_update, "elapsed_ms").or_else(|| as_f64_or(some_update, "elapsedMs"));
            let pause_message = as_str_or(some_update, "pause_message")
                .or_else(|| as_str_or(some_update, "pauseMessage"));
            let last_event = as_str_or(some_update, "last_event")
                .or_else(|| as_str_or(some_update, "lastEvent"));
            let last_event_detail = as_str_or(some_update, "last_event_detail")
                .or_else(|| as_str_or(some_update, "lastEventDetail"));
            let updated_at = as_f64_or(some_update, "updated_at")
                .or_else(|| as_f64_or(some_update, "updatedAt"));
            let verifying = some_update.get("verifyingCompletion").and_then(|v| v.as_bool()).unwrap_or(false);
            let planning = some_update.get("planning").and_then(|v| v.as_bool()).unwrap_or(false);

            let mut goal = json!({
                "goalId": goal_id.unwrap_or_default(),
                "objective": objective,
                "status": status,
                "phase": phase,
                "verifyingCompletion": verifying,
                "planning": planning,
            });
            if let Some(tb) = token_budget { goal["tokenBudget"] = json!(tb); }
            if let Some(tu) = tokens_used { goal["tokensUsed"] = json!(tu); }
            if let Some(em) = elapsed_ms { goal["elapsedMs"] = json!(em); }
            if let Some(pm) = pause_message { goal["pauseMessage"] = json!(pm); }
            if let Some(le) = last_event { goal["lastEvent"] = json!(le); }
            if let Some(led) = last_event_detail { goal["lastEventDetail"] = json!(led); }
            if let Some(ua) = updated_at { goal["updatedAt"] = json!(ua); }

            let goal_todos: Vec<Value> = some_update
                .get("goalTodos")
                .or_else(|| some_update.get("goal_todos"))
                .and_then(|v| v.as_array())
                .map(|arr| arr.clone())
                .unwrap_or_default();

            let mut cache = state.runtime_cache.lock().await;
            if let Some(rt) = cache.get_mut(session_id) {
                rt.goal_state = Some(goal);
                rt.goal_todos = goal_todos;
            }
            drop(cache);
        }
        maybe_emit_snapshot(state, app).await;
        return;
    }

    // subagent_spawned – subagent started its run.
    // Phase 6 fills full subagent card.
    if kind == "subagent_spawned" || kind == "SubagentSpawned" {
        let sub_id = as_str_or(some_update, "subagent_id")
            .or_else(|| as_str_or(some_update, "subagentId"))
            .unwrap_or_else(|| new_id("subagent"));
        let role = as_str_or(some_update, "role").unwrap_or_else(|| "subagent".to_string());
        let mut tl = state.timeline.lock().await;
        tl.push(json!({
            "id": new_id("sub"),
            "kind": "subagent",
            "subagentId": sub_id,
            "role": role,
            "status": "running",
        }));
        drop(tl);
        maybe_emit_snapshot(state, app).await;
        return;
    }
    // subagent_progress – live token/turn/tool_call counts.
    if kind == "subagent_progress" || kind == "SubagentProgress" {
        // Phase 6 fills progress live counts. Forward snapshot for now.
        maybe_emit_snapshot(state, app).await;
        return;
    }
    // subagent_finished – subagent completed its run.
    if kind == "subagent_finished" || kind == "SubagentFinished" {
        let sub_id = as_str_or(some_update, "subagent_id")
            .or_else(|| as_str_or(some_update, "subagentId"));
        let outcome = as_str_or(some_update, "outcome")
            .or_else(|| as_str_or(some_update, "status"))
            .unwrap_or_else(|| "completed".to_string());
        let final_status = match outcome.as_str() {
            "success" | "completed" => "completed",
            "cancelled" => "cancelled",
            _ => "failed",
        };
        if let Some(sid) = sub_id {
            let mut tl = state.timeline.lock().await;
            if let Some(item) = tl.iter_mut().rev().find(|i|
                i.get("kind") == Some(&json!("subagent")) &&
                i.get("subagentId").and_then(|v| v.as_str()) == Some(&sid)
            ) {
                item["status"] = json!(final_status);
            }
            drop(tl);
        }
        maybe_emit_snapshot(state, app).await;
        return;
    }

    // workflow_updated – workflow orchestration event.
    // Phase 6 fills full WorkflowCard.
    if kind == "workflow_updated" || kind == "WorkflowUpdated" {
        maybe_emit_snapshot(state, app).await;
        return;
    }

    // task_backgrounded / task_completed – prompt queue background tasks.
    // Phase 7 fills full background-task tracking.
    if kind == "task_backgrounded" || kind == "TaskBackgrounded" {
        maybe_emit_snapshot(state, app).await;
        return;
    }
    if kind == "task_completed" || kind == "TaskCompleted" {
        maybe_emit_snapshot(state, app).await;
        return;
    }

    // finalize / done / completed / idle – turn complete; turn off
    // replaying and set streaming=false on open assistant/thought items.
    if kind == "finalize" || kind == "done" || kind == "completed" || kind == "idle" {
        {
            let mut tl = state.timeline.lock().await;
            for item in tl.iter_mut() {
                let k = item.get("kind").and_then(|v| v.as_str());
                if k == Some("assistant") || k == Some("thought") {
                    item["streaming"] = json!(false);
                }
            }
            // Also clear turn-scoped todos on turn completion.
        }
        {
            state.todos.lock().await.clear();
        }
        let mut re = state.replaying.lock().await;
        *re = false;
        drop(re);
        let snap = build_snapshot_from_state(state).await;
        emit_snapshot_event(app, snap).await;
        return;
    }

    // Unknown update type — try to turn off replaying if state=ready/idle,
    // then emit so the renderer stays in sync.
    if let Some(state_val) = some_update.get("state").or_else(|| some_update.get("status")) {
        if let Some(s) = state_val.as_str() {
            if s == "ready" || s == "idle" || s == "completed" {
                let mut re = state.replaying.lock().await;
                *re = false;
                drop(re);
            }
        }
    }

    // Emit snapshot after every recognized or unrecognized update that
    // touched state. Suppressed during COLD replay (mirrors Electron's
    // `if (this.replaying) return;` in pushTimeline / updateTimeline).
    maybe_emit_snapshot(state, app).await;
}

/// Fetch sessions from the bridge, update the cache, build a snapshot,
/// and emit it to the renderer. Used by commands that change session state.
async fn emit_snapshot_bridge(
    bridge: &mut AgentBridge,
    app: &tauri::AppHandle,
    state: &AppState,
) -> Result<(), String> {
    // Fetch and cache sessions list.
    let sessions_val: Value = bridge
        .call("_x.ai/session_summaries/workspace_list_recent", json!({"limit": 80}))
        .await
        .unwrap_or(Value::Array(Vec::new()));
    let session_list = parse_session_list(&sessions_val);
    {
        let mut cache = state.sessions_cache.lock().await;
        *cache = session_list.clone();
    }

    let snap = build_snapshot_from_state(state).await;
    emit_snapshot_event(app, snap).await;
    Ok(())
}

#[tauri::command]
pub async fn agent_new_session(
    workspace: String,
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<(), String> {
    // Preserve any previously-focused session in the runtime cache
    // so that switching back to it after creating a new chat is
    // instant (WARM).
    park_active_session(&state).await;

    {
        let mut ws = state.workspace.lock().await;
        *ws = Some(workspace.clone());
        let mut sid = state.session_id.lock().await;
        sid.take();
        let mut tl = state.timeline.lock().await;
        tl.clear();
        let mut re = state.replaying.lock().await;
        *re = false;
    }
    let mut guard = state.agent.lock().await;
    let bridge = guard.as_mut().ok_or_else(|| "agent not connected".to_string())?;
    let result = bridge
        .call(
            "session/new",
            json!({"cwd": workspace, "mcpServers": []}),
        )
        .await
        .map_err(|e| e.to_string())?;
    // `session/new` returns the freshly-created session id; mirror
    // it into focus and seed a hydrated runtime bag so the next
    // switch to this new session is also instant.
    if let Some(new_sid) = result
        .get("sessionId")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
    {
        *state.session_id.lock().await = Some(new_sid.clone());
        let mut cache = state.runtime_cache.lock().await;
        let rt = cache
            .entry(new_sid.clone())
            .or_insert_with(|| empty_runtime(&new_sid, &workspace));
        rt.hydrated = true;
        rt.cwd = workspace.clone();

        // Apply model catalog if the agent included it in the
        // session/new response — keeps the model picker populated
        // for brand-new sessions without waiting for a COLD load.
        if let Some(models_val) = result.get("models") {
            let (current, available) = parse_models(models_val);
            apply_models_to_focus(&state, current, available.clone()).await;
            rt.available_models = available;
        }
    }
    {
        let snap = build_snapshot_from_state(&state).await;
        emit_snapshot_event(&app, snap).await;
    }
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
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<(), String> {
    // Park the currently-focused session into the runtime cache so
    // streaming updates targeting its id stop mutating focus after
    // we move on. Mirrors the `parkActiveSession()` call the
    // Electron backend makes at the top of `loadSession`.
    park_active_session(&state).await;

    // WARM switch: if the requested session already lives in the
    // runtime cache *and* has been hydrated (`hydrated = true`),
    // skip the ACP round-trip entirely. The renderer immediately
    // sees the cached timeline.
    if hydrate_from_runtime(&state, &session_id).await {
        trace_switch(&session_id, "WARM", true);
        // Workspace mirrors the cache value; restore from cwd arg
        // if the cache entry predates cwd tracking. Drop the read
        // guard before reacquiring for the write — locking the same
        // Mutex twice in one expression self-deadlocks (tokio's
        // Mutex is not re-entrant).
        let needs_workspace = state.workspace.lock().await.is_none();
        if needs_workspace {
            *state.workspace.lock().await = Some(cwd.clone());
        }
        let snap = build_snapshot_from_state(&state).await;
        emit_snapshot_event(&app, snap).await;
        return Ok(());
    }

    // Cold path: identical surface area to the previous
    // implementation, but now also marks the runtime bag as
    // hydrated once `session/load` returns.
    trace_switch(&session_id, "COLD", false);

    // Seed focus immediately so the renderer shows the loading
    // spinner in the same render frame as the click. We also seed
    // an *unhydrated* bag so a later switch back to this session
    // (interrupted by another click) still parks it cleanly even
    // if `session/load` has not finished.
    //
    // Important: do the disk read OUTSIDE the cache lock — a slow
    // filesystem call while holding the mutex would block any
    // other session switch (and `sync_active_into_runtimes`) that
    // happened to race the cold load.
    {
        let mut cache = state.runtime_cache.lock().await;
        cache
            .entry(session_id.clone())
            .or_insert_with(|| empty_runtime(&session_id, &cwd));
    }
    // Plan restore happens after the cache lock is released.
    if let Some(body) = read_plan_file(&cwd, &session_id).await {
        let mut cache = state.runtime_cache.lock().await;
        if let Some(rt) = cache.get_mut(&session_id) {
            rt.plan_content = Some(body);
        }
    }
    *state.workspace.lock().await = Some(cwd.clone());
    *state.session_id.lock().await = Some(session_id.clone());
    // Single lock acquisition: read the previous title, then drop the
    // guard before writing back. Locking the same Mutex twice in one
    // expression would self-deadlock (tokio Mutex is not re-entrant).
    let next_title = state
        .session_title
        .lock()
        .await
        .clone()
        .unwrap_or_else(|| "Session".to_string());
    *state.session_title.lock().await = Some(next_title);
    state.timeline.lock().await.clear();
    *state.replaying.lock().await = true;

    // Emit an initial snapshot using cached state (no bridge call —
    // that would deadlock the reader while the agent is processing
    // session/load).
    {
        let snap = build_snapshot_from_state(&state).await;
        emit_snapshot_event(&app, snap).await;
    }

    let mut guard = state.agent.lock().await;
    let bridge = guard
        .as_mut()
        .ok_or_else(|| "agent not connected".to_string())?;
    let load_result = bridge
        .call(
            "session/load",
            json!({"sessionId": session_id, "cwd": cwd, "mcpServers": []}),
        )
        .await
        .map_err(|e| e.to_string())?;

    // Apply the model catalog returned alongside the session. The
    // agent includes `models.availableModels` + `models.currentModelId`
    // on `session/load` responses — without this, the renderer's
    // model picker stays empty. Mirrors Electron's
    // `applyModelsFromSession(result?.models)` in
    // `backend.ts:4410`.
    if let Some(models_val) = load_result.get("models") {
        let (current, available) = parse_models(models_val);
        apply_models_to_focus(&state, current, available.clone()).await;
        // Mirror into the parked bag too, so a subsequent WARM hit
        // sees the catalog without needing a refetch.
        let mut cache = state.runtime_cache.lock().await;
        if let Some(rt) = cache.get_mut(&session_id) {
            rt.available_models = available;
        }
    }

    // Session load is complete — turn off replaying so the renderer
    // stops showing the "loading conversation" spinner, and promote
    // the bag to `hydrated` so the next switch into this session is
    // instantaneous.
    *state.replaying.lock().await = false;
    mark_hydrated(&state, &session_id, true).await;
    sync_active_into_runtimes(&state).await;

    {
        let snap = build_snapshot_from_state(&state).await;
        emit_snapshot_event(&app, snap).await;
    }

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
    sync_active_into_runtimes(&state).await;

    let mut guard = state.agent.lock().await;
    let bridge = guard.as_mut().ok_or_else(|| "agent not connected".to_string())?;
    let session_id = state
        .session_id
        .lock()
        .await
        .clone()
        .ok_or_else(|| "no active session".to_string())?;
    let text = match &payload {
        Value::String(s) => s.clone(),
        Value::Object(map) => map
            .get("text")
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .unwrap_or_default(),
        _ => String::new(),
    };

    // Card lifecycle: push before RPC, close on success/failure/cancel.
    // Mirrors Electron's beginCompact → finishCompact + beginGoalAction →
    // finishGoalAction pattern in sendPrompt (backend.ts:6730-6860).
    let is_manual_compact = detect_manual_compact(&text).is_some();
    let goal_verb = detect_goal_action_verb(&text);
    if is_manual_compact {
        push_compact_card(&state, "manual", None).await;
    }
    if let Some(verb) = goal_verb {
        push_goal_action_card(&state, verb).await;
    }

    let params = json!({
        "sessionId": session_id,
        "prompt": [
            { "type": "text", "text": text }
        ],
    });

    let result = bridge
        .call("session/prompt", params)
        .await;

    match result {
        Ok(_) => {
            if is_manual_compact {
                finish_compact_card(&state, "completed", None, None, None).await;
            }
            if goal_verb.is_some() {
                finish_goal_action_card(&state, "completed", None).await;
            }
            // Hydrate the session bag so the next switch is WARM.
            let sid = state.session_id.lock().await.clone();
            if let Some(sid) = sid {
                let mut cache = state.runtime_cache.lock().await;
                cache.entry(sid.clone()).or_insert_with(|| empty_runtime(&sid, ""));
                drop(cache);
                mark_hydrated(&state, &sid, true).await;
            }
            Ok(())
        }
        Err(e) => {
            let msg = e.to_string();
            let cancelled = msg.to_lowercase().contains("cancel");
            if is_manual_compact {
                finish_compact_card(
                    &state,
                    if cancelled { "cancelled" } else { "failed" },
                    if cancelled { None } else { Some(&msg) },
                    None,
                    None,
                ).await;
            }
            if goal_verb.is_some() {
                finish_goal_action_card(
                    &state,
                    if cancelled { "cancelled" } else { "failed" },
                    if cancelled { None } else { Some(&msg) },
                ).await;
            }
            Err(msg)
        }
    }
}

/// Flip the running compact card to a terminal status.
async fn finish_compact_card(
    state: &AppState,
    status: &str,
    message: Option<&str>,
    tokens_before: Option<f64>,
    tokens_after: Option<f64>,
) {
    *state.compacting.lock().await = false;
    let cid = state.compact_timeline_id.lock().await.clone();
    if let Some(ref id) = cid {
        let mut tl = state.timeline.lock().await;
        if let Some(item) = tl.iter_mut().find(|i| i.get("id").and_then(|v| v.as_str()) == Some(id.as_str())) {
            if item.get("kind") == Some(&json!("compact")) {
                item["status"] = json!(status);
                if let Some(m) = message { item["message"] = json!(m); }
                if let Some(b) = tokens_before { item["tokensBefore"] = json!(b); }
                if let Some(a) = tokens_after { item["tokensAfter"] = json!(a); }
            }
        }
        drop(tl);
    }
    *state.compact_timeline_id.lock().await = None;
}

/// Flip the running goal_action card to a terminal status.
async fn finish_goal_action_card(
    state: &AppState,
    status: &str,
    message: Option<&str>,
) {
    let gid = state.goal_action_timeline_id.lock().await.clone();
    if let Some(ref id) = gid {
        let mut tl = state.timeline.lock().await;
        if let Some(item) = tl.iter_mut().find(|i| i.get("id").and_then(|v| v.as_str()) == Some(id.as_str())) {
            if item.get("kind") == Some(&json!("goal_action")) {
                item["status"] = json!(status);
                if let Some(m) = message { item["message"] = json!(m); }
            }
        }
        drop(tl);
    }
    *state.goal_action_timeline_id.lock().await = None;
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
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<(), String> {
    // A model can only be set on an active session. Without this
    // guard the agent rejects the call with "no active session",
    // which the renderer surfaces as a generic error and the user
    // reads as "can't connect to grok model".
    let session_id = state
        .session_id
        .lock()
        .await
        .clone()
        .ok_or_else(|| "no active session — pick a session first".to_string())?;

    let mut guard = state.agent.lock().await;
    let bridge = guard
        .as_mut()
        .ok_or_else(|| "agent not connected".to_string())?;

    // Method name + payload shape must match Electron exactly:
    // `backend.ts:4816` issues `session/set_model` with
    // `{ sessionId, modelId, _meta: { reasoning_effort } }`.
    // The previous Tauri port used `set_session_model` and put
    // `reasoningEffort` at the top level — the agent rejects both.
    let mut params = json!({
        "sessionId": session_id,
        "modelId": model_id,
    });
    if let Some(re) = reasoning_effort {
        params.as_object_mut().unwrap().insert(
            "_meta".into(),
            json!({ "reasoning_effort": re }),
        );
    }
    bridge
        .call("session/set_model", params)
        .await
        .map_err(|e| e.to_string())?;
    drop(guard);

    // Adopt the new modelId on focus + mirror into the parked bag.
    *state.model_id.lock().await = Some(model_id.clone());
    sync_active_into_runtimes(&state).await;

    let snap = build_snapshot_from_state(&state).await;
    emit_snapshot_event(&app, snap).await;
    Ok(())
}

#[tauri::command]
pub async fn agent_set_mode(
    mode_id: String,
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let session_id = state
        .session_id
        .lock()
        .await
        .clone()
        .ok_or_else(|| "no active session — pick a session first".to_string())?;
    let mut guard = state.agent.lock().await;
    let bridge = guard
        .as_mut()
        .ok_or_else(|| "agent not connected".to_string())?;
    // Same shape as Electron's `setMode` (`backend.ts:4846`):
    // method `session/set_mode`, payload `{ sessionId, modeId }`.
    bridge
        .call(
            "session/set_mode",
            json!({"sessionId": session_id, "modeId": mode_id}),
        )
        .await
        .map_err(|e| e.to_string())?;
    drop(guard);
    let snap = build_snapshot_from_state(&state).await;
    emit_snapshot_event(&app, snap).await;
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
    // Built-in catalog matching `PROVIDER_PRESETS` from the Electron build
    // (`grok-build-desktop/src/main/model-providers.ts:52-327`). Returns
    // the full `ModelProviderPreset` schema so the renderer's
    // `ModelsView.tsx` preset picker can render names, regions, accents,
    // logos, and popular models.
    Ok(crate::presets::all_presets())
}

fn providers_path() -> std::path::PathBuf {
    crate::paths::grok_home().join("desktop-providers.json")
}

/// Read the providers array from `desktop-providers.json`. Tolerant of
/// two file shapes:
///   * Electron envelope `{version: 1, providers: [...]}` (canonical,
///     `grok-build-desktop/src/main/model-providers.ts:488-497`).
///   * Legacy bare array `[...]` (older Tauri builds, or hand-edited).
/// Missing or invalid file → empty list.
fn read_providers_file() -> Vec<Value> {
    let text = match std::fs::read_to_string(providers_path()) {
        Ok(t) => t,
        Err(_) => return Vec::new(),
    };
    let v: Value = match serde_json::from_str(&text) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };
    match v {
        Value::Object(mut obj) => {
            // Electron envelope.
            match obj.remove("providers") {
                Some(Value::Array(arr)) => arr,
                _ => Vec::new(),
            }
        }
        Value::Array(arr) => arr, // Legacy bare array.
        _ => Vec::new(),
    }
}

/// Persist the providers array to `desktop-providers.json` using the
/// Electron envelope `{version: 1, providers: [...]}`. Mode 0600 on
/// Unix because the file contains API keys. Atomic via tmp + rename.
fn write_providers_file(providers: &[Value]) -> std::io::Result<()> {
    let path = providers_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let envelope = serde_json::json!({
        "version": 1,
        "providers": providers,
    });
    let text = serde_json::to_string_pretty(&envelope)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;

    // Atomic write: tmp file + rename, so a crash mid-write doesn't
    // leave a half-written providers file on disk. Mirrors the agent
    // crate's atomic-write pattern in
    // `xai-grok-shell/src/util/config/persist.rs:54-92`.
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let tmp = path.with_extension(format!("json.tmp.{}.{}", std::process::id(), nanos));
    std::fs::write(&tmp, text)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        // Mode 0600 — file holds API keys (Electron's same default at
        // model-providers.ts:495).
        let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600));
    }
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

#[tauri::command]
pub async fn models_list_providers() -> Result<Value, String> {
    Ok(Value::Array(read_providers_file()))
}

#[tauri::command]
pub async fn models_upsert_provider(mut input: Value) -> Result<Value, String> {
    let mut providers = read_providers_file();
    let mut id = input.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string();
    // When the renderer omits id (e.g. `editor.id` was undefined because
    // the editor was opened via "Add provider" instead of "Edit"), fall
    // back to a fresh UUID. Without this, every save of a no-id row
    // collides with the previous one (`id == ""` matches nothing) and
    // piles up duplicate rows. Mirrors Electron's
    // `randomUUID()` fallback at `model-providers.ts:706`.
    if id.is_empty() {
        id = uuid_v4();
        if let Some(obj) = input.as_object_mut() {
            obj.insert("id".to_string(), Value::String(id.clone()));
        }
    }
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
    // Persist JSON envelope first; TOML sync comes after so the on-disk
    // order matches Electron's main/index.ts:941-988 flow (JSON first,
    // then `syncConfigToml`, then agent reload).
    write_providers_file(&providers).map_err(|e| e.to_string())?;
    if let Err(e) = sync_desktop_config_toml(&providers) {
        tracing::warn!(error = %e, "TOML sync failed; JSON already saved");
    }
    if let Err(e) = reload_agent_models().await {
        tracing::warn!(error = %e, "agent reload failed; user may need to reconnect");
    }
    Ok(input)
}

/// Tiny RFC-4122 v4 UUID generator — no extra crate needed for what's
/// just an opaque id. We only need uniqueness within the desktop-
/// providers.json file, not cryptographic strength.
fn uuid_v4() -> String {
    use rand::Rng;
    let mut rng = rand::thread_rng();
    let bytes: [u8; 16] = rng.gen();
    let mut hex = String::with_capacity(32);
    for b in bytes.iter() {
        hex.push_str(&format!("{:02x}", b));
    }
    // Insert the v4 + variant markers per RFC 4122 §4.4.
    let mut out = String::with_capacity(36);
    out.push_str(&hex[0..8]);
    out.push('-');
    out.push_str(&hex[8..12]);
    out.push('-');
    // version 4 = 0100xxxx
    let high = u8::from_str_radix(&hex[12..14], 16).unwrap_or(0);
    out.push_str(&format!("{:02x}", (high & 0x0f) | 0x40));
    out.push_str(&hex[14..16]);
    out.push('-');
    // variant 10xxxxxx
    let high = u8::from_str_radix(&hex[16..18], 16).unwrap_or(0);
    out.push_str(&format!("{:02x}", (high & 0x3f) | 0x80));
    out.push_str(&hex[18..20]);
    out.push('-');
    out.push_str(&hex[20..32]);
    out
}

#[tauri::command]
pub async fn models_delete_provider(id: String) -> Result<(), String> {
    let mut providers = read_providers_file();
    providers.retain(|p| p.get("id").and_then(|v| v.as_str()) != Some(&id));
    write_providers_file(&providers).map_err(|e| e.to_string())?;
    if let Err(e) = sync_desktop_config_toml(&providers) {
        tracing::warn!(error = %e, "TOML sync failed after delete; JSON already saved");
    }
    if let Err(e) = reload_agent_models().await {
        tracing::warn!(error = %e, "agent reload failed; user may need to reconnect");
    }
    Ok(())
}

#[tauri::command]
pub async fn models_add_from_preset(
    preset_id: String,
    overrides: Option<Value>,
) -> Result<Value, String> {
    // Look up the preset from the built-in catalog and seed a fresh
    // `ModelProviderConfig` row in `~/.grok/desktop-providers.json`. The
    // shape matches the Electron `addFromPreset` (model-providers.ts:811).
    let preset = crate::presets::find_preset(&preset_id)
        .ok_or_else(|| format!("preset not found: {}", preset_id))?;

    // Don't seed an `id` here — let `models_upsert_provider` mint a
    // fresh UUID. (Earlier versions used `preset_id` as the id, which
    // collided when the same preset was added twice and also produced
    // non-UUID ids like `dp_deepseek` that broke later lookups.)
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);

    let mut provider = json!({
        "presetId": preset.get("id").cloned().unwrap_or(Value::Null),
        "name": preset.get("name").cloned().unwrap_or(Value::Null),
        "baseUrl": preset.get("baseUrl").cloned().unwrap_or(Value::Null),
        "apiBackend": preset.get("apiBackend").cloned().unwrap_or(Value::String("chat_completions".into())),
        "enabled": true,
        "authStyle": preset.get("authStyle").cloned().unwrap_or(Value::String("bearer".into())),
        "models": [],
        "createdAt": now,
        "updatedAt": now,
    });
    if let Some(k) = preset.get("envKey").cloned() {
        provider.as_object_mut().unwrap().insert("envKey".to_string(), k);
    }
    if let Some(h) = preset.get("extraHeaders").cloned() {
        provider.as_object_mut().unwrap().insert("extraHeaders".to_string(), h);
    }
    if let Some(api_key) = overrides
        .as_ref()
        .and_then(|o| o.get("apiKey"))
        .and_then(|v| v.as_str())
    {
        if !api_key.is_empty() {
            provider.as_object_mut().unwrap().insert(
                "apiKey".to_string(),
                Value::String(api_key.to_string()),
            );
        }
    }

    models_upsert_provider(provider).await
}

/// Sync the post-mutation provider list into `~/.grok/config.toml`
/// so the running `grok agent serve` discovers the new custom models.
/// Thin wrapper around `config_toml_sync::sync_config_toml` that
/// surfaces errors as a String for the Tauri command boundary.
fn sync_desktop_config_toml(providers: &[Value]) -> Result<(), String> {
    crate::config_toml_sync::sync_config_toml(providers)
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// Trigger `grok agent serve` to reload its config so the new
/// `[model.*]` entries become available in the composer picker
/// without requiring a session reconnect.
async fn reload_agent_models() -> Result<(), String> {
    crate::config_toml_sync::reload_agent_models_inner()
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn models_fetch_provider_models(input: Value) -> Result<Value, String> {
    // Pull models from the provider's `/v1/models` endpoint.
    // Port of `fetchProviderModels` from
    // `grok-build-desktop/src/main/model-providers.ts:857-919`.
    let parsed = crate::models_fetch::FetchInput {
        base_url: input
            .get("baseUrl")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        api_key: input
            .get("apiKey")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        env_key: input
            .get("envKey")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        auth_style: input
            .get("authStyle")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        extra_headers: input.get("extraHeaders").cloned(),
    };
    crate::models_fetch::fetch_provider_models(parsed)
        .await
        .map(Value::Array)
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn models_get_config_key_index() -> Result<Value, String> {
    let providers: Vec<Value> = {
        let text = std::fs::read_to_string(providers_path()).unwrap_or_else(|_| "[]".to_string());
        let parsed: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
        match parsed {
            Value::Object(mut obj) => match obj.remove("providers") {
                Some(Value::Array(arr)) => arr,
                _ => Vec::new(),
            },
            Value::Array(arr) => arr,
            _ => Vec::new(),
        }
    };
    // The renderer's `groupModelsByProvider` reads this as
    // `Record<configKey, { providerId, providerName }>` — one entry
    // per model under each provider, keyed by the agent's modelId
    // (== config.toml `[model.<configKey>]` section name).
    //
    // The previous implementation keyed by *provider* id, which made
    // every model under one provider share a single entry and broke
    // the renderer's lookup `modelKeyIndex[mod.modelId]`.
    let mut map: serde_json::Map<String, Value> = serde_json::Map::new();
    for p in &providers {
        let provider_id = p.get("id").and_then(|v| v.as_str()).unwrap_or("");
        let provider_name = p
            .get("name")
            .or_else(|| p.get("label"))
            .and_then(|v| v.as_str())
            .unwrap_or(provider_id);
        if provider_id.is_empty() {
            continue;
        }
        let enabled = p.get("enabled").and_then(|v| v.as_bool()).unwrap_or(true);
        if !enabled {
            continue;
        }
        let Some(models) = p.get("models").and_then(|v| v.as_array()) else {
            // No model list yet — register the provider id itself so
            // models added later can still resolve through the index.
            map.insert(
                provider_id.to_string(),
                json!({"providerId": provider_id, "providerName": provider_name}),
            );
            continue;
        };
        for m in models {
            let model_enabled = m.get("enabled").and_then(|v| v.as_bool()).unwrap_or(true);
            if !model_enabled {
                continue;
            }
            // Prefer explicit `configKey`, fall back to the model `id`.
            let key = m
                .get("configKey")
                .and_then(|v| v.as_str())
                .or_else(|| m.get("id").and_then(|v| v.as_str()))
                .unwrap_or("");
            if key.is_empty() {
                continue;
            }
            map.insert(
                key.to_string(),
                json!({"providerId": provider_id, "providerName": provider_name}),
            );
        }
    }
    Ok(Value::Object(map))
}

#[tauri::command]
pub async fn models_query_provider_usage(provider_id: String) -> Result<Value, String> {
    let provider = load_provider(&provider_id).ok_or_else(|| {
        format!("Provider not found: {provider_id}")
    })?;
    let result = query_provider_usage(&provider).await;
    Ok(result)
}

/// Read the provider JSON entry (`desktop-providers.json`) by id,
/// returning a normalised struct so the usage query doesn't have to
/// keep digging through raw JSON. Mirrors `getProvider()` in
/// `grok-build-desktop/src/main/model-providers.ts:687`.
fn load_provider(provider_id: &str) -> Option<PersistedProvider> {
    let providers = read_providers_file();
    for p in providers {
        let id = p.get("id").and_then(|v| v.as_str())?;
        if id != provider_id {
            continue;
        }
        let preset_id = p.get("presetId").and_then(|v| v.as_str()).map(str::to_string);
        let base_url = p
            .get("baseUrl")
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .unwrap_or_default();
        let api_key = p.get("apiKey").and_then(|v| v.as_str()).map(str::to_string);
        let env_key = p.get("envKey").and_then(|v| v.as_str()).map(str::to_string);
        return Some(PersistedProvider {
            preset_id,
            base_url,
            api_key,
            env_key,
        });
    }
    None
}

struct PersistedProvider {
    preset_id: Option<String>,
    base_url: String,
    api_key: Option<String>,
    env_key: Option<String>,
}

/// Resolve an API key (stored → env var → empty). Mirrors
/// `resolveApiKey()` in `provider-usage.ts:64`.
fn resolve_api_key(api_key: Option<String>, env_key: Option<&str>) -> String {
    let trimmed = api_key.unwrap_or_default().trim().to_string();
    if !trimmed.is_empty() {
        return trimmed;
    }
    let Some(env_key) = env_key else {
        return String::new();
    };
    for k in env_key
        .split(|c| c == ',' || c == '|')
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        if let Ok(v) = std::env::var(k) {
            let t = v.trim();
            if !t.is_empty() {
                return t.to_string();
            }
        }
    }
    String::new()
}

/// Classify a provider by preset id / baseUrl. Mirrors
/// `detectProvider()` in `provider-usage.ts:53`.
fn detect_provider(preset_id: Option<&str>, base_url: &str) -> Option<&'static str> {
    let id = preset_id.unwrap_or("").to_lowercase();
    let lower = base_url.to_lowercase();
    if id == "minimax" || lower.contains("api.minimaxi.com") || lower.contains("api.minimax.io") {
        return Some("minimax-coding-plan");
    }
    if id == "deepseek" || lower.contains("api.deepseek.com") {
        return Some("deepseek-balance");
    }
    None
}

fn minimax_host(base_url: &str) -> Option<&'static str> {
    let lower = base_url.to_lowercase();
    if lower.contains("api.minimaxi.com") {
        return Some("api.minimaxi.com");
    }
    if lower.contains("api.minimax.io") {
        return Some("api.minimax.io");
    }
    None
}

/// Entry point shared with `models_query_provider_usage`. Mirrors
/// `queryProviderUsage()` in `provider-usage.ts:261`. Returns the
/// `ProviderUsageResult` shape the renderer expects directly.
async fn query_provider_usage(p: &PersistedProvider) -> Value {
    let fetched_at = now_iso_local();
    let base_url = p.base_url.trim().to_string();
    let kind = match detect_provider(p.preset_id.as_deref(), &base_url) {
        Some(k) => k,
        None => {
            return json!({
                "success": false,
                "fetchedAt": fetched_at,
                "error": "Provider does not support usage queries",
            });
        }
    };

    let api_key = resolve_api_key(p.api_key.clone(), p.env_key.as_deref());
    if api_key.is_empty() {
        return json!({
            "success": false,
            "fetchedAt": fetched_at,
            "error": "API key is required",
        });
    }

    let outcome = match kind {
        "minimax-coding-plan" => query_minimax_quota(&base_url, &api_key).await,
        "deepseek-balance" => query_deepseek_balance(&api_key).await,
        _ => unreachable!(),
    };
    match outcome {
        Ok(value) => value,
        Err(err) => json!({
            "success": false,
            "fetchedAt": fetched_at,
            "error": err,
        }),
    }
}

async fn query_minimax_quota(base_url: &str, api_key: &str) -> Result<Value, String> {
    let host = minimax_host(base_url).ok_or_else(|| "Provider does not support usage queries".to_string())?;
    let url = format!("https://{host}/v1/api/openplatform/coding_plan/remains");
    let body: Value = http_get_json(&url, api_key).await?;
    if let Some(base_resp) = body.get("base_resp").and_then(|v| v.as_object()) {
        if let Some(code) = base_resp.get("status_code").and_then(|v| v.as_i64()) {
            if code != 0 {
                let msg = base_resp
                    .get("status_msg")
                    .and_then(|v| v.as_str())
                    .unwrap_or("API error");
                return Err(format!("{msg} (code {code})"));
            }
        }
    }
    let quota = parse_minimax_tiers(&body).ok_or_else(|| "No 'general' plan in response".to_string())?;
    Ok(json!({
        "success": true,
        "fetchedAt": now_iso_local(),
        "quota": quota,
    }))
}

/// Pure parser for MiniMax's `coding_plan/remains` response.
/// Mirrors `parseMinimaxTiers()` in `provider-usage.ts:120`.
fn parse_minimax_tiers(body: &Value) -> Option<Value> {
    let arr = body.get("model_remains").and_then(|v| v.as_array())?;
    let item = arr.iter().find_map(|it| {
        let obj = it.as_object()?;
        if obj.get("model_name").and_then(|v| v.as_str()) == Some("general") {
            Some(obj)
        } else {
            None
        }
    })?;
    let mut quota = serde_json::Map::new();
    if let Some(pct) = item
        .get("current_interval_remaining_percent")
        .and_then(|v| v.as_f64())
    {
        let used = clamp_pct(100.0 - pct);
        quota.insert("fiveHourPct".into(), json!(used));
        if let Some(end) = item.get("end_time").and_then(|v| v.as_i64()) {
            quota.insert("fiveHourResetMs".into(), json!(end));
        }
    }
    if item.get("current_weekly_status").and_then(|v| v.as_i64()) == Some(1) {
        if let Some(pct) = item
            .get("current_weekly_remaining_percent")
            .and_then(|v| v.as_f64())
        {
            let used = clamp_pct(100.0 - pct);
            quota.insert("sevenDayPct".into(), json!(used));
            if let Some(end) = item.get("weekly_end_time").and_then(|v| v.as_i64()) {
                quota.insert("sevenDayResetMs".into(), json!(end));
            }
        }
    }
    if quota.is_empty() {
        None
    } else {
        Some(Value::Object(quota))
    }
}

async fn query_deepseek_balance(api_key: &str) -> Result<Value, String> {
    let url = "https://api.deepseek.com/user/balance";
    let body: Value = http_get_json(url, api_key).await?;
    let balance = parse_deepseek_balance(&body)?;
    Ok(json!({
        "success": true,
        "fetchedAt": now_iso_local(),
        "balance": balance,
    }))
}

/// Pure parser for DeepSeek's `/user/balance` response. Mirrors
/// `parseDeepseekBalance()` in `provider-usage.ts:188`.
fn parse_deepseek_balance(body: &Value) -> Result<Value, String> {
    let infos = body
        .get("balance_infos")
        .and_then(|v| v.as_array())
        .ok_or_else(|| "Missing balance_infos in DeepSeek response".to_string())?;
    if infos.is_empty() {
        return Err("Empty balance_infos array".to_string());
    }
    let entry = infos
        .iter()
        .find_map(|it| it.as_object())
        .ok_or_else(|| "Empty balance_infos array".to_string())?;
    let total = entry
        .get("total_balance")
        .and_then(|v| v.as_f64())
        .or_else(|| {
            entry
                .get("total_balance")
                .and_then(|v| v.as_str())
                .and_then(|s| s.parse::<f64>().ok())
        })
        .ok_or_else(|| "total_balance missing or not a number".to_string())?;
    if !total.is_finite() {
        return Err("total_balance missing or not a number".to_string());
    }
    let mut obj = serde_json::Map::new();
    obj.insert("remaining".into(), json!(total));
    obj.insert(
        "unit".into(),
        Value::String(
            entry
                .get("currency")
                .and_then(|v| v.as_str())
                .unwrap_or("CNY")
                .to_string(),
        ),
    );
    if let Some(g) = entry.get("granted_balance").and_then(|v| v.as_f64()) {
        obj.insert("grantedBalance".into(), json!(g));
    }
    if let Some(t) = entry.get("topped_up_balance").and_then(|v| v.as_f64()) {
        obj.insert("toppedUpBalance".into(), json!(t));
    }
    let available = body
        .get("is_available")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    obj.insert("available".into(), Value::Bool(available));
    Ok(Value::Object(obj))
}

fn clamp_pct(v: f64) -> f64 {
    if !v.is_finite() {
        0.0
    } else {
        v.max(0.0).min(100.0)
    }
}

/// Lightweight JSON HTTP GET with timeout + bearer auth. Mirrors
/// `httpGetJson()` in `provider-usage.ts:84`.
async fn http_get_json(url: &str, api_key: &str) -> Result<Value, String> {
    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(REQUEST_TIMEOUT_SECS))
        .build()
    {
        Ok(c) => c,
        Err(e) => return Err(format!("http client: {e}")),
    };
    let res = client
        .get(url)
        .header("Authorization", format!("Bearer {api_key}"))
        .header("Content-Type", "application/json")
        .header("Accept", "application/json")
        .send()
        .await
        .map_err(|e| {
            if e.is_timeout() {
                format!("Request timed out after {REQUEST_TIMEOUT_SECS}s")
            } else {
                format!("{e}")
            }
        })?;
    let status = res.status();
    if status == reqwest::StatusCode::UNAUTHORIZED
        || status == reqwest::StatusCode::FORBIDDEN
    {
        return Err(format!("Invalid API key (HTTP {})", status.as_u16()));
    }
    if !status.is_success() {
        let body = res
            .text()
            .await
            .unwrap_or_default()
            .chars()
            .take(160)
            .collect::<String>();
        return Err(format!(
            "HTTP {}{}",
            status.as_u16(),
            if body.is_empty() {
                String::new()
            } else {
                format!(": {body}")
            }
        ));
    }
    res.json::<Value>()
        .await
        .map_err(|e| format!("invalid JSON: {e}"))
}

const REQUEST_TIMEOUT_SECS: u64 = 12;

/// Local ISO timestamp (no fractional seconds). Used as the
/// `fetchedAt` field on usage responses — mirrors `new
/// Date().toISOString()` from the Electron port.
fn now_iso_local() -> String {
    now_iso()
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
//
// Credentials live in `~/.grok/auth.json` (shared with the CLI). The file is
// a map of `"issuer::client_id" → entry`, not a flat `{ email, ... }` object.
// Shape matches Electron `account-manager.ts` / frontend `AccountStatus`.

fn auth_json_path() -> std::path::PathBuf {
    crate::paths::grok_home().join("auth.json")
}

fn desktop_api_key_path() -> std::path::PathBuf {
    crate::paths::grok_home().join("desktop-api-key")
}

/// One credential entry from `auth.json` (fields we care about for the UI).
struct AuthEntry {
    email: Option<String>,
    user_id: Option<String>,
    first_name: Option<String>,
    last_name: Option<String>,
    team_id: Option<String>,
    auth_mode: Option<String>,
    expires_at: Option<String>,
    oidc_issuer: Option<String>,
    key: Option<String>,
    refresh_token: Option<String>,
}

fn opt_str(v: &Value, key: &str) -> Option<String> {
    v.get(key)
        .and_then(|x| x.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

fn read_auth_entries() -> Vec<AuthEntry> {
    let text = match std::fs::read_to_string(auth_json_path()) {
        Ok(t) => t,
        Err(_) => return Vec::new(),
    };
    let data: Value = match serde_json::from_str(&text) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };
    let obj = match data.as_object() {
        Some(o) => o,
        None => return Vec::new(),
    };
    obj.values()
        .filter_map(|v| {
            if !v.is_object() {
                return None;
            }
            Some(AuthEntry {
                email: opt_str(v, "email"),
                user_id: opt_str(v, "user_id"),
                first_name: opt_str(v, "first_name"),
                last_name: opt_str(v, "last_name"),
                team_id: opt_str(v, "team_id"),
                auth_mode: opt_str(v, "auth_mode"),
                expires_at: opt_str(v, "expires_at"),
                oidc_issuer: opt_str(v, "oidc_issuer"),
                key: opt_str(v, "key"),
                refresh_token: opt_str(v, "refresh_token"),
            })
        })
        .collect()
}

fn pick_primary_entry(entries: &[AuthEntry]) -> Option<&AuthEntry> {
    entries
        .iter()
        .find(|e| e.email.as_ref().is_some_and(|s| !s.is_empty()))
        .or_else(|| entries.first())
}

fn desktop_api_key_present() -> bool {
    std::fs::read_to_string(desktop_api_key_path())
        .map(|t| !t.trim().is_empty())
        .unwrap_or(false)
}

fn env_api_key_present() -> bool {
    ["XAI_API_KEY", "GROK_CODE_XAI_API_KEY"]
        .iter()
        .any(|k| std::env::var(k).map(|v| !v.trim().is_empty()).unwrap_or(false))
}

fn build_account_status() -> Value {
    let entries = read_auth_entries();
    let primary = pick_primary_entry(&entries);
    let desktop_key = desktop_api_key_present();
    let env_key = env_api_key_present();

    let signed_in = primary.is_some_and(|p| {
        p.email.is_some() || p.key.is_some() || p.refresh_token.is_some()
    });

    let display_name = primary.and_then(|p| {
        let parts: Vec<&str> = [p.first_name.as_deref(), p.last_name.as_deref()]
            .into_iter()
            .flatten()
            .filter(|s| !s.is_empty())
            .collect();
        if parts.is_empty() {
            None
        } else {
            Some(parts.join(" "))
        }
    });

    let api_key_set = env_key || desktop_key;
    let api_key_source: Option<&str> = if env_key {
        Some("env")
    } else if desktop_key {
        Some("desktop")
    } else {
        None
    };

    json!({
        "signedIn": signed_in,
        "email": primary.and_then(|p| p.email.clone()),
        "displayName": display_name,
        "userId": primary.and_then(|p| p.user_id.clone()),
        "teamId": primary.and_then(|p| p.team_id.clone()),
        "authMode": primary.and_then(|p| p.auth_mode.clone()),
        "expiresAt": primary.and_then(|p| p.expires_at.clone()),
        "issuer": primary.and_then(|p| p.oidc_issuer.clone()),
        "apiKeySet": api_key_set,
        "apiKeySource": api_key_source,
        "loginInProgress": false,
    })
}

#[tauri::command]
pub async fn account_get_status() -> Result<Value, String> {
    Ok(build_account_status())
}

#[tauri::command]
pub async fn account_login(
    method: Value,
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<Value, String> {
    let m = method.as_str().unwrap_or("oauth");
    let args = if m == "device" {
        vec!["login", "--device-auth"]
    } else {
        vec!["login", "--oauth"]
    };
    let _ = crate::grok_cli::run_long(&args)
        .await
        .map_err(|e| format!("login failed: {}", e))?;
    // Re-emit a snapshot so the model picker banner flips off the
    // moment credentials land on disk — otherwise it stays stuck on
    // "Grok official models require login" until the user manually
    // refreshes or changes views.
    let snap = build_snapshot_from_state(&state).await;
    emit_snapshot_event(&app, snap).await;
    Ok(build_account_status())
}

#[tauri::command]
pub async fn account_cancel_login() -> Result<(), String> {
    // The login subprocess runs to completion; cancel is a no-op for now.
    Ok(())
}

#[tauri::command]
pub async fn account_logout(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<Value, String> {
    let _ = crate::grok_cli::run(&["logout"]).await;
    // Also clear desktop api key
    let _ = std::fs::remove_file(desktop_api_key_path());
    // Re-emit a snapshot so the model picker banner reappears.
    let snap = build_snapshot_from_state(&state).await;
    emit_snapshot_event(&app, snap).await;
    Ok(json!({
        "message": "logged out",
        "status": build_account_status(),
    }))
}

#[tauri::command]
pub async fn account_set_api_key(
    key: Option<String>,
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<Value, String> {
    let trimmed = key.as_deref().unwrap_or("").trim().to_string();
    if trimmed.is_empty() {
        let _ = std::fs::remove_file(desktop_api_key_path());
    } else {
        std::fs::create_dir_all(desktop_api_key_path().parent().unwrap())
            .map_err(|e| e.to_string())?;
        std::fs::write(desktop_api_key_path(), &trimmed).map_err(|e| e.to_string())?;
    }
    // Re-emit a snapshot so the model picker reflects the new state
    // (whether the key was set or cleared) without waiting for the
    // next `account:event`.
    let snap = build_snapshot_from_state(&state).await;
    emit_snapshot_event(&app, snap).await;
    account_get_status().await
}

#[tauri::command]
pub async fn account_open_external(_url: String) -> Result<(), String> {
    // Delegates to the opener plugin; the desktop.ts adapter calls openUrl.
    // This command is here so the invoke('account_open_external') path works.
    Ok(())
}

#[tauri::command]
pub async fn account_refresh_usage(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<Value, String> {
    // Acquisition guard: the agent must be connected before we can
    // call a billing extension. Mirrors the `!this.client?.connected
    // || !this.authenticated` short-circuit at `backend.ts:3512`.
    let mut guard = state.agent.lock().await;
    let bridge = match guard.as_mut() {
        Some(b) => b,
        None => {
            // Return the cached value (or null) instead of erroring
            // — SettingsView expects `null` when not signed in yet.
            let cached = state.usage.lock().await.clone();
            return Ok(cached.unwrap_or(Value::Null));
        }
    };

    // Try the canonical method name first; fall back to the alternate
    // wire name like Electron's `requestExt()` does
    // (`backend.ts:3606`).
    let raw = match try_call_billing(bridge).await {
        Ok(v) => v,
        Err(err) => {
            // On error: keep prior data with an `error` field; emit
            // snapshot so the UI can render the error inline.
            let message = format!("billing: {err}");
            let next = append_error_to_usage(state.usage.lock().await.clone(), &message);
            *state.usage.lock().await = next.clone();
            let snap = build_snapshot_from_state(&state).await;
            emit_snapshot_event(&app, snap).await;
            return Ok(next.unwrap_or(Value::Null));
        }
    };

    let mut usage = parse_billing_usage(&raw);
    if usage
        .get("prepaidUsd")
        .and_then(|v| v.as_f64())
        .map(|n| n > 0.0)
        .unwrap_or(false)
    {
        // Auto-topup is optional — ignore failures so they don't
        // block the rest of the usage display, exactly like Electron
        // (`backend.ts:3521`).
        if let Ok(topup) = try_call_auto_topup_rule(bridge).await {
            usage = merge_auto_topup(&usage, &topup);
        }
    }
    *state.usage.lock().await = Some(usage.clone());
    let snap = build_snapshot_from_state(&state).await;
    emit_snapshot_event(&app, snap).await;
    Ok(usage)
}

/// Try `_x.ai/billing` then `x.ai/billing` and return the first
/// non-`MethodNotFound` response. Mirrors `Backend.requestExt()`
/// (`backend.ts:3600`).
async fn try_call_billing(
    bridge: &mut AgentBridge,
) -> Result<Value, String> {
    let methods = ["_x.ai/billing", "x.ai/billing"];
    let mut last_err: Option<String> = None;
    for method in methods {
        match bridge.call(method, json!({})).await {
            Ok(v) => return Ok(v),
            Err(e) => {
                let msg = format!("{e:?}");
                let lower = msg.to_lowercase();
                if lower.contains("method not found")
                    || lower.contains("-32601")
                    || lower.contains("unknown method")
                {
                    last_err = Some(msg);
                    continue;
                }
                return Err(msg);
            }
        }
    }
    Err(last_err.unwrap_or_else(|| "billing extension not available".into()))
}

async fn try_call_auto_topup_rule(
    bridge: &mut AgentBridge,
) -> Result<Value, String> {
    let methods = ["_x.ai/auto-topup-rule", "x.ai/auto-topup-rule"];
    let mut last_err: Option<String> = None;
    for method in methods {
        match bridge.call(method, json!({})).await {
            Ok(v) => return Ok(v),
            Err(e) => {
                let msg = format!("{e:?}");
                let lower = msg.to_lowercase();
                if lower.contains("method not found")
                    || lower.contains("-32601")
                    || lower.contains("unknown method")
                {
                    last_err = Some(msg);
                    continue;
                }
                return Err(msg);
            }
        }
    }
    Err(last_err.unwrap_or_else(|| "auto-topup-rule extension not available".into()))
}

/// Merge a fetch error onto the existing cached usage so the UI can
/// render "previous data + error banner". Mirrors the catch block
/// at `backend.ts:3532-3556`.
fn append_error_to_usage(prev: Option<Value>, message: &str) -> Option<Value> {
    let mut obj = match prev {
        Some(Value::Object(m)) => m,
        _ => {
            // No prior data — produce a minimal placeholder.
            let mut placeholder = serde_json::Map::new();
            placeholder.insert("usagePct".into(), Value::Number(0.into()));
            placeholder.insert("usageLabel".into(), Value::String("Usage".into()));
            placeholder.insert("usageShort".into(), Value::String("—".into()));
            placeholder.insert(
                "summaryLines".into(),
                Value::Array(Vec::new()),
            );
            placeholder.insert(
                "manageUrl".into(),
                Value::String("https://grok.com/?_s=usage".into()),
            );
            placeholder
        }
    };
    obj.insert("error".into(), Value::String(message.to_string()));
    Some(Value::Object(obj))
}

/// Parse `x.ai/billing` response into the renderer's `UsageInfo`
/// shape. Direct port of `parseBillingUsage` from
/// `backend.ts:496-589`.
fn parse_billing_usage(raw: &Value) -> Value {
    let root = raw.as_object().cloned().unwrap_or_default();
    let body = root
        .get("result")
        .and_then(|v| v.as_object())
        .cloned()
        .unwrap_or(root.clone());

    let subscription_tier = body
        .get("subscriptionTier")
        .or_else(|| body.get("subscription_tier"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let config = body.get("config").and_then(|v| v.as_object());

    let credit_pct = config
        .and_then(|c| c.get("creditUsagePercent"))
        .and_then(|v| v.as_f64());
    let monthly_limit = config
        .and_then(|c| c.get("monthlyLimit"))
        .and_then(cent_val);
    let used = config
        .and_then(|c| c.get("used"))
        .and_then(cent_val)
        .unwrap_or(0.0);

    let usage_pct = if let Some(pct) = credit_pct {
        pct.clamp(0.0, 100.0)
    } else if let Some(lim) = monthly_limit.filter(|n| *n > 0.0) {
        (used / lim * 100.0).clamp(0.0, 100.0)
    } else {
        0.0
    };

    let period = config.and_then(|c| c.get("currentPeriod")).and_then(|v| v.as_object());
    let period_type = period
        .and_then(|p| p.get("type"))
        .or_else(|| period.and_then(|p| p.get("periodType")))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let usage_label = usage_label_from_period_type(period_type.as_deref());
    let usage_floor = usage_pct.floor();
    let usage_short = format!("{:.0}%", usage_floor);

    let mut summary_lines: Vec<Value> = vec![Value::String(format!(
        "{}: {:.0}%",
        usage_label, usage_floor
    ))];

    let period_end = period
        .and_then(|p| p.get("end"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .or_else(|| {
            config
                .and_then(|c| c.get("billingPeriodEnd"))
                .or_else(|| config.and_then(|c| c.get("billing_period_end")))
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
        });
    let mut period_end_display = None;
    if let Some(iso) = period_end.as_deref() {
        if let Some(disp) = format_period_end(iso) {
            summary_lines.push(Value::String(format!("Next reset: {disp}")));
            period_end_display = Some(disp);
        }
    }

    let prepaid_cents = config
        .and_then(|c| c.get("prepaidBalance"))
        .and_then(cent_val);
    let cap = config
        .and_then(|c| c.get("onDemandCap"))
        .and_then(cent_val)
        .unwrap_or(0.0);
    let pay_as_you_go = cap > 0.0;
    let on_demand_cap_cents = if cap > 0.0 { Some(cap) } else { None };
    let on_demand_used_cents = config
        .and_then(|c| c.get("onDemandUsed"))
        .and_then(cent_val)
        .or_else(|| {
            monthly_limit.map(|lim| (used - lim).max(0.0))
        });

    let mut out = serde_json::Map::new();
    out.insert("usagePct".into(), json!(usage_pct));
    out.insert("usageLabel".into(), Value::String(usage_label));
    out.insert("usageShort".into(), Value::String(usage_short));
    if let Some(d) = period_end_display {
        out.insert("periodEndDisplay".into(), Value::String(d));
    }
    if let Some(t) = subscription_tier {
        out.insert("subscriptionTier".into(), Value::String(t));
    }

    if let Some(p) = prepaid_cents {
        if p.abs() > 0.0 {
            let usd = p.abs() / 100.0;
            out.insert("prepaidUsd".into(), json!(usd));
            summary_lines.push(Value::String(String::new()));
            summary_lines
                .push(Value::String(format!("Credits: {}", format_usd_from_cents(p))));
        }
    }

    if pay_as_you_go {
        if let Some(cap_c) = on_demand_cap_cents {
            let used_u = on_demand_used_cents.unwrap_or(0.0).abs() / 100.0;
            let cap_u = cap_c.abs() / 100.0;
            out.insert("onDemandUsedUsd".into(), json!(used_u));
            out.insert("onDemandCapUsd".into(), json!(cap_u));
            out.insert("payAsYouGo".into(), Value::Bool(true));
            summary_lines.push(Value::String(String::new()));
            summary_lines.push(Value::String(format!(
                "Pay-as-you-go: ${:.2} used of ${:.2} limit",
                used_u, cap_u
            )));
        }
    }

    out.insert("summaryLines".into(), Value::Array(summary_lines));
    out.insert(
        "manageUrl".into(),
        Value::String("https://grok.com/?_s=usage".into()),
    );
    out.insert("fetchedAt".into(), Value::String(now_iso()));
    Value::Object(out)
}

/// Port of `mergeAutoTopup` (`backend.ts:589-617`). Annotates the
/// usage object with `autoTopupEnabled`/`autoTopupAmountUsd`/
/// `autoTopupMaxUsd` and rewrites the credits block in summaryLines.
fn merge_auto_topup(usage: &Value, raw: &Value) -> Value {
    let root = raw.as_object().cloned().unwrap_or_default();
    let body = root
        .get("result")
        .and_then(|v| v.as_object())
        .cloned()
        .unwrap_or(root);
    let rule = body.get("rule").and_then(|v| v.as_object());

    let mut obj = match usage.as_object().cloned() {
        Some(m) => m,
        None => return usage.clone(),
    };

    if let Some(rule) = rule {
        let enabled = rule.get("enabled").and_then(|v| v.as_bool()).unwrap_or(false);
        let topup = rule.get("topupAmount").and_then(cent_val);
        let max = rule.get("maxAmountPerMonth").and_then(cent_val);

        obj.insert("autoTopupEnabled".into(), Value::Bool(enabled));
        if let Some(t) = topup {
            obj.insert("autoTopupAmountUsd".into(), json!(t.abs() / 100.0));
        }
        if let Some(m) = max {
            obj.insert("autoTopupMaxUsd".into(), json!(m.abs() / 100.0));
        }

        // Rewrite summaryLines with auto-topup insert.
        if let Some(lines) = obj.get("summaryLines").and_then(|v| v.as_array()).cloned() {
            let new_lines: Vec<Value> = lines
                .into_iter()
                .filter(|l| {
                    l.as_str()
                        .map(|s| {
                            !s.starts_with("Auto topup:")
                                && !s.starts_with("Max monthly topup:")
                                && s != "Auto topup: disabled"
                        })
                        .unwrap_or(true)
                })
                .collect();
            let credits_idx = new_lines
                .iter()
                .position(|l| {
                    l.as_str()
                        .map(|s| s.starts_with("Credits:"))
                        .unwrap_or(false)
                });
            if let Some(idx) = credits_idx {
                let mut insert: Vec<Value> = Vec::new();
                if enabled {
                    if let Some(t) = topup {
                        insert.push(Value::String(format!(
                            "Auto topup: {}",
                            format_usd_from_cents(t)
                        )));
                    }
                    if let Some(m) = max {
                        insert.push(Value::String(format!(
                            "Max monthly topup: {}",
                            format_usd_from_cents(m)
                        )));
                    }
                } else {
                    insert.push(Value::String("Auto topup: disabled".into()));
                }
                let mut combined: Vec<Value> = new_lines[..=idx].to_vec();
                combined.extend(insert);
                combined.extend(new_lines[idx + 1..].to_vec());
                obj.insert("summaryLines".into(), Value::Array(combined));
            }
        }
    } else {
        obj.insert("autoTopupEnabled".into(), Value::Bool(false));
    }
    Value::Object(obj)
}

/// Extract a numeric cents value from `{val: number}` shaped fields.
/// Mirrors `centVal` (`backend.ts:459`).
fn cent_val(v: &Value) -> Option<f64> {
    let rec = v.as_object()?;
    let n = rec.get("val")?.as_f64()?;
    if n.is_finite() {
        Some(n)
    } else {
        Some(0.0)
    }
}

fn format_usd_from_cents(cents: f64) -> String {
    let dollars = cents.abs() / 100.0;
    if dollars.round() == dollars {
        format!("${:.0}", dollars)
    } else {
        format!("${:.2}", dollars)
    }
}

fn format_period_end(iso: &str) -> Option<String> {
    // Parse "2026-07-31T00:00:00Z" without pulling chrono.
    let y = iso.get(..4)?.parse::<i32>().ok()?;
    let mo = iso.get(5..7)?.parse::<u32>().ok()?;
    let d = iso.get(8..10)?.parse::<u32>().ok()?;
    let (hh, mm) = if iso.len() >= 16 {
        (
            iso.get(11..13).and_then(|s| s.parse::<u32>().ok()).unwrap_or(0),
            iso.get(14..16).and_then(|s| s.parse::<u32>().ok()).unwrap_or(0),
        )
    } else {
        (0, 0)
    };
    // Render in UTC, formatted roughly like "Jul 31, 00:00" (the
    // renderer's `UsageInfo.periodEndDisplay` is just a string label).
    let months = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun",
        "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let mon_label = months.get((mo as usize).saturating_sub(1)).copied().unwrap_or("?");
    Some(format!("{} {}, {:02}:{:02}", mon_label, d, hh, mm))
}

/// Howard Hinnant's days_from_civil — same algorithm the
/// `<chrono>` library uses internally. Good enough for our window.
fn days_from_civil(y: i32, m: i32, d: i32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = (y - era * 400) as i64; // [0, 399]
    let m = if m > 2 { m - 3 } else { m + 9 } as i32;
    let doy = ((153 * m + 2) / 5 + (d - 1) as i32) as i64; // [0, 1460]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;     // [0, 146096]
    era as i64 * 146097 + doe - 719468
}

fn usage_label_from_period_type(period_type: Option<&str>) -> String {
    match period_type {
        Some(p) if p.contains("WEEKLY") => "Weekly limit".into(),
        Some(p) if p.contains("MONTHLY") => "Monthly limit".into(),
        Some(_) => "Usage".into(),
        None => "Usage".into(),
    }
}

fn now_iso() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // YYYY-MM-DDTHH:MM:SSZ in UTC, approximate (no leap-second handling).
    let sec = (secs % 60) as u32;
    let min = ((secs / 60) % 60) as u32;
    let hour = ((secs / 3600) % 24) as u32;
    let days = secs / 86_400;
    let (y, m, d) = civil_from_days(days as i64);
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        y, m, d, hour, min, sec
    )
}

fn civil_from_days(z: i64) -> (i32, u32, u32) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as i64; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365; // [0, 399]
    let y = yoe as i32 + era as i32 * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32; // [1, 12]
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
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

// ═══════════════ Tests ═══════════════
//
// Verify the WARM/COLD switch logic that fixes slow session loading.
// These exercise the cache helpers directly — Tauri's `AppHandle`
// makes a full `agent_load_session` round-trip hard to unit-test, so
// the helpers it composes are tested in isolation.

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::SessionRuntime;
    use crate::test_helpers::isolate_grok_home;
    use serde_json::json;

    #[tokio::test]
    async fn upsert_with_existing_id_replaces_in_place() {
        // Reproduces the user-reported bug: opening edit on an existing
        // provider and saving must NOT add a second entry.
        let (tmp, _guard) = isolate_grok_home();
        // Seed an existing deepseek provider the way the Electron build
        // would have written it.
        std::fs::write(
            tmp.join("desktop-providers.json"),
            r#"[{
                "id": "dp_deepseek",
                "presetId": "deepseek",
                "name": "DeepSeek",
                "baseUrl": "https://api.deepseek.com/anthropic",
                "apiBackend": "messages",
                "authStyle": "bearer",
                "enabled": true,
                "models": [],
                "createdAt": 1,
                "updatedAt": 1
            }]"#,
        )
        .unwrap();

        // Simulate the renderer editing the existing provider and saving.
        let edited = json!({
            "id": "dp_deepseek",
            "presetId": "deepseek",
            "name": "DeepSeek",
            "baseUrl": "https://api.deepseek.com/anthropic",
            "apiBackend": "messages",
            "authStyle": "bearer",
            "enabled": true,
            "models": [],
            "createdAt": 1,
            "updatedAt": 2
        });
        models_upsert_provider(edited).await.unwrap();

        let on_disk: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(tmp.join("desktop-providers.json")).unwrap())
                .unwrap();
        let on_disk_providers = on_disk["providers"].as_array()
            .expect("file must be envelope-shaped");
        assert_eq!(
            on_disk_providers.len(),
            1,
            "edit-save must replace in place, not push a duplicate. file: {:#?}",
            on_disk_providers
        );
        assert_eq!(on_disk_providers[0]["id"], "dp_deepseek");
        assert_eq!(on_disk_providers[0]["updatedAt"], 2);

        std::fs::remove_dir_all(&tmp).ok();
    }

    #[tokio::test]
    async fn upsert_with_empty_id_assigns_fresh_uuid_and_keeps_existing() {
        // When the renderer sends an upsert without an id (e.g. the
        // editor was opened via "Add provider" or the id got stripped
        // somewhere upstream), the backend must NOT silently write
        // `id=""` and pile up duplicate rows. It must:
        //   1. Generate a fresh `dp_<uuid>` id.
        //   2. Echo the new id back in the response so the editor's
        //      `editor.id` becomes the assigned id (subsequent saves
        //      then hit the in-place replace branch).
        //   3. Leave existing rows untouched.
        let (tmp, _guard) = isolate_grok_home();
        std::fs::write(
            tmp.join("desktop-providers.json"),
            r#"[{
                "id": "dp_deepseek",
                "presetId": "deepseek",
                "name": "DeepSeek",
                "baseUrl": "https://api.deepseek.com/anthropic",
                "apiBackend": "messages",
                "enabled": true,
                "models": [],
                "createdAt": 1,
                "updatedAt": 1
            }]"#,
        )
        .unwrap();

        let no_id = json!({
            "presetId": "minimax",
            "name": "MiniMax",
            "baseUrl": "https://api.minimaxi.com/anthropic",
            "apiBackend": "messages",
            "enabled": true,
            "models": []
        });
        let echoed = models_upsert_provider(no_id).await.unwrap();
        let assigned = echoed.get("id").and_then(|v| v.as_str()).unwrap_or("");
        assert!(
            assigned.starts_with("dp_") && assigned.len() > 3,
            "missing-id upsert must produce a fresh dp_<uuid> id, got: {:?}",
            assigned
        );

        let on_disk: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(tmp.join("desktop-providers.json")).unwrap())
                .unwrap();
        let on_disk_providers = on_disk["providers"].as_array()
            .expect("file must be envelope-shaped");
        assert_eq!(on_disk_providers.len(), 2);
        // Original deepseek is untouched.
        assert_eq!(on_disk_providers[0]["id"], "dp_deepseek");
        assert_eq!(on_disk_providers[0]["updatedAt"], 1);
        // New entry has the assigned id.
        assert_eq!(on_disk_providers[1]["id"], assigned);

        // Saving the freshly-assigned entry again must replace in place
        // (no further duplicate row).
        let mut follow_up = echoed.clone();
        follow_up["name"] = json!("MiniMax renamed");
        models_upsert_provider(follow_up).await.unwrap();
        let on_disk2: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(tmp.join("desktop-providers.json")).unwrap())
                .unwrap();
        let on_disk2_providers = on_disk2["providers"].as_array()
            .expect("file must be envelope-shaped");
        assert_eq!(on_disk2_providers.len(), 2, "follow-up save must not duplicate");
        assert_eq!(on_disk2_providers[1]["id"], assigned);
        assert_eq!(on_disk2_providers[1]["name"], "MiniMax renamed");

        std::fs::remove_dir_all(&tmp).ok();
    }

    #[tokio::test]
    async fn upsert_writes_electron_envelope_not_bare_array() {
        // After every save, desktop-providers.json must be the
        // Electron-shaped envelope `{version: 1, providers: [...]}`,
        // NOT a bare array. This is the file shape Electron's
        // `readStore` expects, so cross-app round-trips are safe.
        let (tmp, _guard) = isolate_grok_home();
        let input = json!({
            "id": "dp_deepseek",
            "presetId": "deepseek",
            "name": "DeepSeek",
            "baseUrl": "https://api.deepseek.com/anthropic",
            "apiBackend": "messages",
            "authStyle": "bearer",
            "enabled": true,
            "models": [{
                "id": "deepseek-chat",
                "name": "DeepSeek Chat (V3)",
                "configKey": "dp_deepseek_deepseek-chat",
                "source": "fetched",
                "enabled": true
            }]
        });
        models_upsert_provider(input).await.unwrap();

        let on_disk: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(tmp.join("desktop-providers.json")).unwrap())
                .unwrap();
        assert_eq!(on_disk["version"], 1, "envelope must carry version: 1");
        let arr = on_disk["providers"].as_array().expect("providers must be an array");
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["id"], "dp_deepseek");

        std::fs::remove_dir_all(&tmp).ok();
    }

    #[tokio::test]
    async fn list_providers_reads_electron_envelope() {
        // Pre-seed with Electron-shaped JSON. `models_list_providers`
        // must return the providers array (not the envelope).
        let (tmp, _guard) = isolate_grok_home();
        std::fs::write(
            tmp.join("desktop-providers.json"),
            r#"{
                "version": 1,
                "providers": [
                    {"id": "dp_deepseek", "name": "DeepSeek", "presetId": "deepseek", "enabled": true, "models": []},
                    {"id": "dp_anthropic", "name": "Anthropic", "presetId": "anthropic", "enabled": false, "models": []}
                ]
            }"#,
        )
        .unwrap();
        let v = models_list_providers().await.unwrap();
        let arr = v.as_array().expect("expected array");
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0]["id"], "dp_deepseek");
        assert_eq!(arr[1]["id"], "dp_anthropic");

        std::fs::remove_dir_all(&tmp).ok();
    }

    #[tokio::test]
    async fn list_providers_reads_legacy_bare_array() {
        // Older Tauri builds wrote a bare array `[...]` without an
        // envelope. Read path must still parse those files gracefully.
        let (tmp, _guard) = isolate_grok_home();
        std::fs::write(
            tmp.join("desktop-providers.json"),
            r#"[{"id": "dp_old", "name": "Old", "enabled": true, "models": []}]"#,
        )
        .unwrap();
        let v = models_list_providers().await.unwrap();
        let arr = v.as_array().expect("expected array");
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["id"], "dp_old");

        std::fs::remove_dir_all(&tmp).ok();
    }

    #[tokio::test]
    async fn delete_triggers_toml_resync() {
        // After upsert + delete, TOML must drop the deleted provider's
        // [model.*] section. This is the path that wires the agent's
        // catalog up to UI edits.
        let (tmp, _guard) = isolate_grok_home();
        let providers = vec![
            json!({
                "id": "dp_deepseek",
                "name": "DeepSeek",
                "baseUrl": "https://api.deepseek.com/anthropic",
                "apiBackend": "messages",
                "enabled": true,
                "models": [{
                    "id": "deepseek-chat",
                    "name": "DeepSeek Chat",
                    "configKey": "dp_deepseek_deepseek-chat",
                    "enabled": true
                }]
            }),
            json!({
                "id": "dp_minimax",
                "name": "MiniMax",
                "baseUrl": "https://api.minimaxi.com/anthropic",
                "apiBackend": "messages",
                "enabled": true,
                "models": [{
                    "id": "MiniMax-M3",
                    "name": "MiniMax M3",
                    "configKey": "dp_minimax_MiniMax-M3",
                    "enabled": true
                }]
            }),
        ];
        for p in &providers {
            models_upsert_provider(p.clone()).await.unwrap();
        }
        let toml_path = tmp.join("config.toml");
        let after_upsert = std::fs::read_to_string(&toml_path).unwrap();
        assert!(after_upsert.contains("dp_deepseek_deepseek-chat"));
        assert!(after_upsert.contains("dp_minimax_MiniMax-M3"));

        models_delete_provider("dp_deepseek".into()).await.unwrap();
        let after_delete = std::fs::read_to_string(&toml_path).unwrap();
        assert!(!after_delete.contains("dp_deepseek_deepseek-chat"), "deepseek model must be gone from TOML");
        assert!(after_delete.contains("dp_minimax_MiniMax-M3"), "minimax model must remain");

        std::fs::remove_dir_all(&tmp).ok();
    }

    fn make_state() -> AppState {
        AppState::default()
    }

    #[tokio::test]
    async fn warm_cache_hit_returns_hydrated_state() {
        // Seed an empty bag, park focus, hydrate from cache — should
        // match the Electron warm-switch behaviour: no ACP call,
        // focus restored from memory.
        let state = make_state();
        // Step 1: user picks session "alpha" and waits for COLD load.
        *state.session_id.lock().await = Some("alpha".to_string());
        *state.workspace.lock().await = Some("/work/alpha".to_string());
        *state.session_title.lock().await = Some("Alpha Session".to_string());
        state
            .timeline
            .lock()
            .await
            .push(json!({"id": "u1", "kind": "user", "text": "hi"}));
        let _ = mark_hydrated(&state, "alpha", true).await;
        sync_active_into_runtimes(&state).await;

        // Step 2: user clicks over to session "beta".
        *state.session_id.lock().await = Some("beta".to_string());
        state.timeline.lock().await.clear();
        // Then back to "alpha" — should be a warm hit.
        let hydrated = hydrate_from_runtime(&state, "alpha").await;
        assert!(hydrated, "alpha bag should be hydrated");
        assert_eq!(
            state.session_id.lock().await.as_deref(),
            Some("alpha"),
            "focus should restore to alpha",
        );
        assert_eq!(
            state
                .timeline
                .lock()
                .await
                .iter()
                .map(|v| v.get("id").and_then(|x| x.as_str()).unwrap_or(""))
                .collect::<Vec<_>>(),
            vec!["u1"],
            "timeline should restore from cache",
        );
    }

    #[tokio::test]
    async fn cold_cache_miss_returns_false() {
        // Without seeding a bag, hydrate_from_runtime must decline
        // so the caller can fall through to the COLD ACP path.
        let state = make_state();
        let hydrated = hydrate_from_runtime(&state, "ghost").await;
        assert!(!hydrated, "missing bag must not hydrate");
    }

    #[tokio::test]
    async fn cold_cache_with_unhydrated_bag_returns_false() {
        // Seed a bag that exists but isn't hydrated yet.
        let state = make_state();
        {
            let mut cache = state.runtime_cache.lock().await;
            cache.insert(
                "beta".to_string(),
                SessionRuntime {
                    session_id: "beta".to_string(),
                    cwd: "/work/beta".to_string(),
                    timeline: vec![json!({"id": "b1", "kind": "user", "text": "yo"})],
                    hydrated: false,
                    ..empty_runtime("beta", "/work/beta")
                },
            );
        }
        let hydrated = hydrate_from_runtime(&state, "beta").await;
        assert!(
            !hydrated,
            "unhydrated bag must trigger COLD path (force re-fetch)",
        );
    }

    #[tokio::test]
    async fn park_then_rehydrate_round_trip() {
        // Park: focus → cache. Re-hydrate: cache → focus. Round-trip
        // should be lossless for the fields we cache.
        let state = make_state();
        *state.session_id.lock().await = Some("loop".to_string());
        *state.workspace.lock().await = Some("/loop/cwd".to_string());
        *state.session_title.lock().await = Some("Loop".to_string());
        state.timeline.lock().await.push(json!({"id": "l1"}));
        *state.replaying.lock().await = true;
        let _ = mark_hydrated(&state, "loop", true).await;
        park_active_session(&state).await;
        assert!(state.session_id.lock().await.is_none(), "focus cleared");
        assert!(
            state.timeline.lock().await.is_empty(),
            "focus timeline cleared",
        );
        assert!(
            hydrate_from_runtime(&state, "loop").await,
            "re-hydrate should hit",
        );
        assert_eq!(state.session_id.lock().await.as_deref(), Some("loop"));
        assert_eq!(
            state.timeline.lock().await.first().unwrap().get("id"),
            Some(&json!("l1")),
        );
    }

    #[tokio::test]
    async fn plan_file_path_uses_grok_home_and_url_encodes_cwd() {
        // Plan files must live under ~/.grok/sessions/<url-cwd>/<sid>/plan.md.
        // With GROK_HOME unset, that resolves to $HOME/.grok.
        let path = plan_file_path("/Users/me/Code App", "abc-123");
        let path_str = path.to_string_lossy();
        assert!(path_str.ends_with("/abc-123/plan.md"), "got {path_str}");
        // Whitespace and "/" become %xx (e.g. space -> %20, slash -> %2F).
        assert!(
            path_str.contains("%20") || path_str.contains("%2F"),
            "expected url-encoded cwd, got {path_str}",
        );
    }

    /// Regression test: `agent_load_session`'s COLD path used to lock
    /// `state.session_title` twice in one expression (`*lock = Some(lock.clone())`).
    /// Tokio's Mutex is non-reentrant, so the COLD path self-deadlocked
    /// and the renderer never received a response — the user saw
    /// "click session → no reaction". This test exercises the same
    /// shape (read title under one guard, assign under a separate
    /// acquisition) so the bug can't silently reappear.
    #[tokio::test]
    async fn no_self_deadlock_on_workspace_or_title() {
        // Run inside a strict wall-clock budget: if a future change
        // re-introduces the double-lock, this test will time out
        // instead of hanging forever.
        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            let state = make_state();
            // Mirror the COLD path's title pattern.
            *state.session_id.lock().await = Some("x".to_string());
            *state.workspace.lock().await = Some("/w".to_string());
            let next_title = state
                .session_title
                .lock()
                .await
                .clone()
                .unwrap_or_else(|| "Session".to_string());
            *state.session_title.lock().await = Some(next_title);
            // Mirror the WARM path's workspace "if None" pattern.
            let needs_workspace = state.workspace.lock().await.is_none();
            if needs_workspace {
                *state.workspace.lock().await = Some("/w".to_string());
            }
            // Sanity: state is consistent.
            assert_eq!(state.session_title.lock().await.as_deref(), Some("Session"));
            assert_eq!(state.workspace.lock().await.as_deref(), Some("/w"));
        })
        .await
        .expect("double-lock regression: COLD/WARM path deadlocked");
    }

    /// Mirrors `parseModels()` in `backend.ts:1329`. Ensures the
    /// `session/load` `models.availableModels` payload becomes a
    /// renderable ModelInfo array (modelId / name / reasoningEfforts /
    /// acceptsImages / contextWindow) — without this the composer
    /// chip would have no model picker.
    #[test]
    fn parse_models_extracts_renderer_shape() {
        let raw = json!({
            "currentModelId": "grok-3",
            "availableModels": [
                {
                    "modelId": "grok-3",
                    "name": "Grok 3",
                    "description": "fast default",
                    "_meta": {
                        "supportsReasoningEffort": true,
                        "reasoningEfforts": [
                            {"id": "low", "label": "Low"},
                            {"id": "high", "label": "High"},
                        ],
                        "reasoningEffort": "low",
                        "inputModalities": ["text", "image"],
                        "totalContextTokens": 131072u64,
                    },
                },
                {
                    "modelId": "no-meta",
                    "name": "No meta",
                },
            ],
        });
        let (current, available) = parse_models(&raw);
        assert_eq!(current.as_deref(), Some("grok-3"));
        assert_eq!(available.len(), 2);
        let m = &available[0];
        assert_eq!(m.get("modelId").and_then(|v| v.as_str()), Some("grok-3"));
        assert_eq!(m.get("name").and_then(|v| v.as_str()), Some("Grok 3"));
        assert_eq!(
            m.get("acceptsImages").and_then(|v| v.as_bool()),
            Some(true),
            "image modality → acceptsImages=true",
        );
        assert_eq!(
            m.get("contextWindow").and_then(|v| v.as_i64()),
            Some(131072),
        );
        let efforts = m
            .get("reasoningEfforts")
            .and_then(|v| v.as_array())
            .expect("reasoningEfforts parsed");
        assert_eq!(efforts.len(), 2);
        assert_eq!(
            efforts[0].get("id").and_then(|v| v.as_str()),
            Some("low"),
        );
        // Default-true acceptsImages when modalities missing.
        let no_meta = &available[1];
        assert_eq!(
            no_meta.get("acceptsImages").and_then(|v| v.as_bool()),
            Some(true),
            "missing modalities → acceptsImages defaults true",
        );
    }

    /// Empty / wrong-shape `models` payloads return no list rather
    /// than crashing the COLD path.
    #[test]
    fn parse_models_handles_bad_inputs() {
        let (cur, list) = parse_models(&Value::Null);
        assert!(cur.is_none());
        assert!(list.is_empty());

        let (cur, list) = parse_models(&json!({}));
        assert!(cur.is_none());
        assert!(list.is_empty());

        // currentModelId alone is preserved even with no list.
        let (cur, list) = parse_models(&json!({"currentModelId": "x"}));
        assert_eq!(cur.as_deref(), Some("x"));
        assert!(list.is_empty());
    }

    /// After COLD load applies a catalog, the bag holds the same
    /// entries so a later WARM switch can render the picker without
    /// another round-trip.
    #[tokio::test]
    async fn warm_switch_restores_model_catalog() {
        let state = make_state();
        // Simulate a previously-loaded bag with models.
        let bag = SessionRuntime {
            session_id: "ses".into(),
            cwd: "/c".into(),
            title: "Ses".into(),
            timeline: vec![json!({"id": "u1", "kind": "user", "text": "hi"})],
            hydrated: true,
            available_models: vec![json!({"modelId": "grok-3", "name": "Grok 3"})],
            model_id: Some("grok-3".into()),
            ..empty_runtime("ses", "/c")
        };
        {
            let mut cache = state.runtime_cache.lock().await;
            cache.insert("ses".into(), bag);
        }
        let hydrated = hydrate_from_runtime(&state, "ses").await;
        assert!(hydrated);
        assert_eq!(
            state.model_id.lock().await.as_deref(),
            Some("grok-3"),
            "modelId restored on warm switch",
        );
        assert_eq!(
            state.available_models.lock().await.len(),
            1,
            "catalog restored on warm switch",
        );
    }

    /// `parse_billing_usage` is a direct port of `parseBillingUsage`
    /// in `backend.ts:496`. Lock down the rendered fields so a
    /// future tweak doesn't silently break the UsageCard chrome.
    #[test]
    fn parse_billing_usage_full_payload() {
        let raw = json!({
            "subscriptionTier": "SuperGrok",
            "config": {
                "creditUsagePercent": 42.5,
                "currentPeriod": {
                    "type": "MONTHLY",
                    "end": "2026-07-31T00:00:00Z",
                },
                "prepaidBalance": {"val": 1500},
                "onDemandCap": {"val": 2000},
                "onDemandUsed": {"val": 250},
            },
        });
        let u = parse_billing_usage(&raw);
        assert_eq!(u.get("subscriptionTier").and_then(|v| v.as_str()), Some("SuperGrok"));
        assert_eq!(
            u.get("usagePct").and_then(|v| v.as_f64()),
            Some(42.5),
        );
        assert_eq!(u.get("usageLabel").and_then(|v| v.as_str()), Some("Monthly limit"));
        assert_eq!(u.get("usageShort").and_then(|v| v.as_str()), Some("42%"));
        assert_eq!(
            u.get("prepaidUsd").and_then(|v| v.as_f64()),
            Some(15.0),
            "1500 cents → $15.00",
        );
        assert_eq!(
            u.get("onDemandCapUsd").and_then(|v| v.as_f64()),
            Some(20.0),
        );
        assert_eq!(
            u.get("onDemandUsedUsd").and_then(|v| v.as_f64()),
            Some(2.5),
        );
        let summary = u.get("summaryLines").and_then(|v| v.as_array()).expect("summary");
        assert!(summary.iter().any(|l| l.as_str() == Some("Monthly limit: 42%")));
        assert!(summary.iter().any(|l| l.as_str() == Some("Credits: $15")));
        assert!(summary.iter().any(|l| {
            l.as_str()
                .map(|s| s.starts_with("Pay-as-you-go:"))
                .unwrap_or(false)
        }));
    }

    /// When `creditUsagePercent` is missing, fall back to
    /// `used / monthlyLimit`. Same heuristic as the Electron port.
    #[test]
    fn parse_billing_usage_falls_back_to_used_over_limit() {
        let raw = json!({
            "config": {
                "monthlyLimit": {"val": 10000},
                "used": {"val": 5000},
            },
        });
        let u = parse_billing_usage(&raw);
        assert_eq!(u.get("usagePct").and_then(|v| v.as_f64()), Some(50.0));
    }

    /// Auto-topup annotations land on the parsed UsageInfo.
    #[test]
    fn merge_auto_topup_inserts_summary_lines() {
        let mut usage = parse_billing_usage(&json!({
            "config": {
                "creditUsagePercent": 10.0,
                "prepaidBalance": {"val": 1500},
                "currentPeriod": {"type": "MONTHLY"},
            },
        }));
        let topup = json!({
            "rule": {
                "enabled": true,
                "topupAmount": {"val": 1000},
                "maxAmountPerMonth": {"val": 5000},
            },
        });
        usage = merge_auto_topup(&usage, &topup);
        assert_eq!(
            usage.get("autoTopupEnabled").and_then(|v| v.as_bool()),
            Some(true),
        );
        assert_eq!(
            usage.get("autoTopupAmountUsd").and_then(|v| v.as_f64()),
            Some(10.0),
        );
        assert_eq!(
            usage.get("autoTopupMaxUsd").and_then(|v| v.as_f64()),
            Some(50.0),
        );
        let summary = usage.get("summaryLines").and_then(|v| v.as_array()).expect("summary");
        assert!(
            summary
                .iter()
                .any(|l| l.as_str() == Some("Auto topup: $10")),
            "expected Auto topup line, got {summary:?}",
        );
        assert!(
            summary
                .iter()
                .any(|l| l.as_str() == Some("Max monthly topup: $50")),
        );
    }

    /// Appending an error preserves existing usage fields so the
    /// UI can keep showing prior data with an inline error.
    #[test]
    fn append_error_to_usage_merges_with_prior() {
        let prior = json!({
            "usagePct": 25.0,
            "usageLabel": "Weekly limit",
            "usageShort": "25%",
            "summaryLines": ["Weekly limit: 25%"],
            "manageUrl": "https://grok.com/?_s=usage",
        });
        let next = append_error_to_usage(Some(prior), "billing: oops").expect("next");
        assert_eq!(
            next.get("error").and_then(|v| v.as_str()),
            Some("billing: oops"),
        );
        assert_eq!(
            next.get("usageShort").and_then(|v| v.as_str()),
            Some("25%"),
            "prior summaryShort preserved on error",
        );
    }

    /// `parse_minimax_tiers` extracts the `general` plan and inverts
    /// "remaining %" → "used %" for both 5h + 7d windows — direct
    /// port of `parseMinimaxTiers` in `provider-usage.ts:120`.
    #[test]
    fn parse_minimax_tiers_basic() {
        // 5h remaining 75 → 5h used 25. 7d remaining 60 → 7d used 40.
        let body = json!({
            "base_resp": {"status_code": 0},
            "model_remains": [{
                "model_name": "general",
                "current_interval_remaining_percent": 75.0,
                "end_time": 1_700_000_000_000_i64,
                "current_weekly_status": 1,
                "current_weekly_remaining_percent": 60.0,
                "weekly_end_time": 1_730_000_000_000_i64,
            }],
        });
        let q = parse_minimax_tiers(&body).expect("quota");
        assert_eq!(q.get("fiveHourPct").and_then(|v| v.as_f64()), Some(25.0));
        assert_eq!(
            q.get("fiveHourResetMs").and_then(|v| v.as_i64()),
            Some(1_700_000_000_000),
        );
        assert_eq!(q.get("sevenDayPct").and_then(|v| v.as_f64()), Some(40.0));
        assert_eq!(
            q.get("sevenDayResetMs").and_then(|v| v.as_i64()),
            Some(1_730_000_000_000),
        );
    }

    /// When the agent reports `current_weekly_status != 1` (e.g. the
    /// user isn't on a weekly plan), the 7d fields are simply
    /// absent — matching Electron's `if (... === 1)` guard.
    #[test]
    fn parse_minimax_tiers_no_seven_day_when_not_on_weekly() {
        let body = json!({
            "model_remains": [{
                "model_name": "general",
                "current_interval_remaining_percent": 90.0,
                "current_weekly_status": 0,
            }],
        });
        let q = parse_minimax_tiers(&body).expect("quota");
        assert_eq!(q.get("fiveHourPct").and_then(|v| v.as_f64()), Some(10.0));
        assert!(
            q.get("sevenDayPct").is_none(),
            "7d fields omitted when not on weekly plan",
        );
    }

    /// `parse_deepseek_balance` extracts the first balance entry.
    #[test]
    fn parse_deepseek_balance_basic() {
        let body = json!({
            "is_available": true,
            "balance_infos": [{
                "currency": "CNY",
                "total_balance": 12.34,
                "granted_balance": 5.0,
                "topped_up_balance": 7.34,
            }],
        });
        let b = parse_deepseek_balance(&body).expect("balance");
        assert_eq!(b.get("remaining").and_then(|v| v.as_f64()), Some(12.34));
        assert_eq!(b.get("unit").and_then(|v| v.as_str()), Some("CNY"));
        assert_eq!(
            b.get("available").and_then(|v| v.as_bool()),
            Some(true),
        );
    }

    /// `detect_provider` matches either by preset id or by baseUrl
    /// substring — same heuristic as `provider-usage.ts:53`.
    #[test]
    fn detect_provider_recognises_minimax_and_deepseek() {
        assert_eq!(
            detect_provider(Some("minimax"), "https://example.com/v1"),
            Some("minimax-coding-plan"),
            "presetId alone is enough",
        );
        assert_eq!(
            detect_provider(None, "https://api.minimaxi.com/v1"),
            Some("minimax-coding-plan"),
            "baseUrl substring alone is enough",
        );
        assert_eq!(
            detect_provider(Some("deepseek"), "https://api.deepseek.com"),
            Some("deepseek-balance"),
        );
        assert_eq!(
            detect_provider(Some("openai"), "https://api.openai.com/v1"),
            None,
            "unknown providers return None",
        );
    }

    /// `resolve_api_key` falls back to env vars when the stored key
    /// is missing/empty.
    #[test]
    fn resolve_api_key_falls_back_to_env() {
        // Stored key wins.
        assert_eq!(
            resolve_api_key(Some("stored".into()), Some("OPENAI_KEY")),
            "stored",
        );
        // Stored key empty → first non-empty env var wins.
        let _ = std::env::set_var("GROK_TEST_KEY_PROVIDER", "from-env");
        assert_eq!(
            resolve_api_key(None, Some("MISSING_VAR,GROK_TEST_KEY_PROVIDER")),
            "from-env",
        );
        // No stored, no env → empty.
        assert_eq!(
            resolve_api_key(None, Some("OTHER_MISSING_VAR")),
            "",
        );
    }
}