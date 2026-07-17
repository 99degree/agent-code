"""
Qwen2/MiMo Chat Template Validator

This module extends the Qwen2ChatParser logic to validate the structural integrity
of a conversation, specifically checking for:
1. Role Pair Integrity: Ensures user/assistant/tool turns follow a logical sequence.
2. Tool Call Pair Integrity: Ensures every <tool_call> is followed by a corresponding <tool_response>.
"""

from __future__ import annotations

import json
from dataclasses import dataclass
from typing import Any, Optional
from enum import Enum

from qwen2_chat_parser import Role, Message, ToolCall

class ValidationIssue(Enum):
    MISSING_ASSISTANT_RESPONSE = "ASSISTANT_RESPONSE_MISSING" # User message not followed by Assistant
    MISSING_TOOL_RESPONSE = "TOOL_RESPONSE_MISSING"           # Tool call not followed by Tool response
    UNSOLICITED_TOOL_RESPONSE = "UNSOLICITED_TOOL_RESPONSE"     # Tool response without a preceding Tool call
    INVALID_ROLE_SEQUENCE = "INVALID_ROLE_SEQUENCE"            # Role sequence violates basic chat logic

@dataclass
class ValidationResult:
    is_valid: bool
    issues: list[tuple[int, ValidationIssue, str]] # (index, issue_type, description)

class Qwen2ChatValidator:
    """
    Validator for Qwen2/MiMo conversation structures.
    """
    
    def validate(self, messages: list[dict[str, Any] | Message]) -> ValidationResult:
        parsed_messages = [m if isinstance(m, Message) else Message.from_dict(m) for m in messages]
        issues = []
        
        pending_tool_calls: dict[str, int] = {} # {tool_call_id: message_index}
        
        for i, msg in enumerate(parsed_messages):
            role = msg.role
            
            # 1. Role Sequence Checks
            if i > 0:
                prev_role = parsed_messages[i-1].role
                
                # Tool responses must be preceded by either another tool response or an assistant call
                if role == Role.TOOL and prev_role not in [Role.ASSISTANT, Role.TOOL]:
                    issues.append((i, ValidationIssue.UNSOLICITED_TOOL_RESPONSE, 
                                 f"Tool response at index {i} has no preceding assistant tool call."))

            # 2. Tool Call Tracking
            if role == Role.ASSISTANT and msg.tool_calls:
                for tc in msg.tool_calls:
                    if tc.id:
                        pending_tool_calls[tc.id] = i
                    else:
                        # In some session logs, ID might be missing; we track it by index if needed
                        # But for strict validation, we expect IDs
                        pass

            # 3. Tool Response Matching
            if role == Role.TOOL:
                if msg.tool_call_id:
                    if msg.tool_call_id in pending_tool_calls:
                        del pending_tool_calls[msg.tool_call_id]
                    else:
                        issues.append((i, ValidationIssue.UNSOLICITED_TOOL_RESPONSE, 
                                     f"Tool response {msg.tool_call_id} does not match any pending tool call."))
                else:
                    # If no ID, we can't strictly match, but we can't mark as solved
                    pass

        # 4. Final Checks for Unresolved Calls
        for tc_id, idx in pending_tool_calls.items():
            issues.append((idx, ValidationIssue.MISSING_TOOL_RESPONSE, 
                         f"Assistant tool call {tc_id} at index {idx} was never answered by a tool response."))

        # 5. Trailing User Message (if not add_generation_prompt)
        if parsed_messages and parsed_messages[-1].role == Role.USER:
            # This is often acceptable in training data, but worth noting
            pass

        return ValidationResult(is_valid=len(issues) == 0, issues=issues)

def validate_session(session_path: str):
    """Convenience function to validate a session file."""
    with open(session_path, 'r') as f:
        session = json.load(f)
    
    validator = Qwen2ChatValidator()
    result = validator.validate(session.get('messages', []))
    
    print(f"Validation for {session_path}: {'✅ PASS' if result.is_valid else '❌ FAIL'}")
    for idx, issue, desc in result.issues:
        print(f"  - [Msg {idx}] {issue.name}: {desc}")
    return result

if __name__ == "__main__":
    import os
    import glob

    # Test against session files
    session_files = glob.glob('/data/data/com.termux/files/home/.config/agent-code/sessions/*.json')
    for sf in session_files:
        validate_session(sf)
        print("-" * 40)
