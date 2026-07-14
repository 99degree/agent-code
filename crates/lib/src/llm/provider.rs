//! LLM provider abstraction.
//!
//! Two wire formats cover the entire ecosystem:
//! - Anthropic Messages API (Claude models)
//! - OpenAI Chat Completions (GPT, plus Groq, Together, Ollama, DeepSeek, etc.)
//!
//! Each provider translates between our unified message types and
//! the provider-specific JSON format for requests and SSE streams.

use async_trait::async_trait;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use super::message::Message;
use super::stream::StreamEvent;
use crate::tools::ToolSchema;

/// Unified provider trait. Both Anthropic and OpenAI-compatible
/// endpoints implement this.
#[async_trait]
pub trait Provider: Send + Sync {
    /// Human-readable provider name.
    fn name(&self) -> &str;

    /// Send a streaming request. Returns a channel of events.
    async fn stream(
        &self,
        request: &ProviderRequest,
    ) -> Result<mpsc::Receiver<StreamEvent>, ProviderError>;
}

/// Tool choice mode for controlling tool usage.
#[derive(Debug, Clone, Default)]
pub enum ToolChoice {
    /// Model decides whether to use tools.
    #[default]
    Auto,
    /// Model must use a tool.
    Any,
    /// Model must not use tools.
    None,
    /// Model must use a specific tool.
    Specific(String),
}

/// A provider-agnostic request.
pub struct ProviderRequest {
    pub messages: Vec<Message>,
    pub system_prompt: String,
    pub tools: Vec<ToolSchema>,
    pub model: String,
    pub max_tokens: u32,
    pub temperature: Option<f64>,
    pub enable_caching: bool,
    /// Controls whether/how the model should use tools.
    pub tool_choice: ToolChoice,
    /// Metadata to send with the request (e.g., user_id for Anthropic).
    pub metadata: Option<serde_json::Value>,
    /// Cancellation token for interrupting the in-flight streaming HTTP read.
    /// Providers must race `byte_stream.next().await` against
    /// `cancel.cancelled()` so that the spawned streaming task exits
    /// promptly when the user presses Escape or Ctrl+C. Background callers
    /// (memory extraction, consolidation) can pass `CancellationToken::new()`
    /// for an uncancellable request.
    pub cancel: CancellationToken,
}

/// Provider-level errors.
#[derive(Debug)]
pub enum ProviderError {
    Auth(String),
    RateLimited { retry_after_ms: u64 },
    Overloaded,
    RequestTooLarge(String),
    Network(String),
    InvalidResponse(String),
}

impl std::fmt::Display for ProviderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Auth(msg) => write!(f, "auth: {msg}"),
            Self::RateLimited { retry_after_ms } => {
                write!(f, "rate limited (retry in {retry_after_ms}ms)")
            }
            Self::Overloaded => write!(f, "server overloaded"),
            Self::RequestTooLarge(msg) => write!(f, "request too large: {msg}"),
            Self::Network(msg) => write!(f, "network: {msg}"),
            Self::InvalidResponse(msg) => write!(f, "invalid response: {msg}"),
        }
    }
}

/// Detect the right provider from a model name or base URL.
/// Suggested models for a provider, as `(id, description)` pairs.
///
/// Powers the `/model` interactive selector and its tab-completion, so
/// both surfaces stay in sync. Providers without a curated list return
/// an empty slice (the caller falls back to "type any name"). These are
/// suggestions, not an allow-list — `/model <name>` accepts any string.
pub fn models_for_provider(kind: ProviderKind) -> &'static [(&'static str, &'static str)] {
    match kind {
        ProviderKind::Anthropic | ProviderKind::Bedrock | ProviderKind::Vertex => &[
            ("claude-opus-4-8", "Opus 4.8 · Most capable"),
            ("claude-sonnet-5", "Sonnet 5 · Balanced"),
            ("claude-haiku-4-5", "Haiku 4.5 · Fast"),
            ("claude-fable-5", "Fable 5 · Frontier"),
        ],
        ProviderKind::OpenAi => &[
            ("gpt-5.5", "GPT-5.5 · Most capable"),
            ("gpt-5.5-pro", "GPT-5.5 Pro · Reasoning"),
            ("gpt-5.4", "GPT-5.4 · Balanced"),
            ("gpt-5.4-mini", "GPT-5.4 Mini · Fast"),
            ("gpt-5.4-nano", "GPT-5.4 Nano · Fastest"),
            ("o3", "o3 · Reasoning"),
        ],
        ProviderKind::Xai => &[
            ("grok-build-0.1", "Grok Build · Agentic coding (SuperGrok)"),
            ("grok-4.5", "Grok 4.5 · Flagship"),
            ("grok-4.3", "Grok 4.3 · Previous flagship"),
            ("grok-4", "Grok 4 · Balanced"),
        ],
        ProviderKind::Google => &[
            ("gemini-3-pro", "Gemini 3 Pro · Most capable"),
            ("gemini-3.5-flash", "Gemini 3.5 Flash · Fast"),
            ("gemini-2.5-flash", "Gemini 2.5 Flash · Previous gen"),
        ],
        ProviderKind::DeepSeek => &[
            ("deepseek-chat", "DeepSeek Chat · General"),
            ("deepseek-reasoner", "DeepSeek Reasoner · Reasoning"),
        ],
        ProviderKind::Mistral => &[
            ("mistral-large-latest", "Mistral Large · Most capable"),
            ("codestral-latest", "Codestral · Code-focused"),
        ],
        ProviderKind::Zhipu => &[
            ("glm-4.7", "GLM-4.7 · Latest"),
            ("glm-4.6", "GLM-4.6 · Balanced"),
            ("glm-4.6-air", "GLM-4.6 Air · Fast"),
            ("glm-4.5", "GLM-4.5 · Previous gen"),
        ],
        ProviderKind::Cohere => &[
            ("command-r-plus", "Command R+ · Most capable"),
            ("command-r", "Command R · Balanced"),
            ("command-light", "Command Light · Fast"),
        ],
        ProviderKind::Perplexity => &[
            ("sonar-pro", "Sonar Pro · Most capable, web search"),
            ("sonar", "Sonar · Balanced, web search"),
            ("sonar-deep-research", "Sonar Deep Research · In-depth"),
        ],
        ProviderKind::Nvidia => &[
            ("meta/llama-3.1-8b-instruct", "Llama 3.1 8B · Fast, open"),
            (
                "nvidia/llama-3.3-nemotron-super-49b-v1",
                "Nemotron Super 49B · Capable",
            ),
            ("deepseek-ai/deepseek-r1", "DeepSeek R1 · Reasoning"),
            (
                "nvidia/llama-3.1-nemotron-70b-instruct",
                "Nemotron 70B · Instruct",
            ),
            ("minimaxai/minimax-m3", "MiniMax M3"),
        ],
        ProviderKind::OpenRouter => &[
            ("anthropic/claude-sonnet-5", "Claude Sonnet 5 · Balanced"),
            ("anthropic/claude-opus-4.8", "Claude Opus 4.8 · Most capable"),
            ("openai/gpt-5.5", "GPT-5.5 · Most capable"),
            ("google/gemini-3-pro", "Gemini 3 Pro"),
            ("x-ai/grok-4.3", "Grok 4.3"),
            ("deepseek/deepseek-v4-pro", "DeepSeek V4 Pro · Open"),
            ("hy3-free", "Hy3 Free"),
        ],
        ProviderKind::OpenCodeGo => &[
            ("deepseek-v4-flash", "DeepSeek V4 Flash · Fast"),
            ("deepseek-v4-pro", "DeepSeek V4 Pro"),
            ("glm-5.1", "GLM-5.1"),
            ("glm-5.2", "GLM-5.2"),
            ("kimi-k2.6", "Kimi K2.6"),
            ("kimi-k2.7-code", "Kimi K2.7 Code"),
            ("mimo-v2.5", "MiMo V2.5"),
            ("mimo-v2.5-pro", "MiMo V2.5 Pro"),
            ("minimax-m2.7", "MiniMax M2.7"),
            ("minimax-m3", "MiniMax M3"),
            ("qwen3.6-plus", "Qwen3.6 Plus"),
            ("qwen3.7-max", "Qwen3.7 Max"),
            ("qwen3.7-plus", "Qwen3.7 Plus"),
        ],
        ProviderKind::OpenCode => &[
            ("claude-sonnet-5", "Claude Sonnet 5"),
            ("claude-opus-4-8", "Claude Opus 4.8"),
            ("claude-opus-4-7", "Claude Opus 4.7"),
            ("claude-opus-4-6", "Claude Opus 4.6"),
            ("claude-sonnet-4-6", "Claude Sonnet 4.6"),
            ("claude-sonnet-4-5", "Claude Sonnet 4.5"),
            ("claude-haiku-4-5", "Claude Haiku 4.5"),
            ("gpt-5.5", "GPT-5.5"),
            ("gpt-5.5-pro", "GPT-5.5 Pro"),
            ("gpt-5.4", "GPT-5.4"),
            ("gpt-5.4-pro", "GPT-5.4 Pro"),
            ("gpt-5.4-mini", "GPT-5.4 Mini"),
            ("gpt-5.2", "GPT-5.2"),
            ("gpt-5.2-codex", "GPT-5.2 Codex"),
            ("gpt-5.1", "GPT-5.1"),
            ("gpt-5.1-codex", "GPT-5.1 Codex"),
            ("gpt-5", "GPT-5"),
            ("gemini-3.5-flash", "Gemini 3.5 Flash"),
            ("gemini-3.1-pro", "Gemini 3.1 Pro"),
            ("gemini-3-flash", "Gemini 3 Flash"),
            ("deepseek-v4-pro", "DeepSeek V4 Pro"),
            ("deepseek-v4-flash", "DeepSeek V4 Flash"),
            ("grok-4.5", "Grok 4.5"),
            ("grok-build-0.1", "Grok Build 0.1"),
            ("kimi-k2.7-code", "Kimi K2.7 Code"),
            ("kimi-k2.6", "Kimi K2.6"),
            ("qwen3.6-plus", "Qwen3.6 Plus"),
            ("qwen3.5-plus", "Qwen3.5 Plus"),
            ("minimax-m3", "MiniMax M3"),
            ("glm-5.2", "GLM-5.2"),
        ],
        _ => &[],
    }
}

/// Get all models for a provider, including custom models from config.
///
/// Returns a Vec of (id, description) tuples.
pub fn models_for_provider_with_custom(kind: ProviderKind) -> Vec<(&'static str, &'static str)> {
    let mut models: Vec<(&str, &str)> = models_for_provider(kind).to_vec();

    // Load custom models from config.
    let config = super::models_config::load_models_config();
    let provider_name = match kind {
        ProviderKind::Anthropic => "anthropic",
        ProviderKind::OpenAi => "openai",
        ProviderKind::Xai => "xai",
        ProviderKind::Google => "google",
        ProviderKind::DeepSeek => "deepseek",
        ProviderKind::Mistral => "mistral",
        ProviderKind::Nvidia => "nvidia",
        ProviderKind::OpenRouter => "openrouter",
        ProviderKind::OpenCode => "opencode",
        ProviderKind::OpenCodeGo => "opencode-go",
        ProviderKind::Groq => "groq",
        ProviderKind::Together => "together",
        ProviderKind::Zhipu => "zhipu",
        ProviderKind::Cohere => "cohere",
        ProviderKind::Perplexity => "perplexity",
        ProviderKind::Bedrock => "bedrock",
        ProviderKind::Vertex => "vertex",
        ProviderKind::AzureOpenAi => "azure",
        ProviderKind::OpenAiCompatible => "openai-compatible",
    };

    if let Some(provider_models) = config.provider.get(provider_name) {
        for custom in &provider_models.models {
            // Don't add duplicates.
            if !models.iter().any(|(id, _)| *id == custom.id.as_str()) {
                // Leak the strings to get 'static lifetime.
                // This is acceptable since models config is small and loaded once.
                let id: &'static str = Box::leak(custom.id.clone().into_boxed_str());
                let desc: &'static str = Box::leak(custom.description.clone().into_boxed_str());
                models.push((id, desc));
            }
        }
    }

    models
}

/// Create a provider from config (model, base_url, api_key).
/// Used by `/model` to recreate the provider when switching models.
pub fn create_provider_from_config(
    model: &str,
    base_url: &str,
    api_key: &str,
) -> std::sync::Arc<dyn Provider> {
    let kind = detect_provider(model, base_url);
    match kind {
        ProviderKind::AzureOpenAi => {
            std::sync::Arc::new(crate::llm::azure_openai::AzureOpenAiProvider::new(
                base_url,
                api_key,
            ))
        }
        _ => match kind.wire_format() {
            WireFormat::Anthropic => {
                std::sync::Arc::new(crate::llm::anthropic::AnthropicProvider::new(
                    base_url,
                    api_key,
                ))
            }
            WireFormat::OpenAiCompatible => {
                std::sync::Arc::new(crate::llm::openai::OpenAiProvider::new(
                    base_url,
                    api_key,
                ))
            }
        },
    }
}

pub fn detect_provider(model: &str, base_url: &str) -> ProviderKind {
    let model_lower = model.to_lowercase();
    let url_lower = base_url.to_lowercase();

    // AWS Bedrock (Claude via AWS).
    if url_lower.contains("bedrock") || url_lower.contains("amazonaws.com") {
        return ProviderKind::Bedrock;
    }
    // Google Vertex AI (Claude via GCP).
    if url_lower.contains("aiplatform.googleapis.com") {
        return ProviderKind::Vertex;
    }
    if url_lower.contains("anthropic.com") {
        return ProviderKind::Anthropic;
    }
    // Azure OpenAI — must be checked before generic openai.com.
    if url_lower.contains("openai.azure.com")
        || url_lower.contains("azure.com") && url_lower.contains("openai")
    {
        return ProviderKind::AzureOpenAi;
    }
    if url_lower.contains("openai.com") {
        return ProviderKind::OpenAi;
    }
    if url_lower.contains("x.ai") || url_lower.contains("xai.") {
        return ProviderKind::Xai;
    }
    if url_lower.contains("googleapis.com") || url_lower.contains("google") {
        return ProviderKind::Google;
    }
    if url_lower.contains("deepseek.com") {
        return ProviderKind::DeepSeek;
    }
    if url_lower.contains("groq.com") {
        return ProviderKind::Groq;
    }
    if url_lower.contains("mistral.ai") {
        return ProviderKind::Mistral;
    }
    if url_lower.contains("together.xyz") || url_lower.contains("together.ai") {
        return ProviderKind::Together;
    }
    if url_lower.contains("bigmodel.cn")
        || url_lower.contains("z.ai")
        || url_lower.contains("zhipu")
    {
        return ProviderKind::Zhipu;
    }
    if url_lower.contains("openrouter.ai") {
        return ProviderKind::OpenRouter;
    }
    if url_lower.contains("opencode.ai/zen/go") {
        return ProviderKind::OpenCodeGo;
    }
    if url_lower.contains("opencode.ai") {
        return ProviderKind::OpenCode;
    }
    if url_lower.contains("cohere.com") || url_lower.contains("cohere.ai") {
        return ProviderKind::Cohere;
    }
    if url_lower.contains("perplexity.ai") {
        return ProviderKind::Perplexity;
    }
    if url_lower.contains("nvidia") || url_lower.contains("nvidianim") {
        return ProviderKind::Nvidia;
    }
    if url_lower.contains("localhost") || url_lower.contains("127.0.0.1") {
        return ProviderKind::OpenAiCompatible;
    }

    // Detect from model name.
    if model_lower.starts_with("claude")
        || model_lower.contains("opus")
        || model_lower.contains("sonnet")
        || model_lower.contains("haiku")
    {
        return ProviderKind::Anthropic;
    }
    if model_lower.starts_with("gpt")
        || model_lower.starts_with("o1")
        || model_lower.starts_with("o3")
    {
        return ProviderKind::OpenAi;
    }
    if model_lower.starts_with("grok") {
        return ProviderKind::Xai;
    }
    if model_lower.starts_with("gemini") {
        return ProviderKind::Google;
    }
    if model_lower.starts_with("deepseek") {
        return ProviderKind::DeepSeek;
    }
    if model_lower.starts_with("llama") && url_lower.contains("groq") {
        return ProviderKind::Groq;
    }
    if model_lower.starts_with("mistral") || model_lower.starts_with("codestral") {
        return ProviderKind::Mistral;
    }
    if model_lower.starts_with("glm") {
        return ProviderKind::Zhipu;
    }
    if model_lower.starts_with("command") {
        return ProviderKind::Cohere;
    }
    if model_lower.starts_with("pplx") || model_lower.starts_with("sonar") {
        return ProviderKind::Perplexity;
    }

    ProviderKind::OpenAiCompatible
}

/// The two wire formats that cover the entire LLM ecosystem.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WireFormat {
    /// Anthropic Messages API (Claude models, Bedrock, Vertex).
    Anthropic,
    /// OpenAI Chat Completions (GPT, Groq, Together, Ollama, DeepSeek, etc.).
    OpenAiCompatible,
}

/// Provider kinds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderKind {
    Anthropic,
    Bedrock,
    Vertex,
    OpenAi,
    AzureOpenAi,
    Xai,
    Google,
    DeepSeek,
    Groq,
    Mistral,
    Together,
    Zhipu,
    OpenRouter,
    OpenCode,
    OpenCodeGo,
    Cohere,
    Perplexity,
    Nvidia,
    OpenAiCompatible,
}

impl ProviderKind {
    /// All provider kinds (for iteration).
    pub fn all() -> &'static [ProviderKind] {
        &[
            Self::Anthropic,
            Self::OpenAi,
            Self::Xai,
            Self::Google,
            Self::DeepSeek,
            Self::Mistral,
            Self::Nvidia,
            Self::OpenRouter,
            Self::OpenCode,
            Self::OpenCodeGo,
            Self::Groq,
            Self::Together,
            Self::Zhipu,
            Self::Cohere,
            Self::Perplexity,
            Self::Bedrock,
            Self::Vertex,
            Self::AzureOpenAi,
            Self::OpenAiCompatible,
        ]
    }

    /// Check if this provider has an API key configured.
    pub fn is_configured(&self) -> bool {
        // Skip providers that don't use simple API key auth.
        if matches!(self, Self::Bedrock | Self::Vertex) {
            return false;
        }
        // OpenAiCompatible is a fallback, not a real provider.
        if matches!(self, Self::OpenAiCompatible) {
            return false;
        }
        self.api_key_from_env().is_some()
    }

    /// Get API key from environment, with fallback support.
    pub fn api_key_from_env(&self) -> Option<String> {
        // Primary env var.
        if let Ok(key) = std::env::var(self.env_var_name()) {
            if !key.is_empty() {
                return Some(key);
            }
        }
        // Fallback env vars.
        match self {
            Self::OpenCode => {
                // OPENCODE_ZEN_API_KEY → OPENCODE_API_KEY
                std::env::var("OPENCODE_API_KEY").ok().filter(|k| !k.is_empty())
            }
            _ => None,
        }
    }

    /// Which wire format this provider uses.
    pub fn wire_format(&self) -> WireFormat {
        match self {
            Self::Anthropic | Self::Bedrock | Self::Vertex => WireFormat::Anthropic,
            Self::OpenAi
            | Self::AzureOpenAi
            | Self::Xai
            | Self::Google
            | Self::DeepSeek
            | Self::Groq
            | Self::Mistral
            | Self::Together
            | Self::Zhipu
            | Self::OpenRouter
            | Self::OpenCode
            | Self::OpenCodeGo
            | Self::Cohere
            | Self::Perplexity
            | Self::Nvidia
            | Self::OpenAiCompatible => WireFormat::OpenAiCompatible,
        }
    }

    /// The default base URL for this provider, or `None` for providers
    /// whose URL must come from user configuration (Bedrock, Vertex,
    /// and generic OpenAI-compatible endpoints).
    pub fn default_base_url(&self) -> Option<&str> {
        match self {
            Self::Anthropic => Some("https://api.anthropic.com/v1"),
            Self::OpenAi => Some("https://api.openai.com/v1"),
            Self::Xai => Some("https://api.x.ai/v1"),
            Self::Google => Some("https://generativelanguage.googleapis.com/v1beta/openai"),
            Self::DeepSeek => Some("https://api.deepseek.com/v1"),
            Self::Groq => Some("https://api.groq.com/openai/v1"),
            Self::Mistral => Some("https://api.mistral.ai/v1"),
            Self::Together => Some("https://api.together.xyz/v1"),
            Self::Zhipu => Some("https://open.bigmodel.cn/api/paas/v4"),
            Self::OpenRouter => Some("https://openrouter.ai/api/v1"),
            Self::OpenCode => Some("https://opencode.ai/zen/v1"),
            Self::OpenCodeGo => Some("https://opencode.ai/zen/go/v1"),
            Self::Cohere => Some("https://api.cohere.com/v2"),
            Self::Perplexity => Some("https://api.perplexity.ai"),
            Self::Nvidia => Some("https://integrate.api.nvidia.com/v1"),
            // These require user-supplied URLs.
            Self::Bedrock | Self::Vertex | Self::AzureOpenAi | Self::OpenAiCompatible => None,
        }
    }

    /// The environment variable name conventionally used for this provider's API key.
    pub fn env_var_name(&self) -> &str {
        match self {
            Self::Anthropic | Self::Bedrock | Self::Vertex => "ANTHROPIC_API_KEY",
            Self::OpenAi => "OPENAI_API_KEY",
            Self::AzureOpenAi => "AZURE_OPENAI_API_KEY",
            Self::Xai => "XAI_API_KEY",
            Self::Google => "GOOGLE_API_KEY",
            Self::DeepSeek => "DEEPSEEK_API_KEY",
            Self::Groq => "GROQ_API_KEY",
            Self::Mistral => "MISTRAL_API_KEY",
            Self::Together => "TOGETHER_API_KEY",
            Self::Zhipu => "ZHIPU_API_KEY",
            Self::OpenRouter => "OPENROUTER_API_KEY",
            Self::OpenCode => "OPENCODE_ZEN_API_KEY",
            Self::OpenCodeGo => "OPENCODE_GO_API_KEY",
            Self::Cohere => "COHERE_API_KEY",
            Self::Perplexity => "PERPLEXITY_API_KEY",
            Self::Nvidia => "NVIDIA_API_KEY",
            Self::OpenAiCompatible => "OPENAI_API_KEY",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn models_for_provider_returns_expected_catalogs() {
        // Anthropic-family providers share the Claude catalog.
        for k in [
            ProviderKind::Anthropic,
            ProviderKind::Bedrock,
            ProviderKind::Vertex,
        ] {
            let models = models_for_provider(k);
            assert!(models.iter().any(|(id, _)| id.starts_with("claude-")));
        }
        // OpenAI has gpt/o-series; xAI has grok; provider without a
        // curated list returns empty.
        assert!(
            models_for_provider(ProviderKind::OpenAi)
                .iter()
                .any(|(id, _)| id.starts_with("gpt-"))
        );
        assert!(
            models_for_provider(ProviderKind::Xai)
                .iter()
                .any(|(id, _)| id.starts_with("grok-"))
        );
        assert!(models_for_provider(ProviderKind::OpenAiCompatible).is_empty());
    }

    #[test]
    fn test_detect_from_url_anthropic() {
        assert!(matches!(
            detect_provider("any", "https://api.anthropic.com/v1"),
            ProviderKind::Anthropic
        ));
    }

    #[test]
    fn test_detect_from_url_openai() {
        assert!(matches!(
            detect_provider("any", "https://api.openai.com/v1"),
            ProviderKind::OpenAi
        ));
    }

    #[test]
    fn test_detect_from_url_bedrock() {
        assert!(matches!(
            detect_provider("any", "https://bedrock-runtime.us-east-1.amazonaws.com"),
            ProviderKind::Bedrock
        ));
    }

    #[test]
    fn test_detect_from_url_vertex() {
        assert!(matches!(
            detect_provider("any", "https://us-central1-aiplatform.googleapis.com/v1"),
            ProviderKind::Vertex
        ));
    }

    #[test]
    fn test_detect_from_url_azure_openai() {
        assert!(matches!(
            detect_provider(
                "any",
                "https://myresource.openai.azure.com/openai/deployments/gpt-4"
            ),
            ProviderKind::AzureOpenAi
        ));
    }

    #[test]
    fn test_detect_azure_before_generic_openai() {
        // Azure URL contains "openai" but should match Azure, not generic OpenAI.
        assert!(matches!(
            detect_provider(
                "gpt-4",
                "https://myresource.openai.azure.com/openai/deployments/gpt-4"
            ),
            ProviderKind::AzureOpenAi
        ));
    }

    #[test]
    fn test_detect_from_url_xai() {
        assert!(matches!(
            detect_provider("any", "https://api.x.ai/v1"),
            ProviderKind::Xai
        ));
    }

    #[test]
    fn test_detect_from_url_deepseek() {
        assert!(matches!(
            detect_provider("any", "https://api.deepseek.com/v1"),
            ProviderKind::DeepSeek
        ));
    }

    #[test]
    fn test_detect_from_url_groq() {
        assert!(matches!(
            detect_provider("any", "https://api.groq.com/openai/v1"),
            ProviderKind::Groq
        ));
    }

    #[test]
    fn test_detect_from_url_mistral() {
        assert!(matches!(
            detect_provider("any", "https://api.mistral.ai/v1"),
            ProviderKind::Mistral
        ));
    }

    #[test]
    fn test_detect_from_url_together() {
        assert!(matches!(
            detect_provider("any", "https://api.together.xyz/v1"),
            ProviderKind::Together
        ));
    }

    #[test]
    fn test_detect_from_url_cohere() {
        assert!(matches!(
            detect_provider("any", "https://api.cohere.com/v2"),
            ProviderKind::Cohere
        ));
    }

    #[test]
    fn test_detect_from_url_perplexity() {
        assert!(matches!(
            detect_provider("any", "https://api.perplexity.ai"),
            ProviderKind::Perplexity
        ));
    }

    #[test]
    fn test_detect_from_url_nvidia() {
        assert!(matches!(
            detect_provider("any", "https://integrate.api.nvidia.com/v1"),
            ProviderKind::Nvidia
        ));
        assert!(matches!(
            detect_provider("any", "https://ai.api.nvidia.com/v1"),
            ProviderKind::Nvidia
        ));
    }

    #[test]
    fn test_detect_from_model_command_r() {
        assert!(matches!(
            detect_provider("command-r-plus", ""),
            ProviderKind::Cohere
        ));
    }

    #[test]
    fn test_detect_from_model_sonar() {
        assert!(matches!(
            detect_provider("sonar-pro", ""),
            ProviderKind::Perplexity
        ));
    }

    #[test]
    fn test_detect_from_url_openrouter() {
        assert!(matches!(
            detect_provider("any", "https://openrouter.ai/api/v1"),
            ProviderKind::OpenRouter
        ));
    }

    #[test]
    fn test_detect_from_url_localhost() {
        assert!(matches!(
            detect_provider("any", "http://localhost:11434/v1"),
            ProviderKind::OpenAiCompatible
        ));
    }

    #[test]
    fn test_detect_from_model_claude() {
        assert!(matches!(
            detect_provider("claude-sonnet-4", ""),
            ProviderKind::Anthropic
        ));
        assert!(matches!(
            detect_provider("claude-opus-4", ""),
            ProviderKind::Anthropic
        ));
    }

    #[test]
    fn test_detect_from_model_gpt() {
        assert!(matches!(
            detect_provider("gpt-4.1-mini", ""),
            ProviderKind::OpenAi
        ));
        assert!(matches!(
            detect_provider("o3-mini", ""),
            ProviderKind::OpenAi
        ));
    }

    #[test]
    fn test_detect_from_model_grok() {
        assert!(matches!(detect_provider("grok-3", ""), ProviderKind::Xai));
    }

    #[test]
    fn test_detect_from_model_gemini() {
        assert!(matches!(
            detect_provider("gemini-2.5-flash", ""),
            ProviderKind::Google
        ));
    }

    #[test]
    fn test_detect_unknown_defaults_openai_compat() {
        assert!(matches!(
            detect_provider("some-random-model", "https://my-server.com"),
            ProviderKind::OpenAiCompatible
        ));
    }

    #[test]
    fn test_url_takes_priority_over_model() {
        // URL says OpenAI but model says Claude — URL wins.
        assert!(matches!(
            detect_provider("claude-sonnet", "https://api.openai.com/v1"),
            ProviderKind::OpenAi
        ));
    }

    #[test]
    fn test_wire_format_anthropic_family() {
        assert_eq!(ProviderKind::Anthropic.wire_format(), WireFormat::Anthropic);
        assert_eq!(ProviderKind::Bedrock.wire_format(), WireFormat::Anthropic);
        assert_eq!(ProviderKind::Vertex.wire_format(), WireFormat::Anthropic);
    }

    #[test]
    fn test_wire_format_openai_compatible_family() {
        let openai_compat_providers = [
            ProviderKind::OpenAi,
            ProviderKind::Xai,
            ProviderKind::Google,
            ProviderKind::DeepSeek,
            ProviderKind::Groq,
            ProviderKind::Mistral,
            ProviderKind::Together,
            ProviderKind::Zhipu,
            ProviderKind::OpenRouter,
            ProviderKind::Cohere,
            ProviderKind::Perplexity,
            ProviderKind::Nvidia,
            ProviderKind::OpenAiCompatible,
        ];
        for p in openai_compat_providers {
            assert_eq!(
                p.wire_format(),
                WireFormat::OpenAiCompatible,
                "{p:?} should use OpenAiCompatible wire format"
            );
        }
    }

    #[test]
    fn test_default_base_url_returns_some_for_known_providers() {
        let providers_with_urls = [
            ProviderKind::Anthropic,
            ProviderKind::OpenAi,
            ProviderKind::Xai,
            ProviderKind::Google,
            ProviderKind::DeepSeek,
            ProviderKind::Groq,
            ProviderKind::Mistral,
            ProviderKind::Together,
            ProviderKind::Zhipu,
            ProviderKind::OpenRouter,
            ProviderKind::Cohere,
            ProviderKind::Perplexity,
            ProviderKind::Nvidia,
        ];
        for p in providers_with_urls {
            assert!(
                p.default_base_url().is_some(),
                "{p:?} should have a default base URL"
            );
        }
    }

    #[test]
    fn test_default_base_url_returns_none_for_user_configured() {
        assert!(ProviderKind::Bedrock.default_base_url().is_none());
        assert!(ProviderKind::Vertex.default_base_url().is_none());
        assert!(ProviderKind::AzureOpenAi.default_base_url().is_none());
        assert!(ProviderKind::OpenAiCompatible.default_base_url().is_none());
    }

    #[test]
    fn test_env_var_name_all_variants() {
        assert_eq!(ProviderKind::Anthropic.env_var_name(), "ANTHROPIC_API_KEY");
        assert_eq!(ProviderKind::Bedrock.env_var_name(), "ANTHROPIC_API_KEY");
        assert_eq!(ProviderKind::Vertex.env_var_name(), "ANTHROPIC_API_KEY");
        assert_eq!(ProviderKind::OpenAi.env_var_name(), "OPENAI_API_KEY");
        assert_eq!(
            ProviderKind::AzureOpenAi.env_var_name(),
            "AZURE_OPENAI_API_KEY"
        );
        assert_eq!(ProviderKind::Xai.env_var_name(), "XAI_API_KEY");
        assert_eq!(ProviderKind::Google.env_var_name(), "GOOGLE_API_KEY");
        assert_eq!(ProviderKind::DeepSeek.env_var_name(), "DEEPSEEK_API_KEY");
        assert_eq!(ProviderKind::Groq.env_var_name(), "GROQ_API_KEY");
        assert_eq!(ProviderKind::Mistral.env_var_name(), "MISTRAL_API_KEY");
        assert_eq!(ProviderKind::Together.env_var_name(), "TOGETHER_API_KEY");
        assert_eq!(ProviderKind::Zhipu.env_var_name(), "ZHIPU_API_KEY");
        assert_eq!(
            ProviderKind::OpenRouter.env_var_name(),
            "OPENROUTER_API_KEY"
        );
        assert_eq!(ProviderKind::Cohere.env_var_name(), "COHERE_API_KEY");
        assert_eq!(
            ProviderKind::Perplexity.env_var_name(),
            "PERPLEXITY_API_KEY"
        );
        assert_eq!(ProviderKind::Nvidia.env_var_name(), "NVIDIA_API_KEY");
        assert_eq!(
            ProviderKind::OpenAiCompatible.env_var_name(),
            "OPENAI_API_KEY"
        );
    }

    #[test]
    fn test_detect_from_url_zhipu_bigmodel() {
        assert!(matches!(
            detect_provider("any", "https://open.bigmodel.cn/api/paas/v4"),
            ProviderKind::Zhipu
        ));
    }

    #[test]
    fn test_detect_from_model_deepseek_chat() {
        assert!(matches!(
            detect_provider("deepseek-chat", ""),
            ProviderKind::DeepSeek
        ));
    }

    #[test]
    fn test_detect_from_model_mistral_large() {
        assert!(matches!(
            detect_provider("mistral-large", ""),
            ProviderKind::Mistral
        ));
    }

    #[test]
    fn test_detect_from_model_glm4() {
        assert!(matches!(detect_provider("glm-4", ""), ProviderKind::Zhipu));
    }

    #[test]
    fn test_detect_from_model_llama3_with_groq_url() {
        assert!(matches!(
            detect_provider("llama-3", "https://api.groq.com/openai/v1"),
            ProviderKind::Groq
        ));
    }

    #[test]
    fn test_detect_from_model_codestral() {
        assert!(matches!(
            detect_provider("codestral-latest", ""),
            ProviderKind::Mistral
        ));
    }

    #[test]
    fn test_detect_from_model_pplx() {
        assert!(matches!(
            detect_provider("pplx-70b-online", ""),
            ProviderKind::Perplexity
        ));
    }

    #[test]
    fn test_provider_error_display() {
        let err = ProviderError::Auth("bad token".into());
        assert_eq!(format!("{err}"), "auth: bad token");

        let err = ProviderError::RateLimited {
            retry_after_ms: 1000,
        };
        assert_eq!(format!("{err}"), "rate limited (retry in 1000ms)");

        let err = ProviderError::Overloaded;
        assert_eq!(format!("{err}"), "server overloaded");

        let err = ProviderError::RequestTooLarge("4MB limit".into());
        assert_eq!(format!("{err}"), "request too large: 4MB limit");

        let err = ProviderError::Network("timeout".into());
        assert_eq!(format!("{err}"), "network: timeout");

        let err = ProviderError::InvalidResponse("missing field".into());
        assert_eq!(format!("{err}"), "invalid response: missing field");
    }

    #[test]
    fn test_tool_choice_default_is_auto() {
        let tc = ToolChoice::default();
        assert!(matches!(tc, ToolChoice::Auto));
    }
}
