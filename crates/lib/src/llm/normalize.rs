//! Message normalization and validation utilities.
//!
//! Ensures messages conform to API requirements before sending:
//! - Tool use / tool result pairing
//! - Content block ordering
//! - Empty message handling

use super::message::*;
use uuid::Uuid;

/// Ensure every tool_use block has a matching tool_result in the
/// subsequent user message. Orphaned tool_use blocks cause API errors.
/// Ensure every tool_use block has a matching tool_result in the
/// subsequent user message. Orphaned tool_use blocks cause API errors.
/// Deduplicates tool_results for the same tool_use_id by preferring
/// LATER occurrences. Also removes tool_results that appear before
/// any tool_use with that ID (out-of-order corruption).
pub fn ensure_tool_result_pairing(messages: &mut Vec<Message>) {

    // First pass: collect all tool_use IDs in order and count occurrences per ID.
    let mut tool_use_counts: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    let mut tool_use_order: Vec<String> = Vec::new();
    for msg in messages.iter() {
        if let Message::Assistant(a) = msg {
            for block in &a.content {
                if let ContentBlock::ToolUse { id, .. } = block {
                    if !tool_use_counts.contains_key(id) {
                        tool_use_order.push(id.clone());
                    }
                    *tool_use_counts.entry(id.clone()).or_insert(0) += 1;
                }
            }
        }
    }


    // Second pass (forward): pair tool_results with PRECEDING tool_uses in order.
    // Track how many tool_uses we've seen so far per ID.
    let mut seen_counts: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    let mut to_remove_forward: Vec<(usize, usize)> = Vec::new();
    for (msg_idx, msg) in messages.iter().enumerate() {
        if let Message::Assistant(a) = msg {
            for block in &a.content {
                if let ContentBlock::ToolUse { id, .. } = block {
                    *seen_counts.entry(id.clone()).or_insert(0) += 1;
                }
            }
        } else if let Message::User(u) = msg {
            for (content_idx, block) in u.content.iter().enumerate() {
                if let ContentBlock::ToolResult { tool_use_id, .. } = block {
                    let seen = seen_counts.get(tool_use_id).copied().unwrap_or(0);
                    if seen == 0 {
                        // Tool_result appears before any tool_use with this ID - mark for removal.
                        to_remove_forward.push((msg_idx, content_idx));
                    }
                }
            }
        }
    }
    // Remove marked (from highest index to lowest).
    to_remove_forward.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.cmp(&a.1)));
    for (msg_idx, content_idx) in to_remove_forward {
        if let Message::User(u) = &mut messages[msg_idx] {
            u.content.remove(content_idx);
        }
    }

    // Third pass (backward): from the end, keep up to N tool_results per ID
    // where N is the total tool_use count. This handles duplicate IDs.
    let mut kept_counts: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    let mut to_remove_backward: Vec<(usize, usize)> = Vec::new();
    for (msg_idx, msg) in messages.iter().enumerate().rev() {
        if let Message::User(u) = msg {
            for (content_idx, block) in u.content.iter().enumerate().rev() {
                if let ContentBlock::ToolResult { tool_use_id, .. } = block {
                    let max_allowed = tool_use_counts.get(tool_use_id).copied().unwrap_or(0);
                    let already_kept = kept_counts.get(tool_use_id).copied().unwrap_or(0);
                    if already_kept < max_allowed {
                        *kept_counts.entry(tool_use_id.clone()).or_insert(0) += 1;
                    } else {
                        to_remove_backward.push((msg_idx, content_idx));
                    }
                }
            }
        }
    }
    // Remove marked (from highest index to lowest).
    to_remove_backward.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.cmp(&a.1)));
    for (msg_idx, content_idx) in to_remove_backward {
        if let Message::User(u) = &mut messages[msg_idx] {
            u.content.remove(content_idx);
        }
    }

    // Any remaining unpaired tool_use_ids need synthetic error results.
    // Iterate in tool_use_order to preserve original insertion order.
    let unpaired: Vec<String> = tool_use_order
        .into_iter()
        .flat_map(|id| {
            let count = tool_use_counts.get(&id).copied().unwrap_or(0);
            let kept = kept_counts.get(&id).copied().unwrap_or(0);
            (0..count.saturating_sub(kept)).map(move |_| id.clone())
        })
        .collect();
    if !unpaired.is_empty() {
        let blocks: Vec<ContentBlock> = unpaired
            .iter()
            .map(|id| ContentBlock::ToolResult {
                tool_use_id: id.clone(),
                content: "(tool execution was interrupted)".to_string(),
                is_error: true,
                extra_content: vec![],
            })
            .collect();
        messages.push(Message::User(UserMessage {
            uuid: Uuid::new_v4(),
            timestamp: chrono::Utc::now().to_rfc3339(),
            content: blocks,
            is_meta: true,
            is_compact_summary: false,
        }));
    }
}

/// Remove empty text blocks from messages.
pub fn strip_empty_blocks(messages: &mut [Message]) {
    for msg in messages.iter_mut() {
        match msg {
            Message::User(u) => {
                u.content.retain(|b| match b {
                    ContentBlock::Text { text } => !text.is_empty(),
                    _ => true,
                });
            }
            Message::Assistant(a) => {
                a.content.retain(|b| match b {
                    ContentBlock::Text { text } => !text.is_empty(),
                    _ => true,
                });
            }
            _ => {}
        }
    }

    #[test]
    fn test_clean_tool_use_input_null_input() {
        // Regression test: tool_use with null input should be cleaned to empty object.
        let mut messages = vec![
            Message::Assistant(AssistantMessage {
                uuid: Uuid::new_v4(),
                timestamp: String::new(),
                content: vec![ContentBlock::ToolUse {
                    id: "call_1".into(),
                    name: "Write".into(),
                    input: serde_json::Value::Null,
                }],
                model: None,
                usage: None,
                stop_reason: None,
                request_id: None,
            }),
        ];

        clean_tool_use_input(&mut messages);

        // The tool_use input should now be an empty object, not null
        if let Message::Assistant(a) = &messages[0] {
            for block in &a.content {
                if let ContentBlock::ToolUse { input, .. } = block {
                    assert!(!input.is_null(), "tool_use input should not be null");
                    assert!(input.is_object(), "tool_use input should be an object");
                    assert_eq!(*input, serde_json::json!({}));
                    // Note: deref input before comparing
                }
            }
        }
    }

    #[test]
    fn test_clean_tool_use_input_non_object_input() {
        // Regression test: tool_use with non-object input (string, number, bool) should be cleaned.
        let mut messages = vec![
            Message::Assistant(AssistantMessage {
                uuid: Uuid::new_v4(),
                timestamp: String::new(),
                content: vec![ContentBlock::ToolUse {
                    id: "call_2".into(),
                    name: "Bash".into(),
                    input: serde_json::Value::String("not an object".into()),
                }],
                model: None,
                usage: None,
                stop_reason: None,
                request_id: None,
            }),
        ];

        clean_tool_use_input(&mut messages);

        // The tool_use input should now be an empty object
        if let Message::Assistant(a) = &messages[0] {
            for block in &a.content {
                if let ContentBlock::ToolUse { input, .. } = block {
                    assert!(input.is_object(), "tool_use input should be an object");
                    assert_eq!(*input, serde_json::json!({}));
                    // Note: deref input before comparing
                }
            }
        }
    }

    #[test]
    fn test_clean_tool_use_input_empty_object_preserved() {
        // Empty object input should be preserved (not double-cleaned)
        let mut messages = vec![
            Message::Assistant(AssistantMessage {
                uuid: Uuid::new_v4(),
                timestamp: String::new(),
                content: vec![ContentBlock::ToolUse {
                    id: "call_3".into(),
                    name: "Read".into(),
                    input: serde_json::json!({}),
                }],
                model: None,
                usage: None,
                stop_reason: None,
                request_id: None,
            }),
        ];

        clean_tool_use_input(&mut messages);

        // The tool_use input should remain an empty object
        if let Message::Assistant(a) = &messages[0] {
            for block in &a.content {
                if let ContentBlock::ToolUse { input, .. } = block {
                    assert!(input.is_object(), "tool_use input should be an object");
                    assert_eq!(*input, serde_json::json!({}));
                    // Note: deref input before comparing
                }
            }
        }
    }
}

/// Replace null/missing tool_use input with empty object.
/// Some providers reject tool calls with null input or non-object input.
pub fn clean_tool_use_input(messages: &mut [Message]) {
    for msg in messages.iter_mut() {
        if let Message::Assistant(a) = msg {
            for block in a.content.iter_mut() {
                if let ContentBlock::ToolUse { input, .. } = block {
                    if input.is_null() {
                        *input = serde_json::json!({});
                    } else if !input.is_object() {
                        // Non-object input (string, number, bool, array) is invalid for function calling
                        *input = serde_json::json!({});
                    } else if let Some(obj) = input.as_object_mut() {
                        // Ensure input is an object, not a primitive
                        if obj.is_empty() {
                            *input = serde_json::json!({});
                        }
                    }
                }
            }
        }
    }
}

/// Validate that the message sequence alternates correctly
/// (user/assistant/user/assistant...) as required by the API.
pub fn validate_alternation(messages: &[Message]) -> Result<(), String> {
    let mut expect_user = true;

    for (i, msg) in messages.iter().enumerate() {
        match msg {
            Message::System(_) => continue, // System messages don't count.
            Message::User(_) => {
                if !expect_user {
                    return Err(format!("Message {i}: expected assistant, got user"));
                }
                expect_user = false;
            }
            Message::Assistant(_) => {
                if expect_user {
                    return Err(format!("Message {i}: expected user, got assistant"));
                }
                expect_user = true;
            }
        }
    }

    Ok(())
}

/// Convenience wrapper: run the full normalization suite on a message
/// vector. Call this after loading messages from disk (session resume,
/// history import) to guarantee API-compatible alternation.
pub fn normalize_messages(messages: &mut Vec<Message>) {
    normalize_strict(messages);
}

/// Drop every message before the last compaction summary, returning the
/// dropped prefix.
///
/// A compaction summary (`is_compact_summary`) distills everything that
/// preceded it, so re-loading those earlier messages on resume is pure
/// context bloat that inflates the first turn's input tokens (and trips
/// the high-token-usage warning). Keeping only the summary and everything
/// after it shrinks the *active* history to the meaningful tail, which is
/// what the LLM and token accounting see.
///
/// The dropped prefix is returned so the caller can preserve it for
/// on-disk persistence of the *full* history (the active tail alone would
/// lose the distilled precedent). Returns an empty vector when the history
/// holds no compaction summary or the summary is already first.
pub fn truncate_to_last_summary(messages: &mut Vec<Message>) -> Vec<Message> {
    let last_summary = messages
        .iter()
        .rposition(|m| matches!(m, Message::User(u) if u.is_compact_summary));
    match last_summary {
        Some(idx) if idx > 0 => {
            // split_off(idx) leaves `messages` holding [0, idx) (the dropped
            // head) and returns [idx, end) (the active tail, summary first).
            let active = messages.split_off(idx);
            std::mem::replace(messages, active)
        }
        _ => Vec::new(),
    }
}

/// Remove empty messages (messages with no content blocks after stripping).
pub fn remove_empty_messages(messages: &mut Vec<Message>) {
    messages.retain(|msg| match msg {
        Message::User(u) => !u.content.is_empty(),
        Message::Assistant(a) => !a.content.is_empty(),
        Message::System(_) => true,
    });
}

/// Cap oversized document blocks to prevent context blowout.
pub fn cap_document_blocks(messages: &mut [Message], max_bytes: usize) {
    for msg in messages.iter_mut() {
        let content = match msg {
            Message::User(u) => &mut u.content,
            Message::Assistant(a) => &mut a.content,
            _ => continue,
        };
        for block in content.iter_mut() {
            if let ContentBlock::Document { data, title, .. } = block
                && data.len() > max_bytes
            {
                let name = title.as_deref().unwrap_or("document");
                *block = ContentBlock::Text {
                    text: format!(
                        "(Document '{name}' too large for context: {} bytes, max {max_bytes})",
                        data.len()
                    ),
                };
            }
        }
    }
}

/// Remove System messages that appear after the first user/assistant
/// message.  Mid-conversation system messages (e.g. "Stream retry
/// limit reached") break user/assistant alternation once they are
/// filtered out by provider-specific serialization, creating
/// consecutive user messages that cause 400 errors.
///
/// System messages *before* the first user/assistant are preserved
/// because some providers use them for system prompts.
pub fn remove_mid_conversation_system_messages(messages: &mut Vec<Message>) {
    let first_content = messages
        .iter()
        .position(|m| !matches!(m, Message::System(_)));
    if let Some(start) = first_content {
        let prefix: Vec<Message> = messages.drain(..start).collect();
        let before = messages.len();
        messages.retain(|m| !matches!(m, Message::System(_)));
        let mid_systems_removed = before - messages.len();
        // Re-insert the prefix (system messages before first user/assistant).
        if mid_systems_removed == 0 {
            // No mid-conversation systems were removed — just prepend the
            // leading systems back without rotation.  The old rotate-right
            // path is only correct when retain actually shrunk the vec.
            let mut restored = prefix;
            restored.append(messages);
            *messages = restored;
        } else {
            let old_len = messages.len();
            messages.extend(prefix);
            messages.rotate_right(old_len);
        }
    }
}

/// Insert a synthetic assistant text message when a user message containing
/// tool_results is immediately followed by another user message (no assistant
/// in between). This happens when the assistant's response stream is
/// cancelled/interrupted after the tool_results are saved but before the
/// assistant reply is written. Without this, `build_body` would emit
/// consecutive user messages after filtering system messages, causing 400
/// errors from the API.
pub fn ensure_alternation_after_tool_result(messages: &mut Vec<Message>) {
    let mut i = 0;
    while i + 1 < messages.len() {
        let current_has_tool_result = matches!(&messages[i], Message::User(u) if u.content.iter().any(|b| matches!(b, ContentBlock::ToolResult { .. })));
        let next_is_user = matches!(&messages[i + 1], Message::User(_));
        // Don't insert a dummy assistant between two consecutive
        // tool-result-only user messages — they map to contiguous
        // role:"tool" wire messages and must not be split.
        let next_is_tool_only = matches!(&messages[i + 1], Message::User(u)
            if u.content.iter().all(|b| matches!(b, ContentBlock::ToolResult { .. })));

        if current_has_tool_result && next_is_user && !next_is_tool_only {
            // Check if the tool_results in the current user match the
            // preceding assistant's tool_calls. If they do, the tool
            // results belong to that assistant and will be emitted
            // correctly by build_body without breaking alternation.
            let tool_result_ids: Vec<String> = match &messages[i] {
                Message::User(u) => u.content.iter().filter_map(|b| {
                    if let ContentBlock::ToolResult { tool_use_id, .. } = b {
                        Some(tool_use_id.clone())
                    } else { None }
                }).collect(),
                _ => Vec::new(),
            };

            let needs_synthetic = if i > 0 {
                match &messages[i - 1] {
                    Message::Assistant(a) => {
                        let assistant_tool_ids: std::collections::HashSet<String> =
                            a.content.iter().filter_map(|b| {
                                if let ContentBlock::ToolUse { id, .. } = b {
                                    Some(id.clone())
                                } else { None }
                            }).collect();
                        // If ANY tool_result doesn't match the preceding assistant,
                        // we need the synthetic assistant.
                        tool_result_ids.iter().any(|id| !assistant_tool_ids.contains(id))
                    }
                    _ => true, // No preceding assistant — need synthetic
                }
            } else {
                true // First message — need synthetic
            };

            if needs_synthetic {
                // Insert a synthetic assistant message between them.
                let synthetic = Message::Assistant(AssistantMessage {
                    uuid: Uuid::new_v4(),
                    timestamp: chrono::Utc::now().to_rfc3339(),
                    content: vec![ContentBlock::Text {
                        text: "(response interrupted)".into(),
                    }],
                    model: None,
                    usage: None,
                    stop_reason: None,
                    request_id: None,
                });
                messages.insert(i + 1, synthetic);
                // Skip past the inserted message and the next user message.
                i += 2;
            } else {
                // Tool results belong to preceding assistant — no synthetic needed.
                i += 1;
            }
        } else {
            i += 1;
        }
    }
}

/// Remove synthetic assistant messages with only "(response interrupted)" text
/// that were incorrectly inserted between an assistant with tool_calls and
/// its tool results, or between a user (no tool_results) and a user with
/// tool_results that match an earlier assistant.
pub fn remove_stray_synthetic_assistants(messages: &mut Vec<Message>) {
    let mut i = 1;
    while i + 1 < messages.len() {
        // Check if messages[i] is a synthetic assistant with only "(response interrupted)"
        let is_synthetic = match &messages[i] {
            Message::Assistant(a) => a.content.len() == 1 && matches!(&a.content[0], ContentBlock::Text { text } if text == "(response interrupted)"),
            _ => false,
        };

        if is_synthetic {
            // Case 1: Synthetic between assistant with tool_calls and user with matching tool_results
            let prev_has_tool_calls = match &messages[i - 1] {
                Message::Assistant(a) => a.content.iter().any(|b| matches!(b, ContentBlock::ToolUse { .. })),
                _ => false,
            };
            
            let next_tool_result_ids: Vec<String> = if let Message::User(u) = &messages[i + 1] {
                u.content.iter().filter_map(|b| {
                    if let ContentBlock::ToolResult { tool_use_id, .. } = b { Some(tool_use_id.clone()) } else { None }
                }).collect()
            } else { Vec::new() };

            let case1 = prev_has_tool_calls && !next_tool_result_ids.is_empty();
            
            // Case 2: Synthetic between user (no tool_results) and user with tool_results
            // where the tool_results match an earlier assistant's tool_calls
            let prev_is_user_no_tool_results = matches!(&messages[i - 1], Message::User(u) if !u.content.iter().any(|b| matches!(b, ContentBlock::ToolResult { .. })));
            let case2 = if prev_is_user_no_tool_results {
                let next_tool_result_ids: Vec<String> = if let Message::User(u) = &messages[i + 1] {
                    u.content.iter().filter_map(|b| {
                        if let ContentBlock::ToolResult { tool_use_id, .. } = b { Some(tool_use_id.clone()) } else { None }
                    }).collect()
                } else { Vec::new() };
                
                if !next_tool_result_ids.is_empty() {
                    // Find the assistant with tool_calls that match these tool_results
                    let mut found_match = false;
                    for msg in messages[..i].iter().rev() {
                        if let Message::Assistant(a) = msg {
                            let assistant_tool_ids: std::collections::HashSet<String> = a.content.iter().filter_map(|b| {
                                if let ContentBlock::ToolUse { id, .. } = b { Some(id.clone()) } else { None }
                            }).collect();
                            if next_tool_result_ids.iter().any(|id| assistant_tool_ids.contains(id)) {
                                found_match = true;
                                break;
                            }
                        }
                    }
                    found_match
                } else {
                    false
                }
            } else {
                false
            };

            if case1 || case2 {
                messages.remove(i);
                continue;
            }
        }
        i += 1;
    }
}

/// Merge consecutive user messages into a single message.
/// The API requires strict user/assistant alternation.
///
/// **Exception**: User messages that contain *only* `ToolResult` blocks
/// are never merged because each must map to a separate `tool` role
/// message with its own `tool_call_id` in the OpenAI wire format.
pub fn merge_consecutive_user_messages(messages: &mut Vec<Message>) {
    let mut i = 0;
    while i + 1 < messages.len() {
        let current_is_tool_only = matches!(&messages[i], Message::User(u)
            if u.content.iter().all(|b| matches!(b, ContentBlock::ToolResult { .. })));
        let next_is_tool_only = matches!(&messages[i + 1], Message::User(u)
            if u.content.iter().all(|b| matches!(b, ContentBlock::ToolResult { .. })));

        // Never merge two tool-result-only messages — each needs its own
        // tool_call_id in the OpenAI wire format.
        if current_is_tool_only && next_is_tool_only {
            i += 1;
            continue;
        }

        let both_user = matches!(&messages[i], Message::User(_))
            && matches!(&messages[i + 1], Message::User(_));

        if both_user {
            // Merge content from i+1 into i.
            if let Message::User(next) = messages.remove(i + 1)
                && let Message::User(ref mut current) = messages[i]
            {
                current.content.extend(next.content);
            }
        } else {
            i += 1;
        }
    }
}

/// Insert a synthetic assistant text message between any two consecutive
/// user messages to maintain strict alternation.  Unlike
/// [`ensure_alternation_after_tool_result`], which only handles the
/// tool-result case, this covers *all* consecutive-user gaps.
pub fn insert_dummy_assistant_for_consecutive_users(messages: &mut Vec<Message>) {
    let mut i = 0;
    while i + 1 < messages.len() {
        let both_user = matches!(&messages[i], Message::User(_))
            && matches!(&messages[i + 1], Message::User(_));

        if both_user {
            let synthetic = Message::Assistant(AssistantMessage {
                uuid: Uuid::new_v4(),
                timestamp: chrono::Utc::now().to_rfc3339(),
                content: vec![ContentBlock::Text {
                    text: "(response interrupted)".into(),
                }],
                model: None,
                usage: None,
                stop_reason: None,
                request_id: None,
            });
            messages.insert(i + 1, synthetic);
            i += 2;
        } else {
            i += 1;
        }
    }
}

/// Prepend a default system message if the first message is not already
/// a system message.  Required by chat templates that mandate a leading
/// system prompt (e.g. MiMo-V2.5 / Qwen2-style).
pub fn ensure_system_message(messages: &mut Vec<Message>) {
    let has_leading_system = messages
        .first()
        .is_some_and(|m| matches!(m, Message::System(_)));
    if !has_leading_system {
        messages.insert(
            0,
            Message::System(SystemMessage {
                uuid: Uuid::new_v4(),
                timestamp: chrono::Utc::now().to_rfc3339(),
                subtype: SystemMessageType::Informational,
                content: String::new(),
                level: MessageLevel::Info,
            }),
        );
    }
}

/// Report of changes made during a normalization pass.
#[derive(Debug, Default, Clone)]
pub struct NormalizeReport {
    /// Orphaned tool_use blocks that got synthetic error tool_results.
    pub tool_results_added: usize,
    /// Empty text blocks removed from messages.
    pub empty_blocks_removed: usize,
    /// Messages removed because they had no content blocks.
    pub empty_messages_removed: usize,
    /// Oversized document blocks capped to text placeholders.
    pub documents_capped: usize,
    /// Consecutive user messages merged into one.
    pub consecutive_user_merged: usize,
}

impl std::fmt::Display for NormalizeReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut parts = Vec::new();
        if self.tool_results_added > 0 {
            parts.push(format!(
                "{} orphaned tool calls repaired",
                self.tool_results_added
            ));
        }
        if self.empty_blocks_removed > 0 {
            parts.push(format!(
                "{} empty blocks removed",
                self.empty_blocks_removed
            ));
        }
        if self.empty_messages_removed > 0 {
            parts.push(format!(
                "{} empty messages removed",
                self.empty_messages_removed
            ));
        }
        if self.documents_capped > 0 {
            parts.push(format!(
                "{} oversized documents capped",
                self.documents_capped
            ));
        }
        if self.consecutive_user_merged > 0 {
            parts.push(format!(
                "{} consecutive user messages merged",
                self.consecutive_user_merged
            ));
        }
        if parts.is_empty() {
            write!(f, "Session messages are already normalized.")
        } else {
            write!(f, "Normalized: {}", parts.join(", "))
        }
    }
}

/// Strategy for handling consecutive user messages.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConsecutiveUserStrategy {
    /// Merge consecutive user messages into one (lenient).
    Merge,
    /// Insert a synthetic assistant message between them (strict).
    InsertDummyAssistant,
    /// Leave them as-is (for templates that tolerate it).
    Keep,
}

/// Strategy for handling system messages.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SystemMessageStrategy {
    /// Prepend a default empty system message if missing (strict).
    EnsureDefault,
    /// Don't touch existing system messages (lenient).
    KeepExisting,
    /// Remove all system messages.
    RemoveAll,
}

/// Configuration for the normalization pipeline.
#[derive(Debug, Clone)]
pub struct NormalizationConfig {
    /// How to handle consecutive user messages.
    pub consecutive_user_strategy: ConsecutiveUserStrategy,
    /// How to handle system messages.
    pub system_message_strategy: SystemMessageStrategy,
    /// Whether to validate strict alternation after normalization.
    pub validate_alternation: bool,
    /// Whether to pair orphaned tool_use blocks with synthetic results.
    pub ensure_tool_result_pairing: bool,
    /// Maximum byte size for document blocks before capping.
    pub max_document_bytes: usize,
}

impl Default for NormalizationConfig {
    fn default() -> Self {
        strict_config()
    }
}

/// Strict normalization config for templates requiring strict alternation
/// and a leading system message (e.g. MiMo-V2.5, Qwen2, Llama3 with tools).
pub fn strict_config() -> NormalizationConfig {
    NormalizationConfig {
        consecutive_user_strategy: ConsecutiveUserStrategy::InsertDummyAssistant,
        system_message_strategy: SystemMessageStrategy::EnsureDefault,
        validate_alternation: true,
        ensure_tool_result_pairing: true,
        max_document_bytes: 500_000,
    }
}

/// Lenient normalization config for flexible templates that don't require
/// strict alternation or a leading system message.
pub fn lenient_config() -> NormalizationConfig {
    NormalizationConfig {
        consecutive_user_strategy: ConsecutiveUserStrategy::Merge,
        system_message_strategy: SystemMessageStrategy::KeepExisting,
        validate_alternation: false,
        ensure_tool_result_pairing: true,
        max_document_bytes: 500_000,
    }
}

/// Run the full normalization suite and return a report of what changed.
/// This is the diagnostic version of [`normalize_messages`] — it counts
/// every mutation so callers can display a summary.
pub fn normalize_all(messages: &mut Vec<Message>) -> NormalizeReport {
    let mut report = NormalizeReport::default();

    // 1. Tool-result pairing.
    let before = messages.len();
    ensure_tool_result_pairing(messages);
    report.tool_results_added = messages.len() - before;

    // 2. Strip empty text blocks.
    let before = count_text_blocks(messages);
    strip_empty_blocks(messages);
    report.empty_blocks_removed = before.saturating_sub(count_text_blocks(messages));

    // 2.5. Clean tool_use blocks with null input.
    clean_tool_use_input(messages);

    // 3. Remove empty messages.
    let before = messages.len();
    remove_empty_messages(messages);
    report.empty_messages_removed = before.saturating_sub(messages.len());

    // 4. Cap oversized documents.
    let before = count_document_blocks(messages);
    cap_document_blocks(messages, 500_000);
    report.documents_capped = before.saturating_sub(count_document_blocks(messages));

    // 5. Merge consecutive user messages.
    let before = messages.len();
    merge_consecutive_user_messages(messages);
    report.consecutive_user_merged = before.saturating_sub(messages.len());

    report
}

/// Run the normalization pipeline with the given config.
pub fn normalize_with_config(
    messages: &mut Vec<Message>,
    config: &NormalizationConfig,
) -> NormalizeReport {
    let mut report = NormalizeReport::default();

    // 1. Tool-result pairing.
    if config.ensure_tool_result_pairing {
        let before = messages.len();
        ensure_tool_result_pairing(messages);
        report.tool_results_added = messages.len().saturating_sub(before);
    }

    // 2. Strip empty text blocks.
    let before = count_text_blocks(messages);
    strip_empty_blocks(messages);
    report.empty_blocks_removed = before.saturating_sub(count_text_blocks(messages));

    // 3. Remove empty messages.
    let before = messages.len();
    remove_empty_messages(messages);
    report.empty_messages_removed = before.saturating_sub(messages.len());

    // 4. Cap oversized documents.
    let before = count_document_blocks(messages);
    cap_document_blocks(messages, config.max_document_bytes);
    report.documents_capped = before.saturating_sub(count_document_blocks(messages));

    // 5. System message strategy.
    match config.system_message_strategy {
        SystemMessageStrategy::RemoveAll => {
            messages.retain(|m| !matches!(m, Message::System(_)));
        }
        SystemMessageStrategy::EnsureDefault => {
            remove_mid_conversation_system_messages(messages);
            ensure_system_message(messages);
        }
        SystemMessageStrategy::KeepExisting => {}
    }

    // 6. Consecutive user message strategy.
    match config.consecutive_user_strategy {
        ConsecutiveUserStrategy::Merge => {
            let before = messages.len();
            merge_consecutive_user_messages(messages);
            report.consecutive_user_merged = before.saturating_sub(messages.len());
        }
        ConsecutiveUserStrategy::InsertDummyAssistant => {
            ensure_alternation_after_tool_result(messages);
            insert_dummy_assistant_for_consecutive_users(messages);
        }
        ConsecutiveUserStrategy::Keep => {}
    }

    // 7. Validate alternation.
    if config.validate_alternation {
        let _ = validate_alternation(messages);
    }

    report
}

/// Normalize messages using the strict config (for templates requiring
/// strict alternation and a leading system message).
pub fn normalize_strict(messages: &mut Vec<Message>) -> NormalizeReport {
    normalize_with_config(messages, &strict_config())
}

/// Normalize messages using the lenient config (for flexible templates).
pub fn normalize_lenient(messages: &mut Vec<Message>) -> NormalizeReport {
    normalize_with_config(messages, &lenient_config())
}

fn count_text_blocks(messages: &[Message]) -> usize {
    messages
        .iter()
        .filter_map(|m| match m {
            Message::User(u) => Some(u.content.as_slice()),
            Message::Assistant(a) => Some(a.content.as_slice()),
            _ => None,
        })
        .flatten()
        .filter(|b| matches!(b, ContentBlock::Text { .. }))
        .count()
}

fn count_document_blocks(messages: &[Message]) -> usize {
    messages
        .iter()
        .filter_map(|m| match m {
            Message::User(u) => Some(u.content.as_slice()),
            Message::Assistant(a) => Some(a.content.as_slice()),
            _ => None,
        })
        .flatten()
        .filter(|b| matches!(b, ContentBlock::Document { .. }))
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    #[test]
    fn test_clean_tool_use_input_null_input() {
        // Regression test: tool_use with null input should be cleaned to empty object.
        let mut messages = vec![
            Message::System(SystemMessage {
                uuid: Uuid::new_v4(),
                timestamp: String::new(),
                subtype: SystemMessageType::Informational,
                content: String::new(),
                level: MessageLevel::Info,
            }),
            Message::User(UserMessage {
                uuid: Uuid::new_v4(),
                timestamp: String::new(),
                content: vec![ContentBlock::Text { text: "do it".into() }],
                is_meta: false,
                is_compact_summary: false,
            }),
            Message::Assistant(AssistantMessage {
                uuid: Uuid::new_v4(),
                timestamp: String::new(),
                content: vec![ContentBlock::ToolUse {
                    id: "call_1".into(),
                    name: "Write".into(),
                    input: serde_json::Value::Null,
                }],
                model: None,
                usage: None,
                stop_reason: None,
                request_id: None,
            }),
        ];

        clean_tool_use_input(&mut messages);

        // The tool_use input should now be an empty object, not null
        if let Message::Assistant(a) = &messages[2] {
            for block in &a.content {
                if let ContentBlock::ToolUse { input, .. } = block {
                    assert!(!input.is_null(), "tool_use input should not be null");
                    assert!(input.is_object(), "tool_use input should be an object");
                    assert_eq!(*input, serde_json::json!({}));
                    // Note: deref input before comparing
                }
            }
        }
    }

    #[test]
    fn test_clean_tool_use_input_non_object_input() {
        // Regression test: tool_use with non-object input (string, number, bool) should be cleaned.
        let mut messages = vec![
            Message::System(SystemMessage {
                uuid: Uuid::new_v4(),
                timestamp: String::new(),
                subtype: SystemMessageType::Informational,
                content: String::new(),
                level: MessageLevel::Info,
            }),
            Message::User(UserMessage {
                uuid: Uuid::new_v4(),
                timestamp: String::new(),
                content: vec![ContentBlock::Text { text: "do it".into() }],
                is_meta: false,
                is_compact_summary: false,
            }),
            Message::Assistant(AssistantMessage {
                uuid: Uuid::new_v4(),
                timestamp: String::new(),
                content: vec![ContentBlock::ToolUse {
                    id: "call_2".into(),
                    name: "Bash".into(),
                    input: serde_json::Value::String("not an object".into()),
                }],
                model: None,
                usage: None,
                stop_reason: None,
                request_id: None,
            }),
        ];

        clean_tool_use_input(&mut messages);

        // The tool_use input should now be an empty object
        if let Message::Assistant(a) = &messages[2] {
            for block in &a.content {
                if let ContentBlock::ToolUse { input, .. } = block {
                    assert!(input.is_object(), "tool_use input should be an object");
                    assert_eq!(*input, serde_json::json!({}));
                    // Note: deref input before comparing
                }
            }
        }
    }

    #[test]
    fn test_clean_tool_use_input_empty_object_preserved() {
        // Empty object input should be preserved (not double-cleaned)
        let mut messages = vec![
            Message::System(SystemMessage {
                uuid: Uuid::new_v4(),
                timestamp: String::new(),
                subtype: SystemMessageType::Informational,
                content: String::new(),
                level: MessageLevel::Info,
            }),
            Message::User(UserMessage {
                uuid: Uuid::new_v4(),
                timestamp: String::new(),
                content: vec![ContentBlock::Text { text: "do it".into() }],
                is_meta: false,
                is_compact_summary: false,
            }),
            Message::Assistant(AssistantMessage {
                uuid: Uuid::new_v4(),
                timestamp: String::new(),
                content: vec![ContentBlock::ToolUse {
                    id: "call_3".into(),
                    name: "Read".into(),
                    input: serde_json::json!({}),
                }],
                model: None,
                usage: None,
                stop_reason: None,
                request_id: None,
            }),
        ];

        clean_tool_use_input(&mut messages);

        // The tool_use input should remain an empty object
        if let Message::Assistant(a) = &messages[2] {
            for block in &a.content {
                if let ContentBlock::ToolUse { input, .. } = block {
                    assert!(input.is_object(), "tool_use input should be an object");
                    assert_eq!(*input, serde_json::json!({}));
                }
            }
        }
    }

    #[test]
    fn test_tool_result_pairing() {
        let mut messages = vec![
            Message::Assistant(AssistantMessage {
                uuid: Uuid::new_v4(),
                timestamp: String::new(),
                content: vec![ContentBlock::ToolUse {
                    id: "call_1".into(),
                    name: "Bash".into(),
                    input: serde_json::json!({}),
                }],
                model: None,
                usage: None,
                stop_reason: None,
                request_id: None,
            }),
            // No tool_result for call_1!
        ];

        ensure_tool_result_pairing(&mut messages);

        // Should have added a synthetic error result.
        assert_eq!(messages.len(), 2);
        if let Message::User(u) = &messages[1] {
            assert!(matches!(
                &u.content[0],
                ContentBlock::ToolResult { is_error: true, .. }
            ));
        } else {
            panic!("Expected user message with tool result");
        }
    }

    #[test]
    fn test_merge_consecutive_users() {
        let mut messages = vec![
            user_message("hello"),
            user_message("world"),
            Message::Assistant(AssistantMessage {
                uuid: Uuid::new_v4(),
                timestamp: String::new(),
                content: vec![ContentBlock::Text { text: "hi".into() }],
                model: None,
                usage: None,
                stop_reason: None,
                request_id: None,
            }),
        ];

        merge_consecutive_user_messages(&mut messages);
        assert_eq!(messages.len(), 2); // Two user messages merged into one.
    }

    #[test]
    fn test_strip_empty_blocks() {
        let mut messages = vec![Message::User(UserMessage {
            uuid: Uuid::new_v4(),
            timestamp: String::new(),
            content: vec![
                ContentBlock::Text {
                    text: "".into(), // empty — should be removed
                },
                ContentBlock::Text {
                    text: "keep me".into(),
                },
            ],
            is_meta: false,
            is_compact_summary: false,
        })];
        strip_empty_blocks(&mut messages);
        if let Message::User(u) = &messages[0] {
            assert_eq!(u.content.len(), 1);
            assert_eq!(u.content[0].as_text(), Some("keep me"));
        }
    }

    #[test]
    fn test_validate_alternation_valid() {
        let messages = vec![
            user_message("hello"),
            Message::Assistant(AssistantMessage {
                uuid: Uuid::new_v4(),
                timestamp: String::new(),
                content: vec![ContentBlock::Text { text: "hi".into() }],
                model: None,
                usage: None,
                stop_reason: None,
                request_id: None,
            }),
        ];
        assert!(validate_alternation(&messages).is_ok());
    }

    #[test]
    fn test_validate_alternation_invalid() {
        let messages = vec![
            user_message("hello"),
            user_message("world"), // Two users in a row.
        ];
        assert!(validate_alternation(&messages).is_err());
    }

    #[test]
    fn test_remove_empty_messages() {
        let mut messages = vec![
            user_message("keep"),
            Message::User(UserMessage {
                uuid: Uuid::new_v4(),
                timestamp: String::new(),
                content: vec![], // empty — should be removed
                is_meta: false,
                is_compact_summary: false,
            }),
            user_message("also keep"),
        ];
        remove_empty_messages(&mut messages);
        assert_eq!(messages.len(), 2);
    }

    #[test]
    fn test_cap_document_blocks() {
        let mut messages = vec![Message::User(UserMessage {
            uuid: Uuid::new_v4(),
            timestamp: String::new(),
            content: vec![ContentBlock::Document {
                media_type: "application/pdf".into(),
                data: "x".repeat(1000),
                title: Some("big.pdf".into()),
            }],
            is_meta: false,
            is_compact_summary: false,
        })];
        // Cap at 500 bytes — should replace with text.
        cap_document_blocks(&mut messages, 500);
        if let Message::User(u) = &messages[0] {
            assert!(matches!(&u.content[0], ContentBlock::Text { .. }));
            if let ContentBlock::Text { text } = &u.content[0] {
                assert!(text.contains("big.pdf"));
                assert!(text.contains("too large"));
            }
        }
    }

    #[test]
    fn test_cap_document_blocks_within_limit() {
        let mut messages = vec![Message::User(UserMessage {
            uuid: Uuid::new_v4(),
            timestamp: String::new(),
            content: vec![ContentBlock::Document {
                media_type: "application/pdf".into(),
                data: "small".into(),
                title: Some("small.pdf".into()),
            }],
            is_meta: false,
            is_compact_summary: false,
        })];
        // Cap at 500 bytes — should keep as-is.
        cap_document_blocks(&mut messages, 500);
        if let Message::User(u) = &messages[0] {
            assert!(matches!(&u.content[0], ContentBlock::Document { .. }));
        }
    }

    #[test]
    fn test_tool_result_pairing_already_paired() {
        let mut messages = vec![
            Message::Assistant(AssistantMessage {
                uuid: Uuid::new_v4(),
                timestamp: String::new(),
                content: vec![ContentBlock::ToolUse {
                    id: "call_1".into(),
                    name: "Bash".into(),
                    input: serde_json::json!({}),
                }],
                model: None,
                usage: None,
                stop_reason: None,
                request_id: None,
            }),
            Message::User(UserMessage {
                uuid: Uuid::new_v4(),
                timestamp: String::new(),
                content: vec![ContentBlock::ToolResult {
                    tool_use_id: "call_1".into(),
                    content: "ok".into(),
                    is_error: false,
                    extra_content: vec![],
                }],
                is_meta: true,
                is_compact_summary: false,
            }),
        ];

        ensure_tool_result_pairing(&mut messages);
        // No change expected — already paired.
        assert_eq!(messages.len(), 2);
    }

    #[test]
    fn test_tool_result_pairing_multiple_orphans() {
        let mut messages = vec![Message::Assistant(AssistantMessage {
            uuid: Uuid::new_v4(),
            timestamp: String::new(),
            content: vec![
                ContentBlock::ToolUse {
                    id: "call_a".into(),
                    name: "Bash".into(),
                    input: serde_json::json!({}),
                },
                ContentBlock::ToolUse {
                    id: "call_b".into(),
                    name: "FileRead".into(),
                    input: serde_json::json!({}),
                },
            ],
            model: None,
            usage: None,
            stop_reason: None,
            request_id: None,
        })];

        ensure_tool_result_pairing(&mut messages);
        // All orphaned tool results are combined into a single user message
        // so build_body produces contiguous role:"tool" messages.
        assert_eq!(messages.len(), 2);
        if let Message::User(u) = &messages[1] {
            assert_eq!(u.content.len(), 2, "both tool results in one message");
            for block in &u.content {
                assert!(matches!(
                    block,
                    ContentBlock::ToolResult { is_error: true, .. }
                ));
            }
        } else {
            panic!("Expected single user message with both tool results");
        }
    }

    #[test]
    fn test_merge_no_consecutive_users() {
        let assistant = Message::Assistant(AssistantMessage {
            uuid: Uuid::new_v4(),
            timestamp: String::new(),
            content: vec![ContentBlock::Text { text: "hi".into() }],
            model: None,
            usage: None,
            stop_reason: None,
            request_id: None,
        });
        let mut messages = vec![user_message("hello"), assistant, user_message("bye")];

        merge_consecutive_user_messages(&mut messages);
        assert_eq!(messages.len(), 3); // No change.
    }

    #[test]
    fn test_merge_three_consecutive_users() {
        let mut messages = vec![
            user_message("one"),
            user_message("two"),
            user_message("three"),
        ];

        merge_consecutive_user_messages(&mut messages);
        assert_eq!(messages.len(), 1); // All merged into one.
        if let Message::User(u) = &messages[0] {
            assert_eq!(u.content.len(), 3);
        } else {
            panic!("Expected user message");
        }
    }

    #[test]
    fn test_validate_alternation_with_system_messages() {
        let messages = vec![
            Message::System(SystemMessage {
                uuid: Uuid::new_v4(),
                timestamp: String::new(),
                subtype: SystemMessageType::Informational,
                content: "system note".into(),
                level: MessageLevel::Info,
            }),
            user_message("hello"),
            Message::System(SystemMessage {
                uuid: Uuid::new_v4(),
                timestamp: String::new(),
                subtype: SystemMessageType::Informational,
                content: "another note".into(),
                level: MessageLevel::Info,
            }),
            Message::Assistant(AssistantMessage {
                uuid: Uuid::new_v4(),
                timestamp: String::new(),
                content: vec![ContentBlock::Text { text: "hi".into() }],
                model: None,
                usage: None,
                stop_reason: None,
                request_id: None,
            }),
        ];
        assert!(validate_alternation(&messages).is_ok());
    }

    #[test]
    fn test_validate_alternation_empty_list() {
        let messages: Vec<Message> = vec![];
        assert!(validate_alternation(&messages).is_ok());
    }

    #[test]
    fn test_strip_empty_blocks_on_assistant() {
        let mut messages = vec![Message::Assistant(AssistantMessage {
            uuid: Uuid::new_v4(),
            timestamp: String::new(),
            content: vec![
                ContentBlock::Text { text: "".into() },
                ContentBlock::Text {
                    text: "real content".into(),
                },
                ContentBlock::Text { text: "".into() },
            ],
            model: None,
            usage: None,
            stop_reason: None,
            request_id: None,
        })];
        strip_empty_blocks(&mut messages);
        if let Message::Assistant(a) = &messages[0] {
            assert_eq!(a.content.len(), 1);
            assert_eq!(a.content[0].as_text(), Some("real content"));
        }
    }

    #[test]
    fn test_remove_empty_messages_preserves_system() {
        let mut messages = vec![
            Message::System(SystemMessage {
                uuid: Uuid::new_v4(),
                timestamp: String::new(),
                subtype: SystemMessageType::Informational,
                content: "".into(), // Empty content but system messages are always kept.
                level: MessageLevel::Info,
            }),
            Message::User(UserMessage {
                uuid: Uuid::new_v4(),
                timestamp: String::new(),
                content: vec![], // Empty — should be removed.
                is_meta: false,
                is_compact_summary: false,
            }),
            user_message("keep me"),
        ];
        remove_empty_messages(&mut messages);
        assert_eq!(messages.len(), 2); // System + "keep me".
        assert!(matches!(&messages[0], Message::System(_)));
        assert!(matches!(&messages[1], Message::User(_)));
    }

    #[test]
    fn test_cap_document_blocks_no_title_uses_document() {
        let mut messages = vec![Message::User(UserMessage {
            uuid: Uuid::new_v4(),
            timestamp: String::new(),
            content: vec![ContentBlock::Document {
                media_type: "text/plain".into(),
                data: "x".repeat(200),
                title: None,
            }],
            is_meta: false,
            is_compact_summary: false,
        })];
        cap_document_blocks(&mut messages, 100);
        if let Message::User(u) = &messages[0] {
            if let ContentBlock::Text { text } = &u.content[0] {
                assert!(
                    text.contains("document"),
                    "should use fallback name 'document'"
                );
                assert!(text.contains("too large"));
            } else {
                panic!("Expected text block after capping");
            }
        }
    }

    #[test]
    fn test_normalize_all_orphaned_tool_calls() {
        let mut messages = vec![Message::Assistant(AssistantMessage {
            uuid: Uuid::new_v4(),
            timestamp: String::new(),
            content: vec![
                ContentBlock::ToolUse {
                    id: "call_1".into(),
                    name: "Bash".into(),
                    input: serde_json::json!({}),
                },
                ContentBlock::ToolUse {
                    id: "call_2".into(),
                    name: "FileRead".into(),
                    input: serde_json::json!({}),
                },
            ],
            model: None,
            usage: None,
            stop_reason: None,
            request_id: None,
        })];

        let report = normalize_all(&mut messages);
        // Now combined into single user message with both tool results.
        assert_eq!(report.tool_results_added, 1);
        assert_eq!(messages.len(), 2);
        if let Message::User(u) = &messages[1] {
            assert_eq!(u.content.len(), 2);
        } else {
            panic!("expected user message with combined tool results");
        }
    }

    #[test]
    fn test_normalize_all_empty_blocks_and_messages() {
        let mut messages = vec![
            Message::User(UserMessage {
                uuid: Uuid::new_v4(),
                timestamp: String::new(),
                content: vec![
                    ContentBlock::Text { text: "".into() },
                    ContentBlock::Text {
                        text: "keep".into(),
                    },
                ],
                is_meta: false,
                is_compact_summary: false,
            }),
            Message::User(UserMessage {
                uuid: Uuid::new_v4(),
                timestamp: String::new(),
                content: vec![],
                is_meta: false,
                is_compact_summary: false,
            }),
            Message::User(UserMessage {
                uuid: Uuid::new_v4(),
                timestamp: String::new(),
                content: vec![ContentBlock::Text {
                    text: "also keep".into(),
                }],
                is_meta: false,
                is_compact_summary: false,
            }),
        ];

        let report = normalize_all(&mut messages);
        assert!(report.empty_blocks_removed >= 1);
        assert!(report.empty_messages_removed >= 1);
        assert!(report.consecutive_user_merged >= 1);
    }

    #[test]
    fn test_normalize_all_already_clean() {
        let mut messages = vec![
            user_message("hello"),
            Message::Assistant(AssistantMessage {
                uuid: Uuid::new_v4(),
                timestamp: String::new(),
                content: vec![ContentBlock::Text { text: "hi".into() }],
                model: None,
                usage: None,
                stop_reason: None,
                request_id: None,
            }),
            user_message("bye"),
        ];

        let report = normalize_all(&mut messages);
        assert_eq!(report.tool_results_added, 0);
        assert_eq!(report.empty_blocks_removed, 0);
        assert_eq!(report.empty_messages_removed, 0);
        assert_eq!(report.consecutive_user_merged, 0);
    }

    #[test]
    fn test_normalize_report_display() {
        let report = NormalizeReport {
            tool_results_added: 2,
            empty_blocks_removed: 1,
            empty_messages_removed: 0,
            documents_capped: 0,
            consecutive_user_merged: 3,
        };
        let s = report.to_string();
        assert!(s.contains("2 orphaned tool calls repaired"));
        assert!(s.contains("1 empty blocks removed"));
        assert!(s.contains("3 consecutive user messages merged"));
    }

    #[test]
    fn test_normalize_report_display_clean() {
        let report = NormalizeReport::default();
        assert_eq!(
            report.to_string(),
            "Session messages are already normalized."
        );
    }

    #[test]
    fn test_truncate_to_last_summary_drops_head_keeps_summary() {
        let summary = Message::User(UserMessage {
            uuid: Uuid::new_v4(),
            timestamp: String::new(),
            content: vec![ContentBlock::Text {
                text: "prior context summary".into(),
            }],
            is_meta: true,
            is_compact_summary: true,
        });
        let mut messages = vec![
            user_message("old message before summary"),
            user_message("another old message"),
            summary,
            user_message("recent message after summary"),
            Message::Assistant(AssistantMessage {
                uuid: Uuid::new_v4(),
                timestamp: String::new(),
                content: vec![ContentBlock::Text {
                    text: "reply".into(),
                }],
                model: None,
                usage: None,
                stop_reason: None,
                request_id: None,
            }),
        ];

        let head = truncate_to_last_summary(&mut messages);
        // Two pre-summary messages dropped and returned.
        assert_eq!(head.len(), 2);
        // Active tail keeps the summary as the first message.
        assert_eq!(messages.len(), 3);
        if let Message::User(u) = &messages[0] {
            assert!(u.is_compact_summary);
        } else {
            panic!("expected summary first");
        }
    }

    #[test]
    fn test_truncate_to_last_summary_no_summary_is_noop() {
        let mut messages = vec![user_message("a"), user_message("b")];
        let head = truncate_to_last_summary(&mut messages);
        assert!(head.is_empty());
        assert_eq!(messages.len(), 2);
    }

    #[test]
    fn test_normalize_strict_inserts_dummy_assistant() {
        let mut messages = vec![user_message("one"), user_message("two")];
        normalize_strict(&mut messages);
        assert!(validate_alternation(&messages).is_ok());
    }

    #[test]
    fn test_normalize_lenient_merges_consecutive_users() {
        let mut messages = vec![user_message("one"), user_message("two")];
        normalize_lenient(&mut messages);
        assert_eq!(messages.len(), 1);
    }

    #[test]
    fn test_ensure_system_message_adds_when_missing() {
        let mut messages = vec![user_message("hello")];
        ensure_system_message(&mut messages);
        assert_eq!(messages.len(), 2);
        assert!(matches!(&messages[0], Message::System(_)));
    }

    #[test]
    fn test_ensure_system_message_no_op_when_present() {
        let mut messages = vec![
            Message::System(SystemMessage {
                uuid: Uuid::new_v4(),
                timestamp: String::new(),
                subtype: SystemMessageType::Informational,
                content: "existing".into(),
                level: MessageLevel::Info,
            }),
            user_message("hello"),
        ];
        ensure_system_message(&mut messages);
        assert_eq!(messages.len(), 2);
    }

    #[test]
    fn test_insert_dummy_assistant_for_consecutive_users() {
        let mut messages = vec![user_message("a"), user_message("b"), user_message("c")];
        insert_dummy_assistant_for_consecutive_users(&mut messages);
        assert_eq!(messages.len(), 5);
        assert!(validate_alternation(&messages).is_ok());
    }

    #[test]
    fn test_strict_config_fields() {
        let config = strict_config();
        assert_eq!(
            config.consecutive_user_strategy,
            ConsecutiveUserStrategy::InsertDummyAssistant
        );
        assert_eq!(
            config.system_message_strategy,
            SystemMessageStrategy::EnsureDefault
        );
        assert!(config.validate_alternation);
        assert!(config.ensure_tool_result_pairing);
        assert_eq!(config.max_document_bytes, 500_000);
    }

    #[test]
    fn test_lenient_config_fields() {
        let config = lenient_config();
        assert_eq!(
            config.consecutive_user_strategy,
            ConsecutiveUserStrategy::Merge
        );
        assert_eq!(
            config.system_message_strategy,
            SystemMessageStrategy::KeepExisting
        );
        assert!(!config.validate_alternation);
        assert!(config.ensure_tool_result_pairing);
        assert_eq!(config.max_document_bytes, 500_000);
    }

    #[test]
    fn test_normalize_with_config_remove_all_system() {
        let mut messages = vec![
            Message::System(SystemMessage {
                uuid: Uuid::new_v4(),
                timestamp: String::new(),
                subtype: SystemMessageType::Informational,
                content: "remove me".into(),
                level: MessageLevel::Info,
            }),
            user_message("hello"),
            Message::System(SystemMessage {
                uuid: Uuid::new_v4(),
                timestamp: String::new(),
                subtype: SystemMessageType::Informational,
                content: "also remove".into(),
                level: MessageLevel::Info,
            }),
        ];
        let config = NormalizationConfig {
            system_message_strategy: SystemMessageStrategy::RemoveAll,
            ..strict_config()
        };
        normalize_with_config(&mut messages, &config);
        assert!(!messages.iter().any(|m| matches!(m, Message::System(_))));
    }

    #[test]
    fn test_tool_result_pairing_combines_into_single_message() {
        // Two orphaned tool_uses in one assistant → one user message with both results.
        let mut messages = vec![Message::Assistant(AssistantMessage {
            uuid: Uuid::new_v4(),
            timestamp: String::new(),
            content: vec![
                ContentBlock::ToolUse {
                    id: "c1".into(),
                    name: "Bash".into(),
                    input: serde_json::json!({}),
                },
                ContentBlock::ToolUse {
                    id: "c2".into(),
                    name: "FileRead".into(),
                    input: serde_json::json!({}),
                },
                ContentBlock::ToolUse {
                    id: "c3".into(),
                    name: "Grep".into(),
                    input: serde_json::json!({}),
                },
            ],
            model: None,
            usage: None,
            stop_reason: None,
            request_id: None,
        })];

        ensure_tool_result_pairing(&mut messages);

        // 1 assistant + 1 combined user message = 2 total.
        assert_eq!(messages.len(), 2);
        if let Message::User(u) = &messages[1] {
            assert_eq!(u.content.len(), 3);
            let ids: Vec<&str> = u
                .content
                .iter()
                .filter_map(|b| match b {
                    ContentBlock::ToolResult { tool_use_id, .. } => Some(tool_use_id.as_str()),
                    _ => None,
                })
                .collect();
            assert_eq!(ids, vec!["c1", "c2", "c3"]);
        } else {
            panic!("expected user message");
        }
    }

    #[test]
    fn test_ensure_alternation_skips_between_tool_only_messages() {
        // Two consecutive tool-result-only user messages should NOT get a
        // dummy assistant between them.
        let mut messages = vec![
            Message::User(UserMessage {
                uuid: Uuid::new_v4(),
                timestamp: String::new(),
                content: vec![ContentBlock::ToolResult {
                    tool_use_id: "a".into(),
                    content: "ok".into(),
                    is_error: false,
                    extra_content: vec![],
                }],
                is_meta: true,
                is_compact_summary: false,
            }),
            Message::User(UserMessage {
                uuid: Uuid::new_v4(),
                timestamp: String::new(),
                content: vec![ContentBlock::ToolResult {
                    tool_use_id: "b".into(),
                    content: "ok".into(),
                    is_error: false,
                    extra_content: vec![],
                }],
                is_meta: true,
                is_compact_summary: false,
            }),
        ];

        ensure_alternation_after_tool_result(&mut messages);
        // No dummy assistant inserted — both are tool-result-only.
        assert_eq!(messages.len(), 2);
        assert!(messages.iter().all(|m| matches!(m, Message::User(_))));
    }

    #[test]
    fn test_ensure_alternation_still_inserts_for_mixed_content() {
        // Tool-result user followed by a text user → dummy should be inserted.
        let mut messages = vec![
            Message::User(UserMessage {
                uuid: Uuid::new_v4(),
                timestamp: String::new(),
                content: vec![ContentBlock::ToolResult {
                    tool_use_id: "a".into(),
                    content: "ok".into(),
                    is_error: false,
                    extra_content: vec![],
                }],
                is_meta: true,
                is_compact_summary: false,
            }),
            user_message("follow-up question"),
        ];

        ensure_alternation_after_tool_result(&mut messages);
        assert_eq!(messages.len(), 3);
        assert!(matches!(&messages[1], Message::Assistant(_)));
    }

    #[test]
    fn test_strict_normalize_with_multiple_orphaned_tool_uses() {
        // End-to-end: normalize_strict with 2 orphaned tool_uses produces
        // valid alternation and contiguous tool results.
        let mut messages = vec![
            user_message("run tests"),
            Message::Assistant(AssistantMessage {
                uuid: Uuid::new_v4(),
                timestamp: String::new(),
                content: vec![
                    ContentBlock::ToolUse {
                        id: "t1".into(),
                        name: "Bash".into(),
                        input: serde_json::json!({}),
                    },
                    ContentBlock::ToolUse {
                        id: "t2".into(),
                        name: "Bash".into(),
                        input: serde_json::json!({}),
                    },
                ],
                model: None,
                usage: None,
                stop_reason: None,
                request_id: None,
            }),
        ];

        normalize_strict(&mut messages);

        // Must have valid alternation.
        assert!(validate_alternation(&messages).is_ok());

        // The tool-result user message must contain both results (no dummy
        // assistant between them).
        let tool_user = messages
            .iter()
            .find(|m| matches!(m, Message::User(u) if u.content.iter().all(|b| matches!(b, ContentBlock::ToolResult { .. }))));
        match tool_user {
            Some(Message::User(u)) => {
                assert_eq!(
                    u.content.len(),
                    2,
                    "both tool results in one message, no dummy split"
                );
            }
            _ => panic!("expected combined tool-result user message"),
        }
    }

}
