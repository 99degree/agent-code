//! Message normalization and validation utilities.
//!
//! Ensures messages conform to API requirements before sending:
//! - Tool use / tool result pairing
//! - Content block ordering
//! - Empty message handling

use super::message::*;
use uuid::Uuid;

/// Repair tool_use / tool_result pairing so strict providers accept
/// the history.
///
/// Malformed histories (crash mid-turn, imported sessions, permissive
/// upstream models) show up three ways, all rejected with a 400 by
/// strict OpenAI-compatible and local backends:
///
/// - a `tool_use` with no answering `tool_result` → a synthetic error
///   result is appended (long-standing behavior);
/// - a `tool_result` with no preceding `tool_use` for its id
///   (out-of-order or truly orphaned) → the block is dropped;
/// - several `tool_result`s for the same id → the first is kept, the
///   rest are dropped (the first is the one the model actually
///   continued from).
///
/// Blocks emptied by the drops are cleaned up by the
/// `remove_empty_messages` step that follows in the pipeline.
pub fn ensure_tool_result_pairing(messages: &mut Vec<Message>) {
    use std::collections::HashSet;

    // IDs of tool_use blocks seen so far in the walk (a result is only
    // valid when its call precedes it), and IDs already answered.
    let mut seen_use_ids: HashSet<String> = HashSet::new();
    let mut answered_ids: HashSet<String> = HashSet::new();
    // Preserves emission order for the synthetic results appended below.
    let mut pending_tool_ids: Vec<String> = Vec::new();

    for msg in messages.iter_mut() {
        match msg {
            Message::Assistant(a) => {
                for block in &a.content {
                    if let ContentBlock::ToolUse { id, .. } = block
                        && seen_use_ids.insert(id.clone())
                    {
                        pending_tool_ids.push(id.clone());
                    }
                }
            }
            Message::User(u) => {
                u.content.retain(|block| {
                    let ContentBlock::ToolResult { tool_use_id, .. } = block else {
                        return true;
                    };
                    if !seen_use_ids.contains(tool_use_id) {
                        // Out-of-order or orphaned result — no call to
                        // pair with; keeping it guarantees a 400.
                        return false;
                    }
                    // Keep only the first result per id.
                    answered_ids.insert(tool_use_id.clone())
                });
            }
            _ => {}
        }
    }

    // Any calls still unanswered get synthetic error results.
    pending_tool_ids.retain(|id| !answered_ids.contains(id));
    for id in pending_tool_ids {
        messages.push(tool_result_message(
            &id,
            "(tool execution was interrupted)",
            true,
        ));
    }
}

/// Replace `tool_use` inputs that are not JSON objects.
///
/// Providers require tool-call arguments to be an object; some models
/// emit `null` or the arguments as a JSON-encoded *string*. A string
/// that parses to an object is adopted (the arguments were merely
/// double-encoded); anything else non-object becomes `{}` so the
/// request is not rejected outright.
pub fn sanitize_tool_use_input(messages: &mut [Message]) {
    for msg in messages.iter_mut() {
        let Message::Assistant(a) = msg else { continue };
        for block in a.content.iter_mut() {
            let ContentBlock::ToolUse { input, .. } = block else {
                continue;
            };
            if input.is_object() {
                continue;
            }
            if let serde_json::Value::String(s) = &input
                && let Ok(parsed) = serde_json::from_str::<serde_json::Value>(s)
                && parsed.is_object()
            {
                *input = parsed;
                continue;
            }
            *input = serde_json::json!({});
        }
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

/// Run the strict normalization pipeline on the given messages.
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
                Message::User(u) => u
                    .content
                    .iter()
                    .filter_map(|b| {
                        if let ContentBlock::ToolResult { tool_use_id, .. } = b {
                            Some(tool_use_id.clone())
                        } else {
                            None
                        }
                    })
                    .collect(),
                _ => Vec::new(),
            };

            let needs_synthetic = if i > 0 {
                match &messages[i - 1] {
                    Message::Assistant(a) => {
                        let assistant_tool_ids: std::collections::HashSet<String> = a
                            .content
                            .iter()
                            .filter_map(|b| {
                                if let ContentBlock::ToolUse { id, .. } = b {
                                    Some(id.clone())
                                } else {
                                    None
                                }
                            })
                            .collect();
                        // If ANY tool_result doesn't match the preceding assistant,
                        // we need the synthetic assistant.
                        tool_result_ids
                            .iter()
                            .any(|id| !assistant_tool_ids.contains(id))
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
            Message::Assistant(a) => {
                a.content.len() == 1
                    && matches!(&a.content[0], ContentBlock::Text { text } if text == "(response interrupted)")
            }
            _ => false,
        };

        if is_synthetic {
            // Case 1: Synthetic between assistant with tool_calls and user with matching tool_results
            let prev_has_tool_calls = match &messages[i - 1] {
                Message::Assistant(a) => a
                    .content
                    .iter()
                    .any(|b| matches!(b, ContentBlock::ToolUse { .. })),
                _ => false,
            };

            let next_tool_result_ids: Vec<String> = if let Message::User(u) = &messages[i + 1] {
                u.content
                    .iter()
                    .filter_map(|b| {
                        if let ContentBlock::ToolResult { tool_use_id, .. } = b {
                            Some(tool_use_id.clone())
                        } else {
                            None
                        }
                    })
                    .collect()
            } else {
                Vec::new()
            };

            let case1 = prev_has_tool_calls && !next_tool_result_ids.is_empty();

            // Case 2: Synthetic between user (no tool_results) and user with tool_results
            // where the tool_results match an earlier assistant's tool_calls
            let prev_is_user_no_tool_results = matches!(&messages[i - 1], Message::User(u) if !u.content.iter().any(|b| matches!(b, ContentBlock::ToolResult { .. })));
            let case2 = if prev_is_user_no_tool_results {
                let next_tool_result_ids: Vec<String> = if let Message::User(u) = &messages[i + 1] {
                    u.content
                        .iter()
                        .filter_map(|b| {
                            if let ContentBlock::ToolResult { tool_use_id, .. } = b {
                                Some(tool_use_id.clone())
                            } else {
                                None
                            }
                        })
                        .collect()
                } else {
                    Vec::new()
                };

                if !next_tool_result_ids.is_empty() {
                    // Find the assistant with tool_calls that match these tool_results
                    let mut found_match = false;
                    for msg in messages[..i].iter().rev() {
                        if let Message::Assistant(a) = msg {
                            let assistant_tool_ids: std::collections::HashSet<String> = a
                                .content
                                .iter()
                                .filter_map(|b| {
                                    if let ContentBlock::ToolUse { id, .. } = b {
                                        Some(id.clone())
                                    } else {
                                        None
                                    }
                                })
                                .collect();
                            if next_tool_result_ids
                                .iter()
                                .any(|id| assistant_tool_ids.contains(id))
                            {
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

            // Case 3: Synthetic between two tool-result user messages.
            // This splits a single assistant's tool results with a dummy
            // assistant, which OpenAI rejects ("Not the same number of
            // function calls and responses") because the tool results are
            // no longer contiguous after their tool_call assistant. The
            // synthetic was inserted by an older normalize path that
            // didn't guard against consecutive tool-result users; dropping
            // it restores contiguous role:"tool" wire messages.
            let prev_is_user_with_tool_result = matches!(&messages[i - 1], Message::User(u)
                if u.content.iter().any(|b| matches!(b, ContentBlock::ToolResult { .. })));
            let next_is_tool_only_user = matches!(&messages[i + 1], Message::User(u)
                if u.content.iter().all(|b| matches!(b, ContentBlock::ToolResult { .. })));
            let case3 = prev_is_user_with_tool_result && next_is_tool_only_user;

            if case1 || case2 || case3 {
                messages.remove(i);
                continue;
            }
        }
        i += 1;
    }
}

/// Split any user message that mixes `ToolResult` blocks with other block
/// types (text, thinking, image, …) into two consecutive user messages:
/// first the `ToolResult` blocks, then the remaining blocks.
///
/// This prevents a wire-format defect where `build_body` turns the
/// `ToolResult` blocks into `role: "tool"` messages and the other blocks
/// into a `role: "user"` message, producing consecutive `tool` then `user`
/// roles. Both the Mistral and MiMo chat templates reject `user` after
/// `tool` (the order matrix requires `assistant` between them), so the
/// request 400s before any token is generated.
///
/// The split keeps the `ToolResult`-only half adjacent to its preceding
/// assistant `tool_use` (so `build_body` emits `tool` with the right
/// `tool_call_id`), and the remaining half becomes a standalone `user`
/// message. `ensure_alternation_after_tool_result` then inserts a synthetic
/// assistant between the two user halves, restoring valid alternation.
///
/// This is the root-cause fix for mixed tool-result/user messages, which
/// arise whenever steered/queued user text (including a cancel typed
/// during tool execution) is merged with a tool result into one user turn.
pub fn split_mixed_tool_result_users(messages: &mut Vec<Message>) {
    // Collect split points first to avoid mutable-iteration borrow issues.
    let mut expanded: Vec<Message> = Vec::with_capacity(messages.len());
    for msg in messages.drain(..) {
        if let Message::User(u) = &msg {
            let has_tool_result = u
                .content
                .iter()
                .any(|b| matches!(b, ContentBlock::ToolResult { .. }));
            let has_other = u
                .content
                .iter()
                .any(|b| !matches!(b, ContentBlock::ToolResult { .. }));
            if has_tool_result && has_other {
                let mut tool_blocks = Vec::new();
                let mut other_blocks = Vec::new();
                for b in u.content.iter() {
                    if matches!(b, ContentBlock::ToolResult { .. }) {
                        tool_blocks.push(b.clone());
                    } else {
                        other_blocks.push(b.clone());
                    }
                }
                expanded.push(Message::User(UserMessage {
                    uuid: Uuid::new_v4(),
                    timestamp: u.timestamp.clone(),
                    content: tool_blocks,
                    is_meta: u.is_meta,
                    is_compact_summary: u.is_compact_summary,
                }));
                expanded.push(Message::User(UserMessage {
                    uuid: Uuid::new_v4(),
                    timestamp: u.timestamp.clone(),
                    content: other_blocks,
                    is_meta: u.is_meta,
                    is_compact_summary: u.is_compact_summary,
                }));
                continue;
            }
        }
        expanded.push(msg);
    }
    *messages = expanded;
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

    // 2.5. Sanitize tool_use blocks with null/invalid input.
    sanitize_tool_use_input(messages);

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

    // 5.5. Split user messages that mix tool results with other blocks.
    // Must run before the consecutive-user strategy so the split tool-result
    // half and the remaining user half are seen as two consecutive users,
    // letting `ensure_alternation_after_tool_result` insert a synthetic
    // assistant between them (valid `tool → assistant → user` wire order).
    split_mixed_tool_result_users(messages);

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
        // Should add two synthetic error results (one per orphan).
        assert_eq!(messages.len(), 3);
        for msg in &messages[1..] {
            if let Message::User(u) = msg {
                assert!(matches!(
                    &u.content[0],
                    ContentBlock::ToolResult { is_error: true, .. }
                ));
            } else {
                panic!("Expected user message with tool result");
            }
        }
    }

    fn assistant_with_use(id: &str, input: serde_json::Value) -> Message {
        Message::Assistant(AssistantMessage {
            uuid: Uuid::new_v4(),
            timestamp: String::new(),
            content: vec![ContentBlock::ToolUse {
                id: id.into(),
                name: "Bash".into(),
                input,
            }],
            model: None,
            usage: None,
            stop_reason: None,
            request_id: None,
        })
    }

    fn result_content(msg: &Message) -> Vec<(&str, &str)> {
        match msg {
            Message::User(u) => u
                .content
                .iter()
                .filter_map(|b| match b {
                    ContentBlock::ToolResult {
                        tool_use_id,
                        content,
                        ..
                    } => Some((tool_use_id.as_str(), content.as_str())),
                    _ => None,
                })
                .collect(),
            _ => vec![],
        }
    }

    #[test]
    fn orphan_result_with_no_preceding_use_is_dropped() {
        let mut messages = vec![
            user_message("hi"),
            // Result arrives before any tool_use with this id exists.
            tool_result_message("never_called", "stale", false),
            assistant_with_use("call_1", serde_json::json!({})),
            tool_result_message("call_1", "ok", false),
        ];
        ensure_tool_result_pairing(&mut messages);
        let all_results: Vec<_> = messages.iter().flat_map(result_content).collect();
        assert_eq!(
            all_results,
            vec![("call_1", "ok")],
            "orphan result must be dropped, valid pair preserved"
        );
    }

    #[test]
    fn out_of_order_result_before_its_use_is_dropped_then_synthesized() {
        let mut messages = vec![
            // Result precedes its own tool_use — invalid ordering.
            tool_result_message("call_1", "too early", false),
            assistant_with_use("call_1", serde_json::json!({})),
        ];
        ensure_tool_result_pairing(&mut messages);
        let all_results: Vec<_> = messages.iter().flat_map(result_content).collect();
        // The early result is dropped; the now-unanswered call gets a
        // synthetic error result appended.
        assert_eq!(all_results.len(), 1);
        assert_eq!(all_results[0].0, "call_1");
        assert_ne!(all_results[0].1, "too early");
    }

    #[test]
    fn duplicate_results_keep_only_the_first() {
        let mut messages = vec![
            assistant_with_use("call_1", serde_json::json!({})),
            tool_result_message("call_1", "first", false),
            tool_result_message("call_1", "second", false),
            tool_result_message("call_1", "third", false),
        ];
        ensure_tool_result_pairing(&mut messages);
        let all_results: Vec<_> = messages.iter().flat_map(result_content).collect();
        assert_eq!(
            all_results,
            vec![("call_1", "first")],
            "only the first result per id survives"
        );
    }

    #[test]
    fn duplicate_result_inside_one_message_is_deduplicated() {
        let mut messages = vec![
            assistant_with_use("call_1", serde_json::json!({})),
            Message::User(UserMessage {
                uuid: Uuid::new_v4(),
                timestamp: String::new(),
                content: vec![
                    ContentBlock::ToolResult {
                        tool_use_id: "call_1".into(),
                        content: "first".into(),
                        is_error: false,
                        extra_content: vec![],
                    },
                    ContentBlock::ToolResult {
                        tool_use_id: "call_1".into(),
                        content: "second".into(),
                        is_error: false,
                        extra_content: vec![],
                    },
                ],
                is_meta: true,
                is_compact_summary: false,
            }),
        ];
        ensure_tool_result_pairing(&mut messages);
        let all_results: Vec<_> = messages.iter().flat_map(result_content).collect();
        assert_eq!(all_results, vec![("call_1", "first")]);
    }

    #[test]
    fn sanitize_null_input_becomes_empty_object() {
        let mut messages = vec![assistant_with_use("call_1", serde_json::Value::Null)];
        sanitize_tool_use_input(&mut messages);
        let Message::Assistant(a) = &messages[0] else {
            panic!("expected assistant");
        };
        let ContentBlock::ToolUse { input, .. } = &a.content[0] else {
            panic!("expected tool_use");
        };
        assert_eq!(input, &serde_json::json!({}));
    }

    #[test]
    fn sanitize_stringified_object_is_recovered() {
        let mut messages = vec![assistant_with_use(
            "call_1",
            serde_json::Value::String(r#"{"command":"ls"}"#.into()),
        )];
        sanitize_tool_use_input(&mut messages);
        let Message::Assistant(a) = &messages[0] else {
            panic!("expected assistant");
        };
        let ContentBlock::ToolUse { input, .. } = &a.content[0] else {
            panic!("expected tool_use");
        };
        assert_eq!(input, &serde_json::json!({"command": "ls"}));
    }

    #[test]
    fn sanitize_non_object_values_become_empty_object() {
        for bad in [
            serde_json::json!("not json at all"),
            serde_json::json!([1, 2, 3]),
            serde_json::json!(42),
            serde_json::json!("[1,2]"), // parses, but not to an object
        ] {
            let mut messages = vec![assistant_with_use("call_1", bad)];
            sanitize_tool_use_input(&mut messages);
            let Message::Assistant(a) = &messages[0] else {
                panic!("expected assistant");
            };
            let ContentBlock::ToolUse { input, .. } = &a.content[0] else {
                panic!("expected tool_use");
            };
            assert_eq!(input, &serde_json::json!({}));
        }
    }

    #[test]
    fn well_formed_history_passes_through_unchanged() {
        let original = vec![
            user_message("run ls"),
            assistant_with_use("call_1", serde_json::json!({"command": "ls"})),
            tool_result_message("call_1", "file.txt", false),
            Message::Assistant(AssistantMessage {
                uuid: Uuid::new_v4(),
                timestamp: String::new(),
                content: vec![ContentBlock::Text {
                    text: "done".into(),
                }],
                model: None,
                usage: None,
                stop_reason: None,
                request_id: None,
            }),
        ];
        let mut messages = original.clone();
        sanitize_tool_use_input(&mut messages);
        ensure_tool_result_pairing(&mut messages);
        assert_eq!(messages.len(), original.len());
        assert_eq!(
            serde_json::to_string(&messages).unwrap(),
            serde_json::to_string(&original).unwrap(),
            "valid history must not be altered"
        );
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
}
