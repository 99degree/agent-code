#!/usr/bin/env python3
"""
Chat Message History Parser for MiMo-V2.5

This script parses chat history JSON files from agent-code sessions and converts
them according to the MiMo-V2.5 chat template rules defined in tokenizer_config.json.

The template format uses:
- <|im_start>role\ncontent<|im_end|> for messages
- <tool_call><function=name>...</function></tool_call> for tool calls
- <think>reasoning</think>content for reasoning
- <tool_response>...\n</tool_response> for tool results

Key template patterns:
1. System messages as preamble: "You are MiMo, a helpful AI assistant engineered by Xiaomi."
2. Tools section if any tools are used
3. Message sequence with proper role tags
"""

import json
import re
from pathlib import Path
from typing import Dict, List, Any, Optional

def extract_chat_template_from_config():
    """Extract the chat template from the tokenizer_config.json"""
    template_text = """{%- if not add_generation_prompt is defined -%}
    {%- set add_generation_prompt = false -%}
{%- endif -%}
{%- if not enable_thinking is defined -%}
    {%- set enable_thinking = true -%}
{%- endif -%}
{%- if not keep_all_reasoning is defined -%}
    {%- set keep_all_reasoning = true -%}
{%- endif -%}
{%- macro render_extra_keys(json_dict, handled_keys) -%}
    {%- if json_dict is mapping %}
        {%- for json_key in json_dict if json_key not in handled_keys %}
            {%- if json_dict[json_key] is mapping or (json_dict[json_key] is sequence and json_dict[json_key] is not string) %}
                {{- '\\n<' ~ json_key ~ '>' ~ (json_dict[json_key] | tojson | safe) ~ '</' ~ json_key ~ '>' }}
            {%- else %}
                {{-'\\n<' ~ json_key ~ '>' ~ (json_dict[json_key] | string) ~ '</' ~ json_key ~ '>' }}
            {%- endif %}
        {%- endfor %}
    {%- endif %}
{%- endmacro -%}
{%- macro render_content(message_content) -%}
    {%- if message_content is string -%}
        {{- message_content -}}
    {%- else -%}
        {%- for content in message_content -%}
            {%- if content['type'] == 'image' or 'image' in content or 'image_url' in content -%}
                {{- '<|vision_start|><|image_pad|><|vision_end|>' -}}
            {%- elif content['type'] == 'audio' or 'audio' in content or 'audio_url' in content -%}
                {{- '<|mimo_audio_start|><|audio_pad|><|mimo_audio_end|>' -}}
            {%- elif content['type'] == 'video' or 'video' in content or 'video_url' in content -%}
                {{- '<|vision_start|><|video_pad|><|vision_end|>' -}}
            {%- elif 'text' in content -%}
                {{- content['text'] -}}
            {%- endif -%}
        {%- endfor -%}
    {%- endif -%}
{%- endmacro -%}
{%- if messages[0]["role"] == "system" %}
    {%- set system_message = messages[0]["content"] %}
    {%- set loop_messages = messages[1:] %}
{%- else %}
    {%- set loop_messages = messages %}
{%- endif %}
{%- set ns = namespace(last_user_index=-1) %}
{%- for m in loop_messages %}
    {%- if m.role == 'user' %}
        {%- set ns.last_user_index = loop.index0 -%}
    {%- endif %}
{%- endfor %}
{%- if not tools is defined %}
    {%- set tools = [] %}
{%- endif %}
{%- if system_message is defined %}
    {{- "<|im_start|>system\\n" + render_content(system_message) }}
{%- else %}
    {{- "<|im_start|>system\\nYou are MiMo, a helpful AI assistant engineered by Xiaomi." }}
{%- endif %}
{%- if tools is iterable and tools | length > 0 %}
    {{- "\\n\\n# Tools\\n\\nYou may call one or more functions to assist with the user query.\\n\\nYou have access to the following functions:\\n\\n<tools>" }}
    {%- for tool in tools %}
        {%- if tool.function is defined %}
            {%- set tool = tool.function %}
        {%- endif %}
        {{- "\\n<function>\\n<name>" ~ tool.name ~ "</name>" }}
        {%- if tool.description is defined %}
            {{- '\\n<description>' ~ (tool.description | trim) ~ '</description>' }}
        {%- endif %}
        {{- '\\n<parameters>' }}
        {%- if tool.parameters is defined and tool.parameters is mapping and tool.parameters.properties is defined and tool.parameters.properties is mapping %}
            {%- for param_name, param_fields in tool.parameters.properties|items %}
                {{- '\\n<parameter>' }}
                {{- '\\n<name>' ~ param_name ~ '</name>' }}
                {%- if param_fields.type is defined %}
                    {{- '\\n<type>' ~ (param_fields.type | string) ~ '</type>' }}
                {%- endif %}
                {%- if param_fields.description is defined %}
                    {{- '\\n<description>' ~ (param_fields.description | trim) ~ '</description>' }}
                {%- endif %}
                {%- set handled_keys = ['name', 'type', 'description'] %}
                {{- render_extra_keys(param_fields, handled_keys) }}
                {{- '\\n</parameter>' }}
            {%- endfor %}
        {%- endif %}
        {%- set handled_keys = ['type', 'properties'] %}
        {{- render_extra_keys(tool.parameters, handled_keys) }}
        {{- '\\n</parameters>' }}
        {%- set handled_keys = ['type', 'name', 'description', 'parameters'] %}
        {{- render_extra_keys(tool, handled_keys) }}
        {{- '\\n</function>' }}
    {%- endfor %}
    {{- "\\n</tools>" }}
    {{- '\\n\\nFor each function call, output the function name and arguments in the following format:\\\n<tool_call>\\\\n<function=example_function_name>\\\\n<parameter=example_parameter_1>value_1</parameter>\\\\n<parameter=example_parameter_2>This is the value for the second parameter\\\\nthat can span\\\\nmultiple lines</parameter>\\\\n</function>\\\\n</tool_call>\\\\n\\\\n<IMPORTANT>\\\\n- Function calls MUST follow the specified format: an inner <function=...></function> block must be nested within <tool_call></tool_call> XML tags\\\\n- DO NOT use function calls inside <think></think> tags.\\\\n- The value enclosed between parameter tags is preserved exactly as-is, including newlines and spaces.\\\\n</IMPORTANT>' }}
{%- endif %}
{{- '<|im_end|>' }}
{%- for message in loop_messages %}
    {%- if message.content is string %}
        {%- set content = message.content %}
    {%- else %}
        {%- set content = render_content(message.content) %}
    {%- endif %}
    {%- if message.role == "assistant" %}
        {%- if message.reasoning_content is string %}
            {%- set reasoning_content = message.reasoning_content %}
        {%- else %}
            {%- set reasoning_content = '' %}
            {%- if '</think>' in content %}
                {%- set reasoning_content = content.split('</think>')[0].split('<think>')[-1] %}
                {%- set content = content.split('</think>')[-1] %}
            {%- endif %}
        {%- endif %}
        {%- if (keep_all_reasoning or loop.index0 > ns.last_user_index) and reasoning_content -%}
            {{- '<|im_start>>' + message.role + '\\\\n<think>' + reasoning_content + '</think>' + content }}
        {%- else %}
            {{- '<|im_start>>' + message.role + '\\\\n<think></think>' + content }}
        {%- endif %}
        {%- if message.tool_calls is defined and message.tool_calls is iterable and message.tool_calls | length > 0 %}
            {%- for tool_call in message.tool_calls %}
                {%- if tool_call.function is defined %}
                    {%- set tool_call = tool_call.function %}
                {%- endif %}
                {{- '<tool_call>\\\\n<function=' + tool_call.name + '>\\\\n' }}
                {%- if tool_call.arguments is defined %}
                    {%- for args_name, args_value in tool_call.arguments|items %}
                        {{- '<parameter=' + args_name + '>' }}
                        {%- set args_value = args_value | tojson | safe if args_value is mapping or (args_value is sequence and args_value is not string) else args_value | string %}
                        {{- args_value }}
                        {{- '</parameter>\\\\n' }}
                    {%- endfor %}
                {%- endif %}
                {{- '</function>\\\\n</tool_call>' }}
            {%- endfor %}
        {%- endif %}
        {{- '<|im_end|>' }}
    {%- elif message.role == "user" %}
        {{- '<|im_start>>' + message.role + '\\\\n' + render_content(message.content) + '<|im_end|>' }}
    {%- elif message.role == "system" %}
        {{- '<|im_start>>' + message.role + '\\\\n' + render_content(message.content) + '<|im_end|>' }}
    {%- elif message.role == "tool" %}
        {%- if loop.previtem and loop.previtem.role != "tool" %}
            {{- '<|im_start|>tool\\\\n' }}
        {%- endif %}
        {{- '<tool_response>\\\\n' }}
        {{- render_content(message.content) }}
        {{- '\\\\n</tool_response>\\\\n' }}
        {%- if not loop.last and loop.nextitem.role != "tool" %}
            {{- '<|im_end|>' }}
        {%- elif loop.last %}
            {{- '<|im_end|>' }}
        {%- endif %}
    {%- else %}
        {{- '<|im_start>>' + message.role + '\\\\n' + render_content(message.content) + '<|im_end|>' }}
    {%- endif %}
{%- endfor %}
{%- if add_generation_prompt %}
    {{- '<|im_start|>assistant\\\\n' }}
    {%- if not enable_thinking -%}
        {{- '<think></think>' -}}
    {%- else -%}
        {{- '' -}}
    {%- endif -%}
{%- endif %}
"""
    return template_text

class MiMoTemplateParser:
    """Parser for MiMo-V2.5 chat template"""
    
    def __init__(self, template_text: str):
        self.template = template_text
        self.parse_template_rules()
    
    def parse_template_rules(self):
        """Extract key template patterns for parsing"""
        # Core message pattern: <|im_start>role\ncontent<|im_end|>
        self.message_pattern = r'<\|im_start\>(\w+)\n(.*?)<\|im_end\>'
        
        # Tool call pattern: <tool_call><function=name>...</function></tool_call>
        self.tool_call_pattern = r'<tool_call>.*?<function=(\w+)>.*?</function>.*?</tool_call>'
        
        # Reasoning pattern: <think>...</think>
        self.reasoning_pattern = r'<think>(.*?)</think>'
        
        # Tool response pattern: <tool_response>...</tool_response>
        self.tool_response_pattern = r'<tool_response>(.*?)</tool_response>'
        
        # Role prefixes
        self.roles = ['system', 'user', 'assistant', 'tool']
    
    def convert_chat_history(self, messages: List[Dict]) -> str:
        """Convert chat messages to MiMo-V2.5 template format"""
        if not messages:
            return ""
        
        output_lines = []
        
        # Handle system message (if present)
        system_content = self._extract_system_content(messages)
        if system_content:
            output_lines.append(f"<|im_start|>system\n{system_content}")
        else:
            output_lines.append("<|im_start|>system\nYou are MiMo, a helpful AI assistant engineered by Xiaomi.")
        
        # Process messages in order
        for msg in messages:
            msg_type = msg.get('type')
            converted = self._convert_message(msg)
            output_lines.extend(converted)
        
        # Join with newlines and add final im_end if needed
        result = '\n'.join(output_lines)
        if '<|im_end|>' not in result:
            result += '\n<|im_end|>'
        
        return result
    
    def _extract_system_content(self, messages: List[Dict]) -> Optional[str]:
        """Extract system content from first system message"""
        for msg in messages:
            if msg.get('type') == 'system':
                return self._extract_text_content(msg)
        return None
    
    def _extract_text_content(self, msg: Dict) -> str:
        """Extract text content from a message"""
        if isinstance(msg.get('content'), str):
            return msg['content']
        elif isinstance(msg.get('content'), list):
            text_parts = []
            for block in msg['content']:
                if isinstance(block, dict):
                    if block.get('type') == 'text':
                        text_parts.append(block.get('text', ''))
                    elif block.get('type') == 'tool_result':
                        # Handle microcompact placeholders
                        content = block.get('content', '')
                        if content == '[Old tool result cleared]':
                            text_parts.append('[Old tool result cleared]')
                        else:
                            text_parts.append(content)
            return ' '.join(text_parts)
        return ''
    
    def _convert_message(self, msg: Dict) -> List[str]:
        """Convert a single message to template format"""
        msg_type = msg.get('type')
        content_text = self._extract_text_content(msg)
        
        if msg_type == 'system':
            # Handle system messages
            if content_text.startswith('[Conversation compacted. Summary:'):
                # Extract just the summary part
                summary_match = re.search(r'Summary: (.+)', content_text)
                if summary_match:
                    return [f"<|im_start|>system\n{summary_match.group(1)}"]
            return [f"<|im_start|>system\n{content_text}"]
        
        elif msg_type == 'user':
            # User messages
            return [f"<|im_start|>user\n{content_text}", "<|im_end|>"]
        
        elif msg_type == 'assistant':
            # Assistant messages - could be complex
            return self._convert_assistant_message(msg)
        
        elif msg_type == 'tool':
            # Tool responses
            return [f"<tool_response>\n{content_text}\n</tool_response>", "<|im_end|>"]
        
        return []
    
    def _convert_assistant_message(self, msg: Dict) -> List[str]:
        """Convert assistant message with complex structure"""
        content_blocks = msg.get('content', [])
        
        if isinstance(content_blocks, str):
            # Simple text message
            return [f"<|im_start|>assistant\n{content_blocks}", "<|im_end|>"]
        
        # Build assistant message
        lines = ["<|im_start|>assistant\n"]
        
        # Extract reasoning and tool calls from blocks
        reasoning = None
        text_parts = []
        tool_calls = []
        
        for block in content_blocks:
            if isinstance(block, dict):
                block_type = block.get('type')
                if block_type == 'text':
                    text_parts.append(block.get('text', ''))
                    # Check for think tags in text
                    text_content = block.get('text', '')
                    think_match = re.search(r'<think>(.*?)</think>(.*)', text_content, re.DOTALL)
                    if think_match:
                        reasoning = think_match.group(1)
                        # Add remaining text after think tags
                        remaining_text = think_match.group(2)
                        if remaining_text.strip():
                            text_parts.append(remaining_text.strip())
                
                elif block_type == 'tool_use':
                    tool_calls.append(block)
                
                elif block_type == 'think':
                    reasoning = block.get('text', '')
        
        # Add reasoning if present
        if reasoning:
            lines.append(f"<think>{reasoning}</think>")
        
        # Add text content
        if text_parts:
            lines.append(' '.join(text_parts))
        
        # Add tool calls
        for tool_use in tool_calls:
            tool_name = tool_use.get('name', 'unknown')
            tool_input = json.dumps(tool_use.get('input', {}), indent=2)
            lines.append(f"<tool_call>")
            lines.append(f"<function={tool_name}>")
            lines.append(tool_input)
            lines.append("</function>")
            lines.append("</tool_call>")
        
        lines.append("<|im_end|>")
        return lines

class SessionAnalyzer:
    """Analyze session messages for patterns and compliance"""
    
    @staticmethod
    def analyze_template_compliance(messages: List[Dict]) -> Dict[str, Any]:
        """Analyze how well messages comply with template structure"""
        analysis = {
            'template_patterns': {},
            'compliance_score': 0,
            'issues': [],
            'statistics': {}
        }
        
        # Check for key template patterns
        patterns = {
            'message_starts': 0,
            'im_end_tags': 0,
            'tool_calls': 0,
            'think_blocks': 0,
            'system_preamble': False,
            'tool_section': False,
            'compact_summaries': 0
        }
        
        # Check for template compliance
        for msg in messages:
            content = str(msg.get('content', ''))
            
            # Count pattern occurrences
            if '<|im_start>' in content:
                patterns['message_starts'] += 1
            
            if '<|im_end|>' in content:
                patterns['im_end_tags'] += 1
            
            if '<tool_call>' in content:
                patterns['tool_calls'] += 1
            
            if '<think>' in content:
                patterns['think_blocks'] += 1
            
            # Check for system preamble (first system message)
            if msg.get('type') == 'system' and not patterns['system_preamble']:
                analysis['first_system_message'] = msg.get('content', '')[:200]
            
            # Check for compact summary messages
            if (msg.get('type') == 'system' and 
                msg.get('subtype') == 'compact_boundary' and
                isinstance(msg.get('content'), list)):
                patterns['compact_summaries'] += 1
        
        # Check tools section (simplified check)
        for msg in messages:
            if (msg.get('type') == 'system' and 
                'You may call one or more functions' in str(msg.get('content', '')))):
                patterns['tool_section'] = True
                break
        
        analysis['template_patterns'] = patterns
        
        # Calculate compliance score (simplified)
        total_checks = len(messages) * 5  # Approximate number of checks
        pattern_matches = (
            patterns['message_starts'] + patterns['im_end_tags'] + 
            patterns['tool_calls'] + patterns['think_blocks']
        )
        analysis['compliance_score'] = min(100, int((pattern_matches / total_checks) * 100)) if total_checks > 0 else 0
        
        # Identify potential issues
        if patterns['message_starts'] == 0:
            analysis['issues'].append("No template message start patterns found")
        
        if patterns['im_end_tags'] < messages.count('user') + messages.count('assistant'):
            analysis['issues'].append("Missing end tags for some messages")
        
        if patterns['compact_summaries'] == 0:
            analysis['issues'].append("No compact summary messages found")
        
        analysis['statistics'] = {
            'total_messages': len(messages),
            'user_messages': sum(1 for msg in messages if msg.get('type') == 'user'),
            'assistant_messages': sum(1 for msg in messages if msg.get('type') == 'assistant'),
            'system_messages': sum(1 for msg in messages if msg.get('type') == 'system'),
            'tool_messages': sum(1 for msg in messages if msg.get('type') == 'tool'),
        }
        
        return analysis

class MiMoChatHistoryConverter:
    """Main converter for chat history parsing"""
    
    def __init__(self):
        self.template = extract_chat_template_from_config()
        self.parser = MiMoTemplateParser(self.template)
        self.analyzer = SessionAnalyzer()
    
    def convert_session(self, session_data: Dict[str, Any]) -> Dict[str, Any]:
        """Convert a session to MiMo-V2.5 format"""
        session_id = session_data.get('id', 'unknown')
        model = session_data.get('model', 'unknown')
        messages = session_data.get('messages', [])
        
        # Analyze session compliance
        analysis = self.analyzer.analyze_template_compliance(messages)
        
        # Convert chat history
        formatted_chat = self.parser.convert_chat_history(messages)
        
        return {
            'session_info': {
                'id': session_id,
                'model': model,
                'created_at': session_data.get('created_at'),
                'updated_at': session_data.get('updated_at'),
                'turn_count': session_data.get('turn_count'),
                'total_cost_usd': session_data.get('total_cost_usd'),
                'total_input_tokens': session_data.get('total_input_tokens'),
                'total_output_tokens': session_data.get('total_output_tokens')
            },
            'formatted_chat': formatted_chat,
            'analysis': analysis,
            'meta': {
                'template_source': 'tokenizer_config.json',
                'conversion_time': None  # Would add timestamp in real implementation
            }
        }
    
    def convert_session_file(self, file_path: Path) -> Dict[str, Any]:
        """Convert a session from JSON file"""
        with open(file_path) as f:
            session_data = json.load(f)
        
        return self.convert_session(session_data)

def demo_converter():
    """Demonstrate the converter with the sample session"""
    session_path = Path("/data/data/com.termux/files/home/.config/agent-code/sessions/273c5de0.json")
    
    if not session_path.exists():
        print(f"Session file not found: {session_path}")
        return
    
    converter = MiMoChatHistoryConverter()
    
    print("=" * 80)
    print("MiMo-V2.5 Chat History Converter - Session Analysis")
    print("=" * 80)
    
    # Convert the session
    result = converter.convert_session_file(session_path)
    
    # Print session info
    print("\nSession Information:")
    print(f"  ID: {result['session_info']['id']}")
    print(f"  Model: {result['session_info']['model']}")
    print(f"  Created: {result['session_info']['created_at']}")
    print(f"  Total Messages: {result['analysis']['statistics']['total_messages']}")
    print(f"  Turn Count: {result['session_info']['turn_count']}")
    
    # Print analysis
    print("\nTemplate Compliance Analysis:")
    patterns = result['analysis']['template_patterns']
    print(f"  Message start patterns found: {patterns['message_starts']}")
    print(f"  End tags found: {patterns['im_end_tags']}")
    print(f"  Tool calls found: {patterns['tool_calls']}")
    print(f"  Think blocks found: {patterns['think_blocks']}")
    print(f"  Compact summaries: {patterns['compact_summaries']}")
    print(f"  Compliance Score: {result['analysis']['compliance_score']}%")
    
    # Show first system message content snippet
    if 'first_system_message' in result['analysis']:
        sys_content = result['analysis']['first_system_message']
        print(f"\nFirst System Message (first 200 chars):")
        print(f"  ...{sys_content}...")
    
    # Display formatted chat (first 2000 chars)
    print("\n" + "=" * 80)
    print("Formatted Chat History (first 2000 chars):")
    print("=" * 80)
    formatted = result['formatted_chat']
    print(formatted[:2000])
    
    if len(formatted) > 2000:
        print("...\n(truncated - full output available in result['formatted_chat'])")
    
    # Print issues if any
    issues = result['analysis']['issues']
    if issues:
        print("\n" + "=" * 80)
        print("Potential Issues:")
        print("=" * 80)
        for issue in issues:
            print(f"  - {issue}")
    
    # Return result for further processing
    return result

if __name__ == "__main__":
    demo_converter()