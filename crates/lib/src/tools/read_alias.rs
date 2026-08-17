//! Read tool: alias for FileRead with shorter name "read" and "path" parameter.

use async_trait::async_trait;
use serde_json::json;

use super::{Tool, ToolContext, ToolResult};
use crate::error::ToolError;

pub struct ReadTool;

#[async_trait]
impl Tool for ReadTool {
    fn name(&self) -> &'static str {
        "read"
    }

    fn description(&self) -> &'static str {
        "Reads a file from the filesystem. Returns contents with line numbers."
    }

    fn input_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "required": ["path"],
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Absolute or relative path to the file"
                },
                "offset": {
                    "type": "integer",
                    "description": "Line number to start reading from (1-based)"
                },
                "limit": {
                    "type": "integer",
                    "description": "Number of lines to read"
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

    async fn call(
        &self,
        input: serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<ToolResult, ToolError> {
        // Map "path" -> "file_path" for FileReadTool
        let mut mapped_input = input.clone();

        // If "path" exists but "file_path" doesn't, map it
        if mapped_input.get("path").is_some()
            && mapped_input.get("file_path").is_none()
            && let Some(path_val) = mapped_input.get("path").cloned()
        {
            mapped_input
                .as_object_mut()
                .ok_or_else(|| ToolError::InvalidInput("Expected JSON object".into()))?
                .insert("file_path".to_string(), path_val);
        }

        let file_read = super::file_read::FileReadTool;
        file_read.call(mapped_input, ctx).await
    }
}
