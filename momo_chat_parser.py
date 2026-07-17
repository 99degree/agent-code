#!/usr/bin/env python3
"""
Chat Message History Parser for MiMo-V2.5 Tokenizer

Parses the session JSON files and converts them to the MiMo-V2.5 chat template format.

The MiMo-V2.5 template format from tokenizer_config.json:
- Messages: <|im_start>role\ncontent<|im_end|>
- Tools: <tool_call><function=name>...</function></tool_call>
- Reasoning: <think>reasoning</think>content
- Tool responses: <tool_response>content</tool_response>
- System messages: <|im_start>system\ncontent<|im_end|>

This script analyzes session 273c5de0.json and converts it to the MiMo-V2.5 format.
"""

import json
import re
from pathlib import Path
from typing import Dict, List, Any, Optional, Tuple

class MiMoV25Parser:
    """Parser for MiMo-V2.5 chat template format"""
    
    def __init__(self, template_content: str):
        self.template_rules = self._extract_template_rules(template_content)
        self.role_mapping = {
            'system': 'system',
            'user': 'user', 
            'assistant': 'assistant',
            'tool': 'tool'
        }
    
    def _extract_template_rules(self, template: str) -> Dict[str, Any]:
        """Extract key patterns from the template"""
        rules = {}
        
        # Basic message pattern
        rules['message_pattern'] = r'<\|im_start\>(\w+)\n(.*?)<\|im_end\>'
        
        # Tool call pattern
        rules['tool_call_pattern'] = r'<tool_call>\s*<function=(\w+)>(.*?)</function>\s*</tool_call>'
        
        # Reasoning pattern
        rules['reasoning_pattern'] = r'<think>(.*?)</think>(.*?)'
        
        # Tool response pattern
        rules['tool_response_pattern'] = r'<tool_response>(.*?)</tool_response>'
        
        # System preamble pattern
        rules['system_preamble'] = '<|im_start|>system\nYou are MiMo, a helpful AI assistant engineered by Xiaomi.'
        
        return rules
    
    def parse_message_content(self, content: Any) -> str:
        """Parse message content according to MiMo-V2.5 rules"""
        if isinstance(content, str):
            return content
        elif isinstance(content, list):
            # Handle list of content blocks
            return self._parse_content_blocks(content)
        return str(content)
    
    def _parse_content_blocks(self, blocks: List[Dict]) -> str:
        """Parse a list of content blocks into text"""
        text_parts = []
        for block in blocks:
            if isinstance(block, dict):
                block_type = block.get('type')
                if block_type == 'text':
                    text_parts.append(block.get('text', ''))
                elif block_type == 'tool_result':
                    # Handle microcompact placeholders
                    result_content = block.get('content', '')
                    if result_content == '[Old tool result cleared]':
                        text_parts.append('[Old tool result cleared]')
                    else:
                        text_parts.append(result_content)
        return ' '.join(text_parts)
    
    def extract_reasoning_from_text(self, text: str) -> Tuple[str, str]:
        """Extract reasoning (think) from text content using regex"""
        if not text:
            return "", ""
        
        # Pattern for <think>...</think> blocks
        think_pattern = r'<think>(.*?)</think>(.*)'
        match = re.search(think_pattern, text, re.DOTALL)
        
        if match:
            reasoning = match.group(1).strip()
            remaining_text = match.group(2).strip()
            return reasoning, remaining_text
        
        return "", text
    
    def convert_session_to_template(self, session_data: Dict) -> str:
        """Convert entire session to MiMo-V2.5 template format"""
        messages = session_data.get('messages', [])
        
        output_lines = []
        
        # Add system preamble (if not already present)
        system_content = self._extract_first_system_content(messages)
        if system_content and "You are MiMo" not in system_content:
            output_lines.append(f"<|im_start|>system\n{system_content}")
        else:
            output_lines.append("<|im_start|>system\nYou are MiMo, a helpful AI assistant engineered by Xiaomi.")
        
        # Process messages in order
        for msg in messages:
            msg_type = msg.get('type')
            role = self.role_mapping.get(msg_type, msg_type)
            
            if msg_type == 'user':
                content = self.parse_message_content(msg.get('content'))
                output_lines.append(f"<|im_start|>user\n{content}")
                
            elif msg_type == 'assistant':
                content = self.parse_message_content(msg.get('content'))
                
                # Extract reasoning from assistant message
                reasoning, remaining_text = self.extract_reasoning_from_text(content)
                
                # Build assistant message
                assistant_line = f"<|im_start|>assistant\n"
                
                if reasoning:
                    assistant_line += f"<think>{reasoning}</think>"
                
                if remaining_text:
                    assistant_line += remaining_text
                
                output_lines.append(assistant_line)
                
            elif msg_type == 'system':
                # Handle system messages - check for compact summaries
                if self._is_compact_summary(msg):
                    # Extract just the summary part
                    summary = self._extract_summary_content(msg)
                    if summary:
                        output_lines.append(f"<|im_start|>system\n{summary}")
                else:
                    content = self.parse_message_content(msg.get('content'))
                    output_lines.append(f"<|im_start|>system\n{content}")
                    
            elif msg_type == 'tool':
                # Tool response
                content = self.parse_message_content(msg.get('content'))
                output_lines.append(f"<tool_response>\n{content}\n</tool_response>")
        
        # Ensure proper closing
        output_lines.append("<|im_end|>")
        
        return '\n'.join(output_lines)
    
    def _extract_first_system_content(self, messages: List[Dict]) -> Optional[str]:
        """Extract content from first system message"""
        for msg in messages:
            if msg.get('type') == 'system':
                return self.parse_message_content(msg.get('content'))
        return None
    
    def _is_compact_summary(self, msg: Dict) -> bool:
        """Check if message is a compact summary"""
        return (msg.get('type') == 'system' and
                msg.get('subtype') == 'compact_boundary' and
                isinstance(msg.get('content'), list))
    
    def _extract_summary_content(self, msg: Dict) -> Optional[str]:
        """Extract summary content from compact summary message"""
        content = msg.get('content')
        if isinstance(content, list):
            for block in content:
                if isinstance(block, dict) and block.get('type') == 'text':
                    text = block.get('text', '')
                    # Extract just the summary part after "Summary:"
                    summary_match = re.search(r'Summary: (.+)', text)
                    if summary_match:
                        return summary_match.group(1)
        return None

class SessionPatternAnalyzer:
    """Analyzes session patterns and compliance with MiMo-V2.5 template"""
    
    def __init__(self):
        self.template_rules = {
            'expected_roles': ['system', 'user', 'assistant', 'tool'],
            'message_format': '<|im_start>role\ncontent<|im_end|>',
            'tool_calls': '<tool_call><function=name>...</function></tool_call>',
            'reasoning': '<think>...</think>',
            'tool_responses': '<tool_response>...</tool_response>'
        }
    
    def analyze_patterns(self, session_data: Dict) -> Dict[str, Any]:
        """Analyze session for patterns and template compliance"""
        messages = session_data.get('messages', [])
        analysis = {}
        
        # Basic statistics
        analysis['message_count'] = len(messages)
        analysis['turn_count'] = session_data.get('turn_count', 0)
        
        # Role distribution
        role_counts = {}
        for msg in messages:
            role = msg.get('type')
            role_counts[role] = role_counts.get(role, 0) + 1
        analysis['role_distribution'] = role_counts
        
        # Template compliance
        compliance = self._check_template_compliance(messages)
        analysis['template_compliance'] = compliance
        
        # Pattern detection
        patterns = self._detect_patterns(messages)
        analysis['detected_patterns'] = patterns
        
        # Session characteristics
        session_info = {
            'id': session_data.get('id'),
            'model': session_data.get('model'),
            'created_at': session_data.get('created_at'),
            'updated_at': session_data.get('updated_at'),
            'total_cost_usd': session_data.get('total_cost_usd'),
            'total_input_tokens': session_data.get('total_input_tokens'),
            'total_output_tokens': session_data.get('total_output_tokens')
        }
        analysis['session_info'] = session_info
        
        return analysis
    
    def _check_template_compliance(self, messages: List[Dict]) -> Dict[str, Any]:
        """Check compliance with MiMo-V2.5 template rules"""
        compliance = {
            'valid_roles': True,
            'proper_format': True,
            'tool_calls_present': False,
            'reasoning_present': False,
            'system_preamble_present': False,
            'compact_summaries_present': False
        }
        
        for msg in messages:
            role = msg.get('type')
            if role not in self.template_rules['expected_roles']:
                compliance['valid_roles'] = False
                break
            
            # Check for system preamble (first system message should contain MiMo greeting)
            if role == 'system':
                content = str(msg.get('content', ''))
                if 'You are MiMo' in content:
                    compliance['system_preamble_present'] = True
        
        # Check for tool calls and reasoning
        for msg in messages:
            content = str(msg.get('content', ''))
            if '<tool_call>' in content:
                compliance['tool_calls_present'] = True
            
            if '<think>' in content:
                compliance['reasoning_present'] = True
            
            # Check for compact summary messages
            if (msg.get('type') == 'system' and 
                msg.get('subtype') == 'compact_boundary'):
                compliance['compact_summaries_present'] = True
        
        return compliance
    
    def _detect_patterns(self, messages: List[Dict]) -> Dict[str, Any]:
        """Detect patterns in the session messages"""
        patterns = {
            'conversation_flow': [],
            'tool_usage': {},
            'compact_summary_intervals': [],
            'content_evolution': {}
        }
        
        # Detect conversation flow
        for i, msg in enumerate(messages):
            role = msg.get('type')
            patterns['conversation_flow'].append({
                'index': i,
                'role': role,
                'content_preview': str(msg.get('content', ''))[:100]
            })
        
        # Tool usage patterns
        tool_uses = []
        tool_results = []
        
        for msg in messages:
            if msg.get('type') == 'assistant':
                content_blocks = msg.get('content', [])
                if isinstance(content_blocks, list):
                    for block in content_blocks:
                        if isinstance(block, dict):
                            if block.get('type') == 'tool_use':
                                tool_uses.append({
                                    'tool_name': block.get('name'),
                                    'tool_use_id': block.get('id'),
                                    'input_preview': str(block.get('input', ''))[:50]
                                })
            
            elif msg.get('type') == 'user':
                content_blocks = msg.get('content', [])
                if isinstance(content_blocks, list):
                    for block in content_blocks:
                        if isinstance(block, dict) and block.get('type') == 'tool_result':
                            tool_results.append({
                                'tool_use_id': block.get('tool_use_id'),
                                'is_cleared': block.get('content') == '[Old tool result cleared]',
                                'result_preview': str(block.get('content', ''))[:50]
                            })
        
        patterns['tool_usage'] = {
            'total_tool_uses': len(tool_uses),
            'total_tool_results': len(tool_results),
            'cleared_results': sum(1 for r in tool_results if r['is_cleared']),
            'sample_tools': [t['tool_name'] for t in tool_uses[:5]] if tool_uses else []
        }
        
        # Compact summary intervals
        summary_indices = []
        for i, msg in enumerate(messages):
            if msg.get('type') == 'system' and msg.get('subtype') == 'compact_boundary':
                summary_indices.append(i)
        
        patterns['compact_summary_intervals'] = summary_indices
        
        return patterns

class MiMoV25Converter:
    """Main converter class for MiMo-V2.5 format"""
    
    def __init__(self):
        # Load the actual template from the fetched content
        template_content = self._load_template()
        self.parser = MiMoV25Parser(template_content)
        self.analyzer = SessionPatternAnalyzer()
    
    def _load_template(self) -> str:
        """Load the actual MiMo-V2.5 chat template"""
        # This is the template we fetched from tokenizer_config.json
        return """{%- if not add_generation_prompt is defined -%}
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
    
    def convert_session(self, session_data: Dict[str, Any]) -> Dict[str, Any]:
        """Convert a session to MiMo-V2.5 format"""
        session_id = session_data.get('id')
        messages = session_data.get('messages', [])
        
        # Analyze patterns
        analysis = self.analyzer.analyze_patterns(session_data)
        
        # Convert to template format
        template_content = self.parser.convert_session_to_template(session_data)
        
        return {
            'session_id': session_id,
            'analysis': analysis,
            'template_content': template_content,
            'message_count': len(messages),
            'compliance_score': analysis['template_compliance'].get('valid_roles', False)
        }

def main():
    """Main function to process the session"""
    session_path = Path("/data/data/com.termux/files/home/.config/agent-code/sessions/273c5de0.json")
    
    if not session_path.exists():
        print(f"Session file not found: {session_path}")
        return
    
    # Load the session data
    with open(session_path) as f:
        session_data = json.load(f)
    
    print("=" * 80)
    print("MiMo-V2.5 Chat History Pattern Analysis")
    print("=" * 80)
    
    # Create converter
    converter = MiMoV25Converter()
    
    # Convert and analyze
    result = converter.convert_session(session_data)
    
    # Print session info
    print(f"Session ID: {result['session_id']}")
    print(f"Model: {session_data.get('model')}")
    print(f"Total Messages: {result['message_count']}")
    print(f"Turn Count: {session_data.get('turn_count')}")
    print(f"Template Compliance: {result['compliance_score']}")
    
    # Print analysis
    print("\n" + "=" * 80)
    print("SESSION ANALYSIS")
    print("=" * 80)
    
    analysis = result['analysis']
    print(f"\nRole Distribution:")
    for role, count in analysis.get('role_distribution', {}).items():
        print(f"  {role}: {count}")
    
    print(f"\nTemplate Compliance:")
    compliance = analysis['template_compliance']
    for key, value in compliance.items():
        print(f"  {key}: {value}")
    
    print(f"\nPattern Analysis:")
    patterns = analysis['detected_patterns']
    print(f"  Tool Usage:")
    print(f"    Total tool uses: {patterns['tool_usage']['total_tool_uses']}")
    print(f"    Total tool results: {patterns['tool_usage']['total_tool_results']}")
    print(f"    Cleared results: {patterns['tool_usage']['cleared_results']}")
    print(f"    Sample tools: {patterns['tool_usage']['sample_tools']}")
    
    print(f"  Compact summary intervals: {patterns['compact_summary_intervals']}")
    
    print("\n" + "=" * 80)
    print("FORMATTED CHAT OUTPUT (first 2000 chars)")
    print("=" * 80)
    template_output = result['template_content']
    print(template_output[:2000])
    
    if len(template_output) > 2000:
        print("...\n(truncated)")
    
    # Save the output for further processing
    output_path = Path("/data/data/com.termux/files/home/agent-code/momo_chat_output.txt")
    with open(output_path, 'w') as f:
        f.write(template_output)
    
    print(f"\nOutput saved to: {output_path}")
    
    return result

if __name__ == "__main__":
    result = main()