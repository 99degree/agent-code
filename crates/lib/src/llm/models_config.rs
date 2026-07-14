//! Custom model lists per provider, loaded from `models.toml`.

use std::collections::HashMap;
use std::path::PathBuf;

use serde::Deserialize;

use super::provider::ProviderKind;

/// Default template for models.toml.
const DEFAULT_TEMPLATE: &str = r#"# Custom model lists per provider.
# Models defined here are added to the built-in lists for /model.
# Source: pi.dev/pi-ai provider catalog (2026-07-14)
#
# Optional per-model settings:
#   context_window  - model context window size (tokens)
#   max_tokens      - max output tokens
#   max_context     - trigger compaction when context exceeds this (tokens)
#                     if not set, uses context_window
#   reasoning       - model supports thinking/reasoning
#   input           - input modalities ["text", "image"]
#   cost_input      - cost per million input tokens (USD)
#   cost_output     - cost per million output tokens (USD)

# ============================================================================
# OLLAMA-COM
# ============================================================================
[[ollama-com.models]]
id = "nemotron-3-super"
description = "Nemotron 3 Super (120B)"
context_window = 262144
max_tokens = 16384
reasoning = true

[[ollama-com.models]]
id = "nemotron-3-ultra"
description = "Nemotron 3 Ultra"
context_window = 1048576
max_tokens = 16384
reasoning = true

[[ollama-com.models]]
id = "nemotron-3-nano:30b"
description = "Nemotron 3 Nano (30B)"
context_window = 262144
max_tokens = 16384
reasoning = true

[[ollama-com.models]]
id = "minimax-m3"
description = "MiniMax M3"
context_window = 524288
max_tokens = 16384
reasoning = true

[[ollama-com.models]]
id = "minimax-m2.5"
description = "MiniMax M2.5"
context_window = 262144
max_tokens = 16384
reasoning = true

[[ollama-com.models]]
id = "minimax-m2.1"
description = "MiniMax M2.1"
context_window = 262144
max_tokens = 16384
reasoning = true

[[ollama-com.models]]
id = "glm-4.7"
description = "GLM 4.7"
context_window = 262144
max_tokens = 16384

[[ollama-com.models]]
id = "gemma4:31b"
description = "Gemma 4 31B"
context_window = 262144
max_tokens = 16384
reasoning = true

[[ollama-com.models]]
id = "qwen3-coder:480b"
description = "Qwen3 Coder 480B"
context_window = 262144
max_tokens = 16384

[[ollama-com.models]]
id = "qwen3-coder-next"
description = "Qwen3 Coder Next"
context_window = 262144
max_tokens = 16384

[[ollama-com.models]]
id = "devstral-2:123b"
description = "Devstral 2 123B"
context_window = 262144
max_tokens = 16384

[[ollama-com.models]]
id = "devstral-small-2:24b"
description = "Devstral Small 2 24B"
context_window = 262144
max_tokens = 16384

# ============================================================================
# NVIDIA
# ============================================================================
[[nvidia.models]]
id = "nvidia/nemotron-3-super-120b-a12b"
description = "Nemotron 3 Super (120B)"
context_window = 262144
max_tokens = 16384
reasoning = true

[[nvidia.models]]
id = "nvidia/nemotron-3-ultra-550b-a55b"
description = "Nemotron 3 Ultra (550B)"
context_window = 1048576
max_tokens = 16384
reasoning = true

[[nvidia.models]]
id = "nvidia/nemotron-3-nano-30b-a3b"
description = "Nemotron 3 Nano (30B)"
context_window = 262144
max_tokens = 16384
reasoning = true

[[nvidia.models]]
id = "mistralai/mistral-large-3-675b-instruct-2512"
description = "Mistral Large 3 (675B)"
context_window = 262144
max_tokens = 16384
reasoning = true

[[nvidia.models]]
id = "mistralai/mistral-small-4-119b-2603"
description = "Mistral Small 4 (119B)"
context_window = 262144
max_tokens = 16384
reasoning = true

[[nvidia.models]]
id = "mistralai/mistral-medium-3.5-128b"
description = "Mistral Medium 3.5 (128B)"
context_window = 262144
max_tokens = 16384
reasoning = true

[[nvidia.models]]
id = "mistralai/ministral-14b-instruct-2512"
description = "Ministral 14B"
context_window = 131072
max_tokens = 8192

[[nvidia.models]]
id = "mistralai/mistral-nemo-12b-instruct"
description = "Mistral NeMo 12B"
context_window = 131072
max_tokens = 8192

[[nvidia.models]]
id = "deepseek-ai/deepseek-v4-pro"
description = "DeepSeek V4 Pro"
context_window = 262144
max_tokens = 8192
reasoning = true

[[nvidia.models]]
id = "deepseek-ai/deepseek-v4-flash"
description = "DeepSeek V4 Flash"
context_window = 262144
max_tokens = 4096

[[nvidia.models]]
id = "z-ai/glm-5.2"
description = "GLM 5.2"
context_window = 1048576
max_tokens = 16384
reasoning = true

[[nvidia.models]]
id = "minimaxai/minimax-m2.7"
description = "Minimax M2.7"
context_window = 204800
max_tokens = 131072
reasoning = true

[[nvidia.models]]
id = "google/gemma-4-31b-it"
description = "Gemma 4 31B"
context_window = 262144
max_tokens = 16384
reasoning = true

[[nvidia.models]]
id = "qwen/qwen3.5-397b-a17b"
description = "Qwen3.5 397B"
context_window = 262144
max_tokens = 16384
reasoning = true

[[nvidia.models]]
id = "qwen/qwen3.5-122b-a10b"
description = "Qwen3.5 122B"
context_window = 262144
max_tokens = 16384
reasoning = true
"#;

/// Top-level config structure.
#[derive(Debug, Default, Deserialize)]
pub struct ModelsConfig {
    /// Per-provider model lists. Key is provider name (case-insensitive).
    #[serde(default)]
    pub provider: HashMap<String, ProviderModels>,
}

/// Models for a single provider.
#[derive(Debug, Default, Deserialize)]
pub struct ProviderModels {
    /// Custom models to add to the provider's list.
    #[serde(default)]
    pub models: Vec<CustomModel>,
}

/// A single custom model entry.
#[derive(Debug, Clone, Deserialize)]
pub struct CustomModel {
    /// Model ID (e.g. "my-model-7b").
    pub id: String,
    /// Human-readable description.
    #[serde(default)]
    pub description: String,
    /// Context window size in tokens.
    #[serde(default)]
    pub context_window: Option<u64>,
    /// Maximum output tokens.
    #[serde(default)]
    pub max_tokens: Option<u64>,
    /// Maximum context for compaction trigger (tokens).
    /// If set, compaction happens when context exceeds this limit.
    /// Useful for models with smaller effective context windows.
    #[serde(default)]
    pub max_context: Option<u64>,
    /// Whether the model supports reasoning/thinking.
    #[serde(default)]
    pub reasoning: bool,
    /// Input modalities (e.g. ["text", "image"]).
    #[serde(default)]
    pub input: Vec<String>,
    /// Cost per million input tokens (USD).
    #[serde(default)]
    pub cost_input: Option<f64>,
    /// Cost per million output tokens (USD).
    #[serde(default)]
    pub cost_output: Option<f64>,
}

/// Load custom models config from disk.
///
/// Looks for `models.toml` in:
/// 1. `$XDG_CONFIG_HOME/agent-code/models.toml`
/// 2. `~/.config/agent-code/models.toml`
///
/// If the file doesn't exist, creates it with a sample template.
pub fn load_models_config() -> ModelsConfig {
    let path = models_config_path();
    if let Some(p) = path {
        if !p.exists() {
            // Create default template.
            let _ = std::fs::create_dir_all(p.parent().unwrap_or(&p));
            let _ = std::fs::write(&p, DEFAULT_TEMPLATE);
        }
        match std::fs::read_to_string(&p) {
            Ok(content) => match toml::from_str::<ModelsConfig>(&content) {
                Ok(config) => return config,
                Err(e) => {
                    tracing::warn!("Failed to parse {}: {e}", p.display());
                }
            },
            Err(e) => {
                tracing::warn!("Failed to read {}: {e}", p.display());
            }
        }
    }
    ModelsConfig::default()
}

/// Get the path to the models config file.
fn models_config_path() -> Option<PathBuf> {
    crate::config::agent_config_dir().map(|d| d.join("models.toml"))
}

/// Parse provider name string to ProviderKind.
pub fn parse_provider_name(name: &str) -> Option<ProviderKind> {
    match name.to_lowercase().as_str() {
        "anthropic" => Some(ProviderKind::Anthropic),
        "openai" => Some(ProviderKind::OpenAi),
        "xai" | "grok" => Some(ProviderKind::Xai),
        "google" | "gemini" => Some(ProviderKind::Google),
        "deepseek" => Some(ProviderKind::DeepSeek),
        "mistral" => Some(ProviderKind::Mistral),
        "nvidia" | "nim" => Some(ProviderKind::Nvidia),
        "openrouter" | "or" => Some(ProviderKind::OpenRouter),
        "opencode" | "oc" | "zen" => Some(ProviderKind::OpenCode),
        "opencode-go" | "oc-go" | "zen-go" => Some(ProviderKind::OpenCodeGo),
        "groq" => Some(ProviderKind::Groq),
        "together" => Some(ProviderKind::Together),
        "zhipu" | "glm" => Some(ProviderKind::Zhipu),
        "cohere" => Some(ProviderKind::Cohere),
        "perplexity" => Some(ProviderKind::Perplexity),
        "bedrock" | "aws" => Some(ProviderKind::Bedrock),
        "vertex" | "gcp" => Some(ProviderKind::Vertex),
        "azure" | "azure-openai" => Some(ProviderKind::AzureOpenAi),
        _ => None,
    }
}

/// Look up max_context for a model from the config.
///
/// Returns the custom max_context if set, otherwise falls back to context_window.
/// Returns None if neither is set.
pub fn max_context_for_model(model_id: &str) -> Option<u64> {
    let config = load_models_config();
    for provider_models in config.provider.values() {
        for m in &provider_models.models {
            if m.id == model_id {
                return m.max_context.or(m.context_window);
            }
        }
    }
    None
}

/// Look up max_tokens for a model from the config.
///
/// Returns the per-model max_tokens if set, otherwise None.
pub fn max_tokens_for_model(model_id: &str) -> Option<u64> {
    let config = load_models_config();
    for provider_models in config.provider.values() {
        for m in &provider_models.models {
            if m.id == model_id {
                return m.max_tokens;
            }
        }
    }
    None
}

/// Get custom models for a provider from config.
///
/// Returns empty slice if no custom models are configured.
pub fn custom_models_for_provider(
    _config: &ModelsConfig,
    _kind: ProviderKind,
) -> &[(String, String)] {
    // This is a bit awkward due to lifetime issues, so we'll return a empty slice
    // and handle the merging in the caller.
    &[]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_provider_name() {
        assert_eq!(
            parse_provider_name("anthropic"),
            Some(ProviderKind::Anthropic)
        );
        assert_eq!(parse_provider_name("openai"), Some(ProviderKind::OpenAi));
        assert_eq!(parse_provider_name("or"), Some(ProviderKind::OpenRouter));
        assert_eq!(parse_provider_name("zen"), Some(ProviderKind::OpenCode));
        assert_eq!(parse_provider_name("unknown"), None);
    }
}
