use std::fs;
use std::path::PathBuf;

use assert_cmd::Command;

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
fn watch_once_ingests_directory() {
    let tmp = tempfile::TempDir::new().unwrap();
    let vault = tmp.path().join("data.sqlite");
    let watch_dir = tmp.path().join("watch");
    fs::create_dir(&watch_dir).unwrap();
    fs::copy(fixture("minimal.jsonl"), watch_dir.join("minimal.jsonl")).unwrap();

    osl()
        .args(["--vault", &vault.to_string_lossy(), "init"])
        .assert()
        .success();

    let output = osl()
        .args([
            "--vault",
            &vault.to_string_lossy(),
            "watch",
            "--once",
            &watch_dir.to_string_lossy(),
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("ingested"), "stdout: {stdout}");

    let grep = osl()
        .args(["--vault", &vault.to_string_lossy(), "grep", "Hello"])
        .output()
        .unwrap();
    assert!(
        grep.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&grep.stderr)
    );
    let grep_stdout = String::from_utf8_lossy(&grep.stdout);
    assert!(grep_stdout.contains("Hello"));
}

#[test]
#[cfg(target_os = "linux")]
fn watch_once_no_paths_uses_discovery() {
    let tmp = tempfile::TempDir::new().unwrap();
    let home = tmp.path();
    let projects = home.join(".claude").join("projects");
    fs::create_dir_all(&projects).unwrap();
    fs::copy(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("fixtures")
            .join("claude")
            .join("minimal.jsonl"),
        projects.join("minimal.jsonl"),
    )
    .unwrap();

    let vault = home.join("data.sqlite");

    osl()
        .env("HOME", home)
        .env("XDG_DATA_HOME", home)
        .env("XDG_CONFIG_HOME", home)
        .args(["--vault", &vault.to_string_lossy(), "init"])
        .assert()
        .success();

    let output = osl()
        .env("HOME", home)
        .env("XDG_DATA_HOME", home)
        .env("XDG_CONFIG_HOME", home)
        .args(["--vault", &vault.to_string_lossy(), "watch", "--once"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("ingested"), "stdout: {stdout}");

    let grep = osl()
        .env("HOME", home)
        .env("XDG_DATA_HOME", home)
        .env("XDG_CONFIG_HOME", home)
        .args(["--vault", &vault.to_string_lossy(), "grep", "Hello"])
        .output()
        .unwrap();
    assert!(
        grep.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&grep.stderr)
    );
    let grep_stdout = String::from_utf8_lossy(&grep.stdout);
    assert!(grep_stdout.contains("Hello"));
}
