# Edge Cases in Message History Normalization

## Current State
- `normalize_for_mimo` is called in 4 places but **does not exist** in `normalize.rs`
- `normalize_messages` exists but is lenient (merges consecutive users)
- `normalize_all` returns a report but is not used in resume paths

## Four Resume Paths (all call missing `normalize_for_mimo`)
1. `/resume <id>` - `commands/mod.rs:972`
2. Classic session picker - `commands/mod.rs:5938`
3. Modern TUI `apply_resume_action_resume` - `ui/modern/app.rs:1040`
4. Startup `--session` flag - `main.rs:995`

## Edge Cases That Break History

### 1. LLM Provider Error (network, rate limit, API error)
**Location**: `query/mod.rs:1170-1200`
```rust
// Pushes system error message, fires hooks, RETURNS ERROR
// Does NOT push partial assistant message
// Does NOT add tool_results for any tool_use blocks received before error
```
**Result**: Next turn starts with orphaned `tool_use` blocks in context → API error on next call

### 2. Stream Error with Tool Use Blocks
**Location**: `query/mod.rs:1323` (`got_error = true`)
- If error is recoverable (prompt too long, max tokens): retries with recovery message
- If NOT recoverable: falls through to Step 7, extracts tool calls, tries to execute them
- **Problem**: If `content_blocks` has tool_use but stream dies before Done event, those tool calls execute without user seeing the assistant message

### 3. User Cancellation (Ctrl+C)
**Location**: `query/mod.rs:1331-1395`
```rust
// ✓ Correctly handled:
if !content_blocks.is_empty() {
    push assistant_msg with content_blocks
    for each tool_use in content_blocks {
        push tool_result_message(id, "(cancelled)", true)
    }
}
```
**Result**: Clean - maintains tool_use/tool_result pairing

### 4. Max Output Tokens Recovery
**Location**: `query/mod.rs:1440-1470`
- Adds recovery message, continues loop
- Partial assistant message NOT pushed (continues same turn)
- **Result**: Could accumulate multiple partial responses in one turn

### 5. Session Resume After Compaction
**Location**: `session.rs` + `normalize.rs:truncate_to_last_summary`
- `truncate_to_last_summary` correctly drops pre-summary messages
- **Missing**: `normalize_for_mimo` function that should enforce MiMo constraints:
  - Strict user/assistant alternation (insert dummy assistant if needed)
  - System message at start (prepend if missing)
  - Tool use/result pairing (handled by `ensure_tool_result_pairing`)

### 6. Max Turns Reached
**Location**: `query/mod.rs:1810`
- Normal turn completion, pushes all messages
- **Result**: Clean

## Required Normalization for MiMo/Qwen2 Compatibility

### `normalize_strict()` (new function needed)
```rust
pub fn normalize_strict(messages: &mut Vec<Message>) {
    // 1. Ensure tool_use/tool_result pairing
    ensure_tool_result_pairing(messages);
    
    // 2. Remove empty blocks/messages
    strip_empty_blocks(messages);
    remove_empty_messages(messages);
    
    // 3. Cap documents
    cap_document_blocks(messages, 500_000);
    
    // 4. Handle consecutive users: INSERT DUMMY ASSISTANT (not merge)
    insert_dummy_assistant_for_consecutive_users(messages);
    
    // 5. Ensure system message at start
    ensure_system_message(messages);
    
    // 6. Validate alternation
    validate_alternation(messages); // Should pass now
}
```

### Key Differences from `normalize_messages`:
| Step | `normalize_messages` (lenient) | `normalize_strict` (MiMo) |
|------|-------------------------------|---------------------------|
| Consecutive users | Merge | Insert dummy assistant |
| System message | Keep existing | Ensure exists (prepend default) |
| Alternation | Validate only | Enforce via dummy insert |

## Call Sites That Need Fixing

### Replace `normalize_for_mimo` calls with `normalize_strict`:
1. `commands/mod.rs:972` - `/resume <id>`
2. `commands/mod.rs:5938` - session picker resume
3. `main.rs:995` - startup `--session`
4. `ui/modern/app.rs:1040` - modern TUI resume

### Also consider:
- `query/mod.rs:849` - per-turn normalization (currently uses `ensure_tool_result_pairing` + `strip_empty_blocks` + `remove_empty_messages` + `cap_document_blocks` + `merge_consecutive_user_messages`)
  - Should this use `normalize_strict`? Currently uses `merge` for consecutive users (lenient)
  - Per-turn might want lenient to avoid dummy messages in active history

## Testing Scenarios

### Scenario 1: Resume after LLM error with orphaned tool_use
1. User asks "run tests"
2. Assistant starts: `tool_use {id: "1", name: "bash", ...}`
3. Network error - LLM call fails
4. User runs `/resume <session>`
5. **Expected**: `normalize_strict` adds `tool_result {tool_use_id: "1", content: "(interrupted)", is_error: true}`

### Scenario 2: Resume with consecutive user messages
1. Session has: user -> assistant -> user -> user (no assistant between)
2. `/resume <session>`
3. **Expected**: `normalize_strict` inserts dummy assistant between the two users

### Scenario 3: Resume without system message
1. Old session (pre-system-message-requirement)
2. `/resume <session>`
3. **Expected**: `normalize_strict` prepends default system message

### Scenario 4: Compaction summary truncation
1. Session has 50 messages, compaction at message 30
2. `/resume <session>`
3. `truncate_to_last_summary` drops first 30, keeps summary + 20 after
4. `normalize_strict` enforces MiMo constraints on the 21 messages
5. **Full history** preserved in `full_history` field for disk persistence

## Implementation Plan

1. Add `normalize_strict` to `normalize.rs` (use `ConsecutiveUserStrategy::InsertDummyAssistant`, `SystemMessageStrategy::EnsureDefault`)
2. Add `normalize_lenient` for per-turn use (current default behavior)
3. Replace all 4 `normalize_for_mimo` calls with `normalize_strict`
4. Update `query/mod.rs` per-turn normalization to use `normalize_lenient` (explicit)
5. Add tests for the 4 scenarios above
6. Run `cargo test -p agent-code-lib llm::normalize`