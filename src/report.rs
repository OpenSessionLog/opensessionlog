use rusqlite::{params_from_iter, Connection, OptionalExtension, ToSql};
use std::collections::HashMap;

use crate::error::{OslError, Result};
use crate::model::{
    DailyBreakdown, ReportDocument, ReportMetrics, ReportPeriodKind, RoleBreakdown,
    SourceBreakdown, TopProject, TopTool,
};

/// Build project/source filter SQL fragments and bound parameter values.
struct Filter {
    sql: String,
    params: Vec<String>,
}

fn build_filters(project_slug: Option<&str>, source_name: Option<&str>) -> Filter {
    let mut sql = String::new();
    let mut params = Vec::new();
    if let Some(slug) = project_slug {
        sql.push_str(" AND p.slug = ?");
        params.push(slug.to_string());
    }
    if let Some(source) = source_name {
        sql.push_str(" AND src.name = ?");
        params.push(source.to_string());
    }
    Filter { sql, params }
}

fn bind_period_and_filters(
    period_start: &str,
    period_end: &str,
    filter: &Filter,
) -> Vec<Box<dyn ToSql>> {
    let mut out: Vec<Box<dyn ToSql>> = vec![
        Box::new(period_start.to_string()),
        Box::new(period_end.to_string()),
    ];
    out.extend(
        filter
            .params
            .iter()
            .map(|s| Box::new(s.clone()) as Box<dyn ToSql>),
    );
    out
}

fn query_date_now(conn: &Connection) -> Result<String> {
    let today: String = conn.query_row("SELECT date('now')", [], |r| r.get(0))?;
    Ok(today)
}

fn query_generated_at(conn: &Connection) -> Result<String> {
    let ts: String = conn.query_row("SELECT strftime('%Y-%m-%dT%H:%M:%SZ','now')", [], |r| {
        r.get(0)
    })?;
    Ok(ts)
}

/// Resolve the requested period into a (kind, start, end) triple.
fn resolve_period(
    conn: &Connection,
    kind: Option<ReportPeriodKind>,
    from: Option<&str>,
    to: Option<&str>,
) -> Result<(ReportPeriodKind, String, String)> {
    match kind {
        Some(ReportPeriodKind::Daily) => {
            let (s, e): (String, String) =
                conn.query_row("SELECT date('now'), date('now')", [], |r| {
                    Ok((r.get(0)?, r.get(1)?))
                })?;
            Ok((ReportPeriodKind::Daily, s, e))
        }
        Some(ReportPeriodKind::Weekly) => {
            let (s, e): (String, String) =
                conn.query_row("SELECT date('now','-6 days'), date('now')", [], |r| {
                    Ok((r.get(0)?, r.get(1)?))
                })?;
            Ok((ReportPeriodKind::Weekly, s, e))
        }
        Some(ReportPeriodKind::Monthly) => {
            let (s, e): (String, String) = conn.query_row(
                "SELECT date('now','start of month'), date('now')",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?;
            Ok((ReportPeriodKind::Monthly, s, e))
        }
        Some(ReportPeriodKind::Last30Days) => {
            let (s, e): (String, String) =
                conn.query_row("SELECT date('now','-29 days'), date('now')", [], |r| {
                    Ok((r.get(0)?, r.get(1)?))
                })?;
            Ok((ReportPeriodKind::Last30Days, s, e))
        }
        Some(ReportPeriodKind::Custom) | None => {
            let f =
                from.ok_or_else(|| OslError::Usage("--from is required for custom period".into()))?;
            let t =
                to.ok_or_else(|| OslError::Usage("--to is required for custom period".into()))?;
            if f > t {
                return Err(OslError::Usage(format!(
                    "--from ({f}) must not be after --to ({t})"
                )));
            }
            Ok((ReportPeriodKind::Custom, f.to_string(), t.to_string()))
        }
    }
}

struct CachedRow {
    id: i64,
    data_json: String,
    #[allow(dead_code)]
    markdown: Option<String>,
    period_end: String,
}

fn lookup_cached(
    conn: &Connection,
    scope: &str,
    period_start: &str,
    period_end: &str,
) -> Result<Option<CachedRow>> {
    let mut stmt = conn.prepare(
        "SELECT id, data_json, markdown, period_end
         FROM reports
         WHERE scope = ?1 AND period_start = ?2 AND period_end = ?3
         ORDER BY id DESC
         LIMIT 1",
    )?;
    let row = stmt
        .query_row([scope, period_start, period_end], |r| {
            Ok(CachedRow {
                id: r.get(0)?,
                data_json: r.get(1)?,
                markdown: r.get(2)?,
                period_end: r.get(3)?,
            })
        })
        .optional()?;
    Ok(row)
}

/// Upsert the daily rollup table for every (date, source, project) touched in the period.
/// This is intentionally a whole-period rollup with no project/source filter.
fn upsert_usage_summary(conn: &mut Connection, period_start: &str, period_end: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO usage_summary (
            date, source_id, project_id,
            session_count, message_count,
            input_tokens, output_tokens, cache_read_tokens, cache_write_tokens,
            estimated_cost_usd, tool_call_count, error_count
        )
        SELECT
            DATE(s.started_at)        AS date,
            s.source_id,
            s.project_id,
            COUNT(DISTINCT s.id)      AS session_count,
            COALESCE(m.cnt, 0)        AS message_count,
            COALESCE(SUM(s.input_tokens),       0),
            COALESCE(SUM(s.output_tokens),      0),
            COALESCE(SUM(s.cache_read_tokens),  0),
            COALESCE(SUM(s.cache_write_tokens), 0),
            COALESCE(SUM(s.estimated_cost_usd), 0),
            COALESCE(t.cnt, 0)        AS tool_call_count,
            COALESCE(SUM(s.error_count),0)      AS error_count
        FROM sessions s
        LEFT JOIN (
            SELECT session_id, COUNT(*) AS cnt
            FROM messages
            GROUP BY session_id
        ) m ON m.session_id = s.id
        LEFT JOIN (
            SELECT session_id, COUNT(*) AS cnt
            FROM tool_calls
            GROUP BY session_id
        ) t ON t.session_id = s.id
        WHERE s.started_at IS NOT NULL
          AND DATE(s.started_at) BETWEEN ?1 AND ?2
        GROUP BY DATE(s.started_at), s.source_id, s.project_id
        ON CONFLICT(date, source_id, project_id) DO UPDATE SET
            session_count      = excluded.session_count,
            message_count      = excluded.message_count,
            input_tokens       = excluded.input_tokens,
            output_tokens      = excluded.output_tokens,
            cache_read_tokens  = excluded.cache_read_tokens,
            cache_write_tokens = excluded.cache_write_tokens,
            estimated_cost_usd = excluded.estimated_cost_usd,
            tool_call_count    = excluded.tool_call_count,
            error_count        = excluded.error_count",
        [period_start, period_end],
    )?;
    Ok(())
}

/// True if no session falls inside the requested scope + period.
fn is_empty(
    conn: &Connection,
    period_start: &str,
    period_end: &str,
    filter: &Filter,
) -> Result<bool> {
    let sql = format!(
        "SELECT EXISTS(
            SELECT 1 FROM sessions s
            JOIN sources src ON src.id = s.source_id
            LEFT JOIN projects p ON p.id = s.project_id
            WHERE s.started_at IS NOT NULL
              AND DATE(s.started_at) BETWEEN ?1 AND ?2
              {}
        )",
        filter.sql
    );
    let params = bind_period_and_filters(period_start, period_end, filter);
    let exists: i64 = conn.query_row(&sql, params_from_iter(params.iter()), |r| r.get(0))?;
    Ok(exists == 0)
}

/// Aggregate live metrics directly from sessions/messages/tool_calls.
/// usage_summary is a persisted side-effect rollup, but its grain cannot carry
/// role/model/tool_name/project_slug or avg duration, so the report is built
/// from the source tables.
fn compute_metrics(
    conn: &Connection,
    period_start: &str,
    period_end: &str,
    project_slug: Option<&str>,
    source_name: Option<&str>,
) -> Result<ReportMetrics> {
    let filter = build_filters(project_slug, source_name);

    // 1. Totals, cost, errors, unique models, avg duration.
    let totals_sql = format!(
        "SELECT
            COUNT(*),
            COALESCE(SUM(s.input_tokens),0),
            COALESCE(SUM(s.output_tokens),0),
            COALESCE(SUM(s.cache_read_tokens),0),
            COALESCE(SUM(s.cache_write_tokens),0),
            COALESCE(SUM(s.total_tokens),0),
            SUM(s.estimated_cost_usd),
            COALESCE(SUM(s.error_count),0),
            COUNT(DISTINCT s.model),
            AVG(CASE WHEN s.ended_at IS NOT NULL AND s.started_at IS NOT NULL
                     THEN (julianday(s.ended_at) - julianday(s.started_at)) * 86400.0
                     ELSE NULL END)
        FROM sessions s
        LEFT JOIN projects p ON p.id = s.project_id
        JOIN sources src ON src.id = s.source_id
        WHERE s.started_at IS NOT NULL
          AND DATE(s.started_at) BETWEEN ?1 AND ?2
          {}",
        filter.sql
    );
    let totals_params = bind_period_and_filters(period_start, period_end, &filter);
    #[allow(clippy::type_complexity)]
    let (
        total_sessions,
        total_input_tokens,
        total_output_tokens,
        total_cache_read_tokens,
        total_cache_write_tokens,
        total_tokens,
        estimated_cost_usd,
        error_count,
        unique_models,
        avg_session_duration_seconds,
    ): (
        i64,
        i64,
        i64,
        i64,
        i64,
        i64,
        Option<f64>,
        i64,
        i64,
        Option<f64>,
    ) = conn.query_row(&totals_sql, params_from_iter(totals_params.iter()), |r| {
        Ok((
            r.get(0)?,
            r.get(1)?,
            r.get(2)?,
            r.get(3)?,
            r.get(4)?,
            r.get(5)?,
            r.get(6)?,
            r.get(7)?,
            r.get(8)?,
            r.get(9)?,
        ))
    })?;

    // 2. Messages by role.
    let role_sql = format!(
        "SELECT m.role, COUNT(*) AS cnt
        FROM messages m
        JOIN sessions s ON s.id = m.session_id
        LEFT JOIN projects p ON p.id = s.project_id
        JOIN sources src ON src.id = s.source_id
        WHERE s.started_at IS NOT NULL
          AND DATE(s.started_at) BETWEEN ?1 AND ?2
          {}
        GROUP BY m.role
        ORDER BY cnt DESC",
        filter.sql
    );
    let role_params = bind_period_and_filters(period_start, period_end, &filter);
    let mut stmt = conn.prepare(&role_sql)?;
    let role_rows = stmt.query_map(params_from_iter(role_params.iter()), |r| {
        Ok(RoleBreakdown {
            role: r.get(0)?,
            count: r.get(1)?,
        })
    })?;
    let mut messages_by_role = Vec::new();
    let mut message_count = 0i64;
    for row in role_rows {
        let rb = row?;
        message_count += rb.count;
        messages_by_role.push(rb);
    }

    // 3. Tool calls: total count and top 10.
    let tool_total_sql = format!(
        "SELECT COUNT(*)
        FROM tool_calls tc
        JOIN sessions s ON s.id = tc.session_id
        LEFT JOIN projects p ON p.id = s.project_id
        JOIN sources src ON src.id = s.source_id
        WHERE s.started_at IS NOT NULL
          AND DATE(s.started_at) BETWEEN ?1 AND ?2
          {}",
        filter.sql
    );
    let tool_total_params = bind_period_and_filters(period_start, period_end, &filter);
    let tool_call_count: i64 = conn.query_row(
        &tool_total_sql,
        params_from_iter(tool_total_params.iter()),
        |r| r.get(0),
    )?;

    let top_tools_sql = format!(
        "SELECT tc.tool_name, COUNT(*) AS cnt
        FROM tool_calls tc
        JOIN sessions s ON s.id = tc.session_id
        LEFT JOIN projects p ON p.id = s.project_id
        JOIN sources src ON src.id = s.source_id
        WHERE s.started_at IS NOT NULL
          AND DATE(s.started_at) BETWEEN ?1 AND ?2
          {}
        GROUP BY tc.tool_name
        ORDER BY cnt DESC, tc.tool_name ASC
        LIMIT 10",
        filter.sql
    );
    let top_tools_params = bind_period_and_filters(period_start, period_end, &filter);
    let mut stmt = conn.prepare(&top_tools_sql)?;
    let mut top_tools = Vec::new();
    for row in stmt.query_map(params_from_iter(top_tools_params.iter()), |r| {
        Ok(TopTool {
            tool_name: r.get(0)?,
            count: r.get(1)?,
        })
    })? {
        top_tools.push(row?);
    }

    // 4. Top projects.
    let top_projects_sql = format!(
        "SELECT p.slug,
               COUNT(*) AS session_count,
               COALESCE(SUM(s.total_tokens),0) AS total_tokens
        FROM sessions s
        LEFT JOIN projects p ON p.id = s.project_id
        JOIN sources src ON src.id = s.source_id
        WHERE s.started_at IS NOT NULL
          AND DATE(s.started_at) BETWEEN ?1 AND ?2
          {}
        GROUP BY p.id, p.slug
        ORDER BY session_count DESC, COALESCE(p.slug,'') ASC
        LIMIT 10",
        filter.sql
    );
    let top_projects_params = bind_period_and_filters(period_start, period_end, &filter);
    let mut stmt = conn.prepare(&top_projects_sql)?;
    let mut top_projects = Vec::new();
    for row in stmt.query_map(params_from_iter(top_projects_params.iter()), |r| {
        Ok(TopProject {
            slug: r.get(0)?,
            session_count: r.get(1)?,
            total_tokens: r.get(2)?,
        })
    })? {
        top_projects.push(row?);
    }

    // 5. Source breakdown.
    let sources_sql = format!(
        "SELECT src.name,
               COUNT(*) AS session_count,
               COALESCE(SUM(s.total_tokens),0) AS total_tokens
        FROM sessions s
        JOIN sources src ON src.id = s.source_id
        LEFT JOIN projects p ON p.id = s.project_id
        WHERE s.started_at IS NOT NULL
          AND DATE(s.started_at) BETWEEN ?1 AND ?2
          {}
        GROUP BY src.id, src.name
        ORDER BY session_count DESC, src.name ASC",
        filter.sql
    );
    let sources_params = bind_period_and_filters(period_start, period_end, &filter);
    let mut stmt = conn.prepare(&sources_sql)?;
    let mut sources = Vec::new();
    for row in stmt.query_map(params_from_iter(sources_params.iter()), |r| {
        Ok(SourceBreakdown {
            source: r.get(0)?,
            session_count: r.get(1)?,
            total_tokens: r.get(2)?,
        })
    })? {
        sources.push(row?);
    }

    // 6. Daily breakdown via two-step SQL + Rust HashMap aggregation.
    let sessions_sql = format!(
        "SELECT s.id, DATE(s.started_at) AS day, s.total_tokens
        FROM sessions s
        JOIN sources src ON src.id = s.source_id
        LEFT JOIN projects p ON p.id = s.project_id
        WHERE s.started_at IS NOT NULL
          AND DATE(s.started_at) BETWEEN ?1 AND ?2
          {}",
        filter.sql
    );
    let sessions_params = bind_period_and_filters(period_start, period_end, &filter);
    let mut stmt = conn.prepare(&sessions_sql)?;
    let mut session_rows: Vec<(String, String, i64)> = Vec::new();
    for row in stmt.query_map(params_from_iter(sessions_params.iter()), |r| {
        Ok((r.get(0)?, r.get(1)?, r.get(2)?))
    })? {
        session_rows.push(row?);
    }

    // Per-session message/tool counts, scoped to the same filter set.
    let sub_filter_sql = format!(
        "s.started_at IS NOT NULL
         AND DATE(s.started_at) BETWEEN ?1 AND ?2
         {}",
        filter.sql
    );
    let sub_params = bind_period_and_filters(period_start, period_end, &filter);

    let msg_sql = format!(
        "SELECT m.session_id, COUNT(*)
        FROM messages m
        WHERE m.session_id IN (
            SELECT s.id FROM sessions s
            JOIN sources src ON src.id = s.source_id
            LEFT JOIN projects p ON p.id = s.project_id
            WHERE {}
        )
        GROUP BY m.session_id",
        sub_filter_sql
    );
    let mut stmt = conn.prepare(&msg_sql)?;
    let mut msg_counts: HashMap<String, i64> = HashMap::new();
    for row in stmt.query_map(params_from_iter(sub_params.iter()), |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
    })? {
        let (sid, cnt) = row?;
        msg_counts.insert(sid, cnt);
    }

    let sub_params2 = bind_period_and_filters(period_start, period_end, &filter);
    let tool_sql = format!(
        "SELECT tc.session_id, COUNT(*)
        FROM tool_calls tc
        WHERE tc.session_id IN (
            SELECT s.id FROM sessions s
            JOIN sources src ON src.id = s.source_id
            LEFT JOIN projects p ON p.id = s.project_id
            WHERE {}
        )
        GROUP BY tc.session_id",
        sub_filter_sql
    );
    let mut stmt = conn.prepare(&tool_sql)?;
    let mut tool_counts: HashMap<String, i64> = HashMap::new();
    for row in stmt.query_map(params_from_iter(sub_params2.iter()), |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
    })? {
        let (sid, cnt) = row?;
        tool_counts.insert(sid, cnt);
    }

    let mut daily_map: HashMap<String, DailyBreakdown> = HashMap::new();
    for (session_id, day, total_tokens) in session_rows {
        let entry = daily_map.entry(day).or_insert_with(|| DailyBreakdown {
            date: String::new(),
            session_count: 0,
            message_count: 0,
            tool_call_count: 0,
            total_tokens: 0,
        });
        entry.session_count += 1;
        entry.message_count += msg_counts.get(&session_id).copied().unwrap_or(0);
        entry.tool_call_count += tool_counts.get(&session_id).copied().unwrap_or(0);
        entry.total_tokens += total_tokens;
    }
    // Set the date key on each bucket and sort ascending.
    let mut daily_breakdown: Vec<DailyBreakdown> = daily_map
        .into_iter()
        .map(|(date, mut bucket)| {
            bucket.date = date;
            bucket
        })
        .collect();
    daily_breakdown.sort_by(|a, b| a.date.cmp(&b.date));

    Ok(ReportMetrics {
        total_sessions,
        total_input_tokens,
        total_output_tokens,
        total_cache_read_tokens,
        total_cache_write_tokens,
        total_tokens,
        estimated_cost_usd,
        message_count,
        messages_by_role,
        tool_call_count,
        top_tools,
        error_count,
        unique_models,
        avg_session_duration_seconds,
        top_projects,
        daily_breakdown,
        sources,
    })
}

pub fn run_report(
    conn: &mut Connection,
    kind: Option<ReportPeriodKind>,
    from: Option<&str>,
    to: Option<&str>,
    project_slug: Option<&str>,
    source_name: Option<&str>,
    save: bool,
) -> Result<ReportDocument> {
    let (period_kind, period_start, period_end) = resolve_period(conn, kind, from, to)?;
    let scope = match (project_slug, source_name) {
        (None, None) => "global".to_string(),
        (Some(slug), None) => format!("project:{slug}"),
        (None, Some(src)) => format!("source:{src}"),
        (Some(slug), Some(src)) => format!("project:{slug};source:{src}"),
    };
    let today = query_date_now(conn)?;

    // Cache lookup first when saving.
    let cached = if save {
        lookup_cached(conn, &scope, &period_start, &period_end)?
    } else {
        None
    };

    if let Some(ref cached) = cached {
        let closed = cached.period_end < today;
        // Closed periods are immutable; serve the cached document unless the
        // caller explicitly requested a rolling window that is always recomputed.
        if closed && period_kind != ReportPeriodKind::Last30Days {
            let mut doc: ReportDocument = serde_json::from_str(&cached.data_json)?;
            doc.from_cache = true;
            return Ok(doc);
        }
    }

    let filter = build_filters(project_slug, source_name);

    // Empty-vault fast path: skip both upsert and persistence.
    if is_empty(conn, &period_start, &period_end, &filter)? {
        let generated_at = query_generated_at(conn)?;
        return Ok(ReportDocument {
            scope,
            period_kind,
            period_start,
            period_end,
            generated_at,
            from_cache: false,
            metrics: ReportMetrics {
                total_sessions: 0,
                total_input_tokens: 0,
                total_output_tokens: 0,
                total_cache_read_tokens: 0,
                total_cache_write_tokens: 0,
                total_tokens: 0,
                estimated_cost_usd: None,
                message_count: 0,
                messages_by_role: Vec::new(),
                tool_call_count: 0,
                top_tools: Vec::new(),
                error_count: 0,
                unique_models: 0,
                avg_session_duration_seconds: None,
                top_projects: Vec::new(),
                daily_breakdown: Vec::new(),
                sources: Vec::new(),
            },
        });
    }

    // Side-effect: keep usage_summary rollup up to date for the whole period.
    upsert_usage_summary(conn, &period_start, &period_end)?;

    let metrics = compute_metrics(conn, &period_start, &period_end, project_slug, source_name)?;
    let generated_at = query_generated_at(conn)?;
    let doc = ReportDocument {
        scope,
        period_kind,
        period_start,
        period_end,
        generated_at,
        from_cache: false,
        metrics,
    };

    if save {
        // Replace semantics: remove the prior open/rolling cached row (if any)
        // and insert the freshly computed document. previous_report_id is left
        // NULL this phase to avoid FK issues with the self-referencing column.
        if let Some(ref cached) = cached {
            conn.execute("DELETE FROM reports WHERE id = ?1", [cached.id])?;
        }
        conn.execute(
            "INSERT INTO reports(
                scope, period_start, period_end, generated_at,
                data_json, markdown, previous_report_id, token_budget_used
            ) VALUES (?1, ?2, ?3, strftime('%Y-%m-%dT%H:%M:%SZ','now'), ?4, ?5, NULL, ?6)",
            [
                &doc.scope as &dyn ToSql,
                &doc.period_start as &dyn ToSql,
                &doc.period_end as &dyn ToSql,
                &render_json(&doc)? as &dyn ToSql,
                &render_markdown(&doc) as &dyn ToSql,
                &doc.metrics.total_tokens as &dyn ToSql,
            ],
        )?;
    }

    Ok(doc)
}

fn period_kind_label(kind: ReportPeriodKind) -> &'static str {
    match kind {
        ReportPeriodKind::Daily => "daily",
        ReportPeriodKind::Weekly => "weekly",
        ReportPeriodKind::Monthly => "monthly",
        ReportPeriodKind::Last30Days => "last-30-days",
        ReportPeriodKind::Custom => "custom",
    }
}

pub fn render_markdown(doc: &ReportDocument) -> String {
    let mut out = String::new();
    out.push_str(&format!("# Usage Report — {}\n\n", doc.scope));
    out.push_str(&format!(
        "- **Period:** {} ({} → {})\n",
        period_kind_label(doc.period_kind),
        doc.period_start,
        doc.period_end
    ));
    out.push_str(&format!("- **Generated:** {}\n", doc.generated_at));
    out.push_str(&format!(
        "- **Source:** {}\n",
        if doc.from_cache { "cached" } else { "fresh" }
    ));

    out.push_str("\n## Totals\n");
    out.push_str(&format!("- Sessions: {}\n", doc.metrics.total_sessions));
    out.push_str(&format!(
        "- Tokens: in={} out={} cache_r={} cache_w={} total={}\n",
        doc.metrics.total_input_tokens,
        doc.metrics.total_output_tokens,
        doc.metrics.total_cache_read_tokens,
        doc.metrics.total_cache_write_tokens,
        doc.metrics.total_tokens
    ));
    let cost = match doc.metrics.estimated_cost_usd {
        Some(v) => format!("${:.2}", v),
        None => "no data".into(),
    };
    out.push_str(&format!("- Estimated cost: {}\n", cost));
    out.push_str(&format!("- Messages: {}\n", doc.metrics.message_count));
    out.push_str(&format!("- Tool calls: {}\n", doc.metrics.tool_call_count));
    out.push_str(&format!("- Errors: {}\n", doc.metrics.error_count));
    out.push_str(&format!("- Unique models: {}\n", doc.metrics.unique_models));
    let duration = match doc.metrics.avg_session_duration_seconds {
        Some(v) => format!("{:.0}s", v),
        None => "n/a".into(),
    };
    out.push_str(&format!("- Avg session duration: {}\n", duration));

    out.push_str("\n## Messages by role\n");
    if doc.metrics.messages_by_role.is_empty() {
        out.push_str("_(none)_\n");
    } else {
        for rb in &doc.metrics.messages_by_role {
            out.push_str(&format!("- {}: {}\n", rb.role, rb.count));
        }
    }

    out.push_str("\n## Top tools\n");
    if doc.metrics.top_tools.is_empty() {
        out.push_str("_(none)_\n");
    } else {
        for (i, tt) in doc.metrics.top_tools.iter().enumerate() {
            out.push_str(&format!("{}. {} — {}\n", i + 1, tt.tool_name, tt.count));
        }
    }

    out.push_str("\n## Top projects\n");
    if doc.metrics.top_projects.is_empty() {
        out.push_str("_(none)_\n");
    } else {
        for (i, tp) in doc.metrics.top_projects.iter().enumerate() {
            let slug = tp.slug.as_deref().unwrap_or("unknown");
            out.push_str(&format!(
                "{}. {} — {} sessions, {} tokens\n",
                i + 1,
                slug,
                tp.session_count,
                tp.total_tokens
            ));
        }
    }

    out.push_str("\n## Sources\n");
    if doc.metrics.sources.is_empty() {
        out.push_str("_(none)_\n");
    } else {
        for sb in &doc.metrics.sources {
            out.push_str(&format!(
                "- {} — {} sessions, {} tokens\n",
                sb.source, sb.session_count, sb.total_tokens
            ));
        }
    }

    out.push_str("\n## Daily breakdown\n");
    if doc.metrics.daily_breakdown.is_empty() {
        out.push_str("_(none)_\n");
    } else {
        out.push_str("| date       | sessions | messages | tools | tokens |\n");
        out.push_str("|------------|----------|----------|-------|--------|\n");
        for d in &doc.metrics.daily_breakdown {
            out.push_str(&format!(
                "| {} | {} | {} | {} | {} |\n",
                d.date, d.session_count, d.message_count, d.tool_call_count, d.total_tokens
            ));
        }
    }

    if !out.ends_with('\n') {
        out.push('\n');
    }
    out
}

pub fn render_json(doc: &ReportDocument) -> Result<String> {
    serde_json::to_string_pretty(doc).map_err(Into::into)
}
