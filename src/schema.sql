-- OpenSessionLog schema v1 — SQLite + FTS5, WAL mode (pragmas set at runtime)
-- Embeddings (messages.embedding) are NULL in Phase 1; sqlite-vec lands in Phase 2.

CREATE TABLE vault_config (
    key         TEXT    NOT NULL PRIMARY KEY,
    value       TEXT,
    description TEXT,
    updated_at  TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ','now'))
);

CREATE TABLE sources (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    name        TEXT    NOT NULL UNIQUE,
    version_min TEXT,
    version_max TEXT,
    is_active   INTEGER NOT NULL DEFAULT 1
);

CREATE TABLE projects (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    root_path   TEXT    NOT NULL UNIQUE,
    git_remote  TEXT,
    git_owner   TEXT,
    git_repo    TEXT,
    slug        TEXT,
    created_at  TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ','now'))
);
CREATE INDEX idx_projects_slug ON projects(slug);
CREATE INDEX idx_projects_git ON projects(git_owner, git_repo);

CREATE TABLE sessions (
    id                  TEXT    NOT NULL PRIMARY KEY,
    source_id           INTEGER NOT NULL REFERENCES sources(id),
    project_id          INTEGER REFERENCES projects(id),
    title               TEXT,
    started_at          TEXT,
    ended_at            TEXT,
    duration_seconds    INTEGER,
    model               TEXT,
    tool_call_count     INTEGER NOT NULL DEFAULT 0,
    input_tokens        INTEGER NOT NULL DEFAULT 0,
    output_tokens       INTEGER NOT NULL DEFAULT 0,
    cache_read_tokens   INTEGER NOT NULL DEFAULT 0,
    cache_write_tokens  INTEGER NOT NULL DEFAULT 0,
    total_tokens        INTEGER GENERATED ALWAYS AS (input_tokens + output_tokens + cache_read_tokens + cache_write_tokens) STORED,
    estimated_cost_usd  REAL,
    git_branch          TEXT,
    git_sha             TEXT,
    raw_path            TEXT,
    parent_session_id   TEXT    REFERENCES sessions(id),
    error_count         INTEGER NOT NULL DEFAULT 0,
    is_archived         INTEGER NOT NULL DEFAULT 0,
    created_at          TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ','now')),
    updated_at          TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ','now'))
);
CREATE INDEX idx_sessions_source   ON sessions(source_id);
CREATE INDEX idx_sessions_project  ON sessions(project_id);
CREATE INDEX idx_sessions_started  ON sessions(started_at);
CREATE INDEX idx_sessions_parent   ON sessions(parent_session_id);
CREATE INDEX idx_sessions_model    ON sessions(model);
CREATE INDEX idx_sessions_archived ON sessions(is_archived) WHERE is_archived = 0;

CREATE TABLE messages (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    uuid            TEXT    NOT NULL UNIQUE,
    session_id      TEXT    NOT NULL REFERENCES sessions(id),
    role            TEXT    NOT NULL,
    content         TEXT,
    thinking        TEXT,
    parent_uuid     TEXT,
    source_seq      INTEGER,
    turn_number     INTEGER,
    sequence        INTEGER NOT NULL DEFAULT 0,
    input_tokens    INTEGER,
    output_tokens   INTEGER,
    embedding       BLOB,
    created_at      TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ','now'))
);
CREATE INDEX idx_messages_session  ON messages(session_id);
CREATE INDEX idx_messages_role     ON messages(role);
CREATE INDEX idx_messages_turn     ON messages(session_id, turn_number);
CREATE INDEX idx_messages_parent   ON messages(parent_uuid);
CREATE INDEX idx_messages_created  ON messages(created_at);
CREATE INDEX idx_messages_embedded ON messages(embedding) WHERE embedding IS NOT NULL;

CREATE TABLE tool_calls (
    id                  INTEGER PRIMARY KEY AUTOINCREMENT,
    uuid                TEXT    NOT NULL UNIQUE,
    session_id          TEXT    NOT NULL REFERENCES sessions(id),
    request_message_id  INTEGER REFERENCES messages(id),
    response_message_id INTEGER REFERENCES messages(id),
    call_id             TEXT,
    tool_name           TEXT    NOT NULL,
    tool_input          TEXT,
    tool_output         TEXT,
    tool_output_raw     TEXT,
    is_error            INTEGER,
    started_at          TEXT,
    completed_at        TEXT,
    duration_ms         INTEGER,
    created_at          TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ','now'))
);
CREATE INDEX idx_tool_calls_session      ON tool_calls(session_id);
CREATE INDEX idx_tool_calls_request_msg  ON tool_calls(request_message_id);
CREATE INDEX idx_tool_calls_response_msg ON tool_calls(response_message_id);
CREATE INDEX idx_tool_calls_call_id      ON tool_calls(session_id, call_id);
CREATE INDEX idx_tool_calls_tool         ON tool_calls(tool_name);

CREATE VIRTUAL TABLE messages_fts USING fts5(
    content,
    role            UNINDEXED,
    content='messages',
    content_rowid='id',
    tokenize='porter unicode61'
);
CREATE TRIGGER messages_ai AFTER INSERT ON messages BEGIN
    INSERT INTO messages_fts(rowid, content, role) VALUES (new.id, new.content, new.role);
END;
CREATE TRIGGER messages_ad AFTER DELETE ON messages BEGIN
    INSERT INTO messages_fts(messages_fts, rowid, content, role) VALUES ('delete', old.id, old.content, old.role);
END;
CREATE TRIGGER messages_au AFTER UPDATE ON messages BEGIN
    INSERT INTO messages_fts(messages_fts, rowid, content, role) VALUES ('delete', old.id, old.content, old.role);
    INSERT INTO messages_fts(rowid, content, role) VALUES (new.id, new.content, new.role);
END;

CREATE TABLE reports (
    id                  INTEGER PRIMARY KEY AUTOINCREMENT,
    scope               TEXT    NOT NULL,
    period_start        TEXT    NOT NULL,
    period_end          TEXT    NOT NULL,
    generated_at        TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ','now')),
    data_json           TEXT    NOT NULL,
    markdown            TEXT,
    previous_report_id  INTEGER REFERENCES reports(id),
    token_budget_used   INTEGER,
    created_at          TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ','now'))
);
CREATE INDEX idx_reports_scope    ON reports(scope);
CREATE INDEX idx_reports_period   ON reports(period_start, period_end);
CREATE INDEX idx_reports_previous ON reports(previous_report_id);

CREATE TABLE usage_summary (
    id                  INTEGER PRIMARY KEY AUTOINCREMENT,
    date                TEXT    NOT NULL,
    source_id           INTEGER NOT NULL REFERENCES sources(id),
    project_id          INTEGER REFERENCES projects(id),
    session_count       INTEGER NOT NULL DEFAULT 0,
    message_count       INTEGER NOT NULL DEFAULT 0,
    input_tokens        INTEGER NOT NULL DEFAULT 0,
    output_tokens       INTEGER NOT NULL DEFAULT 0,
    cache_read_tokens   INTEGER NOT NULL DEFAULT 0,
    cache_write_tokens  INTEGER NOT NULL DEFAULT 0,
    total_tokens        INTEGER GENERATED ALWAYS AS (input_tokens + output_tokens + cache_read_tokens + cache_write_tokens) STORED,
    estimated_cost_usd  REAL    NOT NULL DEFAULT 0,
    tool_call_count     INTEGER NOT NULL DEFAULT 0,
    error_count         INTEGER NOT NULL DEFAULT 0,
    created_at          TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ','now')),
    UNIQUE(date, source_id, project_id)
);
CREATE INDEX idx_usage_date    ON usage_summary(date);
CREATE INDEX idx_usage_source  ON usage_summary(source_id);
CREATE INDEX idx_usage_project ON usage_summary(project_id);

CREATE TABLE errata (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id  TEXT    REFERENCES sessions(id),
    message_id  INTEGER REFERENCES messages(id),
    source_id   INTEGER NOT NULL REFERENCES sources(id),
    issue_type  TEXT    NOT NULL,
    field_path  TEXT,
    detail      TEXT,
    raw_snippet TEXT,
    created_at  TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ','now'))
);
CREATE INDEX idx_errata_session  ON errata(session_id);
CREATE INDEX idx_errata_source   ON errata(source_id);
CREATE INDEX idx_errata_type     ON errata(issue_type);
