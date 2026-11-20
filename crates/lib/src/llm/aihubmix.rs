//! AIHubMix provider (OpenAI-compatible API)
//!
//! AIHubMix is a model gateway that forwards requests to many underlying
//! models using the OpenAI chat completions interface. Any model ID can be
//! used; the base URL and API key are the only required configuration.
//!
//! Base URL: https://aihubmix.com/v1
//! Auth: Authorization: Bearer <AIHUBMIX_API_KEY>
use async_trait::async_trait;
use tokio::sync::mpsc;

use super::provider::{Provider, ProviderError, ProviderRequest};
use super::stream::StreamEvent;

/// AIHubMix provider (OpenAI-compatible API)
pub struct AihubmixProvider {
    base_url: String,
    api_key: String,
}

impl AihubmixProvider {
    /// Create a new AIHubMix provider from the given base URL and API key.
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
impl Provider for AihubmixProvider {
    fn name(&self) -> &str {
        "aihubmix"
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
        let provider = AihubmixProvider::new("https://aihubmix.com/v1", "test-key");
        assert_eq!(provider.base_url(), "https://aihubmix.com/v1");
        assert_eq!(provider.api_key(), "test-key");
    }
}
