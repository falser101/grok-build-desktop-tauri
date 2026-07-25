//! Synchronise `~/.grok/desktop-providers.json` into `~/.grok/config.toml`
//! as a marker-delimited managed block of `[model.<key>]` sections.
//!
//! Mirrors `grok-build-desktop/src/main/model-providers.ts:541-654`:
//!   * `buildModelSection` (line 541): per-model section writer.
//!   * `syncConfigToml` (line 589): rewrite the marker block + remove
//!     orphan `[model.dp_*]` sections outside the marker.
//!
//! Why both files exist:
//!   * JSON sidecar = UI canonical state (enabled, presetId, source,
//!     timestamps, authStyle, reasoning-effort sentinels, full inventory).
//!   * TOML = projection for `grok agent serve`, which only reads
//!     `config.toml` to discover custom models
//!     (`xai-grok-shell/src/agent/server.rs:42-75`).
//!
//! Without the TOML half the Tauri Models page would let users add
//! providers, but the agent would never see them. With the TOML half
//! alone, the UI loses too much state.

use anyhow::{anyhow, Result};
use serde_json::{json, Value};

/// Marker comments Electron uses around the managed block. Tauri writes
/// the exact same markers so a Tauri-edited config.toml is also
/// recognisable to Electron's `syncConfigToml` for round-tripping.
const MARKER_START: &str = "# >>> grok-desktop-models";
const MARKER_END: &str = "# <<< grok-desktop-models";

/// Number of characters a provider id segment is trimmed to when
/// forming a config.toml section key. Mirrors Electron's
/// `sanitizeSegment` (model-providers.ts:371-377) default.
const SANITIZE_MAX: usize = 28;

/// Sanitise a free-form string into the segment allowed in a TOML
/// section key. Mirrors `sanitizeSegment` from model-providers.ts:371.
fn sanitize_segment(s: &str, max: usize) -> String {
    let cleaned: String = s
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect::<String>()
        .replace("--", "-");
    let trimmed = cleaned.trim_matches('-');
    let out = if trimmed.is_empty() {
        "x".to_string()
    } else {
        trimmed.chars().take(max).collect::<String>()
    };
    if out.is_empty() {
        "x".to_string()
    } else {
        out
    }
}

/// Build a stable `[model.<key>]` config key for a (provider, model)
/// pair. Mirrors `makeConfigKey` from model-providers.ts:380-384.
fn make_config_key(provider_id: &str, model_id: &str) -> String {
    let p = sanitize_segment(provider_id, SANITIZE_MAX);
    let m = sanitize_segment(&model_id.replace('/', "-"), 56);
    format!("dp_{}_{}", p, m)
}

// ── TOML escape / render helpers ──────────────────────────────────────

fn toml_escape(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
        .replace('\t', "\\t")
}

fn toml_string(s: &str) -> String {
    format!("\"{}\"", toml_escape(s))
}

fn toml_inline_table(obj: &serde_json::Map<String, Value>) -> String {
    let parts: Vec<String> = obj
        .iter()
        .map(|(k, v)| {
            let v_str = match v {
                Value::String(s) => toml_string(s),
                other => other.to_string(),
            };
            format!("{} = {}", toml_string(k), v_str)
        })
        .collect();
    format!("{{ {} }}", parts.join(", "))
}

/// Render one `[model.<key>]` section's text block.
fn build_model_section(provider: &Value, model: &Value) -> String {
    let mut lines: Vec<String> = Vec::new();

    // `configKey` is authoritative for the section name. Fall back to
    // `make_config_key` if absent (e.g. JSON written by an older
    // Tauri/Electron build that didn't yet persist `configKey`).
    let section_name = model
        .get("configKey")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(String::from)
        .unwrap_or_else(|| {
            make_config_key(
                provider.get("id").and_then(|v| v.as_str()).unwrap_or("provider"),
                model.get("id").and_then(|v| v.as_str()).unwrap_or("model"),
            )
        });
    lines.push(format!("[model.{}]", section_name));

    let model_id = model.get("id").and_then(|v| v.as_str()).unwrap_or("");
    lines.push(format!("model = {}", toml_string(model_id)));

    if let Some(base_url) = provider.get("baseUrl").and_then(|v| v.as_str()) {
        if !base_url.is_empty() {
            lines.push(format!("base_url = {}", toml_string(base_url)));
        }
    }

    let name = model
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or(model_id);
    lines.push(format!("name = {}", toml_string(name)));

    let provider_name = provider
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("Provider");
    lines.push(format!("description = {}", toml_string(provider_name)));

    let api_backend = provider
        .get("apiBackend")
        .and_then(|v| v.as_str())
        .unwrap_or("chat_completions");
    lines.push(format!("api_backend = {}", toml_string(api_backend)));

    if let Some(cw) = model.get("contextWindow").and_then(|v| v.as_u64()) {
        if cw > 0 {
            lines.push(format!("context_window = {}", cw));
        }
    }

    let mut headers: serde_json::Map<String, Value> = serde_json::Map::new();
    if let Some(Value::Object(extras)) = provider.get("extraHeaders") {
        for (k, v) in extras {
            headers.insert(k.clone(), v.clone());
        }
    }
    let auth_style = provider.get("authStyle").and_then(|v| v.as_str());
    let api_key = provider.get("apiKey").and_then(|v| v.as_str()).unwrap_or("");

    // Electron behaviour: x-api-key authStyle routes the key through the
    // `extra_headers` map; otherwise it lives as a top-level `api_key`.
    if auth_style == Some("x-api-key") && !api_key.is_empty() {
        headers.insert("x-api-key".to_string(), Value::String(api_key.to_string()));
    } else if !api_key.is_empty() {
        lines.push(format!("api_key = {}", toml_string(api_key)));
    }
    if let Some(env_key) = provider.get("envKey").and_then(|v| v.as_str()) {
        if !env_key.is_empty() {
            lines.push(format!("env_key = {}", toml_string(env_key)));
        }
    }
    if !headers.is_empty() {
        lines.push(format!("extra_headers = {}", toml_inline_table(&headers)));
    }

    // Reasoning-effort menu. Only emit when the list is non-empty;
    // explicit `[]` is intentionally omitted (matches Electron and the
    // Rust override parser's "nonempty-only" rule).
    if let Some(Value::Array(items)) = model.get("reasoningEfforts") {
        if !items.is_empty() {
            let rendered: Vec<String> = items
                .iter()
                .filter_map(|item| {
                    let obj = item.as_object()?;
                    let value = obj
                        .get("value")
                        .and_then(|v| v.as_str())
                        .or_else(|| obj.get("id").and_then(|v| v.as_str()))?;
                    let label = obj
                        .get("label")
                        .and_then(|v| v.as_str())
                        .unwrap_or(value);
                    let mut parts = vec![format!("value = {}", toml_string(value))];
                    if let Some(id) = obj.get("id").and_then(|v| v.as_str()) {
                        parts.push(format!("id = {}", toml_string(id)));
                    }
                    parts.push(format!("label = {}", toml_string(label)));
                    if let Some(desc) = obj.get("description").and_then(|v| v.as_str()) {
                        parts.push(format!("description = {}", toml_string(desc)));
                    }
                    if obj
                        .get("default")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false)
                    {
                        parts.push("default = true".to_string());
                    }
                    Some(format!("{{ {} }}", parts.join(", ")))
                })
                .collect();
            if !rendered.is_empty() {
                lines.push(format!("reasoning_efforts = [{}]", rendered.join(", ")));
            }
        }
    }

    lines.join("\n")
}

/// Read the current `~/.grok/config.toml`, strip our previous managed
/// block (between markers) AND any orphan `[model.dp_*]` sections
/// outside markers, then re-emit the managed block from the current
/// providers list. Unknown sections survive untouched.
///
/// Returns the post-write `config.toml` content as a String so callers
/// can assert on it. Atomic on disk via tmp + rename, preserving Unix
/// file mode.
pub fn sync_config_toml(providers: &[Value]) -> Result<String> {
    let path = crate::paths::grok_home().join("config.toml");
    let original = std::fs::read_to_string(&path).unwrap_or_default();

    // Strip the previous marker block (if any).
    let stripped = match (original.find(MARKER_START), original.find(MARKER_END)) {
        (Some(s), Some(e)) if s < e => {
            let before = &original[..s];
            let after_start = e + MARKER_END.len();
            let after = original[after_start..].trim_start_matches('\n');
            format!("{}\n{}", before.trim_end_matches('\n'), after)
        }
        _ => original.clone(),
    };

    // Strip orphan `[model.dp_*]` sections outside the marker.
    let mut cleaned_lines: Vec<&str> = Vec::new();
    let mut skip = false;
    for line in stripped.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            skip = trimmed.starts_with("[model.dp_");
        }
        if !skip {
            cleaned_lines.push(line);
        }
    }
    let mut text = cleaned_lines.join("\n");
    // Collapse any 3+ blank lines left behind by the strip operations
    // and trim trailing whitespace so the appended block lands flush.
    while text.contains("\n\n\n") {
        text = text.replace("\n\n\n", "\n\n");
    }
    text = text.trim_end_matches('\n').to_string();

    // Build the new managed block from enabled providers + enabled models.
    let mut sections: Vec<String> = Vec::new();
    for provider in providers {
        let enabled = provider
            .get("enabled")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);
        if !enabled {
            continue;
        }
        let models = provider.get("models").and_then(|v| v.as_array());
        let Some(models) = models else { continue };
        for model in models {
            let model_enabled = model
                .get("enabled")
                .and_then(|v| v.as_bool())
                .unwrap_or(true);
            if !model_enabled {
                continue;
            }
            sections.push(build_model_section(provider, model));
        }
    }

    let mut out = text;
    if !sections.is_empty() {
        let block_lines = std::iter::once(String::new())
            .chain(std::iter::once(MARKER_START.to_string()))
            .chain(std::iter::once(
                "# Managed by Grok Build Desktop — edit via Models settings UI".to_string(),
            ))
            .chain(std::iter::once(String::new()))
            .chain(
                sections
                    .iter()
                    .enumerate()
                    .flat_map(|(i, s)| {
                        if i == 0 {
                            vec![s.clone()]
                        } else {
                            vec![String::new(), s.clone()]
                        }
                    }),
            )
            .chain(std::iter::once(String::new()))
            .chain(std::iter::once(MARKER_END.to_string()))
            .chain(std::iter::once(String::new()));
        let block: String = block_lines.collect::<Vec<_>>().join("\n");
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(&block);
    } else if !out.is_empty() {
        out.push('\n');
    }

    // Atomic write: tmp + rename, preserving Unix file mode if the
    // file already exists.
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let prior_mode: Option<u32> = {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::metadata(&path)
                .ok()
                .map(|m| m.permissions().mode())
        }
        #[cfg(not(unix))]
        {
            None
        }
    };
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let tmp = path.with_extension(format!("toml.tmp.{}.{}", std::process::id(), nanos));
    std::fs::write(&tmp, &out)?;
    #[cfg(unix)]
    if let Some(mode) = prior_mode {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(mode));
    }
    std::fs::rename(&tmp, &path)?;

    Ok(out)
}

// ── Stub for reload_agent_models ─────────────────────────────────────

/// Trigger the running `grok agent serve` to pick up the new
/// `config.toml`. Today this calls `grok --reload-models` via the CLI
/// helper; if the agent doesn't support hot-reload yet, the user will
/// need to reconnect the session before custom models show up in the
/// composer picker.
///
/// Mirror of `main/index.ts:941-988` in Electron, which fires a
/// `agent-reload-models` IPC after every `syncConfigToml`.
pub async fn reload_agent_models_inner() -> Result<()> {
    crate::grok_cli::run(&["--reload-models"])
        .await
        .map(|_| ())
        .map_err(|e| anyhow!("reload agent: {}", e))
}

// Keep the `Value` import alive when this file is built standalone
// (the import is used inside `build_model_section` via the helper).
#[allow(dead_code)]
fn _value_anchor() -> Value {
    json!({})
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_helpers::isolate_grok_home;

    fn deepseek(enabled: bool) -> Value {
        json!({
            "id": "dp_deepseek",
            "presetId": "deepseek",
            "name": "DeepSeek",
            "baseUrl": "https://api.deepseek.com/anthropic",
            "apiBackend": "messages",
            "authStyle": "bearer",
            "enabled": enabled,
            "models": [{
                "id": "deepseek-chat",
                "name": "DeepSeek Chat (V3)",
                "configKey": "dp_deepseek_deepseek-chat",
                "source": "fetched",
                "enabled": true,
                "reasoningEfforts": []
            }]
        })
    }

    #[test]
    fn sanitize_segment_strips_slashes_and_punctuation() {
        let s = sanitize_segment("anthropic/Claude:Opus 4.6", 28);
        // Slashes are turned into dashes; colons/spaces likewise.
        assert!(s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'));
        assert!(s.len() <= 28);
    }

    #[test]
    fn make_config_key_includes_dp_prefix() {
        let key = make_config_key("deepseek", "deepseek-chat");
        assert!(key.starts_with("dp_"));
        assert!(key.contains("deepseek"));
        assert!(key.contains("deepseek-chat"));
    }

    #[test]
    fn build_model_section_emits_bearer_api_key_inline() {
        let p = json!({
            "id": "dp_deepseek",
            "name": "DeepSeek",
            "baseUrl": "https://api.deepseek.com/anthropic",
            "apiBackend": "messages",
            "authStyle": "bearer",
            "apiKey": "sk-abc",
        });
        let m = json!({
            "id": "deepseek-chat",
            "name": "DeepSeek Chat",
            "configKey": "dp_deepseek_deepseek-chat",
            "enabled": true,
        });
        let text = build_model_section(&p, &m);
        assert!(text.contains("[model.dp_deepseek_deepseek-chat]"));
        assert!(text.contains("api_key = \"sk-abc\""));
        assert!(!text.contains("x-api-key"));
        assert!(text.contains("description = \"DeepSeek\""));
    }

    #[test]
    fn build_model_section_routes_x_api_key_into_extra_headers() {
        let p = json!({
            "id": "dp_anthropic",
            "name": "Anthropic",
            "baseUrl": "https://api.anthropic.com/v1",
            "apiBackend": "messages",
            "authStyle": "x-api-key",
            "apiKey": "sk-ant-123",
            "extraHeaders": { "anthropic-version": "2023-06-01" }
        });
        let m = json!({
            "id": "claude-opus-4-6",
            "name": "Claude Opus 4.6",
            "configKey": "dp_anthropic_claude-opus-4-6",
            "enabled": true,
        });
        let text = build_model_section(&p, &m);
        // x-api-key in extra_headers, no top-level api_key
        assert!(text.contains("\"x-api-key\" = \"sk-ant-123\""), "missing header: {}", text);
        assert!(!text.contains("api_key = "), "x-api-key auth must not write api_key line");
        assert!(text.contains("\"anthropic-version\" = \"2023-06-01\""));
    }

    #[test]
    fn build_model_section_skips_empty_reasoning_efforts() {
        let p = json!({"name": "P", "baseUrl": "https://x", "apiBackend": "chat_completions"});
        let m = json!({
            "id": "m",
            "name": "M",
            "configKey": "dp_x_m",
            "enabled": true,
            "reasoningEfforts": []
        });
        let text = build_model_section(&p, &m);
        assert!(!text.contains("reasoning_efforts"));
    }

    #[test]
    fn build_model_section_emits_reasoning_efforts_when_nonempty() {
        let p = json!({"name": "P", "baseUrl": "https://x", "apiBackend": "chat_completions"});
        let m = json!({
            "id": "m",
            "name": "M",
            "configKey": "dp_x_m",
            "enabled": true,
            "reasoningEfforts": [
                {"value": "xhigh", "id": "xhigh", "label": "Extra high"},
                {"value": "low", "id": "low", "label": "Low", "description": "Faster", "default": true}
            ]
        });
        let text = build_model_section(&p, &m);
        assert!(text.contains("reasoning_efforts = [{ value = \"xhigh\""));
        assert!(text.contains("default = true"));
        assert!(text.contains("description = \"Faster\""));
        assert!(text.contains("label = \"Extra high\""));
    }

    #[test]
    fn build_model_section_falls_back_to_made_config_key() {
        // No `configKey` provided on the model — function synthesises one.
        // `make_config_key` keeps the `dp_` prefix on the provider id
        // (mirrors Electron's exact behaviour: section names end up as
        // `dp_dp_<preset>_<model>`). Asserting on that gives us a
        // round-trip with `get_config_key_index`.
        let p = json!({"id": "dp_a", "name": "A", "baseUrl": "https://x", "apiBackend": "chat_completions"});
        let m = json!({"id": "model-x", "name": "X", "enabled": true});
        let text = build_model_section(&p, &m);
        assert!(text.contains("[model.dp_dp_a_model-x]"));
    }

    #[test]
    fn build_model_section_emits_env_key() {
        let p = json!({
            "name": "P", "baseUrl": "https://x",
            "apiBackend": "chat_completions", "envKey": "DEEPSEEK_API_KEY"
        });
        let m = json!({"id": "m", "name": "M", "configKey": "dp_x_m", "enabled": true});
        let text = build_model_section(&p, &m);
        assert!(text.contains("env_key = \"DEEPSEEK_API_KEY\""));
    }

    #[test]
    fn sync_config_toml_writes_only_enabled_models() {
        let p1 = json!({
            "id": "dp_deepseek", "name": "DeepSeek",
            "baseUrl": "https://api.deepseek.com/anthropic",
            "apiBackend": "messages", "enabled": true,
            "models": [
                {"id": "deepseek-chat", "configKey": "dp_deepseek_deepseek-chat", "name": "DeepSeek Chat", "enabled": true},
                {"id": "deepseek-reasoner", "configKey": "dp_deepseek_deepseek-reasoner", "name": "DeepSeek Reasoner", "enabled": false}
            ]
        });
        let p2 = json!({
            "id": "dp_anthropic", "name": "Anthropic",
            "baseUrl": "https://api.anthropic.com/v1",
            "apiBackend": "messages", "enabled": false,
            "models": [
                {"id": "claude-opus-4-6", "configKey": "dp_anthropic_claude-opus-4-6", "name": "Opus", "enabled": true}
            ]
        });
        let out = sync_config_toml(&[p1, p2]).unwrap();
        assert!(out.contains("dp_deepseek_deepseek-chat"));
        assert!(!out.contains("dp_deepseek_deepseek-reasoner"), "disabled model should not appear");
        assert!(!out.contains("dp_anthropic_claude-opus-4-6"), "provider disabled — none of its models should appear");
        assert!(out.contains(MARKER_START));
        assert!(out.contains(MARKER_END));
    }

    #[test]
    fn sync_config_toml_replaces_prior_marker_block() {
        let (dir, _guard) = isolate_grok_home();
        let toml_path = dir.join("config.toml");

        // Pre-seed with stale content (a marker block listing a no-longer-
        // existing provider, plus an unrelated user `[model.x]`).
        std::fs::write(
            &toml_path,
            format!(
                "[model.user-thing]\nmodel = \"foo\"\nbase_url = \"https://u/v1\"\n\n\
                 {start}\n[model.dp_stale_model]\nmodel = \"stale\"\n{end}\n",
                start = MARKER_START,
                end = MARKER_END,
            ),
        )
        .unwrap();

        let p = deepseek(true);
        sync_config_toml(&[p]).unwrap();

        let final_text = std::fs::read_to_string(&toml_path).unwrap();
        assert!(final_text.contains("dp_deepseek_deepseek-chat"), "fresh provider should be present");
        assert!(!final_text.contains("dp_stale_model"), "stale model inside marker should be gone");
        assert!(final_text.contains("[model.user-thing]"), "user-written section outside marker must survive");
        assert!(final_text.contains(MARKER_START));
        assert!(final_text.contains(MARKER_END));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn sync_config_toml_removes_orphan_dp_sections_outside_marker() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!(
            "grok-toml-orphan-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::env::set_var("GROK_HOME", &dir);
        let toml_path = dir.join("config.toml");

        // Hand-written `[model.dp_orphan]` outside any marker block.
        // Electron's syncConfigToml would also strip these; we mirror
        // that behaviour so users who fork-edit TOML don't leak stale
        // `dp_*` sections.
        std::fs::write(
            &toml_path,
            format!(
                "[model.dp_orphan]\nmodel = \"o\"\nbase_url = \"https://o/v1\"\n\n\
                 {start}\n{end}\n",
                start = MARKER_START,
                end = MARKER_END,
            ),
        )
        .unwrap();

        let p = deepseek(true);
        sync_config_toml(&[p]).unwrap();
        let final_text = std::fs::read_to_string(&toml_path).unwrap();
        assert!(!final_text.contains("dp_orphan"), "orphan outside marker should be removed");
        assert!(final_text.contains("dp_deepseek_deepseek-chat"));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn sync_config_toml_preserves_unrelated_sections() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!(
            "grok-toml-keep-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::env::set_var("GROK_HOME", &dir);
        let toml_path = dir.join("config.toml");

        // Pre-existing CLI config sections, unrelated to providers,
        // must survive.
        std::fs::write(
            &toml_path,
            "[cli]\ntheme = \"dark\"\n[models]\ndefault = \"grok-build\"\n",
        )
        .unwrap();

        let p = deepseek(true);
        sync_config_toml(&[p]).unwrap();
        let final_text = std::fs::read_to_string(&toml_path).unwrap();
        assert!(final_text.contains("[cli]"));
        assert!(final_text.contains("theme = \"dark\""));
        assert!(final_text.contains("[models]"));
        assert!(final_text.contains("default = \"grok-build\""));
        assert!(final_text.contains("dp_deepseek_deepseek-chat"));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn sync_config_toml_with_no_providers_removes_marker_block() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!(
            "grok-toml-empty-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::env::set_var("GROK_HOME", &dir);
        let toml_path = dir.join("config.toml");

        std::fs::write(
            &toml_path,
            format!(
                "[cli]\nx = 1\n\n{start}\n[model.dp_anything]\nmodel = \"a\"\n{end}\n",
                start = MARKER_START,
                end = MARKER_END,
            ),
        )
        .unwrap();

        // Sync with no providers → marker block disappears.
        sync_config_toml(&[]).unwrap();
        let final_text = std::fs::read_to_string(&toml_path).unwrap();
        assert!(!final_text.contains(MARKER_START));
        assert!(!final_text.contains(MARKER_END));
        assert!(!final_text.contains("dp_anything"));
        assert!(final_text.contains("[cli]"));

        std::fs::remove_dir_all(&dir).ok();
    }
}