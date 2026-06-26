use std::path::PathBuf;
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq)]
pub struct SessionRef {
    pub source: String,
    pub native_id: String,
    pub path: PathBuf,
    pub project_path: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub struct NormalizedSession {
    pub id: Uuid,
    pub source: String,
    pub native_id: String,
    pub title: Option<String>,
    pub started_at: Option<String>,
    pub ended_at: Option<String>,
    pub model: Option<String>,
    pub git_branch: Option<String>,
    pub git_sha: Option<String>,
    pub raw_path: PathBuf,
    pub project_root: Option<PathBuf>,
    pub parent_session_id: Option<Uuid>,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_read_tokens: i64,
    pub cache_write_tokens: i64,
    pub tool_call_count: i64,
    pub error_count: i64,
    pub messages: Vec<NormalizedMessage>,
    pub tool_calls: Vec<NormalizedToolCall>,
    pub errata: Vec<Erratum>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct NormalizedMessage {
    pub uuid: Uuid,
    pub role: String,
    pub content: Option<String>,
    pub thinking: Option<String>,
    pub parent_uuid: Option<String>,
    pub source_seq: i64,
    pub turn_number: i64,
    pub sequence: i64,
    pub input_tokens: Option<i64>,
    pub output_tokens: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct NormalizedToolCall {
    pub uuid: Uuid,
    pub call_id: String,
    pub tool_name: String,
    pub tool_input: Option<String>,
    pub tool_output: Option<String>,
    pub tool_output_raw: Option<String>,
    pub is_error: Option<bool>,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
    pub request_message_uuid: Uuid,
    pub response_message_uuid: Option<Uuid>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct Erratum {
    pub issue_type: String,
    pub field_path: Option<String>,
    pub detail: String,
    pub raw_snippet: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct IngestReport {
    pub sessions: Vec<IngestReportSession>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct IngestReportSession {
    pub session_id: Uuid,
    pub title: Option<String>,
    pub message_count: usize,
    pub tool_call_count: usize,
    pub total_tokens: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct GrepHit {
    pub session_id: Uuid,
    pub role: String,
    pub content_snippet: String,
    pub rank: f64,
}
