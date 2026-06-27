use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

use assert_cmd::Command;
use predicates::str::contains;

fn claude_fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("claude")
        .join(name)
}

fn embed_fixture() -> PathBuf {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("embed")
        .join("identity.py");
    let mut perms = fs::metadata(&path).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&path, perms).unwrap();
    path
}

fn osl() -> Command {
    Command::cargo_bin("osl").unwrap()
}

fn count_embedded_messages(vault: &std::path::Path) -> i64 {
    let conn = rusqlite::Connection::open(vault).unwrap();
    conn.query_row(
        "SELECT COUNT(*) FROM messages WHERE embedding IS NOT NULL",
        [],
        |r| r.get(0),
    )
    .unwrap()
}

#[test]
fn setup_no_flags_ingests_all_skips_embed() {
    let tmp = tempfile::TempDir::new().unwrap();
    let home = tmp.path();
    let projects = home.join(".claude").join("projects");
    fs::create_dir_all(&projects).unwrap();
    fs::copy(
        claude_fixture("minimal.jsonl"),
        projects.join("session.jsonl"),
    )
    .unwrap();

    let vault = home.join("data.sqlite");

    let output = osl()
        .env("HOME", home)
        .args(["--vault", &vault.to_string_lossy(), "setup", "--yes"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Embedding step skipped"),
        "stdout: {stdout}"
    );

    assert!(vault.exists());
    let conn = rusqlite::Connection::open(&vault).unwrap();
    let sessions: i64 = conn
        .query_row("SELECT COUNT(*) FROM sessions", [], |r| r.get(0))
        .unwrap();
    assert_eq!(sessions, 1);
    assert_eq!(count_embedded_messages(&vault), 0);
}

#[test]
fn setup_with_provider_runs_embed() {
    let tmp = tempfile::TempDir::new().unwrap();
    let home = tmp.path();
    let projects = home.join(".claude").join("projects");
    fs::create_dir_all(&projects).unwrap();
    fs::copy(
        claude_fixture("minimal.jsonl"),
        projects.join("session.jsonl"),
    )
    .unwrap();

    let vault = home.join("data.sqlite");

    osl()
        .env("HOME", home)
        .args([
            "--vault",
            &vault.to_string_lossy(),
            "setup",
            "--yes",
            "--provider",
            &embed_fixture().to_string_lossy(),
        ])
        .assert()
        .success()
        .stdout(contains("embedded "));

    assert!(count_embedded_messages(&vault) > 0);
}

#[test]
fn setup_no_provider_prints_skip_info() {
    let tmp = tempfile::TempDir::new().unwrap();
    let home = tmp.path();
    let projects = home.join(".claude").join("projects");
    fs::create_dir_all(&projects).unwrap();
    fs::copy(
        claude_fixture("minimal.jsonl"),
        projects.join("session.jsonl"),
    )
    .unwrap();

    let vault = home.join("data.sqlite");

    osl()
        .env("HOME", home)
        .args(["--vault", &vault.to_string_lossy(), "setup", "--yes"])
        .assert()
        .success()
        .stdout(contains(
            "Embedding step skipped — no --provider given and no embedder configured.",
        ));
}

#[test]
fn setup_piped_std_skips_prompts() {
    let tmp = tempfile::TempDir::new().unwrap();
    let home = tmp.path();
    let projects = home.join(".claude").join("projects");
    fs::create_dir_all(&projects).unwrap();
    fs::copy(
        claude_fixture("minimal.jsonl"),
        projects.join("session.jsonl"),
    )
    .unwrap();

    let vault = home.join("data.sqlite");

    let output = osl()
        .env("HOME", home)
        .write_stdin("")
        .args(["--vault", &vault.to_string_lossy(), "setup"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Embedding step skipped"),
        "stdout: {stdout}"
    );
    assert!(vault.exists());
}

#[test]
fn setup_rerun_idempotent() {
    let tmp = tempfile::TempDir::new().unwrap();
    let home = tmp.path();
    let projects = home.join(".claude").join("projects");
    fs::create_dir_all(&projects).unwrap();
    fs::copy(
        claude_fixture("minimal.jsonl"),
        projects.join("session.jsonl"),
    )
    .unwrap();

    let vault = home.join("data.sqlite");

    // First run: create vault and embed.
    let first = osl()
        .env("HOME", home)
        .args([
            "--vault",
            &vault.to_string_lossy(),
            "setup",
            "--yes",
            "--provider",
            &embed_fixture().to_string_lossy(),
        ])
        .output()
        .unwrap();
    assert!(first.status.success());
    let first_stdout = String::from_utf8_lossy(&first.stdout);
    assert!(first_stdout.contains("initialized vault"));
    let first_embedded = count_embedded_messages(&vault);
    assert!(first_embedded > 0);

    // Second run: no --provider; should reuse persisted config and embed nothing new.
    let second = osl()
        .env("HOME", home)
        .args(["--vault", &vault.to_string_lossy(), "setup", "--yes"])
        .output()
        .unwrap();
    assert!(second.status.success());
    let second_stdout = String::from_utf8_lossy(&second.stdout);
    assert!(
        !second_stdout.contains("initialized vault"),
        "rerun should not re-initialize: {second_stdout}"
    );
    let second_embedded = count_embedded_messages(&vault);
    assert_eq!(second_embedded, first_embedded);
}
