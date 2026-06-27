//! Shared recency filter used by `osl ingest` and `osl embed`.
//!
//! `--recency <days>` and `--since <YYYY-MM-DD>` are mutually exclusive. Both
//! reduce to a single inclusive lower-bound cutoff date applied as
//!   - `DATE(<timestamp_column>) >= <cutoff_date>` in SQL queries, and
//!   - `file_mtime_secs >= <cutoff_unix>` for the JSONL mtime pre-filter.
//!
//! All date arithmetic is pure Rust (no chrono), reusing the civil-date routine
//! in `crate::connector::opencode::chrono_from_unix`.

use std::path::Path;
use std::time::UNIX_EPOCH;

use crate::connector::opencode::chrono_from_unix;
use crate::error::{OslError, Result};

#[derive(Debug, Clone, PartialEq)]
pub struct RecencyFilter {
    /// Inclusive lower bound as `YYYY-MM-DD` (UTC). `None` = no filter (ingest/embed
    /// everything). Used as the SQL bind parameter for vault `messages.created_at`
    /// filtering and for SQLite source timestamp filtering (formatted per source).
    since_date: Option<String>,
    /// Same cutoff expressed as Unix seconds at `00:00:00 UTC` of `since_date`. Used
    /// for the JSONL file-mtime pre-filter and for the SQLite source timestamp queries
    /// (Hermes: seconds; OpenCode: seconds × 1000 for ms).
    since_unix: Option<i64>,
}

impl RecencyFilter {
    /// No filtering — ingest/embed everything (current behavior).
    pub fn none() -> Self {
        Self {
            since_date: None,
            since_unix: None,
        }
    }

    /// Construct from parsed CLI flags. `now_secs` is current Unix seconds (injected
    /// for deterministic testing — the CLI passes `now_unix_seconds()`). Returns
    /// `OslError::Usage` on mutual-exclusion violation or invalid date.
    pub fn from_flags(recency: Option<u64>, since: Option<String>, now_secs: i64) -> Result<Self> {
        match (recency, since) {
            (None, None) => Ok(Self::none()),
            (Some(_), Some(_)) => Err(OslError::Usage(
                "--recency and --since are mutually exclusive".into(),
            )),
            (Some(days), None) => {
                if days == 0 {
                    return Err(OslError::Usage("--recency must be >= 1 day".into()));
                }
                // Floor "now" to midnight UTC (Unix epoch days align with UTC midnight),
                // then subtract N days. A file modified any time during (today - N days)
                // or later qualifies as "in the last N days".
                let today_midnight = (now_secs / 86400) * 86400;
                let cutoff_unix = today_midnight - (days as i64) * 86400;
                let date = chrono_from_unix(cutoff_unix);
                Ok(Self {
                    since_date: Some(date[..10].to_string()),
                    since_unix: Some(cutoff_unix),
                })
            }
            (None, Some(date)) => {
                let unix = ymd_to_unix_seconds(&date)?;
                Ok(Self {
                    since_date: Some(date),
                    since_unix: Some(unix),
                })
            }
        }
    }

    /// True when a recency/since cutoff is active.
    pub fn is_active(&self) -> bool {
        self.since_date.is_some()
    }

    /// The `YYYY-MM-DD` cutoff (inclusive lower bound), or `None` when inactive.
    pub fn since_date(&self) -> Option<&str> {
        self.since_date.as_deref()
    }

    /// The cutoff as Unix seconds at `00:00:00 UTC`, or `None` when inactive.
    pub fn since_unix(&self) -> Option<i64> {
        self.since_unix
    }

    /// File-mtime pre-filter for JSONL sources. Returns `true` if the file should be
    /// parsed. Semantics:
    ///   - `None` filter → always `true` (don't filter).
    ///   - `Some(cutoff)` → `true` iff `mtime_secs >= cutoff` (inclusive on the cutoff
    ///     date).
    ///   - On stat failure → `true` (permissive: let the connector's `parse()` surface
    ///     the real error rather than silently dropping the file).
    ///   - Pre-epoch mtime → `false` (cannot represent, exclude).
    pub fn keep_file(&self, path: &Path) -> bool {
        let Some(cutoff) = self.since_unix else {
            return true;
        };
        let Ok(md) = std::fs::metadata(path) else {
            return true;
        };
        let Ok(mtime) = md.modified() else {
            return true;
        };
        match mtime.duration_since(UNIX_EPOCH) {
            Ok(d) => (d.as_secs() as i64) >= cutoff,
            Err(_) => false,
        }
    }
}

/// Parse a strict `YYYY-MM-DD` string into Unix seconds at `00:00:00 UTC`.
/// Uses Howard Hinnant's civil-from-days algorithm (inverse of `chrono_from_unix`).
fn ymd_to_unix_seconds(date: &str) -> Result<i64> {
    let b = date.as_bytes();
    if b.len() != 10 || b[4] != b'-' || b[7] != b'-' {
        return Err(OslError::Usage(format!(
            "--since expects YYYY-MM-DD, got '{date}'"
        )));
    }
    let s = std::str::from_utf8(b)
        .map_err(|_| OslError::Usage(format!("--since expects YYYY-MM-DD, got '{date}'")))?;
    let y: i64 = s[0..4]
        .parse()
        .map_err(|_| OslError::Usage(format!("--since expects YYYY-MM-DD, got '{date}'")))?;
    let m: i64 = s[5..7]
        .parse()
        .map_err(|_| OslError::Usage(format!("--since expects YYYY-MM-DD, got '{date}'")))?;
    let d: i64 = s[8..10]
        .parse()
        .map_err(|_| OslError::Usage(format!("--since expects YYYY-MM-DD, got '{date}'")))?;
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return Err(OslError::Usage(format!(
            "--since date out of range: '{date}'"
        )));
    }
    let y_adj = if m <= 2 { y - 1 } else { y };
    let era = if y_adj >= 0 { y_adj } else { y_adj - 399 } / 400;
    let yoe = y_adj - era * 400;
    let mp = if m > 2 { m - 3 } else { m + 9 };
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    Ok(days * 86400)
}

/// Return today's date as `YYYY-MM-DD` for the given Unix seconds.
/// Convenience helper used by integration tests that need `--since <today>`.
pub fn today_ymd(now_secs: i64) -> String {
    chrono_from_unix(now_secs)[..10].to_string()
}

/// Current Unix seconds from `SystemTime::now()`. Panics only if the system clock is
/// pre-1970 (unreachable in practice).
pub fn now_unix_seconds() -> i64 {
    std::time::SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock is pre-1970")
        .as_secs() as i64
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, SystemTime};

    #[test]
    fn none_is_inactive() {
        let f = RecencyFilter::none();
        assert!(!f.is_active());
        assert_eq!(f.since_date(), None);
        assert_eq!(f.since_unix(), None);
    }

    #[test]
    fn recency_and_since_are_mutually_exclusive() {
        let err = RecencyFilter::from_flags(Some(30), Some("2026-06-01".into()), 1_800_000_000);
        assert!(err.is_err());
        let msg = err.unwrap_err().to_string();
        assert!(msg.contains("mutually exclusive"), "got: {msg}");
    }

    #[test]
    fn recency_and_since_neither_is_none() {
        let f = RecencyFilter::from_flags(None, None, 1_800_000_000).unwrap();
        assert!(!f.is_active());
    }

    #[test]
    fn recency_zero_errors() {
        let err = RecencyFilter::from_flags(Some(0), None, 1_800_000_000);
        assert!(err.is_err());
    }

    #[test]
    fn recency_30_floors_to_midnight_minus_30_days() {
        // Fixed now 2027-01-01 00:48:16 UTC = 1798764496. Floor to midnight
        // (1798764496 / 86400 * 86400 = 1798761600 = 2027-01-01 00:00 UTC).
        // Minus 30 days = 1798761600 - 30*86400 = 1796169600 = 2026-12-02 00:00 UTC.
        let now = 1_798_764_496_i64;
        let f = RecencyFilter::from_flags(Some(30), None, now).unwrap();
        assert_eq!(f.since_date(), Some("2026-12-02"));
        assert_eq!(f.since_unix(), Some(1_796_169_600));
    }

    #[test]
    fn since_valid_date_parses_to_midnight_utc() {
        let f = RecencyFilter::from_flags(None, Some("2026-06-01".into()), 0).unwrap();
        assert!(f.is_active());
        assert_eq!(f.since_date(), Some("2026-06-01"));
        // 2026-06-01 00:00:00 UTC = 1780272000 (civil-date algorithm).
        assert_eq!(f.since_unix(), Some(1_780_272_000));
    }

    #[test]
    fn since_rejects_bad_format() {
        assert!(RecencyFilter::from_flags(None, Some("2026/06/01".into()), 0).is_err());
        assert!(RecencyFilter::from_flags(None, Some("2026-6-1".into()), 0).is_err());
        assert!(RecencyFilter::from_flags(None, Some("garbage".into()), 0).is_err());
        assert!(RecencyFilter::from_flags(None, Some("2026-13-01".into()), 0).is_err());
        assert!(RecencyFilter::from_flags(None, Some("2026-06-32".into()), 0).is_err());
    }

    #[test]
    fn keep_file_unfiltered_always_true() {
        let f = RecencyFilter::none();
        // Non-existent path: permissive → true.
        assert!(f.keep_file(Path::new("/no/such/path/anywhere")));
    }

    #[test]
    fn keep_file_respects_cutoff() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let path = tmp.path();
        // Touch mtime to "now - 60 days". Use RecencyFilter with --recency 30 so cutoff
        // is "now floored to midnight - 30 days". File mtime (60 days old) < cutoff →
        // must be excluded.
        let now = now_unix_seconds();
        let old = now - 60 * 86400;
        let f = std::fs::File::open(path).unwrap();
        f.set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(old as u64))
            .unwrap();
        drop(f);
        let f = RecencyFilter::from_flags(Some(30), None, now).unwrap();
        assert!(!f.keep_file(path));

        // Set mtime to "now" → passes.
        let f2 = std::fs::File::open(path).unwrap();
        f2.set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(now as u64))
            .unwrap();
        drop(f2);
        assert!(f.keep_file(path));
    }
}
