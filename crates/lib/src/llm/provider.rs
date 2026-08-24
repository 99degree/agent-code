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

    /// Fetch the list of available models from the provider.
    /// Returns a vector of (model_id, description) pairs.
    /// The default implementation returns an empty list.
    async fn fetch_models(&self) -> Result<Vec<(String, String)>, ProviderError> {
        Ok(vec![])
    }
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
    /// Bounds the wait for each stream chunk (first byte included), so a
    /// silent model or a stalled connection fails with a "no output" error
    /// instead of hanging until the HTTP client's total deadline. `None`
    /// disables the per-chunk wait.
    pub stream_timeout: Option<std::time::Duration>,
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
        ProviderKind::OpenRouter => &[
            ("anthropic/claude-sonnet-5", "Claude Sonnet 5 · Balanced"),
            (
                "anthropic/claude-opus-4.8",
                "Claude Opus 4.8 · Most capable",
            ),
            ("openai/gpt-5.5", "GPT-5.5 · Most capable"),
            ("google/gemini-3-pro", "Gemini 3 Pro"),
            ("x-ai/grok-4.3", "Grok 4.3"),
            ("deepseek/deepseek-v4-pro", "DeepSeek V4 Pro · Open"),
        ],
        ProviderKind::Nvidia => &[
            (
                "nvidia/nemotron-3-ultra-550b-a55b",
                "Nemotron 3 Ultra · Most capable",
            ),
            ("nvidia/nemotron-3-nano-30b-a3b", "Nemotron 3 Nano · Fast"),
            (
                "nvidia/nemotron-3-nano-omni-30b-a3b-reasoning",
                "Nemotron 3 Nano Omni · Reasoning",
            ),
            ("minimaxai/minimax-m3", "MiniMax M3"),
            ("deepseek-ai/deepseek-v4-pro", "DeepSeek V4 Pro"),
            ("meta/llama-3.1-8b-instruct", "Llama 3.1 8B · Fast"),
        ],
        ProviderKind::Kilo => &[
            ("kilo-auto/frontier", "Kilo Auto Frontier · Most capable"),
            ("kilo-auto/balanced", "Kilo Auto Balanced · Balanced"),
            (
                "kilo-auto/efficient",
                "Kilo Auto Efficient · Cheapest that works",
            ),
            ("kilo-auto/free", "Kilo Auto Free · Rotates free models"),
            ("tencent/hy3:free", "Tencent Hy3 · Free (failover)"),
            ("stepfun/step-3.7-flash:free", "StepFun Step 3.7 Flash · Free"),
            ("poolside/laguna-s-2.1:free", "Poolside Laguna S 2.1 · Free"),
            ("meituan/longcat-2.0-free", "Meituan LongCat 2.0 · Free"),
        ],
        ProviderKind::Novita => &[("tencent/hy3", "Tencent Hy3 · Open")],
        ProviderKind::OpenCode => &[
            ("big-pickle", "Big Pickle · Frontier flagship"),
            ("deepseek-v4-flash", "DeepSeek V4 Flash"),
            ("deepseek-v4-flash-free", "DeepSeek V4 Flash · Free"),
            ("hy3", "Tencent Hy3 · Open"),
            ("hy3-free", "Tencent Hy3 · Free"),
            ("mimo-v2.5", "MiMo 2.5"),
            ("mimo-v2.5-free", "MiMo 2.5 · Free"),
            ("nemotron-3-ultra", "Nemotron 3 Ultra"),
            ("nemotron-3-ultra-free", "Nemotron 3 Ultra · Free"),
            (
                "nemotron-3.5-lightning",
                "Nemotron 3.5 Lightning",
            ),
            ("nemotron-3.5-lightning-free", "Nemotron 3.5 Lightning · Free"),
            ("x-preview-f-free", "x-preview-f · Free preview"),
            ("muse-spark-1.2", "Muse Spark 1.2"),
            ("muse-spark-1.2-contributor-free", "Muse Spark 1.2 · Free"),
            ("laguna-s-2.1", "Laguna S 2.1"),
            ("laguna-s-2.1-free", "Laguna S 2.1 · Free"),
        ],
        ProviderKind::OpenCodeGo => &[
            ("hy3", "Tencent Hy3 · Open"),
            ("hy3-preview", "Tencent Hy3 Preview"),
            ("deepseek-v4-pro", "DeepSeek V4 Pro"),
            ("deepseek-v4-flash", "DeepSeek V4 Flash"),
            ("glm-5.3", "GLM 5.3"),
            ("glm-5.2", "GLM 5.2"),
            ("glm-5.1", "GLM 5.1"),
            ("glm-5", "GLM 5"),
            ("kimi-k3", "Kimi K3"),
            ("kimi-k2.7-code", "Kimi K2.7 Code"),
            ("kimi-k2.6", "Kimi K2.6"),
            ("kimi-k2.5", "Kimi K2.5"),
            ("qwen3.8-max", "Qwen3.8 Max"),
            ("qwen3.7-max", "Qwen3.7 Max"),
            ("qwen3.7-plus", "Qwen3.7 Plus"),
            ("qwen3.6-plus", "Qwen3.6 Plus"),
            ("qwen3.5-plus", "Qwen3.5 Plus"),
            ("mimo-v2-pro", "MiMo 2 Pro"),
            ("mimo-v2-omni", "MiMo 2 Omni"),
            ("mimo-v2.5-pro", "MiMo 2.5 Pro"),
            ("mimo-v2.5", "MiMo 2.5"),
            ("minimax-m3", "MiniMax M3"),
            ("minimax-m2.7", "MiniMax M2.7"),
            ("minimax-m2.5", "MiniMax M2.5"),
            ("grok-4.5", "Grok 4.5"),
            ("gpt-5.6-luna", "GPT-5.6 Luna"),
            ("x-preview-f-free", "x-preview-f · Free preview"),
            ("muse-spark-1.2-contributor", "Muse Spark 1.2 Contributor"),
        ],
        _ => &[],
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
    if url_lower.contains("cohere.com") || url_lower.contains("cohere.ai") {
        return ProviderKind::Cohere;
    }
    if url_lower.contains("perplexity.ai") {
        return ProviderKind::Perplexity;
    }
    if url_lower.contains("opencode.ai/zen/go") {
        return ProviderKind::OpenCodeGo;
    }
    if url_lower.contains("opencode.ai") {
        return ProviderKind::OpenCode;
    }
    if url_lower.contains("nvidia") || url_lower.contains("nvidianim") {
        return ProviderKind::Nvidia;
    }
    if url_lower.contains("novita.ai") {
        return ProviderKind::Novita;
    }
    if url_lower.contains("kilo.ai") {
        return ProviderKind::Kilo;
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
    if model_lower.contains("nemotron") || model_lower.starts_with("nvidia/") {
        return ProviderKind::Nvidia;
    }

    ProviderKind::OpenAiCompatible
}

/// Get a provider kind for a model and base URL, with fallback behavior.
///
/// 1. First, detect a provider candidate from the model and base URL.
///    If the model is found in that provider's model list, return it.
/// 2. Otherwise, search all providers' model lists (by name only) for the
///    model and return the first match.
/// 3. If still not found, fall back to the detected provider from step 1.
pub fn get_provider_for_model(model: &str, base_url: &str) -> ProviderKind {
    // Step 1: Get candidate provider from model and base_url.
    let candidate_kind = detect_provider(model, base_url);

    // Step 2: Check if the model is in the candidate provider's model list.
    let models = models_for_provider(candidate_kind);
    if models.iter().any(|(m, _)| m.eq_ignore_ascii_case(model)) {
        return candidate_kind;
    }

    // Step 3: Search all providers by model name.
    for &kind in ProviderKind::all() {
        let models = models_for_provider(kind);
        if models.iter().any(|(m, _)| m.eq_ignore_ascii_case(model)) {
            return kind;
        }
    }

    // Step 4: Fallback to candidate_kind.
    candidate_kind
}

/// Resolve the API key for a provider: the provider's environment variable
/// first, then the global config key.
pub fn resolve_api_key(kind: ProviderKind, config: &crate::config::Config) -> Option<String> {
    kind.api_key_from_env()
        .or_else(|| config.api.api_key.clone())
}

/// Create a provider from config (model, base_url). The API key is
/// resolved per-provider via [`resolve_api_key`], so each provider uses
/// its own credential (env var, then per-provider config key) instead of
/// a different provider's key. Covers the API-key auth modes; OAuth-based
/// modes (Codex ChatGPT, xAI OAuth) are not constructed here.
pub fn create_provider_from_config(
    model: &str,
    base_url: &str,
    config: &crate::config::Config,
) -> std::sync::Arc<dyn Provider> {
    let kind = detect_provider(model, base_url);
    let resolved_key = resolve_api_key(kind, config).unwrap_or_default();
    match kind {
        ProviderKind::AzureOpenAi => std::sync::Arc::new(
            crate::llm::azure_openai::AzureOpenAiProvider::new(base_url, &resolved_key),
        ),
        ProviderKind::Novita => std::sync::Arc::new(crate::llm::novita::NovitaProvider::new(
            base_url,
            &resolved_key,
        )),
        ProviderKind::Kilo => {
            std::sync::Arc::new(crate::llm::kilo::KiloProvider::new(base_url, &resolved_key))
        }
        ProviderKind::OpenCode => std::sync::Arc::new(crate::llm::opencode::OpenCodeProvider::new(
            base_url,
            &resolved_key,
        )),
        _ => match kind.wire_format() {
            WireFormat::Anthropic => std::sync::Arc::new(
                crate::llm::anthropic::AnthropicProvider::new(base_url, &resolved_key),
            ),
            WireFormat::OpenAiCompatible => {
                // Nemotron models emit tool calls as custom text markup rather
                // than structured `tool_calls` deltas; route them through the
                // Nemotron-aware provider regardless of which OpenAI-compatible
                // endpoint serves them.
                if crate::llm::nemotron::is_nemotron_model(model) {
                    std::sync::Arc::new(crate::llm::openai::OpenAiProvider::new_nemotron(
                        base_url,
                        &resolved_key,
                    ))
                } else {
                    std::sync::Arc::new(crate::llm::openai::OpenAiProvider::new(
                        base_url,
                        &resolved_key,
                    ))
                }
            }
        },
    }
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
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
    Cohere,
    Perplexity,
    Nvidia,
    /// Kilo AI (OpenAI-compatible gateway https://api.kilo.ai/api/gateway).
    Kilo,
    Novita,
    OpenCode,
    OpenCodeGo,
    OpenAiCompatible,
}

impl ProviderKind {
    /// All known provider kinds, in display order.
    pub fn all() -> &'static [ProviderKind] {
        &[
            ProviderKind::Anthropic,
            ProviderKind::Bedrock,
            ProviderKind::Vertex,
            ProviderKind::OpenAi,
            ProviderKind::AzureOpenAi,
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
            ProviderKind::Kilo,
            ProviderKind::Novita,
            ProviderKind::OpenCode,
            ProviderKind::OpenCodeGo,
            ProviderKind::OpenAiCompatible,
        ]
    }

    /// Check if this provider has an API key configured (via its env var).
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
            | Self::Cohere
            | Self::Perplexity
            | Self::Nvidia
            | Self::Kilo
            | Self::Novita
            | Self::OpenCode
            | Self::OpenCodeGo
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
            Self::Kilo => Some("https://api.kilo.ai/api/gateway"),
            Self::Novita => Some("https://api.novita.ai/openai/v1"),
            // These require user-supplied URLs.
            Self::Bedrock | Self::Vertex | Self::AzureOpenAi | Self::OpenAiCompatible => None,
        }
    }

    /// The environment variable name conventionally used for this provider's API key.
    pub fn env_var_name(&self) -> &'static str {
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
            Self::Kilo => "KILO_API_KEY",
            Self::Novita => "NOVITA_API_KEY",
            Self::OpenAiCompatible => "OPENAI_API_KEY",
        }
    }

    /// Canonical CLI / slash-command name for this provider (e.g. `kilo`,
    /// `opencode`, `anthropic`). Used by the `/provider` command and by
    /// error messaging.
    pub fn as_name(&self) -> &'static str {
        match self {
            Self::Anthropic => "anthropic",
            Self::Bedrock => "bedrock",
            Self::Vertex => "vertex",
            Self::OpenAi => "openai",
            Self::AzureOpenAi => "azure",
            Self::Xai => "xai",
            Self::Google => "google",
            Self::DeepSeek => "deepseek",
            Self::Groq => "groq",
            Self::Mistral => "mistral",
            Self::Together => "together",
            Self::Zhipu => "zhipu",
            Self::OpenRouter => "openrouter",
            Self::Cohere => "cohere",
            Self::Perplexity => "perplexity",
            Self::Nvidia => "nvidia",
            Self::Kilo => "kilo",
            Self::Novita => "novita",
            Self::OpenCode => "opencode",
            Self::OpenCodeGo => "opencode-go",
            Self::OpenAiCompatible => "openai-compatible",
        }
    }

    /// Resolve a provider name (as typed to `--provider` or `/provider`)
    /// to a `ProviderKind`, honoring the documented aliases. Returns
    /// `None` for unrecognized names (the caller may then fall back to
    /// `detect_provider`).
    pub fn from_name(name: &str) -> Option<ProviderKind> {
        Some(match name.to_ascii_lowercase().as_str() {
            "anthropic" | "claude" => ProviderKind::Anthropic,
            "openai" | "gpt" => ProviderKind::OpenAi,
            "bedrock" | "aws" => ProviderKind::Bedrock,
            "vertex" | "gcp" => ProviderKind::Vertex,
            "xai" | "grok" => ProviderKind::Xai,
            "google" | "gemini" => ProviderKind::Google,
            "deepseek" => ProviderKind::DeepSeek,
            "groq" => ProviderKind::Groq,
            "mistral" => ProviderKind::Mistral,
            "together" => ProviderKind::Together,
            "zhipu" | "glm" | "z.ai" => ProviderKind::Zhipu,
            "azure" | "azure-openai" => ProviderKind::AzureOpenAi,
            "nvidia" | "nim" => ProviderKind::Nvidia,
            "kilo" => ProviderKind::Kilo,
            "novita" => ProviderKind::Novita,
            "opencode" | "zen" => ProviderKind::OpenCode,
            "opencode-go" | "zen-go" => ProviderKind::OpenCodeGo,
            "openrouter" => ProviderKind::OpenRouter,
            "cohere" => ProviderKind::Cohere,
            "perplexity" => ProviderKind::Perplexity,
            _ => return None,
        })
    }

    /// A sensible default model for this provider (its first curated
    /// catalog entry), or `None` when the provider has no catalog. Used
    /// by the `/provider` command to seed a model when switching.
    pub fn default_model(&self) -> Option<&'static str> {
        models_for_provider(*self).first().map(|(m, _)| *m)
    }

    /// Resolve this provider's API key from its environment variables,
    /// honoring the documented fallbacks (e.g. OpenCode Zen falls back to
    /// `OPENCODE_API_KEY`). Returns `None` when nothing is set.
    pub fn api_key_from_env(&self) -> Option<String> {
        let primary = std::env::var(self.env_var_name())
            .ok()
            .filter(|k| !k.is_empty());
        if primary.is_some() {
            return primary;
        }
        match self {
            // OPENCODE_ZEN_API_KEY → OPENCODE_API_KEY
            Self::OpenCode => std::env::var("OPENCODE_API_KEY")
                .ok()
                .filter(|k| !k.is_empty()),
            // OPENCODE_GO_API_KEY → OPENCODE2_API_KEY
            Self::OpenCodeGo => std::env::var("OPENCODE2_API_KEY")
                .ok()
                .filter(|k| !k.is_empty()),
            _ => None,
        }
    }

    /// Resolve a concrete model id from a provider's live `/models`
    /// catalog by substring match against `search_fragment`. Used by the
    /// cross-provider failover path, where the failover table stores only
    /// the *fragment* of the mirror model (e.g. OpenCode `laguna-s` →
    /// Kilo `laguna-s-2.1:free`, or OpenCode `x-preview-f-free` → Kilo
    /// `stealth/ox-alpha`) and we must not hard-code free-tier names that
    /// change over time.
    ///
    /// Returns `None` when the provider exposes no `/models` endpoint, has
    /// no API key, or no listed model matches the fragment. The caller
    /// decides how to proceed (fall back to the fragment, or abort).
    pub async fn fetch_matching_model(
        provider: ProviderKind,
        search_fragment: &str,
    ) -> Option<String> {
        let base = provider
            .default_base_url()?
            .trim_end_matches('/')
            .to_string();
        let key = provider.api_key_from_env()?;
        let url = format!("{base}/models");
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(15))
            .build()
            .ok()?;
        let resp = client.get(&url).bearer_auth(&key).send().await.ok()?;
        if !resp.status().is_success() {
            return None;
        }
        let body: serde_json::Value = resp.json().await.ok()?;
        // OpenAI-compatible endpoints return { "data": [ { "id": ... }, ... ] }.
        let items = body.get("data").and_then(|v| v.as_array())?;
        let needle = search_fragment.to_ascii_lowercase();
        for item in items {
            if let Some(id) = item.get("id").and_then(|v| v.as_str())
                && id.to_ascii_lowercase().contains(&needle)
            {
                return Some(id.to_string());
            }
        }
        None
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

    #[test]
    fn test_from_name_resolves_aliases() {
        // Canonical names and documented aliases all resolve.
        assert_eq!(
            ProviderKind::from_name("anthropic"),
            Some(ProviderKind::Anthropic)
        );
        assert_eq!(
            ProviderKind::from_name("claude"),
            Some(ProviderKind::Anthropic)
        );
        assert_eq!(
            ProviderKind::from_name("openai"),
            Some(ProviderKind::OpenAi)
        );
        assert_eq!(ProviderKind::from_name("gpt"), Some(ProviderKind::OpenAi));
        assert_eq!(ProviderKind::from_name("xai"), Some(ProviderKind::Xai));
        assert_eq!(ProviderKind::from_name("grok"), Some(ProviderKind::Xai));
        assert_eq!(ProviderKind::from_name("kilo"), Some(ProviderKind::Kilo));
        assert_eq!(
            ProviderKind::from_name("opencode"),
            Some(ProviderKind::OpenCode)
        );
        assert_eq!(ProviderKind::from_name("zen"), Some(ProviderKind::OpenCode));
        assert_eq!(
            ProviderKind::from_name("opencode-go"),
            Some(ProviderKind::OpenCodeGo)
        );
        assert_eq!(
            ProviderKind::from_name("zen-go"),
            Some(ProviderKind::OpenCodeGo)
        );
        assert_eq!(ProviderKind::from_name("zhipu"), Some(ProviderKind::Zhipu));
        assert_eq!(ProviderKind::from_name("glm"), Some(ProviderKind::Zhipu));
        // Unknown names do not resolve.
        assert_eq!(ProviderKind::from_name("not-a-provider"), None);
        // Resolution is case-insensitive.
        assert_eq!(ProviderKind::from_name("Kilo"), Some(ProviderKind::Kilo));
    }

    #[test]
    fn test_as_name_matches_from_name_round_trip() {
        for &kind in ProviderKind::all() {
            let name = kind.as_name();
            // Every kind must round-trip through its canonical name, except
            // the generic OpenAI-compatible fallback (not a switchable
            // provider and therefore absent from the name table by design).
            if kind == ProviderKind::OpenAiCompatible {
                assert_eq!(name, "openai-compatible");
                assert_eq!(ProviderKind::from_name(name), None);
            } else {
                assert_eq!(ProviderKind::from_name(name), Some(kind), "{kind:?}");
            }
        }
    }

    #[test]
    fn test_default_model_picks_first_catalog_entry() {
        assert_eq!(
            ProviderKind::Kilo.default_model(),
            Some("kilo-auto/frontier")
        );
        assert_eq!(ProviderKind::OpenCode.default_model(), Some("big-pickle"));
        // Providers without a curated catalog have no default model.
        assert_eq!(ProviderKind::OpenAiCompatible.default_model(), None);
    }

    // fetch_matching_model is a network + API-key call; the only hermetic
    // guarantee we can assert is that it short-circuits to None when no
    // key is configured for the target provider (no request is sent).
    // Skip the assertion when a key is present, since then it would make
    // a real network call.
    #[tokio::test]
    async fn fetch_matching_model_is_none_without_key() {
        if std::env::var("KILO_API_KEY").is_ok() {
            return;
        }
        assert_eq!(
            ProviderKind::fetch_matching_model(ProviderKind::Kilo, "ox-alpha").await,
            None
        );
    }
}
