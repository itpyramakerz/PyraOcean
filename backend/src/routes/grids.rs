use crate::auth::{
    record_audit_log, resolve_grid_access, resolve_space_access, AppState, AuthError,
    RequireSpaceAccess,
};
use axum::{
    extract::{Multipart, Path, Query, State},
    http::StatusCode,
    Json,
};
use chrono::Utc;
use pyra_ocean_shared::{
    BulkDeleteRowsRequest, BulkDeleteRowsResponse, CellValue, ColumnPropertyConfig,
    ColumnPropertyType, CreateColumnRequest, CreateGridRequest, CreateRowRequest, FileAttachment,
    FilterOperator, GridColumn, GridRow, GridSchemaResponse, PageGrid, PatchRowRequest, RowCellMap,
    SortDirection, UpdateColumnRequest,
};
use serde::Deserialize;
use serde_json::json;
use sqlx::{Postgres, QueryBuilder};
use std::collections::HashMap;
use tokio::{fs, io::AsyncWriteExt};
use uuid::Uuid;

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
    let _access = resolve_space_access(&state.db, user, space_id).await?;

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
    let access = resolve_space_access(&state.db, user.clone(), space_id).await?;
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

    record_audit_log(
        &state.db,
        user.user_id,
        Some(space_id),
        Some(grid.id),
        "grid.create",
        json!({ "title": grid.title }),
    )
    .await?;

    Ok((
        StatusCode::CREATED,
        Json(GridSchemaResponse {
            grid,
            columns: vec![col_primary, col_last_mod],
        }),
    ))
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
/// Returns the `PageGrid` metadata and ordered `Vec<GridColumn>` schema.
pub async fn get_grid_schema(
    State(state): State<AppState>,
    RequireSpaceAccess(user): RequireSpaceAccess,
    Path(grid_id): Path<Uuid>,
) -> Result<Json<GridSchemaResponse>, AuthError> {
    let _access = resolve_grid_access(&state.db, user, grid_id).await?;

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

    Ok(Json(GridSchemaResponse { grid, columns }))
}

/// `POST /api/grids/{grid_id}/columns`
pub async fn create_column(
    State(state): State<AppState>,
    RequireSpaceAccess(user): RequireSpaceAccess,
    Path(grid_id): Path<Uuid>,
    Json(payload): Json<CreateColumnRequest>,
) -> Result<(StatusCode, Json<GridColumn>), AuthError> {
    let access = resolve_grid_access(&state.db, user.clone(), grid_id).await?;
    access.ensure_write()?;

    if payload.name.trim().is_empty() {
        return Err(AuthError::BadRequest("Column name cannot be empty".into()));
    }

    let config = payload
        .property_config
        .unwrap_or_else(|| default_config_for_type(payload.property_type));

    let width_px = payload.width_px.unwrap_or(180).clamp(60, 1200);

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

    record_audit_log(
        &state.db,
        user.user_id,
        Some(access.space_id),
        Some(grid_id),
        "grid.column.create",
        json!({ "column_id": column.id, "name": column.name, "type": column.property_type }),
    )
    .await?;

    Ok((StatusCode::CREATED, Json(column)))
}

/// `PATCH /api/grids/{grid_id}/columns/{col_id}`
pub async fn update_column(
    State(state): State<AppState>,
    RequireSpaceAccess(user): RequireSpaceAccess,
    Path((grid_id, col_id)): Path<(Uuid, Uuid)>,
    Json(payload): Json<UpdateColumnRequest>,
) -> Result<Json<GridColumn>, AuthError> {
    let access = resolve_grid_access(&state.db, user.clone(), grid_id).await?;
    access.ensure_write()?;

    let existing: GridColumn = sqlx::query_as(
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

    let new_name = payload
        .name
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(&existing.name)
        .to_string();
    let new_config = payload.property_config.unwrap_or(existing.property_config);
    let new_width = payload
        .width_px
        .unwrap_or(existing.width_px)
        .clamp(60, 1200);
    let new_position = payload.position.unwrap_or(existing.position);
    let new_hidden = payload.is_hidden.unwrap_or(existing.is_hidden);

    let updated: GridColumn = sqlx::query_as(
        r#"
        UPDATE grid_columns
        SET name = $3,
            property_config = $4,
            width_px = $5,
            position = $6,
            is_hidden = $7
        WHERE id = $1 AND grid_id = $2
        RETURNING id, grid_id, name, property_type, property_config, width_px, position, is_primary, is_hidden, created_at, updated_at
        "#,
    )
    .bind(col_id)
    .bind(grid_id)
    .bind(&new_name)
    .bind(sqlx::types::Json(new_config))
    .bind(new_width)
    .bind(new_position)
    .bind(new_hidden)
    .fetch_one(&state.db)
    .await?;

    Ok(Json(updated))
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
    let _access = resolve_grid_access(&state.db, user, grid_id).await?;

    let columns: Vec<GridColumn> = sqlx::query_as(
        r#"
        SELECT id, grid_id, name, property_type, property_config, width_px, position, is_primary, is_hidden, created_at, updated_at
        FROM grid_columns
        WHERE grid_id = $1
        "#,
    )
    .bind(grid_id)
    .fetch_all(&state.db)
    .await?;

    let col_map: HashMap<Uuid, GridColumn> = columns.into_iter().map(|c| (c.id, c)).collect();

    let mut qb: QueryBuilder<Postgres> = QueryBuilder::new(
        "SELECT id, grid_id, row_order, cells, created_by, updated_by, created_at, last_modified FROM grid_rows WHERE grid_id = ",
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
        qb.push(", row_order ASC, id ASC");
    } else {
        qb.push("row_order ASC, id ASC");
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
    db: &sqlx::PgPool,
    grid_id: Uuid,
    cells: &mut RowCellMap,
) -> Result<(), AuthError> {
    let columns: Vec<GridColumn> = sqlx::query_as(
        r#"
        SELECT id, grid_id, name, property_type, property_config, width_px, position, is_primary, is_hidden, created_at, updated_at
        FROM grid_columns
        WHERE grid_id = $1
        "#,
    )
    .bind(grid_id)
    .fetch_all(db)
    .await?;

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
    let access = resolve_grid_access(&state.db, user.clone(), grid_id).await?;
    access.ensure_write()?;

    validate_and_stamp_cells(&state.db, grid_id, &mut payload.cells).await?;

    let row_order = match payload.row_order {
        Some(ord) => ord,
        None => {
            let max_ord: (f64,) = sqlx::query_as(
                "SELECT COALESCE(MAX(row_order) + 1000.0, 1000.0) FROM grid_rows WHERE grid_id = $1",
            )
            .bind(grid_id)
            .fetch_one(&state.db)
            .await?;
            max_ord.0
        }
    };

    let row: GridRow = sqlx::query_as(
        r#"
        INSERT INTO grid_rows (grid_id, row_order, cells, created_by, updated_by)
        VALUES ($1, $2, $3, $4, $4)
        RETURNING id, grid_id, row_order, cells, created_by, updated_by, created_at, last_modified
        "#,
    )
    .bind(grid_id)
    .bind(row_order)
    .bind(sqlx::types::Json(&payload.cells))
    .bind(user.user_id)
    .fetch_one(&state.db)
    .await?;

    Ok((StatusCode::CREATED, Json(row)))
}

pub async fn update_row(
    State(state): State<AppState>,
    RequireSpaceAccess(user): RequireSpaceAccess,
    Path((grid_id, row_id)): Path<(Uuid, Uuid)>,
    Json(mut payload): Json<PatchRowRequest>,
) -> Result<Json<GridRow>, AuthError> {
    let access = resolve_grid_access(&state.db, user.clone(), grid_id).await?;
    access.ensure_write()?;

    validate_and_stamp_cells(&state.db, grid_id, &mut payload.cells).await?;

    let updated: GridRow = sqlx::query_as(
        r#"
        UPDATE grid_rows
        SET cells = cells || $3::jsonb,
            row_order = COALESCE($4, row_order),
            updated_by = $5,
            last_modified = NOW()
        WHERE id = $1 AND grid_id = $2
        RETURNING id, grid_id, row_order, cells, created_by, updated_by, created_at, last_modified
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

    Ok(Json(updated))
}

pub async fn bulk_delete_rows(
    State(state): State<AppState>,
    RequireSpaceAccess(user): RequireSpaceAccess,
    Path(grid_id): Path<Uuid>,
    Json(payload): Json<BulkDeleteRowsRequest>,
) -> Result<Json<BulkDeleteRowsResponse>, AuthError> {
    let access = resolve_grid_access(&state.db, user.clone(), grid_id).await?;
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

    Ok(Json(BulkDeleteRowsResponse { deleted_count }))
}

pub async fn upload_cell_file(
    State(state): State<AppState>,
    RequireSpaceAccess(user): RequireSpaceAccess,
    Path((grid_id, row_id)): Path<(Uuid, Uuid)>,
    mut multipart: Multipart,
) -> Result<Json<GridRow>, AuthError> {
    let access = resolve_grid_access(&state.db, user.clone(), grid_id).await?;
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
        SELECT id, grid_id, row_order, cells, created_by, updated_by, created_at, last_modified
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
        RETURNING id, grid_id, row_order, cells, created_by, updated_by, created_at, last_modified
        "#,
    )
    .bind(row_id)
    .bind(grid_id)
    .bind(sqlx::types::Json(&row.cells))
    .bind(user.user_id)
    .fetch_one(&state.db)
    .await?;

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
