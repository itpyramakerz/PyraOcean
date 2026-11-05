use crate::{
    api::{
        handle_clipboard_paste_to_ai_attachments, read_file_list_to_ai_attachments, ApiClient,
    },
    components::cell_editor::{
        cell_to_plain_text, compute_floating_popover_style, copy_to_clipboard, tag_color_classes,
        CellEditor, EIGHT_TAG_COLORS,
    },
    state::AuthState,
};
use chrono::{NaiveDate, NaiveTime, Utc};
use leptos::{prelude::*, task::spawn_local};
use pyra_ocean_shared::{
    column_has_numeric_or_currency_values, compute_column_calculation, format_number_with_commas,
    generate_grid_a3_pdf_html_with_options, generate_grid_csv, generate_grid_markdown,
    partition_rows_by_group, slugify_grid_filename, AiAttachment, AiChatMessage, AiExecutedAction,
    AiNavigationTarget, BulkRowCellPatch, CalcMode, CellValue, ChecklistItem, ColumnCalcSummary,
    ColumnPropertyConfig, ColumnPropertyType, CreateColumnRequest, FileAttachment, GridColumn,
    GridHistoryDetail, GridHistoryEntry, GridRow, GridRowComment, NumberFormat, PageGrid,
    RowCellMap, SelectOption, SortDirection, SpaceMemberDetail, TagColor, UpdateColumnRequest,
};
use std::collections::{HashMap, HashSet};
use uuid::Uuid;
use wasm_bindgen::{closure::Closure, JsCast, JsValue};

const PALETTE_COLORS: [TagColor; 10] = [
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

const ALL_PROPERTY_TYPES: [(ColumnPropertyType, &'static str); 12] = [
    (ColumnPropertyType::Text, "Text (Rich / Plain String)"),
    (ColumnPropertyType::Number, "Number (Decimal / Currency)"),
    (ColumnPropertyType::Select, "Select (Single Colored Tag)"),
    (
        ColumnPropertyType::MultiSelect,
        "MultiSelect (Multiple Tags)",
    ),
    (ColumnPropertyType::Date, "Date (ISO-8601 Date Picker)"),
    (ColumnPropertyType::Time, "Time (Time of Day)"),
    (ColumnPropertyType::Files, "Files (Multipart Attachments)"),
    (ColumnPropertyType::Checkbox, "Checkbox (Boolean Toggle)"),
    (
        ColumnPropertyType::Checklist,
        "Checklist (Sub-items & Progress)",
    ),
    (
        ColumnPropertyType::LastModified,
        "LastModified (Auto Timestamp)",
    ),
    (ColumnPropertyType::Person, "Person (Workspace Member)"),
    (ColumnPropertyType::Password, "Password (🔒 Masked Secret)"),
];

const ALL_CALC_MODES: [CalcMode; 10] = [
    CalcMode::None,
    CalcMode::CountAll,
    CalcMode::CountValues,
    CalcMode::CountEmpty,
    CalcMode::CountChecked,
    CalcMode::PercentChecked,
    CalcMode::Sum,
    CalcMode::Average,
    CalcMode::Min,
    CalcMode::Max,
];

/// Represents a reversible user action stored in the client-side multi-step `UndoStack` (capped at 50 items).
#[derive(Clone, Debug)]
enum UndoAction {
    CellEdit {
        row_id: Uuid,
        col_id: Uuid,
        previous_value: Option<CellValue>,
        #[allow(dead_code)]
        new_value: CellValue,
    },
    BulkCellFill {
        previous_cells: Vec<(Uuid, Uuid, Option<CellValue>)>,
    },
    CreateRows {
        row_ids: Vec<Uuid>,
    },
    RowDelete {
        deleted_rows: Vec<GridRow>,
    },
    CreateColumn {
        col_id: Uuid,
    },
    DeleteColumn {
        column: GridColumn,
        cell_values: Vec<(Uuid, CellValue)>,
        previous_col_order: Vec<Uuid>,
    },
    UpdateColumnMeta {
        col_id: Uuid,
        old_name: String,
        old_config: ColumnPropertyConfig,
        old_width: i32,
        old_cells: Vec<(Uuid, Option<CellValue>)>,
    },
    ColumnResize {
        col_id: Uuid,
        old_width: i32,
        #[allow(dead_code)]
        new_width: i32,
    },
    RowReorder {
        previous_order: Vec<Uuid>,
    },
    ColumnReorder {
        previous_order: Vec<Uuid>,
    },
    ColumnTypeConvert {
        col_id: Uuid,
        previous_type: ColumnPropertyType,
        previous_config: ColumnPropertyConfig,
        previous_cells: Vec<(Uuid, Option<CellValue>)>,
    },
    UpdateGridMeta {
        old_title: String,
    },
}

/// Converts a source `CellValue` into a compatible `CellValue` for `target_col` during
/// horizontal or vertical Excel Fill-Handle drag-to-copy.
fn coerce_cell_for_fill(
    src_val: Option<&CellValue>,
    src_col: &GridColumn,
    target_col: &GridColumn,
    members: &[SpaceMemberDetail],
    fallback_last_modified: chrono::DateTime<Utc>,
) -> Option<CellValue> {
    if target_col.property_type == ColumnPropertyType::LastModified {
        return None;
    }

    if src_col.id == target_col.id {
        return src_val.cloned();
    }

    if src_col.property_type == target_col.property_type
        && !matches!(
            target_col.property_type,
            ColumnPropertyType::Select | ColumnPropertyType::MultiSelect
        )
    {
        return src_val.cloned();
    }

    let plain = cell_to_plain_text(src_val, src_col, members, fallback_last_modified);
    let trimmed = plain.trim();

    match target_col.property_type {
        ColumnPropertyType::Text => Some(CellValue::Text(plain)),
        ColumnPropertyType::Number => {
            let cleaned: String = trimmed
                .chars()
                .filter(|c| c.is_ascii_digit() || *c == '.' || *c == '-')
                .collect();
            let parsed = cleaned
                .parse::<f64>()
                .ok()
                .filter(|v| v.is_finite())
                .unwrap_or(0.0);
            Some(CellValue::Number(parsed))
        }
        ColumnPropertyType::Select => {
            if let ColumnPropertyConfig::Select { options } = &target_col.property_config {
                let first_tok = trimmed.split(',').next().unwrap_or("").trim();
                let matched = options
                    .iter()
                    .find(|o| o.label.eq_ignore_ascii_case(first_tok))
                    .map(|o| o.id);
                Some(CellValue::Select(matched))
            } else {
                Some(CellValue::Select(None))
            }
        }
        ColumnPropertyType::MultiSelect => {
            if let ColumnPropertyConfig::MultiSelect { options } = &target_col.property_config {
                let ids: Vec<Uuid> = trimmed
                    .split(',')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .filter_map(|tok| {
                        options
                            .iter()
                            .find(|o| o.label.eq_ignore_ascii_case(tok))
                            .map(|o| o.id)
                    })
                    .collect();
                Some(CellValue::MultiSelect(ids))
            } else {
                Some(CellValue::MultiSelect(Vec::new()))
            }
        }
        ColumnPropertyType::Date => {
            let parsed = NaiveDate::parse_from_str(trimmed, "%Y-%m-%d")
                .or_else(|_| NaiveDate::parse_from_str(trimmed, "%d/%m/%Y"))
                .ok();
            Some(CellValue::Date(parsed))
        }
        ColumnPropertyType::Time => {
            let parsed = NaiveTime::parse_from_str(trimmed, "%H:%M:%S")
                .or_else(|_| NaiveTime::parse_from_str(trimmed, "%H:%M"))
                .ok();
            Some(CellValue::Time {
                time_of_day: parsed,
                duration_seconds: None,
            })
        }
        ColumnPropertyType::Checkbox => {
            let b = matches!(
                trimmed.to_lowercase().as_str(),
                "true" | "1" | "yes" | "[x]" | "checked"
            );
            Some(CellValue::Checkbox(b))
        }
        ColumnPropertyType::Checklist => {
            let items: Vec<ChecklistItem> = trimmed
                .lines()
                .flat_map(|l| l.split(','))
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(|part| {
                    if let Some(rest) = part
                        .strip_prefix("[x]")
                        .or_else(|| part.strip_prefix("[X]"))
                    {
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
            Some(CellValue::Checklist(items))
        }
        ColumnPropertyType::Files => {
            let now = Utc::now();
            let files: Vec<FileAttachment> = trimmed
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(|url| FileAttachment {
                    id: Uuid::new_v4(),
                    filename: url.rsplit('/').next().unwrap_or("file").to_string(),
                    mime_type: "application/octet-stream".into(),
                    size_bytes: 0,
                    storage_url: url.to_string(),
                    uploaded_by: Uuid::nil(),
                    uploaded_at: now,
                })
                .collect();
            Some(CellValue::Files(files))
        }
        ColumnPropertyType::Person => {
            let uids: Vec<Uuid> = trimmed
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .filter_map(|token| {
                    Uuid::parse_str(token).ok().or_else(|| {
                        members
                            .iter()
                            .find(|m| m.display_name.eq_ignore_ascii_case(token))
                            .map(|m| m.user_id)
                    })
                })
                .collect();
            Some(CellValue::Person(uids))
        }
        ColumnPropertyType::Password => Some(CellValue::Password(plain)),
        ColumnPropertyType::LastModified => None,
    }
}

/// Converts `None` into a concrete empty `CellValue` for the column's property type when undoing a cell edit.
fn empty_cell_value_for_column(col: Option<&GridColumn>) -> CellValue {
    match col.map(|c| c.property_type) {
        Some(ColumnPropertyType::Number) => CellValue::Number(0.0),
        Some(ColumnPropertyType::Select) => CellValue::Select(None),
        Some(ColumnPropertyType::MultiSelect) => CellValue::MultiSelect(Vec::new()),
        Some(ColumnPropertyType::Date) => CellValue::Date(None),
        Some(ColumnPropertyType::Time) => CellValue::Time {
            time_of_day: None,
            duration_seconds: None,
        },
        Some(ColumnPropertyType::Files) => CellValue::Files(Vec::new()),
        Some(ColumnPropertyType::Checkbox) => CellValue::Checkbox(false),
        Some(ColumnPropertyType::Checklist) => CellValue::Checklist(Vec::new()),
        Some(ColumnPropertyType::Person) => CellValue::Person(Vec::new()),
        Some(ColumnPropertyType::Password) => CellValue::Password(String::new()),
        _ => CellValue::Text(String::new()),
    }
}

/// Renders a `ColumnCalcSummary` into Dual-Currency (`EGP` Emerald Green + `USD` Ocean Cyan) or numeric/count badges.
fn render_calc_summary_badges(summary: &ColumnCalcSummary, show_mode_prefix: bool) -> AnyView {
    if summary.is_empty() {
        return view! { <span /> }.into_any();
    }

    let prefix = summary.mode.short_prefix();
    let mode_tag = if show_mode_prefix && !prefix.is_empty() {
        Some(prefix)
    } else {
        None
    };

    if let Some(txt) = summary.text_summary.clone() {
        return view! {
            <div class="inline-flex items-center justify-end gap-1.5 font-mono text-xs">
                {mode_tag.map(|p| view! { <span class="text-[9px] uppercase tracking-wider text-slate-400 font-semibold">{p}</span> })}
                <span class="text-slate-100 font-bold">{txt}</span>
            </div>
        }
        .into_any();
    }

    let egp_opt = summary.egp_value;
    let usd_opt = summary.usd_value;
    let pct_opt = summary.percent_value;
    let plain_opt = summary.plain_value;
    let has_both_currencies = egp_opt.is_some() && usd_opt.is_some();

    view! {
        <div class="inline-flex flex-wrap items-center justify-end gap-1 font-mono text-xs">
            {mode_tag.map(|p| view! { <span class="text-[9px] uppercase tracking-wider text-slate-400 font-semibold mr-0.5">{p}</span> })}

            {egp_opt.map(|egp| {
                let formatted = format!("EGP {}", format_number_with_commas(egp, 2));
                view! {
                    <span class="px-1.5 py-0.5 rounded bg-emerald-500/15 border border-emerald-500/30 text-emerald-400 font-bold whitespace-nowrap">
                        {formatted}
                    </span>
                }
            })}

            {if has_both_currencies {
                view! { <span class="text-slate-400 font-bold text-[11px]">"+"</span> }.into_any()
            } else {
                view! { <span /> }.into_any()
            }}

            {usd_opt.map(|usd| {
                let formatted = format!("$ {}", format_number_with_commas(usd, 2));
                view! {
                    <span class="px-1.5 py-0.5 rounded bg-cyan-500/15 border border-cyan-500/30 text-cyan-400 font-bold whitespace-nowrap">
                        {formatted}
                    </span>
                }
            })}

            {pct_opt.map(|pct| {
                let formatted = format!("{:.2}%", pct);
                view! {
                    <span class="px-1.5 py-0.5 rounded bg-amber-500/15 border border-amber-500/30 text-amber-300 font-bold whitespace-nowrap">
                        {formatted}
                    </span>
                }
            })}

            {plain_opt.map(|plain| {
                let formatted = format_number_with_commas(plain, 2);
                view! {
                    <span class="text-emerald-400 font-bold whitespace-nowrap">
                        {formatted}
                    </span>
                }
            })}
        </div>
    }
    .into_any()
}

/// Triggers an instant 0ms client-side browser file download using a `web_sys::Blob` and temporary `<a download="...">`.
fn trigger_client_blob_download(filename: &str, mime_type: &str, content: &str) {
    let Some(window) = web_sys::window() else {
        return;
    };
    let Some(document) = window.document() else {
        return;
    };
    let Some(body) = document.body() else {
        return;
    };

    let parts = js_sys::Array::of1(&JsValue::from_str(content));
    let bag = web_sys::BlobPropertyBag::new();
    bag.set_type(mime_type);

    let Ok(blob) = web_sys::Blob::new_with_str_sequence_and_options(&parts, &bag) else {
        return;
    };
    let Ok(object_url) = web_sys::Url::create_object_url_with_blob(&blob) else {
        return;
    };

    if let Ok(el) = document.create_element("a") {
        if let Ok(anchor) = el.dyn_into::<web_sys::HtmlAnchorElement>() {
            anchor.set_href(&object_url);
            anchor.set_download(filename);
            let _ = anchor.set_attribute("style", "display:none;");
            let _ = body.append_child(&anchor);
            anchor.click();
            let _ = body.remove_child(&anchor);
        }
    }

    let _ = web_sys::Url::revoke_object_url(&object_url);
}

/// Dynamically injects a hidden `<iframe>` with the Executive A3 Landscape HTML document and invokes the native browser print/PDF engine.
fn trigger_a3_landscape_pdf_print(html_doc: &str) {
    let Some(window) = web_sys::window() else {
        return;
    };
    let Some(document) = window.document() else {
        return;
    };
    let Some(body) = document.body() else {
        return;
    };

    // Remove any existing hidden print frame first
    if let Some(existing) = document.get_element_by_id("pyraocean-a3-print-frame") {
        let _ = existing.remove();
    }

    let Ok(el) = document.create_element("iframe") else {
        return;
    };
    let Ok(iframe) = el.dyn_into::<web_sys::HtmlIFrameElement>() else {
        return;
    };

    iframe.set_id("pyraocean-a3-print-frame");
    let _ = iframe.set_attribute(
        "style",
        "position:fixed;right:0;bottom:0;width:0;height:0;border:0;visibility:hidden;",
    );
    iframe.set_srcdoc(html_doc);
    let _ = body.append_child(&iframe);

    // Wait briefly so the iframe DOM and styles finish parsing before triggering `.print()`
    let iframe_for_print = iframe.clone();
    let cb = Closure::once(Box::new(move || {
        if let Some(content_win) = iframe_for_print.content_window() {
            let _ = content_win.focus();
            let _ = content_win.print();
        }
    }) as Box<dyn FnOnce()>);

    let _ = window.set_timeout_with_callback_and_timeout_and_arguments_0(
        cb.as_ref().unchecked_ref(),
        250,
    );
    cb.forget();
}

/// Universal Property Value Resolver for Search & Filtering across all 12 Property Types:
/// Extracts human-readable text tokens for `Text`, `Number`, `Select`, `MultiSelect`, `Date`,
/// `Time`, `Files`, `Checkbox`, `Checklist`, `LastModified`, `Person`, and `Password`.
pub fn extract_searchable_cell_text(col: &GridColumn, val: &serde_json::Value) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    // Unwrap tagged `CellValue` JSON representation `{"t": "...", "v": ...}` if present
    let inner = val.get("v").unwrap_or(val);

    let resolve_option_label = |raw_tok: &str| -> Option<String> {
        let trimmed = raw_tok.trim();
        if trimmed.is_empty() {
            return None;
        }
        if let ColumnPropertyConfig::Select { options }
        | ColumnPropertyConfig::MultiSelect { options } = &col.property_config
        {
            if let Ok(parsed_id) = Uuid::parse_str(trimmed) {
                if let Some(opt) = options.iter().find(|o| o.id == parsed_id) {
                    return Some(opt.label.clone());
                }
            }
            if let Some(opt) = options
                .iter()
                .find(|o| o.label.eq_ignore_ascii_case(trimmed))
            {
                return Some(opt.label.clone());
            }
        }
        None
    };

    match col.property_type {
        ColumnPropertyType::Select => {
            match inner {
                serde_json::Value::String(s) => {
                    let s_trim = s.trim();
                    if !s_trim.is_empty() {
                        if let Some(lbl) = resolve_option_label(s_trim) {
                            out.push(lbl);
                        }
                        out.push(s_trim.to_string());
                    }
                }
                serde_json::Value::Object(map) => {
                    if let Some(lbl) = map.get("label").and_then(|v| v.as_str()) {
                        out.push(lbl.to_string());
                    }
                    if let Some(id_str) = map.get("id").and_then(|v| v.as_str()) {
                        if let Some(lbl) = resolve_option_label(id_str) {
                            out.push(lbl);
                        }
                        out.push(id_str.to_string());
                    }
                }
                _ => {}
            }
        }
        ColumnPropertyType::MultiSelect => {
            match inner {
                serde_json::Value::Array(arr) => {
                    for item in arr {
                        if let Some(s) = item.as_str() {
                            let s_trim = s.trim();
                            if !s_trim.is_empty() {
                                if let Some(lbl) = resolve_option_label(s_trim) {
                                    out.push(lbl);
                                }
                                out.push(s_trim.to_string());
                            }
                        } else if let Some(lbl) = item.get("label").and_then(|v| v.as_str()) {
                            out.push(lbl.to_string());
                        }
                    }
                }
                serde_json::Value::String(s) => {
                    for part in s.split(',').map(str::trim).filter(|p| !p.is_empty()) {
                        if let Some(lbl) = resolve_option_label(part) {
                            out.push(lbl);
                        }
                        out.push(part.to_string());
                    }
                }
                _ => {}
            }
        }
        ColumnPropertyType::Number => {
            if let Some(n) = inner
                .as_f64()
                .or_else(|| inner.as_str().and_then(|s| s.parse::<f64>().ok()))
            {
                out.push(n.to_string());
                out.push(format_number_with_commas(n, 2));
                if let ColumnPropertyConfig::Number { format } = &col.property_config {
                    match format {
                        NumberFormat::Integer => out.push(format!("{n:.0}")),
                        NumberFormat::Decimal { precision } => {
                            out.push(format!("{n:.prec$}", prec = *precision as usize));
                        }
                        NumberFormat::Percentage { precision } => {
                            out.push(format!("{n:.prec$}%", prec = *precision as usize));
                        }
                        NumberFormat::Currency { symbol, precision } => {
                            out.push(format!("{symbol} {n:.prec$}", prec = *precision as usize));
                            out.push(format!(
                                "{symbol} {}",
                                format_number_with_commas(n, *precision as usize)
                            ));
                        }
                    }
                }
            }
        }
        ColumnPropertyType::Checkbox => {
            let b = inner.as_bool().unwrap_or_else(|| {
                inner
                    .as_str()
                    .map(|s| matches!(s.to_lowercase().as_str(), "true" | "1" | "yes" | "checked"))
                    .unwrap_or(false)
            });
            if b {
                out.push("true".into());
                out.push("checked".into());
                out.push("yes".into());
            } else {
                out.push("false".into());
                out.push("unchecked".into());
                out.push("no".into());
            }
        }
        ColumnPropertyType::Checklist => {
            if let Some(arr) = inner.as_array() {
                for item in arr {
                    if let Some(txt) = item
                        .get("item")
                        .or_else(|| item.get("title"))
                        .or_else(|| item.get("text"))
                        .and_then(|v| v.as_str())
                    {
                        if !txt.trim().is_empty() {
                            out.push(txt.trim().to_string());
                        }
                    }
                    let done = item
                        .get("completed")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);
                    if done {
                        out.push("completed".into());
                    } else {
                        out.push("pending".into());
                    }
                }
            }
        }
        ColumnPropertyType::Files => {
            if let Some(arr) = inner.as_array() {
                for file in arr {
                    if let Some(fname) = file
                        .get("filename")
                        .or_else(|| file.get("name"))
                        .and_then(|v| v.as_str())
                    {
                        if !fname.trim().is_empty() {
                            out.push(fname.trim().to_string());
                        }
                    }
                    if let Some(url) = file.get("storage_url").and_then(|v| v.as_str()) {
                        if !url.trim().is_empty() {
                            out.push(url.trim().to_string());
                        }
                    }
                }
            }
        }
        _ => {
            if let Some(s) = inner.as_str() {
                if !s.trim().is_empty() {
                    out.push(s.trim().to_string());
                }
            } else if let Some(arr) = inner.as_array() {
                for item in arr {
                    if let Some(s) = item.as_str() {
                        if !s.trim().is_empty() {
                            out.push(s.trim().to_string());
                        }
                    }
                }
            } else if let Some(t) = inner.get("time_of_day").and_then(|v| v.as_str()) {
                out.push(t.to_string());
            }
        }
    }

    out
}

/// Typed wrapper around `extract_searchable_cell_text` that also resolves `Person` user IDs
/// against `SpaceMemberDetail` (display name + email) and `LastModified` timestamps.
fn extract_searchable_typed_cell_text(
    col: &GridColumn,
    cell: Option<&CellValue>,
    members: &[SpaceMemberDetail],
    row_last_modified: chrono::DateTime<Utc>,
) -> Vec<String> {
    let mut tokens: Vec<String> = match cell {
        Some(c) => {
            let val = serde_json::to_value(c).unwrap_or(serde_json::Value::Null);
            extract_searchable_cell_text(col, &val)
        }
        None => {
            if col.property_type == ColumnPropertyType::Checkbox {
                vec!["false".into(), "unchecked".into(), "no".into()]
            } else if col.property_type == ColumnPropertyType::LastModified {
                vec![row_last_modified.format("%Y-%m-%d %H:%M").to_string()]
            } else {
                Vec::new()
            }
        }
    };

    if let Some(CellValue::Person(uids)) = cell {
        for uid in uids {
            if let Some(m) = members.iter().find(|m| &m.user_id == uid) {
                tokens.push(m.display_name.clone());
                tokens.push(m.email.clone());
            }
        }
    }

    let plain = cell_to_plain_text(cell, col, members, row_last_modified);
    if !plain.trim().is_empty() {
        tokens.push(plain);
    }

    tokens
}

/// Universal Row Filter Matcher combining:
/// 1. The live upper toolbar Search input (`quick_search_q`) across all 12 property types, and
/// 2. The Interactive Property Filter Bar (`filter_col_id`, `filter_text_val`, and `selected_filter_tags`).
fn row_matches_universal_filters(
    row: &GridRow,
    cols: &[GridColumn],
    members: &[SpaceMemberDetail],
    quick_search_q: &str,
    filter_col_id: Option<Uuid>,
    filter_text_val: &str,
    selected_filter_tags: &HashSet<String>,
) -> bool {
    let q_clean = quick_search_q.trim().to_lowercase();
    if !q_clean.is_empty() {
        let any_col_matched = cols.iter().any(|col| {
            let tokens = extract_searchable_typed_cell_text(
                col,
                row.cells.get(&col.id),
                members,
                row.last_modified,
            );
            tokens.iter().any(|t| t.to_lowercase().contains(&q_clean))
        });
        if !any_col_matched {
            return false;
        }
    }

    let f_text = filter_text_val.trim().to_lowercase();
    let has_tags = !selected_filter_tags.is_empty();

    if let Some(target_cid) = filter_col_id {
        if let Some(col) = cols.iter().find(|c| c.id == target_cid) {
            let tokens = extract_searchable_typed_cell_text(
                col,
                row.cells.get(&col.id),
                members,
                row.last_modified,
            );
            if has_tags {
                let tag_matched = selected_filter_tags.iter().any(|tag| {
                    let tag_low = tag.to_lowercase();
                    tokens.iter().any(|t| t.to_lowercase() == tag_low || t.to_lowercase().contains(&tag_low))
                });
                if !tag_matched {
                    return false;
                }
            }
            if !f_text.is_empty() {
                let text_matched = tokens.iter().any(|t| t.to_lowercase().contains(&f_text));
                if !text_matched {
                    return false;
                }
            }
        }
    } else if has_tags || !f_text.is_empty() {
        // When no single column is locked, match selected tags / filter text across all Select/MultiSelect or visible columns
        if has_tags {
            let tag_matched = cols.iter().any(|col| {
                let tokens = extract_searchable_typed_cell_text(
                    col,
                    row.cells.get(&col.id),
                    members,
                    row.last_modified,
                );
                selected_filter_tags.iter().any(|tag| {
                    let tag_low = tag.to_lowercase();
                    tokens.iter().any(|t| t.to_lowercase() == tag_low)
                })
            });
            if !tag_matched {
                return false;
            }
        }
        if !f_text.is_empty() {
            let text_matched = cols.iter().any(|col| {
                let tokens = extract_searchable_typed_cell_text(
                    col,
                    row.cells.get(&col.id),
                    members,
                    row.last_modified,
                );
                tokens.iter().any(|t| t.to_lowercase().contains(&f_text))
            });
            if !text_matched {
                return false;
            }
        }
    }

    true
}

#[component]
pub fn GridViewPage(
    current_path: RwSignal<String>,
    space_id: RwSignal<Uuid>,
    grid_id: RwSignal<Uuid>,
    on_navigate: Callback<String>,
    on_open_import: Callback<Uuid>,
    on_open_search_modal: Callback<()>,
) -> impl IntoView {
    let auth = expect_context::<AuthState>();

    // Reactive URL parameter extraction for `/spaces/:space_id/grids/:grid_id`
    let current_space_id = Memo::new(move |_| {
        let path = current_path.get();
        let segments: Vec<&str> = path.trim_matches('/').split('/').collect();
        if segments.len() >= 2 && segments[0] == "spaces" {
            if let Ok(sid) = Uuid::parse_str(segments[1]) {
                return Some(sid);
            }
        }
        let s = space_id.get();
        if s != Uuid::nil() {
            Some(s)
        } else {
            auth.active_space_id.get()
        }
    });

    let current_grid_id = Memo::new(move |_| {
        let path = current_path.get();
        let segments: Vec<&str> = path.trim_matches('/').split('/').collect();
        if segments.len() >= 4 && segments[0] == "spaces" && segments[2] == "grids" {
            if let Ok(gid) = Uuid::parse_str(segments[3]) {
                return Some(gid);
            }
        }
        let g = grid_id.get();
        if g != Uuid::nil() {
            Some(g)
        } else {
            auth.active_grid_id.get()
        }
    });

    let refresh_trigger = RwSignal::new(0u64);

    let grid_meta = RwSignal::new(Option::<PageGrid>::None);
    let columns = RwSignal::new(Vec::<GridColumn>::new());
    // Strictly isolated per-column width map (`column_widths: RwSignal<HashMap<Uuid, i32>>`)
    let column_widths = RwSignal::new(HashMap::<Uuid, i32>::new());
    let rows = RwSignal::new(Vec::<GridRow>::new());
    let space_members = RwSignal::new(Vec::<SpaceMemberDetail>::new());
    let is_loading = RwSignal::new(false);
    let error_banner = RwSignal::new(Option::<String>::None);

    // Multi-step Undo Stack (capped at 50 items)
    let undo_stack = RwSignal::new(Vec::<UndoAction>::new());
    let push_undo_action = move |action: UndoAction| {
        undo_stack.update(|stack| {
            if stack.len() >= 50 {
                stack.remove(0);
            }
            stack.push(action);
        });
    };

    // 30-Day Grid History Slide-Over Drawer & Full-Screen Preview Modal state
    let show_history_drawer = RwSignal::new(false);
    let history_entries = RwSignal::new(Vec::<GridHistoryEntry>::new());
    let is_loading_history = RwSignal::new(false);
    let preview_history_snapshot = RwSignal::new(Option::<GridHistoryDetail>::None);
    let is_loading_preview_snapshot = RwSignal::new(false);

    // Multi-Format Grid Export Menu (`CSV`, `Markdown`, `A3 Landscape PDF`) state
    let show_export_menu = RwSignal::new(false);
    let export_menu_style = RwSignal::new(String::new());
    let mask_passwords_in_pdf = RwSignal::new(true);

    // Per-cell Password reveal signal (`(row_id, col_id)`) so clicking 👁️ only reveals that single cell
    let revealed_passwords = RwSignal::new(HashSet::<(Uuid, Uuid)>::new());

    // Interactive Column Calculation Footer (`SUM`/`AVG`/`COUNT` + Dual-Currency `EGP`/`USD`) state
    let column_calculations = RwSignal::new(HashMap::<Uuid, CalcMode>::new());
    let open_calc_menu_col_id = RwSignal::new(Option::<Uuid>::None);
    let calc_menu_popover_style = RwSignal::new(String::new());

    // Group-By Department / Select Column & Collapsible Sections state
    let group_by_col_id = RwSignal::new(Option::<Uuid>::None);
    let show_group_by_popover = RwSignal::new(false);
    let collapsed_groups = RwSignal::new(HashSet::<String>::new());

    // Inline Grid Page Renaming state (`PATCH /api/grids/{grid_id}`)
    let editing_grid_title = RwSignal::new(false);
    let draft_grid_title = RwSignal::new(String::new());

    // Sorting & Universal Property Filtering state (all 12 property types + Select/MultiSelect Tag Badges)
    let sort_specs = RwSignal::new(Vec::<(Uuid, SortDirection)>::new());
    let show_sort_popover = RwSignal::new(false);
    let show_filter_popover = RwSignal::new(false);
    let quick_search = RwSignal::new(String::new());
    let server_filter_col = RwSignal::new(Option::<Uuid>::None);
    let server_filter_val = RwSignal::new(String::new());
    let filter_selected_tags = RwSignal::new(HashSet::<String>::new());

    // Bulk Row Selection state
    let selected_row_ids = RwSignal::new(HashSet::<Uuid>::new());

    // Drag-and-Drop Row Reordering & Hover `+` Positional Row Insertion state
    let dragging_row_id = RwSignal::new(Option::<Uuid>::None);
    let drag_over_row_id = RwSignal::new(Option::<Uuid>::None);
    let open_row_insert_menu = RwSignal::new(Option::<(Uuid, i32)>::None);
    let row_insert_menu_style = RwSignal::new(String::new());

    // Notion-Style Column Selection, Drag-and-Drop Reordering & Context Menu state
    let selected_column_id = RwSignal::new(Option::<Uuid>::None);
    let open_col_menu_id = RwSignal::new(Option::<Uuid>::None);
    let col_menu_popover_style = RwSignal::new(String::new());
    let rename_col_input = RwSignal::new(String::new());
    let confirming_delete_col_id = RwSignal::new(Option::<Uuid>::None);
    let dragging_col_id = RwSignal::new(Option::<Uuid>::None);
    let drag_over_col_id = RwSignal::new(Option::<Uuid>::None);

    // Inline Select/MultiSelect Option CRUD state inside Column Header Menu
    let menu_new_opt_label = RwSignal::new(String::new());
    let menu_new_opt_color = RwSignal::new(TagColor::Cyan);
    let menu_editing_opt_id = RwSignal::new(Option::<Uuid>::None);
    let menu_editing_opt_label = RwSignal::new(String::new());
    let menu_editing_opt_color = RwSignal::new(TagColor::Blue);

    // Interactive Column Width Adjustment (Drag-to-Resize) state
    // `(column_id, start_client_x, initial_width_px)`
    let resizing_col = RwSignal::new(Option::<(Uuid, i32, i32)>::None);

    // Excel-Style Active Cell & Fill-Handle Drag-to-Copy state
    // Coordinates are `(visible_row_idx, visible_col_idx, row_id, col_id)`
    let active_cell = RwSignal::new(Option::<(usize, usize, Uuid, Uuid)>::None);
    let fill_drag_start = RwSignal::new(Option::<(usize, usize, Uuid, Uuid)>::None);
    let fill_drag_current = RwSignal::new(Option::<(usize, usize)>::None);

    // `+ Add Column` Popover state (End-of-Table `+` Column)
    let show_end_add_col_popover = RwSignal::new(false);
    let end_add_col_popover_style = RwSignal::new(String::new());
    let new_col_name = RwSignal::new(String::new());
    let new_col_type = RwSignal::new(ColumnPropertyType::Text);
    let new_col_options_csv = RwSignal::new(String::from("Todo, In Progress, Done"));
    let new_col_currency = RwSignal::new(String::new());

    // Phase 16: Row Comments Slide-Over Drawer state
    let row_comment_counts = RwSignal::new(HashMap::<Uuid, i64>::new());
    let open_comments_row_id = RwSignal::new(Option::<Uuid>::None);
    let row_comments_list = RwSignal::new(Vec::<GridRowComment>::new());
    let is_loading_comments = RwSignal::new(false);
    let new_comment_text = RwSignal::new(String::new());

    // Phase 17: Multi-Row Selection (`Shift+Click`, `Ctrl+Click`, `Ctrl+A`) & Bulk Copy (`Ctrl+C`) state
    let last_selected_row_idx = RwSignal::new(Option::<usize>::None);
    let copied_rows_toast = RwSignal::new(Option::<String>::None);
    let copied_rows_json_buffer = RwSignal::new(Vec::<RowCellMap>::new());

    // Phase 17: Duplicate Grid Modal (`POST /api/grids/{grid_id}/duplicate`)
    let show_dup_grid_modal = RwSignal::new(false);
    let dup_grid_name = RwSignal::new(String::new());
    let dup_grid_space_id = RwSignal::new(Option::<Uuid>::None);
    let dup_grid_include_rows = RwSignal::new(true);

    // Phase 17: Move Grid to Another Space Modal (`PATCH /api/grids/{grid_id}/move`)
    let show_move_grid_modal = RwSignal::new(false);
    let move_grid_space_id = RwSignal::new(Option::<Uuid>::None);

    // Phase 17: Copy Rows to Another Grid Modal (`POST /api/grids/{source_grid_id}/rows/copy-to/{target_grid_id}`)
    let show_copy_rows_modal = RwSignal::new(false);
    let copy_rows_target_grid_id = RwSignal::new(Option::<Uuid>::None);
    let copy_rows_auto_create_cols = RwSignal::new(true);
    // `(copied_count, created_columns_count, target_space_id, target_grid_id, target_grid_title)`
    let copy_rows_success_banner =
        RwSignal::new(Option::<(usize, usize, Uuid, Uuid, String)>::None);

    // Phase 18: Grid-Level PyraAI Copilot Slide-Over Drawer state
    let show_ai_drawer = RwSignal::new(false);
    let grid_ai_prompt = RwSignal::new(String::new());
    let grid_ai_mode = RwSignal::new(String::from("ask"));
    let grid_ai_loading = RwSignal::new(false);
    let grid_ai_history = RwSignal::new(Vec::<AiChatMessage>::new());
    let grid_ai_actions = RwSignal::new(Vec::<AiExecutedAction>::new());
    let grid_ai_model = RwSignal::new(String::from("qwen2.5:1.5b"));
    let grid_ai_attachments = RwSignal::new(Vec::<AiAttachment>::new());
    let grid_ai_drag_over = RwSignal::new(false);
    let grid_ai_chat_scroll_ref = NodeRef::<leptos::html::Div>::new();
    let grid_ai_file_input_ref = NodeRef::<leptos::html::Input>::new();

    // Synchronize `column_widths` map from a slice of `GridColumn`s
    let sync_column_widths_from_list = move |cols: &[GridColumn]| {
        let mut map = HashMap::new();
        for c in cols {
            map.insert(c.id, c.width_px.clamp(80, 800));
        }
        column_widths.set(map);
    };

    // Total fixed table width = checkbox_col(40) + row_handle_col(64) + sum(visible col.width_px) + add_col(50)
    let total_table_width = move || -> i32 {
        let widths = column_widths.get();
        let cols_sum: i32 = columns
            .get()
            .into_iter()
            .filter(|c| !c.is_hidden)
            .map(|c| {
                widths
                    .get(&c.id)
                    .copied()
                    .unwrap_or_else(|| c.width_px.clamp(80, 800))
            })
            .sum();
        40 + 64 + cols_sum + 50
    };

    // Clear all active selections and popovers when clicking outside or pressing `Escape`
    let clear_all_selections = move || {
        active_cell.set(None);
        fill_drag_start.set(None);
        fill_drag_current.set(None);
        selected_column_id.set(None);
        selected_row_ids.set(HashSet::new());
        last_selected_row_idx.set(None);
        open_col_menu_id.set(None);
        open_row_insert_menu.set(None);
        confirming_delete_col_id.set(None);
        menu_editing_opt_id.set(None);
        show_sort_popover.set(false);
        show_filter_popover.set(false);
        show_group_by_popover.set(false);
        open_calc_menu_col_id.set(None);
        show_export_menu.set(false);
        show_end_add_col_popover.set(false);
    };

    // Helper to serialize `sort_specs` into `?sorts=col_uuid:asc,col_uuid2:desc`
    let build_sorts_query = move || -> Option<String> {
        let specs = sort_specs.get();
        if specs.is_empty() {
            return None;
        }
        let parts: Vec<String> = specs
            .into_iter()
            .map(|(cid, dir)| {
                let d = match dir {
                    SortDirection::Asc => "asc",
                    SortDirection::Desc => "desc",
                };
                format!("{cid}:{d}")
            })
            .collect();
        Some(parts.join(","))
    };

    // Fetch rows from backend using current sort parameters (filtering runs client-side via `row_matches_universal_filters` across all 12 property types)
    let reload_rows = move || {
        let gid = current_grid_id
            .get_untracked()
            .unwrap_or_else(|| grid_id.get_untracked());
        let sorts_q = build_sorts_query();

        spawn_local(async move {
            match ApiClient::list_rows(&auth, gid, sorts_q, None, None, None).await {
                Ok(fetched) => rows.set(fetched),
                Err(e) => error_banner.set(Some(e)),
            }
        });
    };

    // Load 30-day history snapshots for the drawer
    let load_history_entries = move || {
        let gid = current_grid_id
            .get_untracked()
            .unwrap_or_else(|| grid_id.get_untracked());
        is_loading_history.set(true);
        spawn_local(async move {
            match ApiClient::list_grid_history(&auth, gid).await {
                Ok(entries) => history_entries.set(entries),
                Err(e) => error_banner.set(Some(e)),
            }
            is_loading_history.set(false);
        });
    };

    // Single reactive `Effect` subscribing to `current_grid_id.get()`, `current_space_id.get()`, and `refresh_trigger.get()`
    // Guarantees `is_loading.set(false)` in all `Ok` and `Err` branches so route switching never hangs on "Loading Grid...".
    Effect::new(move |_| {
        let _refresh = refresh_trigger.get();
        let maybe_sid = current_space_id.get();
        let Some(gid) = current_grid_id.get() else {
            is_loading.set(false);
            return;
        };
        let sid = maybe_sid.unwrap_or_else(|| space_id.get_untracked());

        // Immediately clear old selection & popover states on route switch
        active_cell.set(None);
        fill_drag_start.set(None);
        fill_drag_current.set(None);
        selected_row_ids.update(|s| s.clear());
        last_selected_row_idx.set(None);
        copied_rows_toast.set(None);
        show_dup_grid_modal.set(false);
        show_move_grid_modal.set(false);
        show_copy_rows_modal.set(false);
        copy_rows_success_banner.set(None);
        selected_column_id.set(None);
        undo_stack.update(|u| u.clear());
        open_col_menu_id.set(None);
        open_row_insert_menu.set(None);
        confirming_delete_col_id.set(None);
        menu_editing_opt_id.set(None);
        show_sort_popover.set(false);
        show_filter_popover.set(false);
        server_filter_col.set(None);
        server_filter_val.set(String::new());
        filter_selected_tags.set(HashSet::new());
        show_group_by_popover.set(false);
        open_calc_menu_col_id.set(None);
        group_by_col_id.set(None);
        collapsed_groups.update(|c| c.clear());
        revealed_passwords.update(|r| r.clear());
        column_calculations.update(|m| m.clear());
        show_export_menu.set(false);
        show_end_add_col_popover.set(false);
        editing_grid_title.set(false);
        sort_specs.set(Vec::new());
        show_history_drawer.set(false);
        open_comments_row_id.set(None);
        row_comment_counts.set(HashMap::new());
        error_banner.set(None);

        // Optimistically populate title from cached `auth.space_grids` if available so header updates immediately
        if let Some(cached_grid) = auth
            .space_grids
            .get_untracked()
            .get(&sid)
            .and_then(|list| list.iter().find(|g| g.id == gid))
            .cloned()
        {
            draft_grid_title.set(cached_grid.title.clone());
            grid_meta.set(Some(cached_grid));
        }

        is_loading.set(true);

        spawn_local(async move {
            match ApiClient::get_grid_schema(&auth, gid).await {
                Ok(schema) => {
                    if current_grid_id.get_untracked() == Some(gid) {
                        draft_grid_title.set(schema.grid.title.clone());
                        grid_meta.set(Some(schema.grid));
                        sync_column_widths_from_list(&schema.columns);
                        columns.set(schema.columns);
                    }
                }
                Err(e) => {
                    if current_grid_id.get_untracked() == Some(gid) {
                        error_banner.set(Some(e));
                    }
                }
            }

            match ApiClient::list_rows(&auth, gid, None, None, None, None).await {
                Ok(fetched_rows) => {
                    if current_grid_id.get_untracked() == Some(gid) {
                        // Auto-enable `CalcMode::Sum` on numeric or currency columns by default
                        let cols_snapshot = columns.get_untracked();
                        let mut default_calcs = HashMap::new();
                        for col in &cols_snapshot {
                            if column_has_numeric_or_currency_values(col, &fetched_rows) {
                                default_calcs.insert(col.id, CalcMode::Sum);
                            }
                        }
                        column_calculations.set(default_calcs);
                        rows.set(fetched_rows);
                    }
                }
                Err(e) => {
                    if current_grid_id.get_untracked() == Some(gid) {
                        error_banner.set(Some(e));
                    }
                }
            }

            if sid != Uuid::nil() {
                if let Ok(members) = ApiClient::list_space_members(&auth, sid).await {
                    if current_grid_id.get_untracked() == Some(gid) {
                        space_members.set(members);
                    }
                }
            }

            if let Ok(counts) = ApiClient::list_grid_comment_counts(&auth, gid).await {
                if current_grid_id.get_untracked() == Some(gid) {
                    row_comment_counts.set(counts);
                }
            }

            // Guarantee `is_loading.set(false)` in all Ok/Err paths
            is_loading.set(false);

            if let Some(target_rid) = auth.highlighted_row_id.get_untracked() {
                if let Some(window) = web_sys::window() {
                    let row_dom_id = format!("grid-row-{target_rid}");
                    let cb = Closure::once(move || {
                        if let Some(doc) = web_sys::window().and_then(|w| w.document()) {
                            if let Some(el) = doc.get_element_by_id(&row_dom_id) {
                                el.scroll_into_view_with_bool(true);
                            }
                        }
                    });
                    let _ = window.set_timeout_with_callback_and_timeout_and_arguments_0(
                        cb.as_ref().unchecked_ref(),
                        80,
                    );
                    cb.forget();
                }
            }
        });
    });

    // Execute Undo (`Ctrl+Z` / `Cmd+Z` or Toolbar `↶ Undo` Button)
    let perform_undo = move || {
        let Some(action) = undo_stack.try_update(|stack| stack.pop()).flatten() else {
            return;
        };
        let gid = grid_id.get_untracked();

        match action {
            UndoAction::CellEdit {
                row_id,
                col_id,
                previous_value,
                ..
            } => {
                let cols_now = columns.get_untracked();
                let target_col = cols_now.iter().find(|c| c.id == col_id);
                let restored_val = previous_value
                    .clone()
                    .unwrap_or_else(|| empty_cell_value_for_column(target_col));

                rows.update(|list| {
                    if let Some(r) = list.iter_mut().find(|r| r.id == row_id) {
                        if let Some(prev) = &previous_value {
                            r.cells.insert(col_id, prev.clone());
                        } else {
                            r.cells.remove(&col_id);
                        }
                    }
                });

                let mut patch = HashMap::new();
                patch.insert(col_id, restored_val);
                spawn_local(async move {
                    match ApiClient::patch_row(&auth, gid, row_id, patch).await {
                        Ok(updated_row) => {
                            rows.update(|list| {
                                if let Some(r) = list.iter_mut().find(|r| r.id == row_id) {
                                    *r = updated_row;
                                }
                            });
                        }
                        Err(e) => error_banner.set(Some(e)),
                    }
                });
            }
            UndoAction::BulkCellFill { previous_cells } => {
                let cols_now = columns.get_untracked();
                let mut per_row_patch: HashMap<Uuid, RowCellMap> = HashMap::new();

                rows.update(|list| {
                    for (rid, cid, prev_opt) in &previous_cells {
                        if let Some(r) = list.iter_mut().find(|r| r.id == *rid) {
                            if let Some(v) = prev_opt {
                                r.cells.insert(*cid, v.clone());
                            } else {
                                r.cells.remove(cid);
                            }
                        }
                        let target_col = cols_now.iter().find(|c| c.id == *cid);
                        let restored_val = prev_opt
                            .clone()
                            .unwrap_or_else(|| empty_cell_value_for_column(target_col));
                        per_row_patch
                            .entry(*rid)
                            .or_default()
                            .insert(*cid, restored_val);
                    }
                });

                let updates: Vec<BulkRowCellPatch> = per_row_patch
                    .into_iter()
                    .map(|(row_id, cells)| BulkRowCellPatch { row_id, cells })
                    .collect();

                if !updates.is_empty() {
                    spawn_local(async move {
                        if let Err(e) = ApiClient::bulk_update_row_cells(&auth, gid, updates).await
                        {
                            error_banner.set(Some(e));
                        }
                    });
                }
            }
            UndoAction::CreateRows { row_ids } => {
                if row_ids.is_empty() {
                    return;
                }
                let del_set: HashSet<Uuid> = row_ids.iter().copied().collect();
                rows.update(|list| list.retain(|r| !del_set.contains(&r.id)));
                selected_row_ids.update(|set| {
                    for rid in &row_ids {
                        set.remove(rid);
                    }
                });
                spawn_local(async move {
                    if let Err(e) = ApiClient::bulk_delete_rows(&auth, gid, row_ids).await {
                        error_banner.set(Some(e));
                    }
                });
            }
            UndoAction::RowDelete { deleted_rows } => {
                if deleted_rows.is_empty() {
                    return;
                }
                spawn_local(async move {
                    match ApiClient::restore_rows(&auth, gid, deleted_rows).await {
                        Ok(restored_list) => {
                            rows.set(restored_list);
                        }
                        Err(e) => error_banner.set(Some(e)),
                    }
                });
            }
            UndoAction::CreateColumn { col_id } => {
                columns.update(|list| list.retain(|c| c.id != col_id));
                column_widths.update(|map| {
                    map.remove(&col_id);
                });
                rows.update(|rlist| {
                    for r in rlist.iter_mut() {
                        r.cells.remove(&col_id);
                    }
                });
                if selected_column_id.get_untracked() == Some(col_id) {
                    selected_column_id.set(None);
                }
                spawn_local(async move {
                    if let Err(e) = ApiClient::delete_column(&auth, gid, col_id).await {
                        error_banner.set(Some(e));
                    }
                });
            }
            UndoAction::DeleteColumn {
                column,
                cell_values,
                previous_col_order,
            } => {
                let old_col_id = column.id;
                let target_pos = column.position;
                let payload = CreateColumnRequest {
                    name: column.name.clone(),
                    property_type: column.property_type,
                    property_config: Some(column.property_config.clone()),
                    width_px: Some(column.width_px.clamp(80, 800)),
                    position: Some(target_pos),
                    is_primary: Some(column.is_primary),
                };
                spawn_local(async move {
                    match ApiClient::create_column(&auth, gid, payload).await {
                        Ok(created_col) => {
                            let new_col_id = created_col.id;
                            column_widths.update(|map| {
                                map.insert(new_col_id, created_col.width_px.clamp(80, 800));
                            });
                            let ordered_ids: Vec<Uuid> = previous_col_order
                                .into_iter()
                                .map(|id| if id == old_col_id { new_col_id } else { id })
                                .collect();
                            if !ordered_ids.is_empty() {
                                if let Ok(reordered_cols) =
                                    ApiClient::reorder_columns(&auth, gid, ordered_ids).await
                                {
                                    sync_column_widths_from_list(&reordered_cols);
                                    columns.set(reordered_cols);
                                } else {
                                    columns.update(|list| list.push(created_col));
                                }
                            } else {
                                columns.update(|list| list.push(created_col));
                            }

                            if !cell_values.is_empty() {
                                let updates: Vec<BulkRowCellPatch> = cell_values
                                    .into_iter()
                                    .map(|(rid, val)| {
                                        let mut map = HashMap::new();
                                        map.insert(new_col_id, val);
                                        BulkRowCellPatch {
                                            row_id: rid,
                                            cells: map,
                                        }
                                    })
                                    .collect();
                                if let Err(e) =
                                    ApiClient::bulk_update_row_cells(&auth, gid, updates).await
                                {
                                    error_banner.set(Some(e));
                                } else {
                                    reload_rows();
                                }
                            } else {
                                reload_rows();
                            }
                        }
                        Err(e) => error_banner.set(Some(e)),
                    }
                });
            }
            UndoAction::UpdateColumnMeta {
                col_id,
                old_name,
                old_config,
                old_width,
                old_cells,
            } => {
                let clamped_w = old_width.clamp(80, 800);
                column_widths.update(|map| {
                    map.insert(col_id, clamped_w);
                });
                columns.update(|list| {
                    if let Some(c) = list.iter_mut().find(|c| c.id == col_id) {
                        c.name = old_name.clone();
                        c.property_config = old_config.clone();
                        c.width_px = clamped_w;
                    }
                });
                let mut updates: Vec<BulkRowCellPatch> = Vec::new();
                if !old_cells.is_empty() {
                    rows.update(|list| {
                        for (rid, prev_opt) in &old_cells {
                            if let Some(r) = list.iter_mut().find(|r| r.id == *rid) {
                                if let Some(v) = prev_opt {
                                    r.cells.insert(col_id, v.clone());
                                    let mut map = HashMap::new();
                                    map.insert(col_id, v.clone());
                                    updates.push(BulkRowCellPatch {
                                        row_id: *rid,
                                        cells: map,
                                    });
                                } else {
                                    r.cells.remove(&col_id);
                                }
                            }
                        }
                    });
                }
                let req = UpdateColumnRequest {
                    name: Some(old_name),
                    property_type: None,
                    property_config: Some(old_config),
                    width_px: Some(clamped_w),
                    position: None,
                    is_hidden: None,
                };
                spawn_local(async move {
                    match ApiClient::update_column(&auth, gid, col_id, req).await {
                        Ok(updated_col) => {
                            columns.update(|list| {
                                if let Some(c) = list.iter_mut().find(|c| c.id == col_id) {
                                    *c = updated_col;
                                }
                            });
                            if !updates.is_empty() {
                                if let Err(e) =
                                    ApiClient::bulk_update_row_cells(&auth, gid, updates).await
                                {
                                    error_banner.set(Some(e));
                                } else {
                                    reload_rows();
                                }
                            } else {
                                reload_rows();
                            }
                        }
                        Err(e) => error_banner.set(Some(e)),
                    }
                });
            }
            UndoAction::UpdateGridMeta { old_title } => {
                let sid = space_id.get_untracked();
                draft_grid_title.set(old_title.clone());
                spawn_local(async move {
                    match ApiClient::update_grid(&auth, gid, Some(old_title), None).await {
                        Ok(updated_grid) => {
                            draft_grid_title.set(updated_grid.title.clone());
                            grid_meta.set(Some(updated_grid.clone()));
                            auth.space_grids.update(|map| {
                                if let Some(list) = map.get_mut(&sid) {
                                    if let Some(item) = list.iter_mut().find(|g| g.id == gid) {
                                        *item = updated_grid;
                                    }
                                }
                            });
                        }
                        Err(e) => error_banner.set(Some(e)),
                    }
                });
            }
            UndoAction::ColumnResize {
                col_id, old_width, ..
            } => {
                let clamped = old_width.clamp(80, 800);
                column_widths.update(|map| {
                    map.insert(col_id, clamped);
                });
                columns.update(|list| {
                    if let Some(c) = list.iter_mut().find(|c| c.id == col_id) {
                        c.width_px = clamped;
                    }
                });
                let req = UpdateColumnRequest {
                    name: None,
                    property_type: None,
                    property_config: None,
                    width_px: Some(clamped),
                    position: None,
                    is_hidden: None,
                };
                spawn_local(async move {
                    if let Err(e) = ApiClient::update_column(&auth, gid, col_id, req).await {
                        error_banner.set(Some(e));
                    }
                });
            }
            UndoAction::RowReorder { previous_order } => {
                rows.update(|list| {
                    let mut map: HashMap<Uuid, GridRow> =
                        list.drain(..).map(|r| (r.id, r)).collect();
                    for (idx, rid) in previous_order.iter().enumerate() {
                        if let Some(mut r) = map.remove(rid) {
                            r.position = idx as i32;
                            r.row_order = idx as f64;
                            list.push(r);
                        }
                    }
                    let mut remaining: Vec<GridRow> = map.into_values().collect();
                    remaining.sort_by_key(|r| r.position);
                    list.extend(remaining);
                });
                spawn_local(async move {
                    match ApiClient::reorder_rows(&auth, gid, previous_order).await {
                        Ok(updated_rows) => rows.set(updated_rows),
                        Err(e) => error_banner.set(Some(e)),
                    }
                });
            }
            UndoAction::ColumnReorder { previous_order } => {
                columns.update(|list| {
                    let mut map: HashMap<Uuid, GridColumn> =
                        list.drain(..).map(|c| (c.id, c)).collect();
                    for (idx, cid) in previous_order.iter().enumerate() {
                        if let Some(mut c) = map.remove(cid) {
                            c.position = idx as i32;
                            list.push(c);
                        }
                    }
                    let mut remaining: Vec<GridColumn> = map.into_values().collect();
                    remaining.sort_by_key(|c| c.position);
                    list.extend(remaining);
                });
                spawn_local(async move {
                    match ApiClient::reorder_columns(&auth, gid, previous_order).await {
                        Ok(updated_cols) => {
                            sync_column_widths_from_list(&updated_cols);
                            columns.set(updated_cols);
                        }
                        Err(e) => error_banner.set(Some(e)),
                    }
                });
            }
            UndoAction::ColumnTypeConvert {
                col_id,
                previous_type,
                previous_config,
                previous_cells,
            } => {
                let req = UpdateColumnRequest {
                    name: None,
                    property_type: Some(previous_type),
                    property_config: Some(previous_config.clone()),
                    width_px: None,
                    position: None,
                    is_hidden: None,
                };
                columns.update(|list| {
                    if let Some(c) = list.iter_mut().find(|c| c.id == col_id) {
                        c.property_type = previous_type;
                        c.property_config = previous_config.clone();
                    }
                });

                let mut updates: Vec<BulkRowCellPatch> = Vec::new();
                rows.update(|list| {
                    for (rid, prev_opt) in &previous_cells {
                        if let Some(r) = list.iter_mut().find(|r| r.id == *rid) {
                            if let Some(v) = prev_opt {
                                r.cells.insert(col_id, v.clone());
                                let mut map = HashMap::new();
                                map.insert(col_id, v.clone());
                                updates.push(BulkRowCellPatch {
                                    row_id: *rid,
                                    cells: map,
                                });
                            } else {
                                r.cells.remove(&col_id);
                            }
                        }
                    }
                });

                spawn_local(async move {
                    match ApiClient::update_column(&auth, gid, col_id, req).await {
                        Ok(updated_col) => {
                            columns.update(|list| {
                                if let Some(c) = list.iter_mut().find(|c| c.id == col_id) {
                                    *c = updated_col;
                                }
                            });
                            if !updates.is_empty() {
                                if let Err(e) =
                                    ApiClient::bulk_update_row_cells(&auth, gid, updates).await
                                {
                                    error_banner.set(Some(e));
                                } else {
                                    reload_rows();
                                }
                            } else {
                                reload_rows();
                            }
                        }
                        Err(e) => error_banner.set(Some(e)),
                    }
                });
            }
        }
    };

    // Reset Layout (`POST /api/grids/{grid_id}/reset-layout`): resets all column widths to 180px & clears sorts/filters
    let handle_reset_layout = move |_| {
        let gid = grid_id.get_untracked();
        sort_specs.set(Vec::new());
        quick_search.set(String::new());
        server_filter_col.set(None);
        server_filter_val.set(String::new());
        filter_selected_tags.set(HashSet::new());
        show_filter_popover.set(false);

        column_widths.update(|map| {
            for val in map.values_mut() {
                *val = 180;
            }
        });
        columns.update(|list| {
            for c in list.iter_mut() {
                c.width_px = 180;
            }
        });

        spawn_local(async move {
            match ApiClient::reset_grid_layout(&auth, gid).await {
                Ok(updated_cols) => {
                    sync_column_widths_from_list(&updated_cols);
                    columns.set(updated_cols);
                    reload_rows();
                }
                Err(e) => error_banner.set(Some(e)),
            }
        });
    };

    // Open Full-Page History Preview Modal (`GET /api/grids/{grid_id}/history/{history_id}`)
    let open_history_preview = move |history_id: Uuid| {
        let gid = current_grid_id
            .get_untracked()
            .unwrap_or_else(|| grid_id.get_untracked());
        is_loading_preview_snapshot.set(true);
        spawn_local(async move {
            match ApiClient::get_grid_history_detail(&auth, gid, history_id).await {
                Ok(detail) => {
                    is_loading_preview_snapshot.set(false);
                    preview_history_snapshot.set(Some(detail));
                }
                Err(e) => {
                    is_loading_preview_snapshot.set(false);
                    error_banner.set(Some(e));
                }
            }
        });
    };

    // Restore a 30-Day Grid History Snapshot (`POST /api/grids/{grid_id}/history/{history_id}/restore`)
    let handle_restore_snapshot = move |history_id: Uuid| {
        let gid = current_grid_id
            .get_untracked()
            .unwrap_or_else(|| grid_id.get_untracked());
        spawn_local(async move {
            match ApiClient::restore_grid_history(&auth, gid, history_id).await {
                Ok(restored) => {
                    sync_column_widths_from_list(&restored.columns);
                    columns.set(restored.columns);
                    rows.set(restored.rows);
                    preview_history_snapshot.set(None);
                    load_history_entries();
                }
                Err(e) => error_banner.set(Some(e)),
            }
        });
    };

    // Save Inline Grid Page Rename (`PATCH /api/grids/{grid_id}`) and sync Sidebar tree
    let commit_grid_rename = move || {
        editing_grid_title.set(false);
        let next_title = draft_grid_title.get_untracked().trim().to_string();
        if next_title.is_empty() {
            if let Some(g) = grid_meta.get_untracked() {
                draft_grid_title.set(g.title);
            }
            return;
        }
        if let Some(current_meta) = grid_meta.get_untracked() {
            if current_meta.title == next_title {
                return;
            }
            push_undo_action(UndoAction::UpdateGridMeta {
                old_title: current_meta.title.clone(),
            });
        }

        let gid = grid_id.get_untracked();
        let sid = space_id.get_untracked();
        spawn_local(async move {
            match ApiClient::update_grid(&auth, gid, Some(next_title), None).await {
                Ok(updated_grid) => {
                    draft_grid_title.set(updated_grid.title.clone());
                    grid_meta.set(Some(updated_grid.clone()));
                    auth.space_grids.update(|map| {
                        if let Some(list) = map.get_mut(&sid) {
                            if let Some(item) = list.iter_mut().find(|g| g.id == gid) {
                                *item = updated_grid;
                            }
                        }
                    });
                }
                Err(e) => error_banner.set(Some(e)),
            }
        });
    };

    // Click column header to toggle Ascending / Descending sort and re-fetch from PostgreSQL
    let toggle_column_sort = move |col_id: Uuid| {
        sort_specs.update(|specs| {
            if let Some((_, dir)) = specs.iter_mut().find(|(id, _)| *id == col_id) {
                *dir = match *dir {
                    SortDirection::Asc => SortDirection::Desc,
                    SortDirection::Desc => SortDirection::Asc,
                };
            } else {
                specs.clear();
                specs.push((col_id, SortDirection::Asc));
            }
        });
        reload_rows();
    };

    // Add a secondary sort column in the Multi-Column Sort Popover
    let add_multi_sort_column = move |col_id: Uuid, dir: SortDirection| {
        sort_specs.update(|specs| {
            if let Some((_, existing_dir)) = specs.iter_mut().find(|(id, _)| *id == col_id) {
                *existing_dir = dir;
            } else {
                specs.push((col_id, dir));
            }
        });
        reload_rows();
    };

    // Persist column reordering via `PATCH /api/grids/{grid_id}/columns/reorder`
    let persist_column_order = move |ordered_ids: Vec<Uuid>| {
        let gid = grid_id.get_untracked();
        spawn_local(async move {
            match ApiClient::reorder_columns(&auth, gid, ordered_ids).await {
                Ok(updated_cols) => {
                    sync_column_widths_from_list(&updated_cols);
                    columns.set(updated_cols);
                }
                Err(e) => error_banner.set(Some(e)),
            }
        });
    };

    // Drag-and-drop column swap helper
    let handle_drop_column = move |source_col_id: Uuid, target_col_id: Uuid| {
        if source_col_id == target_col_id {
            return;
        }
        let mut current = columns.get_untracked();
        let prev_order: Vec<Uuid> = current.iter().map(|c| c.id).collect();
        let Some(src_idx) = current.iter().position(|c| c.id == source_col_id) else {
            return;
        };
        let Some(dst_idx) = current.iter().position(|c| c.id == target_col_id) else {
            return;
        };
        push_undo_action(UndoAction::ColumnReorder {
            previous_order: prev_order,
        });

        let moved = current.remove(src_idx);
        current.insert(dst_idx, moved);
        for (idx, col) in current.iter_mut().enumerate() {
            col.position = idx as i32;
        }
        let ordered_ids: Vec<Uuid> = current.iter().map(|c| c.id).collect();
        columns.set(current);
        persist_column_order(ordered_ids);
    };

    // Move column left (`dir = -1`) or right (`dir = 1`) from the Column Context Menu
    let move_column_step = move |col_id: Uuid, delta: isize| {
        let mut current = columns.get_untracked();
        let prev_order: Vec<Uuid> = current.iter().map(|c| c.id).collect();
        let Some(idx) = current.iter().position(|c| c.id == col_id) else {
            return;
        };
        let target_idx = idx as isize + delta;
        if target_idx < 0 || target_idx >= current.len() as isize {
            return;
        }
        push_undo_action(UndoAction::ColumnReorder {
            previous_order: prev_order,
        });

        current.swap(idx, target_idx as usize);
        for (i, col) in current.iter_mut().enumerate() {
            col.position = i as i32;
        }
        let ordered_ids: Vec<Uuid> = current.iter().map(|c| c.id).collect();
        columns.set(current);
        persist_column_order(ordered_ids);
    };

    // Drag-and-drop Row Reordering (`PATCH /api/grids/{grid_id}/rows/reorder`)
    let handle_drop_row = move |source_row_id: Uuid, target_row_id: Uuid| {
        if source_row_id == target_row_id {
            return;
        }
        let mut current = rows.get_untracked();
        let prev_order: Vec<Uuid> = current.iter().map(|r| r.id).collect();
        let Some(src_idx) = current.iter().position(|r| r.id == source_row_id) else {
            return;
        };
        let Some(dst_idx) = current.iter().position(|r| r.id == target_row_id) else {
            return;
        };

        push_undo_action(UndoAction::RowReorder {
            previous_order: prev_order,
        });

        let moved = current.remove(src_idx);
        current.insert(dst_idx, moved);
        for (idx, r) in current.iter_mut().enumerate() {
            r.position = idx as i32;
            r.row_order = idx as f64;
        }
        let ordered_row_ids: Vec<Uuid> = current.iter().map(|r| r.id).collect();
        sort_specs.set(Vec::new());
        rows.set(current);

        let gid = grid_id.get_untracked();
        spawn_local(async move {
            match ApiClient::reorder_rows(&auth, gid, ordered_row_ids).await {
                Ok(updated_rows) => rows.set(updated_rows),
                Err(e) => error_banner.set(Some(e)),
            }
        });
    };

    // Save column rename via `PATCH /api/grids/{grid_id}/columns/{col_id}`
    let save_column_rename = move |col_id: Uuid| {
        let new_name = rename_col_input.get_untracked().trim().to_string();
        if new_name.is_empty() {
            return;
        }
        if let Some(existing_col) = columns.get_untracked().into_iter().find(|c| c.id == col_id) {
            if existing_col.name == new_name {
                return;
            }
            push_undo_action(UndoAction::UpdateColumnMeta {
                col_id,
                old_name: existing_col.name,
                old_config: existing_col.property_config,
                old_width: existing_col.width_px,
                old_cells: Vec::new(),
            });
        }
        let gid = grid_id.get_untracked();
        let req = UpdateColumnRequest {
            name: Some(new_name),
            property_type: None,
            property_config: None,
            width_px: None,
            position: None,
            is_hidden: None,
        };
        spawn_local(async move {
            match ApiClient::update_column(&auth, gid, col_id, req).await {
                Ok(updated) => {
                    columns.update(|list| {
                        if let Some(existing) = list.iter_mut().find(|c| c.id == col_id) {
                            *existing = updated;
                        }
                    });
                }
                Err(e) => error_banner.set(Some(e)),
            }
        });
    };

    // Persist `Select` / `MultiSelect` option list changes from the Column Header Settings Menu
    let save_column_select_options =
        move |col_id: Uuid, ptype: ColumnPropertyType, next_opts: Vec<SelectOption>| {
            if let Some(existing_col) = columns.get_untracked().into_iter().find(|c| c.id == col_id) {
                let prev_cells: Vec<(Uuid, Option<CellValue>)> = rows
                    .get_untracked()
                    .into_iter()
                    .map(|r| (r.id, r.cells.get(&col_id).cloned()))
                    .collect();
                push_undo_action(UndoAction::UpdateColumnMeta {
                    col_id,
                    old_name: existing_col.name,
                    old_config: existing_col.property_config,
                    old_width: existing_col.width_px,
                    old_cells: prev_cells,
                });
            }
            let gid = grid_id.get_untracked();
            let cfg = match ptype {
                ColumnPropertyType::Select => ColumnPropertyConfig::Select { options: next_opts },
                _ => ColumnPropertyConfig::MultiSelect { options: next_opts },
            };
            let req = UpdateColumnRequest {
                name: None,
                property_type: None,
                property_config: Some(cfg),
                width_px: None,
                position: None,
                is_hidden: None,
            };
            spawn_local(async move {
                match ApiClient::update_column(&auth, gid, col_id, req).await {
                    Ok(updated_col) => {
                        columns.update(|list| {
                            if let Some(existing) = list.iter_mut().find(|c| c.id == col_id) {
                                *existing = updated_col;
                            }
                        });
                        reload_rows();
                    }
                    Err(e) => error_banner.set(Some(e)),
                }
            });
        };

    // Convert column property type via `PATCH /api/grids/{grid_id}/columns/{col_id}` and reload converted rows
    let change_column_property_type = move |col_id: Uuid, target_type: ColumnPropertyType| {
        let gid = grid_id.get_untracked();
        if let Some(existing_col) = columns
            .get_untracked()
            .into_iter()
            .find(|c| c.id == col_id)
        {
            if existing_col.property_type != target_type {
                let prev_cells: Vec<(Uuid, Option<CellValue>)> = rows
                    .get_untracked()
                    .into_iter()
                    .map(|r| (r.id, r.cells.get(&col_id).cloned()))
                    .collect();
                push_undo_action(UndoAction::ColumnTypeConvert {
                    col_id,
                    previous_type: existing_col.property_type,
                    previous_config: existing_col.property_config,
                    previous_cells: prev_cells,
                });
            }
        }

        let req = UpdateColumnRequest {
            name: None,
            property_type: Some(target_type),
            property_config: None,
            width_px: None,
            position: None,
            is_hidden: None,
        };
        spawn_local(async move {
            match ApiClient::update_column(&auth, gid, col_id, req).await {
                Ok(updated_col) => {
                    columns.update(|list| {
                        if let Some(existing) = list.iter_mut().find(|c| c.id == col_id) {
                            *existing = updated_col;
                        }
                    });
                    reload_rows();
                }
                Err(e) => error_banner.set(Some(e)),
            }
        });
    };

    // Open Row Comments Slide-Over Drawer for `target_row_id`
    let open_row_comments_drawer = move |target_row_id: Uuid| {
        let gid = current_grid_id
            .get_untracked()
            .unwrap_or_else(|| grid_id.get_untracked());
        show_history_drawer.set(false);
        open_comments_row_id.set(Some(target_row_id));
        is_loading_comments.set(true);
        spawn_local(async move {
            match ApiClient::list_row_comments(&auth, gid, target_row_id).await {
                Ok(list) => {
                    is_loading_comments.set(false);
                    row_comments_list.set(list);
                }
                Err(e) => {
                    is_loading_comments.set(false);
                    error_banner.set(Some(e));
                }
            }
        });
    };

    // Post a new comment in the Row Comments Drawer (`POST /api/grids/{grid_id}/rows/{row_id}/comments`)
    let handle_post_row_comment = move |ev: web_sys::SubmitEvent| {
        ev.prevent_default();
        let Some(target_row_id) = open_comments_row_id.get_untracked() else {
            return;
        };
        let content = new_comment_text.get_untracked().trim().to_string();
        if content.is_empty() {
            return;
        }
        let gid = current_grid_id
            .get_untracked()
            .unwrap_or_else(|| grid_id.get_untracked());

        spawn_local(async move {
            match ApiClient::create_row_comment(&auth, gid, target_row_id, content).await {
                Ok(created) => {
                    new_comment_text.set(String::new());
                    row_comments_list.update(|list| list.push(created));
                    row_comment_counts.update(|map| {
                        *map.entry(target_row_id).or_insert(0) += 1;
                    });
                }
                Err(e) => error_banner.set(Some(e)),
            }
        });
    };

    // Delete a comment in the Row Comments Drawer (`DELETE /api/grids/{grid_id}/rows/{row_id}/comments/{comment_id}`)
    let handle_delete_row_comment = move |target_row_id: Uuid, comment_id: Uuid| {
        let gid = current_grid_id
            .get_untracked()
            .unwrap_or_else(|| grid_id.get_untracked());
        spawn_local(async move {
            match ApiClient::delete_row_comment(&auth, gid, target_row_id, comment_id).await {
                Ok(()) => {
                    row_comments_list.update(|list| list.retain(|c| c.id != comment_id));
                    row_comment_counts.update(|map| {
                        if let Some(cnt) = map.get_mut(&target_row_id) {
                            *cnt = (*cnt - 1).max(0);
                        }
                    });
                }
                Err(e) => error_banner.set(Some(e)),
            }
        });
    };

    // Delete column and strip JSONB key via `DELETE /api/grids/{grid_id}/columns/{col_id}`
    let confirm_delete_column = move |col_id: Uuid| {
        let gid = grid_id.get_untracked();
        let cols_snapshot = columns.get_untracked();
        if let Some(target_col) = cols_snapshot.iter().find(|c| c.id == col_id).cloned() {
            let prev_order: Vec<Uuid> = cols_snapshot.iter().map(|c| c.id).collect();
            let cell_values: Vec<(Uuid, CellValue)> = rows
                .get_untracked()
                .into_iter()
                .filter_map(|r| r.cells.get(&col_id).cloned().map(|v| (r.id, v)))
                .collect();
            push_undo_action(UndoAction::DeleteColumn {
                column: target_col,
                cell_values,
                previous_col_order: prev_order,
            });
        }
        spawn_local(async move {
            match ApiClient::delete_column(&auth, gid, col_id).await {
                Ok(_) => {
                    columns.update(|list| list.retain(|c| c.id != col_id));
                    column_widths.update(|map| {
                        map.remove(&col_id);
                    });
                    rows.update(|rlist| {
                        for r in rlist.iter_mut() {
                            r.cells.remove(&col_id);
                        }
                    });
                    if selected_column_id.get_untracked() == Some(col_id) {
                        selected_column_id.set(None);
                    }
                    open_col_menu_id.set(None);
                    confirming_delete_col_id.set(None);
                }
                Err(e) => error_banner.set(Some(e)),
            }
        });
    };

    // Finalize Excel Fill-Handle drag-to-copy and persist via `PATCH /api/grids/{grid_id}/rows/bulk-cells`
    let finish_fill_drag = move || {
        let Some((start_r, start_c, src_row_id, src_col_id)) = fill_drag_start.get_untracked()
        else {
            return;
        };
        let Some((end_r, end_c)) = fill_drag_current.get_untracked() else {
            fill_drag_start.set(None);
            return;
        };

        fill_drag_start.set(None);
        fill_drag_current.set(None);

        if start_r == end_r && start_c == end_c {
            return;
        }

        let visible_cols: Vec<GridColumn> = columns
            .get_untracked()
            .into_iter()
            .filter(|c| !c.is_hidden)
            .collect();
        let Some(src_col) = visible_cols.iter().find(|c| c.id == src_col_id).cloned() else {
            return;
        };

        let filter_q = quick_search.get_untracked();
        let all_cols = columns.get_untracked();
        let members = space_members.get_untracked();
        let f_col = server_filter_col.get_untracked();
        let f_val = server_filter_val.get_untracked();
        let f_tags = filter_selected_tags.get_untracked();
        let all_rows = rows.get_untracked();
        let visible_rows: Vec<GridRow> = all_rows
            .iter()
            .filter(|row| {
                row_matches_universal_filters(
                    row,
                    &all_cols,
                    &members,
                    &filter_q,
                    f_col,
                    &f_val,
                    &f_tags,
                )
            })
            .cloned()
            .collect();

        let Some(src_row) = visible_rows.iter().find(|r| r.id == src_row_id).cloned() else {
            return;
        };
        let src_cell_val = src_row.cells.get(&src_col_id).cloned();

        let dr = (end_r as isize - start_r as isize).unsigned_abs();
        let dc = (end_c as isize - start_c as isize).unsigned_abs();

        let (r_min, r_max, c_min, c_max) = if dr >= dc {
            (start_r.min(end_r), start_r.max(end_r), start_c, start_c)
        } else {
            (start_r, start_r, start_c.min(end_c), start_c.max(end_c))
        };

        let mut bulk_updates: Vec<BulkRowCellPatch> = Vec::new();
        let mut optimistic_map: HashMap<Uuid, RowCellMap> = HashMap::new();
        let mut previous_cells_for_undo: Vec<(Uuid, Uuid, Option<CellValue>)> = Vec::new();

        for r_idx in r_min..=r_max {
            let Some(target_row) = visible_rows.get(r_idx) else {
                continue;
            };
            let mut row_patch: RowCellMap = HashMap::new();

            for c_idx in c_min..=c_max {
                if r_idx == start_r && c_idx == start_c {
                    continue;
                }
                let Some(target_col) = visible_cols.get(c_idx) else {
                    continue;
                };
                if let Some(coerced) = coerce_cell_for_fill(
                    src_cell_val.as_ref(),
                    &src_col,
                    target_col,
                    &members,
                    src_row.last_modified,
                ) {
                    let prev_val = target_row.cells.get(&target_col.id).cloned();
                    previous_cells_for_undo.push((target_row.id, target_col.id, prev_val));
                    row_patch.insert(target_col.id, coerced);
                }
            }

            if !row_patch.is_empty() {
                optimistic_map.insert(target_row.id, row_patch.clone());
                bulk_updates.push(BulkRowCellPatch {
                    row_id: target_row.id,
                    cells: row_patch,
                });
            }
        }

        if bulk_updates.is_empty() {
            return;
        }

        push_undo_action(UndoAction::BulkCellFill {
            previous_cells: previous_cells_for_undo,
        });

        rows.update(|list| {
            for r in list.iter_mut() {
                if let Some(patch_cells) = optimistic_map.get(&r.id) {
                    for (cid, val) in patch_cells {
                        r.cells.insert(*cid, val.clone());
                    }
                }
            }
        });

        let gid = grid_id.get_untracked();
        spawn_local(async move {
            match ApiClient::bulk_update_row_cells(&auth, gid, bulk_updates).await {
                Ok(server_rows) => {
                    let server_map: HashMap<Uuid, GridRow> =
                        server_rows.into_iter().map(|r| (r.id, r)).collect();
                    rows.update(|list| {
                        for r in list.iter_mut() {
                            if let Some(updated) = server_map.get(&r.id) {
                                *r = updated.clone();
                            }
                        }
                    });
                }
                Err(e) => error_banner.set(Some(e)),
            }
        });
    };

    // Global mousemove / mouseup handler on the Grid container for both Independent Column Resize and Fill-Handle Drag
    let on_grid_mousemove = move |ev: web_sys::MouseEvent| {
        if let Some((col_id, start_x, start_w)) = resizing_col.get_untracked() {
            let delta = ev.client_x() - start_x;
            let next_w = (start_w + delta).clamp(80, 800);
            // Update strictly that single column's entry in `column_widths`
            column_widths.update(|map| {
                map.insert(col_id, next_w);
            });
        }
    };

    let on_grid_mouseup = move |_ev: web_sys::MouseEvent| {
        if let Some((col_id, _, start_w)) = resizing_col.get_untracked() {
            resizing_col.set(None);
            let final_w = column_widths
                .get_untracked()
                .get(&col_id)
                .copied()
                .unwrap_or(start_w)
                .clamp(80, 800);

            if final_w != start_w {
                push_undo_action(UndoAction::ColumnResize {
                    col_id,
                    old_width: start_w,
                    new_width: final_w,
                });
            }

            columns.update(|list| {
                if let Some(c) = list.iter_mut().find(|c| c.id == col_id) {
                    c.width_px = final_w;
                }
            });

            let gid = grid_id.get_untracked();
            let req = UpdateColumnRequest {
                name: None,
                property_type: None,
                property_config: None,
                width_px: Some(final_w),
                position: None,
                is_hidden: None,
            };
            spawn_local(async move {
                if let Err(e) = ApiClient::update_column(&auth, gid, col_id, req).await {
                    error_banner.set(Some(e));
                }
            });
        }

        if fill_drag_start.get_untracked().is_some() {
            finish_fill_drag();
        }
    };

    // Helper returning currently visible (quick-filtered & property/tag-filtered) rows in display order
    let get_currently_visible_rows = move || -> Vec<GridRow> {
        let filter_q = quick_search.get_untracked();
        let all_cols = columns.get_untracked();
        let members = space_members.get_untracked();
        let f_col = server_filter_col.get_untracked();
        let f_val = server_filter_val.get_untracked();
        let f_tags = filter_selected_tags.get_untracked();
        rows.get_untracked()
            .into_iter()
            .filter(|row| {
                row_matches_universal_filters(
                    row,
                    &all_cols,
                    &members,
                    &filter_q,
                    f_col,
                    &f_val,
                    &f_tags,
                )
            })
            .collect()
    };

    // Multi-Row Selection Click Handler (`Standard Click`, `Shift + Click` range, `Ctrl/Cmd + Click` toggle)
    let handle_row_select_click =
        move |row_id: Uuid, current_idx: usize, shift_key: bool, ctrl_or_meta_key: bool| {
            let visible_rows = get_currently_visible_rows();
            if shift_key {
                if let Some(last_idx) = last_selected_row_idx.get_untracked() {
                    let start = last_idx.min(current_idx);
                    let end = last_idx.max(current_idx);
                    selected_row_ids.update(|set| {
                        for r in visible_rows
                            .iter()
                            .skip(start)
                            .take(end.saturating_sub(start) + 1)
                        {
                            set.insert(r.id);
                        }
                    });
                    last_selected_row_idx.set(Some(current_idx));
                    return;
                }
            }

            selected_row_ids.update(|set| {
                if set.contains(&row_id) {
                    set.remove(&row_id);
                } else {
                    if !ctrl_or_meta_key {
                        // Standard checkbox/row click toggles individual row cleanly
                    }
                    set.insert(row_id);
                }
            });
            last_selected_row_idx.set(Some(current_idx));
        };

    // Select or Deselect all currently visible rows (`Ctrl+A` or Header Select-All Checkbox)
    let toggle_select_all_visible_rows = move || {
        let visible = get_currently_visible_rows();
        if visible.is_empty() {
            return;
        }
        let current_set = selected_row_ids.get_untracked();
        let all_visible_selected = visible.iter().all(|r| current_set.contains(&r.id));
        if all_visible_selected {
            selected_row_ids.set(HashSet::new());
            last_selected_row_idx.set(None);
        } else {
            let next_set: HashSet<Uuid> = visible.iter().map(|r| r.id).collect();
            selected_row_ids.set(next_set);
        }
    };

    // Bulk Copy Selected Rows (`Ctrl+C` or `📋 Copy Selected Rows (Ctrl+C)` button)
    // Formats selected rows in display order across visible columns as TSV + stores structured row JSON in memory
    let copy_selected_rows_to_clipboard = move || {
        let sel_ids = selected_row_ids.get_untracked();
        if sel_ids.is_empty() {
            return;
        }
        let visible_cols: Vec<GridColumn> = columns
            .get_untracked()
            .into_iter()
            .filter(|c| !c.is_hidden)
            .collect();
        let visible_rows: Vec<GridRow> = get_currently_visible_rows()
            .into_iter()
            .filter(|r| sel_ids.contains(&r.id))
            .collect();

        if visible_rows.is_empty() {
            return;
        }

        let members = space_members.get_untracked();
        let mut tsv_lines: Vec<String> = Vec::with_capacity(visible_rows.len());
        let mut json_rows: Vec<RowCellMap> = Vec::with_capacity(visible_rows.len());

        for r in &visible_rows {
            json_rows.push(r.cells.clone());
            let mut row_cells_tsv: Vec<String> = Vec::with_capacity(visible_cols.len());
            for col in &visible_cols {
                let plain =
                    cell_to_plain_text(r.cells.get(&col.id), col, &members, r.last_modified);
                // Sanitize tabs/newlines inside a single cell so TSV stays aligned in Excel/Sheets
                let sanitized = plain.replace('\t', " ").replace('\n', "; ");
                row_cells_tsv.push(sanitized);
            }
            tsv_lines.push(row_cells_tsv.join("\t"));
        }

        let tsv_payload = tsv_lines.join("\n");
        let _ = copy_to_clipboard(&tsv_payload);
        copied_rows_json_buffer.set(json_rows);

        let count = visible_rows.len();
        copied_rows_toast.set(Some(format!("✓ Copied {count} rows to clipboard!")));
        if let Some(window) = web_sys::window() {
            let cb = Closure::once(move || {
                copied_rows_toast.set(None);
            });
            let _ = window.set_timeout_with_callback_and_timeout_and_arguments_0(
                cb.as_ref().unchecked_ref(),
                2600,
            );
            cb.forget();
        }
    };

    // Keyboard handler (`Escape` to close preview modal or deselect all, `Ctrl+Z` to Undo, `Ctrl+A` Select All, `Ctrl+C` Bulk Copy)
    let on_grid_keydown = move |ev: web_sys::KeyboardEvent| {
        let key = ev.key();
        if key == "Escape" {
            if preview_history_snapshot.get_untracked().is_some() {
                preview_history_snapshot.set(None);
                return;
            }
            clear_all_selections();
            return;
        }

        // Check if focus is currently inside a text input / textarea / select / contenteditable
        let is_editing_input = ev
            .target()
            .and_then(|t| t.dyn_into::<web_sys::HtmlElement>().ok())
            .map(|el| {
                let tag = el.tag_name().to_uppercase();
                tag == "INPUT" || tag == "TEXTAREA" || tag == "SELECT" || el.is_content_editable()
            })
            .unwrap_or(false);

        if (ev.ctrl_key() || ev.meta_key()) && !ev.shift_key() && key.eq_ignore_ascii_case("z") {
            if !is_editing_input {
                ev.prevent_default();
                perform_undo();
            }
            return;
        }

        if (ev.ctrl_key() || ev.meta_key()) && !ev.shift_key() && key.eq_ignore_ascii_case("a") {
            if !is_editing_input {
                ev.prevent_default();
                toggle_select_all_visible_rows();
            }
            return;
        }

        if (ev.ctrl_key() || ev.meta_key()) && !ev.shift_key() && key.eq_ignore_ascii_case("c") {
            if !is_editing_input && !selected_row_ids.get_untracked().is_empty() {
                ev.prevent_default();
                copy_selected_rows_to_clipboard();
            }
        }
    };

    // Helper to determine whether `(r_idx, c_idx)` is inside the active Fill-Handle drag range
    let is_in_fill_drag_range = move |r_idx: usize, c_idx: usize| -> bool {
        let Some((start_r, start_c, _, _)) = fill_drag_start.get() else {
            return false;
        };
        let Some((end_r, end_c)) = fill_drag_current.get() else {
            return false;
        };
        let dr = (end_r as isize - start_r as isize).unsigned_abs();
        let dc = (end_c as isize - start_c as isize).unsigned_abs();

        let (r_min, r_max, c_min, c_max) = if dr >= dc {
            (start_r.min(end_r), start_r.max(end_r), start_c, start_c)
        } else {
            (start_r, start_r, start_c.min(end_c), start_c.max(end_c))
        };

        r_idx >= r_min && r_idx <= r_max && c_idx >= c_min && c_idx <= c_max
    };

    // Handler for `+ New Row` button (appends row at end)
    let handle_add_row = move |_| {
        let gid = grid_id.get();
        let mut initial_cells: RowCellMap = HashMap::new();
        for col in columns.get() {
            if col.is_primary && col.property_type == ColumnPropertyType::Text {
                initial_cells.insert(col.id, CellValue::Text(String::new()));
            }
        }
        spawn_local(async move {
            match ApiClient::create_row(&auth, gid, initial_cells).await {
                Ok(new_row) => {
                    push_undo_action(UndoAction::CreateRows {
                        row_ids: vec![new_row.id],
                    });
                    rows.update(|list| list.push(new_row));
                }
                Err(e) => error_banner.set(Some(e)),
            }
        });
    };

    // Handler for `+ Add row to {Group}` inside a Group Header banner (creates a row pre-populated with that group's tag/value)
    let handle_add_row_to_group =
        move |group_col: GridColumn, option_id: Option<Uuid>, group_label: String| {
            let gid = current_grid_id
                .get_untracked()
                .unwrap_or_else(|| grid_id.get_untracked());
            let mut initial_cells: RowCellMap = HashMap::new();
            for col in columns.get_untracked() {
                if col.is_primary && col.property_type == ColumnPropertyType::Text {
                    initial_cells.insert(col.id, CellValue::Text(String::new()));
                }
            }
            if group_label != "Uncategorized" {
                match group_col.property_type {
                    ColumnPropertyType::Select => {
                        if let Some(oid) = option_id {
                            initial_cells.insert(group_col.id, CellValue::Select(Some(oid)));
                        }
                    }
                    ColumnPropertyType::MultiSelect => {
                        if let Some(oid) = option_id {
                            initial_cells
                                .insert(group_col.id, CellValue::MultiSelect(vec![oid]));
                        }
                    }
                    ColumnPropertyType::Text => {
                        initial_cells.insert(group_col.id, CellValue::Text(group_label));
                    }
                    ColumnPropertyType::Checkbox => {
                        let b = matches!(
                            group_label.to_lowercase().as_str(),
                            "true" | "1" | "yes" | "[x]"
                        );
                        initial_cells.insert(group_col.id, CellValue::Checkbox(b));
                    }
                    _ => {}
                }
            }
            spawn_local(async move {
                match ApiClient::create_row(&auth, gid, initial_cells).await {
                    Ok(new_row) => {
                        push_undo_action(UndoAction::CreateRows {
                            row_ids: vec![new_row.id],
                        });
                        rows.update(|list| list.push(new_row));
                    }
                    Err(e) => error_banner.set(Some(e)),
                }
            });
        };

    // Handler for Notion-style Hover `+` Button (`↑ Insert Row Above` or `↓ Insert Row Below`)
    let handle_insert_row_relative =
        move |target_row_id: Uuid, current_row_pos: i32, placement: &'static str| {
            open_row_insert_menu.set(None);
            let gid = current_grid_id
                .get_untracked()
                .unwrap_or_else(|| grid_id.get_untracked());

            let insert_pos = if placement == "above" {
                current_row_pos.max(0)
            } else {
                (current_row_pos + 1).max(0)
            };

            let mut initial_cells: RowCellMap = HashMap::new();
            for col in columns.get_untracked() {
                if col.is_primary && col.property_type == ColumnPropertyType::Text {
                    initial_cells.insert(col.id, CellValue::Text(String::new()));
                }
            }

            spawn_local(async move {
                match ApiClient::create_row_at(
                    &auth,
                    gid,
                    initial_cells,
                    Some(insert_pos),
                    Some(target_row_id),
                    Some(placement.to_string()),
                )
                .await
                {
                    Ok(new_row) => {
                        push_undo_action(UndoAction::CreateRows {
                            row_ids: vec![new_row.id],
                        });
                        rows.update(|list| {
                            if let Some(target_idx) =
                                list.iter().position(|r| r.id == target_row_id)
                            {
                                let idx = if placement == "above" {
                                    target_idx
                                } else {
                                    (target_idx + 1).min(list.len())
                                };
                                for r in list.iter_mut().skip(idx) {
                                    r.position += 1;
                                }
                                list.insert(idx, new_row);
                            } else {
                                list.push(new_row);
                            }
                        });
                    }
                    Err(e) => error_banner.set(Some(e)),
                }
            });
        };

    // Handler for `+ Add Column` popover submit (shared by Toolbar & End-of-Table `+` Column)
    let handle_create_column = move |ev: web_sys::SubmitEvent| {
        ev.prevent_default();
        let gid = grid_id.get();
        let name = new_col_name.get().trim().to_string();
        if name.is_empty() {
            return;
        }
        let ptype = new_col_type.get();

        let config = match ptype {
            ColumnPropertyType::Select => {
                let options = new_col_options_csv
                    .get()
                    .split(',')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .enumerate()
                    .map(|(i, label)| SelectOption {
                        id: Uuid::new_v4(),
                        label: label.to_string(),
                        color: PALETTE_COLORS[i % PALETTE_COLORS.len()],
                    })
                    .collect();
                Some(ColumnPropertyConfig::Select { options })
            }
            ColumnPropertyType::MultiSelect => {
                let options = new_col_options_csv
                    .get()
                    .split(',')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .enumerate()
                    .map(|(i, label)| SelectOption {
                        id: Uuid::new_v4(),
                        label: label.to_string(),
                        color: PALETTE_COLORS[i % PALETTE_COLORS.len()],
                    })
                    .collect();
                Some(ColumnPropertyConfig::MultiSelect { options })
            }
            ColumnPropertyType::Number => {
                let sym = new_col_currency.get().trim().to_string();
                let format = if sym.is_empty() {
                    NumberFormat::Decimal { precision: 2 }
                } else {
                    NumberFormat::Currency {
                        symbol: sym,
                        precision: 2,
                    }
                };
                Some(ColumnPropertyConfig::Number { format })
            }
            _ => None,
        };

        let payload = CreateColumnRequest {
            name,
            property_type: ptype,
            property_config: config,
            width_px: Some(180),
            position: None,
            is_primary: Some(false),
        };

        spawn_local(async move {
            match ApiClient::create_column(&auth, gid, payload).await {
                Ok(col) => {
                    push_undo_action(UndoAction::CreateColumn { col_id: col.id });
                    column_widths.update(|map| {
                        map.insert(col.id, col.width_px.clamp(80, 800));
                    });
                    columns.update(|list| list.push(col));
                    new_col_name.set(String::new());
                    show_end_add_col_popover.set(false);
                }
                Err(e) => error_banner.set(Some(e)),
            }
        });
    };

    // Handler for Native Bulk Delete (`DELETE /api/grids/{grid_id}/rows/bulk`) with Undo support
    let handle_bulk_delete = move |_| {
        let gid = grid_id.get();
        let ids_set = selected_row_ids.get();
        if ids_set.is_empty() {
            return;
        }
        let ids_to_delete: Vec<Uuid> = ids_set.iter().copied().collect();
        let deleted_snapshot: Vec<GridRow> = rows
            .get_untracked()
            .into_iter()
            .filter(|r| ids_set.contains(&r.id))
            .collect();

        push_undo_action(UndoAction::RowDelete {
            deleted_rows: deleted_snapshot,
        });

        spawn_local(async move {
            match ApiClient::bulk_delete_rows(&auth, gid, ids_to_delete.clone()).await {
                Ok(_) => {
                    let del_set: HashSet<Uuid> = ids_to_delete.into_iter().collect();
                    rows.update(|list| list.retain(|r| !del_set.contains(&r.id)));
                    selected_row_ids.set(HashSet::new());
                }
                Err(e) => error_banner.set(Some(e)),
            }
        });
    };

    // Helper to collect the currently visible columns and currently visible (filtered/sorted) rows for export
    let gather_export_state =
        move || -> (String, String, Vec<GridColumn>, Vec<GridRow>, HashMap<Uuid, String>) {
            let title = grid_meta
                .get_untracked()
                .map(|g| g.title)
                .unwrap_or_else(|| "Dynamic Grid".to_string());
            let sid = current_space_id
                .get_untracked()
                .unwrap_or_else(|| space_id.get_untracked());
            let space_name = auth
                .spaces
                .get_untracked()
                .into_iter()
                .find(|s| s.space.id == sid)
                .map(|s| s.space.name)
                .unwrap_or_else(|| "PyraOcean Workspace".to_string());

            let all_cols = columns.get_untracked();
            let visible_cols: Vec<GridColumn> = all_cols
                .iter()
                .filter(|c| !c.is_hidden)
                .cloned()
                .collect();

            let filter_q = quick_search.get_untracked();
            let members = space_members.get_untracked();
            let f_col = server_filter_col.get_untracked();
            let f_val = server_filter_val.get_untracked();
            let f_tags = filter_selected_tags.get_untracked();
            let visible_rows: Vec<GridRow> = rows
                .get_untracked()
                .into_iter()
                .filter(|row| {
                    row_matches_universal_filters(
                        row,
                        &all_cols,
                        &members,
                        &filter_q,
                        f_col,
                        &f_val,
                        &f_tags,
                    )
                })
                .collect();

            let person_lookup: HashMap<Uuid, String> = members
                .into_iter()
                .map(|m| (m.user_id, m.display_name))
                .collect();
            (title, space_name, visible_cols, visible_rows, person_lookup)
        };

    // Instant 0ms Client-Side CSV Export (`📄 Export as CSV (.csv)`)
    let handle_export_csv = move |_| {
        show_export_menu.set(false);
        let (title, _space_name, visible_cols, visible_rows, person_lookup) = gather_export_state();
        let csv_body = generate_grid_csv(&visible_cols, &visible_rows, &person_lookup);
        let filename = slugify_grid_filename(&title, "csv");
        trigger_client_blob_download(&filename, "text/csv;charset=utf-8", &csv_body);
    };

    // Instant 0ms Client-Side Markdown Export (`📝 Export as Markdown (.md)`)
    let handle_export_markdown = move |_| {
        show_export_menu.set(false);
        let (title, space_name, visible_cols, visible_rows, person_lookup) = gather_export_state();
        let md_body = generate_grid_markdown(
            &title,
            &space_name,
            &visible_cols,
            &visible_rows,
            &person_lookup,
            Utc::now(),
        );
        let filename = slugify_grid_filename(&title, "md");
        trigger_client_blob_download(&filename, "text/markdown;charset=utf-8", &md_body);
    };

    // Executive A3 Landscape Branded PDF Export (`🖨️ Export as PDF (.pdf)`)
    // Preserves active Group-By sections, Department Subtotals, Dual-Currency Grand Totals, and Password masking
    let handle_export_pdf = move |_| {
        show_export_menu.set(false);
        let (title, space_name, visible_cols, visible_rows, person_lookup) = gather_export_state();
        let active_group_col = group_by_col_id.get_untracked();
        let active_calcs = column_calculations.get_untracked();
        let mask_pwds = mask_passwords_in_pdf.get_untracked();
        let html_doc = generate_grid_a3_pdf_html_with_options(
            &title,
            &space_name,
            &visible_cols,
            &visible_rows,
            &person_lookup,
            Utc::now(),
            active_group_col,
            &active_calcs,
            mask_pwds,
        );
        trigger_a3_landscape_pdf_print(&html_doc);
    };

    // Phase 17: Duplicate Selected Rows Here (`👯 Duplicate Selected Rows Here`)
    // Clones the selected rows directly below their current positions inside the same grid
    let handle_duplicate_selected_rows_here = move |_| {
        let gid = grid_id.get_untracked();
        let sel_ids = selected_row_ids.get_untracked();
        if sel_ids.is_empty() {
            return;
        }
        let selected_rows_ordered: Vec<GridRow> = get_currently_visible_rows()
            .into_iter()
            .filter(|r| sel_ids.contains(&r.id))
            .collect();
        if selected_rows_ordered.is_empty() {
            return;
        }

        spawn_local(async move {
            let mut prev_anchor_id: Option<Uuid> = selected_rows_ordered.last().map(|r| r.id);
            let mut prev_pos: i32 = selected_rows_ordered
                .last()
                .map(|r| (r.position + 1).max(0))
                .unwrap_or(0);
            let mut cloned_count = 0usize;

            let mut created_ids: Vec<Uuid> = Vec::new();

            for src_row in selected_rows_ordered {
                match ApiClient::create_row_at(
                    &auth,
                    gid,
                    src_row.cells.clone(),
                    Some(prev_pos),
                    prev_anchor_id,
                    Some("below".to_string()),
                )
                .await
                {
                    Ok(created_row) => {
                        prev_anchor_id = Some(created_row.id);
                        prev_pos = (created_row.position + 1).max(0);
                        created_ids.push(created_row.id);
                        cloned_count += 1;
                    }
                    Err(e) => {
                        error_banner.set(Some(e));
                        break;
                    }
                }
            }

            if !created_ids.is_empty() {
                push_undo_action(UndoAction::CreateRows {
                    row_ids: created_ids,
                });
            }

            if cloned_count > 0 {
                reload_rows();
                copied_rows_toast.set(Some(format!(
                    "✓ Duplicated {cloned_count} row(s) below selection!"
                )));
                if let Some(window) = web_sys::window() {
                    let cb = Closure::once(move || {
                        copied_rows_toast.set(None);
                    });
                    let _ = window.set_timeout_with_callback_and_timeout_and_arguments_0(
                        cb.as_ref().unchecked_ref(),
                        2600,
                    );
                    cb.forget();
                }
            }
        });
    };

    // Phase 17: Open Duplicate Grid Modal (`📑 Duplicate Grid`)
    let open_dup_grid_modal = move |_| {
        let title = grid_meta
            .get_untracked()
            .map(|g| g.title)
            .unwrap_or_else(|| "Grid".to_string());
        dup_grid_name.set(format!("{title} (Copy)"));
        dup_grid_space_id.set(Some(space_id.get_untracked()));
        dup_grid_include_rows.set(true);
        show_dup_grid_modal.set(true);
    };

    // Phase 17: Submit Duplicate Grid (`POST /api/grids/{grid_id}/duplicate`)
    let handle_submit_duplicate_grid = move |ev: web_sys::SubmitEvent| {
        ev.prevent_default();
        let gid = grid_id.get_untracked();
        let target_sid = dup_grid_space_id
            .get_untracked()
            .unwrap_or_else(|| space_id.get_untracked());
        let name = dup_grid_name.get_untracked().trim().to_string();
        let include_rows = dup_grid_include_rows.get_untracked();
        let new_name_opt = if name.is_empty() { None } else { Some(name) };

        spawn_local(async move {
            match ApiClient::duplicate_grid(
                &auth,
                gid,
                Some(target_sid),
                new_name_opt,
                include_rows,
            )
            .await
            {
                Ok(new_grid) => {
                    show_dup_grid_modal.set(false);
                    if let Ok(list) = ApiClient::list_space_grids(&auth, target_sid).await {
                        auth.space_grids.update(|map| {
                            map.insert(target_sid, list);
                        });
                    }
                    auth.active_space_id.set(Some(target_sid));
                    auth.active_grid_id.set(Some(new_grid.id));
                    on_navigate.run(format!("/spaces/{}/grids/{}", target_sid, new_grid.id));
                }
                Err(e) => error_banner.set(Some(e)),
            }
        });
    };

    // Phase 17: Open Move Grid to Another Space Modal (`📂 Move to Space...`)
    let open_move_grid_modal = move |_| {
        let cur_sid = space_id.get_untracked();
        let first_other = auth
            .spaces
            .get_untracked()
            .into_iter()
            .find(|s| s.space.id != cur_sid && auth.can_edit_space(s.space.id))
            .map(|s| s.space.id);
        move_grid_space_id.set(first_other);
        show_move_grid_modal.set(true);
    };

    // Phase 17: Submit Move Grid (`PATCH /api/grids/{grid_id}/move`)
    let handle_submit_move_grid = move |ev: web_sys::SubmitEvent| {
        ev.prevent_default();
        let gid = grid_id.get_untracked();
        let old_sid = space_id.get_untracked();
        let Some(target_sid) = move_grid_space_id.get_untracked() else {
            return;
        };
        if target_sid == old_sid {
            show_move_grid_modal.set(false);
            return;
        }

        spawn_local(async move {
            match ApiClient::move_grid(&auth, gid, target_sid).await {
                Ok(moved_grid) => {
                    show_move_grid_modal.set(false);
                    if let Ok(old_list) = ApiClient::list_space_grids(&auth, old_sid).await {
                        auth.space_grids.update(|map| {
                            map.insert(old_sid, old_list);
                        });
                    }
                    if let Ok(new_list) = ApiClient::list_space_grids(&auth, target_sid).await {
                        auth.space_grids.update(|map| {
                            map.insert(target_sid, new_list);
                        });
                    }
                    auth.active_space_id.set(Some(target_sid));
                    auth.active_grid_id.set(Some(moved_grid.id));
                    on_navigate.run(format!("/spaces/{}/grids/{}", target_sid, moved_grid.id));
                }
                Err(e) => error_banner.set(Some(e)),
            }
        });
    };

    // Phase 17: Open Cross-Grid Row Copy Modal (`↗️ Copy Rows to Another Grid...`)
    let open_copy_rows_modal = move |_| {
        copy_rows_success_banner.set(None);
        copy_rows_auto_create_cols.set(true);
        show_copy_rows_modal.set(true);

        // Ensure all Spaces' grids are loaded into `auth.space_grids` so the grouped dropdown is complete
        let spaces_list = auth.spaces.get_untracked();
        let cur_gid = grid_id.get_untracked();
        spawn_local(async move {
            for sp in spaces_list {
                let sid = sp.space.id;
                if let Ok(glist) = ApiClient::list_space_grids(&auth, sid).await {
                    auth.space_grids.update(|map| {
                        map.insert(sid, glist);
                    });
                }
            }
            // Pick a default target grid (prefer first grid != cur_gid, or cur_gid if only 1 grid exists)
            let map = auth.space_grids.get_untracked();
            let mut default_target: Option<Uuid> = None;
            for (_sid, glist) in &map {
                for g in glist {
                    if g.id != cur_gid {
                        default_target = Some(g.id);
                        break;
                    }
                }
                if default_target.is_some() {
                    break;
                }
            }
            if default_target.is_none() {
                default_target = Some(cur_gid);
            }
            copy_rows_target_grid_id.set(default_target);
        });
    };

    // Phase 17: Submit Cross-Grid Row Copy (`POST /api/grids/{source_grid_id}/rows/copy-to/{target_grid_id}`)
    let handle_submit_copy_rows = move |ev: web_sys::SubmitEvent| {
        ev.prevent_default();
        let src_gid = grid_id.get_untracked();
        let Some(target_gid) = copy_rows_target_grid_id.get_untracked() else {
            return;
        };
        let sel_ids = selected_row_ids.get_untracked();
        if sel_ids.is_empty() {
            return;
        }
        // Preserve display order of selected rows
        let ordered_row_ids: Vec<Uuid> = get_currently_visible_rows()
            .into_iter()
            .filter(|r| sel_ids.contains(&r.id))
            .map(|r| r.id)
            .collect();
        if ordered_row_ids.is_empty() {
            return;
        }
        let auto_create = copy_rows_auto_create_cols.get_untracked();

        // Lookup target grid's space_id and title from `auth.space_grids`
        let mut target_sid = space_id.get_untracked();
        let mut target_title = "Target Grid".to_string();
        for (sid, glist) in auth.space_grids.get_untracked() {
            if let Some(found) = glist.into_iter().find(|g| g.id == target_gid) {
                target_sid = sid;
                target_title = found.title;
                break;
            }
        }

        spawn_local(async move {
            match ApiClient::copy_rows_to_grid(
                &auth,
                src_gid,
                target_gid,
                ordered_row_ids,
                auto_create,
            )
            .await
            {
                Ok(resp) => {
                    if target_gid == src_gid {
                        reload_rows();
                    }
                    copy_rows_success_banner.set(Some((
                        resp.copied_count,
                        resp.created_columns_count,
                        target_sid,
                        target_gid,
                        target_title,
                    )));
                }
                Err(e) => error_banner.set(Some(e)),
            }
        });
    };

    view! {
        <div
            tabindex="-1"
            on:mousemove=on_grid_mousemove
            on:mouseup=on_grid_mouseup
            on:keydown=on_grid_keydown
            on:click=move |_| clear_all_selections()
            class="h-[calc(100vh-3.5rem)] min-h-full flex flex-col overflow-hidden bg-[#14171A] relative focus:outline-none"
        >
            // ================================================================
            // 1. Grid Toolbar: Breadcrumb, Inline Title, Search, Sort, Undo, Reset Layout, History, +Column
            // ================================================================
            <div class="flex-1 min-h-0 flex flex-col overflow-hidden">
                <header class="shrink-0 px-6 py-4 border-b border-[#2A2F37] bg-[#1E2227]/60 flex flex-col lg:flex-row lg:items-center justify-between gap-4">
                    // Space Breadcrumb & Click-to-Edit Grid Title
                    <div class="flex items-center gap-3">
                        <img
                            src=move || auth.active_logo_url()
                            alt="PyraOcean"
                            class="h-9 w-9 rounded-xl object-contain border border-[#06B6D4]/40 shrink-0"
                        />
                        <div>
                            <div class="flex items-center gap-2 text-xs text-slate-400">
                                <span>
                                    {move || {
                                        let sid = space_id.get();
                                        auth.spaces
                                            .get()
                                            .into_iter()
                                            .find(|s| s.space.id == sid)
                                            .map(|s| s.space.name)
                                            .unwrap_or_else(|| "Workspace".into())
                                    }}
                                </span>
                                <span>"/"</span>
                                <span class="text-[#06B6D4] font-semibold">
                                    {move || {
                                        grid_meta
                                            .get()
                                            .map(|g| g.title)
                                            .unwrap_or_else(|| "Loading Grid...".into())
                                    }}
                                </span>
                            </div>
                            <div class="flex items-center gap-2 mt-0.5">
                                <Show
                                    when=move || editing_grid_title.get() && auth.can_edit_space(space_id.get())
                                    fallback=move || {
                                        let can_edit_title = auth.can_edit_space(space_id.get());
                                        view! {
                                            <div
                                                on:click=move |ev| {
                                                    ev.stop_propagation();
                                                    if can_edit_title {
                                                        if let Some(g) = grid_meta.get_untracked() {
                                                            draft_grid_title.set(g.title);
                                                        }
                                                        editing_grid_title.set(true);
                                                    }
                                                }
                                                title=if can_edit_title { "Click to rename Grid Page" } else { "" }
                                                class=if can_edit_title {
                                                    "group/title flex items-center gap-1.5 cursor-pointer rounded px-1.5 -ml-1.5 py-0.5 hover:bg-slate-800/70 transition"
                                                } else {
                                                    "flex items-center gap-1.5"
                                                }
                                            >
                                                <h1 class="text-lg font-bold tracking-tight">
                                                    {move || {
                                                        grid_meta
                                                            .get()
                                                            .map(|g| g.title)
                                                            .unwrap_or_else(|| "Dynamic Grid".into())
                                                    }}
                                                </h1>
                                                <Show when=move || can_edit_title>
                                                    <span class="text-xs text-slate-500 group-hover/title:text-[#06B6D4] transition">
                                                        "✎"
                                                    </span>
                                                </Show>
                                            </div>
                                        }
                                    }
                                >
                                    <input
                                        type="text"
                                        autofocus
                                        on:click=move |ev| ev.stop_propagation()
                                        class="px-2.5 py-0.5 rounded-lg bg-[#14171A] border border-[#06B6D4] text-base font-bold text-white focus:outline-none"
                                        prop:value=draft_grid_title
                                        on:input=move |ev| draft_grid_title.set(event_target_value(&ev))
                                        on:blur=move |_| commit_grid_rename()
                                        on:keydown=move |ev: web_sys::KeyboardEvent| {
                                            ev.stop_propagation();
                                            if ev.key() == "Enter" {
                                                ev.prevent_default();
                                                commit_grid_rename();
                                            } else if ev.key() == "Escape" {
                                                editing_grid_title.set(false);
                                            }
                                        }
                                    />
                                </Show>

                                {move || {
                                    let editable = auth.can_edit_space(space_id.get());
                                    let (label, cls) = if editable {
                                        ("Can Edit", "bg-emerald-500/15 text-emerald-300 border-emerald-500/30")
                                    } else {
                                        ("View Only", "bg-slate-500/15 text-slate-400 border-slate-500/30")
                                    };
                                    view! {
                                        <span class=format!("px-2 py-0.5 rounded-full text-[10px] font-semibold border {cls}")>
                                            {label}
                                        </span>
                                    }
                                }}
                            </div>
                        </div>
                    </div>

                    // Right Toolbar Controls
                    <div
                        on:click=move |ev| ev.stop_propagation()
                        class="flex flex-wrap items-center gap-2"
                    >
                        // Scope 1: Instant Universal Grid Search Bar (Resolves all 12 property types including Select & MultiSelect option labels)
                        <div class="relative flex items-center">
                            <input
                                type="search"
                                placeholder="Filter cells, tags, options..."
                                class="w-48 px-3 py-1.5 rounded-lg bg-[#14171A] border border-[#2A2F37] text-xs text-white focus:outline-none focus:border-[#06B6D4]"
                                prop:value=quick_search
                                on:input=move |ev| quick_search.set(event_target_value(&ev))
                            />
                        </div>

                        // Scope 2 & 3: Global / Scoped Search Modal Trigger
                        <button
                            type="button"
                            title="Search This Grid, This Space, or All Spaces (Ctrl+K)"
                            on:click=move |_| on_open_search_modal.run(())
                            class="px-3 py-1.5 rounded-lg bg-[#14171A] border border-[#2A2F37] hover:border-[#06B6D4] text-xs font-medium text-slate-300 flex items-center gap-1.5 transition"
                        >
                            <span class="text-[#06B6D4]">"🔍"</span>
                            <span>"Scoped Search"</span>
                        </button>

                        // Multi-Step Undo Button (`Ctrl+Z`)
                        <Show when=move || auth.can_edit_space(space_id.get())>
                            <button
                                type="button"
                                title="Undo last action (Ctrl+Z)"
                                disabled=move || undo_stack.get().is_empty()
                                on:click=move |_| perform_undo()
                                class=move || {
                                    let count = undo_stack.get().len();
                                    if count > 0 {
                                        "px-3 py-1.5 rounded-lg bg-[#14171A] border border-[#06B6D4]/50 hover:border-[#06B6D4] text-xs font-semibold text-[#06B6D4] flex items-center gap-1.5 transition cursor-pointer"
                                    } else {
                                        "px-3 py-1.5 rounded-lg bg-[#14171A]/50 border border-[#2A2F37] text-xs font-medium text-slate-600 flex items-center gap-1.5 cursor-not-allowed"
                                    }
                                }
                            >
                                <span>"↶ Undo"</span>
                                {move || {
                                    let count = undo_stack.get().len();
                                    if count > 0 {
                                        view! {
                                            <span class="px-1.5 py-0.2 rounded-full bg-[#06B6D4]/20 text-[#06B6D4] font-mono text-[10px]">
                                                {count}
                                            </span>
                                        }
                                        .into_any()
                                    } else {
                                        view! { <span /> }.into_any()
                                    }
                                }}
                            </button>

                            // Reset Layout Button (`POST /api/grids/{grid_id}/reset-layout`)
                            <button
                                type="button"
                                title="Reset all column widths to 180px and clear active sorts/filters"
                                on:click=handle_reset_layout
                                class="px-3 py-1.5 rounded-lg bg-[#14171A] border border-[#2A2F37] hover:border-[#06B6D4] text-xs font-medium text-slate-300 flex items-center gap-1.5 transition"
                            >
                                <span>"↺ Reset Layout"</span>
                            </button>
                        </Show>

                        // 30-Day Grid History Drawer Trigger
                        <button
                            type="button"
                            title="View 30-Day Grid Version History & Restore Snapshots"
                            on:click=move |_| {
                                let next = !show_history_drawer.get_untracked();
                                show_history_drawer.set(next);
                                if next {
                                    load_history_entries();
                                }
                            }
                            class="px-3 py-1.5 rounded-lg bg-[#14171A] border border-[#2A2F37] hover:border-[#F97316] text-xs font-medium text-slate-300 flex items-center gap-1.5 transition"
                        >
                            <span>"🕒 30-Day History"</span>
                        </button>

                        // Multi-Format Grid Export Dropdown (`CSV`, `Markdown`, `A3 Landscape PDF`)
                        <div class="relative">
                            <button
                                type="button"
                                title="Export Grid as CSV, GitHub Markdown, or Executive A3 Landscape PDF"
                                on:click=move |ev: web_sys::MouseEvent| {
                                    ev.stop_propagation();
                                    let next = !show_export_menu.get_untracked();
                                    if next {
                                        export_menu_style.set(compute_floating_popover_style(&ev, 260.0, 210.0));
                                    }
                                    show_export_menu.set(next);
                                }
                                class="px-3 py-1.5 rounded-lg bg-[#14171A] border border-[#06B6D4]/40 hover:border-[#06B6D4] text-xs font-semibold text-[#06B6D4] flex items-center gap-1.5 transition"
                            >
                                <span>"📥 Export"</span>
                                <span class="text-[10px] opacity-80">"▾"</span>
                            </button>

                            <Show when=move || show_export_menu.get()>
                                <div
                                    style=move || export_menu_style.get()
                                    on:click=move |ev| ev.stop_propagation()
                                    class="fixed z-[9999] rounded-2xl bg-[#1E2227] border border-[#2A2F37] shadow-2xl p-2 space-y-1"
                                >
                                    <div class="px-3 py-1.5 border-b border-[#2A2F37] flex items-center justify-between">
                                        <span class="text-[11px] font-bold uppercase tracking-wider text-slate-400">
                                            "Export Grid Format"
                                        </span>
                                        <span class="text-[10px] font-mono text-[#06B6D4]">
                                            {move || format!("{} rows", rows.get().len())}
                                        </span>
                                    </div>

                                    <button
                                        type="button"
                                        on:click=handle_export_csv
                                        class="w-full px-3 py-2 rounded-xl hover:bg-slate-800/90 text-left flex items-center justify-between gap-2 transition group"
                                    >
                                        <div class="flex items-center gap-2.5">
                                            <span class="text-sm">"📄"</span>
                                            <div>
                                                <div class="text-xs font-semibold text-white group-hover:text-[#06B6D4] transition">
                                                    "Export as CSV (.csv)"
                                                </div>
                                                <div class="text-[10px] text-slate-400">
                                                    "UTF-8 with BOM for Excel & LibreOffice"
                                                </div>
                                            </div>
                                        </div>
                                    </button>

                                    <button
                                        type="button"
                                        on:click=handle_export_markdown
                                        class="w-full px-3 py-2 rounded-xl hover:bg-slate-800/90 text-left flex items-center justify-between gap-2 transition group"
                                    >
                                        <div class="flex items-center gap-2.5">
                                            <span class="text-sm">"📝"</span>
                                            <div>
                                                <div class="text-xs font-semibold text-white group-hover:text-[#06B6D4] transition">
                                                    "Export as Markdown (.md)"
                                                </div>
                                                <div class="text-[10px] text-slate-400">
                                                    "GitHub-Flavored Markdown Pipe Table"
                                                </div>
                                            </div>
                                        </div>
                                    </button>

                                    <button
                                        type="button"
                                        on:click=handle_export_pdf
                                        class="w-full px-3 py-2 rounded-xl hover:bg-slate-800/90 text-left flex items-center justify-between gap-2 transition group"
                                    >
                                        <div class="flex items-center gap-2.5">
                                            <span class="text-sm">"🖨️"</span>
                                            <div>
                                                <div class="text-xs font-semibold text-white group-hover:text-[#F97316] transition">
                                                    "Export as PDF (.pdf)"
                                                </div>
                                                <div class="text-[10px] text-slate-400">
                                                    "Executive A3 Landscape Branded Report"
                                                </div>
                                            </div>
                                        </div>
                                    </button>

                                    <div class="pt-1.5 mt-1 border-t border-slate-800 px-2 flex items-center justify-between gap-2">
                                        <label class="flex items-center gap-2 text-[11px] text-slate-300 cursor-pointer select-none">
                                            <input
                                                type="checkbox"
                                                class="h-3.5 w-3.5 rounded accent-[#06B6D4] cursor-pointer"
                                                prop:checked=move || mask_passwords_in_pdf.get()
                                                on:change=move |ev| mask_passwords_in_pdf.set(event_target_checked(&ev))
                                            />
                                            <span>"Mask 🔒 Passwords in PDF"</span>
                                        </label>
                                    </div>
                                </div>
                            </Show>
                        </div>

                        // Group-By Department / Column Popover Button (`📂 Group By`)
                        <div class="relative" on:click=move |ev| ev.stop_propagation()>
                            <button
                                type="button"
                                on:click=move |_| {
                                    show_sort_popover.set(false);
                                    show_export_menu.set(false);
                                    show_group_by_popover.update(|v| *v = !*v);
                                }
                                class=move || {
                                    if group_by_col_id.get().is_some() {
                                        "px-3 py-1.5 rounded-lg bg-[#06B6D4]/15 border border-[#06B6D4] text-[#06B6D4] text-xs font-semibold flex items-center gap-1.5 transition"
                                    } else {
                                        "px-3 py-1.5 rounded-lg bg-[#14171A] border border-[#2A2F37] hover:border-[#06B6D4] text-xs font-medium flex items-center gap-1.5 transition"
                                    }
                                }
                            >
                                <span>"📂 Group By"</span>
                                {move || {
                                    if let Some(gid_col) = group_by_col_id.get() {
                                        let col_name = columns
                                            .get()
                                            .into_iter()
                                            .find(|c| c.id == gid_col)
                                            .map(|c| c.name)
                                            .unwrap_or_else(|| "Active".to_string());
                                        view! {
                                            <span class="px-1.5 py-0.5 rounded bg-[#06B6D4] text-slate-950 font-bold text-[10px]">
                                                {col_name}
                                            </span>
                                        }
                                        .into_any()
                                    } else {
                                        view! { <span /> }.into_any()
                                    }
                                }}
                            </button>

                            <Show when=move || show_group_by_popover.get()>
                                <div class="absolute right-0 top-10 z-40 w-72 rounded-2xl bg-[#1E2227] border border-[#2A2F37] shadow-2xl p-3.5 space-y-2.5">
                                    <div class="flex items-center justify-between border-b border-[#2A2F37] pb-2">
                                        <div>
                                            <span class="text-xs font-bold text-white block">"Group Rows & Subtotals"</span>
                                            <span class="text-[10px] text-slate-400">"Partition by Department, Tag, or Owner"</span>
                                        </div>
                                        <Show when=move || group_by_col_id.get().is_some()>
                                            <button
                                                type="button"
                                                on:click=move |_| {
                                                    group_by_col_id.set(None);
                                                    collapsed_groups.set(HashSet::new());
                                                    show_group_by_popover.set(false);
                                                }
                                                class="text-[11px] text-[#F97316] hover:underline font-semibold"
                                            >
                                                "Clear"
                                            </button>
                                        </Show>
                                    </div>

                                    <div class="space-y-1 max-h-60 overflow-y-auto">
                                        <button
                                            type="button"
                                            on:click=move |_| {
                                                group_by_col_id.set(None);
                                                collapsed_groups.set(HashSet::new());
                                                show_group_by_popover.set(false);
                                            }
                                            class=move || {
                                                if group_by_col_id.get().is_none() {
                                                    "w-full px-2.5 py-1.5 rounded-lg bg-[#06B6D4]/15 border border-[#06B6D4]/50 text-[#06B6D4] text-xs font-semibold flex items-center justify-between"
                                                } else {
                                                    "w-full px-2.5 py-1.5 rounded-lg hover:bg-slate-800 text-slate-300 text-xs flex items-center justify-between"
                                                }
                                            }
                                        >
                                            <span>"None (Ungrouped)"</span>
                                            {move || if group_by_col_id.get().is_none() { "✓" } else { "" }}
                                        </button>

                                        {move || {
                                            columns
                                                .get()
                                                .into_iter()
                                                .filter(|c| {
                                                    !c.is_hidden
                                                        && matches!(
                                                            c.property_type,
                                                            ColumnPropertyType::Select
                                                                | ColumnPropertyType::MultiSelect
                                                                | ColumnPropertyType::Text
                                                                | ColumnPropertyType::Person
                                                                | ColumnPropertyType::Checkbox
                                                        )
                                                })
                                                .map(|col| {
                                                    let cid = col.id;
                                                    let cname = col.name.clone();
                                                    let icon = property_type_icon(col.property_type);
                                                    let is_active = group_by_col_id.get() == Some(cid);
                                                    view! {
                                                        <button
                                                            type="button"
                                                            on:click=move |_| {
                                                                group_by_col_id.set(Some(cid));
                                                                collapsed_groups.set(HashSet::new());
                                                                show_group_by_popover.set(false);
                                                            }
                                                            class=if is_active {
                                                                "w-full px-2.5 py-1.5 rounded-lg bg-[#06B6D4]/15 border border-[#06B6D4]/50 text-[#06B6D4] text-xs font-semibold flex items-center justify-between gap-2"
                                                            } else {
                                                                "w-full px-2.5 py-1.5 rounded-lg hover:bg-slate-800 text-slate-200 text-xs flex items-center justify-between gap-2"
                                                            }
                                                        >
                                                            <span class="flex items-center gap-2 truncate">
                                                                <span class="text-[11px] text-[#06B6D4] font-mono">{icon}</span>
                                                                <span class="truncate">{cname}</span>
                                                            </span>
                                                            <span class="text-[10px] font-mono text-slate-400 shrink-0">
                                                                {if is_active { "✓ Active" } else { "Group" }}
                                                            </span>
                                                        </button>
                                                    }
                                                })
                                                .collect_view()
                                        }}
                                    </div>
                                </div>
                            </Show>
                        </div>

                        // Interactive Universal Property Filter Popover (`🎯 Filter` across all 12 property types + Select/MultiSelect Tag Badges)
                        <div class="relative" on:click=move |ev| ev.stop_propagation()>
                            <button
                                type="button"
                                title="Filter rows by Column Property Type, Select/MultiSelect Tag Badges, Checkbox State, or Text"
                                on:click=move |_| {
                                    show_sort_popover.set(false);
                                    show_group_by_popover.set(false);
                                    show_export_menu.set(false);
                                    show_filter_popover.update(|v| *v = !*v);
                                }
                                class=move || {
                                    let active = server_filter_col.get().is_some()
                                        || !server_filter_val.get().trim().is_empty()
                                        || !filter_selected_tags.get().is_empty();
                                    if active {
                                        "px-3 py-1.5 rounded-lg bg-[#F97316]/15 border border-[#F97316] text-[#F97316] text-xs font-semibold flex items-center gap-1.5 transition"
                                    } else {
                                        "px-3 py-1.5 rounded-lg bg-[#14171A] border border-[#2A2F37] hover:border-[#06B6D4] text-xs font-medium flex items-center gap-1.5 transition"
                                    }
                                }
                            >
                                <span>"🎯 Filter"</span>
                                {move || {
                                    let tag_cnt = filter_selected_tags.get().len();
                                    let has_text = !server_filter_val.get().trim().is_empty();
                                    let total_active = tag_cnt + usize::from(has_text);
                                    if total_active > 0 {
                                        view! {
                                            <span class="px-1.5 py-0.2 rounded-full bg-[#F97316] text-white font-bold text-[10px]">
                                                {total_active}
                                            </span>
                                        }
                                        .into_any()
                                    } else if let Some(cid) = server_filter_col.get() {
                                        let cname = columns
                                            .get()
                                            .into_iter()
                                            .find(|c| c.id == cid)
                                            .map(|c| c.name)
                                            .unwrap_or_else(|| "Col".to_string());
                                        view! {
                                            <span class="px-1.5 py-0.2 rounded bg-[#F97316]/30 text-orange-200 font-semibold text-[10px]">
                                                {cname}
                                            </span>
                                        }
                                        .into_any()
                                    } else {
                                        view! { <span /> }.into_any()
                                    }
                                }}
                            </button>

                            <Show when=move || show_filter_popover.get()>
                                <div class="absolute right-0 top-10 z-50 w-96 rounded-2xl bg-[#1E2227] border border-[#2A2F37] shadow-2xl p-4 space-y-3">
                                    <div class="flex items-center justify-between border-b border-[#2A2F37] pb-2">
                                        <div>
                                            <span class="text-xs font-bold text-white block">
                                                "Universal Property & Tag Filter"
                                            </span>
                                            <span class="text-[10px] text-slate-400">
                                                "Filter by Select/MultiSelect Tags, Checkbox, Person, Number, or Text"
                                            </span>
                                        </div>
                                        <button
                                            type="button"
                                            on:click=move |_| {
                                                server_filter_col.set(None);
                                                server_filter_val.set(String::new());
                                                filter_selected_tags.set(HashSet::new());
                                            }
                                            class="text-[11px] text-[#F97316] hover:underline font-semibold"
                                        >
                                            "Clear All"
                                        </button>
                                    </div>

                                    // 1. Target Column Selector (All Columns or any of the 12 Property Types)
                                    <div>
                                        <label class="block text-[10px] uppercase tracking-wider text-slate-400 mb-1 font-semibold">
                                            "1. Property Column"
                                        </label>
                                        <select
                                            class="w-full px-2.5 py-1.5 rounded-lg bg-[#14171A] border border-[#2A2F37] text-xs text-white focus:outline-none focus:border-[#06B6D4]"
                                            on:change=move |ev| {
                                                let val = event_target_value(&ev);
                                                filter_selected_tags.set(HashSet::new());
                                                server_filter_val.set(String::new());
                                                if val.is_empty() {
                                                    server_filter_col.set(None);
                                                } else if let Ok(uid) = Uuid::parse_str(&val) {
                                                    server_filter_col.set(Some(uid));
                                                }
                                            }
                                        >
                                            <option
                                                value=""
                                                selected=move || server_filter_col.get().is_none()
                                            >
                                                "🌐 All Columns (Any Property / Tag)"
                                            </option>
                                            {move || {
                                                let active_cid = server_filter_col.get();
                                                columns
                                                    .get()
                                                    .into_iter()
                                                    .filter(|c| !c.is_hidden)
                                                    .map(|col| {
                                                        let cid = col.id;
                                                        let cid_str = cid.to_string();
                                                        let icon = property_type_icon(col.property_type);
                                                        let label = format!("{icon} {} ({:?})", col.name, col.property_type);
                                                        view! {
                                                            <option
                                                                value=cid_str
                                                                selected=move || active_cid == Some(cid)
                                                            >
                                                                {label}
                                                            </option>
                                                        }
                                                    })
                                                    .collect_view()
                                            }}
                                        </select>
                                    </div>

                                    // 2. Interactive Select & MultiSelect Colored Option Badges Picker
                                    {move || {
                                        let cols = columns.get();
                                        let target_cid = server_filter_col.get();

                                        // Collect SelectOption badges from the selected column (if Select/MultiSelect) or from all Select/MultiSelect columns
                                        let mut badge_options: Vec<(String, SelectOption)> = Vec::new();
                                        for col in &cols {
                                            if col.is_hidden {
                                                continue;
                                            }
                                            if let Some(tcid) = target_cid {
                                                if col.id != tcid {
                                                    continue;
                                                }
                                            }
                                            match &col.property_config {
                                                ColumnPropertyConfig::Select { options }
                                                | ColumnPropertyConfig::MultiSelect { options } => {
                                                    for opt in options {
                                                        if !badge_options
                                                            .iter()
                                                            .any(|(_, existing)| existing.label.eq_ignore_ascii_case(&opt.label))
                                                        {
                                                            badge_options.push((col.name.clone(), opt.clone()));
                                                        }
                                                    }
                                                }
                                                _ => {}
                                            }
                                        }

                                        if badge_options.is_empty() {
                                            view! { <div /> }.into_any()
                                        } else {
                                            view! {
                                                <div>
                                                    <div class="flex items-center justify-between mb-1.5">
                                                        <label class="text-[10px] uppercase tracking-wider text-slate-400 font-semibold">
                                                            "2. Click Select / MultiSelect Tags to Filter"
                                                        </label>
                                                        <span class="text-[10px] text-cyan-400 font-mono">
                                                            {move || {
                                                                let n = filter_selected_tags.get().len();
                                                                if n > 0 { format!("{n} selected") } else { "Click badge".to_string() }
                                                            }}
                                                        </span>
                                                    </div>
                                                    <div class="flex flex-wrap gap-1.5 max-h-40 overflow-y-auto p-2 rounded-xl bg-[#14171A] border border-[#2A2F37]">
                                                        {badge_options
                                                            .into_iter()
                                                            .map(|(col_name, opt)| {
                                                                let lbl = opt.label.clone();
                                                                let lbl_title = format!("Filter by tag '{lbl}' ({col_name})");
                                                                let lbl_click = lbl.clone();
                                                                let lbl_check = lbl.clone();
                                                                let color_cls = tag_color_classes(opt.color);
                                                                view! {
                                                                    <button
                                                                        type="button"
                                                                        title=lbl_title
                                                                        on:click=move |_| {
                                                                            let target = lbl_click.clone();
                                                                            filter_selected_tags.update(|set| {
                                                                                if set.contains(&target) {
                                                                                    set.remove(&target);
                                                                                } else {
                                                                                    set.insert(target);
                                                                                }
                                                                            });
                                                                        }
                                                                        class=move || {
                                                                            let is_on = filter_selected_tags.get().contains(&lbl_check);
                                                                            if is_on {
                                                                                format!("px-2.5 py-1 rounded-full text-[11px] font-bold border-2 ring-2 ring-[#06B6D4]/50 transition flex items-center gap-1 {color_cls}")
                                                                            } else {
                                                                                format!("px-2.5 py-1 rounded-full text-[11px] font-medium border opacity-75 hover:opacity-100 transition flex items-center gap-1 {color_cls}")
                                                                            }
                                                                        }
                                                                    >
                                                                        <span>{lbl}</span>
                                                                    </button>
                                                                }
                                                            })
                                                            .collect_view()}
                                                    </div>
                                                </div>
                                            }
                                            .into_any()
                                        }
                                    }}

                                    // 3. Checkbox Quick Toggle when a Checkbox column is selected
                                    {move || {
                                        let target_col = server_filter_col
                                            .get()
                                            .and_then(|cid| columns.get().into_iter().find(|c| c.id == cid));
                                        if let Some(col) = target_col {
                                            if col.property_type == ColumnPropertyType::Checkbox {
                                                return view! {
                                                    <div>
                                                        <label class="block text-[10px] uppercase tracking-wider text-slate-400 mb-1 font-semibold">
                                                            "Checkbox State"
                                                        </label>
                                                        <div class="grid grid-cols-3 gap-1.5">
                                                            <button
                                                                type="button"
                                                                on:click=move |_| server_filter_val.set(String::new())
                                                                class=move || {
                                                                    if server_filter_val.get().trim().is_empty() {
                                                                        "py-1.5 rounded-lg bg-[#06B6D4] text-slate-950 font-bold text-xs"
                                                                    } else {
                                                                        "py-1.5 rounded-lg bg-[#14171A] text-slate-300 border border-[#2A2F37] text-xs"
                                                                    }
                                                                }
                                                            >
                                                                "All"
                                                            </button>
                                                            <button
                                                                type="button"
                                                                on:click=move |_| server_filter_val.set("checked".to_string())
                                                                class=move || {
                                                                    if server_filter_val.get() == "checked" {
                                                                        "py-1.5 rounded-lg bg-emerald-500 text-slate-950 font-bold text-xs"
                                                                    } else {
                                                                        "py-1.5 rounded-lg bg-[#14171A] text-slate-300 border border-[#2A2F37] text-xs"
                                                                    }
                                                                }
                                                            >
                                                                "✓ Checked"
                                                            </button>
                                                            <button
                                                                type="button"
                                                                on:click=move |_| server_filter_val.set("unchecked".to_string())
                                                                class=move || {
                                                                    if server_filter_val.get() == "unchecked" {
                                                                        "py-1.5 rounded-lg bg-orange-500 text-white font-bold text-xs"
                                                                    } else {
                                                                        "py-1.5 rounded-lg bg-[#14171A] text-slate-300 border border-[#2A2F37] text-xs"
                                                                    }
                                                                }
                                                            >
                                                                "Unchecked"
                                                            </button>
                                                        </div>
                                                    </div>
                                                }
                                                .into_any();
                                            }
                                        }
                                        view! { <div /> }.into_any()
                                    }}

                                    // 4. Open Text / Value Search Input (matches partial tag names, numbers, dates, files, checklist items, people)
                                    <div>
                                        <label class="block text-[10px] uppercase tracking-wider text-slate-400 mb-1 font-semibold">
                                            "Text / Value Contains"
                                        </label>
                                        <input
                                            type="search"
                                            placeholder="Type tag (e.g. Marketing, Paid, B2C), number, or text..."
                                            class="w-full px-3 py-1.5 rounded-lg bg-[#14171A] border border-[#2A2F37] text-xs text-white focus:outline-none focus:border-[#06B6D4]"
                                            prop:value=server_filter_val
                                            on:input=move |ev| server_filter_val.set(event_target_value(&ev))
                                        />
                                    </div>
                                </div>
                            </Show>
                        </div>

                        // Multi-Column Sort Popover Button
                        <div class="relative">
                            <button
                                type="button"
                                on:click=move |_| show_sort_popover.update(|s| *s = !*s)
                                class="px-3 py-1.5 rounded-lg bg-[#14171A] border border-[#2A2F37] hover:border-[#06B6D4] text-xs font-medium flex items-center gap-1.5 transition"
                            >
                                <span>"⇅ Sort"</span>
                                {move || {
                                    let count = sort_specs.get().len();
                                    if count > 0 {
                                        view! {
                                            <span class="px-1.5 py-0.2 rounded-full bg-[#06B6D4] text-slate-950 font-bold text-[10px]">
                                                {count}
                                            </span>
                                        }
                                        .into_any()
                                    } else {
                                        view! { <span /> }.into_any()
                                    }
                                }}
                            </button>

                            <Show when=move || show_sort_popover.get()>
                                <div class="absolute right-0 top-10 z-40 w-72 rounded-2xl bg-[#1E2227] border border-[#2A2F37] shadow-2xl p-4 space-y-3">
                                    <div class="flex items-center justify-between border-b border-[#2A2F37] pb-2">
                                        <span class="text-xs font-bold text-white">"Multi-Column JSONB Sort"</span>
                                        <button
                                            type="button"
                                            on:click=move |_| {
                                                sort_specs.set(Vec::new());
                                                reload_rows();
                                            }
                                            class="text-[11px] text-[#F97316] hover:underline"
                                        >
                                            "Reset All"
                                        </button>
                                    </div>

                                    <div class="space-y-1.5 max-h-48 overflow-y-auto">
                                        {move || {
                                            columns
                                                .get()
                                                .into_iter()
                                                .map(|col| {
                                                    let cid = col.id;
                                                    let cname = col.name.clone();
                                                    let active_dir = sort_specs
                                                        .get()
                                                        .into_iter()
                                                        .find(|(id, _)| *id == cid)
                                                        .map(|(_, d)| d);
                                                    view! {
                                                        <div class="flex items-center justify-between gap-2 text-xs py-1">
                                                            <span class="truncate text-slate-200">{cname}</span>
                                                            <div class="flex items-center gap-1 shrink-0">
                                                                <button
                                                                    type="button"
                                                                    on:click=move |_| add_multi_sort_column(cid, SortDirection::Asc)
                                                                    class=if active_dir == Some(SortDirection::Asc) {
                                                                        "px-2 py-0.5 rounded bg-[#06B6D4] text-slate-950 font-bold text-[10px]"
                                                                    } else {
                                                                        "px-2 py-0.5 rounded bg-[#14171A] text-slate-400 text-[10px]"
                                                                    }
                                                                >
                                                                    "ASC ↑"
                                                                </button>
                                                                <button
                                                                    type="button"
                                                                    on:click=move |_| add_multi_sort_column(cid, SortDirection::Desc)
                                                                    class=if active_dir == Some(SortDirection::Desc) {
                                                                        "px-2 py-0.5 rounded bg-[#F97316] text-white font-bold text-[10px]"
                                                                    } else {
                                                                        "px-2 py-0.5 rounded bg-[#14171A] text-slate-400 text-[10px]"
                                                                    }
                                                                >
                                                                    "DESC ↓"
                                                                </button>
                                                            </div>
                                                        </div>
                                                    }
                                                })
                                                .collect_view()
                                        }}
                                    </div>
                                </div>
                            </Show>
                        </div>

                        // Phase 18: `✨ PyraAI` Copilot Toolbar Button
                        <button
                            type="button"
                            title="Open PyraAI Copilot (Natural Language Search, Executive Reports & Autonomous Task Execution)"
                            on:click=move |_| show_ai_drawer.update(|v| *v = !*v)
                            class="px-3 py-1.5 rounded-lg bg-gradient-to-r from-[#06B6D4]/20 to-[#F97316]/20 hover:from-[#06B6D4]/30 hover:to-[#F97316]/30 border border-[#06B6D4]/50 text-xs font-bold text-white flex items-center gap-1.5 transition shadow-sm"
                        >
                            <span>"✨ PyraAI"</span>
                        </button>

                        // Phase 17: `📑 Duplicate Grid` Toolbar Button (`POST /api/grids/{grid_id}/duplicate`)
                        <button
                            type="button"
                            title="Duplicate this Grid (columns & optional rows) into this Space or another Space"
                            on:click=open_dup_grid_modal
                            class="px-3 py-1.5 rounded-lg bg-[#14171A] border border-[#2A2F37] hover:border-[#06B6D4] text-xs font-medium text-slate-300 flex items-center gap-1.5 transition"
                        >
                            <span>"📑 Duplicate Grid"</span>
                        </button>

                        // Conditional Edit Controls: Move to Space & Import (`+ Add Column` is in the far-right `<thead>` header; `Delete Grid` is in the left Sidebar tree)
                        <Show when=move || auth.can_edit_space(space_id.get())>
                            // Phase 17: `📂 Move to Space...` Toolbar Button (`PATCH /api/grids/{grid_id}/move`)
                            <button
                                type="button"
                                title="Move this Grid to another Space"
                                on:click=open_move_grid_modal
                                class="px-3 py-1.5 rounded-lg bg-[#14171A] border border-[#2A2F37] hover:border-[#06B6D4] text-xs font-medium text-slate-300 flex items-center gap-1.5 transition"
                            >
                                <span>"📂 Move to Space..."</span>
                            </button>

                            <button
                                type="button"
                                on:click=move |_| on_open_import.run(space_id.get())
                                class="px-3 py-1.5 rounded-lg bg-[#06B6D4]/15 hover:bg-[#06B6D4]/25 text-[#06B6D4] border border-[#06B6D4]/30 text-xs font-semibold transition"
                            >
                                "⇄ Import"
                            </button>
                        </Show>
                    </div>
                </header>

                // Active Property & Tag Filter Bar (Displays clickable active tag pills and quick filter summary below header)
                <Show when=move || {
                    !quick_search.get().trim().is_empty()
                        || server_filter_col.get().is_some()
                        || !server_filter_val.get().trim().is_empty()
                        || !filter_selected_tags.get().is_empty()
                }>
                    <div
                        on:click=move |ev| ev.stop_propagation()
                        class="px-6 py-2 bg-[#14171A]/95 border-b border-[#2A2F37] flex flex-wrap items-center justify-between gap-2 text-xs"
                    >
                        <div class="flex flex-wrap items-center gap-2">
                            <span class="text-[11px] font-bold uppercase tracking-wider text-slate-400">
                                "Active Filters:"
                            </span>

                            {move || {
                                let q = quick_search.get();
                                if q.trim().is_empty() {
                                    view! { <span /> }.into_any()
                                } else {
                                    view! {
                                        <span class="px-2.5 py-0.5 rounded-full bg-cyan-500/15 border border-cyan-500/40 text-cyan-300 text-[11px] font-semibold flex items-center gap-1.5">
                                            <span>{format!("Search: \"{q}\"")}</span>
                                            <button
                                                type="button"
                                                on:click=move |_| quick_search.set(String::new())
                                                class="hover:text-white font-bold"
                                            >
                                                "✕"
                                            </button>
                                        </span>
                                    }
                                    .into_any()
                                }
                            }}

                            {move || {
                                if let Some(cid) = server_filter_col.get() {
                                    let cname = columns
                                        .get()
                                        .into_iter()
                                        .find(|c| c.id == cid)
                                        .map(|c| c.name)
                                        .unwrap_or_else(|| "Column".to_string());
                                    view! {
                                        <span class="px-2.5 py-0.5 rounded-full bg-purple-500/15 border border-purple-500/40 text-purple-300 text-[11px] font-semibold flex items-center gap-1.5">
                                            <span>{format!("Column: {cname}")}</span>
                                            <button
                                                type="button"
                                                on:click=move |_| server_filter_col.set(None)
                                                class="hover:text-white font-bold"
                                            >
                                                "✕"
                                            </button>
                                        </span>
                                    }
                                    .into_any()
                                } else {
                                    view! { <span /> }.into_any()
                                }
                            }}

                            {move || {
                                let fv = server_filter_val.get();
                                if fv.trim().is_empty() {
                                    view! { <span /> }.into_any()
                                } else {
                                    view! {
                                        <span class="px-2.5 py-0.5 rounded-full bg-orange-500/15 border border-orange-500/40 text-orange-300 text-[11px] font-semibold flex items-center gap-1.5">
                                            <span>{format!("Contains: \"{fv}\"")}</span>
                                            <button
                                                type="button"
                                                on:click=move |_| server_filter_val.set(String::new())
                                                class="hover:text-white font-bold"
                                            >
                                                "✕"
                                            </button>
                                        </span>
                                    }
                                    .into_any()
                                }
                            }}

                            {move || {
                                let tags: Vec<String> = filter_selected_tags.get().into_iter().collect();
                                tags.into_iter()
                                    .map(|tag| {
                                        let tag_remove = tag.clone();
                                        view! {
                                            <span class="px-2.5 py-0.5 rounded-full bg-emerald-500/15 border border-emerald-500/40 text-emerald-300 text-[11px] font-semibold flex items-center gap-1.5">
                                                <span>{format!("🏷️ {tag}")}</span>
                                                <button
                                                    type="button"
                                                    on:click=move |_| {
                                                        let t = tag_remove.clone();
                                                        filter_selected_tags.update(|set| {
                                                            set.remove(&t);
                                                        });
                                                    }
                                                    class="hover:text-white font-bold"
                                                >
                                                    "✕"
                                                </button>
                                            </span>
                                        }
                                    })
                                    .collect_view()
                            }}
                        </div>

                        <button
                            type="button"
                            on:click=move |_| {
                                quick_search.set(String::new());
                                server_filter_col.set(None);
                                server_filter_val.set(String::new());
                                filter_selected_tags.set(HashSet::new());
                            }
                            class="text-[11px] text-[#F97316] hover:underline font-semibold"
                        >
                            "Clear All Filters"
                        </button>
                    </div>
                </Show>

                // Error toast banner if any cell validation or network call fails
                {move || {
                    error_banner.get().map(|msg| {
                        view! {
                            <div
                                on:click=move |ev| ev.stop_propagation()
                                class="mx-6 mt-3 p-3 rounded-xl bg-red-500/15 border border-red-500/40 text-xs text-red-300 flex items-center justify-between"
                            >
                                <span>{msg}</span>
                                <button
                                    type="button"
                                    on:click=move |_| error_banner.set(None)
                                    class="font-bold ml-4"
                                >
                                    "✕"
                                </button>
                            </div>
                        }
                    })
                }}

                // ============================================================
                // 2. Excel-Style Dynamic Spreadsheet Table (Viewport-Pinned Scrollbar + Sticky Header + Strict <colgroup>)
                // ============================================================
                <div class="flex-1 min-h-0 w-full overflow-x-auto overflow-y-auto relative custom-scrollbar px-6 pt-4 pb-8">
                    <table
                        style=move || format!("table-layout: fixed; width: {}px;", total_table_width())
                        class="border-collapse text-left border-t border-l border-[#2A2F37]"
                    >
                        // Explicit <colgroup> locking each column's width strictly to its own `Uuid`
                        <colgroup>
                            <col style="width: 40px;" />
                            <col style="width: 64px;" />
                            {move || {
                                let widths = column_widths.get();
                                columns
                                    .get()
                                    .into_iter()
                                    .filter(|c| !c.is_hidden)
                                    .map(|col| {
                                        let cid = col.id;
                                        let w = widths
                                            .get(&cid)
                                            .copied()
                                            .unwrap_or_else(|| col.width_px.clamp(80, 800));
                                        view! {
                                            <col style=format!("width: {}px;", w) />
                                        }
                                    })
                                    .collect_view()
                            }}
                            <col style="width: 50px;" />
                        </colgroup>

                        <thead class="sticky top-0 z-20 bg-[#1B1F24]">
                            <tr class="border-b border-[#2A2F37] bg-[#1B1F24] text-[11px] uppercase tracking-wider text-slate-400 select-none">
                                // Leftmost Sticky Bulk-Selection Header Checkbox (40px)
                                <th
                                    on:click=move |ev| ev.stop_propagation()
                                    style="width: 40px;"
                                    class="sticky left-0 z-30 bg-[#1B1F24] px-2 py-2.5 border-r border-[#2A2F37] text-center"
                                >
                                    <input
                                        type="checkbox"
                                        title="Select or deselect all visible rows (Ctrl+A)"
                                        class="h-3.5 w-3.5 rounded accent-[#F97316] cursor-pointer"
                                        prop:checked=move || {
                                            let filter_q = quick_search.get();
                                            let all_cols = columns.get();
                                            let members = space_members.get();
                                            let f_col = server_filter_col.get();
                                            let f_val = server_filter_val.get();
                                            let f_tags = filter_selected_tags.get();
                                            let visible: Vec<Uuid> = rows
                                                .get()
                                                .into_iter()
                                                .filter(|row| {
                                                    row_matches_universal_filters(
                                                        row,
                                                        &all_cols,
                                                        &members,
                                                        &filter_q,
                                                        f_col,
                                                        &f_val,
                                                        &f_tags,
                                                    )
                                                })
                                                .map(|r| r.id)
                                                .collect();
                                            let sel = selected_row_ids.get();
                                            !visible.is_empty() && visible.iter().all(|id| sel.contains(id))
                                        }
                                        on:change=move |_| {
                                            toggle_select_all_visible_rows();
                                        }
                                    />
                                </th>

                                // Row Hover `+` Insert + Drag Handle + Index Header (64px)
                                <th
                                    style="width: 64px;"
                                    class="px-1.5 py-2.5 border-r border-[#2A2F37] text-center font-mono text-[10px]"
                                >
                                    "#"
                                </th>

                                // Dynamic Typed Column Headers (Draggable + Strictly Isolated Resizable + Notion-Style Context Menu)
                                {move || {
                                    let can_edit_cols = auth.can_edit_space(space_id.get());
                                    columns
                                        .get()
                                        .into_iter()
                                        .filter(|c| !c.is_hidden)
                                        .map(|col| {
                                            let cid = col.id;
                                            let cname = col.name.clone();
                                            let cname_for_menu = col.name.clone();
                                            let ptype = col.property_type;
                                            let col_config_for_menu = col.property_config.clone();
                                            let initial_col_w = col.width_px.clamp(80, 800);

                                            let sort_badge = move || {
                                                sort_specs
                                                    .get()
                                                    .into_iter()
                                                    .find(|(id, _)| *id == cid)
                                                    .map(|(_, dir)| match dir {
                                                        SortDirection::Asc => "↑",
                                                        SortDirection::Desc => "↓",
                                                    })
                                                    .unwrap_or("")
                                            };

                                            view! {
                                                <th
                                                    on:click=move |ev| ev.stop_propagation()
                                                    draggable=move || if can_edit_cols && resizing_col.get().is_none() { "true" } else { "false" }
                                                    on:dragstart=move |_| {
                                                        if can_edit_cols && resizing_col.get_untracked().is_none() {
                                                            dragging_col_id.set(Some(cid));
                                                        }
                                                    }
                                                    on:dragover=move |ev: web_sys::DragEvent| {
                                                        if can_edit_cols && dragging_col_id.get_untracked().is_some() {
                                                            ev.prevent_default();
                                                            drag_over_col_id.set(Some(cid));
                                                        }
                                                    }
                                                    on:dragleave=move |_| {
                                                        if drag_over_col_id.get_untracked() == Some(cid) {
                                                            drag_over_col_id.set(None);
                                                        }
                                                    }
                                                    on:drop=move |ev: web_sys::DragEvent| {
                                                        if can_edit_cols {
                                                            ev.prevent_default();
                                                            if let Some(src_id) = dragging_col_id.get_untracked() {
                                                                handle_drop_column(src_id, cid);
                                                            }
                                                            dragging_col_id.set(None);
                                                            drag_over_col_id.set(None);
                                                        }
                                                    }
                                                    on:dragend=move |_| {
                                                        dragging_col_id.set(None);
                                                        drag_over_col_id.set(None);
                                                    }
                                                    class=move || {
                                                        let is_col_selected = selected_column_id.get() == Some(cid);
                                                        let is_drag_target = drag_over_col_id.get() == Some(cid);
                                                        let bg_cls = if is_drag_target {
                                                            "bg-[#F97316]/25 border-l-2 border-l-[#F97316]"
                                                        } else if is_col_selected {
                                                            "bg-[#06B6D4]/20 text-white"
                                                        } else {
                                                            "hover:bg-slate-800/50"
                                                        };
                                                        format!("relative px-3 py-2.5 border-r border-[#2A2F37] transition {bg_cls}")
                                                    }
                                                >
                                                    <div class="flex items-center justify-between gap-2 pr-1 overflow-hidden">
                                                        // Left: Drag Handle + Type Icon + Column Name (Click to Select Column & Sort)
                                                        <div
                                                            on:click=move |ev| {
                                                                ev.stop_propagation();
                                                                selected_column_id.update(|cur| {
                                                                    if *cur == Some(cid) {
                                                                        *cur = None;
                                                                    } else {
                                                                        *cur = Some(cid);
                                                                    }
                                                                });
                                                                toggle_column_sort(cid);
                                                            }
                                                            class="flex items-center gap-1.5 truncate cursor-pointer flex-1"
                                                        >
                                                            <Show when=move || can_edit_cols>
                                                                <span
                                                                    title="Drag column header to reorder"
                                                                    class="text-slate-500 hover:text-[#F97316] cursor-grab text-xs"
                                                                >
                                                                    "⋮⋮"
                                                                </span>
                                                            </Show>
                                                            <span class="text-[#06B6D4] font-mono text-[10px]">
                                                                {property_type_icon(ptype)}
                                                            </span>
                                                            <span class="font-semibold text-slate-200 truncate">
                                                                {cname}
                                                            </span>
                                                        </div>

                                                        // Right: Active Sort Indicator + Notion-Style Column Context Menu Trigger (`▾`)
                                                        <div class="flex items-center gap-1 shrink-0">
                                                            <span class="text-[#F97316] font-bold text-xs">
                                                                {sort_badge}
                                                            </span>
                                                            <button
                                                                type="button"
                                                                title="Column Options (Rename, Options, Convert Type, Move, Delete)"
                                                                on:click=move |ev: web_sys::MouseEvent| {
                                                                    ev.stop_propagation();
                                                                    selected_column_id.set(Some(cid));
                                                                    confirming_delete_col_id.set(None);
                                                                    menu_editing_opt_id.set(None);
                                                                    let current_open = open_col_menu_id.get_untracked();
                                                                    if current_open == Some(cid) {
                                                                        open_col_menu_id.set(None);
                                                                    } else {
                                                                        col_menu_popover_style.set(
                                                                            compute_floating_popover_style(&ev, 320.0, 420.0),
                                                                        );
                                                                        rename_col_input.set(cname_for_menu.clone());
                                                                        open_col_menu_id.set(Some(cid));
                                                                    }
                                                                }
                                                                class="px-1.5 py-0.5 rounded hover:bg-slate-700/70 text-slate-400 hover:text-white text-[10px]"
                                                            >
                                                                "▾"
                                                            </button>
                                                        </div>
                                                    </div>

                                                    // 4px Wide Interactive Column Resize Handle on the Right Border
                                                    <div
                                                        title="Drag to resize column width independently (80px – 800px)"
                                                        on:mousedown=move |ev: web_sys::MouseEvent| {
                                                            ev.stop_propagation();
                                                            ev.prevent_default();
                                                            if can_edit_cols {
                                                                let cur_w = column_widths
                                                                    .get_untracked()
                                                                    .get(&cid)
                                                                    .copied()
                                                                    .unwrap_or(initial_col_w);
                                                                resizing_col.set(Some((cid, ev.client_x(), cur_w)));
                                                            }
                                                        }
                                                        on:click=move |ev| ev.stop_propagation()
                                                        class="absolute top-0 right-0 bottom-0 w-1 cursor-col-resize hover:bg-cyan-500 active:bg-cyan-400 z-30 transition-colors"
                                                    />

                                                    // Notion-Style Column Context Menu Popover (Fixed Viewport-Aware)
                                                    <Show when=move || open_col_menu_id.get() == Some(cid)>
                                                        {
                                                            let col_cfg_inner = col_config_for_menu.clone();
                                                            view! {
                                                                <div
                                                                    style=move || col_menu_popover_style.get()
                                                                    on:click=move |ev| ev.stop_propagation()
                                                                    class="fixed z-[9999] w-80 max-h-[80vh] overflow-y-auto rounded-xl bg-slate-900 border border-slate-700 shadow-2xl p-3.5 space-y-3 normal-case tracking-normal text-left font-normal"
                                                                >
                                                                    <div class="flex items-center justify-between border-b border-[#2A2F37] pb-2">
                                                                        <span class="text-xs font-bold text-white">
                                                                            "Column Settings"
                                                                        </span>
                                                                        <button
                                                                            type="button"
                                                                            on:click=move |_| {
                                                                                open_col_menu_id.set(None);
                                                                                confirming_delete_col_id.set(None);
                                                                                menu_editing_opt_id.set(None);
                                                                            }
                                                                            class="text-xs text-slate-400 hover:text-white"
                                                                        >
                                                                            "✕"
                                                                        </button>
                                                                    </div>

                                                                    <Show
                                                                        when=move || can_edit_cols
                                                                        fallback=move || {
                                                                            view! {
                                                                                <p class="text-xs text-slate-400">
                                                                                    "View-only access: column schema modifications are disabled."
                                                                                </p>
                                                                            }
                                                                        }
                                                                    >
                                                                        // 1. Rename Column Inline Input
                                                                        <div class="space-y-1">
                                                                            <label class="block text-[10px] font-semibold uppercase tracking-wider text-slate-400">
                                                                                "Rename Column"
                                                                            </label>
                                                                            <div class="flex items-center gap-1.5">
                                                                                <input
                                                                                    type="text"
                                                                                    class="flex-1 px-2.5 py-1.5 rounded-lg bg-[#14171A] border border-[#2A2F37] text-xs text-white focus:outline-none focus:border-[#06B6D4]"
                                                                                    prop:value=rename_col_input
                                                                                    on:input=move |ev| rename_col_input.set(event_target_value(&ev))
                                                                                    on:keydown=move |ev: web_sys::KeyboardEvent| {
                                                                                        ev.stop_propagation();
                                                                                        if ev.key() == "Enter" {
                                                                                            ev.prevent_default();
                                                                                            save_column_rename(cid);
                                                                                        }
                                                                                    }
                                                                                />
                                                                                <button
                                                                                    type="button"
                                                                                    on:click=move |_| save_column_rename(cid)
                                                                                    class="px-2.5 py-1.5 rounded-lg bg-[#06B6D4] text-slate-950 font-bold text-xs"
                                                                                >
                                                                                    "Save"
                                                                                </button>
                                                                            </div>
                                                                        </div>

                                                                        // 2. Inline Select / MultiSelect Option CRUD inside Column Header Menu
                                                                        {
                                                                            let maybe_opts: Option<Vec<SelectOption>> = match &col_cfg_inner {
                                                                                ColumnPropertyConfig::Select { options } => Some(options.clone()),
                                                                                ColumnPropertyConfig::MultiSelect { options } => Some(options.clone()),
                                                                                _ => None,
                                                                            };
                                                                            if let Some(current_opts) = maybe_opts {
                                                                                let opts_for_list = current_opts.clone();
                                                                                let opts_for_add = current_opts.clone();
                                                                                view! {
                                                                                    <div class="space-y-2 pt-2 border-t border-[#2A2F37]">
                                                                                        <label class="block text-[10px] font-semibold uppercase tracking-wider text-slate-400">
                                                                                            "Select / MultiSelect Options"
                                                                                        </label>
                                                                                        <div class="max-h-40 overflow-y-auto space-y-1 rounded-xl bg-[#14171A] p-1.5 border border-[#2A2F37]">
                                                                                            {opts_for_list
                                                                                                .into_iter()
                                                                                                .map(|opt| {
                                                                                                    let oid = opt.id;
                                                                                                    let olabel = opt.label.clone();
                                                                                                    let olabel_edit = opt.label.clone();
                                                                                                    let ocolor = opt.color;
                                                                                                    let badge_cls = tag_color_classes(opt.color);
                                                                                                    let opts_for_save = current_opts.clone();
                                                                                                    let opts_for_del = current_opts.clone();

                                                                                                    view! {
                                                                                                        <div>
                                                                                                            <Show
                                                                                                                when=move || menu_editing_opt_id.get() == Some(oid)
                                                                                                                fallback=move || {
                                                                                                                    let olabel_chip = olabel.clone();
                                                                                                                    let olabel_init = olabel_edit.clone();
                                                                                                                    let opts_del_inner = opts_for_del.clone();
                                                                                                                    view! {
                                                                                                                        <div class="flex items-center justify-between gap-1 px-1.5 py-1 rounded-lg hover:bg-slate-800/60">
                                                                                                                            <span class=format!("px-2 py-0.5 rounded-full text-[11px] font-medium truncate {badge_cls}")>
                                                                                                                                {olabel_chip}
                                                                                                                            </span>
                                                                                                                            <div class="flex items-center gap-1 shrink-0">
                                                                                                                                <button
                                                                                                                                    type="button"
                                                                                                                                    title="Rename or recolor option"
                                                                                                                                    on:click=move |_| {
                                                                                                                                        menu_editing_opt_label.set(olabel_init.clone());
                                                                                                                                        menu_editing_opt_color.set(ocolor);
                                                                                                                                        menu_editing_opt_id.set(Some(oid));
                                                                                                                                    }
                                                                                                                                    class="px-1 text-[11px] text-slate-400 hover:text-[#06B6D4]"
                                                                                                                                >
                                                                                                                                    "✎"
                                                                                                                                </button>
                                                                                                                                <button
                                                                                                                                    type="button"
                                                                                                                                    title="Delete option"
                                                                                                                                    on:click=move |_| {
                                                                                                                                        let mut next = opts_del_inner.clone();
                                                                                                                                        next.retain(|o| o.id != oid);
                                                                                                                                        save_column_select_options(cid, ptype, next);
                                                                                                                                    }
                                                                                                                                    class="px-1 text-[11px] text-slate-400 hover:text-red-400"
                                                                                                                                >
                                                                                                                                    "×"
                                                                                                                                </button>
                                                                                                                            </div>
                                                                                                                        </div>
                                                                                                                    }
                                                                                                                }
                                                                                                            >
                                                                                                                {
                                                                                                                    let opts_save_inner = opts_for_save.clone();
                                                                                                                    view! {
                                                                                                                        <div class="p-1.5 rounded-lg bg-[#1E2227] border border-[#2A2F37] space-y-1.5">
                                                                                                                            <div class="flex items-center gap-1">
                                                                                                                                <input
                                                                                                                                    type="text"
                                                                                                                                    class="flex-1 px-2 py-1 rounded bg-[#14171A] border border-[#2A2F37] text-xs text-white"
                                                                                                                                    prop:value=menu_editing_opt_label
                                                                                                                                    on:input=move |ev| menu_editing_opt_label.set(event_target_value(&ev))
                                                                                                                                    on:keydown=move |ev: web_sys::KeyboardEvent| {
                                                                                                                                        ev.stop_propagation();
                                                                                                                                    }
                                                                                                                                />
                                                                                                                                <button
                                                                                                                                    type="button"
                                                                                                                                    on:click=move |_| {
                                                                                                                                        let lbl = menu_editing_opt_label.get_untracked().trim().to_string();
                                                                                                                                        if !lbl.is_empty() {
                                                                                                                                            let mut next = opts_save_inner.clone();
                                                                                                                                            if let Some(item) = next.iter_mut().find(|o| o.id == oid) {
                                                                                                                                                item.label = lbl;
                                                                                                                                                item.color = menu_editing_opt_color.get_untracked();
                                                                                                                                            }
                                                                                                                                            menu_editing_opt_id.set(None);
                                                                                                                                            save_column_select_options(cid, ptype, next);
                                                                                                                                        }
                                                                                                                                    }
                                                                                                                                    class="px-2 py-1 rounded bg-[#06B6D4] text-slate-950 font-bold text-[10px]"
                                                                                                                                >
                                                                                                                                    "Save"
                                                                                                                                </button>
                                                                                                                            </div>
                                                                                                                            <div class="flex flex-wrap gap-1">
                                                                                                                                {EIGHT_TAG_COLORS
                                                                                                                                    .into_iter()
                                                                                                                                    .map(|(tc, tc_name, _)| {
                                                                                                                                        let pill_cls = tag_color_classes(tc);
                                                                                                                                        view! {
                                                                                                                                            <button
                                                                                                                                                type="button"
                                                                                                                                                on:click=move |_| menu_editing_opt_color.set(tc)
                                                                                                                                                class=move || {
                                                                                                                                                    let ring = if menu_editing_opt_color.get() == tc {
                                                                                                                                                        "ring-2 ring-white"
                                                                                                                                                    } else {
                                                                                                                                                        "opacity-75 hover:opacity-100"
                                                                                                                                                    };
                                                                                                                                                    format!("px-1.5 py-0.5 rounded text-[9px] font-semibold {pill_cls} {ring}")
                                                                                                                                                }
                                                                                                                                            >
                                                                                                                                                {tc_name}
                                                                                                                                            </button>
                                                                                                                                        }
                                                                                                                                    })
                                                                                                                                    .collect_view()}
                                                                                                                            </div>
                                                                                                                        </div>
                                                                                                                    }
                                                                                                                }
                                                                                                            </Show>
                                                                                                        </div>
                                                                                                    }
                                                                                                })
                                                                                                .collect_view()}
                                                                                        </div>

                                                                                        // Add New Option Inline inside Column Settings Menu
                                                                                        <div class="space-y-1.5 pt-1">
                                                                                            <div class="flex items-center gap-1">
                                                                                                <input
                                                                                                    type="text"
                                                                                                    placeholder="+ Add option..."
                                                                                                    class="flex-1 px-2 py-1 rounded-lg bg-[#14171A] border border-[#2A2F37] text-xs text-white"
                                                                                                    prop:value=menu_new_opt_label
                                                                                                    on:input=move |ev| menu_new_opt_label.set(event_target_value(&ev))
                                                                                                    on:keydown=move |ev: web_sys::KeyboardEvent| {
                                                                                                        ev.stop_propagation();
                                                                                                    }
                                                                                                />
                                                                                                <button
                                                                                                    type="button"
                                                                                                    on:click=move |_| {
                                                                                                        let lbl = menu_new_opt_label.get_untracked().trim().to_string();
                                                                                                        if !lbl.is_empty() {
                                                                                                            let mut next = opts_for_add.clone();
                                                                                                            next.push(SelectOption {
                                                                                                                id: Uuid::new_v4(),
                                                                                                                label: lbl,
                                                                                                                color: menu_new_opt_color.get_untracked(),
                                                                                                            });
                                                                                                            menu_new_opt_label.set(String::new());
                                                                                                            save_column_select_options(cid, ptype, next);
                                                                                                        }
                                                                                                    }
                                                                                                    class="px-2.5 py-1 rounded-lg bg-[#F97316] hover:bg-[#EA580C] text-white font-semibold text-xs"
                                                                                                >
                                                                                                    "Add"
                                                                                                </button>
                                                                                            </div>
                                                                                            <div class="flex flex-wrap gap-1">
                                                                                                {EIGHT_TAG_COLORS
                                                                                                    .into_iter()
                                                                                                    .map(|(tc, tc_name, _)| {
                                                                                                        let pill_cls = tag_color_classes(tc);
                                                                                                        view! {
                                                                                                            <button
                                                                                                                type="button"
                                                                                                                on:click=move |_| menu_new_opt_color.set(tc)
                                                                                                                class=move || {
                                                                                                                    let ring = if menu_new_opt_color.get() == tc {
                                                                                                                        "ring-2 ring-white"
                                                                                                                    } else {
                                                                                                                        "opacity-70 hover:opacity-100"
                                                                                                                    };
                                                                                                                    format!("px-1.5 py-0.5 rounded text-[9px] font-semibold {pill_cls} {ring}")
                                                                                                                }
                                                                                                            >
                                                                                                                {tc_name}
                                                                                                            </button>
                                                                                                        }
                                                                                                    })
                                                                                                    .collect_view()}
                                                                                            </div>
                                                                                        </div>
                                                                                    </div>
                                                                                }
                                                                                .into_any()
                                                                            } else {
                                                                                view! { <span /> }.into_any()
                                                                            }
                                                                        }

                                                                        // 3. Change Property Type (All 11 Engines with Lossless Conversion)
                                                                        <div class="space-y-1 pt-2 border-t border-[#2A2F37]">
                                                                            <label class="block text-[10px] font-semibold uppercase tracking-wider text-slate-400">
                                                                                "Change Property Type (11 Engines)"
                                                                            </label>
                                                                            <div class="max-h-36 overflow-y-auto space-y-0.5 rounded-xl bg-[#14171A] p-1.5 border border-[#2A2F37]">
                                                                                {ALL_PROPERTY_TYPES
                                                                                    .into_iter()
                                                                                    .map(|(target_pt, label)| {
                                                                                        let is_current = ptype == target_pt;
                                                                                        let btn_cls = if is_current {
                                                                                            "bg-[#06B6D4]/20 text-[#06B6D4] font-semibold"
                                                                                        } else {
                                                                                            "text-slate-300 hover:bg-slate-800"
                                                                                        };
                                                                                        view! {
                                                                                            <button
                                                                                                type="button"
                                                                                                on:click=move |_| {
                                                                                                    if !is_current {
                                                                                                        change_column_property_type(cid, target_pt);
                                                                                                    }
                                                                                                }
                                                                                                class=format!("w-full text-left px-2 py-1 rounded-lg text-[11px] flex items-center justify-between transition {btn_cls}")
                                                                                            >
                                                                                                <span class="truncate">
                                                                                                    {property_type_icon(target_pt)} " " {label}
                                                                                                </span>
                                                                                                {if is_current {
                                                                                                    view! { <span class="text-[10px]">"✓"</span> }.into_any()
                                                                                                } else {
                                                                                                    view! { <span /> }.into_any()
                                                                                                }}
                                                                                            </button>
                                                                                        }
                                                                                    })
                                                                                    .collect_view()}
                                                                            </div>
                                                                        </div>

                                                                        // 4. Move Left / Move Right Buttons
                                                                        <div class="flex items-center gap-1.5 pt-1">
                                                                            <button
                                                                                type="button"
                                                                                on:click=move |_| move_column_step(cid, -1)
                                                                                class="flex-1 px-2.5 py-1.5 rounded-lg bg-[#14171A] hover:bg-slate-800 border border-[#2A2F37] text-xs text-slate-200 font-medium transition"
                                                                            >
                                                                                "← Move Left"
                                                                            </button>
                                                                            <button
                                                                                type="button"
                                                                                on:click=move |_| move_column_step(cid, 1)
                                                                                class="flex-1 px-2.5 py-1.5 rounded-lg bg-[#14171A] hover:bg-slate-800 border border-[#2A2F37] text-xs text-slate-200 font-medium transition"
                                                                            >
                                                                                "Move Right →"
                                                                            </button>
                                                                        </div>

                                                                        // 5. Delete Column with Confirmation
                                                                        <div class="pt-2 border-t border-[#2A2F37]">
                                                                            <Show
                                                                                when=move || confirming_delete_col_id.get() == Some(cid)
                                                                                fallback=move || {
                                                                                    view! {
                                                                                        <button
                                                                                            type="button"
                                                                                            on:click=move |_| confirming_delete_col_id.set(Some(cid))
                                                                                            class="w-full py-1.5 rounded-lg bg-red-500/15 hover:bg-red-500/25 border border-red-500/30 text-red-400 text-xs font-semibold transition"
                                                                                        >
                                                                                            "🗑 Delete Column"
                                                                                        </button>
                                                                                    }
                                                                                }
                                                                            >
                                                                                <div class="space-y-1.5 p-2 rounded-xl bg-red-950/40 border border-red-500/40">
                                                                                    <p class="text-[11px] text-red-200">
                                                                                        "Delete this column and remove its values from all rows?"
                                                                                    </p>
                                                                                    <div class="flex items-center gap-1.5">
                                                                                        <button
                                                                                            type="button"
                                                                                            on:click=move |_| confirm_delete_column(cid)
                                                                                            class="flex-1 py-1 rounded bg-red-600 hover:bg-red-500 text-white text-xs font-bold"
                                                                                        >
                                                                                            "Confirm Delete"
                                                                                        </button>
                                                                                        <button
                                                                                            type="button"
                                                                                            on:click=move |_| confirming_delete_col_id.set(None)
                                                                                            class="px-2.5 py-1 rounded bg-slate-800 text-slate-300 text-xs"
                                                                                        >
                                                                                            "Cancel"
                                                                                        </button>
                                                                                    </div>
                                                                                </div>
                                                                            </Show>
                                                                        </div>
                                                                    </Show>
                                                                </div>
                                                            }
                                                        }
                                                    </Show>
                                                </th>
                                            }
                                        })
                                        .collect_view()
                                }}

                                // ============================================================
                                // Notion-Style `+` End Column Header for Instant Creation (50px)
                                // ============================================================
                                <th
                                    on:click=move |ev| ev.stop_propagation()
                                    style="width: 50px;"
                                    class="relative px-2 py-2.5 border-r border-[#2A2F37] text-center hover:bg-slate-800/50 cursor-pointer transition"
                                >
                                    <Show
                                        when=move || auth.can_edit_space(space_id.get())
                                        fallback=move || view! { <span class="text-slate-600">"+"</span> }
                                    >
                                        <button
                                            type="button"
                                            title="Add a new column to the end of this Grid"
                                            on:click=move |ev: web_sys::MouseEvent| {
                                                ev.stop_propagation();
                                                end_add_col_popover_style.set(
                                                    compute_floating_popover_style(&ev, 320.0, 360.0),
                                                );
                                                show_end_add_col_popover.update(|v| *v = !*v);
                                            }
                                            class="w-full h-full flex items-center justify-center text-base font-bold text-[#06B6D4] hover:text-[#F97316] transition"
                                        >
                                            "+"
                                        </button>

                                        <Show when=move || show_end_add_col_popover.get()>
                                            <div
                                                style=move || end_add_col_popover_style.get()
                                                on:click=move |ev| ev.stop_propagation()
                                                class="fixed z-[9999] w-80 max-h-80 overflow-y-auto rounded-lg bg-slate-900 border border-slate-700 shadow-2xl p-4 space-y-3 normal-case tracking-normal text-left font-normal"
                                            >
                                                <div class="flex items-center justify-between border-b border-[#2A2F37] pb-2">
                                                    <span class="text-xs font-bold text-white">"New Column"</span>
                                                    <button
                                                        type="button"
                                                        on:click=move |_| show_end_add_col_popover.set(false)
                                                        class="text-xs text-slate-400 hover:text-white"
                                                    >
                                                        "✕"
                                                    </button>
                                                </div>

                                                <form class="space-y-3" on:submit=handle_create_column>
                                                    <div>
                                                        <label class="block text-[11px] text-slate-400 mb-1">"Column Name"</label>
                                                        <input
                                                            type="text"
                                                            required
                                                            autofocus
                                                            placeholder="e.g. Priority, Owner, Notes"
                                                            class="w-full px-2.5 py-1.5 rounded-lg bg-[#14171A] border border-[#2A2F37] text-xs text-white"
                                                            prop:value=new_col_name
                                                            on:input=move |ev| new_col_name.set(event_target_value(&ev))
                                                        />
                                                    </div>

                                                    <div>
                                                        <label class="block text-[11px] text-slate-400 mb-1">
                                                            "Property Type (12 Strict Engines)"
                                                        </label>
                                                        <select
                                                            class="w-full px-2.5 py-1.5 rounded-lg bg-[#14171A] border border-[#2A2F37] text-xs text-white"
                                                            on:change=move |ev| {
                                                                let val = event_target_value(&ev);
                                                                let pt = match val.as_str() {
                                                                    "number" => ColumnPropertyType::Number,
                                                                    "select" => ColumnPropertyType::Select,
                                                                    "multi_select" => ColumnPropertyType::MultiSelect,
                                                                    "date" => ColumnPropertyType::Date,
                                                                    "time" => ColumnPropertyType::Time,
                                                                    "files" => ColumnPropertyType::Files,
                                                                    "checkbox" => ColumnPropertyType::Checkbox,
                                                                    "checklist" => ColumnPropertyType::Checklist,
                                                                    "last_modified" => ColumnPropertyType::LastModified,
                                                                    "person" => ColumnPropertyType::Person,
                                                                    "password" => ColumnPropertyType::Password,
                                                                    _ => ColumnPropertyType::Text,
                                                                };
                                                                new_col_type.set(pt);
                                                            }
                                                        >
                                                            <option value="text">"1. Text (Rich / Plain String)"</option>
                                                            <option value="number">"2. Number (Decimal / Currency)"</option>
                                                            <option value="select">"3. Select (Single Colored Tag)"</option>
                                                            <option value="multi_select">"4. MultiSelect (Multiple Colored Tags)"</option>
                                                            <option value="date">"5. Date (ISO-8601 Picker)"</option>
                                                            <option value="time">"6. Time (Time of Day)"</option>
                                                            <option value="files">"7. Files (Multipart Attachments)"</option>
                                                            <option value="checkbox">"8. Checkbox (Boolean Toggle)"</option>
                                                            <option value="checklist">"9. Checklist (Sub-items & Progress Bar)"</option>
                                                            <option value="last_modified">"10. LastModified (Auto Timestamp)"</option>
                                                            <option value="person">"11. Person (Workspace User Reference)"</option>
                                                            <option value="password">"12. 🔒 Password (Masked Secret)"</option>
                                                        </select>
                                                    </div>

                                                    <Show when=move || {
                                                        matches!(
                                                            new_col_type.get(),
                                                            ColumnPropertyType::Select | ColumnPropertyType::MultiSelect
                                                        )
                                                    }>
                                                        <div>
                                                            <label class="block text-[11px] text-slate-400 mb-1">
                                                                "Option Labels (comma-separated)"
                                                            </label>
                                                            <input
                                                                type="text"
                                                                placeholder="Todo, In Progress, Done"
                                                                class="w-full px-2.5 py-1.5 rounded-lg bg-[#14171A] border border-[#2A2F37] text-xs text-white"
                                                                prop:value=new_col_options_csv
                                                                on:input=move |ev| new_col_options_csv.set(event_target_value(&ev))
                                                            />
                                                        </div>
                                                    </Show>

                                                    <Show when=move || new_col_type.get() == ColumnPropertyType::Number>
                                                        <div>
                                                            <label class="block text-[11px] text-slate-400 mb-1">
                                                                "Currency Symbol (optional)"
                                                            </label>
                                                            <input
                                                                type="text"
                                                                placeholder="EGP"
                                                                class="w-full px-2.5 py-1.5 rounded-lg bg-[#14171A] border border-[#2A2F37] text-xs text-white"
                                                                prop:value=new_col_currency
                                                                on:input=move |ev| new_col_currency.set(event_target_value(&ev))
                                                            />
                                                        </div>
                                                    </Show>

                                                    <button
                                                        type="submit"
                                                        class="w-full py-2 rounded-lg bg-[#F97316] hover:bg-[#EA580C] text-white text-xs font-semibold shadow"
                                                    >
                                                        "Create Column"
                                                    </button>
                                                </form>
                                            </div>
                                        </Show>
                                    </Show>
                                </th>
                            </tr>
                        </thead>

                        <tbody class="divide-y divide-[#2A2F37]/70">
                            {move || {
                                let filter_q = quick_search.get().trim().to_lowercase();
                                let cols = columns.get();
                                let members = space_members.get();
                                let f_col = server_filter_col.get();
                                let f_val = server_filter_val.get();
                                let f_tags = filter_selected_tags.get();
                                let visible_cols: Vec<GridColumn> =
                                    cols.iter().filter(|c| !c.is_hidden).cloned().collect();
                                let can_edit = auth.can_edit_space(space_id.get());
                                let gid = grid_id.get();

                                let filtered_rows: Vec<GridRow> = rows
                                    .get()
                                    .into_iter()
                                    .filter(|row| {
                                        row_matches_universal_filters(
                                            row,
                                            &cols,
                                            &members,
                                            &filter_q,
                                            f_col,
                                            &f_val,
                                            &f_tags,
                                        )
                                    })
                                    .collect();

                                let render_data_row = move |r_idx: usize,
                                                            row: GridRow,
                                                            cols_for_row: Vec<GridColumn>,
                                                            filter_q_for_row: String|
                                      -> AnyView {
                                    let rid = row.id;
                                    let row_pos = if row.position > 0 || r_idx == 0 {
                                        row.position
                                    } else {
                                        r_idx as i32
                                    };

                                    view! {
                                        <tr
                                            id=format!("grid-row-{rid}")
                                            on:dragover=move |ev: web_sys::DragEvent| {
                                                if can_edit && dragging_row_id.get_untracked().is_some() {
                                                    ev.prevent_default();
                                                    drag_over_row_id.set(Some(rid));
                                                }
                                            }
                                            on:dragleave=move |_| {
                                                if drag_over_row_id.get_untracked() == Some(rid) {
                                                    drag_over_row_id.set(None);
                                                }
                                            }
                                            on:drop=move |ev: web_sys::DragEvent| {
                                                if can_edit {
                                                    ev.prevent_default();
                                                    if let Some(src_rid) = dragging_row_id.get_untracked() {
                                                        handle_drop_row(src_rid, rid);
                                                    }
                                                    dragging_row_id.set(None);
                                                    drag_over_row_id.set(None);
                                                }
                                            }
                                            class=move || {
                                                let is_sel = selected_row_ids.get().contains(&rid);
                                                let is_search_target = auth.highlighted_row_id.get() == Some(rid);
                                                let is_row_drop_target = drag_over_row_id.get() == Some(rid);
                                                let drop_cls = if is_row_drop_target {
                                                    "border-t-2 border-cyan-500 bg-cyan-500/10"
                                                } else {
                                                    ""
                                                };
                                                let base_cls = if is_search_target {
                                                    "group bg-[#F97316]/20 ring-1 ring-[#F97316] transition-colors"
                                                } else if is_sel {
                                                    "group bg-cyan-950/40 border-l-2 border-l-cyan-400 hover:bg-cyan-950/60 transition-colors"
                                                } else {
                                                    "group hover:bg-slate-800/40 transition-colors"
                                                };
                                                format!("{base_cls} {drop_cls}")
                                            }
                                        >
                                            // Sticky Left Checkbox Cell (40px)
                                            <td
                                                on:click=move |ev: web_sys::MouseEvent| {
                                                    ev.stop_propagation();
                                                    handle_row_select_click(
                                                        rid,
                                                        r_idx,
                                                        ev.shift_key(),
                                                        ev.ctrl_key() || ev.meta_key(),
                                                    );
                                                }
                                                style="width: 40px;"
                                                class="sticky left-0 z-10 bg-[#14171A] px-2 py-1 border-r border-[#2A2F37] text-center group/cell relative cursor-pointer"
                                            >
                                                <input
                                                    type="checkbox"
                                                    on:click=move |ev: web_sys::MouseEvent| {
                                                        ev.stop_propagation();
                                                        handle_row_select_click(
                                                            rid,
                                                            r_idx,
                                                            ev.shift_key(),
                                                            ev.ctrl_key() || ev.meta_key(),
                                                        );
                                                    }
                                                    class="h-3.5 w-3.5 rounded accent-[#06B6D4] cursor-pointer"
                                                    prop:checked=move || selected_row_ids.get().contains(&rid)
                                                />
                                            </td>

                                            // Hover-Revealed `+` Row Insert Button + Six-Dot Drag Handle (`⋮⋮`) + Row Index Number (64px)
                                            <td
                                                on:click=move |ev: web_sys::MouseEvent| {
                                                    ev.stop_propagation();
                                                    if ev.shift_key() || ev.ctrl_key() || ev.meta_key() {
                                                        handle_row_select_click(
                                                            rid,
                                                            r_idx,
                                                            ev.shift_key(),
                                                            ev.ctrl_key() || ev.meta_key(),
                                                        );
                                                    }
                                                }
                                                style="width: 64px;"
                                                draggable=move || if can_edit { "true" } else { "false" }
                                                on:dragstart=move |_| {
                                                    if can_edit {
                                                        dragging_row_id.set(Some(rid));
                                                    }
                                                }
                                                on:dragend=move |_| {
                                                    dragging_row_id.set(None);
                                                    drag_over_row_id.set(None);
                                                }
                                                title=if can_edit { "Drag grip (⋮⋮) to reorder row, Shift/Ctrl+Click row # to select, or click (+) to insert row above/below" } else { "Shift+Click or Ctrl+Click row # to select rows" }
                                                class="px-1 py-1 border-r border-[#2A2F37] text-center font-mono text-[11px] text-slate-500 select-none group/cell relative cursor-pointer"
                                            >
                                                <div class="flex items-center justify-center gap-0.5">
                                                    <Show when=move || can_edit>
                                                        <button
                                                            type="button"
                                                            title="Insert row above or below (Shift+Click or Alt+Click to insert above immediately)"
                                                            on:click=move |ev: web_sys::MouseEvent| {
                                                                ev.stop_propagation();
                                                                if ev.shift_key() || ev.alt_key() {
                                                                    handle_insert_row_relative(rid, row_pos, "above");
                                                                    return;
                                                                }
                                                                let is_already_open = open_row_insert_menu
                                                                    .get_untracked()
                                                                    .map(|(id, _)| id == rid)
                                                                    .unwrap_or(false);
                                                                if is_already_open {
                                                                    open_row_insert_menu.set(None);
                                                                } else {
                                                                    row_insert_menu_style.set(
                                                                        compute_floating_popover_style(&ev, 180.0, 96.0),
                                                                    );
                                                                    open_row_insert_menu.set(Some((rid, row_pos)));
                                                                }
                                                            }
                                                            class="opacity-0 group-hover:opacity-100 focus:opacity-100 transition-opacity duration-150 w-4 h-4 rounded flex items-center justify-center text-slate-400 hover:text-cyan-400 hover:bg-slate-700/70 cursor-pointer font-bold text-xs"
                                                        >
                                                            "+"
                                                        </button>
                                                        <span class="text-slate-600 hover:text-[#06B6D4] cursor-grab active:cursor-grabbing text-[10px]">
                                                            "⋮⋮"
                                                        </span>
                                                    </Show>
                                                    <span
                                                        on:click=move |ev: web_sys::MouseEvent| {
                                                            ev.stop_propagation();
                                                            handle_row_select_click(
                                                                rid,
                                                                r_idx,
                                                                ev.shift_key(),
                                                                ev.ctrl_key() || ev.meta_key(),
                                                            );
                                                        }
                                                        class="hover:text-cyan-400 transition-colors"
                                                    >
                                                        {r_idx + 1}
                                                    </span>
                                                    {move || {
                                                        let cnt = row_comment_counts
                                                            .get()
                                                            .get(&rid)
                                                            .copied()
                                                            .unwrap_or(0);
                                                        if cnt > 0 {
                                                            view! {
                                                                <button
                                                                    type="button"
                                                                    title="Open Row Comments"
                                                                    on:click=move |ev| {
                                                                        ev.stop_propagation();
                                                                        open_row_comments_drawer(rid);
                                                                    }
                                                                    class="ml-0.5 px-1 py-0.2 rounded bg-cyan-500/20 border border-cyan-500/40 text-cyan-300 text-[9px] font-bold flex items-center gap-0.5 hover:bg-cyan-500/30 transition"
                                                                >
                                                                    <span>"💬"</span>
                                                                    <span>{cnt}</span>
                                                                </button>
                                                            }
                                                            .into_any()
                                                        } else {
                                                            view! {
                                                                <button
                                                                    type="button"
                                                                    title="Add or view Row Comments"
                                                                    on:click=move |ev| {
                                                                        ev.stop_propagation();
                                                                        open_row_comments_drawer(rid);
                                                                    }
                                                                    class="opacity-0 group-hover:opacity-100 transition-opacity duration-150 w-4 h-4 rounded flex items-center justify-center text-slate-400 hover:text-cyan-400 hover:bg-slate-700/70 text-[10px]"
                                                                >
                                                                    "💬"
                                                                </button>
                                                            }
                                                            .into_any()
                                                        }
                                                    }}
                                                </div>
                                            </td>

                                            // Dynamic Typed Cells (All 12 Property Types + Strictly Isolated Width + Active Cell + Excel Fill Handle)
                                            {cols_for_row
                                                .into_iter()
                                                .filter(|c| !c.is_hidden)
                                                .enumerate()
                                                .map(|(c_idx, col)| {
                                                    let cid = col.id;
                                                    let row_clone = row.clone();
                                                    let cell_tokens = extract_searchable_typed_cell_text(
                                                        &col,
                                                        row.cells.get(&cid),
                                                        &space_members.get_untracked(),
                                                        row.last_modified,
                                                    );
                                                    let active_tags_now = filter_selected_tags.get_untracked();
                                                    let active_fval_now = server_filter_val.get_untracked().trim().to_lowercase();
                                                    let active_fcol_now = server_filter_col.get_untracked();
                                                    let col_in_filter_scope = active_fcol_now.map(|fc| fc == cid).unwrap_or(true);
                                                    let cell_matches_filter = (!filter_q_for_row.is_empty()
                                                        && cell_tokens
                                                            .iter()
                                                            .any(|t| t.to_lowercase().contains(&filter_q_for_row)))
                                                        || (col_in_filter_scope
                                                            && !active_fval_now.is_empty()
                                                            && cell_tokens
                                                                .iter()
                                                                .any(|t| t.to_lowercase().contains(&active_fval_now)))
                                                        || (col_in_filter_scope
                                                            && !active_tags_now.is_empty()
                                                            && active_tags_now.iter().any(|tag| {
                                                                let tl = tag.to_lowercase();
                                                                cell_tokens.iter().any(|t| t.to_lowercase() == tl)
                                                            }));

                                                    view! {
                                                        <td
                                                            style=move || {
                                                                if active_cell.get()
                                                                    .map(|(_, _, ar, ac)| ar == rid && ac == cid)
                                                                    .unwrap_or(false)
                                                                {
                                                                    "outline: 2px solid #06B6D4; outline-offset: -2px;"
                                                                } else {
                                                                    ""
                                                                }
                                                            }
                                                            on:click=move |ev| {
                                                                ev.stop_propagation();
                                                                open_col_menu_id.set(None);
                                                                open_row_insert_menu.set(None);
                                                                open_calc_menu_col_id.set(None);
                                                                show_export_menu.set(false);
                                                                show_end_add_col_popover.set(false);
                                                                show_sort_popover.set(false);
                                                                show_filter_popover.set(false);
                                                                show_group_by_popover.set(false);
                                                                active_cell.set(Some((r_idx, c_idx, rid, cid)));
                                                            }
                                                            on:mouseenter=move |_| {
                                                                if fill_drag_start.get_untracked().is_some() {
                                                                    fill_drag_current.set(Some((r_idx, c_idx)));
                                                                }
                                                            }
                                                            class=move || {
                                                                let col_sel = selected_column_id.get() == Some(cid);
                                                                let in_fill = is_in_fill_drag_range(r_idx, c_idx);
                                                                let highlight_cls = if in_fill {
                                                                    "bg-cyan-500/10 border border-dashed border-cyan-400"
                                                                } else if cell_matches_filter {
                                                                    "bg-[#F97316]/15 ring-1 ring-inset ring-[#F97316]/60"
                                                                } else if col_sel {
                                                                    "bg-[#06B6D4]/10"
                                                                } else {
                                                                    ""
                                                                };
                                                                format!("relative p-0 border-r border-[#2A2F37] align-middle overflow-visible {highlight_cls}")
                                                            }
                                                        >
                                                            <CellEditor
                                                                grid_id=gid
                                                                row=row_clone
                                                                column=col
                                                                can_edit=can_edit
                                                                space_members=space_members
                                                                revealed_passwords=revealed_passwords
                                                                on_before_column_edit=Callback::new(move |old_col: GridColumn| {
                                                                    let prev_cells: Vec<(Uuid, Option<CellValue>)> = rows
                                                                        .get_untracked()
                                                                        .into_iter()
                                                                        .map(|r| (r.id, r.cells.get(&old_col.id).cloned()))
                                                                        .collect();
                                                                    push_undo_action(UndoAction::UpdateColumnMeta {
                                                                        col_id: old_col.id,
                                                                        old_name: old_col.name,
                                                                        old_config: old_col.property_config,
                                                                        old_width: old_col.width_px,
                                                                        old_cells: prev_cells,
                                                                    });
                                                                })
                                                                on_row_updated=Callback::new(move |updated_row: GridRow| {
                                                                    rows.update(|list| {
                                                                        if let Some(existing) = list.iter_mut().find(|r| r.id == updated_row.id) {
                                                                            *existing = updated_row;
                                                                        }
                                                                    });
                                                                })
                                                                on_column_updated=Callback::new(move |updated_col: GridColumn| {
                                                                    columns.update(|list| {
                                                                        if let Some(existing) = list.iter_mut().find(|c| c.id == updated_col.id) {
                                                                            *existing = updated_col;
                                                                        }
                                                                    });
                                                                    reload_rows();
                                                                })
                                                                on_before_cell_edit=Callback::new(move |(r_id, c_id, prev_val, new_val): (Uuid, Uuid, Option<CellValue>, CellValue)| {
                                                                    push_undo_action(UndoAction::CellEdit {
                                                                        row_id: r_id,
                                                                        col_id: c_id,
                                                                        previous_value: prev_val,
                                                                        new_value: new_val,
                                                                    });
                                                                })
                                                                on_error=Callback::new(move |err: String| {
                                                                    error_banner.set(Some(err));
                                                                })
                                                            />

                                                            // Excel Fill Handle (`6x6px` solid cyan square at bottom-right of active cell)
                                                            <Show when=move || {
                                                                can_edit
                                                                    && active_cell
                                                                        .get()
                                                                        .map(|(_, _, ar, ac)| ar == rid && ac == cid)
                                                                        .unwrap_or(false)
                                                            }>
                                                                <div
                                                                    title="Drag Fill Handle vertically or horizontally to copy cell value"
                                                                    on:mousedown=move |ev: web_sys::MouseEvent| {
                                                                        ev.stop_propagation();
                                                                        ev.prevent_default();
                                                                        fill_drag_start.set(Some((r_idx, c_idx, rid, cid)));
                                                                        fill_drag_current.set(Some((r_idx, c_idx)));
                                                                    }
                                                                    on:click=move |ev| ev.stop_propagation()
                                                                    class="absolute -bottom-[3px] -right-[3px] z-30 h-[6px] w-[6px] bg-[#06B6D4] ring-1 ring-[#14171A] cursor-crosshair"
                                                                />
                                                            </Show>
                                                        </td>
                                                    }
                                                })
                                                .collect_view()}

                                            // Matching Empty Trailing `<td>` for the End-of-Table `+` Column (50px)
                                            <td
                                                style="width: 50px;"
                                                class="border-r border-[#2A2F37] bg-[#14171A]/30 group/cell relative "
                                            />
                                        </tr>
                                    }
                                    .into_any()
                                };

                                // Check whether Group-By is active
                                let active_group_col = group_by_col_id
                                    .get()
                                    .and_then(|gc_id| cols.iter().find(|c| c.id == gc_id).cloned());

                                if let Some(group_col) = active_group_col {
                                    let person_lookup: HashMap<Uuid, String> = space_members
                                        .get()
                                        .into_iter()
                                        .map(|m| (m.user_id, m.display_name))
                                        .collect();
                                    let groups = partition_rows_by_group(
                                        &filtered_rows,
                                        &group_col,
                                        &person_lookup,
                                    );
                                    let total_colspan = 2 + visible_cols.len() + 1;
                                    let active_calcs = column_calculations.get();

                                    groups
                                        .into_iter()
                                        .map(|grp| {
                                            let grp_key = grp.key.clone();
                                            let grp_key_toggle = grp_key.clone();
                                            let grp_label = grp.label.clone();
                                            let grp_label_sub = grp.label.clone();
                                            let grp_label_add = grp.label.clone();
                                            let grp_opt_id = grp.option_id;
                                            let grp_col_for_add = group_col.clone();
                                            let grp_rows = grp.rows;
                                            let grp_row_count = grp_rows.len();
                                            let is_collapsed =
                                                collapsed_groups.get().contains(&grp_key);

                                            let badge_cls = match grp.color {
                                                Some(TagColor::Orange) | Some(TagColor::Amber) => "bg-orange-500/20 text-orange-300 border-orange-500/40",
                                                Some(TagColor::Cyan) | Some(TagColor::Teal) | Some(TagColor::Blue) => "bg-cyan-500/20 text-cyan-300 border-cyan-500/40",
                                                Some(TagColor::Emerald) | Some(TagColor::Green) => "bg-emerald-500/20 text-emerald-300 border-emerald-500/40",
                                                Some(TagColor::Purple) | Some(TagColor::Violet) | Some(TagColor::Indigo) => "bg-purple-500/20 text-purple-300 border-purple-500/40",
                                                Some(TagColor::Pink) | Some(TagColor::Rose) | Some(TagColor::Red) => "bg-pink-500/20 text-pink-300 border-pink-500/40",
                                                _ => "bg-slate-700/60 text-slate-200 border-slate-600",
                                            };

                                            let body_rows_view: Vec<AnyView> = if is_collapsed {
                                                Vec::new()
                                            } else {
                                                grp_rows
                                                    .iter()
                                                    .enumerate()
                                                    .map(|(idx, r)| {
                                                        render_data_row(
                                                            idx,
                                                            r.clone(),
                                                            cols.clone(),
                                                            filter_q.clone(),
                                                        )
                                                    })
                                                    .collect()
                                            };

                                            let subtotal_cells_view = visible_cols
                                                .iter()
                                                .enumerate()
                                                .map(|(v_idx, col)| {
                                                    let mode = active_calcs
                                                        .get(&col.id)
                                                        .copied()
                                                        .unwrap_or_else(|| {
                                                            if col.property_type
                                                                == ColumnPropertyType::Number
                                                                || column_has_numeric_or_currency_values(
                                                                    col,
                                                                    &grp_rows,
                                                                )
                                                            {
                                                                CalcMode::Sum
                                                            } else {
                                                                CalcMode::None
                                                            }
                                                        });
                                                    let summary = compute_column_calculation(
                                                        col,
                                                        &grp_rows,
                                                        mode,
                                                    );
                                                    let is_first_col = v_idx == 0;
                                                    let sub_title = grp_label_sub.clone();

                                                    view! {
                                                        <td class="px-2.5 py-2 border-r border-slate-700/80 align-middle text-xs font-mono group/cell relative ">
                                                            {if is_first_col && mode == CalcMode::None {
                                                                view! {
                                                                    <span class="text-slate-300 font-semibold">
                                                                        {format!("✓ Subtotal — {sub_title}")}
                                                                    </span>
                                                                }
                                                                .into_any()
                                                            } else if is_first_col {
                                                                view! {
                                                                    <div class="flex flex-col gap-0.5">
                                                                        <span class="text-[10px] text-slate-400 font-sans font-semibold">
                                                                            {format!("✓ Subtotal — {sub_title}")}
                                                                        </span>
                                                                        {render_calc_summary_badges(&summary, false)}
                                                                    </div>
                                                                }
                                                                .into_any()
                                                            } else {
                                                                render_calc_summary_badges(&summary, false)
                                                            }}
                                                        </td>
                                                    }
                                                })
                                                .collect_view();

                                            view! {
                                                // 1. Collapsible Group Section Header Row
                                                <tr class="bg-slate-800/80 border-y border-slate-700/80">
                                                    <td
                                                        colspan=total_colspan
                                                        class="px-3 py-2 text-left group/cell relative "
                                                    >
                                                        <div class="flex items-center justify-between gap-3">
                                                            <div class="flex items-center gap-2.5">
                                                                <button
                                                                    type="button"
                                                                    on:click=move |ev| {
                                                                        ev.stop_propagation();
                                                                        let k = grp_key_toggle.clone();
                                                                        collapsed_groups.update(|set| {
                                                                            if set.contains(&k) {
                                                                                set.remove(&k);
                                                                            } else {
                                                                                set.insert(k);
                                                                            }
                                                                        });
                                                                    }
                                                                    class="w-5 h-5 rounded bg-slate-900/80 hover:bg-slate-700 text-cyan-400 flex items-center justify-center text-[10px] font-bold transition"
                                                                >
                                                                    {if is_collapsed { "▶" } else { "▼" }}
                                                                </button>
                                                                <span class=format!("px-2.5 py-0.5 rounded-md border text-xs font-bold {badge_cls}")>
                                                                    {grp_label}
                                                                </span>
                                                                <span class="px-2 py-0.5 rounded-full bg-slate-900/90 border border-slate-700 text-[11px] font-mono text-slate-300">
                                                                    {format!("{grp_row_count} items")}
                                                                </span>
                                                            </div>

                                                            <Show when=move || can_edit>
                                                                {
                                                                    let g_col = grp_col_for_add.clone();
                                                                    let g_lbl = grp_label_add.clone();
                                                                    let g_lbl_btn = grp_label_add.clone();
                                                                    view! {
                                                                        <button
                                                                            type="button"
                                                                            on:click=move |ev| {
                                                                                ev.stop_propagation();
                                                                                handle_add_row_to_group(
                                                                                    g_col.clone(),
                                                                                    grp_opt_id,
                                                                                    g_lbl.clone(),
                                                                                );
                                                                            }
                                                                            class="px-2.5 py-1 rounded-lg bg-slate-900/80 hover:bg-cyan-500/20 border border-slate-700 hover:border-cyan-500/50 text-[11px] font-semibold text-slate-300 hover:text-cyan-300 transition flex items-center gap-1"
                                                                        >
                                                                            <span class="text-cyan-400 font-bold">"+"</span>
                                                                            <span>{format!("Add row to {g_lbl_btn}")}</span>
                                                                        </button>
                                                                    }
                                                                }
                                                            </Show>
                                                        </div>
                                                    </td>
                                                </tr>

                                                // 2. Group Body Rows (hidden when collapsed)
                                                {body_rows_view}

                                                // 3. Automatic Group Subtotal Row (`✓ Subtotal — {Group Name}`)
                                                <tr class="bg-slate-900/90 border-t-2 border-slate-700 font-bold">
                                                    <td
                                                        style="width: 40px;"
                                                        class="sticky left-0 z-10 bg-slate-900 px-2 py-2 border-r border-slate-700 text-center text-emerald-400 text-xs group/cell relative "
                                                    >
                                                        "✓"
                                                    </td>
                                                    <td
                                                        style="width: 64px;"
                                                        class="px-1 py-2 border-r border-slate-700 text-center font-mono text-[10px] text-slate-400 group/cell relative "
                                                    >
                                                        "SUB"
                                                    </td>
                                                    {subtotal_cells_view}
                                                    <td
                                                        style="width: 50px;"
                                                        class="border-r border-slate-700 bg-slate-900/50 group/cell relative "
                                                    />
                                                </tr>
                                            }
                                            .into_any()
                                        })
                                        .collect_view()
                                        .into_any()
                                } else {
                                    filtered_rows
                                        .into_iter()
                                        .enumerate()
                                        .map(|(r_idx, row)| {
                                            render_data_row(
                                                r_idx,
                                                row,
                                                cols.clone(),
                                                filter_q.clone(),
                                            )
                                        })
                                        .collect_view()
                                        .into_any()
                                }
                            }}
                        </tbody>

                        // ================================================================
                        // Interactive Column Calculation Footer (`SUM`, `AVG`, `COUNT` + Dual-Currency `EGP` & `USD` Engine)
                        // ================================================================
                        <tfoot class="sticky bottom-0 z-20 bg-slate-900/95 backdrop-blur border-t-2 border-slate-700 text-xs font-mono">
                            <tr>
                                <td
                                    style="width: 40px;"
                                    class="sticky left-0 z-20 bg-slate-900 px-2 py-2 border-r border-slate-700 text-center text-cyan-400 font-bold group/cell relative "
                                    title="Column Calculations & Dual-Currency Grand Totals"
                                >
                                    "Σ"
                                </td>
                                <td
                                    style="width: 64px;"
                                    class="px-1 py-2 border-r border-slate-700 text-center text-[10px] text-slate-400 font-semibold select-none group/cell relative "
                                >
                                    "TOTAL"
                                </td>
                                {move || {
                                    let filter_q = quick_search.get();
                                    let all_cols = columns.get();
                                    let members = space_members.get();
                                    let f_col = server_filter_col.get();
                                    let f_val = server_filter_val.get();
                                    let f_tags = filter_selected_tags.get();
                                    let visible_rows: Vec<GridRow> = rows
                                        .get()
                                        .into_iter()
                                        .filter(|row| {
                                            row_matches_universal_filters(
                                                row,
                                                &all_cols,
                                                &members,
                                                &filter_q,
                                                f_col,
                                                &f_val,
                                                &f_tags,
                                            )
                                        })
                                        .collect();

                                    let calcs_map = column_calculations.get();

                                    columns
                                        .get()
                                        .into_iter()
                                        .filter(|c| !c.is_hidden)
                                        .map(|col| {
                                            let cid = col.id;
                                            let mode = calcs_map
                                                .get(&cid)
                                                .copied()
                                                .unwrap_or(CalcMode::None);
                                            let summary = compute_column_calculation(
                                                &col,
                                                &visible_rows,
                                                mode,
                                            );

                                            view! {
                                                <td
                                                    on:click=move |ev: web_sys::MouseEvent| {
                                                        ev.stop_propagation();
                                                        let already_open =
                                                            open_calc_menu_col_id.get_untracked() == Some(cid);
                                                        if already_open {
                                                            open_calc_menu_col_id.set(None);
                                                        } else {
                                                            calc_menu_popover_style.set(
                                                                compute_floating_popover_style(&ev, 210.0, 320.0),
                                                            );
                                                            open_calc_menu_col_id.set(Some(cid));
                                                        }
                                                    }
                                                    title="Click to choose column calculation (Sum, Average, Count, Min, Max)"
                                                    class="group/calc px-2.5 py-2 border-r border-slate-700/80 hover:bg-slate-800/80 cursor-pointer transition-colors align-middle select-none group/cell relative "
                                                >
                                                    {if mode == CalcMode::None {
                                                        view! {
                                                            <div class="flex items-center justify-between text-[11px] text-slate-500 group-hover/calc:text-slate-300 transition-colors">
                                                                <span class="opacity-40 group-hover/calc:opacity-100">"Calculate"</span>
                                                                <span class="opacity-40 group-hover/calc:opacity-100">"▾"</span>
                                                            </div>
                                                        }
                                                        .into_any()
                                                    } else {
                                                        view! {
                                                            <div class="flex items-center justify-between gap-1.5">
                                                                {render_calc_summary_badges(&summary, true)}
                                                                <span class="text-[10px] text-slate-500 opacity-0 group-hover/calc:opacity-100 transition-opacity shrink-0">
                                                                    "▾"
                                                                </span>
                                                            </div>
                                                        }
                                                        .into_any()
                                                    }}
                                                </td>
                                            }
                                        })
                                        .collect_view()
                                }}
                                <td
                                    style="width: 50px;"
                                    class="border-r border-slate-700 bg-slate-900/80 group/cell relative "
                                />
                            </tr>
                        </tfoot>
                    </table>

                    // Inline Under-Table `+ New Row` Bar (positioned directly underneath `<table>` in natural document flow)
                    <Show when=move || auth.can_edit_space(space_id.get())>
                        <div
                            style=move || format!("width: {}px;", total_table_width())
                            on:click=move |ev| {
                                ev.stop_propagation();
                                handle_add_row(ev);
                            }
                            class="py-2.5 px-4 border-t border-b border-l border-r border-slate-800 bg-slate-900/40 hover:bg-slate-800/60 text-slate-400 hover:text-orange-400 flex items-center gap-2 cursor-pointer transition-colors text-xs font-medium select-none"
                        >
                            <span class="font-bold text-sm text-[#F97316]">"+"</span>
                            <span>"New Row"</span>
                        </div>
                    </Show>
                </div>
            </div>

            // ================================================================
            // 2.4 Viewport-Aware Floating Column Calculation (`CalcMode`) Menu
            // ================================================================
            <Show when=move || open_calc_menu_col_id.get().is_some()>
                {move || {
                    let Some(target_cid) = open_calc_menu_col_id.get() else {
                        return view! { <div /> }.into_any();
                    };
                    let current_mode = column_calculations
                        .get()
                        .get(&target_cid)
                        .copied()
                        .unwrap_or(CalcMode::None);

                    view! {
                        <div
                            style=move || calc_menu_popover_style.get()
                            on:click=move |ev| ev.stop_propagation()
                            class="fixed z-[9999] w-52 rounded-xl bg-slate-900 border border-slate-700 shadow-2xl py-1.5 text-xs"
                        >
                            <div class="px-3 py-1.5 border-b border-slate-800 text-[10px] font-bold uppercase tracking-wider text-slate-400 flex items-center justify-between">
                                <span>"Column Calculation"</span>
                                <span class="text-cyan-400 font-mono">"EGP / USD"</span>
                            </div>
                            {ALL_CALC_MODES
                                .into_iter()
                                .map(|mode_opt| {
                                    let is_active = current_mode == mode_opt;
                                    view! {
                                        <button
                                            type="button"
                                            on:click=move |ev| {
                                                ev.stop_propagation();
                                                column_calculations.update(|map| {
                                                    map.insert(target_cid, mode_opt);
                                                });
                                                open_calc_menu_col_id.set(None);
                                            }
                                            class=if is_active {
                                                "w-full px-3 py-1.5 bg-cyan-500/15 text-cyan-300 font-semibold flex items-center justify-between text-left transition-colors"
                                            } else {
                                                "w-full px-3 py-1.5 hover:bg-slate-800 text-slate-200 flex items-center justify-between text-left transition-colors"
                                            }
                                        >
                                            <span>{mode_opt.label()}</span>
                                            <span class="font-mono text-[10px] text-slate-400">
                                                {if is_active { "✓" } else { mode_opt.short_prefix() }}
                                            </span>
                                        </button>
                                    }
                                })
                                .collect_view()}
                        </div>
                    }
                    .into_any()
                }}
            </Show>

            // ================================================================
            // 2.5 Notion-Style Hover `+` Row Insert Above / Below Mini-Menu
            // ================================================================
            <Show when=move || open_row_insert_menu.get().is_some()>
                {move || {
                    let Some((target_rid, target_pos)) = open_row_insert_menu.get() else {
                        return view! { <div /> }.into_any();
                    };
                    view! {
                        <div
                            style=move || row_insert_menu_style.get()
                            on:click=move |ev| ev.stop_propagation()
                            class="fixed z-[9999] bg-slate-900 border border-slate-700 rounded-lg shadow-xl py-1 w-44 text-xs"
                        >
                            <button
                                type="button"
                                on:click=move |ev| {
                                    ev.stop_propagation();
                                    handle_insert_row_relative(target_rid, target_pos, "above");
                                }
                                class="w-full px-3 py-1.5 hover:bg-slate-800 text-slate-200 hover:text-cyan-400 flex items-center gap-2 text-left font-medium transition-colors"
                            >
                                <span class="text-cyan-400 font-bold">"↑"</span>
                                <span>"Insert Row Above"</span>
                            </button>
                            <button
                                type="button"
                                on:click=move |ev| {
                                    ev.stop_propagation();
                                    handle_insert_row_relative(target_rid, target_pos, "below");
                                }
                                class="w-full px-3 py-1.5 hover:bg-slate-800 text-slate-200 hover:text-orange-400 flex items-center gap-2 text-left font-medium transition-colors"
                            >
                                <span class="text-orange-400 font-bold">"↓"</span>
                                <span>"Insert Row Below"</span>
                            </button>
                        </div>
                    }
                    .into_any()
                }}
            </Show>

            // ================================================================
            // 2.8 Row Comments Slide-Over Drawer (`GET/POST /api/grids/{grid_id}/rows/{row_id}/comments`)
            // ================================================================
            <Show when=move || open_comments_row_id.get().is_some()>
                {move || {
                    let Some(active_rid) = open_comments_row_id.get() else {
                        return view! { <div /> }.into_any();
                    };

                    // Extract the row's primary column label for context
                    let row_title = rows
                        .get()
                        .into_iter()
                        .find(|r| r.id == active_rid)
                        .and_then(|r| {
                            let pri_col = columns
                                .get()
                                .into_iter()
                                .find(|c| c.is_primary)
                                .or_else(|| columns.get().into_iter().next())?;
                            let txt = cell_to_plain_text(
                                r.cells.get(&pri_col.id),
                                &pri_col,
                                &space_members.get(),
                                r.last_modified,
                            );
                            if txt.trim().is_empty() {
                                None
                            } else {
                                Some(txt)
                            }
                        })
                        .unwrap_or_else(|| format!("Row {}", &active_rid.to_string()[..8]));

                    view! {
                        <div
                            on:click=move |ev| ev.stop_propagation()
                            class="fixed inset-y-0 right-0 z-40 w-96 max-w-full bg-[#1E2227] border-l border-[#2A2F37] shadow-2xl flex flex-col"
                        >
                            <div class="px-5 py-4 border-b border-[#2A2F37] flex items-center justify-between">
                                <div class="min-w-0 pr-2">
                                    <h2 class="text-sm font-bold text-white flex items-center gap-2">
                                        <span class="text-[#06B6D4]">"💬"</span>
                                        <span>"Row Discussion & Comments"</span>
                                    </h2>
                                    <p class="text-[11px] text-[#06B6D4] font-semibold truncate mt-0.5">
                                        {row_title}
                                    </p>
                                </div>
                                <button
                                    type="button"
                                    on:click=move |_| open_comments_row_id.set(None)
                                    class="px-2 py-1 rounded-lg bg-[#14171A] text-slate-400 hover:text-white text-xs shrink-0"
                                >
                                    "✕"
                                </button>
                            </div>

                            <div class="flex-1 overflow-y-auto p-4 space-y-3">
                                <Show
                                    when=move || !is_loading_comments.get()
                                    fallback=move || {
                                        view! {
                                            <p class="text-xs text-slate-400 text-center py-8">
                                                "Loading row comments..."
                                            </p>
                                        }
                                    }
                                >
                                    {move || {
                                        let list = row_comments_list.get();
                                        let current_uid = auth.user.get().map(|u| u.id);
                                        let can_manage = auth.can_edit_space(space_id.get());
                                        if list.is_empty() {
                                            view! {
                                                <div class="p-6 rounded-2xl bg-[#14171A] border border-[#2A2F37] text-center space-y-1">
                                                    <p class="text-xs font-semibold text-slate-300">
                                                        "No comments on this row yet"
                                                    </p>
                                                    <p class="text-[11px] text-slate-500">
                                                        "Start a discussion thread or leave review notes for your team below."
                                                    </p>
                                                </div>
                                            }
                                            .into_any()
                                        } else {
                                            view! {
                                                <div class="space-y-2.5">
                                                    {list
                                                        .into_iter()
                                                        .map(|cm| {
                                                            let cid = cm.id;
                                                            let author = cm.author_email.clone();
                                                            let body = cm.content.clone();
                                                            let ts = cm
                                                                .created_at
                                                                .format("%Y-%m-%d %H:%M")
                                                                .to_string();
                                                            let can_del = can_manage
                                                                || (current_uid.is_some()
                                                                    && cm.author_id == current_uid);

                                                            view! {
                                                                <div class="p-3 rounded-xl bg-[#14171A] border border-[#2A2F37] space-y-1.5">
                                                                    <div class="flex items-center justify-between gap-2">
                                                                        <span class="text-[11px] font-semibold text-[#06B6D4] truncate">
                                                                            {author}
                                                                        </span>
                                                                        <div class="flex items-center gap-2 shrink-0">
                                                                            <span class="text-[10px] font-mono text-slate-500">
                                                                                {ts}
                                                                            </span>
                                                                            <Show when=move || can_del>
                                                                                <button
                                                                                    type="button"
                                                                                    title="Delete comment"
                                                                                    on:click=move |_| {
                                                                                        handle_delete_row_comment(active_rid, cid);
                                                                                    }
                                                                                    class="text-[10px] text-slate-500 hover:text-red-400"
                                                                                >
                                                                                    "🗑️"
                                                                                </button>
                                                                            </Show>
                                                                        </div>
                                                                    </div>
                                                                    <p class="text-xs text-slate-200 whitespace-pre-wrap break-words leading-relaxed">
                                                                        {body}
                                                                    </p>
                                                                </div>
                                                            }
                                                        })
                                                        .collect_view()}
                                                </div>
                                            }
                                            .into_any()
                                        }
                                    }}
                                </Show>
                            </div>

                            <form
                                on:submit=handle_post_row_comment
                                class="p-4 border-t border-[#2A2F37] bg-[#14171A]/90 space-y-2"
                            >
                                <textarea
                                    rows="2"
                                    required
                                    placeholder="Write a comment or note on this row..."
                                    class="w-full px-3 py-2 rounded-xl bg-[#1E2227] border border-[#2A2F37] text-xs text-white focus:outline-none focus:border-[#06B6D4] resize-none"
                                    prop:value=new_comment_text
                                    on:input=move |ev| new_comment_text.set(event_target_value(&ev))
                                />
                                <div class="flex justify-end">
                                    <button
                                        type="submit"
                                        class="px-4 py-1.5 rounded-lg bg-[#06B6D4] hover:bg-[#0891B2] text-slate-950 font-bold text-xs shadow transition"
                                    >
                                        "Post Comment"
                                    </button>
                                </div>
                            </form>
                        </div>
                    }
                    .into_any()
                }}
            </Show>

            // ================================================================
            // 3. 30-Day Grid History Slide-Over Drawer (`GET /api/grids/{grid_id}/history`)
            // ================================================================
            <Show when=move || show_history_drawer.get()>
                <div
                    on:click=move |ev| ev.stop_propagation()
                    class="fixed inset-y-0 right-0 z-40 w-96 max-w-full bg-[#1E2227] border-l border-[#2A2F37] shadow-2xl flex flex-col"
                >
                    <div class="px-5 py-4 border-b border-[#2A2F37] flex items-center justify-between">
                        <div>
                            <h2 class="text-sm font-bold text-white flex items-center gap-2">
                                <span class="text-[#F97316]">"🕒"</span>
                                <span>"30-Day Version History"</span>
                            </h2>
                            <p class="text-[11px] text-slate-400 mt-0.5">
                                "Click any snapshot to inspect its full table preview before restoring"
                            </p>
                        </div>
                        <button
                            type="button"
                            on:click=move |_| show_history_drawer.set(false)
                            class="px-2 py-1 rounded-lg bg-[#14171A] text-slate-400 hover:text-white text-xs"
                        >
                            "✕"
                        </button>
                    </div>

                    <div class="flex-1 overflow-y-auto p-4 space-y-2.5">
                        <Show
                            when=move || !is_loading_history.get()
                            fallback=move || {
                                view! {
                                    <p class="text-xs text-slate-400 text-center py-8">
                                        "Loading 30-day snapshots..."
                                    </p>
                                }
                            }
                        >
                            {move || {
                                let list = history_entries.get();
                                if list.is_empty() {
                                    view! {
                                        <div class="p-6 rounded-2xl bg-[#14171A] border border-[#2A2F37] text-center space-y-1">
                                            <p class="text-xs font-semibold text-slate-300">
                                                "No history snapshots recorded yet"
                                            </p>
                                            <p class="text-[11px] text-slate-500">
                                                "Snapshots are captured automatically whenever rows or columns are modified, imported, or deleted."
                                            </p>
                                        </div>
                                    }
                                    .into_any()
                                } else {
                                    let can_restore = auth.can_edit_space(space_id.get());
                                    view! {
                                        <div class="space-y-2.5">
                                            {list
                                                .into_iter()
                                                .map(|entry| {
                                                    let hid = entry.id;
                                                    let action_label = entry.action_type.clone();
                                                    let summary_text = entry.summary.clone();
                                                    let actor = entry
                                                        .actor_email
                                                        .unwrap_or_else(|| "System".into());
                                                    let ts = entry
                                                        .created_at
                                                        .format("%Y-%m-%d %H:%M:%S UTC")
                                                        .to_string();

                                                    view! {
                                                        <div
                                                            on:click=move |_| open_history_preview(hid)
                                                            class="p-3.5 rounded-xl bg-[#14171A] border border-[#2A2F37] hover:border-[#06B6D4] cursor-pointer transition space-y-2"
                                                        >
                                                            <div class="flex items-start justify-between gap-2">
                                                                <span class="px-2 py-0.5 rounded-md bg-[#06B6D4]/15 text-[#06B6D4] font-mono text-[11px] font-semibold">
                                                                    {action_label}
                                                                </span>
                                                                <span class="text-[10px] font-mono text-slate-400">
                                                                    {ts}
                                                                </span>
                                                            </div>
                                                            <p class="text-xs text-slate-200 truncate">
                                                                {summary_text}
                                                            </p>
                                                            <div class="flex items-center justify-between text-[11px] text-slate-400">
                                                                <span class="truncate">
                                                                    "By: " <strong class="text-slate-200">{actor}</strong>
                                                                </span>
                                                                <span>
                                                                    {format!("{} cols · {} rows", entry.column_count, entry.row_count)}
                                                                </span>
                                                            </div>
                                                            <div class="flex items-center gap-1.5 pt-1">
                                                                <button
                                                                    type="button"
                                                                    on:click=move |ev| {
                                                                        ev.stop_propagation();
                                                                        open_history_preview(hid);
                                                                    }
                                                                    class="flex-1 py-1.5 rounded-lg bg-[#06B6D4]/15 hover:bg-[#06B6D4]/25 text-[#06B6D4] border border-[#06B6D4]/40 text-xs font-semibold transition"
                                                                >
                                                                    "👁 Preview Table"
                                                                </button>
                                                                <Show when=move || can_restore>
                                                                    <button
                                                                        type="button"
                                                                        on:click=move |ev| {
                                                                            ev.stop_propagation();
                                                                            handle_restore_snapshot(hid);
                                                                        }
                                                                        class="flex-1 py-1.5 rounded-lg bg-[#F97316]/15 hover:bg-[#F97316] text-[#F97316] hover:text-white border border-[#F97316]/40 text-xs font-semibold transition"
                                                                    >
                                                                        "↺ Restore"
                                                                    </button>
                                                                </Show>
                                                            </div>
                                                        </div>
                                                    }
                                                })
                                                .collect_view()}
                                        </div>
                                    }
                                    .into_any()
                                }
                            }}
                        </Show>
                    </div>
                </div>
            </Show>

            // ================================================================
            // 3.5 Full-Page History Snapshot Preview Modal (`GET /api/grids/{grid_id}/history/{history_id}`)
            // ================================================================
            <Show when=move || is_loading_preview_snapshot.get() || preview_history_snapshot.get().is_some()>
                <div
                    on:click=move |ev| ev.stop_propagation()
                    class="fixed inset-4 z-50 bg-slate-900/95 border border-slate-700 rounded-xl shadow-2xl flex flex-col overflow-hidden"
                >
                    {move || {
                        if is_loading_preview_snapshot.get() {
                            return view! {
                                <div class="flex-1 flex flex-col items-center justify-center gap-3 p-8 text-center">
                                    <div class="text-2xl text-[#06B6D4] animate-spin">"⟳"</div>
                                    <p class="text-sm font-semibold text-white">
                                        "Loading Historical Grid Snapshot..."
                                    </p>
                                    <button
                                        type="button"
                                        on:click=move |_| is_loading_preview_snapshot.set(false)
                                        class="px-3 py-1.5 rounded-lg bg-slate-800 text-xs text-slate-300 hover:text-white"
                                    >
                                        "Cancel"
                                    </button>
                                </div>
                            }
                            .into_any();
                        }

                        let Some(detail) = preview_history_snapshot.get() else {
                            return view! { <div /> }.into_any();
                        };

                        let hid = detail.id;
                        let snap_gid = detail.grid_id;
                        let ts = detail.created_at.format("%Y-%m-%d %H:%M:%S UTC").to_string();
                        let actor = detail.actor_email.unwrap_or_else(|| "System".into());
                        let action_badge = detail.action_type.clone();
                        let summary_str = detail.summary.clone();
                        let snap_cols: Vec<GridColumn> = detail
                            .snapshot_data
                            .columns
                            .into_iter()
                            .filter(|c| !c.is_hidden)
                            .collect();
                        let snap_rows: Vec<GridRow> = detail.snapshot_data.rows;
                        let snap_table_width: i32 = 48
                            + snap_cols
                                .iter()
                                .map(|c| c.width_px.clamp(80, 800))
                                .sum::<i32>();
                        let can_restore = auth.can_edit_space(space_id.get());

                        view! {
                            <div class="flex flex-col h-full overflow-hidden">
                                // Modal Header: Timestamp, User, Action Type Badge, Restore Button, Close Button
                                <div class="px-6 py-4 border-b border-slate-700 bg-[#14171A] flex flex-wrap items-center justify-between gap-4 shrink-0">
                                    <div class="flex flex-wrap items-center gap-3">
                                        <span class="px-2.5 py-1 rounded-lg bg-[#06B6D4]/15 border border-[#06B6D4]/40 text-[#06B6D4] font-mono text-xs font-bold">
                                            {action_badge}
                                        </span>
                                        <div>
                                            <div class="flex items-center gap-2 text-sm font-bold text-white">
                                                <span>"Snapshot Preview — " {ts}</span>
                                                <span class="px-2 py-0.5 rounded bg-slate-800 text-[11px] font-normal text-slate-300">
                                                    "Read-Only"
                                                </span>
                                            </div>
                                            <div class="text-xs text-slate-400 mt-0.5 flex items-center gap-3">
                                                <span>
                                                    "Modified by: " <strong class="text-slate-200">{actor}</strong>
                                                </span>
                                                <span>"•"</span>
                                                <span>{summary_str}</span>
                                                <span>"•"</span>
                                                <span class="font-mono text-[#06B6D4]">
                                                    {format!("{} columns · {} rows", snap_cols.len(), snap_rows.len())}
                                                </span>
                                            </div>
                                        </div>
                                    </div>

                                    <div class="flex items-center gap-2.5">
                                        <Show when=move || can_restore>
                                            <button
                                                type="button"
                                                on:click=move |_| handle_restore_snapshot(hid)
                                                class="px-4 py-2 rounded-lg bg-[#F97316] hover:bg-[#EA580C] text-white text-xs font-bold shadow-lg flex items-center gap-1.5 transition cursor-pointer"
                                            >
                                                <span>"↺"</span>
                                                <span>"Restore This Version"</span>
                                            </button>
                                        </Show>
                                        <button
                                            type="button"
                                            on:click=move |_| preview_history_snapshot.set(None)
                                            class="px-3.5 py-2 rounded-lg bg-slate-800 hover:bg-slate-700 border border-slate-600 text-slate-200 text-xs font-semibold transition cursor-pointer"
                                        >
                                            "Close Preview (Esc)"
                                        </button>
                                    </div>
                                </div>

                                // Modal Body: Full Scrollable Read-Only Grid Table View
                                <div class="flex-1 overflow-auto bg-[#0F1115]">
                                    <table
                                        style=format!("table-layout: fixed; width: {}px;", snap_table_width)
                                        class="border-collapse text-left"
                                    >
                                        <colgroup>
                                            <col style="width: 48px;" />
                                            {snap_cols
                                                .iter()
                                                .map(|c| {
                                                    let w = c.width_px.clamp(80, 800);
                                                    view! { <col style=format!("width: {}px;", w) /> }
                                                })
                                                .collect_view()}
                                        </colgroup>
                                        <thead class="sticky top-0 z-20 bg-[#111315]">
                                            <tr class="border-b border-[#2A2F37] text-[11px] uppercase tracking-wider text-slate-400">
                                                <th class="px-2 py-2.5 border-r border-[#2A2F37] text-center font-mono text-[10px]">
                                                    "#"
                                                </th>
                                                {snap_cols
                                                    .iter()
                                                    .map(|col| {
                                                        let cname = col.name.clone();
                                                        let ptype = col.property_type;
                                                        view! {
                                                            <th class="px-3 py-2.5 border-r border-[#2A2F37] font-semibold text-slate-200 truncate">
                                                                <div class="flex items-center gap-1.5 truncate">
                                                                    <span class="text-[#06B6D4] font-mono text-[11px]">
                                                                        {property_type_icon(ptype)}
                                                                    </span>
                                                                    <span class="truncate">{cname}</span>
                                                                </div>
                                                            </th>
                                                        }
                                                    })
                                                    .collect_view()}
                                            </tr>
                                        </thead>
                                        <tbody class="divide-y divide-[#2A2F37]/70">
                                            {snap_rows
                                                .into_iter()
                                                .enumerate()
                                                .map(|(r_idx, snap_row)| {
                                                    let cols_for_snap_row = snap_cols.clone();
                                                    view! {
                                                        <tr class="hover:bg-slate-800/30 transition">
                                                            <td class="px-2 py-1 border-r border-[#2A2F37] text-center font-mono text-[11px] text-slate-500 group/cell relative ">
                                                                {r_idx + 1}
                                                            </td>
                                                            {cols_for_snap_row
                                                                .into_iter()
                                                                .map(|col| {
                                                                    let r_clone = snap_row.clone();
                                                                    view! {
                                                                        <td class="p-0 border-r border-[#2A2F37] align-middle overflow-visible group/cell relative">
                                                                            <CellEditor
                                                                                grid_id=snap_gid
                                                                                row=r_clone
                                                                                column=col
                                                                                can_edit=false
                                                                                space_members=space_members
                                                                                on_row_updated=Callback::new(|_| {})
                                                                                on_column_updated=Callback::new(|_| {})
                                                                                on_before_cell_edit=Callback::new(|_| {})
                                                                                on_error=Callback::new(|_| {})
                                                                            />
                                                                        </td>
                                                                    }
                                                                })
                                                                .collect_view()}
                                                        </tr>
                                                    }
                                                })
                                                .collect_view()}
                                        </tbody>
                                    </table>
                                </div>
                            </div>
                        }
                        .into_any()
                    }}
                </div>
            </Show>

            // ================================================================
            // 4. Phase 17: Floating Bulk Selection Action Bar & Toast
            // ================================================================
            {move || {
                copied_rows_toast.get().map(|msg| {
                    view! {
                        <div
                            on:click=move |ev| ev.stop_propagation()
                            class="fixed bottom-20 left-1/2 -translate-x-1/2 z-[60] px-4 py-2 rounded-xl bg-emerald-950/95 border border-emerald-500/50 text-emerald-300 text-xs font-semibold shadow-2xl flex items-center gap-2"
                        >
                            <span>{msg}</span>
                        </div>
                    }
                })
            }}

            <Show when=move || !selected_row_ids.get().is_empty()>
                <div
                    on:click=move |ev| ev.stop_propagation()
                    class="fixed bottom-6 left-1/2 -translate-x-1/2 z-50 bg-slate-900/95 border border-cyan-500/40 rounded-xl shadow-2xl px-4 py-2.5 flex flex-wrap items-center gap-3"
                >
                    <span class="px-2.5 py-1 rounded-lg bg-cyan-500/15 border border-cyan-500/30 text-cyan-300 text-xs font-bold font-mono">
                        {move || format!("{} rows selected", selected_row_ids.get().len())}
                    </span>

                    <button
                        type="button"
                        on:click=move |_| copy_selected_rows_to_clipboard()
                        class="px-3 py-1.5 rounded-lg bg-slate-800 hover:bg-slate-700 border border-slate-700 text-slate-100 text-xs font-semibold flex items-center gap-1.5 transition cursor-pointer"
                    >
                        <span>"📋 Copy Selected Rows (Ctrl+C)"</span>
                    </button>

                    <button
                        type="button"
                        on:click=open_copy_rows_modal
                        class="px-3 py-1.5 rounded-lg bg-cyan-500/15 hover:bg-cyan-500/25 border border-cyan-500/40 text-cyan-300 text-xs font-semibold flex items-center gap-1.5 transition cursor-pointer"
                    >
                        <span>"↗️ Copy Rows to Another Grid..."</span>
                    </button>

                    <Show when=move || auth.can_edit_space(space_id.get())>
                        <button
                            type="button"
                            on:click=handle_duplicate_selected_rows_here
                            class="px-3 py-1.5 rounded-lg bg-slate-800 hover:bg-slate-700 border border-slate-700 text-slate-200 text-xs font-semibold flex items-center gap-1.5 transition cursor-pointer"
                        >
                            <span>"👯 Duplicate Selected Rows Here"</span>
                        </button>

                        <button
                            type="button"
                            on:click=handle_bulk_delete
                            class="px-3 py-1.5 rounded-lg bg-red-600/90 hover:bg-red-500 text-white text-xs font-bold shadow transition cursor-pointer"
                        >
                            <span>"🗑️ Delete Selected Rows"</span>
                        </button>
                    </Show>

                    <button
                        type="button"
                        title="Clear Selection (Escape)"
                        on:click=move |_| {
                            selected_row_ids.set(HashSet::new());
                            last_selected_row_idx.set(None);
                        }
                        class="px-2.5 py-1.5 rounded-lg bg-slate-800/80 hover:bg-slate-700 text-slate-400 hover:text-white text-xs font-medium transition cursor-pointer"
                    >
                        "✕ Clear Selection"
                    </button>
                </div>
            </Show>

            // ================================================================
            // 5. Phase 17: Duplicate Grid Modal (`POST /api/grids/{grid_id}/duplicate`)
            // ================================================================
            <Show when=move || show_dup_grid_modal.get()>
                <div
                    on:click=move |ev| ev.stop_propagation()
                    class="fixed inset-0 z-50 bg-black/70 backdrop-blur-sm flex items-center justify-center p-4"
                >
                    <form
                        on:submit=handle_submit_duplicate_grid
                        class="w-full max-w-md rounded-2xl bg-[#1E2227] border border-[#2A2F37] p-6 space-y-4 shadow-2xl"
                    >
                        <div class="flex items-center justify-between">
                            <div>
                                <h2 class="text-base font-bold text-white">"📑 Duplicate Grid"</h2>
                                <p class="text-xs text-slate-400 mt-0.5">
                                    "Clone columns and optional rows into this Space or another Space"
                                </p>
                            </div>
                            <button
                                type="button"
                                on:click=move |_| show_dup_grid_modal.set(false)
                                class="text-slate-400 hover:text-white text-sm"
                            >
                                "✕"
                            </button>
                        </div>

                        <div>
                            <label class="block text-xs font-medium text-slate-300 mb-1">
                                "New Grid Name"
                            </label>
                            <input
                                type="text"
                                required
                                class="w-full px-3 py-2 rounded-xl bg-[#14171A] border border-[#2A2F37] text-sm text-white focus:outline-none focus:border-[#06B6D4]"
                                prop:value=dup_grid_name
                                on:input=move |ev| dup_grid_name.set(event_target_value(&ev))
                            />
                        </div>

                        <div>
                            <label class="block text-xs font-medium text-slate-300 mb-1">
                                "Destination Space"
                            </label>
                            <select
                                class="w-full px-3 py-2 rounded-xl bg-[#14171A] border border-[#2A2F37] text-xs text-white focus:outline-none focus:border-[#06B6D4]"
                                on:change=move |ev| {
                                    if let Ok(parsed) = Uuid::parse_str(&event_target_value(&ev)) {
                                        dup_grid_space_id.set(Some(parsed));
                                    }
                                }
                            >
                                {move || {
                                    let selected_sid = dup_grid_space_id.get();
                                    auth.spaces
                                        .get()
                                        .into_iter()
                                        .filter(|s| auth.can_edit_space(s.space.id))
                                        .map(|s| {
                                            let sid = s.space.id;
                                            let is_sel = selected_sid == Some(sid);
                                            view! {
                                                <option value=sid.to_string() selected=is_sel>
                                                    {format!("{} {}", s.space.icon.unwrap_or_else(|| "📁".into()), s.space.name)}
                                                </option>
                                            }
                                        })
                                        .collect_view()
                                }}
                            </select>
                        </div>

                        <label class="flex items-center gap-2.5 p-3 rounded-xl bg-[#14171A] border border-[#2A2F37] cursor-pointer select-none">
                            <input
                                type="checkbox"
                                class="h-4 w-4 rounded accent-[#06B6D4]"
                                prop:checked=dup_grid_include_rows
                                on:change=move |ev| dup_grid_include_rows.set(event_target_checked(&ev))
                            />
                            <div>
                                <div class="text-xs font-semibold text-white">
                                    "Include all rows & data"
                                </div>
                                <div class="text-[11px] text-slate-400">
                                    "Uncheck to create a clean template copy with columns only"
                                </div>
                            </div>
                        </label>

                        <div class="flex justify-end gap-2 pt-2">
                            <button
                                type="button"
                                on:click=move |_| show_dup_grid_modal.set(false)
                                class="px-4 py-2 rounded-xl bg-[#14171A] text-slate-300 text-xs font-medium"
                            >
                                "Cancel"
                            </button>
                            <button
                                type="submit"
                                class="px-4 py-2 rounded-xl bg-[#06B6D4] hover:bg-[#0891B2] text-slate-950 text-xs font-bold shadow"
                            >
                                "Duplicate Grid"
                            </button>
                        </div>
                    </form>
                </div>
            </Show>

            // ================================================================
            // 6. Phase 17: Move Grid to Another Space Modal (`PATCH /api/grids/{grid_id}/move`)
            // ================================================================
            <Show when=move || show_move_grid_modal.get()>
                <div
                    on:click=move |ev| ev.stop_propagation()
                    class="fixed inset-0 z-50 bg-black/70 backdrop-blur-sm flex items-center justify-center p-4"
                >
                    <form
                        on:submit=handle_submit_move_grid
                        class="w-full max-w-md rounded-2xl bg-[#1E2227] border border-[#2A2F37] p-6 space-y-4 shadow-2xl"
                    >
                        <div class="flex items-center justify-between">
                            <div>
                                <h2 class="text-base font-bold text-white">"📂 Move Grid to Another Space"</h2>
                                <p class="text-xs text-slate-400 mt-0.5">
                                    "All columns, rows, attachments, and 30-day history move intact"
                                </p>
                            </div>
                            <button
                                type="button"
                                on:click=move |_| show_move_grid_modal.set(false)
                                class="text-slate-400 hover:text-white text-sm"
                            >
                                "✕"
                            </button>
                        </div>

                        <div>
                            <label class="block text-xs font-medium text-slate-300 mb-1">
                                "Target Space"
                            </label>
                            <select
                                class="w-full px-3 py-2 rounded-xl bg-[#14171A] border border-[#2A2F37] text-xs text-white focus:outline-none focus:border-[#06B6D4]"
                                on:change=move |ev| {
                                    if let Ok(parsed) = Uuid::parse_str(&event_target_value(&ev)) {
                                        move_grid_space_id.set(Some(parsed));
                                    }
                                }
                            >
                                {move || {
                                    let cur_sid = space_id.get();
                                    let selected_sid = move_grid_space_id.get();
                                    auth.spaces
                                        .get()
                                        .into_iter()
                                        .filter(|s| s.space.id != cur_sid && auth.can_edit_space(s.space.id))
                                        .map(|s| {
                                            let sid = s.space.id;
                                            let is_sel = selected_sid == Some(sid);
                                            view! {
                                                <option value=sid.to_string() selected=is_sel>
                                                    {format!("{} {}", s.space.icon.unwrap_or_else(|| "📁".into()), s.space.name)}
                                                </option>
                                            }
                                        })
                                        .collect_view()
                                }}
                            </select>
                        </div>

                        <div class="flex justify-end gap-2 pt-2">
                            <button
                                type="button"
                                on:click=move |_| show_move_grid_modal.set(false)
                                class="px-4 py-2 rounded-xl bg-[#14171A] text-slate-300 text-xs font-medium"
                            >
                                "Cancel"
                            </button>
                            <button
                                type="submit"
                                disabled=move || move_grid_space_id.get().is_none()
                                class="px-4 py-2 rounded-xl bg-[#F97316] hover:bg-[#EA580C] disabled:opacity-40 text-white text-xs font-bold shadow"
                            >
                                "Move Grid"
                            </button>
                        </div>
                    </form>
                </div>
            </Show>

            // ================================================================
            // 7. Phase 17: Copy Specific Rows to Another Grid Modal
            //    (`POST /api/grids/{source_grid_id}/rows/copy-to/{target_grid_id}`)
            // ================================================================
            <Show when=move || show_copy_rows_modal.get()>
                <div
                    on:click=move |ev| ev.stop_propagation()
                    class="fixed inset-0 z-50 bg-black/70 backdrop-blur-sm flex items-center justify-center p-4"
                >
                    <form
                        on:submit=handle_submit_copy_rows
                        class="w-full max-w-md rounded-2xl bg-[#1E2227] border border-[#2A2F37] p-6 space-y-4 shadow-2xl"
                    >
                        <div class="flex items-center justify-between">
                            <div>
                                <h2 class="text-base font-bold text-white">
                                    "↗️ Copy Rows to Another Grid"
                                </h2>
                                <p class="text-xs text-[#06B6D4] font-semibold mt-0.5">
                                    {move || format!("Copying {} selected row(s)", selected_row_ids.get().len())}
                                </p>
                            </div>
                            <button
                                type="button"
                                on:click=move |_| {
                                    show_copy_rows_modal.set(false);
                                    copy_rows_success_banner.set(None);
                                }
                                class="text-slate-400 hover:text-white text-sm"
                            >
                                "✕"
                            </button>
                        </div>

                        <div>
                            <label class="block text-xs font-medium text-slate-300 mb-1">
                                "Target Grid (Space Name → Grid Name)"
                            </label>
                            <select
                                class="w-full px-3 py-2 rounded-xl bg-[#14171A] border border-[#2A2F37] text-xs text-white focus:outline-none focus:border-[#06B6D4]"
                                on:change=move |ev| {
                                    if let Ok(parsed) = Uuid::parse_str(&event_target_value(&ev)) {
                                        copy_rows_target_grid_id.set(Some(parsed));
                                    }
                                }
                            >
                                {move || {
                                    let selected_gid = copy_rows_target_grid_id.get();
                                    let cur_gid = grid_id.get();
                                    let grids_map = auth.space_grids.get();
                                    auth.spaces
                                        .get()
                                        .into_iter()
                                        .filter(|s| auth.can_edit_space(s.space.id))
                                        .flat_map(|s| {
                                            let sp_name = s.space.name.clone();
                                            let list = grids_map
                                                .get(&s.space.id)
                                                .cloned()
                                                .unwrap_or_default();
                                            list.into_iter()
                                                .map(move |g| (sp_name.clone(), g))
                                                .collect::<Vec<_>>()
                                        })
                                        .map(|(sp_name, g)| {
                                            let gid_opt = g.id;
                                            let is_sel = selected_gid == Some(gid_opt);
                                            let suffix = if gid_opt == cur_gid {
                                                " (Current Grid)"
                                            } else {
                                                ""
                                            };
                                            view! {
                                                <option value=gid_opt.to_string() selected=is_sel>
                                                    {format!("{} → {}{}", sp_name, g.title, suffix)}
                                                </option>
                                            }
                                        })
                                        .collect_view()
                                }}
                            </select>
                        </div>

                        <label class="flex items-center gap-2.5 p-3 rounded-xl bg-[#14171A] border border-[#2A2F37] cursor-pointer select-none">
                            <input
                                type="checkbox"
                                class="h-4 w-4 rounded accent-[#06B6D4]"
                                prop:checked=copy_rows_auto_create_cols
                                on:change=move |ev| copy_rows_auto_create_cols.set(event_target_checked(&ev))
                            />
                            <div>
                                <div class="text-xs font-semibold text-white">
                                    "Automatically create missing columns in target grid"
                                </div>
                                <div class="text-[11px] text-slate-400">
                                    "Matches columns by name and creates any missing columns or Select/MultiSelect options automatically"
                                </div>
                            </div>
                        </label>

                        {move || {
                            copy_rows_success_banner.get().map(
                                |(copied_cnt, created_cols_cnt, target_sid, target_gid, target_title)| {
                                    view! {
                                        <div class="p-3.5 rounded-xl bg-emerald-950/60 border border-emerald-500/40 space-y-2">
                                            <div class="text-xs font-bold text-emerald-300">
                                                {format!("✓ Copied {copied_cnt} rows to {target_title}!")}
                                            </div>
                                            {if created_cols_cnt > 0 {
                                                view! {
                                                    <div class="text-[11px] text-emerald-200/80">
                                                        {format!("Automatically created {created_cols_cnt} missing column(s) in {target_title}.")}
                                                    </div>
                                                }
                                                .into_any()
                                            } else {
                                                view! { <span /> }.into_any()
                                            }}
                                            <div>
                                                <button
                                                    type="button"
                                                    on:click=move |_| {
                                                        show_copy_rows_modal.set(false);
                                                        copy_rows_success_banner.set(None);
                                                        auth.active_space_id.set(Some(target_sid));
                                                        auth.active_grid_id.set(Some(target_gid));
                                                        on_navigate.run(format!(
                                                            "/spaces/{target_sid}/grids/{target_gid}"
                                                        ));
                                                    }
                                                    class="px-3 py-1.5 rounded-lg bg-emerald-500 hover:bg-emerald-400 text-slate-950 text-xs font-bold transition cursor-pointer"
                                                >
                                                    "Open Target Grid →"
                                                </button>
                                            </div>
                                        </div>
                                    }
                                },
                            )
                        }}

                        <div class="flex justify-end gap-2 pt-2">
                            <button
                                type="button"
                                on:click=move |_| {
                                    show_copy_rows_modal.set(false);
                                    copy_rows_success_banner.set(None);
                                }
                                class="px-4 py-2 rounded-xl bg-[#14171A] text-slate-300 text-xs font-medium"
                            >
                                "Close"
                            </button>
                            <button
                                type="submit"
                                disabled=move || copy_rows_target_grid_id.get().is_none()
                                class="px-4 py-2 rounded-xl bg-[#06B6D4] hover:bg-[#0891B2] disabled:opacity-40 text-slate-950 text-xs font-bold shadow"
                            >
                                "Copy Rows Now"
                            </button>
                        </div>
                    </form>
                </div>
            </Show>

            // ================================================================
            // Phase 18: Grid-Level PyraAI Copilot Slide-Over Drawer (`POST /api/ai/copilot`)
            // ================================================================
            <Show when=move || show_ai_drawer.get()>
                {
                    let scroll_ai_to_bottom = move || {
                        if let Some(window) = web_sys::window() {
                            let chat_ref = grid_ai_chat_scroll_ref;
                            let cb = Closure::once(move || {
                                if let Some(el) = chat_ref.get_untracked() {
                                    el.set_scroll_top(el.scroll_height());
                                }
                            });
                            let _ = window.set_timeout_with_callback_and_timeout_and_arguments_0(
                                cb.as_ref().unchecked_ref(),
                                30,
                            );
                            cb.forget();
                        }
                    };

                    let scroll_row_into_view = move |rid: Uuid| {
                        if let Some(window) = web_sys::window() {
                            let row_dom_id = format!("grid-row-{rid}");
                            let cb = Closure::once(move || {
                                if let Some(doc) = web_sys::window().and_then(|w| w.document()) {
                                    if let Some(el) = doc.get_element_by_id(&row_dom_id) {
                                        el.scroll_into_view_with_bool(true);
                                    }
                                }
                            });
                            let _ = window.set_timeout_with_callback_and_timeout_and_arguments_0(
                                cb.as_ref().unchecked_ref(),
                                80,
                            );
                            cb.forget();
                        }
                    };

                    let dispatch_grid_ai_prompt = move |prompt_text: String, mode_override: Option<String>| {
                        let prompt = prompt_text.trim().to_string();
                        let pending_atts = grid_ai_attachments.get_untracked();
                        if (prompt.is_empty() && pending_atts.is_empty()) || grid_ai_loading.get_untracked() {
                            return;
                        }
                        if let Some(ref m) = mode_override {
                            grid_ai_mode.set(m.clone());
                        }
                        let mode = mode_override.unwrap_or_else(|| grid_ai_mode.get_untracked());
                        let hist = grid_ai_history.get_untracked();
                        let sid = space_id.get_untracked();
                        let gid = grid_id.get_untracked();
                        let display_prompt = if prompt.is_empty() {
                            format!("Attached {} file(s)", pending_atts.len())
                        } else {
                            prompt.clone()
                        };

                        grid_ai_history.update(|list| {
                            list.push(AiChatMessage {
                                role: "user".into(),
                                content: display_prompt,
                                attachments: pending_atts.clone(),
                                navigation_targets: Vec::new(),
                            });
                        });
                        grid_ai_prompt.set(String::new());
                        grid_ai_attachments.set(Vec::new());
                        grid_ai_loading.set(true);
                        scroll_ai_to_bottom();

                        spawn_local(async move {
                            match ApiClient::run_ai_copilot(&auth, prompt, mode, Some(sid), Some(gid), hist, pending_atts).await {
                                Ok(resp) => {
                                    grid_ai_model.set(resp.model_used);
                                    let did_mutate = !resp.executed_actions.is_empty();
                                    if did_mutate {
                                        grid_ai_actions.set(resp.executed_actions);
                                        refresh_trigger.update(|n| *n += 1);
                                        reload_rows();
                                    }
                                    let nav_targets = resp.navigation_targets.clone();
                                    grid_ai_history.update(|list| {
                                        list.push(AiChatMessage {
                                            role: "assistant".into(),
                                            content: resp.reply,
                                            attachments: Vec::new(),
                                            navigation_targets: nav_targets,
                                        });
                                    });
                                    if let Some(target_rid) = resp.navigated_row_id {
                                        auth.highlighted_row_id.set(Some(target_rid));
                                    }
                                    if let Some(new_gid) = resp.navigated_grid_id {
                                        let target_sid = resp.navigated_space_id.unwrap_or(sid);
                                        if new_gid != gid || target_sid != sid {
                                            if let Ok(grids) = ApiClient::list_space_grids(&auth, target_sid).await {
                                                auth.space_grids.update(|map| {
                                                    map.insert(target_sid, grids);
                                                });
                                            }
                                            auth.active_space_id.set(Some(target_sid));
                                            auth.active_grid_id.set(Some(new_gid));
                                            on_navigate.run(format!("/spaces/{target_sid}/grids/{new_gid}"));
                                        } else if let Some(target_rid) = resp.navigated_row_id {
                                            scroll_row_into_view(target_rid);
                                        }
                                    } else if let Some(target_rid) = resp.navigated_row_id {
                                        scroll_row_into_view(target_rid);
                                    }
                                }
                                Err(err) => {
                                    grid_ai_history.update(|list| {
                                        list.push(AiChatMessage {
                                            role: "assistant".into(),
                                            content: format!("⚠️ Error: {err}"),
                                            attachments: Vec::new(),
                                            navigation_targets: Vec::new(),
                                        });
                                    });
                                }
                            }
                            grid_ai_loading.set(false);
                            scroll_ai_to_bottom();
                        });
                    };

                    view! {
                        <div
                            on:click=move |ev| ev.stop_propagation()
                            on:dragover=move |ev: web_sys::DragEvent| {
                                ev.prevent_default();
                                grid_ai_drag_over.set(true);
                            }
                            on:dragleave=move |ev: web_sys::DragEvent| {
                                ev.prevent_default();
                                grid_ai_drag_over.set(false);
                            }
                            on:drop=move |ev: web_sys::DragEvent| {
                                ev.prevent_default();
                                grid_ai_drag_over.set(false);
                                if let Some(dt) = ev.data_transfer() {
                                    read_file_list_to_ai_attachments(dt.files(), grid_ai_attachments);
                                }
                            }
                            on:paste=move |ev: web_sys::Event| {
                                let _ = handle_clipboard_paste_to_ai_attachments(&ev, grid_ai_attachments);
                            }
                            class=move || {
                                let ring_cls = if grid_ai_drag_over.get() {
                                    "border-[#06B6D4] ring-2 ring-[#06B6D4]/50"
                                } else {
                                    "border-[#2A2F37]"
                                };
                                format!("fixed inset-y-0 right-0 z-50 w-full max-w-md bg-[#1E2227] border-l {ring_cls} shadow-2xl flex flex-col p-5 space-y-4")
                            }
                        >
                            <div class="flex items-center justify-between border-b border-[#2A2F37] pb-3 shrink-0">
                                <div class="flex items-center gap-2">
                                    <span class="text-lg">"✨"</span>
                                    <div>
                                        <div class="text-sm font-bold text-white flex flex-wrap items-center gap-2">
                                            <span>"PyraAI Grid Copilot"</span>
                                            <span class="px-1.5 py-0.5 rounded bg-[#06B6D4]/15 border border-[#06B6D4]/40 text-[10px] font-mono text-[#06B6D4]">
                                                {move || grid_ai_model.get()}
                                            </span>
                                            {move || {
                                                if grid_ai_loading.get() {
                                                    view! {
                                                        <span class="px-2 py-0.5 rounded-full bg-amber-500/20 border border-amber-400/60 text-[10px] font-bold text-amber-300 animate-pulse flex items-center gap-1">
                                                            <span>"⚡ Processing..."</span>
                                                        </span>
                                                    }
                                                    .into_any()
                                                } else {
                                                    view! {
                                                        <span class="px-2 py-0.5 rounded-full bg-emerald-500/15 border border-emerald-500/40 text-[10px] font-semibold text-emerald-300 flex items-center gap-1">
                                                            <span>"🟢 Ready"</span>
                                                        </span>
                                                    }
                                                    .into_any()
                                                }
                                            }}
                                        </div>
                                        <div class="text-[11px] text-slate-400">
                                            "Search rows, generate Executive Reports, or add/edit/delete rows & columns"
                                        </div>
                                    </div>
                                </div>
                                <button
                                    type="button"
                                    on:click=move |_| show_ai_drawer.set(false)
                                    class="text-slate-400 hover:text-white"
                                >
                                    "✕"
                                </button>
                            </div>

                            // Mode Switcher Pills
                            <div class="grid grid-cols-3 gap-1.5 shrink-0">
                                <button
                                    type="button"
                                    disabled=move || grid_ai_loading.get()
                                    on:click=move |_| grid_ai_mode.set("ask".into())
                                    class=move || {
                                        let active = grid_ai_mode.get() == "ask";
                                        let cls = if active {
                                            "bg-[#06B6D4] text-slate-950 font-bold"
                                        } else {
                                            "bg-[#14171A] text-slate-300 border border-[#2A2F37]"
                                        };
                                        format!("py-1.5 px-2 rounded-lg text-[11px] transition disabled:opacity-50 {cls}")
                                    }
                                >
                                    "💬 Ask & Search"
                                </button>
                                <button
                                    type="button"
                                    disabled=move || grid_ai_loading.get()
                                    on:click=move |_| grid_ai_mode.set("report".into())
                                    class=move || {
                                        let active = grid_ai_mode.get() == "report";
                                        let cls = if active {
                                            "bg-[#06B6D4] text-slate-950 font-bold"
                                        } else {
                                            "bg-[#14171A] text-slate-300 border border-[#2A2F37]"
                                        };
                                        format!("py-1.5 px-2 rounded-lg text-[11px] transition disabled:opacity-50 {cls}")
                                    }
                                >
                                    "📊 Exec Report"
                                </button>
                                <button
                                    type="button"
                                    disabled=move || grid_ai_loading.get()
                                    on:click=move |_| grid_ai_mode.set("execute".into())
                                    class=move || {
                                        let active = grid_ai_mode.get() == "execute";
                                        let cls = if active {
                                            "bg-[#F97316] text-white font-bold"
                                        } else {
                                            "bg-[#14171A] text-slate-300 border border-[#2A2F37]"
                                        };
                                        format!("py-1.5 px-2 rounded-lg text-[11px] transition disabled:opacity-50 {cls}")
                                    }
                                >
                                    "⚡ Execute Task"
                                </button>
                            </div>

                            // Quick Action Chips (Click to immediately execute or pre-fill)
                            <div class="flex flex-wrap gap-1.5 shrink-0">
                                <button
                                    type="button"
                                    disabled=move || grid_ai_loading.get()
                                    on:click=move |_| {
                                        dispatch_grid_ai_prompt(
                                            "Generate an executive report with SUM, AVG, EGP/USD totals, and department breakdown".into(),
                                            Some("report".into()),
                                        );
                                    }
                                    class="px-2 py-1 rounded-lg bg-[#14171A] hover:bg-slate-800 disabled:opacity-50 border border-[#2A2F37] text-[10px] text-slate-300 transition"
                                >
                                    "📊 Executive Report"
                                </button>
                                <button
                                    type="button"
                                    disabled=move || grid_ai_loading.get()
                                    on:click=move |_| {
                                        grid_ai_mode.set("execute".into());
                                        grid_ai_prompt.set("Add row with Title = Q4 Audit, Status = In Progress".into());
                                    }
                                    class="px-2 py-1 rounded-lg bg-[#14171A] hover:bg-slate-800 disabled:opacity-50 border border-[#2A2F37] text-[10px] text-slate-300 transition"
                                >
                                    "➕ Add Row..."
                                </button>
                                <button
                                    type="button"
                                    disabled=move || grid_ai_loading.get()
                                    on:click=move |_| {
                                        grid_ai_mode.set("execute".into());
                                        grid_ai_prompt.set("Add column Budget of type currency".into());
                                    }
                                    class="px-2 py-1 rounded-lg bg-[#14171A] hover:bg-slate-800 disabled:opacity-50 border border-[#2A2F37] text-[10px] text-slate-300 transition"
                                >
                                    "🏗️ Add Column..."
                                </button>
                            </div>

                            // Messages, Live Working Card & Executed Task Log
                            <div
                                node_ref=grid_ai_chat_scroll_ref
                                class="flex-1 overflow-y-auto space-y-2.5 p-3 rounded-xl bg-[#14171A] border border-[#2A2F37]"
                            >
                                {move || {
                                    let msgs = grid_ai_history.get();
                                    let is_working = grid_ai_loading.get();
                                    if msgs.is_empty() && !is_working {
                                        return view! {
                                            <div class="py-10 text-center text-xs text-slate-400 space-y-1">
                                                <div>"PyraAI is connected to this Grid."</div>
                                                <div class="text-[11px] text-slate-500">
                                                    "Try: 'Add row with Name = Sara, Budget = 5000' or 'Generate an executive report'"
                                                </div>
                                            </div>
                                        }
                                        .into_any();
                                    }
                                    msgs.into_iter()
                                        .map(|m| {
                                            let is_user = m.role == "user";
                                            let cls = if is_user {
                                                "bg-[#F97316]/15 border-[#F97316]/40 text-white ml-6"
                                            } else {
                                                "bg-[#1E2227] border-[#06B6D4]/40 text-slate-200 mr-2"
                                            };
                                            let msg_atts = m.attachments.clone();
                                            let nav_targets = m.navigation_targets.clone();
                                            view! {
                                                <div class=format!("p-2.5 rounded-xl border text-xs whitespace-pre-wrap leading-relaxed {cls}")>
                                                    <div class="text-[10px] font-bold uppercase tracking-wider text-slate-400 mb-0.5">
                                                        {if is_user { "You" } else { "✨ PyraAI" }}
                                                    </div>
                                                    {if msg_atts.is_empty() {
                                                        view! { <span /> }.into_any()
                                                    } else {
                                                        view! {
                                                            <div class="flex flex-wrap gap-1.5 mb-1.5">
                                                                {msg_atts
                                                                    .into_iter()
                                                                    .map(|att| {
                                                                        let is_img = att.mime_type.to_ascii_lowercase().starts_with("image/");
                                                                        if is_img {
                                                                            let data_url = format!("data:{};base64,{}", att.mime_type, att.data_base64);
                                                                            view! {
                                                                                <div class="flex items-center gap-1.5 p-1 rounded-lg bg-slate-900/80 border border-slate-700/70">
                                                                                    <img
                                                                                        src=data_url
                                                                                        alt=att.filename.clone()
                                                                                        class="h-10 w-10 rounded object-cover border border-cyan-500/30"
                                                                                    />
                                                                                    <span class="text-[10px] text-slate-300 max-w-[110px] truncate">{att.filename}</span>
                                                                                </div>
                                                                            }
                                                                            .into_any()
                                                                        } else {
                                                                            view! {
                                                                                <div class="px-2 py-1 rounded-lg bg-slate-900/80 border border-slate-700/70 text-[10px] text-cyan-300 flex items-center gap-1">
                                                                                    <span>"📄"</span>
                                                                                    <span class="max-w-[140px] truncate">{att.filename}</span>
                                                                                </div>
                                                                            }
                                                                            .into_any()
                                                                        }
                                                                    })
                                                                    .collect_view()}
                                                            </div>
                                                        }
                                                        .into_any()
                                                    }}
                                                    <div>{m.content}</div>
                                                    {if nav_targets.is_empty() {
                                                        view! { <span /> }.into_any()
                                                    } else {
                                                        view! {
                                                            <div class="mt-2.5 pt-2 border-t border-[#2A2F37]/80 space-y-1.5">
                                                                <div class="text-[10px] font-bold uppercase tracking-wider text-cyan-400">
                                                                    {format!("🧭 Matched Locations Across Workspace ({}) — Click to Jump:", nav_targets.len())}
                                                                </div>
                                                                <div class="space-y-1">
                                                                    {nav_targets
                                                                        .into_iter()
                                                                        .map(|target: AiNavigationTarget| {
                                                                            let t_sid = target.space_id;
                                                                            let t_gid = target.grid_id;
                                                                            let t_rid = target.row_id;
                                                                            let badge_text = if let Some(rnum) = target.row_number {
                                                                                format!("{} / {} · Row #{}", target.space_name, target.grid_name, rnum)
                                                                            } else {
                                                                                format!("{} / {}", target.space_name, target.grid_name)
                                                                            };
                                                                            view! {
                                                                                <button
                                                                                    type="button"
                                                                                    on:click=move |_| {
                                                                                        auth.active_space_id.set(Some(t_sid));
                                                                                        auth.active_grid_id.set(Some(t_gid));
                                                                                        auth.highlighted_row_id.set(t_rid);
                                                                                        if t_gid != grid_id.get_untracked() || t_sid != space_id.get_untracked() {
                                                                                            spawn_local(async move {
                                                                                                if let Ok(grids) = ApiClient::list_space_grids(&auth, t_sid).await {
                                                                                                    auth.space_grids.update(|map| {
                                                                                                        map.insert(t_sid, grids);
                                                                                                    });
                                                                                                }
                                                                                            });
                                                                                            on_navigate.run(format!("/spaces/{t_sid}/grids/{t_gid}"));
                                                                                        } else if let Some(rid) = t_rid {
                                                                                            scroll_row_into_view(rid);
                                                                                        }
                                                                                    }
                                                                                    class="w-full text-left p-2 rounded-lg bg-[#14171A] hover:bg-cyan-500/15 border border-cyan-500/30 hover:border-cyan-400 transition flex items-center justify-between gap-2 group"
                                                                                >
                                                                                    <div class="min-w-0 flex-1">
                                                                                        <div class="flex flex-wrap items-center gap-1.5 text-[11px] font-bold text-white">
                                                                                            <span>{target.grid_icon.unwrap_or_else(|| "📄".into())}</span>
                                                                                            <span class="text-cyan-300 truncate">{target.primary_label}</span>
                                                                                            <span class="px-1.5 py-0.5 rounded bg-slate-800 border border-slate-700 text-[10px] font-normal text-slate-300">
                                                                                                {badge_text}
                                                                                            </span>
                                                                                        </div>
                                                                                        <div class="text-[10px] text-slate-400 truncate mt-0.5">
                                                                                            {target.matched_preview}
                                                                                        </div>
                                                                                    </div>
                                                                                    <span class="text-[10px] font-bold text-[#F97316] group-hover:translate-x-0.5 transition shrink-0">
                                                                                        "Jump →"
                                                                                    </span>
                                                                                </button>
                                                                            }
                                                                        })
                                                                        .collect_view()}
                                                                </div>
                                                            </div>
                                                        }
                                                        .into_any()
                                                    }}
                                                </div>
                                            }
                                        })
                                        .collect_view()
                                        .into_any()
                                }}

                                // High-Visibility "PyraAI is working on your task..." Live Thinking Card
                                <Show when=move || grid_ai_loading.get()>
                                    <div class="bg-slate-900/90 border border-cyan-500/40 rounded-xl p-4 shadow-lg flex flex-col gap-2.5 animate-pulse">
                                        <div class="flex items-center justify-between gap-2">
                                            <div class="flex items-center gap-2.5">
                                                <div class="animate-spin h-5 w-5 border-2 border-cyan-400 border-t-orange-500 rounded-full shrink-0" />
                                                <span class="text-xs font-bold text-cyan-300">
                                                    "✨ PyraAI is working on your task..."
                                                </span>
                                            </div>
                                            <div class="flex items-center gap-1">
                                                <span class="h-1.5 w-1.5 rounded-full bg-cyan-400 animate-bounce" />
                                                <span class="h-1.5 w-1.5 rounded-full bg-orange-400 animate-bounce" />
                                                <span class="h-1.5 w-1.5 rounded-full bg-cyan-400 animate-bounce" />
                                            </div>
                                        </div>

                                        <div class="text-[11px] text-slate-300 space-y-1 pl-7">
                                            <div>
                                                {move || {
                                                    let m = grid_ai_mode.get();
                                                    if m == "execute" {
                                                        "🔍 Parsing task schema & validating grid columns..."
                                                    } else if m == "report" {
                                                        "🔍 Scanning grid columns, tags & EGP/USD totals..."
                                                    } else {
                                                        "🔍 Scanning grid rows, tags & property values..."
                                                    }
                                                }}
                                            </div>
                                            <div class="text-cyan-400/90 font-medium">
                                                {move || {
                                                    let m = grid_ai_mode.get();
                                                    if m == "execute" {
                                                        "🧠 Generating response & executing workspace actions..."
                                                    } else if m == "report" {
                                                        "🧠 Synthesizing Executive Markdown Report with Ollama..."
                                                    } else {
                                                        "🧠 Generating response & matching workspace context..."
                                                    }
                                                }}
                                            </div>
                                        </div>

                                        <div class="w-full h-1.5 bg-slate-800 rounded-full overflow-hidden">
                                            <div class="h-full w-2/3 bg-gradient-to-r from-cyan-400 via-orange-500 to-cyan-400 rounded-full animate-pulse" />
                                        </div>
                                    </div>
                                </Show>

                                {move || {
                                    let acts = grid_ai_actions.get();
                                    if acts.is_empty() {
                                        view! { <span /> }.into_any()
                                    } else {
                                        view! {
                                            <div class="space-y-1 pt-1">
                                                {acts
                                                    .into_iter()
                                                    .map(|a| {
                                                        view! {
                                                            <div class="px-2.5 py-1.5 rounded-lg bg-emerald-500/15 border border-emerald-500/40 text-emerald-300 text-[11px] font-semibold">
                                                                "✓ " {a.summary}
                                                            </div>
                                                        }
                                                    })
                                                    .collect_view()}
                                            </div>
                                        }
                                        .into_any()
                                    }
                                }}
                            </div>

                            // Pending Attachments Preview Strip (Images & Files via Paperclip, Drag & Drop, or Ctrl+V)
                            <Show when=move || !grid_ai_attachments.get().is_empty()>
                                <div class="flex flex-wrap gap-1.5 p-2 rounded-xl bg-[#14171A] border border-cyan-500/30 shrink-0">
                                    {move || {
                                        grid_ai_attachments
                                            .get()
                                            .into_iter()
                                            .enumerate()
                                            .map(|(idx, att)| {
                                                let is_img = att.mime_type.to_ascii_lowercase().starts_with("image/");
                                                let data_url = format!("data:{};base64,{}", att.mime_type, att.data_base64);
                                                view! {
                                                    <div class="flex items-center gap-1.5 px-2 py-1 rounded-lg bg-[#1E2227] border border-[#2A2F37] text-[11px] text-slate-200">
                                                        {if is_img {
                                                            view! {
                                                                <img
                                                                    src=data_url
                                                                    alt=att.filename.clone()
                                                                    class="h-7 w-7 rounded object-cover border border-cyan-500/40 shrink-0"
                                                                />
                                                            }
                                                            .into_any()
                                                        } else {
                                                            view! { <span class="text-cyan-400">"📄"</span> }.into_any()
                                                        }}
                                                        <span class="max-w-[110px] truncate font-medium">{att.filename}</span>
                                                        <button
                                                            type="button"
                                                            on:click=move |_| {
                                                                grid_ai_attachments.update(|list| {
                                                                    if idx < list.len() {
                                                                        list.remove(idx);
                                                                    }
                                                                });
                                                            }
                                                            class="text-slate-400 hover:text-rose-400 font-bold ml-1"
                                                            title="Remove attachment"
                                                        >
                                                            "✕"
                                                        </button>
                                                    </div>
                                                }
                                            })
                                            .collect_view()
                                    }}
                                </div>
                            </Show>

                            <input
                                node_ref=grid_ai_file_input_ref
                                type="file"
                                multiple=true
                                accept="image/*,.csv,.tsv,.txt,.md,.json,.pdf"
                                class="hidden"
                                on:change=move |ev| {
                                    let input: web_sys::HtmlInputElement = event_target(&ev);
                                    read_file_list_to_ai_attachments(input.files(), grid_ai_attachments);
                                    input.set_value("");
                                }
                            />

                            <form
                                class="flex items-center gap-1.5 shrink-0"
                                on:submit=move |ev: web_sys::SubmitEvent| {
                                    ev.prevent_default();
                                    let prompt = grid_ai_prompt.get();
                                    dispatch_grid_ai_prompt(prompt, None);
                                }
                            >
                                <button
                                    type="button"
                                    disabled=move || grid_ai_loading.get()
                                    on:click=move |_| {
                                        if let Some(el) = grid_ai_file_input_ref.get_untracked() {
                                            el.click();
                                        }
                                    }
                                    title="Attach image or file (or Drag & Drop / Ctrl+V screenshot)"
                                    class="px-2.5 py-2 rounded-xl bg-[#14171A] hover:bg-slate-800 border border-[#2A2F37] disabled:opacity-50 text-xs text-cyan-400 transition shrink-0"
                                >
                                    "📎"
                                </button>
                                <input
                                    type="text"
                                    disabled=move || grid_ai_loading.get()
                                    placeholder=move || {
                                        if grid_ai_loading.get() {
                                            "PyraAI is processing your request..."
                                        } else {
                                            "Ask PyraAI, paste screenshot (Ctrl+V), or attach file..."
                                        }
                                    }
                                    class="flex-1 px-3 py-2 rounded-xl bg-[#14171A] border border-[#2A2F37] disabled:opacity-60 disabled:cursor-wait text-xs text-white focus:outline-none focus:border-[#06B6D4]"
                                    prop:value=grid_ai_prompt
                                    on:input=move |ev| grid_ai_prompt.set(event_target_value(&ev))
                                />
                                <button
                                    type="submit"
                                    disabled=move || grid_ai_loading.get()
                                    class=move || {
                                        if grid_ai_loading.get() {
                                            "px-3.5 py-2 rounded-xl bg-cyan-600/50 text-cyan-200 cursor-wait font-bold text-xs shadow transition shrink-0 flex items-center gap-1.5"
                                        } else {
                                            "px-3.5 py-2 rounded-xl bg-[#06B6D4] hover:bg-[#0284C7] text-slate-950 font-bold text-xs shadow transition shrink-0 flex items-center gap-1.5"
                                        }
                                    }
                                >
                                    {move || {
                                        if grid_ai_loading.get() {
                                            view! {
                                                <>
                                                    <span class="inline-block animate-spin h-3.5 w-3.5 border-2 border-cyan-200 border-t-orange-400 rounded-full" />
                                                    <span>"⏳ Working..."</span>
                                                </>
                                            }
                                            .into_any()
                                        } else {
                                            view! { <span>"Send"</span> }.into_any()
                                        }
                                    }}
                                </button>
                            </form>
                        </div>
                    }
                }
            </Show>
        </div>
    }
}

fn property_type_icon(pt: ColumnPropertyType) -> &'static str {
    match pt {
        ColumnPropertyType::Text => "Aa",
        ColumnPropertyType::Number => "#",
        ColumnPropertyType::Select => "◉",
        ColumnPropertyType::MultiSelect => "☰",
        ColumnPropertyType::Date => "📅",
        ColumnPropertyType::Time => "🕒",
        ColumnPropertyType::Files => "📎",
        ColumnPropertyType::Checkbox => "☑",
        ColumnPropertyType::Checklist => "🗹",
        ColumnPropertyType::LastModified => "⏱",
        ColumnPropertyType::Person => "👤",
        ColumnPropertyType::Password => "🔒",
    }
}
