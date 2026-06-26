use rusqlite::Connection;
use uuid::Uuid;

use crate::error::Result;
use crate::model::GrepHit;

/// Full-text search over messages using the FTS5 virtual table.
/// The `pattern` is passed through to FTS5 MATCH verbatim (FTS5 query syntax).
pub fn grep(
    conn: &Connection,
    pattern: &str,
    limit: u32,
    project_slug: Option<&str>,
) -> Result<Vec<GrepHit>> {
    let sql = if project_slug.is_some() {
        "SELECT m.session_id, m.role, m.content, f.rank
         FROM messages_fts f
         JOIN messages m ON m.id = f.rowid
         JOIN sessions s ON s.id = m.session_id
         JOIN projects p ON p.id = s.project_id
         WHERE messages_fts MATCH ?1 AND p.slug = ?3
         ORDER BY f.rank LIMIT ?2"
    } else {
        "SELECT m.session_id, m.role, m.content, f.rank
         FROM messages_fts f
         JOIN messages m ON m.id = f.rowid
         WHERE messages_fts MATCH ?1
         ORDER BY f.rank LIMIT ?2"
    };

    let mut stmt = conn.prepare(sql)?;
    let limit_str = limit.to_string();

    let mut hits = Vec::new();
    if let Some(slug) = project_slug {
        let mut rows = stmt.query([pattern, &limit_str, slug])?;
        while let Some(row) = rows.next()? {
            let session_id: String = row.get(0)?;
            let role: String = row.get(1)?;
            let content: Option<String> = row.get(2)?;
            let rank: f64 = row.get(3)?;
            hits.push(GrepHit {
                session_id: Uuid::parse_str(&session_id).unwrap(),
                role,
                content_snippet: content.unwrap_or_default().chars().take(200).collect(),
                rank,
            });
        }
    } else {
        let mut rows = stmt.query([pattern, &limit_str])?;
        while let Some(row) = rows.next()? {
            let session_id: String = row.get(0)?;
            let role: String = row.get(1)?;
            let content: Option<String> = row.get(2)?;
            let rank: f64 = row.get(3)?;
            hits.push(GrepHit {
                session_id: Uuid::parse_str(&session_id).unwrap(),
                role,
                content_snippet: content.unwrap_or_default().chars().take(200).collect(),
                rank,
            });
        }
    }

    Ok(hits)
}
