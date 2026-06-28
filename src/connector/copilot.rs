//! GitHub Copilot Chat local-storage connector.
//!
//! Parses two VS Code on-disk formats found under
//! `~/.config/Code/User/workspaceStorage/<hash>/GitHub.copilot-chat/`:
//!
//! * `chatSessions/*.jsonl` — patch-replay JSONL. The first line is a `kind: 0`
//!   seed carrying the full session state in `v`; later `kind: 1` property patches
//!   and `kind: 2` array patches mutate that state.
//!
//! * `transcripts/*.jsonl` — event-stream JSONL starting with
//!   `type: "session.start"` followed by `user.message`, `assistant.message`, and
//!   optional `tool.execution_*` events.
//!
//! Both formats are read-only, carry no usage/token data, and contain no git
//! metadata. IDs are deterministic UUIDv5 derivations so re-ingest is idempotent.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_json::Value;
use uuid::Uuid;

use crate::connector::opencode::ms_to_iso;
use crate::connector::{probe, reader, walk, Connector};
use crate::error::{OslError, Result};
use crate::ids::{message_id, session_id, tool_call_id};
use crate::model::{Erratum, NormalizedMessage, NormalizedSession, NormalizedToolCall, SessionRef};

pub struct CopilotChatConnector;

impl Connector for CopilotChatConnector {
    fn name(&self) -> &'static str {
        "copilot"
    }

    fn discover(&self, directory: &Path) -> Result<Vec<SessionRef>> {
        walk::discover_jsonl(directory, directory, "copilot", &|p| {
            probe::peek_copilot_id(p)
        })
    }

    fn parse(&self, session_ref: &SessionRef) -> Result<NormalizedSession> {
        parse_file(session_ref)
    }
}

// === shred helpers (mirrors codex.rs) ===

fn get_str<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key).and_then(|v| v.as_str())
}

fn get_i64(value: &Value, key: &str) -> Option<i64> {
    value.get(key).and_then(|v| v.as_i64())
}

fn join_content_parts(content: &Value) -> Option<String> {
    let arr = content.as_array()?;
    let parts: Vec<String> = arr
        .iter()
        .filter_map(|part| {
            let part_type = part.get("type").and_then(|v| v.as_str())?;
            if part_type == "text" || part_type == "output_text" || part_type == "input_text" {
                part.get("text")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
            } else {
                None
            }
        })
        .filter(|s| !s.is_empty())
        .collect();
    if parts.is_empty() {
        None
    } else {
        Some(parts.join("\n"))
    }
}

fn extract_text_content(value: &Value) -> Option<String> {
    if let Some(s) = value.as_str() {
        if !s.is_empty() {
            return Some(s.to_string());
        }
    }
    join_content_parts(value)
}

// === chatSessions replay structs ===

#[derive(Deserialize)]
struct ChatSessionsLine {
    #[serde(default)]
    kind: Option<i64>,
    #[serde(default)]
    k: Option<Vec<String>>,
    #[serde(default)]
    v: Value,
    #[serde(flatten)]
    _extra: serde_json::Map<String, Value>,
}

struct ChatState {
    session_id: Option<String>,
    creation_date_ms: Option<i64>,
    custom_title: Option<String>,
    requests: Vec<(i64, Value)>,
}

impl ChatState {
    fn new() -> Self {
        Self {
            session_id: None,
            creation_date_ms: None,
            custom_title: None,
            requests: Vec::new(),
        }
    }
}

fn parse_file(session_ref: &SessionRef) -> Result<NormalizedSession> {
    let lines = reader::read_all_lines(&session_ref.path)?;
    let mut events: Vec<(i64, Value)> = Vec::new();
    let mut errata: Vec<Erratum> = Vec::new();

    for (source_seq, line) in lines {
        match serde_json::from_str::<Value>(&line) {
            Ok(value) => events.push((source_seq, value)),
            Err(e) => {
                errata.push(Erratum {
                    issue_type: "parse_error".to_string(),
                    field_path: Some(format!("line:{source_seq}")),
                    detail: format!("failed to parse JSONL line: {e}"),
                    raw_snippet: Some(line.chars().take(500).collect()),
                });
            }
        }
    }

    if events.is_empty() {
        return Err(OslError::Connector(format!(
            "no parseable events in {}",
            session_ref.path.display()
        )));
    }

    let first = &events[0].1;

    // chatSessions first line carries integer kind 0 + v object.
    if first.get("kind").and_then(|v| v.as_i64()).is_some()
        && first.get("v").map(|v| v.is_object()).unwrap_or(false)
    {
        return parse_chatsessions(events, session_ref, errata);
    }

    // transcripts first line is session.start.
    if first.get("type").and_then(|v| v.as_str()) == Some("session.start") {
        return parse_transcripts(events, session_ref, errata);
    }

    Err(OslError::Connector(format!(
        "unrecognized copilot session format: {}",
        session_ref.path.display()
    )))
}

// === chatSessions replay parser ===

fn parse_chatsessions(
    events: Vec<(i64, Value)>,
    session_ref: &SessionRef,
    mut errata: Vec<Erratum>,
) -> Result<NormalizedSession> {
    let mut state = ChatState::new();

    for (source_seq, value) in events {
        let line: ChatSessionsLine = match serde_json::from_value(value) {
            Ok(line) => line,
            Err(e) => {
                errata.push(Erratum {
                    issue_type: "parse_error".to_string(),
                    field_path: Some(format!("line:{source_seq}")),
                    detail: format!("failed to parse chatSessions line: {e}"),
                    raw_snippet: None,
                });
                continue;
            }
        };

        match line.kind {
            Some(0) => {
                // Seed state from v.
                if let Some(sid) = get_str(&line.v, "sessionId") {
                    state.session_id = Some(sid.to_string());
                }
                state.creation_date_ms = get_i64(&line.v, "creationDate");
                if let Some(title) = get_str(&line.v, "customTitle") {
                    state.custom_title = Some(title.to_string());
                }
                if let Some(requests) = line.v.get("requests").and_then(|v| v.as_array()) {
                    for req in requests {
                        state.requests.push((source_seq, req.clone()));
                    }
                }
            }
            Some(1) => {
                // Property patch.
                if line.k.as_deref() == Some(&["customTitle".to_string()][..]) {
                    if let Some(title) = line.v.as_str() {
                        state.custom_title = Some(title.to_string());
                    }
                } else {
                    errata.push(Erratum {
                        issue_type: "unhandled_property_patch".to_string(),
                        field_path: Some(
                            serde_json::to_string(&line.k.unwrap_or_default()).unwrap_or_default(),
                        ),
                        detail: format!("unhandled property patch at line {source_seq}"),
                        raw_snippet: None,
                    });
                }
            }
            Some(2) => {
                // Array append patch.
                if line.k.as_deref() == Some(&["requests".to_string()][..]) {
                    if let Some(requests) = line.v.as_array() {
                        for req in requests {
                            state.requests.push((source_seq, req.clone()));
                        }
                    }
                } else {
                    errata.push(Erratum {
                        issue_type: "unhandled_array_patch".to_string(),
                        field_path: Some(
                            serde_json::to_string(&line.k.unwrap_or_default()).unwrap_or_default(),
                        ),
                        detail: format!("unhandled array patch at line {source_seq}"),
                        raw_snippet: None,
                    });
                }
            }
            _ => {
                errata.push(Erratum {
                    issue_type: "unknown_patch_kind".to_string(),
                    field_path: Some(format!(
                        "kind:{},k:{}",
                        line.kind.map(|k| k.to_string()).unwrap_or_default(),
                        serde_json::to_string(&line.k.unwrap_or_default()).unwrap_or_default()
                    )),
                    detail: format!("unknown patch kind at line {source_seq}"),
                    raw_snippet: None,
                });
            }
        }
    }

    let native_id = state
        .session_id
        .ok_or_else(|| OslError::Connector("missing copilot sessionId".to_string()))?;
    let sid = session_id(session_ref.source.as_str(), &native_id);

    let started_at = state.creation_date_ms.map(ms_to_iso);
    let ended_at = None;
    let project_root: Option<PathBuf> = None;

    let mut messages: Vec<NormalizedMessage> = Vec::new();
    let mut turn_number: i64 = 0;
    let mut model: Option<String> = None;

    for (req_index, (source_seq, request)) in state.requests.iter().enumerate() {
        if model.is_none() {
            model = get_str(request, "model").map(|s| s.to_string());
        }

        // User message.
        turn_number += 1;
        let user_uuid = message_id(sid, &format!("req{req_index}"));
        let user_content = request
            .get("message")
            .and_then(|m| get_str(m, "text").map(|s| s.to_string()))
            .or_else(|| {
                request
                    .get("message")
                    .and_then(|m| m.get("content"))
                    .and_then(extract_text_content)
            })
            .or_else(|| {
                request
                    .get("message")
                    .and_then(|m| m.get("parts"))
                    .and_then(join_content_parts)
            });

        messages.push(NormalizedMessage {
            uuid: user_uuid,
            role: "user".to_string(),
            content: user_content,
            thinking: None,
            parent_uuid: None,
            source_seq: *source_seq,
            turn_number,
            sequence: 0,
            input_tokens: None,
            output_tokens: None,
        });

        // Assistant message.
        turn_number += 1;
        let assistant_uuid = message_id(sid, &format!("req{req_index}-resp"));
        let assistant_content = request
            .get("responseItems")
            .and_then(|v| v.as_array())
            .map(|items| {
                let parts: Vec<String> = items
                    .iter()
                    .filter_map(|item| {
                        let kind = item.get("kind").and_then(|v| v.as_str()).unwrap_or("text");
                        if kind == "text" || kind == "code" || kind == "markdown" {
                            item.get("text")
                                .and_then(|v| v.as_str())
                                .map(|s| s.to_string())
                        } else {
                            None
                        }
                    })
                    .collect();
                if parts.is_empty() {
                    None
                } else {
                    Some(parts.join("\n"))
                }
            })
            .unwrap_or_else(|| {
                request
                    .get("response")
                    .and_then(|v| v.as_array())
                    .map(|responses| {
                        let parts: Vec<String> = responses
                            .iter()
                            .filter_map(|r| get_str(r, "value").map(|s| s.to_string()))
                            .collect();
                        if parts.is_empty() {
                            None
                        } else {
                            Some(parts.join("\n"))
                        }
                    })
                    .unwrap_or(None)
            });

        messages.push(NormalizedMessage {
            uuid: assistant_uuid,
            role: "assistant".to_string(),
            content: assistant_content,
            thinking: None,
            parent_uuid: Some(user_uuid.to_string()),
            source_seq: *source_seq,
            turn_number,
            sequence: 0,
            input_tokens: None,
            output_tokens: None,
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

    let tool_call_count = 0;
    let error_count = errata.len() as i64;

    Ok(NormalizedSession {
        id: sid,
        source: session_ref.source.clone(),
        native_id,
        title: state.custom_title,
        started_at,
        ended_at,
        model,
        git_branch: None,
        git_sha: None,
        raw_path: session_ref.path.clone(),
        project_root,
        parent_session_id: None,
        input_tokens: 0,
        output_tokens: 0,
        cache_read_tokens: 0,
        cache_write_tokens: 0,
        tool_call_count,
        error_count,
        messages,
        tool_calls: Vec::new(),
        errata,
    })
}

// === transcripts event parser ===

#[derive(Deserialize)]
struct TranscriptLine {
    #[serde(rename = "type")]
    kind: String,
    #[serde(rename = "sessionId", default)]
    session_id: Option<String>,
    #[serde(default)]
    timestamp: Option<i64>,
    #[serde(default)]
    content: Option<Value>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    tool_name: Option<String>,
    #[serde(default)]
    tool_call_id: Option<String>,
    #[serde(default)]
    result: Option<Value>,
    #[serde(flatten)]
    _extra: serde_json::Map<String, Value>,
}

fn parse_transcripts(
    events: Vec<(i64, Value)>,
    session_ref: &SessionRef,
    mut errata: Vec<Erratum>,
) -> Result<NormalizedSession> {
    // First pass: parse and find native_id from session.start so UUIDs are stable.
    let mut parsed: Vec<(i64, TranscriptLine)> = Vec::with_capacity(events.len());
    let mut native_id: Option<String> = None;
    for (source_seq, value) in events {
        match serde_json::from_value::<TranscriptLine>(value) {
            Ok(event) => {
                if event.kind == "session.start" && native_id.is_none() {
                    native_id = event.session_id.clone();
                }
                parsed.push((source_seq, event));
            }
            Err(e) => {
                errata.push(Erratum {
                    issue_type: "parse_error".to_string(),
                    field_path: Some(format!("line:{source_seq}")),
                    detail: format!("failed to parse transcript event: {e}"),
                    raw_snippet: None,
                });
            }
        }
    }

    let native_id = native_id.ok_or_else(|| {
        OslError::Connector(format!(
            "missing session.start in {}",
            session_ref.path.display()
        ))
    })?;
    let sid = session_id(session_ref.source.as_str(), &native_id);

    let mut started_at_ms: Option<i64> = None;
    let mut ended_at_ms: Option<i64> = None;
    let mut model: Option<String> = None;

    let mut messages: Vec<NormalizedMessage> = Vec::new();
    let mut pending_tool_calls: HashMap<String, NormalizedToolCall> = HashMap::new();
    let mut turn_number: i64 = 0;

    let mut current_assistant_msg_uuid: Option<Uuid> = None;
    let mut current_user_msg_uuid: Option<Uuid> = None;
    let mut last_message_uuid: Option<Uuid> = None;

    for (source_seq, event) in parsed {
        if let Some(ts) = event.timestamp {
            if started_at_ms.is_none() {
                started_at_ms = Some(ts);
            }
            ended_at_ms = Some(ts);
        }

        match event.kind.as_str() {
            "session.start" => {}
            "user.message" => {
                turn_number += 1;
                let content = event.content.as_ref().and_then(extract_text_content);
                let msg_uuid = message_id(sid, &format!("u{turn_number}"));
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
                current_user_msg_uuid = Some(msg_uuid);
                last_message_uuid = Some(msg_uuid);
            }
            "assistant.message" => {
                turn_number += 1;
                let content = event.content.as_ref().and_then(extract_text_content);
                if let Some(m) = event.model.as_deref() {
                    if !m.is_empty() {
                        model = Some(m.to_string());
                    }
                }
                let msg_uuid = message_id(sid, &format!("a{turn_number}"));
                messages.push(NormalizedMessage {
                    uuid: msg_uuid,
                    role: "assistant".to_string(),
                    content,
                    thinking: None,
                    parent_uuid: None,
                    source_seq,
                    turn_number,
                    sequence: 0,
                    input_tokens: None,
                    output_tokens: None,
                });
                current_assistant_msg_uuid = Some(msg_uuid);
                last_message_uuid = Some(msg_uuid);
            }
            "tool.execution_start" => {
                let ts = event.timestamp;
                let call_id = event
                    .tool_call_id
                    .clone()
                    .filter(|s| !s.is_empty())
                    .unwrap_or_else(|| format!("{source_seq}"));
                let (request_uuid, erratum) = if let Some(uuid) = current_assistant_msg_uuid {
                    (uuid, None)
                } else if let Some(uuid) = current_user_msg_uuid {
                    (
                        uuid,
                        Some(Erratum {
                            issue_type: "tool_before_assistant".to_string(),
                            field_path: Some(format!("line:{source_seq}")),
                            detail: "tool.execution_start before any assistant.message".to_string(),
                            raw_snippet: None,
                        }),
                    )
                } else {
                    let synthetic = message_id(sid, &format!("tool-{source_seq}"));
                    (
                        synthetic,
                        Some(Erratum {
                            issue_type: "tool_before_any_message".to_string(),
                            field_path: Some(format!("line:{source_seq}")),
                            detail: "tool.execution_start before any message".to_string(),
                            raw_snippet: None,
                        }),
                    )
                };
                if let Some(e) = erratum {
                    errata.push(e);
                }
                let tc_uuid = tool_call_id(sid, request_uuid, &call_id);
                pending_tool_calls.insert(
                    call_id.clone(),
                    NormalizedToolCall {
                        uuid: tc_uuid,
                        call_id,
                        tool_name: event
                            .tool_name
                            .clone()
                            .unwrap_or_else(|| "unknown".to_string()),
                        tool_input: None,
                        tool_output: None,
                        tool_output_raw: None,
                        is_error: None,
                        started_at: ts.map(ms_to_iso),
                        completed_at: None,
                        request_message_uuid: request_uuid,
                        response_message_uuid: None,
                    },
                );
            }
            "tool.execution_complete" => {
                let ts = event.timestamp;
                let call_id = event
                    .tool_call_id
                    .clone()
                    .unwrap_or_else(|| format!("{source_seq}"));

                if let Some(tc) = pending_tool_calls.get_mut(&call_id) {
                    let (tool_output, tool_output_raw) = event
                        .result
                        .as_ref()
                        .map(normalize_result)
                        .unwrap_or((None, None));
                    tc.tool_output = tool_output;
                    tc.tool_output_raw = tool_output_raw;
                    tc.completed_at = ts.map(ms_to_iso);

                    if let Some(resp) = current_assistant_msg_uuid {
                        tc.response_message_uuid = Some(resp);
                    } else if let Some(resp) = current_user_msg_uuid {
                        tc.response_message_uuid = Some(resp);
                        errata.push(Erratum {
                            issue_type: "tool_response_no_assistant_msg".to_string(),
                            field_path: Some(format!("line:{source_seq}")),
                            detail: "tool.execution_complete has no preceding assistant message"
                                .to_string(),
                            raw_snippet: None,
                        });
                    } else {
                        tc.response_message_uuid = last_message_uuid;
                    }
                } else {
                    errata.push(Erratum {
                        issue_type: "orphan_tool_complete".to_string(),
                        field_path: Some(format!("line:{source_seq}")),
                        detail: format!(
                            "tool.execution_complete for unknown call_id {call_id} at line {source_seq}"
                        ),
                        raw_snippet: None,
                    });
                    if let Some(request_uuid) = current_assistant_msg_uuid.or(current_user_msg_uuid)
                    {
                        let tc_uuid = tool_call_id(sid, request_uuid, &call_id);
                        let (tool_output, tool_output_raw) = event
                            .result
                            .as_ref()
                            .map(normalize_result)
                            .unwrap_or((None, None));
                        pending_tool_calls.insert(
                            call_id.clone(),
                            NormalizedToolCall {
                                uuid: tc_uuid,
                                call_id,
                                tool_name: event
                                    .tool_name
                                    .clone()
                                    .unwrap_or_else(|| "unknown".to_string()),
                                tool_input: None,
                                tool_output,
                                tool_output_raw,
                                is_error: None,
                                started_at: ts.map(ms_to_iso),
                                completed_at: ts.map(ms_to_iso),
                                request_message_uuid: request_uuid,
                                response_message_uuid: current_assistant_msg_uuid
                                    .or(current_user_msg_uuid),
                            },
                        );
                    }
                }
            }
            other => {
                errata.push(Erratum {
                    issue_type: "unknown_transcript_event".to_string(),
                    field_path: Some(format!("type:{other}")),
                    detail: format!("unknown transcript event '{other}' at line {source_seq}"),
                    raw_snippet: None,
                });
            }
        }
    }

    // Drain pending tool calls. Dangling starts get tied to the last message we saw.
    let mut tool_calls: Vec<NormalizedToolCall> = Vec::new();
    for (call_id, mut tc) in pending_tool_calls {
        if tc.response_message_uuid.is_none() {
            tc.response_message_uuid = current_assistant_msg_uuid
                .or(current_user_msg_uuid)
                .or(last_message_uuid);
            errata.push(Erratum {
                issue_type: "dangling_tool_start".to_string(),
                field_path: Some(format!("call_id:{call_id}")),
                detail: format!("tool.execution_start for {call_id} never completed"),
                raw_snippet: None,
            });
        }
        tool_calls.push(tc);
    }

    let started_at = started_at_ms.map(ms_to_iso);
    let ended_at = ended_at_ms.map(ms_to_iso);

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
        native_id,
        title: None,
        started_at,
        ended_at,
        model,
        git_branch: None,
        git_sha: None,
        raw_path: session_ref.path.clone(),
        project_root: None,
        parent_session_id: None,
        input_tokens: 0,
        output_tokens: 0,
        cache_read_tokens: 0,
        cache_write_tokens: 0,
        tool_call_count,
        error_count,
        messages,
        tool_calls,
        errata,
    })
}

fn normalize_result(result: &Value) -> (Option<String>, Option<String>) {
    if let Some(s) = result.as_str() {
        (Some(s.to_string()), None)
    } else {
        (None, Some(result.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ms_to_iso_reused_from_opencode() {
        // 1780251875000 ms ≈ 2026-06-01T...
        let iso = ms_to_iso(1780251875000);
        assert!(iso.starts_with("2026-"));
        assert!(iso.ends_with("Z"));
    }
}
