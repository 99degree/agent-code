//! Bash tool alias: "bash" (lowercase) alias for FileWriteTool's sibling BashTool.

use async_trait::async_trait;
use serde_json::json;

use super::{Tool, ToolContext, ToolResult};
use crate::error::ToolError;

pub struct BashToolAlias;

#[async_trait]
impl Tool for BashToolAlias {
    fn name(&self) -> &'static str {
        "bash"
    }

    fn description(&self) -> &'static str {
        "Executes a shell command and returns its output."
    }

    fn input_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "required": ["command"],
            "properties": {
                "command": {
                    "type": "string",
                    "description": "The command to execute"
                },
                "timeout": {
                    "type": "integer",
                    "description": "Timeout in milliseconds (max 600000)"
                },
                "description": {
                    "type": "string",
                    "description": "Description of what this command does"
                },
                "run_in_background": {
                    "type": "boolean",
                    "description": "Run the command in the background and return immediately"
                },
                "dangerouslyDisableSandbox": {
                    "type": "boolean",
                    "description": "Disable safety checks for this command. Use only when explicitly asked."
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
        let bash = super::bash::BashTool;
        bash.call(input, ctx).await
    }
}
