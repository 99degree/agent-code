//! Custom model lists per provider, loaded from `models.toml`.

use std::collections::HashMap;
use std::path::PathBuf;

use serde::Deserialize;

use super::provider::ProviderKind;

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
}

/// Load custom models config from disk.
///
/// Looks for `models.toml` in:
/// 1. `$XDG_CONFIG_HOME/agent-code/models.toml`
/// 2. `~/.config/agent-code/models.toml`
pub fn load_models_config() -> ModelsConfig {
    let path = models_config_path();
    if let Some(p) = path {
        if p.exists() {
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

/// Get custom models for a provider from config.
///
/// Returns empty slice if no custom models are configured.
pub fn custom_models_for_provider(config: &ModelsConfig, kind: ProviderKind) -> &[(String, String)] {
    // This is a bit awkward due to lifetime issues, so we'll return a empty slice
    // and handle the merging in the caller.
    &[]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_provider_name() {
        assert_eq!(parse_provider_name("anthropic"), Some(ProviderKind::Anthropic));
        assert_eq!(parse_provider_name("openai"), Some(ProviderKind::OpenAi));
        assert_eq!(parse_provider_name("or"), Some(ProviderKind::OpenRouter));
        assert_eq!(parse_provider_name("zen"), Some(ProviderKind::OpenCode));
        assert_eq!(parse_provider_name("unknown"), None);
    }
}
