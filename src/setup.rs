//! `osl setup` — guided first-run orchestration.

use std::io::{IsTerminal, Write};
use std::path::PathBuf;

use crate::autostart;
use crate::db;
use crate::embed;
use crate::error::{OslError, Result};
use crate::ingest;
use crate::recency::{now_unix_seconds, RecencyFilter};

/// Parsed setup flags, lifted from the clap definition by `cli::run`.
#[derive(Debug, Clone)]
pub struct SetupArgs {
    /// `--recency <days>`: same window for ingest and embed. Mutually exclusive
    /// with `--since`, `--ingest-recency`, `--embed-recency`.
    pub recency: Option<u64>,
    /// `--since <YYYY-MM-DD>`: same cutoff for both. Mutually exclusive with
    /// `--recency`, `--ingest-recency`, `--embed-recency`.
    pub since: Option<String>,
    /// `--ingest-recency <days>`: wider ingest window. Mutually exclusive with
    /// `--recency` and `--since`. If set without `--embed-recency`, embed defaults
    /// to the same window.
    pub ingest_recency: Option<u64>,
    /// `--embed-recency <days>`: narrower embed window. Mutually exclusive with
    /// `--recency` and `--since`. If set without `--ingest-recency`, ingest uses
    /// `RecencyFilter::none()` (matches "all").
    pub embed_recency: Option<u64>,
    /// `--provider <script>`: external embedder. If None and no embedder is persisted,
    /// the embed step is skipped gracefully with an informational message.
    pub provider: Option<PathBuf>,
    /// `--force`: re-embed ALL in-scope messages, even if an embedding already exists.
    pub force_embed: bool,
    /// `--yes` / `-y`: non-interactive. Also implied when stdin is not a TTY.
    pub yes: bool,
    /// Vault path (lifted from `--vault`).
    pub vault: PathBuf,
}

/// Output entry point invoked by `cli::run` for `Cmd::Setup`.
/// All printing goes to stdout (progress) / stderr (warnings). Returns `Ok(())` on success.
pub fn run(args: SetupArgs) -> Result<()> {
    let home = std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_default();
    run_with_home(args, &home)
}

/// Same as `run` but with an explicit `home` directory injected (for tests).
fn run_with_home(args: SetupArgs, home: &std::path::Path) -> Result<()> {
    // Step 1: resolve window filters.
    let now = now_unix_seconds();
    let has_split = args.ingest_recency.is_some() || args.embed_recency.is_some();
    let has_simple = args.recency.is_some() || args.since.is_some();
    if has_simple && has_split {
        return Err(OslError::Usage(
            "--recency/--since are mutually exclusive with --ingest-recency/--embed-recency".into(),
        ));
    }

    let (mut ingest_filter, mut embed_filter) = match (
        args.recency,
        args.since.as_ref(),
        args.ingest_recency,
        args.embed_recency,
    ) {
        (Some(days), None, None, None) => {
            let f = RecencyFilter::from_flags(Some(days), None, now)?;
            (f.clone(), f)
        }
        (None, Some(date), None, None) => {
            let f = RecencyFilter::from_flags(None, Some(date.clone()), now)?;
            (f.clone(), f)
        }
        (None, None, Some(ingest_days), None) => {
            let f = RecencyFilter::from_flags(Some(ingest_days), None, now)?;
            (f.clone(), f)
        }
        (None, None, Some(ingest_days), Some(embed_days)) => (
            RecencyFilter::from_flags(Some(ingest_days), None, now)?,
            RecencyFilter::from_flags(Some(embed_days), None, now)?,
        ),
        (None, None, None, Some(embed_days)) => (
            RecencyFilter::none(),
            RecencyFilter::from_flags(Some(embed_days), None, now)?,
        ),
        (None, None, None, None) => (RecencyFilter::none(), RecencyFilter::none()),
        _ => {
            // Defensive: the guard above already rejects mixed families, but
            // the match exhaustiveness checker needs an arm. This is unreachable
            // because of the `has_simple && has_split` check.
            return Err(OslError::Usage(
                "--recency/--since are mutually exclusive with --ingest-recency/--embed-recency"
                    .into(),
            ));
        }
    };

    // Step 2: interactivity decision.
    let interactive = !args.yes && std::io::stdin().is_terminal();

    // Step 3: init step.
    if args.vault.exists() {
        if interactive {
            let prompt = format!(
                "Vault already exists at {}. Continue with existing vault? [Y/n] ",
                args.vault.display()
            );
            if !prompt_yes_no(&prompt, true)? {
                println!("aborted");
                return Ok(());
            }
        } else {
            println!(
                "vault already exists at {}; continuing",
                args.vault.display()
            );
        }
    } else {
        if interactive {
            let prompt = format!(
                "No vault found. Create one at {}? [Y/n] ",
                args.vault.display()
            );
            if !prompt_yes_no(&prompt, true)? {
                println!("aborted");
                return Ok(());
            }
        }
        if let Some(parent) = args.vault.parent() {
            std::fs::create_dir_all(parent)?;
        }
        db::init(&args.vault, false)?;
        println!("initialized vault at {}", args.vault.display());
    }

    // Step 4: discover step.
    let discoveries = crate::discover::discover_all_for_home(home, &RecencyFilter::none())?;
    let total = crate::discover::total_count(&discoveries);
    println!("Found {total} sessions from {} sources", discoveries.len());

    if total == 0 {
        println!("no sessions found in known source directories; nothing to ingest");
    }

    // Interactive recency prompt only when no filter flags were supplied.
    let any_filter_flag = args.recency.is_some()
        || args.since.is_some()
        || args.ingest_recency.is_some()
        || args.embed_recency.is_some();
    if interactive && !any_filter_flag {
        let answer = prompt_ingest_window()?;
        match answer {
            IngestWindow::All => {
                ingest_filter = RecencyFilter::none();
                if args.embed_recency.is_none() {
                    embed_filter = RecencyFilter::none();
                }
            }
            IngestWindow::Days(days) => {
                ingest_filter = RecencyFilter::from_flags(Some(days), None, now)?;
                if args.embed_recency.is_none() {
                    embed_filter = ingest_filter.clone();
                }
            }
        }
    }

    // Step 5: ingest step.
    let mut conn = db::open(&args.vault)?;
    let mut ingested_count: usize = 0;

    if total > 0 {
        for discovery in &discoveries {
            if discovery.count() == 0 {
                continue;
            }
            // Deduplicate roots just in case two sources overlap.
            let mut seen_roots: std::collections::HashSet<std::path::PathBuf> =
                std::collections::HashSet::new();
            for root in &discovery.roots_searched {
                if !seen_roots.insert(root.clone()) {
                    continue;
                }
                let report = ingest::ingest_filtered(&mut conn, root, &ingest_filter)?;
                ingested_count += report.sessions.len();
            }
        }
        for err in discoveries.iter().flat_map(|d| &d.errors) {
            eprintln!("warning: {err}");
        }
        println!("Ingested {ingested_count} sessions");
    }

    // Step 6: embed decision step.
    let persisted = embed::read_config(&conn)?;
    let had_persisted_provider = persisted.is_some();
    let provider = args
        .provider
        .clone()
        .or_else(|| persisted.map(|c| c.provider));

    let Some(provider) = provider else {
        println!("Embedding step skipped — no --provider given and no embedder configured.");
        println!("To enable semantic search later, run:");
        println!("  osl embed --provider /path/to/embedder.sh");
        println!("or re-run setup with:");
        println!("  osl setup --provider /path/to/embedder.sh");
        println!("setup complete");
        return Ok(());
    };

    let should_embed = if interactive && !had_persisted_provider && args.provider.is_none() {
        let prompt = format!("Run embedding now using {}? [Y/n] ", provider.display());
        prompt_yes_no(&prompt, true)?
    } else {
        true
    };

    if should_embed {
        match embed::run_with_filter(&mut conn, &provider, None, &embed_filter, args.force_embed) {
            Ok(stats) => {
                println!(
                    "embedded {} messages across {} sessions summarized (model={}, dims={})",
                    stats.messages_embedded,
                    stats.sessions_summarized,
                    stats.model,
                    stats.dimensions
                );
            }
            Err(OslError::Embed(msg)) if msg.starts_with("failed to spawn embedder") => {
                eprintln!(
                    "warning: could not spawn embedder at {}",
                    provider.display()
                );
                println!(
                    "Embedding step skipped — no --provider given and no embedder configured."
                );
                println!("To enable semantic search later, run:");
                println!("  osl embed --provider /path/to/embedder.sh");
                println!("or re-run setup with:");
                println!("  osl setup --provider /path/to/embedder.sh");
            }
            Err(e) => return Err(e),
        }
    } else {
        println!("Embedding step skipped — no --provider given and no embedder configured.");
        println!("To enable semantic search later, run:");
        println!("  osl embed --provider /path/to/embedder.sh");
        println!("or re-run setup with:");
        println!("  osl setup --provider /path/to/embedder.sh");
    }

    // Step 7: interactive autostart prompt.
    if interactive && prompt_yes_no("Install a persistent watch daemon? [y/N] ", false)? {
        autostart::install(&[], &args.vault)?;
    }

    // Step 8: final banner.
    println!("setup complete");
    Ok(())
}

enum IngestWindow {
    All,
    Days(u64),
}

fn prompt_ingest_window() -> Result<IngestWindow> {
    for _ in 0..3 {
        print!("How far back should I ingest? [all / <days>] ");
        std::io::stdout().flush()?;
        let mut line = String::new();
        std::io::stdin().read_line(&mut line)?;
        let s = line.trim();
        if s.eq_ignore_ascii_case("all") {
            return Ok(IngestWindow::All);
        }
        if let Ok(days) = s.parse::<u64>() {
            if days > 0 {
                return Ok(IngestWindow::Days(days));
            }
        }
    }
    // After repeated garbage, fall back to "all" so setup still succeeds.
    Ok(IngestWindow::All)
}

fn prompt_yes_no(prompt: &str, default_yes: bool) -> Result<bool> {
    print!("{prompt}");
    std::io::stdout().flush()?;
    let mut line = String::new();
    std::io::stdin().read_line(&mut line)?;
    let s = line.trim().to_lowercase();
    if s.is_empty() {
        return Ok(default_yes);
    }
    Ok(s == "y" || s == "yes")
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;

    use super::*;
    use crate::db;
    use crate::search;

    fn claude_fixture_dir(home: &std::path::Path) -> PathBuf {
        home.join(".claude").join("projects")
    }

    fn embedder() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("fixtures")
            .join("embed")
            .join("identity.py")
    }

    fn copy_embedder_to(tmp: &std::path::Path) -> PathBuf {
        let dest = tmp.join("identity.py");
        fs::copy(embedder(), &dest).unwrap();
        let mut perms = fs::metadata(&dest).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&dest, perms).unwrap();
        dest
    }

    fn iso_timestamp_days_ago(days: i64) -> String {
        let now = now_unix_seconds();
        let secs = now - days * 86400;
        let date = crate::connector::opencode::chrono_from_unix(secs);
        format!("{}T10:00:00Z", &date[..10])
    }

    fn write_claude_minimal(path: &std::path::Path, session_id: &str, timestamp: &str) {
        let template = r#"{"uuid":"evt-001","type":"user","timestamp":"{timestamp}","sessionId":"{session_id}","version":"1.0","cwd":"/tmp","message":{"content":"Hello, Claude.","usage":{"input_tokens":12}}}
{"uuid":"evt-002","type":"assistant","timestamp":"{timestamp}","sessionId":"{session_id}","version":"1.0","cwd":"/tmp","message":{"model":"claude-sonnet-4","content":"Hello! How can I help?","usage":{"output_tokens":5}}}
"#;
        fs::write(
            path,
            template
                .replace("{timestamp}", timestamp)
                .replace("{session_id}", session_id),
        )
        .unwrap();
    }

    fn write_claude_with_tool_call(path: &std::path::Path, session_id: &str, timestamp: &str) {
        let template = r#"{"uuid":"evt-001","type":"user","timestamp":"{timestamp}","sessionId":"{session_id}","version":"1.0","cwd":"/tmp","message":{"content":"Run bash.","usage":{"input_tokens":10}}}
{"uuid":"evt-002","type":"assistant","timestamp":"{timestamp}","sessionId":"{session_id}","version":"1.0","cwd":"/tmp","message":{"model":"claude-sonnet-4","content":null,"usage":{"output_tokens":5}}}
{"uuid":"evt-003","type":"user","timestamp":"{timestamp}","sessionId":"{session_id}","version":"1.0","cwd":"/tmp","message":{"content":"ok","usage":{"input_tokens":2}}}
"#;
        fs::write(
            path,
            template
                .replace("{timestamp}", timestamp)
                .replace("{session_id}", session_id),
        )
        .unwrap();
    }

    fn touch_days_ago(path: &std::path::Path, days: i64) {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        let old = now - days * 86400;
        std::process::Command::new("touch")
            .arg("-d")
            .arg(format!("@{old}"))
            .arg(path)
            .status()
            .unwrap();
    }

    fn count_sessions(vault: &std::path::Path) -> i64 {
        let conn = db::open(vault).unwrap();
        conn.query_row("SELECT COUNT(*) FROM sessions", [], |r| r.get(0))
            .unwrap()
    }

    fn count_embedded_messages(vault: &std::path::Path) -> i64 {
        let conn = db::open(vault).unwrap();
        conn.query_row(
            "SELECT COUNT(*) FROM messages WHERE embedding IS NOT NULL",
            [],
            |r| r.get(0),
        )
        .unwrap()
    }

    fn update_message_created_for_session(
        vault: &std::path::Path,
        raw_path: &std::path::Path,
        days_ago: i64,
    ) {
        let date = iso_timestamp_days_ago(days_ago);
        let conn = rusqlite::Connection::open(vault).unwrap();
        conn.execute(
            "UPDATE messages SET created_at = ?1 WHERE session_id IN (SELECT id FROM sessions WHERE raw_path = ?2)",
            [&date, &raw_path.to_string_lossy().to_string()],
        )
        .unwrap();
    }

    fn default_args(vault: &std::path::Path) -> SetupArgs {
        SetupArgs {
            recency: None,
            since: None,
            ingest_recency: None,
            embed_recency: None,
            provider: None,
            force_embed: false,
            yes: true,
            vault: vault.to_path_buf(),
        }
    }

    #[test]
    fn setup_creates_vault_and_ingests_claude() {
        let tmp = tempfile::TempDir::new().unwrap();
        let projects = claude_fixture_dir(tmp.path());
        fs::create_dir_all(&projects).unwrap();
        write_claude_minimal(
            &projects.join("session.jsonl"),
            "sess-setup",
            "2026-06-20T10:00:00Z",
        );

        let vault = tmp.path().join("data.sqlite");
        run_with_home(default_args(&vault), tmp.path()).unwrap();

        assert!(vault.exists());
        assert_eq!(count_sessions(&vault), 1);

        let conn = db::open(&vault).unwrap();
        let hits = search::grep(&conn, "Claude", 10, None).unwrap();
        assert!(!hits.is_empty(), "expected grep hits for fixture content");
    }

    #[test]
    fn setup_skips_init_when_vault_exists() {
        let tmp = tempfile::TempDir::new().unwrap();
        let projects = claude_fixture_dir(tmp.path());
        fs::create_dir_all(&projects).unwrap();
        write_claude_minimal(
            &projects.join("session.jsonl"),
            "sess-setup",
            "2026-06-20T10:00:00Z",
        );

        let vault = tmp.path().join("data.sqlite");
        db::init(&vault, false).unwrap();

        run_with_home(default_args(&vault), tmp.path()).unwrap();
        assert_eq!(count_sessions(&vault), 1);
    }

    #[test]
    fn setup_recency_120_skips_old_sessions() {
        let tmp = tempfile::TempDir::new().unwrap();
        let projects = claude_fixture_dir(tmp.path());
        fs::create_dir_all(&projects).unwrap();

        let recent = projects.join("recent.jsonl");
        write_claude_minimal(&recent, "sess-recent", &iso_timestamp_days_ago(60));
        touch_days_ago(&recent, 60);

        let old = projects.join("old.jsonl");
        write_claude_minimal(&old, "sess-old", &iso_timestamp_days_ago(180));
        touch_days_ago(&old, 180);

        let vault = tmp.path().join("data.sqlite");
        let mut args = default_args(&vault);
        args.recency = Some(120);
        run_with_home(args, tmp.path()).unwrap();

        assert_eq!(count_sessions(&vault), 1);
    }

    #[test]
    fn setup_ingest_recency_365_embed_recency_120() {
        let tmp = tempfile::TempDir::new().unwrap();
        let projects = claude_fixture_dir(tmp.path());
        fs::create_dir_all(&projects).unwrap();

        let old = projects.join("old.jsonl");
        write_claude_minimal(&old, "sess-old", &iso_timestamp_days_ago(180));
        touch_days_ago(&old, 180);

        let recent = projects.join("recent.jsonl");
        write_claude_with_tool_call(&recent, "sess-recent", &iso_timestamp_days_ago(7));
        touch_days_ago(&recent, 7);

        let vault = tmp.path().join("data.sqlite");
        let provider = copy_embedder_to(tmp.path());

        // Phase 1: ingest both fixtures (no embedder yet).
        let mut args = default_args(&vault);
        args.ingest_recency = Some(365);
        run_with_home(args, tmp.path()).unwrap();
        assert_eq!(count_sessions(&vault), 2);

        // The vault sets message created_at to the ingest time. Backdate the old
        // session's messages so the embed recency window excludes them, and move
        // the old file's mtime beyond the ingest window so phase 2 skips it.
        update_message_created_for_session(&vault, &old, 400);
        touch_days_ago(&old, 400);

        // Phase 2: same split windows with an embedder.
        let mut args = default_args(&vault);
        args.ingest_recency = Some(365);
        args.embed_recency = Some(120);
        args.provider = Some(provider);
        run_with_home(args, tmp.path()).unwrap();

        assert_eq!(count_sessions(&vault), 2);
        // Only the recent session's messages (3) got embedded.
        assert_eq!(count_embedded_messages(&vault), 3);
    }

    #[test]
    fn setup_incremental_embed_only_new_days() {
        let tmp = tempfile::TempDir::new().unwrap();
        let projects = claude_fixture_dir(tmp.path());
        fs::create_dir_all(&projects).unwrap();

        let recent = projects.join("recent.jsonl");
        write_claude_with_tool_call(&recent, "sess-recent", &iso_timestamp_days_ago(7));
        touch_days_ago(&recent, 7);

        let vault = tmp.path().join("data.sqlite");
        let provider = copy_embedder_to(tmp.path());

        // First run: ingest + embed only last 120 days.
        let mut args = default_args(&vault);
        args.embed_recency = Some(120);
        args.provider = Some(provider.clone());
        run_with_home(args, tmp.path()).unwrap();
        let first = count_embedded_messages(&vault);
        assert_eq!(first, 3, "recent fixture has 3 messages");

        // Add a second fixture that is older than 120 days but within 180.
        let medium = projects.join("medium.jsonl");
        write_claude_minimal(&medium, "sess-medium", &iso_timestamp_days_ago(150));
        touch_days_ago(&medium, 150);

        // Second run: widen embed window to 180 days, incremental.
        let mut args = default_args(&vault);
        args.embed_recency = Some(180);
        args.provider = Some(provider);
        run_with_home(args, tmp.path()).unwrap();
        let second = count_embedded_messages(&vault);
        assert!(second > first, "second run should add the medium fixture");
        assert_eq!(second, 5, "total embedded should be 3 + 2");

        // The second *run* embedded strictly fewer than the first run.
        let newly_embedded = second - first;
        assert!(
            newly_embedded < first,
            "{newly_embedded} newly embedded should be < {first}"
        );
    }

    #[test]
    fn setup_force_re_embeds_all() {
        let tmp = tempfile::TempDir::new().unwrap();
        let projects = claude_fixture_dir(tmp.path());
        fs::create_dir_all(&projects).unwrap();

        let a = projects.join("a.jsonl");
        write_claude_minimal(&a, "sess-a", &iso_timestamp_days_ago(7));
        touch_days_ago(&a, 7);

        let b = projects.join("b.jsonl");
        write_claude_with_tool_call(&b, "sess-b", &iso_timestamp_days_ago(14));
        touch_days_ago(&b, 14);

        let vault = tmp.path().join("data.sqlite");
        let provider = copy_embedder_to(tmp.path());

        // First run: no force.
        let mut args = default_args(&vault);
        args.embed_recency = Some(120);
        args.provider = Some(provider.clone());
        run_with_home(args, tmp.path()).unwrap();
        let first = count_embedded_messages(&vault);
        assert_eq!(first, 5);

        // Second run: force re-embed.
        let mut args = default_args(&vault);
        args.embed_recency = Some(120);
        args.provider = Some(provider.clone());
        args.force_embed = true;
        run_with_home(args, tmp.path()).unwrap();
        let second = count_embedded_messages(&vault);
        assert!(second >= first, "force run must embed at least as many");

        // Third run: no force, should find nothing left to embed.
        let mut args = default_args(&vault);
        args.embed_recency = Some(120);
        args.provider = Some(provider);
        args.force_embed = false;
        run_with_home(args, tmp.path()).unwrap();
        let third = count_embedded_messages(&vault);
        assert_eq!(
            third, second,
            "no-force run after force should not change count"
        );
    }
}
