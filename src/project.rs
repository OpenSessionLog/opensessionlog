use std::path::Path;

use rusqlite::Connection;

use crate::error::Result;

#[derive(Debug, Clone)]
pub struct Project {
    pub root_path: String,
    pub git_remote: Option<String>,
    pub git_owner: Option<String>,
    pub git_repo: Option<String>,
    pub slug: String,
}

pub fn resolve(root_path: &Path) -> Result<Project> {
    let canonical = root_path.canonicalize()?;
    let root_str = canonical.to_string_lossy().to_string();
    let slug = canonical
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "unknown".to_string());

    let git_remote = git_origin(&canonical).ok();
    let (git_owner, git_repo) = git_remote
        .as_ref()
        .map(|s| parse_git_remote(s))
        .unwrap_or((None, None));

    Ok(Project {
        root_path: root_str,
        git_remote,
        git_owner,
        git_repo,
        slug,
    })
}

fn git_origin(path: &Path) -> Result<String> {
    let output = std::process::Command::new("git")
        .args(["-C", &path.to_string_lossy(), "remote", "get-url", "origin"])
        .output()?;
    if !output.status.success() {
        return Err(crate::error::OslError::Connector(
            "git remote failed".to_string(),
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn parse_git_remote(url: &str) -> (Option<String>, Option<String>) {
    let trimmed = url.trim();
    if let Some(rest) = trimmed.strip_prefix("git@github.com:") {
        let parts: Vec<&str> = rest.trim_end_matches(".git").split('/').collect();
        if parts.len() == 2 {
            return (Some(parts[0].to_string()), Some(parts[1].to_string()));
        }
    }
    // HTTPS-style URLs: https://github.com/owner/repo.git
    if let Some(idx) = trimmed.find("//") {
        let after_scheme = &trimmed[idx + 2..];
        let path_start = after_scheme.find('/').map(|i| i + idx + 2);
        if let Some(start) = path_start {
            let path = &trimmed[start..];
            let segments: Vec<&str> = path.trim_matches('/').split('/').collect();
            if segments.len() >= 2 {
                return (
                    Some(segments[0].to_string()),
                    Some(segments[1].trim_end_matches(".git").to_string()),
                );
            }
        }
    }
    (None, None)
}

pub fn upsert(conn: &Connection, project: &Project) -> Result<i64> {
    conn.execute(
        "INSERT INTO projects (root_path, git_remote, git_owner, git_repo, slug)
         VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(root_path) DO UPDATE SET
           git_remote=excluded.git_remote,
           git_owner=excluded.git_owner,
           git_repo=excluded.git_repo,
           slug=excluded.slug",
        (
            &project.root_path,
            project.git_remote.as_deref(),
            project.git_owner.as_deref(),
            project.git_repo.as_deref(),
            &project.slug,
        ),
    )?;
    let id: i64 = conn.query_row(
        "SELECT id FROM projects WHERE root_path = ?1",
        [&project.root_path],
        |r| r.get(0),
    )?;
    Ok(id)
}
