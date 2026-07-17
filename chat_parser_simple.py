#!/usr/bin/env python3
"""
Simple Chat Message History Parser for MiMo-V2.5 tokenizer_config.json
This demonstrates parsing chat history according to the MiMo-V2.5 template rules.
"""

import json
import re
from typing import Dict, List, Any
from pathlib import Path

# Extract the chat template from the actual file
TEMPLATE_PATH = Path("/data/data/com.termux/files/home/agent-code/tokenizer_config.json")

def load_template():
    """Load and extract the chat_template from tokenizer_config.json"""
    if not TEMPLATE_PATH.exists():
        # Try to find the file from the fetched content
        # We'll extract it from the WebFetch result we got earlier
        return get_chat_template_from_fetch()
    
    with open(TEMPLATE_PATH) as f:
        data = json.load(f)
        return data.get('chat_template', '')

def get_chat_template_from_fetch():
    """Get the chat template from the fetched content"""
    # The chat template starts with the macro definitions and has specific patterns
    # We'll extract the key parts needed for parsing
    template = """
{%- if not add_generation_prompt is defined -%}
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
                {{- '\n<' ~ json_key ~ '>' ~ (json_dict[json_key] | tojson | safe) ~ '</' ~ json_key ~ '>' }}
            {%- else %}
                {{-'\n<' ~ json_key ~ '>' ~ (json_dict[json_key] | string) ~ '</' ~ json_key ~ '>' }}
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
    {{- "<|im_start|>system\n" + render_content(system_message) }}
{%- else %}
    {{- "<|im_start|>system\nYou are MiMo, a helpful AI assistant engineered by Xiaomi." }}
{%- endif %}
{%- if tools is iterable and tools | length > 0 %}
    {{- "\n\n# Tools\n\nYou may call one or more functions to assist with the user query.\n\nYou have access to the following functions:\n\n<tools>" }}
    {%- for tool in tools %}
        {%- if tool.function is defined %}
            {%- set tool = tool.function %}
        {%- endif %}
        {{- "\n<function>\n<name>" ~ tool.name ~ "</name>" }}
        {%- if tool.description is defined %}
            {{- '\n<description>' ~ (tool.description | trim) ~ '</description>' }}
        {%- endif %}
        {{- '\n<parameters>' }}
        {%- if tool.parameters is defined and tool.parameters is mapping and tool.parameters.properties is defined and tool.parameters.properties is mapping %}
            {%- for param_name, param_fields in tool.parameters.properties|items %}
                {{- '\n<parameter>' }}
                {{- '\n<name>' ~ param_name ~ '</name>' }}
                {%- if param_fields.type is defined %}
                    {{- '\n<type>' ~ (param_fields.type | string) ~ '</type>' }}
                {%- endif %}
                {%- if param_fields.description is defined %}
                    {{- '\n<description>' ~ (param_fields.description | trim) ~ '</description>' }}
                {%- endif %}
                {%- set handled_keys = ['name', 'type', 'description'] %}
                {{- render_extra_keys(param_fields, handled_keys) }}
                {{- '\n</parameter>' }}
            {%- endfor %}
        {%- endif %}
        {%- set handled_keys = ['type', 'properties'] %}
        {{- render_extra_keys(tool.parameters, handled_keys) }}
        {{- '\n</parameters>' }}
        {%- set handled_keys = ['type', 'name', 'description', 'parameters'] %}
        {{- render_extra_keys(tool, handled_keys) }}
        {{- '\n</function>' }}
    {%- endfor %}
    {{- "\n</tools>" }}
    {{- '\n\nFor each function call, output the function name and arguments in the following format:\\n<tool_call>\\n<function=example_function_name>\\n<parameter=example_parameter_1>value_1</parameter>\\n<parameter=example_parameter_2>This is the value for the second parameter\\nthat can span\\nmultiple lines</parameter>\\n</function>\\n</tool_call>\\n\\n<IMPORTANT>\\n- Function calls MUST follow the specified format: an inner <function=...></function> block must be nested within <tool_call></tool_call> XML tags\\n- DO NOT use function calls inside <think></think> tags.\\n- The value enclosed between parameter tags is preserved exactly as-is, including newlines and spaces.\\n</IMPORTANT>' }}
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
            {{- '<|im_start>>' + message.role + '\\n<think>' + reasoning_content + '</think>' + content }}
        {%- else %}
            {{- '<|im_start>>' + message.role + '\\n<think></think>' + content }}
        {%- endif %}
        {%- if message.tool_calls is defined and message.tool_calls is iterable and message.tool_calls | length > 0 %}
            {%- for tool_call in message.tool_calls %}
                {%- if tool_call.function is defined %}
                    {%- set tool_call = tool_call.function %}
                {%- endif %}
                {{- '<tool_call>\\n<function=' + tool_call.name + '>\\n' }}
                {%- if tool_call.arguments is defined %}
                    {%- for args_name, args_value in tool_call.arguments|items %}
                        {{- '<parameter=' + args_name + '>' }}
                        {%- set args_value = args_value | tojson | safe if args_value is mapping or (args_value is sequence and args_value is not string) else args_value | string %}
                        {{- args_value }}
                        {{- '</parameter>\\n' }}
                    {%- endfor %}
                {%- endif %}
                {{- '</function>\\n</tool_call>' }}
            {%- endfor %}
        {%- endif %}
        {{- '<|im_end|>' }}
    {%- elif message.role == "user" %}
        {{- '<|im_start>>' + message.role + '\\n' + render_content(message.content) + '<|im_end|>' }}
    {%- elif message.role == "system" %}
        {{- '<|im_start>>' + message.role + '\\n' + render_content(message.content) + '<|im_end|>' }}
    {%- elif message.role == "tool" %}
        {%- if loop.previtem and loop.previtem.role != "tool" %}
            {{- '<|im_start|>tool\\n' }}
        {%- endif %}
        {{- '<tool_response>\\n' }}
        {{- render_content(message.content) }}
        {{- '\\n</tool_response>\\n' }}
        {%- if not loop.last and loop.nextitem.role != "tool" %}
            {{- '<|im_end|>' }}
        {%- elif loop.last %}
            {{- '<|im_end|>' }}
        {%- endif %}
    {%- else %}
        {{- '<|im_start>>' + message.role + '\\n' + render_content(message.content) + '<|im_end|>' }}
    {%- endif %}
{%- endfor %}
{%- if add_generation_prompt %}
    {{- '<|im_start|>assistant\\n' }}
    {%- if not enable_thinking -%}
        {{- '<think></think>' -}}
    {%- else -%}
        {{- '' -}}
    {%- endif -%}
{%- endif %}
"""

def simple_extract_system_content(messages):
    """Extract system content from first system message if present"""
    for msg in messages:
        if msg.get('type') == 'system':
            content = msg.get('content', '')
            if isinstance(content, str):
                return content
            elif isinstance(content, list):
                # Concatenate text from message content
                texts = []
                for block in content:
                    if isinstance(block, dict) and block.get('type') == 'text':
                        texts.append(block.get('text', ''))
                return ' '.join(texts)
    return None

def extract_message_content(msg):
    """Extract text content from a message, handling compact summaries"""
    if isinstance(msg.get('content'), str):
        return msg['content']
    
    content_text = []
    for block in msg.get('content', []):
        if isinstance(block, dict):
            if block.get('type') == 'text':
                content_text.append(block.get('text', ''))
            elif block.get('type') == 'tool_result':
                # Handle microcompact placeholder
                if block.get('content') == '[Old tool result cleared]':
                    content_text.append('[Old tool result cleared]')
                else:
                    content_text.append(block.get('content', ''))
    
    return ' '.join(content_text)

def is_compact_summary_message(msg):
    """Check if a message is a compact summary"""
    return (msg.get('type') == 'system' and 
            msg.get('subtype') == 'compact_boundary' and
            extract_message_content(msg).startswith('[Conversation compacted. Summary:'))

def convert_chat_history(messages):
    """Convert chat history messages to MiMo-V2.5 template format"""
    if not messages:
        return ""
    
    result = []
    
    # Handle system content for preamble
    system_content = simple_extract_system_content(messages)
    if system_content:
        result.append(f"<|im_start|>system\n{system_content}")
    else:
        result.append("<|im_start|>system\nYou are MiMo, a helpful AI assistant engineered by Xiaomi.")
    
    # Process messages in order
    for i, msg in enumerate(messages):
        msg_type = msg.get('type')
        content_text = extract_message_content(msg)
        
        if msg_type == 'user':
            result.append(f"<|im_start|>user\n{content_text}")
        
        elif msg_type == 'assistant':
            # Build assistant message
            assistant_line = "<|im_start|>assistant\n"
            
            # Extract reasoning if present (simplified)
            reasoning_content = None
            # Look for think blocks in text content
            if content_text:
                # Simple extraction - in reality this would need to parse the structure
                think_start = content_text.find('<think>')
                think_end = content_text.find('</think>', think_start)
                if think_start != -1 and think_end != -1:
                    reasoning_content = content_text[think_start + 6:think_end]
                    # Remove think blocks from final content
                    content_text = content_text[:think_start] + content_text[think_end + 7:]
            
            if reasoning_content:
                assistant_line += f"<think>{reasoning_content}</think>"
            
            # Add remaining text content
            if content_text and content_text.strip():
                assistant_line += content_text.strip()
            
            # Add tool calls (simplified - would need proper parsing)
            # In reality, tool_use blocks would be converted
            
            result.append(assistant_line)
        
        elif msg_type == 'system' and is_compact_summary_message(msg):
            # Extract summary content
            content_text = extract_message_content(msg)
            # Find the actual summary text after "Summary:"
            summary_match = re.search(r'Summary: (.+)', content_text)
            if summary_match:
                result.append(f"<|im_start|>system\n{summary_match.group(1)}")
        
        elif msg_type == 'tool':
            # Tool response
            result.append(f"<tool_response>\n{content_text}\n</tool_response>")
        
        # Add end tags for non-compact messages
        if msg_type not in ['user', 'assistant']:
            result.append("<|im_end|>")
    
    # Ensure we end with im_end
    if result and not result[-1].endswith('<|im_end|>'):
        result.append("<|im_end|>")
    
    return '\n'.join(result)

def main():
    """Test the converter with the session"""
    session_path = Path("~/.config/agent-code/sessions/273c5de0.json").expanduser()
    
    if not session_path.exists():
        print(f"Session file not found: {session_path}")
        return
    
    with open(session_path) as f:
        session_data = json.load(f)
    
    print("=" * 80)
    print("MiMo-V2.5 Chat History Converter")
    print("=" * 80)
    print(f"Session ID: {session_data.get('id')}")
    print(f"Model: {session_data.get('model')}")
    print(f"Total messages: {len(session_data.get('messages', []))}")
    print(f"Turn count: {session_data.get('turn_count')}")
    print()
    
    # Convert the chat history
    messages = session_data.get('messages', [])
    
    print("Message Type Analysis:")
    msg_types = {}
    for msg in messages:
        t = msg.get('type')
        msg_types[t] = msg_types.get(t, 0) + 1
    print(json.dumps(msg_types, indent=2))
    
    # Find compact summary markers
    compact_summaries = [i for i, msg in enumerate(messages) if is_compact_summary_message(msg)]
    print(f"\nCompact summary messages at indices: {compact_summaries}")
    
    # Extract first few messages for context
    print(f"\nFirst 10 messages:")
    for i, msg in enumerate(messages[:10]):
        print(f"  {i}: {msg.get('type')} - {str(msg.get('content', '')[:100])}")
    
    # Convert and show result
    print("\n" + "=" * 80)
    print("FORMATTED CHAT OUTPUT (first 2000 chars):")
    print("=" * 80)
    formatted = convert_chat_history(messages)
    print(formatted[:2000])
    
    if len(formatted) > 2000:
        print("...\n(truncated)")
    
    print("\n" + "=" * 80)
    print("ANALYSIS")
    print("=" * 80)
    
    # Check for patterns in the original data
    tool_uses = sum(1 for msg in messages if msg.get('type') == 'assistant')
    user_msgs = sum(1 for msg in messages if msg.get('type') == 'user')
    print(f"Assistant messages: {tool_uses} (often with tool calls)")
    print(f"User messages: {user_msgs}")
    print(f"Compact summary messages: {len(compact_summaries)}")
    
    # Check message size
    total_content_length = sum(len(str(msg.get('content', ''))) for msg in messages)
    print(f"Total content length: {total_content_length} chars")
    
    avg_msg_length = total_content_length / len(messages) if messages else 0
    print(f"Average message length: {avg_msg_length:.1f} chars")

if __name__ == "__main__":
    main()