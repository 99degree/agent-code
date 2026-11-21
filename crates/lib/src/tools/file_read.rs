//! FileRead tool: read file contents with optional line ranges.
//!
//! When the model requests a small range (under 25 lines) that falls inside
//! a function, the read is auto-expanded to cover the rest of that function
//! (capped at 25 lines). This avoids extra round-trips when the model only
//! grabbed a fragment of a function body.

use async_trait::async_trait;
use serde_json::json;
use std::path::PathBuf;

use super::{Tool, ToolContext, ToolResult};
use crate::error::ToolError;

pub struct FileReadTool;

const FUNCTION_EXPAND_MAX_LINES: usize = 25;

#[async_trait]
impl Tool for FileReadTool {
    fn name(&self) -> &'static str {
        "FileRead"
    }

    fn description(&self) -> &'static str {
        "Reads a file from the filesystem. Returns contents with line numbers."
    }

    fn input_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "required": ["file_path"],
            "properties": {
                "file_path": {
                    "type": "string",
                    "description": "Absolute path to the file"
                },
                "offset": {
                    "type": "integer",
                    "description": "Line number to start reading from (1-based)"
                },
                "limit": {
                    "type": "integer",
                    "description": "Number of lines to read. When under 25 and the range falls inside a function, the read is auto-expanded to the end of that function (capped at 25 lines)."
                },
                "pages": {
                    "type": "string",
                    "description": "Page range for PDF files (e.g., \"1-5\", \"3\", \"10-20\"). Max 20 pages per request."
                }
            }
        })
    }

    fn is_read_only(&self) -> bool {
        true
    }

    fn is_concurrency_safe(&self) -> bool {
        true
    }

    fn get_path(&self, input: &serde_json::Value) -> Option<PathBuf> {
        input
            .get("file_path")
            .and_then(|v| v.as_str())
            .map(PathBuf::from)
    }

    async fn call(
        &self,
        input: serde_json::Value,
        _ctx: &ToolContext,
    ) -> Result<ToolResult, ToolError> {
        let file_path = input
            .get("file_path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidInput("'file_path' is required".into()))?;

        let offset = input.get("offset").and_then(|v| v.as_u64()).unwrap_or(1) as usize;

        let requested_limit = input.get("limit").and_then(|v| v.as_u64()).unwrap_or(2000) as usize;

        let path = std::path::Path::new(file_path);

        // Block device and virtual filesystem paths.
        const BLOCKED_PREFIXES: &[&str] = &["/dev/", "/proc/", "/sys/"];
        if BLOCKED_PREFIXES
            .iter()
            .any(|prefix| file_path.starts_with(prefix))
        {
            return Err(ToolError::InvalidInput(format!(
                "Cannot read virtual/device file: {file_path}"
            )));
        }

        let pages = input
            .get("pages")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        // Handle binary/special file types.
        match path.extension().and_then(|e| e.to_str()) {
            Some("pdf") => {
                return read_pdf(file_path, pages.as_deref()).await;
            }
            Some("ipynb") => {
                return read_notebook(file_path).await;
            }
            Some("png" | "jpg" | "jpeg" | "gif" | "webp" | "svg" | "ico" | "bmp") => {
                let meta = tokio::fs::metadata(file_path).await.ok();
                let size = meta.map(|m| m.len()).unwrap_or(0);

                // For small images (< 5MB), embed as base64 for vision models.
                if size < 5 * 1024 * 1024
                    && crate::llm::message::image_block_from_file(path).is_ok()
                {
                    return Ok(ToolResult::success(format!(
                        "(Image: {file_path}, {size} bytes — loaded for vision analysis)"
                    )));
                }

                return Ok(ToolResult::success(format!(
                    "(Image file: {file_path}, {size} bytes — \
                     too large for inline embedding)"
                )));
            }
            Some("wasm" | "exe" | "dll" | "so" | "dylib" | "o" | "a") => {
                let meta = tokio::fs::metadata(file_path).await.ok();
                let size = meta.map(|m| m.len()).unwrap_or(0);
                return Ok(ToolResult::success(format!(
                    "(Binary file: {file_path}, {size} bytes)"
                )));
            }
            _ => {}
        }

        // Try to read as text; if it fails (binary content), report the file type.
        let content = match tokio::fs::read_to_string(file_path).await {
            Ok(c) => c,
            Err(e) => {
                // May be binary — try to read size at least.
                if let Ok(meta) = tokio::fs::metadata(file_path).await {
                    return Ok(ToolResult::success(format!(
                        "(Binary or unreadable file: {file_path}, {} bytes: {e})",
                        meta.len()
                    )));
                }
                return Err(ToolError::ExecutionFailed(format!(
                    "Failed to read {file_path}: {e}"
                )));
            }
        };

        // Apply line range and add line numbers (1-indexed).
        let lines: Vec<&str> = content.lines().collect();
        let start = (offset.saturating_sub(1)).min(lines.len());

        // Auto-expand small ranges that fall inside a function so the model
        // gets the full function body instead of a fragment, saving an extra
        // read round-trip. Capped at FUNCTION_EXPAND_MAX_LINES total lines.
        let effective_limit = if requested_limit < FUNCTION_EXPAND_MAX_LINES {
            function_aware_limit(&lines, start, requested_limit)
        } else {
            requested_limit
        };

        let end = (start + effective_limit).min(lines.len());

        let mut output = String::new();
        for (i, line) in lines[start..end].iter().enumerate() {
            let line_num = start + i + 1;
            output.push_str(&format!("{line_num}\t{line}\n"));
        }

        if output.is_empty() {
            output = "(empty file)".to_string();
        }

        // Token-based size limit: estimate ~4 bytes per token, cap at 100K tokens.
        const MAX_TOKENS: usize = 100_000;
        const BYTES_PER_TOKEN: usize = 4;
        let max_bytes = MAX_TOKENS * BYTES_PER_TOKEN;
        if output.len() > max_bytes {
            output.truncate(max_bytes);
            // Avoid splitting a multi-byte character.
            while !output.is_char_boundary(output.len()) {
                output.pop();
            }
            output.push_str(&format!(
                "\n\n(File content truncated: exceeded ~{MAX_TOKENS} token estimate. \
                 Use offset/limit to read specific sections.)"
            ));
        }

        Ok(ToolResult::success(output))
    }
}

/// Programming language families used for function-boundary detection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Language {
    /// Brace-delimited with `fn`/`func`/`function` keywords (Rust, Go, JS/TS).
    Rust,
    /// Python (indentation-delimited).
    Python,
    /// JavaScript / TypeScript (brace-delimited).
    JavaScript,
    /// Go (brace-delimited, `func` keyword).
    Go,
    /// C-family languages (brace-delimited): C, C++, C#, Java, Kotlin, etc.
    CFamily,
    /// Shell scripts (brace-delimited, `function`/`name()` keywords).
    Shell,
    /// PHP (brace-delimited, `function` keyword).
    Php,
    /// Ruby (`def` keyword, `end` delimiter).
    Ruby,
    /// Swift (`func` keyword, brace-delimited).
    Swift,
    /// Dart (`func`-ish, brace-delimited).
    Dart,
    /// Lua (`function` keyword, `end` delimiter).
    Lua,
    /// Unrecognized — no function-aware expansion.
    Unknown,
}

/// When a small read range falls inside a function, expand the line count to
/// cover the rest of that function (capped at FUNCTION_EXPAND_MAX_LINES).
///
/// `start_idx` is the 0-based index of the first line to read.
/// `requested_limit` is the number of lines the model asked for.
fn function_aware_limit(lines: &[&str], start_idx: usize, requested_limit: usize) -> usize {
    if lines.is_empty() || start_idx >= lines.len() {
        return requested_limit;
    }

    // Detect language heuristically from content (file path isn't available here).
    let lang = detect_language_from_content(lines);

    match lang {
        Language::Unknown => requested_limit,
        Language::Python => expand_python_function(lines, start_idx, requested_limit),
        Language::Ruby | Language::Lua => {
            expand_keyword_end_function(lines, start_idx, requested_limit, "def", "end")
        }
        _ => expand_brace_function(lines, start_idx, requested_limit),
    }
}

/// Detect language from content heuristics (first ~50 lines).
fn detect_language_from_content(lines: &[&str]) -> Language {
    let sample: String = lines
        .iter()
        .take(50)
        .copied()
        .collect::<Vec<_>>()
        .join("\n");
    let lower = sample.to_lowercase();

    // Python indicators.
    if (lower.contains("def ") || lower.contains("import ") || lower.contains("from "))
        && (lower.contains("self.") || lower.contains(":\\n") || lower.contains("print("))
    {
        return Language::Python;
    }

    // Rust indicators.
    if lower.contains("fn ") && (lower.contains("use ") || lower.contains("pub ")) {
        return Language::Rust;
    }

    // Go indicators.
    if lower.contains("func ") && lower.contains("package ") {
        return Language::Go;
    }

    // JavaScript/TypeScript indicators.
    if (lower.contains("function ") || lower.contains("=>") || lower.contains("const "))
        && (lower.contains("console.log")
            || lower.contains("document.")
            || lower.contains("export "))
    {
        return Language::JavaScript;
    }

    // Shell indicators.
    if lower.contains("#!/bin/bash") || lower.contains("#!/bin/sh") || lower.contains("function ") {
        return Language::Shell;
    }

    // C-family indicators.
    if lower.contains("#include") || lower.contains("public class") || lower.contains("void ") {
        return Language::CFamily;
    }

    Language::Unknown
}

/// Expand a read range inside a brace-delimited function to cover the rest of
/// the function body, capped at FUNCTION_EXPAND_MAX_LINES total lines.
///
/// Scans backwards from `start_idx` to find the function's opening line
/// (a line that looks like a function declaration containing `{`), then scans
/// forwards to find the matching closing `}` at column 0.
fn expand_brace_function(lines: &[&str], start_idx: usize, requested_limit: usize) -> usize {
    // Scan backwards from start_idx to find the enclosing function declaration
    // line (the nearest line above that looks like a `fn`/`func`/`function`
    // declaration).
    let mut func_start = start_idx;
    let mut found_decl = false;
    for i in (0..=start_idx).rev() {
        if is_function_declaration(lines[i].trim_start()) {
            func_start = i;
            found_decl = true;
            break;
        }
    }
    if !found_decl {
        return requested_limit;
    }

    // Scan forward from func_start counting braces. Track whether we've
    // entered the function body (depth > 0). When depth returns to 0 after
    // being positive, we've found the matching close brace at column 0.
    let mut depth: usize = 0;
    let mut entered = false;
    let mut func_end = lines.len();
    for (offset, line) in lines[func_start..].iter().enumerate() {
        let trimmed = line.trim_start();
        for c in trimmed.chars() {
            match c {
                '{' => {
                    depth += 1;
                    entered = true;
                }
                '}' if depth > 0 => {
                    depth -= 1;
                    if depth == 0 && entered {
                        func_end = func_start + offset + 1;
                        break;
                    }
                }
                _ => {}
            }
        }
    }

    let func_lines_from_start = func_end.saturating_sub(start_idx);
    let expand_to = func_lines_from_start.min(FUNCTION_EXPAND_MAX_LINES);

    if expand_to > requested_limit {
        return expand_to;
    }
    requested_limit
}

/// Expand a read range inside a Python function to cover the rest of the
/// function body, capped at FUNCTION_EXPAND_MAX_LINES total lines.
///
/// Scans backwards for the enclosing `def `/`class `/`async def ` line,
/// then forwards for the next line at the same or lesser indentation that
/// starts a new top-level statement.
fn expand_python_function(lines: &[&str], start_idx: usize, requested_limit: usize) -> usize {
    // Find the indentation of the start line.
    let start_line = lines[start_idx];
    let start_indent = leading_indent(start_line);

    // Scan backwards for the enclosing function/class definition.
    let mut func_start = start_idx;
    for i in (0..=start_idx).rev() {
        let line = lines[i];
        let trimmed = line.trim_start();
        if trimmed.starts_with("def ")
            || trimmed.starts_with("async def ")
            || trimmed.starts_with("class ")
        {
            let decl_indent = leading_indent(line);
            if decl_indent <= start_indent {
                func_start = i;
                break;
            }
        }
    }

    // Scan forwards from func_start for the next line at <= func_start indent
    // that is a new statement (not blank, not comment, not decorator).
    let func_indent = leading_indent(lines[func_start]);
    let mut func_end = lines.len();
    for (i, line) in lines[func_start + 1..].iter().enumerate() {
        if line.trim().is_empty() || line.trim_start().starts_with('#') {
            continue;
        }
        let indent = leading_indent(line);
        if indent <= func_indent {
            func_end = func_start + 1 + i;
            break;
        }
    }

    let func_lines_from_start = func_end.saturating_sub(start_idx);
    let expand_to = func_lines_from_start.min(FUNCTION_EXPAND_MAX_LINES);

    if expand_to > requested_limit {
        return expand_to;
    }
    requested_limit
}

/// Expand a read range inside a `def`/`end`-delimited function (Ruby, Lua).
fn expand_keyword_end_function(
    lines: &[&str],
    start_idx: usize,
    requested_limit: usize,
    start_kw: &str,
    end_kw: &str,
) -> usize {
    let mut func_start = start_idx;
    for i in (0..=start_idx).rev() {
        let trimmed = lines[i].trim_start();
        if trimmed.starts_with(start_kw) {
            func_start = i;
            break;
        }
    }

    let mut func_end = lines.len();
    for (i, line) in lines[func_start + 1..].iter().enumerate() {
        let trimmed = line.trim_start();
        if trimmed == end_kw
            || trimmed.starts_with(&(end_kw.to_owned() + " "))
            || trimmed.starts_with(&(end_kw.to_owned() + "\t"))
        {
            func_end = func_start + 1 + i;
            break;
        }
    }

    let func_lines_from_start = func_end.saturating_sub(start_idx);
    let expand_to = func_lines_from_start.min(FUNCTION_EXPAND_MAX_LINES);

    if expand_to > requested_limit {
        return expand_to;
    }
    requested_limit
}

/// Returns true when `line` (already trimmed of leading whitespace) looks
/// like a function/method declaration in a brace-delimited language.
fn is_function_declaration(line: &str) -> bool {
    let trimmed = line.trim_start();
    if trimmed.starts_with('}') || trimmed.starts_with('{') {
        return false;
    }
    // Must contain an opening paren to be a callable declaration.
    if !trimmed.contains('(') {
        return false;
    }
    let starts_with_kw = [
        "fn ",
        "pub fn ",
        "async fn ",
        "pub async fn ",
        "func ",
        "public func ",
        "function ",
        "export function ",
        "async function ",
        "export async function ",
        "public ",
        "private ",
        "protected ",
        "internal ",
        "sub ",
        "void ",
        "static ",
    ];
    starts_with_kw.iter().any(|kw| trimmed.starts_with(*kw)) || trimmed.contains("->")
}

/// Returns the number of leading whitespace characters (spaces or tabs).
fn leading_indent(line: &str) -> usize {
    line.chars()
        .take_while(|c| c.is_whitespace() && *c != '\n')
        .count()
}

/// Extract text from a PDF file using pdftotext (poppler-utils).
async fn read_pdf(file_path: &str, pages: Option<&str>) -> Result<ToolResult, ToolError> {
    // Build pdftotext command with optional page range.
    let mut cmd = tokio::process::Command::new("pdftotext");
    cmd.stdin(std::process::Stdio::null());

    if let Some(page_spec) = pages {
        // Parse page spec like "1-5", "3", "10-20".
        let (first, last) = if let Some((start, end)) = page_spec.split_once('-') {
            (start.trim().to_string(), end.trim().to_string())
        } else {
            let page = page_spec.trim().to_string();
            (page.clone(), page)
        };
        cmd.arg("-f").arg(&first).arg("-l").arg(&last);
    }

    cmd.arg(file_path).arg("-");
    let output = cmd.output().await;

    match output {
        Ok(out) if out.status.success() => {
            let text = String::from_utf8_lossy(&out.stdout).to_string();
            if text.trim().is_empty() {
                Ok(ToolResult::success(format!(
                    "(PDF file: {file_path} — extracted but contains no text. \
                     May be image-based; OCR would be needed.)"
                )))
            } else {
                // Truncate very large PDFs.
                let display = if text.len() > 100_000 {
                    format!(
                        "{}\n\n(PDF truncated: {} chars total)",
                        &text[..100_000],
                        text.len()
                    )
                } else {
                    text
                };
                Ok(ToolResult::success(display))
            }
        }
        _ => {
            // pdftotext not available — report file info.
            let meta = tokio::fs::metadata(file_path).await.ok();
            let size = meta.map(|m| m.len()).unwrap_or(0);
            Ok(ToolResult::success(format!(
                "(PDF file: {file_path}, {size} bytes. \
                 Install poppler-utils for text extraction: \
                 apt install poppler-utils / brew install poppler)"
            )))
        }
    }
}

/// Render a Jupyter notebook (.ipynb) as readable text.
async fn read_notebook(file_path: &str) -> Result<ToolResult, ToolError> {
    let content = tokio::fs::read_to_string(file_path)
        .await
        .map_err(|e| ToolError::ExecutionFailed(format!("Failed to read {file_path}: {e}")))?;

    let notebook: serde_json::Value = serde_json::from_str(&content)
        .map_err(|e| ToolError::ExecutionFailed(format!("Invalid notebook JSON: {e}")))?;

    let cells = notebook
        .get("cells")
        .and_then(|v| v.as_array())
        .ok_or_else(|| ToolError::ExecutionFailed("Notebook has no 'cells' array".into()))?;

    let mut output = String::new();
    for (i, cell) in cells.iter().enumerate() {
        let cell_type = cell
            .get("cell_type")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");

        output.push_str(&format!("--- Cell {} ({}) ---\n", i + 1, cell_type));

        // Source lines.
        if let Some(source) = cell.get("source") {
            let text = match source {
                serde_json::Value::Array(lines) => lines
                    .iter()
                    .filter_map(|l| l.as_str())
                    .collect::<Vec<_>>()
                    .join(""),
                serde_json::Value::String(s) => s.clone(),
                _ => String::new(),
            };
            output.push_str(&text);
            if !text.ends_with('\n') {
                output.push('\n');
            }
        }

        // Outputs (for code cells).
        if cell_type == "code"
            && let Some(outputs) = cell.get("outputs").and_then(|v| v.as_array())
        {
            for out in outputs {
                if let Some(text) = out.get("text").and_then(|v| v.as_array()) {
                    output.push_str("Output:\n");
                    for line in text {
                        if let Some(s) = line.as_str() {
                            output.push_str(s);
                        }
                    }
                }
                if let Some(data) = out.get("data")
                    && let Some(plain) = data.get("text/plain").and_then(|v| v.as_array())
                {
                    output.push_str("Output:\n");
                    for line in plain {
                        if let Some(s) = line.as_str() {
                            output.push_str(s);
                        }
                    }
                }
            }
        }

        output.push('\n');
    }

    Ok(ToolResult::success(output))
}
