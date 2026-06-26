use std::path::PathBuf;

use clap::{Parser, Subcommand};

use crate::db;
use crate::error::Result;
use crate::export;
use crate::ingest;
use crate::search;

#[derive(Parser)]
#[command(
    name = "osl",
    version,
    about = "OpenSessionLog — searchable AI session vault"
)]
pub struct Cli {
    #[command(subcommand)]
    pub cmd: Cmd,

    /// Vault database path (default: ~/.opensessionlog/data.sqlite; OSL_VAULT env override)
    #[arg(long, global = true, default_value_t = db::default_vault_path().to_string_lossy().to_string())]
    pub vault: String,
}

#[derive(Subcommand)]
pub enum Cmd {
    /// Initialize a new vault.
    Init {
        /// Vault path (defaults to the global --vault default).
        path: Option<PathBuf>,
        /// Overwrite an existing vault.
        #[arg(long)]
        force: bool,
    },
    /// Ingest session files into the vault.
    Ingest {
        /// File or directory to ingest.
        path: PathBuf,
    },
    /// Search messages with FTS5.
    Grep {
        /// FTS5 query pattern.
        pattern: String,
        /// Maximum number of results.
        #[arg(long, default_value_t = 20)]
        limit: u32,
        /// Filter to a project slug.
        #[arg(long)]
        project: Option<String>,
    },
    /// Export a session transcript.
    Export {
        /// Session ID (UUID).
        session_id: String,
        /// Output format.
        #[arg(long, default_value = "markdown")]
        format: String,
    },
}

impl Cli {
    pub fn parse_args() -> Result<Self> {
        Ok(Self::parse())
    }
}

pub fn run(cli: Cli) -> Result<()> {
    let vault = PathBuf::from(&cli.vault);
    match cli.cmd {
        Cmd::Init { path, force } => {
            let target = path.unwrap_or(vault);
            db::init(&target, force)?;
            println!("initialized vault at {}", target.display());
        }
        Cmd::Ingest { path } => {
            let mut conn = db::open(&vault)?;
            let report = ingest::ingest(&mut conn, &path)?;
            for session in report.sessions {
                println!(
                    "ingested {} ({}) — {} messages, {} tool calls, {} tokens",
                    session.title.as_deref().unwrap_or("Untitled"),
                    session.session_id,
                    session.message_count,
                    session.tool_call_count,
                    session.total_tokens
                );
            }
        }
        Cmd::Grep {
            pattern,
            limit,
            project,
        } => {
            let conn = db::open(&vault)?;
            let hits = search::grep(&conn, &pattern, limit, project.as_deref())?;
            for hit in hits {
                println!(
                    "[{}] {}: {}… (rank {})",
                    hit.session_id, hit.role, hit.content_snippet, hit.rank
                );
            }
        }
        Cmd::Export { session_id, format } => {
            let conn = db::open(&vault)?;
            match format.as_str() {
                "markdown" | "md" => {
                    let md = export::export_markdown(&conn, &session_id)?;
                    print!("{md}");
                }
                other => {
                    return Err(crate::error::OslError::Usage(format!(
                        "unsupported export format: {other}"
                    )));
                }
            }
        }
    }
    Ok(())
}
