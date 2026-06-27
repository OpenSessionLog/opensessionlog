use std::collections::HashSet;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant, SystemTime};

use notify::{RecursiveMode, Watcher};

use crate::error::{OslError, Result};
use crate::ingest;
use crate::model::IngestReportSession;
use rusqlite::Connection;

fn is_db_file(path: &Path) -> bool {
    path.extension()
        .map(|ext| ext == "db" || ext == "sqlite")
        .unwrap_or(false)
}

fn notify_to_io(e: notify::Error) -> io::Error {
    io::Error::other(e)
}

fn is_jsonl_file(path: &Path) -> bool {
    path.extension()
        .map(|ext| ext.eq_ignore_ascii_case("jsonl"))
        .unwrap_or(false)
}

fn print_session(session: &IngestReportSession) {
    println!(
        "ingested {} ({}) — {} messages, {} tool calls, {} tokens",
        session.title.as_deref().unwrap_or("Untitled"),
        session.session_id,
        session.message_count,
        session.tool_call_count,
        session.total_tokens
    );
}

fn flush_pending(conn: &mut Connection, pending: &mut HashSet<PathBuf>) -> Result<usize> {
    let snapshot: HashSet<PathBuf> = std::mem::take(pending);
    let mut total = 0;
    for path in snapshot {
        match ingest::ingest(conn, &path) {
            Ok(report) => {
                for session in &report.sessions {
                    print_session(session);
                    total += 1;
                }
            }
            Err(e) => eprintln!("osl watch: failed to ingest {}: {e}", path.display()),
        }
    }
    Ok(total)
}

/// Returns true when the debounce deadline has been reached.
pub(crate) fn should_flush(deadline: Option<Instant>, now: Instant) -> bool {
    deadline.is_some_and(|d| now >= d)
}

/// Scan the given paths once, ingest anything that matches the routing rules,
/// and return the number of sessions ingested.
pub fn scan_once(
    conn: &mut Connection,
    paths: &[PathBuf],
    _interval: u64,
    _daemon: bool,
) -> Result<usize> {
    let mut total = 0;
    for path in paths {
        match ingest::ingest(conn, path) {
            Ok(report) => {
                for session in &report.sessions {
                    print_session(session);
                    total += 1;
                }
            }
            Err(e) => eprintln!("osl watch: failed to ingest {}: {e}", path.display()),
        }
    }
    Ok(total)
}

/// Watch directories for JSONL changes and poll SQLite files for changes.
pub fn watch(
    conn: &mut Connection,
    paths: &[PathBuf],
    debounce_ms: u64,
    db_poll_secs: u64,
) -> Result<()> {
    let debounce = Duration::from_millis(debounce_ms);
    let poll_interval = Duration::from_secs(db_poll_secs);

    let mut jsonl_dirs: Vec<PathBuf> = Vec::new();
    let mut sqlite_files: Vec<(PathBuf, Option<SystemTime>)> = Vec::new();

    for path in paths {
        if path.is_file() && is_db_file(path) {
            let mtime = fs::metadata(path).and_then(|m| m.modified()).ok();
            sqlite_files.push((path.clone(), mtime));
        } else if path.is_dir() {
            jsonl_dirs.push(path.clone());
        } else if path.is_file() && is_jsonl_file(path) {
            // Watch the parent directory so file-level changes are reported.
            if let Some(parent) = path.parent() {
                jsonl_dirs.push(parent.to_path_buf());
            }
        }
    }

    let (tx, rx) = mpsc::channel::<notify::Result<notify::Event>>();
    let mut watcher = notify::recommended_watcher(tx).map_err(notify_to_io)?;
    for dir in &jsonl_dirs {
        watcher
            .watch(dir, RecursiveMode::Recursive)
            .map_err(notify_to_io)?;
    }

    let mut pending: HashSet<PathBuf> = HashSet::new();
    let mut deadline: Option<Instant> = None;
    let mut last_poll = Instant::now();

    loop {
        let now = Instant::now();

        match rx.recv_timeout(Duration::from_millis(500)) {
            Ok(Ok(event)) => {
                for path in event.paths {
                    if is_jsonl_file(&path) {
                        pending.insert(path);
                        deadline = Some(now + debounce);
                    }
                }
            }
            Ok(Err(e)) => {
                return Err(OslError::Io(io::Error::other(format!("watch error: {e}"))));
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }

        let now = Instant::now();

        if now.duration_since(last_poll) >= poll_interval {
            last_poll = now;
            for (path, last_mtime) in &mut sqlite_files {
                if let Ok(metadata) = fs::metadata(path.as_path()) {
                    if let Ok(mtime) = metadata.modified() {
                        if last_mtime.is_none_or(|last| mtime != last) {
                            pending.insert(path.clone());
                            deadline = Some(now + debounce);
                            *last_mtime = Some(mtime);
                        }
                    }
                }
            }

            // Discover new .db/.sqlite files that appeared since startup.
            for dir in &jsonl_dirs {
                if let Ok(entries) = std::fs::read_dir(dir) {
                    for entry in entries.flatten() {
                        let path = entry.path();
                        if is_db_file(&path) && !sqlite_files.iter().any(|(p, _)| p == &path) {
                            let mtime = fs::metadata(&path).and_then(|m| m.modified()).ok();
                            sqlite_files.push((path, mtime));
                        }
                    }
                }
            }
        }

        if should_flush(deadline, now) && !pending.is_empty() {
            let _ = flush_pending(conn, &mut pending);
            deadline = None;
        }
    }

    println!("watch stopped");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flush_dedupes_repeated_events() {
        let mut pending: HashSet<PathBuf> = HashSet::new();
        let p = PathBuf::from("/tmp/session.jsonl");
        pending.insert(p.clone());
        pending.insert(p.clone());
        assert_eq!(pending.len(), 1);

        let now = Instant::now();
        assert!(!should_flush(None, now));
        assert!(should_flush(Some(now), now));
        assert!(should_flush(Some(now - Duration::from_millis(1)), now));
        assert!(!should_flush(Some(now + Duration::from_millis(100)), now));
    }
}
