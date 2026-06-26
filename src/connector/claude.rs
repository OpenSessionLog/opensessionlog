use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use crate::connector::Connector;
use crate::error::{OslError, Result};
use crate::ids::{message_id, session_id, tool_call_id};
use crate::model::{Erratum, NormalizedMessage, NormalizedSession, NormalizedToolCall, SessionRef};
use serde::Deserialize;

pub struct ClaudeCodeConnector;

impl Connector for ClaudeCodeConnector {
    fn name(&self) -> &'static str {
        "claude"
    }

    fn discover(&self, directory: &Path) -> Result<Vec<SessionRef>> {
        let mut refs = Vec::new();
        discover_recursive(directory, directory, &mut refs)?;
        Ok(refs)
    }

    fn parse(&self, session_ref: &SessionRef) -> Result<NormalizedSession> {
        parse_file(session_ref)
    }
}

fn discover_recursive(root: &Path, current: &Path, out: &mut Vec<SessionRef>) -> Result<()> {
    if let Ok(entries) = std::fs::read_dir(current) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                // Phase 1: ignore subagents/ subdirectories.
                if path.file_name().map(|n| n == "subagents").unwrap_or(false) {
                    continue;
                }
                discover_recursive(root, &path, out)?;
            } else if path.extension().map(|e| e == "jsonl").unwrap_or(false) {
                if let Some(native_id) = peek_session_id(&path)? {
                    out.push(SessionRef {
                        source: "claude".to_string(),
                        native_id,
                        path,
                        project_path: Some(root.to_path_buf()),
                    });
                }
            }
        }
    }
    Ok(())
}

fn peek_session_id(path: &Path) -> Result<Option<String>> {
    let file = File::open(path)?;
    let mut reader = BufReader::new(file);
    let mut first_line = String::new();
    if reader.read_line(&mut first_line)? == 0 {
        return Ok(None);
    }
    match serde_json::from_str::<ClaudeEvent>(&first_line) {
        Ok(event) => Ok(Some(event.session_id)),
        Err(_) => Ok(None),
    }
}

#[derive(Deserialize, Debug)]
#[allow(dead_code)]
struct ClaudeEvent {
    uuid: String,
    #[serde(rename = "parentUuid", default)]
    parent_uuid: Option<String>,
    #[serde(rename = "type")]
    kind: String,
    timestamp: String,
    #[serde(rename = "sessionId")]
    session_id: String,
    version: String,
    cwd: String,
    #[serde(rename = "isSidechain", default)]
    is_sidechain: bool,
    #[serde(rename = "gitBranch", default)]
    git_branch: Option<String>,
    message: Option<ClaudeMessage>,
    #[serde(flatten)]
    extra: serde_json::Map<String, serde_json::Value>,
}

#[derive(Deserialize, Debug)]
#[allow(dead_code)]
struct ClaudeMessage {
    id: Option<String>,
    model: Option<String>,
    content: Option<ClaudeContent>,
    usage: Option<ClaudeUsage>,
}

#[derive(Deserialize, Debug)]
#[serde(untagged)]
enum ClaudeContent {
    Text(String),
    Blocks(Vec<ClaudeBlock>),
}

#[derive(Deserialize, Debug)]
#[serde(tag = "type", rename_all = "snake_case")]
#[allow(dead_code)]
enum ClaudeBlock {
    Text {
        text: String,
    },
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },
    ToolResult {
        #[serde(rename = "tool_use_id")]
        tool_use_id: String,
        content: ToolResultContent,
        #[serde(default)]
        is_error: Option<bool>,
    },
    Thinking {
        thinking: String,
        #[serde(default)]
        signature: Option<String>,
    },
    Image {
        source: serde_json::Value,
    },
    RedactedThinking,
    #[serde(other)]
    Unknown,
}

#[derive(Deserialize, Debug)]
struct TextBlock {
    text: String,
}

#[derive(Deserialize, Debug)]
#[serde(untagged)]
enum ToolResultContent {
    Text(String),
    Array(Vec<TextBlock>),
}

#[derive(Deserialize, Debug, Default)]
struct ClaudeUsage {
    #[serde(default)]
    input_tokens: i64,
    #[serde(default)]
    output_tokens: i64,
    #[serde(default)]
    cache_creation_input_tokens: i64,
    #[serde(default)]
    cache_read_input_tokens: i64,
}

fn parse_file(session_ref: &SessionRef) -> Result<NormalizedSession> {
    let file = File::open(&session_ref.path)?;
    let reader = BufReader::new(file);
    let mut events: Vec<(i64, ClaudeEvent)> = Vec::new();
    let mut errata: Vec<Erratum> = Vec::new();

    for (source_seq, line) in reader.lines().enumerate() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let source_seq = (source_seq + 1) as i64;
        match serde_json::from_str::<ClaudeEvent>(&line) {
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

    let native_id = events[0].1.session_id.clone();
    let started_at = events[0].1.timestamp.clone();
    let ended_at = events.last().map(|e| e.1.timestamp.clone());
    let project_root = Some(PathBuf::from(&events[0].1.cwd));
    let mut git_branch: Option<String> = None;
    let git_sha: Option<String> = None;
    let mut title: Option<String> = None;
    let mut model: Option<String> = None;

    let sid = session_id(session_ref.source.as_str(), &native_id);

    let mut messages: Vec<NormalizedMessage> = Vec::new();
    let mut tool_calls: Vec<NormalizedToolCall> = Vec::new();
    let mut pending_tool_calls: HashMap<String, NormalizedToolCall> = HashMap::new();
    let mut turn_number: i64 = 0;

    let mut input_tokens: i64 = 0;
    let mut output_tokens: i64 = 0;
    let mut cache_read_tokens: i64 = 0;
    let mut cache_write_tokens: i64 = 0;

    for (source_seq, event) in events {
        if let Some(branch) = event.git_branch.as_ref() {
            if !branch.is_empty() {
                git_branch = Some(branch.clone());
            }
        }

        match event.kind.as_str() {
            "custom-title" | "ai-title" => {
                if let Some(msg) = &event.message {
                    if let Some(ClaudeContent::Text(t)) = msg.content.as_ref() {
                        title = Some(t.clone());
                    }
                }
                continue;
            }
            "system" => continue,
            _ => {}
        }

        let usage = event.message.as_ref().and_then(|m| m.usage.as_ref());
        if let Some(u) = usage {
            input_tokens += u.input_tokens;
            output_tokens += u.output_tokens;
            cache_read_tokens += u.cache_read_input_tokens;
            cache_write_tokens += u.cache_creation_input_tokens;
        }

        if let Some(m) = event.message.as_ref() {
            if model.is_none() && m.model.is_some() {
                model = m.model.clone();
            }
        }

        match event.kind.as_str() {
            "user" => {
                turn_number += 1;
                let msg_uuid = message_id(sid, &event.uuid);
                let mut content = None;
                let thinking = None;

                if let Some(ClaudeContent::Text(text)) =
                    event.message.as_ref().and_then(|m| m.content.as_ref())
                {
                    content = Some(text.clone());
                } else if let Some(ClaudeContent::Blocks(blocks)) =
                    event.message.as_ref().and_then(|m| m.content.as_ref())
                {
                    let mut synthesized: Vec<String> = Vec::new();
                    for block in blocks {
                        match block {
                            ClaudeBlock::ToolResult {
                                tool_use_id,
                                content: trc,
                                is_error,
                            } => {
                                let output_text = tool_result_text(trc);
                                synthesized.push(output_text.clone());
                                if let Some(tc) = pending_tool_calls.get_mut(tool_use_id) {
                                    tc.response_message_uuid = Some(msg_uuid);
                                    tc.tool_output = Some(output_text);
                                    tc.is_error = *is_error;
                                }
                            }
                            ClaudeBlock::Text { text } => {
                                synthesized.push(text.clone());
                            }
                            _ => {}
                        }
                    }
                    if !synthesized.is_empty() {
                        content = Some(synthesized.join("\n"));
                    }
                }

                messages.push(NormalizedMessage {
                    uuid: msg_uuid,
                    role: "user".to_string(),
                    content,
                    thinking,
                    parent_uuid: event.parent_uuid.clone(),
                    source_seq,
                    turn_number,
                    sequence: 0,
                    input_tokens: usage.map(|u| u.input_tokens),
                    output_tokens: None,
                });
            }
            "assistant" => {
                turn_number += 1;
                let msg_uuid = message_id(sid, &event.uuid);
                let mut text_parts: Vec<String> = Vec::new();
                let mut thinking_parts: Vec<String> = Vec::new();

                if let Some(ClaudeContent::Text(text)) =
                    event.message.as_ref().and_then(|m| m.content.as_ref())
                {
                    text_parts.push(text.clone());
                } else if let Some(ClaudeContent::Blocks(blocks)) =
                    event.message.as_ref().and_then(|m| m.content.as_ref())
                {
                    for block in blocks {
                        match block {
                            ClaudeBlock::Text { text } => text_parts.push(text.clone()),
                            ClaudeBlock::Thinking { thinking: t, .. } => {
                                thinking_parts.push(t.clone())
                            }
                            ClaudeBlock::ToolUse { id, name, input } => {
                                let tc_uuid = tool_call_id(sid, msg_uuid, id);
                                let tc = NormalizedToolCall {
                                    uuid: tc_uuid,
                                    call_id: id.clone(),
                                    tool_name: name.clone(),
                                    tool_input: Some(input.to_string()),
                                    tool_output: None,
                                    tool_output_raw: None,
                                    is_error: None,
                                    started_at: Some(event.timestamp.clone()),
                                    completed_at: None,
                                    request_message_uuid: msg_uuid,
                                    response_message_uuid: None,
                                };
                                pending_tool_calls.insert(id.clone(), tc);
                            }
                            _ => {}
                        }
                    }
                }

                messages.push(NormalizedMessage {
                    uuid: msg_uuid,
                    role: "assistant".to_string(),
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
                    parent_uuid: event.parent_uuid.clone(),
                    source_seq,
                    turn_number,
                    sequence: 0,
                    input_tokens: None,
                    output_tokens: usage.map(|u| u.output_tokens),
                });
            }
            other => {
                errata.push(Erratum {
                    issue_type: "unknown_event_kind".to_string(),
                    field_path: Some(format!("type:{other}")),
                    detail: format!("unhandled event kind '{other}' at line {source_seq}"),
                    raw_snippet: None,
                });
            }
        }
    }

    // Move pending tool calls into the final list.
    for (_, tc) in pending_tool_calls {
        tool_calls.push(tc);
    }

    let tool_call_count = tool_calls.len() as i64;
    let error_count = errata.len() as i64;

    Ok(NormalizedSession {
        id: sid,
        source: session_ref.source.clone(),
        native_id,
        title,
        started_at: Some(started_at),
        ended_at,
        model,
        git_branch,
        git_sha,
        raw_path: session_ref.path.clone(),
        project_root,
        parent_session_id: None,
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

fn tool_result_text(content: &ToolResultContent) -> String {
    match content {
        ToolResultContent::Text(s) => s.clone(),
        ToolResultContent::Array(blocks) => blocks
            .iter()
            .map(|b| b.text.clone())
            .collect::<Vec<_>>()
            .join("\n"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_path(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("fixtures")
            .join("claude")
            .join(name)
    }

    fn parse_fixture(name: &str) -> NormalizedSession {
        let path = fixture_path(name);
        let connector = ClaudeCodeConnector;
        let native_id = peek_session_id(&path).unwrap().unwrap();
        let session_ref = SessionRef {
            source: "claude".to_string(),
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
        assert_eq!(session.input_tokens, 12);
        assert_eq!(session.output_tokens, 5);
    }

    #[test]
    fn with_tool_call_fixture() {
        let session = parse_fixture("with_tool_call.jsonl");
        assert_eq!(session.messages.len(), 3);
        assert_eq!(session.tool_calls.len(), 1);
        let tc = &session.tool_calls[0];
        assert!(tc.response_message_uuid.is_some());
        assert_eq!(tc.tool_output.as_deref(), Some("mock output"));
    }

    #[test]
    fn multi_tool_fixture() {
        let session = parse_fixture("multi_tool.jsonl");
        assert_eq!(session.tool_calls.len(), 2);
        assert_eq!(session.tool_call_count, 2);
    }

    #[test]
    fn thinking_fixture() {
        let session = parse_fixture("thinking.jsonl");
        let assistant = session
            .messages
            .iter()
            .find(|m| m.role == "assistant")
            .unwrap();
        assert!(assistant.thinking.is_some());
    }

    #[test]
    fn task_tool_array_result_fixture() {
        let session = parse_fixture("task_tool_array_result.jsonl");
        let user = session
            .messages
            .iter()
            .find(|m| m.role == "user" && m.content.as_deref().unwrap_or("").contains("line one"))
            .unwrap();
        assert!(user.content.as_deref().unwrap_or("").contains("line two"));
    }

    #[test]
    fn system_and_compact_fixture() {
        let session = parse_fixture("system_and_compact.jsonl");
        assert!(!session.messages.iter().any(|m| m.role == "system"));
        assert_eq!(session.title.as_deref(), Some("Compact session"));
    }

    #[test]
    fn unknown_fields_fixture() {
        let session = parse_fixture("unknown_fields.jsonl");
        assert_eq!(session.errata.len(), 2);
        assert!(!session.messages.is_empty());
    }
}
