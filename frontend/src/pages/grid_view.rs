use crate::{
    api::ApiClient,
    components::cell_editor::{
        cell_to_plain_text, compute_floating_popover_style, tag_color_classes, CellEditor,
        EIGHT_TAG_COLORS,
    },
    state::AuthState,
};
use chrono::{NaiveDate, NaiveTime, Utc};
use leptos::{prelude::*, task::spawn_local};
use pyra_ocean_shared::{
    generate_grid_a3_pdf_html, generate_grid_csv, generate_grid_markdown, slugify_grid_filename,
    BulkRowCellPatch, CellValue, ChecklistItem, ColumnPropertyConfig, ColumnPropertyType,
    CreateColumnRequest, FileAttachment, GridColumn, GridHistoryDetail, GridHistoryEntry, GridRow,
    NumberFormat, PageGrid, RowCellMap, SelectOption, SortDirection, SpaceMemberDetail, TagColor,
    UpdateColumnRequest,
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

const ALL_PROPERTY_TYPES: [(ColumnPropertyType, &'static str); 11] = [
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
    RowCreate {
        row_id: Uuid,
    },
    RowDelete {
        deleted_rows: Vec<GridRow>,
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
        _ => CellValue::Text(String::new()),
    }
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
    let confirming_delete_grid = RwSignal::new(false);

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

    // Inline Grid Page Renaming state (`PATCH /api/grids/{grid_id}`)
    let editing_grid_title = RwSignal::new(false);
    let draft_grid_title = RwSignal::new(String::new());

    // Sorting & Filtering state
    let sort_specs = RwSignal::new(Vec::<(Uuid, SortDirection)>::new());
    let show_sort_popover = RwSignal::new(false);
    let quick_search = RwSignal::new(String::new());
    let server_filter_col = RwSignal::new(Option::<Uuid>::None);
    let server_filter_val = RwSignal::new(String::new());

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

    // `+ Add Column` Popover state (Toolbar & End-of-Table `+` Column)
    let show_add_col_popover = RwSignal::new(false);
    let show_end_add_col_popover = RwSignal::new(false);
    let end_add_col_popover_style = RwSignal::new(String::new());
    let new_col_name = RwSignal::new(String::new());
    let new_col_type = RwSignal::new(ColumnPropertyType::Text);
    let new_col_options_csv = RwSignal::new(String::from("Todo, In Progress, Done"));
    let new_col_currency = RwSignal::new(String::new());

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
        open_col_menu_id.set(None);
        open_row_insert_menu.set(None);
        confirming_delete_col_id.set(None);
        menu_editing_opt_id.set(None);
        show_sort_popover.set(false);
        show_export_menu.set(false);
        show_add_col_popover.set(false);
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

    // Fetch rows from backend using current sort/filter parameters
    let reload_rows = move || {
        let gid = current_grid_id
            .get_untracked()
            .unwrap_or_else(|| grid_id.get_untracked());
        let sorts_q = build_sorts_query();
        let f_col = server_filter_col.get_untracked();
        let f_val = server_filter_val.get_untracked();

        spawn_local(async move {
            let f_op = if f_col.is_some() && !f_val.trim().is_empty() {
                Some("contains")
            } else {
                None
            };
            let f_val_ref = if f_val.trim().is_empty() {
                None
            } else {
                Some(f_val.trim())
            };

            match ApiClient::list_rows(&auth, gid, sorts_q, f_col, f_op, f_val_ref).await {
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
        selected_column_id.set(None);
        undo_stack.update(|u| u.clear());
        open_col_menu_id.set(None);
        open_row_insert_menu.set(None);
        confirming_delete_col_id.set(None);
        confirming_delete_grid.set(false);
        menu_editing_opt_id.set(None);
        show_sort_popover.set(false);
        show_export_menu.set(false);
        show_add_col_popover.set(false);
        show_end_add_col_popover.set(false);
        editing_grid_title.set(false);
        sort_specs.set(Vec::new());
        show_history_drawer.set(false);
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

            // Guarantee `is_loading.set(false)` in all Ok/Err paths
            is_loading.set(false);
        });
    });

    // Delete current Grid from Toolbar (`DELETE /api/grids/{grid_id}`) and redirect seamlessly
    let handle_delete_current_grid = move |_| {
        let Some(gid) = current_grid_id.get_untracked() else {
            return;
        };
        let sid = current_space_id
            .get_untracked()
            .unwrap_or_else(|| space_id.get_untracked());

        spawn_local(async move {
            match ApiClient::delete_grid(&auth, gid).await {
                Ok(_) => {
                    confirming_delete_grid.set(false);
                    let mut next_grid_id: Option<Uuid> = None;
                    auth.space_grids.update(|map| {
                        if let Some(list) = map.get_mut(&sid) {
                            list.retain(|g| g.id != gid);
                            next_grid_id = list.first().map(|g| g.id);
                        }
                    });
                    if let Some(next_gid) = next_grid_id {
                        auth.active_space_id.set(Some(sid));
                        auth.active_grid_id.set(Some(next_gid));
                        on_navigate.run(format!("/spaces/{sid}/grids/{next_gid}"));
                    } else {
                        auth.active_grid_id.set(None);
                        on_navigate.run("/".into());
                    }
                }
                Err(e) => error_banner.set(Some(e)),
            }
        });
    };

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
            UndoAction::RowCreate { row_id } => {
                rows.update(|list| list.retain(|r| r.id != row_id));
                selected_row_ids.update(|set| {
                    set.remove(&row_id);
                });
                spawn_local(async move {
                    if let Err(e) = ApiClient::bulk_delete_rows(&auth, gid, vec![row_id]).await {
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
        }
    };

    // Reset Layout (`POST /api/grids/{grid_id}/reset-layout`): resets all column widths to 180px & clears sorts/filters
    let handle_reset_layout = move |_| {
        let gid = grid_id.get_untracked();
        sort_specs.set(Vec::new());
        quick_search.set(String::new());
        server_filter_col.set(None);
        server_filter_val.set(String::new());

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

    // Delete column and strip JSONB key via `DELETE /api/grids/{grid_id}/columns/{col_id}`
    let confirm_delete_column = move |col_id: Uuid| {
        let gid = grid_id.get_untracked();
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

        let filter_q = quick_search.get_untracked().trim().to_lowercase();
        let all_rows = rows.get_untracked();
        let visible_rows: Vec<GridRow> = all_rows
            .iter()
            .filter(|row| {
                if filter_q.is_empty() {
                    return true;
                }
                serde_json::to_string(&row.cells)
                    .unwrap_or_default()
                    .to_lowercase()
                    .contains(&filter_q)
            })
            .cloned()
            .collect();

        let Some(src_row) = visible_rows.iter().find(|r| r.id == src_row_id).cloned() else {
            return;
        };
        let src_cell_val = src_row.cells.get(&src_col_id).cloned();
        let members = space_members.get_untracked();

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

    // Keyboard handler (`Escape` to close preview modal or deselect all, `Ctrl+Z` / `Cmd+Z` to Undo)
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
        if (ev.ctrl_key() || ev.meta_key()) && !ev.shift_key() && key.eq_ignore_ascii_case("z") {
            ev.prevent_default();
            perform_undo();
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
                    push_undo_action(UndoAction::RowCreate {
                        row_id: new_row.id,
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
                        push_undo_action(UndoAction::RowCreate {
                            row_id: new_row.id,
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
                    column_widths.update(|map| {
                        map.insert(col.id, col.width_px.clamp(80, 800));
                    });
                    columns.update(|list| list.push(col));
                    new_col_name.set(String::new());
                    show_add_col_popover.set(false);
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

            let visible_cols: Vec<GridColumn> = columns
                .get_untracked()
                .into_iter()
                .filter(|c| !c.is_hidden)
                .collect();

            let filter_q = quick_search.get_untracked().trim().to_lowercase();
            let visible_rows: Vec<GridRow> = rows
                .get_untracked()
                .into_iter()
                .filter(|row| {
                    if filter_q.is_empty() {
                        return true;
                    }
                    serde_json::to_string(&row.cells)
                        .unwrap_or_default()
                        .to_lowercase()
                        .contains(&filter_q)
                })
                .collect();

            let person_lookup: HashMap<Uuid, String> = space_members
                .get_untracked()
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
    let handle_export_pdf = move |_| {
        show_export_menu.set(false);
        let (title, space_name, visible_cols, visible_rows, person_lookup) = gather_export_state();
        let html_doc = generate_grid_a3_pdf_html(
            &title,
            &space_name,
            &visible_cols,
            &visible_rows,
            &person_lookup,
            Utc::now(),
        );
        trigger_a3_landscape_pdf_print(&html_doc);
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
                        <div class="h-9 w-9 rounded-xl bg-[#06B6D4]/15 border border-[#06B6D4]/40 flex items-center justify-center text-[#06B6D4] font-bold">
                            "⊞"
                        </div>
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
                        // Scope 1: Instant Grid Search Bar
                        <div class="relative flex items-center">
                            <input
                                type="search"
                                placeholder="Filter this grid..."
                                class="w-44 px-3 py-1.5 rounded-lg bg-[#14171A] border border-[#2A2F37] text-xs text-white focus:outline-none focus:border-[#06B6D4]"
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

                        // Conditional Edit Controls: Import, `+ Add Column`, `+ New Row`
                        <Show when=move || auth.can_edit_space(space_id.get())>
                            <button
                                type="button"
                                on:click=move |_| on_open_import.run(space_id.get())
                                class="px-3 py-1.5 rounded-lg bg-[#06B6D4]/15 hover:bg-[#06B6D4]/25 text-[#06B6D4] border border-[#06B6D4]/30 text-xs font-semibold transition"
                            >
                                "⇄ Import"
                            </button>

                            // `+ Add Column` Popover Trigger
                            <div class="relative">
                                <button
                                    type="button"
                                    on:click=move |_| {
                                        show_end_add_col_popover.set(false);
                                        show_add_col_popover.update(|v| *v = !*v);
                                    }
                                    class="px-3 py-1.5 rounded-lg bg-[#14171A] border border-[#2A2F37] hover:border-[#F97316] text-xs font-semibold transition"
                                >
                                    "+ Add Column"
                                </button>

                                <Show when=move || show_add_col_popover.get()>
                                    <div class="absolute right-0 top-10 z-40 w-80 rounded-2xl bg-[#1E2227] border border-[#2A2F37] shadow-2xl p-4 space-y-3">
                                        <div class="flex items-center justify-between border-b border-[#2A2F37] pb-2">
                                            <span class="text-xs font-bold text-white">"Add Typed Grid Column"</span>
                                            <button
                                                type="button"
                                                on:click=move |_| show_add_col_popover.set(false)
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
                                                    placeholder="e.g. Status, Budget, Due Date"
                                                    class="w-full px-2.5 py-1.5 rounded-lg bg-[#14171A] border border-[#2A2F37] text-xs text-white"
                                                    prop:value=new_col_name
                                                    on:input=move |ev| new_col_name.set(event_target_value(&ev))
                                                />
                                            </div>

                                            <div>
                                                <label class="block text-[11px] text-slate-400 mb-1">
                                                    "Property Type (11 Strict Engines)"
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
                                                        "Currency Symbol (optional, e.g. $, EGP, €)"
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
                            </div>

                            // Delete Grid Button with Inline Confirmation Popover (`DELETE /api/grids/{grid_id}`)
                            <div class="relative">
                                <button
                                    type="button"
                                    title="Delete this Grid Page"
                                    on:click=move |_| {
                                        let _ = refresh_trigger;
                                        confirming_delete_grid.update(|v| *v = !*v);
                                    }
                                    class="px-3 py-1.5 rounded-lg bg-red-500/15 hover:bg-red-500/25 text-red-400 border border-red-500/30 text-xs font-semibold flex items-center gap-1 transition"
                                >
                                    <span>"🗑️ Delete Grid"</span>
                                </button>

                                <Show when=move || confirming_delete_grid.get()>
                                    <div
                                        on:click=move |ev| ev.stop_propagation()
                                        class="absolute right-0 top-10 z-50 w-72 rounded-2xl bg-[#1E2227] border border-red-500/50 shadow-2xl p-4 space-y-3"
                                    >
                                        <p class="text-xs font-semibold text-white">
                                            "Delete this Grid Page?"
                                        </p>
                                        <p class="text-[11px] text-slate-400">
                                            "All columns, rows, and 30-day history snapshots in this Grid will be permanently deleted."
                                        </p>
                                        <div class="flex items-center gap-2">
                                            <button
                                                type="button"
                                                on:click=handle_delete_current_grid
                                                class="flex-1 py-1.5 rounded-lg bg-red-600 hover:bg-red-500 text-white text-xs font-bold shadow transition"
                                            >
                                                "Confirm Delete"
                                            </button>
                                            <button
                                                type="button"
                                                on:click=move |_| confirming_delete_grid.set(false)
                                                class="px-3 py-1.5 rounded-lg bg-[#14171A] text-slate-300 hover:text-white border border-[#2A2F37] text-xs"
                                            >
                                                "Cancel"
                                            </button>
                                        </div>
                                    </div>
                                </Show>
                            </div>
                        </Show>
                    </div>
                </header>

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
                                        class="h-3.5 w-3.5 rounded accent-[#F97316] cursor-pointer"
                                        prop:checked=move || {
                                            let all = rows.get();
                                            !all.is_empty() && selected_row_ids.get().len() == all.len()
                                        }
                                        on:change=move |ev| {
                                            let checked = event_target_checked(&ev);
                                            if checked {
                                                let all_ids: HashSet<Uuid> =
                                                    rows.get().into_iter().map(|r| r.id).collect();
                                                selected_row_ids.set(all_ids);
                                            } else {
                                                selected_row_ids.set(HashSet::new());
                                            }
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
                                                show_add_col_popover.set(false);
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
                                                            "Property Type (11 Strict Engines)"
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
                                let can_edit = auth.can_edit_space(space_id.get());
                                let gid = grid_id.get();

                                rows.get()
                                    .into_iter()
                                    .filter(|row| {
                                        if filter_q.is_empty() {
                                            return true;
                                        }
                                        serde_json::to_string(&row.cells)
                                            .unwrap_or_default()
                                            .to_lowercase()
                                            .contains(&filter_q)
                                    })
                                    .enumerate()
                                    .map(|(r_idx, row)| {
                                        let rid = row.id;
                                        let row_pos = if row.position > 0 || r_idx == 0 {
                                            row.position
                                        } else {
                                            r_idx as i32
                                        };
                                        let cols_for_row = cols.clone();
                                        let filter_q_for_row = filter_q.clone();

                                        view! {
                                            <tr
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
                                                        "group bg-[#06B6D4]/10 hover:bg-[#06B6D4]/15 transition-colors"
                                                    } else {
                                                        "group hover:bg-slate-800/40 transition-colors"
                                                    };
                                                    format!("{base_cls} {drop_cls}")
                                                }
                                            >
                                                // Sticky Left Checkbox Cell (40px)
                                                <td
                                                    on:click=move |ev| ev.stop_propagation()
                                                    style="width: 40px;"
                                                    class="sticky left-0 z-10 bg-[#14171A] px-2 py-1 border-r border-[#2A2F37] text-center"
                                                >
                                                    <input
                                                        type="checkbox"
                                                        class="h-3.5 w-3.5 rounded accent-[#06B6D4] cursor-pointer"
                                                        prop:checked=move || selected_row_ids.get().contains(&rid)
                                                        on:change=move |_| {
                                                            selected_row_ids.update(|set| {
                                                                if set.contains(&rid) {
                                                                    set.remove(&rid);
                                                                } else {
                                                                    set.insert(rid);
                                                                }
                                                            });
                                                        }
                                                    />
                                                </td>

                                                // Hover-Revealed `+` Row Insert Button + Six-Dot Drag Handle (`⋮⋮`) + Row Index Number (64px)
                                                <td
                                                    on:click=move |ev| ev.stop_propagation()
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
                                                    title=if can_edit { "Drag grip (⋮⋮) to reorder row, or click (+) to insert row above/below" } else { "" }
                                                    class="px-1 py-1 border-r border-[#2A2F37] text-center font-mono text-[11px] text-slate-500 select-none"
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
                                                                class="opacity-0 group-hover:opacity-100 focus:opacity-100 transition-opacity duration-150 w-5 h-5 rounded flex items-center justify-center text-slate-400 hover:text-cyan-400 hover:bg-slate-700/70 cursor-pointer font-bold text-xs"
                                                            >
                                                                "+"
                                                            </button>
                                                            <span class="text-slate-600 hover:text-[#06B6D4] cursor-grab active:cursor-grabbing text-[10px]">
                                                                "⋮⋮"
                                                            </span>
                                                        </Show>
                                                        <span>{r_idx + 1}</span>
                                                    </div>
                                                </td>

                                                // Dynamic Typed Cells (All 11 Property Types + Strictly Isolated Width + Active Cell + Excel Fill Handle)
                                                {cols_for_row
                                                    .into_iter()
                                                    .filter(|c| !c.is_hidden)
                                                    .enumerate()
                                                    .map(|(c_idx, col)| {
                                                        let cid = col.id;
                                                        let row_clone = row.clone();
                                                        let cell_matches_filter = !filter_q_for_row.is_empty()
                                                            && cell_to_plain_text(
                                                                row.cells.get(&cid),
                                                                &col,
                                                                &space_members.get_untracked(),
                                                                row.last_modified,
                                                            )
                                                            .to_lowercase()
                                                            .contains(&filter_q_for_row);

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
                                                    class="border-r border-[#2A2F37] bg-[#14171A]/30"
                                                />
                                            </tr>
                                        }
                                    })
                                    .collect_view()
                            }}
                        </tbody>
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
                                                            <td class="px-2 py-1 border-r border-[#2A2F37] text-center font-mono text-[11px] text-slate-500">
                                                                {r_idx + 1}
                                                            </td>
                                                            {cols_for_snap_row
                                                                .into_iter()
                                                                .map(|col| {
                                                                    let r_clone = snap_row.clone();
                                                                    view! {
                                                                        <td class="p-0 border-r border-[#2A2F37] align-middle overflow-hidden">
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
            // 4. Floating Bulk Action Bar (`DELETE /api/grids/{grid_id}/rows/bulk`)
            // ================================================================
            <Show when=move || !selected_row_ids.get().is_empty()>
                <div
                    on:click=move |ev| ev.stop_propagation()
                    class="fixed bottom-6 left-1/2 -translate-x-1/2 z-40 px-5 py-3 rounded-2xl bg-[#1E2227] border border-[#06B6D4]/50 shadow-2xl flex items-center gap-5"
                >
                    <span class="text-xs font-semibold text-white">
                        {move || format!("{} rows selected", selected_row_ids.get().len())}
                    </span>
                    <button
                        type="button"
                        on:click=move |_| selected_row_ids.set(HashSet::new())
                        class="text-xs text-slate-400 hover:text-white"
                    >
                        "Clear selection"
                    </button>
                    <Show when=move || auth.can_edit_space(space_id.get())>
                        <button
                            type="button"
                            on:click=handle_bulk_delete
                            class="px-4 py-1.5 rounded-xl bg-red-600 hover:bg-red-500 text-white text-xs font-bold shadow-lg transition"
                        >
                            "Delete Selected Rows"
                        </button>
                    </Show>
                </div>
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
    }
}
