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
fn cli_ingest_recency_skips_old_file() {
    let tmp = tempfile::TempDir::new().unwrap();
    let vault = tmp.path().join("data.sqlite");
    let session_file = tmp.path().join("session.jsonl");
    fs::copy(fixture("minimal.jsonl"), &session_file).unwrap();

    // Set mtime to 60 days ago using touch.
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let old = now - 60 * 86400;
    std::process::Command::new("touch")
        .arg("-d")
        .arg(format!("@{old}"))
        .arg(&session_file)
        .status()
        .unwrap();

    osl()
        .args(["--vault", &vault.to_string_lossy(), "init"])
        .assert()
        .success();

    let ingest = osl()
        .args([
            "--vault",
            &vault.to_string_lossy(),
            "ingest",
            "--recency",
            "30",
            &session_file.to_string_lossy(),
        ])
        .output()
        .unwrap();
    assert!(ingest.status.success());
    let stdout = String::from_utf8_lossy(&ingest.stdout);
    assert!(
        !stdout.contains("ingested"),
        "old file should be skipped: {stdout}"
    );

    let count: i64 = rusqlite::Connection::open(&vault)
        .unwrap()
        .query_row("SELECT COUNT(*) FROM sessions", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 0);
}

#[test]
fn cli_ingest_since_works() {
    let tmp = tempfile::TempDir::new().unwrap();
    let vault = tmp.path().join("data.sqlite");
    let session_file = tmp.path().join("session.jsonl");
    fs::copy(fixture("minimal.jsonl"), &session_file).unwrap();

    osl()
        .args(["--vault", &vault.to_string_lossy(), "init"])
        .assert()
        .success();

    osl()
        .args([
            "--vault",
            &vault.to_string_lossy(),
            "ingest",
            "--since",
            "2000-01-01",
            &session_file.to_string_lossy(),
        ])
        .assert()
        .success()
        .stdout(contains("ingested"));

    let count: i64 = rusqlite::Connection::open(&vault)
        .unwrap()
        .query_row("SELECT COUNT(*) FROM sessions", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 1);
}

#[test]
fn cli_ingest_both_flags_errors() {
    let tmp = tempfile::TempDir::new().unwrap();
    let vault = tmp.path().join("data.sqlite");
    let session_file = tmp.path().join("session.jsonl");
    fs::copy(fixture("minimal.jsonl"), &session_file).unwrap();

    osl()
        .args(["--vault", &vault.to_string_lossy(), "init"])
        .assert()
        .success();

    osl()
        .args([
            "--vault",
            &vault.to_string_lossy(),
            "ingest",
            "--recency",
            "1",
            "--since",
            "2026-01-01",
            &session_file.to_string_lossy(),
        ])
        .assert()
        .failure()
        .stderr(contains("mutually exclusive"));
}

#[test]
fn cli_embed_force_and_recency_smoke() {
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

    osl()
        .args([
            "--vault",
            &vault.to_string_lossy(),
            "embed",
            "--provider",
            &embed_fixture().to_string_lossy(),
            "--force",
            "--recency",
            "30",
        ])
        .assert()
        .success()
        .stdout(contains("embedded"));
}

#[test]
fn cli_embed_both_flags_errors() {
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
            "embed",
            "--provider",
            &embed_fixture().to_string_lossy(),
            "--recency",
            "1",
            "--since",
            "2026-01-01",
        ])
        .assert()
        .failure()
        .stderr(contains("mutually exclusive"));
}
