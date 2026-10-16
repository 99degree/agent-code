//! Edit tool: alias for FileEdit with shorter name "edit" and "path"/"old_string"/"new_string" parameters.

use async_trait::async_trait;
use serde_json::json;

use super::{Tool, ToolContext, ToolResult};
use crate::error::ToolError;

pub struct EditTool;

#[async_trait]
impl Tool for EditTool {
    fn name(&self) -> &'static str {
        "edit"
    }

    fn description(&self) -> &'static str {
        "Make edits to a file. Replaces old_string with new_string."
    }

    fn input_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "required": ["path", "old_string", "new_string"],
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Absolute or relative path to the file"
                },
                "old_string": {
                    "type": "string",
                    "description": "Text to replace"
                },
                "new_string": {
                    "type": "string",
                    "description": "Text to replace it with"
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
        // Map "path" -> "file_path" for FileEditTool
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

        let file_edit = super::file_edit::FileEditTool;
        file_edit.call(mapped_input, ctx).await
    }
}
