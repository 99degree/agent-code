#!/usr/bin/env python3
"""
Chat Message History Parser for MiMo-V2.5 tokenizer_config.json

Parses chat history JSON and formats it according to the MiMo-V2.5 chat template
rules defined in tokenizer_config.json.

Key template features:
- <|im_start|>role\ncontent<|im_end|>
- <|im_start|>system\nYou are MiMo, a helpful AI assistant engineered by Xiaomi.
- Tool formatting: <tool_call><function=name>...</function></tool_call>
- Reasoning: <think>reasoning</think>content
- Tool responses: <tool_response>...\n</tool_response>
"""

import json
import re
from typing import Dict, List, Any, Optional
from enum import Enum
from dataclasses import dataclass
from pathlib import Path

class MessageRole(Enum):
    SYSTEM = "system"
    USER = "user"
    ASSISTANT = "assistant"
    TOOL = "tool"

@dataclass
class ContentBlock:
    block_type: str
    content: str
    metadata: Dict[str, Any] = None

@dataclass
class ParsedMessage:
    role: MessageRole
    content: str
    metadata: Dict[str, Any] = None
    is_compact_summary: bool = False

class MiMoChatTemplateParser:
    """Parser for MiMo-V2.5 chat template format."""
    
    def __init__(self, template_content: str):
        self.template = template_content
        self.parse_template_rules()
    
    def parse_template_rules(self):
        """Extract parsing rules from the template."""
        # Basic message format: <|im_start|>role\ncontent<|im_end|>
        self.message_pattern = r'<\|im_start\>(\w+)\n(.*?)<\|im_end\>'
        
        # Tool call format: <tool_call><function=name>...</function></tool_call>
        self.tool_use_pattern = r'<tool_call>\s*<function=(\w+)>(.*?)</function>\s*</tool_call>'
        
        # Reasoning format: <think>reasoning</think>content
        self.reasoning_pattern = r'<think>(.*?)</think>(.*?)'
        
        # Tool response format: <tool_response>...\n</tool_response>
        self.tool_response_pattern = r'<tool_response>(.*?)</tool_response>'
        
        # Content inside messages (could have multiple blocks)
        self.content_extractor_pattern = r'<\|im_start\>(?:\w+)\n(.*?)(?:\n<\|im_end\>|\Z)'

    def parse_template(self, messages: List[Dict]) -> str:
        """Convert chat messages to MiMo-V2.5 format."""
        result = []
        
        # Handle system message (if present)
        system_content = self._extract_system_content(messages)
        if system_content:
            result.append(f"<|im_start|>system\n{system_content}")
        else:
            result.append("<|im_start|>system\nYou are MiMo, a helpful AI assistant engineered by Xiaomi.")
        
        # Process messages in order
        for msg in messages:
            msg_type = msg.get('type')
            content_blocks = msg.get('content', [])
            
            if msg_type == 'system':
                self._parse_system_message(msg, result)
            elif msg_type == 'user':
                self._parse_user_message(msg, result)
            elif msg_type == 'assistant':
                self._parse_assistant_message(msg, result)
            elif msg_type == 'tool':
                self._parse_tool_message(msg, result)
        
        # Format based on template rules
        return self._format_to_template(result, messages)
    
    def _extract_system_content(self, messages: List[Dict]) -> Optional[str]:
        """Extract first system message content for preamble."""
        for msg in messages:
            if msg.get('type') == 'system':
                return self._extract_content_text(msg)
        return None
    
    def _extract_content_text(self, msg: Dict) -> str:
        """Extract text content from message blocks."""
        if isinstance(msg.get('content'), str):
            return msg['content']
        elif isinstance(msg.get('content'), list):
            # Concatenate text from all text blocks
            text_parts = []
            for block in msg['content']:
                if isinstance(block, dict):
                    if block.get('type') == 'text':
                        text_parts.append(block.get('text', ''))
                    elif block.get('type') == 'tool_result':
                        text_parts.append(block.get('content', ''))
            return ' '.join(text_parts)
        return ''
    
    def _parse_system_message(self, msg: Dict, result: List[str]):
        """Parse system message."""
        if msg.get('subtype') == 'compact_boundary':
            # Compact boundary messages indicate continuation
            content = self._extract_content_text(msg)
            if content.startswith('[Conversation compacted. Summary:'):
                # Extract just the summary part
                import re
                summary_match = re.search(r'Summary: (.+)', content)
                if summary_match:
                    result.append(f"<|im_start|>system\n{summary_match.group(1)}")
    
    def _parse_user_message(self, msg: Dict, result: List[str]):
        """Parse user message according to template rules."""
        content_text = self._extract_content_text(msg)
        # Add user turn
        result.append(f"<|im_start|>user\n{content_text}")
    
    def _parse_assistant_message(self, msg: Dict, result: List[str]):
        """Parse assistant message with reasoning and tool calls."""
        content_blocks = msg.get('content', [])
        
        if isinstance(content_blocks, str):
            # Single string message
            result.append(f"<|im_start|>assistant\n{content_blocks}")
            return
        
        # Process content blocks in order
        reasoning = None
        tool_uses = []
        text_content = []
        
        for block in content_blocks:
            if isinstance(block, dict):
                block_type = block.get('type')
                if block_type == 'text':
                    text_content.append(block.get('text', ''))
                elif block_type == 'tool_use':
                    tool_uses.append(block)
                elif block_type == 'think':
                    reasoning = block.get('text', '')
        
        # Build assistant message according to template
        assistant_line = "<|im_start|>assistant\n"
        
        # Add reasoning if present and enabled
        if reasoning:
            assistant_line += f"<think>{reasoning}</think>"
        
        # Add text content
        if text_content:
            assistant_line += ' '.join(text_content)
        
        # Add tool calls
        for tool_use in tool_uses:
            tool_name = tool_use.get('name')
            tool_input = json.dumps(tool_use.get('input', {}), indent=2)
            assistant_line += f"\n<tool_call>\n<function={tool_name}>\n{tool_input}\n</function>\n</tool_call>"
        
        result.append(assistant_line)
    
    def _parse_tool_message(self, msg: Dict, result: List[str]):
        """Parse tool response message."""
        content_blocks = msg.get('content', [])
        
        if isinstance(content_blocks, str):
            # String tool response
            result.append(f"<tool_response>\n{content_blocks}\n</tool_response>")
            return
        
        # For list format
        tool_response_text = []
        for block in content_blocks:
            if isinstance(block, dict):
                if block.get('type') == 'text':
                    tool_response_text.append(block.get('text', ''))
                elif block.get('type') == 'tool_result':
                    # Check if it's a cleared placeholder
                    if block.get('content') == '[Old tool result cleared]':
                        # Microcompact placeholder
                        tool_response_text.append("[Old tool result cleared]")
                    else:
                        tool_response_text.append(block.get('content', ''))
        
        response = '\n'.join(tool_response_text)
        result.append(f"<tool_response>\n{response}\n</tool_response>")
    
    def _format_to_template(self, result: List[str], original_messages: List[Dict]) -> str:
        """Format parsed messages into final MiMo-V2.5 template format."""
        # Join with newlines
        output = '\n'.join(result)
        
        # Add closing system tag if it's not there
        if '<|im_end|>' not in output:
            output += '\n<|im_end|>'
        
        # Handle tools section (simplified - add if there were tools in the session)
        has_tools = any(
            isinstance(block, dict) and block.get('type') == 'tool_use'
            for msg in original_messages
            for block in msg.get('content', [])
            if isinstance(block, dict)
        )
        
        if has_tools:
            # Add tools section as per template
            output += "\n\n# Tools\n\nYou may call one or more functions to assist with the user query.\n\nYou have access to the following functions:\n\n<tools>"
            # In real implementation, would enumerate available tools
            output += "\n</tools>"
        
        return output

class ChatHistoryConverter:
    """Main converter for chat history JSON files."""
    
    def __init__(self, template_path: Optional[str] = None):
        if template_path and Path(template_path).exists():
            with open(template_path) as f:
                template_data = json.load(f)
                self.chat_template = template_data.get('chat_template', '')
        else:
            self.chat_template = ''
        
        self.parser = MiMoChatTemplateParser(self.chat_template) if self.chat_template else None
    
    def convert_session(self, session_data: Dict) -> Dict[str, Any]:
        """Convert a session JSON to formatted chat output."""
        if not self.parser:
            return {"error": "No template available"}
        
        messages = session_data.get('messages', [])
        formatted_output = self.parser.parse_template(messages)
        
        return {
            "session_id": session_data.get('id'),
            "model": session_data.get('model'),
            "timestamp": session_data.get('created_at'),
            "formatted_chat": formatted_output,
            "message_count": len(messages),
            "analysis": self._analyze_session(session_data)
        }
    
    def _analyze_session(self, session_data: Dict) -> Dict[str, Any]:
        """Analyze session for insights about message patterns."""
        messages = session_data.get('messages', [])
        analysis = {
            "message_types": {},
            "content_blocks": {},
            "has_compact_summaries": False,
            "tool_usage_patterns": {},
            "turn_sequence": []
        }
        
        for msg in messages:
            msg_type = msg.get('type')
            analysis["message_types"][msg_type] = analysis["message_types"].get(msg_type, 0) + 1
            
            # Check for compact summary
            if msg.get('type') == 'system' and msg.get('subtype') == 'compact_boundary':
                analysis["has_compact_summaries"] = True
            
            # Track turn sequence
            if msg_type == 'assistant':
                content_blocks = msg.get('content', [])
                text_blocks = sum(1 for b in content_blocks if isinstance(b, dict) and b.get('type') == 'text')
                tool_uses = sum(1 for b in content_blocks if isinstance(b, dict) and b.get('type') == 'tool_use')
                analysis["turn_sequence"].append(f"assistant:{text_blocks}text,{tool_uses}tools")
            elif msg_type == 'user':
                analysis["turn_sequence"].append("user")
            elif msg_type == 'system':
                analysis["turn_sequence"].append(f"system:{msg.get('subtype')}")
        
        return analysis

def main():
    """Example usage with debugging."""
    # Test with the session file
    session_path = "/data/data/com.termux/files/home/agent-code/.config/agent-code/sessions/273c5de0.json"
    
    if not Path(session_path).exists():
        print(f"Session file not found: {session_path}")
        return
    
    # Find tokenizer_config.json
    template_path = "/data/data/com.termux/files/home/agent-code/tokenizer_config.json"
    if not Path(template_path).exists():
        print("Could not find tokenizer_config.json with template")
        # Try to find it
        from pathlib import Path
        for p in Path("/data/data/com.termux/files/home/agent-code").rglob("*.json"):
            try:
                with open(p) as f:
                    if "chat_template" in f.read():
                        template_path = str(p)
                        print(f"Found template at: {template_path}")
                        break
            except:
                pass
    
    converter = ChatHistoryConverter(template_path)
    
    with open(session_path) as f:
        session_data = json.load(f)
    
    print("=" * 80)
    print("SESSION ANALYSIS")
    print("=" * 80)
    print(f"Session ID: {session_data.get('id')}")
    print(f"Model: {session_data.get('model')}")
    print(f"Total messages: {len(session_data.get('messages', []))}")
    print(f"Turn count: {session_data.get('turn_count')}")
    
    analysis = converter.convert_session(session_data)
    print("\n" + "=" * 80)
    print("FORMATTED CHAT OUTPUT")
    print("=" * 80)
    print(analysis['formatted_chat'][:2000] + "..." if len(analysis['formatted_chat']) > 2000 else analysis['formatted_chat'])
    
    print("\n" + "=" * 80)
    print("SESSION ANALYSIS")
    print("=" * 80)
    print(f"Message types: {analysis['analysis']['message_types']}")
    print(f"Has compact summaries: {analysis['analysis']['has_compact_summaries']}")
    print(f"Turn sequence (first 10): {' '.join(analysis['analysis']['turn_sequence'][:10])}...")

if __name__ == "__main__":
    main()