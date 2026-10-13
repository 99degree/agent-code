//! Write tool: alias for FileWrite with shorter name "write" and "path" parameter.

use async_trait::async_trait;
use serde_json::json;

use super::{Tool, ToolContext, ToolResult};
use crate::error::ToolError;

pub struct WriteTool;

#[async_trait]
impl Tool for WriteTool {
    fn name(&self) -> &'static str {
        "write"
    }

    fn description(&self) -> &'static str {
        "Writes content to a file, creating it if needed or overwriting if it exists."
    }

    fn input_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "required": ["path", "content"],
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Absolute or relative path to the file"
                },
                "content": {
                    "type": "string",
                    "description": "Content to write"
                }
            }
        })
    }

    fn is_read_only(&self) -> bool {
        false
    }

    async fn call(
        &self,
        input: serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<ToolResult, ToolError> {
        // Map "path" -> "file_path" for FileWriteTool
        let mut mapped_input = input.clone();

        if mapped_input.get("path").is_some()
            && mapped_input.get("file_path").is_none()
            && let Some(path_val) = mapped_input.get("path").cloned()
        {
            mapped_input
                .as_object_mut()
                .ok_or_else(|| ToolError::InvalidInput("Expected JSON object".into()))?
                .insert("file_path".to_string(), path_val);
        }

        let file_write = super::file_write::FileWriteTool;
        file_write.call(mapped_input, ctx).await
    }
}
