use std::path::Path;

use rusqlite::Connection;
use uuid::Uuid;

use crate::connector::claude::peek_session_id;
use crate::connector::codex::peek_codex_id;
use crate::connector::for_source;
use crate::connector::probe;
use crate::error::{OslError, Result};
use crate::model::{IngestReport, IngestReportSession, NormalizedSession, SessionRef};
use crate::project;

/// Detect whether a file path points to a SQLite database based on its extension.
fn is_db_file(path: &Path) -> bool {
    path.extension()
        .map(|ext| ext == "db" || ext == "sqlite")
        .unwrap_or(false)
}

/// Detect whether a SQLite database is a Hermes state.db or an OpenCode opencode.db
/// by checking for the Hermes-specific `compression_locks` table.
fn detect_sqlite_kind(path: &Path) -> Result<&'static str> {
    let conn = Connection::open(path)?;
    let is_hermes: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='compression_locks')",
        [],
        |r| r.get(0),
    )?;
    if is_hermes {
        Ok("hermes")
    } else {
        Ok("opencode")
    }
}

/// Ingest a file or directory into the vault.
///
/// Routing rules:
/// - `.db` / `.sqlite` files     → Schema detection: Hermes (compression_locks table) or OpenCode
/// - `.jsonl` files               → Detected as Claude Code or Codex CLI rollout JSONL
/// - Directories                  → Concatenate discoveries from Claude and Codex connectors
/// - Other files                  → Unsupported
///
/// For SQLite databases, pass the `.db` file path directly; the connector is chosen
/// by schema detection.
pub fn ingest(conn: &mut Connection, path: &Path) -> Result<IngestReport> {
    ingest_filtered(conn, path, &crate::recency::RecencyFilter::none())
}

pub fn ingest_filtered(
    conn: &mut Connection,
    path: &Path,
    filter: &crate::recency::RecencyFilter,
) -> Result<IngestReport> {
    let refs: Vec<SessionRef> = if path.is_file() && is_db_file(path) {
        // SQLite database — detect Hermes vs OpenCode by schema, then discover.
        let kind = detect_sqlite_kind(path)?;
        let connector = for_source(kind)
            .ok_or_else(|| OslError::Connector(format!("no connector for '{kind}'")))?;
        connector.discover_filtered(path, filter)?
    } else if path.is_file() {
        // Single session file — detect JSONL kind (Claude vs Codex).
        let kind = probe::detect_jsonl_kind(path)?;
        if !filter.keep_file(path) {
            // Old file: skip it entirely. Return an empty report (this is a pre-filter
            // miss, not an error).
            return Ok(IngestReport {
                sessions: Vec::new(),
            });
        }
        match kind {
            Some("claude") => {
                let native_id = peek_session_id(path)?.ok_or_else(|| {
                    OslError::Connector(format!(
                        "could not read claude session id from {}",
                        path.display()
                    ))
                })?;
                vec![SessionRef {
                    source: "claude".to_string(),
                    native_id,
                    path: path.to_path_buf(),
                    project_path: path.parent().map(Path::to_path_buf),
                }]
            }
            Some("codex") => {
                let native_id = peek_codex_id(path)?.ok_or_else(|| {
                    OslError::Connector(format!(
                        "could not read codex session id from {}",
                        path.display()
                    ))
                })?;
                vec![SessionRef {
                    source: "codex".to_string(),
                    native_id,
                    path: path.to_path_buf(),
                    project_path: path.parent().map(Path::to_path_buf),
                }]
            }
            Some("copilot") => {
                let native_id = probe::peek_copilot_id(path)?.ok_or_else(|| {
                    OslError::Connector(format!(
                        "could not read copilot session id from {}",
                        path.display()
                    ))
                })?;
                vec![SessionRef {
                    source: "copilot".to_string(),
                    native_id,
                    path: path.to_path_buf(),
                    project_path: path.parent().map(Path::to_path_buf),
                }]
            }
            _ => {
                return Err(OslError::Connector(format!(
                    "unsupported file format: {}",
                    path.display()
                )));
            }
        }
    } else {
        // Directory — discover both Claude and Codex rollout files and concatenate
        // results so a mixed directory can be ingested in one pass.
        let claude_connector = for_source("claude")
            .ok_or_else(|| OslError::Connector("no connector for 'claude'".to_string()))?;
        let codex_connector = for_source("codex")
            .ok_or_else(|| OslError::Connector("no connector for 'codex'".to_string()))?;
        let copilot_connector = for_source("copilot")
            .ok_or_else(|| OslError::Connector("no connector for 'copilot'".to_string()))?;
        let mut refs = claude_connector.discover_filtered(path, filter)?;
        refs.extend(codex_connector.discover_filtered(path, filter)?);
        refs.extend(copilot_connector.discover_filtered(path, filter)?);
        // JSONL connectors use the default discover_filtered (returns all refs); apply
        // the mtime pre-filter here before parse().
        let refs: Vec<SessionRef> = refs
            .into_iter()
            .filter(|r| filter.keep_file(&r.path))
            .collect();
        refs
    };

    let mut sessions = Vec::with_capacity(refs.len());
    for session_ref in refs {
        let connector = for_source(&session_ref.source).ok_or_else(|| {
            OslError::Connector(format!("no connector for '{}'", session_ref.source))
        })?;
        let session = connector.parse(&session_ref)?;
        let report = write_session(conn, &session)?;
        sessions.push(report);
    }

    Ok(IngestReport { sessions })
}

/// Write (or rewrite) a single session transactionally.
/// Purge-and-reload per session gives idempotency regardless of source-file edits.
fn write_session(
    conn: &mut Connection,
    session: &NormalizedSession,
) -> Result<IngestReportSession> {
    let tx = conn.transaction()?;

    let source_id: i64 = tx.query_row(
        "SELECT id FROM sources WHERE name = ?1",
        [&session.source],
        |r| r.get(0),
    )?;

    let project_id = if let Some(root) = session.project_root.as_ref() {
        let project = project::resolve(root)?;
        Some(project::upsert(&tx, &project)?)
    } else {
        None
    };

    let sid = session.id.to_string();

    // Guard: if parent_session_id references a session not yet in the vault,
    // null it out to avoid FK violation (PRAGMA foreign_keys=ON is set).
    let parent_session_id_str: Option<String> = match session.parent_session_id {
        Some(parent) => {
            let pid = parent.to_string();
            let exists: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM sessions WHERE id = ?1)",
                [&pid],
                |r| r.get(0),
            )?;
            if exists {
                Some(pid)
            } else {
                None
            }
        }
        None => None,
    };

    // Null out any child sessions' parent_session_id references before purging
    // this session, to avoid FK constraint violation (children may reference
    // this session as their parent).
    tx.execute(
        "UPDATE sessions SET parent_session_id = NULL WHERE parent_session_id = ?1",
        [&sid],
    )?;

    // 1. Purge existing session rows. FTS5 'delete' triggers fire on messages.
    tx.execute("DELETE FROM errata WHERE session_id = ?1", [&sid])?;
    tx.execute("DELETE FROM tool_calls WHERE session_id = ?1", [&sid])?;
    tx.execute("DELETE FROM messages WHERE session_id = ?1", [&sid])?;
    tx.execute("DELETE FROM sessions WHERE id = ?1", [&sid])?;

    // 2. Insert session. total_tokens is GENERATED — do not supply it.
    tx.execute(
        "INSERT INTO sessions (
            id, source_id, project_id, title, started_at, ended_at, model,
            tool_call_count, input_tokens, output_tokens, cache_read_tokens, cache_write_tokens,
            git_branch, git_sha, raw_path, parent_session_id, error_count
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)",
        rusqlite::params![
            sid,
            source_id,
            project_id,
            session.title.as_deref(),
            session.started_at.as_deref(),
            session.ended_at.as_deref(),
            session.model.as_deref(),
            session.tool_call_count,
            session.input_tokens,
            session.output_tokens,
            session.cache_read_tokens,
            session.cache_write_tokens,
            session.git_branch.as_deref(),
            session.git_sha.as_deref(),
            session.raw_path.to_string_lossy().to_string(),
            parent_session_id_str.as_deref(),
            session.error_count,
        ],
    )?;

    // 3. Insert messages and build uuid -> rowid map.
    let mut msg_rowids: std::collections::HashMap<Uuid, i64> = std::collections::HashMap::new();
    for msg in &session.messages {
        tx.execute(
            "INSERT INTO messages (
                uuid, session_id, role, content, thinking, parent_uuid,
                source_seq, turn_number, sequence, input_tokens, output_tokens
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            rusqlite::params![
                msg.uuid.to_string(),
                sid,
                &msg.role,
                msg.content.as_deref(),
                msg.thinking.as_deref(),
                msg.parent_uuid.as_deref(),
                msg.source_seq,
                msg.turn_number,
                msg.sequence,
                msg.input_tokens,
                msg.output_tokens,
            ],
        )?;
        let rowid = tx.last_insert_rowid();
        msg_rowids.insert(msg.uuid, rowid);
    }

    // 4. Insert tool_calls, mapping request/response message uuids to rowids.
    for tc in &session.tool_calls {
        let request_id = *msg_rowids.get(&tc.request_message_uuid).ok_or_else(|| {
            OslError::Connector(format!(
                "missing request message {}",
                tc.request_message_uuid
            ))
        })?;
        let response_id = tc
            .response_message_uuid
            .and_then(|u| msg_rowids.get(&u).copied());
        tx.execute(
            "INSERT INTO tool_calls (
                uuid, session_id, request_message_id, response_message_id, call_id,
                tool_name, tool_input, tool_output, tool_output_raw, is_error,
                started_at, completed_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            rusqlite::params![
                tc.uuid.to_string(),
                sid,
                request_id,
                response_id,
                &tc.call_id,
                &tc.tool_name,
                tc.tool_input.as_deref(),
                tc.tool_output.as_deref(),
                tc.tool_output_raw.as_deref(),
                tc.is_error.map(|b| if b { 1 } else { 0 }),
                tc.started_at.as_deref(),
                tc.completed_at.as_deref(),
            ],
        )?;
    }

    // 5. Insert errata.
    for err in &session.errata {
        tx.execute(
            "INSERT INTO errata (session_id, source_id, issue_type, field_path, detail, raw_snippet)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![
                sid,
                source_id,
                &err.issue_type,
                err.field_path.as_deref(),
                &err.detail,
                err.raw_snippet.as_deref(),
            ],
        )?;
    }

    tx.commit()?;

    Ok(IngestReportSession {
        session_id: session.id,
        title: session.title.clone(),
        message_count: session.messages.len(),
        tool_call_count: session.tool_calls.len(),
        total_tokens: session.input_tokens
            + session.output_tokens
            + session.cache_read_tokens
            + session.cache_write_tokens,
    })
}
