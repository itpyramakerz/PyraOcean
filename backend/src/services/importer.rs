use crate::{
    auth::{record_audit_log, resolve_space_access_cached, AppState, AuthError, RequireSpaceAccess},
    routes::grids::record_grid_history_snapshot,
};
use axum::{
    extract::{Multipart, Path, State},
    http::StatusCode,
    Json,
};
use chrono::{DateTime, NaiveDate, NaiveTime, Utc};
use flate2::read::ZlibDecoder;
use pulldown_cmark::{Event, Parser};
use pyra_ocean_shared::{
    CellValue, ChecklistItem, ColumnPropertyConfig, ColumnPropertyType, FileAttachment,
    GridColumn, ImportGridResponse, ImportMode, NumberFormat, PageGrid, PreviewColumnInfo,
    PreviewImportResponse, RowCellMap, SelectOption, TagColor,
};
use serde_json::json;
use sqlx::{Postgres, QueryBuilder};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    io::Read,
};
use uuid::Uuid;

// ============================================================================
// 1. Internal Parsed Table Representation
// ============================================================================

struct ParsedDataset {
    headers: Vec<String>,
    rows: Vec<Vec<String>>,
    detected_delimiter: Option<char>,
}

const TAG_PALETTE: [TagColor; 12] = [
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
    TagColor::Green,
    TagColor::Purple,
];

const FILE_EXTENSIONS: [&str; 14] = [
    ".pdf", ".png", ".jpg", ".jpeg", ".webp", ".gif", ".svg", ".doc", ".docx", ".xls", ".xlsx",
    ".csv", ".zip", ".mp4",
];

// ============================================================================
// 2. HTTP Multipart Import Handler (`POST /api/spaces/{space_id}/grids/import`)
// ============================================================================

/// Accepts a multipart upload containing:
/// - `file`: CSV, JSON, or Markdown export from Notion or AppFlowy
/// - `title`: (Optional) Grid title; defaults to uploaded filename stem
/// - `mode`: `"formatted_schema_inference"` (or `"formatted"`) | `"unformatted_raw_text"` (or `"unformatted"`)
pub async fn import_grid_handler(
    State(state): State<AppState>,
    RequireSpaceAccess(user): RequireSpaceAccess,
    Path(space_id): Path<Uuid>,
    mut multipart: Multipart,
) -> Result<(StatusCode, Json<ImportGridResponse>), AuthError> {
    let access = resolve_space_access_cached(&state, user.clone(), space_id).await?;
    access.ensure_manage()?;

    let mut grid_title: Option<String> = None;
    let mut import_mode = ImportMode::FormattedSchemaInference;
    let mut filename = String::from("Imported Grid.csv");
    let mut raw_bytes: Option<Vec<u8>> = None;

    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|e| AuthError::BadRequest(format!("Invalid multipart payload: {e}")))?
    {
        let name = field.name().unwrap_or_default().to_string();
        match name.as_str() {
            "title" => {
                let text = field
                    .text()
                    .await
                    .map_err(|e| AuthError::BadRequest(e.to_string()))?;
                if !text.trim().is_empty() {
                    grid_title = Some(text.trim().to_string());
                }
            }
            "mode" => {
                let text = field
                    .text()
                    .await
                    .map_err(|e| AuthError::BadRequest(e.to_string()))?;
                let norm = text.trim().to_lowercase();
                if norm == "unformatted" || norm == "unformatted_raw_text" || norm == "raw" {
                    import_mode = ImportMode::UnformattedRawText;
                } else {
                    import_mode = ImportMode::FormattedSchemaInference;
                }
            }
            "file" => {
                if let Some(fname) = field.file_name() {
                    filename = fname.to_string();
                }
                let bytes = field
                    .bytes()
                    .await
                    .map_err(|e| AuthError::BadRequest(format!("Failed to read upload: {e}")))?;
                raw_bytes = Some(bytes.to_vec());
            }
            _ => {}
        }
    }

    let file_bytes = raw_bytes.ok_or_else(|| {
        AuthError::BadRequest("Multipart request must include a 'file' field".into())
    })?;

    let derived_title = grid_title.unwrap_or_else(|| {
        std::path::Path::new(&filename)
            .file_stem()
            .and_then(|s| s.to_str())
            .filter(|s| !s.is_empty())
            .unwrap_or("Imported Grid")
            .to_string()
    });

    let is_pdf = filename.to_lowercase().ends_with(".pdf") || file_bytes.starts_with(b"%PDF");
    let dataset = if is_pdf {
        parse_pdf_dataset(&file_bytes)?
    } else {
        let content = String::from_utf8_lossy(&file_bytes)
            .trim_start_matches('\u{feff}')
            .to_string();
        parse_tabular_input(&filename, &content)?
    };
    if dataset.headers.is_empty() {
        return Err(AuthError::BadRequest(
            "Uploaded file does not contain any recognizable columns".into(),
        ));
    }

    // Lookup map of existing users by email and display_name (lowercased) for `Person` inference
    let user_lookup = load_user_lookup(&state.db).await?;

    let mut tx = state.db.begin().await?;

    // 1. Create the PageGrid record
    let grid: PageGrid = sqlx::query_as(
        r#"
        INSERT INTO pages_grids (space_id, title, description, icon, position, created_by, is_archived)
        VALUES ($1, $2, $3, 'table', 0, $4, FALSE)
        RETURNING id, space_id, title, description, icon, position, created_by, is_archived, created_at, updated_at
        "#,
    )
    .bind(space_id)
    .bind(&derived_title)
    .bind(Some(format!("Imported from {filename}")))
    .bind(user.user_id)
    .fetch_one(&mut *tx)
    .await?;

    // 2. Infer or construct Column Schemas
    let mut created_columns: Vec<GridColumn> = Vec::with_capacity(dataset.headers.len() + 1);
    let sample_rows: Vec<&Vec<String>> = dataset.rows.iter().take(25).collect();

    for (col_idx, header_raw) in dataset.headers.iter().enumerate() {
        let col_name = {
            let cleaned = strip_markdown_and_html(header_raw);
            if cleaned.is_empty() {
                format!("Column {}", col_idx + 1)
            } else {
                cleaned
            }
        };

        let (prop_type, prop_config) = match import_mode {
            ImportMode::UnformattedRawText => (
                ColumnPropertyType::Text,
                ColumnPropertyConfig::Text {
                    rich_text: false,
                    max_length: None,
                },
            ),
            ImportMode::FormattedSchemaInference => {
                let samples: Vec<&str> = sample_rows
                    .iter()
                    .filter_map(|r| r.get(col_idx).map(|s| s.trim()))
                    .filter(|s| !s.is_empty())
                    .collect();
                infer_column_schema(&samples, &dataset.rows, col_idx, &user_lookup)
            }
        };

        let inserted_col: GridColumn = sqlx::query_as(
            r#"
            INSERT INTO grid_columns (
                grid_id, name, property_type, property_config, width_px, position, is_primary, is_hidden
            )
            VALUES ($1, $2, $3, $4, 200, $5, $6, FALSE)
            RETURNING id, grid_id, name, property_type, property_config, width_px, position, is_primary, is_hidden, created_at, updated_at
            "#,
        )
        .bind(grid.id)
        .bind(&col_name)
        .bind(prop_type)
        .bind(sqlx::types::Json(prop_config))
        .bind(col_idx as i32)
        .bind(col_idx == 0)
        .fetch_one(&mut *tx)
        .await?;

        created_columns.push(inserted_col);
    }

    // In Formatted mode, automatically append a system `LastModified` column if not already present
    if import_mode == ImportMode::FormattedSchemaInference
        && !created_columns
            .iter()
            .any(|c| c.property_type == ColumnPropertyType::LastModified)
    {
        let last_mod_col: GridColumn = sqlx::query_as(
            r#"
            INSERT INTO grid_columns (
                grid_id, name, property_type, property_config, width_px, position, is_primary, is_hidden
            )
            VALUES ($1, 'Last Modified', 'last_modified', $2, 180, $3, FALSE, FALSE)
            RETURNING id, grid_id, name, property_type, property_config, width_px, position, is_primary, is_hidden, created_at, updated_at
            "#,
        )
        .bind(grid.id)
        .bind(sqlx::types::Json(ColumnPropertyConfig::LastModified))
        .bind(created_columns.len() as i32)
        .fetch_one(&mut *tx)
        .await?;

        created_columns.push(last_mod_col);
    }

    // 3. Convert & Bulk Insert Rows using batched SQLx QueryBuilder
    let now = Utc::now();
    let mut prepared_rows: Vec<(i32, f64, RowCellMap)> = Vec::with_capacity(dataset.rows.len());

    for (row_idx, raw_row) in dataset.rows.iter().enumerate() {
        let mut cell_map: RowCellMap = HashMap::with_capacity(created_columns.len());

        for (col_idx, col_schema) in created_columns.iter().enumerate() {
            if col_schema.property_type == ColumnPropertyType::LastModified {
                cell_map.insert(col_schema.id, CellValue::LastModified(now));
                continue;
            }

            let raw_cell = raw_row.get(col_idx).map(|s| s.as_str()).unwrap_or("");
            if raw_cell.trim().is_empty() {
                continue;
            }

            let cell_val = match import_mode {
                ImportMode::UnformattedRawText => {
                    CellValue::Text(strip_markdown_and_html(raw_cell))
                }
                ImportMode::FormattedSchemaInference => {
                    coerce_cell_value(raw_cell, col_schema, &user_lookup, user.user_id, now)
                }
            };

            cell_map.insert(col_schema.id, cell_val);
        }

        let position = row_idx as i32;
        let row_order = ((row_idx + 1) as f64) * 1000.0;
        prepared_rows.push((position, row_order, cell_map));
    }

    // Batch insert in chunks of 500 rows to stay well within PostgreSQL bind parameter limits
    for chunk in prepared_rows.chunks(500) {
        let mut qb: QueryBuilder<Postgres> = QueryBuilder::new(
            "INSERT INTO grid_rows (grid_id, position, row_order, cells, created_by, updated_by) ",
        );

        qb.push_values(chunk, |mut b, (position, row_order, cells)| {
            b.push_bind(grid.id)
                .push_bind(*position)
                .push_bind(*row_order)
                .push_bind(sqlx::types::Json(cells))
                .push_bind(user.user_id)
                .push_bind(user.user_id);
        });

        qb.build().execute(&mut *tx).await?;
    }

    tx.commit().await?;

    state.cache.invalidate_space(&space_id);

    let imported_row_count = prepared_rows.len();

    record_audit_log(
        &state.db,
        user.user_id,
        Some(space_id),
        Some(grid.id),
        "grid.import",
        json!({
            "filename": filename,
            "mode": import_mode,
            "columns": created_columns.len(),
            "rows": imported_row_count,
            "delimiter": dataset.detected_delimiter.map(|c| c.to_string()),
        }),
    )
    .await?;

    let _ = record_grid_history_snapshot(
        &state.db,
        grid.id,
        "grid.import",
        Some(user.user_id),
        Some(&user.email),
        &format!("Imported {imported_row_count} rows from '{filename}'"),
    )
    .await;

    Ok((
        StatusCode::CREATED,
        Json(ImportGridResponse {
            grid,
            columns: created_columns,
            imported_row_count,
            mode: import_mode,
            detected_delimiter: dataset.detected_delimiter.map(|c| c.to_string()),
        }),
    ))
}

/// `POST /api/spaces/{space_id}/grids/import/preview`
/// Parses an uploaded `.pdf`, `.csv`, `.json`, or `.md` file without writing to PostgreSQL,
/// returning the detected column headers, inferred property types, total row count, and sample rows
/// for interactive preview inside the Import Modal.
pub async fn preview_import_handler(
    State(state): State<AppState>,
    RequireSpaceAccess(user): RequireSpaceAccess,
    Path(space_id): Path<Uuid>,
    mut multipart: Multipart,
) -> Result<Json<PreviewImportResponse>, AuthError> {
    let access = resolve_space_access_cached(&state, user, space_id).await?;
    access.ensure_manage()?;

    let mut import_mode = ImportMode::FormattedSchemaInference;
    let mut filename = String::from("Imported Grid.csv");
    let mut raw_bytes: Option<Vec<u8>> = None;

    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|e| AuthError::BadRequest(format!("Invalid multipart payload: {e}")))?
    {
        let name = field.name().unwrap_or_default().to_string();
        match name.as_str() {
            "mode" => {
                let text = field
                    .text()
                    .await
                    .map_err(|e| AuthError::BadRequest(e.to_string()))?;
                let norm = text.trim().to_lowercase();
                if norm == "unformatted" || norm == "unformatted_raw_text" || norm == "raw" {
                    import_mode = ImportMode::UnformattedRawText;
                } else {
                    import_mode = ImportMode::FormattedSchemaInference;
                }
            }
            "file" => {
                if let Some(fname) = field.file_name() {
                    filename = fname.to_string();
                }
                let bytes = field
                    .bytes()
                    .await
                    .map_err(|e| AuthError::BadRequest(format!("Failed to read upload: {e}")))?;
                raw_bytes = Some(bytes.to_vec());
            }
            _ => {}
        }
    }

    let file_bytes = raw_bytes.ok_or_else(|| {
        AuthError::BadRequest("Multipart request must include a 'file' field".into())
    })?;

    let suggested_title = std::path::Path::new(&filename)
        .file_stem()
        .and_then(|s| s.to_str())
        .filter(|s| !s.is_empty())
        .unwrap_or("Imported Grid")
        .to_string();

    let is_pdf = filename.to_lowercase().ends_with(".pdf") || file_bytes.starts_with(b"%PDF");
    let dataset = if is_pdf {
        parse_pdf_dataset(&file_bytes)?
    } else {
        let content = String::from_utf8_lossy(&file_bytes)
            .trim_start_matches('\u{feff}')
            .to_string();
        parse_tabular_input(&filename, &content)?
    };

    if dataset.headers.is_empty() {
        return Err(AuthError::BadRequest(
            "Uploaded file does not contain any recognizable columns".into(),
        ));
    }

    let user_lookup = load_user_lookup(&state.db).await?;
    let sample_refs: Vec<&Vec<String>> = dataset.rows.iter().take(25).collect();

    let mut columns = Vec::with_capacity(dataset.headers.len());
    for (col_idx, header_raw) in dataset.headers.iter().enumerate() {
        let col_name = {
            let cleaned = strip_markdown_and_html(header_raw);
            if cleaned.is_empty() {
                format!("Column {}", col_idx + 1)
            } else {
                cleaned
            }
        };

        let inferred_type = match import_mode {
            ImportMode::UnformattedRawText => ColumnPropertyType::Text,
            ImportMode::FormattedSchemaInference => {
                let samples: Vec<&str> = sample_refs
                    .iter()
                    .filter_map(|r| r.get(col_idx).map(|s| s.trim()))
                    .filter(|s| !s.is_empty())
                    .collect();
                infer_column_schema(&samples, &dataset.rows, col_idx, &user_lookup).0
            }
        };

        columns.push(PreviewColumnInfo {
            name: col_name,
            inferred_type,
        });
    }

    let total_row_count = dataset.rows.len();
    let sample_rows: Vec<Vec<String>> = dataset
        .rows
        .into_iter()
        .take(10)
        .map(|mut row| {
            if row.len() < columns.len() {
                row.resize(columns.len(), String::new());
            } else if row.len() > columns.len() && !columns.is_empty() {
                let tail = row.split_off(columns.len() - 1).join(" ");
                row.push(tail);
            }
            row
        })
        .collect();

    Ok(Json(PreviewImportResponse {
        suggested_title,
        columns,
        sample_rows,
        total_row_count,
        detected_delimiter: dataset.detected_delimiter.map(|c| c.to_string()),
    }))
}

// ============================================================================
// 3. Smart Delimiter Detection & Multi-Format Parsing (CSV, JSON, Markdown)
// ============================================================================

/// Inspects the first 10 non-empty lines of a CSV string to automatically detect whether
/// columns are separated by `,`, `;`, or `\t`.
pub fn detect_csv_delimiter(content: &str) -> u8 {
    let sample_lines: Vec<&str> = content
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .take(10)
        .collect();

    if sample_lines.is_empty() {
        return b',';
    }

    let candidates = [b',', b';', b'\t'];
    let mut best_delim = b',';
    let mut best_score = 0usize;

    for &delim in &candidates {
        let counts: Vec<usize> = sample_lines
            .iter()
            .map(|line| count_unquoted_delimiters(line.as_bytes(), delim))
            .collect();

        let first = counts[0];
        if first == 0 {
            continue;
        }

        // Reward delimiters that appear consistently across sample rows
        let consistent_lines = counts.iter().filter(|&&c| c == first).count();
        let score = consistent_lines * 100 + first;
        if score > best_score {
            best_score = score;
            best_delim = delim;
        }
    }

    best_delim
}

fn count_unquoted_delimiters(line: &[u8], delim: u8) -> usize {
    let mut in_quotes = false;
    let mut count = 0;
    for &b in line {
        if b == b'"' {
            in_quotes = !in_quotes;
        } else if b == delim && !in_quotes {
            count += 1;
        }
    }
    count
}

fn parse_tabular_input(filename: &str, content: &str) -> Result<ParsedDataset, AuthError> {
    let lower_name = filename.to_lowercase();
    let trimmed = content.trim();

    if lower_name.ends_with(".json") || trimmed.starts_with('[') || trimmed.starts_with('{') {
        return parse_json_dataset(trimmed);
    }

    if (lower_name.ends_with(".md") || lower_name.ends_with(".markdown"))
        && trimmed.contains('|')
    {
        return parse_markdown_table(trimmed);
    }

    // Default to smart-delimiter CSV parser
    let delim = detect_csv_delimiter(content);
    let mut rdr = csv::ReaderBuilder::new()
        .delimiter(delim)
        .flexible(true)
        .has_headers(true)
        .from_reader(content.as_bytes());

    let headers: Vec<String> = rdr
        .headers()
        .map_err(|e| AuthError::BadRequest(format!("CSV header parse error: {e}")))?
        .iter()
        .map(|s| s.trim().to_string())
        .collect();

    let mut rows = Vec::new();
    for result in rdr.records() {
        let record =
            result.map_err(|e| AuthError::BadRequest(format!("CSV row parse error: {e}")))?;
        let row: Vec<String> = record.iter().map(|s| s.to_string()).collect();
        if row.iter().any(|cell| !cell.trim().is_empty()) {
            rows.push(row);
        }
    }

    Ok(ParsedDataset {
        headers,
        rows,
        detected_delimiter: Some(delim as char),
    })
}

fn parse_json_dataset(content: &str) -> Result<ParsedDataset, AuthError> {
    let parsed: serde_json::Value = serde_json::from_str(content)
        .map_err(|e| AuthError::BadRequest(format!("Invalid JSON import file: {e}")))?;

    let array = match parsed {
        serde_json::Value::Array(arr) => arr,
        serde_json::Value::Object(mut obj) => {
            if let Some(serde_json::Value::Array(rows)) = obj.remove("rows").or_else(|| obj.remove("data")) {
                rows
            } else {
                vec![serde_json::Value::Object(obj)]
            }
        }
        _ => {
            return Err(AuthError::BadRequest(
                "JSON import must be an array of row objects".into(),
            ))
        }
    };

    let mut header_set: Vec<String> = Vec::new();
    for item in &array {
        if let serde_json::Value::Object(map) = item {
            for key in map.keys() {
                if !header_set.contains(key) {
                    header_set.push(key.clone());
                }
            }
        }
    }

    let mut rows = Vec::with_capacity(array.len());
    for item in array {
        if let serde_json::Value::Object(map) = item {
            let mut row = Vec::with_capacity(header_set.len());
            for key in &header_set {
                let cell_str = match map.get(key) {
                    None | Some(serde_json::Value::Null) => String::new(),
                    Some(serde_json::Value::String(s)) => s.clone(),
                    Some(serde_json::Value::Bool(b)) => b.to_string(),
                    Some(serde_json::Value::Number(n)) => n.to_string(),
                    Some(serde_json::Value::Array(arr)) => arr
                        .iter()
                        .map(|v| match v {
                            serde_json::Value::String(s) => s.clone(),
                            other => other.to_string(),
                        })
                        .collect::<Vec<_>>()
                        .join(", "),
                    Some(other) => other.to_string(),
                };
                row.push(cell_str);
            }
            rows.push(row);
        }
    }

    Ok(ParsedDataset {
        headers: header_set,
        rows,
        detected_delimiter: None,
    })
}

fn parse_markdown_table(content: &str) -> Result<ParsedDataset, AuthError> {
    let table_lines: Vec<&str> = content
        .lines()
        .map(str::trim)
        .filter(|l| l.starts_with('|') && l.ends_with('|'))
        .collect();

    if table_lines.is_empty() {
        return Err(AuthError::BadRequest(
            "Markdown file does not contain a valid pipe table".into(),
        ));
    }

    let split_pipe_line = |line: &str| -> Vec<String> {
        line.trim_matches('|')
            .split('|')
            .map(|s| s.trim().to_string())
            .collect()
    };

    let headers = split_pipe_line(table_lines[0]);
    let mut rows = Vec::new();

    for line in table_lines.into_iter().skip(1) {
        // Skip Markdown separator line like `| --- | :--- |`
        let is_separator = line
            .trim_matches('|')
            .split('|')
            .all(|cell| cell.trim().chars().all(|c| c == '-' || c == ':' || c == ' '));
        if is_separator {
            continue;
        }
        rows.push(split_pipe_line(line));
    }

    Ok(ParsedDataset {
        headers,
        rows,
        detected_delimiter: Some('|'),
    })
}

/// Multi-Stage Coordinate/Layout-Aware PDF Table Parser:
/// 1. Stage 1 (`pdftotext -layout -nopgbrk` + Rust Stream Fallback):
///    Writes the PDF bytes to a temporary file and runs `pdftotext -layout -nopgbrk <temp_pdf> -`
///    (from `poppler-utils`) to preserve exact 2D `(X, Y)` horizontal character spacing and vertical
///    row alignment. Falls back to the pure-Rust PDF stream extractor if `pdftotext` is unavailable.
/// 2. Stage 2 (Currency Protection, Smart Column Boundary Detection & Row Normalization):
///    - Protects currency prefixes (`EGP 1,000`, `EGP 10,940`, `$ 75.00`, `USD 500`) so right-aligned
///      currency spacing is never split into separate columns.
///    - Filters out non-tabular header/footer banner lines and decorative borders.
///    - Detects columns via pipes (`|`), tabs (`\t`), or horizontal multi-space gaps (`2+` spaces)
///      and normalizes every extracted row to match the header column count.
fn parse_pdf_dataset(pdf_bytes: &[u8]) -> Result<ParsedDataset, AuthError> {
    let raw_layout_lines = extract_pdf_layout_via_pdftotext(pdf_bytes).unwrap_or_else(|| {
        let mut stream_lines = extract_text_lines_from_pdf_streams(pdf_bytes);
        if stream_lines.is_empty() {
            let lossy = String::from_utf8_lossy(pdf_bytes);
            for line in lossy.lines() {
                let trimmed = line.trim();
                if trimmed.len() >= 3
                    && !trimmed.starts_with('%')
                    && !trimmed.starts_with('/')
                    && !trimmed.starts_with("<<")
                    && !trimmed.starts_with(">>")
                    && !matches!(
                        trimmed,
                        "obj" | "endobj" | "stream" | "endstream" | "xref" | "trailer" | "startxref"
                    )
                    && !trimmed.ends_with(" obj")
                    && trimmed.chars().filter(|c| c.is_alphabetic()).count() >= 2
                {
                    stream_lines.push(line.to_string());
                }
            }
        }
        stream_lines
    });

    // Filter out empty lines, decorative border lines (`---`, `===`), and standalone page footers
    let candidate_lines: Vec<String> = raw_layout_lines
        .into_iter()
        .map(|l| normalize_currency_spacing(&l))
        .filter(|l| {
            let trimmed = l.trim();
            if trimmed.is_empty() {
                return false;
            }
            // Decorative border lines (`+---+---+`, `=======`, `-------`)
            let is_border = trimmed
                .chars()
                .all(|c| matches!(c, '-' | '=' | '_' | '+' | '*' | ' ' | '─' | '━' | '│' | '┼'));
            if is_border {
                return false;
            }
            // Standalone page number lines like "Page 1 of 3" or "- 1 -"
            let lower = trimmed.to_lowercase();
            if (lower.starts_with("page ") && lower.len() <= 18 && !lower.contains("  "))
                || (trimmed.starts_with("- ") && trimmed.ends_with(" -") && trimmed.len() <= 10)
            {
                return false;
            }
            true
        })
        .collect();

    if candidate_lines.is_empty() {
        return Err(AuthError::BadRequest(
            "Could not extract any readable text or table rows from the uploaded PDF file.".into(),
        ));
    }

    let mut split_rows: Vec<Vec<String>> = Vec::new();
    let mut detected_delim: Option<char> = Some(' ');

    // Also compute vertical whitespace gap boundaries across fixed-width layout lines
    // as a fallback when columns are separated by a single aligned space column.
    let histogram_boundaries = detect_vertical_column_boundaries(&candidate_lines);

    for line in &candidate_lines {
        let trimmed = line.trim();
        let raw_cols: Vec<String> = if trimmed.contains('|') {
            detected_delim = Some('|');
            trimmed
                .trim_matches('|')
                .split('|')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect()
        } else if line.contains('\t') {
            detected_delim = Some('\t');
            line.split('\t')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect()
        } else if trimmed.contains("  ") {
            split_on_multi_spaces(trimmed)
        } else if histogram_boundaries.len() >= 2 {
            split_line_by_boundaries(line, &histogram_boundaries)
        } else if trimmed.contains(',') && trimmed.split(',').count() >= 2 {
            detected_delim = Some(',');
            trimmed.split(',').map(|s| s.trim().to_string()).collect()
        } else {
            vec![trimmed.to_string()]
        };

        let cols = merge_split_currency_tokens(raw_cols);
        if !cols.is_empty() {
            split_rows.push(cols);
        }
    }

    if split_rows.is_empty() {
        return Err(AuthError::BadRequest(
            "PDF file did not contain any valid table rows".into(),
        ));
    }

    // Filter out non-tabular header/footer banner lines (lines with fewer than 2 columns
    // when multi-column table lines exist)
    let multi_col_rows: Vec<Vec<String>> = split_rows
        .iter()
        .filter(|r| r.len() >= 2)
        .cloned()
        .collect();

    let effective_rows = if multi_col_rows.len() >= 2 {
        // Find the dominant (modal) column count among multi-column rows so title banners
        // with 2 tokens don't override a 5-column table header
        let mut col_count_freq: HashMap<usize, usize> = HashMap::new();
        for r in &multi_col_rows {
            *col_count_freq.entry(r.len()).or_insert(0) += 1;
        }
        let dominant_col_count = col_count_freq
            .into_iter()
            .max_by_key(|&(col_len, freq)| (freq, col_len))
            .map(|(col_len, _)| col_len)
            .unwrap_or(2);

        // Start the table at the first row that has at least `dominant_col_count.saturating_sub(1).max(2)` columns
        let min_header_cols = dominant_col_count.saturating_sub(1).max(2);
        let start_idx = multi_col_rows
            .iter()
            .position(|r| r.len() >= min_header_cols)
            .unwrap_or(0);
        multi_col_rows.into_iter().skip(start_idx).collect()
    } else {
        split_rows
    };

    let headers = effective_rows[0].clone();
    let target_col_count = headers.len().max(1);

    // Normalize every extracted row to match `headers.len()` (pad short rows with empty strings
    // and merge overflow tokens into the last column)
    let mut rows: Vec<Vec<String>> = Vec::with_capacity(effective_rows.len().saturating_sub(1));
    for mut row in effective_rows.into_iter().skip(1) {
        // Skip repeated page headers if a multi-page PDF repeats the exact table header row
        if row == headers {
            continue;
        }
        if row.len() < target_col_count {
            row.resize(target_col_count, String::new());
        } else if row.len() > target_col_count {
            let overflow = row.split_off(target_col_count - 1).join(" ");
            row.push(overflow);
        }
        if row.iter().any(|c| !c.trim().is_empty()) {
            rows.push(row);
        }
    }

    Ok(ParsedDataset {
        headers,
        rows,
        detected_delimiter: detected_delim,
    })
}

/// Runs `pdftotext -layout -nopgbrk <temp_pdf_path> -` to extract 2D layout-preserved text lines.
/// Returns `None` if `pdftotext` is not installed or fails.
fn extract_pdf_layout_via_pdftotext(pdf_bytes: &[u8]) -> Option<Vec<String>> {
    let temp_path = std::env::temp_dir().join(format!("pyraocean_pdf_{}.pdf", Uuid::new_v4()));
    if std::fs::write(&temp_path, pdf_bytes).is_err() {
        return None;
    }

    let output_res = std::process::Command::new("pdftotext")
        .args([
            "-layout",
            "-nopgbrk",
            temp_path.to_str().unwrap_or_default(),
            "-",
        ])
        .output();

    let _ = std::fs::remove_file(&temp_path);

    let output = output_res.ok()?;
    if !output.status.success() {
        return None;
    }

    let text = String::from_utf8_lossy(&output.stdout);
    let lines: Vec<String> = text
        .lines()
        .map(|l| l.trim_end().to_string())
        .filter(|l| !l.trim().is_empty())
        .collect();

    if lines.is_empty() {
        None
    } else {
        Some(lines)
    }
}

/// Collapses multi-space gaps between a currency prefix (`EGP`, `USD`, `EUR`, `GBP`, `SAR`, `AED`,
/// `$`, `€`, `£`, `¥`) and a following number (`"EGP    1,000"` -> `"EGP 1,000"`) so right-aligned
/// currency columns in `pdftotext -layout` stay together in a single cell.
fn normalize_currency_spacing(line: &str) -> String {
    const CURRENCY_PREFIXES: [&str; 10] = [
        "EGP", "USD", "EUR", "GBP", "SAR", "AED", "$", "€", "£", "¥",
    ];
    let chars: Vec<char> = line.chars().collect();
    let mut out = String::with_capacity(line.len());
    let mut i = 0usize;

    while i < chars.len() {
        let remaining: String = chars[i..].iter().collect();
        let mut matched_prefix: Option<&str> = None;

        for &sym in &CURRENCY_PREFIXES {
            if remaining.starts_with(sym) {
                let prev_ok = i == 0 || !chars[i - 1].is_alphabetic();
                let after_idx = i + sym.chars().count();
                let next_is_space = after_idx < chars.len() && chars[after_idx] == ' ';
                if prev_ok && next_is_space {
                    matched_prefix = Some(sym);
                    break;
                }
            }
        }

        if let Some(sym) = matched_prefix {
            let sym_len = sym.chars().count();
            let mut space_end = i + sym_len;
            while space_end < chars.len() && chars[space_end] == ' ' {
                space_end += 1;
            }
            if space_end < chars.len()
                && (chars[space_end].is_ascii_digit()
                    || (chars[space_end] == '-'
                        && space_end + 1 < chars.len()
                        && chars[space_end + 1].is_ascii_digit()))
            {
                // Keep the column alignment by shifting the extra spaces before the currency symbol
                let extra_spaces = (space_end - (i + sym_len)).saturating_sub(1);
                for _ in 0..extra_spaces {
                    out.push(' ');
                }
                out.push_str(sym);
                out.push(' ');
                i = space_end;
                continue;
            }
        }

        out.push(chars[i]);
        i += 1;
    }

    out
}

/// Post-processes extracted row tokens to merge any standalone currency symbol token (`"EGP"`, `"$"`, etc.)
/// with an immediately following numeric token (`"1,000"`, `"75.00"`).
fn merge_split_currency_tokens(tokens: Vec<String>) -> Vec<String> {
    const CURRENCY_CODES: [&str; 10] = [
        "EGP", "USD", "EUR", "GBP", "SAR", "AED", "$", "€", "£", "¥",
    ];
    let mut merged: Vec<String> = Vec::with_capacity(tokens.len());
    let mut idx = 0usize;

    while idx < tokens.len() {
        let cur = tokens[idx].trim();
        let is_currency = CURRENCY_CODES
            .iter()
            .any(|c| cur.eq_ignore_ascii_case(c));
        if is_currency && idx + 1 < tokens.len() {
            let next_tok = tokens[idx + 1].trim();
            let looks_numeric = !next_tok.is_empty()
                && next_tok
                    .chars()
                    .next()
                    .map(|c| c.is_ascii_digit() || c == '-')
                    .unwrap_or(false)
                && next_tok
                    .chars()
                    .all(|c| c.is_ascii_digit() || matches!(c, ',' | '.' | '-' | ' '));
            if looks_numeric {
                merged.push(format!("{cur} {next_tok}"));
                idx += 2;
                continue;
            }
        }
        merged.push(tokens[idx].clone());
        idx += 1;
    }

    merged
}

/// Analyzes vertical character column indices across candidate lines to detect consistent
/// whitespace column boundaries in fixed-width layout tables.
fn detect_vertical_column_boundaries(lines: &[String]) -> Vec<usize> {
    if lines.len() < 2 {
        return Vec::new();
    }
    let max_len = lines
        .iter()
        .map(|l| l.chars().count())
        .max()
        .unwrap_or(0)
        .min(300);
    if max_len < 10 {
        return Vec::new();
    }

    let mut space_counts = vec![0usize; max_len];
    let sample_count = lines.len().min(40);
    for line in lines.iter().take(sample_count) {
        let chars: Vec<char> = line.chars().collect();
        for (col_idx, count) in space_counts.iter_mut().enumerate().take(max_len) {
            if chars.get(col_idx).copied().unwrap_or(' ') == ' ' {
                *count += 1;
            }
        }
    }

    let threshold = (sample_count * 85) / 100;
    let mut boundaries = Vec::new();
    let mut in_gap = true;
    for (idx, &cnt) in space_counts.iter().enumerate() {
        let is_space_col = cnt >= threshold.max(2);
        if !is_space_col && in_gap {
            boundaries.push(idx);
            in_gap = false;
        } else if is_space_col {
            in_gap = true;
        }
    }
    boundaries
}

fn split_line_by_boundaries(line: &str, boundaries: &[usize]) -> Vec<String> {
    let chars: Vec<char> = line.chars().collect();
    let mut cols = Vec::with_capacity(boundaries.len());
    for (i, &start) in boundaries.iter().enumerate() {
        if start >= chars.len() {
            cols.push(String::new());
            continue;
        }
        let end = boundaries
            .get(i + 1)
            .copied()
            .unwrap_or(chars.len())
            .min(chars.len());
        let slice: String = chars[start..end].iter().collect();
        cols.push(slice.trim().to_string());
    }
    while cols.last().map(|s| s.is_empty()).unwrap_or(false) {
        cols.pop();
    }
    cols
}

fn split_on_multi_spaces(line: &str) -> Vec<String> {
    let mut cols = Vec::new();
    let mut current = String::new();
    let mut space_run = 0usize;

    for ch in line.chars() {
        if ch == ' ' {
            space_run += 1;
        } else {
            if space_run >= 2 {
                let trimmed = current.trim().to_string();
                if !trimmed.is_empty() {
                    cols.push(trimmed);
                }
                current.clear();
            } else if space_run == 1 && !current.is_empty() {
                current.push(' ');
            }
            space_run = 0;
            current.push(ch);
        }
    }

    let tail = current.trim().to_string();
    if !tail.is_empty() {
        cols.push(tail);
    }
    cols
}

fn extract_text_lines_from_pdf_streams(pdf_bytes: &[u8]) -> Vec<String> {
    let mut lines = Vec::new();
    let mut cursor = 0usize;

    while let Some(stream_pos) = find_subslice(&pdf_bytes[cursor..], b"stream") {
        let abs_stream = cursor + stream_pos + 6;
        let mut data_start = abs_stream;
        if pdf_bytes.get(data_start) == Some(&b'\r') {
            data_start += 1;
        }
        if pdf_bytes.get(data_start) == Some(&b'\n') {
            data_start += 1;
        }

        let Some(end_offset) = find_subslice(&pdf_bytes[data_start..], b"endstream") else {
            break;
        };
        let data_end = data_start + end_offset;
        let raw_stream = &pdf_bytes[data_start..data_end];

        // Attempt zlib/FlateDecode decompression first; fall back to raw stream bytes
        let decoded_bytes = {
            let trimmed_stream = trim_crlf(raw_stream);
            let mut decoder = ZlibDecoder::new(trimmed_stream);
            let mut decompressed = Vec::new();
            if decoder.read_to_end(&mut decompressed).is_ok() && !decompressed.is_empty() {
                decompressed
            } else {
                trimmed_stream.to_vec()
            }
        };

        let content_str = String::from_utf8_lossy(&decoded_bytes);
        extract_bt_et_lines(&content_str, &mut lines);

        cursor = data_end + 9;
    }

    // Also inspect top-level content in case the PDF stores unstreamed text operators
    if lines.is_empty() {
        let full_lossy = String::from_utf8_lossy(pdf_bytes);
        extract_bt_et_lines(&full_lossy, &mut lines);
    }

    lines
}

fn trim_crlf(mut bytes: &[u8]) -> &[u8] {
    while matches!(bytes.last(), Some(b'\r' | b'\n')) {
        bytes = &bytes[..bytes.len() - 1];
    }
    bytes
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn extract_bt_et_lines(stream_text: &str, out_lines: &mut Vec<String>) {
    let mut current_line = String::new();
    let chars: Vec<char> = stream_text.chars().collect();
    let mut i = 0usize;

    while i < chars.len() {
        if chars[i] == '(' {
            // Parse PDF literal string `(...)` with escape handling
            i += 1;
            let mut depth = 1usize;
            let mut token = String::new();
            while i < chars.len() && depth > 0 {
                let c = chars[i];
                if c == '\\' && i + 1 < chars.len() {
                    i += 1;
                    match chars[i] {
                        'n' => token.push('\n'),
                        'r' => token.push('\r'),
                        't' => token.push('\t'),
                        other => token.push(other),
                    }
                } else if c == '(' {
                    depth += 1;
                    token.push(c);
                } else if c == ')' {
                    depth -= 1;
                    if depth > 0 {
                        token.push(c);
                    }
                } else {
                    token.push(c);
                }
                i += 1;
            }
            if !token.is_empty() {
                if !current_line.is_empty() && !current_line.ends_with(' ') {
                    current_line.push_str("  ");
                }
                current_line.push_str(&token);
            }
        } else if chars[i] == 'T' && i + 1 < chars.len() {
            let next = chars[i + 1];
            // `Td`, `TD`, `T*` move to a new text line in PDF coordinate space
            if matches!(next, 'd' | 'D' | '*') {
                let trimmed = current_line.trim().to_string();
                if !trimmed.is_empty() {
                    out_lines.push(trimmed);
                }
                current_line.clear();
                i += 2;
                continue;
            }
            i += 1;
        } else if chars[i] == 'E' && i + 1 < chars.len() && chars[i + 1] == 'T' {
            let trimmed = current_line.trim().to_string();
            if !trimmed.is_empty() {
                out_lines.push(trimmed);
            }
            current_line.clear();
            i += 2;
        } else {
            i += 1;
        }
    }

    let trimmed = current_line.trim().to_string();
    if !trimmed.is_empty() {
        out_lines.push(trimmed);
    }
}

// ============================================================================
// 4. Schema Inference Engine (All 11 Strict Property Types)
// ============================================================================

async fn load_user_lookup(db: &sqlx::PgPool) -> Result<HashMap<String, Uuid>, AuthError> {
    let rows: Vec<(Uuid, String, String)> =
        sqlx::query_as("SELECT id, email, display_name FROM users WHERE is_active = TRUE")
            .fetch_all(db)
            .await?;

    let mut map = HashMap::with_capacity(rows.len() * 2);
    for (id, email, display_name) in rows {
        map.insert(email.to_lowercase(), id);
        map.insert(display_name.to_lowercase(), id);
    }
    Ok(map)
}

fn infer_column_schema(
    samples: &[&str],
    all_rows: &[Vec<String>],
    col_idx: usize,
    user_lookup: &HashMap<String, Uuid>,
) -> (ColumnPropertyType, ColumnPropertyConfig) {
    if samples.is_empty() {
        return (
            ColumnPropertyType::Text,
            ColumnPropertyConfig::Text {
                rich_text: false,
                max_length: None,
            },
        );
    }

    // 1. Check `Checkbox`: "true", "false", "yes", "no", "[x]", "[ ]"
    if samples.iter().all(|s| parse_checkbox_str(s).is_some()) {
        return (ColumnPropertyType::Checkbox, ColumnPropertyConfig::Checkbox);
    }

    // 2. Check `Checklist`: Multiple "[x] task" / "[ ] task" items inside a cell
    if samples.iter().all(|s| is_checklist_str(s)) {
        return (
            ColumnPropertyType::Checklist,
            ColumnPropertyConfig::Checklist {
                show_progress_bar: true,
            },
        );
    }

    // 3. Check `Date`: ISO-8601 (`YYYY-MM-DD`) or `DD/MM/YYYY`
    if samples.iter().all(|s| parse_date_str(s).is_some()) {
        return (
            ColumnPropertyType::Date,
            ColumnPropertyConfig::Date {
                display_format: "%Y-%m-%d".into(),
                include_time: false,
            },
        );
    }

    // 4. Check `Time`: `HH:MM` or `HH:MM:SS`
    if samples.iter().all(|s| parse_time_str(s).is_some()) {
        return (
            ColumnPropertyType::Time,
            ColumnPropertyConfig::Time {
                use_24_hour: true,
                is_duration: false,
            },
        );
    }

    // 5. Check `Number` (including currency `$100`, `EGP 1,000`, `42.5`)
    let parsed_numbers: Vec<Option<(f64, Option<String>, bool)>> =
        samples.iter().map(|s| parse_number_or_currency(s)).collect();
    if parsed_numbers.iter().all(|opt| opt.is_some()) {
        let mut detected_currency: Option<String> = None;
        let mut has_decimal = false;
        for item in parsed_numbers.into_iter().flatten() {
            if item.1.is_some() {
                detected_currency = item.1;
            }
            if item.2 {
                has_decimal = true;
            }
        }
        let format = if let Some(symbol) = detected_currency {
            NumberFormat::Currency {
                symbol,
                precision: 2,
            }
        } else if has_decimal {
            NumberFormat::Decimal { precision: 2 }
        } else {
            NumberFormat::Integer
        };

        return (
            ColumnPropertyType::Number,
            ColumnPropertyConfig::Number { format },
        );
    }

    // 6. Check `Person`: Matches active user emails or display names in PostgreSQL
    if !user_lookup.is_empty()
        && samples.iter().all(|s| {
            s.split(',')
                .map(str::trim)
                .filter(|p| !p.is_empty())
                .all(|token| user_lookup.contains_key(&token.to_lowercase()))
        })
    {
        return (
            ColumnPropertyType::Person,
            ColumnPropertyConfig::Person {
                allow_multiple: true,
            },
        );
    }

    // 7. Check `Files`: URLs ending in common attachment extensions
    if samples.iter().all(|s| is_file_url_list(s)) {
        return (
            ColumnPropertyType::Files,
            ColumnPropertyConfig::Files {
                allowed_mime_prefixes: Vec::new(),
                max_file_size_bytes: 50 * 1024 * 1024,
            },
        );
    }

    // 8. Check `MultiSelect`: Comma-separated tokens with repeating tags across rows
    let has_commas = samples.iter().any(|s| s.contains(','));
    if has_commas {
        let mut tag_counts: BTreeMap<String, usize> = BTreeMap::new();
        let mut total_tokens = 0usize;
        for row in all_rows {
            if let Some(cell) = row.get(col_idx) {
                for token in cell.split(',').map(str::trim).filter(|t| !t.is_empty()) {
                    if token.len() <= 48 {
                        *tag_counts.entry(token.to_string()).or_insert(0) += 1;
                        total_tokens += 1;
                    }
                }
            }
        }
        if !tag_counts.is_empty() && tag_counts.len() < total_tokens {
            let options = tag_counts
                .into_keys()
                .enumerate()
                .map(|(idx, label)| SelectOption {
                    id: Uuid::new_v4(),
                    label,
                    color: TAG_PALETTE[idx % TAG_PALETTE.len()],
                })
                .collect();
            return (
                ColumnPropertyType::MultiSelect,
                ColumnPropertyConfig::MultiSelect { options },
            );
        }
    }

    // 9. Check `Select`: Single categorical values that repeat across rows
    if samples.len() >= 2 && samples.iter().all(|s| s.len() <= 60 && !s.contains('\n')) {
        let mut category_counts: BTreeMap<String, usize> = BTreeMap::new();
        let mut non_empty_rows = 0usize;
        for row in all_rows {
            if let Some(cell) = row.get(col_idx).map(|s| s.trim()).filter(|s| !s.is_empty()) {
                *category_counts.entry(cell.to_string()).or_insert(0) += 1;
                non_empty_rows += 1;
            }
        }
        // If unique categories are fewer than total non-empty rows (repeating values)
        if !category_counts.is_empty()
            && category_counts.len() < non_empty_rows
            && category_counts.len() <= 50
        {
            let options = category_counts
                .into_keys()
                .enumerate()
                .map(|(idx, label)| SelectOption {
                    id: Uuid::new_v4(),
                    label,
                    color: TAG_PALETTE[idx % TAG_PALETTE.len()],
                })
                .collect();
            return (
                ColumnPropertyType::Select,
                ColumnPropertyConfig::Select { options },
            );
        }
    }

    // 10. Fallback -> `Text`
    (
        ColumnPropertyType::Text,
        ColumnPropertyConfig::Text {
            rich_text: false,
            max_length: None,
        },
    )
}

// ============================================================================
// 5. Value Parsers & Coercion Helpers
// ============================================================================

fn parse_checkbox_str(s: &str) -> Option<bool> {
    match s.trim().to_lowercase().as_str() {
        "true" | "yes" | "[x]" | "1" | "checked" => Some(true),
        "false" | "no" | "[ ]" | "[]" | "0" | "unchecked" => Some(false),
        _ => None,
    }
}

fn is_checklist_str(s: &str) -> bool {
    let trimmed = s.trim();
    if !(trimmed.contains("[x]") || trimmed.contains("[X]") || trimmed.contains("[ ]")) {
        return false;
    }
    // Distinguish a single "[x]" (which is a Checkbox) from a Checklist with item text
    trimmed.len() > 3
}

fn parse_checklist_items(s: &str) -> Vec<ChecklistItem> {
    let mut items = Vec::new();
    for part in s.lines().flat_map(|l| l.split(';')) {
        let p = part.trim().trim_start_matches('-').trim_start_matches('*').trim();
        if let Some(rest) = p.strip_prefix("[x]").or_else(|| p.strip_prefix("[X]")) {
            items.push(ChecklistItem {
                id: Uuid::new_v4(),
                item: rest.trim().to_string(),
                completed: true,
            });
        } else if let Some(rest) = p.strip_prefix("[ ]").or_else(|| p.strip_prefix("[]")) {
            items.push(ChecklistItem {
                id: Uuid::new_v4(),
                item: rest.trim().to_string(),
                completed: false,
            });
        }
    }
    items
}

fn parse_date_str(s: &str) -> Option<NaiveDate> {
    let trimmed = s.trim();
    let formats = ["%Y-%m-%d", "%d/%m/%Y", "%Y/%m/%d", "%m/%d/%Y", "%B %d, %Y", "%b %d, %Y"];
    for fmt in formats {
        if let Ok(d) = NaiveDate::parse_from_str(trimmed, fmt) {
            return Some(d);
        }
    }
    if let Ok(dt) = DateTime::parse_from_rfc3339(trimmed) {
        return Some(dt.date_naive());
    }
    None
}

fn parse_time_str(s: &str) -> Option<NaiveTime> {
    let trimmed = s.trim();
    NaiveTime::parse_from_str(trimmed, "%H:%M:%S")
        .or_else(|_| NaiveTime::parse_from_str(trimmed, "%H:%M"))
        .ok()
}

fn parse_number_or_currency(s: &str) -> Option<(f64, Option<String>, bool)> {
    let mut working = s.trim().to_string();
    if working.is_empty() {
        return None;
    }

    let currencies = ["EGP", "USD", "EUR", "GBP", "SAR", "AED", "$", "€", "£", "¥"];
    let mut detected_symbol: Option<String> = None;

    for sym in currencies {
        if let Some(stripped) = working.strip_prefix(sym) {
            detected_symbol = Some(sym.to_string());
            working = stripped.trim().to_string();
            break;
        } else if let Some(stripped) = working.strip_suffix(sym) {
            detected_symbol = Some(sym.to_string());
            working = stripped.trim().to_string();
            break;
        }
    }

    let cleaned = working.replace(',', "");
    let has_decimal = cleaned.contains('.');
    let val: f64 = cleaned.parse().ok()?;
    if !val.is_finite() {
        return None;
    }

    Some((val, detected_symbol, has_decimal))
}

fn is_file_url_list(s: &str) -> bool {
    let parts: Vec<&str> = s.split(',').map(str::trim).filter(|p| !p.is_empty()).collect();
    if parts.is_empty() {
        return false;
    }
    parts.iter().all(|part| {
        let lower = part.to_lowercase();
        (lower.starts_with("http://") || lower.starts_with("https://") || lower.starts_with("/uploads/"))
            && FILE_EXTENSIONS.iter().any(|ext| lower.ends_with(ext))
    })
}

fn coerce_cell_value(
    raw: &str,
    col: &GridColumn,
    user_lookup: &HashMap<String, Uuid>,
    actor_id: Uuid,
    now: DateTime<Utc>,
) -> CellValue {
    match (&col.property_type, &col.property_config) {
        (ColumnPropertyType::Checkbox, _) => {
            CellValue::Checkbox(parse_checkbox_str(raw).unwrap_or(false))
        }
        (ColumnPropertyType::Checklist, _) => CellValue::Checklist(parse_checklist_items(raw)),
        (ColumnPropertyType::Date, _) => CellValue::Date(parse_date_str(raw)),
        (ColumnPropertyType::Time, _) => CellValue::Time {
            time_of_day: parse_time_str(raw),
            duration_seconds: None,
        },
        (ColumnPropertyType::Number, _) => {
            let num = parse_number_or_currency(raw).map(|t| t.0).unwrap_or(0.0);
            CellValue::Number(num)
        }
        (ColumnPropertyType::Select, ColumnPropertyConfig::Select { options }) => {
            let target = raw.trim();
            let matched = options
                .iter()
                .find(|o| o.label.eq_ignore_ascii_case(target))
                .map(|o| o.id);
            CellValue::Select(matched)
        }
        (ColumnPropertyType::MultiSelect, ColumnPropertyConfig::MultiSelect { options }) => {
            let mut ids = Vec::new();
            for token in raw.split(',').map(str::trim).filter(|t| !t.is_empty()) {
                if let Some(opt) = options.iter().find(|o| o.label.eq_ignore_ascii_case(token)) {
                    ids.push(opt.id);
                }
            }
            CellValue::MultiSelect(ids)
        }
        (ColumnPropertyType::Person, _) => {
            let mut user_ids = Vec::new();
            let mut seen = HashSet::new();
            for token in raw.split(',').map(str::trim).filter(|t| !t.is_empty()) {
                if let Some(&uid) = user_lookup.get(&token.to_lowercase()) {
                    if seen.insert(uid) {
                        user_ids.push(uid);
                    }
                }
            }
            CellValue::Person(user_ids)
        }
        (ColumnPropertyType::Files, _) => {
            let attachments = raw
                .split(',')
                .map(str::trim)
                .filter(|u| !u.is_empty())
                .map(|url| {
                    let filename = url
                        .rsplit('/')
                        .next()
                        .unwrap_or("attachment")
                        .to_string();
                    FileAttachment {
                        id: Uuid::new_v4(),
                        filename,
                        mime_type: "application/octet-stream".into(),
                        size_bytes: 0,
                        storage_url: url.to_string(),
                        uploaded_by: actor_id,
                        uploaded_at: now,
                    }
                })
                .collect();
            CellValue::Files(attachments)
        }
        (ColumnPropertyType::LastModified, _) => CellValue::LastModified(now),
        _ => CellValue::Text(raw.trim().to_string()),
    }
}

/// Strips Markdown formatting via `pulldown-cmark` and removes raw HTML tags for `UnformattedRawText` imports.
pub fn strip_markdown_and_html(input: &str) -> String {
    let parser = Parser::new(input);
    let mut plain = String::with_capacity(input.len());

    for event in parser {
        match event {
            Event::Text(t) | Event::Code(t) => plain.push_str(&t),
            Event::SoftBreak | Event::HardBreak => plain.push(' '),
            _ => {}
        }
    }

    // Strip any inline HTML tags `<...>`
    let mut out = String::with_capacity(plain.len());
    let mut inside_tag = false;
    for ch in plain.chars() {
        match ch {
            '<' => inside_tag = true,
            '>' => inside_tag = false,
            _ if !inside_tag => out.push(ch),
            _ => {}
        }
    }

    out.trim().to_string()
}

