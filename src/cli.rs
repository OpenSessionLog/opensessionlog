use std::path::PathBuf;

use clap::{Parser, Subcommand};
use uuid::Uuid;

use crate::db;
use crate::embed;
use crate::error::{OslError, Result};
use crate::export;
use crate::ingest;
use crate::search;
use crate::watch;

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
    /// Compute and store message embeddings via a user-supplied embedder script.
    Embed {
        /// Path to the embedder script (invoked once; NDJSON over stdin/stdout).
        #[arg(long)]
        provider: PathBuf,
        /// Embed at most N messages with NULL embeddings (default: all).
        #[arg(long)]
        limit: Option<u64>,
    },
    /// Semantic KNN search over stored embeddings.
    Search {
        /// Natural-language query to embed and match.
        query: String,
        /// Maximum number of results.
        #[arg(long, default_value_t = 20)]
        limit: u32,
    },
    /// Find sessions similar to a given session by summary embedding.
    Similar {
        /// Session ID (UUID) to compare against.
        session_id: String,
        /// Maximum number of similar sessions to return.
        #[arg(long, default_value_t = 10)]
        limit: u32,
    },
    /// Watch directories and auto-ingest changed session files.
    Watch {
        /// Directories/files to watch (default: ~/.claude/projects/).
        paths: Vec<PathBuf>,
        /// Debounce window in milliseconds before flushing an ingest batch.
        #[arg(long, default_value_t = 1500)]
        debounce: u64,
        /// Poll interval (seconds) for SQLite databases (inotify-unfriendly).
        #[arg(long, default_value_t = 60)]
        interval: u64,
        /// Scan once and exit (do not run as a daemon). Useful for tests/cron.
        #[arg(long)]
        once: bool,
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
        Cmd::Embed { provider, limit } => {
            let mut conn = db::open(&vault)?;
            let stats = embed::run(&mut conn, &provider, limit)?;
            println!(
                "embedded {} messages across {} sessions summarized (model={}, dims={})",
                stats.messages_embedded, stats.sessions_summarized, stats.model, stats.dimensions
            );
        }
        Cmd::Search { query, limit } => {
            let conn = db::open(&vault)?;
            if !search::has_embeddings(&conn)? {
                println!("no embeddings found; run 'osl embed' first");
            } else {
                let hits = search::semantic(&conn, &query, limit)?;
                for hit in hits {
                    println!(
                        "[{}] {}: {} (dist {})",
                        hit.session_id, hit.role, hit.content_snippet, hit.distance
                    );
                }
            }
        }
        Cmd::Similar { session_id, limit } => {
            let conn = db::open(&vault)?;
            let id = Uuid::parse_str(&session_id).map_err(OslError::from)?;
            if !search::has_summary_embedding(&conn, &id)? {
                println!("session {session_id} has no summary embedding; run 'osl embed' first");
                return Ok(());
            }
            let hits = search::similar(&conn, &id, limit)?;
            for hit in hits {
                println!(
                    "[{}] {} (dist {})",
                    hit.session_id,
                    hit.title.unwrap_or_default(),
                    hit.distance
                );
            }
        }
        Cmd::Watch {
            paths,
            debounce,
            interval,
            once,
        } => {
            let mut paths = paths;
            if paths.is_empty() {
                let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
                paths.push(PathBuf::from(home).join(".claude/projects"));
            }
            let mut conn = db::open(&vault)?;
            if once {
                watch::scan_once(&mut conn, &paths, interval, false)?;
            } else {
                watch::watch(&mut conn, &paths, debounce, interval)?;
            }
        }
    }
    Ok(())
}
