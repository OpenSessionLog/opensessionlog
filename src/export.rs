use rusqlite::Connection;
use uuid::Uuid;

use crate::error::{OslError, Result};

struct SessionHeader {
    title: Option<String>,
    started_at: Option<String>,
    ended_at: Option<String>,
    source_name: String,
    model: Option<String>,
    input_tokens: i64,
    output_tokens: i64,
    cache_read: i64,
    cache_write: i64,
    tool_call_count: i64,
    project_slug: Option<String>,
}

type ToolCallRow = (String, String, Option<String>, Option<String>, Option<bool>);

/// Export a single session as markdown.
pub fn export_markdown(conn: &Connection, session_id: &str) -> Result<String> {
    let sid = Uuid::parse_str(session_id)?;

    let header = conn
        .query_row(
            "SELECT s.title, s.started_at, s.ended_at, src.name,
                s.model, s.input_tokens, s.output_tokens, s.cache_read_tokens,
                s.cache_write_tokens, s.tool_call_count, p.slug
         FROM sessions s
         JOIN sources src ON src.id = s.source_id
         LEFT JOIN projects p ON p.id = s.project_id
         WHERE s.id = ?1",
            [sid.to_string()],
            |r| {
                Ok(SessionHeader {
                    title: r.get(0)?,
                    started_at: r.get(1)?,
                    ended_at: r.get(2)?,
                    source_name: r.get(3)?,
                    model: r.get(4)?,
                    input_tokens: r.get(5)?,
                    output_tokens: r.get(6)?,
                    cache_read: r.get(7)?,
                    cache_write: r.get(8)?,
                    tool_call_count: r.get(9)?,
                    project_slug: r.get(10)?,
                })
            },
        )
        .map_err(|_| OslError::NotFound(format!("session {session_id} not found")))?;

    let mut md = String::new();
    md.push_str(&format!(
        "# {}\n\n",
        header.title.as_deref().unwrap_or("Untitled session")
    ));
    md.push_str(&format!("- **Session ID:** {session_id}\n"));
    md.push_str(&format!("- **Source:** {}\n", header.source_name));
    md.push_str(&format!(
        "- **Project:** {}\n",
        header.project_slug.as_deref().unwrap_or("unknown")
    ));
    md.push_str(&format!(
        "- **Started:** {} · **Ended:** {}\n",
        header.started_at.as_deref().unwrap_or("?"),
        header.ended_at.as_deref().unwrap_or("?")
    ));
    md.push_str(&format!(
        "- **Model:** {}\n",
        header.model.as_deref().unwrap_or("unknown")
    ));
    md.push_str(&format!(
        "- **Tokens:** in={} out={} cache_r={} cache_w={}\n",
        header.input_tokens, header.output_tokens, header.cache_read, header.cache_write
    ));
    md.push_str(&format!("- **Tool calls:** {}\n\n", header.tool_call_count));
    md.push_str("---\n\n");

    let mut stmt = conn.prepare(
        "SELECT id, uuid, role, content, thinking, source_seq, turn_number
         FROM messages
         WHERE session_id = ?1
         ORDER BY source_seq ASC",
    )?;
    let mut rows = stmt.query([sid.to_string()])?;

    // Pre-load tool calls indexed by request_message_id.
    let mut tool_stmt = conn.prepare(
        "SELECT request_message_id, tool_name, call_id, tool_input, tool_output, is_error
         FROM tool_calls
         WHERE session_id = ?1",
    )?;
    let tool_rows = tool_stmt.query_map([sid.to_string()], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, Option<String>>(3)?,
            r.get::<_, Option<String>>(4)?,
            r.get::<_, Option<i64>>(5)?,
        ))
    })?;
    let mut tools_by_request: std::collections::HashMap<i64, Vec<ToolCallRow>> =
        std::collections::HashMap::new();
    for row in tool_rows {
        let (req_id, name, call_id, input, output, is_err) = row?;
        let is_err = is_err.map(|v| v != 0);
        tools_by_request
            .entry(req_id)
            .or_default()
            .push((name, call_id, input, output, is_err));
    }

    while let Some(row) = rows.next()? {
        let msg_id: i64 = row.get(0)?;
        let role: String = row.get(2)?;
        let content: Option<String> = row.get(3)?;
        let thinking: Option<String> = row.get(4)?;
        let turn_number: i64 = row.get(6)?;

        md.push_str(&format!("## [{turn_number}] {role}\n"));
        if let Some(c) = content {
            md.push_str(&c);
            md.push('\n');
        }
        if let Some(t) = thinking {
            md.push_str(&format!("> **thinking:** {t}\n\n"));
        }

        if let Some(tools) = tools_by_request.get(&msg_id) {
            for (tool_name, call_id, input, output, is_err) in tools {
                md.push_str(&format!("### Tool: {tool_name} (`{call_id}`)\n"));
                if let Some(inp) = input {
                    md.push_str("**Input:**\n```json\n");
                    md.push_str(inp);
                    md.push_str("\n```\n");
                }
                md.push_str("**Output:**\n```\n");
                if let Some(out) = output {
                    md.push_str(out);
                }
                if *is_err == Some(true) {
                    md.push_str("\n(error)");
                }
                md.push_str("\n```\n\n");
            }
        }
        md.push('\n');
    }

    Ok(md)
}
