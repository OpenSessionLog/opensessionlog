use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

use assert_cmd::Command;
use predicates::str::contains;

fn fixture(name: &str) -> PathBuf {
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

#[test]
fn cli_init_and_ingest_and_grep_and_export() {
    let tmp = tempfile::TempDir::new().unwrap();
    let vault = tmp.path().join("data.sqlite");

    osl()
        .args(["--vault", &vault.to_string_lossy(), "init"])
        .assert()
        .success()
        .stdout(contains("initialized vault"));

    osl()
        .args([
            "--vault",
            &vault.to_string_lossy(),
            "ingest",
            &fixture("with_tool_call.jsonl").to_string_lossy(),
        ])
        .assert()
        .success()
        .stdout(contains("ingested"));

    let grep = osl()
        .args(["--vault", &vault.to_string_lossy(), "grep", "mock output"])
        .output()
        .unwrap();
    assert!(grep.status.success());
    let stdout = String::from_utf8_lossy(&grep.stdout);
    assert!(stdout.contains("mock output"));

    // Extract session id from grep output: `[<uuid>] user: ...`
    let session_id = stdout
        .split(']')
        .next()
        .unwrap()
        .trim_start_matches('[')
        .trim();

    osl()
        .args([
            "--vault",
            &vault.to_string_lossy(),
            "export",
            session_id,
            "--format",
            "markdown",
        ])
        .assert()
        .success()
        .stdout(contains("Tool: Bash"));
}

#[test]
fn cli_init_fails_when_vault_exists() {
    let tmp = tempfile::TempDir::new().unwrap();
    let vault = tmp.path().join("data.sqlite");

    osl()
        .args(["--vault", &vault.to_string_lossy(), "init"])
        .assert()
        .success();

    osl()
        .args(["--vault", &vault.to_string_lossy(), "init"])
        .assert()
        .failure();
}

#[test]
fn cli_embed_search_similar_smoke() {
    let tmp = tempfile::TempDir::new().unwrap();
    let vault = tmp.path().join("data.sqlite");

    osl()
        .args(["--vault", &vault.to_string_lossy(), "init"])
        .assert()
        .success();

    osl()
        .args([
            "--vault",
            &vault.to_string_lossy(),
            "ingest",
            &fixture("minimal.jsonl").to_string_lossy(),
        ])
        .assert()
        .success()
        .stdout(contains("ingested"));

    osl()
        .args([
            "--vault",
            &vault.to_string_lossy(),
            "embed",
            "--provider",
            &embed_fixture().to_string_lossy(),
            "--limit",
            "5",
        ])
        .assert()
        .success()
        .stdout(contains("embedded"));

    let search = osl()
        .args(["--vault", &vault.to_string_lossy(), "search", "hello"])
        .output()
        .unwrap();
    assert!(
        search.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&search.stderr)
    );
    let search_stdout = String::from_utf8_lossy(&search.stdout);
    assert!(search_stdout.starts_with('['), "stdout: {search_stdout}");

    // Extract session id from `[<uuid>] user: ...`
    let session_id = search_stdout
        .split(']')
        .next()
        .unwrap()
        .trim_start_matches('[')
        .trim();

    osl()
        .args([
            "--vault",
            &vault.to_string_lossy(),
            "export",
            session_id,
            "--format",
            "markdown",
        ])
        .assert()
        .success()
        .stdout(contains("Hello, Claude"));

    osl()
        .args(["--vault", &vault.to_string_lossy(), "similar", session_id])
        .assert()
        .success();
}

#[test]
fn cli_similar_bad_uuid_errors() {
    let tmp = tempfile::TempDir::new().unwrap();
    let vault = tmp.path().join("data.sqlite");

    osl()
        .args(["--vault", &vault.to_string_lossy(), "init"])
        .assert()
        .success();

    osl()
        .args(["--vault", &vault.to_string_lossy(), "similar", "not-a-uuid"])
        .assert()
        .failure()
        .stderr(contains("UUID error"));
}

#[test]
fn cli_similar_good_uuid_no_summary_graceful() {
    let tmp = tempfile::TempDir::new().unwrap();
    let vault = tmp.path().join("data.sqlite");

    osl()
        .args(["--vault", &vault.to_string_lossy(), "init"])
        .assert()
        .success();

    let ingest = osl()
        .args([
            "--vault",
            &vault.to_string_lossy(),
            "ingest",
            &fixture("minimal.jsonl").to_string_lossy(),
        ])
        .output()
        .unwrap();
    assert!(ingest.status.success());
    let ingest_stdout = String::from_utf8_lossy(&ingest.stdout);
    let session_id = ingest_stdout
        .split('(')
        .nth(1)
        .unwrap()
        .split(')')
        .next()
        .unwrap()
        .trim();

    osl()
        .args(["--vault", &vault.to_string_lossy(), "similar", session_id])
        .assert()
        .success()
        .stdout(contains("has no summary embedding"));
}
