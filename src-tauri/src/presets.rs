//! Built-in catalog of custom model providers ("presets") that the user can
//! pick from the Settings → Models → Add provider flow.
//!
//! Mirrors `PROVIDER_PRESETS` in `grok-build-desktop/src/main/model-providers.ts:52-327`.
//! The shape matches `ModelProviderPreset` in `src/shared/types.ts:1005`:
//!   { id, name, nameZh, region, baseUrl, apiBackend, envKey?, authStyle?,
//!     extraHeaders?, protocolEndpoints?, modelsListBaseUrl?,
//!     popularModels?: [{id, name}], accent?, logo? }
//!
//! The renderer (ModelsView preset picker at `src/ModelsView.tsx:956-1049`)
//! relies on every preset exposing `name`, `region`, `baseUrl`, `accent`,
//! and optionally `logo` / `nameZh` / `popularModels`. The simplified stub
//! we replaced only exposed `label`/`defaultBaseUrl`/`defaultModel`, which
//! made the picker silently render an empty list.

use serde_json::{json, Value};

/// All built-in presets. Order is preserved when shipped to the renderer so
/// the picker renders them in the same sequence the Electron build does.
pub const PRESET_COUNT: usize = 17;

/// `serde_json::Value` representation of one preset, with the same shape as
/// `ModelProviderPreset` in the renderer's `shared/types.ts`. Helpers
/// below build this so the JSON stays readable.
fn preset(
    id: &str,
    name: &str,
    name_zh: &str,
    region: &str,
    base_url: &str,
    api_backend: &str,
    env_key: Option<&str>,
    auth_style: Option<&str>,
    extra_headers: Option<Value>,
    protocol_endpoints: Option<Value>,
    models_list_base_url: Option<&str>,
    popular_models: Value,
    accent: Option<&str>,
    logo: Option<&str>,
) -> Value {
    let mut v = json!({
        "id": id,
        "name": name,
        "nameZh": name_zh,
        "region": region,
        "baseUrl": base_url,
        "apiBackend": api_backend,
    });
    if let Some(k) = env_key {
        v.as_object_mut()
            .unwrap()
            .insert("envKey".to_string(), Value::String(k.to_string()));
    }
    if let Some(a) = auth_style {
        v.as_object_mut()
            .unwrap()
            .insert("authStyle".to_string(), Value::String(a.to_string()));
    }
    if let Some(h) = extra_headers {
        v.as_object_mut().unwrap().insert("extraHeaders".to_string(), h);
    }
    if let Some(p) = protocol_endpoints {
        v.as_object_mut().unwrap().insert("protocolEndpoints".to_string(), p);
    }
    if let Some(u) = models_list_base_url {
        v.as_object_mut()
            .unwrap()
            .insert("modelsListBaseUrl".to_string(), Value::String(u.to_string()));
    }
    if !popular_models.as_array().map(|a| a.is_empty()).unwrap_or(true) {
        v.as_object_mut()
            .unwrap()
            .insert("popularModels".to_string(), popular_models);
    }
    if let Some(c) = accent {
        v.as_object_mut()
            .unwrap()
            .insert("accent".to_string(), Value::String(c.to_string()));
    }
    if let Some(l) = logo {
        v.as_object_mut()
            .unwrap()
            .insert("logo".to_string(), Value::String(l.to_string()));
    }
    v
}

fn popular(items: &[(&str, &str)]) -> Value {
    Value::Array(
        items
            .iter()
            .map(|(id, name)| json!({"id": *id, "name": *name}))
            .collect(),
    )
}

/// Returns the full preset catalog as a `serde_json::Value` array. The
/// renderer casts each entry to `ModelProviderPreset` (see
/// `src/shared/types.ts:1005`) so the shape must match exactly.
pub fn all_presets() -> Value {
    let v = vec![
        // International ───────────────────────────────────────────────
        preset(
            "openai",
            "OpenAI",
            "OpenAI",
            "intl",
            "https://api.openai.com/v1",
            "chat_completions",
            Some("OPENAI_API_KEY"),
            None,
            None,
            None,
            None,
            popular(&[
                ("gpt-4.1", "GPT-4.1"),
                ("gpt-4.1-mini", "GPT-4.1 Mini"),
                ("gpt-4o", "GPT-4o"),
                ("o3", "o3"),
                ("o4-mini", "o4-mini"),
            ]),
            Some("#10a37f"),
            Some("./assets/provider-icons/openai.svg"),
        ),
        preset(
            "anthropic",
            "Anthropic",
            "Anthropic",
            "intl",
            "https://api.anthropic.com/v1",
            "messages",
            Some("ANTHROPIC_API_KEY"),
            Some("x-api-key"),
            Some(json!({"anthropic-version": "2023-06-01"})),
            None,
            None,
            popular(&[
                ("claude-opus-4-6", "Claude Opus 4.6"),
                ("claude-sonnet-4-6", "Claude Sonnet 4.6"),
                ("claude-haiku-4-5-20251001", "Claude Haiku 4.5"),
            ]),
            Some("#d97757"),
            Some("./assets/provider-icons/anthropic.svg"),
        ),
        preset(
            "openrouter",
            "OpenRouter",
            "OpenRouter",
            "intl",
            "https://openrouter.ai/api/v1",
            "chat_completions",
            Some("OPENROUTER_API_KEY"),
            None,
            None,
            None,
            None,
            popular(&[
                ("anthropic/claude-sonnet-4", "Claude Sonnet 4"),
                ("openai/gpt-4o", "GPT-4o"),
                ("google/gemini-2.5-pro", "Gemini 2.5 Pro"),
            ]),
            Some("#7c5cff"),
            Some("./assets/provider-icons/openrouter.svg"),
        ),
        preset(
            "groq",
            "Groq",
            "Groq",
            "intl",
            "https://api.groq.com/openai/v1",
            "chat_completions",
            Some("GROQ_API_KEY"),
            None,
            None,
            None,
            None,
            popular(&[
                ("llama-3.3-70b-versatile", "Llama 3.3 70B"),
                ("qwen/qwen3-32b", "Qwen3 32B"),
            ]),
            Some("#f55036"),
            Some("./assets/provider-icons/groq.svg"),
        ),
        preset(
            "together",
            "Together AI",
            "Together AI",
            "intl",
            "https://api.together.xyz/v1",
            "chat_completions",
            Some("TOGETHER_API_KEY"),
            None,
            None,
            None,
            None,
            popular(&[]),
            Some("#0fb5ba"),
            Some("./assets/provider-icons/together.svg"),
        ),
        preset(
            "gemini",
            "Google Gemini",
            "Google Gemini",
            "intl",
            "https://generativelanguage.googleapis.com/v1beta/openai",
            "chat_completions",
            Some("GEMINI_API_KEY"),
            None,
            None,
            None,
            None,
            popular(&[
                ("gemini-2.5-pro", "Gemini 2.5 Pro"),
                ("gemini-2.5-flash", "Gemini 2.5 Flash"),
            ]),
            Some("#4285f4"),
            Some("./assets/provider-icons/gemini.svg"),
        ),
        // China ────────────────────────────────────────────────────────
        preset(
            "deepseek",
            "DeepSeek",
            "DeepSeek 深度求索",
            "cn",
            // Official docs (https://api-docs.deepseek.com/guides/anthropic_api/):
            //   OpenAI-compatible: POST https://api.deepseek.com/chat/completions
            //   Anthropic-compatible: POST https://api.deepseek.com/anthropic/v1/messages
            // Anthropic path uses `x-api-key` header (same convention as Anthropic).
            "https://api.deepseek.com/anthropic/v1",
            "messages",
            Some("DEEPSEEK_API_KEY"),
            Some("x-api-key"),
            None,
            Some(json!({
                "messages": "https://api.deepseek.com/anthropic/v1",
                "chat_completions": "https://api.deepseek.com",
            })),
            Some("https://api.deepseek.com"),
            popular(&[
                ("deepseek-chat", "DeepSeek Chat (V3)"),
                ("deepseek-reasoner", "DeepSeek Reasoner (R1)"),
            ]),
            Some("#4d8aff"),
            Some("./assets/provider-icons/deepseek.png"),
        ),
        preset(
            "moonshot",
            "Moonshot (Kimi)",
            "月之暗面 Kimi",
            "cn",
            "https://api.moonshot.cn/v1",
            "chat_completions",
            Some("MOONSHOT_API_KEY"),
            None,
            None,
            None,
            None,
            popular(&[
                ("kimi-k2-turbo-preview", "Kimi K2 Turbo"),
                ("moonshot-v1-128k", "Moonshot v1 128K"),
                ("moonshot-v1-32k", "Moonshot v1 32K"),
            ]),
            Some("#1a1a2e"),
            Some("./assets/provider-icons/moonshot.svg"),
        ),
        preset(
            "dashscope",
            "Alibaba DashScope (Qwen)",
            "阿里云百炼 Qwen",
            "cn",
            "https://dashscope.aliyuncs.com/compatible-mode/v1",
            "chat_completions",
            Some("DASHSCOPE_API_KEY"),
            None,
            None,
            None,
            None,
            popular(&[
                ("qwen-max", "Qwen Max"),
                ("qwen-plus", "Qwen Plus"),
                ("qwen-turbo", "Qwen Turbo"),
                ("qwen3-coder-plus", "Qwen3 Coder Plus"),
            ]),
            Some("#ff6a00"),
            Some("./assets/provider-icons/qwen.svg"),
        ),
        preset(
            "zhipu",
            "Zhipu GLM",
            "智谱 GLM",
            "cn",
            "https://open.bigmodel.cn/api/paas/v4",
            "chat_completions",
            Some("ZHIPU_API_KEY"),
            None,
            None,
            None,
            None,
            popular(&[
                ("glm-4.5", "GLM-4.5"),
                ("glm-4.5-air", "GLM-4.5 Air"),
                ("glm-4-flash", "GLM-4 Flash"),
            ]),
            Some("#3859ff"),
            Some("./assets/provider-icons/zhipu.svg"),
        ),
        preset(
            "siliconflow",
            "SiliconFlow",
            "硅基流动",
            "cn",
            "https://api.siliconflow.cn/v1",
            "chat_completions",
            Some("SILICONFLOW_API_KEY"),
            None,
            None,
            None,
            None,
            popular(&[
                ("deepseek-ai/DeepSeek-V3", "DeepSeek V3"),
                ("Qwen/Qwen3-235B-A22B", "Qwen3 235B"),
                ("moonshotai/Kimi-K2-Instruct", "Kimi K2"),
            ]),
            Some("#7c3aed"),
            Some("./assets/provider-icons/siliconflow.png"),
        ),
        preset(
            "volcengine",
            "Volcengine (Doubao)",
            "火山引擎 豆包",
            "cn",
            "https://ark.cn-beijing.volces.com/api/v3",
            "chat_completions",
            Some("ARK_API_KEY"),
            None,
            None,
            None,
            None,
            popular(&[
                ("doubao-seed-1-6-250615", "Doubao Seed 1.6"),
                ("doubao-1-5-pro-32k-250115", "Doubao 1.5 Pro"),
            ]),
            Some("#3b82f6"),
            Some("./assets/provider-icons/volcengine.svg"),
        ),
        preset(
            "minimax",
            "MiniMax",
            "MiniMax",
            "cn",
            // Electron pins the Anthropic-compatible path for messages;
            // protocolEndpoints carries the OpenAI + Responses hosts.
            "https://api.minimaxi.com/anthropic/v1",
            "messages",
            Some("MINIMAX_API_KEY"),
            Some("x-api-key"),
            None,
            Some(json!({
                "messages": "https://api.minimaxi.com/anthropic/v1",
                "chat_completions": "https://api.minimaxi.com/v1",
                "responses": "https://api.minimaxi.com/v1",
            })),
            Some("https://api.minimaxi.com/v1"),
            popular(&[
                ("MiniMax-M3", "MiniMax M3"),
                ("MiniMax-M2.7", "MiniMax M2.7"),
                ("MiniMax-M2.5", "MiniMax M2.5"),
            ]),
            Some("#ff4d4f"),
            Some("./assets/provider-icons/minimax.svg"),
        ),
        preset(
            "stepfun",
            "StepFun",
            "阶跃星辰",
            "cn",
            "https://api.stepfun.com/v1",
            "chat_completions",
            Some("STEPFUN_API_KEY"),
            None,
            None,
            None,
            None,
            popular(&[
                ("step-2-16k", "Step 2 16K"),
                ("step-1-flash", "Step 1 Flash"),
            ]),
            Some("#5b21b6"),
            Some("./assets/provider-icons/stepfun.svg"),
        ),
        // Local ────────────────────────────────────────────────────────
        preset(
            "ollama",
            "Ollama (local)",
            "Ollama（本地）",
            "local",
            "http://localhost:11434/v1",
            "chat_completions",
            None,
            None,
            None,
            None,
            None,
            popular(&[
                ("llama3.2", "Llama 3.2"),
                ("qwen2.5-coder", "Qwen2.5 Coder"),
                ("codellama", "Code Llama"),
            ]),
            Some("#1a1a1a"),
            Some("./assets/provider-icons/ollama.svg"),
        ),
        preset(
            "lmstudio",
            "LM Studio (local)",
            "LM Studio（本地）",
            "local",
            "http://localhost:1234/v1",
            "chat_completions",
            None,
            None,
            None,
            None,
            None,
            popular(&[]),
            Some("#0f172a"),
            Some("./assets/provider-icons/lmstudio.svg"),
        ),
        // Custom ───────────────────────────────────────────────────────
        preset(
            "custom",
            "Custom (OpenAI-compatible)",
            "自定义（OpenAI 兼容）",
            "local",
            "",
            "chat_completions",
            None,
            None,
            None,
            None,
            None,
            popular(&[]),
            Some("#64748b"),
            None,
        ),
    ];
    assert_eq!(v.len(), PRESET_COUNT, "preset catalog changed");
    Value::Array(v)
}

/// Look up one preset by id. Returns `None` for unknown ids (caller decides
/// how to surface the error — the renderer falls back to a custom endpoint).
pub fn find_preset(id: &str) -> Option<Value> {
    let arr = all_presets().as_array()?.clone();
    arr.into_iter().find(|p| {
        p.get("id").and_then(|v| v.as_str()) == Some(id)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preset_catalog_has_expected_count() {
        let binding = all_presets();
        let arr = binding.as_array().unwrap();
        assert_eq!(arr.len(), PRESET_COUNT);
    }

    #[test]
    fn preset_ids_are_unique() {
        let binding = all_presets();
        let arr = binding.as_array().unwrap();
        let mut seen = std::collections::HashSet::new();
        for p in arr {
            let id = p.get("id").and_then(|v| v.as_str()).unwrap();
            assert!(seen.insert(id), "duplicate preset id: {}", id);
        }
    }

    #[test]
    fn every_preset_has_required_fields() {
        let binding = all_presets();
        let arr = binding.as_array().unwrap();
        for p in arr {
            let id = p.get("id").and_then(|v| v.as_str()).unwrap();
            assert!(p.get("name").and_then(|v| v.as_str()).is_some(), "{} missing name", id);
            assert!(p.get("nameZh").and_then(|v| v.as_str()).is_some(), "{} missing nameZh", id);
            assert!(p.get("region").and_then(|v| v.as_str()).is_some(), "{} missing region", id);
            assert!(p.get("baseUrl").and_then(|v| v.as_str()).is_some(), "{} missing baseUrl", id);
            assert!(p.get("apiBackend").and_then(|v| v.as_str()).is_some(), "{} missing apiBackend", id);
            match p.get("region").and_then(|v| v.as_str()).unwrap() {
                "intl" | "cn" | "local" => {}
                other => panic!("preset {} has invalid region: {}", id, other),
            }
            match p.get("apiBackend").and_then(|v| v.as_str()).unwrap() {
                "chat_completions" | "responses" | "messages" => {}
                other => panic!("preset {} has invalid apiBackend: {}", id, other),
            }
        }
    }

    #[test]
    fn anthropic_uses_x_api_key_and_anthropic_version_header() {
        let p = find_preset("anthropic").unwrap();
        assert_eq!(p.get("authStyle").and_then(|v| v.as_str()), Some("x-api-key"));
        let headers = p.get("extraHeaders").and_then(|v| v.as_object()).unwrap();
        assert_eq!(
            headers.get("anthropic-version").and_then(|v| v.as_str()),
            Some("2023-06-01")
        );
    }

    #[test]
    fn x_api_key_only_for_anthropic_minimax_and_deepseek() {
        // The renderer's auth-style picker (`ModelsView.tsx:1233-1241`)
        // shows the x-api-key option only when the preset declares
        // `authStyle: "x-api-key"`. Anthropic, MiniMax, and DeepSeek
        // all require the `x-api-key` header on their Anthropic-
        // compatible endpoints — guard against accidentally re-introducing
        // `x-api-key` on a Bearer-only preset (or, conversely, dropping
        // it from the three vendors that need it).
        let allowed: &[&str] = &["anthropic", "minimax", "deepseek"];
        let binding = all_presets();
        let arr = binding.as_array().unwrap();
        for p in arr {
            let id = p.get("id").and_then(|v| v.as_str()).unwrap();
            let has_x_api = p.get("authStyle").and_then(|v| v.as_str()) == Some("x-api-key");
            if has_x_api {
                assert!(
                    allowed.contains(&id),
                    "only Anthropic/MiniMax/DeepSeek may opt into x-api-key auth, found: {}",
                    id
                );
            } else if allowed.contains(&id) {
                panic!("{} is expected to opt into x-api-key auth", id);
            }
        }
    }

    #[test]
    fn deepseek_pins_anthropic_base_url_with_messages() {
        let p = find_preset("deepseek").unwrap();
        assert_eq!(p.get("apiBackend").and_then(|v| v.as_str()), Some("messages"));
        assert_eq!(
            p.get("baseUrl").and_then(|v| v.as_str()),
            Some("https://api.deepseek.com/anthropic/v1")
        );
        let endpoints = p
            .get("protocolEndpoints")
            .and_then(|v| v.as_object())
            .unwrap();
        assert_eq!(
            endpoints.get("chat_completions").and_then(|v| v.as_str()),
            Some("https://api.deepseek.com")
        );
        assert_eq!(
            endpoints.get("messages").and_then(|v| v.as_str()),
            Some("https://api.deepseek.com/anthropic/v1")
        );
    }

    #[test]
    fn deepseek_uses_x_api_key_auth() {
        // DeepSeek's Anthropic-compatible endpoint requires the
        // `x-api-key` header (same convention as Anthropic). The
        // renderer only emits `x-api-key` when the preset declares
        // `authStyle: "x-api-key"`.
        let p = find_preset("deepseek").unwrap();
        assert_eq!(
            p.get("authStyle").and_then(|v| v.as_str()),
            Some("x-api-key"),
            "deepseek must use x-api-key auth, got: {:?}",
            p.get("authStyle")
        );
    }

    #[test]
    fn minimax_pins_anthropic_base_url_with_messages() {
        let p = find_preset("minimax").unwrap();
        assert_eq!(p.get("apiBackend").and_then(|v| v.as_str()), Some("messages"));
        assert_eq!(
            p.get("baseUrl").and_then(|v| v.as_str()),
            Some("https://api.minimaxi.com/anthropic/v1")
        );
        let endpoints = p
            .get("protocolEndpoints")
            .and_then(|v| v.as_object())
            .unwrap();
        assert_eq!(
            endpoints.get("chat_completions").and_then(|v| v.as_str()),
            Some("https://api.minimaxi.com/v1")
        );
        assert_eq!(
            endpoints.get("responses").and_then(|v| v.as_str()),
            Some("https://api.minimaxi.com/v1")
        );
    }

    #[test]
    fn local_providers_have_no_env_key() {
        for id in ["ollama", "lmstudio"] {
            let p = find_preset(id).unwrap();
            assert!(
                p.get("envKey").is_none(),
                "{} should not declare an env key",
                id
            );
        }
    }

    #[test]
    fn popular_models_have_id_and_name() {
        let binding = all_presets();
        let arr = binding.as_array().unwrap();
        for p in arr {
            let id = p.get("id").and_then(|v| v.as_str()).unwrap();
            if let Some(items) = p.get("popularModels").and_then(|v| v.as_array()) {
                for m in items {
                    assert!(
                        m.get("id").and_then(|v| v.as_str()).is_some(),
                        "{} popular model missing id",
                        id
                    );
                    assert!(
                        m.get("name").and_then(|v| v.as_str()).is_some(),
                        "{} popular model missing name",
                        id
                    );
                }
            }
        }
    }

    #[test]
    fn find_preset_returns_none_for_unknown() {
        assert!(find_preset("not-a-real-provider").is_none());
    }

    #[test]
    fn all_presets_have_logo_path_or_no_path() {
        // Just make sure every preset either has a string logo or no logo
        // (never null) so the renderer's `preset.logo ? <img/> : <char/>`
        // branch works.
        let binding = all_presets();
        let arr = binding.as_array().unwrap();
        for p in arr {
            match p.get("logo") {
                None | Some(Value::Null) => {}
                Some(Value::String(_)) => {}
                other => panic!(
                    "preset {} has invalid logo value: {:?}",
                    p.get("id").unwrap(),
                    other
                ),
            }
        }
    }
}