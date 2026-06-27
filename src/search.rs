use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::Path;

use rusqlite::Connection;
use serde_json::json;
use uuid::Uuid;

use crate::embed::{self, encode_f32_le, parse_header, parse_result, spawn};
use crate::error::{OslError, Result};
use crate::model::{GrepHit, SemanticHit, SimilarSession};

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
                session_id: Uuid::parse_str(&session_id)?,
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
                session_id: Uuid::parse_str(&session_id)?,
                role,
                content_snippet: content.unwrap_or_default().chars().take(200).collect(),
                rank,
            });
        }
    }

    Ok(hits)
}

/// Returns true if the vault contains at least one embedded message.
pub fn has_embeddings(conn: &Connection) -> Result<bool> {
    let found: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM messages WHERE embedding IS NOT NULL)",
        [],
        |r| r.get(0),
    )?;
    Ok(found)
}

fn embed_text(provider: &Path, text: &str) -> Result<(String, u32, Vec<f32>)> {
    let mut child = spawn(provider)?;
    let stdin = child
        .stdin
        .take()
        .ok_or_else(|| OslError::Embed("failed to open embedder stdin".into()))?;
    {
        let mut writer = BufWriter::new(stdin);
        let line = json!({"id": "query", "text": text}).to_string();
        writeln!(writer, "{line}").map_err(OslError::from)?;
        writer.flush().map_err(OslError::from)?;
        drop(writer);
    }

    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| OslError::Embed("failed to open embedder stdout".into()))?;
    let reader = BufReader::new(stdout);
    let mut lines = reader.lines();

    let header_line = loop {
        match lines.next() {
            Some(Ok(line)) if !line.trim().is_empty() => break line,
            Some(Err(e)) => return Err(OslError::from(e)),
            None => return Err(OslError::Embed("embedder produced no header".into())),
            _ => continue,
        }
    };
    let (model, dimensions) = parse_header(&header_line)?;

    let result_line = loop {
        match lines.next() {
            Some(Ok(line)) if !line.trim().is_empty() => break line,
            Some(Err(e)) => return Err(OslError::from(e)),
            None => {
                return Err(OslError::Embed(
                    "embedder produced no result for query".into(),
                ))
            }
            _ => continue,
        }
    };
    let (_id, query_vec) = parse_result(&result_line)?;

    let status = child
        .wait()
        .map_err(|e| OslError::Embed(format!("embedder wait failed: {e}")))?;
    if !status.success() {
        return Err(OslError::Embed(format!(
            "embedder exited with status {status}"
        )));
    }

    Ok((model, dimensions, query_vec))
}

/// Semantic KNN search over stored message embeddings.
pub fn semantic(conn: &Connection, query: &str, limit: u32) -> Result<Vec<SemanticHit>> {
    if !has_embeddings(conn)? {
        return Ok(Vec::new());
    }

    let cfg = embed::read_config(conn)?.ok_or_else(|| {
        OslError::Usage("no embedder configured; run 'osl embed --provider <script>' first".into())
    })?;

    let (_model, _dimensions, query_vec) = embed_text(&cfg.provider, query)?;
    if query_vec.len() != cfg.dimensions as usize {
        return Err(OslError::Embed(format!(
            "dimension mismatch: query {} vs configured {}",
            query_vec.len(),
            cfg.dimensions
        )));
    }

    let qblob = encode_f32_le(&query_vec);
    let mut stmt = conn.prepare(
        "SELECT m.session_id, m.uuid, m.role, m.content,
                vec_distance_cosine(m.embedding, ?1) AS dist
         FROM messages m
         WHERE m.embedding IS NOT NULL
         ORDER BY dist ASC
         LIMIT ?2",
    )?;
    let mut rows = stmt.query(rusqlite::params![&qblob as &[u8], limit.to_string()])?;

    let mut hits = Vec::new();
    while let Some(row) = rows.next()? {
        let session_id: String = row.get(0)?;
        let message_uuid: String = row.get(1)?;
        let role: String = row.get(2)?;
        let content: Option<String> = row.get(3)?;
        let distance: f64 = row.get(4)?;
        hits.push(SemanticHit {
            session_id: Uuid::parse_str(&session_id)?,
            message_uuid: Uuid::parse_str(&message_uuid)?,
            role,
            content_snippet: content.unwrap_or_default().chars().take(200).collect(),
            distance,
        });
    }

    Ok(hits)
}

/// Returns true if the given session has a stored summary embedding.
pub fn has_summary_embedding(conn: &Connection, session_id: &Uuid) -> Result<bool> {
    let found: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sessions WHERE id = ?1 AND summary_embedding IS NOT NULL)",
        [session_id.to_string()],
        |r| r.get(0),
    )?;
    Ok(found)
}

/// Find sessions with a summary embedding similar to the target session.
pub fn similar(conn: &Connection, session_id: &Uuid, limit: u32) -> Result<Vec<SimilarSession>> {
    if !has_summary_embedding(conn, session_id)? {
        return Ok(Vec::new());
    }

    let target_blob: Vec<u8> = conn.query_row(
        "SELECT summary_embedding FROM sessions WHERE id = ?1",
        [session_id.to_string()],
        |r| r.get(0),
    )?;

    let mut stmt = conn.prepare(
        "SELECT s.id, s.title,
                vec_distance_cosine(s.summary_embedding, ?1) AS dist
         FROM sessions s
         WHERE s.summary_embedding IS NOT NULL AND s.id <> ?2
         ORDER BY dist ASC
         LIMIT ?3",
    )?;
    let mut rows = stmt.query(rusqlite::params![
        &target_blob as &[u8],
        session_id.to_string(),
        limit.to_string()
    ])?;

    let mut hits = Vec::new();
    while let Some(row) = rows.next()? {
        let id: String = row.get(0)?;
        let title: Option<String> = row.get(1)?;
        let distance: f64 = row.get(2)?;
        hits.push(SimilarSession {
            session_id: Uuid::parse_str(&id)?,
            title,
            distance,
        });
    }

    Ok(hits)
}
