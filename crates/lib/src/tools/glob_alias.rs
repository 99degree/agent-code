//! Glob tool alias: "glob" (lowercase) alias for GlobTool.

use async_trait::async_trait;
use serde_json::json;

use super::{Tool, ToolContext, ToolResult};
use crate::error::ToolError;

pub struct GlobToolAlias;

#[async_trait]
impl Tool for GlobToolAlias {
    fn name(&self) -> &'static str {
        "glob"
    }

    fn description(&self) -> &'static str {
        "Finds files matching a glob pattern. Returns paths sorted by modification time."
    }

    fn input_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "required": ["pattern"],
            "properties": {
                "pattern": {
                    "type": "string",
                    "description": "Glob pattern (e.g., \"**/*.rs\", \"src/**/*.toml\")"
                },
                "path": {
                    "type": "string",
                    "description": "Directory to search in (defaults to cwd)"
                }
            }
        })
    }

    fn is_read_only(&self) -> bool {
        true
    }

    async fn call(
        &self,
        input: serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<ToolResult, ToolError> {
        let glob = super::glob::GlobTool;
        glob.call(input, ctx).await
    }
}
