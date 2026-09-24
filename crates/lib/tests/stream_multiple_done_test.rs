use agent_code_lib::llm::message::{StopReason, Usage};
use agent_code_lib::llm::stream::{MessageDeltaPayload, RawSseEvent, StreamEvent, StreamParser};

#[tokio::test]
async fn test_multiple_message_delta_events_with_tool_use() {
    // Test case: Multiple MessageDelta events with tool_use stop reason, followed by MessageStop
    // This simulates the issue where Anthropic sends multiple message_delta events
    // each with stop_reason: "tool_use", followed by a message_stop

    let mut parser = StreamParser::new();
    let mut events = Vec::new();

    // Simulate message_start
    events.extend(parser.process(RawSseEvent::MessageStart {
        message: agent_code_lib::llm::stream::MessageStartPayload {
            id: Some("test-id".to_string()),
            model: Some("test-model".to_string()),
            usage: Some(Usage::default()),
        },
    }));

    // Simulate content_block_start for tool_use
    events.extend(parser.process(RawSseEvent::ContentBlockStart {
        index: 0,
        content_block: agent_code_lib::llm::stream::RawContentBlock::ToolUse {
            id: "test-tool-id".to_string(),
            name: "test_tool".to_string(),
            input: None,
        },
    }));

    // First MessageDelta with tool_use
    events.extend(parser.process(RawSseEvent::MessageDelta {
        delta: MessageDeltaPayload {
            stop_reason: Some(StopReason::ToolUse),
        },
        usage: None,
    }));

    // Second MessageDelta with tool_use (the problematic duplicate)
    events.extend(parser.process(RawSseEvent::MessageDelta {
        delta: MessageDeltaPayload {
            stop_reason: Some(StopReason::ToolUse),
        },
        usage: None,
    }));

    // MessageStop (should trigger the final Done event)
    events.extend(parser.process(RawSseEvent::MessageStop {}));

    // Verify that we only got one Done event at the end
    let done_events: Vec<&StreamEvent> = events
        .iter()
        .filter(|e| matches!(e, StreamEvent::Done { .. }))
        .collect();

    // Should have exactly one Done event (from MessageStop)
    assert_eq!(
        done_events.len(),
        1,
        "Expected exactly one Done event, got {}",
        done_events.len()
    );

    // The Done event should have tool_use stop reason
    if let StreamEvent::Done {
        usage: _,
        stop_reason,
    } = done_events[0]
    {
        assert_eq!(
            stop_reason.as_ref(),
            Some(&StopReason::ToolUse),
            "Expected stop_reason to be ToolUse, got {:?}",
            stop_reason
        );
    } else {
        panic!("Expected Done event");
    }

    // Also verify we got the expected tool use start event
    let tool_use_start_events: Vec<&StreamEvent> = events
        .iter()
        .filter(|e| matches!(e, StreamEvent::ToolUseStart { .. }))
        .collect();

    assert_eq!(
        tool_use_start_events.len(),
        1,
        "Expected exactly one ToolUseStart event"
    );

    // We didn't send any input json deltas, so this should be empty
    let tool_input_delta_events: Vec<&StreamEvent> = events
        .iter()
        .filter(|e| matches!(e, StreamEvent::ToolInputDelta { .. }))
        .collect();

    assert_eq!(
        tool_input_delta_events.len(),
        0,
        "Expected no ToolInputDelta events"
    );
}

#[tokio::test]
async fn test_multiple_message_delta_events_with_end_then_tool_use() {
    // Test case: MessageDelta with end_turn, then MessageDelta with tool_use, then MessageStop
    // This tests that tool_use properly overrides end_turn when it comes later

    let mut parser = StreamParser::new();
    let mut events = Vec::new();

    // Simulate message_start
    events.extend(parser.process(RawSseEvent::MessageStart {
        message: agent_code_lib::llm::stream::MessageStartPayload {
            id: Some("test-id".to_string()),
            model: Some("test-model".to_string()),
            usage: Some(Usage::default()),
        },
    }));

    // First MessageDelta with end_turn
    events.extend(parser.process(RawSseEvent::MessageDelta {
        delta: MessageDeltaPayload {
            stop_reason: Some(StopReason::EndTurn),
        },
        usage: None,
    }));

    // Second MessageDelta with tool_use (should override the previous end_turn)
    events.extend(parser.process(RawSseEvent::MessageDelta {
        delta: MessageDeltaPayload {
            stop_reason: Some(StopReason::ToolUse),
        },
        usage: None,
    }));

    // MessageStop (should trigger the final Done event)
    events.extend(parser.process(RawSseEvent::MessageStop {}));

    // Verify that we only got one Done event at the end
    let done_events: Vec<&StreamEvent> = events
        .iter()
        .filter(|e| matches!(e, StreamEvent::Done { .. }))
        .collect();

    // Should have exactly one Done event (from MessageStop)
    assert_eq!(
        done_events.len(),
        1,
        "Expected exactly one Done event, got {}",
        done_events.len()
    );

    // The Done event should have tool_use stop reason (not end_turn)
    if let StreamEvent::Done {
        usage: _,
        stop_reason,
    } = done_events[0]
    {
        assert_eq!(
            stop_reason.as_ref(),
            Some(&StopReason::ToolUse),
            "Expected stop_reason to be ToolUse (overriding EndTurn), got {:?}",
            stop_reason
        );
    } else {
        panic!("Expected Done event");
    }
}
