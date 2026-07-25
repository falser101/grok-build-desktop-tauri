//! Fetch the model list from a provider's `/v1/models` endpoint.
//!
//! Port of `fetchProviderModels` from
//! `grok-build-desktop/src/main/model-providers.ts:857-968`. Used by
//! Settings → Models → "Fetch models" so the user can populate the model
//! list without typing IDs by hand.
//!
//! Protocol-wise this is plain HTTP (not bridged through `grok agent
//! serve`); the agent isn't running when the user is configuring
//! providers, and the `/v1/models` endpoint is standardised enough to
//! hit directly.

use std::collections::HashSet;
use std::time::Duration;

use anyhow::{anyhow, Result};
use serde_json::{json, Value};

/// Input shape matches what `ModelsView.tsx:onFetch` sends to
/// `desktop.fetchProviderModels`. Field names are camelCase to match
/// the renderer's `FetchProviderModelsInput` type.
#[derive(Debug, Clone)]
pub struct FetchInput {
    pub base_url: String,
    pub api_key: Option<String>,
    pub env_key: Option<String>,
    pub auth_style: Option<String>,
    pub extra_headers: Option<Value>,
}

const FETCH_TIMEOUT: Duration = Duration::from_secs(30);

/// Build the absolute URL for the models endpoint.
///
/// Mirrors `joinModelsUrl` in model-providers.ts:851 — strips trailing
/// slashes and appends `/models` unless the path already ends in it.
fn join_models_url(base_url: &str) -> String {
    let trimmed = base_url.trim().trim_end_matches('/');
    if trimmed.is_empty() {
        return trimmed.to_string();
    }
    if trimmed.ends_with("/models") {
        return trimmed.to_string();
    }
    format!("{}/models", trimmed)
}

/// Resolve the API key: prefer the literal `apiKey`, fall back to the
/// `envKey` (which may list several env var names separated by `,` or
/// `|`). Matches model-providers.ts:869-879.
fn resolve_api_key(api_key: Option<&str>, env_key: Option<&str>) -> Option<String> {
    if let Some(k) = api_key.map(str::trim).filter(|s| !s.is_empty()) {
        return Some(k.to_string());
    }
    if let Some(ek) = env_key {
        for k in ek.split(|c| c == ',' || c == '|').map(str::trim).filter(|s| !s.is_empty()) {
            if let Ok(v) = std::env::var(k) {
                let v = v.trim();
                if !v.is_empty() {
                    return Some(v.to_string());
                }
            }
        }
    }
    None
}

/// Build the auth header for the outgoing request.
fn auth_header(api_key: &str, auth_style: Option<&str>) -> Option<(&'static str, String)> {
    if api_key.is_empty() {
        return None;
    }
    if auth_style == Some("x-api-key") {
        Some(("x-api-key", api_key.to_string()))
    } else {
        Some(("Authorization", format!("Bearer {}", api_key)))
    }
}

/// Parse an OpenAI-style `/v1/models` response into the canonical
/// `FetchedModelInfo[]` shape. Tolerates three layouts the user might
/// hit:
///
///   1. Top-level array: `["a", "b"]` or `[{id, name}, ...]`
///   2. Wrapped in `.data`: `{"data": [...]}`  (OpenAI default)
///   3. Wrapped in `.models` or `.items`: `{models: [...]}` / `{items: [...]}`
///
/// Each item can be a plain string id or `{id|name|model, name?, owned_by?}`.
/// Duplicates are dropped, results sorted by id. Mirrors
/// `parseModelsResponse` in model-providers.ts:921-967.
pub fn parse_models_response(json: &Value) -> Vec<Value> {
    let mut out: Vec<Value> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();

    let mut push = |id: &str, name: Option<&str>, owned_by: Option<&str>| {
        let mid = id.trim();
        if mid.is_empty() || !seen.insert(mid.to_string()) {
            return;
        }
        let mut o = json!({
            "id": mid,
            "name": name.unwrap_or(mid).trim(),
        });
        if let Some(o2) = owned_by.map(str::trim).filter(|s| !s.is_empty()) {
            o.as_object_mut().unwrap().insert(
                "ownedBy".to_string(),
                Value::String(o2.to_string()),
            );
        }
        out.push(o);
    };

    if let Value::Array(items) = json {
        for item in items {
            match item {
                Value::String(s) => push(s, None, None),
                Value::Object(_) => {
                    let o = item.as_object().unwrap();
                    let id = o
                        .get("id")
                        .or_else(|| o.get("model"))
                        .or_else(|| o.get("name"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    if id.is_empty() {
                        continue;
                    }
                    let name = o.get("name").and_then(|v| v.as_str());
                    push(id, name, None);
                }
                _ => {}
            }
        }
    } else if let Value::Object(root) = json {
        let data = root
            .get("data")
            .or_else(|| root.get("models"))
            .or_else(|| root.get("items"));
        if let Some(Value::Array(items)) = data {
            for item in items {
                match item {
                    Value::String(s) => push(s, None, None),
                    Value::Object(_) => {
                        let o = item.as_object().unwrap();
                        let id = o
                            .get("id")
                            .or_else(|| o.get("model"))
                            .or_else(|| o.get("name"))
                            .and_then(|v| v.as_str())
                            .unwrap_or("");
                        if id.is_empty() {
                            continue;
                        }
                        let name = o.get("name").and_then(|v| v.as_str());
                        let owned_by = o.get("owned_by").and_then(|v| v.as_str());
                        push(id, name, owned_by);
                    }
                    _ => {}
                }
            }
        }
    }

    out.sort_by(|a, b| {
        let ai = a.get("id").and_then(|v| v.as_str()).unwrap_or("");
        let bi = b.get("id").and_then(|v| v.as_str()).unwrap_or("");
        ai.cmp(bi)
    });
    out
}

/// Fetch the model list. Returns the parsed array on success, or an
/// error message containing the HTTP status + body snippet on failure.
pub async fn fetch_provider_models(input: FetchInput) -> Result<Vec<Value>> {
    let base_url = input.base_url.trim().to_string();
    if base_url.is_empty() {
        return Err(anyhow!("Base URL is required to fetch models"));
    }

    let api_key = resolve_api_key(
        input.api_key.as_deref(),
        input.env_key.as_deref(),
    );

    let mut headers: Vec<(String, String)> = vec![("Accept".to_string(), "application/json".to_string())];
    if let Some(extra) = input.extra_headers.as_ref().and_then(|v| v.as_object()) {
        for (k, v) in extra {
            if let Some(s) = v.as_str() {
                headers.push((k.clone(), s.to_string()));
            }
        }
    }
    if let Some(key) = api_key.as_deref() {
        if let Some((name, value)) = auth_header(key, input.auth_style.as_deref()) {
            headers.push((name.to_string(), value));
        }
    }

    let url = join_models_url(&base_url);
    let client = reqwest::Client::builder()
        .timeout(FETCH_TIMEOUT)
        .build()
        .map_err(|e| anyhow!("build HTTP client: {}", e))?;
    let mut req = client.get(&url);
    for (k, v) in &headers {
        req = req.header(k.as_str(), v.as_str());
    }

    let resp = req.send().await.map_err(|e| {
        if e.is_timeout() {
            anyhow!("Fetch models timed out after {:?}", FETCH_TIMEOUT)
        } else {
            anyhow!("fetch {}: {}", url, e)
        }
    })?;

    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        let snippet: String = body.chars().take(200).collect();
        return Err(anyhow!(
            "HTTP {} {}{}",
            status.as_u16(),
            status.canonical_reason().unwrap_or(""),
            if snippet.is_empty() { String::new() } else { format!(": {}", snippet) }
        ));
    }

    let json: Value = resp.json().await
        .map_err(|e| anyhow!("decode response: {}", e))?;
    Ok(parse_models_response(&json))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn join_models_url_strips_trailing_slashes() {
        assert_eq!(join_models_url("https://api.deepseek.com/"), "https://api.deepseek.com/models");
        assert_eq!(join_models_url("https://api.deepseek.com///"), "https://api.deepseek.com/models");
    }

    #[test]
    fn join_models_url_no_double_models() {
        assert_eq!(join_models_url("https://api.deepseek.com/models"), "https://api.deepseek.com/models");
        assert_eq!(join_models_url("https://api.deepseek.com/models/"), "https://api.deepseek.com/models");
    }

    #[test]
    fn join_models_url_handles_empty() {
        assert_eq!(join_models_url(""), "");
        assert_eq!(join_models_url("/"), "");
    }

    #[test]
    fn parse_top_level_array_of_strings() {
        let v = json!(["deepseek-chat", "deepseek-reasoner"]);
        let parsed = parse_models_response(&v);
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0]["id"], "deepseek-chat");
        assert_eq!(parsed[0]["name"], "deepseek-chat");
    }

    #[test]
    fn parse_top_level_array_of_objects() {
        let v = json!([
            {"id": "gpt-4o", "name": "GPT-4o"},
            {"id": "o3"},
        ]);
        let parsed = parse_models_response(&v);
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0]["id"], "gpt-4o");
        assert_eq!(parsed[0]["name"], "GPT-4o");
        assert_eq!(parsed[1]["id"], "o3");
        // Falls back to id when name missing.
        assert_eq!(parsed[1]["name"], "o3");
    }

    #[test]
    fn parse_openai_data_envelope() {
        let v = json!({
            "object": "list",
            "data": [
                {"id": "deepseek-chat", "object": "model", "owned_by": "deepseek"},
                {"id": "deepseek-reasoner", "object": "model", "owned_by": "deepseek"}
            ]
        });
        let parsed = parse_models_response(&v);
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0]["id"], "deepseek-chat");
        assert_eq!(parsed[0]["ownedBy"], "deepseek");
        assert_eq!(parsed[1]["id"], "deepseek-reasoner");
    }

    #[test]
    fn parse_models_envelope() {
        let v = json!({"models": [{"model": "claude-opus-4-6", "name": "Opus"}]});
        let parsed = parse_models_response(&v);
        assert_eq!(parsed.len(), 1);
        // Falls back through id → model → name.
        assert_eq!(parsed[0]["id"], "claude-opus-4-6");
        assert_eq!(parsed[0]["name"], "Opus");
    }

    #[test]
    fn parse_items_envelope() {
        let v = json!({"items": ["a", "b", "c"]});
        let parsed = parse_models_response(&v);
        assert_eq!(parsed.len(), 3);
        assert_eq!(parsed[0]["id"], "a");
        assert_eq!(parsed[2]["id"], "c");
    }

    #[test]
    fn parse_dedups_and_sorts() {
        let v = json!({
            "data": [
                {"id": "z"},
                {"id": "a"},
                {"id": "z"}, // dup
                {"id": "m"}
            ]
        });
        let parsed = parse_models_response(&v);
        assert_eq!(parsed.len(), 3);
        let ids: Vec<&str> = parsed.iter().map(|p| p["id"].as_str().unwrap()).collect();
        assert_eq!(ids, vec!["a", "m", "z"]);
    }

    #[test]
    fn parse_drops_empty_ids() {
        // Truly empty entries (no id/model/name fields) are dropped; an
        // entry with only `id: ""` also drops. Real ids survive.
        let v = json!([
            {"id": ""},
            {"id": "real"},
            {"foo": "bar"}
        ]);
        let parsed = parse_models_response(&v);
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0]["id"], "real");
    }

    #[test]
    fn parse_unknown_shape_returns_empty() {
        assert_eq!(parse_models_response(&json!({"foo": "bar"})).len(), 0);
        assert_eq!(parse_models_response(&json!(null)).len(), 0);
        assert_eq!(parse_models_response(&json!("not an array")).len(), 0);
    }

    #[test]
    fn resolve_api_key_prefers_literal_over_env() {
        // Env is set but literal wins.
        std::env::set_var("MODELS_TEST_KEY", "from-env");
        let got = resolve_api_key(Some("from-literal"), Some("MODELS_TEST_KEY"));
        assert_eq!(got.as_deref(), Some("from-literal"));
        std::env::remove_var("MODELS_TEST_KEY");
    }

    #[test]
    fn resolve_api_key_falls_back_to_env() {
        std::env::set_var("MODELS_TEST_KEY", "from-env");
        let got = resolve_api_key(None, Some("MODELS_TEST_KEY"));
        assert_eq!(got.as_deref(), Some("from-env"));
        std::env::remove_var("MODELS_TEST_KEY");
    }

    #[test]
    fn resolve_api_key_handles_multiple_env_keys() {
        std::env::set_var("MODELS_TEST_KEY_B", "from-b");
        let got = resolve_api_key(None, Some("MODELS_TEST_KEY_A,MODELS_TEST_KEY_B|MODELS_TEST_KEY_C"));
        assert_eq!(got.as_deref(), Some("from-b"));
        std::env::remove_var("MODELS_TEST_KEY_B");
    }

    #[test]
    fn resolve_api_key_skips_unset_env() {
        std::env::remove_var("MODELS_TEST_KEY");
        let got = resolve_api_key(None, Some("MODELS_TEST_KEY"));
        assert_eq!(got, None);
    }

    #[test]
    fn resolve_api_key_skips_empty_literal() {
        let got = resolve_api_key(Some("   "), None);
        assert_eq!(got, None);
    }

    #[test]
    fn auth_header_bearer_by_default() {
        let h = auth_header("sk-abc", None);
        assert_eq!(h, Some(("Authorization", "Bearer sk-abc".to_string())));
    }

    #[test]
    fn auth_header_x_api_key_when_explicit() {
        let h = auth_header("sk-abc", Some("x-api-key"));
        assert_eq!(h, Some(("x-api-key", "sk-abc".to_string())));
    }

    #[test]
    fn auth_header_none_when_key_empty() {
        assert_eq!(auth_header("", Some("bearer")), None);
        assert_eq!(auth_header("", None), None);
    }
}