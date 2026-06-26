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

pub trait Connector {
    fn name(&self) -> &'static str;
    fn discover(&self, directory: &Path) -> Result<Vec<SessionRef>>;
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
        _ => None,
    }
}
