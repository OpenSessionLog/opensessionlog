use std::io::BufRead;
use std::path::PathBuf;

use osl::connector::{CodexCliConnector, Connector};
use osl::ids::session_id;

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("codex")
        .join(name)
}

fn parse_fixture(name: &str) -> osl::model::NormalizedSession {
    let path = fixture(name);
    let connector = CodexCliConnector;
    let native_id = {
        let file = std::fs::File::open(&path).unwrap();
        let mut reader = std::io::BufReader::new(file);
        let mut first = String::new();
        reader.read_line(&mut first).unwrap();
        let ev: serde_json::Value = serde_json::from_str(&first).unwrap();
        ev["payload"]["id"].as_str().unwrap().to_string()
    };
    let session_ref = osl::model::SessionRef {
        source: "codex".to_string(),
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
    assert_eq!(session.input_tokens, 150);
    assert_eq!(session.output_tokens, 80);
    assert_eq!(session.cache_read_tokens, 70);
}

#[test]
fn with_tool_call_pairs_request_and_response() {
    let session = parse_fixture("with_tool_call.jsonl");
    assert_eq!(session.tool_calls.len(), 1);
    let tc = &session.tool_calls[0];
    assert_eq!(tc.tool_name, "exec_command");
    assert!(tc.response_message_uuid.is_some());
    assert_eq!(tc.tool_output.as_deref(), Some("mock output"));
    assert!(session
        .messages
        .iter()
        .any(|m| Some(m.uuid) == tc.response_message_uuid));
}

#[test]
fn with_reasoning_populates_thinking() {
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
fn with_token_count_reconciles_max_cumulatives() {
    let session = parse_fixture("with_token_count.jsonl");
    assert_eq!(session.input_tokens, 150);
    assert_eq!(session.output_tokens, 90);
    assert_eq!(session.cache_read_tokens, 70);
}

#[test]
fn unknown_events_produce_errata_but_ingest() {
    let session = parse_fixture("unknown_events.jsonl");
    assert!(!session.messages.is_empty());
    assert!(!session.errata.is_empty());
    assert!(!session
        .errata
        .iter()
        .any(|e| e.field_path.as_deref() == Some("event_msg:turn_started")));
    assert!(!session
        .errata
        .iter()
        .any(|e| e.field_path.as_deref() == Some("event_msg:turn_complete")));
}

#[test]
fn mcp_tool_from_event_msg_reconstructs_tool_call() {
    let session = parse_fixture("mcp_tool_from_event_msg.jsonl");
    assert!(!session.tool_calls.is_empty());
    assert!(session
        .errata
        .iter()
        .any(|e| e.issue_type == "tool_call_from_event_msg"));
}

#[test]
fn with_subagent_link_sets_parent_session_id() {
    let session = parse_fixture("with_subagent_link.jsonl");
    let expected = session_id("codex", "codex-parent");
    assert_eq!(session.parent_session_id, Some(expected));
}

#[test]
fn parse_is_idempotent() {
    let a = parse_fixture("minimal.jsonl");
    let b = parse_fixture("minimal.jsonl");
    assert_eq!(a.id, b.id);
    assert_eq!(a.native_id, b.native_id);
    assert_eq!(a.messages.len(), b.messages.len());
    assert_eq!(a.tool_calls.len(), b.tool_calls.len());
}

#[test]
fn discover_finds_codex_files_only() {
    let tmp = tempfile::TempDir::new().unwrap();
    let codex_path = tmp.path().join("codex.jsonl");
    let claude_path = tmp.path().join("claude.jsonl");

    std::fs::write(
        &codex_path,
        r#"{"timestamp":"2026-06-01T09:15:22Z","type":"session_meta","payload":{"id":"discover-codex"}}
"#,
    )
    .unwrap();
    std::fs::write(
        &claude_path,
        r#"{"timestamp":"2026-06-01T09:15:22Z","type":"user","uuid":"u1","sessionId":"discover-claude","version":"1","cwd":"/tmp"}
"#,
    )
    .unwrap();

    let connector = CodexCliConnector;
    let refs = connector.discover(tmp.path()).unwrap();
    assert_eq!(refs.len(), 1);
    assert_eq!(refs[0].native_id, "discover-codex");
    assert_eq!(refs[0].source, "codex");
}

#[test]
fn for_source_routes_codex() {
    assert!(osl::connector::for_source("codex").is_some());
    let connector = osl::connector::for_source("codex").unwrap();
    assert_eq!(connector.name(), "codex");
}
