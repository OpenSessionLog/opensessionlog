use std::path::Path;

use serde_json::Value;

use crate::connector::reader;
use crate::error::Result;

/// Classify a single non-empty JSONL line as a known Copilot format and
/// extract the session ID. Returns `None` for any non-Copilot line.
///
/// Copied verbatim from `copilot.rs::classify_copilot_line` (current
/// lines 83–110). Supports:
/// - chatSessions seed patches (`kind == 0` with `v.sessionId`)
/// - transcripts `session.start` events (top-level `sessionId` or
///   `payload.sessionId`)
///
/// Private to this module — called only by `peek_copilot_id` and
/// `detect_jsonl_kind`, both within `probe.rs`.
fn classify_copilot_line(line: &str) -> Option<String> {
    let value: Value = serde_json::from_str(line).ok()?;

    // chatSessions seed patch.
    if value.get("kind").and_then(|v| v.as_i64()) == Some(0) {
        if let Some(session_id) = value
            .get("v")
            .and_then(|v| v.get("sessionId"))
            .and_then(|v| v.as_str())
        {
            return Some(session_id.to_string());
        }
    }

    // transcripts session.start event.
    if value.get("type").and_then(|v| v.as_str()) == Some("session.start") {
        if let Some(session_id) = value.get("sessionId").and_then(|v| v.as_str()).or_else(|| {
            value
                .get("payload")
                .and_then(|p| p.get("sessionId"))
                .and_then(|v| v.as_str())
        }) {
            return Some(session_id.to_string());
        }
    }

    None
}

/// Peek at the first line of a JSONL file and decide whether it is a
/// Copilot Chat local-storage file. Delegates to `classify_copilot_line`
/// after reading the first line via `reader::read_first_line`.
///
/// `pub(crate)` because `ingest.rs` calls this for `Some("copilot")` routing.
pub(crate) fn peek_copilot_id(path: &Path) -> Result<Option<String>> {
    let line = match reader::read_first_line(path)? {
        Some(l) => l,
        None => return Ok(None),
    };
    Ok(classify_copilot_line(&line))
}

/// Detect whether a single `.jsonl` file is a Copilot Chat, Claude Code, or
/// Codex CLI session by inspecting the first line. Returns
/// `Some("copilot")`, `Some("claude")`, `Some("codex")`, or `None`.
///
/// Order is critical and MUST be preserved:
///  1. Copilot first (delegate to `classify_copilot_line`).
///  2. Claude Code next  (top-level `sessionId` present).
///  3. Codex CLI last    (`type == "session_meta"` AND `payload.id` is a
///     non-empty string).
///
/// Moved verbatim from `ingest.rs::detect_jsonl_kind` (current lines
/// 178–224), except the file-read is replaced by `reader::read_first_line`.
///
/// `pub(crate)` is sufficient (only `ingest.rs` calls this). Marking it
/// `pub(crate)` rather than `pub` keeps the crate surface unchanged.
pub(crate) fn detect_jsonl_kind(path: &Path) -> Result<Option<&'static str>> {
    let first_line = match reader::read_first_line(path)? {
        Some(l) => l,
        None => return Ok(None),
    };

    // 1) Copilot — delegate to the shared classifier.
    if classify_copilot_line(&first_line).is_some() {
        return Ok(Some("copilot"));
    }

    #[derive(serde::Deserialize)]
    struct FirstEvent {
        #[serde(rename = "sessionId", default)]
        session_id: Option<String>,
        // Bound to JSON key "type" via serde(rename). Used by Codex detection
        // (type == "session_meta").
        #[serde(rename = "type", default)]
        kind_str: Option<String>,
        #[serde(default)]
        payload: Option<serde_json::Value>,
    }

    match serde_json::from_str::<FirstEvent>(&first_line) {
        Ok(event) => {
            // 2) Claude Code: top-level sessionId.
            if event.session_id.is_some() {
                return Ok(Some("claude"));
            }
            // 3) Codex CLI: session_meta with payload.id.
            if event.kind_str.as_deref() == Some("session_meta") {
                if let Some(payload) = event.payload {
                    if payload.get("id").and_then(|v| v.as_str()).is_some() {
                        return Ok(Some("codex"));
                    }
                }
            }
            Ok(None)
        }
        Err(_) => Ok(None),
    }
}
