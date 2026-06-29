use std::fs;
use std::path::PathBuf;

use assert_cmd::Command;

fn osl() -> Command {
    Command::cargo_bin("osl").unwrap()
}

#[test]
#[cfg(target_os = "linux")]
fn autostart_writes_systemd_unit() {
    let tmp = tempfile::TempDir::new().unwrap();
    let home = tmp.path();
    let vault = home.join("vault.sqlite");

    let output = osl()
        .env("HOME", home)
        .args(["--vault", &vault.to_string_lossy(), "autostart"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let service = home.join(".config/systemd/user/osl-watch.service");
    assert!(service.exists());
    let content = fs::read_to_string(&service).unwrap();
    assert!(content.contains("ExecStart="));
    assert!(content.contains("--interval 60"));
    assert!(content.contains("--vault"));
    assert!(
        !content.contains(".claude/projects"),
        "no source paths should appear when none given"
    );
}

#[test]
#[cfg(target_os = "linux")]
fn autostart_with_paths_embeds_them() {
    let tmp = tempfile::TempDir::new().unwrap();
    let home = tmp.path();
    let custom = home.join("custom");
    fs::create_dir(&custom).unwrap();
    let vault = home.join("vault.sqlite");

    let output = osl()
        .env("HOME", home)
        .args([
            "--vault",
            &vault.to_string_lossy(),
            "autostart",
            &custom.to_string_lossy(),
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let service = home.join(".config/systemd/user/osl-watch.service");
    let content = fs::read_to_string(&service).unwrap();
    assert!(content.contains(&*custom.to_string_lossy()));
}

#[test]
#[cfg(target_os = "linux")]
fn autostart_remove_idempotent() {
    let tmp = tempfile::TempDir::new().unwrap();
    let home = tmp.path();

    let output = osl()
        .env("HOME", home)
        .args(["autostart", "--remove"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("No autostart config found"));
}

#[test]
#[cfg(target_os = "linux")]
fn autostart_install_then_remove() {
    let tmp = tempfile::TempDir::new().unwrap();
    let home = tmp.path();
    let vault = home.join("vault.sqlite");

    let out1 = osl()
        .env("HOME", home)
        .args(["--vault", &vault.to_string_lossy(), "autostart"])
        .output()
        .unwrap();
    assert!(out1.status.success());
    let service = home.join(".config/systemd/user/osl-watch.service");
    assert!(service.exists());

    let out2 = osl()
        .env("HOME", home)
        .args(["autostart", "--remove"])
        .output()
        .unwrap();
    assert!(out2.status.success());
    let stdout2 = String::from_utf8_lossy(&out2.stdout);
    assert!(stdout2.contains("systemctl --user disable --now"));
    assert!(!service.exists());
}

#[test]
#[cfg(target_os = "linux")]
fn autostart_overwrite_says_updated() {
    let tmp = tempfile::TempDir::new().unwrap();
    let home = tmp.path();
    let vault = home.join("vault.sqlite");

    let out1 = osl()
        .env("HOME", home)
        .args(["--vault", &vault.to_string_lossy(), "autostart"])
        .output()
        .unwrap();
    assert!(out1.status.success());
    let stdout1 = String::from_utf8_lossy(&out1.stdout);
    assert!(stdout1.contains("enable --now"));
    let content1 = fs::read_to_string(home.join(".config/systemd/user/osl-watch.service")).unwrap();

    let out2 = osl()
        .env("HOME", home)
        .args(["--vault", &vault.to_string_lossy(), "autostart"])
        .output()
        .unwrap();
    assert!(out2.status.success());
    let stdout2 = String::from_utf8_lossy(&out2.stdout);
    assert!(stdout2.contains("restart"));
    assert!(!stdout2.contains("enable --now"));
    let content2 = fs::read_to_string(home.join(".config/systemd/user/osl-watch.service")).unwrap();

    assert_eq!(
        content1, content2,
        "re-running autostart with identical inputs should produce identical content"
    );
}

#[test]
#[cfg(target_os = "linux")]
fn autostart_explicit_then_auto_overwrite_clears_paths() {
    let tmp = tempfile::TempDir::new().unwrap();
    let home = tmp.path();
    let custom = home.join("custom");
    fs::create_dir(&custom).unwrap();
    let vault = home.join("vault.sqlite");

    let out1 = osl()
        .env("HOME", home)
        .args([
            "--vault",
            &vault.to_string_lossy(),
            "autostart",
            &custom.to_string_lossy(),
        ])
        .output()
        .unwrap();
    assert!(out1.status.success());
    let content1 = fs::read_to_string(home.join(".config/systemd/user/osl-watch.service")).unwrap();
    assert!(content1.contains(&*custom.to_string_lossy()));

    let out2 = osl()
        .env("HOME", home)
        .args(["--vault", &vault.to_string_lossy(), "autostart"])
        .output()
        .unwrap();
    assert!(out2.status.success());
    let content2 = fs::read_to_string(home.join(".config/systemd/user/osl-watch.service")).unwrap();
    assert!(!content2.contains(&*custom.to_string_lossy()));
}

#[test]
#[cfg(target_os = "linux")]
fn setup_yes_skips_autostart_prompt() {
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
        !stdout.contains("Install a persistent watch daemon"),
        "setup --yes should not prompt for autostart: {stdout}"
    );
}
