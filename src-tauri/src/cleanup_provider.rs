//! AI providers. Dictation cleanup, long-form Note Taker cleanup, meeting
//! notes, and voice search answers all go through `complete`, which speaks
//! either the
//! OpenAI-compatible chat completions shape (DeepSeek, OpenAI, Gemini, Groq,
//! OpenRouter, and any custom endpoint such as Ollama or LM Studio) or the
//! Anthropic Messages API.

use crate::settings::{provider_key, UserSettings};
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::time::Duration;

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub enum CleanupProvider {
    #[default]
    #[serde(rename = "deepseek")]
    DeepSeek,
    #[serde(rename = "openai")]
    OpenAi,
    #[serde(rename = "anthropic")]
    Anthropic,
    #[serde(rename = "gemini")]
    Gemini,
    #[serde(rename = "groq")]
    Groq,
    #[serde(rename = "openrouter")]
    OpenRouter,
    /// Any OpenAI-compatible endpoint; the URL and model come from settings.
    #[serde(rename = "custom")]
    Custom,
}

impl CleanupProvider {
    pub const ALL: [CleanupProvider; 7] = [
        Self::DeepSeek,
        Self::OpenAi,
        Self::Anthropic,
        Self::Gemini,
        Self::Groq,
        Self::OpenRouter,
        Self::Custom,
    ];

    pub fn id(self) -> &'static str {
        match self {
            Self::DeepSeek => "deepseek",
            Self::OpenAi => "openai",
            Self::Anthropic => "anthropic",
            Self::Gemini => "gemini",
            Self::Groq => "groq",
            Self::OpenRouter => "openrouter",
            Self::Custom => "custom",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::DeepSeek => "DeepSeek",
            Self::OpenAi => "OpenAI",
            Self::Anthropic => "Anthropic",
            Self::Gemini => "Google Gemini",
            Self::Groq => "Groq",
            Self::OpenRouter => "OpenRouter",
            Self::Custom => "Custom endpoint",
        }
    }

    /// Message prefix for "Add {article} {label} API key".
    fn article(self) -> &'static str {
        match self {
            Self::OpenAi | Self::Anthropic => "an",
            _ => "a",
        }
    }

    fn endpoint(self) -> Option<&'static str> {
        match self {
            Self::DeepSeek => Some("https://api.deepseek.com/chat/completions"),
            Self::OpenAi => Some("https://api.openai.com/v1/chat/completions"),
            Self::Anthropic => Some("https://api.anthropic.com/v1/messages"),
            Self::Gemini => {
                Some("https://generativelanguage.googleapis.com/v1beta/openai/chat/completions")
            }
            Self::Groq => Some("https://api.groq.com/openai/v1/chat/completions"),
            Self::OpenRouter => Some("https://openrouter.ai/api/v1/chat/completions"),
            Self::Custom => None,
        }
    }

    pub fn default_model(self) -> Option<&'static str> {
        match self {
            Self::DeepSeek => Some("deepseek-v4-flash"),
            Self::OpenAi => Some("gpt-4.1-mini"),
            Self::Anthropic => Some("claude-opus-5-5"),
            Self::Gemini => Some("gemini-2.5-flash-lite"),
            Self::Groq => Some("llama-3.3-70b-versatile"),
            Self::OpenRouter => Some("openai/gpt-4.1-mini"),
            Self::Custom => None,
        }
    }

    /// Keyring account holding this provider's API key. DeepSeek keeps the
    /// account name older releases used so existing keys carry over.
    pub fn keyring_account(self) -> &'static str {
        match self {
            Self::DeepSeek => "deepseek-api-key",
            Self::OpenAi => "openai-api-key",
            Self::Anthropic => "anthropic-api-key",
            Self::Gemini => "gemini-api-key",
            Self::Groq => "groq-api-key",
            Self::OpenRouter => "openrouter-api-key",
            Self::Custom => "custom-api-key",
        }
    }

    pub fn env_var(self) -> &'static str {
        match self {
            Self::DeepSeek => "DEEPSEEK_API_KEY",
            Self::OpenAi => "OPENAI_API_KEY",
            Self::Anthropic => "ANTHROPIC_API_KEY",
            Self::Gemini => "GEMINI_API_KEY",
            Self::Groq => "GROQ_API_KEY",
            Self::OpenRouter => "OPENROUTER_API_KEY",
            Self::Custom => "PRONTO_CUSTOM_API_KEY",
        }
    }

    /// Local servers (Ollama, LM Studio) behind a custom endpoint usually
    /// need no key; every hosted provider does.
    pub fn requires_key(self) -> bool {
        self != Self::Custom
    }
}

/// Provider catalog sent to Settings so the UI stays data-driven.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderInfo {
    pub id: &'static str,
    pub label: &'static str,
    pub default_model: Option<&'static str>,
    pub requires_key: bool,
    pub key_configured: bool,
}

pub fn catalog() -> Vec<ProviderInfo> {
    CleanupProvider::ALL
        .iter()
        .map(|&provider| ProviderInfo {
            id: provider.id(),
            label: provider.label(),
            default_model: provider.default_model(),
            requires_key: provider.requires_key(),
            key_configured: provider_key(provider).is_some(),
        })
        .collect()
}

/// A fully resolved cleanup destination: everything a request needs.
#[derive(Clone, Debug)]
pub struct CleanupTarget {
    pub provider: CleanupProvider,
    pub endpoint: String,
    pub model: String,
    pub api_key: Option<String>,
}

impl CleanupTarget {
    pub fn label(&self) -> &'static str {
        self.provider.label()
    }
}

/// Resolves the configured provider. The error says what is missing,
/// phrased so callers can append their purpose ("… to enable AI cleanup").
pub fn resolve(settings: &UserSettings) -> Result<CleanupTarget, String> {
    let provider = settings.cleanup_provider;
    let endpoint = match provider.endpoint() {
        Some(endpoint) => endpoint.to_string(),
        None => settings
            .cleanup_endpoint
            .as_deref()
            .map(custom_chat_url)
            .ok_or_else(|| "Add a custom endpoint URL in Settings".to_string())?,
    };
    let model = settings
        .cleanup_model
        .as_deref()
        .map(str::trim)
        .filter(|model| !model.is_empty())
        .or(provider.default_model())
        .ok_or_else(|| "Add a model name for the custom endpoint in Settings".to_string())?
        .to_string();
    let api_key = provider_key(provider);
    if api_key.is_none() && provider.requires_key() {
        return Err(format!(
            "Add {} {} API key in Settings",
            provider.article(),
            provider.label()
        ));
    }
    Ok(CleanupTarget {
        provider,
        endpoint,
        model,
        api_key,
    })
}

/// Accepts either a base URL (`http://localhost:11434/v1`) or the full
/// chat completions URL.
fn custom_chat_url(url: &str) -> String {
    let url = url.trim().trim_end_matches('/');
    if url.ends_with("/chat/completions") {
        url.to_string()
    } else {
        format!("{url}/chat/completions")
    }
}

/// Sends one system + user exchange and returns the trimmed reply text.
pub fn complete(
    client: &Client,
    target: &CleanupTarget,
    system_prompt: &str,
    user_content: &str,
    max_tokens: u32,
) -> Result<String, String> {
    complete_task(
        client,
        target,
        &Task {
            name: "cleanup",
            timeout: None,
        },
        system_prompt,
        user_content,
        max_tokens,
    )
}

/// What a request is for: `name` appears in error messages ("OpenAI answer
/// failed"), and `timeout` overrides the client's default when set.
pub struct Task {
    pub name: &'static str,
    pub timeout: Option<Duration>,
}

pub fn complete_task(
    client: &Client,
    target: &CleanupTarget,
    task: &Task,
    system_prompt: &str,
    user_content: &str,
    max_tokens: u32,
) -> Result<String, String> {
    let label = target.label();
    let content = match target.provider {
        CleanupProvider::Anthropic => anthropic_complete(
            client,
            target,
            task,
            system_prompt,
            user_content,
            max_tokens,
        )?,
        _ => openai_complete(
            client,
            target,
            task,
            system_prompt,
            user_content,
            max_tokens,
        )?,
    };
    let content = content.trim().to_string();
    if content.is_empty() {
        return Err(format!("{label} returned an empty {}", task.name));
    }
    Ok(content)
}

fn openai_complete(
    client: &Client,
    target: &CleanupTarget,
    task: &Task,
    system_prompt: &str,
    user_content: &str,
    max_tokens: u32,
) -> Result<String, String> {
    let label = target.label();
    let what = task.name;
    let mut body = json!({
        "model": target.model,
        "messages": [
            { "role": "system", "content": system_prompt },
            { "role": "user", "content": user_content }
        ],
        "stream": false
    });
    match target.provider {
        // OpenAI's reasoning models reject `max_tokens` and any non-default
        // temperature; `max_completion_tokens` works on every chat model.
        CleanupProvider::OpenAi => {
            body["max_completion_tokens"] = json!(max_tokens);
            if !is_openai_reasoning_model(&target.model) {
                body["temperature"] = json!(0);
            }
        }
        CleanupProvider::DeepSeek => {
            body["thinking"] = json!({ "type": "disabled" });
            body["temperature"] = json!(0);
            body["max_tokens"] = json!(max_tokens);
        }
        _ => {
            body["temperature"] = json!(0);
            body["max_tokens"] = json!(max_tokens);
        }
    }
    let mut request = client.post(&target.endpoint).json(&body);
    if let Some(timeout) = task.timeout {
        request = request.timeout(timeout);
    }
    if let Some(key) = &target.api_key {
        request = request.bearer_auth(key);
    }
    let response = request
        .send()
        .map_err(|error| format!("{label} {what} failed: {error}"))?;
    if !response.status().is_success() {
        let status = response.status();
        let detail = response.text().unwrap_or_default();
        return Err(format!("{label} {what} returned {status}: {detail}"));
    }

    #[derive(Deserialize)]
    struct ChatResponse {
        choices: Vec<ChatChoice>,
    }
    #[derive(Deserialize)]
    struct ChatChoice {
        message: ChatMessage,
    }
    #[derive(Deserialize)]
    struct ChatMessage {
        #[serde(default)]
        content: Option<String>,
    }

    response
        .json::<ChatResponse>()
        .map_err(|error| format!("Invalid {label} {what} response: {error}"))?
        .choices
        .into_iter()
        .next()
        .and_then(|choice| choice.message.content)
        .ok_or_else(|| format!("{label} returned an empty {what}"))
}

fn is_openai_reasoning_model(model: &str) -> bool {
    let model = model.to_ascii_lowercase();
    model.starts_with("gpt-5")
        || model.starts_with("o1")
        || model.starts_with("o3")
        || model.starts_with("o4")
}

/// Thinking tokens count against `max_tokens` on current Claude models, so
/// the visible-output budget callers pass gets this much headroom on top.
const ANTHROPIC_THINKING_HEADROOM: u32 = 4096;

fn anthropic_complete(
    client: &Client,
    target: &CleanupTarget,
    task: &Task,
    system_prompt: &str,
    user_content: &str,
    max_tokens: u32,
) -> Result<String, String> {
    let label = target.label();
    let what = task.name;
    let mut body = json!({
        "model": target.model,
        "max_tokens": max_tokens + ANTHROPIC_THINKING_HEADROOM,
        "system": system_prompt,
        "messages": [{ "role": "user", "content": user_content }]
    });
    // Cleanup is a light edit: keep thinking short where the model takes an
    // effort setting (older models reject the field).
    if anthropic_supports_effort(&target.model) {
        body["output_config"] = json!({ "effort": "low" });
    }
    let fallbacks = anthropic_supports_fallbacks(&target.model);
    if fallbacks {
        body["fallbacks"] = json!("default");
    }
    let mut request = client
        .post(&target.endpoint)
        .header("anthropic-version", "2023-06-01")
        .json(&body);
    if let Some(key) = &target.api_key {
        request = request.header("x-api-key", key);
    }
    if fallbacks {
        request = request.header("anthropic-beta", "server-side-fallback-2026-07-01");
    }
    if let Some(timeout) = task.timeout {
        request = request.timeout(timeout);
    }
    let response = request
        .send()
        .map_err(|error| format!("{label} {what} failed: {error}"))?;
    if !response.status().is_success() {
        let status = response.status();
        let detail = response.text().unwrap_or_default();
        return Err(format!("{label} {what} returned {status}: {detail}"));
    }
    let message = response
        .json::<Value>()
        .map_err(|error| format!("Invalid {label} {what} response: {error}"))?;
    if message["stop_reason"] == "refusal" {
        return Err(format!("{label} declined this {what} request"));
    }
    let text = message["content"]
        .as_array()
        .map(|blocks| {
            blocks
                .iter()
                .filter(|block| block["type"] == "text")
                .filter_map(|block| block["text"].as_str())
                .collect::<String>()
        })
        .unwrap_or_default();
    Ok(text)
}

fn anthropic_supports_effort(model: &str) -> bool {
    [
        "claude-opus-5",
        "claude-fable-5",
        "claude-sonnet-5",
        "claude-haiku-5",
        "claude-opus-4-7",
        "claude-opus-4-8",
    ]
    .iter()
    .any(|prefix| model.starts_with(prefix))
}

/// Server-side refusal fallback (`fallbacks: "default"`) is accepted only on
/// these models on the Claude API.
fn anthropic_supports_fallbacks(model: &str) -> bool {
    matches!(
        model,
        "claude-opus-5-5" | "claude-opus-5" | "claude-fable-5-1" | "claude-sonnet-5-5"
    )
}

/// Models endpoint for a provider: the chat URL with `/chat/completions`
/// swapped for `/models`, or Anthropic's own listing.
fn models_url(provider: CleanupProvider, custom_endpoint: Option<&str>) -> Option<String> {
    if provider == CleanupProvider::Anthropic {
        return Some("https://api.anthropic.com/v1/models?limit=1000".into());
    }
    let chat = match provider.endpoint() {
        Some(endpoint) => endpoint.to_string(),
        None => custom_chat_url(custom_endpoint?),
    };
    Some(format!(
        "{}/models",
        chat.strip_suffix("/chat/completions")?
    ))
}

/// Lists the chat models a provider offers, newest-style ids sorted, with
/// embedding, speech, image, and moderation models filtered out.
pub fn list_models(
    client: &Client,
    provider: CleanupProvider,
    custom_endpoint: Option<&str>,
) -> Result<Vec<String>, String> {
    let label = provider.label();
    let url = models_url(provider, custom_endpoint)
        .ok_or_else(|| "Add a custom endpoint URL first".to_string())?;
    let api_key = provider_key(provider);
    if api_key.is_none() && provider.requires_key() {
        return Err(format!("Save your {label} API key to load models"));
    }
    let mut request = client.get(&url).timeout(Duration::from_secs(10));
    if let Some(key) = &api_key {
        request = match provider {
            CleanupProvider::Anthropic => request
                .header("x-api-key", key)
                .header("anthropic-version", "2023-06-01"),
            _ => request.bearer_auth(key),
        };
    }
    let response = request
        .send()
        .map_err(|error| format!("Could not load {label} models: {error}"))?;
    if !response.status().is_success() {
        return Err(format!(
            "Could not load {label} models ({})",
            response.status()
        ));
    }
    let listing = response
        .json::<Value>()
        .map_err(|error| format!("Invalid {label} model list: {error}"))?;
    Ok(chat_model_ids(&listing))
}

fn chat_model_ids(listing: &Value) -> Vec<String> {
    const NOT_CHAT: [&str; 14] = [
        "embed",
        "tts",
        "whisper",
        "dall-e",
        "moderation",
        "audio",
        "realtime",
        "transcribe",
        "image",
        "imagen",
        "veo",
        "rerank",
        "guard",
        "aqa",
    ];
    let mut ids: Vec<String> = listing["data"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|model| model["id"].as_str())
        // Gemini's OpenAI-compatible listing prefixes ids with `models/`.
        .map(|id| id.strip_prefix("models/").unwrap_or(id).to_string())
        .filter(|id| {
            let lower = id.to_ascii_lowercase();
            !NOT_CHAT.iter().any(|word| lower.contains(word))
        })
        .collect();
    ids.sort();
    ids.dedup();
    ids
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn providers_round_trip_through_settings_ids() {
        for provider in CleanupProvider::ALL {
            let json = serde_json::to_string(&provider).unwrap();
            assert_eq!(json, format!("\"{}\"", provider.id()));
            let back: CleanupProvider = serde_json::from_str(&json).unwrap();
            assert_eq!(back, provider);
        }
    }

    #[test]
    fn custom_endpoint_accepts_base_or_full_url() {
        assert_eq!(
            custom_chat_url("http://localhost:11434/v1/"),
            "http://localhost:11434/v1/chat/completions"
        );
        assert_eq!(
            custom_chat_url("http://localhost:1234/v1/chat/completions"),
            "http://localhost:1234/v1/chat/completions"
        );
    }

    #[test]
    fn custom_provider_needs_endpoint_and_model() {
        let mut settings = UserSettings {
            cleanup_provider: CleanupProvider::Custom,
            ..UserSettings::default()
        };
        assert!(resolve(&settings).unwrap_err().contains("endpoint URL"));
        settings.cleanup_endpoint = Some("http://localhost:11434/v1".into());
        assert!(resolve(&settings).unwrap_err().contains("model name"));
        settings.cleanup_model = Some("llama3.2".into());
        let target = resolve(&settings).unwrap();
        assert_eq!(
            target.endpoint,
            "http://localhost:11434/v1/chat/completions"
        );
        assert_eq!(target.model, "llama3.2");
    }

    #[test]
    fn models_url_follows_the_chat_endpoint() {
        assert_eq!(
            models_url(CleanupProvider::OpenAi, None).as_deref(),
            Some("https://api.openai.com/v1/models")
        );
        assert_eq!(
            models_url(CleanupProvider::DeepSeek, None).as_deref(),
            Some("https://api.deepseek.com/models")
        );
        assert_eq!(
            models_url(CleanupProvider::Custom, Some("http://localhost:11434/v1")).as_deref(),
            Some("http://localhost:11434/v1/models")
        );
        assert!(models_url(CleanupProvider::Custom, None).is_none());
    }

    #[test]
    fn model_listing_keeps_chat_models_only() {
        let listing = json!({ "data": [
            { "id": "gpt-4.1-mini" },
            { "id": "text-embedding-3-small" },
            { "id": "models/gemini-2.5-flash" },
            { "id": "gpt-4o-mini-tts" },
            { "id": "gpt-4.1-mini" }
        ]});
        assert_eq!(
            chat_model_ids(&listing),
            vec!["gemini-2.5-flash", "gpt-4.1-mini"]
        );
    }

    #[test]
    fn openai_reasoning_models_are_detected() {
        assert!(is_openai_reasoning_model("gpt-5-mini"));
        assert!(is_openai_reasoning_model("o4-mini"));
        assert!(!is_openai_reasoning_model("gpt-4.1-mini"));
    }
}
