use crate::auth::{AppState, AuthError, RequireSpaceAccess};
use axum::{
    extract::{Query, State},
    Json,
};
use chrono::{DateTime, Utc};
use pyra_ocean_shared::{CellValue, RowCellMap, SearchHit, SearchScope, SystemRole};
use serde::Deserialize;
use sqlx::FromRow;
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
/// strictly enforcing RBAC (`SuperAdmin` sees all; `Admin` & `User` only search permitted Spaces).
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
    let scope = params.scope.unwrap_or(SearchScope::All);
    let limit = params.limit.unwrap_or(50).clamp(1, 200);
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

    // 2. Search matching Row Cells in `grid_rows` (`JSONB`)
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
          AND s.is_archived = FALSE
          AND gr.cells::text ILIKE $1
          AND ($2::uuid IS NULL OR s.id = $2)
          AND ($3::uuid IS NULL OR pg.id = $3)
          AND (
              $4 = TRUE
              OR EXISTS (
                  SELECT 1 FROM space_members sm
                  WHERE sm.space_id = s.id AND sm.user_id = $5
              )
          )
        ORDER BY gr.last_modified DESC
        LIMIT $6
        "#,
    )
    .bind(&pattern)
    .bind(filter_space_id)
    .bind(filter_grid_id)
    .bind(is_super_admin)
    .bind(user.user_id)
    .bind(limit)
    .fetch_all(&state.db)
    .await?;

    let q_lower = trimmed_q.to_lowercase();
    for rm in row_matches {
        let snippet = extract_matching_cell_snippet(&rm.cells, &q_lower);
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

    Ok(Json(results))
}

fn extract_matching_cell_snippet(cells: &RowCellMap, q_lower: &str) -> String {
    let mut matched_parts: Vec<String> = Vec::new();
    let mut fallback_parts: Vec<String> = Vec::new();

    for val in cells.values() {
        let text = match val {
            CellValue::Text(s) => s.clone(),
            CellValue::Number(n) => n.to_string(),
            CellValue::Date(Some(d)) => d.format("%Y-%m-%d").to_string(),
            CellValue::Time {
                time_of_day: Some(t),
                ..
            } => t.format("%H:%M").to_string(),
            CellValue::Checklist(items) => items
                .iter()
                .map(|i| i.item.clone())
                .collect::<Vec<_>>()
                .join(", "),
            CellValue::Files(files) => files
                .iter()
                .map(|f| f.filename.clone())
                .collect::<Vec<_>>()
                .join(", "),
            _ => String::new(),
        };

        let trimmed = text.trim();
        if trimmed.is_empty() {
            continue;
        }
        if trimmed.to_lowercase().contains(q_lower) {
            matched_parts.push(trimmed.to_string());
        } else if fallback_parts.len() < 2 {
            fallback_parts.push(trimmed.to_string());
        }
    }

    if !matched_parts.is_empty() {
        matched_parts.join(" · ")
    } else if !fallback_parts.is_empty() {
        fallback_parts.join(" · ")
    } else {
        "Matching Grid Row".into()
    }
}

