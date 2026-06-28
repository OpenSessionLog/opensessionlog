use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;

use crate::error::Result;

/// Read the first non-empty line of a file. Returns `Ok(None)` for empty
/// files (including files whose first line is empty after trimming).
///
/// Matches the existing skeleton in `claude.rs::peek_session_id`,
/// `codex.rs::peek_codex_id`, `copilot.rs::peek_copilot_id`, and
/// `ingest.rs::peek_session_id`/`detect_jsonl_kind`: uses `BufReader::read_line`
/// and returns `None` when zero bytes are read.
///
/// IMPORTANT: this helper returns the *raw* first line including its trailing
/// newline (caller may `trim()`); it does NOT skip blank leading lines.
/// (`reader::read_all_lines` skips blanks; this matches the existing
/// per-connector peek semantics where the first line — even if blank —
/// is what is parsed.)
pub fn read_first_line(path: &Path) -> Result<Option<String>> {
    let file = File::open(path)?;
    let mut reader = BufReader::new(file);
    let mut buf = String::new();
    if reader.read_line(&mut buf)? == 0 {
        return Ok(None);
    }
    Ok(Some(buf))
}

/// Read all lines from a file, returning `(line_number_1_indexed, line)` pairs.
/// Empty lines (per Rust's `BufRead::lines` semantics, which already strip
/// trailing `\n`) are skipped *before* numbering, mirroring the existing
/// loop pattern in `claude.rs`/`codex.rs`/`copilot.rs::parse_file`:
///
/// ```ignore
/// for (source_seq, line) in reader.lines().enumerate() {
///     let line = line?;
///     if line.trim().is_empty() { continue; }
///     let source_seq = (source_seq + 1) as i64;
///     ...
/// }
/// ```
///
/// The returned `line_number` is the *1-indexed enumerate index over the raw
/// stream*, NOT a count of non-blank lines, so fixtures relying on
/// `source_seq` line numbers continue to match disk positions.
pub fn read_all_lines(path: &Path) -> Result<Vec<(i64, String)>> {
    let file = File::open(path)?;
    let reader = BufReader::new(file);
    let mut lines = Vec::new();
    for (i, line) in reader.lines().enumerate() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        lines.push(((i + 1) as i64, line));
    }
    Ok(lines)
}
