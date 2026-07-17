# Normalization Implementation Plan

## Overview
This document describes the complete implementation plan for fixing message normalization in agent-code to support MiMo-V2.5/Qwen2 chat templates, handle edge cases, and restore broken call sites.

## Problem Statement

### Current Issues
1. **`normalize_for_mimo` does not exist** - Called in 4 places but missing from `normalize.rs`
2. **No strict normalization mode** - Current `normalize_messages` merges consecutive users (lenient), but MiMo requires dummy assistant insertion
3. **Missing system message enforcement** - MiMo requires system message at start
4. **Edge cases break history** - LLM errors, cancellations, network issues leave orphaned tool_use blocks
5. **Per-turn normalization unclear** - `query/mod.rs` uses ad-hoc calls instead of centralized config

### Chat Template Requirements (MiMo-V2.5 / Qwen2)

From `tokenizer_config.json`:
- Role boundaries: `角色` / `角色`
- Tool grouping: `工具`  
- Reasoning: `问题思考` / `回答`
- **Strict user/assistant alternation required**
- **System message required at start**

### Implications for `normalize_strict`:
- `ConsecutiveUserStrategy::InsertDummyAssistant` (preserve turn boundaries)
- `SystemMessageStrategy::EnsureDefault` (prepend if missing)
- `validate_alternation: true`
- `ensure_tool_result_pairing: true`

---

## API Surface (Target)

### New Types in `normalize.rs`
```rust
pub enum ConsecutiveUserStrategy { Merge, InsertDummyAssistant, Keep }
pub enum SystemMessageStrategy { EnsureDefault, KeepExisting, RemoveAll }
pub struct NormalizationConfig { ... }
pub struct NormalizeReport { ... }
```

### New Public Functions
```rust
pub fn normalize_with_config(messages: &mut Vec<Message>, config: &NormalizationConfig)
pub fn normalize_messages(messages: &mut Vec<Message>)          // lenient (default)
pub fn normalize_strict(messages: &mut Vec<Message>)            // MiMo/Qwen2 strict
pub fn normalize_lenient(messages: &mut Vec<Message>)           // explicit lenient
pub fn normalize_all(messages: &mut Vec<Message>) -> NormalizeReport
pub fn truncate_to_last_summary(messages: &mut Vec<Message>) -> Vec<Message>
pub fn insert_dummy_assistant_for_consecutive_users(messages: &mut Vec<Message>)
pub fn ensure_system_message(messages: &mut Vec<Message>)
pub fn ensure_tool_result_pairing(messages: &mut Vec<Message>)
pub fn strip_empty_blocks(messages: &mut [Message])
pub fn remove_empty_messages(messages: &mut Vec<Message>)
pub fn cap_document_blocks(messages: &mut Vec<Message>, max_bytes: usize)
pub fn merge_consecutive_user_messages(messages: &mut Vec<Message>)
pub fn validate_alternation(messages: &[Message]) -> Result<(), String>

// Config presets
pub fn strict_config() -> NormalizationConfig
pub fn lenient_config() -> NormalizationConfig
```

### Config Values

| Config | `strict_config()` | `lenient_config()` | `default()` |
|--------|-------------------|---------------------|-------------|
| consecutive_user_strategy | InsertDummyAssistant | Merge | Merge |
| system_message_strategy | EnsureDefault | KeepExisting | KeepExisting |
| validate_alternation | true | false | true |
| ensure_tool_result_pairing | true | true | true |
| strip_empty_blocks | true | true | true |
| remove_empty_messages | true | true | true |
| max_document_bytes | 500_000 | 500_000 | 500_000 |

---

## Call Sites to Fix (4 locations)

### 1. `crates/cli/src/main.rs:995` - Startup `--session`
```rust
// BEFORE:
agent_code_lib::llm::normalize::normalize_for_mimo(&mut state.messages);

// AFTER:
agent_code_lib::llm::normalize::normalize_strict(&mut state.messages);
```

### 2. `crates/cli/src/commands/mod.rs:972` - `/resume <id>`
```rust
// BEFORE:
agent_code_lib::llm::normalize::normalize_for_mimo(&mut state.messages);

// AFTER:
agent_code_lib::llm::normalize::normalize_strict(&mut state.messages);
```

### 3. `crates/cli/src/commands/mod.rs:5938` - Session picker resume
```rust
// BEFORE:
agent_code_lib::llm::normalize::normalize_for_mimo(&mut state.messages);

// AFTER:
agent_code_lib::llm::normalize::normalize_strict(&mut state.messages);
```

### 4. `crates/cli/src/ui/modern/app.rs:1040` - Modern TUI resume
```rust
// BEFORE:
agent_code_lib::llm::normalize::normalize_for_mimo(&mut state.messages);

// AFTER:
agent_code_lib::llm::normalize::normalize_strict(&mut state.messages);
```

---

## Per-Turn Normalization in `query/mod.rs`

### Current (lines 848-853):
```rust
crate::llm::normalize::ensure_tool_result_pairing(&mut self.state.messages);
crate::llm::normalize::strip_empty_blocks(&mut self.state.messages);
crate::llm::normalize::remove_empty_messages(&mut self.state.messages);
crate::llm::normalize::cap_document_blocks(&mut self.state.messages, 500_000);
crate::llm::normalize::merge_consecutive_user_messages(&mut self.state.messages);
```

### Proposed:
```rust
// Explicit lenient config for per-turn (keeps current behavior)
crate::llm::normalize::normalize_lenient(&mut self.state.messages);
```

**Rationale**: Per-turn normalization should be lenient to avoid inserting dummy messages into active history. Strict normalization is only needed at resume boundaries where chat template constraints apply.

---

## Edge Case Handling

### 1. LLM Provider Error (Network/Rate Limit/API Error)
**Location**: `query/mod.rs:1170-1200`
- **Current**: Pushes system error, returns error, does NOT push partial assistant or tool_results
- **Fix**: `normalize_strict` at resume adds synthetic tool_results for orphaned tool_use

### 2. Stream Error with Tool Use
**Location**: `query/mod.rs:1323`
- If recoverable: retries with recovery message
- If not: executes tool calls without assistant message visible to user
- **Fix**: Same as above - resume normalization repairs

### 3. User Cancellation (Ctrl+C)
**Location**: `query/mod.rs:1331-1395` ✓ **Already correct**
- Pushes partial assistant message + tool_results "(cancelled)"

### 4. Max Output Tokens Recovery
**Location**: `query/mod.rs:1440-1470`
- Adds recovery message, continues same turn
- Multiple partial responses possible in one turn

### 5. Max Turns Reached
**Location**: `query/mod.rs:1810` ✓ **Clean**

---

## Implementation Steps

### Step 1: Rewrite `normalize.rs` (Single PR)

1. **Add imports**: `use uuid::Uuid; use chrono::Utc; use super::message::*;`
2. **Define types**: `ConsecutiveUserStrategy`, `SystemMessageStrategy`, `NormalizationConfig`, `NormalizeReport`
3. **Add config presets**: `strict_config()`, `lenient_config()`, `Default`
4. **Private helpers**: `count_text_blocks()`, `count_document_blocks()`
5. **Core steps** (each once, public for query/mod.rs):
   - `remove_empty_messages()`
   - `strip_empty_blocks()`
   - `cap_document_blocks()`
   - `merge_consecutive_user_messages()`
   - `ensure_tool_result_pairing()`
   - `insert_dummy_assistant_for_consecutive_users()`
   - `ensure_system_message()`
   - `validate_alternation()`
6. **High-level API**:
   - `normalize_with_config()` - dispatcher
   - `normalize_messages()` - default (lenient)
   - `normalize_strict()` - MiMo/Qwen2
   - `normalize_lenient()` - explicit lenient
   - `normalize_all()` - returns report
   - `truncate_to_last_summary()` - existing logic

### Step 2: Fix 4 Call Sites (Same PR)
Replace all `normalize_for_mimo` → `normalize_strict`

### Step 3: Update Per-Turn Normalization (Same PR)
```rust
// query/mod.rs
crate::llm::normalize::normalize_lenient(&mut self.state.messages);
```

### Step 4: Tests
- Unit tests for each core step
- Integration tests for 4 edge case scenarios
- `cargo test -p agent-code-lib llm::normalize`

### Step 5: Lint
- `cargo clippy -p agent-code-lib`
- `cargo fmt --all -- --check`

---

## Test Scenarios

### Scenario 1: Resume after LLM error with orphaned tool_use
```
1. Session: user -> assistant(tool_use:"1")
2. LLM error (network) - no tool_result pushed
3. /resume <session>
4. normalize_strict adds tool_result("1", "(interrupted)", is_error=true)
5. History valid for next API call
```

### Scenario 2: Resume with consecutive user messages
```
1. Session: user -> assistant -> user -> user (no assistant)
2. /resume <session>
3. normalize_strict inserts dummy assistant between users
4. Alternation: user -> assistant -> user -> assistant(dummy) -> user
```

### Scenario 3: Resume without system message
```
1. Old session (no system message)
2. /resume <session>
3. normalize_strict prepends default system message
4. First message is now System
```

### Scenario 4: Compaction summary truncation
```
1. Session: 50 messages, compaction at 30
2. /resume <session>
3. truncate_to_last_summary drops first 30, keeps summary + 20
4. normalize_strict enforces MiMo on remaining 21
5. full_history preserves dropped 30 for disk
```

---

## Files Modified

| File | Change Type |
|------|-------------|
| `crates/lib/src/llm/normalize.rs` | Complete rewrite |
| `crates/cli/src/main.rs` | 1 line: normalize_for_mimo → normalize_strict |
| `crates/cli/src/commands/mod.rs` | 2 lines: normalize_for_mimo → normalize_strict |
| `crates/cli/src/ui/modern/app.rs` | 1 line: normalize_for_mimo → normalize_strict |
| `crates/lib/src/query/mod.rs` | ~6 lines → 1 line: normalize_lenient |

---

## Validation Commands

```bash
# Compile check
cargo check -p agent-code-lib
cargo check -p agent-code

# Unit tests
cargo test -p agent-code-lib llm::normalize

# All tests
cargo test --all-targets

# Lint
cargo clippy --all-targets -- -D warnings

# Format
cargo fmt --all -- --check
```

---

## Rollback Plan

If issues arise:
1. `git revert` the normalize.rs rewrite
2. Restore `normalize_for_mimo` as alias to `normalize_messages` (lenient)
3. This maintains backward compatibility while fixing call sites

---

## Future Considerations

1. **Provider-specific configs**: Different models may need different strategies
2. **Configuration file**: Allow users to set default normalization mode
3. **Telemetry**: Track which normalization path is used
4. **Benchmark**: Measure token overhead of dummy assistant insertion