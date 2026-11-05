use crate::auth::{
    record_audit_log, resolve_grid_access_cached, resolve_space_access_cached, AppState, AuthError,
    AuthUser, RequireSuperAdmin,
};
use axum::{extract::State, Json};
use chrono::{NaiveDate, NaiveTime, Utc};
use pyra_ocean_shared::{
    compute_column_calculation, format_cell_for_export, format_number_with_commas, AiAttachment,
    AiCopilotRequest, AiCopilotResponse, AiExecutedAction, AiSettings, CalcMode, CellValue,
    ChecklistItem, ColumnCalcSummary, ColumnPropertyConfig, ColumnPropertyType, GridColumn,
    GridRow, NumberFormat, PageGrid, RowCellMap, SelectOption, SystemRole, TagColor,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::{timeout, Duration};
use uuid::Uuid;

// ============================================================================
// 1. Admin & Status Endpoints for Local AI Copilot (`PyraAI`)
// ============================================================================

pub async fn get_ai_settings_internal(db: &sqlx::PgPool) -> Result<AiSettings, AuthError> {
    let row: Option<(bool, String, String, String)> = sqlx::query_as(
        "SELECT ai_enabled, ollama_base_url, model_name, system_prompt FROM system_ai_settings WHERE id = 1",
    )
    .fetch_optional(db)
    .await?;

    match row {
        Some((ai_enabled, ollama_base_url, model_name, system_prompt)) => Ok(AiSettings {
            ai_enabled,
            ollama_base_url,
            model_name,
            system_prompt,
        }),
        None => Ok(AiSettings::default()),
    }
}

pub async fn get_ai_settings(
    State(state): State<AppState>,
    RequireSuperAdmin(_admin): RequireSuperAdmin,
) -> Result<Json<AiSettings>, AuthError> {
    let settings = get_ai_settings_internal(&state.db).await?;
    Ok(Json(settings))
}

pub async fn get_ai_status(
    State(state): State<AppState>,
    _user: AuthUser,
) -> Result<Json<AiSettings>, AuthError> {
    let settings = get_ai_settings_internal(&state.db).await?;
    Ok(Json(settings))
}

pub async fn update_ai_settings(
    State(state): State<AppState>,
    RequireSuperAdmin(admin): RequireSuperAdmin,
    Json(payload): Json<AiSettings>,
) -> Result<Json<AiSettings>, AuthError> {
    let clean_url = payload
        .ollama_base_url
        .trim()
        .trim_end_matches('/')
        .to_string();
    let clean_model = if payload.model_name.trim().is_empty() {
        "qwen2.5:1.5b".to_string()
    } else {
        payload.model_name.trim().to_string()
    };
    let clean_prompt = if payload.system_prompt.trim().is_empty() {
        AiSettings::default().system_prompt
    } else {
        payload.system_prompt.trim().to_string()
    };

    sqlx::query(
        r#"
        INSERT INTO system_ai_settings (id, ai_enabled, ollama_base_url, model_name, system_prompt, updated_at)
        VALUES (1, $1, $2, $3, $4, NOW())
        ON CONFLICT (id) DO UPDATE SET
            ai_enabled = EXCLUDED.ai_enabled,
            ollama_base_url = EXCLUDED.ollama_base_url,
            model_name = EXCLUDED.model_name,
            system_prompt = EXCLUDED.system_prompt,
            updated_at = NOW()
        "#,
    )
    .bind(payload.ai_enabled)
    .bind(&clean_url)
    .bind(&clean_model)
    .bind(&clean_prompt)
    .execute(&state.db)
    .await?;

    let _ = record_audit_log(
        &state.db,
        admin.user_id,
        None,
        None,
        "admin.update_ai_settings",
        json!({
            "ai_enabled": payload.ai_enabled,
            "ollama_base_url": clean_url,
            "model_name": clean_model,
        }),
    )
    .await;

    Ok(Json(AiSettings {
        ai_enabled: payload.ai_enabled,
        ollama_base_url: clean_url,
        model_name: clean_model,
        system_prompt: clean_prompt,
    }))
}

// ============================================================================
// 2. Pure-Tokio Async HTTP/1.1 Client for Local Ollama (`/api/chat`)
// ============================================================================

#[derive(Debug, Clone, Serialize)]
struct OllamaMessage {
    role: String,
    content: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    images: Vec<String>,
}

#[derive(Debug, Serialize)]
struct OllamaChatRequest {
    model: String,
    messages: Vec<OllamaMessage>,
    stream: bool,
    keep_alive: i32,
    options: Value,
}

#[derive(Debug, Deserialize)]
struct OllamaChatResponse {
    message: Option<OllamaResponseMessage>,
    response: Option<String>,
}

#[derive(Debug, Deserialize)]
struct OllamaResponseMessage {
    content: String,
}

async fn call_ollama_chat(
    base_url: &str,
    model: &str,
    messages: Vec<OllamaMessage>,
) -> Result<String, String> {
    let mut candidate_urls: Vec<String> = Vec::new();
    let primary = base_url.trim().trim_end_matches('/').to_string();
    if !primary.is_empty() {
        candidate_urls.push(primary);
    }
    for fallback in [
        "http://10.10.10.4:11434",
        "http://pyraocean-ai:11434",
        "http://host.docker.internal:11434",
    ] {
        if !candidate_urls
            .iter()
            .any(|u| u.eq_ignore_ascii_case(fallback))
        {
            candidate_urls.push(fallback.to_string());
        }
    }

    let req_body = serde_json::to_string(&OllamaChatRequest {
        model: model.to_string(),
        messages,
        stream: false,
        keep_alive: -1,
        options: json!({
            "temperature": 0.1,
            "num_ctx": 4096,
            "num_predict": 600
        }),
    })
    .map_err(|e| format!("Failed to serialize Ollama request: {e}"))?;

    let mut errors: Vec<String> = Vec::new();

    for candidate in &candidate_urls {
        match call_single_ollama_endpoint(candidate, &req_body).await {
            Ok(reply) => return Ok(reply),
            Err(err) => {
                errors.push(format!("{candidate}: {err}"));
            }
        }
    }

    Err(errors.join(" | "))
}

async fn call_single_ollama_endpoint(base_url: &str, req_body: &str) -> Result<String, String> {
    let stripped = base_url
        .trim()
        .strip_prefix("http://")
        .or_else(|| base_url.trim().strip_prefix("https://"))
        .unwrap_or(base_url.trim());
    let (host_port, base_path) = match stripped.find('/') {
        Some(idx) => (&stripped[..idx], &stripped[idx..]),
        None => (stripped, ""),
    };
    let addr = if host_port.contains(':') {
        host_port.to_string()
    } else {
        format!("{host_port}:11434")
    };
    let endpoint_path = format!("{}/api/chat", base_path.trim_end_matches('/'));

    let mut stream = timeout(Duration::from_secs(6), TcpStream::connect(&addr))
        .await
        .map_err(|_| format!("Connection timed out to Ollama at {addr}"))?
        .map_err(|e| format!("Failed to connect to Ollama at {addr}: {e}"))?;

    let http_req = format!(
        "POST {endpoint_path} HTTP/1.1\r\nHost: {host_port}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        req_body.len(),
        req_body
    );

    timeout(Duration::from_secs(15), stream.write_all(http_req.as_bytes()))
        .await
        .map_err(|_| "Timed out writing request to Ollama".to_string())?
        .map_err(|e| format!("Write error to Ollama: {e}"))?;

    let mut raw_resp = Vec::new();
    timeout(Duration::from_secs(120), stream.read_to_end(&mut raw_resp))
        .await
        .map_err(|_| "Timed out waiting for Ollama inference response (120s)".to_string())?
        .map_err(|e| format!("Read error from Ollama: {e}"))?;

    let resp_str = String::from_utf8_lossy(&raw_resp);
    let (headers_part, body_part) = resp_str
        .split_once("\r\n\r\n")
        .ok_or_else(|| "Malformed HTTP response from Ollama".to_string())?;

    let is_chunked = headers_part
        .lines()
        .any(|l| l.to_ascii_lowercase().contains("transfer-encoding: chunked"));

    let decoded_body = if is_chunked {
        decode_chunked_body(body_part)
    } else {
        body_part.trim().to_string()
    };

    if let Ok(parsed) = serde_json::from_str::<OllamaChatResponse>(&decoded_body) {
        if let Some(msg) = parsed.message {
            if !msg.content.trim().is_empty() {
                return Ok(msg.content.trim().to_string());
            }
        }
        if let Some(resp) = parsed.response {
            if !resp.trim().is_empty() {
                return Ok(resp.trim().to_string());
            }
        }
    }

    if let Some(start) = decoded_body.find('{') {
        if let Some(end) = decoded_body.rfind('}') {
            if end >= start {
                let slice = &decoded_body[start..=end];
                if let Ok(parsed) = serde_json::from_str::<OllamaChatResponse>(slice) {
                    if let Some(msg) = parsed.message {
                        return Ok(msg.content.trim().to_string());
                    }
                }
            }
        }
    }

    Err(format!(
        "Unexpected Ollama response format: {}",
        decoded_body.chars().take(240).collect::<String>()
    ))
}

fn decode_chunked_body(raw: &str) -> String {
    let mut out = String::new();
    let mut rest = raw;
    while let Some((len_line, after_len)) = rest.split_once("\r\n") {
        let len_hex = len_line.trim().split(';').next().unwrap_or("");
        let Ok(chunk_len) = usize::from_str_radix(len_hex, 16) else {
            out.push_str(rest);
            break;
        };
        if chunk_len == 0 {
            break;
        }
        if after_len.len() >= chunk_len {
            out.push_str(&after_len[..chunk_len]);
            rest = &after_len[chunk_len..];
            if let Some(stripped) = rest.strip_prefix("\r\n") {
                rest = stripped;
            }
        } else {
            out.push_str(after_len);
            break;
        }
    }
    if out.is_empty() {
        raw.trim().to_string()
    } else {
        out
    }
}

fn strip_data_url_prefix(raw: &str) -> &str {
    let trimmed = raw.trim();
    if let Some((_, b64)) = trimmed.split_once(";base64,") {
        b64.trim()
    } else if let Some( idx) = trimmed.find(",") {
        if trimmed[..idx].starts_with("data:") {
            trimmed[idx + 1..].trim()
        } else {
            trimmed
        }
    } else {
        trimmed
    }
}

fn decode_base64_bytes(b64_raw: &str) -> Vec<u8> {
    let clean = strip_data_url_prefix(b64_raw);
    let mut out = Vec::with_capacity(clean.len() * 3 / 4);
    let mut buf: u32 = 0;
    let mut bits: u8 = 0;
    for b in clean.bytes() {
        let val = match b {
            b'A'..=b'Z' => b - b'A',
            b'a'..=b'z' => b - b'a' + 26,
            b'0'..=b'9' => b - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            b'=' => break,
            _ => continue,
        };
        buf = (buf << 6) | (val as u32);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push(((buf >> bits) & 0xFF) as u8);
        }
    }
    out
}

fn is_text_like_attachment(att: &AiAttachment) -> bool {
    let mime = att.mime_type.to_ascii_lowercase();
    let fname = att.filename.to_ascii_lowercase();
    mime.starts_with("text/")
        || mime.contains("json")
        || mime.contains("csv")
        || mime.contains("xml")
        || mime.contains("yaml")
        || fname.ends_with(".csv")
        || fname.ends_with(".tsv")
        || fname.ends_with(".txt")
        || fname.ends_with(".md")
        || fname.ends_with(".json")
        || fname.ends_with(".log")
}

/// Extracts `(ollama_base64_images, attachments_context_block, parsed_tabular_rows)` from a slice of `AiAttachment`s.
fn process_ai_attachments(
    attachments: &[AiAttachment],
    active_columns: &[GridColumn],
) -> (Vec<String>, String, Vec<HashMap<String, Value>>) {
    let mut images: Vec<String> = Vec::new();
    let mut text_sections: Vec<String> = Vec::new();
    let mut tabular_rows: Vec<HashMap<String, Value>> = Vec::new();

    for att in attachments {
        let clean_b64 = strip_data_url_prefix(&att.data_base64).to_string();
        if att.mime_type.to_ascii_lowercase().starts_with("image/") {
            if !clean_b64.is_empty() {
                images.push(clean_b64);
            }
            text_sections.push(format!(
                "- Attached Image: `{}` (`{}`)",
                att.filename, att.mime_type
            ));
        } else if is_text_like_attachment(att) {
            let bytes = decode_base64_bytes(&att.data_base64);
            let text_content = String::from_utf8_lossy(&bytes).to_string();
            let preview: String = text_content.chars().take(6000).collect();
            text_sections.push(format!(
                "--- Attached File: `{}` (`{}`) ---\n{}\n--- End of `{}` ---",
                att.filename, att.mime_type, preview, att.filename
            ));

            // Also attempt to parse CSV/TSV/JSON rows so if the user asks to import/add rows from the file, we can execute immediately!
            let fname_lower = att.filename.to_ascii_lowercase();
            if fname_lower.ends_with(".json") || att.mime_type.contains("json") {
                if let Ok(Value::Array(arr)) = serde_json::from_str::<Value>(&text_content) {
                    for item in arr.into_iter().take(100) {
                        if let Value::Object(map) = item {
                            let mut row_map = HashMap::new();
                            for (k, v) in map {
                                row_map.insert(k, v);
                            }
                            if !row_map.is_empty() {
                                tabular_rows.push(row_map);
                            }
                        }
                    }
                }
            } else if fname_lower.ends_with(".csv")
                || fname_lower.ends_with(".tsv")
                || att.mime_type.contains("csv")
                || text_content.lines().count() >= 2
            {
                let delim = if fname_lower.ends_with(".tsv") || text_content.contains('\t') {
                    '\t'
                } else {
                    ','
                };
                let mut lines = text_content
                    .lines()
                    .map(|l| l.trim())
                    .filter(|l| !l.is_empty());
                if let Some(header_line) = lines.next() {
                    let headers: Vec<String> = header_line
                        .split(delim)
                        .map(|h| h.trim().trim_matches(['"', '\'']).to_string())
                        .collect();
                    let matches_any_col = headers.iter().any(|h| {
                        active_columns
                            .iter()
                            .any(|c| c.name.eq_ignore_ascii_case(h))
                    });
                    if headers.len() >= 2
                        || matches_any_col
                        || fname_lower.ends_with(".csv")
                        || fname_lower.ends_with(".tsv")
                    {
                        for line in lines.take(100) {
                            let cols: Vec<&str> = line.split(delim).collect();
                            let mut row_map = HashMap::new();
                            for (idx, h) in headers.iter().enumerate() {
                                if h.is_empty() {
                                    continue;
                                }
                                if let Some(raw_val) = cols.get(idx) {
                                    let clean_v = raw_val.trim().trim_matches(['"', '\'']);
                                    if !clean_v.is_empty() {
                                        row_map.insert(
                                            h.clone(),
                                            Value::String(clean_v.to_string()),
                                        );
                                    }
                                }
                            }
                            if !row_map.is_empty() {
                                tabular_rows.push(row_map);
                            }
                        }
                    }
                }
            }
        } else {
            let approx_kb = (clean_b64.len() * 3 / 4) / 1024;
            text_sections.push(format!(
                "- Attached Binary File: `{}` (`{}`, ~{} KB)",
                att.filename, att.mime_type, approx_kb
            ));
        }
    }

    let block = if text_sections.is_empty() {
        String::new()
    } else {
        format!(
            "\nUSER ATTACHED FILES & IMAGES ({}):\n{}\n",
            attachments.len(),
            text_sections.join("\n")
        )
    };

    (images, block, tabular_rows)
}

// ============================================================================
// 3. Autonomous Task Spec & Cell Parsing Helpers
// ============================================================================

#[derive(Debug, Clone, Deserialize)]
struct LLMCommandEnvelope {
    #[serde(default)]
    explanation: Option<String>,
    #[serde(default)]
    actions: Vec<LLMTaskSpec>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
enum LLMTaskSpec {
    CreateGrid {
        name: String,
        #[serde(default)]
        icon: Option<String>,
        #[serde(default)]
        columns: Vec<LLMColumnSpec>,
        #[serde(default)]
        rows: Vec<HashMap<String, Value>>,
    },
    AddColumn {
        name: String,
        #[serde(default = "default_text_type")]
        property_type: String,
    },
    AddRow {
        #[serde(default)]
        values: HashMap<String, Value>,
    },
    UpdateRow {
        #[serde(default)]
        match_text: String,
        #[serde(default)]
        values: HashMap<String, Value>,
        #[serde(default)]
        append_tags: bool,
        #[serde(default)]
        remove_tags: bool,
    },
    DeleteRow {
        #[serde(default)]
        match_text: String,
    },
}

fn default_text_type() -> String {
    "text".to_string()
}

#[derive(Debug, Clone, Deserialize)]
struct LLMColumnSpec {
    name: String,
    #[serde(default = "default_text_type")]
    property_type: String,
}

fn format_calc_summary_plain(summary: &ColumnCalcSummary) -> String {
    if summary.is_empty() {
        return String::new();
    }
    if let Some(txt) = &summary.text_summary {
        return txt.clone();
    }
    let mut parts: Vec<String> = Vec::new();
    if let Some(egp) = summary.egp_value {
        parts.push(format!("EGP {}", format_number_with_commas(egp, 2)));
    }
    if let Some(usd) = summary.usd_value {
        parts.push(format!("$ {}", format_number_with_commas(usd, 2)));
    }
    if let Some(pct) = summary.percent_value {
        parts.push(format!("{pct:.2}%"));
    }
    if let Some(plain) = summary.plain_value {
        parts.push(format_number_with_commas(plain, 2));
    }
    parts.join(" + ")
}

fn parse_property_type_and_config(raw: &str) -> (ColumnPropertyType, ColumnPropertyConfig) {
    let lower = raw.trim().to_ascii_lowercase();
    match lower.as_str() {
        "number" | "numeric" | "float" | "int" => (
            ColumnPropertyType::Number,
            ColumnPropertyConfig::Number {
                format: NumberFormat::Decimal { precision: 2 },
            },
        ),
        "currency" | "money" | "cost" | "price" | "budget" | "amount" | "egp" => (
            ColumnPropertyType::Number,
            ColumnPropertyConfig::Number {
                format: NumberFormat::Currency {
                    symbol: "EGP".to_string(),
                    precision: 2,
                },
            },
        ),
        "usd" => (
            ColumnPropertyType::Number,
            ColumnPropertyConfig::Number {
                format: NumberFormat::Currency {
                    symbol: "$".to_string(),
                    precision: 2,
                },
            },
        ),
        "percent" | "percentage" => (
            ColumnPropertyType::Number,
            ColumnPropertyConfig::Number {
                format: NumberFormat::Percentage { precision: 2 },
            },
        ),
        "select" | "status" | "category" | "department" => (
            ColumnPropertyType::Select,
            ColumnPropertyConfig::Select {
                options: vec![
                    SelectOption {
                        id: Uuid::new_v4(),
                        label: "Todo".into(),
                        color: TagColor::Slate,
                    },
                    SelectOption {
                        id: Uuid::new_v4(),
                        label: "In Progress".into(),
                        color: TagColor::Blue,
                    },
                    SelectOption {
                        id: Uuid::new_v4(),
                        label: "Done".into(),
                        color: TagColor::Emerald,
                    },
                ],
            },
        ),
        "multi_select" | "multiselect" | "tags" => (
            ColumnPropertyType::MultiSelect,
            ColumnPropertyConfig::MultiSelect {
                options: Vec::new(),
            },
        ),
        "date" | "deadline" | "due_date" => (
            ColumnPropertyType::Date,
            ColumnPropertyConfig::Date {
                display_format: "%Y-%m-%d".into(),
                include_time: false,
            },
        ),
        "time" | "duration" => (
            ColumnPropertyType::Time,
            ColumnPropertyConfig::Time {
                use_24_hour: true,
                is_duration: false,
            },
        ),
        "checkbox" | "bool" | "boolean" | "done" => {
            (ColumnPropertyType::Checkbox, ColumnPropertyConfig::Checkbox)
        }
        "checklist" | "tasks" => (
            ColumnPropertyType::Checklist,
            ColumnPropertyConfig::Checklist {
                show_progress_bar: true,
            },
        ),
        "person" | "owner" | "assignee" => (
            ColumnPropertyType::Person,
            ColumnPropertyConfig::Person {
                allow_multiple: true,
            },
        ),
        "password" | "secret" => (ColumnPropertyType::Password, ColumnPropertyConfig::Password),
        _ => (
            ColumnPropertyType::Text,
            ColumnPropertyConfig::Text {
                rich_text: false,
                max_length: None,
            },
        ),
    }
}

const AI_TAG_PALETTE: [TagColor; 8] = [
    TagColor::Cyan,
    TagColor::Blue,
    TagColor::Emerald,
    TagColor::Purple,
    TagColor::Orange,
    TagColor::Pink,
    TagColor::Teal,
    TagColor::Amber,
];

/// Resolves any raw JSON cell value (`serde_json::Value`, whether an externally-tagged `CellValue`
/// like `{"Select": "uuid"}` / `{"MultiSelect": ["uuid1", "uuid2"]}` or an untyped string/array/object)
/// into its human-readable display text using `col.property_config`.
pub fn resolve_cell_display_text(col: &GridColumn, val: &Value) -> String {
    let config_json = serde_json::to_value(&col.property_config).unwrap_or(Value::Null);
    let mut option_map: HashMap<String, String> = HashMap::new();
    if let Some(opts) = config_json.get("options").and_then(|o| o.as_array()) {
        for opt in opts {
            let label = opt
                .get("label")
                .or_else(|| opt.get("name"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .trim();
            if !label.is_empty() {
                option_map.insert(label.to_ascii_lowercase(), label.to_string());
                if let Some(id_str) = opt.get("id").and_then(|v| v.as_str()) {
                    option_map.insert(id_str.trim().to_ascii_lowercase(), label.to_string());
                }
            }
        }
    }

    // Unwrap externally-tagged `CellValue` enum wrapper if present (e.g. `{"Select": ...}`)
    let inner: &Value = if let Value::Object(map) = val {
        if map.len() == 1 {
            if let Some((k, v)) = map.iter().next() {
                if matches!(
                    k.as_str(),
                    "Text"
                        | "Number"
                        | "Select"
                        | "MultiSelect"
                        | "Date"
                        | "Time"
                        | "Files"
                        | "Checkbox"
                        | "Checklist"
                        | "LastModified"
                        | "Person"
                        | "Password"
                ) {
                    v
                } else {
                    val
                }
            } else {
                val
            }
        } else {
            val
        }
    } else {
        val
    };

    match col.property_type {
        ColumnPropertyType::Select => match inner {
            Value::Null => String::new(),
            Value::String(s) => {
                let clean = s.trim();
                if clean.is_empty() {
                    String::new()
                } else {
                    option_map
                        .get(&clean.to_ascii_lowercase())
                        .cloned()
                        .unwrap_or_else(|| clean.to_string())
                }
            }
            Value::Object(obj) => {
                if let Some(lbl) = obj
                    .get("label")
                    .or_else(|| obj.get("name"))
                    .and_then(|v| v.as_str())
                {
                    lbl.trim().to_string()
                } else if let Some(id_str) = obj.get("id").and_then(|v| v.as_str()) {
                    option_map
                        .get(&id_str.trim().to_ascii_lowercase())
                        .cloned()
                        .unwrap_or_else(|| id_str.trim().to_string())
                } else {
                    String::new()
                }
            }
            Value::Array(arr) => arr
                .first()
                .map(|first| resolve_cell_display_text(col, first))
                .unwrap_or_default(),
            _ => String::new(),
        },
        ColumnPropertyType::MultiSelect => match inner {
            Value::Null => String::new(),
            Value::Array(items) => {
                let mut labels: Vec<String> = Vec::new();
                for item in items {
                    let resolved = match item {
                        Value::String(s) => {
                            let clean = s.trim();
                            if clean.is_empty() {
                                String::new()
                            } else {
                                option_map
                                    .get(&clean.to_ascii_lowercase())
                                    .cloned()
                                    .unwrap_or_else(|| clean.to_string())
                            }
                        }
                        Value::Object(obj) => obj
                            .get("label")
                            .or_else(|| obj.get("name"))
                            .and_then(|v| v.as_str())
                            .map(|s| s.trim().to_string())
                            .or_else(|| {
                                obj.get("id").and_then(|v| v.as_str()).map(|id_str| {
                                    option_map
                                        .get(&id_str.trim().to_ascii_lowercase())
                                        .cloned()
                                        .unwrap_or_else(|| id_str.trim().to_string())
                                })
                            })
                            .unwrap_or_default(),
                        _ => String::new(),
                    };
                    if !resolved.is_empty() && !labels.contains(&resolved) {
                        labels.push(resolved);
                    }
                }
                labels.join(", ")
            }
            Value::String(s) => {
                let labels: Vec<String> = s
                    .split([',', ';'])
                    .map(|part| part.trim())
                    .filter(|part| !part.is_empty())
                    .map(|part| {
                        option_map
                            .get(&part.to_ascii_lowercase())
                            .cloned()
                            .unwrap_or_else(|| part.to_string())
                    })
                    .collect();
                labels.join(", ")
            }
            _ => String::new(),
        },
        ColumnPropertyType::Number => match inner {
            Value::Number(n) => {
                if let Some(f) = n.as_f64() {
                    match &col.property_config {
                        ColumnPropertyConfig::Number {
                            format: NumberFormat::Currency { symbol, precision },
                        } => format!(
                            "{} {}",
                            symbol,
                            format_number_with_commas(f, *precision as usize)
                        ),
                        ColumnPropertyConfig::Number {
                            format: NumberFormat::Percentage { precision },
                        } => format!("{:.prec$}%", f, prec = *precision as usize),
                        ColumnPropertyConfig::Number {
                            format: NumberFormat::Decimal { precision },
                        } => format_number_with_commas(f, *precision as usize),
                        _ => format_number_with_commas(f, 2),
                    }
                } else {
                    n.to_string()
                }
            }
            Value::String(s) => s.trim().to_string(),
            _ => String::new(),
        },
        ColumnPropertyType::Checkbox => match inner {
            Value::Bool(b) => {
                if *b {
                    "Checked (true)".to_string()
                } else {
                    "Unchecked (false)".to_string()
                }
            }
            Value::String(s) => s.trim().to_string(),
            _ => String::new(),
        },
        ColumnPropertyType::Checklist => match inner {
            Value::Array(items) => items
                .iter()
                .filter_map(|it| {
                    let title = it
                        .get("item")
                        .or_else(|| it.get("title"))
                        .and_then(|v| v.as_str())?;
                    let done = it
                        .get("completed")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);
                    Some(format!(
                        "{} {}",
                        if done { "[x]" } else { "[ ]" },
                        title.trim()
                    ))
                })
                .collect::<Vec<_>>()
                .join(", "),
            Value::String(s) => s.trim().to_string(),
            _ => String::new(),
        },
        ColumnPropertyType::Files => match inner {
            Value::Array(files) => files
                .iter()
                .filter_map(|f| {
                    f.get("filename")
                        .or_else(|| f.get("name"))
                        .and_then(|v| v.as_str())
                        .map(|s| s.trim().to_string())
                })
                .collect::<Vec<_>>()
                .join(", "),
            Value::String(s) => s.trim().to_string(),
            _ => String::new(),
        },
        ColumnPropertyType::Time => match inner {
            Value::String(s) => s.trim().to_string(),
            Value::Object(obj) => obj
                .get("time_of_day")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .trim()
                .to_string(),
            _ => String::new(),
        },
        _ => match inner {
            Value::String(s) => s.trim().to_string(),
            Value::Number(n) => n.to_string(),
            Value::Bool(b) => b.to_string(),
            _ => String::new(),
        },
    }
}

/// Resolves a typed `Option<&CellValue>` using `resolve_cell_display_text` plus `user_display_map` for `Person` cells.
fn resolve_row_cell_display(
    col: &GridColumn,
    cell: Option<&CellValue>,
    user_display_map: &HashMap<Uuid, String>,
    row_last_modified: chrono::DateTime<Utc>,
) -> String {
    let Some(c) = cell else {
        if col.property_type == ColumnPropertyType::LastModified {
            return row_last_modified.format("%Y-%m-%d %H:%M UTC").to_string();
        }
        return String::new();
    };
    if let CellValue::Person(uids) = c {
        return uids
            .iter()
            .map(|u| {
                user_display_map
                    .get(u)
                    .cloned()
                    .unwrap_or_else(|| u.to_string())
            })
            .collect::<Vec<_>>()
            .join(", ");
    }
    if let Ok(val) = serde_json::to_value(c) {
        let resolved = resolve_cell_display_text(col, &val);
        if !resolved.is_empty() {
            return resolved;
        }
    }
    format_cell_for_export(cell, col, user_display_map, row_last_modified, true)
}

/// Returns the list of configured option labels for a `Select` or `MultiSelect` column.
fn get_column_option_labels(col: &GridColumn) -> Vec<String> {
    match &col.property_config {
        ColumnPropertyConfig::Select { options }
        | ColumnPropertyConfig::MultiSelect { options } => {
            options.iter().map(|o| o.label.clone()).collect()
        }
        _ => Vec::new(),
    }
}

/// Splits a raw `MultiSelect` input (`Value::Array` or comma/semicolon/`+`/` and ` separated string)
/// into individual normalized tag labels.
fn extract_multiselect_tag_tokens(raw_input: &Value) -> Vec<String> {
    let mut raw_parts: Vec<String> = Vec::new();
    match raw_input {
        Value::Array(arr) => {
            for item in arr {
                if let Some(s) = item.as_str() {
                    raw_parts.push(s.to_string());
                } else if let Some(lbl) = item
                    .get("label")
                    .or_else(|| item.get("name"))
                    .and_then(|v| v.as_str())
                {
                    raw_parts.push(lbl.to_string());
                }
            }
        }
        Value::String(s) => {
            // Normalize " and " or "+" when used as tag separators
            let normalized = s.replace(" + ", ",");
            for part in normalized.split([',', ';']) {
                raw_parts.push(part.to_string());
            }
        }
        Value::Object(obj) => {
            if let Some(lbl) = obj
                .get("label")
                .or_else(|| obj.get("name"))
                .and_then(|v| v.as_str())
            {
                raw_parts.push(lbl.to_string());
            }
        }
        _ => {}
    }

    let mut out: Vec<String> = Vec::new();
    for part in raw_parts {
        let clean = part
            .trim()
            .trim_matches(['"', '\'', '[', ']'])
            .trim()
            .to_string();
        if !clean.is_empty()
            && !out
                .iter()
                .any(|existing| existing.eq_ignore_ascii_case(&clean))
        {
            out.push(clean);
        }
    }
    out
}

/// Async helper that prepares a `CellValue` for `Select`, `MultiSelect`, or any of the 12 property types,
/// auto-creating missing `SelectOption` tags in `col.property_config` and persisting them to PostgreSQL
/// (`UPDATE grid_columns SET property_config = $1, updated_at = NOW() WHERE id = $2`) so `cell_editor.rs`
/// renders the colored option badges immediately.
pub async fn prepare_cell_value_and_sync_options(
    pool: &sqlx::PgPool,
    col: &mut GridColumn,
    raw_input: &Value,
) -> Value {
    let cell =
        prepare_typed_cell_value_and_sync_options(pool, col, raw_input, None, false, false).await;
    serde_json::to_value(cell).unwrap_or(Value::Null)
}

async fn prepare_typed_cell_value_and_sync_options(
    pool: &sqlx::PgPool,
    col: &mut GridColumn,
    raw_input: &Value,
    existing_cell: Option<&CellValue>,
    append_tags: bool,
    remove_tags: bool,
) -> CellValue {
    let mut config_modified = false;

    match col.property_type {
        ColumnPropertyType::Select => {
            let input_str = match raw_input {
                Value::String(s) => s.trim().trim_matches(['"', '\'']).trim().to_string(),
                Value::Array(arr) => arr
                    .first()
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .trim()
                    .to_string(),
                Value::Object(obj) => obj
                    .get("label")
                    .or_else(|| obj.get("name"))
                    .or_else(|| obj.get("id"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .trim()
                    .to_string(),
                _ => String::new(),
            };

            if input_str.is_empty() {
                return CellValue::Select(None);
            }

            if !matches!(col.property_config, ColumnPropertyConfig::Select { .. }) {
                col.property_config = ColumnPropertyConfig::Select {
                    options: Vec::new(),
                };
                config_modified = true;
            }

            let selected_id =
                if let ColumnPropertyConfig::Select { options } = &mut col.property_config {
                    if let Some(existing) = options.iter().find(|o| {
                        o.label.eq_ignore_ascii_case(&input_str)
                            || o.id.to_string().eq_ignore_ascii_case(&input_str)
                    }) {
                        Some(existing.id)
                    } else {
                        let next_color = AI_TAG_PALETTE[options.len() % AI_TAG_PALETTE.len()];
                        let new_opt = SelectOption {
                            id: Uuid::new_v4(),
                            label: input_str,
                            color: next_color,
                        };
                        let id = new_opt.id;
                        options.push(new_opt);
                        config_modified = true;
                        Some(id)
                    }
                } else {
                    None
                };

            if config_modified {
                let _ = sqlx::query(
                    "UPDATE grid_columns SET property_config = $1, updated_at = NOW() WHERE id = $2",
                )
                .bind(sqlx::types::Json(&col.property_config))
                .bind(col.id)
                .execute(pool)
                .await;
            }

            CellValue::Select(selected_id)
        }

        ColumnPropertyType::MultiSelect => {
            let tokens = extract_multiselect_tag_tokens(raw_input);

            if !matches!(
                col.property_config,
                ColumnPropertyConfig::MultiSelect { .. }
            ) {
                col.property_config = ColumnPropertyConfig::MultiSelect {
                    options: Vec::new(),
                };
                config_modified = true;
            }

            let mut resolved_ids: Vec<Uuid> = Vec::new();
            if let ColumnPropertyConfig::MultiSelect { options } = &mut col.property_config {
                for tok in tokens {
                    if let Some(existing) = options.iter().find(|o| {
                        o.label.eq_ignore_ascii_case(&tok)
                            || o.id.to_string().eq_ignore_ascii_case(&tok)
                    }) {
                        if !resolved_ids.contains(&existing.id) {
                            resolved_ids.push(existing.id);
                        }
                    } else if !remove_tags {
                        let next_color = AI_TAG_PALETTE[options.len() % AI_TAG_PALETTE.len()];
                        let new_opt = SelectOption {
                            id: Uuid::new_v4(),
                            label: tok,
                            color: next_color,
                        };
                        resolved_ids.push(new_opt.id);
                        options.push(new_opt);
                        config_modified = true;
                    }
                }
            }

            if config_modified {
                let _ = sqlx::query(
                    "UPDATE grid_columns SET property_config = $1, updated_at = NOW() WHERE id = $2",
                )
                .bind(sqlx::types::Json(&col.property_config))
                .bind(col.id)
                .execute(pool)
                .await;
            }

            let final_ids = if append_tags {
                let mut current = match existing_cell {
                    Some(CellValue::MultiSelect(existing_ids)) => existing_ids.clone(),
                    _ => Vec::new(),
                };
                for id in resolved_ids {
                    if !current.contains(&id) {
                        current.push(id);
                    }
                }
                current
            } else if remove_tags {
                let mut current = match existing_cell {
                    Some(CellValue::MultiSelect(existing_ids)) => existing_ids.clone(),
                    _ => Vec::new(),
                };
                current.retain(|id| !resolved_ids.contains(id));
                current
            } else {
                resolved_ids
            };

            CellValue::MultiSelect(final_ids)
        }

        _ => {
            let (cell, cfg_changed) = json_value_to_cell_value(col, raw_input);
            if cfg_changed {
                let _ = sqlx::query(
                    "UPDATE grid_columns SET property_config = $1, updated_at = NOW() WHERE id = $2",
                )
                .bind(sqlx::types::Json(&col.property_config))
                .bind(col.id)
                .execute(pool)
                .await;
            }
            cell
        }
    }
}

fn json_value_to_cell_value(col: &mut GridColumn, raw: &Value) -> (CellValue, bool) {
    let mut config_modified = false;
    let as_str = match raw {
        Value::String(s) => s.trim().to_string(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Array(arr) => arr
            .iter()
            .filter_map(|v| v.as_str().map(|s| s.to_string()))
            .collect::<Vec<_>>()
            .join(", "),
        _ => String::new(),
    };

    let cell = match col.property_type {
        ColumnPropertyType::Text => CellValue::Text(as_str),
        ColumnPropertyType::Password => CellValue::Password(as_str),
        ColumnPropertyType::Number => {
            let num = raw.as_f64().or_else(|| {
                let cleaned: String = as_str
                    .chars()
                    .filter(|c| c.is_ascii_digit() || *c == '.' || *c == '-')
                    .collect();
                cleaned.parse::<f64>().ok()
            });
            CellValue::Number(num.filter(|v| v.is_finite()).unwrap_or(0.0))
        }
        ColumnPropertyType::Select => {
            if as_str.is_empty() {
                CellValue::Select(None)
            } else if let ColumnPropertyConfig::Select { options } = &mut col.property_config {
                if let Some(existing) = options
                    .iter()
                    .find(|o| o.label.eq_ignore_ascii_case(&as_str))
                {
                    CellValue::Select(Some(existing.id))
                } else {
                    let new_opt = SelectOption {
                        id: Uuid::new_v4(),
                        label: as_str,
                        color: TagColor::Cyan,
                    };
                    let new_id = new_opt.id;
                    options.push(new_opt);
                    config_modified = true;
                    CellValue::Select(Some(new_id))
                }
            } else {
                CellValue::Select(None)
            }
        }
        ColumnPropertyType::MultiSelect => {
            let tokens: Vec<String> = as_str
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
            let mut ids = Vec::new();
            if let ColumnPropertyConfig::MultiSelect { options } = &mut col.property_config {
                for tok in tokens {
                    if let Some(existing) =
                        options.iter().find(|o| o.label.eq_ignore_ascii_case(&tok))
                    {
                        ids.push(existing.id);
                    } else {
                        let new_opt = SelectOption {
                            id: Uuid::new_v4(),
                            label: tok,
                            color: TagColor::Blue,
                        };
                        ids.push(new_opt.id);
                        options.push(new_opt);
                        config_modified = true;
                    }
                }
            }
            CellValue::MultiSelect(ids)
        }
        ColumnPropertyType::Date => {
            let parsed = NaiveDate::parse_from_str(&as_str, "%Y-%m-%d")
                .or_else(|_| NaiveDate::parse_from_str(&as_str, "%d/%m/%Y"))
                .ok();
            CellValue::Date(parsed)
        }
        ColumnPropertyType::Time => {
            let parsed = NaiveTime::parse_from_str(&as_str, "%H:%M:%S")
                .or_else(|_| NaiveTime::parse_from_str(&as_str, "%H:%M"))
                .ok();
            CellValue::Time {
                time_of_day: parsed,
                duration_seconds: None,
            }
        }
        ColumnPropertyType::Checkbox => {
            let b = raw.as_bool().unwrap_or_else(|| {
                matches!(
                    as_str.to_ascii_lowercase().as_str(),
                    "true" | "1" | "yes" | "done" | "checked"
                )
            });
            CellValue::Checkbox(b)
        }
        ColumnPropertyType::Checklist => {
            let items: Vec<ChecklistItem> = as_str
                .split(',')
                .map(|s| s.trim())
                .filter(|s| !s.is_empty())
                .map(|t| ChecklistItem {
                    id: Uuid::new_v4(),
                    item: t.to_string(),
                    completed: false,
                })
                .collect();
            CellValue::Checklist(items)
        }
        ColumnPropertyType::Files => CellValue::Files(Vec::new()),
        ColumnPropertyType::Person => CellValue::Person(Vec::new()),
        ColumnPropertyType::LastModified => CellValue::LastModified(Utc::now()),
    };

    (cell, config_modified)
}

// ============================================================================
// 4. Main Copilot Endpoint (`POST /api/ai/copilot`)
// ============================================================================

pub async fn run_ai_copilot(
    State(state): State<AppState>,
    auth_user: AuthUser,
    Json(req): Json<AiCopilotRequest>,
) -> Result<Json<AiCopilotResponse>, AuthError> {
    let settings = get_ai_settings_internal(&state.db).await?;
    if !settings.ai_enabled {
        return Err(AuthError::BadRequest(
            "PyraAI Copilot is currently disabled by the SuperAdmin.".into(),
        ));
    }

    let prompt_trimmed = req.prompt.trim();
    if prompt_trimmed.is_empty() && req.attachments.is_empty() {
        return Err(AuthError::BadRequest("Prompt cannot be empty.".into()));
    }
    let effective_prompt = if prompt_trimmed.is_empty() {
        "Analyze the attached file(s) and summarize their contents in relation to this Grid."
    } else {
        prompt_trimmed
    };

    // Build user display map for formatting Person cells
    let user_rows: Vec<(Uuid, String)> = sqlx::query_as("SELECT id, email FROM users")
        .fetch_all(&state.db)
        .await
        .unwrap_or_default();
    let user_display_map: HashMap<Uuid, String> = user_rows.into_iter().collect();

    // 1. Load Active Grid Context (if `grid_id` is provided and accessible)
    let mut active_grid: Option<PageGrid> = None;
    let mut active_columns: Vec<GridColumn> = Vec::new();
    let mut active_rows: Vec<GridRow> = Vec::new();
    let mut can_edit_active_grid = false;

    if let Some(gid) = req.grid_id {
        if let Ok(access) = resolve_grid_access_cached(&state, auth_user.clone(), gid).await {
            can_edit_active_grid = access.effective_space_role.can_write();
            active_grid = sqlx::query_as::<_, PageGrid>(
                "SELECT id, space_id, title, description, icon, position, created_by, is_archived, created_at, updated_at, deleted_at, deleted_by FROM pages_grids WHERE id = $1 AND deleted_at IS NULL",
            )
            .bind(gid)
            .fetch_optional(&state.db)
            .await?;

            if active_grid.is_some() {
                active_columns = sqlx::query_as::<_, GridColumn>(
                    "SELECT id, grid_id, name, property_type, property_config, width_px, position, is_primary, is_hidden, created_at, updated_at FROM grid_columns WHERE grid_id = $1 ORDER BY position ASC, created_at ASC",
                )
                .bind(gid)
                .fetch_all(&state.db)
                .await?;

                active_rows = sqlx::query_as::<_, GridRow>(
                    "SELECT id, grid_id, position, row_order, cells, created_by, updated_by, created_at, last_modified FROM grid_rows WHERE grid_id = $1 ORDER BY position ASC, row_order ASC, created_at ASC LIMIT 300",
                )
                .bind(gid)
                .fetch_all(&state.db)
                .await?;
            }
        }
    }

    // 2. Load Accessible Workspace Grids for Global Search & Navigation Context
    let is_super_admin = auth_user.system_role == SystemRole::SuperAdmin;
    let accessible_grids: Vec<(Uuid, Uuid, String, String)> = sqlx::query_as(
        r#"
        SELECT g.id, g.space_id, s.name AS space_name, g.title
        FROM pages_grids g
        JOIN spaces s ON s.id = g.space_id
        WHERE g.deleted_at IS NULL AND g.is_archived = FALSE AND s.is_archived = FALSE
          AND (
              $1 = TRUE
              OR EXISTS (SELECT 1 FROM space_members sm WHERE sm.space_id = g.space_id AND sm.user_id = $2)
              OR EXISTS (SELECT 1 FROM page_grid_permissions gp WHERE gp.grid_id = g.id AND gp.user_id = $2)
          )
        ORDER BY s.name ASC, g.position ASC
        LIMIT 60
        "#,
    )
    .bind(is_super_admin)
    .bind(auth_user.user_id)
    .fetch_all(&state.db)
    .await?;

    // 3. Build Deterministic Context Digest & Pre-Computed Analytics Report
    let (mut context_digest, mut analytics_markdown, matched_rows_count) =
        build_workspace_context_and_report(
            &active_grid,
            &active_columns,
            &active_rows,
            &accessible_grids,
            &user_display_map,
            effective_prompt,
        );

    let (attached_images, attachments_context_block, parsed_attachment_rows) =
        process_ai_attachments(&req.attachments, &active_columns);
    if !attachments_context_block.is_empty() {
        context_digest.push_str(&attachments_context_block);
        analytics_markdown.push_str("\n#### 📎 Attached Files & Images\n");
        for att in &req.attachments {
            analytics_markdown.push_str(&format!("- `{}` (`{}`)\n", att.filename, att.mime_type));
        }
    }

    let lower_prompt = effective_prompt.to_ascii_lowercase();
    let wants_attachment_import = !parsed_attachment_rows.is_empty()
        && (lower_prompt.contains("import")
            || lower_prompt.contains("insert")
            || lower_prompt.contains("add rows")
            || lower_prompt.contains("add all"));

    let mut mode = req.mode.trim().to_ascii_lowercase();
    if mode != "execute"
        && (prompt_is_explicit_task_command(effective_prompt) || wants_attachment_import)
    {
        mode = "execute".to_string();
    }
    let mut executed_actions: Vec<AiExecutedAction> = Vec::new();
    let mut navigated_grid_id: Option<Uuid> = None;

    // 4. Mode == "execute" -> Autonomous Task Execution (Create Grid, Add Column, Add/Update/Delete Row)
    if mode == "execute" {
        let exec_system_prompt = format!(
            "{}\n\nYou are operating in AUTONOMOUS TASK EXECUTION MODE for PyraOcean.\n\
            Convert the user's instruction into a valid JSON object with the exact structure:\n\
            {{\n\
              \"explanation\": \"Brief summary of what you did\",\n\
              \"actions\": [\n\
                {{\"action\": \"create_grid\", \"name\": \"Project Tracker\", \"icon\": \"🚀\", \"columns\": [{{\"name\": \"Task\", \"property_type\": \"text\"}}, {{\"name\": \"Status\", \"property_type\": \"select\"}}, {{\"name\": \"Department\", \"property_type\": \"multi_select\"}}, {{\"name\": \"Budget\", \"property_type\": \"currency\"}}], \"rows\": [{{\"Task\": \"Launch MVP\", \"Status\": \"In Progress\", \"Department\": [\"Software\", \"Innovation\"], \"Budget\": 25000}}]}},\n\
                {{\"action\": \"add_column\", \"name\": \"Priority\", \"property_type\": \"select\"}},\n\
                {{\"action\": \"add_row\", \"values\": {{\"Service\": \"Canva\", \"Department\": [\"Marketing\", \"B2C\"], \"Payment\": \"Yearly\", \"Cost\": 1500}}}},\n\
                {{\"action\": \"update_row\", \"match_text\": \"ChatGPT\", \"values\": {{\"Department\": [\"Software\", \"B2C\", \"Innovation\"]}}, \"append_tags\": false, \"remove_tags\": false}},\n\
                {{\"action\": \"delete_row\", \"match_text\": \"row keyword or row number\"}}\n\
              ]\n\
            }}\n\
            For Select columns, pass the option label string (e.g. \"Marketing\" or \"Yearly\").\n\
            For MultiSelect columns, pass either a JSON array of tag strings (e.g. [\"Software\", \"B2C\", \"Innovation\"]) or a comma-separated string (\"Software, B2C, Innovation\").\n\
            If the user asks to ADD/APPEND a tag to an existing MultiSelect cell without replacing existing tags, set \"append_tags\": true on update_row.\n\
            If the user asks to REMOVE a tag from an existing MultiSelect cell, set \"remove_tags\": true on update_row.\n\
            Allowed property_type values: text, number, currency, select, multi_select, date, checkbox, checklist, person, password.\n\
            Output ONLY raw JSON without markdown fences if possible.\n\n\
            CURRENT WORKSPACE CONTEXT:\n{}",
            settings.system_prompt, context_digest
        );

        let mut messages = vec![OllamaMessage {
            role: "system".into(),
            content: exec_system_prompt,
            images: Vec::new(),
        }];
        for h in req.history.iter().rev().take(4).rev() {
            let hist_imgs: Vec<String> = h
                .attachments
                .iter()
                .filter(|a| a.mime_type.to_ascii_lowercase().starts_with("image/"))
                .map(|a| strip_data_url_prefix(&a.data_base64).to_string())
                .filter(|s| !s.is_empty())
                .collect();
            messages.push(OllamaMessage {
                role: h.role.clone(),
                content: h.content.clone(),
                images: hist_imgs,
            });
        }
        messages.push(OllamaMessage {
            role: "user".into(),
            content: effective_prompt.to_string(),
            images: attached_images.clone(),
        });

        let mut deterministic_env = fallback_intent_parser(effective_prompt, &active_columns);
        if deterministic_env.actions.is_empty() && !parsed_attachment_rows.is_empty() {
            deterministic_env = LLMCommandEnvelope {
                explanation: Some(format!(
                    "Imported {} row(s) from attached file.",
                    parsed_attachment_rows.len()
                )),
                actions: parsed_attachment_rows
                    .iter()
                    .cloned()
                    .map(|values| LLMTaskSpec::AddRow { values })
                    .collect(),
            };
        }
        let envelope: LLMCommandEnvelope = if !deterministic_env.actions.is_empty() {
            deterministic_env
        } else {
            let llm_raw =
                call_ollama_chat(&settings.ollama_base_url, &settings.model_name, messages).await;
            match &llm_raw {
                Ok(text) => parse_llm_command_envelope(text)
                    .filter(|e| !e.actions.is_empty())
                    .unwrap_or(deterministic_env),
                Err(_) => deterministic_env,
            }
        };

        for spec in envelope.actions {
            match spec {
                LLMTaskSpec::CreateGrid {
                    name,
                    icon,
                    columns,
                    rows,
                } => {
                    let target_space_id = if let Some(sid) = req.space_id {
                        sid
                    } else if let Some(ref g) = active_grid {
                        g.space_id
                    } else if let Some((_, sid, _, _)) = accessible_grids.first() {
                        *sid
                    } else {
                        let found: Option<(Uuid,)> =
                            sqlx::query_as("SELECT id FROM spaces WHERE is_archived = FALSE ORDER BY created_at ASC LIMIT 1")
                                .fetch_optional(&state.db)
                                .await?;
                        found
                            .ok_or_else(|| {
                                AuthError::BadRequest("No active Space available to create a Grid.".into())
                            })?
                            .0
                    };

                    let space_access =
                        resolve_space_access_cached(&state, auth_user.clone(), target_space_id).await?;
                    space_access.ensure_write()?;

                    let clean_name = if name.trim().is_empty() {
                        "AI Generated Grid".to_string()
                    } else {
                        name.trim().to_string()
                    };
                    let clean_icon = icon.unwrap_or_else(|| "🤖".to_string());

                    let (next_pos,): (i32,) = sqlx::query_as(
                        "SELECT COALESCE(MAX(position) + 1, 0) FROM pages_grids WHERE space_id = $1 AND deleted_at IS NULL",
                    )
                    .bind(target_space_id)
                    .fetch_one(&state.db)
                    .await?;

                    let new_grid: PageGrid = sqlx::query_as(
                        r#"
                        INSERT INTO pages_grids (space_id, title, icon, position, created_by, is_archived)
                        VALUES ($1, $2, $3, $4, $5, FALSE)
                        RETURNING id, space_id, title, description, icon, position, created_by, is_archived, created_at, updated_at, deleted_at, deleted_by
                        "#,
                    )
                    .bind(target_space_id)
                    .bind(&clean_name)
                    .bind(&clean_icon)
                    .bind(next_pos)
                    .bind(auth_user.user_id)
                    .fetch_one(&state.db)
                    .await?;

                    let col_specs = if columns.is_empty() {
                        vec![
                            LLMColumnSpec {
                                name: "Name".into(),
                                property_type: "text".into(),
                            },
                            LLMColumnSpec {
                                name: "Status".into(),
                                property_type: "select".into(),
                            },
                            LLMColumnSpec {
                                name: "Notes".into(),
                                property_type: "text".into(),
                            },
                        ]
                    } else {
                        columns
                    };

                    let mut created_cols: Vec<GridColumn> = Vec::new();
                    for (idx, cspec) in col_specs.iter().enumerate() {
                        let (ptype, pcfg) = parse_property_type_and_config(&cspec.property_type);
                        let inserted_col: GridColumn = sqlx::query_as(
                            r#"
                            INSERT INTO grid_columns (grid_id, name, property_type, property_config, width_px, position, is_primary, is_hidden)
                            VALUES ($1, $2, $3, $4, 200, $5, $6, FALSE)
                            RETURNING id, grid_id, name, property_type, property_config, width_px, position, is_primary, is_hidden, created_at, updated_at
                            "#,
                        )
                        .bind(new_grid.id)
                        .bind(cspec.name.trim())
                        .bind(ptype)
                        .bind(sqlx::types::Json(pcfg))
                        .bind(idx as i32)
                        .bind(idx == 0)
                        .fetch_one(&state.db)
                        .await?;
                        created_cols.push(inserted_col);
                    }

                    let mut inserted_rows_count = 0usize;
                    for (r_idx, row_map) in rows.iter().enumerate() {
                        let mut cells: RowCellMap = HashMap::new();
                        for (k, v) in row_map {
                            if let Some(col) = created_cols
                                .iter_mut()
                                .find(|c| c.name.eq_ignore_ascii_case(k.trim()))
                            {
                                let prepared_json =
                                    prepare_cell_value_and_sync_options(&state.db, col, v).await;
                                if let Ok(cell_val) =
                                    serde_json::from_value::<CellValue>(prepared_json)
                                {
                                    cells.insert(col.id, cell_val);
                                }
                            }
                        }

                        sqlx::query(
                            r#"
                            INSERT INTO grid_rows (grid_id, position, row_order, cells, created_by, updated_by)
                            VALUES ($1, $2, $3, $4, $5, $5)
                            "#,
                        )
                        .bind(new_grid.id)
                        .bind(r_idx as i32)
                        .bind(((r_idx + 1) * 1000) as f64)
                        .bind(sqlx::types::Json(cells))
                        .bind(auth_user.user_id)
                        .execute(&state.db)
                        .await?;
                        inserted_rows_count += 1;
                    }

                    state.cache.invalidate_space(&target_space_id);
                    navigated_grid_id = Some(new_grid.id);
                    executed_actions.push(AiExecutedAction {
                        action_type: "create_grid".into(),
                        summary: format!(
                            "Created grid '{}' with {} columns and {} rows",
                            clean_name,
                            created_cols.len(),
                            inserted_rows_count
                        ),
                        grid_id: Some(new_grid.id),
                        row_id: None,
                    });
                }

                LLMTaskSpec::AddColumn {
                    name,
                    property_type,
                } => {
                    let Some(ref g) = active_grid else {
                        return Err(AuthError::BadRequest(
                            "Open a Grid first before asking PyraAI to add a column.".into(),
                        ));
                    };
                    if !can_edit_active_grid {
                        return Err(AuthError::Forbidden {
                            required: "space editor or space_admin",
                        });
                    }
                    let clean_col_name = name.trim();
                    if clean_col_name.is_empty() {
                        continue;
                    }
                    let (ptype, pcfg) = parse_property_type_and_config(&property_type);
                    let next_order = active_columns
                        .iter()
                        .map(|c| c.position)
                        .max()
                        .unwrap_or(0)
                        + 1;

                    let new_col: GridColumn = sqlx::query_as(
                        r#"
                        INSERT INTO grid_columns (grid_id, name, property_type, property_config, width_px, position, is_primary, is_hidden)
                        VALUES ($1, $2, $3, $4, 200, $5, FALSE, FALSE)
                        RETURNING id, grid_id, name, property_type, property_config, width_px, position, is_primary, is_hidden, created_at, updated_at
                        "#,
                    )
                    .bind(g.id)
                    .bind(clean_col_name)
                    .bind(ptype)
                    .bind(sqlx::types::Json(pcfg))
                    .bind(next_order)
                    .fetch_one(&state.db)
                    .await?;

                    active_columns.push(new_col);
                    state.cache.invalidate_grid(&g.id);
                    executed_actions.push(AiExecutedAction {
                        action_type: "add_column".into(),
                        summary: format!("Added column '{}' ({:?})", clean_col_name, ptype),
                        grid_id: Some(g.id),
                        row_id: None,
                    });
                }

                LLMTaskSpec::AddRow { values } => {
                    let Some(ref g) = active_grid else {
                        return Err(AuthError::BadRequest(
                            "Open a Grid first before asking PyraAI to add rows.".into(),
                        ));
                    };
                    if !can_edit_active_grid {
                        return Err(AuthError::Forbidden {
                            required: "space editor or space_admin",
                        });
                    }
                    let mut cells: RowCellMap = HashMap::new();
                    for (k, v) in &values {
                        let col_idx = active_columns
                            .iter()
                            .position(|c| c.name.eq_ignore_ascii_case(k.trim()))
                            .or_else(|| {
                                active_columns.iter().position(|c| {
                                    c.name
                                        .to_ascii_lowercase()
                                        .contains(&k.trim().to_ascii_lowercase())
                                })
                            });
                        if let Some(idx) = col_idx {
                            let col = &mut active_columns[idx];
                            let prepared_json =
                                prepare_cell_value_and_sync_options(&state.db, col, v).await;
                            if let Ok(cell_val) =
                                serde_json::from_value::<CellValue>(prepared_json)
                            {
                                cells.insert(col.id, cell_val);
                            }
                        }
                    }

                    if cells.is_empty() {
                        if let Some(first_col) = active_columns.first_mut() {
                            if let Some((_, first_val)) = values.iter().next() {
                                let prepared_json =
                                    prepare_cell_value_and_sync_options(&state.db, first_col, first_val)
                                        .await;
                                if let Ok(cell_val) =
                                    serde_json::from_value::<CellValue>(prepared_json)
                                {
                                    cells.insert(first_col.id, cell_val);
                                }
                            }
                        }
                    }

                    let (next_pos, next_row_order): (i32, f64) = sqlx::query_as(
                        "SELECT COALESCE(MAX(position) + 1, 0), COALESCE(MAX(row_order) + 1000.0, 1000.0) FROM grid_rows WHERE grid_id = $1",
                    )
                    .bind(g.id)
                    .fetch_one(&state.db)
                    .await?;

                    let inserted_row_id: (Uuid,) = sqlx::query_as(
                        r#"
                        INSERT INTO grid_rows (grid_id, position, row_order, cells, created_by, updated_by)
                        VALUES ($1, $2, $3, $4, $5, $5)
                        RETURNING id
                        "#,
                    )
                    .bind(g.id)
                    .bind(next_pos)
                    .bind(next_row_order)
                    .bind(sqlx::types::Json(&cells))
                    .bind(auth_user.user_id)
                    .fetch_one(&state.db)
                    .await?;

                    state.cache.invalidate_grid(&g.id);
                    executed_actions.push(AiExecutedAction {
                        action_type: "add_row".into(),
                        summary: format!("Inserted new row into '{}'", g.title),
                        grid_id: Some(g.id),
                        row_id: Some(inserted_row_id.0),
                    });
                }

                LLMTaskSpec::UpdateRow {
                    match_text,
                    values,
                    append_tags,
                    remove_tags,
                } => {
                    let Some(ref g) = active_grid else {
                        return Err(AuthError::BadRequest(
                            "Open a Grid first before asking PyraAI to update rows.".into(),
                        ));
                    };
                    if !can_edit_active_grid {
                        return Err(AuthError::Forbidden {
                            required: "space editor or space_admin",
                        });
                    }
                    let needle = match_text.trim().to_ascii_lowercase();
                    let row_index_match: Option<usize> = needle
                        .strip_prefix("row #")
                        .or_else(|| needle.strip_prefix("row "))
                        .or_else(|| needle.strip_prefix('#'))
                        .unwrap_or(&needle)
                        .trim()
                        .parse::<usize>()
                        .ok()
                        .filter(|n| *n >= 1);

                    let target_row = if let Some(one_based) = row_index_match {
                        active_rows.get(one_based - 1)
                    } else {
                        active_rows.iter().find(|r| {
                            if needle.is_empty() {
                                return false;
                            }
                            active_columns.iter().any(|col| {
                                resolve_row_cell_display(
                                    col,
                                    r.cells.get(&col.id),
                                    &user_display_map,
                                    r.last_modified,
                                )
                                .to_ascii_lowercase()
                                .contains(&needle)
                            })
                        })
                    };

                    if let Some(row) = target_row {
                        let mut updated_cells = row.cells.clone();
                        for (k, v) in &values {
                            if let Some(col) = active_columns.iter_mut().find(|c| {
                                c.name.eq_ignore_ascii_case(k.trim())
                                    || c.name
                                        .to_ascii_lowercase()
                                        .contains(&k.trim().to_ascii_lowercase())
                            }) {
                                let existing_cell = updated_cells.get(&col.id).cloned();
                                let cell_val = prepare_typed_cell_value_and_sync_options(
                                    &state.db,
                                    col,
                                    v,
                                    existing_cell.as_ref(),
                                    append_tags,
                                    remove_tags,
                                )
                                .await;
                                updated_cells.insert(col.id, cell_val);
                            }
                        }

                        sqlx::query(
                            "UPDATE grid_rows SET cells = $1, updated_by = $2, last_modified = NOW() WHERE id = $3 AND grid_id = $4",
                        )
                        .bind(sqlx::types::Json(&updated_cells))
                        .bind(auth_user.user_id)
                        .bind(row.id)
                        .bind(g.id)
                        .execute(&state.db)
                        .await?;

                        state.cache.invalidate_grid(&g.id);
                        executed_actions.push(AiExecutedAction {
                            action_type: "update_row".into(),
                            summary: format!("Updated row matching '{}'", match_text),
                            grid_id: Some(g.id),
                            row_id: Some(row.id),
                        });
                    }
                }

                LLMTaskSpec::DeleteRow { match_text } => {
                    let Some(ref g) = active_grid else {
                        return Err(AuthError::BadRequest(
                            "Open a Grid first before asking PyraAI to delete rows.".into(),
                        ));
                    };
                    if !can_edit_active_grid {
                        return Err(AuthError::Forbidden {
                            required: "space editor or space_admin",
                        });
                    }
                    let needle = match_text.trim().to_ascii_lowercase();
                    if !needle.is_empty() {
                        let row_index_match: Option<usize> = needle
                            .strip_prefix("row #")
                            .or_else(|| needle.strip_prefix("row "))
                            .or_else(|| needle.strip_prefix('#'))
                            .unwrap_or(&needle)
                            .trim()
                            .parse::<usize>()
                            .ok()
                            .filter(|n| *n >= 1);

                        let matching_ids: Vec<Uuid> = if let Some(one_based) = row_index_match {
                            active_rows
                                .get(one_based - 1)
                                .map(|r| vec![r.id])
                                .unwrap_or_default()
                        } else {
                            active_rows
                                .iter()
                                .filter(|r| {
                                    active_columns.iter().any(|col| {
                                        resolve_row_cell_display(
                                            col,
                                            r.cells.get(&col.id),
                                            &user_display_map,
                                            r.last_modified,
                                        )
                                        .to_ascii_lowercase()
                                        .contains(&needle)
                                    })
                                })
                                .map(|r| r.id)
                                .collect()
                        };

                        if !matching_ids.is_empty() {
                            let res = sqlx::query(
                                "DELETE FROM grid_rows WHERE grid_id = $1 AND id = ANY($2)",
                            )
                            .bind(g.id)
                            .bind(&matching_ids)
                            .execute(&state.db)
                            .await?;

                            state.cache.invalidate_grid(&g.id);
                            executed_actions.push(AiExecutedAction {
                                action_type: "delete_row".into(),
                                summary: format!(
                                    "Deleted {} row(s) matching '{}'",
                                    res.rows_affected(),
                                    match_text
                                ),
                                grid_id: Some(g.id),
                                row_id: matching_ids.first().copied(),
                            });
                        }
                    }
                }
            }
        }

        let reply = if !executed_actions.is_empty() {
            let mut summary_lines = vec![envelope
                .explanation
                .unwrap_or_else(|| "Completed your requested workspace tasks:".to_string())];
            for act in &executed_actions {
                summary_lines.push(format!("- **{}**: {}", act.action_type, act.summary));
            }
            summary_lines.join("\n")
        } else if matched_rows_count > 0
            || prompt_trimmed.to_ascii_lowercase().starts_with("search ")
            || prompt_trimmed.to_ascii_lowercase().starts_with("show ")
            || prompt_trimmed.to_ascii_lowercase().starts_with("find ")
            || prompt_trimmed.to_ascii_lowercase().starts_with("list ")
        {
            analytics_markdown.clone()
        } else {
            envelope.explanation.unwrap_or_else(|| {
                "No matching rows or actionable commands were found for that task. Try specifying the column names and values clearly (e.g., `Add row Service = Canva, Department = Marketing, B2C, Payment = Yearly`).".to_string()
            })
        };

        return Ok(Json(AiCopilotResponse {
            reply,
            model_used: settings.model_name,
            executed_actions,
            matched_rows_count,
            navigated_grid_id,
        }));
    }

    // 5. Mode == "report" or "ask" -> Natural Language Search & Executive Report Generation
    let mode_instruction = if mode == "report" {
        "Generate a comprehensive, executive-ready Markdown report with: (1) Executive Overview, (2) Financial & Numeric Totals (including Dual-Currency EGP / USD totals and averages where present), (3) Category / Department Breakdown, and (4) Key Actionable Recommendations."
    } else {
        "Answer the user's question accurately using the provided PyraOcean Grid & Workspace data. Cite exact row names, numbers, statuses, and dates from the context."
    };

    let full_system_prompt = format!(
        "{}\n\n{}\n\nWORKSPACE & GRID DATA CONTEXT:\n{}\n\nPRE-COMPUTED ANALYTICS & TOTALS:\n{}",
        settings.system_prompt, mode_instruction, context_digest, analytics_markdown
    );

    let mut chat_messages = vec![OllamaMessage {
        role: "system".into(),
        content: full_system_prompt,
        images: Vec::new(),
    }];
    for h in req.history.iter().rev().take(6).rev() {
        let hist_imgs: Vec<String> = h
            .attachments
            .iter()
            .filter(|a| a.mime_type.to_ascii_lowercase().starts_with("image/"))
            .map(|a| strip_data_url_prefix(&a.data_base64).to_string())
            .filter(|s| !s.is_empty())
            .collect();
        chat_messages.push(OllamaMessage {
            role: h.role.clone(),
            content: h.content.clone(),
            images: hist_imgs,
        });
    }
    chat_messages.push(OllamaMessage {
        role: "user".into(),
        content: effective_prompt.to_string(),
        images: attached_images,
    });

    match call_ollama_chat(&settings.ollama_base_url, &settings.model_name, chat_messages).await {
        Ok(llm_answer) => Ok(Json(AiCopilotResponse {
            reply: llm_answer,
            model_used: settings.model_name,
            executed_actions,
            matched_rows_count,
            navigated_grid_id: None,
        })),
        Err(ollama_err) => {
            let fallback_answer = format!(
                "{analytics_markdown}\n\n---\n*Note: Local Ollama (`{}` — model `{}`) was unreachable ({ollama_err}), so PyraAI generated this instant deterministic report directly from your live PostgreSQL Grid data.*",
                settings.ollama_base_url, settings.model_name
            );
            Ok(Json(AiCopilotResponse {
                reply: fallback_answer,
                model_used: format!("{} (Built-in Analytics Fallback)", settings.model_name),
                executed_actions,
                matched_rows_count,
                navigated_grid_id: None,
            }))
        }
    }
}

// ============================================================================
// 5. Context Builder, Deterministic Analytics & Fallback Intent Parser
// ============================================================================

fn build_workspace_context_and_report(
    active_grid: &Option<PageGrid>,
    columns: &[GridColumn],
    rows: &[GridRow],
    accessible_grids: &[(Uuid, Uuid, String, String)],
    user_display_map: &HashMap<Uuid, String>,
    prompt: &str,
) -> (String, String, usize) {
    let mut ctx = String::new();
    let mut report = String::new();
    let mut matched_count = 0usize;

    ctx.push_str("Accessible Workspace Grids:\n");
    for (gid, _sid, sname, gname) in accessible_grids.iter().take(30) {
        ctx.push_str(&format!("- [{}] {} / {}\n", gid, sname, gname));
    }

    if let Some(grid) = active_grid {
        let visible_cols: Vec<&GridColumn> = columns.iter().filter(|c| !c.is_hidden).collect();
        let col_schema_line = visible_cols
            .iter()
            .map(|c| {
                let opts = get_column_option_labels(c);
                if opts.is_empty() {
                    format!("{}({:?})", c.name, c.property_type)
                } else {
                    format!(
                        "{}({:?}, options: [{}])",
                        c.name,
                        c.property_type,
                        opts.join(", ")
                    )
                }
            })
            .collect::<Vec<_>>()
            .join(", ");

        ctx.push_str(&format!(
            "\nActive Grid: {} (Total Rows: {})\nColumns & Available Tag Options: {}\n",
            grid.title,
            rows.len(),
            col_schema_line
        ));

        report.push_str(&format!(
            "### 📊 Executive Summary: {} {}\n- **Total Rows:** `{}`\n- **Columns (`{}`):** {}\n\n",
            grid.icon.as_deref().unwrap_or("📄"),
            grid.title,
            rows.len(),
            columns.len(),
            columns
                .iter()
                .map(|c| format!("`{}`", c.name))
                .collect::<Vec<_>>()
                .join(", ")
        ));

        // Compute numeric/currency column calculations (`Sum` and `Average`)
        let mut calc_bullets: Vec<String> = Vec::new();
        for col in columns {
            let sum_res = compute_column_calculation(col, rows, CalcMode::Sum);
            let avg_res = compute_column_calculation(col, rows, CalcMode::Average);
            let sum_str = format_calc_summary_plain(&sum_res);
            let avg_str = format_calc_summary_plain(&avg_res);
            if !sum_str.is_empty() {
                calc_bullets.push(format!(
                    "- **{}**: Total `SUM = {}` | `AVG = {}`",
                    col.name, sum_str, avg_str
                ));
            }
        }
        if !calc_bullets.is_empty() {
            report.push_str("#### 💰 Financial & Numeric Totals\n");
            ctx.push_str("Pre-Calculated EGP/USD & Numeric Totals:\n");
            for b in &calc_bullets {
                report.push_str(b);
                report.push('\n');
                ctx.push_str(b);
                ctx.push('\n');
            }
            report.push('\n');
        }

        // Compute individual tag & group breakdowns + Department Subtotals (EGP / USD) for Select / MultiSelect columns
        for col in columns.iter().filter(|c| {
            matches!(
                c.property_type,
                ColumnPropertyType::Select | ColumnPropertyType::MultiSelect
            )
        }) {
            let mut tag_buckets: Vec<(String, Vec<GridRow>)> = Vec::new();
            for opt_label in get_column_option_labels(col) {
                tag_buckets.push((opt_label, Vec::new()));
            }
            let mut uncategorized_rows: Vec<GridRow> = Vec::new();

            for r in rows {
                let display = resolve_row_cell_display(
                    col,
                    r.cells.get(&col.id),
                    user_display_map,
                    r.last_modified,
                );
                let tags: Vec<String> = display
                    .split(',')
                    .map(|t| t.trim().to_string())
                    .filter(|t| !t.is_empty())
                    .collect();
                if tags.is_empty() {
                    uncategorized_rows.push(r.clone());
                } else {
                    for tag in tags {
                        if let Some(entry) = tag_buckets
                            .iter_mut()
                            .find(|(lbl, _)| lbl.eq_ignore_ascii_case(&tag))
                        {
                            entry.1.push(r.clone());
                        } else {
                            tag_buckets.push((tag, vec![r.clone()]));
                        }
                    }
                }
            }

            tag_buckets.retain(|(_, bucket_rows)| !bucket_rows.is_empty());
            if !tag_buckets.is_empty() || !uncategorized_rows.is_empty() {
                report.push_str(&format!("#### 🗂️ Breakdown & Subtotals by `{}`\n", col.name));
                ctx.push_str(&format!("Tag Breakdown & Subtotals for {}:\n", col.name));
                for (lbl, bucket_rows) in &tag_buckets {
                    let mut sub_parts: Vec<String> = Vec::new();
                    for num_col in columns {
                        let sum_res = compute_column_calculation(num_col, bucket_rows, CalcMode::Sum);
                        let sum_str = format_calc_summary_plain(&sum_res);
                        if !sum_str.is_empty() {
                            sub_parts.push(format!("{}: {}", num_col.name, sum_str));
                        }
                    }
                    let sub_suffix = if sub_parts.is_empty() {
                        String::new()
                    } else {
                        format!(" — Subtotal `{}`", sub_parts.join(" | "))
                    };
                    let line = format!(
                        "- **{}**: `{}` row(s){}\n",
                        lbl,
                        bucket_rows.len(),
                        sub_suffix
                    );
                    report.push_str(&line);
                    ctx.push_str(&format!(
                        "- {}: {} row(s){}\n",
                        lbl,
                        bucket_rows.len(),
                        sub_suffix
                    ));
                }
                if !uncategorized_rows.is_empty() {
                    report.push_str(&format!(
                        "- **Uncategorized**: `{}` row(s)\n",
                        uncategorized_rows.len()
                    ));
                }
                report.push('\n');
            }
        }

        // Filter matching rows for the user's query keywords across all 12 property types (including Select & MultiSelect option labels)
        let stop_words = [
            "what", "where", "which", "show", "find", "list", "give", "report", "summary", "the",
            "and", "for", "with", "from", "that", "this", "have", "rows", "grid", "all", "about",
            "search", "filter", "department", "status", "payment", "category", "tags", "tag", "are",
        ];
        let keywords: Vec<String> = prompt
            .split(|c: char| !c.is_alphanumeric())
            .map(|w| w.trim().to_ascii_lowercase())
            .filter(|w| w.len() >= 2 && !stop_words.contains(&w.as_str()))
            .collect();

        let header_tsv = visible_cols
            .iter()
            .map(|c| c.name.as_str())
            .collect::<Vec<_>>()
            .join("\t");
        ctx.push_str(&format!(
            "\nRows (Compact TSV, up to 60 relevant rows):\n#\t{}\n",
            header_tsv
        ));
        let mut matched_rows_md: Vec<String> = Vec::new();
        let mut tsv_matched_lines: Vec<String> = Vec::new();
        let mut tsv_other_lines: Vec<String> = Vec::new();

        for (idx, r) in rows.iter().enumerate() {
            let mut vals: Vec<String> = Vec::with_capacity(visible_cols.len());
            let mut pairs: Vec<String> = Vec::new();
            for col in &visible_cols {
                let text = resolve_row_cell_display(
                    col,
                    r.cells.get(&col.id),
                    user_display_map,
                    r.last_modified,
                );
                let clean = text.replace(['\t', '\n', '\r'], " ");
                if !clean.is_empty() {
                    pairs.push(format!("{}: {}", col.name, clean));
                }
                vals.push(if clean.is_empty() { "-".into() } else { clean });
            }
            let tsv_line = format!("{}\t{}", idx + 1, vals.join("\t"));
            let row_line = pairs.join(" | ");
            let is_kw_match = !keywords.is_empty() && {
                let lower_line = row_line.to_ascii_lowercase();
                keywords.iter().any(|kw| lower_line.contains(kw))
            };
            if is_kw_match {
                if matched_rows_md.len() < 50 {
                    matched_rows_md.push(format!("{}. {}", idx + 1, row_line));
                }
                if tsv_matched_lines.len() < 60 {
                    tsv_matched_lines.push(tsv_line);
                }
            } else if tsv_other_lines.len() < 60 {
                tsv_other_lines.push(tsv_line);
            }
        }

        let mut added = 0usize;
        for line in tsv_matched_lines
            .into_iter()
            .chain(tsv_other_lines.into_iter())
        {
            if added >= 60 {
                break;
            }
            ctx.push_str(&line);
            ctx.push('\n');
            added += 1;
        }

        if !matched_rows_md.is_empty() {
            matched_count = matched_rows_md.len();
            report.push_str(&format!(
                "#### 🔍 Matching Rows for Your Query (`{}` found)\n",
                matched_count
            ));
            for m in matched_rows_md {
                report.push_str(&format!("{m}\n"));
            }
        }
    } else {
        report.push_str("### 🌐 Workspace Overview\n");
        report.push_str(&format!(
            "- **Accessible Grids:** `{}`\n",
            accessible_grids.len()
        ));
        for (_gid, _sid, sname, gname) in accessible_grids.iter().take(15) {
            report.push_str(&format!("- **{}** → `{}`\n", sname, gname));
        }
    }

    (ctx, report, matched_count)
}

fn parse_llm_command_envelope(raw: &str) -> Option<LLMCommandEnvelope> {
    let trimmed = raw.trim();
    if let Ok(env) = serde_json::from_str::<LLMCommandEnvelope>(trimmed) {
        if !env.actions.is_empty() || env.explanation.is_some() {
            return Some(env);
        }
    }
    if let Some(start) = trimmed.find('{') {
        if let Some(end) = trimmed.rfind('}') {
            if end > start {
                let slice = &trimmed[start..=end];
                if let Ok(env) = serde_json::from_str::<LLMCommandEnvelope>(slice) {
                    return Some(env);
                }
                if let Ok(single) = serde_json::from_str::<LLMTaskSpec>(slice) {
                    return Some(LLMCommandEnvelope {
                        explanation: Some("Executed requested action.".into()),
                        actions: vec![single],
                    });
                }
            }
        }
    }
    None
}

fn prompt_is_explicit_task_command(prompt: &str) -> bool {
    let lower = prompt.trim().to_ascii_lowercase();
    lower.starts_with("create grid")
        || lower.starts_with("create a grid")
        || lower.starts_with("new grid")
        || lower.starts_with("add column")
        || lower.starts_with("create column")
        || lower.starts_with("new column")
        || lower.starts_with("add row")
        || lower.starts_with("add a row")
        || lower.starts_with("insert row")
        || lower.starts_with("update row")
        || lower.starts_with("edit row")
        || lower.starts_with("delete row")
        || lower.starts_with("remove row")
        || lower.starts_with("add tag ")
        || lower.starts_with("remove tag ")
        || (lower.starts_with("set ") && (lower.contains(" for ") || lower.contains(" on ")))
}

/// Multi-tag-aware key-value parser that parses `Service = Canva, Department = Marketing, B2C, Payment = Yearly`
/// so comma-separated MultiSelect tags (`Marketing, B2C`) remain attached to `Department` rather than being lost.
fn parse_key_value_pairs_from_prompt(
    text: &str,
    active_columns: &[GridColumn],
) -> HashMap<String, Value> {
    let mut raw_pairs: Vec<(String, Vec<String>)> = Vec::new();

    for segment in text.split([',', ';', '\n']) {
        let trimmed = segment.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Some((k, v)) = trimmed.split_once('=').or_else(|| trimmed.split_once(':')) {
            let clean_k = k
                .trim()
                .trim_start_matches(|c: char| !c.is_alphanumeric())
                .trim()
                .to_string();
            // Match against active_columns by suffix or substring so leading words like "add row Service" resolve to "Service"
            let matched_col_name = active_columns
                .iter()
                .find(|c| {
                    clean_k.eq_ignore_ascii_case(&c.name)
                        || clean_k
                            .to_ascii_lowercase()
                            .ends_with(&c.name.to_ascii_lowercase())
                })
                .or_else(|| {
                    let last_word = clean_k.split_whitespace().last().unwrap_or(&clean_k);
                    active_columns.iter().find(|c| {
                        c.name.eq_ignore_ascii_case(last_word)
                            || c.name
                                .to_ascii_lowercase()
                                .contains(&last_word.to_ascii_lowercase())
                    })
                })
                .map(|c| c.name.clone())
                .unwrap_or_else(|| {
                    clean_k
                        .split_whitespace()
                        .last()
                        .unwrap_or(&clean_k)
                        .to_string()
                });

            let clean_v = v
                .trim()
                .trim_matches(['"', '\'', '[', ']'])
                .trim()
                .to_string();
            let mut vals = Vec::new();
            for sub in clean_v.split(" and ") {
                let s = sub.trim().trim_matches(['"', '\'']).trim();
                if !s.is_empty() {
                    vals.push(s.to_string());
                }
            }
            raw_pairs.push((matched_col_name, vals));
        } else if let Some((_, last_vals)) = raw_pairs.last_mut() {
            // Continuation tag for the preceding key (e.g. `, B2C` in `Department = Marketing, B2C, Payment = Yearly`)
            let clean_extra = trimmed
                .trim_matches(['"', '\'', '[', ']'])
                .trim()
                .to_string();
            for sub in clean_extra.split(" and ") {
                let s = sub.trim().trim_matches(['"', '\'']).trim();
                if !s.is_empty() {
                    last_vals.push(s.to_string());
                }
            }
        }
    }

    let mut values: HashMap<String, Value> = HashMap::new();
    for (col_name, vals) in raw_pairs {
        let is_multi = active_columns
            .iter()
            .find(|c| c.name.eq_ignore_ascii_case(&col_name))
            .map(|c| c.property_type == ColumnPropertyType::MultiSelect)
            .unwrap_or(vals.len() > 1);

        if is_multi {
            values.insert(
                col_name,
                Value::Array(vals.into_iter().map(Value::String).collect()),
            );
        } else {
            values.insert(col_name, Value::String(vals.join(", ")));
        }
    }
    values
}

fn fallback_intent_parser(prompt: &str, active_columns: &[GridColumn]) -> LLMCommandEnvelope {
    let lower = prompt.trim().to_ascii_lowercase();

    // 1. Create Grid intent (`create grid ...` or `new grid ...`)
    if lower.contains("create grid")
        || lower.contains("create a grid")
        || lower.contains("new grid")
    {
        let name = prompt
            .split_once("called")
            .or_else(|| prompt.split_once("named"))
            .or_else(|| prompt.split_once("grid"))
            .map(|(_, right)| {
                right
                    .split(['(', ',', '.'])
                    .next()
                    .unwrap_or("")
                    .trim()
                    .trim_matches(['"', '\'', ':'])
                    .to_string()
            })
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "AI Workspace Grid".to_string());

        return LLMCommandEnvelope {
            explanation: Some(format!("Created new grid '{name}' via PyraAI.")),
            actions: vec![LLMTaskSpec::CreateGrid {
                name,
                icon: Some("✨".into()),
                columns: vec![
                    LLMColumnSpec {
                        name: "Name".into(),
                        property_type: "text".into(),
                    },
                    LLMColumnSpec {
                        name: "Status".into(),
                        property_type: "select".into(),
                    },
                    LLMColumnSpec {
                        name: "Budget".into(),
                        property_type: "currency".into(),
                    },
                ],
                rows: Vec::new(),
            }],
        };
    }

    // 2. Add Column intent (`add column ...`)
    if lower.contains("add column") || lower.contains("create column") || lower.contains("new column") {
        let col_name = prompt
            .split_once("column")
            .map(|(_, r)| {
                r.trim()
                    .trim_start_matches("called ")
                    .trim_start_matches("named ")
                    .split_whitespace()
                    .take(3)
                    .collect::<Vec<_>>()
                    .join(" ")
                    .trim_matches(['"', '\'', ',', '.'])
                    .to_string()
            })
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "New Column".to_string());

        let ptype = if lower.contains("multi_select")
            || lower.contains("multiselect")
            || lower.contains("multiple")
            || lower.contains("tags")
        {
            "multi_select"
        } else if lower.contains("currency") || lower.contains("cost") || lower.contains("budget") {
            "currency"
        } else if lower.contains("number") {
            "number"
        } else if lower.contains("select") || lower.contains("status") {
            "select"
        } else if lower.contains("date") {
            "date"
        } else if lower.contains("checkbox") {
            "checkbox"
        } else {
            "text"
        };

        return LLMCommandEnvelope {
            explanation: Some(format!("Added column '{col_name}' ({ptype}).")),
            actions: vec![LLMTaskSpec::AddColumn {
                name: col_name,
                property_type: ptype.to_string(),
            }],
        };
    }

    // 3. Delete Row intent (`delete row ...` or `remove row ...`)
    if lower.contains("delete row") || lower.contains("remove row") {
        let match_text = prompt
            .split_once("row")
            .map(|(_, r)| {
                r.trim()
                    .trim_start_matches("where ")
                    .trim_start_matches("matching ")
                    .trim_matches(['"', '\'', '.'])
                    .to_string()
            })
            .unwrap_or_default();

        return LLMCommandEnvelope {
            explanation: Some(format!("Deleted row(s) matching '{match_text}'.")),
            actions: vec![LLMTaskSpec::DeleteRow { match_text }],
        };
    }

    // 4. Add/Remove Single or Multiple Tags on MultiSelect (`add tag 'B2C' to ChatGPT`, `remove tag 'B2C' from ChatGPT`)
    if lower.starts_with("add tag ") || lower.starts_with("remove tag ") {
        let is_remove = lower.starts_with("remove tag ");
        let body = if is_remove {
            &prompt.trim()[11..]
        } else {
            &prompt.trim()[8..]
        };
        let split_pos = if is_remove {
            body.to_ascii_lowercase().rfind(" from ")
        } else {
            body.to_ascii_lowercase().rfind(" to ")
        };
        if let Some(idx) = split_pos {
            let sep_len = if is_remove { 6 } else { 4 };
            let tag_str = body[..idx].trim().trim_matches(['"', '\'']).to_string();
            let target_row_str = body[idx + sep_len..]
                .trim()
                .trim_matches(['"', '\'', '.'])
                .to_string();

            let multi_col = active_columns
                .iter()
                .find(|c| c.property_type == ColumnPropertyType::MultiSelect)
                .or_else(|| {
                    active_columns
                        .iter()
                        .find(|c| c.property_type == ColumnPropertyType::Select)
                });

            if let Some(col) = multi_col {
                let mut values = HashMap::new();
                values.insert(col.name.clone(), Value::String(tag_str.clone()));
                return LLMCommandEnvelope {
                    explanation: Some(format!(
                        "{} tag '{}' on '{}'",
                        if is_remove { "Removed" } else { "Added" },
                        tag_str,
                        target_row_str
                    )),
                    actions: vec![LLMTaskSpec::UpdateRow {
                        match_text: target_row_str,
                        values,
                        append_tags: !is_remove,
                        remove_tags: is_remove,
                    }],
                };
            }
        }
    }

    // 5. Natural `Set <Column> to <Val1, Val2> for <Row>` (`Set Department to Software and Innovation for row 1`)
    if lower.starts_with("set ") && (lower.contains(" for ") || lower.contains(" on ")) {
        let body = &prompt.trim()[4..];
        let split_kw = if body.to_ascii_lowercase().contains(" for ") {
            " for "
        } else {
            " on "
        };
        if let Some(for_idx) = body.to_ascii_lowercase().rfind(split_kw) {
            let assignment_part = &body[..for_idx];
            let target_row_str = body[for_idx + split_kw.len()..]
                .trim()
                .trim_matches(['"', '\'', '.'])
                .to_string();

            if let Some(to_idx) = assignment_part.to_ascii_lowercase().find(" to ") {
                let col_part = assignment_part[..to_idx].trim();
                let val_part = assignment_part[to_idx + 4..].trim();
                let synthetic = format!("{col_part} = {val_part}");
                let values = parse_key_value_pairs_from_prompt(&synthetic, active_columns);
                if !values.is_empty() {
                    return LLMCommandEnvelope {
                        explanation: Some(format!("Updated '{target_row_str}' via PyraAI.")),
                        actions: vec![LLMTaskSpec::UpdateRow {
                            match_text: target_row_str,
                            values,
                            append_tags: false,
                            remove_tags: false,
                        }],
                    };
                }
            }
        }
    }

    // 6. Update Row intent (`update row 3 Department = Software, Innovation` or `edit row ChatGPT Payment = Yearly`)
    if lower.contains("update row") || lower.contains("edit row") {
        let after_row = prompt
            .split_once("row")
            .map(|(_, r)| r.trim())
            .unwrap_or("");
        // Extract the row identifier before the first column name + `=`/`:`
        let mut match_text = String::new();
        let mut kv_start_slice = after_row;

        if let Some(eq_idx) = after_row.find(['=', ':']) {
            let before_eq = after_row[..eq_idx].trim();
            let words: Vec<&str> = before_eq.split_whitespace().collect();
            if words.len() >= 2 {
                // Check if the last 1 or 2 words match a column name
                let last_two = if words.len() >= 3 {
                    format!("{} {}", words[words.len() - 2], words[words.len() - 1])
                } else {
                    String::new()
                };
                let two_word_col = !last_two.is_empty()
                    && active_columns
                        .iter()
                        .any(|c| c.name.eq_ignore_ascii_case(&last_two));
                let col_word_count = if two_word_col { 2 } else { 1 };
                let row_words = &words[..words.len() - col_word_count];
                match_text = row_words
                    .join(" ")
                    .trim_matches([',', '"', '\''])
                    .trim()
                    .to_string();
                let col_part = words[words.len() - col_word_count..].join(" ");
                let after_eq = &after_row[eq_idx..];
                let rebuilt = format!("{col_part}{after_eq}");
                let values = parse_key_value_pairs_from_prompt(&rebuilt, active_columns);
                return LLMCommandEnvelope {
                    explanation: Some(format!("Updated row '{match_text}' via PyraAI.")),
                    actions: vec![LLMTaskSpec::UpdateRow {
                        match_text,
                        values,
                        append_tags: false,
                        remove_tags: false,
                    }],
                };
            }
        } else {
            kv_start_slice = after_row;
        }

        let values = parse_key_value_pairs_from_prompt(kv_start_slice, active_columns);
        return LLMCommandEnvelope {
            explanation: Some(format!("Updated row '{match_text}' via PyraAI.")),
            actions: vec![LLMTaskSpec::UpdateRow {
                match_text,
                values,
                append_tags: false,
                remove_tags: false,
            }],
        };
    }

    // 7. Add Row intent (`add row Service = Canva, Department = Marketing, B2C, Payment = Yearly`)
    if lower.contains("add row") || lower.contains("add a row") || lower.contains("insert row") {
        let values = parse_key_value_pairs_from_prompt(prompt, active_columns);
        let mut final_values = values;
        if final_values.is_empty() {
            if let Some(first_col) = active_columns.first() {
                let fallback_text = prompt
                    .split_once("row")
                    .map(|(_, r)| r.trim().trim_matches(['"', '\'', ':']).to_string())
                    .filter(|s| !s.is_empty())
                    .unwrap_or_else(|| "New AI Row".to_string());
                final_values.insert(first_col.name.clone(), Value::String(fallback_text));
            }
        }
        return LLMCommandEnvelope {
            explanation: Some("Inserted new row via PyraAI.".into()),
            actions: vec![LLMTaskSpec::AddRow {
                values: final_values,
            }],
        };
    }

    LLMCommandEnvelope {
        explanation: None,
        actions: Vec::new(),
    }
}
