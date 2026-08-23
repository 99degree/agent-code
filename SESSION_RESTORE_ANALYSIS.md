# Session compaction & restore: what is saved, what is lost

Analysis of how the TUI compacts the conversation and persists it to the
session file, and why a restored session cannot fully replay the prior
environment.

## 1. What compaction does

Entry point: `QueryEngine::run_compaction_cascade` (`crates/lib/src/query/mod.rs:809`),
fired at a turn boundary when context ≥ `auto_compact_threshold(model)`
(auto) or when `/compact` is queued (`state.compact_requested`, consumed at
`crates/lib/src/query/mod.rs:1386`). Three-stage cascade, all of which
**mutate `state.messages` in place**:

1. **Microcompact** (`compact::microcompact`, `query/mod.rs:842`) — drops
   stale `tool_result` blocks beyond the last 5 messages. Cheap, always runs.
2. **LLM compaction** (`compact::compact_with_llm`, `query/mod.rs:858`) — if
   still over threshold, summarizes older messages into a `UserMessage` with
   `is_compact_summary = true` (`llm/message.rs:50`). Sets
   `compact_tracking.was_compacted`.
3. **Context-collapse fallback** (`context_collapse::collapse_to_budget`,
   `query/mod.rs:878`) — snips the middle of the history to a token budget.
   Last resort: aggressive microcompact to 2.

`PreCompact`/`PostCompact` hooks fire around it. The realized message/token
delta is reported once at the end.

**Key consequence:** after compaction, `state.messages` *is* the compacted
conversation. The original verbatim turns are gone. The compaction markers
(`is_compact_summary`, and `SystemMessage { subtype: CompactBoundary }`) are
themselves messages, so they ride along inside `messages` and are persisted
— a resumed session therefore will not immediately re-compact.

## 2. How it is saved to the session file

Snapshot is built by `session_snapshot` (`crates/cli/src/ui/modern/run.rs:2782`)
from the live engine `AppState`, then `write_session_snapshot`
(`run.rs:2818`) → `save_session_full` (`crates/lib/src/services/session.rs:222`).

The on-disk `SessionData` (`session.rs:63`) carries only:

| Field | Source in `AppState` |
|---|---|
| `id`, `created_at`, `updated_at` | session identity |
| `messages` | `st.messages` ← **the (compacted) conversation** |
| `cwd`, `repo`, `base_url` | `st.cwd` / git / `config.api.base_url` |
| `model` | `config.api.model` |
| `turn_count` | `st.turn_count` |
| `total_cost_usd` | `st.total_cost_usd` |
| `total_input_tokens`, `total_output_tokens` | `st.total_usage` (input/output only) |
| `plan_mode` | `st.plan_mode` |
| `brief_mode` | `st.brief_mode` |
| `response_style` | `st.response_style.name()` |
| `label`, `tags` | user-set |
| `provider` | resolved `ProviderIdentity` (base_url + auth_mode) |

Write path: `serialize_masked` (`session.rs:179`, secret-masked JSON) →
`atomic_write_secret` (temp + rename under a per-session `flock`). Save timing:
- periodic checkpoint every `SAVE_INTERVAL = 120s` while idle (`run.rs:2679`);
- before a `/resume` overwrites the engine in place (`run.rs:1608`);
- inline at process exit after the turn is joined (`run.rs:2744`).

On restore, `run.rs` (~1600–1700) rewrites the engine: sets `session_id`,
normalizes restored `messages` with `normalize_strict`, copies back
`model`/`base_url`/`brief_mode`/`response_style`/`plan_mode`, rebuilds the
checklist from messages (`adopt_restored_todos`), and rebuilds the visible
transcript via `transcript_from_messages` + `restore_transcript`.

## 3. The restore gap: state that is never persisted

The conversation (`messages`) and the scalar counters above *are* saved. But
a large slice of live session/environment state lives only in memory and is
**lost on a process restart** (the only thing `/resume` re-reads is the JSON
file). Notable missing pieces:

### Lost on every restore (no field in `SessionData`)
- **Reasoning effort** — `config.api.effort` is *not* in `SessionData` and
  *not* in `session_snapshot`. On resume it is taken from the running
  process's engine default (`run.rs:1687`), so a session run with
  `--effort high` / `/effort` reverts to the default.
- **Working-set dirs** — `additional_dirs` (from `/add-dir`,
  `state/mod.rs:130`). Not saved; the restored system prompt no longer knows
  it may read/edit outside `cwd`.
- **Fast mode** — `pre_fast_model` (`state/mod.rs:146`). Fast mode silently
  ends on restart.
- **Disk output style** — `disk_output_style` (`state/mod.rs:142`) is not
  saved; only the built-in `response_style.name()` is. A disk-loaded style
  (via `/output-style <name>` mapping to a file) does not survive.
- **Per-model usage breakdown** — `model_usage: HashMap<String, Usage>` is
  never written; only the aggregate `input`/`output` tokens are.
- **Cache token counters** — `cache_creation_input_tokens` /
  `cache_read_input_tokens` on `Usage` are dropped; only input/output are
  persisted, so the cached-prefix accounting restarts from zero.
- **Background tasks / subagents** — `TaskManager` (`state/mod.rs:117`) is not
  serialized. Running/paused local subagents and their output are gone; the
  `/tasks` pane is empty after restore.

### Per-session, in-memory only (lost on restart, not by design)
- **Queued prompts** — `app.queue` is never persisted; unsent prompts are
  dropped (the exit path only prints them, `run.rs:590`).
- **"Allow for this session" grants** — `PermissionChecker` session grants
  are in-memory; restored sessions re-ask. (Deliberately rebuilt after reset,
  `run.rs:1681`.)
- **Scroll / expand / selection view** — `SessionViews` (`session_views.rs`)
  is an in-process cache bounded to `MAX_VIEWS = 8` / `MAX_BYTES = 8 MiB`.
  Within one process, switching away and back restores your place; a
  cross-process restart rebuilds the transcript from the bottom with
  everything collapsed (`restore_transcript`, `session_picker.rs:760`).
- **Composer draft, modal/picker state, completion menu** — all transient UI
  state, not persisted.

### Persisted elsewhere (restored, not via the session file)
- Theme, edit mode (vi/emacs), keybindings, MCP config — live in the config
  files, reloaded at startup.

## 4. Net effect

A `/resume` rehydrates the **conversation text + scalar counters + plan/brief/
style/model/cwd** faithfully, and the compaction result is preserved inside
`messages`. What it *cannot* replay is the surrounding environment the
conversation was produced in: reasoning effort, working-set dirs, fast mode,
disk output style, cache-accounting continuity, live subagents/tasks, queued
prompts, and the exact on-screen view. For a session that was mid-compaction
or mid-turn, the persisted `messages` are authoritative but the *last word of
state* (effort, dirs, tasks) is what feels "missing" after restore.

## 5. Candidate fixes (highest impact first)
1. Add `effort: Option<String>` to `SessionData` + `session_snapshot` and
   restore it (`run.rs:1687`). One-line-ish, high user-visible impact.
2. Persist `additional_dirs` / `pre_fast_model` / `disk_output_style` in the
   snapshot (they are already `AppState` fields).
3. Persist `model_usage` and the cache-token counters in `Usage`.
4. Serialize a lightweight `TaskManager` summary for the `/tasks` pane (or
   accept it as non-restorable and document it).
5. Persist `app.queue` (or flush it to a sidecar) so queued prompts survive.
6. Make `SessionViews` a durable sidecar keyed by session id for cross-process
   scroll/expand restore (or accept it as in-process-only and document).

None of these touch the security invariants in AGENTS.md §3; they only widen
what `save_session_full` round-trips.

## 6. The "Ctrl+C then Ctrl+D drops the last message" bug (verified)

### Symptom
Quit via Ctrl+C (cancel in-progress turn) followed by Ctrl+D, and the most
recent assistant reply is missing or truncated from the restored session — the
messages you saw on screen are not in the file.

### Root cause (engine-side, not save-timing)
The session file is built from `state.messages` (`session_snapshot`,
`run.rs:2782`). The assistant reply is pushed to `state.messages` only at:
- `query/mod.rs:2049` — after `StreamEvent::Done` (normal finish);
- `query/mod.rs:1944` — the cancel branch, but only `if !content_blocks.is_empty()`.

`content_blocks` is populated **only** by `StreamEvent::ContentBlockComplete`
(`query/mod.rs:1821`, `1905`). The text the user reads on screen is delivered
as `StreamEvent::TextDelta`, which is forwarded straight to the UI via
`sink.on_text` (`query/mod.rs:1773`) and is **never accumulated** into
`content_blocks`. So a reply cancelled before its text block received a
`ContentBlockStop` is never assembled and is dropped — it lives only in the UI
transcript, not in `state.messages`, and therefore never reaches disk.

### Why it feels intermittent
If the turn had already emitted `Done` before Ctrl+C, the message is saved
normally. The loss is precisely the in-progress (mid-stream) reply you
cancelled — hence "the latest few messages".

### The worse cousin: double Ctrl+C hard-exits
`install_signal_handler` (`query/mod.rs:1090`) listens for OS SIGINT. A second
Ctrl+C while already cancelled calls `std::process::exit(130)`
(`query/mod.rs:1099`), which bypasses the entire save block in
`run_modern_tui` (`run.rs:2707`). That loses everything since the last 120s
autosave checkpoint / last `/resume`. The Ctrl+D in the "Ctrl+C then Ctrl+D"
sequence is a TUI key (`run.rs:3406`) → graceful `should_quit` → save, so it
does NOT hit this path; but any true double-Ctrl+C does.

### Fix direction (engine-side; the `StreamParser` is out of reach)
The `StreamParser` (`stream.rs:161`) lives in the **provider's** stream task
and is dropped the instant `rx.recv()` ends — the engine never holds it, so it
cannot be flushed on cancel. The fix must live in the engine loop:

1. Add a `pending_text: String` to the streaming state (`query/mod.rs:1755`).
2. On `StreamEvent::TextDelta(text)` (`query/mod.rs:1773`): also append to
   `pending_text` (currently it only calls `sink.on_text`).
3. On `StreamEvent::ContentBlockComplete(ContentBlock::Text { .. })`
   (`query/mod.rs:1905`): clear `pending_text` — the completed block already
   carries the full text, so no double-counting.
4. On the cancel branch (`query/mod.rs:1942`): if `!pending_text.is_empty()`,
   push `ContentBlock::Text { text: pending_text.clone() }` into
   `content_blocks` **before** the existing `if !content_blocks.is_empty()`
   guard, so the partial reply is recorded with the rest.

This makes mid-stream cancel capture exactly the text the user already saw on
screen, and it flows into `state.messages` → `session_snapshot` → disk. The
already-completed `content_blocks` already survive cancel; only the in-flight
block was being lost.

Neither option alters the AGENTS.md §3 security invariants — they only broaden
what `state.messages` contains at cancel time.

### Second, orthogonal loss: OS double-Ctrl+C hard-exit
Aside from the mid-stream cancel, the SIGINT handler
(`install_signal_handler`, `query/mod.rs:1090`) calls
`std::process::exit(130)` on a second Ctrl+C while already cancelled
(`query/mod.rs:1099`). That exit bypasses the entire save block in
`run_modern_tui` (`run.rs:2707`) and loses everything since the last 120s
autosave checkpoint or last `/resume`. The Ctrl+D in the "Ctrl+C then Ctrl+D"
sequence is a TUI key (`run.rs:3406`) → graceful `should_quit` → save, so it
does **not** hit this path; but any literal second Ctrl+C at a real terminal
does. Hardening that path (e.g. persist a checkpoint before `process::exit`)
is a separate, larger fix.
