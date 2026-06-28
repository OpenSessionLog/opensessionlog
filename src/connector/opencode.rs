use std::collections::HashSet;
use std::path::{Path, PathBuf};

use rusqlite::Connection;

use crate::connector::Connector;
use crate::error::{OslError, Result};
use crate::ids::{message_id, session_id, tool_call_id};
use crate::model::{Erratum, NormalizedMessage, NormalizedSession, NormalizedToolCall, SessionRef};

pub struct OpenCodeConnector;

fn resolve_db_path(target: &Path) -> Result<PathBuf> {
    if target.is_dir() {
        let candidate = target.join("opencode.db");
        if candidate.exists() {
            Ok(candidate)
        } else {
            // Also check for .db files in the directory
            let mut found: Option<PathBuf> = None;
            if let Ok(entries) = std::fs::read_dir(target) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.is_file() {
                        if let Some(ext) = path.extension() {
                            if ext == "db" || ext == "sqlite" {
                                found = Some(path);
                                break;
                            }
                        }
                    }
                }
            }
            found.ok_or_else(|| {
                OslError::Connector(format!(
                    "no .db file found in directory {}",
                    target.display()
                ))
            })
        }
    } else if target.is_file() {
        Ok(target.to_path_buf())
    } else {
        Err(OslError::Connector(format!(
            "path does not exist: {}",
            target.display()
        )))
    }
}

impl Connector for OpenCodeConnector {
    fn name(&self) -> &'static str {
        "opencode"
    }

    fn discover(&self, target: &Path) -> Result<Vec<SessionRef>> {
        let db_path = resolve_db_path(target)?;
        let conn = Connection::open(&db_path)?;
        conn.execute_batch("PRAGMA busy_timeout=5000;")?;
        let mut stmt = conn.prepare("SELECT id FROM session ORDER BY time_created")?;
        let refs = stmt
            .query_map([], |row| {
                let native_id: String = row.get(0)?;
                Ok(SessionRef {
                    source: "opencode".to_string(),
                    native_id,
                    path: db_path.clone(),
                    project_path: None,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        if refs.is_empty() {
            return Err(OslError::Connector(format!(
                "no sessions found in {}",
                db_path.display()
            )));
        }

        Ok(refs)
    }

    fn discover_filtered(
        &self,
        target: &std::path::Path,
        filter: &crate::recency::RecencyFilter,
    ) -> Result<Vec<SessionRef>> {
        if !filter.is_active() {
            return self.discover(target);
        }
        let db_path = resolve_db_path(target)?;
        let conn = Connection::open(&db_path)?;
        conn.execute_batch("PRAGMA busy_timeout=5000;")?;
        // OpenCode session.time_created is INTEGER Unix MILLISECONDS.
        let mut stmt =
            conn.prepare("SELECT id FROM session WHERE time_created >= ?1 ORDER BY time_created")?;
        let cutoff_ms: i64 = filter.since_unix().unwrap() * 1000;
        let refs = stmt
            .query_map([cutoff_ms], |row| {
                let native_id: String = row.get(0)?;
                Ok(SessionRef {
                    source: "opencode".to_string(),
                    native_id,
                    path: db_path.clone(),
                    project_path: None,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        if refs.is_empty() {
            return Err(OslError::Connector(format!(
                "no sessions found in {} matching the recency filter",
                db_path.display()
            )));
        }
        Ok(refs)
    }

    fn parse(&self, session_ref: &SessionRef) -> Result<NormalizedSession> {
        let conn = Connection::open(&session_ref.path)?;
        conn.execute_batch("PRAGMA busy_timeout=5000;")?;
        parse_session(&conn, session_ref)
    }
}

pub(crate) fn ms_to_iso(ms: i64) -> String {
    // OpenCode timestamps are Unix milliseconds since the epoch (post-epoch).
    debug_assert!(ms >= 0, "negative milliseconds are not supported");
    let secs = ms / 1000;
    let millis = ms % 1000;
    // Simple ISO 8601 formatting from Unix seconds (UTC, hence trailing Z).
    let naive = chrono_from_unix(secs);
    format!("{naive}.{millis:03}Z")
}

/// Build an ISO 8601 date-time string from Unix seconds (UTC).
/// Avoids pulling in chrono — we only need this one formatting operation.
pub(crate) fn chrono_from_unix(secs: i64) -> String {
    // Days from Unix epoch (1970-01-01) using civil date arithmetic.
    let mut remaining = secs;

    // Compute date
    let (year, month, day) = {
        let z = (remaining / 86400) + 719468; // days since 0000-03-01
        let era = if z >= 0 { z } else { z - 146096 } / 146097;
        let doe = z - era * 146097; // day of era
        let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
        let y = yoe + era * 400;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let d = doy - (153 * mp + 2) / 5 + 1;
        let m = if mp < 10 { mp + 3 } else { mp - 9 };
        let y = if m <= 2 { y + 1 } else { y };
        (y, m, d)
    };

    // Compute time
    remaining %= 86400;
    let hours = remaining / 3600;
    remaining %= 3600;
    let minutes = remaining / 60;
    let seconds = remaining % 60;

    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}",
        year, month, day, hours, minutes, seconds
    )
}

/// Extract the model ID from the session `model` JSON column.
/// Format: `{"id":"big-pickle","providerID":"opencode","variant":"default"}`
fn extract_model_id(
    model_json: Option<&str>,
) -> std::result::Result<Option<String>, serde_json::Error> {
    let Some(json) = model_json else {
        return Ok(None);
    };
    let v: serde_json::Value = serde_json::from_str(json)?;
    Ok(v.get("id").and_then(|v| v.as_str()).map(String::from))
}

/// Row type for the session table query in `parse_session`.
type SessionRow = (
    Option<String>, // title
    i64,            // time_created
    i64,            // time_updated
    Option<String>, // model (JSON)
    Option<String>, // agent
    Option<String>, // directory
    i64,            // tokens_input
    i64,            // tokens_output
    i64,            // tokens_reasoning
    i64,            // tokens_cache_read
    i64,            // tokens_cache_write
    Option<f64>,    // cost
);

/// Parse a session from the OpenCode SQLite database.
fn parse_session(conn: &Connection, session_ref: &SessionRef) -> Result<NormalizedSession> {
    let native_id = &session_ref.native_id;

    // ── 1. Read session row ──────────────────────────────────────────────
    let (
        title,
        time_created,
        time_updated,
        model_json,
        _agent,
        directory,
        tokens_input,
        tokens_output,
        _tokens_reasoning,
        tokens_cache_read,
        tokens_cache_write,
        _cost,
    ): SessionRow = conn
        .query_row(
            "SELECT title, time_created, time_updated, model, agent, directory,
                    tokens_input, tokens_output, tokens_reasoning,
                    tokens_cache_read, tokens_cache_write, cost
             FROM session WHERE id = ?1",
            [native_id],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                    row.get(7)?,
                    row.get(8)?,
                    row.get(9)?,
                    row.get(10)?,
                    row.get(11)?,
                ))
            },
        )
        .map_err(|e| {
            OslError::Connector(format!(
                "session {native_id} not found in {}: {e}",
                session_ref.path.display()
            ))
        })?;

    let mut errata: Vec<Erratum> = Vec::new();

    let model = match extract_model_id(model_json.as_deref()) {
        Ok(m) => m,
        Err(e) => {
            errata.push(Erratum {
                issue_type: "parse_error".to_string(),
                field_path: Some("session.model".to_string()),
                detail: format!("failed to parse session model JSON: {e}"),
                raw_snippet: model_json.as_deref().map(|s| s.chars().take(500).collect()),
            });
            None
        }
    };
    let started_at = Some(ms_to_iso(time_created));
    let ended_at = Some(ms_to_iso(time_updated));
    let project_root = directory.map(PathBuf::from);
    let sid = session_id("opencode", native_id);

    // ── 2. Read messages ──────────────────────────────────────────────────
    let mut messages: Vec<NormalizedMessage> = Vec::new();
    let mut tool_calls: Vec<NormalizedToolCall> = Vec::new();

    let mut msg_stmt = conn.prepare(
        "SELECT id, data, time_created FROM message WHERE session_id = ?1 ORDER BY time_created",
    )?;
    let msg_rows = msg_stmt.query_map([native_id], |row| {
        let id: String = row.get(0)?;
        let data: String = row.get(1)?;
        let time_created: i64 = row.get(2)?;
        Ok((id, data, time_created))
    })?;

    let mut turn_number: i64 = 0;

    // Prepare once and reuse for every message's parts.
    let mut part_stmt =
        conn.prepare("SELECT id, data FROM part WHERE message_id = ?1 ORDER BY time_created")?;

    for (source_seq, msg_result) in msg_rows.enumerate() {
        let source_seq = (source_seq + 1) as i64;
        let (msg_id, msg_data, _msg_time) = msg_result?;

        let msg_json: serde_json::Value = match serde_json::from_str(&msg_data) {
            Ok(v) => v,
            Err(e) => {
                errata.push(Erratum {
                    issue_type: "parse_error".to_string(),
                    field_path: Some(format!("message:{msg_id}")),
                    detail: format!("failed to parse message data JSON: {e}"),
                    raw_snippet: Some(msg_data.chars().take(500).collect()),
                });
                continue;
            }
        };

        let role = msg_json
            .get("role")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string();

        // System events (agent-switched, model-switched) are in session_message,
        // not message. Skip messages without a recognized role.
        if role != "user" && role != "assistant" {
            continue;
        }

        turn_number += 1;
        let msg_uuid = message_id(sid, &msg_id);

        // Guard against duplicate callIDs within the same message. callIDs may repeat
        // across different assistant messages in a session, so this set is per-message.
        let mut seen_call_ids: HashSet<String> = HashSet::new();

        // Extract token info from assistant messages
        let (input_tokens, output_tokens) = if role == "assistant" {
            let tokens = msg_json.get("tokens");
            (
                tokens.and_then(|t| t.get("input")).and_then(|v| v.as_i64()),
                tokens
                    .and_then(|t| t.get("output"))
                    .and_then(|v| v.as_i64()),
            )
        } else {
            (None, None)
        };

        // Read parts for this message
        let part_rows = part_stmt.query_map([&msg_id], |row| {
            let id: String = row.get(0)?;
            let data: String = row.get(1)?;
            Ok((id, data))
        })?;

        let mut text_parts: Vec<String> = Vec::new();
        let mut thinking_parts: Vec<String> = Vec::new();

        for part_result in part_rows {
            let (_part_id, part_data) = part_result?;

            let part_json: serde_json::Value = match serde_json::from_str(&part_data) {
                Ok(v) => v,
                Err(e) => {
                    errata.push(Erratum {
                        issue_type: "parse_error".to_string(),
                        field_path: Some(format!("part:{msg_id}")),
                        detail: format!("failed to parse part JSON: {e}"),
                        raw_snippet: Some(part_data.chars().take(500).collect()),
                    });
                    continue;
                }
            };

            let part_type = part_json
                .get("type")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");

            match part_type {
                "text" => {
                    if let Some(text) = part_json.get("text").and_then(|v| v.as_str()) {
                        text_parts.push(text.to_string());
                    }
                }
                "reasoning" => {
                    if let Some(text) = part_json.get("text").and_then(|v| v.as_str()) {
                        thinking_parts.push(text.to_string());
                    }
                }
                "tool" => {
                    // Real OpenCode stores tool requests and their results inside the
                    // same assistant-message part, so we emit each tool call immediately.
                    let tool_name = part_json
                        .get("tool")
                        .and_then(|v| v.as_str())
                        .unwrap_or("unknown")
                        .to_string();
                    let call_id = part_json
                        .get("callID")
                        .and_then(|v| v.as_str())
                        .unwrap_or("unknown")
                        .to_string();

                    // Deduplicate repeated (message_id, call_id) pairs within one message.
                    if !seen_call_ids.insert(call_id.clone()) {
                        errata.push(Erratum {
                            issue_type: "duplicate_tool_call".to_string(),
                            field_path: Some(format!("message:{msg_id}:callID:{call_id}")),
                            detail: format!(
                                "duplicate tool callID '{call_id}' within message {msg_id}"
                            ),
                            raw_snippet: None,
                        });
                        continue;
                    }

                    let state = part_json.get("state");
                    let status = state.and_then(|s| s.get("status")).and_then(|v| v.as_str());

                    // Terminal statuses give a concrete boolean; non-terminal statuses
                    // leave is_error NULL rather than implying success.
                    let is_error = match status {
                        Some("error") => Some(true),
                        Some("completed") => Some(false),
                        _ => None,
                    };
                    if status == Some("running") {
                        errata.push(Erratum {
                            issue_type: "incomplete_tool_call".to_string(),
                            field_path: Some(format!("part:{msg_id}:{call_id}")),
                            detail: format!(
                                "tool call '{call_id}' in message {msg_id} has non-terminal status 'running'"
                            ),
                            raw_snippet: None,
                        });
                    }

                    let tool_input = state.and_then(|s| s.get("input")).map(|v| v.to_string());
                    let tool_output = state
                        .and_then(|s| s.get("output"))
                        .and_then(|v| v.as_str())
                        .map(String::from);

                    let time = state.and_then(|s| s.get("time"));
                    let started_at = time
                        .and_then(|t| t.get("start"))
                        .and_then(|v| v.as_i64())
                        .map(ms_to_iso);
                    let completed_at = time
                        .and_then(|t| t.get("end"))
                        .and_then(|v| v.as_i64())
                        .map(ms_to_iso);

                    let tc_uuid = tool_call_id(sid, msg_uuid, &call_id);

                    tool_calls.push(NormalizedToolCall {
                        uuid: tc_uuid,
                        call_id,
                        tool_name,
                        tool_input,
                        tool_output,
                        tool_output_raw: None,
                        is_error,
                        started_at,
                        completed_at,
                        request_message_uuid: msg_uuid,
                        response_message_uuid: Some(msg_uuid),
                    });
                }
                "file" | "step-start" | "step-finish" | "compaction" => {
                    // Skip: file content, step markers, compaction events
                }
                other => {
                    errata.push(Erratum {
                        issue_type: "unknown_part_type".to_string(),
                        field_path: Some(format!("part_type:{other}")),
                        detail: format!("unhandled part type '{other}' in message {msg_id}"),
                        raw_snippet: None,
                    });
                }
            }
        }

        // Determine parent: user messages may reference parentID in their data
        let parent_uuid = if role == "assistant" {
            msg_json
                .get("parentID")
                .and_then(|v| v.as_str())
                .map(|pid| message_id(sid, pid).to_string())
        } else {
            None
        };

        messages.push(NormalizedMessage {
            uuid: msg_uuid,
            role,
            content: if text_parts.is_empty() {
                None
            } else {
                Some(text_parts.join("\n"))
            },
            thinking: if thinking_parts.is_empty() {
                None
            } else {
                Some(thinking_parts.join("\n"))
            },
            parent_uuid,
            source_seq,
            turn_number,
            sequence: 0,
            input_tokens,
            output_tokens,
        });
    }

    if messages.is_empty() {
        errata.push(Erratum {
            issue_type: "empty_session".to_string(),
            field_path: Some(format!("session:{native_id}")),
            detail: "session produced zero normalized messages".to_string(),
            raw_snippet: None,
        });
    }

    let tool_call_count = tool_calls.len() as i64;
    let error_count = errata.len() as i64;

    Ok(NormalizedSession {
        id: sid,
        source: session_ref.source.clone(),
        native_id: native_id.clone(),
        title,
        started_at,
        ended_at,
        model,
        // Phase-1 gaps: OpenCode stores enough to derive these, but we keep them
        // None until we decide how to extract/persist raw output, branch, and sha.
        git_branch: None,
        git_sha: None,
        raw_path: session_ref.path.clone(),
        project_root,
        parent_session_id: None,
        input_tokens: tokens_input,
        output_tokens: tokens_output,
        cache_read_tokens: tokens_cache_read,
        cache_write_tokens: tokens_cache_write,
        tool_call_count,
        error_count,
        messages,
        tool_calls,
        errata,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// Build an in-memory SQLite database with the same schema as opencode.db.
    /// Populated with minimal synthetic session data for testing.
    fn build_test_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        build_test_db_schema(&conn);
        conn
    }

    fn build_test_db_schema(conn: &Connection) {
        conn.execute_batch(
            "
            CREATE TABLE session (
                id TEXT PRIMARY KEY,
                project_id TEXT,
                parent_id TEXT,
                slug TEXT,
                directory TEXT,
                title TEXT,
                version TEXT,
                share_url TEXT,
                summary_additions INTEGER,
                summary_deletions INTEGER,
                summary_files INTEGER,
                summary_diffs TEXT,
                revert TEXT,
                permission TEXT,
                time_created INTEGER,
                time_updated INTEGER,
                time_compacting INTEGER,
                time_archived INTEGER,
                workspace_id TEXT,
                path TEXT,
                agent TEXT,
                model TEXT,
                cost REAL,
                tokens_input INTEGER,
                tokens_output INTEGER,
                tokens_reasoning INTEGER,
                tokens_cache_read INTEGER,
                tokens_cache_write INTEGER,
                metadata TEXT
            );
            CREATE TABLE message (
                id TEXT PRIMARY KEY,
                session_id TEXT,
                time_created INTEGER,
                time_updated INTEGER,
                data TEXT
            );
            CREATE TABLE part (
                id TEXT PRIMARY KEY,
                message_id TEXT,
                session_id TEXT,
                time_created INTEGER,
                time_updated INTEGER,
                data TEXT
            );
            CREATE TABLE project (
                id TEXT PRIMARY KEY,
                worktree TEXT,
                vcs TEXT,
                name TEXT,
                icon_url TEXT,
                icon_color TEXT,
                time_created INTEGER,
                time_updated INTEGER,
                time_initialized INTEGER,
                sandboxes TEXT,
                commands TEXT,
                icon_url_override TEXT
            );
            CREATE TABLE session_message (
                id TEXT PRIMARY KEY,
                session_id TEXT,
                type TEXT,
                time_created INTEGER,
                time_updated INTEGER,
                data TEXT,
                seq INTEGER
            );
            ",
        )
        .unwrap();
    }

    /// Insert a minimal session with 2 messages (user + assistant) and no tool calls.
    fn insert_minimal_session(conn: &Connection, sid: &str) {
        let now_ms = 1780251875000i64;

        conn.execute(
            "INSERT INTO session (id, title, directory, time_created, time_updated,
             tokens_input, tokens_output, tokens_reasoning, tokens_cache_read, tokens_cache_write,
             cost, model, agent, version)
             VALUES (?1, 'Test session', '/tmp', ?2, ?3, 100, 50, 10, 0, 0, 0.0,
             '{\"id\":\"big-pickle\",\"providerID\":\"opencode\"}', 'build', 'local')",
            rusqlite::params![sid, now_ms, now_ms + 30000],
        )
        .unwrap();

        // User message
        let msg1_id = format!("msg_{}_1", sid);
        conn.execute(
            "INSERT INTO message (id, session_id, time_created, time_updated, data)
             VALUES (?1, ?2, ?3, ?3, ?4)",
            rusqlite::params![
                msg1_id,
                sid,
                now_ms + 1000,
                r#"{"role":"user","time":{"created":1780251876000},"agent":"build","model":{"providerID":"opencode","modelID":"big-pickle"}}"#
            ],
        )
        .unwrap();

        conn.execute(
            "INSERT INTO part (id, message_id, session_id, time_created, time_updated, data)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![
                format!("prt_{}_1", sid),
                msg1_id,
                sid,
                now_ms + 1000,
                now_ms + 1000,
                r#"{"type":"text","text":"Hello, can you help?"}"#
            ],
        )
        .unwrap();

        // Assistant message
        let msg2_id = format!("msg_{}_2", sid);
        conn.execute(
            "INSERT INTO message (id, session_id, time_created, time_updated, data)
             VALUES (?1, ?2, ?3, ?3, ?4)",
            rusqlite::params![
                msg2_id,
                sid,
                now_ms + 5000,
                format!(
                    r#"{{"parentID":"{}","role":"assistant","mode":"build","agent":"build","cost":0,"tokens":{{"total":150,"input":100,"output":50,"reasoning":10,"cache":{{"write":0,"read":0}}}},"modelID":"big-pickle","providerID":"opencode","time":{{"created":{},"completed":{}}},"finish":"stop"}}"#,
                    msg1_id, now_ms + 5000, now_ms + 8000
                )
            ],
        )
        .unwrap();

        conn.execute(
            "INSERT INTO part (id, message_id, session_id, time_created, time_updated, data)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![
                format!("prt_{}_2", sid),
                msg2_id,
                sid,
                now_ms + 5000,
                now_ms + 5000,
                r#"{"type":"text","text":"Sure, I can help! What do you need?"}"#
            ],
        )
        .unwrap();
    }

    /// Insert a session with tool calls.
    fn insert_tool_call_session(conn: &Connection, sid: &str) {
        let now_ms = 1780251900000i64;

        conn.execute(
            "INSERT INTO session (id, title, directory, time_created, time_updated,
             tokens_input, tokens_output, tokens_reasoning, tokens_cache_read, tokens_cache_write,
             cost, model, agent, version)
             VALUES (?1, 'Tool session', '/tmp', ?2, ?3, 200, 100, 20, 0, 0, 0.0,
             '{\"id\":\"deepseek-v4-flash-free\",\"providerID\":\"opencode\"}', 'build', 'local')",
            rusqlite::params![sid, now_ms, now_ms + 60000],
        )
        .unwrap();

        // User message
        let msg1_id = format!("msg_{}_1", sid);
        conn.execute(
            "INSERT INTO message (id, session_id, time_created, time_updated, data)
             VALUES (?1, ?2, ?3, ?3, ?4)",
            rusqlite::params![
                msg1_id,
                sid,
                now_ms + 1000,
                r#"{"role":"user","time":{"created":1780251901000},"agent":"build"}"#
            ],
        )
        .unwrap();

        conn.execute(
            "INSERT INTO part (id, message_id, session_id, time_created, time_updated, data)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![
                format!("prt_{}_1", sid),
                msg1_id,
                sid,
                now_ms + 1000,
                now_ms + 1000,
                r#"{"type":"text","text":"Run the tests"}"#
            ],
        )
        .unwrap();

        // Assistant message with reasoning + tool call + text
        let msg2_id = format!("msg_{}_2", sid);
        conn.execute(
            "INSERT INTO message (id, session_id, time_created, time_updated, data)
             VALUES (?1, ?2, ?3, ?3, ?4)",
            rusqlite::params![
                msg2_id,
                sid,
                now_ms + 5000,
                format!(
                    r#"{{"parentID":"{}","role":"assistant","mode":"build","agent":"build","cost":0,"tokens":{{"total":300,"input":200,"output":100,"reasoning":20,"cache":{{"write":0,"read":0}}}},"modelID":"deepseek-v4-flash-free","providerID":"opencode","time":{{"created":{},"completed":{}}},"finish":"stop"}}"#,
                    msg1_id, now_ms + 5000, now_ms + 55000
                )
            ],
        )
        .unwrap();

        conn.execute(
            "INSERT INTO part (id, message_id, session_id, time_created, time_updated, data)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![
                format!("prt_{}_2a", sid),
                msg2_id,
                sid,
                now_ms + 5000,
                now_ms + 5000,
                r#"{"type":"reasoning","text":"The user wants to run tests. Let me execute the test command.","time":{"start":1780251905000,"end":1780251907000}}"#
            ],
        )
        .unwrap();

        conn.execute(
            "INSERT INTO part (id, message_id, session_id, time_created, time_updated, data)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![
                format!("prt_{}_2b", sid),
                msg2_id,
                sid,
                now_ms + 6000,
                now_ms + 6000,
                format!(
                    r#"{{"type":"tool","tool":"bash","callID":"call_00_test","state":{{"status":"completed","input":{{"command":"npm test","description":"Run tests","timeout":120000}},"output":"PASS all tests","metadata":{{"exit":0,"truncated":false}},"title":"Run tests","time":{{"start":{},"end":{}}}}}}}"#,
                    now_ms + 7000, now_ms + 50000
                )
            ],
        )
        .unwrap();

        conn.execute(
            "INSERT INTO part (id, message_id, session_id, time_created, time_updated, data)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![
                format!("prt_{}_2c", sid),
                msg2_id,
                sid,
                now_ms + 51000,
                now_ms + 51000,
                r#"{"type":"text","text":"Tests passed successfully!"}"#
            ],
        )
        .unwrap();

        // User message (tool result visible to user)
        let msg3_id = format!("msg_{}_3", sid);
        conn.execute(
            "INSERT INTO message (id, session_id, time_created, time_updated, data)
             VALUES (?1, ?2, ?3, ?3, ?4)",
            rusqlite::params![
                msg3_id,
                sid,
                now_ms + 55000,
                r#"{"role":"user","time":{"created":1780251955000},"agent":"build","summary":{"diffs":[]}}"#
            ],
        )
        .unwrap();
    }

    #[test]
    fn discover_filtered_recency_30_keeps_only_recent_sessions() {
        let tmp = tempfile::TempDir::new().unwrap();
        let db_path = tmp.path().join("opencode.db");
        let conn = Connection::open(&db_path).unwrap();
        build_test_db_schema(&conn);

        let now = crate::recency::now_unix_seconds();
        let old_ms = (now - 60 * 86400) * 1000;
        let recent_ms = now * 1000;

        conn.execute(
            "INSERT INTO session (id, title, directory, time_created, time_updated,
             tokens_input, tokens_output, tokens_reasoning, tokens_cache_read, tokens_cache_write,
             cost, model, agent, version)
             VALUES (?1, 'Old', '/tmp', ?2, ?3, 0, 0, 0, 0, 0, 0.0,
             '{\"id\":\"x\"}', 'build', 'local')",
            rusqlite::params!["old_session", old_ms, old_ms + 60000],
        )
        .unwrap();

        conn.execute(
            "INSERT INTO session (id, title, directory, time_created, time_updated,
             tokens_input, tokens_output, tokens_reasoning, tokens_cache_read, tokens_cache_write,
             cost, model, agent, version)
             VALUES (?1, 'Recent', '/tmp', ?2, ?3, 0, 0, 0, 0, 0, 0.0,
             '{\"id\":\"x\"}', 'build', 'local')",
            rusqlite::params!["recent_session", recent_ms, recent_ms + 60000],
        )
        .unwrap();

        drop(conn);

        let connector = OpenCodeConnector;
        let filter = crate::recency::RecencyFilter::from_flags(Some(30), None, now).unwrap();
        let refs = connector.discover_filtered(&db_path, &filter).unwrap();
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].native_id, "recent_session");
    }

    #[test]
    fn discover_filtered_none_returns_all_sessions() {
        let tmp = tempfile::TempDir::new().unwrap();
        let db_path = tmp.path().join("opencode.db");
        let conn = Connection::open(&db_path).unwrap();
        build_test_db_schema(&conn);

        let now_ms = 1780251900000i64;
        for sid in ["session_a", "session_b"] {
            conn.execute(
                "INSERT INTO session (id, title, directory, time_created, time_updated,
                 tokens_input, tokens_output, tokens_reasoning, tokens_cache_read, tokens_cache_write,
                 cost, model, agent, version)
                 VALUES (?1, 'Test', '/tmp', ?2, ?3, 0, 0, 0, 0, 0, 0.0,
                 '{\"id\":\"x\"}', 'build', 'local')",
                rusqlite::params![sid, now_ms, now_ms + 60000],
            ).unwrap();
        }
        drop(conn);

        let connector = OpenCodeConnector;
        let refs = connector
            .discover_filtered(&db_path, &crate::recency::RecencyFilter::none())
            .unwrap();
        assert_eq!(refs.len(), 2);
    }

    #[test]
    fn minimal_session_has_two_messages() {
        let conn = build_test_db();
        let sid = "ses_test_minimal";
        insert_minimal_session(&conn, sid);

        let session_ref = SessionRef {
            source: "opencode".to_string(),
            native_id: sid.to_string(),
            path: PathBuf::from("/tmp/test_opencode.db"),
            project_path: None,
        };

        let session = parse_session(&conn, &session_ref).unwrap();
        assert_eq!(session.native_id, sid);
        assert_eq!(session.title.as_deref(), Some("Test session"));
        assert_eq!(session.messages.len(), 2);
        assert_eq!(session.messages[0].role, "user");
        assert_eq!(session.messages[1].role, "assistant");
        assert_eq!(
            session.messages[0].content.as_deref(),
            Some("Hello, can you help?")
        );
        assert_eq!(
            session.messages[1].content.as_deref(),
            Some("Sure, I can help! What do you need?")
        );
        assert!(session.tool_calls.is_empty());
        assert_eq!(session.input_tokens, 100);
        assert_eq!(session.output_tokens, 50);
        assert_eq!(session.model.as_deref(), Some("big-pickle"));
    }

    #[test]
    fn session_with_tool_calls() {
        let conn = build_test_db();
        let sid = "ses_test_tools";
        insert_tool_call_session(&conn, sid);

        let session_ref = SessionRef {
            source: "opencode".to_string(),
            native_id: sid.to_string(),
            path: PathBuf::from("/tmp/test_opencode.db"),
            project_path: None,
        };

        let session = parse_session(&conn, &session_ref).unwrap();
        assert_eq!(session.messages.len(), 3);
        assert_eq!(session.tool_calls.len(), 1);

        let tc = &session.tool_calls[0];
        assert_eq!(tc.tool_name, "bash");
        assert_eq!(tc.call_id, "call_00_test");
        assert!(tc.tool_input.as_deref().unwrap_or("").contains("npm test"));
        assert_eq!(tc.tool_output.as_deref(), Some("PASS all tests"));
        assert_eq!(tc.is_error, Some(false));

        // Check reasoning
        let assistant = &session.messages[1];
        assert_eq!(assistant.role, "assistant");
        assert!(assistant
            .thinking
            .as_deref()
            .unwrap_or("")
            .contains("run tests"));
        assert_eq!(
            assistant.content.as_deref(),
            Some("Tests passed successfully!")
        );

        // Check model
        assert_eq!(session.model.as_deref(), Some("deepseek-v4-flash-free"));
    }

    #[test]
    fn repeated_call_id_across_messages_emits_both() {
        let conn = build_test_db();
        let sid = "ses_repeated_callid";
        let now_ms = 1780252000000i64;

        conn.execute(
            "INSERT INTO session (id, title, directory, time_created, time_updated,
             tokens_input, tokens_output, tokens_reasoning, tokens_cache_read, tokens_cache_write,
             cost, model, agent, version)
             VALUES (?1, 'Repeated callID', '/tmp', ?2, ?3, 0, 0, 0, 0, 0, 0.0,
             '{\"id\":\"test-model\"}', 'build', 'local')",
            rusqlite::params![sid, now_ms, now_ms + 60000],
        )
        .unwrap();

        // Two user messages (turns) each followed by an assistant message containing
        // a tool part with the same callID. This mirrors real OpenCode data.
        for i in 0..2 {
            let user_id = format!("msg_{}_user_{}", sid, i);
            let assistant_id = format!("msg_{}_assistant_{}", sid, i);
            let base_ms = now_ms + i * 10000;

            conn.execute(
                "INSERT INTO message (id, session_id, time_created, time_updated, data)
                 VALUES (?1, ?2, ?3, ?3, ?4)",
                rusqlite::params![
                    user_id,
                    sid,
                    base_ms + 1000,
                    r#"{"role":"user","time":{"created":0},"agent":"build"}"#
                ],
            )
            .unwrap();

            conn.execute(
                "INSERT INTO message (id, session_id, time_created, time_updated, data)
                 VALUES (?1, ?2, ?3, ?3, ?4)",
                rusqlite::params![
                    assistant_id.clone(),
                    sid,
                    base_ms + 5000,
                    format!(
                        r#"{{"role":"assistant","time":{{"created":{}}},"agent":"build"}}"#,
                        base_ms + 5000
                    )
                ],
            )
            .unwrap();

            conn.execute(
                "INSERT INTO part (id, message_id, session_id, time_created, time_updated, data)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                rusqlite::params![
                    format!("prt_{}_{}", sid, i),
                    assistant_id,
                    sid,
                    base_ms + 6000,
                    base_ms + 6000,
                    format!(
                        r#"{{"type":"tool","tool":"bash","callID":"shared_call_id","state":{{"status":"completed","input":{{"command":"echo {}"}},"output":"out{}","time":{{"start":{},"end":{}}}}}}}"#,
                        i, i, base_ms + 7000, base_ms + 8000
                    )
                ],
            )
            .unwrap();
        }

        let session_ref = SessionRef {
            source: "opencode".to_string(),
            native_id: sid.to_string(),
            path: PathBuf::from("/tmp/test_opencode.db"),
            project_path: None,
        };

        let session = parse_session(&conn, &session_ref).unwrap();
        assert_eq!(session.tool_calls.len(), 2);
        assert_ne!(session.tool_calls[0].uuid, session.tool_calls[1].uuid);
        assert_eq!(session.tool_calls[0].call_id, "shared_call_id");
        assert_eq!(session.tool_calls[1].call_id, "shared_call_id");
        assert_eq!(
            session.tool_calls[0].request_message_uuid,
            session.tool_calls[0].response_message_uuid.unwrap()
        );
    }

    #[test]
    fn session_discovery_returns_all_sessions() {
        let conn = build_test_db();
        insert_minimal_session(&conn, "ses_disc_1");
        insert_tool_call_session(&conn, "ses_disc_2");

        // We can't test discover() directly since it opens a file, but we can
        // verify that both sessions are parseable.
        let session_ref_1 = SessionRef {
            source: "opencode".to_string(),
            native_id: "ses_disc_1".to_string(),
            path: PathBuf::from("/tmp/test_opencode.db"),
            project_path: None,
        };
        let session_ref_2 = SessionRef {
            source: "opencode".to_string(),
            native_id: "ses_disc_2".to_string(),
            path: PathBuf::from("/tmp/test_opencode.db"),
            project_path: None,
        };

        let s1 = parse_session(&conn, &session_ref_1).unwrap();
        let s2 = parse_session(&conn, &session_ref_2).unwrap();

        assert_eq!(s1.messages.len(), 2);
        assert_eq!(s2.messages.len(), 3);
        assert_eq!(s1.model.as_deref(), Some("big-pickle"));
        assert_eq!(s2.model.as_deref(), Some("deepseek-v4-flash-free"));
    }

    #[test]
    fn session_ids_are_deterministic() {
        let sid = "ses_deterministic";
        let conn = build_test_db();
        insert_minimal_session(&conn, sid);

        let session_ref = SessionRef {
            source: "opencode".to_string(),
            native_id: sid.to_string(),
            path: PathBuf::from("/tmp/test_opencode.db"),
            project_path: None,
        };

        let s1 = parse_session(&conn, &session_ref).unwrap();
        let s2 = parse_session(&conn, &session_ref).unwrap();

        assert_eq!(s1.id, s2.id);
        for (m1, m2) in s1.messages.iter().zip(s2.messages.iter()) {
            assert_eq!(m1.uuid, m2.uuid);
        }
    }

    #[test]
    fn ms_conversion_is_correct() {
        // 1780251875000 ms = 2026-06-01T... (approximately)
        let iso = ms_to_iso(1780251875000);
        assert!(iso.starts_with("2026-"));
        assert!(iso.ends_with("Z"));
        assert!(iso.contains('T'));

        // Unix epoch
        assert_eq!(ms_to_iso(0), "1970-01-01T00:00:00.000Z");
    }

    #[test]
    fn model_id_extraction() {
        assert_eq!(
            extract_model_id(Some(r#"{"id":"big-pickle"}"#)).unwrap(),
            Some("big-pickle".to_string())
        );
        assert_eq!(
            extract_model_id(Some(
                r#"{"id":"deepseek-v4-flash-free","providerID":"opencode"}"#
            ))
            .unwrap(),
            Some("deepseek-v4-flash-free".to_string())
        );
        assert_eq!(extract_model_id(None).unwrap(), None);
        assert_eq!(extract_model_id(Some("{}")).unwrap(), None);
        assert!(extract_model_id(Some("not json")).is_err());
    }

    #[test]
    fn empty_session_returns_error() {
        let conn = build_test_db();
        // No session inserted — parse should fail
        let session_ref = SessionRef {
            source: "opencode".to_string(),
            native_id: "ses_nonexistent".to_string(),
            path: PathBuf::from("/tmp/test_opencode.db"),
            project_path: None,
        };

        let result = parse_session(&conn, &session_ref);
        assert!(result.is_err());
    }
}
