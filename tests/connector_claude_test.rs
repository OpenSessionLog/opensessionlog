use std::io::BufRead;
use std::path::PathBuf;

use osl::connector::{ClaudeCodeConnector, Connector};

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("claude")
        .join(name)
}

fn parse_fixture(name: &str) -> osl::model::NormalizedSession {
    let path = fixture(name);
    let connector = ClaudeCodeConnector;
    let native_id = {
        let file = std::fs::File::open(&path).unwrap();
        let mut reader = std::io::BufReader::new(file);
        let mut first = String::new();
        reader.read_line(&mut first).unwrap();
        let ev: serde_json::Value = serde_json::from_str(&first).unwrap();
        ev["sessionId"].as_str().unwrap().to_string()
    };
    let session_ref = osl::model::SessionRef {
        source: "claude".to_string(),
        native_id,
        path,
        project_path: None,
    };
    connector.parse(&session_ref).unwrap()
}

#[test]
fn minimal_has_two_messages_and_zero_tool_calls() {
    let session = parse_fixture("minimal.jsonl");
    assert_eq!(session.messages.len(), 2);
    assert!(session.tool_calls.is_empty());
    assert_eq!(session.input_tokens, 12);
    assert_eq!(session.output_tokens, 5);
}

#[test]
fn with_tool_call_pairs_request_and_response() {
    let session = parse_fixture("with_tool_call.jsonl");
    assert_eq!(session.messages.len(), 3);
    assert_eq!(session.tool_calls.len(), 1);
    let tc = &session.tool_calls[0];
    assert_eq!(tc.tool_name, "Bash");
    assert!(tc.response_message_uuid.is_some());
    assert_eq!(tc.tool_output.as_deref(), Some("mock output"));
}

#[test]
fn multi_tool_pairs_both_calls() {
    let session = parse_fixture("multi_tool.jsonl");
    assert_eq!(session.tool_calls.len(), 2);
    assert!(session
        .tool_calls
        .iter()
        .all(|tc| tc.response_message_uuid.is_some()));
    assert_eq!(session.tool_call_count, 2);
}

#[test]
fn thinking_fixture_populates_thinking() {
    let session = parse_fixture("thinking.jsonl");
    let assistant = session
        .messages
        .iter()
        .find(|m| m.role == "assistant")
        .unwrap();
    assert!(assistant
        .thinking
        .as_deref()
        .unwrap()
        .contains("audit existing tables"));
}

#[test]
fn array_tool_result_joins_text() {
    let session = parse_fixture("task_tool_array_result.jsonl");
    let user = session
        .messages
        .iter()
        .find(|m| m.role == "user" && m.content.as_deref().unwrap_or("").contains("line one"))
        .unwrap();
    assert!(user.content.as_deref().unwrap().contains("line two"));
}

#[test]
fn system_skipped_and_title_set() {
    let session = parse_fixture("system_and_compact.jsonl");
    assert!(!session.messages.iter().any(|m| m.role == "system"));
    assert_eq!(session.title.as_deref(), Some("Compact session"));
}

#[test]
fn unknown_fields_produces_errata_but_ingests() {
    let session = parse_fixture("unknown_fields.jsonl");
    assert_eq!(session.errata.len(), 2);
    assert!(!session.messages.is_empty());
}
