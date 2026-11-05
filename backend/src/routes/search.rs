use crate::auth::{AppState, AuthError, RequireSpaceAccess};
use axum::{
    extract::{Query, State},
    Json,
};
use chrono::{DateTime, Utc};
use pyra_ocean_shared::{
    CellValue, ColumnPropertyConfig, ColumnPropertyType, GridColumn, NumberFormat, RowCellMap,
    SearchHit, SearchScope, SystemRole,
};
use serde::Deserialize;
use sqlx::FromRow;
use std::collections::HashMap;
use uuid::Uuid;

#[derive(Debug, Deserialize)]
pub struct SearchQueryParams {
    pub q: String,
    pub scope: Option<SearchScope>,
    pub space_id: Option<Uuid>,
    pub grid_id: Option<Uuid>,
    pub limit: Option<i64>,
}

/// `GET /api/search?q=...&scope=grid|space|all&space_id=...&grid_id=...`
/// 3-Tier Scoped Search Engine across Grid Titles and `grid_rows.cells` (`JSONB`),
/// resolving all 12 property types including `Select` & `MultiSelect` option labels,
/// `Checklist` items, `Files` filenames, `Person` display names/emails, `Checkbox` states,
/// and `Number`/`Currency` values while strictly enforcing RBAC.
pub async fn search_handler(
    State(state): State<AppState>,
    RequireSpaceAccess(user): RequireSpaceAccess,
    Query(params): Query<SearchQueryParams>,
) -> Result<Json<Vec<SearchHit>>, AuthError> {
    let trimmed_q = params.q.trim();
    if trimmed_q.is_empty() {
        return Ok(Json(Vec::new()));
    }

    let pattern = format!("%{trimmed_q}%");
    let q_lower = trimmed_q.to_lowercase();
    let scope = params.scope.unwrap_or(SearchScope::All);
    let limit = params.limit.unwrap_or(50).clamp(1, 200) as usize;
    let is_super_admin = user.system_role == SystemRole::SuperAdmin;

    let filter_space_id = match scope {
        SearchScope::Space | SearchScope::Grid => params.space_id,
        SearchScope::All => None,
    };
    let filter_grid_id = match scope {
        SearchScope::Grid => params.grid_id,
        _ => None,
    };

    let mut results: Vec<SearchHit> = Vec::new();

    // 1. Search matching Grid Page Titles (when searching across a Space or All Spaces)
    if scope != SearchScope::Grid {
        #[derive(FromRow)]
        struct GridTitleRow {
            space_id: Uuid,
            space_name: String,
            grid_id: Uuid,
            grid_title: String,
            updated_at: DateTime<Utc>,
        }

        let grid_matches: Vec<GridTitleRow> = sqlx::query_as(
            r#"
            SELECT
                s.id AS space_id,
                s.name AS space_name,
                pg.id AS grid_id,
                pg.title AS grid_title,
                pg.updated_at
            FROM pages_grids pg
            INNER JOIN spaces s ON s.id = pg.space_id
            WHERE pg.is_archived = FALSE
              AND pg.deleted_at IS NULL
              AND s.is_archived = FALSE
              AND pg.title ILIKE $1
              AND ($2::uuid IS NULL OR s.id = $2)
              AND (
                  $3 = TRUE
                  OR EXISTS (
                      SELECT 1 FROM space_members sm
                      WHERE sm.space_id = s.id AND sm.user_id = $4
                  )
              )
            ORDER BY pg.updated_at DESC
            LIMIT 15
            "#,
        )
        .bind(&pattern)
        .bind(filter_space_id)
        .bind(is_super_admin)
        .bind(user.user_id)
        .fetch_all(&state.db)
        .await?;

        for gm in grid_matches {
            results.push(SearchHit {
                space_id: gm.space_id,
                space_name: gm.space_name,
                grid_id: gm.grid_id,
                grid_title: gm.grid_title.clone(),
                row_id: None,
                match_type: "grid_title".into(),
                snippet: format!("Grid Page: {}", gm.grid_title),
                last_modified: gm.updated_at,
            });
        }
    }

    // 2. Load columns for permitted grids in scope so `Select` / `MultiSelect` option IDs
    // and column names/types can be resolved to human-readable labels.
    let columns_in_scope: Vec<GridColumn> = sqlx::query_as(
        r#"
        SELECT
            gc.id,
            gc.grid_id,
            gc.name,
            gc.property_type,
            gc.property_config,
            gc.width_px,
            gc.position,
            gc.is_primary,
            gc.is_hidden,
            gc.created_at,
            gc.updated_at
        FROM grid_columns gc
        INNER JOIN pages_grids pg ON pg.id = gc.grid_id
        INNER JOIN spaces s ON s.id = pg.space_id
        WHERE pg.is_archived = FALSE
          AND pg.deleted_at IS NULL
          AND s.is_archived = FALSE
          AND ($1::uuid IS NULL OR s.id = $1)
          AND ($2::uuid IS NULL OR pg.id = $2)
          AND (
              $3 = TRUE
              OR EXISTS (
                  SELECT 1 FROM space_members sm
                  WHERE sm.space_id = s.id AND sm.user_id = $4
              )
          )
        ORDER BY gc.position ASC, gc.created_at ASC
        "#,
    )
    .bind(filter_space_id)
    .bind(filter_grid_id)
    .bind(is_super_admin)
    .bind(user.user_id)
    .fetch_all(&state.db)
    .await?;

    // Map `grid_id -> Vec<GridColumn>` and find any `Select`/`MultiSelect` option UUIDs whose label matches `q_lower`
    let mut grid_columns_map: HashMap<Uuid, Vec<GridColumn>> = HashMap::new();
    let mut matching_option_ids: Vec<String> = Vec::new();

    for col in columns_in_scope {
        match &col.property_config {
            ColumnPropertyConfig::Select { options }
            | ColumnPropertyConfig::MultiSelect { options } => {
                for opt in options {
                    if opt.label.to_lowercase().contains(&q_lower) {
                        matching_option_ids.push(opt.id.to_string());
                    }
                }
            }
            _ => {}
        }
        grid_columns_map.entry(col.grid_id).or_default().push(col);
    }

    // Load user lookup map for `Person` property resolution
    let user_rows: Vec<(Uuid, String, String)> =
        sqlx::query_as("SELECT id, display_name, email FROM users")
            .fetch_all(&state.db)
            .await
            .unwrap_or_default();
    let mut user_lookup: HashMap<Uuid, (String, String)> = HashMap::new();
    let mut matching_user_ids: Vec<String> = Vec::new();
    for (uid, dname, email) in user_rows {
        if dname.to_lowercase().contains(&q_lower) || email.to_lowercase().contains(&q_lower) {
            matching_user_ids.push(uid.to_string());
        }
        user_lookup.insert(uid, (dname, email));
    }

    // Check if the query is a boolean keyword for `Checkbox` / `Checklist`
    let is_boolean_or_status_query = matches!(
        q_lower.as_str(),
        "true" | "false" | "checked" | "unchecked" | "yes" | "no" | "completed" | "pending"
    );

    // 3. Query candidate rows in `grid_rows` (`JSONB`)
    // Matches either `gr.cells::text ILIKE $1`, or any matched Select/MultiSelect option UUID, or Person UUID, or boolean query.
    #[derive(FromRow)]
    struct RowMatchRecord {
        space_id: Uuid,
        space_name: String,
        grid_id: Uuid,
        grid_title: String,
        row_id: Uuid,
        #[sqlx(json)]
        cells: RowCellMap,
        last_modified: DateTime<Utc>,
    }

    let mut extra_patterns: Vec<String> = Vec::new();
    for oid in &matching_option_ids {
        extra_patterns.push(format!("%{oid}%"));
    }
    for uid in &matching_user_ids {
        extra_patterns.push(format!("%{uid}%"));
    }

    let row_matches: Vec<RowMatchRecord> = sqlx::query_as(
        r#"
        SELECT
            s.id AS space_id,
            s.name AS space_name,
            pg.id AS grid_id,
            pg.title AS grid_title,
            gr.id AS row_id,
            gr.cells,
            gr.last_modified
        FROM grid_rows gr
        INNER JOIN pages_grids pg ON pg.id = gr.grid_id
        INNER JOIN spaces s ON s.id = pg.space_id
        WHERE pg.is_archived = FALSE
          AND pg.deleted_at IS NULL
          AND s.is_archived = FALSE
          AND ($2::uuid IS NULL OR s.id = $2)
          AND ($3::uuid IS NULL OR pg.id = $3)
          AND (
              gr.cells::text ILIKE $1
              OR gr.cells::text ILIKE ANY($6)
              OR $7 = TRUE
          )
          AND (
              $4 = TRUE
              OR EXISTS (
                  SELECT 1 FROM space_members sm
                  WHERE sm.space_id = s.id AND sm.user_id = $5
              )
          )
        ORDER BY gr.last_modified DESC
        LIMIT $8
        "#,
    )
    .bind(&pattern)
    .bind(filter_space_id)
    .bind(filter_grid_id)
    .bind(is_super_admin)
    .bind(user.user_id)
    .bind(&extra_patterns)
    .bind(is_boolean_or_status_query)
    .bind((limit * 3).max(100) as i64)
    .fetch_all(&state.db)
    .await?;

    for rm in row_matches {
        if results.len() >= limit {
            break;
        }
        let cols = grid_columns_map
            .get(&rm.grid_id)
            .map(|v| v.as_slice())
            .unwrap_or(&[]);
        if let Some(snippet) =
            extract_matching_cell_snippet_with_schema(&rm.cells, cols, &user_lookup, &q_lower)
        {
            results.push(SearchHit {
                space_id: rm.space_id,
                space_name: rm.space_name,
                grid_id: rm.grid_id,
                grid_title: rm.grid_title,
                row_id: Some(rm.row_id),
                match_type: "row_cell".into(),
                snippet,
                last_modified: rm.last_modified,
            });
        }
    }

    Ok(Json(results))
}

fn property_type_short_label(pt: ColumnPropertyType) -> &'static str {
    match pt {
        ColumnPropertyType::Text => "Text",
        ColumnPropertyType::Number => "Number",
        ColumnPropertyType::Select => "Select",
        ColumnPropertyType::MultiSelect => "MultiSelect",
        ColumnPropertyType::Date => "Date",
        ColumnPropertyType::Time => "Time",
        ColumnPropertyType::Files => "Files",
        ColumnPropertyType::Checkbox => "Checkbox",
        ColumnPropertyType::Checklist => "Checklist",
        ColumnPropertyType::LastModified => "LastModified",
        ColumnPropertyType::Person => "Person",
        ColumnPropertyType::Password => "Password",
    }
}

/// Inspects a row's `cells` against its `GridColumn` schema (including `Select` & `MultiSelect` option labels,
/// `Checklist` items, `Files` filenames, `Person` names/emails, `Checkbox` states, and `Number` formatting)
/// and returns a rich snippet showing the primary row title + matched column name, property type, and human-readable value.
fn extract_matching_cell_snippet_with_schema(
    cells: &RowCellMap,
    columns: &[GridColumn],
    user_lookup: &HashMap<Uuid, (String, String)>,
    q_lower: &str,
) -> Option<String> {
    let mut matched_parts: Vec<String> = Vec::new();
    let mut primary_title: Option<String> = None;

    for col in columns {
        let cell_opt = cells.get(&col.id);
        let (display_text, searchable_tokens) =
            resolve_cell_display_and_tokens(col, cell_opt, user_lookup);

        if col.is_primary && !display_text.trim().is_empty() {
            primary_title = Some(display_text.trim().to_string());
        }

        let matched = searchable_tokens
            .iter()
            .any(|tok| tok.to_lowercase().contains(q_lower));

        if matched {
            let shown = if display_text.trim().is_empty() {
                searchable_tokens.join(", ")
            } else {
                display_text.trim().to_string()
            };
            matched_parts.push(format!(
                "[{} · {}] {}",
                col.name,
                property_type_short_label(col.property_type),
                shown
            ));
        }
    }

    // Also inspect any unmapped JSONB cells in case a column isn't in `columns`
    if matched_parts.is_empty() {
        for (cid, val) in cells {
            if columns.iter().any(|c| &c.id == cid) {
                continue;
            }
            if let Ok(raw_json) = serde_json::to_string(val) {
                if raw_json.to_lowercase().contains(q_lower) {
                    matched_parts.push(raw_json);
                }
            }
        }
    }

    if matched_parts.is_empty() {
        return None;
    }

    let joined_matches = matched_parts.join(" · ");
    if let Some(pri) = primary_title {
        if !joined_matches.contains(&pri) {
            return Some(format!("{pri} — {joined_matches}"));
        }
    }
    Some(joined_matches)
}

fn resolve_cell_display_and_tokens(
    col: &GridColumn,
    cell: Option<&CellValue>,
    user_lookup: &HashMap<Uuid, (String, String)>,
) -> (String, Vec<String>) {
    let Some(val) = cell else {
        if col.property_type == ColumnPropertyType::Checkbox {
            return (
                "Unchecked".to_string(),
                vec!["false".into(), "unchecked".into(), "no".into()],
            );
        }
        return (String::new(), Vec::new());
    };

    match val {
        CellValue::Text(s) => (s.clone(), vec![s.clone()]),
        CellValue::Password(s) => ("••••••••".to_string(), vec![s.clone()]),
        CellValue::Number(n) => {
            let raw = n.to_string();
            let formatted = match &col.property_config {
                ColumnPropertyConfig::Number { format } => match format {
                    NumberFormat::Integer => format!("{n:.0}"),
                    NumberFormat::Decimal { precision } => {
                        format!("{n:.prec$}", prec = *precision as usize)
                    }
                    NumberFormat::Percentage { precision } => {
                        format!("{n:.prec$}%", prec = *precision as usize)
                    }
                    NumberFormat::Currency { symbol, precision } => {
                        format!("{symbol} {n:.prec$}", prec = *precision as usize)
                    }
                },
                _ => raw.clone(),
            };
            (formatted.clone(), vec![raw, formatted])
        }
        CellValue::Select(Some(oid)) => {
            let mut tokens = vec![oid.to_string()];
            let mut label = oid.to_string();
            if let ColumnPropertyConfig::Select { options }
            | ColumnPropertyConfig::MultiSelect { options } = &col.property_config
            {
                if let Some(opt) = options.iter().find(|o| &o.id == oid) {
                    label = opt.label.clone();
                    tokens.push(opt.label.clone());
                }
            }
            (label, tokens)
        }
        CellValue::Select(None) => (String::new(), Vec::new()),
        CellValue::MultiSelect(oids) => {
            let mut labels: Vec<String> = Vec::new();
            let mut tokens: Vec<String> = Vec::new();
            if let ColumnPropertyConfig::MultiSelect { options }
            | ColumnPropertyConfig::Select { options } = &col.property_config
            {
                for oid in oids {
                    tokens.push(oid.to_string());
                    if let Some(opt) = options.iter().find(|o| &o.id == oid) {
                        labels.push(opt.label.clone());
                        tokens.push(opt.label.clone());
                    }
                }
            } else {
                for oid in oids {
                    tokens.push(oid.to_string());
                }
            }
            (labels.join(", "), tokens)
        }
        CellValue::Date(Some(d)) => {
            let s = d.format("%Y-%m-%d").to_string();
            (s.clone(), vec![s, d.format("%d/%m/%Y").to_string()])
        }
        CellValue::Date(None) => (String::new(), Vec::new()),
        CellValue::Time {
            time_of_day: Some(t),
            ..
        } => {
            let s = t.format("%H:%M").to_string();
            (s.clone(), vec![s, t.format("%H:%M:%S").to_string()])
        }
        CellValue::Time { .. } => (String::new(), Vec::new()),
        CellValue::Checkbox(b) => {
            if *b {
                (
                    "Checked (✓)".to_string(),
                    vec!["true".into(), "checked".into(), "yes".into()],
                )
            } else {
                (
                    "Unchecked".to_string(),
                    vec!["false".into(), "unchecked".into(), "no".into()],
                )
            }
        }
        CellValue::Checklist(items) => {
            let mut tokens: Vec<String> = Vec::new();
            let mut display_items: Vec<String> = Vec::new();
            for item in items {
                tokens.push(item.item.clone());
                if item.completed {
                    tokens.push("completed".into());
                    display_items.push(format!("✓ {}", item.item));
                } else {
                    tokens.push("pending".into());
                    display_items.push(format!("☐ {}", item.item));
                }
            }
            (display_items.join(", "), tokens)
        }
        CellValue::Files(files) => {
            let names: Vec<String> = files.iter().map(|f| f.filename.clone()).collect();
            let mut tokens = names.clone();
            for f in files {
                tokens.push(f.storage_url.clone());
            }
            (names.join(", "), tokens)
        }
        CellValue::Person(uids) => {
            let mut names: Vec<String> = Vec::new();
            let mut tokens: Vec<String> = Vec::new();
            for uid in uids {
                tokens.push(uid.to_string());
                if let Some((dname, email)) = user_lookup.get(uid) {
                    names.push(dname.clone());
                    tokens.push(dname.clone());
                    tokens.push(email.clone());
                }
            }
            (names.join(", "), tokens)
        }
        CellValue::LastModified(dt) => {
            let s = dt.format("%Y-%m-%d %H:%M").to_string();
            (s.clone(), vec![s])
        }
    }
}
