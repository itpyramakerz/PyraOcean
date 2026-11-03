use crate::{api::ApiClient, state::AuthState};
use chrono::{DateTime, NaiveDate, NaiveTime, Utc};
use leptos::{prelude::*, task::spawn_local};
use pyra_ocean_shared::{
    CellValue, ChecklistItem, ColumnPropertyConfig, ColumnPropertyType, FileAttachment, GridColumn,
    GridRow, NumberFormat, RowCellMap, SelectOption, SpaceMemberDetail, TagColor,
    UpdateColumnRequest,
};
use std::collections::HashMap;
use uuid::Uuid;
use wasm_bindgen::{prelude::Closure, JsCast};

pub const EIGHT_TAG_COLORS: [(TagColor, &'static str, &'static str); 8] = [
    (TagColor::Blue, "Blue", "bg-blue-500"),
    (TagColor::Cyan, "Cyan", "bg-cyan-500"),
    (TagColor::Emerald, "Emerald", "bg-emerald-500"),
    (TagColor::Amber, "Amber", "bg-amber-500"),
    (TagColor::Orange, "Orange", "bg-orange-500"),
    (TagColor::Rose, "Rose", "bg-rose-500"),
    (TagColor::Purple, "Purple", "bg-purple-500"),
    (TagColor::Slate, "Slate", "bg-slate-500"),
];

/// Maps `TagColor` variants to Tailwind CSS background/text/border classes.
pub fn tag_color_classes(color: TagColor) -> &'static str {
    match color {
        TagColor::Slate => "bg-slate-500/20 text-slate-300 border-slate-500/40",
        TagColor::Gray => "bg-gray-500/20 text-gray-300 border-gray-500/40",
        TagColor::Red => "bg-red-500/20 text-red-300 border-red-500/40",
        TagColor::Orange => "bg-orange-500/20 text-orange-300 border-orange-500/40",
        TagColor::Amber => "bg-amber-500/20 text-amber-300 border-amber-500/40",
        TagColor::Green => "bg-green-500/20 text-green-300 border-green-500/40",
        TagColor::Emerald => "bg-emerald-500/20 text-emerald-300 border-emerald-500/40",
        TagColor::Teal => "bg-teal-500/20 text-teal-300 border-teal-500/40",
        TagColor::Cyan => "bg-cyan-500/20 text-cyan-300 border-cyan-500/40",
        TagColor::Blue => "bg-blue-500/20 text-blue-300 border-blue-500/40",
        TagColor::Indigo => "bg-indigo-500/20 text-indigo-300 border-indigo-500/40",
        TagColor::Violet => "bg-violet-500/20 text-violet-300 border-violet-500/40",
        TagColor::Purple => "bg-purple-500/20 text-purple-300 border-purple-500/40",
        TagColor::Pink => "bg-pink-500/20 text-pink-300 border-pink-500/40",
        TagColor::Rose => "bg-rose-500/20 text-rose-300 border-rose-500/40",
    }
}

/// Serializes any of the 11 `CellValue` variants into clean, human-readable plain text for one-click Clipboard copy.
pub fn cell_to_plain_text(
    cell: Option<&CellValue>,
    column: &GridColumn,
    members: &[SpaceMemberDetail],
    fallback_last_modified: DateTime<Utc>,
) -> String {
    match (&column.property_type, cell) {
        (ColumnPropertyType::LastModified, Some(CellValue::LastModified(dt))) => {
            dt.format("%Y-%m-%d %H:%M:%S").to_string()
        }
        (ColumnPropertyType::LastModified, _) => {
            fallback_last_modified.format("%Y-%m-%d %H:%M:%S").to_string()
        }
        (_, None) => String::new(),
        (_, Some(CellValue::Text(s))) => s.clone(),
        (_, Some(CellValue::Number(n))) => match &column.property_config {
            ColumnPropertyConfig::Number { format } => match format {
                NumberFormat::Integer => format!("{:.0}", n),
                NumberFormat::Decimal { precision } => {
                    format!("{:.prec$}", n, prec = *precision as usize)
                }
                NumberFormat::Percentage { precision } => {
                    format!("{:.prec$}%", n, prec = *precision as usize)
                }
                NumberFormat::Currency { symbol, precision } => {
                    format!("{symbol} {:.prec$}", n, prec = *precision as usize)
                }
            },
            _ => n.to_string(),
        },
        (_, Some(CellValue::Select(Some(oid)))) => {
            if let ColumnPropertyConfig::Select { options } = &column.property_config {
                options
                    .iter()
                    .find(|o| &o.id == oid)
                    .map(|o| o.label.clone())
                    .unwrap_or_default()
            } else {
                String::new()
            }
        }
        (_, Some(CellValue::Select(None))) => String::new(),
        (_, Some(CellValue::MultiSelect(oids))) => {
            if let ColumnPropertyConfig::MultiSelect { options } = &column.property_config {
                oids.iter()
                    .filter_map(|id| options.iter().find(|o| &o.id == id).map(|o| o.label.clone()))
                    .collect::<Vec<_>>()
                    .join(", ")
            } else {
                String::new()
            }
        }
        (_, Some(CellValue::Date(Some(d)))) => d.format("%Y-%m-%d").to_string(),
        (_, Some(CellValue::Date(None))) => String::new(),
        (
            _,
            Some(CellValue::Time {
                time_of_day: Some(t),
                ..
            }),
        ) => t.format("%H:%M").to_string(),
        (_, Some(CellValue::Time { .. })) => String::new(),
        (_, Some(CellValue::Checkbox(b))) => if *b { "true" } else { "false" }.to_string(),
        (_, Some(CellValue::Checklist(items))) => items
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
        (_, Some(CellValue::Files(files))) => files
            .iter()
            .map(|f| f.storage_url.clone())
            .collect::<Vec<_>>()
            .join(", "),
        (_, Some(CellValue::Person(uids))) => uids
            .iter()
            .map(|uid| {
                members
                    .iter()
                    .find(|m| &m.user_id == uid)
                    .map(|m| m.display_name.clone())
                    .unwrap_or_else(|| uid.to_string())
            })
            .collect::<Vec<_>>()
            .join(", "),
        (_, Some(CellValue::LastModified(dt))) => dt.format("%Y-%m-%d %H:%M:%S").to_string(),
    }
}

/// Universal Rust WASM clipboard helper (`copy_to_clipboard(text: &str) -> bool`).
///
/// 1. First checks if `window.isSecureContext` is true; if so, calls `navigator.clipboard.writeText(text)`.
/// 2. **Fallback for Non-HTTPS LAN IPs (e.g. `http://10.10.10.4:8080`):**
///    Dynamically creates an off-screen `<textarea>`, sets `.value = text`, appends it to
///    `document.body`, calls `.focus()` and `.select()`, executes `document.execCommand("copy")`,
///    and removes the element from the DOM.
pub fn copy_to_clipboard(text: &str) -> bool {
    let Some(window) = web_sys::window() else {
        return false;
    };

    if window.is_secure_context() {
        let clipboard = window.navigator().clipboard();
        let _ = clipboard.write_text(text);
        return true;
    }

    let Some(document) = window.document() else {
        return false;
    };
    let Some(body) = document.body() else {
        return false;
    };
    let Ok(el) = document.create_element("textarea") else {
        return false;
    };
    let Ok(textarea) = el.dyn_into::<web_sys::HtmlTextAreaElement>() else {
        return false;
    };

    textarea.set_value(text);
    let _ = textarea.set_attribute(
        "style",
        "position: fixed; top: -9999px; left: -9999px; opacity: 0; pointer-events: none;",
    );

    if body.append_child(&textarea).is_err() {
        return false;
    }

    let _ = textarea.focus();
    textarea.select();

    let copied_ok = document
        .dyn_ref::<web_sys::HtmlDocument>()
        .and_then(|html_doc| html_doc.exec_command("copy").ok())
        .unwrap_or(false);

    let _ = body.remove_child(&textarea);
    copied_ok
}

/// Computes a viewport-aware `position: fixed` inline style string (`left`, `top` or `bottom`, `z-index: 9999`)
/// from a click event's target element bounding box (`get_bounding_client_rect()`).
///
/// Automatically flips the popover to open **upward** (`bottom: {win_h - rect.top() + 4.0}px`)
/// whenever `rect.bottom() + popover_height > win_h` so popovers on rows near the bottom of the
/// screen are 100% visible and never clipped by any table or scroll container.
pub fn compute_floating_popover_style(
    ev: &web_sys::MouseEvent,
    popover_width: f64,
    popover_height: f64,
) -> String {
    let (win_w, win_h) = web_sys::window()
        .map(|w| {
            let ww = w
                .inner_width()
                .ok()
                .and_then(|v| v.as_f64())
                .unwrap_or(1280.0);
            let wh = w
                .inner_height()
                .ok()
                .and_then(|v| v.as_f64())
                .unwrap_or(800.0);
            (ww, wh)
        })
        .unwrap_or((1280.0, 800.0));

    let (rect_left, rect_top, rect_bottom) = ev
        .current_target()
        .and_then(|t| t.dyn_into::<web_sys::Element>().ok())
        .map(|el| {
            let r = el.get_bounding_client_rect();
            (r.left(), r.top(), r.bottom())
        })
        .unwrap_or_else(|| {
            let cx = ev.client_x() as f64;
            let cy = ev.client_y() as f64;
            (cx, cy - 16.0, cy + 16.0)
        });

    let left = rect_left
        .max(8.0)
        .min((win_w - popover_width - 12.0).max(8.0));

    if rect_bottom + popover_height > win_h && rect_top > popover_height * 0.5 {
        let bottom = (win_h - rect_top + 4.0).max(8.0);
        format!("position: fixed; left: {left:.1}px; bottom: {bottom:.1}px; z-index: 9999;")
    } else {
        let top = (rect_bottom + 4.0).max(8.0);
        format!("position: fixed; left: {left:.1}px; top: {top:.1}px; z-index: 9999;")
    }
}

/// Copies plain text to the browser clipboard (supporting both HTTPS Secure Contexts and
/// plain HTTP LAN IPs via `copy_to_clipboard`) and triggers a 1.5s `✓` feedback state.
fn copy_text_to_clipboard(text: String, copied_signal: RwSignal<bool>) {
    if copy_to_clipboard(&text) {
        copied_signal.set(true);
        if let Some(window) = web_sys::window() {
            let cb = Closure::once(move || {
                copied_signal.set(false);
            });
            let _ = window.set_timeout_with_callback_and_timeout_and_arguments_0(
                cb.as_ref().unchecked_ref(),
                1500,
            );
            cb.forget();
        }
    }
}

/// Renders and edits a single spreadsheet cell for any of the 11 strict `ColumnPropertyType`s,
/// wrapped in a `group relative` container with a hover-to-copy action button.
#[component]
pub fn CellEditor(
    grid_id: Uuid,
    row: GridRow,
    column: GridColumn,
    can_edit: bool,
    space_members: RwSignal<Vec<SpaceMemberDetail>>,
    on_row_updated: Callback<GridRow>,
    on_column_updated: Callback<GridColumn>,
    on_before_cell_edit: Callback<(Uuid, Uuid, Option<CellValue>, CellValue)>,
    on_error: Callback<String>,
) -> impl IntoView {
    let auth = expect_context::<AuthState>();
    let row_id = row.id;
    let col_id = column.id;
    let prop_type = column.property_type;

    let current_cell = row.cells.get(&col_id).cloned();
    let row_last_modified = row.last_modified;
    let row_updated_by = row.updated_by;

    let copied = RwSignal::new(false);
    let col_for_copy = StoredValue::new(column.clone());
    let cell_for_copy = RwSignal::new(current_cell.clone());

    // Helper closure to send `PATCH /api/grids/{grid_id}/rows/{row_id}` with a single updated cell
    let save_cell = Callback::new(move |new_val: CellValue| {
        if !can_edit {
            return;
        }
        let prev_val = cell_for_copy.get_untracked();
        on_before_cell_edit.run((row_id, col_id, prev_val, new_val.clone()));
        cell_for_copy.set(Some(new_val.clone()));
        let mut patch_map: RowCellMap = HashMap::new();
        patch_map.insert(col_id, new_val);

        spawn_local(async move {
            match ApiClient::patch_row(&auth, grid_id, row_id, patch_map).await {
                Ok(updated_row) => on_row_updated.run(updated_row),
                Err(e) => on_error.run(e),
            }
        });
    });

    // Helper closure to persist updated `Select` or `MultiSelect` options via `PATCH /api/grids/{grid_id}/columns/{col_id}`
    let persist_select_options = Callback::new(move |(is_multi, next_opts): (bool, Vec<SelectOption>)| {
        if !can_edit {
            return;
        }
        let cfg = if is_multi {
            ColumnPropertyConfig::MultiSelect { options: next_opts }
        } else {
            ColumnPropertyConfig::Select { options: next_opts }
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
            match ApiClient::update_column(&auth, grid_id, col_id, req).await {
                Ok(updated_col) => on_column_updated.run(updated_col),
                Err(e) => on_error.run(e),
            }
        });
    });

    let inner_editor = match prop_type {
        // ====================================================================
        // 1. `Text`: Inline text input on click/double-click; saves on Enter or Blur
        // ====================================================================
        ColumnPropertyType::Text => {
            let initial_text = match &current_cell {
                Some(CellValue::Text(s)) => s.clone(),
                _ => String::new(),
            };
            let editing = RwSignal::new(false);
            let draft = RwSignal::new(initial_text.clone());
            let committed = RwSignal::new(initial_text);

            let commit_text = move || {
                editing.set(false);
                let next = draft.get();
                if next != committed.get_untracked() {
                    committed.set(next.clone());
                    save_cell.run(CellValue::Text(next));
                }
            };

            view! {
                <div
                    class="w-full h-full min-h-[34px] flex items-center px-2.5 pr-8 py-1 cursor-text"
                    on:click=move |_| {
                        if can_edit && !editing.get() {
                            editing.set(true);
                        }
                    }
                    on:dblclick=move |_| {
                        if can_edit {
                            editing.set(true);
                        }
                    }
                >
                    <Show
                        when=move || editing.get() && can_edit
                        fallback=move || {
                            let val = committed.get();
                            if val.is_empty() {
                                view! { <span class="text-slate-600 text-xs select-none">"—"</span> }.into_any()
                            } else {
                                view! { <span class="text-xs truncate">{val}</span> }.into_any()
                            }
                        }
                    >
                        <input
                            type="text"
                            autofocus
                            class="w-full px-1.5 py-0.5 rounded bg-[#14171A] border border-[#06B6D4] text-xs text-white focus:outline-none"
                            prop:value=draft
                            on:input=move |ev| draft.set(event_target_value(&ev))
                            on:blur=move |_| commit_text()
                            on:keydown=move |ev: web_sys::KeyboardEvent| {
                                if ev.key() == "Enter" {
                                    commit_text();
                                } else if ev.key() == "Escape" {
                                    draft.set(committed.get_untracked());
                                    editing.set(false);
                                }
                            }
                        />
                    </Show>
                </div>
            }
            .into_any()
        }

        // ====================================================================
        // 2. `Number`: Right-aligned numeric formatting with optional currency/precision
        // ====================================================================
        ColumnPropertyType::Number => {
            let initial_num = match &current_cell {
                Some(CellValue::Number(n)) => Some(*n),
                _ => None,
            };
            let number_format = match &column.property_config {
                ColumnPropertyConfig::Number { format } => format.clone(),
                _ => NumberFormat::default(),
            };
            let nf_store = StoredValue::new(number_format);

            let editing = RwSignal::new(false);
            let committed = RwSignal::new(initial_num);
            let draft_str = RwSignal::new(
                initial_num
                    .map(|n| n.to_string())
                    .unwrap_or_default(),
            );

            let commit_num = move || {
                editing.set(false);
                let raw = draft_str.get().trim().replace(',', "");
                if let Ok(parsed) = raw.parse::<f64>() {
                    if parsed.is_finite() && Some(parsed) != committed.get_untracked() {
                        committed.set(Some(parsed));
                        save_cell.run(CellValue::Number(parsed));
                    }
                }
            };

            view! {
                <div
                    class="w-full h-full min-h-[34px] flex items-center justify-end px-2.5 pr-8 py-1 font-mono cursor-text"
                    on:click=move |_| {
                        if can_edit && !editing.get() {
                            editing.set(true);
                        }
                    }
                >
                    <Show
                        when=move || editing.get() && can_edit
                        fallback=move || {
                            match committed.get() {
                                None => view! { <span class="text-slate-600 text-xs">"—"</span> }.into_any(),
                                Some(val) => {
                                    let formatted = match nf_store.get_value() {
                                        NumberFormat::Integer => format!("{:.0}", val),
                                        NumberFormat::Decimal { precision } => {
                                            format!("{:.prec$}", val, prec = precision as usize)
                                        }
                                        NumberFormat::Percentage { precision } => {
                                            format!("{:.prec$}%", val, prec = precision as usize)
                                        }
                                        NumberFormat::Currency { symbol, precision } => {
                                            format!("{symbol} {:.prec$}", val, prec = precision as usize)
                                        }
                                    };
                                    view! { <span class="text-xs text-emerald-400 font-semibold">{formatted}</span> }.into_any()
                                }
                            }
                        }
                    >
                        <input
                            type="number"
                            step="any"
                            autofocus
                            class="w-full text-right px-1.5 py-0.5 rounded bg-[#14171A] border border-[#06B6D4] text-xs text-white focus:outline-none"
                            prop:value=draft_str
                            on:input=move |ev| draft_str.set(event_target_value(&ev))
                            on:blur=move |_| commit_num()
                            on:keydown=move |ev: web_sys::KeyboardEvent| {
                                if ev.key() == "Enter" {
                                    commit_num();
                                } else if ev.key() == "Escape" {
                                    editing.set(false);
                                }
                            }
                        />
                    </Show>
                </div>
            }
            .into_any()
        }

        // ====================================================================
        // 3. `Select`: Colored pill badge + floating option picker with Inline Option CRUD
        // ====================================================================
        ColumnPropertyType::Select => {
            let options: Vec<SelectOption> = match &column.property_config {
                ColumnPropertyConfig::Select { options } => options.clone(),
                _ => Vec::new(),
            };
            let options_sig = RwSignal::new(options);

            let selected_id = RwSignal::new(match &current_cell {
                Some(CellValue::Select(opt)) => *opt,
                _ => None,
            });
            let open_dropdown = RwSignal::new(false);
            let popover_style = RwSignal::new(String::new());

            // Inline Add / Edit Option state
            let new_opt_label = RwSignal::new(String::new());
            let new_opt_color = RwSignal::new(TagColor::Blue);
            let editing_opt_id = RwSignal::new(Option::<Uuid>::None);
            let editing_opt_label = RwSignal::new(String::new());
            let editing_opt_color = RwSignal::new(TagColor::Blue);

            let add_new_option = move || {
                let label = new_opt_label.get_untracked().trim().to_string();
                if label.is_empty() {
                    return;
                }
                let created = SelectOption {
                    id: Uuid::new_v4(),
                    label,
                    color: new_opt_color.get_untracked(),
                };
                let mut next = options_sig.get_untracked();
                next.push(created.clone());
                options_sig.set(next.clone());
                new_opt_label.set(String::new());
                persist_select_options.run((false, next));
                selected_id.set(Some(created.id));
                save_cell.run(CellValue::Select(Some(created.id)));
            };

            let save_edited_option = move |oid: Uuid| {
                let label = editing_opt_label.get_untracked().trim().to_string();
                if label.is_empty() {
                    return;
                }
                let color = editing_opt_color.get_untracked();
                let mut next = options_sig.get_untracked();
                if let Some(item) = next.iter_mut().find(|o| o.id == oid) {
                    item.label = label;
                    item.color = color;
                }
                options_sig.set(next.clone());
                editing_opt_id.set(None);
                persist_select_options.run((false, next));
            };

            let delete_option = move |oid: Uuid| {
                let mut next = options_sig.get_untracked();
                next.retain(|o| o.id != oid);
                options_sig.set(next.clone());
                if selected_id.get_untracked() == Some(oid) {
                    selected_id.set(None);
                    save_cell.run(CellValue::Select(None));
                }
                persist_select_options.run((false, next));
            };

            view! {
                <div class="relative w-full h-full min-h-[34px] flex items-center px-2 pr-8 py-1">
                    <button
                        type="button"
                        on:click=move |ev: web_sys::MouseEvent| {
                            if can_edit {
                                popover_style.set(compute_floating_popover_style(&ev, 260.0, 320.0));
                                open_dropdown.update(|o| *o = !*o);
                            }
                        }
                        class="w-full text-left flex items-center justify-between gap-1"
                    >
                        {move || {
                            let cur = selected_id.get();
                            let found = cur.and_then(|id| {
                                options_sig.get().into_iter().find(|o| o.id == id)
                            });
                            match found {
                                Some(opt) => {
                                    let cls = tag_color_classes(opt.color);
                                    view! {
                                        <span class=format!("px-2 py-0.5 rounded-full border text-[11px] font-medium truncate {cls}")>
                                            {opt.label}
                                        </span>
                                    }
                                    .into_any()
                                }
                                None => {
                                    view! {
                                        <span class="text-slate-500 text-[11px]">"Select..."</span>
                                    }
                                    .into_any()
                                }
                            }
                        }}
                    </button>

                    <Show when=move || open_dropdown.get() && can_edit>
                        <div
                            style=move || popover_style.get()
                            on:click=move |ev| ev.stop_propagation()
                            class="fixed z-[9999] w-64 max-h-80 overflow-y-auto shadow-2xl border border-slate-700 bg-slate-900 rounded-lg p-2.5 space-y-2"
                        >
                            <div class="flex items-center justify-between px-1 pb-1 border-b border-[#2A2F37] text-[10px] text-slate-400">
                                <span>"Select Option"</span>
                                <div class="flex items-center gap-2">
                                    <button
                                        type="button"
                                        on:click=move |_| {
                                            selected_id.set(None);
                                            open_dropdown.set(false);
                                            save_cell.run(CellValue::Select(None));
                                        }
                                        class="text-[10px] text-slate-400 hover:text-white"
                                    >
                                        "Clear"
                                    </button>
                                    <button
                                        type="button"
                                        on:click=move |_| open_dropdown.set(false)
                                        class="text-slate-400 hover:text-white font-bold"
                                    >
                                        "✕"
                                    </button>
                                </div>
                            </div>

                            <div class="max-h-44 overflow-y-auto space-y-1">
                                {move || {
                                    options_sig
                                        .get()
                                        .into_iter()
                                        .map(|opt| {
                                            let oid = opt.id;
                                            let olabel = opt.label.clone();
                                            let olabel_edit = opt.label.clone();
                                            let ocolor = opt.color;
                                            let cls = tag_color_classes(ocolor);

                                            view! {
                                                <div>
                                                    <Show
                                                        when=move || editing_opt_id.get() == Some(oid)
                                                        fallback=move || {
                                                            let lbl_for_btn = olabel.clone();
                                                            let lbl_for_edit = olabel_edit.clone();
                                                            view! {
                                                                <div class="flex items-center justify-between gap-1 px-1 py-0.5 rounded hover:bg-slate-800/80">
                                                                    <button
                                                                        type="button"
                                                                        on:click=move |_| {
                                                                            selected_id.set(Some(oid));
                                                                            open_dropdown.set(false);
                                                                            save_cell.run(CellValue::Select(Some(oid)));
                                                                        }
                                                                        class="flex-1 text-left truncate py-0.5"
                                                                    >
                                                                        <span class=format!("px-2 py-0.5 rounded-full border text-[11px] font-medium {cls}")>
                                                                            {lbl_for_btn}
                                                                        </span>
                                                                    </button>
                                                                    <div class="flex items-center gap-1 shrink-0">
                                                                        <button
                                                                            type="button"
                                                                            title="Edit option label & color"
                                                                            on:click=move |ev| {
                                                                                ev.stop_propagation();
                                                                                editing_opt_label.set(lbl_for_edit.clone());
                                                                                editing_opt_color.set(ocolor);
                                                                                editing_opt_id.set(Some(oid));
                                                                            }
                                                                            class="px-1 text-[10px] text-slate-400 hover:text-[#06B6D4]"
                                                                        >
                                                                            "✎"
                                                                        </button>
                                                                        <button
                                                                            type="button"
                                                                            title="Delete option"
                                                                            on:click=move |ev| {
                                                                                ev.stop_propagation();
                                                                                delete_option(oid);
                                                                            }
                                                                            class="px-1 text-[11px] text-slate-500 hover:text-red-400 font-bold"
                                                                        >
                                                                            "×"
                                                                        </button>
                                                                    </div>
                                                                </div>
                                                            }
                                                        }
                                                    >
                                                        <div class="p-1.5 rounded-lg bg-[#14171A] border border-[#2A2F37] space-y-1.5">
                                                            <div class="flex items-center gap-1">
                                                                <input
                                                                    type="text"
                                                                    class="flex-1 px-2 py-1 rounded bg-[#1E2227] border border-[#2A2F37] text-[11px] text-white focus:outline-none focus:border-[#06B6D4]"
                                                                    prop:value=editing_opt_label
                                                                    on:input=move |ev| editing_opt_label.set(event_target_value(&ev))
                                                                    on:keydown=move |ev: web_sys::KeyboardEvent| {
                                                                        if ev.key() == "Enter" {
                                                                            ev.prevent_default();
                                                                            save_edited_option(oid);
                                                                        }
                                                                    }
                                                                />
                                                                <button
                                                                    type="button"
                                                                    on:click=move |_| save_edited_option(oid)
                                                                    class="px-2 py-1 rounded bg-[#06B6D4] text-slate-950 font-bold text-[10px]"
                                                                >
                                                                    "Save"
                                                                </button>
                                                            </div>
                                                            <div class="flex items-center gap-1">
                                                                {EIGHT_TAG_COLORS
                                                                    .into_iter()
                                                                    .map(|(tc, tc_name, dot_cls)| {
                                                                        view! {
                                                                            <button
                                                                                type="button"
                                                                                title=tc_name
                                                                                on:click=move |_| editing_opt_color.set(tc)
                                                                                class=move || {
                                                                                    let ring = if editing_opt_color.get() == tc {
                                                                                        "ring-2 ring-white scale-110"
                                                                                    } else {
                                                                                        "opacity-70 hover:opacity-100"
                                                                                    };
                                                                                    format!("h-3.5 w-3.5 rounded-full transition {dot_cls} {ring}")
                                                                                }
                                                                            />
                                                                        }
                                                                    })
                                                                    .collect_view()}
                                                            </div>
                                                        </div>
                                                    </Show>
                                                </div>
                                            }
                                        })
                                        .collect_view()
                                }}
                            </div>

                            // Inline `+ Add option...` + 8-Color Tag Picker
                            <div class="pt-1.5 border-t border-[#2A2F37] space-y-1.5">
                                <div class="flex items-center gap-1">
                                    <input
                                        type="text"
                                        placeholder="+ Add option..."
                                        class="flex-1 px-2 py-1 rounded bg-[#14171A] border border-[#2A2F37] text-[11px] text-white focus:outline-none focus:border-[#06B6D4]"
                                        prop:value=new_opt_label
                                        on:input=move |ev| new_opt_label.set(event_target_value(&ev))
                                        on:keydown=move |ev: web_sys::KeyboardEvent| {
                                            if ev.key() == "Enter" {
                                                ev.prevent_default();
                                                add_new_option();
                                            }
                                        }
                                    />
                                    <button
                                        type="button"
                                        on:click=move |_| add_new_option()
                                        class="px-2 py-1 rounded bg-[#F97316] text-white font-bold text-[10px]"
                                    >
                                        "Add"
                                    </button>
                                </div>
                                <div class="flex items-center gap-1.5 px-0.5">
                                    {EIGHT_TAG_COLORS
                                        .into_iter()
                                        .map(|(tc, tc_name, dot_cls)| {
                                            view! {
                                                <button
                                                    type="button"
                                                    title=tc_name
                                                    on:click=move |_| new_opt_color.set(tc)
                                                    class=move || {
                                                        let ring = if new_opt_color.get() == tc {
                                                            "ring-2 ring-white scale-110"
                                                        } else {
                                                            "opacity-65 hover:opacity-100"
                                                        };
                                                        format!("h-3.5 w-3.5 rounded-full transition {dot_cls} {ring}")
                                                    }
                                                />
                                            }
                                        })
                                        .collect_view()}
                                </div>
                            </div>
                        </div>
                    </Show>
                </div>
            }
            .into_any()
        }

        // ====================================================================
        // 4. `MultiSelect`: Colored pills with `×` remove + `+` toggle popover & Inline Option CRUD
        // ====================================================================
        ColumnPropertyType::MultiSelect => {
            let options: Vec<SelectOption> = match &column.property_config {
                ColumnPropertyConfig::MultiSelect { options } => options.clone(),
                _ => Vec::new(),
            };
            let options_sig = RwSignal::new(options);

            let selected_ids = RwSignal::new(match &current_cell {
                Some(CellValue::MultiSelect(ids)) => ids.clone(),
                _ => Vec::new(),
            });
            let open_popover = RwSignal::new(false);
            let popover_style = RwSignal::new(String::new());

            // Inline Add / Edit Option state
            let new_opt_label = RwSignal::new(String::new());
            let new_opt_color = RwSignal::new(TagColor::Cyan);
            let editing_opt_id = RwSignal::new(Option::<Uuid>::None);
            let editing_opt_label = RwSignal::new(String::new());
            let editing_opt_color = RwSignal::new(TagColor::Cyan);

            let toggle_option = move |oid: Uuid| {
                if !can_edit {
                    return;
                }
                let mut next = selected_ids.get();
                if let Some(pos) = next.iter().position(|&x| x == oid) {
                    next.remove(pos);
                } else {
                    next.push(oid);
                }
                selected_ids.set(next.clone());
                save_cell.run(CellValue::MultiSelect(next));
            };

            let add_new_multi_option = move || {
                let label = new_opt_label.get_untracked().trim().to_string();
                if label.is_empty() {
                    return;
                }
                let created = SelectOption {
                    id: Uuid::new_v4(),
                    label,
                    color: new_opt_color.get_untracked(),
                };
                let mut next_opts = options_sig.get_untracked();
                next_opts.push(created.clone());
                options_sig.set(next_opts.clone());
                new_opt_label.set(String::new());
                persist_select_options.run((true, next_opts));

                let mut next_sel = selected_ids.get_untracked();
                next_sel.push(created.id);
                selected_ids.set(next_sel.clone());
                save_cell.run(CellValue::MultiSelect(next_sel));
            };

            let save_edited_multi_option = move |oid: Uuid| {
                let label = editing_opt_label.get_untracked().trim().to_string();
                if label.is_empty() {
                    return;
                }
                let color = editing_opt_color.get_untracked();
                let mut next = options_sig.get_untracked();
                if let Some(item) = next.iter_mut().find(|o| o.id == oid) {
                    item.label = label;
                    item.color = color;
                }
                options_sig.set(next.clone());
                editing_opt_id.set(None);
                persist_select_options.run((true, next));
            };

            let delete_multi_option = move |oid: Uuid| {
                let mut next = options_sig.get_untracked();
                next.retain(|o| o.id != oid);
                options_sig.set(next.clone());

                let mut sel = selected_ids.get_untracked();
                let before = sel.len();
                sel.retain(|&id| id != oid);
                if sel.len() != before {
                    selected_ids.set(sel.clone());
                    save_cell.run(CellValue::MultiSelect(sel));
                }
                persist_select_options.run((true, next));
            };

            view! {
                <div class="relative w-full h-full min-h-[34px] flex items-center flex-wrap gap-1 px-2 pr-8 py-1">
                    {move || {
                        let opts = options_sig.get();
                        selected_ids
                            .get()
                            .into_iter()
                            .filter_map(|id| opts.iter().find(|o| o.id == id).cloned())
                            .map(|opt| {
                                let oid = opt.id;
                                let cls = tag_color_classes(opt.color);
                                view! {
                                    <span class=format!("inline-flex items-center gap-1 px-2 py-0.5 rounded-full border text-[11px] font-medium {cls}")>
                                        <span>{opt.label}</span>
                                        <Show when=move || can_edit>
                                            <button
                                                type="button"
                                                on:click=move |ev| {
                                                    ev.stop_propagation();
                                                    toggle_option(oid);
                                                }
                                                class="hover:text-white font-bold"
                                            >
                                                "×"
                                            </button>
                                        </Show>
                                    </span>
                                }
                            })
                            .collect_view()
                    }}

                    <Show when=move || can_edit>
                        <button
                            type="button"
                            on:click=move |ev: web_sys::MouseEvent| {
                                popover_style.set(compute_floating_popover_style(&ev, 260.0, 320.0));
                                open_popover.update(|o| *o = !*o);
                            }
                            class="px-1.5 py-0.5 rounded bg-slate-800 hover:bg-slate-700 text-[10px] text-slate-300 border border-[#2A2F37]"
                        >
                            "+"
                        </button>
                    </Show>

                    <Show when=move || open_popover.get() && can_edit>
                        <div
                            style=move || popover_style.get()
                            on:click=move |ev| ev.stop_propagation()
                            class="fixed z-[9999] w-64 max-h-80 overflow-y-auto shadow-2xl border border-slate-700 bg-slate-900 rounded-lg p-2.5 space-y-2"
                        >
                            <div class="flex items-center justify-between px-1 pb-1 border-b border-[#2A2F37] text-[10px] text-slate-400">
                                <span>"Multi-Select Tags"</span>
                                <button
                                    type="button"
                                    on:click=move |_| open_popover.set(false)
                                    class="text-slate-400 hover:text-white font-bold"
                                >
                                    "✕"
                                </button>
                            </div>
                            <div class="max-h-44 overflow-y-auto space-y-1">
                                {move || {
                                    let current = selected_ids.get();
                                    options_sig
                                        .get()
                                        .into_iter()
                                        .map(|opt| {
                                            let oid = opt.id;
                                            let olabel = opt.label.clone();
                                            let olabel_edit = opt.label.clone();
                                            let ocolor = opt.color;
                                            let is_checked = current.contains(&oid);
                                            let cls = tag_color_classes(ocolor);

                                            view! {
                                                <div>
                                                    <Show
                                                        when=move || editing_opt_id.get() == Some(oid)
                                                        fallback=move || {
                                                            let lbl_btn = olabel.clone();
                                                            let lbl_edit = olabel_edit.clone();
                                                            view! {
                                                                <div class="flex items-center justify-between gap-1 px-1 py-0.5 rounded hover:bg-slate-800/80">
                                                                    <button
                                                                        type="button"
                                                                        on:click=move |_| toggle_option(oid)
                                                                        class="flex-1 text-left flex items-center justify-between pr-1 py-0.5 truncate"
                                                                    >
                                                                        <span class=format!("px-2 py-0.5 rounded-full border text-[11px] font-medium {cls}")>
                                                                            {lbl_btn}
                                                                        </span>
                                                                        <span class="text-xs text-[#06B6D4]">
                                                                            {if is_checked { "✓" } else { "" }}
                                                                        </span>
                                                                    </button>
                                                                    <div class="flex items-center gap-1 shrink-0">
                                                                        <button
                                                                            type="button"
                                                                            title="Edit option label & color"
                                                                            on:click=move |ev| {
                                                                                ev.stop_propagation();
                                                                                editing_opt_label.set(lbl_edit.clone());
                                                                                editing_opt_color.set(ocolor);
                                                                                editing_opt_id.set(Some(oid));
                                                                            }
                                                                            class="px-1 text-[10px] text-slate-400 hover:text-[#06B6D4]"
                                                                        >
                                                                            "✎"
                                                                        </button>
                                                                        <button
                                                                            type="button"
                                                                            title="Delete option"
                                                                            on:click=move |ev| {
                                                                                ev.stop_propagation();
                                                                                delete_multi_option(oid);
                                                                            }
                                                                            class="px-1 text-[11px] text-slate-500 hover:text-red-400 font-bold"
                                                                        >
                                                                            "×"
                                                                        </button>
                                                                    </div>
                                                                </div>
                                                            }
                                                        }
                                                    >
                                                        <div class="p-1.5 rounded-lg bg-[#14171A] border border-[#2A2F37] space-y-1.5">
                                                            <div class="flex items-center gap-1">
                                                                <input
                                                                    type="text"
                                                                    class="flex-1 px-2 py-1 rounded bg-[#1E2227] border border-[#2A2F37] text-[11px] text-white focus:outline-none focus:border-[#06B6D4]"
                                                                    prop:value=editing_opt_label
                                                                    on:input=move |ev| editing_opt_label.set(event_target_value(&ev))
                                                                    on:keydown=move |ev: web_sys::KeyboardEvent| {
                                                                        if ev.key() == "Enter" {
                                                                            ev.prevent_default();
                                                                            save_edited_multi_option(oid);
                                                                        }
                                                                    }
                                                                />
                                                                <button
                                                                    type="button"
                                                                    on:click=move |_| save_edited_multi_option(oid)
                                                                    class="px-2 py-1 rounded bg-[#06B6D4] text-slate-950 font-bold text-[10px]"
                                                                >
                                                                    "Save"
                                                                </button>
                                                            </div>
                                                            <div class="flex items-center gap-1">
                                                                {EIGHT_TAG_COLORS
                                                                    .into_iter()
                                                                    .map(|(tc, tc_name, dot_cls)| {
                                                                        view! {
                                                                            <button
                                                                                type="button"
                                                                                title=tc_name
                                                                                on:click=move |_| editing_opt_color.set(tc)
                                                                                class=move || {
                                                                                    let ring = if editing_opt_color.get() == tc {
                                                                                        "ring-2 ring-white scale-110"
                                                                                    } else {
                                                                                        "opacity-70 hover:opacity-100"
                                                                                    };
                                                                                    format!("h-3.5 w-3.5 rounded-full transition {dot_cls} {ring}")
                                                                                }
                                                                            />
                                                                        }
                                                                    })
                                                                    .collect_view()}
                                                            </div>
                                                        </div>
                                                    </Show>
                                                </div>
                                            }
                                        })
                                        .collect_view()
                                }}
                            </div>

                            // Inline `+ Add option...` + 8-Color Tag Picker
                            <div class="pt-1.5 border-t border-[#2A2F37] space-y-1.5">
                                <div class="flex items-center gap-1">
                                    <input
                                        type="text"
                                        placeholder="+ Add option..."
                                        class="flex-1 px-2 py-1 rounded bg-[#14171A] border border-[#2A2F37] text-[11px] text-white focus:outline-none focus:border-[#06B6D4]"
                                        prop:value=new_opt_label
                                        on:input=move |ev| new_opt_label.set(event_target_value(&ev))
                                        on:keydown=move |ev: web_sys::KeyboardEvent| {
                                            if ev.key() == "Enter" {
                                                ev.prevent_default();
                                                add_new_multi_option();
                                            }
                                        }
                                    />
                                    <button
                                        type="button"
                                        on:click=move |_| add_new_multi_option()
                                        class="px-2 py-1 rounded bg-[#F97316] text-white font-bold text-[10px]"
                                    >
                                        "Add"
                                    </button>
                                </div>
                                <div class="flex items-center gap-1.5 px-0.5">
                                    {EIGHT_TAG_COLORS
                                        .into_iter()
                                        .map(|(tc, tc_name, dot_cls)| {
                                            view! {
                                                <button
                                                    type="button"
                                                    title=tc_name
                                                    on:click=move |_| new_opt_color.set(tc)
                                                    class=move || {
                                                        let ring = if new_opt_color.get() == tc {
                                                            "ring-2 ring-white scale-110"
                                                        } else {
                                                            "opacity-65 hover:opacity-100"
                                                        };
                                                        format!("h-3.5 w-3.5 rounded-full transition {dot_cls} {ring}")
                                                    }
                                                />
                                            }
                                        })
                                        .collect_view()}
                                </div>
                            </div>
                        </div>
                    </Show>
                </div>
            }
            .into_any()
        }

        // ====================================================================
        // 5. `Date`: Native HTML5 `<input type="date">` (ISO-8601)
        // ====================================================================
        ColumnPropertyType::Date => {
            let initial_date = match &current_cell {
                Some(CellValue::Date(Some(d))) => d.format("%Y-%m-%d").to_string(),
                _ => String::new(),
            };
            let date_str = RwSignal::new(initial_date);

            view! {
                <div class="w-full h-full min-h-[34px] flex items-center px-2 pr-8 py-1">
                    <input
                        type="date"
                        disabled=!can_edit
                        class="w-full bg-transparent text-xs font-mono focus:outline-none focus:text-[#06B6D4] cursor-pointer"
                        prop:value=date_str
                        on:change=move |ev| {
                            let v = event_target_value(&ev);
                            date_str.set(v.clone());
                            let parsed = NaiveDate::parse_from_str(v.trim(), "%Y-%m-%d").ok();
                            save_cell.run(CellValue::Date(parsed));
                        }
                    />
                </div>
            }
            .into_any()
        }

        // ====================================================================
        // 6. `Time`: Native HTML5 `<input type="time">`
        // ====================================================================
        ColumnPropertyType::Time => {
            let initial_time = match &current_cell {
                Some(CellValue::Time {
                    time_of_day: Some(t),
                    ..
                }) => t.format("%H:%M").to_string(),
                _ => String::new(),
            };
            let time_str = RwSignal::new(initial_time);

            view! {
                <div class="w-full h-full min-h-[34px] flex items-center px-2 pr-8 py-1">
                    <input
                        type="time"
                        disabled=!can_edit
                        class="w-full bg-transparent text-xs font-mono focus:outline-none focus:text-[#06B6D4] cursor-pointer"
                        prop:value=time_str
                        on:change=move |ev| {
                            let v = event_target_value(&ev);
                            time_str.set(v.clone());
                            let parsed = NaiveTime::parse_from_str(v.trim(), "%H:%M:%S")
                                .or_else(|_| NaiveTime::parse_from_str(v.trim(), "%H:%M"))
                                .ok();
                            save_cell.run(CellValue::Time {
                                time_of_day: parsed,
                                duration_seconds: None,
                            });
                        }
                    />
                </div>
            }
            .into_any()
        }

        // ====================================================================
        // 7. `Checkbox`: Centered interactive boolean checkbox
        // ====================================================================
        ColumnPropertyType::Checkbox => {
            let checked = RwSignal::new(match &current_cell {
                Some(CellValue::Checkbox(b)) => *b,
                _ => false,
            });

            view! {
                <div class="w-full h-full min-h-[34px] flex items-center justify-center pr-6">
                    <input
                        type="checkbox"
                        disabled=!can_edit
                        class="h-4 w-4 rounded accent-[#F97316] cursor-pointer"
                        prop:checked=checked
                        on:change=move |ev| {
                            let next = event_target_checked(&ev);
                            checked.set(next);
                            save_cell.run(CellValue::Checkbox(next));
                        }
                    />
                </div>
            }
            .into_any()
        }

        // ====================================================================
        // 8. `Checklist`: Progress bar + counter (`3/5 completed`) + Interactive Popover
        // ====================================================================
        ColumnPropertyType::Checklist => {
            let items = RwSignal::new(match &current_cell {
                Some(CellValue::Checklist(list)) => list.clone(),
                _ => Vec::new(),
            });
            let open_checklist = RwSignal::new(false);
            let checklist_popover_style = RwSignal::new(String::new());
            let new_item_text = RwSignal::new(String::new());

            let toggle_subitem = move |item_id: Uuid| {
                if !can_edit {
                    return;
                }
                let mut list = items.get();
                if let Some(it) = list.iter_mut().find(|i| i.id == item_id) {
                    it.completed = !it.completed;
                }
                items.set(list.clone());
                save_cell.run(CellValue::Checklist(list));
            };

            let remove_subitem = move |item_id: Uuid| {
                if !can_edit {
                    return;
                }
                let mut list = items.get();
                list.retain(|i| i.id != item_id);
                items.set(list.clone());
                save_cell.run(CellValue::Checklist(list));
            };

            let add_subitem = move |ev: web_sys::SubmitEvent| {
                ev.prevent_default();
                if !can_edit {
                    return;
                }
                let txt = new_item_text.get().trim().to_string();
                if txt.is_empty() {
                    return;
                }
                let mut list = items.get();
                list.push(ChecklistItem {
                    id: Uuid::new_v4(),
                    item: txt,
                    completed: false,
                });
                new_item_text.set(String::new());
                items.set(list.clone());
                save_cell.run(CellValue::Checklist(list));
            };

            view! {
                <div class="relative w-full h-full min-h-[34px] flex items-center px-2.5 pr-8 py-1">
                    <button
                        type="button"
                        on:click=move |ev: web_sys::MouseEvent| {
                            checklist_popover_style.set(compute_floating_popover_style(&ev, 264.0, 320.0));
                            open_checklist.update(|o| *o = !*o);
                        }
                        class="w-full flex items-center gap-2 text-left"
                    >
                        {move || {
                            let list = items.get();
                            let total = list.len();
                            let done = list.iter().filter(|i| i.completed).count();
                            let pct = if total == 0 {
                                0
                            } else {
                                ((done as f32 / total as f32) * 100.0).round() as u32
                            };

                            view! {
                                <div class="flex-1 bg-slate-800 h-2 rounded-full overflow-hidden">
                                    <div
                                        class="bg-gradient-to-r from-[#F97316] to-[#06B6D4] h-full transition-all"
                                        style=format!("width: {pct}%")
                                    />
                                </div>
                                <span class="text-[11px] font-mono text-slate-300 shrink-0">
                                    {format!("{done}/{total}")}
                                </span>
                            }
                        }}
                    </button>

                    <Show when=move || open_checklist.get()>
                        <div
                            style=move || checklist_popover_style.get()
                            on:click=move |ev| ev.stop_propagation()
                            class="fixed z-[9999] w-64 max-h-80 overflow-y-auto shadow-2xl border border-slate-700 bg-slate-900 rounded-lg p-3 space-y-2"
                        >
                            <div class="flex items-center justify-between border-b border-[#2A2F37] pb-1.5">
                                <span class="text-xs font-bold text-white">"Checklist Items"</span>
                                <button
                                    type="button"
                                    on:click=move |_| open_checklist.set(false)
                                    class="text-xs text-slate-400 hover:text-white"
                                >
                                    "✕"
                                </button>
                            </div>

                            <div class="max-h-44 overflow-y-auto space-y-1.5">
                                {move || {
                                    items
                                        .get()
                                        .into_iter()
                                        .map(|sub| {
                                            let sid = sub.id;
                                            let completed = sub.completed;
                                            view! {
                                                <div class="flex items-center justify-between gap-2 text-xs">
                                                    <label class="flex items-center gap-2 truncate cursor-pointer">
                                                        <input
                                                            type="checkbox"
                                                            disabled=!can_edit
                                                            class="rounded accent-[#06B6D4]"
                                                            prop:checked=completed
                                                            on:change=move |_| toggle_subitem(sid)
                                                        />
                                                        <span class=if completed { "line-through text-slate-500 truncate" } else { "text-slate-200 truncate" }>
                                                            {sub.item}
                                                        </span>
                                                    </label>
                                                    <Show when=move || can_edit>
                                                        <button
                                                            type="button"
                                                            on:click=move |_| remove_subitem(sid)
                                                            class="text-slate-500 hover:text-red-400 font-bold"
                                                        >
                                                            "×"
                                                        </button>
                                                    </Show>
                                                </div>
                                            }
                                        })
                                        .collect_view()
                                }}
                            </div>

                            <Show when=move || can_edit>
                                <form class="flex items-center gap-1 pt-1" on:submit=add_subitem>
                                    <input
                                        type="text"
                                        placeholder="New checklist item..."
                                        class="flex-1 px-2 py-1 rounded bg-[#14171A] border border-[#2A2F37] text-xs text-white focus:outline-none focus:border-[#06B6D4]"
                                        prop:value=new_item_text
                                        on:input=move |ev| new_item_text.set(event_target_value(&ev))
                                    />
                                    <button
                                        type="submit"
                                        class="px-2 py-1 rounded bg-[#F97316] text-white text-xs font-semibold"
                                    >
                                        "Add"
                                    </button>
                                </form>
                            </Show>
                        </div>
                    </Show>
                </div>
            }
            .into_any()
        }

        // ====================================================================
        // 9. `Files`: Clickable file chips + `+ Upload` multipart file button
        // ====================================================================
        ColumnPropertyType::Files => {
            let attachments = RwSignal::new(match &current_cell {
                Some(CellValue::Files(files)) => files.clone(),
                _ => Vec::new(),
            });
            let uploading = RwSignal::new(false);
            let file_input_ref = NodeRef::<leptos::html::Input>::new();

            let on_files_selected = move |ev: web_sys::Event| {
                let Some(target) = ev.target() else {
                    return;
                };
                let Ok(input) = target.dyn_into::<web_sys::HtmlInputElement>() else {
                    return;
                };
                let Some(file_list) = input.files() else {
                    return;
                };

                let mut selected_files: Vec<web_sys::File> = Vec::new();
                for idx in 0..file_list.length() {
                    if let Some(f) = file_list.get(idx) {
                        selected_files.push(f);
                    }
                }
                if selected_files.is_empty() {
                    return;
                }

                uploading.set(true);
                spawn_local(async move {
                    match ApiClient::upload_cell_files(&auth, grid_id, row_id, col_id, selected_files).await {
                        Ok(updated_row) => {
                            if let Some(CellValue::Files(new_files)) = updated_row.cells.get(&col_id) {
                                attachments.set(new_files.clone());
                                cell_for_copy.set(Some(CellValue::Files(new_files.clone())));
                            }
                            uploading.set(false);
                            on_row_updated.run(updated_row);
                        }
                        Err(err) => {
                            uploading.set(false);
                            on_error.run(err);
                        }
                    }
                });
            };

            view! {
                <div class="w-full h-full min-h-[34px] flex items-center flex-wrap gap-1 px-2 pr-8 py-1">
                    {move || {
                        attachments
                            .get()
                            .into_iter()
                            .map(|att: FileAttachment| {
                                view! {
                                    <a
                                        href=att.storage_url
                                        target="_blank"
                                        rel="noopener noreferrer"
                                        class="inline-flex items-center gap-1 px-2 py-0.5 rounded bg-[#06B6D4]/15 hover:bg-[#06B6D4]/25 text-[#06B6D4] border border-[#06B6D4]/30 text-[11px] font-medium max-w-[150px] truncate"
                                    >
                                        <span>"📎"</span>
                                        <span class="truncate">{att.filename}</span>
                                    </a>
                                }
                            })
                            .collect_view()
                    }}

                    <Show when=move || can_edit>
                        <input
                            node_ref=file_input_ref
                            type="file"
                            multiple
                            class="hidden"
                            on:change=on_files_selected
                        />
                        <button
                            type="button"
                            disabled=move || uploading.get()
                            on:click=move |_| {
                                if let Some(input) = file_input_ref.get() {
                                    input.click();
                                }
                            }
                            class="px-1.5 py-0.5 rounded bg-slate-800 hover:bg-slate-700 text-[10px] text-slate-300 border border-[#2A2F37]"
                        >
                            {move || if uploading.get() { "..." } else { "+ Upload" }}
                        </button>
                    </Show>
                </div>
            }
            .into_any()
        }

        // ====================================================================
        // 10. `Person`: User avatar initials + name badge + Space Member selector
        // ====================================================================
        ColumnPropertyType::Person => {
            let assigned_ids = RwSignal::new(match &current_cell {
                Some(CellValue::Person(uids)) => uids.clone(),
                _ => Vec::new(),
            });
            let open_person_picker = RwSignal::new(false);
            let person_popover_style = RwSignal::new(String::new());

            let toggle_person = move |uid: Uuid| {
                if !can_edit {
                    return;
                }
                let mut current = assigned_ids.get();
                if let Some(pos) = current.iter().position(|&x| x == uid) {
                    current.remove(pos);
                } else {
                    current.push(uid);
                }
                assigned_ids.set(current.clone());
                save_cell.run(CellValue::Person(current));
            };

            view! {
                <div class="relative w-full h-full min-h-[34px] flex items-center flex-wrap gap-1 px-2 pr-8 py-1">
                    {move || {
                        let members = space_members.get();
                        assigned_ids
                            .get()
                            .into_iter()
                            .map(|uid| {
                                let label = members
                                    .iter()
                                    .find(|m| m.user_id == uid)
                                    .map(|m| m.display_name.clone())
                                    .unwrap_or_else(|| format!("User {}", &uid.to_string()[..4]));
                                let initials: String = label
                                    .split_whitespace()
                                    .filter_map(|w| w.chars().next())
                                    .take(2)
                                    .collect::<String>()
                                    .to_uppercase();

                                view! {
                                    <span class="inline-flex items-center gap-1.5 px-2 py-0.5 rounded-full bg-indigo-500/15 border border-indigo-500/30 text-[11px] text-indigo-300">
                                        <span class="h-4 w-4 rounded-full bg-[#F97316] text-white text-[9px] font-bold flex items-center justify-center">
                                            {initials}
                                        </span>
                                        <span class="truncate max-w-[100px]">{label}</span>
                                    </span>
                                }
                            })
                            .collect_view()
                    }}

                    <Show when=move || can_edit>
                        <button
                            type="button"
                            on:click=move |ev: web_sys::MouseEvent| {
                                person_popover_style.set(compute_floating_popover_style(&ev, 232.0, 320.0));
                                open_person_picker.update(|o| *o = !*o);
                            }
                            class="px-1.5 py-0.5 rounded bg-slate-800 hover:bg-slate-700 text-[10px] text-slate-300 border border-[#2A2F37]"
                        >
                            "+ 👤"
                        </button>
                    </Show>

                    <Show when=move || open_person_picker.get() && can_edit>
                        <div
                            style=move || person_popover_style.get()
                            on:click=move |ev| ev.stop_propagation()
                            class="fixed z-[9999] w-56 max-h-80 overflow-y-auto shadow-2xl border border-slate-700 bg-slate-900 rounded-lg p-2.5 space-y-1"
                        >
                            <div class="flex items-center justify-between px-1 pb-1 border-b border-[#2A2F37] text-[10px] text-slate-400">
                                <span>"Assign Space Members"</span>
                                <button
                                    type="button"
                                    on:click=move |_| open_person_picker.set(false)
                                    class="text-slate-400 hover:text-white font-bold"
                                >
                                    "✕"
                                </button>
                            </div>
                            {move || {
                                let current = assigned_ids.get();
                                space_members
                                    .get()
                                    .into_iter()
                                    .map(|m| {
                                        let uid = m.user_id;
                                        let is_selected = current.contains(&uid);
                                        view! {
                                            <button
                                                type="button"
                                                on:click=move |_| toggle_person(uid)
                                                class="w-full text-left px-2 py-1.5 rounded hover:bg-slate-800 flex items-center justify-between text-xs"
                                            >
                                                <div class="truncate">
                                                    <div class="font-medium text-white truncate">{m.display_name}</div>
                                                    <div class="text-[10px] text-slate-400 font-mono truncate">{m.email}</div>
                                                </div>
                                                <span class="text-[#06B6D4] font-bold">
                                                    {if is_selected { "✓" } else { "" }}
                                                </span>
                                            </button>
                                        }
                                    })
                                    .collect_view()
                            }}
                        </div>
                    </Show>
                </div>
            }
            .into_any()
        }

        // ====================================================================
        // 11. `LastModified`: Read-only system timestamp & last editor badge
        // ====================================================================
        ColumnPropertyType::LastModified => {
            let ts = match &current_cell {
                Some(CellValue::LastModified(dt)) => *dt,
                _ => row_last_modified,
            };

            view! {
                <div class="w-full h-full min-h-[34px] flex items-center justify-between gap-1.5 px-2.5 pr-8 py-1 text-[11px] text-slate-400 select-none">
                    <span class="font-mono">{ts.format("%Y-%m-%d %H:%M:%S").to_string()}</span>
                    <span class="px-1.5 py-0.5 rounded bg-slate-800/80 text-[10px] text-slate-300 truncate max-w-[90px]">
                        {move || {
                            space_members
                                .get()
                                .into_iter()
                                .find(|m| m.user_id == row_updated_by)
                                .map(|m| m.display_name)
                                .unwrap_or_else(|| format!("#{}", &row_updated_by.to_string()[..4]))
                        }}
                    </span>
                </div>
            }
            .into_any()
        }
    };

    view! {
        <div class="group relative w-full h-full">
            {inner_editor}

            // Floating One-Click Copy Button (revealed on cell hover)
            <button
                type="button"
                title="Copy cell value to clipboard"
                on:mousedown=move |ev| {
                    ev.stop_propagation();
                }
                on:dblclick=move |ev| {
                    ev.stop_propagation();
                }
                on:click=move |ev| {
                    ev.stop_propagation();
                    let col_val = col_for_copy.get_value();
                    let cell_val = cell_for_copy.get();
                    let members = space_members.get();
                    let plain = cell_to_plain_text(
                        cell_val.as_ref(),
                        &col_val,
                        &members,
                        row_last_modified,
                    );
                    copy_text_to_clipboard(plain, copied);
                }
                class=move || {
                    if copied.get() {
                        "absolute right-1 top-1/2 -translate-y-1/2 z-30 px-1.5 py-0.5 rounded bg-emerald-500/20 border border-emerald-500/50 text-emerald-300 text-[10px] font-bold opacity-100 transition-opacity"
                    } else {
                        "absolute right-1 top-1/2 -translate-y-1/2 z-30 px-1.5 py-0.5 rounded bg-[#1E2227]/95 hover:bg-slate-700 border border-[#2A2F37] text-slate-300 text-[10px] opacity-0 group-hover:opacity-100 transition-opacity shadow"
                    }
                }
            >
                {move || if copied.get() { "✓" } else { "⧉" }}
            </button>
        </div>
    }
}
