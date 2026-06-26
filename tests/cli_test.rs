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
