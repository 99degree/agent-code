# Normalize.rs Rewrite Plan

## Problem
The current `crates/lib/src/llm/normalize.rs` has:
- Massive code duplication (every function defined 3-4 times)
- Missing imports (`Uuid`, `NormalizeReport`)
- Missing helper functions (`remove_empty_messages`, `cap_document_blocks`, `merge_consecutive_user_messages`, `count_text_blocks`, `count_document_blocks`)

## Chat Template Constraints (from MiMo-V2.5 / Qwen2 tokenizer_config.json)

### MiMo-V2.5 (Qwen2-style)
- Role boundaries: `角色` / `角色`
- Tool grouping: `工具`
- Reasoning: `问题思考` / `回答`
- **Strict user/assistant alternation required**
- **System message required at start**

### Qwen2 / Llama3 with tool use
- Also require strict alternation
- System message recommended

### Implication for `normalize_strict()`:
- `ConsecutiveUserStrategy::InsertDummyAssistant` (preserve turn boundaries)
- `SystemMessageStrategy::EnsureDefault` (prepend if missing)
- `validate_alternation: true`
- `ensure_tool_result_pairing: true`

### `normalize_lenient()` for flexible templates:
- `ConsecutiveUserStrategy::Merge` (merge consecutive users)
- `SystemMessageStrategy::KeepExisting` (don't force system)
- `validate_alternation: false`

## Target API Surface (keep these exact signatures)

### Public types
```rust
pub enum ConsecutiveUserStrategy { Merge, InsertDummyAssistant, Keep }
pub enum SystemMessageStrategy { EnsureDefault, KeepExisting, RemoveAll }
pub struct NormalizationConfig { ... }
pub struct NormalizeReport { ... }
```

### Public functions
```rust
pub fn normalize_with_config(messages: &mut Vec<Message>, config: &NormalizationConfig)
pub fn normalize_messages(messages: &mut Vec<Message>)
pub fn normalize_strict(messages: &mut Vec<Message>)
pub fn normalize_lenient(messages: &mut Vec<Message>)
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
```

### Config presets
```rust
pub fn strict_config() -> NormalizationConfig
pub fn lenient_config() -> NormalizationConfig
```

## Implementation Order

1. **Imports**: `use uuid::Uuid; use super::message::*; use chrono;`
2. **Types**: `NormalizationConfig`, `NormalizeReport`, strategy enums
3. **Config presets**: `strict_config()`, `lenient_config()`, `Default`
4. **Core helpers** (private): `count_text_blocks`, `count_document_blocks`
5. **Normalization steps** (each once):
   - `remove_empty_messages`
   - `strip_empty_blocks`
   - `cap_document_blocks`
   - `merge_consecutive_user_messages`
   - `ensure_tool_result_pairing`
   - `insert_dummy_assistant_for_consecutive_users`
   - `ensure_system_message`
   - `validate_alternation`
6. **High-level API**:
   - `normalize_with_config` (dispatches to steps based on config)
   - `normalize_messages` / `normalize_strict` / `normalize_lenient` (presets)
   - `normalize_all` (returns `NormalizeReport`)
   - `truncate_to_last_summary`

## Required Fixes in Callers
After this compiles, update callers:
- `commands/mod.rs`: replace `normalize_all` / `normalize_messages` with `normalize_strict`
- `main.rs`: replace `normalize_all` with `normalize_strict`
- `ui/modern/app.rs`: replace `normalize_all` with `normalize_strict`
- `query/mod.rs`: uses `remove_empty_messages`, `cap_document_blocks`, `merge_consecutive_user_messages` - keep as-is (will be public)

## Test Checklist
- `cargo check -p agent-code-lib`
- `cargo test -p agent-code-lib llm::normalize`
- `cargo clippy -p agent-code-lib`