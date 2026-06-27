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

#[derive(Debug, Clone, PartialEq)]
pub struct SemanticHit {
    pub session_id: Uuid,
    pub message_uuid: Uuid,
    pub role: String,
    pub content_snippet: String,
    pub distance: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SimilarSession {
    pub session_id: Uuid,
    pub title: Option<String>,
    pub distance: f64,
}

/// Which named period or custom range was requested.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ReportPeriodKind {
    Daily,
    Weekly,
    Monthly,
    #[serde(rename = "last-30-days")]
    Last30Days,
    Custom, // --from/--to
}

/// Top-level report envelope. This is what `--format json` serializes
/// and what is stored in `reports.data_json` when `--save` is set.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ReportDocument {
    pub scope: String, // "global" | "project:<slug>" | "source:<name>" | "project:<slug>;source:<name>"
    pub period_kind: ReportPeriodKind, // daily|weekly|monthly|last-30-days|custom
    pub period_start: String, // YYYY-MM-DD (inclusive)
    pub period_end: String, // YYYY-MM-DD (inclusive)
    pub generated_at: String, // ISO 8601 UTC timestamp (strftime('%Y-%m-%dT%H:%M:%SZ','now'))
    pub from_cache: bool, // true iff served from reports table (closed period)
    pub metrics: ReportMetrics,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ReportMetrics {
    pub total_sessions: i64,
    pub total_input_tokens: i64,
    pub total_output_tokens: i64,
    pub total_cache_read_tokens: i64,
    pub total_cache_write_tokens: i64,
    pub total_tokens: i64, // sum of the four above (OK to read sessions.total_tokens too)
    pub estimated_cost_usd: Option<f64>, // None => "no data"
    pub message_count: i64,
    pub messages_by_role: Vec<RoleBreakdown>,
    pub tool_call_count: i64,
    pub top_tools: Vec<TopTool>, // top 10 by count within scope
    pub error_count: i64,
    pub unique_models: i64,
    pub avg_session_duration_seconds: Option<f64>, // None if no session has both started_at & ended_at
    pub top_projects: Vec<TopProject>,             // top 10 by session_count within scope
    pub daily_breakdown: Vec<DailyBreakdown>,
    pub sources: Vec<SourceBreakdown>, // one row per source present in scope
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct RoleBreakdown {
    pub role: String,
    pub count: i64,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct TopTool {
    pub tool_name: String,
    pub count: i64,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct TopProject {
    pub slug: Option<String>,
    pub session_count: i64,
    pub total_tokens: i64,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SourceBreakdown {
    pub source: String,
    pub session_count: i64,
    pub total_tokens: i64,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct DailyBreakdown {
    pub date: String, // YYYY-MM-DD (DATE(started_at))
    pub session_count: i64,
    pub message_count: i64,
    pub tool_call_count: i64,
    pub total_tokens: i64,
}
