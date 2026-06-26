# Hermes Agent Connector — Exploration Report

**Date:** 2026-06-26
**Status:** Exploration complete, ready for implementation planning
**Author:** Scout agent

---

## 1. Mission

Explore the feasibility and surface area of building an OpenSessionLog connector for [Hermes Agent](https://hermes-agent.nousresearch.com/) — the conversational AI brain of a household assistant system running on the same machine as OpenSessionLog.

The goal: ingest Hermes session data from `~/.hermes/state.db` into an OpenSessionLog vault, allowing Hermes conversations to be searched, exported, and analyzed alongside OpenCode and Claude Code sessions.

---

## 2. Environment Inventory

This machine (Dell OptiPlex 3050, Ubuntu 24.04) is a live household AI assistant. Relevant services:

| Service | Location | Format |
|---------|----------|--------|
| Hermes agent | `~/.hermes/` | SQLite (`state.db`), config (`config.yaml`), session dumps |
| OpenCode | `~/.local/share/opencode/` | SQLite (`opencode.db`, 242 MB, ~thousands of sessions) |
| OSL vault | `~/.opensessionlog/data.sqlite` | OSL's own SQLite vault |
| OSL watch daemon | systemd user service | Auto-ingests `opencode.db` every 60s |

Existing OpenSessionLog connectors:

| Connector | Source | Format |
|-----------|--------|--------|
| `claude` | Claude Code CLI | `*.jsonl` session files, recursive directory discovery |
| `opencode` | OpenCode app | `opencode.db` (SQLite), discovers all sessions via `SELECT id FROM session` |

---

## 3. Hermes Storage Format — `state.db`

Hermes stores session data in **`~/.hermes/state.db`**, a SQLite database with two primary tables.

### 3.1 `sessions` table

```sql
CREATE TABLE sessions (
    id              TEXT PRIMARY KEY,       -- e.g. "20260626_070301_b669cd23"
    source          TEXT NOT NULL,           -- "telegram" | "cron"
    user_id         TEXT,
    model           TEXT,                    -- e.g. "deepseek-v4-flash-free"
    model_config    TEXT,
    system_prompt   TEXT,
    parent_session_id TEXT,
    started_at      REAL NOT NULL,           -- Unix seconds (fractional)
    ended_at        REAL,
    end_reason      TEXT,                    -- "session_reset" | "cron_complete" | "compression" | "agent_close" | NULL
    message_count   INTEGER DEFAULT 0,
    tool_call_count INTEGER DEFAULT 0,
    input_tokens    INTEGER DEFAULT 0,
    output_tokens   INTEGER DEFAULT 0,
    cache_read_tokens  INTEGER DEFAULT 0,
    cache_write_tokens INTEGER DEFAULT 0,
    reasoning_tokens   INTEGER DEFAULT 0,
    billing_provider   TEXT,
    billing_base_url   TEXT,
    billing_mode       TEXT,
    estimated_cost_usd REAL,
    actual_cost_usd    REAL,
    cost_status        TEXT,
    cost_source        TEXT,
    pricing_version    TEXT,
    title              TEXT,
    api_call_count     INTEGER DEFAULT 0,
    handoff_state      TEXT,
    handoff_platform   TEXT,
    handoff_error      TEXT,
    cwd                TEXT,
    rewind_count       INTEGER NOT NULL DEFAULT 0,
    archived           INTEGER NOT NULL DEFAULT 0,
    FOREIGN KEY (parent_session_id) REFERENCES sessions(id)
);
```

**Stats (live):** 186 sessions — 132 `telegram`, 54 `cron`.

### 3.2 `messages` table

```sql
CREATE TABLE messages (
    id                  INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id          TEXT NOT NULL REFERENCES sessions(id),
    role                TEXT NOT NULL,       -- "user" | "assistant" | "tool" | "session_meta"
    content             TEXT,
    tool_call_id        TEXT,                -- links a "tool" role message to its call
    tool_calls          TEXT,                -- JSON array of tool call requests
    tool_name           TEXT,
    timestamp           REAL NOT NULL,       -- Unix seconds (fractional)
    token_count         INTEGER,
    finish_reason       TEXT,
    reasoning           TEXT,
    reasoning_content   TEXT,                -- model's chain-of-thought
    reasoning_details   TEXT,
    codex_reasoning_items  TEXT,
    codex_message_items    TEXT,
    platform_message_id    TEXT,
    observed            INTEGER DEFAULT 0,
    active              INTEGER NOT NULL DEFAULT 1,
    compacted           INTEGER NOT NULL DEFAULT 0
);
```

**Stats (live):** 12,247 messages across all sessions.

### 3.3 Tool Calls Format

Tool call requests are stored as a JSON array in `messages.tool_calls` (on `role = "assistant"` rows):

```json
[
  {
    "id": "call_00_abc123",
    "call_id": "call_00_abc123",
    "response_item_id": "fc_call_00_abc123",
    "type": "function",
    "function": {
      "name": "household_memory_search",
      "arguments": "{\"query\": \"...\"}"
    }
  }
]
```

Tool responses are stored on separate `role = "tool"` rows, linked via `tool_call_id` (matches `call_id` / `id` from the request).

### 3.4 Additional Hermes Files

| Path | Description |
|------|-------------|
| `~/.hermes/sessions/sessions.json` | Gateway routing index (maps platform chats to session IDs) — **not** a sessions list |
| `~/.hermes/sessions/request_dump_*.json` | Full API request/response dumps (only 8 present; `write_json_snapshots: false` in config) |
| `~/.hermes/data/household.db` | Household MCP server data (chores, shopping, calendar — not agent sessions) |

---

## 4. OpenSessionLog Connector Pattern

From `src/connector/mod.rs`:

```rust
pub trait Connector {
    fn name(&self) -> &'static str;
    fn discover(&self, directory: &Path) -> Result<Vec<SessionRef>>;
    fn session_id(&self, session_ref: &SessionRef) -> Uuid { /* deterministic UUIDv5 */ }
    fn parse(&self, session_ref: &SessionRef) -> Result<NormalizedSession>;
}
```

`SessionRef` contains: `source` (connector name), `native_id` (session's own ID), `path` (file path), `project_path` (optional).

The `for_source("hermes")` factory in `mod.rs` dispatches to the right connector. Routing in `ingest.rs` currently maps by file extension (`.db` → opencode, `.jsonl` → claude, directories → claude).

---

## 5. Mapping: Hermes → NormalizedSession

| NormalizedSession field | Hermes source | Notes |
|------------------------|---------------|-------|
| `id` | — | `session_id("hermes", native_id)` via `ids.rs` |
| `source` | — | `"hermes"` |
| `native_id` | `sessions.id` | e.g. `"20260626_070301_b669cd23"` |
| `title` | `sessions.title` | May be NULL |
| `started_at` | `sessions.started_at` | **REAL (Unix seconds)** — need `seconds_to_iso()` helper |
| `ended_at` | `sessions.ended_at` | Same |
| `model` | `sessions.model` | e.g. `"deepseek-v4-flash-free"` |
| `git_branch` | — | Not available in Hermes; always `None` |
| `git_sha` | — | Not available; always `None` |
| `raw_path` | — | Path to `state.db` |
| `project_root` | `sessions.cwd` | Map to `PathBuf` |
| `parent_session_id` | `sessions.parent_session_id` | Map via `session_id()` |
| `input_tokens` | `sessions.input_tokens` | |
| `output_tokens` | `sessions.output_tokens` | |
| `cache_read_tokens` | `sessions.cache_read_tokens` | |
| `cache_write_tokens` | `sessions.cache_write_tokens` | |
| `tool_call_count` | `sessions.tool_call_count` | |
| `error_count` | — | Count of errata encountered during parse |
| `messages` | Join messages WHERE session_id = ? | See message mapping below |
| `tool_calls` | Correlate assistant+tool messages | See tool call mapping below |
| `errata` | Generated during parse | For malformed JSON, unknown roles, etc. |

### 5.1 Message Mapping

| NormalizedMessage field | Hermes source |
|------------------------|---------------|
| `uuid` | `message_id(sid, format!("{msg_row.id}"))` |
| `role` | `messages.role` (skip `session_meta`) |
| `content` | `messages.content` |
| `thinking` | `messages.reasoning_content` |
| `parent_uuid` | Not directly available; could use `sessions.parent_session_id` on first message |
| `source_seq` | Enumerate from 0 |
| `turn_number` | Increment on user/assistant messages |
| `sequence` | 0 (single-part messages) |
| `input_tokens` | Set on `assistant` messages from `messages.token_count` |
| `output_tokens` | — |

### 5.2 Tool Call Mapping

Tool calls require correlating `assistant` messages (which contain `tool_calls` JSON) with subsequent `tool` messages (which contain `tool_call_id` + `content`). Approach:

1. On `role = "assistant"` rows with non-NULL `tool_calls`: parse JSON, emit `NormalizedToolCall` entries with `tool_output = None` and `response_message_uuid = None`
2. Maintain a map of `tool_call_id → NormalizedToolCall` (pending resolution)
3. On `role = "tool"` rows: look up `tool_call_id` in pending map, fill in `tool_output` and `response_message_uuid`
4. After processing all messages, any unresolved tool calls still have `is_error = None`

| NormalizedToolCall field | Hermes source |
|--------------------------|---------------|
| `uuid` | `tool_call_id(sid, msg_uuid, call_id)` |
| `call_id` | `tool_call.id` / `tool_call.call_id` |
| `tool_name` | `tool_call.function.name` |
| `tool_input` | `tool_call.function.arguments` |
| `tool_output` | `messages.content` on the corresponding `tool` row |
| `tool_output_raw` | `None` |
| `is_error` | Not explicitly available; infer from content heuristics or `None` |
| `started_at` | `messages.timestamp` of the assistant message |
| `completed_at` | `messages.timestamp` of the tool response message |
| `request_message_uuid` | UUID of the assistant message |
| `response_message_uuid` | UUID of the tool message (resolved via correlation) |

---

## 6. Routing & Detection Strategy

The current `ingest.rs` routing is extension-based:

```
.db / .sqlite  →  opencode connector (fails if schema doesn't match)
.jsonl          →  claude connector
directory       →  claude connector
```

To add Hermes support, the routing needs a **schema detection** step when opening a `.db` file:

1. Open SQLite connection
2. Check for Hermes-signature tables: `SELECT name FROM sqlite_master WHERE name IN ('compression_locks', 'state_meta', 'schema_version')`
3. If Hermes tables exist → route to `hermes` connector
4. Else → fall back to `opencode` connector (existing behavior)

Alternatively, since Hermes state.db is always at `~/.hermes/state.db`, the connector's `discover()` could be called explicitly by the user, or the routing could detect by checking `PRAGMA schema_version` or looking for the `sessions` table schema (Hermes `sessions(id TEXT PRIMARY KEY, ...)` vs OpenCode `session(id TEXT PRIMARY KEY, ...)`).

**Recommendation:** Use table existence check (`compression_locks` is unique to Hermes).

---

## 7. Key Differences & Risks

### 7.1 Timestamp Format

**Critical.** OpenCode uses `INTEGER` milliseconds since Unix epoch. The existing `ms_to_iso()` helper assumes `i64`. Hermes uses `REAL` seconds (e.g., `1782496825.270838`). Need a new `seconds_to_iso()` function (or make the existing one generic).

### 7.2 Role Vocabulary

Hermes has an additional `session_meta` role that OpenCode (which has `user`/`assistant` only, with system events in a separate `session_message` table) does not. Must skip `session_meta` rows.

### 7.3 Source Labeling

Hermes sessions have `source = "telegram"` or `"cron"` in their own schema. The connector's `name()` returns `"hermes"`, but we lose the sub-source granularity. Consider embedding the Hermes source in `native_id` (e.g., `"telegram:20260626_070301_b669cd23"`) or adding it to the `title` / an erratum note.

### 7.4 No Git Metadata

Hermes doesn't track git state. `git_branch` and `git_sha` will always be `None`.

### 7.5 Token Accounting

Hermes has per-message `token_count` but also per-session aggregated `input_tokens` / `output_tokens`. The aggregated values may or may not sum to the per-message values. Use session-level totals for the top-level fields (matching OpenCode behavior).

### 7.6 Tool Call Correlation Timing

Tool calls and their responses may span multiple assistant/tool message pairs. The correlation must be careful about ordering within a session (same session_id, ordered by `id` / `timestamp`).

---

## 8. Implementation Checklist

- [ ] **`src/connector/hermes.rs`** — New connector file implementing `Connector` trait
  - [ ] `name()` → `"hermes"`
  - [ ] `discover()` — Open `state.db`, query `SELECT id FROM sessions WHERE archived = 0`
  - [ ] `parse()` — Read session + messages + tool calls, build `NormalizedSession`
  - [ ] Helper: `seconds_to_iso()` for `REAL` timestamps
  - [ ] Unit tests with in-memory Hermes-schema SQLite
- [ ] **`src/connector/mod.rs`** — Register `pub mod hermes;` and `"hermes" => Some(Box::new(HermesConnector))`
- [ ] **`src/ingest.rs`** — Add Hermes detection logic for `.db` files (check for `compression_locks` table)
- [ ] **`tests/fixtures/hermes/`** — Create test fixtures (minimal session, tool call session, multi-turn)
- [ ] **`osl watch` support** — Add `~/.hermes/state.db` as a default watch path (or document manual invocation)
- [ ] **Integration test** — Run `osl ingest ~/.hermes/state.db` on the live database, verify output

---

## 9. Test Fixture Plan

Create synthetic Hermes-style SQLite databases in `tests/fixtures/hermes/`:

| Fixture | Description |
|---------|-------------|
| `minimal.db` | Single user → assistant exchange, no tools |
| `with_tool_call.db` | User → assistant (tool call) → tool response → assistant |
| `multi_turn.db` | 3+ user/assistant exchanges |
| `with_reasoning.db` | Assistant messages with `reasoning_content` |
| `cron_session.db` | A cron-triggered session (source = "cron") |
| `empty.db` | No sessions (edge case) |

---

## 10. Questions for Team Lead

1. **Sub-source preservation:** Should we embed the Hermes `source` (telegram/cron) into the `native_id` or store it in `title` / a tag system?
2. **`cwd` → `project_root`:** Hermes sessions may have NULL or generic cwds. Should we still map it?
3. **Parent sessions:** Hermes supports `parent_session_id`. Should we resolve cross-session parent references, or skip for now (Phase-1 gap)?
4. **Watch integration:** Should the OSL watch daemon be extended to monitor `~/.hermes/state.db` alongside `opencode.db`, or should this be a separate user service?
5. **Cost data:** Hermes tracks `estimated_cost_usd` and `actual_cost_usd`. The OSL model doesn't have a cost field — should we add one, or exclude for now?

---

## 11. Appendix: Live Data Samples

### Hermes session row
```
id=20260626_070301_b669cd23
source=telegram
model=deepseek-v4-flash-free
message_count=14
tool_call_count=4
input_tokens=0
output_tokens=0
started_at=1782471781.2060373
ended_at=1782472023.2029707
```

### Hermes messages (truncated)
```
#13072 role=user
  content: "Looks like Rebekah has a pretty severe cold..."

#13073 role=assistant
  tool_calls: [{"id":"call_00_...", "function":{"name":"household_family_members_list",...}}]
  reasoning_content: "Let me check the family roster first..."

#13074 role=tool
  content: '{"members": [...]}'
  tool_call_id: call_00_...
  tool_name: household_family_members_list
```

### Existing OpenCode connector (reference)
- 1,040 lines in `src/connector/opencode.rs`
- Handles `session`, `message`, `part` tables
- Complex tool call deduplication (callIDs can repeat across messages)
- Extensive test suite with in-memory DB fixtures
