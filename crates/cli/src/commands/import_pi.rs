//! Import pi.dev JSONL session files into agent-code format.
//!
//! Pi.dev session entry types:
//! - `session` - Session header (id, timestamp, cwd, version)
//! - `message` - User/assistant/toolResult/custom messages
//! - `thinking_level_change` - Thinking level changes
//! - `model_change` - Model changes (provider, modelId)
//! - `compaction` - Compaction summaries (summary, firstKeptEntryId, tokensBefore)
//! - `custom` - Extension entries (not in LLM context)
//! - `session_info` - Session display name
//! - `custom_message` - Extension messages (in LLM context)
//! - `label` - Bookmarks on entries
//! - `branch_summary` - Summary of abandoned paths

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use agent_code_lib::llm::message::{
    AssistantMessage, ContentBlock, Message, StopReason, SystemMessage, SystemMessageType,
    Usage, UserMessage,
};
use agent_code_lib::services::session::SessionData;

/// Execute `/import-pi [path]` — list or import pi.dev JSONL session files.
pub fn execute(args: Option<&str>, engine: &mut agent_code_lib::query::QueryEngine) -> String {
    let path = args.map(|a| a.trim()).unwrap_or("");

    if path.is_empty() {
        return list_sessions_for_cwd(&engine.state().cwd);
    }

    // Expand ~ to home directory.
    let expanded = if path.starts_with('~') {
        if let Some(home) = std::env::var_os("HOME") {
            format!("{}{}", home.to_string_lossy(), &path[1..])
        } else {
            path.to_string()
        }
    } else {
        path.to_string()
    };

    // If the argument is a number, treat it as an index from the list.
    if let Ok(index) = expanded.parse::<usize>() {
        return import_by_index(index, &engine.state().cwd);
    }

    // Try the path as-is first.
    let mut pi_path = PathBuf::from(&expanded);
    if !pi_path.exists() {
        // Try searching in ~/.pi/agent/sessions/ for a matching file.
        if let Some(sessions_base) = get_pi_sessions_dir() {
            let search_name = Path::new(&expanded)
                .file_name()
                .map(|f| f.to_string_lossy().to_string());
            if let Some(name) = search_name {
                if let Ok(entries) = std::fs::read_dir(&sessions_base) {
                    for entry in entries.filter_map(|e| e.ok()) {
                        let dir = entry.path();
                        if dir.is_dir() {
                            let candidate = dir.join(&name);
                            if candidate.exists() {
                                pi_path = candidate;
                                break;
                            }
                        }
                    }
                }
            }
        }
    }

    if !pi_path.exists() {
        return format!("File not found: {expanded}");
    }

    match import_pi_session(&pi_path) {
        Ok(result) => result,
        Err(e) => format!("Import failed: {e}"),
    }
}

/// Import a session by index from the list.
fn import_by_index(index: usize, cwd: &str) -> String {
    let Some(sessions_base) = get_pi_sessions_dir() else {
        return "Cannot determine pi.dev sessions directory".into();
    };

    let dir_pattern = cwd_to_pi_dir_name(cwd);
    let session_dir = sessions_base.join(&dir_pattern);

    if !session_dir.exists() {
        return "No pi.dev sessions found for this directory.".into();
    }

    let sessions = get_session_files(&session_dir);
    if sessions.is_empty() {
        return "No pi.dev sessions found for this directory.".into();
    }

    if index == 0 || index > sessions.len() {
        return format!("Invalid index: {index}. Use 1-{}.", sessions.len());
    }

    let (file_name, _) = &sessions[index - 1];
    let full_path = session_dir.join(file_name);

    match import_pi_session(&full_path) {
        Ok(result) => result,
        Err(e) => format!("Import failed: {e}"),
    }
}

/// Convert a directory path to pi.dev session folder name.
fn cwd_to_pi_dir_name(cwd: &str) -> String {
    // Trim leading/trailing slashes to avoid extra dashes in the pattern.
    let dir_name = cwd.trim_matches('/').replace('/', "-");
    format!("--{dir_name}--")
}

/// Get the pi.dev sessions directory.
fn get_pi_sessions_dir() -> Option<PathBuf> {
    agent_code_lib::config::agent_config_dir().map(|d| {
        std::path::PathBuf::from(d.to_string_lossy().replace(".config/agent-code", ".pi/agent"))
            .join("sessions")
    })
}

/// Get sorted list of .jsonl session files with optional labels.
fn get_session_files(dir: &Path) -> Vec<(String, Option<String>)> {
    let mut entries: Vec<(String, Option<String>)> = std::fs::read_dir(dir)
        .ok()
        .map(|dirs| {
            dirs.filter_map(|e| e.ok())
                .filter(|e| {
                    e.path()
                        .extension()
                        .and_then(|ext| ext.to_str())
                        == Some("jsonl")
                })
                .filter_map(|e| {
                    let name = e.file_name().to_str()?.to_string();
                    let path = e.path();
                    let label = extract_session_label(&path);
                    Some((name, label))
                })
                .collect()
        })
        .unwrap_or_default();
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    entries
}

/// Extract session label/name from a pi.dev JSONL file (first line only).
fn extract_session_label(path: &Path) -> Option<String> {
    use std::io::{BufRead, BufReader};
    let file = std::fs::File::open(path).ok()?;
    let mut reader = BufReader::new(file);
    let mut line = String::new();
    reader.read_line(&mut line).ok()?;
    let entry: PiEntry = serde_json::from_str(&line).ok()?;
    match entry {
        PiEntry::Session(meta) => meta.label,
        _ => None,
    }
}

/// Format a file's age as a compact string (e.g., "3d", "2h", "1w 2d").
fn file_age(path: &Path) -> String {
    let Ok(metadata) = path.metadata() else {
        return "?".into();
    };
    let Ok(modified) = metadata.modified() else {
        return "?".into();
    };
    let Ok(elapsed) = modified.elapsed() else {
        return "?".into();
    };

    let secs = elapsed.as_secs();
    let years = secs / 31536000;
    let months = (secs % 31536000) / 2592000;
    let weeks = (secs % 2592000) / 604800;
    let days = (secs % 604800) / 86400;
    let hours = (secs % 86400) / 3600;
    let mins = (secs % 3600) / 60;

    // Only show the largest unit for compactness
    if years > 0 {
        format!("{years}y")
    } else if months > 0 {
        format!("{months}mo")
    } else if weeks > 0 {
        format!("{weeks}w")
    } else if days > 0 {
        format!("{days}d")
    } else if hours > 0 {
        format!("{hours}h")
    } else if mins > 0 {
        format!("{mins}m")
    } else {
        "now".into()
    }
}

/// Scanned summary metadata for a pi.dev JSONL file.
struct ScanMeta {
    model: Option<String>,
    message_count: usize,
    label: Option<String>,
}

/// Lightweight scan of a pi.dev JSONL file for model + message count + label.
fn scan_session_meta(path: &Path) -> ScanMeta {
    use std::io::{BufRead, BufReader};
    let mut model: Option<String> = None;
    let mut message_count = 0usize;
    let mut label: Option<String> = None;
    if let Ok(file) = std::fs::File::open(path) {
        let reader = BufReader::new(file);
        for line in reader.lines().flatten() {
            let entry: PiEntry = match serde_json::from_str(&line) {
                Ok(e) => e,
                Err(_) => continue,
            };
            match entry {
                PiEntry::ModelChange(mc) => {
                    if let Some(m) = mc.model_id {
                        model = Some(m);
                    }
                }
                PiEntry::Message(_) => message_count += 1,
                PiEntry::Session(meta) => {
                    label = meta.label;
                }
                PiEntry::SessionInfo(info) => {
                    if label.is_none() {
                        label = Some(info.name);
                    }
                }
                _ => {}
            }
        }
    }
    ScanMeta { model, message_count, label }
}

/// List pi.dev sessions across all directories so the user can choose.
fn list_sessions_for_cwd(cwd: &str) -> String {
    let Some(sessions_base) = get_pi_sessions_dir() else {
        return "Cannot determine pi.dev sessions directory".into();
    };

    if !sessions_base.exists() {
        return format!(
            "No pi.dev sessions directory found at: {}",
            sessions_base.display()
        );
    }

    // Collect sessions from every project folder, not just the cwd-matched one.
    let dir_pattern = cwd_to_pi_dir_name(cwd);
    let mut all: Vec<(String, Option<String>, PathBuf, String)> = Vec::new();

    if let Ok(projects) = std::fs::read_dir(&sessions_base) {
        for project in projects.filter_map(|e| e.ok()) {
            let proj_dir = project.path();
            if !proj_dir.is_dir() {
                continue;
            }
            for (name, label) in get_session_files(&proj_dir) {
                let full = proj_dir.join(&name);
                let disp = name.strip_suffix(".jsonl").unwrap_or(&name).to_string();
                all.push((disp, label, full, proj_dir.file_name().unwrap_or_default().to_string_lossy().to_string()));
            }
        }
    }

    if all.is_empty() {
        return "No pi.dev sessions found.".into();
    }

    // Sort by most recently modified (newest first).
    all.sort_by(|a, b| {
        let ma = a.2.metadata().and_then(|m| m.modified()).ok();
        let mb = b.2.metadata().and_then(|m| m.modified()).ok();
        mb.cmp(&ma)
    });

    // Filter out sessions with < 100 messages.
    all.retain(|(_, _, path, _)| {
        scan_session_meta(path).message_count >= 100
    });

    if all.is_empty() {
        return "No pi.dev sessions with 100+ messages found.".into();
    }

    let mut out = String::from("pi.dev sessions (newest first, 100+ msgs):\n\n");
    for (i, (name, label, path, proj)) in all.iter().enumerate() {
        let age = file_age(path);
        let meta = scan_session_meta(path);
        let model_str = meta.model.as_deref().map(|m| format!(" · {m}")).unwrap_or_default();
        let msgs = meta.message_count;
        let meta_label = meta.label.as_deref().map(|l| format!(" \"{l}\"")).unwrap_or_default();
        let label_str = label.as_deref().map(|l| format!(" \"{l}\"")).unwrap_or_default();
        // Highlight sessions matching the current directory.
        let marker = if *proj == dir_pattern { " *" } else { "" };
        out.push_str(&format!(
            "  {}. {}{label_str}{meta_label}{model_str} · {} msgs · {} · {}{}\n",
            i + 1,
            name,
            msgs,
            age,
            proj.replace("--", ""),
            marker
        ));
    }
    out.push_str("\n* = matches current directory\n");
    out.push_str("Use: /import-pi <number> or /import-pi <full-path>\n");
    out
}

/// Import a pi.dev JSONL session file and save as agent-code session.
fn import_pi_session(pi_path: &Path) -> Result<String, String> {
    let content = std::fs::read_to_string(pi_path)
        .map_err(|e| format!("Failed to read file: {e}"))?;

    let mut session_meta: Option<PiSessionMeta> = None;
    let mut model_name = String::from("unknown");
    let mut messages: Vec<Message> = Vec::new();
    let mut _tool_calls: HashMap<String, String> = HashMap::new();

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
                if let Some(message) = convert_message(&msg, &mut _tool_calls) {
                    messages.push(message);
                }
            }
            PiEntry::Compaction(comp) => {
                if let Some(summary) = comp.summary {
                    let tokens_info = comp
                        .tokens_before
                        .map(|t| format!(" (from {}k tokens)", t / 1000))
                        .unwrap_or_default();
                    messages.push(Message::System(SystemMessage {
                        uuid: uuid::Uuid::new_v4(),
                        timestamp: chrono::Utc::now().to_rfc3339(),
                        subtype: SystemMessageType::CompactBoundary,
                        content: format!("[Compacted{tokens_info}]: {summary}"),
                        level: agent_code_lib::llm::message::MessageLevel::Info,
                    }));
                }
            }
            PiEntry::BranchSummary(bs) => {
                if let Some(summary) = bs.summary {
                    messages.push(Message::System(SystemMessage {
                        uuid: uuid::Uuid::new_v4(),
                        timestamp: chrono::Utc::now().to_rfc3339(),
                        subtype: SystemMessageType::Informational,
                        content: format!("[Branch summary]: {summary}"),
                        level: agent_code_lib::llm::message::MessageLevel::Info,
                    }));
                }
            }
            PiEntry::SessionInfo(info) => {
                // Use session_info name as label if no label set.
                if session_meta.as_ref().and_then(|m| m.label.is_none().then(|| &m.id)).is_some() {
                    if let Some(ref mut meta) = session_meta {
                        meta.label = Some(info.name);
                    }
                }
            }
            PiEntry::ThinkingLevelChange(_)
            | PiEntry::Custom(_)
            | PiEntry::CustomMessage(_)
            | PiEntry::Label(_) => {
                // Ignored for import.
            }
        }
    }

    if messages.is_empty() {
        return Err("No messages found in pi.dev session".into());
    }

    let meta = session_meta.ok_or("No session metadata found")?;
    let session_id = format!("pi-{}", &meta.id[..8.min(meta.id.len())]);
    let cwd = meta.cwd.unwrap_or_else(|| ".".into());

    let session = SessionData {
        id: session_id.clone(),
        created_at: meta.timestamp,
        updated_at: chrono::Utc::now().to_rfc3339(),
        cwd,
        model: model_name,
        base_url: String::new(),
        messages,
        turn_count: 0,
        total_cost_usd: 0.0,
        total_input_tokens: 0,
        total_output_tokens: 0,
        plan_mode: false,
        brief_mode: false,
        response_style: String::new(),
        label: meta.label.or_else(|| Some("Imported from pi.dev".into())),
        tags: vec!["imported".into(), "pi".into()],
    };

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

    let label = session
        .label
        .as_deref()
        .map(|l| format!(" \"{l}\""))
        .unwrap_or_default();
    Ok(format!(
        "Imported pi.dev session{label} as: {session_id}\nResume with: /session {session_id}"
    ))
}

// ============================================================================
// Pi.dev entry types
// ============================================================================

#[derive(serde::Deserialize)]
struct PiSessionMeta {
    #[serde(rename = "id")]
    id: String,
    #[serde(rename = "timestamp")]
    timestamp: String,
    #[serde(rename = "cwd")]
    cwd: Option<String>,
    #[serde(rename = "label", default)]
    label: Option<String>,
}

#[derive(serde::Deserialize)]
struct PiModelChange {
    #[serde(rename = "modelId")]
    model_id: Option<String>,
}

#[derive(serde::Deserialize)]
struct PiCompaction {
    #[serde(rename = "summary", default)]
    summary: Option<String>,
    #[serde(rename = "tokensBefore", default)]
    tokens_before: Option<u64>,
}

#[derive(serde::Deserialize)]
struct PiBranchSummary {
    #[serde(rename = "summary", default)]
    summary: Option<String>,
}

#[derive(serde::Deserialize)]
struct PiSessionInfo {
    #[serde(rename = "name", default)]
    name: String,
}

#[derive(serde::Deserialize)]
struct PiCustom {
    #[serde(rename = "customType", default)]
    custom_type: Option<String>,
}

#[derive(serde::Deserialize)]
struct PiCustomMessage {
    #[serde(rename = "customType", default)]
    custom_type: Option<String>,
}

#[derive(serde::Deserialize)]
struct PiLabel {
    #[serde(rename = "targetId", default)]
    target_id: Option<String>,
    #[serde(rename = "label", default)]
    label: Option<String>,
}

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
    #[serde(rename = "compaction")]
    Compaction(PiCompaction),
    #[serde(rename = "branch_summary")]
    BranchSummary(PiBranchSummary),
    #[serde(rename = "session_info")]
    SessionInfo(PiSessionInfo),
    #[serde(rename = "custom")]
    Custom(PiCustom),
    #[serde(rename = "custom_message")]
    CustomMessage(PiCustomMessage),
    #[serde(rename = "label")]
    Label(PiLabel),
}

#[derive(serde::Deserialize)]
struct PiMessage {
    #[serde(rename = "id")]
    _id: String,
    #[serde(rename = "message")]
    message: PiInnerMessage,
}

#[derive(serde::Deserialize)]
struct PiInnerMessage {
    #[serde(rename = "role")]
    role: String,
    #[serde(rename = "content", default)]
    content: Vec<PiContentBlock>,
    #[serde(rename = "model", default)]
    model: Option<String>,
    #[serde(rename = "usage", default)]
    usage: Option<PiUsage>,
    #[serde(rename = "stopReason", default)]
    stop_reason: Option<String>,
    #[serde(rename = "toolCallId", default)]
    tool_call_id: Option<String>,
    #[serde(rename = "isError", default)]
    is_error: Option<bool>,
    // bashExecution fields
    #[serde(rename = "command", default)]
    command: Option<String>,
    #[serde(rename = "output", default)]
    output: Option<String>,
    #[serde(rename = "exitCode", default)]
    exit_code: Option<i32>,
}

#[derive(serde::Deserialize)]
#[serde(tag = "type")]
enum PiContentBlock {
    #[serde(rename = "text")]
    Text {
        text: String,
    },
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

// ============================================================================
// Conversion
// ============================================================================

fn convert_message(
    msg: &PiMessage,
    tool_calls: &mut HashMap<String, String>,
) -> Option<Message> {
    let inner = &msg.message;
    let timestamp = chrono::Utc::now().to_rfc3339();

    match inner.role.as_str() {
        "user" => {
            let content: Vec<ContentBlock> = inner
                .content
                .iter()
                .filter_map(|cb| match cb {
                    PiContentBlock::Text { text } => {
                        Some(ContentBlock::Text { text: text.clone() })
                    }
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
                    PiContentBlock::ToolCall {
                        id,
                        name,
                        arguments,
                    } => {
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
        "bashExecution" => {
            // Convert bashExecution to a user message with tool result.
            let cmd = inner.command.as_deref().unwrap_or("unknown");
            let output = inner.output.as_deref().unwrap_or("");
            let exit_code = inner.exit_code.unwrap_or(0);
            let is_error = exit_code != 0;
            let text = if output.is_empty() {
                format!("Command exited with code {exit_code}")
            } else {
                output.to_string()
            };

            Some(Message::User(UserMessage {
                uuid: uuid::Uuid::new_v4(),
                timestamp,
                content: vec![ContentBlock::ToolResult {
                    tool_use_id: format!("bash-{cmd}"),
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
