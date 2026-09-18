//! OpenCode Zen provider (OpenAI-compatible API)
//!
//! OpenCode Zen provides an OpenAI-compatible chat completions API at
//! https://opencode.ai/zen/v1. It exposes a range of frontier models
//! (Claude, GPT, DeepSeek, GLM, Kimi, MiMo, etc.).
//!
//! This provider is a thin wrapper around the OpenAI provider because the API
//! is compatible. We reuse the OpenAI provider's logic but allow a different
//! base URL and API key environment variable.

use async_trait::async_trait;
use futures::StreamExt;
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderName, HeaderValue};
use tokio::sync::mpsc;
use tracing::{debug, warn};
use uuid::Uuid;

use super::identity;
use super::message::{ContentBlock, Message, StopReason, Usage};
use super::provider::{Provider, ProviderError, ProviderRequest, ToolChoice};
use super::stream::{StreamEvent, stream_timeout_error, wait_for_stream_timeout};

/// OpenCode Zen provider (OpenAI-compatible API)
pub struct OpenCodeProvider {
    base_url: String,
    api_key: String,
    session_id: String,
}

impl OpenCodeProvider {
    /// Create a new OpenCode Zen provider from the given base URL and API key.
    pub fn new(base_url: &str, api_key: &str) -> Self {
        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            api_key: api_key.to_string(),
            session_id: Uuid::new_v4().to_string(),
        }
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    pub fn api_key(&self) -> &str {
        &self.api_key
    }

    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    /// Build the request body in OpenAI format.
    fn build_body(&self, request: &ProviderRequest) -> serde_json::Value {
        // Convert our messages to OpenAI format.
        // Key difference: system message goes in the messages array, not separate.
        let mut messages = Vec::new();

        // System message as first message.
        if !request.system_prompt.is_empty() {
            messages.push(serde_json::json!({
                "role": "system",
                "content": request.system_prompt,
            }));
        }

        // Convert conversation messages.
        for msg in &request.messages {
            match msg {
                Message::User(u) => {
                    let content = blocks_to_openai_content(&u.content);
                    messages.push(serde_json::json!({
                        "role": "user",
                        "content": content,
                    }));
                }
                Message::Assistant(a) => {
                    let mut msg_json = serde_json::json!({
                        "role": "assistant",
                    });

                    // Check for tool calls.
                    let tool_calls: Vec<serde_json::Value> = a
                        .content
                        .iter()
                        .filter_map(|b| match b {
                            ContentBlock::ToolUse { id, name, input } => Some(serde_json::json!({
                                "id": id,
                                "type": "function",
                                "function": {
                                    "name": name,
                                    "arguments": serde_json::to_string(input).unwrap_or_default(),
                                }
                            })),
                            _ => None,
                        })
                        .collect();

                    // Text content.
                    let text: String = a
                        .content
                        .iter()
                        .filter_map(|b| match b {
                            ContentBlock::Text { text } => Some(text.as_str()),
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                        .join("");

                    // OpenAI requires content to be a string, never null.
                    msg_json["content"] = serde_json::Value::String(text);
                    if !tool_calls.is_empty() {
                        msg_json["tool_calls"] = serde_json::Value::Array(tool_calls);
                    }

                    messages.push(msg_json);
                }
                Message::System(_) => {
                    // The system subtypes (CompactBoundary, ApiError,
                    // Informational, TurnDuration, MemorySaved, ToolProgress)
                    // are all bookkeeping; none of their content is meant to
                    // drive inference. Dropping them entirely (rather than
                    // emitting an empty message) avoids providers that reject
                    // empty `content` ("message content cannot be empty").
                }
            }
        }

        // Handle tool results (OpenAI uses role: "tool").
        // We need a second pass to convert our tool_result content blocks.
        let mut final_messages = Vec::new();
        for msg in messages {
            if msg.get("role").and_then(|r| r.as_str()) == Some("user") {
                // Check if this is actually a tool result message.
                if let Some(content) = msg.get("content")
                    && let Some(arr) = content.as_array()
                {
                    let mut tool_results = Vec::new();
                    let mut other_content = Vec::new();

                    for block in arr {
                        if block.get("type").and_then(|t| t.as_str()) == Some("tool_result") {
                            tool_results.push(serde_json::json!({
                                    "role": "tool",
                                    "tool_call_id": block.get("tool_use_id").and_then(|v| v.as_str()).unwrap_or(""),
                                    "content": block.get("content").and_then(|v| v.as_str()).unwrap_or(""),
                                }));
                        } else {
                            other_content.push(block.clone());
                        }
                    }

                    if !tool_results.is_empty() {
                        // Emit tool results as separate messages.
                        for tr in tool_results {
                            final_messages.push(tr);
                        }
                        if !other_content.is_empty() {
                            let mut m = msg.clone();
                            m["content"] = serde_json::Value::Array(other_content);
                            final_messages.push(m);
                        }
                        continue;
                    }
                }
            }
            final_messages.push(msg);
        }

        // Build tools in OpenAI format.
        let tools: Vec<serde_json::Value> = request
            .tools
            .iter()
            .map(|t| {
                serde_json::json!({
                    "type": "function",
                    "function": {
                        "name": t.name,
                        "description": t.description,
                        "parameters": t.input_schema,
                    }
                })
            })
            .collect();

        // Newer models (o1, o3, gpt-5.x) use max_completion_tokens.
        let model_lower = request.model.to_lowercase();
        let uses_new_token_param = model_lower.starts_with("o1")
            || model_lower.starts_with("o3")
            || model_lower.contains("gpt-5")
            || model_lower.contains("gpt-4.1");

        let mut body = serde_json::json!({
            "model": request.model,
            "messages": final_messages,
            "stream": true,
            "stream_options": { "include_usage": true },
        });

        if uses_new_token_param {
            body["max_completion_tokens"] = serde_json::json!(request.max_tokens);
        } else {
            body["max_tokens"] = serde_json::json!(request.max_tokens);
        }

        if !tools.is_empty() {
            body["tools"] = serde_json::Value::Array(tools);

            match &request.tool_choice {
                ToolChoice::Auto => {
                    body["tool_choice"] = serde_json::json!("auto");
                }
                ToolChoice::Any => {
                    body["tool_choice"] = serde_json::json!("required");
                }
                ToolChoice::None => {
                    body["tool_choice"] = serde_json::json!("none");
                }
                ToolChoice::Specific(name) => {
                    body["tool_choice"] = serde_json::json!({
                        "type": "function",
                        "function": { "name": name }
                    });
                }
            }
        }
        if let Some(temp) = request.temperature {
            body["temperature"] = serde_json::json!(temp);
        }

        body
    }
}

#[async_trait]
impl Provider for OpenCodeProvider {
    fn name(&self) -> &str {
        "opencode"
    }

    async fn stream(
        &self,
        request: &ProviderRequest,
    ) -> Result<mpsc::Receiver<StreamEvent>, ProviderError> {
        let url = format!("{}/chat/completions", self.base_url);
        let body = self.build_body(request);

        let mut headers = HeaderMap::new();
        // Skip Authorization header for anonymous/free models (empty API key or "public")
        if !self.api_key.is_empty() && self.api_key != "public" {
            headers.insert(
                AUTHORIZATION,
                HeaderValue::from_str(&format!("Bearer {}", self.api_key))
                    .map_err(|e| ProviderError::Auth(e.to_string()))?,
            );
        }
        headers.extend(identity::headers());
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.insert(
            HeaderName::from_static("x-opencode-session"),
            HeaderValue::from_str(&self.session_id)
                .map_err(|e| ProviderError::Auth(e.to_string()))?,
        );
        headers.insert(
            HeaderName::from_static("x-session-id"),
            HeaderValue::from_str(&self.session_id)
                .map_err(|e| ProviderError::Auth(e.to_string()))?,
        );
        headers.insert(
            HeaderName::from_static("x-opencode-client"),
            HeaderValue::from_static("agent-code"),
        );

        debug!("OpenCode request to {url}");

        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(300))
            .build()
            .expect("failed to build HTTP client");

        let response = http
            .post(&url)
            .headers(headers)
            .json(&body)
            .send()
            .await
            .map_err(|e| ProviderError::Network(e.to_string()))?;

        let status = response.status();
        if !status.is_success() {
            let ra_ms = retry_after_ms(&response, 1000);
            let body_text = response
                .text()
                .await
                .map_err(|e| ProviderError::Network(format!("error decoding response body: {e}")))?;
            warn!("OpenCode chat/completions API error {status}: {body_text}");
            return match status.as_u16() {
                401 | 403 => Err(ProviderError::Auth(body_text)),
                429 => Err(ProviderError::RateLimited {
                    retry_after_ms: ra_ms,
                }),
                529 | 503 => Err(ProviderError::Overloaded),
                413 => Err(ProviderError::RequestTooLarge(body_text)),
                400 => {
                    if is_context_too_long(&body_text) {
                        Err(ProviderError::RequestTooLarge(body_text))
                    } else {
                        Err(ProviderError::InvalidResponse(body_text))
                    }
                }
                404 => Err(ProviderError::InvalidResponse(body_text)),
                _ => Err(ProviderError::Network(format!("{status}: {body_text}"))),
            };
        }

        Ok(spawn_chat_completions_stream(
            response,
            request.cancel.clone(),
            request.stream_timeout,
        ))
    }

    async fn fetch_models(&self) -> Result<Vec<(String, String)>, ProviderError> {
        let openai_provider = super::openai::OpenAiProvider::new(self.base_url(), self.api_key());
        openai_provider.fetch_models().await
    }
}

fn retry_after_ms(response: &reqwest::Response, default_ms: u64) -> u64 {
    response
        .headers()
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.trim().parse::<f64>().ok())
        .filter(|secs| *secs >= 0.0)
        .map(|secs| (secs * 1000.0) as u64)
        .unwrap_or(default_ms)
}

fn is_context_too_long(body: &str) -> bool {
    let lowered = body.to_ascii_lowercase();
    lowered.contains("exceeds the maximum allowed input length")
        || lowered.contains("maximum context length")
        || lowered.contains("input length") && lowered.contains("exceeds")
}

fn spawn_chat_completions_stream(
    response: reqwest::Response,
    cancel: tokio_util::sync::CancellationToken,
    stream_timeout: Option<std::time::Duration>,
) -> mpsc::Receiver<StreamEvent> {
    let (tx, rx) = mpsc::channel(64);
    tokio::spawn(async move {
        let mut byte_stream = response.bytes_stream();
        let mut buffer = String::new();
        let mut current_tool_id = String::new();
        let mut current_tool_name = String::new();
        let mut current_tool_args = String::new();
        let mut usage = Usage::default();
        let mut stop_reason: Option<StopReason> = None;

        loop {
            // On cancel, drop the byte stream to abort the HTTP connection.
            let chunk_result = tokio::select! {
                biased;
                _ = cancel.cancelled() => return,
                chunk = byte_stream.next() => match chunk {
                    Some(Ok(chunk)) => chunk,
                    Some(Err(e)) => {
                        let _ = tx.send(StreamEvent::Error(e.to_string())).await;
                        return;
                    }
                    None => break,
                },
                _ = wait_for_stream_timeout(stream_timeout) => {
                    let _ = tx
                        .send(StreamEvent::Error(stream_timeout_error(stream_timeout)))
                        .await;
                    break;
                }
            };
            let chunk = chunk_result;

            buffer.push_str(&String::from_utf8_lossy(&chunk));

            while let Some(pos) = buffer.find("\n\n") {
                let event_text = buffer[..pos].to_string();
                buffer = buffer[pos + 2..].to_string();

                for data in sse_data_lines(&event_text) {
                    if data == "[DONE]" {
                        emit_pending_chat_tool_call(
                            &tx,
                            &mut current_tool_id,
                            &mut current_tool_name,
                            &mut current_tool_args,
                        )
                        .await;
                        let _ = tx
                            .send(StreamEvent::Done {
                                usage: usage.clone(),
                                stop_reason: stop_reason.clone().or(Some(StopReason::EndTurn)),
                            })
                            .await;
                        return;
                    }

                    let parsed: serde_json::Value = match serde_json::from_str(&data) {
                        Ok(v) => v,
                        Err(_) => continue,
                    };

                    // Usage may arrive on its own trailing chunk (choices: [])
                    // or attached to the final delta/finish chunk. Record it
                    // whenever it is present, independently of the delta match
                    // below — otherwise providers that send usage alongside the
                    // finish chunk (e.g. OpenRouter) are silently undercounted
                    // to zero, which also zeroes the derived cost.
                    merge_chat_usage(&mut usage, &parsed);

                    let delta = match parsed
                        .get("choices")
                        .and_then(|c| c.get(0))
                        .and_then(|c| c.get("delta"))
                    {
                        Some(d) => d,
                        None => continue,
                    };

                    if let Some(content) = delta.get("content").and_then(|c| c.as_str())
                        && !content.is_empty()
                    {
                        debug!("OpenCode text delta: {}", &content[..content.len().min(80)]);
                        let _ = tx.send(StreamEvent::TextDelta(content.to_string())).await;
                    }

                    if let Some(finish) = parsed
                        .get("choices")
                        .and_then(|c| c.get(0))
                        .and_then(|c| c.get("finish_reason"))
                        .and_then(|f| f.as_str())
                    {
                        debug!("OpenCode finish_reason: {finish}");
                        match finish {
                            "stop" => stop_reason = Some(StopReason::EndTurn),
                            "tool_calls" => stop_reason = Some(StopReason::ToolUse),
                            "length" => stop_reason = Some(StopReason::MaxTokens),
                            _ => {}
                        }
                    }

                    if let Some(tool_calls) = delta.get("tool_calls").and_then(|t| t.as_array()) {
                        for tc in tool_calls {
                            if let Some(func) = tc.get("function") {
                                if let Some(name) = func.get("name").and_then(|n| n.as_str()) {
                                    if !current_tool_id.is_empty() && !current_tool_args.is_empty()
                                    {
                                        emit_pending_chat_tool_call(
                                            &tx,
                                            &mut current_tool_id,
                                            &mut current_tool_name,
                                            &mut current_tool_args,
                                        )
                                        .await;
                                    }
                                    current_tool_id = tc
                                        .get("id")
                                        .and_then(|i| i.as_str())
                                        .unwrap_or("")
                                        .to_string();
                                    current_tool_name = name.to_string();
                                    current_tool_args.clear();
                                }
                                if let Some(args) = func.get("arguments").and_then(|a| a.as_str()) {
                                    current_tool_args.push_str(args);
                                }
                            }
                        }
                    }
                }
            }
        }

        emit_pending_chat_tool_call(
            &tx,
            &mut current_tool_id,
            &mut current_tool_name,
            &mut current_tool_args,
        )
        .await;
        let _ = tx
            .send(StreamEvent::Done {
                usage,
                stop_reason: Some(StopReason::EndTurn),
            })
            .await;
    });

    rx
}

async fn emit_pending_chat_tool_call(
    tx: &mpsc::Sender<StreamEvent>,
    current_tool_id: &mut String,
    current_tool_name: &mut String,
    current_tool_args: &mut String,
) {
    if current_tool_id.is_empty() {
        return;
    }

    let input: serde_json::Value = serde_json::from_str(current_tool_args).unwrap_or_default();
    let _ = tx
        .send(StreamEvent::ContentBlockComplete(ContentBlock::ToolUse {
            id: std::mem::take(current_tool_id),
            name: std::mem::take(current_tool_name),
            input,
        }))
        .await;
    current_tool_args.clear();
}

fn sse_data_lines(event_text: &str) -> Vec<String> {
    event_text
        .lines()
        .filter_map(|line| line.strip_prefix("data: "))
        .map(str::to_string)
        .collect()
}

/// Merge OpenAI-style chat streaming usage from one parsed SSE chunk into
/// `usage`, if the chunk carries a non-null `usage` object. Providers differ
/// on where the token counts ride — a dedicated trailing chunk with an empty
/// `choices` array, or the same chunk as the finish delta — so this is called
/// on every chunk rather than only when a delta is absent.
fn merge_chat_usage(usage: &mut Usage, parsed: &serde_json::Value) {
    let Some(u) = parsed.get("usage").filter(|u| !u.is_null()) else {
        return;
    };
    if let Some(o) = u.get("completion_tokens").and_then(|v| v.as_u64()) {
        usage.output_tokens = o;
    }
    // OpenAI reports `cached_tokens` as a SUBSET of `prompt_tokens`, but the
    // cost model bills `input_tokens` and `cache_read_input_tokens`
    // independently. Split the prompt total so the cached portion is only
    // charged at the (cheaper) cache-read rate, not twice.
    let cached = u
        .get("prompt_tokens_details")
        .and_then(|d| d.get("cached_tokens"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    if let Some(prompt) = u.get("prompt_tokens").and_then(|v| v.as_u64()) {
        usage.input_tokens = prompt.saturating_sub(cached);
    }
    if cached > 0 {
        usage.cache_read_input_tokens = cached;
    }
}

/// Convert content blocks to OpenAI format.
fn blocks_to_openai_content(blocks: &[ContentBlock]) -> serde_json::Value {
    if blocks.len() == 1
        && let ContentBlock::Text { text } = &blocks[0]
    {
        return serde_json::Value::String(text.clone());
    }

    let parts: Vec<serde_json::Value> = blocks
        .iter()
        .map(|b| match b {
            ContentBlock::Text { text } => serde_json::json!({
                "type": "text",
                "text": text,
            }),
            ContentBlock::Image { media_type, data } => serde_json::json!({
                "type": "image_url",
                "image_url": {
                    "url": format!("data:{media_type};base64:{data}"),
                }
            }),
            ContentBlock::ToolResult {
                tool_use_id,
                content,
                is_error,
                ..
            } => serde_json::json!({
                "type": "tool_result",
                "tool_use_id": tool_use_id,
                "content": content,
                "is_error": is_error,
            }),
            ContentBlock::Thinking { thinking, .. } => serde_json::json!({
                "type": "text",
                "text": thinking,
            }),
            ContentBlock::ToolUse { name, input, .. } => serde_json::json!({
                "type": "text",
                "text": format!("[Tool call: {name}({input})]"),
            }),
            ContentBlock::Document { title, .. } => serde_json::json!({
                "type": "text",
                "text": format!("[Document: {}]", title.as_deref().unwrap_or("untitled")),
            }),
        })
        .collect();

    serde_json::Value::Array(parts)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_with_base_url_and_api_key() {
        let provider = OpenCodeProvider::new("https://opencode.ai/zen/v1", "test-key");
        assert_eq!(provider.base_url(), "https://opencode.ai/zen/v1");
        assert_eq!(provider.api_key(), "test-key");
    }
}