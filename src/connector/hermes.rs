use std::path::{Path, PathBuf};

use rusqlite::Connection;

use crate::connector::SqliteSessionParser;
use crate::error::{OslError, Result};
use crate::ids::{message_id, session_id, tool_call_id};
use crate::model::{Erratum, NormalizedMessage, NormalizedSession, NormalizedToolCall, SessionRef};
use crate::recency::RecencyFilter;

pub struct HermesSessionParser;

impl SqliteSessionParser for HermesSessionParser {
    fn source_name(&self) -> &'static str {
        "hermes"
    }

    fn db_filename(&self) -> &'static str {
        "state.db"
    }

    fn discover_sessions(
        &self,
        conn: &Connection,
        db_path: &Path,
        filter: Option<&RecencyFilter>,
    ) -> Result<Vec<SessionRef>> {
        let (refs, filtered) = match filter {
            None => {
                let mut stmt = conn.prepare(
                    "SELECT id, source FROM sessions WHERE archived = 0 ORDER BY started_at",
                )?;
                let refs: Vec<SessionRef> = stmt
                    .query_map([], |row| {
                        let sid: String = row.get(0)?;
                        let source: String = row.get(1)?;
                        let native_id = format!("{}:{}", source, sid);
                        Ok(SessionRef {
                            source: "hermes".to_string(),
                            native_id,
                            path: db_path.to_path_buf(),
                            project_path: None,
                        })
                    })?
                    .collect::<std::result::Result<Vec<_>, _>>()?;
                (refs, false)
            }
            Some(f) => {
                // Hermes sessions.started_at is REAL Unix seconds.
                let mut stmt = conn.prepare(
                    "SELECT id, source FROM sessions
                     WHERE archived = 0 AND started_at >= ?1
                     ORDER BY started_at",
                )?;
                let cutoff: f64 = f.since_unix().unwrap() as f64;
                let refs: Vec<SessionRef> = stmt
                    .query_map([cutoff], |row| {
                        let sid: String = row.get(0)?;
                        let source: String = row.get(1)?;
                        let native_id = format!("{}:{}", source, sid);
                        Ok(SessionRef {
                            source: "hermes".to_string(),
                            native_id,
                            path: db_path.to_path_buf(),
                            project_path: None,
                        })
                    })?
                    .collect::<std::result::Result<Vec<_>, _>>()?;
                (refs, true)
            }
        };

        if refs.is_empty() {
            return Err(OslError::Connector(format!(
                "no sessions found in {}{}",
                db_path.display(),
                if filtered {
                    " matching the recency filter"
                } else {
                    ""
                }
            )));
        }

        Ok(refs)
    }

    fn parse_session(
        &self,
        conn: &Connection,
        session_ref: &SessionRef,
    ) -> Result<NormalizedSession> {
        // Delegate to the existing free function (unchanged).
        parse_session(conn, session_ref)
    }
}

fn seconds_to_iso(secs: f64) -> String {
    debug_assert!(secs >= 0.0, "negative timestamps are not supported");
    // Convert to total milliseconds, then split — handles rounding edge cases
    // (e.g. 0.9996s → 1000ms → rolls over to next second correctly).
    let total_millis = (secs * 1000.0).round() as i64;
    let whole_secs = total_millis / 1000;
    let millis = total_millis % 1000;
    let naive = crate::connector::opencode::chrono_from_unix(whole_secs);
    format!("{naive}.{millis:03}Z")
}

fn infer_is_error(content: &str) -> Option<bool> {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(content) else {
        return None;
    };
    // Check for explicit error field
    if let Some(err) = v.get("error") {
        if !err.is_null() {
            return Some(true);
        }
    }
    // Check exit_code
    if let Some(code) = v.get("exit_code").and_then(|v| v.as_i64()) {
        return Some(code != 0);
    }
    None
}

struct MessageRow {
    id: i64,
    role: String,
    content: Option<String>,
    tool_calls: Option<String>,
    tool_call_id: Option<String>,
    reasoning_content: Option<String>,
    timestamp: f64,
}

// Named tuple for the sessions row to keep clippy::type_complexity happy.
type SessionRow = (
    Option<String>, // model
    Option<String>, // parent_session_id
    f64,            // started_at
    Option<f64>,    // ended_at
    Option<String>, // title
    i64,            // input_tokens
    i64,            // output_tokens
    i64,            // cache_read_tokens
    i64,            // cache_write_tokens
    Option<String>, // cwd
);

fn parse_session(conn: &Connection, session_ref: &SessionRef) -> Result<NormalizedSession> {
    let native_id = &session_ref.native_id;

    // The native_id is "{hermes_source}:{raw_session_id}". Split to get the raw ID
    // for querying the Hermes DB.
    let raw_session_id = native_id.split_once(':').map(|x| x.1).unwrap_or(native_id);

    let mut errata: Vec<Erratum> = Vec::new();

    // ── 1. Read session row ──────────────────────────────────────────────
    // NOTE: `source` and `tool_call_count` are intentionally NOT selected —
    // the session's own source is already embedded in `native_id` (from
    // discover()), and the tool call count is recomputed from parsed data.
    // Selecting them would bind unused variables (compiler warnings).
    let (
        model,
        parent_raw_id,
        started_at,
        ended_at,
        title,
        input_tokens,
        output_tokens,
        cache_read_tokens,
        cache_write_tokens,
        cwd,
    ): SessionRow = conn
        .query_row(
            "SELECT model, parent_session_id, started_at, ended_at, title,
                    input_tokens, output_tokens, cache_read_tokens, cache_write_tokens,
                    cwd
             FROM sessions WHERE id = ?1",
            [&raw_session_id],
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
                ))
            },
        )
        .map_err(|e| {
            OslError::Connector(format!(
                "session {raw_session_id} not found in {}: {e}",
                session_ref.path.display()
            ))
        })?;

    let sid = session_id("hermes", native_id);
    let started_at_iso = Some(seconds_to_iso(started_at));
    let ended_at_iso = ended_at.map(seconds_to_iso);

    // project_root: map cwd when non-NULL and non-empty
    let project_root = cwd.filter(|c| !c.is_empty()).map(PathBuf::from);

    // ── 2. Resolve parent_session_id ─────────────────────────────────────
    let parent_session_id = if let Some(parent_raw) = parent_raw_id {
        // Look up the parent's source to construct the correct prefixed native_id
        let parent_source: Option<String> = conn
            .query_row(
                "SELECT source FROM sessions WHERE id = ?1",
                [&parent_raw],
                |r| r.get(0),
            )
            .ok();
        if let Some(ps) = parent_source {
            let parent_native = format!("{}:{}", ps, parent_raw);
            Some(session_id("hermes", &parent_native))
        } else {
            // Parent not in this DB — skip
            None
        }
    } else {
        None
    };

    // ── 3. Read messages ─────────────────────────────────────────────────
    let mut messages: Vec<NormalizedMessage> = Vec::new();
    // Pending tool calls stored as (request_source_seq, NormalizedToolCall).
    // A Vec (not HashMap) is used so that:
    //   - duplicate call_ids across different assistant messages are preserved
    //     (no silent data loss — a HashMap keyed by call_id alone would
    //     overwrite earlier entries),
    //   - the final list is sorted deterministically (see end of function).
    // Resolution matches tool responses to the earliest *unresolved* pending
    // call with the same call_id (FIFO order).
    let mut pending_tool_calls: Vec<(i64, NormalizedToolCall)> = Vec::new();
    let mut turn_number: i64 = 0;

    let mut msg_stmt = conn.prepare(
        "SELECT id, role, content, tool_calls, tool_call_id,
                reasoning_content, timestamp
         FROM messages
         WHERE session_id = ?1 AND active = 1
         ORDER BY id",
    )?;
    let msg_rows = msg_stmt.query_map([&raw_session_id], |row| {
        Ok(MessageRow {
            id: row.get(0)?,
            role: row.get(1)?,
            content: row.get(2)?,
            tool_calls: row.get(3)?,
            tool_call_id: row.get(4)?,
            reasoning_content: row.get(5)?,
            timestamp: row.get(6)?,
        })
    })?;

    for (source_seq, msg_result) in msg_rows.enumerate() {
        let source_seq = (source_seq + 1) as i64;
        let row = msg_result?;

        match row.role.as_str() {
            "session_meta" => {
                // Skip session metadata messages — not conversation content
                continue;
            }
            "user" => {
                turn_number += 1;
                let msg_uuid = message_id(sid, &row.id.to_string());
                let content = row.content.filter(|c| !c.is_empty());

                messages.push(NormalizedMessage {
                    uuid: msg_uuid,
                    role: "user".to_string(),
                    content,
                    thinking: None,
                    parent_uuid: None,
                    source_seq,
                    turn_number,
                    sequence: 0,
                    input_tokens: None,
                    output_tokens: None,
                });
            }
            "assistant" => {
                turn_number += 1;
                let msg_uuid = message_id(sid, &row.id.to_string());
                let msg_ts = seconds_to_iso(row.timestamp);

                let content = row.content.filter(|c| !c.is_empty());
                let thinking = row.reasoning_content.filter(|c| !c.is_empty());

                // Parse tool_calls JSON array if present
                if let Some(tc_json) = row.tool_calls.as_deref() {
                    match serde_json::from_str::<serde_json::Value>(tc_json) {
                        Ok(serde_json::Value::Array(arr)) => {
                            for tc in arr {
                                let call_id = tc
                                    .get("call_id")
                                    .and_then(|v| v.as_str())
                                    .or_else(|| tc.get("id").and_then(|v| v.as_str()))
                                    .unwrap_or("unknown")
                                    .to_string();
                                let tool_name = tc
                                    .get("function")
                                    .and_then(|f| f.get("name"))
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("unknown")
                                    .to_string();
                                let tool_input = tc
                                    .get("function")
                                    .and_then(|f| f.get("arguments"))
                                    .and_then(|v| v.as_str())
                                    .map(String::from);

                                let tc_uuid = tool_call_id(sid, msg_uuid, &call_id);
                                let tc = NormalizedToolCall {
                                    uuid: tc_uuid,
                                    call_id: call_id.clone(),
                                    tool_name,
                                    tool_input,
                                    tool_output: None,
                                    tool_output_raw: None,
                                    is_error: None,
                                    started_at: Some(msg_ts.clone()),
                                    completed_at: None,
                                    request_message_uuid: msg_uuid,
                                    // response_message_uuid is None because tool
                                    // role messages are NOT persisted as
                                    // NormalizedMessages — setting it here would
                                    // create a dangling reference that
                                    // write_session silently resolves to NULL.
                                    response_message_uuid: None,
                                };
                                pending_tool_calls.push((source_seq, tc));
                            }
                        }
                        Ok(_) => {
                            errata.push(Erratum {
                                issue_type: "parse_error".to_string(),
                                field_path: Some(format!("message:{}:tool_calls", row.id)),
                                detail: "tool_calls is not a JSON array".to_string(),
                                raw_snippet: Some(tc_json.chars().take(500).collect()),
                            });
                        }
                        Err(e) => {
                            errata.push(Erratum {
                                issue_type: "parse_error".to_string(),
                                field_path: Some(format!("message:{}:tool_calls", row.id)),
                                detail: format!("failed to parse tool_calls JSON: {e}"),
                                raw_snippet: Some(tc_json.chars().take(500).collect()),
                            });
                        }
                    }
                }

                // Per-message input tokens are not available from the Hermes
                // messages schema — session-level token totals (from the
                // sessions row above) are authoritative. Setting
                // input_tokens: None avoids shadowing the session-level
                // `input_tokens` binding used in the NormalizedSession
                // construction.
                messages.push(NormalizedMessage {
                    uuid: msg_uuid,
                    role: "assistant".to_string(),
                    content,
                    thinking,
                    parent_uuid: None,
                    source_seq,
                    turn_number,
                    sequence: 0,
                    input_tokens: None,
                    output_tokens: None,
                });
            }
            "tool" => {
                // Tool response — resolve the earliest *unresolved* pending tool
                // call with a matching call_id (FIFO order). Do NOT emit as a
                // NormalizedMessage (matches OpenCode/Claude pattern where tool
                // results live in tool_calls, not messages).
                if let Some(call_id) = row.tool_call_id.as_deref() {
                    // Find all unresolved pending calls matching this call_id.
                    let matching: Vec<usize> = pending_tool_calls
                        .iter()
                        .enumerate()
                        .filter(|(_, (_, tc))| {
                            tc.call_id.as_str() == call_id && tc.completed_at.is_none()
                        })
                        .map(|(i, _)| i)
                        .collect();

                    if let Some(&idx) = matching.first() {
                        // Ambiguity: more than one pending call shares this call_id.
                        // This happens when two assistant messages emit tool calls
                        // with the same call_id. We resolve the earliest (FIFO) and
                        // flag the situation — no data is lost (both calls retained).
                        if matching.len() > 1 {
                            errata.push(Erratum {
                                issue_type: "duplicate_call_id".to_string(),
                                field_path: Some(format!(
                                    "message:{}:tool_call_id:{}",
                                    row.id, call_id
                                )),
                                detail: format!(
                                    "tool response for call_id '{call_id}' matches {} \
                                     pending tool calls; resolving the earliest (FIFO)",
                                    matching.len()
                                ),
                                raw_snippet: None,
                            });
                        }

                        let content = row.content.as_deref().unwrap_or("");
                        let (_, tc) = &mut pending_tool_calls[idx];
                        tc.tool_output = if content.is_empty() {
                            None
                        } else {
                            Some(content.to_string())
                        };
                        tc.is_error = infer_is_error(content);
                        tc.completed_at = Some(seconds_to_iso(row.timestamp));
                        // response_message_uuid is intentionally left None — tool
                        // role messages are not persisted as NormalizedMessages,
                        // so a UUID here would never resolve in write_session
                        // (it would silently become NULL in the vault).
                    } else {
                        // Orphaned tool response — no matching assistant tool_calls
                        errata.push(Erratum {
                            issue_type: "orphaned_tool_response".to_string(),
                            field_path: Some(format!(
                                "message:{}:tool_call_id:{}",
                                row.id, call_id
                            )),
                            detail: format!(
                                "tool response has no matching assistant tool_calls \
                                 entry for call_id '{call_id}'"
                            ),
                            raw_snippet: None,
                        });
                    }
                }
            }
            other => {
                errata.push(Erratum {
                    issue_type: "unknown_role".to_string(),
                    field_path: Some(format!("message:{}:role", row.id)),
                    detail: format!("unhandled message role '{other}'"),
                    raw_snippet: None,
                });
            }
        }
    }

    // Sort pending tool calls deterministically by (request_source_seq, call_id)
    // so export output and test assertions are stable across runs (Vec
    // iteration would otherwise be insertion-order, but sorting makes the
    // contract explicit and robust to future refactors).
    pending_tool_calls.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.call_id.cmp(&b.1.call_id)));

    let mut tool_calls: Vec<NormalizedToolCall> = Vec::with_capacity(pending_tool_calls.len());
    for (source_seq, tc) in pending_tool_calls {
        // Flag tool calls that were requested but never received a response
        // message. This distinguishes them from resolved calls with empty output.
        if tc.completed_at.is_none() {
            errata.push(Erratum {
                issue_type: "unresolved_tool_call".to_string(),
                field_path: Some(format!(
                    "message_seq:{}:tool_call:{}",
                    source_seq, tc.call_id
                )),
                detail: format!(
                    "tool call '{}' (call_id '{}') has no matching tool response \
                     message",
                    tc.tool_name, tc.call_id
                ),
                raw_snippet: None,
            });
        }
        tool_calls.push(tc);
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
        started_at: started_at_iso,
        ended_at: ended_at_iso,
        model,
        git_branch: None,
        git_sha: None,
        raw_path: session_ref.path.clone(),
        project_root,
        parent_session_id,
        input_tokens,
        output_tokens,
        cache_read_tokens,
        cache_write_tokens,
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
    use crate::connector::Connector;
    use std::path::PathBuf;

    fn build_test_db_schema(conn: &Connection) {
        conn.execute_batch(
            "
            CREATE TABLE sessions (
                id TEXT PRIMARY KEY,
                source TEXT NOT NULL,
                user_id TEXT,
                model TEXT,
                model_config TEXT,
                system_prompt TEXT,
                parent_session_id TEXT,
                started_at REAL NOT NULL,
                ended_at REAL,
                end_reason TEXT,
                message_count INTEGER DEFAULT 0,
                tool_call_count INTEGER DEFAULT 0,
                input_tokens INTEGER DEFAULT 0,
                output_tokens INTEGER DEFAULT 0,
                cache_read_tokens INTEGER DEFAULT 0,
                cache_write_tokens INTEGER DEFAULT 0,
                reasoning_tokens INTEGER DEFAULT 0,
                billing_provider TEXT,
                billing_base_url TEXT,
                billing_mode TEXT,
                estimated_cost_usd REAL,
                actual_cost_usd REAL,
                cost_status TEXT,
                cost_source TEXT,
                pricing_version TEXT,
                title TEXT,
                api_call_count INTEGER DEFAULT 0,
                handoff_state TEXT,
                handoff_platform TEXT,
                handoff_error TEXT,
                cwd TEXT,
                rewind_count INTEGER NOT NULL DEFAULT 0,
                archived INTEGER NOT NULL DEFAULT 0,
                FOREIGN KEY (parent_session_id) REFERENCES sessions(id)
            );
            CREATE TABLE messages (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                session_id TEXT NOT NULL REFERENCES sessions(id),
                role TEXT NOT NULL,
                content TEXT,
                tool_call_id TEXT,
                tool_calls TEXT,
                tool_name TEXT,
                timestamp REAL NOT NULL,
                token_count INTEGER,
                finish_reason TEXT,
                reasoning TEXT,
                reasoning_content TEXT,
                reasoning_details TEXT,
                codex_reasoning_items TEXT,
                codex_message_items TEXT,
                platform_message_id TEXT,
                observed INTEGER DEFAULT 0,
                active INTEGER NOT NULL DEFAULT 1,
                compacted INTEGER NOT NULL DEFAULT 0
            );
            CREATE TABLE compression_locks (
                session_id TEXT PRIMARY KEY,
                holder TEXT NOT NULL,
                acquired_at REAL NOT NULL,
                expires_at REAL NOT NULL
            );
            CREATE TABLE schema_version (version INTEGER NOT NULL);
            CREATE TABLE state_meta (key TEXT PRIMARY KEY, value TEXT);
            ",
        )
        .unwrap();
    }

    fn build_test_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        build_test_db_schema(&conn);
        conn
    }

    fn insert_session(conn: &Connection, sid: &str, source: &str, model: &str) {
        conn.execute(
            "INSERT INTO sessions (
                id, source, model, started_at, ended_at,
                input_tokens, output_tokens, cache_read_tokens, cache_write_tokens,
                title, cwd, archived
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, 0)",
            rusqlite::params![
                sid,
                source,
                model,
                1780258945.0_f64,
                1780258960.0_f64,
                100_i64,
                50_i64,
                10_i64,
                0_i64,
                "Test session",
                Option::<&str>::None,
            ],
        )
        .unwrap();
    }

    #[allow(clippy::too_many_arguments)]
    fn insert_message(
        conn: &Connection,
        session_id: &str,
        role: &str,
        content: Option<&str>,
        tool_calls: Option<&str>,
        tool_call_id: Option<&str>,
        reasoning_content: Option<&str>,
        timestamp: f64,
    ) {
        conn.execute(
            "INSERT INTO messages (
                session_id, role, content, tool_calls, tool_call_id,
                reasoning_content, timestamp
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            rusqlite::params![
                session_id,
                role,
                content,
                tool_calls,
                tool_call_id,
                reasoning_content,
                timestamp,
            ],
        )
        .unwrap();
    }

    fn make_session_ref(native_id: &str) -> SessionRef {
        SessionRef {
            source: "hermes".to_string(),
            native_id: native_id.to_string(),
            path: PathBuf::from("/tmp/test_hermes.db"),
            project_path: None,
        }
    }

    #[test]
    fn minimal_session_has_two_messages() {
        let conn = build_test_db();
        conn.execute(
            "INSERT INTO sessions (
                id, source, model, started_at, ended_at,
                input_tokens, output_tokens, cache_read_tokens, cache_write_tokens,
                title, cwd, archived
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, 0)",
            rusqlite::params![
                "test_minimal_001",
                "telegram",
                "claude-sonnet-4-5",
                1780258945.039_f64,
                1780258950.1_f64,
                100_i64,
                50_i64,
                0_i64,
                0_i64,
                Option::<&str>::None,
                Option::<&str>::None,
            ],
        )
        .unwrap();

        insert_message(
            &conn,
            "test_minimal_001",
            "user",
            Some("Hello Hermes"),
            None,
            None,
            None,
            1780258945.5,
        );
        insert_message(
            &conn,
            "test_minimal_001",
            "assistant",
            Some("Hi there!"),
            None,
            None,
            None,
            1780258946.0,
        );

        let session = parse_session(&conn, &make_session_ref("telegram:test_minimal_001")).unwrap();

        assert_eq!(session.native_id, "telegram:test_minimal_001");
        assert_eq!(session.messages.len(), 2);
        assert_eq!(session.messages[0].role, "user");
        assert_eq!(session.messages[0].content.as_deref(), Some("Hello Hermes"));
        assert_eq!(session.messages[1].role, "assistant");
        assert_eq!(session.messages[1].content.as_deref(), Some("Hi there!"));
        assert!(session.tool_calls.is_empty());
        assert_eq!(session.model.as_deref(), Some("claude-sonnet-4-5"));
        assert_eq!(session.source, "hermes");
        assert!(session.started_at.as_deref().unwrap().starts_with("2026-"));
        assert!(session.started_at.as_deref().unwrap().ends_with('Z'));
        assert_eq!(session.project_root, None);
    }

    #[test]
    fn session_with_tool_calls() {
        let conn = build_test_db();
        insert_session(&conn, "test_tools_001", "telegram", "claude-sonnet-4-5");

        insert_message(
            &conn,
            "test_tools_001",
            "user",
            Some("Run command"),
            None,
            None,
            None,
            1780258945.5,
        );
        insert_message(
            &conn,
            "test_tools_001",
            "assistant",
            Some(""),
            Some(
                r#"[{"id":"call_00_test","call_id":"call_00_test","type":"function","function":{"name":"terminal","arguments":"{\"command\":\"echo hello\"}"}}]"#,
            ),
            None,
            None,
            1780258946.0,
        );
        insert_message(
            &conn,
            "test_tools_001",
            "tool",
            Some(r#"{"output": "hello\n", "exit_code": 0, "error": null}"#),
            None,
            Some("call_00_test"),
            None,
            1780258947.0,
        );
        insert_message(
            &conn,
            "test_tools_001",
            "assistant",
            Some("Done"),
            None,
            None,
            None,
            1780258948.0,
        );

        let session = parse_session(&conn, &make_session_ref("telegram:test_tools_001")).unwrap();

        assert_eq!(session.messages.len(), 3);
        assert_eq!(session.tool_calls.len(), 1);

        let tc = &session.tool_calls[0];
        assert_eq!(tc.call_id, "call_00_test");
        assert_eq!(tc.tool_name, "terminal");
        assert!(tc.tool_input.as_deref().unwrap().contains("echo hello"));
        assert_eq!(
            tc.tool_output.as_deref(),
            Some(r#"{"output": "hello\n", "exit_code": 0, "error": null}"#)
        );
        assert_eq!(tc.is_error, Some(false));
        assert_eq!(tc.request_message_uuid, session.messages[1].uuid);
        assert_eq!(tc.response_message_uuid, None);
        assert!(tc.started_at.is_some());
        assert!(tc.completed_at.is_some());
    }

    #[test]
    fn cron_session_native_id_prefix() {
        let conn = build_test_db();
        insert_session(&conn, "cron_test_001", "cron", "deepseek-v4-flash-free");
        insert_message(
            &conn,
            "cron_test_001",
            "user",
            Some("Cron hello"),
            None,
            None,
            None,
            1780258945.5,
        );
        insert_message(
            &conn,
            "cron_test_001",
            "assistant",
            Some("Cron hi"),
            None,
            None,
            None,
            1780258946.0,
        );

        let session = parse_session(&conn, &make_session_ref("cron:cron_test_001")).unwrap();
        assert_eq!(session.native_id, "cron:cron_test_001");
        assert!(session.native_id.starts_with("cron:"));
    }

    #[test]
    fn session_meta_is_skipped() {
        let conn = build_test_db();
        insert_session(&conn, "test_meta_001", "telegram", "claude-sonnet-4-5");
        insert_message(
            &conn,
            "test_meta_001",
            "user",
            Some("Hello"),
            None,
            None,
            None,
            1780258945.5,
        );
        insert_message(
            &conn,
            "test_meta_001",
            "session_meta",
            Some("meta"),
            None,
            None,
            None,
            1780258945.6,
        );
        insert_message(
            &conn,
            "test_meta_001",
            "assistant",
            Some("Hi"),
            None,
            None,
            None,
            1780258946.0,
        );

        let session = parse_session(&conn, &make_session_ref("telegram:test_meta_001")).unwrap();
        assert_eq!(session.messages.len(), 2);
        assert!(!session.messages.iter().any(|m| m.role == "session_meta"));
    }

    #[test]
    fn reasoning_content_maps_to_thinking() {
        let conn = build_test_db();
        insert_session(&conn, "test_reason_001", "telegram", "claude-sonnet-4-5");
        insert_message(
            &conn,
            "test_reason_001",
            "user",
            Some("Hello"),
            None,
            None,
            None,
            1780258945.5,
        );
        insert_message(
            &conn,
            "test_reason_001",
            "assistant",
            Some("Hi"),
            None,
            None,
            Some("Let me think about this..."),
            1780258946.0,
        );

        let session = parse_session(&conn, &make_session_ref("telegram:test_reason_001")).unwrap();
        assert_eq!(
            session.messages[1].thinking.as_deref(),
            Some("Let me think about this...")
        );
    }

    #[test]
    fn seconds_to_iso_conversion() {
        assert_eq!(seconds_to_iso(0.0), "1970-01-01T00:00:00.000Z");

        let iso = seconds_to_iso(1780258945.039);
        assert!(iso.starts_with("2026-"));
        assert!(iso.ends_with('Z'));
        assert!(iso.contains('T'));

        // Verify exactly three digits between the dot and 'Z'.
        let dot = iso.rfind('.').unwrap();
        let millis = &iso[dot + 1..iso.len() - 1];
        assert_eq!(millis.len(), 3);
    }

    #[test]
    fn orphaned_tool_response_emits_erratum() {
        let conn = build_test_db();
        insert_session(&conn, "test_orphan_001", "telegram", "claude-sonnet-4-5");
        insert_message(
            &conn,
            "test_orphan_001",
            "user",
            Some("Hello"),
            None,
            None,
            None,
            1780258945.5,
        );
        insert_message(
            &conn,
            "test_orphan_001",
            "tool",
            Some(r#"{"exit_code": 0, "error": null}"#),
            None,
            Some("no_such_call"),
            None,
            1780258946.0,
        );

        let session = parse_session(&conn, &make_session_ref("telegram:test_orphan_001")).unwrap();
        assert!(session
            .errata
            .iter()
            .any(|e| e.issue_type == "orphaned_tool_response"));
        assert!(session.tool_calls.is_empty());
    }

    #[test]
    fn parent_session_id_resolution() {
        let conn = build_test_db();
        insert_session(&conn, "parent_001", "telegram", "claude-sonnet-4-5");
        insert_message(
            &conn,
            "parent_001",
            "user",
            Some("Parent"),
            None,
            None,
            None,
            1780258945.5,
        );

        conn.execute(
            "INSERT INTO sessions (
                id, source, model, started_at, ended_at,
                input_tokens, output_tokens, cache_read_tokens, cache_write_tokens,
                title, cwd, archived, parent_session_id
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, 0, ?12)",
            rusqlite::params![
                "child_001",
                "telegram",
                "claude-sonnet-4-5",
                1780258950.0_f64,
                1780258960.0_f64,
                10_i64,
                5_i64,
                0_i64,
                0_i64,
                "Child session",
                Option::<&str>::None,
                "parent_001",
            ],
        )
        .unwrap();

        let session = parse_session(&conn, &make_session_ref("telegram:child_001")).unwrap();
        assert_eq!(
            session.parent_session_id,
            Some(session_id("hermes", "telegram:parent_001"))
        );
    }

    #[test]
    fn discover_filtered_recency_30_keeps_only_recent_sessions() {
        let tmp = tempfile::TempDir::new().unwrap();
        let db_path = tmp.path().join("hermes.db");
        let conn = Connection::open(&db_path).unwrap();
        build_test_db_schema(&conn);

        let now = crate::recency::now_unix_seconds();
        let old_started = (now - 60 * 86400) as f64;
        let recent_started = now as f64;

        conn.execute(
            "INSERT INTO sessions (
                id, source, model, started_at, ended_at,
                input_tokens, output_tokens, cache_read_tokens, cache_write_tokens,
                title, cwd, archived
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, 0)",
            rusqlite::params![
                "old_session",
                "telegram",
                "claude-sonnet-4-5",
                old_started,
                old_started + 10.0,
                0_i64,
                0_i64,
                0_i64,
                0_i64,
                "Old session",
                Option::<&str>::None,
            ],
        )
        .unwrap();

        conn.execute(
            "INSERT INTO sessions (
                id, source, model, started_at, ended_at,
                input_tokens, output_tokens, cache_read_tokens, cache_write_tokens,
                title, cwd, archived
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, 0)",
            rusqlite::params![
                "recent_session",
                "telegram",
                "claude-sonnet-4-5",
                recent_started,
                recent_started + 10.0,
                0_i64,
                0_i64,
                0_i64,
                0_i64,
                "Recent session",
                Option::<&str>::None,
            ],
        )
        .unwrap();

        drop(conn);

        let connector = crate::connector::SqliteConnector {
            parser: HermesSessionParser,
        };
        let filter = crate::recency::RecencyFilter::from_flags(Some(30), None, now).unwrap();
        let refs = connector.discover_filtered(&db_path, &filter).unwrap();
        assert_eq!(refs.len(), 1);
        assert!(
            refs[0].native_id.ends_with("recent_session"),
            "got {:?}",
            refs[0].native_id
        );
    }

    #[test]
    fn discover_filtered_none_returns_all_sessions() {
        let tmp = tempfile::TempDir::new().unwrap();
        let db_path = tmp.path().join("hermes.db");
        let conn = Connection::open(&db_path).unwrap();
        build_test_db_schema(&conn);

        insert_session(&conn, "session_a", "telegram", "claude-sonnet-4-5");
        insert_session(&conn, "session_b", "telegram", "claude-sonnet-4-5");
        drop(conn);

        let connector = crate::connector::SqliteConnector {
            parser: HermesSessionParser,
        };
        let refs = connector
            .discover_filtered(&db_path, &crate::recency::RecencyFilter::none())
            .unwrap();
        assert_eq!(refs.len(), 2);
    }

    #[test]
    fn session_ids_are_deterministic() {
        let conn = build_test_db();
        insert_session(&conn, "test_det_001", "telegram", "claude-sonnet-4-5");
        insert_message(
            &conn,
            "test_det_001",
            "user",
            Some("Hello"),
            None,
            None,
            None,
            1780258945.5,
        );
        insert_message(
            &conn,
            "test_det_001",
            "assistant",
            Some("Hi"),
            None,
            None,
            None,
            1780258946.0,
        );

        let ref1 = make_session_ref("telegram:test_det_001");
        let ref2 = make_session_ref("telegram:test_det_001");
        let s1 = parse_session(&conn, &ref1).unwrap();
        let s2 = parse_session(&conn, &ref2).unwrap();

        assert_eq!(s1.id, s2.id);
        for (m1, m2) in s1.messages.iter().zip(s2.messages.iter()) {
            assert_eq!(m1.uuid, m2.uuid);
        }
    }

    #[test]
    fn empty_session_returns_error() {
        let conn = build_test_db();
        let result = parse_session(&conn, &make_session_ref("telegram:does_not_exist"));
        assert!(result.is_err());
    }

    #[test]
    fn infer_is_error_heuristic() {
        assert_eq!(
            infer_is_error(r#"{"exit_code": 0, "error": null}"#),
            Some(false)
        );
        assert_eq!(
            infer_is_error(r#"{"exit_code": 1, "error": null}"#),
            Some(true)
        );
        assert_eq!(
            infer_is_error(r#"{"error": "something failed"}"#),
            Some(true)
        );
        assert_eq!(infer_is_error(r#"{"output": "ok"}"#), None);
        assert_eq!(infer_is_error("not json"), None);
    }

    #[test]
    fn unresolved_tool_call_emits_erratum() {
        let conn = build_test_db();
        insert_session(&conn, "test_unres_001", "telegram", "claude-sonnet-4-5");
        insert_message(
            &conn,
            "test_unres_001",
            "user",
            Some("Hello"),
            None,
            None,
            None,
            1780258945.5,
        );
        insert_message(
            &conn,
            "test_unres_001",
            "assistant",
            Some(""),
            Some(
                r#"[{"id":"call_unres_01","call_id":"call_unres_01","type":"function","function":{"name":"terminal","arguments":"{\"command\":\"echo hi\"}"}}]"#,
            ),
            None,
            None,
            1780258946.0,
        );
        insert_message(
            &conn,
            "test_unres_001",
            "assistant",
            Some("Follow-up"),
            None,
            None,
            None,
            1780258947.0,
        );

        let session = parse_session(&conn, &make_session_ref("telegram:test_unres_001")).unwrap();

        assert_eq!(session.tool_calls.len(), 1);
        assert_eq!(session.tool_calls[0].completed_at, None);
        assert_eq!(session.tool_calls[0].tool_output, None);
        assert!(session
            .errata
            .iter()
            .any(|e| e.issue_type == "unresolved_tool_call"));
        assert_eq!(session.messages.len(), 3);
    }

    #[test]
    fn duplicate_call_id_preserves_both() {
        let conn = build_test_db();
        insert_session(&conn, "test_dup_001", "telegram", "claude-sonnet-4-5");
        insert_message(
            &conn,
            "test_dup_001",
            "user",
            Some("Hello"),
            None,
            None,
            None,
            1780258945.5,
        );
        insert_message(
            &conn,
            "test_dup_001",
            "assistant",
            Some(""),
            Some(
                r#"[{"id":"dup_call_01","call_id":"dup_call_01","type":"function","function":{"name":"terminal","arguments":"{\"command\":\"echo one\"}"}}]"#,
            ),
            None,
            None,
            1780258946.0,
        );
        insert_message(
            &conn,
            "test_dup_001",
            "assistant",
            Some(""),
            Some(
                r#"[{"id":"dup_call_01","call_id":"dup_call_01","type":"function","function":{"name":"terminal","arguments":"{\"command\":\"echo two\"}"}}]"#,
            ),
            None,
            None,
            1780258947.0,
        );
        insert_message(
            &conn,
            "test_dup_001",
            "tool",
            Some(r#"{"exit_code":0,"error":null}"#),
            None,
            Some("dup_call_01"),
            None,
            1780258948.0,
        );
        insert_message(
            &conn,
            "test_dup_001",
            "assistant",
            Some("Done"),
            None,
            None,
            None,
            1780258949.0,
        );

        let session = parse_session(&conn, &make_session_ref("telegram:test_dup_001")).unwrap();

        assert_eq!(session.tool_calls.len(), 2);
        let completed_count = session
            .tool_calls
            .iter()
            .filter(|tc| tc.completed_at.is_some())
            .count();
        assert_eq!(completed_count, 1);
        assert!(session
            .errata
            .iter()
            .any(|e| e.issue_type == "duplicate_call_id"));
        assert!(session
            .errata
            .iter()
            .any(|e| e.issue_type == "unresolved_tool_call"));
    }
}
