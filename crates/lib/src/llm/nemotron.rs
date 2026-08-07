//! Nemotron custom tool-call support.
//!
//! Nemotron models (nvidia/nemotron-3-*, nvidia/llama-3.3-nemotron-*,
//! nvidia/nvidia-nemotron-*, ...) do not always emit structured
//! `tool_calls` deltas. Instead the model emits tool calls as *text*
//! content in a custom format, documented by NVIDIA's integration
//! packages (langchain-nvidia-ai-endpoints / langgraph deepagents
//! "Nemotron 3 Ultra" harness profile):
//!
//! - `<function=NAME><parameter name=KEY>VALUE</parameter></function>`
//! - `<function><name>NAME</name><parameter name=KEY>VALUE</parameter></function></tool_call>`
//!   (alternate markup, including `<name=NAME</name>` and bare
//!   `<parameter>KEY: VALUE</parameter>` forms)
//! - `{"tool": "NAME", "args": {...}}` or `{"tool": "NAME", "cmd": "..."}`
//!
//! Reasoning is wrapped in `<think>...</think>` blocks. This module
//! provides a streaming parser that converts those text constructs into
//! `StreamEvent::ContentBlockComplete` tool-use / thinking blocks so the
//! rest of the agent pipeline sees standard tool calls.

use regex::Regex;
use std::sync::OnceLock;

use super::message::ContentBlock;
use super::stream::StreamEvent;

/// Whether a model id is a Nemotron model that may use the custom
/// text-based tool-call format. Matches every Nemotron variant on the
/// NVIDIA endpoint (nemotron-3-*, llama-3.3-nemotron-*,
/// nvidia-nemotron-*, nemotron-mini-*, mistral-nemotron).
pub fn is_nemotron_model(model: &str) -> bool {
    model.to_lowercase().contains("nemotron")
}

/// Marker prefixes the scanner looks for in the text stream. Longest is
/// `<function` (9 bytes); the hold-back logic uses that as its bound.
const MARKER_PREFIXES: [&str; 3] = ["<think", "<function", "{\"tool"];

/// Streaming parser for Nemotron's text-based tool-call format.
///
/// Feed it the `content` deltas from each SSE chunk; it buffers text and
/// emits `StreamEvent`s as complete constructs are recognized. Tool calls
/// are validated against the tool names from the request (case-insensitive,
/// with shell aliases bash/sh/shell/execute normalized to the Bash tool),
/// so stray markup in prose is left as plain text.
pub struct NemotronStreamParser {
    /// Text not yet emitted or consumed by a capture.
    pending: String,
    /// Active capture, if any.
    capture: Option<Capture>,
    /// Tool names from the request; empty means "accept any name".
    tool_names: Vec<String>,
}

enum Capture {
    /// Inside `<function=...>` / `<function>...</function>`; raw text so far.
    Function(String),
    /// Inside `<think>...</think>`; reasoning text so far.
    Think(String),
    /// Inside a JSON tool object; raw JSON text so far.
    Json(String),
}

/// Result of one parser step.
enum Step {
    /// An event was produced; keep stepping.
    Event(StreamEvent),
    /// A capture was just started; keep stepping to process it.
    Continue,
    /// Waiting for more input (capture in progress or partial marker).
    Waiting,
    /// Nothing more can be done with the current input.
    Idle,
}

impl NemotronStreamParser {
    pub fn new(tool_names: Vec<String>) -> Self {
        Self {
            pending: String::new(),
            capture: None,
            tool_names,
        }
    }

    /// Process one text delta, returning any events produced.
    pub fn push_text(&mut self, delta: &str) -> Vec<StreamEvent> {
        self.pending.push_str(delta);
        let mut events = Vec::new();
        loop {
            match self.step() {
                Step::Event(ev) => events.push(ev),
                Step::Continue => {}
                Step::Waiting | Step::Idle => break,
            }
        }
        events
    }

    /// Flush any buffered text at stream end. Incomplete captures are
    /// emitted as raw text (the model may have been truncated mid-block).
    pub fn finish(&mut self) -> Vec<StreamEvent> {
        let mut events = Vec::new();
        loop {
            match self.step() {
                Step::Event(ev) => events.push(ev),
                Step::Continue => {}
                Step::Waiting => {
                    if let Some(capture) = self.capture.take() {
                        let text = match capture {
                            Capture::Function(b) | Capture::Think(b) | Capture::Json(b) => b,
                        };
                        if !text.is_empty() {
                            events.push(StreamEvent::TextDelta(text));
                        }
                    }
                    self.flush_pending(&mut events);
                    break;
                }
                Step::Idle => {
                    self.flush_pending(&mut events);
                    break;
                }
            }
        }
        events
    }

    fn flush_pending(&mut self, events: &mut Vec<StreamEvent>) {
        if !self.pending.is_empty() {
            events.push(StreamEvent::TextDelta(std::mem::take(&mut self.pending)));
        }
    }

    fn step(&mut self) -> Step {
        match self.capture.take() {
            None => self.step_scan(),
            Some(Capture::Function(buf)) => self.step_function(buf),
            Some(Capture::Think(buf)) => self.step_think(buf),
            Some(Capture::Json(buf)) => self.step_json(buf),
        }
    }

    /// Not capturing: scan the buffer for a marker, emitting plain text
    /// up to it and starting a capture when one is found.
    fn step_scan(&mut self) -> Step {
        let pending = std::mem::take(&mut self.pending);
        if pending.is_empty() {
            return Step::Idle;
        }
        // Hold back any trailing bytes that could be the start of a marker
        // split across deltas.
        let hold = held_back_len(&pending);
        let scan_len = pending.len() - hold;
        let scan = &pending[..scan_len];

        match find_earliest_marker(scan) {
            Some((kind, start)) => {
                if start > 0 {
                    let text = pending[..start].to_string();
                    self.pending = pending[start..].to_string();
                    Step::Event(StreamEvent::TextDelta(text))
                } else {
                    self.capture = Some(match kind {
                        MarkerKind::Function => Capture::Function(pending),
                        MarkerKind::Think => Capture::Think(pending),
                        MarkerKind::Json => Capture::Json(pending),
                    });
                    Step::Continue
                }
            }
            None => {
                if scan_len > 0 {
                    let text = pending[..scan_len].to_string();
                    self.pending = pending[scan_len..].to_string();
                    Step::Event(StreamEvent::TextDelta(text))
                } else {
                    // Entirely a partial marker prefix; wait for more data.
                    self.pending = pending;
                    Step::Idle
                }
            }
        }
    }

    fn step_function(&mut self, mut buf: String) -> Step {
        buf.push_str(&self.pending);
        self.pending.clear();
        if let Some(pos) = buf.find("</function>") {
            let end = pos + "</function>".len();
            let block = buf[..end].to_string();
            let mut rest = buf[end..].to_string();
            rest = rest.trim_start_matches(char::is_whitespace).to_string();
            rest = rest
                .strip_prefix("</tool_call>")
                .map(str::to_string)
                .unwrap_or(rest);
            self.pending = rest;
            match parse_function_block(&block, &self.tool_names) {
                Some((name, input)) => Step::Event(tool_use_event(name, input)),
                None => Step::Event(StreamEvent::TextDelta(block)),
            }
        } else {
            self.capture = Some(Capture::Function(buf));
            Step::Waiting
        }
    }

    fn step_think(&mut self, mut buf: String) -> Step {
        buf.push_str(&self.pending);
        self.pending.clear();
        if let Some(pos) = buf.find("</think>") {
            // buf starts at the `<think` marker; strip the opening tag.
            let open_end = buf.find('>').map(|p| p + 1).unwrap_or(0);
            let thinking = buf[open_end.min(pos)..pos].trim().to_string();
            self.pending = buf[pos + "</think>".len()..].to_string();
            if thinking.is_empty() {
                Step::Continue
            } else {
                Step::Event(StreamEvent::ContentBlockComplete(ContentBlock::Thinking {
                    thinking,
                    signature: None,
                }))
            }
        } else {
            self.capture = Some(Capture::Think(buf));
            Step::Waiting
        }
    }

    fn step_json(&mut self, mut buf: String) -> Step {
        buf.push_str(&self.pending);
        self.pending.clear();
        match json_object_end(&buf) {
            Some(end) => {
                let block = buf[..end].to_string();
                self.pending = buf[end..].to_string();
                match parse_json_tool_call(&block, &self.tool_names) {
                    Some((name, input)) => Step::Event(tool_use_event(name, input)),
                    None => Step::Event(StreamEvent::TextDelta(block)),
                }
            }
            None => {
                self.capture = Some(Capture::Json(buf));
                Step::Waiting
            }
        }
    }
}

fn tool_use_event(name: String, input: serde_json::Value) -> StreamEvent {
    StreamEvent::ContentBlockComplete(ContentBlock::ToolUse {
        id: new_tool_call_id(),
        name,
        input,
    })
}

fn new_tool_call_id() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

#[derive(Clone, Copy)]
enum MarkerKind {
    Function,
    Think,
    Json,
}

/// Length of the longest suffix of `s` that is a prefix of a marker
/// string (so a marker split across chunk boundaries is not emitted).
fn held_back_len(s: &str) -> usize {
    let n = s.len();
    let max = MARKER_PREFIXES.iter().map(|m| m.len()).max().unwrap_or(0);
    let mut best = 0;
    for len in 1..=n.min(max) {
        let suffix = &s[n - len..];
        if MARKER_PREFIXES.iter().any(|m| m.starts_with(suffix)) {
            best = len;
        }
    }
    best
}

/// Find the earliest marker in `s`, verifying the character that follows
/// the marker prefix (needed to distinguish `<function=` / `<function>`
/// from prose like `<functions>`).
fn find_earliest_marker(s: &str) -> Option<(MarkerKind, usize)> {
    let mut best: Option<(MarkerKind, usize)> = None;

    if let Some(p) = s.find("<think") {
        let after = s[p + "<think".len()..].chars().next();
        if after.is_none_or(|c| c == '>' || c.is_whitespace()) {
            best = Some((MarkerKind::Think, p));
        }
    }
    if let Some(p) = s.find("<function") {
        let after = s[p + "<function".len()..].chars().next();
        if after.is_none_or(|c| c == '=' || c == '>') {
            best = Some((MarkerKind::Function, p));
        }
    }
    if let Some(p) = s.find("{\"tool") {
        best = Some((MarkerKind::Json, p));
    }

    best
}

/// Parse a complete `<function=...>` / `<function>...</function>` block
/// into a (name, input) pair, or `None` when it does not name an
/// available tool.
fn parse_function_block(block: &str, tool_names: &[String]) -> Option<(String, serde_json::Value)> {
    static FUNCTION_BLOCK_RE: OnceLock<Regex> = OnceLock::new();
    static PARAMETER_RE: OnceLock<Regex> = OnceLock::new();
    static ALT_FUNCTION_BLOCK_RE: OnceLock<Regex> = OnceLock::new();
    static ALT_NAME_RE: OnceLock<Regex> = OnceLock::new();
    static ALT_PARAMETER_RE: OnceLock<Regex> = OnceLock::new();
    static ALT_INLINE_ARG_RE: OnceLock<Regex> = OnceLock::new();

    let function_re = FUNCTION_BLOCK_RE
        .get_or_init(|| Regex::new(r"(?s)<function=([^>\s]+)\s*>(.*?)</function>").unwrap());
    let param_re = PARAMETER_RE.get_or_init(|| {
        Regex::new(r"(?s)<parameter\s+name=([^>\s]+)\s*>(.*?)</parameter>").unwrap()
    });

    if let Some(caps) = function_re.captures(block) {
        let name = normalize_tool_name(&caps[1], tool_names)?;
        let mut args = serde_json::Map::new();
        for p in param_re.captures_iter(&caps[2]) {
            let key = p[1].trim().trim_matches('"').trim_matches('\'');
            args.insert(key.to_string(), coerce_arg_value(&p[2]));
        }
        return Some((name, serde_json::Value::Object(args)));
    }

    // Alternate markup: <function>...</function></tool_call>.
    let alt_re = ALT_FUNCTION_BLOCK_RE.get_or_init(|| {
        Regex::new(r"(?is)<function>\s*(.*?)</function>\s*(?:</tool_call>)?").unwrap()
    });
    let alt_name_re = ALT_NAME_RE
        .get_or_init(|| Regex::new(r"(?is)<name\s*>(.*?)</name>|<name=([^<>\s]+)</name>").unwrap());
    let alt_param_re = ALT_PARAMETER_RE.get_or_init(|| {
        Regex::new(r"(?is)<parameter(?:\s+name=([^>\s]+))?\s*>(.*?)</parameter>").unwrap()
    });
    let inline_re = ALT_INLINE_ARG_RE.get_or_init(|| {
        Regex::new(r"(?s)^\s*<?([A-Za-z_][A-Za-z0-9_-]*)>?\s*:\s*(.*?)\s*$").unwrap()
    });

    let caps = alt_re.captures(block)?;
    let body = &caps[1];
    let name = alt_name_re
        .captures(body)
        .and_then(|c| c.get(1).or_else(|| c.get(2)))
        .map(|m| {
            m.as_str()
                .trim()
                .trim_matches('"')
                .trim_matches('\'')
                .to_string()
        })
        .and_then(|n| normalize_tool_name(&n, tool_names))?;

    let mut args = serde_json::Map::new();
    for p in alt_param_re.captures_iter(body) {
        if let Some(key) = p.get(1) {
            let key = key.as_str().trim().trim_matches('"').trim_matches('\'');
            args.insert(key.to_string(), coerce_arg_value(&p[2]));
        } else if let Some(inline) = inline_re.captures(&p[2]) {
            let key = inline[1].trim().trim_matches('"').trim_matches('\'');
            args.insert(key.to_string(), coerce_arg_value(&inline[2]));
        }
    }
    Some((name, serde_json::Value::Object(args)))
}

/// Parse a complete `{"tool": ...}` JSON tool call.
fn parse_json_tool_call(text: &str, tool_names: &[String]) -> Option<(String, serde_json::Value)> {
    let obj: serde_json::Value = serde_json::from_str(text).ok()?;
    let obj = obj.as_object()?;
    let raw_name = obj.get("tool")?.as_str()?;
    let name = normalize_tool_name(raw_name, tool_names)?;

    let input = if let Some(args) = obj.get("args").and_then(|v| v.as_object()) {
        serde_json::Value::Object(args.clone())
    } else if let Some(cmd) = obj
        .get("cmd")
        .and_then(|v| v.as_str())
        .or_else(|| obj.get("command").and_then(|v| v.as_str()))
    {
        serde_json::json!({ "command": cmd })
    } else {
        serde_json::Value::Object(Default::default())
    };

    Some((name, input))
}

/// Resolve a raw tool name against the request's tool list. Accepts any
/// case; shell aliases (bash/sh/shell/execute) normalize to the Bash tool.
/// Returns `None` when the name is not an available tool.
fn normalize_tool_name(raw: &str, tool_names: &[String]) -> Option<String> {
    let trimmed = raw.trim().trim_matches('"').trim_matches('\'');
    if trimmed.is_empty() {
        return None;
    }
    if tool_names.is_empty() {
        return Some(trimmed.to_string());
    }
    for t in tool_names {
        if t.eq_ignore_ascii_case(trimmed) {
            return Some(t.clone());
        }
    }
    if matches!(
        trimmed.to_lowercase().as_str(),
        "bash" | "sh" | "shell" | "execute"
    ) {
        for t in tool_names {
            if t.eq_ignore_ascii_case("bash") {
                return Some(t.clone());
            }
        }
    }
    None
}

/// Coerce a tag-form parameter value: numbers, booleans, null, arrays and
/// objects become typed JSON; anything else stays a string. Mirrors the
/// langchain harness's `_parse_tool_value`.
fn coerce_arg_value(v: &str) -> serde_json::Value {
    let trimmed = v.trim();
    if trimmed.is_empty() {
        return serde_json::Value::Null;
    }
    match serde_json::from_str::<serde_json::Value>(trimmed) {
        Ok(val) => val,
        Err(_) => serde_json::Value::String(trimmed.to_string()),
    }
}

/// Find the byte offset just past the closing `}` of the top-level JSON
/// object, tracking braces inside strings. Returns `None` while the object
/// is still open.
fn json_object_end(buf: &str) -> Option<usize> {
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    for (i, ch) in buf.char_indices() {
        if in_string {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
            }
        } else {
            match ch {
                '"' => in_string = true,
                '{' => depth += 1,
                '}' => {
                    depth = depth.saturating_sub(1);
                    if depth == 0 {
                        return Some(i + 1);
                    }
                }
                _ => {}
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::stream::StreamEvent;

    fn tool_names() -> Vec<String> {
        ["Bash", "FileRead", "FileWrite", "Glob"]
            .iter()
            .map(|s| s.to_string())
            .collect()
    }

    fn text_events(events: &[StreamEvent]) -> String {
        events
            .iter()
            .filter_map(|e| match e {
                StreamEvent::TextDelta(t) => Some(t.as_str()),
                _ => None,
            })
            .collect()
    }

    fn tool_uses(events: &[StreamEvent]) -> Vec<(String, serde_json::Value)> {
        events
            .iter()
            .filter_map(|e| match e {
                StreamEvent::ContentBlockComplete(ContentBlock::ToolUse {
                    name, input, ..
                }) => Some((name.clone(), input.clone())),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn is_nemotron_model_matches_family() {
        for m in [
            "nvidia/nemotron-3-ultra-550b-a55b",
            "nvidia/nemotron-3-super-120b-a12b",
            "nvidia/nvidia-nemotron-nano-9b-v2",
            "nvidia/llama-3.3-nemotron-super-49b-v1",
            "mistralai/mistral-nemotron",
        ] {
            assert!(is_nemotron_model(m), "{m}");
        }
        for m in ["gpt-4o", "nvidia/meta/llama-3.3-70b-instruct", ""] {
            assert!(!is_nemotron_model(m), "{m}");
        }
    }

    #[test]
    fn primary_function_block_single_chunk() {
        let mut p = NemotronStreamParser::new(tool_names());
        let events = p.push_text(
            "<function=FileRead><parameter name=file_path>/etc/hostname</parameter></function>",
        );
        let calls = tool_uses(&events);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "FileRead");
        assert_eq!(calls[0].1["file_path"], "/etc/hostname");
        assert!(text_events(&events).is_empty());
    }

    #[test]
    fn function_block_split_across_deltas() {
        let mut p = NemotronStreamParser::new(tool_names());
        let mut events = Vec::new();
        events.extend(p.push_text("Let me check.\n<function=FileRea"));
        events.extend(
            p.push_text("d><parameter name=file_path>/etc/hostname</parameter></function>"),
        );
        events.extend(p.finish());
        assert_eq!(text_events(&events), "Let me check.\n");
        let calls = tool_uses(&events);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "FileRead");
        assert_eq!(calls[0].1["file_path"], "/etc/hostname");
    }

    #[test]
    fn alternate_markup_with_tool_call_suffix() {
        let mut p = NemotronStreamParser::new(tool_names());
        let events = p.push_text(
            "<function>\n<name>Bash</name>\n<parameter name=command>ls -la</parameter>\n</function></tool_call>",
        );
        let calls = tool_uses(&events);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "Bash");
        assert_eq!(calls[0].1["command"], "ls -la");
    }

    #[test]
    fn alternate_markup_with_name_equals_and_inline_arg() {
        // Mirrors the langchain harness test: <name=X</name> and a bare
        // <parameter>KEY: VALUE</parameter> with angle-bracketed key.
        let mut p = NemotronStreamParser::new(vec!["get_service_name".to_string()]);
        let events = p.push_text(
            "<function>\n<name=get_service_name</name>\n<parameter>\n<service_id>:0\n</parameter>\n</function>\n</tool_call>",
        );
        let calls = tool_uses(&events);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "get_service_name");
        // Tag-form values are coerced to typed JSON when unambiguous
        // ("0" is a number, not a path).
        assert_eq!(calls[0].1["service_id"], 0);
    }

    #[test]
    fn inline_parameter_key_value() {
        let mut p = NemotronStreamParser::new(tool_names());
        let events = p.push_text(
            "<function><name>Bash</name><parameter>command: echo hi</parameter></function>",
        );
        let calls = tool_uses(&events);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].1["command"], "echo hi");
    }

    #[test]
    fn json_tool_call_with_cmd() {
        let mut p = NemotronStreamParser::new(tool_names());
        let events = p.push_text(r#"{"tool": "Bash", "cmd": "pwd"}"#);
        let calls = tool_uses(&events);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "Bash");
        assert_eq!(calls[0].1["command"], "pwd");
    }

    #[test]
    fn json_tool_call_with_args_keeps_types() {
        let mut p = NemotronStreamParser::new(tool_names());
        let events =
            p.push_text(r#"{"tool": "FileRead", "args": {"file_path": "/a.txt", "limit": 10}}"#);
        let calls = tool_uses(&events);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "FileRead");
        assert_eq!(calls[0].1["file_path"], "/a.txt");
        assert_eq!(calls[0].1["limit"], 10);
    }

    #[test]
    fn json_nested_braces_in_string() {
        let mut p = NemotronStreamParser::new(tool_names());
        let events = p.push_text(r#"{"tool": "Bash", "args": {"command": "echo {a}"}}"#);
        let calls = tool_uses(&events);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].1["command"], "echo {a}");
    }

    #[test]
    fn json_tool_name_case_insensitive() {
        let mut p = NemotronStreamParser::new(tool_names());
        let events = p.push_text(r#"{"tool": "bash", "cmd": "ls"}"#);
        let calls = tool_uses(&events);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "Bash");
    }

    #[test]
    fn shell_alias_normalized_to_bash() {
        for alias in ["bash", "sh", "shell", "execute"] {
            let mut q = NemotronStreamParser::new(tool_names());
            let events = q.push_text(&format!(r#"{{"tool": "{alias}", "cmd": "ls"}}"#));
            let calls = tool_uses(&events);
            assert_eq!(calls.len(), 1, "{alias}");
            assert_eq!(calls[0].0, "Bash", "{alias}");
        }
    }

    #[test]
    fn unknown_tool_kept_as_text() {
        let mut p = NemotronStreamParser::new(tool_names());
        let events =
            p.push_text("<function=ReadFile><parameter name=file_path>/x</parameter></function>");
        assert!(tool_uses(&events).is_empty());
        assert!(text_events(&events).contains("<function=ReadFile>"));
    }

    #[test]
    fn think_block_extracted() {
        let mut p = NemotronStreamParser::new(tool_names());
        let mut events = Vec::new();
        events.extend(p.push_text("Before I act <think>let me consider</think> I'll proceed"));
        events.extend(p.finish());
        assert_eq!(text_events(&events), "Before I act  I'll proceed");
        let thinks: Vec<&str> = events
            .iter()
            .filter_map(|e| match e {
                StreamEvent::ContentBlockComplete(ContentBlock::Thinking { thinking, .. }) => {
                    Some(thinking.as_str())
                }
                _ => None,
            })
            .collect();
        assert_eq!(thinks, vec!["let me consider"]);
    }

    #[test]
    fn empty_think_block_ignored() {
        let mut p = NemotronStreamParser::new(tool_names());
        let events = p.push_text("<think>\n</think>visible");
        assert_eq!(text_events(&events), "visible");
    }

    #[test]
    fn numeric_parameter_values_coerced() {
        let mut p = NemotronStreamParser::new(vec!["FileRead".to_string()]);
        let events = p.push_text(
            "<function=FileRead><parameter name=file_path>/x</parameter><parameter name=offset>42</parameter><parameter name=flag>true</parameter><parameter name=name>hello world</parameter></function>",
        );
        let calls = tool_uses(&events);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].1["file_path"], "/x");
        assert_eq!(calls[0].1["offset"], 42);
        assert_eq!(calls[0].1["flag"], true);
        assert_eq!(calls[0].1["name"], "hello world");
    }

    #[test]
    fn multiple_blocks_and_text_in_one_chunk() {
        let mut p = NemotronStreamParser::new(tool_names());
        let events = p.push_text(
            "first<function=Bash><parameter name=command>a</parameter></function>second<function=Bash><parameter name=command>b</parameter></function>third",
        );
        assert_eq!(text_events(&events), "firstsecondthird");
        let calls = tool_uses(&events);
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].1["command"], "a");
        assert_eq!(calls[1].1["command"], "b");
    }

    #[test]
    fn marker_split_at_chunk_boundary_holds_back() {
        let mut p = NemotronStreamParser::new(tool_names());
        let mut events = Vec::new();
        events.extend(p.push_text("text <fun"));
        events.extend(
            p.push_text("ction=FileRead><parameter name=file_path>/x</parameter></function>"),
        );
        events.extend(p.finish());
        assert_eq!(text_events(&events), "text ");
        let calls = tool_uses(&events);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].1["file_path"], "/x");
    }

    #[test]
    fn json_split_across_deltas() {
        // Split mid-value (the realistic case); a key is never split across
        // multi-kilobyte SSE chunks.
        let mut p = NemotronStreamParser::new(tool_names());
        let mut events = Vec::new();
        events.extend(p.push_text(r#"{"tool": "Bash", "cmd": "pw"#));
        events.extend(p.push_text("d\"}"));
        events.extend(p.finish());
        let calls = tool_uses(&events);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].1["command"], "pwd");
    }

    #[test]
    fn finish_flushes_truncated_block_as_text() {
        let mut p = NemotronStreamParser::new(tool_names());
        let events = p.push_text("<function=Bash><parameter name=command>ls");
        assert!(tool_uses(&events).is_empty());
        let events = p.finish();
        assert!(text_events(&events).contains("<function=Bash>"));
    }

    #[test]
    fn think_tag_prefix_in_prose_is_plain_text() {
        // "<thinkpad" is not a think marker.
        let mut p = NemotronStreamParser::new(tool_names());
        let events = p.push_text("I use a thinkpad laptop.");
        assert_eq!(text_events(&events), "I use a thinkpad laptop.");
        assert!(tool_uses(&events).is_empty());
    }

    #[test]
    fn empty_parser_events() {
        let mut p = NemotronStreamParser::new(tool_names());
        assert!(p.push_text("").is_empty());
        assert!(p.finish().is_empty());
    }

    #[test]
    fn prose_json_with_unknown_tool_is_text() {
        let mut p = NemotronStreamParser::new(tool_names());
        let events = p.push_text(r#"Use {"tool": "Frobnicate", "args": {}} now"#);
        assert!(tool_uses(&events).is_empty());
        assert!(text_events(&events).contains(r#"{"tool": "Frobnicate""#));
    }
}
