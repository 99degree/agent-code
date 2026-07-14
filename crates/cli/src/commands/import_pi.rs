//! Import pi.dev JSONL session files into agent-code format.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use agent_code_lib::llm::message::{
    AssistantMessage, ContentBlock, Message, StopReason, Usage, UserMessage,
};
use agent_code_lib::services::session::SessionData;

/// Execute `/import-pi <path>` — import a pi.dev JSONL session file.
pub fn execute(args: Option<&str>, engine: &mut agent_code_lib::query::QueryEngine) -> String {
    let Some(path) = args else {
        return "Usage: /import-pi <path-to-pi-session.jsonl>".into();
    };
    let path = path.trim();
    if path.is_empty() {
        return "Usage: /import-pi <path-to-pi-session.jsonl>".into();
    }

    let pi_path = PathBuf::from(path);
    if !pi_path.exists() {
        return format!("File not found: {path}");
    }

    match import_pi_session(&pi_path) {
        Ok(session_id) => {
            let cwd = engine.state().cwd.clone();
            format!("Imported pi.dev session as agent-code session: {session_id}\nResume with: /session {session_id}")
        }
        Err(e) => format!("Import failed: {e}"),
    }
}

/// Import a pi.dev JSONL session file and save as agent-code session.
fn import_pi_session(pi_path: &Path) -> Result<String, String> {
    let content = std::fs::read_to_string(pi_path)
        .map_err(|e| format!("Failed to read file: {e}"))?;

    let mut session_meta: Option<PiSessionMeta> = None;
    let mut model_name = String::from("unknown");
    let mut messages: Vec<Message> = Vec::new();
    // Track tool calls by ID so we can match tool results.
    let mut tool_calls: HashMap<String, String> = HashMap::new();

    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        let entry: PiEntry = serde_json::from_str(line)
            .map_err(|e| format!("Failed to parse JSONL line: {e}\nLine: {line}"))?;

        match entry {
            PiEntry::Session(meta) => {
                session_meta = Some(meta);
            }
            PiEntry::ModelChange(mc) => {
                model_name = mc.model_id.unwrap_or_default();
            }
            PiEntry::Message(msg) => {
                if let Some(message) = convert_message(&msg, &mut tool_calls) {
                    messages.push(message);
                }
            }
            _ => {} // Ignore thinking_level_change and other types
        }
    }

    if messages.is_empty() {
        return Err("No messages found in pi.dev session".into());
    }

    let meta = session_meta.ok_or("No session metadata found")?;
    let session_id = format!("pi-{}", &meta.id[..8]);
    let cwd = meta.cwd.unwrap_or_else(|| ".".into());

    let session = SessionData {
        id: session_id.clone(),
        created_at: meta.timestamp,
        updated_at: chrono::Utc::now().to_rfc3339(),
        cwd,
        model: model_name,
        messages,
        turn_count: 0,
        total_cost_usd: 0.0,
        total_input_tokens: 0,
        total_output_tokens: 0,
        plan_mode: false,
        label: Some(format!("Imported from pi.dev")),
        tags: vec!["imported".into(), "pi".into()],
    };

    // Save using agent-code's session persistence.
    let json = serde_json::to_string_pretty(&session)
        .map_err(|e| format!("Failed to serialize session: {e}"))?;

    let sessions_dir = agent_code_lib::config::agent_config_dir()
        .ok_or("Cannot determine config directory")?
        .join("sessions");

    std::fs::create_dir_all(&sessions_dir)
        .map_err(|e| format!("Failed to create sessions directory: {e}"))?;

    let session_path = sessions_dir.join(format!("{session_id}.json"));
    std::fs::write(&session_path, &json)
        .map_err(|e| format!("Failed to write session file: {e}"))?;

    Ok(session_id)
}

/// pi.dev session metadata.
#[derive(serde::Deserialize)]
struct PiSessionMeta {
    #[serde(rename = "id")]
    id: String,
    #[serde(rename = "timestamp")]
    timestamp: String,
    #[serde(rename = "cwd")]
    cwd: Option<String>,
}

/// pi.dev model change event.
#[derive(serde::Deserialize)]
struct PiModelChange {
    #[serde(rename = "modelId")]
    model_id: Option<String>,
}

/// Top-level pi.dev JSONL entry.
#[derive(serde::Deserialize)]
#[serde(tag = "type")]
enum PiEntry {
    #[serde(rename = "session")]
    Session(PiSessionMeta),
    #[serde(rename = "model_change")]
    ModelChange(PiModelChange),
    #[serde(rename = "message")]
    Message(PiMessage),
    #[serde(rename = "thinking_level_change")]
    ThinkingLevelChange(serde_json::Value),
}

/// pi.dev message entry.
#[derive(serde::Deserialize)]
struct PiMessage {
    #[serde(rename = "id")]
    id: String,
    #[serde(rename = "message")]
    message: PiInnerMessage,
}

/// Inner message structure.
#[derive(serde::Deserialize)]
struct PiInnerMessage {
    #[serde(rename = "role")]
    role: String,
    #[serde(rename = "content")]
    content: Vec<PiContentBlock>,
    #[serde(rename = "model", default)]
    model: Option<String>,
    #[serde(rename = "usage", default)]
    usage: Option<PiUsage>,
    #[serde(rename = "stopReason", default)]
    stop_reason: Option<String>,
    #[serde(rename = "toolCallId", default)]
    tool_call_id: Option<String>,
    #[serde(rename = "toolName", default)]
    tool_name: Option<String>,
    #[serde(rename = "isError", default)]
    is_error: Option<bool>,
}

/// pi.dev content block.
#[derive(serde::Deserialize)]
#[serde(tag = "type")]
enum PiContentBlock {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "thinking")]
    Thinking {
        thinking: String,
        #[serde(rename = "thinkingSignature", default)]
        signature: Option<String>,
    },
    #[serde(rename = "toolCall")]
    ToolCall {
        id: String,
        name: String,
        arguments: serde_json::Value,
    },
}

/// pi.dev usage stats.
#[derive(serde::Deserialize)]
struct PiUsage {
    #[serde(rename = "input", default)]
    input: u64,
    #[serde(rename = "output", default)]
    output: u64,
    #[serde(rename = "cacheRead", default)]
    cache_read: u64,
    #[serde(rename = "cacheWrite", default)]
    cache_write: u64,
}

/// Convert a pi.dev message to agent-code Message.
fn convert_message(
    msg: &PiMessage,
    tool_calls: &mut HashMap<String, String>,
) -> Option<Message> {
    let inner = &msg.message;
    let timestamp = inner
        .model
        .as_ref()
        .map(|_| chrono::Utc::now().to_rfc3339())
        .unwrap_or_else(|| chrono::Utc::now().to_rfc3339());

    match inner.role.as_str() {
        "user" => {
            let content: Vec<ContentBlock> = inner
                .content
                .iter()
                .filter_map(|cb| match cb {
                    PiContentBlock::Text { text } => Some(ContentBlock::Text { text: text.clone() }),
                    _ => None,
                })
                .collect();

            if content.is_empty() {
                return None;
            }

            Some(Message::User(UserMessage {
                uuid: uuid::Uuid::new_v4(),
                timestamp,
                content,
                is_meta: false,
                is_compact_summary: false,
            }))
        }
        "assistant" => {
            let mut content: Vec<ContentBlock> = Vec::new();

            for cb in &inner.content {
                match cb {
                    PiContentBlock::Text { text } => {
                        content.push(ContentBlock::Text { text: text.clone() });
                    }
                    PiContentBlock::Thinking { thinking, signature } => {
                        content.push(ContentBlock::Thinking {
                            thinking: thinking.clone(),
                            signature: signature.clone(),
                        });
                    }
                    PiContentBlock::ToolCall { id, name, arguments } => {
                        tool_calls.insert(id.clone(), name.clone());
                        content.push(ContentBlock::ToolUse {
                            id: id.clone(),
                            name: name.clone(),
                            input: arguments.clone(),
                        });
                    }
                }
            }

            if content.is_empty() {
                return None;
            }

            let usage = inner.usage.as_ref().map(|u| Usage {
                input_tokens: u.input,
                output_tokens: u.output,
                cache_creation_input_tokens: u.cache_write,
                cache_read_input_tokens: u.cache_read,
            });

            let stop_reason = inner.stop_reason.as_deref().map(|sr| match sr {
                "toolUse" => StopReason::ToolUse,
                "endTurn" => StopReason::EndTurn,
                "maxTokens" => StopReason::MaxTokens,
                _ => StopReason::EndTurn,
            });

            Some(Message::Assistant(AssistantMessage {
                uuid: uuid::Uuid::new_v4(),
                timestamp,
                content,
                model: inner.model.clone(),
                usage,
                stop_reason,
                request_id: None,
            }))
        }
        "toolResult" => {
            let tool_call_id = inner.tool_call_id.clone()?;
            let text: String = inner
                .content
                .iter()
                .filter_map(|cb| match cb {
                    PiContentBlock::Text { text } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n");

            let is_error = inner.is_error.unwrap_or(false);

            // Tool results in pi.dev are separate messages, but in agent-code
            // they're content blocks within user messages. We wrap them as
            // user messages with meta=true.
            Some(Message::User(UserMessage {
                uuid: uuid::Uuid::new_v4(),
                timestamp,
                content: vec![ContentBlock::ToolResult {
                    tool_use_id: tool_call_id,
                    content: text,
                    is_error,
                    extra_content: vec![],
                }],
                is_meta: true,
                is_compact_summary: false,
            }))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_pi_session_line() {
        let line = r#"{"type":"session","version":3,"id":"test1234-5678","timestamp":"2026-01-01T00:00:00.000Z","cwd":"/home/test"}"#;
        let entry: PiEntry = serde_json::from_str(line).unwrap();
        match entry {
            PiEntry::Session(meta) => {
                assert_eq!(meta.id, "test1234-5678");
                assert_eq!(meta.cwd, Some("/home/test".into()));
            }
            _ => panic!("Expected Session entry"),
        }
    }

    #[test]
    fn parse_pi_message_line() {
        let line = r#"{"type":"message","id":"msg1","parentId":null,"timestamp":"2026-01-01T00:00:01.000Z","message":{"role":"user","content":[{"type":"text","text":"hello"}]}}"#;
        let entry: PiEntry = serde_json::from_str(line).unwrap();
        match entry {
            PiEntry::Message(msg) => {
                assert_eq!(msg.message.role, "user");
                assert_eq!(msg.message.content.len(), 1);
            }
            _ => panic!("Expected Message entry"),
        }
    }
}
