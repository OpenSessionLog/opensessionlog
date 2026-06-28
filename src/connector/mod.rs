use std::path::Path;

use uuid::Uuid;

use crate::error::Result;
use crate::model::{NormalizedSession, SessionRef};

pub mod claude;
pub use claude::ClaudeCodeConnector;

pub mod codex;
pub use codex::CodexCliConnector;

pub mod opencode;
pub use opencode::OpenCodeConnector;

pub mod hermes;
pub use hermes::HermesConnector;

pub mod copilot;
pub use copilot::CopilotChatConnector;

pub mod probe;
pub mod reader;
pub mod walk;

pub trait Connector {
    fn name(&self) -> &'static str;
    fn discover(&self, directory: &Path) -> Result<Vec<SessionRef>>;
    /// Same as `discover`, but optionally restricted to sessions whose timestamp
    /// passes `filter`. Connectors that can push the filter into their source query
    /// (SQLite sources) override this to filter server-side; JSONL connectors use the
    /// default impl (return all refs) and let callers apply the mtime pre-filter.
    fn discover_filtered(
        &self,
        directory: &std::path::Path,
        filter: &crate::recency::RecencyFilter,
    ) -> Result<Vec<SessionRef>> {
        // Default: ignore the filter (backward-compatible with `discover`).
        let _ = filter;
        self.discover(directory)
    }
    fn session_id(&self, session_ref: &SessionRef) -> Uuid {
        crate::ids::session_id(self.name(), &session_ref.native_id)
    }
    fn parse(&self, session_ref: &SessionRef) -> Result<NormalizedSession>;
}

pub fn for_source(name: &str) -> Option<Box<dyn Connector>> {
    match name {
        "claude" => Some(Box::new(ClaudeCodeConnector)),
        "codex" => Some(Box::new(CodexCliConnector)),
        "opencode" => Some(Box::new(OpenCodeConnector)),
        "hermes" => Some(Box::new(HermesConnector)),
        "copilot" => Some(Box::new(CopilotChatConnector)),
        _ => None,
    }
}
