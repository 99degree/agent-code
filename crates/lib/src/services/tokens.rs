//! Token estimation.
//!
//! Estimates token counts for messages and content blocks using a
//! character-based heuristic. Uses actual API usage data when
//! available, falling back to rough estimation for new messages.
//!
//! Default ratio: 4 bytes per token (conservative for most content).

use crate::llm::message::{ContentBlock, Message};

/// Default bytes per token for estimation.
const BYTES_PER_TOKEN: f64 = 4.0;

/// Fixed token estimate for image content blocks.
const IMAGE_TOKEN_ESTIMATE: u64 = 2000;

/// Estimate token count from a string.
pub fn estimate_tokens(content: &str) -> u64 {
    (content.len() as f64 / BYTES_PER_TOKEN).round() as u64
}

/// Estimate tokens for a single content block.
pub fn estimate_block_tokens(block: &ContentBlock) -> u64 {
    match block {
        ContentBlock::Text { text } => estimate_tokens(text),
        ContentBlock::ToolUse { name, input, .. } => {
            let input_str = serde_json::to_string(input).unwrap_or_default();
            estimate_tokens(name) + estimate_tokens(&input_str)
        }
        ContentBlock::ToolResult { content, .. } => estimate_tokens(content),
        ContentBlock::Thinking { thinking, .. } => estimate_tokens(thinking),
        ContentBlock::Image { .. } => IMAGE_TOKEN_ESTIMATE,
        ContentBlock::Document { data, .. } => {
            // Base64-encoded documents: estimate from decoded size.
            let decoded_size = (data.len() as f64 * 0.75) as u64;
            (decoded_size as f64 / BYTES_PER_TOKEN).round() as u64
        }
    }
}

/// Estimate tokens for a single message.
pub fn estimate_message_tokens(msg: &Message) -> u64 {
    match msg {
        Message::User(u) => {
            // Per-message overhead (role, formatting).
            let overhead = 4;
            let content: u64 = u.content.iter().map(estimate_block_tokens).sum();
            overhead + content
        }
        Message::Assistant(a) => {
            let overhead = 4;
            let content: u64 = a.content.iter().map(estimate_block_tokens).sum();
            overhead + content
        }
        Message::System(s) => {
            let overhead = 4;
            overhead + estimate_tokens(&s.content)
        }
    }
}

/// Estimate total context tokens for a message history.
///
/// Uses a hybrid approach: actual API usage counts for the most
/// recent assistant response, plus rough estimation for any
/// messages added after that point.
pub fn estimate_context_tokens(messages: &[Message]) -> u64 {
    if messages.is_empty() {
        return 0;
    }

    // Find the most recent assistant message with usage data.
    let mut last_usage_idx = None;
    for (i, msg) in messages.iter().enumerate().rev() {
        if let Message::Assistant(a) = msg
            && a.usage.is_some()
        {
            last_usage_idx = Some(i);
            break;
        }
    }

    match last_usage_idx {
        Some(idx) => {
            // Use actual API token count up to this point.
            let usage = messages[idx]
                .as_assistant()
                .and_then(|a| a.usage.as_ref())
                .unwrap();
            let api_tokens = usage.total();

            // Estimate tokens for messages added after the API call.
            let new_tokens: u64 = messages[idx + 1..]
                .iter()
                .map(estimate_message_tokens)
                .sum();

            api_tokens + new_tokens
        }
        None => {
            // No API usage data — estimate everything.
            messages.iter().map(estimate_message_tokens).sum()
        }
    }
}

/// Known model context windows, sourced from provider catalogs (e.g. pi.dev).
///
/// Large-context models (1M+) must be listed explicitly or they collapse to
/// the 128K default and the conversation is compacted prematurely. Keyed by
/// the bare model id (provider prefix stripped, lowercased).
const MODEL_CONTEXT_WINDOWS: &[(&str, u64)] = &[
    // Anthropic / Claude
    ("claude-opus-4", 200_000),
    ("claude-opus-4-1", 200_000),
    ("claude-opus-4-5", 200_000),
    ("claude-opus-4-6", 1_000_000),
    ("claude-opus-4-7", 1_000_000),
    ("claude-opus-4-8", 1_000_000),
    ("claude-opus-5", 1_000_000),
    ("claude-sonnet-4", 200_000),
    ("claude-sonnet-4-5", 1_000_000),
    ("claude-sonnet-4-6", 1_000_000),
    ("claude-sonnet-5", 1_000_000),
    ("claude-haiku-4-5", 200_000),
    ("claude-fable-5", 1_000_000),
    // OpenAI / GPT
    ("gpt-5", 400_000),
    ("gpt-5.1", 400_000),
    ("gpt-5.2", 400_000),
    ("gpt-5.3-codex", 400_000),
    ("gpt-5.4", 1_050_000),
    ("gpt-5.4-mini", 400_000),
    ("gpt-5.4-nano", 400_000),
    ("gpt-5.4-pro", 1_050_000),
    ("gpt-5.5", 1_050_000),
    ("gpt-5.5-pro", 1_050_000),
    ("gpt-5.6-luna", 1_050_000),
    ("gpt-5.6-sol", 1_050_000),
    ("gpt-5.6-terra", 1_050_000),
    ("gpt-4.1", 1_047_576),
    ("gpt-4.1-mini", 1_047_576),
    ("gpt-4.1-nano", 1_047_576),
    ("gpt-4o", 128_000),
    ("gpt-4o-mini", 128_000),
    ("gpt-oss-120b", 131_072),
    ("gpt-oss-20b", 131_072),
    // Zhipu / GLM
    ("glm-5.2", 1_000_000),
    ("glm-5.3", 1_000_000),
    ("glm-5-turbo", 200_000),
    ("glm-5.2-highspeed", 1_000_000),
    ("glm-4.7", 204_800),
    ("glm-4.6", 202_752),
    ("glm-4.5", 131_072),
    // NVIDIA / Nemotron
    ("nemotron-3-ultra-550b-a55b", 1_000_000),
    ("nemotron-3-nano-30b-a3b", 131_072),
    ("nemotron-3-nano-omni-30b-a3b-reasoning", 256_000),
    ("nemotron-3-super-120b-a12b", 262_144),
    ("nemotron-3.5-lightning-30b-a3b", 262_144),
    ("nemotron-nano-12b-v2-vl", 128_000),
    ("nemotron-nano-9b-v2", 131_072),
    ("nemotron-3-ultra-free", 1_000_000),
    ("nemotron-3.5-lightning-free", 1_000_000),
    ("llama-3.3-nemotron-super-49b-v1", 131_072),
    ("llama-3.3-nemotron-super-49b-v1.5", 131_072),
    // DeepSeek
    ("deepseek-v4-pro", 1_000_000),
    ("deepseek-v4-flash", 1_000_000),
    ("deepseek-v4-flash-free", 1_000_000),
    ("deepseek-chat", 128_000),
    ("deepseek-r1", 64_000),
    // Google / Gemini
    ("gemini-3-pro", 1_048_576),
    ("gemini-3.1-pro-preview", 1_048_576),
    ("gemini-3.5-flash", 1_048_576),
    ("gemini-3.7-flash", 1_048_576),
    ("gemini-2.5-pro", 1_048_576),
    ("gemini-2.5-flash", 1_048_576),
    // xAI / Grok
    ("grok-4.5", 500_000),
    ("grok-4.6", 500_000),
    ("grok-4.3", 1_000_000),
    ("grok-4.20", 2_000_000),
    ("grok-build-0.1", 256_000),
    // Mistral
    ("codestral-latest", 256_000),
    ("devstral-latest", 262_144),
    ("mistral-large-latest", 262_144),
    ("mistral-medium-latest", 262_144),
    ("mistral-small-latest", 256_000),
    ("ministral-8b-latest", 128_000),
    ("pixtral-large-latest", 128_000),
    ("voxtral-small-latest", 32_000),
    ("magistral-medium-latest", 128_000),
    // MiniMax
    ("minimax-m3", 524_288),
    // Qwen
    ("qwen3.6-plus", 1_000_000),
    ("qwen3.7-max", 1_000_000),
    ("qwen3.7-plus", 1_000_000),
    ("qwen3.8-max", 1_000_000),
    ("qwen3-max", 262_144),
    ("qwen-plus", 1_000_000),
    // Kimi
    ("kimi-k2.6", 262_144),
    ("kimi-k3", 1_048_576),
    ("kimi-k2.7-code", 262_144),
    // MiMo
    ("mimo-v2.5", 1_000_000),
    ("mimo-v2.5-pro", 1_048_576),
    // Llama
    ("llama-4-maverick", 1_048_576),
    ("llama-4-scout", 327_680),
    // Muse
    ("muse-spark-1.2", 1_048_576),
    ("muse-glimmer-30b", 131_072),
    // Tencent
    ("hy3", 256_000),
    ("hy3-free", 262_144),
    // MiMo (free)
    ("mimo-v2.5-free", 1_000_000),
    // Poolside (free)
    ("laguna-s-2.1-free", 262_144),
    // Muse (free)
    ("muse-spark-1.2-contributor-free", 1_000_000),
    // Kilo auto-routing tiers (aggregate many models)
    ("kilo-auto/frontier", 1_000_000),
    ("kilo-auto/balanced", 1_000_000),
    ("kilo-auto/efficient", 1_000_000),
    ("kilo-auto/free", 256_000),
    // Misc
    ("big-pickle", 200_000),
];

/// Get the context window size for a model.
pub fn context_window_for_model(model: &str) -> u64 {
    let lower = model.to_lowercase();

    // Check for extended context variants first.
    if lower.contains("1m") || lower.contains("1000k") {
        return 1_000_000;
    }

    // Exact (or provider-prefix-stripped) lookup against the known table.
    let bare = lower.rsplit('/').next().unwrap_or(lower.as_str());
    for (id, window) in MODEL_CONTEXT_WINDOWS {
        if *id == lower || *id == bare {
            return *window;
        }
    }

    // Heuristic fallback for unknown models.
    if lower.contains("opus") || lower.contains("sonnet") || lower.contains("haiku") {
        200_000
    } else if lower.contains("nemotron") {
        // NVIDIA Nemotron `-free` variants expose a 1M context window;
        // treat the family as 1M to avoid premature compaction.
        1_000_000
    } else if lower.contains("gpt-5") {
        400_000
    } else if lower.contains("gpt-4") {
        128_000
    } else if lower.contains("gpt-3.5") {
        16_384
    } else {
        128_000
    }
}

/// Get the max output tokens for a model.
pub fn max_output_tokens_for_model(model: &str) -> u64 {
    let lower = model.to_lowercase();
    if lower.contains("opus") || lower.contains("sonnet") {
        16_384
    } else if lower.contains("haiku") {
        8_192
    } else {
        16_384
    }
}

/// Get the maximum thinking token budget for a model.
pub fn max_thinking_tokens_for_model(model: &str) -> u64 {
    let lower = model.to_lowercase();
    if lower.contains("opus") {
        32_000
    } else if lower.contains("sonnet") {
        16_000
    } else if lower.contains("haiku") {
        8_000
    } else {
        16_000
    }
}

// Helper to extract assistant message ref.
trait AsAssistant {
    fn as_assistant(&self) -> Option<&crate::llm::message::AssistantMessage>;
}

impl AsAssistant for Message {
    fn as_assistant(&self) -> Option<&crate::llm::message::AssistantMessage> {
        match self {
            Message::Assistant(a) => Some(a),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_estimate_tokens() {
        // 100 chars / 4 = 25 tokens.
        let text = "a".repeat(100);
        assert_eq!(estimate_tokens(&text), 25);
    }

    #[test]
    fn test_empty_messages() {
        assert_eq!(estimate_context_tokens(&[]), 0);
    }

    #[test]
    fn test_estimate_block_tokens_text() {
        let block = ContentBlock::Text {
            text: "a".repeat(400),
        };
        assert_eq!(estimate_block_tokens(&block), 100);
    }

    #[test]
    fn test_estimate_block_tokens_image() {
        let block = ContentBlock::Image {
            media_type: "image/png".into(),
            data: "base64data".into(),
        };
        assert_eq!(estimate_block_tokens(&block), IMAGE_TOKEN_ESTIMATE);
    }

    #[test]
    fn test_estimate_block_tokens_tool_use() {
        let block = ContentBlock::ToolUse {
            id: "call_1".into(),
            name: "Bash".into(),
            input: serde_json::json!({"command": "ls"}),
        };
        let tokens = estimate_block_tokens(&block);
        assert!(tokens > 0);
    }

    #[test]
    fn test_estimate_message_tokens() {
        let msg = crate::llm::message::user_message("hello world");
        let tokens = estimate_message_tokens(&msg);
        // 11 chars / 4 = ~3, + 4 overhead = ~7
        assert!(tokens >= 5);
    }

    #[test]
    fn test_context_window_for_model() {
        assert_eq!(context_window_for_model("claude-opus-4"), 200_000);
        assert_eq!(context_window_for_model("claude-sonnet-4"), 200_000);
        assert_eq!(context_window_for_model("gpt-4"), 128_000);
        assert_eq!(context_window_for_model("claude-sonnet-1m"), 1_000_000);
    }

    #[test]
    fn test_max_output_tokens() {
        assert_eq!(max_output_tokens_for_model("claude-opus"), 16_384);
        assert_eq!(max_output_tokens_for_model("claude-haiku"), 8_192);
    }

    #[test]
    fn test_max_thinking_tokens() {
        assert_eq!(max_thinking_tokens_for_model("claude-opus"), 32_000);
        assert_eq!(max_thinking_tokens_for_model("claude-sonnet"), 16_000);
        assert_eq!(max_thinking_tokens_for_model("claude-haiku"), 8_000);
    }

    #[test]
    fn test_estimate_tokens_empty_string() {
        assert_eq!(estimate_tokens(""), 0);
    }

    #[test]
    fn test_estimate_tokens_unicode() {
        // Multi-byte chars: each char may be 2-4 bytes in UTF-8.
        let text = "\u{1F600}\u{1F600}\u{1F600}"; // 3 emoji, 4 bytes each = 12 bytes
        let tokens = estimate_tokens(text);
        // 12 / 4 = 3
        assert_eq!(tokens, 3);
    }

    #[test]
    fn test_estimate_block_tokens_document() {
        let block = ContentBlock::Document {
            media_type: "application/pdf".into(),
            data: "a".repeat(400), // 400 base64 chars -> ~300 decoded bytes -> 300/4 = 75 tokens
            title: Some("test.pdf".into()),
        };
        let tokens = estimate_block_tokens(&block);
        assert!(tokens > 0);
        assert_eq!(tokens, 75);
    }

    #[test]
    fn test_estimate_block_tokens_thinking() {
        let block = ContentBlock::Thinking {
            thinking: "a".repeat(200),
            signature: Some("sig".into()),
        };
        let tokens = estimate_block_tokens(&block);
        // 200 / 4 = 50
        assert_eq!(tokens, 50);
    }

    #[test]
    fn test_estimate_block_tokens_tool_result() {
        let block = ContentBlock::ToolResult {
            tool_use_id: "call_1".into(),
            content: "a".repeat(80),
            is_error: false,
            extra_content: vec![],
        };
        let tokens = estimate_block_tokens(&block);
        // 80 / 4 = 20
        assert_eq!(tokens, 20);
    }

    #[test]
    fn test_estimate_message_tokens_system() {
        let msg = Message::System(crate::llm::message::SystemMessage {
            uuid: uuid::Uuid::new_v4(),
            timestamp: String::new(),
            subtype: crate::llm::message::SystemMessageType::Informational,
            content: "a".repeat(40),
            level: crate::llm::message::MessageLevel::Info,
        });
        let tokens = estimate_message_tokens(&msg);
        // 40/4 = 10 + 4 overhead = 14
        assert_eq!(tokens, 14);
    }

    #[test]
    fn test_estimate_message_tokens_assistant_with_tool_use() {
        let msg = Message::Assistant(crate::llm::message::AssistantMessage {
            uuid: uuid::Uuid::new_v4(),
            timestamp: String::new(),
            content: vec![
                ContentBlock::Text {
                    text: "Let me run that.".into(),
                },
                ContentBlock::ToolUse {
                    id: "call_1".into(),
                    name: "Bash".into(),
                    input: serde_json::json!({"command": "ls"}),
                },
            ],
            model: None,
            usage: None,
            stop_reason: None,
            request_id: None,
        });
        let tokens = estimate_message_tokens(&msg);
        // Should include overhead + text tokens + tool_use tokens
        assert!(tokens > 4);
    }

    #[test]
    fn test_estimate_context_tokens_only_user_messages() {
        let messages = vec![
            crate::llm::message::user_message("hello world"),
            crate::llm::message::user_message("how are you"),
        ];
        let tokens = estimate_context_tokens(&messages);
        // No usage data, so everything is estimated.
        assert!(tokens > 0);
    }

    #[test]
    fn test_context_window_for_gpt35() {
        assert_eq!(context_window_for_model("gpt-3.5-turbo"), 16_384);
    }

    #[test]
    fn test_context_window_for_unknown_model() {
        // Unknown models default to 128K.
        assert_eq!(context_window_for_model("some-unknown-model"), 128_000);
    }

    #[test]
    fn test_context_window_for_nemotron_models() {
        // The Nemotron `-free` variants expose a 1M context window.
        assert_eq!(context_window_for_model("nemotron-3-ultra-free"), 1_000_000);
        assert_eq!(
            context_window_for_model("nemotron-3.5-lightning-free"),
            1_000_000
        );
        // The catalog value for the 30B variant is respected (131072),
        // not the blanket 1M family heuristic.
        assert_eq!(
            context_window_for_model("nvidia/nemotron-3-nano-30b-a3b"),
            131_072
        );
    }

    #[test]
    fn test_context_window_for_1000k_variant() {
        assert_eq!(context_window_for_model("claude-sonnet-1000k"), 1_000_000);
    }

    #[test]
    fn test_context_window_for_explicit_table_models() {
        // Explicit table entries override the family heuristic.
        assert_eq!(
            context_window_for_model("nvidia/nemotron-3-nano-30b-a3b"),
            131_072
        );
        assert_eq!(context_window_for_model("gpt-4.1"), 1_047_576);
        assert_eq!(context_window_for_model("glm-5.2"), 1_000_000);
        assert_eq!(context_window_for_model("grok-4.5"), 500_000);
        assert_eq!(context_window_for_model("mistral-large-latest"), 262_144);
        // Provider prefix is stripped for the lookup.
        assert_eq!(context_window_for_model("openrouter/auto"), 128_000);
        // Unknown models still default to 128K.
        assert_eq!(context_window_for_model("some-unknown-model"), 128_000);
    }

    #[test]
    fn test_max_output_tokens_for_unknown_model() {
        // Unknown models default to 16384.
        assert_eq!(max_output_tokens_for_model("unknown-llm"), 16_384);
    }

    #[test]
    fn test_max_thinking_tokens_for_unknown_model() {
        // Unknown models default to 16000.
        assert_eq!(max_thinking_tokens_for_model("unknown-llm"), 16_000);
    }
}
