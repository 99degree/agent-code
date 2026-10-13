//! Poolside provider (OpenAI-compatible API)
//!
//! Poolside provides an OpenAI-compatible chat completions API.
//! This provider is a thin wrapper around the OpenAI provider because the API
//! is compatible. We reuse the OpenAI provider's logic but allow a different
//! base URL and API key environment variable.

use async_trait::async_trait;
use tokio::sync::mpsc;

use super::provider::{Provider, ProviderError, ProviderRequest};
use super::stream::StreamEvent;

/// Poolside provider (OpenAI-compatible API)
pub struct PoolsideProvider {
    base_url: String,
    api_key: String,
}

impl PoolsideProvider {
    /// Create a new Poolside provider from the given base URL and API key.
    pub fn new(base_url: &str, api_key: &str) -> Self {
        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            api_key: api_key.to_string(),
        }
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    pub fn api_key(&self) -> &str {
        &self.api_key
    }
}

#[async_trait]
impl Provider for PoolsideProvider {
    fn name(&self) -> &str {
        "poolside"
    }

    async fn stream(
        &self,
        request: &ProviderRequest,
    ) -> Result<mpsc::Receiver<StreamEvent>, ProviderError> {
        let openai_provider = super::openai::OpenAiProvider::new(self.base_url(), self.api_key());
        openai_provider.stream(request).await
    }

    async fn fetch_models(&self) -> Result<Vec<(String, String, Option<u64>)>, ProviderError> {
        let openai_provider = super::openai::OpenAiProvider::new(self.base_url(), self.api_key());
        openai_provider.fetch_models().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_with_base_url_and_api_key() {
        let provider = PoolsideProvider::new("https://api.poolside.ai/v1", "test-key");
        assert_eq!(provider.base_url(), "https://api.poolside.ai/v1");
        assert_eq!(provider.api_key(), "test-key");
    }
}
