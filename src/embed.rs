use std::collections::HashMap;
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

use rusqlite::{Connection, OptionalExtension};
use serde_json::json;

use crate::error::{OslError, Result};

/// Stored embedding configuration from a previous `osl embed` run.
#[derive(Debug, Clone, PartialEq)]
pub struct EmbedConfig {
    pub model: String,
    pub dimensions: u32,
    pub provider: PathBuf,
}

/// Statistics returned by `embed::run`.
#[derive(Debug, Clone, PartialEq)]
pub struct EmbedStats {
    pub messages_embedded: u64,
    pub sessions_summarized: u64,
    pub model: String,
    pub dimensions: u32,
}

/// Read the persisted embedder configuration, returning `None` if any key is missing/NULL.
pub fn read_config(conn: &Connection) -> Result<Option<EmbedConfig>> {
    let model: Option<String> = conn
        .query_row(
            "SELECT value FROM vault_config WHERE key = 'embedding_model'",
            [],
            |r| r.get::<_, Option<String>>(0),
        )
        .optional()?
        .flatten();
    let dims: Option<String> = conn
        .query_row(
            "SELECT value FROM vault_config WHERE key = 'embedding_dimensions'",
            [],
            |r| r.get::<_, Option<String>>(0),
        )
        .optional()?
        .flatten();
    let provider: Option<String> = conn
        .query_row(
            "SELECT value FROM vault_config WHERE key = 'embedder_path'",
            [],
            |r| r.get::<_, Option<String>>(0),
        )
        .optional()?
        .flatten();

    match (model, dims, provider) {
        (Some(m), Some(d), Some(p)) => {
            let dimensions: u32 = d
                .parse()
                .map_err(|e| OslError::Embed(format!("invalid dimensions in vault_config: {e}")))?;
            Ok(Some(EmbedConfig {
                model: m,
                dimensions,
                provider: PathBuf::from(p),
            }))
        }
        _ => Ok(None),
    }
}

/// Spawn the user-supplied embedder subprocess with piped stdin/stdout.
pub fn spawn(provider: &Path) -> Result<Child> {
    Command::new(provider)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(|e| OslError::Embed(format!("failed to spawn embedder: {e}")))
}

/// Encode a vector of f32 values as little-endian bytes.
pub fn encode_f32_le(v: &[f32]) -> Vec<u8> {
    v.iter().flat_map(|f| f.to_bits().to_le_bytes()).collect()
}

fn text_for_message(role: &str, content: Option<&str>, thinking: Option<&str>) -> String {
    let body = content
        .or(thinking)
        .map(|s| s.to_string())
        .unwrap_or_default();
    format!("{role}\n{body}")
}

/// Parse the embedder's mandatory model header.
pub(crate) fn parse_header(line: &str) -> Result<(String, u32)> {
    let trimmed = line.trim();
    let obj: serde_json::Value =
        serde_json::from_str(trimmed).map_err(|e| OslError::Embed(format!("bad header: {e}")))?;
    let obj = obj
        .as_object()
        .ok_or_else(|| OslError::Embed("header is not a JSON object".into()))?;
    let ty = obj
        .get("type")
        .and_then(|v| v.as_str())
        .ok_or_else(|| OslError::Embed("header missing type".into()))?;
    if ty != "model" {
        return Err(OslError::Embed(format!(
            "header type is '{ty}', expected 'model'"
        )));
    }
    let model = obj
        .get("model")
        .and_then(|v| v.as_str())
        .ok_or_else(|| OslError::Embed("header missing model".into()))?
        .to_string();
    let dimensions = obj
        .get("dimensions")
        .and_then(|v| v.as_u64())
        .ok_or_else(|| OslError::Embed("header missing dimensions".into()))?;
    if dimensions == 0 || dimensions > u32::MAX as u64 {
        return Err(OslError::Embed(format!("invalid dimensions: {dimensions}")));
    }
    Ok((model, dimensions as u32))
}

/// Parse a single result line from the embedder.
pub(crate) fn parse_result(line: &str) -> Result<(String, Vec<f32>)> {
    let trimmed = line.trim();
    let obj: serde_json::Value =
        serde_json::from_str(trimmed).map_err(|e| OslError::Embed(format!("bad result: {e}")))?;
    let obj = obj
        .as_object()
        .ok_or_else(|| OslError::Embed("result is not a JSON object".into()))?;
    let id = obj
        .get("id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| OslError::Embed("result missing id".into()))?
        .to_string();
    let embedding = obj
        .get("embedding")
        .and_then(|v| v.as_array())
        .ok_or_else(|| OslError::Embed("result missing embedding".into()))?;
    let mut vec = Vec::with_capacity(embedding.len());
    for (i, val) in embedding.iter().enumerate() {
        let f = val
            .as_f64()
            .ok_or_else(|| OslError::Embed(format!("embedding[{i}] is not a number")))?;
        vec.push(f as f32);
    }
    Ok((id, vec))
}

/// Embed all NULL-embedding messages and update per-session summary embeddings.
/// (Unchanged signature — delegates with no filter and no force.)
pub fn run(
    conn: &mut Connection,
    provider: &Path,
    limit_messages: Option<u64>,
) -> Result<EmbedStats> {
    run_with_filter(
        conn,
        provider,
        limit_messages,
        &crate::recency::RecencyFilter::none(),
        false,
    )
}

/// Embed messages in the vault, optionally narrowed by `filter` and forced via `force`.
///
/// - `filter` narrows which messages are candidates (applied to `messages.created_at`).
/// - `force=false` (default): only messages with `embedding IS NULL`.
/// - `force=true`: re-embed ALL messages matching the filter, regardless of existing
///   embeddings.
pub fn run_with_filter(
    conn: &mut Connection,
    provider: &Path,
    limit_messages: Option<u64>,
    filter: &crate::recency::RecencyFilter,
    force: bool,
) -> Result<EmbedStats> {
    crate::vec::init();

    // 1. SELECT (buffer all rows before any UPDATE — Phase 2 SQLITE_LOCKED lesson).
    let (sql, params) = crate::db::messages_for_embedding_query(filter, force);
    let mut stmt = conn.prepare(&sql)?;
    let mut rows = stmt.query(rusqlite::params_from_iter(params.iter()))?;
    let cap = limit_messages.map(|n| n as usize).unwrap_or(usize::MAX);
    let mut inputs: Vec<(String, String, String)> = Vec::new();
    while let Some(row) = rows.next()? {
        if inputs.len() >= cap {
            break;
        }
        let id: String = row.get(0)?;
        let session_id: String = row.get(1)?;
        let role: String = row.get(2)?;
        let content: Option<String> = row.get(3)?;
        let thinking: Option<String> = row.get(4)?;
        inputs.push((
            id,
            session_id,
            text_for_message(&role, content.as_deref(), thinking.as_deref()),
        ));
    }
    drop(rows);
    drop(stmt);

    // 2. Spawn the embedder and send the buffered inputs as NDJSON.
    let mut child = spawn(provider)?;
    let stdin = child
        .stdin
        .take()
        .ok_or_else(|| OslError::Embed("failed to open embedder stdin".into()))?;
    {
        let mut writer = BufWriter::new(stdin);
        for (id, _session_id, text) in &inputs {
            let line = json!({"id": id, "text": text}).to_string();
            writeln!(writer, "{line}").map_err(OslError::from)?;
        }
        writer.flush().map_err(OslError::from)?;
        // Close stdin so the child can finish and we avoid pipe deadlock.
        drop(writer);
    }

    // 3. Read the header and all result lines.
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| OslError::Embed("failed to open embedder stdout".into()))?;
    let reader = BufReader::new(stdout);
    let mut lines = reader.lines();

    let mut header_line: Option<String> = None;
    for line in lines.by_ref() {
        let line = line.map_err(OslError::from)?;
        if line.trim().is_empty() {
            continue;
        }
        header_line = Some(line);
        break;
    }
    let header_line =
        header_line.ok_or_else(|| OslError::Embed("embedder produced no header line".into()))?;
    let (model, dimensions) = parse_header(&header_line)?;

    let mut by_id: HashMap<String, String> = inputs
        .iter()
        .map(|(id, session_id, _)| (id.clone(), session_id.clone()))
        .collect();

    let mut messages_embedded: u64 = 0;
    let mut accumulators: HashMap<String, (Vec<f64>, u64)> = HashMap::new();

    let mut update_stmt = conn.prepare("UPDATE messages SET embedding = ?2 WHERE uuid = ?1")?;

    for line in lines {
        let line = line.map_err(OslError::from)?;
        if line.trim().is_empty() {
            continue;
        }
        let (id, embedding) = parse_result(&line)?;
        if embedding.len() != dimensions as usize {
            return Err(OslError::Embed(format!(
                "dimension mismatch: message {id} has {} dims, expected {dimensions}",
                embedding.len()
            )));
        }
        let session_id = by_id
            .get(&id)
            .ok_or_else(|| OslError::Embed(format!("embedder returned unknown id: {id}")))?;

        let blob = encode_f32_le(&embedding);
        update_stmt.execute(rusqlite::params![id, blob])?;
        messages_embedded += 1;

        let (sum, count) = accumulators
            .entry(session_id.clone())
            .or_insert_with(|| (vec![0.0; dimensions as usize], 0));
        for (i, &v) in embedding.iter().enumerate() {
            sum[i] += f64::from(v);
        }
        *count += 1;
        by_id.remove(&id);
    }
    drop(update_stmt);

    // 4. Wait for the child to exit successfully.
    let status = child
        .wait()
        .map_err(|e| OslError::Embed(format!("embedder wait failed: {e}")))?;
    if !status.success() {
        return Err(OslError::Embed(format!(
            "embedder exited with status {status}"
        )));
    }

    // 5. Persist config via upsert (critical for Phase-1 -> Phase-2 upgraded vaults).
    let tx = conn.transaction()?;
    let upsert_sql = "INSERT INTO vault_config(key,value,description,updated_at)
                      VALUES (?1,?2,?3,strftime('%Y-%m-%dT%H:%M:%SZ','now'))
                      ON CONFLICT(key) DO UPDATE SET value=excluded.value, updated_at=excluded.updated_at";
    tx.execute(
        upsert_sql,
        rusqlite::params![
            "embedding_model",
            model.clone(),
            "User-supplied embedder model name (set in Phase 2)"
        ],
    )?;
    tx.execute(
        upsert_sql,
        rusqlite::params![
            "embedding_dimensions",
            dimensions.to_string(),
            "Embedding vector dimension (set in Phase 2)"
        ],
    )?;
    tx.execute(
        upsert_sql,
        rusqlite::params![
            "embedder_path",
            provider.to_string_lossy().to_string(),
            "Absolute path to the user-supplied embedder script (set by osl embed)"
        ],
    )?;

    // 6. Compute and store per-session mean summary embeddings.
    let mut sessions_summarized: u64 = 0;
    let mut summary_stmt =
        tx.prepare("UPDATE sessions SET summary_embedding = ?2 WHERE id = ?1")?;
    for (session_id, (sum, count)) in accumulators {
        if count == 0 {
            continue;
        }
        let mean: Vec<f32> = sum.iter().map(|&v| (v / count as f64) as f32).collect();
        summary_stmt.execute(rusqlite::params![session_id, encode_f32_le(&mean)])?;
        sessions_summarized += 1;
    }
    drop(summary_stmt);
    tx.commit()?;

    Ok(EmbedStats {
        messages_embedded,
        sessions_summarized,
        model,
        dimensions,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_f32_le_roundtrip() {
        let v = vec![0.0f32, -1.5, 1.25, f32::MAX, f32::MIN];
        let bytes = encode_f32_le(&v);
        assert_eq!(bytes.len(), v.len() * 4);
        let mut decoded = Vec::new();
        for chunk in bytes.chunks_exact(4) {
            let bits = u32::from_le_bytes(chunk.try_into().unwrap());
            decoded.push(f32::from_bits(bits));
        }
        assert_eq!(v, decoded);
    }

    #[test]
    fn parse_header_valid() {
        let line = r#"{"type":"model","model":"identity-fixture","dimensions":8}"#;
        let (model, dims) = parse_header(line).unwrap();
        assert_eq!(model, "identity-fixture");
        assert_eq!(dims, 8);
    }

    #[test]
    fn parse_header_missing_model_errors() {
        let line = r#"{"type":"model","dimensions":8}"#;
        assert!(parse_header(line).is_err());
    }

    #[test]
    fn parse_header_zero_dimensions_errors() {
        let line = r#"{"type":"model","model":"x","dimensions":0}"#;
        assert!(parse_header(line).is_err());
    }

    #[test]
    fn parse_result_valid() {
        let line = r#"{"id":"msg-1","embedding":[0.0,1.0,-2.5]}"#;
        let (id, vec) = parse_result(line).unwrap();
        assert_eq!(id, "msg-1");
        assert_eq!(vec, vec![0.0, 1.0, -2.5]);
    }

    #[test]
    fn parse_result_missing_embedding_errors() {
        let line = r#"{"id":"msg-1"}"#;
        assert!(parse_result(line).is_err());
    }

    #[test]
    fn parse_header_empty_input_is_noop() {
        // A header line with zero following results still parses cleanly.
        let line = r#"{"type":"model","model":"identity-fixture","dimensions":8}"#;
        let (model, dims) = parse_header(line).unwrap();
        assert_eq!(model, "identity-fixture");
        assert_eq!(dims, 8);
    }
}
