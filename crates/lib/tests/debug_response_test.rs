use agent_code_lib::config::Config;
use agent_code_lib::llm::message::{StopReason, Usage};
use agent_code_lib::llm::provider::{Provider, ProviderError, ProviderRequest};
use agent_code_lib::llm::stream::StreamEvent;
use agent_code_lib::permissions::PermissionChecker;
use agent_code_lib::query::{QueryEngine, QueryEngineConfig};
use agent_code_lib::state::AppState;
use agent_code_lib::tools::registry::ToolRegistry;
use std::sync::Arc;
use tokio::sync::mpsc;

struct MockProvider {
    events: Vec<StreamEvent>,
}

#[async_trait::async_trait]
impl Provider for MockProvider {
    fn name(&self) -> &str {
        "mock"
    }

    async fn stream(
        &self,
        _request: &ProviderRequest,
    ) -> Result<mpsc::Receiver<StreamEvent>, ProviderError> {
        let (tx, rx) = mpsc::channel(100);
        for event in self.events.clone() {
            let _ = tx.send(event).await;
        }
        Ok(rx)
    }

    async fn fetch_models(&self) -> Result<Vec<(String, String, Option<u64>)>, ProviderError> {
        Ok(vec![])
    }
}

#[tokio::test]
async fn test_debug_response_contains_serialized_events() {
    let mock_events = vec![
        StreamEvent::TextDelta("hello".into()),
        StreamEvent::Done {
            usage: Usage::default(),
            stop_reason: Some(StopReason::EndTurn),
        },
    ];
    let provider = Arc::new(MockProvider {
        events: mock_events,
    });

    let tools = ToolRegistry::new();
    let permissions = PermissionChecker::allow_all();
    let mut config = Config::default();
    config.features.debug_mode = true;
    let state = AppState::new(config);
    let mut query_engine_config = QueryEngineConfig::default();
    query_engine_config.debug_enabled = true;

    let mut engine = QueryEngine::new(provider, tools, permissions, state, query_engine_config);

    let mut sink = agent_code_lib::query::NullSink;

    let last_response = engine.last_llm_response();
    assert_eq!(last_response, "");

    let _ = engine.run_turn_with_sink("test prompt", &mut sink).await;

    let last_response = engine.last_llm_response();
    println!("Last response: {}", last_response);

    assert!(last_response.contains(r#"{"TextDelta":"hello""#));
    assert!(last_response.contains(r#""stop_reason":"end_turn""#));
    assert!(last_response.len() > 50);
}

#[tokio::test]
async fn test_trivial() {
    assert_eq!(1, 1);
}
