//! Well-known source directory discovery for `osl setup`.
//!
//! Returns concrete filesystem paths and session counts per source, WITHOUT ingesting.
//! Connectors themselves remain the authority on parsing; this module only locates
//! the standard roots and asks each connector to `discover_filtered` against them.

use std::path::{Path, PathBuf};

use crate::connector::for_source;
use crate::error::Result;
use crate::model::SessionRef;
use crate::recency::RecencyFilter;

/// A single source's discovery result: where we looked, what we found.
#[derive(Debug, Clone)]
pub struct SourceDiscovery {
    /// Stable connector slug (e.g. "claude", "codex", "opencode", "hermes").
    pub source: &'static str,
    /// One or more roots that were actually probed on disk.
    pub roots_searched: Vec<PathBuf>,
    /// All session refs discovered across `roots_searched`.
    pub refs: Vec<SessionRef>,
    /// Per-root error strings (corrupt SQLite, unreadable dir, etc.). The whole
    /// `discover_all` call still returns `Ok` when these are non-empty.
    pub errors: Vec<String>,
}

impl SourceDiscovery {
    /// Total session count for this source (sum across refs).
    pub fn count(&self) -> usize {
        self.refs.len()
    }
}

/// The hard-coded source → default root(s) table, expanded against `$HOME`
/// (and `$XDG_*` with `$HOME`-based fallbacks). Returns roots that exist on disk
/// (directory or file). Non-existent roots are dropped silently — the caller learns
/// about absence via an empty `SourceDiscovery.roots_searched`.
///
/// Sources returned in a stable, human-friendly order: claude, codex, opencode, hermes.
/// (pi omitted until implemented.) If `$HOME` is unset, only the entries that don't
/// require `$HOME` (none of them do, today) are skipped; the function returns `vec![]`
/// rather than erroring.
pub fn default_roots() -> Vec<(&'static str, Vec<PathBuf>)> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default();
    default_roots_for_home(&home)
}

/// Same as `default_roots()` but takes an explicit `home` directory. This is the
/// testable core: in-process unit tests call this with a tempdir `home` and avoid
/// touching the process-wide `$HOME` env (which would race in the parallel harness).
/// `default_roots()` is a thin shim that reads `$HOME` and forwards here.
pub fn default_roots_for_home(home: &Path) -> Vec<(&'static str, Vec<PathBuf>)> {
    let mut result = Vec::new();

    // Claude Code: $HOME/.claude/projects/
    let claude_root = home.join(".claude").join("projects");
    if claude_root.exists() {
        result.push(("claude", vec![claude_root]));
    }

    // Codex CLI: $HOME/.codex/sessions/ and XDG fallback $HOME/.config/codex/sessions/
    let codex_primary = home.join(".codex").join("sessions");
    let codex_xdg = home.join(".config").join("codex").join("sessions");
    let codex_roots: Vec<PathBuf> = [codex_primary, codex_xdg]
        .into_iter()
        .filter(|p| p.exists())
        .collect();
    if !codex_roots.is_empty() {
        result.push(("codex", codex_roots));
    }

    // OpenCode: $HOME/.config/opencode/opencode.db
    let opencode_db = home.join(".config").join("opencode").join("opencode.db");
    if opencode_db.exists() {
        result.push(("opencode", vec![opencode_db]));
    }

    // Hermes: first existing of well-known state.db candidates.
    let xdg_data_home = std::env::var_os("XDG_DATA_HOME").map(PathBuf::from);
    let xdg_config_home = std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from);

    let hermes_candidates: Vec<PathBuf> = [
        Some(home.join(".hermes").join("state.db")),
        xdg_data_home.map(|p| p.join("hermes").join("state.db")),
        Some(
            home.join(".local")
                .join("share")
                .join("hermes")
                .join("state.db"),
        ),
        xdg_config_home.map(|p| p.join("hermes").join("state.db")),
        Some(home.join(".config").join("hermes").join("state.db")),
    ]
    .into_iter()
    .flatten()
    .collect();

    let hermes_roots: Vec<PathBuf> = hermes_candidates
        .into_iter()
        .filter(|p| p.exists())
        .take(1)
        .collect();
    if !hermes_roots.is_empty() {
        result.push(("hermes", hermes_roots));
    }

    // Copilot Chat (VS Code): workspaceStorage/<hash>/GitHub.copilot-chat
    // contains chatSessions/*.jsonl and transcripts/*.jsonl. On Linux the
    // User dir is ~/.config/Code/User; VS Code Insiders uses 'Code - Insiders'.
    // macOS/Windows roots are NOT added here by default (this runs on the
    // user's host); only HOME-based Linux paths are probed, mirroring how
    // other connectors stay platform-conservative. The connector itself
    // walks recursively from whatever roots exist.
    let copilot_roots: Vec<PathBuf> = [
        home.join(".config")
            .join("Code")
            .join("User")
            .join("workspaceStorage"),
        home.join(".config")
            .join("Code - Insiders")
            .join("User")
            .join("workspaceStorage"),
    ]
    .into_iter()
    .filter(|p| p.exists())
    .collect();
    if !copilot_roots.is_empty() {
        result.push(("copilot", copilot_roots));
    }

    result
}

/// Discover all available sources (roots that exist on disk get probed).
///
/// For each `(source, existing_roots)` returned by `default_roots`:
///   - Look up the connector via `crate::connector::for_source(source)`.
///     If None (e.g. future "pi" stub), skip silently and do NOT include in the result.
///   - Concatenate `connector.discover_filtered(root, &filter)` across all existing roots
///     for that source. On a per-root error (e.g. corrupt SQLite), record the error on
///     the `SourceDiscovery.errors` field — DO NOT abort the whole discover.
///
/// `filter` is the *ingest* window; setup calls this with `RecencyFilter::none()` for
/// the unfiltered "Found N total" summary. Discovery is idempotent and cheap.
pub fn discover_all(filter: &RecencyFilter) -> Result<Vec<SourceDiscovery>> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default();
    discover_all_for_home(&home, filter)
}

/// Same as `discover_all` but takes an explicit `home` directory (injectable for tests).
pub fn discover_all_for_home(home: &Path, filter: &RecencyFilter) -> Result<Vec<SourceDiscovery>> {
    let mut discoveries = Vec::new();

    for (source, roots) in default_roots_for_home(home) {
        let Some(connector) = for_source(source) else {
            // Future/unimplemented source (e.g. "pi") — skip silently.
            continue;
        };

        let mut refs = Vec::new();
        let mut errors = Vec::new();
        let roots_searched = roots.clone();

        for root in roots {
            match connector.discover_filtered(&root, filter) {
                Ok(mut found) => refs.append(&mut found),
                Err(e) => {
                    let msg = e.to_string();
                    // Empty-but-valid DBs (e.g. fresh state.db) should not produce warnings.
                    if !msg.contains("no sessions found") {
                        errors.push(format!("{}: {}", root.display(), msg));
                    }
                }
            }
        }

        discoveries.push(SourceDiscovery {
            source,
            roots_searched,
            refs,
            errors,
        });
    }

    Ok(discoveries)
}

/// Total session refs across all sources.
pub fn total_count(discoveries: &[SourceDiscovery]) -> usize {
    discoveries.iter().map(|d| d.count()).sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_roots_for_home_returns_expected_slugs() {
        let tmp = tempfile::TempDir::new().unwrap();
        // Empty tempdir: no known source roots exist.
        let roots = default_roots_for_home(tmp.path());
        let slugs: Vec<_> = roots.iter().map(|(s, _)| *s).collect();
        assert!(
            slugs.is_empty(),
            "expected no slugs in empty tmpdir, got {slugs:?}"
        );

        // Create a Claude projects directory.
        let claude_projects = tmp.path().join(".claude").join("projects");
        std::fs::create_dir_all(&claude_projects).unwrap();
        let roots = default_roots_for_home(tmp.path());
        let slugs: Vec<_> = roots.iter().map(|(s, _)| *s).collect();
        assert_eq!(slugs, vec!["claude"]);
        assert_eq!(roots[0].1, vec![claude_projects]);
    }

    #[test]
    fn total_count_sums_across_sources() {
        let a = SourceDiscovery {
            source: "claude",
            roots_searched: Vec::new(),
            refs: vec![SessionRef {
                source: "claude".to_string(),
                native_id: "a".to_string(),
                path: PathBuf::from("/tmp/a"),
                project_path: None,
            }],
            errors: Vec::new(),
        };
        let b = SourceDiscovery {
            source: "codex",
            roots_searched: Vec::new(),
            refs: vec![
                SessionRef {
                    source: "codex".to_string(),
                    native_id: "b1".to_string(),
                    path: PathBuf::from("/tmp/b1"),
                    project_path: None,
                },
                SessionRef {
                    source: "codex".to_string(),
                    native_id: "b2".to_string(),
                    path: PathBuf::from("/tmp/b2"),
                    project_path: None,
                },
            ],
            errors: Vec::new(),
        };
        assert_eq!(total_count(&[a, b]), 3);
    }
}
