use crate::auth::{
    record_audit_log, resolve_grid_access_cached, resolve_space_access_cached, AppState, AuthError,
    RequireSpaceAccess,
};
use axum::{
    extract::{Multipart, Path, Query, State},
    http::StatusCode,
    Json,
};
use chrono::{NaiveDate, NaiveTime, Utc};
use pyra_ocean_shared::{
    BulkDeleteGridsRequest, BulkDeleteGridsResponse, BulkDeleteRowsRequest, BulkDeleteRowsResponse,
    BulkPatchCellsRequest, CellValue, ChecklistItem, ColumnPropertyConfig, ColumnPropertyType,
    CreateColumnRequest, CreateGridRequest, CreateRowRequest, FileAttachment, FilterOperator,
    GridColumn, GridHistoryDetail, GridHistoryEntry, GridRow, GridSchemaResponse, GridSnapshotData,
    PageGrid, PatchRowRequest, ReorderColumnsRequest, ReorderRowsRequest,
    RestoreGridHistoryResponse, RestoreRowsRequest, RowCellMap, SelectOption, SortDirection,
    TagColor, UpdateColumnRequest, UpdateGridRequest,
};
use serde::Deserialize;
use serde_json::json;
use sqlx::{Postgres, QueryBuilder};
use std::collections::{BTreeMap, HashMap};
use tokio::{fs, io::AsyncWriteExt};
use uuid::Uuid;

const CONVERT_TAG_PALETTE: [TagColor; 10] = [
    TagColor::Blue,
    TagColor::Emerald,
    TagColor::Amber,
    TagColor::Violet,
    TagColor::Rose,
    TagColor::Cyan,
    TagColor::Orange,
    TagColor::Indigo,
    TagColor::Teal,
    TagColor::Pink,
];

// ============================================================================
// 1. Query Parameters for Grid Rows
// ============================================================================

#[derive(Debug, Deserialize)]
pub struct ListRowsQuery {
    pub sort_by: Option<Uuid>,
    pub order: Option<SortDirection>,
    pub sorts: Option<String>,
    pub filter_col: Option<Uuid>,
    pub filter_op: Option<FilterOperator>,
    pub filter_val: Option<String>,
    pub limit: Option<i64>,
    pub offset: Option<i64>,
}

// ============================================================================
// 2. Space Grid Discovery & Schema Management
// ============================================================================

/// `GET /api/spaces/{space_id}/grids`
/// Lists all active Grid Pages within a Space for the Sidebar tree navigation.
pub async fn list_space_grids(
    State(state): State<AppState>,
    RequireSpaceAccess(user): RequireSpaceAccess,
    Path(space_id): Path<Uuid>,
) -> Result<Json<Vec<PageGrid>>, AuthError> {
    let _access = resolve_space_access_cached(&state, user, space_id).await?;

    if let Some(cached) = state.cache.get_space_grids(&space_id) {
        return Ok(Json(cached));
    }

    let grids: Vec<PageGrid> = sqlx::query_as(
        r#"
        SELECT id, space_id, title, description, icon, position, created_by, is_archived, created_at, updated_at
        FROM pages_grids
        WHERE space_id = $1 AND is_archived = FALSE
        ORDER BY position ASC, created_at ASC
        "#,
    )
    .bind(space_id)
    .fetch_all(&state.db)
    .await?;

    state.cache.set_space_grids(space_id, grids.clone());
    Ok(Json(grids))
}

/// `POST /api/spaces/{space_id}/grids`
/// Creates a new Grid Page inside a Space and seeds a primary `Text` column + `LastModified` column.
pub async fn create_grid(
    State(state): State<AppState>,
    RequireSpaceAccess(user): RequireSpaceAccess,
    Path(space_id): Path<Uuid>,
    Json(payload): Json<CreateGridRequest>,
) -> Result<(StatusCode, Json<GridSchemaResponse>), AuthError> {
    let access = resolve_space_access_cached(&state, user.clone(), space_id).await?;
    access.ensure_write()?;

    if payload.title.trim().is_empty() {
        return Err(AuthError::BadRequest("Grid title cannot be empty".into()));
    }

    let mut tx = state.db.begin().await?;

    let grid: PageGrid = sqlx::query_as(
        r#"
        INSERT INTO pages_grids (space_id, title, description, icon, position, created_by, is_archived)
        VALUES ($1, $2, $3, $4, $5, $6, FALSE)
        RETURNING id, space_id, title, description, icon, position, created_by, is_archived, created_at, updated_at
        "#,
    )
    .bind(space_id)
    .bind(payload.title.trim())
    .bind(&payload.description)
    .bind(&payload.icon)
    .bind(payload.position.unwrap_or(0))
    .bind(user.user_id)
    .fetch_one(&mut *tx)
    .await?;

    let primary_config = ColumnPropertyConfig::Text {
        rich_text: false,
        max_length: None,
    };
    let col_primary: GridColumn = sqlx::query_as(
        r#"
        INSERT INTO grid_columns (grid_id, name, property_type, property_config, width_px, position, is_primary, is_hidden)
        VALUES ($1, 'Name', 'text', $2, 240, 0, TRUE, FALSE)
        RETURNING id, grid_id, name, property_type, property_config, width_px, position, is_primary, is_hidden, created_at, updated_at
        "#,
    )
    .bind(grid.id)
    .bind(sqlx::types::Json(primary_config))
    .fetch_one(&mut *tx)
    .await?;

    let last_mod_config = ColumnPropertyConfig::LastModified;
    let col_last_mod: GridColumn = sqlx::query_as(
        r#"
        INSERT INTO grid_columns (grid_id, name, property_type, property_config, width_px, position, is_primary, is_hidden)
        VALUES ($1, 'Last Modified', 'last_modified', $2, 180, 1, FALSE, FALSE)
        RETURNING id, grid_id, name, property_type, property_config, width_px, position, is_primary, is_hidden, created_at, updated_at
        "#,
    )
    .bind(grid.id)
    .bind(sqlx::types::Json(last_mod_config))
    .fetch_one(&mut *tx)
    .await?;

    tx.commit().await?;

    let schema = GridSchemaResponse {
        grid: grid.clone(),
        columns: vec![col_primary, col_last_mod],
    };
    state.cache.invalidate_space(&space_id);
    state.cache.set_grid_schema(grid.id, schema.clone());

    record_audit_log(
        &state.db,
        user.user_id,
        Some(space_id),
        Some(grid.id),
        "grid.create",
        json!({ "title": grid.title }),
    )
    .await?;

    Ok((StatusCode::CREATED, Json(schema)))
}

pub fn default_config_for_type(prop_type: ColumnPropertyType) -> ColumnPropertyConfig {
    match prop_type {
        ColumnPropertyType::Text => ColumnPropertyConfig::Text {
            rich_text: false,
            max_length: None,
        },
        ColumnPropertyType::Number => ColumnPropertyConfig::Number {
            format: Default::default(),
        },
        ColumnPropertyType::Select => ColumnPropertyConfig::Select {
            options: Vec::new(),
        },
        ColumnPropertyType::MultiSelect => ColumnPropertyConfig::MultiSelect {
            options: Vec::new(),
        },
        ColumnPropertyType::Date => ColumnPropertyConfig::Date {
            display_format: "%Y-%m-%d".into(),
            include_time: false,
        },
        ColumnPropertyType::Time => ColumnPropertyConfig::Time {
            use_24_hour: true,
            is_duration: false,
        },
        ColumnPropertyType::Files => ColumnPropertyConfig::Files {
            allowed_mime_prefixes: Vec::new(),
            max_file_size_bytes: 50 * 1024 * 1024,
        },
        ColumnPropertyType::Checkbox => ColumnPropertyConfig::Checkbox,
        ColumnPropertyType::Checklist => ColumnPropertyConfig::Checklist {
            show_progress_bar: true,
        },
        ColumnPropertyType::LastModified => ColumnPropertyConfig::LastModified,
        ColumnPropertyType::Person => ColumnPropertyConfig::Person {
            allow_multiple: true,
        },
    }
}

/// `GET /api/grids/{grid_id}`
/// Returns the `PageGrid` metadata and ordered `Vec<GridColumn>` schema (served from `AppCache` when fresh).
pub async fn get_grid_schema(
    State(state): State<AppState>,
    RequireSpaceAccess(user): RequireSpaceAccess,
    Path(grid_id): Path<Uuid>,
) -> Result<Json<GridSchemaResponse>, AuthError> {
    let _access = resolve_grid_access_cached(&state, user, grid_id).await?;

    if let Some(cached) = state.cache.get_grid_schema(&grid_id) {
        return Ok(Json(cached));
    }

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

    let schema = GridSchemaResponse { grid, columns };
    state.cache.set_grid_schema(grid_id, schema.clone());
    Ok(Json(schema))
}

/// `PATCH /api/grids/{grid_id}`
/// Renames a Grid Page or updates its description/icon (`CanEdit` / `SpaceAdmin` / `SuperAdmin`).
pub async fn update_grid(
    State(state): State<AppState>,
    RequireSpaceAccess(user): RequireSpaceAccess,
    Path(grid_id): Path<Uuid>,
    Json(payload): Json<UpdateGridRequest>,
) -> Result<Json<PageGrid>, AuthError> {
    let access = resolve_grid_access_cached(&state, user.clone(), grid_id).await?;
    access.ensure_write()?;

    let existing: PageGrid = sqlx::query_as(
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

    let new_title = payload
        .name
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(&existing.title)
        .to_string();

    let new_description = match payload.description {
        Some(desc) => Some(desc),
        None => existing.description.clone(),
    };
    let new_icon = match payload.icon {
        Some(ic) => Some(ic),
        None => existing.icon.clone(),
    };

    let updated: PageGrid = sqlx::query_as(
        r#"
        UPDATE pages_grids
        SET title = $2,
            description = $3,
            icon = $4,
            updated_at = NOW()
        WHERE id = $1 AND is_archived = FALSE
        RETURNING id, space_id, title, description, icon, position, created_by, is_archived, created_at, updated_at
        "#,
    )
    .bind(grid_id)
    .bind(&new_title)
    .bind(&new_description)
    .bind(&new_icon)
    .fetch_one(&state.db)
    .await?;

    state.cache.invalidate_grid(&grid_id);

    record_audit_log(
        &state.db,
        user.user_id,
        Some(access.space_id),
        Some(grid_id),
        "grid.rename",
        json!({ "old_title": existing.title, "new_title": updated.title }),
    )
    .await?;

    Ok(Json(updated))
}

/// `DELETE /api/grids/{grid_id}`
/// Hard-deletes a single Grid Page (`ON DELETE CASCADE` cleans up `grid_columns`, `grid_rows`,
/// and `grid_history_snapshots`), invalidates `AppCache`, and records an async audit log entry.
pub async fn delete_grid(
    State(state): State<AppState>,
    RequireSpaceAccess(user): RequireSpaceAccess,
    Path(grid_id): Path<Uuid>,
) -> Result<StatusCode, AuthError> {
    let access = resolve_grid_access_cached(&state, user.clone(), grid_id).await?;
    access.ensure_write()?;

    let result = sqlx::query("DELETE FROM pages_grids WHERE id = $1")
        .bind(grid_id)
        .execute(&state.db)
        .await?;

    if result.rows_affected() == 0 {
        return Err(AuthError::NotFound(format!("Grid {grid_id} not found")));
    }

    state.cache.invalidate_grid(&grid_id);
    state.cache.invalidate_space(&access.space_id);

    record_audit_log(
        &state.db,
        user.user_id,
        Some(access.space_id),
        Some(grid_id),
        "grid.delete",
        json!({ "deleted_grid_id": grid_id }),
    )
    .await?;

    Ok(StatusCode::NO_CONTENT)
}

/// `DELETE /api/spaces/{space_id}/grids/bulk`
/// Atomically deletes multiple Grids within a Space (`DELETE FROM pages_grids WHERE space_id = $1 AND id = ANY($2)`),
/// invalidates the in-memory cache for all deleted `grid_ids`, and returns `BulkDeleteGridsResponse`.
pub async fn bulk_delete_grids(
    State(state): State<AppState>,
    RequireSpaceAccess(user): RequireSpaceAccess,
    Path(space_id): Path<Uuid>,
    Json(payload): Json<BulkDeleteGridsRequest>,
) -> Result<Json<BulkDeleteGridsResponse>, AuthError> {
    let access = resolve_space_access_cached(&state, user.clone(), space_id).await?;
    access.ensure_write()?;

    if payload.grid_ids.is_empty() {
        return Ok(Json(BulkDeleteGridsResponse { deleted_count: 0 }));
    }

    let result = sqlx::query("DELETE FROM pages_grids WHERE space_id = $1 AND id = ANY($2)")
        .bind(space_id)
        .bind(&payload.grid_ids)
        .execute(&state.db)
        .await?;

    let deleted_count = result.rows_affected();

    for gid in &payload.grid_ids {
        state.cache.invalidate_grid(gid);
    }
    state.cache.invalidate_space(&space_id);

    record_audit_log(
        &state.db,
        user.user_id,
        Some(space_id),
        None,
        "grid.bulk_delete",
        json!({ "deleted_count": deleted_count, "grid_ids": payload.grid_ids }),
    )
    .await?;

    Ok(Json(BulkDeleteGridsResponse { deleted_count }))
}

/// `POST /api/grids/{grid_id}/columns`
pub async fn create_column(
    State(state): State<AppState>,
    RequireSpaceAccess(user): RequireSpaceAccess,
    Path(grid_id): Path<Uuid>,
    Json(payload): Json<CreateColumnRequest>,
) -> Result<(StatusCode, Json<GridColumn>), AuthError> {
    let access = resolve_grid_access_cached(&state, user.clone(), grid_id).await?;
    access.ensure_write()?;

    if payload.name.trim().is_empty() {
        return Err(AuthError::BadRequest("Column name cannot be empty".into()));
    }

    let config = payload
        .property_config
        .unwrap_or_else(|| default_config_for_type(payload.property_type));

    let width_px = payload.width_px.unwrap_or(180).clamp(80, 800);

    let next_pos: (i32,) = sqlx::query_as(
        "SELECT COALESCE(MAX(position) + 1, 0) FROM grid_columns WHERE grid_id = $1",
    )
    .bind(grid_id)
    .fetch_one(&state.db)
    .await?;

    let column: GridColumn = sqlx::query_as(
        r#"
        INSERT INTO grid_columns (
            grid_id, name, property_type, property_config, width_px, position, is_primary, is_hidden
        )
        VALUES ($1, $2, $3, $4, $5, $6, $7, FALSE)
        RETURNING id, grid_id, name, property_type, property_config, width_px, position, is_primary, is_hidden, created_at, updated_at
        "#,
    )
    .bind(grid_id)
    .bind(payload.name.trim())
    .bind(payload.property_type)
    .bind(sqlx::types::Json(config))
    .bind(width_px)
    .bind(payload.position.unwrap_or(next_pos.0))
    .bind(payload.is_primary.unwrap_or(false))
    .fetch_one(&state.db)
    .await?;

    state.cache.invalidate_grid(&grid_id);

    record_audit_log(
        &state.db,
        user.user_id,
        Some(access.space_id),
        Some(grid_id),
        "grid.column.create",
        json!({ "column_id": column.id, "name": column.name, "type": column.property_type }),
    )
    .await?;

    let _ = record_grid_history_snapshot(
        &state.db,
        grid_id,
        "column.create",
        Some(user.user_id),
        Some(&user.email),
        &format!("Added column '{}' ({:?})", column.name, column.property_type),
    )
    .await;

    Ok((StatusCode::CREATED, Json(column)))
}

/// `PATCH /api/grids/{grid_id}/columns/reorder`
/// Atomically updates `position` for all columns in a Grid after drag-and-drop or Move Left/Right.
pub async fn reorder_columns(
    State(state): State<AppState>,
    RequireSpaceAccess(user): RequireSpaceAccess,
    Path(grid_id): Path<Uuid>,
    Json(payload): Json<ReorderColumnsRequest>,
) -> Result<Json<Vec<GridColumn>>, AuthError> {
    let access = resolve_grid_access_cached(&state, user.clone(), grid_id).await?;
    access.ensure_write()?;

    let mut tx = state.db.begin().await?;

    for (idx, col_id) in payload.column_ids.iter().enumerate() {
        sqlx::query(
            "UPDATE grid_columns SET position = $3 WHERE id = $1 AND grid_id = $2",
        )
        .bind(col_id)
        .bind(grid_id)
        .bind(idx as i32)
        .execute(&mut *tx)
        .await?;
    }

    let columns: Vec<GridColumn> = sqlx::query_as(
        r#"
        SELECT id, grid_id, name, property_type, property_config, width_px, position, is_primary, is_hidden, created_at, updated_at
        FROM grid_columns
        WHERE grid_id = $1
        ORDER BY position ASC, created_at ASC
        "#,
    )
    .bind(grid_id)
    .fetch_all(&mut *tx)
    .await?;

    tx.commit().await?;

    state.cache.invalidate_grid(&grid_id);

    record_audit_log(
        &state.db,
        user.user_id,
        Some(access.space_id),
        Some(grid_id),
        "grid.columns.reorder",
        json!({ "column_ids": payload.column_ids }),
    )
    .await?;

    Ok(Json(columns))
}

/// `PATCH /api/grids/{grid_id}/columns/{col_id}`
/// Updates column name, width, position, select options, or converts `property_type` while safely
/// casting existing cell values in `grid_rows.cells` (`JSONB`) without data loss.
pub async fn update_column(
    State(state): State<AppState>,
    RequireSpaceAccess(user): RequireSpaceAccess,
    Path((grid_id, col_id)): Path<(Uuid, Uuid)>,
    Json(payload): Json<UpdateColumnRequest>,
) -> Result<Json<GridColumn>, AuthError> {
    let access = resolve_grid_access_cached(&state, user.clone(), grid_id).await?;
    access.ensure_write()?;

    let mut tx = state.db.begin().await?;

    let existing: GridColumn = sqlx::query_as(
        r#"
        SELECT id, grid_id, name, property_type, property_config, width_px, position, is_primary, is_hidden, created_at, updated_at
        FROM grid_columns
        WHERE id = $1 AND grid_id = $2
        "#,
    )
    .bind(col_id)
    .bind(grid_id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(|| AuthError::NotFound(format!("Column {col_id} not found in grid {grid_id}")))?;

    let new_name = payload
        .name
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(&existing.name)
        .to_string();

    let target_type = payload.property_type.unwrap_or(existing.property_type);
    let type_changed = target_type != existing.property_type;

    let mut new_config = match payload.property_config {
        Some(cfg) => cfg,
        None if type_changed => default_config_for_type(target_type),
        None => existing.property_config.clone(),
    };

    let new_width = payload
        .width_px
        .unwrap_or(existing.width_px)
        .clamp(80, 800);
    let new_position = payload.position.unwrap_or(existing.position);
    let new_hidden = payload.is_hidden.unwrap_or(existing.is_hidden);

    // If the column's `property_type` was changed, safely convert existing cells in `grid_rows`
    if type_changed {
        let mut rows: Vec<GridRow> = sqlx::query_as(
            r#"
            SELECT id, grid_id, position, row_order, cells, created_by, updated_by, created_at, last_modified
            FROM grid_rows
            WHERE grid_id = $1
            ORDER BY position ASC, row_order ASC, created_at ASC
            "#,
        )
        .bind(grid_id)
        .fetch_all(&mut *tx)
        .await?;

        // Extract human-readable string representations of each row's existing cell value
        let mut extracted_strings: HashMap<Uuid, String> = HashMap::new();
        for row in &rows {
            if let Some(old_val) = row.cells.get(&col_id) {
                let s = cell_to_convertible_string(old_val, &existing.property_config);
                if !s.trim().is_empty() {
                    extracted_strings.insert(row.id, s);
                }
            }
        }

        // Auto-build `SelectOption` tags if converting into `Select` or `MultiSelect`
        if matches!(target_type, ColumnPropertyType::Select | ColumnPropertyType::MultiSelect) {
            let mut labels: BTreeMap<String, SelectOption> = BTreeMap::new();
            for text in extracted_strings.values() {
                if target_type == ColumnPropertyType::MultiSelect {
                    for token in text.split(',').map(str::trim).filter(|t| !t.is_empty()) {
                        let idx = labels.len();
                        labels
                            .entry(token.to_string())
                            .or_insert_with(|| SelectOption {
                                id: Uuid::new_v4(),
                                label: token.to_string(),
                                color: CONVERT_TAG_PALETTE[idx % CONVERT_TAG_PALETTE.len()],
                            });
                    }
                } else {
                    let trimmed = text.trim();
                    if !trimmed.is_empty() {
                        let idx = labels.len();
                        labels
                            .entry(trimmed.to_string())
                            .or_insert_with(|| SelectOption {
                                id: Uuid::new_v4(),
                                label: trimmed.to_string(),
                                color: CONVERT_TAG_PALETTE[idx % CONVERT_TAG_PALETTE.len()],
                            });
                    }
                }
            }

            let generated_opts: Vec<SelectOption> = labels.into_values().collect();
            new_config = if target_type == ColumnPropertyType::Select {
                ColumnPropertyConfig::Select {
                    options: generated_opts,
                }
            } else {
                ColumnPropertyConfig::MultiSelect {
                    options: generated_opts,
                }
            };
        }

        let now = Utc::now();
        for row in &mut rows {
            if let Some(raw_str) = extracted_strings.get(&row.id) {
                let converted =
                    convert_string_to_cell(raw_str, target_type, &new_config, user.user_id, now);
                row.cells.insert(col_id, converted);
            } else if target_type == ColumnPropertyType::LastModified {
                row.cells
                    .insert(col_id, CellValue::LastModified(row.last_modified));
            } else {
                row.cells.remove(&col_id);
            }

            sqlx::query("UPDATE grid_rows SET cells = $2 WHERE id = $1")
                .bind(row.id)
                .bind(sqlx::types::Json(&row.cells))
                .execute(&mut *tx)
                .await?;
        }
    } else if matches!(
        target_type,
        ColumnPropertyType::Select | ColumnPropertyType::MultiSelect
    ) {
        // If `Select` or `MultiSelect` option tags were deleted, clean removed option IDs from existing cells
        let valid_ids: std::collections::HashSet<Uuid> = match &new_config {
            ColumnPropertyConfig::Select { options }
            | ColumnPropertyConfig::MultiSelect { options } => {
                options.iter().map(|o| o.id).collect()
            }
            _ => std::collections::HashSet::new(),
        };

        let mut rows: Vec<GridRow> = sqlx::query_as(
            r#"
            SELECT id, grid_id, position, row_order, cells, created_by, updated_by, created_at, last_modified
            FROM grid_rows
            WHERE grid_id = $1
            "#,
        )
        .bind(grid_id)
        .fetch_all(&mut *tx)
        .await?;

        for row in &mut rows {
            let mut modified = false;
            if let Some(cell) = row.cells.get_mut(&col_id) {
                match cell {
                    CellValue::Select(Some(oid)) => {
                        if !valid_ids.contains(oid) {
                            *cell = CellValue::Select(None);
                            modified = true;
                        }
                    }
                    CellValue::MultiSelect(oids) => {
                        let before = oids.len();
                        oids.retain(|id| valid_ids.contains(id));
                        if oids.len() != before {
                            modified = true;
                        }
                    }
                    _ => {}
                }
            }
            if modified {
                sqlx::query("UPDATE grid_rows SET cells = $2 WHERE id = $1")
                    .bind(row.id)
                    .bind(sqlx::types::Json(&row.cells))
                    .execute(&mut *tx)
                    .await?;
            }
        }
    }

    let updated: GridColumn = sqlx::query_as(
        r#"
        UPDATE grid_columns
        SET name = $3,
            property_type = $4,
            property_config = $5,
            width_px = $6,
            position = $7,
            is_hidden = $8
        WHERE id = $1 AND grid_id = $2
        RETURNING id, grid_id, name, property_type, property_config, width_px, position, is_primary, is_hidden, created_at, updated_at
        "#,
    )
    .bind(col_id)
    .bind(grid_id)
    .bind(&new_name)
    .bind(target_type)
    .bind(sqlx::types::Json(new_config))
    .bind(new_width)
    .bind(new_position)
    .bind(new_hidden)
    .fetch_one(&mut *tx)
    .await?;

    tx.commit().await?;

    state.cache.invalidate_grid(&grid_id);

    record_audit_log(
        &state.db,
        user.user_id,
        Some(access.space_id),
        Some(grid_id),
        "grid.column.update",
        json!({
            "column_id": col_id,
            "name": updated.name,
            "property_type": updated.property_type,
            "type_changed": type_changed
        }),
    )
    .await?;

    let _ = record_grid_history_snapshot(
        &state.db,
        grid_id,
        "column.update",
        Some(user.user_id),
        Some(&user.email),
        &format!("Updated column '{}' ({:?})", updated.name, updated.property_type),
    )
    .await;

    Ok(Json(updated))
}

/// `DELETE /api/grids/{grid_id}/columns/{col_id}`
/// Removes a column from `grid_columns` and strips its key from `grid_rows.cells` (`cells = cells - $1`).
pub async fn delete_column(
    State(state): State<AppState>,
    RequireSpaceAccess(user): RequireSpaceAccess,
    Path((grid_id, col_id)): Path<(Uuid, Uuid)>,
) -> Result<StatusCode, AuthError> {
    let access = resolve_grid_access_cached(&state, user.clone(), grid_id).await?;
    access.ensure_write()?;

    let mut tx = state.db.begin().await?;

    let deleted = sqlx::query("DELETE FROM grid_columns WHERE id = $1 AND grid_id = $2")
        .bind(col_id)
        .bind(grid_id)
        .execute(&mut *tx)
        .await?;

    if deleted.rows_affected() == 0 {
        return Err(AuthError::NotFound(format!(
            "Column {col_id} not found in grid {grid_id}"
        )));
    }

    // Strip the deleted column's UUID key from all rows in this grid
    sqlx::query("UPDATE grid_rows SET cells = cells - $1 WHERE grid_id = $2")
        .bind(col_id.to_string())
        .bind(grid_id)
        .execute(&mut *tx)
        .await?;

    tx.commit().await?;

    state.cache.invalidate_grid(&grid_id);

    record_audit_log(
        &state.db,
        user.user_id,
        Some(access.space_id),
        Some(grid_id),
        "grid.column.delete",
        json!({ "deleted_column_id": col_id }),
    )
    .await?;

    let _ = record_grid_history_snapshot(
        &state.db,
        grid_id,
        "column.delete",
        Some(user.user_id),
        Some(&user.email),
        &format!("Deleted column {col_id}"),
    )
    .await;

    Ok(StatusCode::NO_CONTENT)
}

fn cell_to_convertible_string(val: &CellValue, old_cfg: &ColumnPropertyConfig) -> String {
    match val {
        CellValue::Text(s) => s.clone(),
        CellValue::Number(n) => n.to_string(),
        CellValue::Select(Some(oid)) => {
            if let ColumnPropertyConfig::Select { options } = old_cfg {
                if let Some(opt) = options.iter().find(|o| &o.id == oid) {
                    return opt.label.clone();
                }
            }
            oid.to_string()
        }
        CellValue::Select(None) => String::new(),
        CellValue::MultiSelect(oids) => {
            if let ColumnPropertyConfig::MultiSelect { options } = old_cfg {
                let labels: Vec<String> = oids
                    .iter()
                    .filter_map(|id| options.iter().find(|o| &o.id == id).map(|o| o.label.clone()))
                    .collect();
                if !labels.is_empty() {
                    return labels.join(", ");
                }
            }
            String::new()
        }
        CellValue::Date(Some(d)) => d.format("%Y-%m-%d").to_string(),
        CellValue::Date(None) => String::new(),
        CellValue::Time {
            time_of_day: Some(t),
            ..
        } => t.format("%H:%M").to_string(),
        CellValue::Time { .. } => String::new(),
        CellValue::Files(files) => files
            .iter()
            .map(|f| f.storage_url.clone())
            .collect::<Vec<_>>()
            .join(", "),
        CellValue::Checkbox(b) => b.to_string(),
        CellValue::Checklist(items) => items
            .iter()
            .map(|i| {
                if i.completed {
                    format!("[x] {}", i.item)
                } else {
                    format!("[ ] {}", i.item)
                }
            })
            .collect::<Vec<_>>()
            .join("\n"),
        CellValue::LastModified(dt) => dt.format("%Y-%m-%d %H:%M:%S").to_string(),
        CellValue::Person(uids) => uids
            .iter()
            .map(|u| u.to_string())
            .collect::<Vec<_>>()
            .join(", "),
    }
}

fn convert_string_to_cell(
    raw: &str,
    target_type: ColumnPropertyType,
    target_cfg: &ColumnPropertyConfig,
    actor_id: Uuid,
    now: chrono::DateTime<Utc>,
) -> CellValue {
    let trimmed = raw.trim();
    match target_type {
        ColumnPropertyType::Text => CellValue::Text(trimmed.to_string()),
        ColumnPropertyType::Number => {
            let cleaned: String = trimmed
                .chars()
                .filter(|c| c.is_ascii_digit() || *c == '.' || *c == '-')
                .collect();
            let n = cleaned.parse::<f64>().ok().filter(|v| v.is_finite()).unwrap_or(0.0);
            CellValue::Number(n)
        }
        ColumnPropertyType::Select => {
            if let ColumnPropertyConfig::Select { options } = target_cfg {
                let first_token = trimmed.split(',').next().unwrap_or("").trim();
                let found = options
                    .iter()
                    .find(|o| o.label.eq_ignore_ascii_case(first_token))
                    .map(|o| o.id);
                CellValue::Select(found)
            } else {
                CellValue::Select(None)
            }
        }
        ColumnPropertyType::MultiSelect => {
            if let ColumnPropertyConfig::MultiSelect { options } = target_cfg {
                let ids = trimmed
                    .split(',')
                    .map(str::trim)
                    .filter(|t| !t.is_empty())
                    .filter_map(|tok| {
                        options
                            .iter()
                            .find(|o| o.label.eq_ignore_ascii_case(tok))
                            .map(|o| o.id)
                    })
                    .collect();
                CellValue::MultiSelect(ids)
            } else {
                CellValue::MultiSelect(Vec::new())
            }
        }
        ColumnPropertyType::Date => {
            let d = NaiveDate::parse_from_str(trimmed, "%Y-%m-%d")
                .or_else(|_| NaiveDate::parse_from_str(trimmed, "%d/%m/%Y"))
                .ok();
            CellValue::Date(d)
        }
        ColumnPropertyType::Time => {
            let t = NaiveTime::parse_from_str(trimmed, "%H:%M:%S")
                .or_else(|_| NaiveTime::parse_from_str(trimmed, "%H:%M"))
                .ok();
            CellValue::Time {
                time_of_day: t,
                duration_seconds: None,
            }
        }
        ColumnPropertyType::Checkbox => {
            let b = matches!(
                trimmed.to_lowercase().as_str(),
                "true" | "1" | "yes" | "[x]" | "checked"
            );
            CellValue::Checkbox(b)
        }
        ColumnPropertyType::Checklist => {
            let items = trimmed
                .lines()
                .flat_map(|l| l.split(','))
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(|part| {
                    if let Some(rest) = part.strip_prefix("[x]").or_else(|| part.strip_prefix("[X]")) {
                        ChecklistItem {
                            id: Uuid::new_v4(),
                            item: rest.trim().to_string(),
                            completed: true,
                        }
                    } else if let Some(rest) = part.strip_prefix("[ ]") {
                        ChecklistItem {
                            id: Uuid::new_v4(),
                            item: rest.trim().to_string(),
                            completed: false,
                        }
                    } else {
                        ChecklistItem {
                            id: Uuid::new_v4(),
                            item: part.to_string(),
                            completed: false,
                        }
                    }
                })
                .collect();
            CellValue::Checklist(items)
        }
        ColumnPropertyType::Files => {
            let files = trimmed
                .split(',')
                .map(str::trim)
                .filter(|u| !u.is_empty())
                .map(|url| FileAttachment {
                    id: Uuid::new_v4(),
                    filename: url.rsplit('/').next().unwrap_or("file").to_string(),
                    mime_type: "application/octet-stream".into(),
                    size_bytes: 0,
                    storage_url: url.to_string(),
                    uploaded_by: actor_id,
                    uploaded_at: now,
                })
                .collect();
            CellValue::Files(files)
        }
        ColumnPropertyType::Person => {
            let uids = trimmed
                .split(',')
                .filter_map(|s| Uuid::parse_str(s.trim()).ok())
                .collect();
            CellValue::Person(uids)
        }
        ColumnPropertyType::LastModified => CellValue::LastModified(now),
    }
}

// ============================================================================
// 3. High-Performance Row Operations, JSONB Sorting/Filtering & Bulk Delete
// ============================================================================

pub async fn list_rows(
    State(state): State<AppState>,
    RequireSpaceAccess(user): RequireSpaceAccess,
    Path(grid_id): Path<Uuid>,
    Query(query): Query<ListRowsQuery>,
) -> Result<Json<Vec<GridRow>>, AuthError> {
    let _access = resolve_grid_access_cached(&state, user, grid_id).await?;

    let columns: Vec<GridColumn> = if let Some(cached_cols) = state.cache.get_grid_columns(&grid_id) {
        cached_cols
    } else {
        sqlx::query_as(
            r#"
            SELECT id, grid_id, name, property_type, property_config, width_px, position, is_primary, is_hidden, created_at, updated_at
            FROM grid_columns
            WHERE grid_id = $1
            "#,
        )
        .bind(grid_id)
        .fetch_all(&state.db)
        .await?
    };

    let col_map: HashMap<Uuid, GridColumn> = columns.into_iter().map(|c| (c.id, c)).collect();

    let mut qb: QueryBuilder<Postgres> = QueryBuilder::new(
        "SELECT id, grid_id, position, row_order, cells, created_by, updated_by, created_at, last_modified FROM grid_rows WHERE grid_id = ",
    );
    qb.push_bind(grid_id);

    if let (Some(f_col_id), Some(f_op)) = (query.filter_col, query.filter_op) {
        if let Some(col_schema) = col_map.get(&f_col_id) {
            let col_key = f_col_id.to_string();
            match f_op {
                FilterOperator::IsEmpty => {
                    qb.push(" AND (NOT (cells ? ");
                    qb.push_bind(col_key.clone());
                    qb.push(") OR cells -> ");
                    qb.push_bind(col_key.clone());
                    qb.push(" ->> 'v' IS NULL OR cells -> ");
                    qb.push_bind(col_key);
                    qb.push(" ->> 'v' = '')");
                }
                FilterOperator::IsNotEmpty => {
                    qb.push(" AND (cells ? ");
                    qb.push_bind(col_key.clone());
                    qb.push(" AND cells -> ");
                    qb.push_bind(col_key.clone());
                    qb.push(" ->> 'v' IS NOT NULL AND cells -> ");
                    qb.push_bind(col_key);
                    qb.push(" ->> 'v' <> '')");
                }
                FilterOperator::Contains => {
                    let val = query.filter_val.unwrap_or_default();
                    qb.push(" AND (cells -> ");
                    qb.push_bind(col_key);
                    qb.push(" ->> 'v') ILIKE ");
                    qb.push_bind(format!("%{val}%"));
                }
                FilterOperator::Equals => {
                    let val = query.filter_val.unwrap_or_default();
                    qb.push(" AND (cells -> ");
                    qb.push_bind(col_key);
                    qb.push(" ->> 'v') = ");
                    qb.push_bind(val);
                }
                FilterOperator::NotEquals => {
                    let val = query.filter_val.unwrap_or_default();
                    qb.push(" AND (cells -> ");
                    qb.push_bind(col_key);
                    qb.push(" ->> 'v') IS DISTINCT FROM ");
                    qb.push_bind(val);
                }
                FilterOperator::GreaterThan | FilterOperator::LessThan => {
                    let op_sql = if f_op == FilterOperator::GreaterThan {
                        " > "
                    } else {
                        " < "
                    };
                    let raw_val = query.filter_val.unwrap_or_default();
                    match col_schema.property_type {
                        ColumnPropertyType::Number => {
                            if let Ok(num) = raw_val.parse::<f64>() {
                                qb.push(" AND ((cells -> ");
                                qb.push_bind(col_key);
                                qb.push(" ->> 'v')::double precision)");
                                qb.push(op_sql);
                                qb.push_bind(num);
                            }
                        }
                        ColumnPropertyType::Date => {
                            if let Ok(dt) = chrono::NaiveDate::parse_from_str(&raw_val, "%Y-%m-%d") {
                                qb.push(" AND ((cells -> ");
                                qb.push_bind(col_key);
                                qb.push(" ->> 'v')::date)");
                                qb.push(op_sql);
                                qb.push_bind(dt);
                            }
                        }
                        _ => {
                            qb.push(" AND (cells -> ");
                            qb.push_bind(col_key);
                            qb.push(" ->> 'v')");
                            qb.push(op_sql);
                            qb.push_bind(raw_val);
                        }
                    }
                }
            }
        }
    }

    let mut sort_specs: Vec<(Uuid, SortDirection)> = Vec::new();
    if let Some(col_id) = query.sort_by {
        sort_specs.push((col_id, query.order.unwrap_or(SortDirection::Asc)));
    }
    if let Some(multi) = &query.sorts {
        for part in multi.split(',') {
            let mut iter = part.trim().split(':');
            if let Some(id_str) = iter.next() {
                if let Ok(cid) = Uuid::parse_str(id_str) {
                    let dir = match iter.next().map(|s| s.to_lowercase()).as_deref() {
                        Some("desc") => SortDirection::Desc,
                        _ => SortDirection::Asc,
                    };
                    sort_specs.push((cid, dir));
                }
            }
        }
    }

    qb.push(" ORDER BY ");
    let mut pushed_sort = false;

    for (col_id, direction) in sort_specs {
        if let Some(col_schema) = col_map.get(&col_id) {
            if pushed_sort {
                qb.push(", ");
            }
            let col_key = col_id.to_string();
            match col_schema.property_type {
                ColumnPropertyType::Number => {
                    qb.push("((cells -> ");
                    qb.push_bind(col_key);
                    qb.push(" ->> 'v')::double precision)");
                }
                ColumnPropertyType::Date => {
                    qb.push("((cells -> ");
                    qb.push_bind(col_key);
                    qb.push(" ->> 'v')::date)");
                }
                ColumnPropertyType::LastModified => {
                    qb.push("last_modified");
                }
                ColumnPropertyType::Checkbox => {
                    qb.push("((cells -> ");
                    qb.push_bind(col_key);
                    qb.push(" ->> 'v')::boolean)");
                }
                _ => {
                    qb.push("LOWER(cells -> ");
                    qb.push_bind(col_key);
                    qb.push(" ->> 'v')");
                }
            }

            match direction {
                SortDirection::Asc => qb.push(" ASC NULLS LAST"),
                SortDirection::Desc => qb.push(" DESC NULLS LAST"),
            };
            pushed_sort = true;
        }
    }

    if pushed_sort {
        qb.push(", position ASC, row_order ASC, created_at ASC, id ASC");
    } else {
        qb.push("position ASC, row_order ASC, created_at ASC, id ASC");
    }

    let limit = query.limit.unwrap_or(200).clamp(1, 2000);
    let offset = query.offset.unwrap_or(0).max(0);
    qb.push(" LIMIT ");
    qb.push_bind(limit);
    qb.push(" OFFSET ");
    qb.push_bind(offset);

    let rows: Vec<GridRow> = qb.build_query_as().fetch_all(&state.db).await?;
    Ok(Json(rows))
}

async fn validate_and_stamp_cells(
    state: &AppState,
    grid_id: Uuid,
    cells: &mut RowCellMap,
) -> Result<(), AuthError> {
    let columns: Vec<GridColumn> = if let Some(cached_cols) = state.cache.get_grid_columns(&grid_id) {
        cached_cols
    } else {
        sqlx::query_as(
            r#"
            SELECT id, grid_id, name, property_type, property_config, width_px, position, is_primary, is_hidden, created_at, updated_at
            FROM grid_columns
            WHERE grid_id = $1
            "#,
        )
        .bind(grid_id)
        .fetch_all(&state.db)
        .await?
    };

    let col_map: HashMap<Uuid, GridColumn> = columns.into_iter().map(|c| (c.id, c)).collect();

    for (col_id, cell_value) in cells.iter() {
        let col_schema = col_map.get(col_id).ok_or_else(|| {
            AuthError::BadRequest(format!("Column {col_id} does not belong to grid {grid_id}"))
        })?;

        cell_value
            .validate(col_schema)
            .map_err(|e| AuthError::BadRequest(e.to_string()))?;
    }

    let now = Utc::now();
    for col in col_map.values() {
        if col.property_type == ColumnPropertyType::LastModified {
            cells.insert(col.id, CellValue::LastModified(now));
        }
    }

    Ok(())
}

pub async fn create_row(
    State(state): State<AppState>,
    RequireSpaceAccess(user): RequireSpaceAccess,
    Path(grid_id): Path<Uuid>,
    Json(mut payload): Json<CreateRowRequest>,
) -> Result<(StatusCode, Json<GridRow>), AuthError> {
    let access = resolve_grid_access_cached(&state, user.clone(), grid_id).await?;
    access.ensure_write()?;

    validate_and_stamp_cells(&state, grid_id, &mut payload.cells).await?;

    let mut tx = state.db.begin().await?;

    // Resolve optional positional insertion (`target_row_id` + `placement` or `insert_at_position`)
    let mut resolved_insert_pos: Option<i32> = payload.insert_at_position.map(|p| p.max(0));
    if let Some(target_row_id) = payload.target_row_id {
        let maybe_target_pos: Option<i32> = sqlx::query_scalar(
            "SELECT position FROM grid_rows WHERE id = $1 AND grid_id = $2",
        )
        .bind(target_row_id)
        .bind(grid_id)
        .fetch_optional(&mut *tx)
        .await?;

        if let Some(t_pos) = maybe_target_pos {
            let is_above = payload
                .placement
                .as_deref()
                .map(|s| s.eq_ignore_ascii_case("above"))
                .unwrap_or(false);
            resolved_insert_pos = Some(if is_above {
                t_pos.max(0)
            } else {
                (t_pos + 1).max(0)
            });
        }
    }

    let (target_pos, row_order) = if let Some(insert_pos) = resolved_insert_pos {
        // Shift subsequent rows down by 1 inside the transaction
        sqlx::query(
            r#"
            UPDATE grid_rows
            SET position = position + 1,
                row_order = (position + 1)::double precision * 1000.0
            WHERE grid_id = $1 AND position >= $2
            "#,
        )
        .bind(grid_id)
        .bind(insert_pos)
        .execute(&mut *tx)
        .await?;

        let ord = payload.row_order.unwrap_or((insert_pos as f64) * 1000.0);
        (insert_pos, ord)
    } else {
        let (next_pos, max_ord): (i32, f64) = sqlx::query_as(
            "SELECT COALESCE(MAX(position) + 1, 0), COALESCE(MAX(row_order) + 1000.0, 1000.0) FROM grid_rows WHERE grid_id = $1",
        )
        .bind(grid_id)
        .fetch_one(&mut *tx)
        .await?;

        let ord = payload.row_order.unwrap_or(max_ord);
        (next_pos, ord)
    };

    let row: GridRow = sqlx::query_as(
        r#"
        INSERT INTO grid_rows (grid_id, position, row_order, cells, created_by, updated_by)
        VALUES ($1, $2, $3, $4, $5, $5)
        RETURNING id, grid_id, position, row_order, cells, created_by, updated_by, created_at, last_modified
        "#,
    )
    .bind(grid_id)
    .bind(target_pos)
    .bind(row_order)
    .bind(sqlx::types::Json(&payload.cells))
    .bind(user.user_id)
    .fetch_one(&mut *tx)
    .await?;

    tx.commit().await?;

    let summary_msg = if let Some(p) = resolved_insert_pos {
        format!("Inserted new row at position {}", p + 1)
    } else {
        "Created new row".to_string()
    };

    let _ = record_grid_history_snapshot(
        &state.db,
        grid_id,
        "row.create",
        Some(user.user_id),
        Some(&user.email),
        &summary_msg,
    )
    .await;

    Ok((StatusCode::CREATED, Json(row)))
}

pub async fn update_row(
    State(state): State<AppState>,
    RequireSpaceAccess(user): RequireSpaceAccess,
    Path((grid_id, row_id)): Path<(Uuid, Uuid)>,
    Json(mut payload): Json<PatchRowRequest>,
) -> Result<Json<GridRow>, AuthError> {
    let access = resolve_grid_access_cached(&state, user.clone(), grid_id).await?;
    access.ensure_write()?;

    validate_and_stamp_cells(&state, grid_id, &mut payload.cells).await?;

    let updated: GridRow = sqlx::query_as(
        r#"
        UPDATE grid_rows
        SET cells = cells || $3::jsonb,
            row_order = COALESCE($4, row_order),
            updated_by = $5,
            last_modified = NOW()
        WHERE id = $1 AND grid_id = $2
        RETURNING id, grid_id, position, row_order, cells, created_by, updated_by, created_at, last_modified
        "#,
    )
    .bind(row_id)
    .bind(grid_id)
    .bind(sqlx::types::Json(&payload.cells))
    .bind(payload.row_order)
    .bind(user.user_id)
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(|| AuthError::NotFound(format!("Row {row_id} not found in grid {grid_id}")))?;

    let _ = record_grid_history_snapshot(
        &state.db,
        grid_id,
        "row.update",
        Some(user.user_id),
        Some(&user.email),
        &format!("Edited row {row_id}"),
    )
    .await;

    Ok(Json(updated))
}

/// `PATCH /api/grids/{grid_id}/rows/bulk-cells`
/// Batch-updates cell values across multiple rows in a single PostgreSQL transaction
/// (used by Excel Fill-Handle drag-to-copy).
pub async fn bulk_update_row_cells(
    State(state): State<AppState>,
    RequireSpaceAccess(user): RequireSpaceAccess,
    Path(grid_id): Path<Uuid>,
    Json(mut payload): Json<BulkPatchCellsRequest>,
) -> Result<Json<Vec<GridRow>>, AuthError> {
    let access = resolve_grid_access_cached(&state, user.clone(), grid_id).await?;
    access.ensure_write()?;

    if payload.updates.is_empty() {
        return Ok(Json(Vec::new()));
    }

    for item in &mut payload.updates {
        validate_and_stamp_cells(&state, grid_id, &mut item.cells).await?;
    }

    let mut tx = state.db.begin().await?;
    let mut updated_rows = Vec::with_capacity(payload.updates.len());

    for item in payload.updates {
        let updated: Option<GridRow> = sqlx::query_as(
            r#"
            UPDATE grid_rows
            SET cells = cells || $3::jsonb,
                updated_by = $4,
                last_modified = NOW()
            WHERE id = $1 AND grid_id = $2
            RETURNING id, grid_id, position, row_order, cells, created_by, updated_by, created_at, last_modified
            "#,
        )
        .bind(item.row_id)
        .bind(grid_id)
        .bind(sqlx::types::Json(&item.cells))
        .bind(user.user_id)
        .fetch_optional(&mut *tx)
        .await?;

        if let Some(r) = updated {
            updated_rows.push(r);
        }
    }

    tx.commit().await?;

    record_audit_log(
        &state.db,
        user.user_id,
        Some(access.space_id),
        Some(grid_id),
        "grid.rows.bulk_fill_cells",
        json!({ "updated_count": updated_rows.len() }),
    )
    .await?;

    let _ = record_grid_history_snapshot(
        &state.db,
        grid_id,
        "rows.bulk_fill",
        Some(user.user_id),
        Some(&user.email),
        &format!("Bulk-filled cells across {} rows", updated_rows.len()),
    )
    .await;

    Ok(Json(updated_rows))
}

pub async fn bulk_delete_rows(
    State(state): State<AppState>,
    RequireSpaceAccess(user): RequireSpaceAccess,
    Path(grid_id): Path<Uuid>,
    Json(payload): Json<BulkDeleteRowsRequest>,
) -> Result<Json<BulkDeleteRowsResponse>, AuthError> {
    let access = resolve_grid_access_cached(&state, user.clone(), grid_id).await?;
    access.ensure_write()?;

    if payload.row_ids.is_empty() {
        return Ok(Json(BulkDeleteRowsResponse { deleted_count: 0 }));
    }

    let result = sqlx::query(
        "DELETE FROM grid_rows WHERE grid_id = $1 AND id = ANY($2)",
    )
    .bind(grid_id)
    .bind(&payload.row_ids)
    .execute(&state.db)
    .await?;

    let deleted_count = result.rows_affected();

    record_audit_log(
        &state.db,
        user.user_id,
        Some(access.space_id),
        Some(grid_id),
        "grid.rows.bulk_delete",
        json!({ "deleted_count": deleted_count, "row_ids": payload.row_ids }),
    )
    .await?;

    let _ = record_grid_history_snapshot(
        &state.db,
        grid_id,
        "rows.bulk_delete",
        Some(user.user_id),
        Some(&user.email),
        &format!("Deleted {deleted_count} row(s)"),
    )
    .await;

    Ok(Json(BulkDeleteRowsResponse { deleted_count }))
}

/// `PATCH /api/grids/{grid_id}/rows/reorder`
/// Atomically updates `position` and `row_order` for all rows in a Grid after drag-and-drop row rearranging.
pub async fn reorder_rows(
    State(state): State<AppState>,
    RequireSpaceAccess(user): RequireSpaceAccess,
    Path(grid_id): Path<Uuid>,
    Json(payload): Json<ReorderRowsRequest>,
) -> Result<Json<Vec<GridRow>>, AuthError> {
    let access = resolve_grid_access_cached(&state, user.clone(), grid_id).await?;
    access.ensure_write()?;

    let mut tx = state.db.begin().await?;

    for (idx, row_id) in payload.row_ids.iter().enumerate() {
        let pos = idx as i32;
        let ord = ((idx + 1) as f64) * 1000.0;
        sqlx::query(
            "UPDATE grid_rows SET position = $3, row_order = $4 WHERE id = $1 AND grid_id = $2",
        )
        .bind(row_id)
        .bind(grid_id)
        .bind(pos)
        .bind(ord)
        .execute(&mut *tx)
        .await?;
    }

    let rows: Vec<GridRow> = sqlx::query_as(
        r#"
        SELECT id, grid_id, position, row_order, cells, created_by, updated_by, created_at, last_modified
        FROM grid_rows
        WHERE grid_id = $1
        ORDER BY position ASC, row_order ASC, created_at ASC
        "#,
    )
    .bind(grid_id)
    .fetch_all(&mut *tx)
    .await?;

    tx.commit().await?;

    record_audit_log(
        &state.db,
        user.user_id,
        Some(access.space_id),
        Some(grid_id),
        "grid.rows.reorder",
        json!({ "row_count": payload.row_ids.len() }),
    )
    .await?;

    Ok(Json(rows))
}

/// `POST /api/grids/{grid_id}/rows/restore`
/// Restores previously deleted `GridRow`s (used by the `Ctrl+Z` Multi-Step Undo Engine).
pub async fn restore_rows(
    State(state): State<AppState>,
    RequireSpaceAccess(user): RequireSpaceAccess,
    Path(grid_id): Path<Uuid>,
    Json(payload): Json<RestoreRowsRequest>,
) -> Result<Json<Vec<GridRow>>, AuthError> {
    let access = resolve_grid_access_cached(&state, user.clone(), grid_id).await?;
    access.ensure_write()?;

    let mut tx = state.db.begin().await?;

    for row in &payload.rows {
        sqlx::query(
            r#"
            INSERT INTO grid_rows (id, grid_id, position, row_order, cells, created_by, updated_by, created_at, last_modified)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
            ON CONFLICT (id) DO UPDATE SET
                position = EXCLUDED.position,
                row_order = EXCLUDED.row_order,
                cells = EXCLUDED.cells,
                updated_by = EXCLUDED.updated_by
            "#,
        )
        .bind(row.id)
        .bind(grid_id)
        .bind(row.position)
        .bind(row.row_order)
        .bind(sqlx::types::Json(&row.cells))
        .bind(row.created_by)
        .bind(user.user_id)
        .bind(row.created_at)
        .bind(row.last_modified)
        .execute(&mut *tx)
        .await?;
    }

    let rows: Vec<GridRow> = sqlx::query_as(
        r#"
        SELECT id, grid_id, position, row_order, cells, created_by, updated_by, created_at, last_modified
        FROM grid_rows
        WHERE grid_id = $1
        ORDER BY position ASC, row_order ASC, created_at ASC
        "#,
    )
    .bind(grid_id)
    .fetch_all(&mut *tx)
    .await?;

    tx.commit().await?;

    let _ = record_grid_history_snapshot(
        &state.db,
        grid_id,
        "rows.restore_undo",
        Some(user.user_id),
        Some(&user.email),
        &format!("Restored {} deleted row(s) via Undo", payload.rows.len()),
    )
    .await;

    Ok(Json(rows))
}

/// `POST /api/grids/{grid_id}/reset-layout`
/// Resets all column widths in a Grid back to their default `180px` and returns the updated columns.
pub async fn reset_grid_layout(
    State(state): State<AppState>,
    RequireSpaceAccess(user): RequireSpaceAccess,
    Path(grid_id): Path<Uuid>,
) -> Result<Json<Vec<GridColumn>>, AuthError> {
    let access = resolve_grid_access_cached(&state, user.clone(), grid_id).await?;
    access.ensure_write()?;

    let columns: Vec<GridColumn> = sqlx::query_as(
        r#"
        UPDATE grid_columns
        SET width_px = 180
        WHERE grid_id = $1
        RETURNING id, grid_id, name, property_type, property_config, width_px, position, is_primary, is_hidden, created_at, updated_at
        "#,
    )
    .bind(grid_id)
    .fetch_all(&state.db)
    .await?;

    let mut sorted = columns;
    sorted.sort_by_key(|c| (c.position, c.created_at));

    state.cache.invalidate_grid(&grid_id);

    record_audit_log(
        &state.db,
        user.user_id,
        Some(access.space_id),
        Some(grid_id),
        "grid.reset_layout",
        json!({ "default_width_px": 180 }),
    )
    .await?;

    Ok(Json(sorted))
}

/// Asynchronously records a point-in-time snapshot into `grid_history_snapshots` and purges snapshots
/// older than 30 days in a non-blocking `tokio::spawn` background task so interactive cell/column edits
/// return to the client immediately.
pub async fn record_grid_history_snapshot(
    db: &sqlx::PgPool,
    grid_id: Uuid,
    action_type: &str,
    actor_user_id: Option<Uuid>,
    actor_email: Option<&str>,
    summary: &str,
) -> Result<(), AuthError> {
    let db_clone = db.clone();
    let action_owned = action_type.to_string();
    let email_owned = actor_email.map(|s| s.to_string());
    let summary_owned = summary.to_string();

    tokio::spawn(async move {
        let Ok(columns) = sqlx::query_as::<_, GridColumn>(
            r#"
            SELECT id, grid_id, name, property_type, property_config, width_px, position, is_primary, is_hidden, created_at, updated_at
            FROM grid_columns
            WHERE grid_id = $1
            ORDER BY position ASC, created_at ASC
            "#,
        )
        .bind(grid_id)
        .fetch_all(&db_clone)
        .await
        else {
            return;
        };

        let Ok(rows) = sqlx::query_as::<_, GridRow>(
            r#"
            SELECT id, grid_id, position, row_order, cells, created_by, updated_by, created_at, last_modified
            FROM grid_rows
            WHERE grid_id = $1
            ORDER BY position ASC, row_order ASC, created_at ASC
            "#,
        )
        .bind(grid_id)
        .fetch_all(&db_clone)
        .await
        else {
            return;
        };

        let snapshot = GridSnapshotData {
            columns,
            rows,
            summary: summary_owned,
        };

        let _ = sqlx::query(
            r#"
            INSERT INTO grid_history_snapshots (grid_id, action_type, actor_user_id, actor_email, snapshot_data)
            VALUES ($1, $2, $3, $4, $5)
            "#,
        )
        .bind(grid_id)
        .bind(&action_owned)
        .bind(actor_user_id)
        .bind(email_owned.as_deref())
        .bind(sqlx::types::Json(&snapshot))
        .execute(&db_clone)
        .await;

        let _ = sqlx::query(
            "DELETE FROM grid_history_snapshots WHERE created_at < NOW() - INTERVAL '30 days'",
        )
        .execute(&db_clone)
        .await;
    });

    Ok(())
}

/// `GET /api/grids/{grid_id}/history`
/// Returns the last 30 days of `GridHistoryEntry` snapshots for a Grid.
pub async fn list_grid_history(
    State(state): State<AppState>,
    RequireSpaceAccess(user): RequireSpaceAccess,
    Path(grid_id): Path<Uuid>,
) -> Result<Json<Vec<GridHistoryEntry>>, AuthError> {
    let _access = resolve_grid_access_cached(&state, user, grid_id).await?;

    // Purge expired snapshots older than 30 days
    let _ = sqlx::query(
        "DELETE FROM grid_history_snapshots WHERE created_at < NOW() - INTERVAL '30 days'",
    )
    .execute(&state.db)
    .await;

    let entries: Vec<GridHistoryEntry> = sqlx::query_as(
        r#"
        SELECT
            id,
            grid_id,
            action_type,
            actor_user_id,
            actor_email,
            COALESCE(snapshot_data ->> 'summary', action_type) AS summary,
            COALESCE(jsonb_array_length(snapshot_data -> 'columns'), 0)::int AS column_count,
            COALESCE(jsonb_array_length(snapshot_data -> 'rows'), 0)::int AS row_count,
            created_at
        FROM grid_history_snapshots
        WHERE grid_id = $1
          AND created_at >= NOW() - INTERVAL '30 days'
        ORDER BY created_at DESC
        LIMIT 100
        "#,
    )
    .bind(grid_id)
    .fetch_all(&state.db)
    .await?;

    Ok(Json(entries))
}

/// `GET /api/grids/{grid_id}/history/{history_id}`
/// Returns the full `GridHistoryDetail` including `snapshot_data` (`columns` and `rows`) for the Full-Page History Preview Modal.
pub async fn get_grid_history_detail(
    State(state): State<AppState>,
    RequireSpaceAccess(user): RequireSpaceAccess,
    Path((grid_id, history_id)): Path<(Uuid, Uuid)>,
) -> Result<Json<GridHistoryDetail>, AuthError> {
    let _access = resolve_grid_access_cached(&state, user, grid_id).await?;

    let row_opt: Option<(
        Uuid,
        Uuid,
        String,
        Option<Uuid>,
        Option<String>,
        sqlx::types::Json<GridSnapshotData>,
        chrono::DateTime<Utc>,
    )> = sqlx::query_as(
        r#"
        SELECT id, grid_id, action_type, actor_user_id, actor_email, snapshot_data, created_at
        FROM grid_history_snapshots
        WHERE id = $1 AND grid_id = $2
        "#,
    )
    .bind(history_id)
    .bind(grid_id)
    .fetch_optional(&state.db)
    .await?;

    let (id, grid_id, action_type, actor_user_id, actor_email, sqlx::types::Json(snapshot_data), created_at) =
        row_opt.ok_or_else(|| {
            AuthError::NotFound(format!(
                "History snapshot {history_id} not found for grid {grid_id}"
            ))
        })?;

    let summary = if snapshot_data.summary.trim().is_empty() {
        action_type.clone()
    } else {
        snapshot_data.summary.clone()
    };

    Ok(Json(GridHistoryDetail {
        id,
        grid_id,
        action_type,
        actor_user_id,
        actor_email,
        summary,
        created_at,
        snapshot_data,
    }))
}

/// `POST /api/grids/{grid_id}/history/{history_id}/restore`
/// Restores a Grid's columns and rows to an exact historical point-in-time snapshot.
pub async fn restore_grid_history(
    State(state): State<AppState>,
    RequireSpaceAccess(user): RequireSpaceAccess,
    Path((grid_id, history_id)): Path<(Uuid, Uuid)>,
) -> Result<Json<RestoreGridHistoryResponse>, AuthError> {
    let access = resolve_grid_access_cached(&state, user.clone(), grid_id).await?;
    access.ensure_write()?;

    let row_opt: Option<(sqlx::types::Json<GridSnapshotData>,)> = sqlx::query_as(
        "SELECT snapshot_data FROM grid_history_snapshots WHERE id = $1 AND grid_id = $2",
    )
    .bind(history_id)
    .bind(grid_id)
    .fetch_optional(&state.db)
    .await?;

    let (sqlx::types::Json(snapshot),) = row_opt.ok_or_else(|| {
        AuthError::NotFound(format!("History snapshot {history_id} not found for grid {grid_id}"))
    })?;

    let mut tx = state.db.begin().await?;

    let grid: PageGrid = sqlx::query_as(
        r#"
        SELECT id, space_id, title, description, icon, position, created_by, is_archived, created_at, updated_at
        FROM pages_grids
        WHERE id = $1 AND is_archived = FALSE
        "#,
    )
    .bind(grid_id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(|| AuthError::NotFound(format!("Grid {grid_id} not found")))?;

    sqlx::query("DELETE FROM grid_rows WHERE grid_id = $1")
        .bind(grid_id)
        .execute(&mut *tx)
        .await?;

    sqlx::query("DELETE FROM grid_columns WHERE grid_id = $1")
        .bind(grid_id)
        .execute(&mut *tx)
        .await?;

    let mut restored_columns = Vec::with_capacity(snapshot.columns.len());
    for col in &snapshot.columns {
        let inserted_col: GridColumn = sqlx::query_as(
            r#"
            INSERT INTO grid_columns (
                id, grid_id, name, property_type, property_config, width_px, position, is_primary, is_hidden, created_at, updated_at
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, NOW())
            RETURNING id, grid_id, name, property_type, property_config, width_px, position, is_primary, is_hidden, created_at, updated_at
            "#,
        )
        .bind(col.id)
        .bind(grid_id)
        .bind(&col.name)
        .bind(col.property_type)
        .bind(sqlx::types::Json(&col.property_config))
        .bind(col.width_px.clamp(80, 800))
        .bind(col.position)
        .bind(col.is_primary)
        .bind(col.is_hidden)
        .bind(col.created_at)
        .fetch_one(&mut *tx)
        .await?;
        restored_columns.push(inserted_col);
    }

    let mut restored_rows = Vec::with_capacity(snapshot.rows.len());
    for r in &snapshot.rows {
        let inserted_row: GridRow = sqlx::query_as(
            r#"
            INSERT INTO grid_rows (
                id, grid_id, position, row_order, cells, created_by, updated_by, created_at, last_modified
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
            RETURNING id, grid_id, position, row_order, cells, created_by, updated_by, created_at, last_modified
            "#,
        )
        .bind(r.id)
        .bind(grid_id)
        .bind(r.position)
        .bind(r.row_order)
        .bind(sqlx::types::Json(&r.cells))
        .bind(r.created_by)
        .bind(user.user_id)
        .bind(r.created_at)
        .bind(r.last_modified)
        .fetch_one(&mut *tx)
        .await?;
        restored_rows.push(inserted_row);
    }

    tx.commit().await?;

    state.cache.invalidate_grid(&grid_id);

    record_audit_log(
        &state.db,
        user.user_id,
        Some(access.space_id),
        Some(grid_id),
        "grid.history.restore",
        json!({ "history_id": history_id, "columns": restored_columns.len(), "rows": restored_rows.len() }),
    )
    .await?;

    let _ = record_grid_history_snapshot(
        &state.db,
        grid_id,
        "history.restore",
        Some(user.user_id),
        Some(&user.email),
        &format!("Restored version snapshot {history_id}"),
    )
    .await;

    Ok(Json(RestoreGridHistoryResponse {
        grid,
        columns: restored_columns,
        rows: restored_rows,
    }))
}

pub async fn upload_cell_file(
    State(state): State<AppState>,
    RequireSpaceAccess(user): RequireSpaceAccess,
    Path((grid_id, row_id)): Path<(Uuid, Uuid)>,
    mut multipart: Multipart,
) -> Result<Json<GridRow>, AuthError> {
    let access = resolve_grid_access_cached(&state, user.clone(), grid_id).await?;
    access.ensure_write()?;

    let target_dir = state
        .uploads_dir
        .join(access.space_id.to_string())
        .join(grid_id.to_string());

    fs::create_dir_all(&target_dir)
        .await
        .map_err(|e| AuthError::InternalError(format!("Failed to create upload directory: {e}")))?;

    let mut target_column_id: Option<Uuid> = None;
    let mut uploaded_attachments: Vec<FileAttachment> = Vec::new();

    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|e| AuthError::BadRequest(format!("Invalid multipart stream: {e}")))?
    {
        let field_name = field.name().unwrap_or_default().to_string();

        if field_name == "column_id" {
            let text = field
                .text()
                .await
                .map_err(|e| AuthError::BadRequest(format!("Invalid column_id field: {e}")))?;
            let parsed = Uuid::parse_str(text.trim())
                .map_err(|_| AuthError::BadRequest("column_id must be a valid UUID".into()))?;
            target_column_id = Some(parsed);
        } else if field_name == "file" {
            let orig_filename = field
                .file_name()
                .map(sanitize_filename)
                .unwrap_or_else(|| "attachment.bin".into());
            let mime_type = field
                .content_type()
                .unwrap_or("application/octet-stream")
                .to_string();

            let bytes = field
                .bytes()
                .await
                .map_err(|e| AuthError::BadRequest(format!("Failed to read file bytes: {e}")))?;

            let attachment_id = Uuid::new_v4();
            let stored_filename = format!("{attachment_id}_{orig_filename}");
            let disk_path = target_dir.join(&stored_filename);

            let mut file = fs::File::create(&disk_path).await.map_err(|e| {
                AuthError::InternalError(format!("Failed to create file on volume: {e}"))
            })?;
            file.write_all(&bytes).await.map_err(|e| {
                AuthError::InternalError(format!("Failed to write file bytes: {e}"))
            })?;

            let download_url = format!(
                "/uploads/{}/{}/{}",
                access.space_id, grid_id, stored_filename
            );

            uploaded_attachments.push(FileAttachment {
                id: attachment_id,
                filename: orig_filename,
                mime_type,
                size_bytes: bytes.len() as u64,
                storage_url: download_url,
                uploaded_by: user.user_id,
                uploaded_at: Utc::now(),
            });
        }
    }

    let col_id = target_column_id.ok_or_else(|| {
        AuthError::BadRequest("Multipart request must include a 'column_id' field".into())
    })?;

    if uploaded_attachments.is_empty() {
        return Err(AuthError::BadRequest(
            "Multipart request did not contain any 'file' parts".into(),
        ));
    }

    let col: GridColumn = sqlx::query_as(
        r#"
        SELECT id, grid_id, name, property_type, property_config, width_px, position, is_primary, is_hidden, created_at, updated_at
        FROM grid_columns
        WHERE id = $1 AND grid_id = $2
        "#,
    )
    .bind(col_id)
    .bind(grid_id)
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(|| AuthError::NotFound(format!("Column {col_id} not found in grid {grid_id}")))?;

    if col.property_type != ColumnPropertyType::Files {
        return Err(AuthError::BadRequest(format!(
            "Column '{}' is of type {:?}, expected Files",
            col.name, col.property_type
        )));
    }

    let mut row: GridRow = sqlx::query_as(
        r#"
        SELECT id, grid_id, position, row_order, cells, created_by, updated_by, created_at, last_modified
        FROM grid_rows
        WHERE id = $1 AND grid_id = $2
        "#,
    )
    .bind(row_id)
    .bind(grid_id)
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(|| AuthError::NotFound(format!("Row {row_id} not found in grid {grid_id}")))?;

    let mut current_files = match row.cells.remove(&col_id) {
        Some(CellValue::Files(existing)) => existing,
        _ => Vec::new(),
    };
    current_files.extend(uploaded_attachments);

    let new_cell = CellValue::Files(current_files);
    new_cell
        .validate(&col)
        .map_err(|e| AuthError::BadRequest(e.to_string()))?;

    row.cells.insert(col_id, new_cell);

    let updated_row: GridRow = sqlx::query_as(
        r#"
        UPDATE grid_rows
        SET cells = $3,
            updated_by = $4,
            last_modified = NOW()
        WHERE id = $1 AND grid_id = $2
        RETURNING id, grid_id, position, row_order, cells, created_by, updated_by, created_at, last_modified
        "#,
    )
    .bind(row_id)
    .bind(grid_id)
    .bind(sqlx::types::Json(&row.cells))
    .bind(user.user_id)
    .fetch_one(&state.db)
    .await?;

    let _ = record_grid_history_snapshot(
        &state.db,
        grid_id,
        "row.upload_file",
        Some(user.user_id),
        Some(&user.email),
        &format!("Uploaded file attachment to row {row_id}"),
    )
    .await;

    Ok(Json(updated_row))
}

fn sanitize_filename(raw: &str) -> String {
    raw.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}
