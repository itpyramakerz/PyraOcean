use crate::auth::{
    record_audit_log, resolve_grid_access_cached, resolve_space_access_cached, AppState, AuthError,
    AuthUser, RequireSuperAdmin,
};
use axum::{extract::State, Json};
use chrono::{NaiveDate, NaiveTime, Utc};
use pyra_ocean_shared::{
    compute_column_calculation, format_cell_for_export, format_number_with_commas, AiAttachment,
    AiCopilotRequest, AiCopilotResponse, AiExecutedAction, AiNavigationTarget, AiSettings,
    CalcMode, CellValue, ChecklistItem, ColumnCalcSummary, ColumnPropertyConfig,
    ColumnPropertyType, GridColumn, GridRow, NumberFormat, PageGrid, RowCellMap, SelectOption,
    SystemRole, TagColor,
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
    RenameColumn {
        #[serde(alias = "old", alias = "from", alias = "name")]
        old_name: String,
        #[serde(alias = "new", alias = "to")]
        new_name: String,
    },
    DeleteColumn {
        #[serde(alias = "column", alias = "col")]
        name: String,
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
    Navigate {
        #[serde(default, alias = "query", alias = "name", alias = "match_text")]
        target: String,
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

    // 1. Load Accessible Workspace Grids across ALL Spaces for Omnipotent Cross-Space Search, Navigation & Task Execution
    let is_super_admin = auth_user.system_role == SystemRole::SuperAdmin;
    let accessible_grids: Vec<(Uuid, Uuid, String, String, Option<String>)> = sqlx::query_as(
        r#"
        SELECT g.id, g.space_id, s.name AS space_name, g.title, g.icon
        FROM pages_grids g
        JOIN spaces s ON s.id = g.space_id
        WHERE g.deleted_at IS NULL AND g.is_archived = FALSE AND s.is_archived = FALSE
          AND (
              $1 = TRUE
              OR EXISTS (SELECT 1 FROM space_members sm WHERE sm.space_id = g.space_id AND sm.user_id = $2)
              OR EXISTS (SELECT 1 FROM page_grid_permissions gp WHERE gp.grid_id = g.id AND gp.user_id = $2)
          )
        ORDER BY s.name ASC, g.position ASC
        LIMIT 80
        "#,
    )
    .bind(is_super_admin)
    .bind(auth_user.user_id)
    .fetch_all(&state.db)
    .await?;

    let all_accessible_grid_ids: Vec<Uuid> =
        accessible_grids.iter().map(|(gid, ..)| *gid).collect();

    let all_workspace_columns: Vec<GridColumn> = if all_accessible_grid_ids.is_empty() {
        Vec::new()
    } else {
        sqlx::query_as::<_, GridColumn>(
            "SELECT id, grid_id, name, property_type, property_config, width_px, position, is_primary, is_hidden, created_at, updated_at FROM grid_columns WHERE grid_id = ANY($1) ORDER BY grid_id, position ASC, created_at ASC",
        )
        .bind(&all_accessible_grid_ids)
        .fetch_all(&state.db)
        .await
        .unwrap_or_default()
    };

    let all_workspace_rows: Vec<GridRow> = if all_accessible_grid_ids.is_empty() {
        Vec::new()
    } else {
        sqlx::query_as::<_, GridRow>(
            "SELECT id, grid_id, position, row_order, cells, created_by, updated_by, created_at, last_modified FROM grid_rows WHERE grid_id = ANY($1) ORDER BY grid_id, position ASC, row_order ASC, created_at ASC LIMIT 2500",
        )
        .bind(&all_accessible_grid_ids)
        .fetch_all(&state.db)
        .await
        .unwrap_or_default()
    };

    let mut grid_columns_map: HashMap<Uuid, Vec<GridColumn>> = HashMap::new();
    for col in all_workspace_columns {
        grid_columns_map.entry(col.grid_id).or_default().push(col);
    }

    let mut grid_rows_map: HashMap<Uuid, Vec<GridRow>> = HashMap::new();
    for r in all_workspace_rows {
        grid_rows_map.entry(r.grid_id).or_default().push(r);
    }

    // 2. Load Active Grid Context (or auto-resolve target Grid from prompt across all Spaces)
    let mut active_grid: Option<PageGrid> = None;
    let mut active_columns: Vec<GridColumn> = Vec::new();
    let mut active_rows: Vec<GridRow> = Vec::new();
    let mut can_edit_active_grid = false;

    let lower_prompt_for_target = effective_prompt.to_ascii_lowercase();
    let prompt_matched_grid_id: Option<Uuid> = accessible_grids
        .iter()
        .filter(|(_, _, _, gtitle, _)| {
            let gt = gtitle.trim().to_ascii_lowercase();
            gt.len() >= 3 && lower_prompt_for_target.contains(&gt)
        })
        .max_by_key(|(_, _, _, gtitle, _)| gtitle.len())
        .map(|(gid, ..)| *gid);

    let resolved_initial_grid_id = req
        .grid_id
        .or(prompt_matched_grid_id)
        .or_else(|| accessible_grids.first().map(|(gid, ..)| *gid));

    if let Some(gid) = resolved_initial_grid_id {
        if let Ok(access) = resolve_grid_access_cached(&state, auth_user.clone(), gid).await {
            can_edit_active_grid = access.effective_space_role.can_write();
            active_grid = sqlx::query_as::<_, PageGrid>(
                "SELECT id, space_id, title, description, icon, position, created_by, is_archived, created_at, updated_at, deleted_at, deleted_by FROM pages_grids WHERE id = $1 AND deleted_at IS NULL",
            )
            .bind(gid)
            .fetch_optional(&state.db)
            .await?;

            if active_grid.is_some() {
                active_columns = grid_columns_map.get(&gid).cloned().unwrap_or_default();
                active_rows = grid_rows_map.get(&gid).cloned().unwrap_or_default();
            }
        }
    }

    // 3. Build Deterministic Context Digest, Cross-Space Search Targets & Pre-Computed Analytics Report
    let (mut context_digest, mut analytics_markdown, matched_rows_count, mut navigation_targets) =
        build_workspace_context_and_report(
            &active_grid,
            &active_columns,
            &active_rows,
            &accessible_grids,
            &grid_columns_map,
            &grid_rows_map,
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

    let pre_parsed_env = fallback_intent_parser(effective_prompt, &active_columns);
    let mut mode = req.mode.trim().to_ascii_lowercase();
    if mode != "execute"
        && (prompt_is_explicit_task_command(effective_prompt)
            || !pre_parsed_env.actions.is_empty()
            || wants_attachment_import)
    {
        mode = "execute".to_string();
    }
    let mut executed_actions: Vec<AiExecutedAction> = Vec::new();
    let mut navigated_space_id: Option<Uuid> = None;
    let mut navigated_grid_id: Option<Uuid> = None;
    let mut navigated_row_id: Option<Uuid> = None;

    // 4. Mode == "execute" -> Autonomous Task Execution (Create Grid, Add/Rename/Delete Column, Add/Update/Delete Row, Navigate)
    if mode == "execute" {
        let exec_system_prompt = format!(
            "{}\n\nYou are operating in AUTONOMOUS TASK EXECUTION MODE for PyraOcean.\n\
            When the user asks for multiple tasks in one prompt (e.g. adding multiple columns, renaming/deleting columns, adding/updating rows, or navigating), you MUST include EVERY requested task in order in the `actions` array (or inside a ```json_actions [ ... ] ``` array).\n\
            Convert the user's instruction into a valid JSON object with the exact structure:\n\
            {{\n\
              \"explanation\": \"Brief summary of all tasks executed\",\n\
              \"actions\": [\n\
                {{\"action\": \"create_grid\", \"name\": \"Project Tracker\", \"icon\": \"🚀\", \"columns\": [{{\"name\": \"Task\", \"property_type\": \"text\"}}, {{\"name\": \"Status\", \"property_type\": \"select\"}}, {{\"name\": \"Department\", \"property_type\": \"multi_select\"}}, {{\"name\": \"Budget\", \"property_type\": \"currency\"}}], \"rows\": [{{\"Task\": \"Launch MVP\", \"Status\": \"In Progress\", \"Department\": [\"Software\", \"Innovation\"], \"Budget\": 25000}}]}},\n\
                {{\"action\": \"add_column\", \"name\": \"username\", \"property_type\": \"text\"}},\n\
                {{\"action\": \"add_column\", \"name\": \"Size\", \"property_type\": \"text\"}},\n\
                {{\"action\": \"rename_column\", \"old_name\": \"OldName\", \"new_name\": \"NewName\"}},\n\
                {{\"action\": \"delete_column\", \"name\": \"ObsoleteColumn\"}},\n\
                {{\"action\": \"add_row\", \"values\": {{\"Service\": \"Canva\", \"Department\": [\"Marketing\", \"B2C\"], \"Payment\": \"Yearly\", \"Cost\": 1500}}}},\n\
                {{\"action\": \"update_row\", \"match_text\": \"ChatGPT\", \"values\": {{\"Department\": [\"Software\", \"B2C\", \"Innovation\"]}}, \"append_tags\": false, \"remove_tags\": false}},\n\
                {{\"action\": \"delete_row\", \"match_text\": \"row keyword or row number\"}},\n\
                {{\"action\": \"navigate\", \"target\": \"Grid name, Space name, or Row keyword\"}}\n\
              ]\n\
            }}\n\
            For Select columns, pass the option label string (e.g. \"Marketing\" or \"Yearly\").\n\
            For MultiSelect columns, pass either a JSON array of tag strings (e.g. [\"Software\", \"B2C\", \"Innovation\"]) or a comma-separated string (\"Software, B2C, Innovation\").\n\
            If the user asks to ADD/APPEND a tag to an existing MultiSelect cell without replacing existing tags, set \"append_tags\": true on update_row.\n\
            If the user asks to REMOVE a tag from an existing MultiSelect cell, set \"remove_tags\": true on update_row.\n\
            Allowed property_type values: text, number, currency, select, multi_select, date, checkbox, checklist, person, password.\n\
            Output ONLY raw JSON or a ```json_actions [ ... ] ``` block.\n\n\
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

        let mut deterministic_env = pre_parsed_env;
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

        let mut next_col_position = active_columns
            .iter()
            .map(|c| c.position)
            .max()
            .unwrap_or(-1)
            + 1;

        for spec in envelope.actions {
            match spec {
                LLMTaskSpec::Navigate { target } => {
                    let needle = target.trim().to_ascii_lowercase();
                    // 1. Check if `target` matches any Grid title or Space name
                    let grid_hit = accessible_grids
                        .iter()
                        .find(|(_, _, sname, gtitle, _)| {
                            gtitle.eq_ignore_ascii_case(target.trim())
                                || gtitle.to_ascii_lowercase().contains(&needle)
                                || sname.eq_ignore_ascii_case(target.trim())
                                || sname.to_ascii_lowercase().contains(&needle)
                        });
                    if let Some((gid, sid, sname, gtitle, gicon)) = grid_hit {
                        navigated_space_id = Some(*sid);
                        navigated_grid_id = Some(*gid);
                        navigated_row_id = None;
                        let nav_item = AiNavigationTarget {
                            space_id: *sid,
                            space_name: sname.clone(),
                            grid_id: *gid,
                            grid_name: gtitle.clone(),
                            grid_icon: gicon.clone(),
                            row_id: None,
                            row_number: None,
                            primary_label: gtitle.clone(),
                            matched_preview: format!("Space: {} → Grid: {}", sname, gtitle),
                        };
                        if !navigation_targets
                            .iter()
                            .any(|t| t.grid_id == *gid && t.row_id.is_none())
                        {
                            navigation_targets.insert(0, nav_item);
                        }
                        executed_actions.push(AiExecutedAction {
                            action_type: "navigate".into(),
                            summary: format!("Navigated to {} / {}", sname, gtitle),
                            space_id: Some(*sid),
                            grid_id: Some(*gid),
                            row_id: None,
                        });
                    } else if let Some(first_row_hit) = navigation_targets.first().cloned() {
                        navigated_space_id = Some(first_row_hit.space_id);
                        navigated_grid_id = Some(first_row_hit.grid_id);
                        navigated_row_id = first_row_hit.row_id;
                        executed_actions.push(AiExecutedAction {
                            action_type: "navigate".into(),
                            summary: format!(
                                "Navigated to '{}' in {} / {}",
                                first_row_hit.primary_label,
                                first_row_hit.space_name,
                                first_row_hit.grid_name
                            ),
                            space_id: Some(first_row_hit.space_id),
                            grid_id: Some(first_row_hit.grid_id),
                            row_id: first_row_hit.row_id,
                        });
                    }
                }

                LLMTaskSpec::CreateGrid {
                    name,
                    icon,
                    columns,
                    rows,
                } => {
                    // Check if user specified a Space name in the prompt
                    let matched_space_from_prompt = accessible_grids
                        .iter()
                        .find(|(_, _, sname, _, _)| {
                            let sl = sname.to_ascii_lowercase();
                            sl.len() >= 3 && lower_prompt.contains(&sl)
                        })
                        .map(|(_, sid, _, _, _)| *sid);

                    let target_space_id = if let Some(sid) = matched_space_from_prompt {
                        sid
                    } else if let Some(sid) = req.space_id {
                        sid
                    } else if let Some(ref g) = active_grid {
                        g.space_id
                    } else if let Some((_, sid, ..)) = accessible_grids.first() {
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

                    let space_name_str = accessible_grids
                        .iter()
                        .find(|(_, sid, ..)| *sid == target_space_id)
                        .map(|(_, _, sname, ..)| sname.clone())
                        .unwrap_or_else(|| "Workspace".into());

                    state.cache.invalidate_space(&target_space_id);
                    navigated_space_id = Some(target_space_id);
                    navigated_grid_id = Some(new_grid.id);
                    active_grid = Some(new_grid.clone());
                    active_columns = created_cols.clone();
                    next_col_position = active_columns.len() as i32;
                    can_edit_active_grid = true;
                    navigation_targets.insert(
                        0,
                        AiNavigationTarget {
                            space_id: target_space_id,
                            space_name: space_name_str,
                            grid_id: new_grid.id,
                            grid_name: clean_name.clone(),
                            grid_icon: Some(clean_icon),
                            row_id: None,
                            row_number: None,
                            primary_label: clean_name.clone(),
                            matched_preview: format!(
                                "Created with {} columns and {} rows",
                                created_cols.len(),
                                inserted_rows_count
                            ),
                        },
                    );
                    executed_actions.push(AiExecutedAction {
                        action_type: "create_grid".into(),
                        summary: format!(
                            "Created grid '{}' with {} columns and {} rows",
                            clean_name,
                            created_cols.len(),
                            inserted_rows_count
                        ),
                        space_id: Some(target_space_id),
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
                    let clean_col_name = name
                        .trim()
                        .trim_start_matches(['=', ':'])
                        .trim()
                        .trim_matches(['"', '\''])
                        .trim();
                    if clean_col_name.is_empty() {
                        continue;
                    }
                    let (ptype, pcfg) = parse_property_type_and_config(&property_type);
                    let col_pos = next_col_position;
                    next_col_position += 1;

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
                    .bind(col_pos)
                    .fetch_one(&state.db)
                    .await?;

                    active_columns.push(new_col);
                    state.cache.invalidate_grid(&g.id);
                    if req.grid_id != Some(g.id) {
                        navigated_space_id = Some(g.space_id);
                        navigated_grid_id = Some(g.id);
                    }
                    executed_actions.push(AiExecutedAction {
                        action_type: "add_column".into(),
                        summary: format!(
                            "Added column '{}' ({:?}) to '{}'",
                            clean_col_name, ptype, g.title
                        ),
                        space_id: Some(g.space_id),
                        grid_id: Some(g.id),
                        row_id: None,
                    });
                }

                LLMTaskSpec::RenameColumn { old_name, new_name } => {
                    let Some(ref g) = active_grid else {
                        return Err(AuthError::BadRequest(
                            "Open a Grid first before asking PyraAI to rename a column.".into(),
                        ));
                    };
                    if !can_edit_active_grid {
                        return Err(AuthError::Forbidden {
                            required: "space editor or space_admin",
                        });
                    }
                    let clean_old = old_name.trim().trim_matches(['"', '\'']).trim();
                    let clean_new = new_name
                        .trim()
                        .trim_start_matches(['=', ':'])
                        .trim()
                        .trim_matches(['"', '\''])
                        .trim();
                    if clean_old.is_empty() || clean_new.is_empty() {
                        continue;
                    }
                    if let Some(col) = active_columns.iter_mut().find(|c| {
                        c.name.eq_ignore_ascii_case(clean_old)
                            || c.name
                                .to_ascii_lowercase()
                                .contains(&clean_old.to_ascii_lowercase())
                    }) {
                        let prev_name = col.name.clone();
                        sqlx::query(
                            "UPDATE grid_columns SET name = $1, updated_at = NOW() WHERE id = $2 AND grid_id = $3",
                        )
                        .bind(clean_new)
                        .bind(col.id)
                        .bind(g.id)
                        .execute(&state.db)
                        .await?;

                        col.name = clean_new.to_string();
                        state.cache.invalidate_grid(&g.id);
                        if req.grid_id != Some(g.id) {
                            navigated_space_id = Some(g.space_id);
                            navigated_grid_id = Some(g.id);
                        }
                        executed_actions.push(AiExecutedAction {
                            action_type: "rename_column".into(),
                            summary: format!("Renamed column '{}' to '{}'", prev_name, clean_new),
                            space_id: Some(g.space_id),
                            grid_id: Some(g.id),
                            row_id: None,
                        });
                    }
                }

                LLMTaskSpec::DeleteColumn { name } => {
                    let Some(ref g) = active_grid else {
                        return Err(AuthError::BadRequest(
                            "Open a Grid first before asking PyraAI to delete a column.".into(),
                        ));
                    };
                    if !can_edit_active_grid {
                        return Err(AuthError::Forbidden {
                            required: "space editor or space_admin",
                        });
                    }
                    let clean_col = name
                        .trim()
                        .trim_start_matches(['=', ':'])
                        .trim()
                        .trim_matches(['"', '\''])
                        .trim();
                    if clean_col.is_empty() {
                        continue;
                    }
                    if let Some(pos) = active_columns
                        .iter()
                        .position(|c| c.name.eq_ignore_ascii_case(clean_col))
                    {
                        let removed_col = active_columns.remove(pos);
                        sqlx::query("DELETE FROM grid_columns WHERE id = $1 AND grid_id = $2")
                            .bind(removed_col.id)
                            .bind(g.id)
                            .execute(&state.db)
                            .await?;

                        sqlx::query(
                            "UPDATE grid_rows SET cells = cells - $1, last_modified = NOW() WHERE grid_id = $2",
                        )
                        .bind(removed_col.id.to_string())
                        .bind(g.id)
                        .execute(&state.db)
                        .await?;

                        for r in &mut active_rows {
                            r.cells.remove(&removed_col.id);
                        }
                        state.cache.invalidate_grid(&g.id);
                        if req.grid_id != Some(g.id) {
                            navigated_space_id = Some(g.space_id);
                            navigated_grid_id = Some(g.id);
                        }
                        executed_actions.push(AiExecutedAction {
                            action_type: "delete_column".into(),
                            summary: format!("Deleted column '{}'", removed_col.name),
                            space_id: Some(g.space_id),
                            grid_id: Some(g.id),
                            row_id: None,
                        });
                    }
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

                    let inserted_row: GridRow = sqlx::query_as(
                        r#"
                        INSERT INTO grid_rows (grid_id, position, row_order, cells, created_by, updated_by)
                        VALUES ($1, $2, $3, $4, $5, $5)
                        RETURNING id, grid_id, position, row_order, cells, created_by, updated_by, created_at, last_modified
                        "#,
                    )
                    .bind(g.id)
                    .bind(next_pos)
                    .bind(next_row_order)
                    .bind(sqlx::types::Json(&cells))
                    .bind(auth_user.user_id)
                    .fetch_one(&state.db)
                    .await?;

                    let new_row_id = inserted_row.id;
                    let row_num = active_rows.len() + 1;
                    let (prim_label, preview_str) = summarize_row_for_navigation(
                        &active_columns,
                        &inserted_row,
                        row_num,
                        &user_display_map,
                    );
                    let space_name_str = accessible_grids
                        .iter()
                        .find(|(gid, ..)| *gid == g.id)
                        .map(|(_, _, sname, ..)| sname.clone())
                        .unwrap_or_else(|| "Workspace".into());

                    active_rows.push(inserted_row);
                    state.cache.invalidate_grid(&g.id);
                    if req.grid_id != Some(g.id) {
                        navigated_space_id = Some(g.space_id);
                        navigated_grid_id = Some(g.id);
                    }
                    navigated_row_id = Some(new_row_id);
                    navigation_targets.insert(
                        0,
                        AiNavigationTarget {
                            space_id: g.space_id,
                            space_name: space_name_str,
                            grid_id: g.id,
                            grid_name: g.title.clone(),
                            grid_icon: g.icon.clone(),
                            row_id: Some(new_row_id),
                            row_number: Some(row_num),
                            primary_label: prim_label,
                            matched_preview: preview_str,
                        },
                    );
                    executed_actions.push(AiExecutedAction {
                        action_type: "add_row".into(),
                        summary: format!("Inserted new row into '{}'", g.title),
                        space_id: Some(g.space_id),
                        grid_id: Some(g.id),
                        row_id: Some(new_row_id),
                    });
                }

                LLMTaskSpec::UpdateRow {
                    match_text,
                    values,
                    append_tags,
                    remove_tags,
                } => {
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

                    let mut target_row_idx = if let Some(one_based) = row_index_match {
                        if one_based - 1 < active_rows.len() {
                            Some(one_based - 1)
                        } else {
                            None
                        }
                    } else {
                        active_rows.iter().position(|r| {
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

                    // Omnipotent Cross-Space Fallback: if not found in active_grid, search across ALL accessible Spaces & Grids!
                    if target_row_idx.is_none() && !needle.is_empty() {
                        for (cand_gid, _cand_sid, _cand_sname, _cand_gtitle, _cand_icon) in
                            &accessible_grids
                        {
                            if active_grid.as_ref().map(|g| g.id) == Some(*cand_gid) {
                                continue;
                            }
                            let Some(cand_cols) = grid_columns_map.get(cand_gid) else {
                                continue;
                            };
                            let Some(cand_rows) = grid_rows_map.get(cand_gid) else {
                                continue;
                            };
                            if let Some(found_idx) = cand_rows.iter().position(|r| {
                                cand_cols.iter().any(|col| {
                                    resolve_row_cell_display(
                                        col,
                                        r.cells.get(&col.id),
                                        &user_display_map,
                                        r.last_modified,
                                    )
                                    .to_ascii_lowercase()
                                    .contains(&needle)
                                })
                            }) {
                                if let Ok(access) = resolve_grid_access_cached(
                                    &state,
                                    auth_user.clone(),
                                    *cand_gid,
                                )
                                .await
                                {
                                    if access.effective_space_role.can_write() {
                                        if let Ok(Some( switched_grid )) = sqlx::query_as::<_, PageGrid>(
                                            "SELECT id, space_id, title, description, icon, position, created_by, is_archived, created_at, updated_at, deleted_at, deleted_by FROM pages_grids WHERE id = $1 AND deleted_at IS NULL",
                                        )
                                        .bind(*cand_gid)
                                        .fetch_optional(&state.db)
                                        .await
                                        {
                                            active_grid = Some(switched_grid);
                                            active_columns = cand_cols.clone();
                                            active_rows = cand_rows.clone();
                                            can_edit_active_grid = true;
                                            target_row_idx = Some(found_idx);
                                            break;
                                        }
                                    }
                                }
                            }
                        }
                    }

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

                    if let Some(r_idx) = target_row_idx {
                        let row_id = active_rows[r_idx].id;
                        let mut updated_cells = active_rows[r_idx].cells.clone();
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
                        .bind(row_id)
                        .bind(g.id)
                        .execute(&state.db)
                        .await?;

                        active_rows[r_idx].cells = updated_cells;
                        let (prim_label, preview_str) = summarize_row_for_navigation(
                            &active_columns,
                            &active_rows[r_idx],
                            r_idx + 1,
                            &user_display_map,
                        );
                        let space_name_str = accessible_grids
                            .iter()
                            .find(|(gid, ..)| *gid == g.id)
                            .map(|(_, _, sname, ..)| sname.clone())
                            .unwrap_or_else(|| "Workspace".into());

                        state.cache.invalidate_grid(&g.id);
                        if req.grid_id != Some(g.id) {
                            navigated_space_id = Some(g.space_id);
                            navigated_grid_id = Some(g.id);
                        }
                        navigated_row_id = Some(row_id);
                        navigation_targets.insert(
                            0,
                            AiNavigationTarget {
                                space_id: g.space_id,
                                space_name: space_name_str.clone(),
                                grid_id: g.id,
                                grid_name: g.title.clone(),
                                grid_icon: g.icon.clone(),
                                row_id: Some(row_id),
                                row_number: Some(r_idx + 1),
                                primary_label: prim_label,
                                matched_preview: preview_str,
                            },
                        );
                        executed_actions.push(AiExecutedAction {
                            action_type: "update_row".into(),
                            summary: format!(
                                "Updated row matching '{}' in {} / {}",
                                match_text, space_name_str, g.title
                            ),
                            space_id: Some(g.space_id),
                            grid_id: Some(g.id),
                            row_id: Some(row_id),
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

                            active_rows.retain(|r| !matching_ids.contains(&r.id));
                            state.cache.invalidate_grid(&g.id);
                            executed_actions.push(AiExecutedAction {
                                action_type: "delete_row".into(),
                                summary: format!(
                                    "Deleted {} row(s) matching '{}'",
                                    res.rows_affected(),
                                    match_text
                                ),
                                space_id: Some(g.space_id),
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
            || !navigation_targets.is_empty()
            || prompt_trimmed.to_ascii_lowercase().starts_with("search ")
            || prompt_trimmed.to_ascii_lowercase().starts_with("show ")
            || prompt_trimmed.to_ascii_lowercase().starts_with("find ")
            || prompt_trimmed.to_ascii_lowercase().starts_with("where ")
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
            navigated_space_id,
            navigated_grid_id,
            navigated_row_id,
            navigation_targets,
        }));
    }

    // 5. Mode == "report" or "ask" -> Natural Language Cross-Space Search & Executive Report Generation
    let is_direct_lookup = lower_prompt.starts_with("where is ")
        || lower_prompt.starts_with("find ")
        || lower_prompt.starts_with("locate ")
        || lower_prompt.starts_with("go to ")
        || lower_prompt.starts_with("open ")
        || lower_prompt.starts_with("navigate to ")
        || lower_prompt.starts_with("take me to ")
        || lower_prompt.starts_with("jump to ");

    if let Some(best_target) = navigation_targets.first() {
        if is_direct_lookup
            || (navigation_targets.len() == 1 && req.grid_id != Some(best_target.grid_id))
        {
            navigated_space_id = Some(best_target.space_id);
            navigated_grid_id = Some(best_target.grid_id);
            navigated_row_id = best_target.row_id;
        } else if req.grid_id == Some(best_target.grid_id) && best_target.row_id.is_some() {
            navigated_row_id = best_target.row_id;
        }
    }

    let mode_instruction = if mode == "report" {
        "Generate a comprehensive, executive-ready Markdown report with: (1) Executive Overview, (2) Financial & Numeric Totals (including Dual-Currency EGP / USD totals and averages where present), (3) Category / Department Breakdown, and (4) Key Actionable Recommendations."
    } else {
        "Answer the user's question accurately using the provided PyraOcean Grid & Cross-Space Workspace data. Always cite exact Space names, Grid names, Row numbers, statuses, and values so the user knows exactly where each match lives."
    };

    let full_system_prompt = format!(
        "{}\n\n{}\n\nWORKSPACE & GRID DATA CONTEXT:\n{}\n\nPRE-COMPUTED ANALYTICS & CROSS-SPACE MATCHES:\n{}",
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
            navigated_space_id,
            navigated_grid_id,
            navigated_row_id,
            navigation_targets,
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
                navigated_space_id,
                navigated_grid_id,
                navigated_row_id,
                navigation_targets,
            }))
        }
    }
}

// ============================================================================
// 5. Context Builder, Deterministic Analytics & Fallback Intent Parser
// ============================================================================

fn summarize_row_for_navigation(
    columns: &[GridColumn],
    row: &GridRow,
    one_based_index: usize,
    user_display_map: &HashMap<Uuid, String>,
) -> (String, String) {
    let mut primary_label = String::new();
    let mut preview_parts: Vec<String> = Vec::new();

    for col in columns.iter().filter(|c| !c.is_hidden) {
        let val_str = resolve_row_cell_display(
            col,
            row.cells.get(&col.id),
            user_display_map,
            row.last_modified,
        )
        .replace(['\t', '\n', '\r'], " ");
        let clean = val_str.trim();
        if clean.is_empty() {
            continue;
        }
        if primary_label.is_empty() && (col.is_primary || preview_parts.is_empty()) {
            primary_label = clean.to_string();
        }
        if preview_parts.len() < 4 {
            preview_parts.push(format!("{}: {}", col.name, clean));
        }
    }

    if primary_label.is_empty() {
        primary_label = format!("Row #{}", one_based_index);
    }
    let preview = if preview_parts.is_empty() {
        format!("Row #{}", one_based_index)
    } else {
        preview_parts.join(" · ")
    };
    (primary_label, preview)
}

#[allow(clippy::too_many_arguments)]
fn build_workspace_context_and_report(
    active_grid: &Option<PageGrid>,
    columns: &[GridColumn],
    rows: &[GridRow],
    accessible_grids: &[(Uuid, Uuid, String, String, Option<String>)],
    grid_columns_map: &HashMap<Uuid, Vec<GridColumn>>,
    grid_rows_map: &HashMap<Uuid, Vec<GridRow>>,
    user_display_map: &HashMap<Uuid, String>,
    prompt: &str,
) -> (String, String, usize, Vec<AiNavigationTarget>) {
    let mut ctx = String::new();
    let mut report = String::new();
    let mut matched_count = 0usize;
    let mut navigation_targets: Vec<AiNavigationTarget> = Vec::new();

    ctx.push_str("Accessible PyraOcean Workspace Spaces & Grids:\n");
    for (gid, _sid, sname, gname, gicon) in accessible_grids.iter().take(40) {
        let rcount = grid_rows_map.get(gid).map(|v| v.len()).unwrap_or(0);
        let cnames = grid_columns_map
            .get(gid)
            .map(|cols| {
                cols.iter()
                    .filter(|c| !c.is_hidden)
                    .map(|c| c.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .unwrap_or_default();
        ctx.push_str(&format!(
            "- [{}] {} / {} {} (Rows: {}, Columns: [{}])\n",
            gid,
            sname,
            gicon.as_deref().unwrap_or("📄"),
            gname,
            rcount,
            cnames
        ));
    }

    let stop_words = [
        "what", "where", "which", "show", "find", "list", "give", "report", "summary", "the",
        "and", "for", "with", "from", "that", "this", "have", "rows", "grid", "all", "about",
        "search", "filter", "department", "status", "payment", "category", "tags", "tag", "are",
        "open", "goto", "navigate", "take", "jump", "space", "into", "please", "can", "you",
    ];
    let keywords: Vec<String> = prompt
        .split(|c: char| !c.is_alphanumeric())
        .map(|w| w.trim().to_ascii_lowercase())
        .filter(|w| w.len() >= 2 && !stop_words.contains(&w.as_str()))
        .collect();

    if let Some(grid) = active_grid {
        let active_space_name = accessible_grids
            .iter()
            .find(|(gid, ..)| *gid == grid.id)
            .map(|(_, _, sname, ..)| sname.clone())
            .unwrap_or_else(|| "Workspace".into());

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
            "\nActive Grid: {} / {} (Total Rows: {})\nColumns & Available Tag Options: {}\n",
            active_space_name,
            grid.title,
            rows.len(),
            col_schema_line
        ));

        report.push_str(&format!(
            "### 📊 Executive Summary: {} {} (`{}`)\n- **Total Rows:** `{}`\n- **Columns (`{}`):** {}\n\n",
            grid.icon.as_deref().unwrap_or("📄"),
            grid.title,
            active_space_name,
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

        let header_tsv = visible_cols
            .iter()
            .map(|c| c.name.as_str())
            .collect::<Vec<_>>()
            .join("\t");
        ctx.push_str(&format!(
            "\nActive Grid Rows (Compact TSV, up to 60 relevant rows):\n#\t{}\n",
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
                let (prim_label, preview_str) =
                    summarize_row_for_navigation(columns, r, idx + 1, user_display_map);
                if navigation_targets.len() < 15 {
                    navigation_targets.push(AiNavigationTarget {
                        space_id: grid.space_id,
                        space_name: active_space_name.clone(),
                        grid_id: grid.id,
                        grid_name: grid.title.clone(),
                        grid_icon: grid.icon.clone(),
                        row_id: Some(r.id),
                        row_number: Some(idx + 1),
                        primary_label: prim_label,
                        matched_preview: preview_str,
                    });
                }
                if matched_rows_md.len() < 50 {
                    matched_rows_md.push(format!(
                        "{}. **[{} / {} — Row #{}]** {}",
                        idx + 1,
                        active_space_name,
                        grid.title,
                        idx + 1,
                        row_line
                    ));
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
            matched_count += matched_rows_md.len();
            report.push_str(&format!(
                "#### 🔍 Matching Rows in Current Grid (`{}` found)\n",
                matched_rows_md.len()
            ));
            for m in matched_rows_md {
                report.push_str(&format!("{m}\n"));
            }
            report.push('\n');
        }
    } else {
        report.push_str("### 🌐 PyraOcean Workspace Overview\n");
        report.push_str(&format!(
            "- **Accessible Grids:** `{}`\n",
            accessible_grids.len()
        ));
        for (_gid, _sid, sname, gname, gicon) in accessible_grids.iter().take(15) {
            report.push_str(&format!(
                "- **{}** → {} `{}`\n",
                sname,
                gicon.as_deref().unwrap_or("📄"),
                gname
            ));
        }
        report.push('\n');
    }

    // Cross-Space Global Search across ALL other accessible Spaces & Grids
    if !keywords.is_empty() {
        let mut cross_space_md: Vec<String> = Vec::new();
        let active_gid = active_grid.as_ref().map(|g| g.id);

        // Also match Grid/Space titles directly
        for (gid, sid, sname, gtitle, gicon) in accessible_grids {
            let combined_title = format!("{} {}", sname, gtitle).to_ascii_lowercase();
            if keywords.iter().any(|kw| combined_title.contains(kw)) {
                if navigation_targets.len() < 15
                    && !navigation_targets
                        .iter()
                        .any(|t| t.grid_id == *gid && t.row_id.is_none())
                {
                    let rcount = grid_rows_map.get(gid).map(|v| v.len()).unwrap_or(0);
                    navigation_targets.push(AiNavigationTarget {
                        space_id: *sid,
                        space_name: sname.clone(),
                        grid_id: *gid,
                        grid_name: gtitle.clone(),
                        grid_icon: gicon.clone(),
                        row_id: None,
                        row_number: None,
                        primary_label: format!("{} / {}", sname, gtitle),
                        matched_preview: format!("Grid in Space '{}' ({} rows)", sname, rcount),
                    });
                }
            }

            if Some(*gid) == active_gid {
                continue;
            }
            let Some(other_cols) = grid_columns_map.get(gid) else {
                continue;
            };
            let Some(other_rows) = grid_rows_map.get(gid) else {
                continue;
            };

            for (r_idx, r) in other_rows.iter().enumerate() {
                let mut pairs: Vec<String> = Vec::new();
                for col in other_cols.iter().filter(|c| !c.is_hidden) {
                    let text = resolve_row_cell_display(
                        col,
                        r.cells.get(&col.id),
                        user_display_map,
                        r.last_modified,
                    )
                    .replace(['\t', '\n', '\r'], " ");
                    let clean = text.trim();
                    if !clean.is_empty() {
                        pairs.push(format!("{}: {}", col.name, clean));
                    }
                }
                let row_line = pairs.join(" | ");
                let lower_line = row_line.to_ascii_lowercase();
                if keywords.iter().any(|kw| lower_line.contains(kw)) {
                    let (prim_label, preview_str) =
                        summarize_row_for_navigation(other_cols, r, r_idx + 1, user_display_map);
                    if navigation_targets.len() < 15 {
                        navigation_targets.push(AiNavigationTarget {
                            space_id: *sid,
                            space_name: sname.clone(),
                            grid_id: *gid,
                            grid_name: gtitle.clone(),
                            grid_icon: gicon.clone(),
                            row_id: Some(r.id),
                            row_number: Some(r_idx + 1),
                            primary_label: prim_label,
                            matched_preview: preview_str,
                        });
                    }
                    if cross_space_md.len() < 35 {
                        cross_space_md.push(format!(
                            "- **[{} / {} — Row #{}]** {}",
                            sname,
                            gtitle,
                            r_idx + 1,
                            row_line
                        ));
                    }
                }
            }
        }

        if !cross_space_md.is_empty() {
            matched_count += cross_space_md.len();
            ctx.push_str("\nCross-Space Matches Found Across Other Spaces & Grids:\n");
            report.push_str(&format!(
                "#### 🧭 Cross-Space Matches Across Other Spaces & Grids (`{}` found)\n",
                cross_space_md.len()
            ));
            for line in cross_space_md {
                ctx.push_str(&line);
                ctx.push('\n');
                report.push_str(&line);
                report.push('\n');
            }
            report.push('\n');
        }
    }

    (ctx, report, matched_count, navigation_targets)
}

fn parse_llm_command_envelope(raw: &str) -> Option<LLMCommandEnvelope> {
    let trimmed = raw.trim();

    // 1. Direct envelope {"explanation": ..., "actions": [...]}
    if let Ok(env) = serde_json::from_str::<LLMCommandEnvelope>(trimmed) {
        if !env.actions.is_empty() || env.explanation.is_some() {
            return Some(env);
        }
    }

    // 2. Direct JSON array [{"action": ...}, {"action": ...}]
    if let Ok(arr) = serde_json::from_str::<Vec<LLMTaskSpec>>(trimmed) {
        if !arr.is_empty() {
            return Some(LLMCommandEnvelope {
                explanation: Some(format!("Executed {} requested task(s).", arr.len())),
                actions: arr,
            });
        }
    }

    // 3. Extract from ```json_actions ... ``` or ```json ... ``` fenced blocks (supports multiple blocks)
    let mut collected_actions: Vec<LLMTaskSpec> = Vec::new();
    let mut explanation: Option<String> = None;
    let mut search_slice = trimmed;
    while let Some(fence_start) = search_slice.find("```") {
        let after_ticks = &search_slice[fence_start + 3..];
        let body_start = after_ticks.find('\n').map(|i| i + 1).unwrap_or(0);
        let after_header = &after_ticks[body_start..];
        if let Some(fence_end) = after_header.find("```") {
            let block = after_header[..fence_end].trim();
            if let Ok(env) = serde_json::from_str::<LLMCommandEnvelope>(block) {
                if explanation.is_none() {
                    explanation = env.explanation;
                }
                collected_actions.extend(env.actions);
            } else if let Ok(arr) = serde_json::from_str::<Vec<LLMTaskSpec>>(block) {
                collected_actions.extend(arr);
            } else if let Ok(single) = serde_json::from_str::<LLMTaskSpec>(block) {
                collected_actions.push(single);
            }
            search_slice = &after_header[fence_end + 3..];
        } else {
            break;
        }
    }
    if !collected_actions.is_empty() {
        return Some(LLMCommandEnvelope {
            explanation: Some(explanation.unwrap_or_else(|| {
                format!("Executed {} requested task(s).", collected_actions.len())
            })),
            actions: collected_actions,
        });
    }

    // 4. Try array slice [...] if present
    if let (Some(a_start), Some(a_end)) = (trimmed.find('['), trimmed.rfind(']')) {
        if a_end > a_start {
            let arr_slice = &trimmed[a_start..=a_end];
            if let Ok(arr) = serde_json::from_str::<Vec<LLMTaskSpec>>(arr_slice) {
                if !arr.is_empty() {
                    return Some(LLMCommandEnvelope {
                        explanation: Some(format!("Executed {} requested task(s).", arr.len())),
                        actions: arr,
                    });
                }
            }
        }
    }

    // 5. Try object slice {...} or multiple newline-separated JSON objects
    for line in trimmed.lines() {
        let l = line.trim().trim_end_matches(',');
        if l.starts_with('{') && l.ends_with('}') {
            if let Ok(single) = serde_json::from_str::<LLMTaskSpec>(l) {
                collected_actions.push(single);
            }
        }
    }
    if !collected_actions.is_empty() {
        return Some(LLMCommandEnvelope {
            explanation: Some(format!(
                "Executed {} requested task(s).",
                collected_actions.len()
            )),
            actions: collected_actions,
        });
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

fn strip_polite_prefix(s: &str) -> &str {
    let mut cur = s.trim();
    loop {
        let lower = cur.to_ascii_lowercase();
        if let Some( idx ) = ["please ", "can you ", "could you ", "kindly "]
            .iter()
            .find_map(|p| if lower.starts_with(p) { Some(p.len()) } else { None })
        {
            cur = cur[idx..].trim();
        } else {
            break;
        }
    }
    cur
}

fn starts_with_action_verb(s: &str) -> bool {
    let clean = strip_polite_prefix(s);
    let lower = clean.to_ascii_lowercase();
    lower.starts_with("create grid")
        || lower.starts_with("create a grid")
        || lower.starts_with("new grid")
        || lower.starts_with("add column")
        || lower.starts_with("add columns")
        || lower.starts_with("create column")
        || lower.starts_with("create columns")
        || lower.starts_with("new column")
        || lower.starts_with("new columns")
        || lower.starts_with("rename column")
        || lower.starts_with("edit column")
        || lower.starts_with("delete column")
        || lower.starts_with("delete columns")
        || lower.starts_with("remove column")
        || lower.starts_with("remove columns")
        || lower.starts_with("drop column")
        || lower.starts_with("drop columns")
        || lower.starts_with("add row")
        || lower.starts_with("add a row")
        || lower.starts_with("insert row")
        || lower.starts_with("create row")
        || lower.starts_with("new row")
        || lower.starts_with("update row")
        || lower.starts_with("edit row")
        || lower.starts_with("modify row")
        || lower.starts_with("delete row")
        || lower.starts_with("remove row")
        || lower.starts_with("add tag ")
        || lower.starts_with("remove tag ")
        || lower.starts_with("go to ")
        || lower.starts_with("open ")
        || lower.starts_with("navigate to ")
        || lower.starts_with("take me to ")
        || lower.starts_with("jump to ")
        || lower.starts_with("switch to ")
        || (lower.starts_with("set ") && (lower.contains(" for ") || lower.contains(" on ")))
}

fn prompt_is_explicit_task_command(prompt: &str) -> bool {
    starts_with_action_verb(prompt)
}

/// Splits a multi-task user prompt into individual atomic command strings whenever action verbs
/// are separated by `,`, `;`, `\n`, or conjunctions (`and`, `then`, `also`), and expands
/// multi-column lists like `"add column username, Size, Department"` into separate tasks,
/// while preserving comma-separated key-value pairs inside `add row` / `update row`.
fn split_into_atomic_tasks(prompt: &str) -> Vec<String> {
    let trimmed = prompt.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }

    let lower = trimmed.to_ascii_lowercase();
    let bytes = lower.as_bytes();
    let len = bytes.len();

    let mut raw_clauses: Vec<String> = Vec::new();
    let mut clause_start = 0usize;
    let mut i = 0usize;

    while i < len {
        // Check for delimiter at position `i`: `,`, `;`, `\n`, or word conjunction `" and "`, `" then "`, `" also "`
        let mut delim_end: Option<usize> = None;

        if matches!(bytes[i], b',' | b';' | b'\n') {
            delim_end = Some(i + 1);
        } else if bytes[i].is_ascii_whitespace() {
            let rem = &lower[i..];
            for conj in [" and ", " then ", " also "] {
                if rem.starts_with(conj) {
                    delim_end = Some(i + conj.len());
                    break;
                }
            }
        }

        if let Some(mut after_delim) = delim_end {
            // Skip any whitespace and optional chained conjunctions (e.g. `, and then add column ...`)
            while after_delim < len && bytes[after_delim].is_ascii_whitespace() {
                after_delim += 1;
            }
            loop {
                let rem = &lower[after_delim..];
                let mut stripped_conj = false;
                for conj in ["and ", "then ", "also "] {
                    if rem.starts_with(conj) {
                        after_delim += conj.len();
                        while after_delim < len && bytes[after_delim].is_ascii_whitespace() {
                            after_delim += 1;
                        }
                        stripped_conj = true;
                        break;
                    }
                }
                if !stripped_conj {
                    break;
                }
            }

            if after_delim < len && trimmed.is_char_boundary(i) && trimmed.is_char_boundary(after_delim) {
                let rest = &trimmed[after_delim..];
                if starts_with_action_verb(rest) {
                    let prev_clause = trimmed[clause_start..i].trim();
                    if !prev_clause.is_empty() {
                        raw_clauses.push(prev_clause.to_string());
                    }
                    clause_start = after_delim;
                    i = after_delim;
                    continue;
                }
            }
        }

        // Advance `i` to the next UTF-8 char boundary
        i += 1;
        while i < len && !trimmed.is_char_boundary(i) {
            i += 1;
        }
    }

    if clause_start < len {
        let tail = trimmed[clause_start..].trim();
        if !tail.is_empty() {
            raw_clauses.push(tail.to_string());
        }
    }

    // Pass 2: Expand multi-column `add column A, B, C` or `delete column A, B` clauses into atomic tasks
    let mut atomic_tasks: Vec<String> = Vec::new();
    for clause in raw_clauses {
        let clean_clause = strip_polite_prefix(&clause);
        let cl_lower = clean_clause.to_ascii_lowercase();

        let is_add_col = cl_lower.starts_with("add column")
            || cl_lower.starts_with("add columns")
            || cl_lower.starts_with("create column")
            || cl_lower.starts_with("create columns")
            || cl_lower.starts_with("new column")
            || cl_lower.starts_with("new columns");

        let is_del_col = cl_lower.starts_with("delete column")
            || cl_lower.starts_with("delete columns")
            || cl_lower.starts_with("remove column")
            || cl_lower.starts_with("remove columns")
            || cl_lower.starts_with("drop column")
            || cl_lower.starts_with("drop columns");

        if is_add_col || is_del_col {
            // Find where the word "columns" or "column" ends
            let after_kw = if let Some(pos) = cl_lower.find("columns") {
                &clean_clause[pos + "columns".len()..]
            } else if let Some(pos) = cl_lower.find("column") {
                &clean_clause[pos + "column".len()..]
            } else {
                ""
            };
            let after_kw = after_kw
                .trim()
                .trim_start_matches(['=', ':'])
                .trim()
                .trim_start_matches("called ")
                .trim_start_matches("named ")
                .trim();

            // Split multiple column names separated by `,` or `" and "`
            let mut sub_names: Vec<String> = Vec::new();
            for comma_part in after_kw.split(',') {
                let cp = comma_part.trim();
                if cp.is_empty() {
                    continue;
                }
                // Also split on case-insensitive " and "
                let cp_lower = cp.to_ascii_lowercase();
                if cp_lower.contains(" and ") {
                    let mut start_idx = 0usize;
                    while let Some(rel_idx) = cp_lower[start_idx..].find(" and ") {
                        let abs_idx = start_idx + rel_idx;
                        let piece = cp[start_idx..abs_idx].trim();
                        if !piece.is_empty() {
                            sub_names.push(piece.to_string());
                        }
                        start_idx = abs_idx + 5;
                    }
                    let rem_piece = cp[start_idx..].trim();
                    if !rem_piece.is_empty() {
                        sub_names.push(rem_piece.to_string());
                    }
                } else {
                    sub_names.push(cp.to_string());
                }
            }

            if sub_names.len() > 1 {
                let verb = if is_del_col {
                    "delete column"
                } else {
                    "add column"
                };
                for sub in sub_names {
                    let clean_sub = sub
                        .trim()
                        .trim_start_matches("column ")
                        .trim_start_matches("columns ")
                        .trim_start_matches(['=', ':'])
                        .trim();
                    if !clean_sub.is_empty() {
                        atomic_tasks.push(format!("{verb} {clean_sub}"));
                    }
                }
                continue;
            }
        }

        atomic_tasks.push(clean_clause.to_string());
    }

    atomic_tasks
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

fn extract_column_name_and_type(raw_after_col: &str) -> (String, String) {
    let mut cleaned = raw_after_col
        .trim()
        .trim_start_matches("s ")
        .trim()
        .trim_start_matches(['=', ':'])
        .trim()
        .trim_start_matches("called ")
        .trim_start_matches("named ")
        .trim()
        .trim_start_matches(['=', ':'])
        .trim()
        .trim_matches(['"', '\'', ',', '.'])
        .trim()
        .to_string();

    let mut explicit_type: Option<&str> = None;
    let lower_cleaned = cleaned.to_ascii_lowercase();

    // Check trailing parenthesized or keyword type specifier like `Size (number)` or `Size as number` or `Size type select`
    for (kw, mapped) in [
        ("multi_select", "multi_select"),
        ("multiselect", "multi_select"),
        ("currency", "currency"),
        ("number", "number"),
        ("select", "select"),
        ("checkbox", "checkbox"),
        ("checklist", "checklist"),
        ("person", "person"),
        ("password", "password"),
        ("date", "date"),
        ("text", "text"),
    ] {
        for suffix_pat in [
            format!("({kw})"),
            format!(" as {kw}"),
            format!(" type {kw}"),
            format!(" : {kw}"),
            format!(": {kw}"),
        ] {
            if lower_cleaned.ends_with(&suffix_pat) {
                let keep_len = cleaned.len().saturating_sub(suffix_pat.len());
                cleaned = cleaned[..keep_len]
                    .trim()
                    .trim_matches(['"', '\'', ',', '.'])
                    .trim()
                    .to_string();
                explicit_type = Some(mapped);
                break;
            }
        }
        if explicit_type.is_some() {
            break;
        }
    }

    let ptype = if let Some(t) = explicit_type {
        t
    } else {
        let lc = cleaned.to_ascii_lowercase();
        if lc == "tags" || lc == "departments" || lc == "categories" {
            "multi_select"
        } else if lc == "cost" || lc == "budget" || lc == "price" || lc == "amount" {
            "currency"
        } else if lc == "status" || lc == "priority" {
            "select"
        } else if lc == "date" || lc == "deadline" || lc == "due date" {
            "date"
        } else {
            "text"
        }
    };

    if cleaned.is_empty() {
        cleaned = "New Column".to_string();
    }

    (cleaned, ptype.to_string())
}

fn parse_single_atomic_task(
    clause: &str,
    simulated_columns: &mut Vec<GridColumn>,
) -> Option<LLMTaskSpec> {
    let prompt = strip_polite_prefix(clause);
    let lower = prompt.to_ascii_lowercase();

    // 0. Direct Navigation intent (`go to ...`, `open ...`, `navigate to ...`, `take me to ...`)
    for nav_prefix in [
        "navigate to ",
        "take me to ",
        "switch to ",
        "jump to ",
        "go to ",
        "open ",
    ] {
        if lower.starts_with(nav_prefix) {
            let target = prompt[nav_prefix.len()..]
                .trim()
                .trim_start_matches("grid ")
                .trim_start_matches("space ")
                .trim_start_matches("row ")
                .trim_matches(['"', '\'', '.', ':'])
                .trim()
                .to_string();
            if !target.is_empty() {
                return Some(LLMTaskSpec::Navigate { target });
            }
        }
    }

    // 1. Create Grid intent (`create grid ...` or `new grid ...`)
    if lower.starts_with("create grid")
        || lower.starts_with("create a grid")
        || lower.starts_with("new grid")
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
                    .trim_start_matches(['=', ':'])
                    .trim()
                    .trim_matches(['"', '\'', ':'])
                    .to_string()
            })
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "AI Workspace Grid".to_string());

        return Some(LLMTaskSpec::CreateGrid {
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
        });
    }

    // 2. Rename Column intent (`rename column A to B` or `edit column A to B`)
    if lower.starts_with("rename column") || lower.starts_with("edit column") {
        if let Some((_, after_col)) = prompt.split_once("column") {
            let trimmed = after_col
                .trim()
                .trim_start_matches(['=', ':'])
                .trim();
            let split_res = if let Some(idx) = trimmed.to_ascii_lowercase().rfind(" to ") {
                Some((&trimmed[..idx], &trimmed[idx + 4..]))
            } else {
                trimmed.split_once('=').or_else(|| trimmed.split_once(':'))
            };
            if let Some((old_raw, new_raw)) = split_res {
                let old_name = old_raw
                    .trim()
                    .trim_matches(['"', '\'', ',', '.'])
                    .trim()
                    .to_string();
                let new_name = new_raw
                    .trim()
                    .trim_start_matches(['=', ':'])
                    .trim()
                    .trim_matches(['"', '\'', ',', '.'])
                    .trim()
                    .to_string();
                if !old_name.is_empty() && !new_name.is_empty() {
                    if let Some(col) = simulated_columns
                        .iter_mut()
                        .find(|c| c.name.eq_ignore_ascii_case(&old_name))
                    {
                        col.name = new_name.clone();
                    }
                    return Some(LLMTaskSpec::RenameColumn { old_name, new_name });
                }
            }
        }
    }

    // 3. Delete Column intent (`delete column D`, `remove column D`, `drop column D`)
    if lower.starts_with("delete column")
        || lower.starts_with("remove column")
        || lower.starts_with("drop column")
    {
        if let Some((_, after_col)) = prompt.split_once("column") {
            let col_name = after_col
                .trim()
                .trim_start_matches("s ")
                .trim()
                .trim_start_matches(['=', ':'])
                .trim()
                .trim_start_matches("called ")
                .trim_start_matches("named ")
                .trim()
                .trim_matches(['"', '\'', ',', '.'])
                .trim()
                .to_string();
            if !col_name.is_empty() {
                simulated_columns.retain(|c| !c.name.eq_ignore_ascii_case(&col_name));
                return Some(LLMTaskSpec::DeleteColumn { name: col_name });
            }
        }
    }

    // 4. Add Column intent (`add column = username`, `add column Size`, `create column ...`)
    if lower.starts_with("add column")
        || lower.starts_with("create column")
        || lower.starts_with("new column")
    {
        let raw_after = prompt
            .split_once("columns")
            .or_else(|| prompt.split_once("column"))
            .map(|(_, r)| r)
            .unwrap_or("");
        let (col_name, ptype) = extract_column_name_and_type(raw_after);
        let (parsed_ptype, parsed_cfg) = parse_property_type_and_config(&ptype);

        // Add simulated column in memory so subsequent `add row` / `update row` tasks in the same prompt recognize it!
        simulated_columns.push(GridColumn {
            id: Uuid::new_v4(),
            grid_id: Uuid::nil(),
            name: col_name.clone(),
            property_type: parsed_ptype,
            property_config: parsed_cfg,
            width_px: 200,
            position: simulated_columns.len() as i32,
            is_primary: false,
            is_hidden: false,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        });

        return Some(LLMTaskSpec::AddColumn {
            name: col_name,
            property_type: ptype,
        });
    }

    // 5. Delete Row intent (`delete row ...` or `remove row ...`)
    if lower.starts_with("delete row") || lower.starts_with("remove row") {
        let match_text = prompt
            .split_once("row")
            .map(|(_, r)| {
                r.trim()
                    .trim_start_matches("where ")
                    .trim_start_matches("matching ")
                    .trim_start_matches(['=', ':'])
                    .trim()
                    .trim_matches(['"', '\'', '.'])
                    .to_string()
            })
            .unwrap_or_default();

        return Some(LLMTaskSpec::DeleteRow { match_text });
    }

    // 6. Add/Remove Single or Multiple Tags on MultiSelect (`add tag 'B2C' to ChatGPT`, `remove tag 'B2C' from ChatGPT`)
    if lower.starts_with("add tag ") || lower.starts_with("remove tag ") {
        let is_remove = lower.starts_with("remove tag ");
        let body = if is_remove { &prompt[11..] } else { &prompt[8..] };
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

            let multi_col = simulated_columns
                .iter()
                .find(|c| c.property_type == ColumnPropertyType::MultiSelect)
                .or_else(|| {
                    simulated_columns
                        .iter()
                        .find(|c| c.property_type == ColumnPropertyType::Select)
                });

            if let Some(col) = multi_col {
                let mut values = HashMap::new();
                values.insert(col.name.clone(), Value::String(tag_str));
                return Some(LLMTaskSpec::UpdateRow {
                    match_text: target_row_str,
                    values,
                    append_tags: !is_remove,
                    remove_tags: is_remove,
                });
            }
        }
    }

    // 7. Natural `Set <Column> to <Val1, Val2> for <Row>` (`Set Department to Software and Innovation for row 1`)
    if lower.starts_with("set ") && (lower.contains(" for ") || lower.contains(" on ")) {
        let body = &prompt[4..];
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
                let values = parse_key_value_pairs_from_prompt(&synthetic, simulated_columns);
                if !values.is_empty() {
                    return Some(LLMTaskSpec::UpdateRow {
                        match_text: target_row_str,
                        values,
                        append_tags: false,
                        remove_tags: false,
                    });
                }
            }
        }
    }

    // 8. Update Row intent (`update row 3 Department = Software, Innovation` or `edit row ChatGPT Payment = Yearly`)
    if lower.starts_with("update row")
        || lower.starts_with("edit row")
        || lower.starts_with("modify row")
    {
        let after_row = prompt
            .split_once("row")
            .map(|(_, r)| r.trim())
            .unwrap_or("");
        let mut match_text = String::new();
        let mut kv_start_slice = after_row;

        if let Some(eq_idx) = after_row.find(['=', ':']) {
            let before_eq = after_row[..eq_idx].trim();
            let words: Vec<&str> = before_eq.split_whitespace().collect();
            if words.len() >= 2 {
                let last_two = if words.len() >= 3 {
                    format!("{} {}", words[words.len() - 2], words[words.len() - 1])
                } else {
                    String::new()
                };
                let two_word_col = !last_two.is_empty()
                    && simulated_columns
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
                let values = parse_key_value_pairs_from_prompt(&rebuilt, simulated_columns);
                return Some(LLMTaskSpec::UpdateRow {
                    match_text,
                    values,
                    append_tags: false,
                    remove_tags: false,
                });
            }
        } else {
            kv_start_slice = after_row;
        }

        let values = parse_key_value_pairs_from_prompt(kv_start_slice, simulated_columns);
        return Some(LLMTaskSpec::UpdateRow {
            match_text,
            values,
            append_tags: false,
            remove_tags: false,
        });
    }

    // 9. Add Row intent (`add row Service = Canva, Department = Marketing, B2C, Payment = Yearly`)
    if lower.starts_with("add row")
        || lower.starts_with("add a row")
        || lower.starts_with("insert row")
        || lower.starts_with("create row")
        || lower.starts_with("new row")
    {
        let values = parse_key_value_pairs_from_prompt(prompt, simulated_columns);
        let mut final_values = values;
        if final_values.is_empty() {
            if let Some(first_col) = simulated_columns.first() {
                let fallback_text = prompt
                    .split_once("row")
                    .map(|(_, r)| {
                        r.trim()
                            .trim_start_matches(['=', ':'])
                            .trim()
                            .trim_matches(['"', '\'', ':'])
                            .to_string()
                    })
                    .filter(|s| !s.is_empty())
                    .unwrap_or_else(|| "New AI Row".to_string());
                final_values.insert(first_col.name.clone(), Value::String(fallback_text));
            }
        }
        return Some(LLMTaskSpec::AddRow {
            values: final_values,
        });
    }

    None
}

fn fallback_intent_parser(prompt: &str, active_columns: &[GridColumn]) -> LLMCommandEnvelope {
    let atomic_clauses = split_into_atomic_tasks(prompt);
    let mut simulated_columns: Vec<GridColumn> = active_columns.to_vec();
    let mut actions: Vec<LLMTaskSpec> = Vec::new();

    for clause in &atomic_clauses {
        if let Some(spec) = parse_single_atomic_task(clause, &mut simulated_columns) {
            actions.push(spec);
        }
    }

    if actions.is_empty() {
        LLMCommandEnvelope {
            explanation: None,
            actions: Vec::new(),
        }
    } else {
        let explanation = if actions.len() == 1 {
            "Completed 1 requested workspace task via PyraAI.".to_string()
        } else {
            format!(
                "Completed all {} requested workspace tasks in sequence via PyraAI:",
                actions.len()
            )
        };
        LLMCommandEnvelope {
            explanation: Some(explanation),
            actions,
        }
    }
}
