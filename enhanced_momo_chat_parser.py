#!/usr/bin/env python3
"""
Enhanced Chat Message History Parser for MiMo-V2.5 Tokenizer

This enhanced version provides detailed error reporting for template compliance,
showing:
- WHAT failed
- WHERE it failed
- WHY it failed
- WHICH rule was violated

Parses chat history JSON files from ~/.config/agent-code/sessions//*.json
and converts them to MiMo-V2.5 chat template format with detailed analysis.

Template Rules:
1. System preamble must contain "You are MiMo, a helpful AI assistant engineered by Xiaomi."
2. Messages must be properly formatted as <|im_start>role\ncontent<|im_end|>
3. Tool calls must be wrapped in <tool_call><function=name>...</function></tool_call>
4. Reasoning must be wrapped in <think>reasoning</think>
5. Tool responses must be wrapped in <tool_response>content</tool_response>
6. Compact summary messages must be properly handled
"""

import json
import re
import sys
from pathlib import Path
from typing import Dict, List, Any, Optional
from enum import Enum

# Enums for violation types
class ViolationType(Enum):
    MISSING_SYSTEM_PREAMBLE = "MISSING_SYSTEM_PREAMBLE"
    INVALID_MESSAGE_FORMAT = "INVALID_MESSAGE_FORMAT"
    MISSING_TOOL_CALL_TAGS = "MISSING_TOOL_CALL_TAGS"
    MISSING_REASONING_TAGS = "MISSING_REASONING_TAGS"
    MISSING_TOOL_RESPONSE_TAGS = "MISSING_TOOL_RESPONSE_TAGS"
    COMPACT_SUMMARY_NOT_HANDLED = "COMPACT_SUMMARY_NOT_HANDLED"
    TOOL_CALL_NOT_PROPERLY_FORMATTED = "TOOL_CALL_NOT_PROPERLY_FORMATTED"

class ViolationError(Exception):
    """Exception representing a template violation with detailed context"""
    def __init__(self, violation_type: ViolationType, message: str, location: str, rule: str):
        self.violation_type = violation_type
        self.message = message
        self.location = location
        self.rule = rule
        super().__init__(f"[{violation_type.value}] {message} at {location}: {rule}")

class DetailedTemplateChecker:
    """Enhanced checker that reports detailed violations with location and rule info"""
    
    def __init__(self):
        self.violations: List[ViolationError] = []
        self.rules = self._load_template_rules()
    
    def _load_template_rules(self) -> Dict[str, str]:
        """Load detailed template rules with error messages"""
        return {
            'system_preamble': 'System message must start with "You are MiMo, a helpful AI assistant engineered by Xiaomi."',
            'message_format': 'Messages must be formatted as "<|im_start>role\\ncontent<|im_end|>"',
            'tool_call_format': 'Tool calls must be formatted as "<tool_call><function=name>...</function></tool_call>"',
            'reasoning_format': 'Reasoning must be wrapped in "<think>reasoning</think>"',
            'tool_response_format': 'Tool responses must be wrapped in "<tool_response>content</tool_response>"',
            'compact_summary': 'Compact summary messages must be properly handled by extracting summary content',
            'valid_roles': 'Only "system", "user", "assistant", and "tool" roles are allowed'
        }
    
    def run_all_checks(self, messages: List[Dict]) -> bool:
        """Run all checks and return overall validity"""
        self.violations.clear()
        
        # Check system preamble
        if not self._check_system_preamble(messages):
            pass
        
        # Check message formats
        if not self._check_message_formats(messages):
            pass
        
        # Check tool calls
        if not self._check_tool_calls(messages):
            pass
        
        # Check reasoning format
        if not self._check_reasoning_format(messages):
            pass
        
        # Check tool response format
        if not self._check_tool_response_format(messages):
            pass
        
        # Check compact summaries
        if not self._check_compact_summaries(messages):
            pass
        
        return len(self.violations) == 0
    
    def _check_system_preamble(self, messages: List[Dict]) -> bool:
        """Check if system preamble is properly formatted"""
        for i, msg in enumerate(messages):
            if msg.get('type') == 'system':
                content = str(msg.get('content', ''))
                if 'You are MiMo' not in content and i == 0:
                    self.violations.append(ViolationError(
                        violation_type=ViolationType.MISSING_SYSTEM_PREAMBLE,
                        message=f"System preamble missing MiMo greeting",
                        location=f"message {i}, role: system",
                        rule=self.rules['system_preamble']
                    ))
                    return False
                break
        return True
    
    def _check_message_formats(self, messages: List[Dict]) -> bool:
        """Check if individual messages follow proper format"""
        all_valid = True
        
        for i, msg in enumerate(messages):
            role = msg.get('type')
            
            # Check role validity
            if role not in ['system', 'user', 'assistant', 'tool']:
                self.violations.append(ViolationError(
                    violation_type=ViolationType.INVALID_MESSAGE_FORMAT,
                    message=f"Invalid role '{role}' in message",
                    location=f"message {i}, role: {role}",
                    rule=self.rules['valid_roles']
                ))
                all_valid = False
                continue
            
            # Check content format
            if isinstance(msg.get('content'), str):
                # Check for proper message format
                if not msg.get('content', '').startswith(f"<|im_start>{role}") or '<|im_end|>' not in msg.get('content', ''):
                    self.violations.append(ViolationError(
                        violation_type=ViolationType.INVALID_MESSAGE_FORMAT,
                        message=f"Message {role} doesn't match template format",
                        location=f"message {i}, role: {role}",
                        rule=self.rules['message_format']
                    ))
                    all_valid = False
        
        return all_valid
    
    def _check_tool_calls(self, messages: List[Dict]) -> bool:
        """Check if tool calls are properly formatted"""
        all_valid = True
        
        for i, msg in enumerate(messages):
            content = str(msg.get('content', ''))
            
            if '<tool_call>' in content:
                # Found tool call - check if properly formatted
                if '<function=' not in content or '</function>' not in content:
                    self.violations.append(ViolationError(
                        violation_type=ViolationType.TOOL_CALL_NOT_PROPERLY_FORMATTED,
                        message=f"Tool call found but not properly formatted",
                        location=f"message {i}, content contains '<tool_call>'",
                        rule=self.rules['tool_call_format']
                    ))
                    all_valid = False
        
        return all_valid
    
    def _check_reasoning_format(self, messages: List[Dict]) -> bool:
        """Check if reasoning tags are properly formatted"""
        all_valid = True
        
        for i, msg in enumerate(messages):
            content = str(msg.get('content', ''))
            
            if '<think>' in content:
                # Found think tag - check for closing </think>
                if '</think>' not in content:
                    self.violations.append(ViolationError(
                        violation_type=ViolationType.MISSING_REASONING_TAGS,
                        message=f"Found <think> opening but no closing </think> tag",
                        location=f"message {i}, content contains '<think>'",
                        rule=self.rules['reasoning_format']
                    ))
                    all_valid = False
        
        return all_valid
    
    def _check_tool_response_format(self, messages: List[Dict]) -> bool:
        """Check if tool responses are properly formatted"""
        all_valid = True
        
        for i, msg in enumerate(messages):
            content = str(msg.get('content', ''))
            
            if '<tool_response>' in content:
                # Found tool response - check for closing </tool_response>
                if '</tool_response>' not in content:
                    self.violations.append(ViolationError(
                        violation_type=ViolationType.MISSING_TOOL_RESPONSE_TAGS,
                        message=f"Found <tool_response> opening but no closing </tool_response> tag",
                        location=f"message {i}, content contains '<tool_response>'",
                        rule=self.rules['tool_response_format']
                    ))
                    all_valid = False
        
        return all_valid
    
    def _check_compact_summaries(self, messages: List[Dict]) -> bool:
        """Check if compact summaries are properly handled"""
        all_valid = True
        
        for i, msg in enumerate(messages):
            if msg.get('type') == 'system' and msg.get('subtype') == 'compact_boundary':
                # Found compact summary - check if properly formatted
                content = msg.get('content', '')
                if isinstance(content, str) and '[Conversation compacted. Summary:' in content:
                    # It's a summary but might not be properly extracted
                    if 'Summary:' not in content:
                        self.violations.append(ViolationError(
                            violation_type=ViolationType.COMPACT_SUMMARY_NOT_HANDLED,
                            message=f"Compact summary not properly extracted - missing 'Summary:' tag",
                            location=f"message {i}, type: system, subtype: compact_boundary",
                            rule=self.rules['compact_summary']
                        ))
                        all_valid = False
        
        return all_valid
    
    def get_detailed_report(self) -> str:
        """Get detailed report of all violations"""
        if not self.violations:
            return "✓ All template rules passed - no violations found"
        
        report = []
        report.append("\n❌ VIOLATIONS DETECTED:\n")
        
        for violation in self.violations:
            report.append(f"   Violation #{len(report)//5 + 1}:")
            report.append(f"   📋 WHAT: {violation.message}")
            report.append(f"   📍 WHERE: {violation.location}")
            report.append(f"   🔍 WHY: {violation.violation_type.value}")
            report.append(f"   ⚖️  RULE VIOLATED: {violation.rule}")
            report.append("")
        
        return "\n".join(report)

class MiMoV25SessionParser:
    """Parser for MiMo-V2.5 chat template format"""
    
    def __init__(self):
        self.checker = DetailedTemplateChecker()
    
    def convert_session_to_template(self, session_data: Dict) -> str:
        """Convert session data to MiMo-V2.5 template format"""
        messages = session_data.get('messages', [])
        
        # Run template compliance checks
        self.checker.run_all_checks(messages)
        
        # Build template output
        template_lines = []
        
        # Add system preamble (first system message)
        for msg in messages:
            if msg.get('type') == 'system':
                content = self._extract_text_content(msg)
                if content:
                    template_lines.append("<|im_start|>system\n" + content)
                break
        
        # Add default MiMo preamble if none exists
        if not any(msg.get('type') == 'system' and 'You are MiMo' in self._extract_text_content(msg) for msg in messages):
            template_lines.append("<|im_start|>system\nYou are MiMo, a helpful AI assistant engineered by Xiaomi.")
        
        # Process other messages
        for msg in messages:
            msg_type = msg.get('type')
            text_content = self._extract_text_content(msg)
            
            if msg_type == 'user':
                template_lines.append(f"<|im_start|>user\n{text_content}")
                template_lines.append("<|im_end|>")
            
            elif msg_type == 'assistant':
                assistant_line = "<|im_start|>assistant\n"
                
                # Extract reasoning
                reasoning = self._extract_reasoning(text_content)
                if reasoning:
                    assistant_line += f"<think>{reasoning}</think>"
                
                # Add remaining text
                remaining_text = self._remove_reasoning_tags(text_content)
                if remaining_text:
                    assistant_line += remaining_text
                
                template_lines.append(assistant_line)
                template_lines.append("<|im_end|>")
            
            elif msg_type == 'system' and msg.get('subtype') == 'compact_boundary':
                # Handle compact summary
                summary = self._extract_summary(msg)
                if summary:
                    template_lines.append(f"<|im_start|>system\n{summary}")
            
            elif msg_type == 'tool':
                template_lines.append(f"<tool_response>\n{text_content}\n</tool_response>")
                template_lines.append("<|im_end|>")
        
        # Add final end tag if missing
        if "<|im_end|>" not in "\n".join(template_lines):
            template_lines.append("<|im_end|>")
        
        return "\n".join(template_lines)
    
    def _extract_text_content(self, msg: Dict) -> str:
        """Extract text content from message"""
        content = msg.get('content', '')
        
        if isinstance(content, str):
            return content
        
        text_parts = []
        for block in content:
            if isinstance(block, dict):
                if block.get('type') == 'text':
                    text_parts.append(block.get('text', ''))
                elif block.get('type') == 'tool_result':
                    result_content = block.get('content', '')
                    if result_content == '[Old tool result cleared]':
                        text_parts.append('[Old tool result cleared]')
                    else:
                        text_parts.append(result_content)
        
        return ' '.join(text_parts)
    
    def _extract_reasoning(self, text: str) -> Optional[str]:
        """Extract reasoning content from text"""
        if not text:
            return None
        
        # Look for <think>...</think> patterns
        think_pattern = r'<think>(.*?)</think>'
        matches = re.findall(think_pattern, text, re.DOTALL)
        
        if matches:
            # Return the first reasoning found
            reasoning = matches[0].strip()
            return reasoning if reasoning else None
        
        return None
    
    def _remove_reasoning_tags(self, text: str) -> str:
        """Remove reasoning tags from text"""
        if not text:
            return ''
        
        # Remove <think>...</think> blocks
        cleaned = re.sub(r'<think>.*?</think>', '', text, flags=re.DOTALL)
        
        # Clean up extra whitespace
        return ' '.join(cleaned.split())
    
    def _extract_summary(self, msg: Dict) -> Optional[str]:
        """Extract summary from compact summary message"""
        content = msg.get('content', '')
        
        if isinstance(content, str):
            summary_match = re.search(r'Summary: (.+)', content)
            if summary_match:
                return summary_match.group(1)
        
        return None
    
    def generate_analysis(self, session_data: Dict, template_output: str) -> Dict[str, Any]:
        """Generate detailed analysis report"""
        messages = session_data.get('messages', [])
        
        # Get violation report
        violation_report = self.checker.get_detailed_report()
        
        # Calculate statistics
        message_types = {}
        for msg in messages:
            role = msg.get('type')
            message_types[role] = message_types.get(role, 0) + 1
        
        # Check for template compliance
        is_compliant = len(self.checker.violations) == 0
        
        return {
            'session_info': {
                'id': session_data.get('id'),
                'model': session_data.get('model'),
                'created_at': session_data.get('created_at'),
                'updated_at': session_data.get('updated_at'),
                'turn_count': session_data.get('turn_count'),
                'total_cost_usd': session_data.get('total_cost_usd'),
                'total_input_tokens': session_data.get('total_input_tokens'),
                'total_output_tokens': session_data.get('total_output_tokens')
            },
            'message_stats': {
                'total_messages': len(messages),
                'message_types': message_types,
                'has_compact_summaries': any(msg.get('type') == 'system' and msg.get('subtype') == 'compact_boundary' for msg in messages)
            },
            'template_compliance': {
                'is_compliant': is_compliant,
                'violation_count': len(self.checker.violations),
                'violation_report': violation_report
            },
            'template_output': template_output
        }

class DetailedReporter:
    """Generates detailed reports with what, where, why, and which rule failed"""
    
    @staticmethod
    def generate_detailed_analysis(session_data: Dict, analysis: Dict[str, Any]) -> str:
        """Generate detailed analysis report"""
        report = []
        
        report.append("=" * 80)
        report.append("DETAILED TEMPLATE COMPLIANCE ANALYSIS")
        report.append("=" * 80)
        report.append(f"Session ID: {analysis['session_info']['id']}")
        report.append(f"Model: {analysis['session_info']['model']}")
        report.append(f"Total Messages: {analysis['message_stats']['total_messages']}")
        report.append(f"Created: {analysis['session_info']['created_at']}")
        report.append(f"Updated: {analysis['session_info']['updated_at']}")
        report.append("")
        
        report.append("=" * 80)
        report.append("MESSAGE STATISTICS")
        report.append("=" * 80)
        
        for role, count in analysis['message_stats']['message_types'].items():
            report.append(f"  {role.capitalize()}: {count}")
        
        report.append(f"  Has Compact Summaries: {analysis['message_stats']['has_compact_summaries']}")
        report.append("")
        
        report.append("=" * 80)
        report.append("TEMPLATE COMPLIANCE ANALYSIS")
        report.append("=" * 80)
        
        if analysis['template_compliance']['is_compliant']:
            report.append("✓ COMPLIANT: All template rules passed")
        else:
            report.append(f"⚠️  VIOLATIONS: {analysis['template_compliance']['violation_count']} violations found")
            report.append("")
            
            # Extract detailed violation information
            violation_text = analysis['template_compliance']['violation_report']
            
            # Parse violations for detailed reporting
            if "VIOLATIONS DETECTED:" in violation_text:
                sections = violation_text.split("VIOLATIONS DETECTED:")
                
                for i, section in enumerate(sections[1:], 1):  # Skip first empty section
                    report.append(f"\n❌ VIOLATION #{i}:")
                    
                    # Extract information from each section
                    lines = section.strip().split('\n')
                    
                    for line in lines:
                        if line.strip():
                            if "WHAT:" in line:
                                report.append(f"   📋 WHAT: {line.split('WHAT:')[1].strip()}")
                            elif "WHERE:" in line:
                                report.append(f"   📍 WHERE: {line.split('WHERE:')[1].strip()}")
                            elif "WHY:" in line:
                                report.append(f"   🔍 WHY: {line.split('WHY:')[1].strip()}")
                            elif "RULE VIOLATED:" in line:
                                report.append(f"   ⚖️  RULE VIOLATED: {line.split('RULE VIOLATED:')[1].strip()}")
        
        report.append("")
        report.append("=" * 80)
        report.append("FORMATTED CHAT OUTPUT (first 2000 characters)")
        report.append("=" * 80)
        report.append(analysis['template_output'][:2000])
        
        if len(analysis['template_output']) > 2000:
            report.append("...\n(truncated)")
        
        return "\n".join(report)

class SessionProcessor:
    """Processes session files with detailed error reporting"""
    
    @staticmethod
    def process_files(session_paths: List[Path]) -> None:
        """Process multiple session files with detailed reporting"""
        all_results = []
        total_violations = 0
        
        for i, session_path in enumerate(session_paths, 1):
            print(f"\n{'='*80}")
            print(f"Processing session {i}/{len(session_paths)}: {session_path.name}")
            print(f"{'='*80}")
            
            if not session_path.exists():
                print(f"Warning: Session file not found: {session_path}")
                continue
            
            try:
                with open(session_path, 'r') as f:
                    session_data = json.load(f)
            except json.JSONDecodeError as e:
                print(f"Error: Invalid JSON in session file {session_path.name}: {e}")
                continue
            
            try:
                # Create parser and convert
                parser = MiMoV25SessionParser()
                template_output = parser.convert_session_to_template(session_data)
                analysis = parser.generate_analysis(session_data, template_output)
                
                # Generate detailed report
                report = DetailedReporter.generate_detailed_analysis(session_data, analysis)
                
                # Print the report
                print(report)
                
                # Save report
                output_path = session_path.with_name(f"{session_path.stem}_analysis.txt")
                with open(output_path, 'w', encoding='utf-8') as f:
                    f.write(report)
                
                print(f"\n✅ Analysis saved to: {output_path}")
                
                # Track results
                all_results.append({
                    'session_path': str(session_path),
                    'analysis': analysis,
                    'report_path': str(output_path)
                })
                
                total_violations += analysis['template_compliance']['violation_count']
                
            except Exception as e:
                print(f"Error processing session {session_path.name}: {e}")
                import traceback
                traceback.print_exc()
        
        # Print summary
        print(f"\n{'='*80}")
        print("PROCESSING SUMMARY")
        print(f"{'='*80}")
        print(f"Sessions processed: {len(session_paths)}")
        print(f"Successful conversions: {len([r for r in all_results if r['analysis']['template_compliance']['is_compliant']])}")
        print(f"Sessions with violations: {len([r for r in all_results if not r['analysis']['template_compliance']['is_compliant']])}")
        print(f"Total template violations found: {total_violations}")
        
        if all_results:
            print(f"\n📁 All reports saved to their respective session analysis files.")

def main():
    """Main function for command-line interface"""
    print("Enhanced MiMo-V2.5 Session Chat Parser")
    print("=" * 80)
    print("This script processes chat history JSON files from session storage")
    print("and converts them to MiMo-V2.5 chat template format.")
    print("")
    print("Usage:")
    print("  python enhanced_momo_chat_parser.py [session_paths]")
    print("")
    print("Examples:")
    print("  python enhanced_momo_chat_parser.py ~/.config/agent-code/sessions/*.json")
    print("  python enhanced_momo_chat_parser.py ~/.config/agent-code/sessions/273c5de0.json")
    print("  python enhanced_momo_chat_parser.py")
    print("")
    
    # Determine what to process
    session_paths = []
    
    if len(sys.argv) > 1:
        # Use provided paths
        for arg in sys.argv[1:]:
            path_obj = Path(arg).expanduser()
            if path_obj.exists():
                if path_obj.is_file():
                    session_paths.append(path_obj)
                elif path_obj.is_dir():
                    # Add all JSON files from directory
                    session_paths.extend(list(path_obj.glob("*.json")))
            else:
                # Try to find the path in sessions directory
                expanded = Path.home() / ".config/agent-code/sessions" / arg
                if expanded.exists():
                    session_paths.append(expanded)
                else:
                    print(f"Warning: Path not found: {arg}")
    else:
        # Default: process all session files
        sessions_dir = Path.home() / ".config/agent-code/sessions"
        if sessions_dir.exists():
            session_paths = list(sessions_dir.glob("*.json"))
            print(f"Default: Processing all {len(session_paths)} session files from {sessions_dir}")
        else:
            print(f"Error: Sessions directory not found: {sessions_dir}")
            print("Please provide session file paths as arguments")
            return
    
    if not session_paths:
        print("No session files to process")
        return
    
    # Process the files
    SessionProcessor.process_files(session_paths)

if __name__ == "__main__":
    main()