"""
Qwen2/MiMo Chat Template Parser - Python Implementation

This module provides a Python class that replicates the Jinja2 chat template logic
for the Qwen2/MiMo model family, including:
- System message handling
- Multi-turn conversation rendering
- Tool/function calling support
- Reasoning/thinking content handling
- Multi-modal content (images, audio, video)
- Special token management
"""

from __future__ import annotations

import json
import re
from dataclasses import dataclass, field
from typing import Any, Literal
from enum import Enum


class Role(str, Enum):
    SYSTEM = "system"
    USER = "user"
    ASSISTANT = "assistant"
    TOOL = "tool"


class ContentType(str, Enum):
    TEXT = "text"
    IMAGE = "image"
    AUDIO = "audio"
    VIDEO = "video"


@dataclass
class ContentPart:
    """Represents a single content part (text, image, audio, video)."""
    type: ContentType | str
    text: str | None = None
    image: str | None = None
    image_url: str | None = None
    audio: str | None = None
    audio_url: str | None = None
    video: str | None = None
    video_url: str | None = None

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> ContentPart:
        """Create ContentPart from dictionary."""
        ctype = data.get("type", "text")
        return cls(
            type=ctype,
            text=data.get("text"),
            image=data.get("image"),
            image_url=data.get("image_url"),
            audio=data.get("audio"),
            audio_url=data.get("audio_url"),
            video=data.get("video"),
            video_url=data.get("video_url"),
        )

    def render(self) -> str:
        """Render content part to string with special tokens."""
        if self.type == ContentType.TEXT or self.text is not None:
            return self.text or ""
        elif self.type == ContentType.IMAGE or self.image or self.image_url:
            return "<|vision_start|><|image_pad|><|vision_end|>"
        elif self.type == ContentType.AUDIO or self.audio or self.audio_url:
            return "<|mimo_audio_start|><|audio_pad|><|mimo_audio_end|>"
        elif self.type == ContentType.VIDEO or self.video or self.video_url:
            return "<|vision_start|><|video_pad|><|vision_end|>"
        return ""


@dataclass
class ToolCall:
    """Represents a tool/function call."""
    name: str
    arguments: dict[str, Any]
    id: str | None = None

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> ToolCall:
        """Create ToolCall from dictionary (OpenAI format)."""
        func = data.get("function", data)
        return cls(
            name=func.get("name", ""),
            arguments=func.get("arguments", {}),
            id=data.get("id"),
        )

    def render(self) -> str:
        """Render tool call in the expected format."""
        lines = [f"<tool_call>\n<function={self.name}>"]
        args = self.arguments
        if isinstance(args, str):
            try:
                args = json.loads(args)
            except (json.JSONDecodeError, TypeError):
                args = None
        if isinstance(args, dict):
            for arg_name, arg_value in args.items():
                if isinstance(arg_value, (dict, list)):
                    arg_str = json.dumps(arg_value, ensure_ascii=False)
                else:
                    arg_str = str(arg_value)
                lines.append(f"<parameter={arg_name}>{arg_str}</parameter>")
        lines.append("</function>\n</tool_call>")
        return "\n".join(lines)


@dataclass
class Tool:
    """Represents a tool/function definition."""
    name: str
    description: str | None = None
    parameters: dict[str, Any] | None = None
    extra: dict[str, Any] = field(default_factory=dict)

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> Tool:
        """Create Tool from dictionary (OpenAI format)."""
        func = data.get("function", data)
        handled = {"type", "name", "description", "parameters"}
        extra = {k: v for k, v in func.items() if k not in handled}
        return cls(
            name=func.get("name", ""),
            description=func.get("description"),
            parameters=func.get("parameters"),
            extra=extra,
        )

    def render_parameter(self, param_name: str, param_fields: dict[str, Any]) -> str:
        """Render a single parameter definition."""
        lines = ["\n<parameter>"]
        lines.append(f"\n<name>{param_name}</name>")
        if "type" in param_fields:
            lines.append(f"\n<type>{param_fields['type']}</type>")
        if "description" in param_fields:
            lines.append(f"\n<description>{param_fields['description'].strip()}</description>")

        handled = {"name", "type", "description"}
        extra_lines = self._render_extra_keys(param_fields, handled)
        if extra_lines:
            lines.append(extra_lines)

        lines.append("\n</parameter>")
        return "".join(lines)

    def _render_extra_keys(self, obj: dict[str, Any], handled: set[str]) -> str:
        """Render extra keys not in handled set."""
        if not isinstance(obj, dict):
            return ""
        parts = []
        for key, value in obj.items():
            if key in handled:
                continue
            if isinstance(value, (dict, list)) and not isinstance(value, str):
                parts.append(f"\n<{key}>{json.dumps(value, ensure_ascii=False)}</{key}>")
            else:
                parts.append(f"\n<{key}>{value}</{key}>")
        return "".join(parts)

    def render(self) -> str:
        """Render tool definition in the expected format."""
        lines = [f"\n<function>\n<name>{self.name}</name>"]
        if self.description:
            lines.append(f"\n<description>{self.description.strip()}</description>")
        lines.append("\n<parameters>")

        if self.parameters and isinstance(self.parameters, dict):
            props = self.parameters.get("properties", {})
            if isinstance(props, dict):
                for param_name, param_fields in props.items():
                    if isinstance(param_fields, dict):
                        lines.append(self.render_parameter(param_name, param_fields))

            handled = {"type", "properties"}
            extra = self._render_extra_keys(self.parameters, handled)
            if extra:
                lines.append(extra)

        lines.append("\n</parameters>")

        handled = {"type", "name", "description", "parameters"}
        extra = self._render_extra_keys(self.extra, handled)
        if extra:
            lines.append(extra)

        lines.append("\n</function>")
        return "".join(lines)


@dataclass
class Message:
    """Represents a chat message."""
    role: Role | str
    content: str | list[ContentPart] | None = None
    reasoning_content: str | None = None
    tool_calls: list[ToolCall] | None = None
    tool_call_id: str | None = None
    name: str | None = None

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> Message:
        """Create Message from dictionary, supporting both OpenAI and Session Log formats."""
        # Support 'type' as role (session log format)
        role_str = data.get("role") or data.get("type", "user")
        try:
            role = Role(role_str)
        except ValueError:
            role = role_str

        raw_content = data.get("content")
        content = None
        tool_calls = []

        if isinstance(raw_content, list):
            processed_parts = []
            for item in raw_content:
                if isinstance(item, dict):
                    # Handle tool_use in content (session log format)
                    if item.get("type") == "tool_use":
                        tool_calls.append(ToolCall(
                            name=item.get("name", ""),
                            arguments=item.get("input", {}),
                            id=item.get("id")
                        ))
                    # Handle text/image/etc
                    else:
                        processed_parts.append(ContentPart.from_dict(item))
                else:
                    processed_parts.append(ContentPart(type=ContentType.TEXT, text=str(item)))
            
            # If only tool calls were found, content stays None/empty
            # If text parts exist, use them
            content = processed_parts if processed_parts else None
        elif isinstance(raw_content, str):
            content = raw_content

        # Also check for standard 'tool_calls' field (OpenAI format)
        standard_tool_calls = data.get("tool_calls")
        if standard_tool_calls:
            tool_calls.extend([ToolCall.from_dict(tc) for tc in standard_tool_calls])

        return cls(
            role=role,
            content=content,
            reasoning_content=data.get("reasoning_content"),
            tool_calls=tool_calls if tool_calls else None,
            tool_call_id=data.get("tool_call_id"),
            name=data.get("name"),
        )

    def render_content(self) -> str:
        """Render message content to string."""
        if self.content is None:
            return ""
        if isinstance(self.content, str):
            return self.content
        return "".join(part.render() for part in self.content)


@dataclass
class ChatTemplateConfig:
    """Configuration for chat template rendering."""
    add_generation_prompt: bool = False
    enable_thinking: bool = True
    keep_all_reasoning: bool = True


class Qwen2ChatParser:
    """
    Python implementation of the Qwen2/MiMo chat template.
    """

    IM_START = "<|im_start|>"
    IM_END = "<|im_end|>"
    THINK_START = "<think>"
    THINK_END = "</think>"

    def __init__(self, config: ChatTemplateConfig | None = None):
        self.config = config or ChatTemplateConfig()

    def render(
        self,
        messages: list[dict[str, Any] | Message],
        tools: list[dict[str, Any] | Tool] | None = None,
        system_message: str | None = None,
        **kwargs,
    ) -> str:
        config = ChatTemplateConfig(
            add_generation_prompt=kwargs.get("add_generation_prompt", self.config.add_generation_prompt),
            enable_thinking=kwargs.get("enable_thinking", self.config.enable_thinking),
            keep_all_reasoning=kwargs.get("keep_all_reasoning", self.config.keep_all_reasoning),
        )

        parsed_messages = [m if isinstance(m, Message) else Message.from_dict(m) for m in messages]
        parsed_tools = [t if isinstance(t, Tool) else Tool.from_dict(t) for t in tools] if tools else []

        system_msg = system_message
        loop_messages = parsed_messages

        if parsed_messages and parsed_messages[0].role == Role.SYSTEM:
            system_msg = parsed_messages[0].render_content()
            loop_messages = parsed_messages[1:]

        last_user_index = -1
        for i, msg in enumerate(loop_messages):
            if msg.role == Role.USER:
                last_user_index = i

        output = []

        if system_msg is not None:
            output.append(f"{self.IM_START}system\n{system_msg}")
        else:
            output.append(f"{self.IM_START}system\nYou are MiMo, a helpful AI assistant engineered by Xiaomi.")

        if parsed_tools:
            output.append("\n\n# Tools\n\nYou may call one or more functions to assist with the user query.\n\nYou have access to the following functions:\n\n")
            output.append("<tools>")
            for tool in parsed_tools:
                output.append(tool.render())
            output.append("\n</tools>")
            output.append(
                "\n\nFor each function call, output the function name and arguments in the following format:\n"
                "<tool_call>\n<function=example_function_name>\n<parameter=example_parameter_1>value_1</parameter>\n"
                "<parameter=example_parameter_2>This is the value for the second parameter\nthat can span\nmultiple lines</parameter>\n"
                "</function>\n</tool_call>\n\n"
                "<IMPORTANT>\n- Function calls MUST follow the specified format: an inner <function=...></function> block must be nested within <tool_call></tool_call> XML tags\n"
                "- DO NOT use function calls inside <think></think> tags.\n"
                "- The value enclosed between parameter tags is preserved exactly as-is, including newlines and spaces.\n"
                "</IMPORTANT>"
            )

        output.append(f"{self.IM_END}")

        for i, msg in enumerate(loop_messages):
            content = msg.render_content()
            
            if msg.role == Role.ASSISTANT:
                reasoning = msg.reasoning_content or ""
                if not reasoning and self.THINK_END in content:
                    parts = content.split(self.THINK_END)
                    if len(parts) > 1:
                        reasoning = parts[0].split(self.THINK_START)[-1]
                        content = self.THINK_END.join(parts[1:])

                show_reasoning = (config.keep_all_reasoning or i > last_user_index) and reasoning
                
                if show_reasoning:
                    output.append(f"{self.IM_START}assistant\n{self.THINK_START}{reasoning}{self.THINK_END}{content}")
                else:
                    output.append(f"{self.IM_START}assistant\n{self.THINK_START}{self.THINK_END}{content}")

                if msg.tool_calls:
                    for tc in msg.tool_calls:
                        output.append("\n" + tc.render())
                output.append(f"{self.IM_END}")

            elif msg.role == Role.USER:
                output.append(f"{self.IM_START}user\n{content}{self.IM_END}")
            elif msg.role == Role.SYSTEM:
                output.append(f"{self.IM_START}system\n{content}{self.IM_END}")
            elif msg.role == Role.TOOL:
                if i == 0 or loop_messages[i-1].role != Role.TOOL:
                    output.append(f"{self.IM_START}tool\n")
                output.append(f"<tool_response>\n{content}\n</tool_response>\n")
                if i == len(loop_messages) - 1 or loop_messages[i+1].role != Role.TOOL:
                    output.append(f"{self.IM_END}")
            else:
                output.append(f"{self.IM_START}{msg.role}\n{content}{self.IM_END}")

        if config.add_generation_prompt:
            output.append(f"{self.IM_START}assistant\n")
            if not config.enable_thinking:
                output.append(f"{self.THINK_START}{self.THINK_END}")

        return "".join(output)


def render_chat_template(messages, tools=None, system_message=None, **kwargs):
    return Qwen2ChatParser().render(messages, tools, system_message, **kwargs)

if __name__ == "__main__":
    messages = [{"role": "user", "content": "Hello!"}]
    print(render_chat_template(messages, add_generation_prompt=True))
