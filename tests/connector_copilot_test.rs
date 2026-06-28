use std::io::BufRead;
use std::path::PathBuf;

use osl::connector::{Connector, CopilotChatConnector};
use osl::ids::session_id;
use osl::model::{NormalizedSession, SessionRef};

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("copilot")
        .join(name)
}

fn parse_fixture(name: &str) -> NormalizedSession {
    let path = fixture(name);
    let native_id = {
        let file = std::fs::File::open(&path).unwrap();
        let mut reader = std::io::BufReader::new(file);
        let mut first = String::new();
        reader.read_line(&mut first).unwrap();
        let ev: serde_json::Value = serde_json::from_str(&first).unwrap();
        ev["v"]["sessionId"]
            .as_str()
            .or_else(|| ev["sessionId"].as_str())
            .unwrap()
            .to_string()
    };
    let session_ref = SessionRef {
        source: "copilot".to_string(),
        native_id,
        path,
        project_path: None,
    };
    CopilotChatConnector.parse(&session_ref).unwrap()
}

#[test]
fn chatsessions_minimal_parses_two_messages() {
    let session = parse_fixture("minimal_chatsessions.jsonl");
    assert_eq!(session.messages.len(), 2);
    assert_eq!(session.messages[0].role, "user");
    assert_eq!(session.messages[1].role, "assistant");
    assert_eq!(session.messages[0].turn_number, 1);
    assert_eq!(session.messages[1].turn_number, 2);
    assert!(session.tool_calls.is_empty());
    assert_eq!(session.tool_call_count, 0);
    assert_eq!(session.input_tokens, 0);
    assert_eq!(session.output_tokens, 0);
    assert!(session.model.is_some());
    assert!(session.started_at.is_some());
    assert_eq!(session.error_count, 0);
}

#[test]
fn chatsessions_title_patch_applied() {
    let session = parse_fixture("chatsessions_title_patch.jsonl");
    assert_eq!(session.title.as_deref(), Some("Patched Title"));
    assert_eq!(session.messages.len(), 2);
}

#[test]
fn chatsessions_multi_request_appended() {
    let session = parse_fixture("chatsessions_multi_request_append.jsonl");
    assert_eq!(session.messages.len(), 4);
    assert_eq!(session.messages[0].turn_number, 1);
    assert_eq!(session.messages[1].turn_number, 2);
    assert_eq!(session.messages[2].turn_number, 3);
    assert_eq!(session.messages[3].turn_number, 4);
    // First request had no model; second request supplied gpt-4o.
    assert_eq!(session.model.as_deref(), Some("gpt-4o"));
}

#[test]
fn transcripts_minimal_sets_started_and_ended_at() {
    let session = parse_fixture("transcripts_minimal.jsonl");
    assert_eq!(session.messages.len(), 2);
    assert_eq!(session.messages[0].role, "user");
    assert_eq!(session.messages[1].role, "assistant");
    assert_eq!(session.messages[0].turn_number, 1);
    assert_eq!(session.messages[1].turn_number, 2);
    assert!(session.started_at.is_some());
    assert!(session.ended_at.is_some());
    assert!(
        session.ended_at > session.started_at,
        "ended_at should be strictly later than started_at"
    );
    assert_eq!(session.model.as_deref(), Some("gpt-4o"));
}

#[test]
fn transcripts_unknown_event_produces_erratum() {
    let session = parse_fixture("transcripts_unknown_event.jsonl");
    assert!(session.messages.len() >= 2);
    assert!(session
        .errata
        .iter()
        .any(|e| e.issue_type == "unknown_transcript_event"));
}

#[test]
fn transcripts_with_tool_pairs_request_and_response() {
    let session = parse_fixture("transcripts_with_tool_pair.jsonl");
    assert_eq!(session.messages.len(), 2);
    assert_eq!(session.tool_calls.len(), 1);
    let tc = &session.tool_calls[0];
    assert_eq!(tc.tool_name, "bash");
    assert!(tc.started_at.is_some());
    assert!(tc.completed_at.is_some());
    assert!(
        !tc.request_message_uuid.is_nil(),
        "request_message_uuid must be set"
    );
    assert!(
        tc.response_message_uuid.is_some(),
        "response_message_uuid must be set"
    );
    assert!(session
        .messages
        .iter()
        .any(|m| m.uuid == tc.request_message_uuid));
    assert!(session
        .messages
        .iter()
        .any(|m| Some(m.uuid) == tc.response_message_uuid));
}

#[test]
fn copilot_does_not_claude_or_codex_files() {
    let tmp = tempfile::TempDir::new().unwrap();
    let copilot_path = tmp.path().join("copilot.jsonl");
    let claude_path = tmp.path().join("claude.jsonl");
    let codex_path = tmp.path().join("codex.jsonl");

    std::fs::write(
        &copilot_path,
        r#"{"kind":0,"v":{"sessionId":"discover-copilot","creationDate":1780251875000,"requests":[]}}"#,
    )
    .unwrap();
    std::fs::write(
        &claude_path,
        r#"{"timestamp":"2026-06-01T09:15:22Z","type":"user","uuid":"u1","sessionId":"discover-claude","version":"1","cwd":"/tmp"}"#,
    )
    .unwrap();
    std::fs::write(
        &codex_path,
        r#"{"timestamp":"2026-06-01T09:15:22Z","type":"session_meta","payload":{"id":"discover-codex"}}"#,
    )
    .unwrap();

    let connector = CopilotChatConnector;
    let refs = connector.discover(tmp.path()).unwrap();
    assert_eq!(refs.len(), 1);
    assert_eq!(refs[0].native_id, "discover-copilot");
    assert_eq!(refs[0].source, "copilot");
}

#[test]
fn parse_is_idempotent() {
    let a = parse_fixture("minimal_chatsessions.jsonl");
    let b = parse_fixture("minimal_chatsessions.jsonl");
    assert_eq!(a.id, b.id);
    assert_eq!(a.native_id, b.native_id);
    assert_eq!(a.messages.len(), b.messages.len());
    assert_eq!(a.tool_calls.len(), b.tool_calls.len());
}

#[test]
fn for_source_routes_copilot() {
    assert!(osl::connector::for_source("copilot").is_some());
    let connector = osl::connector::for_source("copilot").unwrap();
    assert_eq!(connector.name(), "copilot");
}

#[test]
fn discover_finds_only_copilot_jsonl() {
    let tmp = tempfile::TempDir::new().unwrap();
    let copilot_path = tmp.path().join("copilot.jsonl");
    let claude_path = tmp.path().join("claude.jsonl");

    std::fs::write(
        &copilot_path,
        r#"{"kind":0,"v":{"sessionId":"discover-copilot-only","creationDate":1780251875000,"requests":[]}}"#,
    )
    .unwrap();
    std::fs::write(
        &claude_path,
        r#"{"timestamp":"2026-06-01T09:15:22Z","type":"user","uuid":"u1","sessionId":"discover-claude","version":"1","cwd":"/tmp"}"#,
    )
    .unwrap();

    let connector = CopilotChatConnector;
    let refs = connector.discover(tmp.path()).unwrap();
    assert_eq!(refs.len(), 1);
    assert_eq!(refs[0].source, "copilot");
}

#[test]
fn suppress_defunct_patch_kinds_make_errata() {
    let session = parse_fixture("unknown_lines.jsonl");
    assert_eq!(session.messages.len(), 2);
    assert!(!session.errata.is_empty());
    assert!(session.errata.iter().any(|e| e.issue_type == "parse_error"));
    assert!(session
        .errata
        .iter()
        .any(|e| e.issue_type == "unknown_patch_kind"));
    let unknown = session
        .errata
        .iter()
        .find(|e| e.issue_type == "unknown_patch_kind")
        .unwrap();
    assert!(
        unknown
            .field_path
            .as_deref()
            .unwrap_or("")
            .contains("[\"requests\"]"),
        "field_path should include the k array, got {:?}",
        unknown.field_path
    );
}

fn open_tmp_vault() -> (tempfile::TempDir, rusqlite::Connection) {
    let tmp = tempfile::TempDir::new().unwrap();
    let path = tmp.path().join("vault.sqlite");
    osl::db::init(&path, true).unwrap();
    let conn = osl::db::open(&path).unwrap();
    (tmp, conn)
}

#[test]
fn detect_jsonl_kind_routes_chatsessions_via_ingest() {
    let (tmp, mut conn) = open_tmp_vault();
    let src = fixture("chatSessions_detect_fixture.jsonl");
    let copy = tmp.path().join("chatsessions_detect.jsonl");
    std::fs::copy(&src, &copy).unwrap();

    let report = osl::ingest::ingest(&mut conn, &copy).unwrap();
    assert_eq!(report.sessions.len(), 1);

    let expected_id = session_id("copilot", "cs-detect");
    assert_eq!(report.sessions[0].session_id, expected_id);
}

#[test]
fn detect_jsonl_kind_routes_transcripts_via_ingest() {
    let (tmp, mut conn) = open_tmp_vault();
    let src = fixture("transcripts_detect_fixture.jsonl");
    let copy = tmp.path().join("transcripts_detect.jsonl");
    std::fs::copy(&src, &copy).unwrap();

    let report = osl::ingest::ingest(&mut conn, &copy).unwrap();
    assert_eq!(report.sessions.len(), 1);

    let expected_id = session_id("copilot", "tr-detect");
    assert_eq!(report.sessions[0].session_id, expected_id);
}

#[test]
fn detect_jsonl_kind_does_not_steal_claude_file() {
    let (tmp, mut conn) = open_tmp_vault();
    let path = tmp.path().join("claude_first_line.jsonl");
    std::fs::write(
        &path,
        r#"{"timestamp":"2026-06-01T09:15:22Z","type":"user","uuid":"u1","sessionId":"discover-claude","version":"1","cwd":"/tmp"}
"#,
    )
    .unwrap();

    let report = osl::ingest::ingest(&mut conn, &path).unwrap();
    assert_eq!(report.sessions.len(), 1);

    let expected_id = session_id("claude", "discover-claude");
    assert_eq!(report.sessions[0].session_id, expected_id);

    let source_name: String = conn
        .query_row(
            "SELECT src.name FROM sessions s JOIN sources src ON s.source_id = src.id WHERE s.id = ?1",
            [expected_id.to_string()],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(source_name, "claude");
}

#[test]
fn default_roots_for_home_finds_copilot_workspace_storage() {
    let tmp = tempfile::TempDir::new().unwrap();
    let workspace = tmp
        .path()
        .join(".config")
        .join("Code")
        .join("User")
        .join("workspaceStorage")
        .join("a1b2c3d4")
        .join("GitHub.copilot-chat")
        .join("chatSessions");
    std::fs::create_dir_all(&workspace).unwrap();
    std::fs::write(
        workspace.join("session.jsonl"),
        r#"{"kind":0,"v":{"sessionId":"root-detect","creationDate":1780251875000,"requests":[]}}"#,
    )
    .unwrap();

    let discoveries =
        osl::discover::discover_all_for_home(tmp.path(), &osl::recency::RecencyFilter::none())
            .unwrap();
    let copilot = discoveries
        .iter()
        .find(|d| d.source == "copilot")
        .expect("copilot discovery missing");
    assert_eq!(copilot.count(), 1);
    assert_eq!(copilot.refs[0].native_id, "root-detect");
}
