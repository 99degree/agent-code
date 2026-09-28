use agent_code_lib::config::{Config, FeaturesConfig};
use agent_code_lib::hooks::HookRegistry;
use agent_code_lib::llm::message::{StopReason, Usage};
use agent_code_lib::llm::provider::{Provider, ProviderRequest, ProviderError};
use agent_code_lib::llm::stream::StreamEvent;
use agent_code_lib::permissions::PermissionChecker;
use agent_code_lib::query::{QueryEngine, QueryEngineConfig};
use agent_code_lib::state::AppState;
use agent_code_lib::tools::registry::ToolRegistry;
use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use async_trait::async_trait;

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
        // Use a buffered channel with capacity 100 for simplicity.
        let (tx, rx) = mpsc::channel(100);
        // Send all events; if the channel is full, we wait (but in a test with few events, it's okay).
        for event in self.events.clone() {
            let _ = tx.send(event).await;
        }
        Ok(rx)
    }

    async fn fetch_models(&self) -> Result<Vec<(String, String)>, ProviderError> {
        Ok(vec![])
    }
}

#[tokio::test]
async fn test_debug_response_contains_serialized_events() {
    // Set up the mock provider with a known stream of events.
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

    // Set up other dependencies with default values.
    let tools = ToolRegistry::new();
    let permissions = PermissionChecker::allow_all(); // Not wrapped in Arc
    let mut config = Config::default();
    // Enable debug mode via the /debug command (feature flag)
    config.features.debug_mode = true;
    let state = AppState::new(config);
    // Enable debug mode via the CLI flag (this is separate from the feature flag)
    let mut query_engine_config = QueryEngineConfig::default();
    query_engine_config.debug_enabled = true;

    // Create the query engine.
    let mut engine = QueryEngine::new(
        provider,
        tools,
        permissions,
        state,
        query_engine_config,
    );

    // Create a sink that does nothing (we don't care about the events for this test).
    let mut sink = agent_code_lib::query::NullSink;

    // Run a turn with a dummy prompt.
    let _ = engine.run_turn_with_sink("test prompt", &mut sink).await;

    // Get the last LLM response from the engine.
    let last_response = engine.last_llm_response();

    // Debug: print the last response to see what we got.
    // println!("Last response: {}", last_response);

    // We expect the debug response to contain the JSON of the two events, each on a new line.
    // Note: The debug response is built by serializing each StreamEvent and appending a newline.
    // We expect:
    //   {"TextDelta":"hello"}\n{"Done":{"usage":{"input_tokens":0,"output_tokens":0,"cache_read_input_tokens":0,"cache_creation_input_tokens":0},"stop_reason":"EndTurn"}}\n

    // Check that the response contains the TextDelta event.
    assert!(last_response.contains(r#"{"TextDelta":"hello""#));
    // Check that the response contains the Done event with the stop_reason.
    assert!(last_response.contains(r#""stop_reason":"EndTurn""#));
    // We can also check that there are two newlines (or at least one newline between the two events).
    // But note: the last event also has a trailing newline.
    // We'll just check that the length is reasonable and that we have both events.
    assert!(last_response.len() > 50); // Arbitrary, just to make sure it's not empty.
}

#[tokio::test]
async fn test_trivial() {
    assert_eq!(1, 1);
}
#[tokio::test]
async fn test_multiple_done_events_keeps_last_tool_use() {
    // Regression test: some providers emit multiple Done events per
    // response (one per message_delta). The final Done may carry
    // end_turn even though the response contains tool calls. The
    // orchestrator must keep the last Done whose stop_reason is
    // ToolUse and ignore trailing non-tool_use Dones, so the agentic
    // chain continues (tool results go back to the model).
    let mock_events = vec![
        StreamEvent::TextDelta("calling tool".into()),
        StreamEvent::Done {
            usage: Usage::default(),
            stop_reason: Some(StopReason::ToolUse),
        },
        StreamEvent::TextDelta("more text"),
        StreamEvent::Done {
            usage: Usage::default(),
            stop_reason: Some(StopReason::EndTurn),
        },
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

    let mut engine = QueryEngine::new(
        provider,
        tools,
        permissions,
        state,
        query_engine_config,
    );

    let mut sink = agent_code_lib::query::NullSink;
    let _ = engine.run_turn_with_sink("test prompt", &mut sink).await;

    let last_response = engine.last_llm_response();

    // The debug response must contain the ToolUse Done event.
    assert!(last_response.contains(r#""stop_reason":"tool_use""#));
    // It must NOT end with the trailing EndTurn Done as the effective
    // completion — the last serialized Done should be the ToolUse one.
    let tool_use_pos = last_response
        .find(r#""stop_reason":"tool_use""#)
        .expect("tool_use stop reason missing");
    let end_turn_pos = last_response
        .rfind(r#""stop_reason":"end_turn""#)
        .expect("end_turn stop reason missing");
    assert!(
        tool_use_pos > end_turn_pos,
        "tool_use Done should appear after the last end_turn Done in the debug response"
    );
}
