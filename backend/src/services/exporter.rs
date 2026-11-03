use crate::auth::{resolve_grid_access_cached, AppState, AuthError, RequireSpaceAccess};
use axum::{
    extract::{Path, Query, State},
    http::{header, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
};
use chrono::Utc;
use pyra_ocean_shared::{
    generate_grid_a3_pdf_html, generate_grid_csv, generate_grid_markdown, slugify_grid_filename,
    GridColumn, GridRow, PageGrid,
};
use serde::Deserialize;
use std::collections::HashMap;
use uuid::Uuid;

#[derive(Debug, Deserialize)]
pub struct ExportGridQuery {
    pub format: Option<String>,
}

/// `GET /api/grids/{grid_id}/export?format=csv|md`
/// Exports a Grid's ordered columns and rows as:
/// - `format=csv`: UTF-8 BOM (`\u{FEFF}`) + RFC 4180 CSV (`text/csv; charset=utf-8`)
/// - `format=md`: GitHub-Flavored Markdown table (`text/markdown; charset=utf-8`)
/// - `format=pdf`: Executive A3 Landscape HTML print document (`text/html; charset=utf-8`)
pub async fn export_grid_handler(
    State(state): State<AppState>,
    RequireSpaceAccess(user): RequireSpaceAccess,
    Path(grid_id): Path<Uuid>,
    Query(query): Query<ExportGridQuery>,
) -> Result<Response, AuthError> {
    let access = resolve_grid_access_cached(&state, user, grid_id).await?;

    let grid: PageGrid = sqlx::query_as(
        r#"
        SELECT id, space_id, title, description, icon, position, created_by, is_archived, created_at, updated_at
        FROM pages_grids
        WHERE id = $1 AND is_archived = FALSE
        "#,
    )
    .bind(grid_id)
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(|| AuthError::NotFound(format!("Grid {grid_id} not found")))?;

    let space_name: String = sqlx::query_scalar("SELECT name FROM spaces WHERE id = $1")
        .bind(access.space_id)
        .fetch_optional(&state.db)
        .await?
        .unwrap_or_else(|| String::from("Workspace"));

    let columns: Vec<GridColumn> = sqlx::query_as(
        r#"
        SELECT id, grid_id, name, property_type, property_config, width_px, position, is_primary, is_hidden, created_at, updated_at
        FROM grid_columns
        WHERE grid_id = $1
        ORDER BY position ASC, created_at ASC
        "#,
    )
    .bind(grid_id)
    .fetch_all(&state.db)
    .await?;

    let rows: Vec<GridRow> = sqlx::query_as(
        r#"
        SELECT id, grid_id, position, row_order, cells, created_by, updated_by, created_at, last_modified
        FROM grid_rows
        WHERE grid_id = $1
        ORDER BY position ASC, row_order ASC, created_at ASC
        "#,
    )
    .bind(grid_id)
    .fetch_all(&state.db)
    .await?;

    let user_rows: Vec<(Uuid, String)> =
        sqlx::query_as("SELECT id, display_name FROM users")
            .fetch_all(&state.db)
            .await?;
    let user_display_map: HashMap<Uuid, String> = user_rows.into_iter().collect();

    let fmt = query
        .format
        .as_deref()
        .unwrap_or("csv")
        .trim()
        .to_lowercase();

    match fmt.as_str() {
        "md" | "markdown" => {
            let md_body = generate_grid_markdown(
                &grid.title,
                &space_name,
                &columns,
                &rows,
                &user_display_map,
                Utc::now(),
            );
            let filename = slugify_grid_filename(&grid.title, "md");
            let disposition = format!("attachment; filename=\"{filename}\"");
            let mut resp = (StatusCode::OK, md_body).into_response();
            resp.headers_mut().insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("text/markdown; charset=utf-8"),
            );
            if let Ok(val) = HeaderValue::from_str(&disposition) {
                resp.headers_mut()
                    .insert(header::CONTENT_DISPOSITION, val);
            }
            Ok(resp)
        }
        "pdf" | "html" => {
            let html_body = generate_grid_a3_pdf_html(
                &grid.title,
                &space_name,
                &columns,
                &rows,
                &user_display_map,
                Utc::now(),
            );
            let mut resp = (StatusCode::OK, html_body).into_response();
            resp.headers_mut().insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("text/html; charset=utf-8"),
            );
            Ok(resp)
        }
        _ => {
            let csv_body = generate_grid_csv(&columns, &rows, &user_display_map);
            let filename = slugify_grid_filename(&grid.title, "csv");
            let disposition = format!("attachment; filename=\"{filename}\"");
            let mut resp = (StatusCode::OK, csv_body).into_response();
            resp.headers_mut().insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("text/csv; charset=utf-8"),
            );
            if let Ok(val) = HeaderValue::from_str(&disposition) {
                resp.headers_mut()
                    .insert(header::CONTENT_DISPOSITION, val);
            }
            Ok(resp)
        }
    }
}

