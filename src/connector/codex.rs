use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_json::Value;
use uuid::Uuid;

use crate::connector::{reader, walk, Connector};
use crate::error::{OslError, Result};
use crate::ids::{message_id, session_id, tool_call_id};
use crate::model::{Erratum, NormalizedMessage, NormalizedSession, NormalizedToolCall, SessionRef};

/// Codex CLI rollout-format connector.
///
/// Parses the on-disk JSONL session files produced by the Codex CLI
/// (`~/.codex/sessions/YYYY/MM/DD/rollout-*.jsonl`). Every line is a JSON
/// object with exactly `timestamp`, `type`, and `payload` fields; subtyping
/// is deferred to runtime matches on `payload.type` so the parser stays
/// tolerant of format drift between Codex releases.
pub struct CodexCliConnector;

impl Connector for CodexCliConnector {
    fn name(&self) -> &'static str {
        "codex"
    }

    fn discover(&self, directory: &Path) -> Result<Vec<SessionRef>> {
        walk::discover_jsonl(directory, directory, "codex", &|p| peek_codex_id(p))
    }

    fn parse(&self, session_ref: &SessionRef) -> Result<NormalizedSession> {
        parse_file(session_ref)
    }
}

/// Peek at the first non-empty line of a JSONL file and, if it is a Codex
/// `session_meta` event, return the session id stored in `payload.id`.
///
/// This is `pub(crate)` so `src/ingest.rs` can use the same detection logic
/// for single-file routing without duplicating the rollout-format heuristic.
pub(crate) fn peek_codex_id(path: &Path) -> Result<Option<String>> {
    let first_line = match reader::read_first_line(path)? {
        Some(l) => l,
        None => return Ok(None),
    };
    match serde_json::from_str::<CodexLine>(&first_line) {
        Ok(line) if line.kind == "session_meta" => {
            if let Some(id) = line.payload.get("id").and_then(|v| v.as_str()) {
                if !id.is_empty() {
                    return Ok(Some(id.to_string()));
                }
            }
            Ok(None)
        }
        Ok(_) => Ok(None),
        Err(_) => Ok(None),
    }
}

#[derive(Deserialize, Debug)]
#[allow(dead_code)]
struct CodexLine {
    timestamp: Option<String>,
    #[serde(rename = "type")]
    kind: String,
    payload: serde_json::Value,
}

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

fn token_count_value(payload: &Value, top_key: &str, nested: &[&str]) -> Option<i64> {
    get_i64(payload, top_key).or_else(|| {
        let mut current = payload.get("info")?;
        for key in nested {
            current = current.get(*key)?;
        }
        current.as_i64()
    })
}

fn parse_file(session_ref: &SessionRef) -> Result<NormalizedSession> {
    let lines = reader::read_all_lines(&session_ref.path)?;
    let mut events: Vec<(i64, CodexLine)> = Vec::new();
    let mut errata: Vec<Erratum> = Vec::new();

    for (source_seq, line) in lines {
        match serde_json::from_str::<CodexLine>(&line) {
            Ok(event) => events.push((source_seq, event)),
            Err(e) => {
                errata.push(Erratum {
                    issue_type: "parse_error".to_string(),
                    field_path: Some(format!("line:{source_seq}")),
                    detail: format!("failed to parse event: {e}"),
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

    // First pass: extract session-level metadata from the first session_meta event.
    let mut native_id: Option<String> = None;
    let mut cwd: Option<String> = None;
    let mut git_branch: Option<String> = None;
    let mut git_sha: Option<String> = None;
    let mut parent_session_id: Option<Uuid> = None;
    let mut dropped_metadata = false;

    for (_, event) in &events {
        if event.kind == "session_meta" {
            let payload = &event.payload;
            if let Some(id) = get_str(payload, "id") {
                native_id = Some(id.to_string());
            }
            if let Some(c) = get_str(payload, "cwd") {
                cwd = Some(c.to_string());
            }
            if let Some(git) = payload.get("git") {
                if let Some(branch) = get_str(git, "branch") {
                    git_branch = Some(branch.to_string());
                }
                if let Some(sha) = get_str(git, "sha") {
                    git_sha = Some(sha.to_string());
                }
            }
            if let Some(parent) =
                get_str(payload, "forked_from_id").or_else(|| get_str(payload, "parent_thread_id"))
            {
                parent_session_id = Some(session_id("codex", parent));
            }
            if get_str(payload, "agent_nickname").is_some()
                || get_str(payload, "agent_role").is_some()
                || get_str(payload, "agent_path").is_some()
                || get_str(payload, "model_provider").is_some()
                || get_str(payload, "originator").is_some()
                || get_str(payload, "cli_version").is_some()
            {
                dropped_metadata = true;
            }
            break;
        }
    }

    let native_id = native_id.ok_or_else(|| {
        OslError::Connector(format!(
            "missing session_meta.id in {}",
            session_ref.path.display()
        ))
    })?;

    let sid = session_id(session_ref.source.as_str(), &native_id);
    let started_at = events[0].1.timestamp.clone();
    let ended_at = events.last().and_then(|e| e.1.timestamp.clone());
    let project_root = cwd.as_ref().and_then(|c| {
        if c.is_empty() {
            None
        } else {
            Some(PathBuf::from(c))
        }
    });

    let mut messages: Vec<NormalizedMessage> = Vec::new();
    let mut tool_calls: Vec<NormalizedToolCall> = Vec::new();
    let mut pending_tool_calls: HashMap<String, NormalizedToolCall> = HashMap::new();
    // Begin-event payloads keyed by call_id so end-event reconstruction can
    // recover arguments / query that were only present on the begin line.
    let mut begin_payloads: HashMap<String, Value> = HashMap::new();
    let mut turn_number: i64 = 0;

    let mut last_input_total: i64 = -1;
    let mut last_output_total: i64 = -1;
    let mut last_cached_total: i64 = -1;

    let mut last_assistant_msg_uuid: Option<Uuid> = None;

    let mut title: Option<String> = None;
    let mut model: Option<String> = None;

    let mut seen_skips: HashSet<String> = HashSet::new();
    let quiet_event_subtypes: HashSet<&str> = [
        "turn_started",
        "turn_complete",
        "task_started",
        "task_complete",
        "model_reroute",
        "exec_approval_request",
    ]
    .into_iter()
    .collect();

    if dropped_metadata {
        errata.push(Erratum {
            issue_type: "dropped_metadata".to_string(),
            field_path: Some("session_meta.*".to_string()),
            detail: "session_meta contains fields not stored by this connector".to_string(),
            raw_snippet: None,
        });
    }

    for (source_seq, event) in events {
        let payload = &event.payload;

        if event.kind == "turn_context" {
            if let Some(m) = get_str(payload, "model") {
                if model.is_none() && !m.is_empty() {
                    model = Some(m.to_string());
                }
            }
            if title.is_none() {
                if let Some(summary) = get_str(payload, "summary") {
                    if !summary.is_empty() {
                        title = Some(summary.to_string());
                    }
                }
            }
            continue;
        }

        match event.kind.as_str() {
            "session_meta" => {
                // Session-level metadata was consumed in the first pass.
                continue;
            }
            "turn_context" => {
                // Already handled above.
                continue;
            }
            "compacted" => {
                if title.is_none() {
                    if let Some(summary) = get_str(payload, "summary") {
                        if !summary.is_empty() {
                            title = Some(summary.to_string());
                        }
                    }
                }
                errata.push(Erratum {
                    issue_type: "history_compacted".to_string(),
                    field_path: Some("type:compacted".to_string()),
                    detail: "session contains compacted history not fully replayed".to_string(),
                    raw_snippet: None,
                });
                continue;
            }
            "world_state" | "inter_agent_communication" | "ghost_snapshot" => {
                let key = format!("type:{}", event.kind);
                if seen_skips.insert(key.clone()) {
                    errata.push(Erratum {
                        issue_type: "skipped_event".to_string(),
                        field_path: Some(key),
                        detail: format!("skipping {} event", event.kind),
                        raw_snippet: None,
                    });
                }
                continue;
            }
            "response_item" => {
                let subtype = get_str(payload, "type").unwrap_or("");
                match subtype {
                    "message" => {
                        turn_number += 1;
                        let msg_uuid = message_id(sid, &format!("seq-{source_seq}"));
                        let content = payload.get("content").and_then(join_content_parts);
                        let role = get_str(payload, "role").unwrap_or("assistant");
                        messages.push(NormalizedMessage {
                            uuid: msg_uuid,
                            role: role.to_string(),
                            content,
                            thinking: None,
                            parent_uuid: None,
                            source_seq,
                            turn_number,
                            sequence: 0,
                            input_tokens: None,
                            output_tokens: None,
                        });
                        if role == "assistant" {
                            last_assistant_msg_uuid = Some(msg_uuid);
                        }
                    }
                    "reasoning" => {
                        turn_number += 1;
                        let msg_uuid = message_id(sid, &format!("seq-{source_seq}"));
                        let thinking = get_str(payload, "text").map(|s| s.to_string());
                        messages.push(NormalizedMessage {
                            uuid: msg_uuid,
                            role: "assistant".to_string(),
                            content: None,
                            thinking,
                            parent_uuid: None,
                            source_seq,
                            turn_number,
                            sequence: 0,
                            input_tokens: None,
                            output_tokens: None,
                        });
                        last_assistant_msg_uuid = Some(msg_uuid);
                    }
                    "function_call" => {
                        let call_id = get_str(payload, "call_id").unwrap_or("").to_string();
                        if call_id.is_empty() {
                            continue;
                        }
                        let (request_uuid, orphan) = if let Some(uuid) = last_assistant_msg_uuid {
                            (uuid, false)
                        } else {
                            let synthetic = message_id(sid, &format!("seq-{source_seq}"));
                            errata.push(Erratum {
                                issue_type: "orphan_tool_call".to_string(),
                                field_path: Some(format!("line:{source_seq}")),
                                detail: "function_call has no preceding assistant message"
                                    .to_string(),
                                raw_snippet: None,
                            });
                            (synthetic, true)
                        };
                        let tool_input = payload
                            .get("arguments")
                            .map(|v| v.to_string())
                            .or_else(|| payload.get("input").map(|v| v.to_string()));
                        let tc_uuid = tool_call_id(sid, request_uuid, &call_id);
                        pending_tool_calls.insert(
                            call_id.clone(),
                            NormalizedToolCall {
                                uuid: tc_uuid,
                                call_id,
                                tool_name: get_str(payload, "name")
                                    .unwrap_or("unknown")
                                    .to_string(),
                                tool_input,
                                tool_output: None,
                                tool_output_raw: None,
                                is_error: None,
                                started_at: event.timestamp.clone(),
                                completed_at: None,
                                request_message_uuid: request_uuid,
                                response_message_uuid: if orphan {
                                    Some(request_uuid)
                                } else {
                                    None
                                },
                            },
                        );
                    }
                    "function_call_output" => {
                        let call_id = get_str(payload, "call_id").unwrap_or("").to_string();
                        if let Some(tc) = pending_tool_calls.get_mut(&call_id) {
                            tc.tool_output = payload.get("output").map(|v| {
                                v.as_str()
                                    .map(|s| s.to_string())
                                    .unwrap_or_else(|| v.to_string())
                            });
                            tc.completed_at = event.timestamp.clone();
                            tc.response_message_uuid = last_assistant_msg_uuid;
                        } else {
                            errata.push(Erratum {
                                issue_type: "unpaired_tool_output".to_string(),
                                field_path: Some(format!("line:{source_seq}")),
                                detail: format!(
                                    "function_call_output for unknown call_id {call_id}"
                                ),
                                raw_snippet: None,
                            });
                        }
                    }
                    "web_search_call" => {
                        let call_id = get_str(payload, "call_id").unwrap_or("").to_string();
                        if call_id.is_empty() {
                            continue;
                        }
                        let (request_uuid, orphan) = if let Some(uuid) = last_assistant_msg_uuid {
                            (uuid, false)
                        } else {
                            let synthetic = message_id(sid, &format!("seq-{source_seq}"));
                            errata.push(Erratum {
                                issue_type: "orphan_tool_call".to_string(),
                                field_path: Some(format!("line:{source_seq}")),
                                detail: "web_search_call has no preceding assistant message"
                                    .to_string(),
                                raw_snippet: None,
                            });
                            (synthetic, true)
                        };
                        let tc_uuid = tool_call_id(sid, request_uuid, &call_id);
                        pending_tool_calls.insert(
                            call_id.clone(),
                            NormalizedToolCall {
                                uuid: tc_uuid,
                                call_id,
                                tool_name: "web_search".to_string(),
                                tool_input: get_str(payload, "query").map(|s| s.to_string()),
                                tool_output: None,
                                tool_output_raw: None,
                                is_error: None,
                                started_at: event.timestamp.clone(),
                                completed_at: None,
                                request_message_uuid: request_uuid,
                                response_message_uuid: if orphan {
                                    Some(request_uuid)
                                } else {
                                    None
                                },
                            },
                        );
                    }
                    _ => {
                        errata.push(Erratum {
                            issue_type: "unknown_response_subtype".to_string(),
                            field_path: Some(format!("response_item:{subtype}")),
                            detail: format!(
                                "unhandled response_item subtype '{subtype}' at line {source_seq}"
                            ),
                            raw_snippet: None,
                        });
                    }
                }
            }
            "event_msg" => {
                let subtype = get_str(payload, "type").unwrap_or("");
                match subtype {
                    "user_message" => {
                        turn_number += 1;
                        let msg_uuid = message_id(sid, &format!("seq-{source_seq}"));
                        let content = get_str(payload, "message").map(|s| s.to_string());
                        if title.is_none() {
                            if let Some(text) = content.as_deref() {
                                let truncated: String = text.chars().take(80).collect();
                                if !truncated.is_empty() {
                                    title = Some(truncated);
                                }
                            }
                        }
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
                    "agent_message" => {
                        turn_number += 1;
                        let msg_uuid = message_id(sid, &format!("seq-{source_seq}"));
                        let content = get_str(payload, "message").map(|s| s.to_string());
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
                        last_assistant_msg_uuid = Some(msg_uuid);
                    }
                    "token_count" => {
                        let input = token_count_value(
                            payload,
                            "input",
                            &["total_token_usage", "input_tokens"],
                        )
                        .unwrap_or(0);
                        let output = token_count_value(
                            payload,
                            "output",
                            &["total_token_usage", "output_tokens"],
                        )
                        .unwrap_or(0);
                        let cached_input = token_count_value(
                            payload,
                            "cached_input",
                            &["total_token_usage", "cached_input_tokens"],
                        )
                        .unwrap_or(0);
                        let reasoning_output = token_count_value(
                            payload,
                            "reasoning_output",
                            &["total_token_usage", "reasoning_output_tokens"],
                        )
                        .unwrap_or(0);
                        // Codex token_count events report cumulative totals. We keep the
                        // maximum observed value for each channel. Reasoning tokens are
                        // folded into the output channel so the total output column
                        // reflects all generated tokens (sum-then-max, not max-then-fold).
                        let effective_output = output + reasoning_output;
                        if input > last_input_total {
                            last_input_total = input;
                        }
                        if effective_output > last_output_total {
                            last_output_total = effective_output;
                        }
                        if cached_input > last_cached_total {
                            last_cached_total = cached_input;
                        }
                    }
                    "exec_command_begin" | "mcp_tool_call_begin" | "web_search_begin" => {
                        if let Some(call_id) = get_str(payload, "call_id") {
                            begin_payloads.insert(call_id.to_string(), payload.clone());
                            if let Some(tc) = pending_tool_calls.get_mut(call_id) {
                                if tc.started_at.is_none() {
                                    tc.started_at = event.timestamp.clone();
                                }
                            }
                        }
                    }
                    "exec_command_end" | "mcp_tool_call_end" => {
                        let call_id = get_str(payload, "call_id").unwrap_or("").to_string();
                        let exit_code = get_i64(payload, "exit_code");
                        let is_error = exit_code.map(|code| code != 0);
                        let output_text = payload
                            .get("aggregated_output")
                            .or_else(|| payload.get("result"))
                            .and_then(|v| {
                                v.as_str()
                                    .map(|s| s.to_string())
                                    .or_else(|| Some(v.to_string()))
                            });

                        if let Some(tc) = pending_tool_calls.get_mut(&call_id) {
                            tc.completed_at = event.timestamp.clone();
                            if is_error.is_some() {
                                tc.is_error = is_error;
                            }
                            if tc.tool_output.is_none() {
                                tc.tool_output = output_text.clone();
                            }
                        } else if output_text.is_some() || is_error.is_some() {
                            // Tool appeared only via event_msg begin/end with no
                            // preceding response_item/function_call envelope.
                            let (request_uuid, response_uuid) = if let Some(uuid) =
                                last_assistant_msg_uuid
                            {
                                (uuid, Some(uuid))
                            } else {
                                let synthetic = message_id(sid, &format!("seq-{source_seq}"));
                                errata.push(Erratum {
                                    issue_type: "orphan_tool_call".to_string(),
                                    field_path: Some(format!("line:{source_seq}")),
                                    detail: format!("{subtype} has no preceding assistant message"),
                                    raw_snippet: None,
                                });
                                (synthetic, Some(synthetic))
                            };
                            let tool_name = if subtype == "exec_command_end" {
                                "exec_command"
                            } else {
                                get_str(payload, "tool").unwrap_or("mcp_tool_call")
                            };
                            let tool_input = begin_payloads
                                .get(&call_id)
                                .and_then(|begin| {
                                    begin.get("arguments").map(|v| v.to_string()).or_else(|| {
                                        begin
                                            .get("query")
                                            .and_then(|q| q.as_str().map(|s| s.to_string()))
                                    })
                                })
                                .or_else(|| payload.get("arguments").map(|v| v.to_string()));
                            let effective_call_id = if call_id.is_empty() {
                                format!("synthetic-{source_seq}")
                            } else {
                                call_id.clone()
                            };
                            let tc_uuid = tool_call_id(sid, request_uuid, &effective_call_id);
                            pending_tool_calls.insert(
                                effective_call_id.clone(),
                                NormalizedToolCall {
                                    uuid: tc_uuid,
                                    call_id: effective_call_id,
                                    tool_name: tool_name.to_string(),
                                    tool_input,
                                    tool_output: output_text,
                                    tool_output_raw: None,
                                    is_error,
                                    started_at: event.timestamp.clone(),
                                    completed_at: event.timestamp.clone(),
                                    request_message_uuid: request_uuid,
                                    response_message_uuid: response_uuid,
                                },
                            );
                            errata.push(Erratum {
                                issue_type: "tool_call_from_event_msg".to_string(),
                                field_path: Some(format!("line:{source_seq}")),
                                detail: format!("reconstructed tool call from {subtype}"),
                                raw_snippet: None,
                            });
                        }
                    }
                    "error" | "warning" | "stream_error" => {
                        let detail = get_str(payload, "message")
                            .map(|s| s.to_string())
                            .unwrap_or_else(|| serde_json::to_string(payload).unwrap_or_default());
                        errata.push(Erratum {
                            issue_type: subtype.to_string(),
                            field_path: Some(format!("event_msg:{subtype}")),
                            detail,
                            raw_snippet: None,
                        });
                    }
                    _ => {
                        if quiet_event_subtypes.contains(subtype) {
                            continue;
                        }
                        let key = format!("event_msg:{subtype}");
                        if seen_skips.insert(key.clone()) {
                            errata.push(Erratum {
                                issue_type: "skipped_event".to_string(),
                                field_path: Some(key),
                                detail: format!("skipping event_msg subtype '{subtype}'"),
                                raw_snippet: None,
                            });
                        }
                    }
                }
            }
            other => {
                let key = format!("type:{other}");
                if seen_skips.insert(key.clone()) {
                    errata.push(Erratum {
                        issue_type: "unknown_event_type".to_string(),
                        field_path: Some(key),
                        detail: format!("unhandled event type '{other}' at line {source_seq}"),
                        raw_snippet: None,
                    });
                }
            }
        }
    }

    for (_, tc) in pending_tool_calls {
        tool_calls.push(tc);
    }

    let input_tokens = if last_input_total < 0 {
        0
    } else {
        last_input_total
    };
    let output_tokens = if last_output_total < 0 {
        0
    } else {
        last_output_total
    };
    let cache_read_tokens = if last_cached_total < 0 {
        0
    } else {
        last_cached_total
    };

    let tool_call_count = tool_calls.len() as i64;
    let error_count = errata.len() as i64;

    Ok(NormalizedSession {
        id: sid,
        source: session_ref.source.clone(),
        native_id,
        title,
        started_at,
        ended_at,
        model,
        git_branch,
        git_sha,
        raw_path: session_ref.path.clone(),
        project_root,
        parent_session_id,
        input_tokens,
        output_tokens,
        cache_read_tokens,
        cache_write_tokens: 0,
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

    fn fixture_path(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("fixtures")
            .join("codex")
            .join(name)
    }

    fn parse_fixture(name: &str) -> NormalizedSession {
        let path = fixture_path(name);
        let connector = CodexCliConnector;
        let native_id = peek_codex_id(&path).unwrap().unwrap();
        let session_ref = SessionRef {
            source: "codex".to_string(),
            native_id,
            path,
            project_path: None,
        };
        connector.parse(&session_ref).unwrap()
    }

    #[test]
    fn minimal_fixture() {
        let session = parse_fixture("minimal.jsonl");
        assert_eq!(session.messages.len(), 2);
        assert!(session.tool_calls.is_empty());
        assert_eq!(session.input_tokens, 150);
        assert_eq!(session.output_tokens, 80);
        assert_eq!(session.cache_read_tokens, 70);
    }

    #[test]
    fn with_tool_call_fixture() {
        let session = parse_fixture("with_tool_call.jsonl");
        assert_eq!(session.tool_calls.len(), 1);
        let tc = &session.tool_calls[0];
        assert_eq!(tc.tool_name, "exec_command");
        assert!(tc.response_message_uuid.is_some());
        assert_eq!(tc.tool_output.as_deref(), Some("mock output"));
    }

    #[test]
    fn with_reasoning_fixture() {
        let session = parse_fixture("with_reasoning.jsonl");
        let assistant = session
            .messages
            .iter()
            .find(|m| m.role == "assistant" && m.thinking.is_some())
            .unwrap();
        assert!(assistant
            .thinking
            .as_deref()
            .unwrap()
            .contains("audit existing tables"));
    }

    #[test]
    fn with_token_count_fixture() {
        let session = parse_fixture("with_token_count.jsonl");
        assert_eq!(session.input_tokens, 150);
        // output (80) + reasoning_output (10) is the max effective output observed.
        assert_eq!(session.output_tokens, 90);
        assert_eq!(session.cache_read_tokens, 70);
    }

    #[test]
    fn unknown_events_fixture() {
        let session = parse_fixture("unknown_events.jsonl");
        assert!(!session.messages.is_empty());
        assert!(!session.errata.is_empty());
        assert!(!session
            .errata
            .iter()
            .any(|e| e.issue_type == "skipped_event"
                && e.field_path.as_deref() == Some("event_msg:turn_started")));
        assert!(!session
            .errata
            .iter()
            .any(|e| e.issue_type == "skipped_event"
                && e.field_path.as_deref() == Some("event_msg:turn_complete")));
    }
}
