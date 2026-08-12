//! AI provider metadata and credential references.
//!
//! Provider metadata is deliberately kept separate from `nebula_settings.txt`:
//! the latter is a user-editable runtime file, while provider credentials need
//! a stable identity and must never be serialized beside ordinary settings.

use std::io;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use crate::event::{Event, EventType};

const STORE_FILE: &str = "nebula_providers.json";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderKind {
    OpenAi,
    Anthropic,
    Google,
    Ollama,
    OpenRouter,
    Qwen,
    DeepSeek,
    Kimi,
    Zhipu,
    Doubao,
    Mimo,
    AzureOpenAi,
    Custom,
}

impl ProviderKind {
    pub const PRESETS: [Self; 13] = [
        Self::OpenAi,
        Self::Anthropic,
        Self::Google,
        Self::Ollama,
        Self::OpenRouter,
        Self::Qwen,
        Self::DeepSeek,
        Self::Kimi,
        Self::Zhipu,
        Self::Doubao,
        Self::Mimo,
        Self::AzureOpenAi,
        Self::Custom,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::OpenAi => "OpenAI",
            Self::Anthropic => "Anthropic",
            Self::Google => "Google",
            Self::Ollama => "Ollama",
            Self::OpenRouter => "OpenRouter",
            Self::Qwen => "Qwen",
            Self::DeepSeek => "DeepSeek",
            Self::Kimi => "Kimi",
            Self::Zhipu => "Zhipu",
            Self::Doubao => "Doubao",
            Self::Mimo => "Xiaomi MiMo",
            Self::AzureOpenAi => "Azure OpenAI",
            Self::Custom => "OpenAI Compatible",
        }
    }

    pub fn default_base_url(self) -> &'static str {
        match self {
            Self::OpenAi => "https://api.openai.com/v1",
            Self::Anthropic => "https://api.anthropic.com/v1",
            Self::Google => "https://generativelanguage.googleapis.com/v1beta",
            Self::Ollama => "http://localhost:11434/v1",
            Self::OpenRouter => "https://openrouter.ai/api/v1",
            Self::Qwen => "https://dashscope.aliyuncs.com/compatible-mode/v1",
            Self::DeepSeek => "https://api.deepseek.com/v1",
            Self::Kimi => "https://api.moonshot.ai/v1",
            Self::Zhipu => "https://open.bigmodel.cn/api/paas/v4",
            Self::Doubao => "https://ark.cn-beijing.volces.com/api/v3",
            Self::Mimo => "https://api.xiaomimimo.com/v1",
            Self::AzureOpenAi => "https://{resource}.openai.azure.com/openai/deployments",
            Self::Custom => "",
        }
    }

    pub fn default_model(self) -> &'static str {
        match self {
            Self::OpenAi => "gpt-5.4-mini",
            Self::Anthropic => "claude-sonnet-4-5",
            Self::Google => "gemini-2.5-flash",
            Self::Ollama => "qwen3",
            Self::OpenRouter => "openai/gpt-5.4-mini",
            Self::Qwen => "qwen3.7-plus",
            Self::DeepSeek => "deepseek-chat",
            Self::Kimi => "kimi-k2.6",
            Self::Zhipu => "glm-5.1",
            Self::Doubao => "doubao-seed-2-0-pro-260215",
            Self::Mimo => "mimo-v2.5-pro",
            Self::AzureOpenAi => "gpt-4o",
            Self::Custom => "",
        }
    }

    pub fn requires_api_key(self) -> bool {
        !matches!(self, Self::Ollama)
    }

    pub fn uses_openai_protocol(self) -> bool {
        !matches!(self, Self::Anthropic | Self::Google)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AiProvider {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub note: String,
    #[serde(default)]
    pub website_url: String,
    pub kind: ProviderKind,
    pub base_url: String,
    pub model: String,
    #[serde(default)]
    pub enabled: bool,
    /// A credential reference only. The actual key is held by the OS store.
    #[serde(default)]
    pub api_key_set: bool,
    #[serde(default)]
    pub api_key_hint: String,
    #[serde(default)]
    pub full_url: bool,
    /// Optional Codex feature flags applied only by the explicit Codex action.
    #[serde(default)]
    pub codex_goals: bool,
    #[serde(default)]
    pub codex_remote_compaction: bool,
}

impl AiProvider {
    pub fn preset(kind: ProviderKind, id: impl Into<String>) -> Self {
        let kind = kind;
        Self {
            id: id.into(),
            name: kind.label().to_owned(),
            note: String::new(),
            website_url: String::new(),
            kind,
            base_url: kind.default_base_url().to_owned(),
            model: kind.default_model().to_owned(),
            enabled: true,
            api_key_set: false,
            api_key_hint: String::new(),
            full_url: false,
            codex_goals: false,
            codex_remote_compaction: false,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderStore {
    #[serde(default)]
    pub active_id: String,
    #[serde(default)]
    pub providers: Vec<AiProvider>,
}

#[derive(Debug, Clone)]
pub struct ProviderTestRequest {
    pub request_id: u64,
    pub provider: AiProvider,
}

pub fn store_path() -> PathBuf {
    crate::display::nebula_data_dir().join(STORE_FILE)
}

pub fn load() -> ProviderStore {
    let mut store = std::fs::read_to_string(store_path())
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default();
    normalize(&mut store);
    store
}

/// Older builds only materialized a preset after it was clicked. Keep those
/// records intact, but fill the missing presets so the visible list can be a
/// real, index-addressable collection and multiple custom entries remain
/// independently editable.
fn normalize(store: &mut ProviderStore) {
    let mut existing = std::mem::take(&mut store.providers);
    let mut ordered = Vec::with_capacity(ProviderKind::PRESETS.len().max(existing.len()));
    for kind in ProviderKind::PRESETS {
        if kind == ProviderKind::Custom {
            continue;
        }
        if let Some(index) = existing.iter().position(|provider| provider.kind == kind) {
            ordered.push(existing.remove(index));
        } else {
            ordered.push(AiProvider::preset(kind, preset_id(kind)));
        }
    }
    let had_custom = existing.iter().any(|provider| provider.kind == ProviderKind::Custom);
    ordered.extend(existing.into_iter().filter(|provider| provider.kind == ProviderKind::Custom));
    if !had_custom {
        ordered.push(AiProvider::preset(ProviderKind::Custom, preset_id(ProviderKind::Custom)));
    }
    store.providers = ordered;
    if !store.providers.iter().any(|provider| provider.id == store.active_id) {
        store.active_id =
            store.providers.first().map(|provider| provider.id.clone()).unwrap_or_default();
    }
}

pub fn preset_id(kind: ProviderKind) -> String {
    format!("preset-{}", kind.label().to_ascii_lowercase().replace(' ', "-"))
}

pub fn next_custom_id(store: &ProviderStore) -> String {
    let mut sequence = 1usize;
    loop {
        let id = format!("custom-{sequence}");
        if !store.providers.iter().any(|provider| provider.id == id) {
            return id;
        }
        sequence += 1;
    }
}

pub fn active_enabled(store: &ProviderStore) -> Option<&AiProvider> {
    store.providers.iter().find(|provider| provider.id == store.active_id && provider.enabled)
}

pub fn save(store: &ProviderStore) -> io::Result<()> {
    let path = store_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let text = serde_json::to_string_pretty(store)
        .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
    std::fs::write(path, text)
}

pub fn credential_target(id: &str) -> String {
    format!("Nebula/AI/{id}")
}

pub fn api_key_hint(key: &str) -> String {
    let key = key.trim();
    if key.is_empty() {
        return String::new();
    }
    let tail: String = key.chars().rev().take(4).collect::<String>().chars().rev().collect();
    format!("••••{tail}")
}

pub fn save_api_key(id: &str, key: &str) -> io::Result<String> {
    let key = key.trim();
    if key.is_empty() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "API key is empty"));
    }
    #[cfg(windows)]
    crate::ssh_credentials::store_generic_secret(&credential_target(id), key.as_bytes())?;
    #[cfg(not(windows))]
    return Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "system credential storage is not available on this build",
    ));
    Ok(api_key_hint(key))
}

pub fn delete_api_key(id: &str) -> io::Result<()> {
    #[cfg(windows)]
    crate::ssh_credentials::delete_generic_secret(&credential_target(id))?;
    #[cfg(not(windows))]
    return Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "system credential storage is not available on this build",
    ));
    Ok(())
}

pub fn load_api_key(id: &str) -> io::Result<Option<Vec<u8>>> {
    #[cfg(windows)]
    return crate::ssh_credentials::load_generic_secret(&credential_target(id));
    #[cfg(not(windows))]
    {
        let _ = id;
        Ok(None)
    }
}

fn test_url(provider: &AiProvider) -> Result<String, String> {
    let base = provider.base_url.trim().trim_end_matches('/');
    if !(base.starts_with("https://") || base.starts_with("http://")) {
        return Err("请求地址必须以 http:// 或 https:// 开头".to_owned());
    }
    if provider.full_url {
        return Ok(base.to_owned());
    }
    Ok(match provider.kind {
        ProviderKind::AzureOpenAi => {
            let resource = base.strip_suffix("/openai/deployments").unwrap_or(base);
            format!("{resource}/openai/models?api-version=2024-10-21")
        },
        _ => format!("{base}/models"),
    })
}

fn sanitized_error(error: &ureq::Error) -> String {
    let message = match error {
        ureq::Error::Timeout(_) => "连接超时".to_owned(),
        ureq::Error::HostNotFound => "找不到服务器".to_owned(),
        ureq::Error::ConnectionFailed => "无法连接服务器".to_owned(),
        ureq::Error::Io(err) => format!("网络错误：{}", err.kind()),
        ureq::Error::Tls(_) => "TLS 握手失败".to_owned(),
        _ => "请求失败".to_owned(),
    };
    message.chars().take(180).collect()
}

fn run_test(provider: &AiProvider) -> (bool, String, u64) {
    let started = Instant::now();
    let elapsed = || started.elapsed().as_millis().min(u64::MAX as u128) as u64;
    let url = match test_url(provider) {
        Ok(url) => url,
        Err(message) => return (false, message, elapsed()),
    };
    if provider.model.trim().is_empty() {
        return (false, "请先填写默认模型".to_owned(), elapsed());
    }
    let key = if provider.kind.requires_api_key() {
        let secret = match load_api_key(&provider.id) {
            Ok(Some(secret)) if !secret.is_empty() => secret,
            Ok(_) => return (false, "请先保存 API Key".to_owned(), elapsed()),
            Err(_) => return (false, "无法从凭据管理器读取 API Key".to_owned(), elapsed()),
        };
        match String::from_utf8(secret) {
            Ok(key) => Zeroizing::new(key),
            Err(_) => return (false, "凭据管理器中的 API Key 编码无效".to_owned(), elapsed()),
        }
    } else {
        Zeroizing::new(String::new())
    };
    let config = ureq::config::Config::builder()
        .timeout_global(Some(Duration::from_secs(12)))
        .http_status_as_error(false)
        .build();
    let agent: ureq::Agent = config.new_agent();
    let mut request = agent.get(&url);
    request = match provider.kind {
        ProviderKind::Anthropic => {
            request.header("x-api-key", key.as_str()).header("anthropic-version", "2023-06-01")
        },
        ProviderKind::Google => request.header("x-goog-api-key", key.as_str()),
        ProviderKind::AzureOpenAi => request.header("api-key", key.as_str()),
        _ if key.is_empty() => request,
        _ => {
            let bearer = Zeroizing::new(format!("Bearer {}", key.as_str()));
            request.header("Authorization", bearer.as_str())
        },
    };
    let response = match request.call() {
        Ok(response) => response,
        Err(error) => return (false, sanitized_error(&error), elapsed()),
    };
    let status = response.status().as_u16();
    let (ok, message) = match status {
        200..=299 => (true, format!("连接成功（HTTP {status}）")),
        401 | 403 => (false, format!("鉴权失败（HTTP {status}），请检查 API Key")),
        404 => (false, "接口不存在（HTTP 404），请检查请求地址".to_owned()),
        429 => (false, "服务限流（HTTP 429），连接可达但当前无法验证额度".to_owned()),
        _ => (false, format!("服务返回 HTTP {status}")),
    };
    (ok, message, elapsed())
}

pub fn spawn_test(
    request: ProviderTestRequest,
    proxy: winit::event_loop::EventLoopProxy<Event>,
    window_id: winit::window::WindowId,
) -> io::Result<()> {
    std::thread::Builder::new()
        .name("nebula-provider-test".into())
        .spawn(move || {
            let provider_id = request.provider.id.clone();
            let (ok, message, elapsed_ms) = run_test(&request.provider);
            let _ = proxy.send_event(Event::new(
                EventType::ProviderTestDone {
                    request_id: request.request_id,
                    provider_id,
                    ok,
                    message,
                    elapsed_ms,
                },
                window_id,
            ));
        })
        .map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presets_have_safe_defaults() {
        for kind in ProviderKind::PRESETS {
            let provider = AiProvider::preset(kind, "test");
            if kind != ProviderKind::Custom {
                assert!(!provider.base_url.is_empty());
            }
            assert!(!provider.api_key_set);
        }
    }

    #[test]
    fn key_hint_never_contains_the_full_secret() {
        let hint = api_key_hint("sk-this-is-a-long-secret");
        assert_eq!(hint, "••••cret");
        assert!(!hint.contains("long-secret"));
    }

    #[test]
    fn normalization_preserves_multiple_custom_entries() {
        let mut store = ProviderStore {
            active_id: "custom-2".to_owned(),
            providers: vec![
                AiProvider::preset(ProviderKind::Custom, "custom-1"),
                AiProvider::preset(ProviderKind::Custom, "custom-2"),
            ],
        };
        normalize(&mut store);
        assert_eq!(
            store.providers.iter().filter(|provider| provider.kind == ProviderKind::Custom).count(),
            2
        );
        assert_eq!(store.active_id, "custom-2");
        assert_eq!(next_custom_id(&store), "custom-3");
    }

    #[test]
    fn provider_test_urls_follow_each_protocol() {
        let openai = AiProvider::preset(ProviderKind::OpenAi, "openai");
        assert_eq!(test_url(&openai).unwrap(), "https://api.openai.com/v1/models");
        let azure = AiProvider::preset(ProviderKind::AzureOpenAi, "azure");
        assert!(test_url(&azure).unwrap().contains("/openai/models?api-version="));
    }
}
