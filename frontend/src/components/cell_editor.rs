use crate::{api::ApiClient, state::AuthState};
use chrono::{NaiveDate, NaiveTime};
use leptos::{prelude::*, task::spawn_local};
use pyra_ocean_shared::{
    CellValue, ChecklistItem, ColumnPropertyConfig, ColumnPropertyType, FileAttachment, GridColumn,
    GridRow, NumberFormat, RowCellMap, SelectOption, SpaceMemberDetail, TagColor,
};
use std::collections::HashMap;
use uuid::Uuid;
use wasm_bindgen::JsCast;

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

/// Renders and edits a single spreadsheet cell for any of the 11 strict `ColumnPropertyType`s.
#[component]
pub fn CellEditor(
    grid_id: Uuid,
    row: GridRow,
    column: GridColumn,
    can_edit: bool,
    space_members: RwSignal<Vec<SpaceMemberDetail>>,
    on_row_updated: Callback<GridRow>,
    on_error: Callback<String>,
) -> impl IntoView {
    let auth = expect_context::<AuthState>();
    let row_id = row.id;
    let col_id = column.id;
    let prop_type = column.property_type;

    let current_cell = row.cells.get(&col_id).cloned();
    let row_last_modified = row.last_modified;
    let row_updated_by = row.updated_by;

    // Helper closure to send `PATCH /api/grids/{grid_id}/rows/{row_id}` with a single updated cell
    let save_cell = Callback::new(move |new_val: CellValue| {
        if !can_edit {
            return;
        }
        let mut patch_map: RowCellMap = HashMap::new();
        patch_map.insert(col_id, new_val);

        spawn_local(async move {
            match ApiClient::patch_row(&auth, grid_id, row_id, patch_map).await {
                Ok(updated_row) => on_row_updated.run(updated_row),
                Err(e) => on_error.run(e),
            }
        });
    });

    match prop_type {
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
                    class="w-full h-full min-h-[34px] flex items-center px-2.5 py-1 cursor-text"
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
                    class="w-full h-full min-h-[34px] flex items-center justify-end px-2.5 py-1 font-mono cursor-text"
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
        // 3. `Select`: Colored pill badge + floating option picker
        // ====================================================================
        ColumnPropertyType::Select => {
            let options: Vec<SelectOption> = match &column.property_config {
                ColumnPropertyConfig::Select { options } => options.clone(),
                _ => Vec::new(),
            };
            let options_store = StoredValue::new(options);

            let selected_id = RwSignal::new(match &current_cell {
                Some(CellValue::Select(opt)) => *opt,
                _ => None,
            });
            let open_dropdown = RwSignal::new(false);

            view! {
                <div class="relative w-full h-full min-h-[34px] flex items-center px-2 py-1">
                    <button
                        type="button"
                        on:click=move |_| {
                            if can_edit {
                                open_dropdown.update(|o| *o = !*o);
                            }
                        }
                        class="w-full text-left flex items-center justify-between gap-1"
                    >
                        {move || {
                            let cur = selected_id.get();
                            let found = cur.and_then(|id| {
                                options_store.get_value().into_iter().find(|o| o.id == id)
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
                        <div class="absolute left-1 top-9 z-40 w-48 rounded-xl bg-[#1E2227] border border-[#2A2F37] shadow-2xl p-1.5 space-y-1">
                            <button
                                type="button"
                                on:click=move |_| {
                                    selected_id.set(None);
                                    open_dropdown.set(false);
                                    save_cell.run(CellValue::Select(None));
                                }
                                class="w-full text-left px-2 py-1 rounded text-[11px] text-slate-400 hover:bg-slate-800"
                            >
                                "Clear selection"
                            </button>
                            {move || {
                                options_store
                                    .get_value()
                                    .into_iter()
                                    .map(|opt| {
                                        let oid = opt.id;
                                        let cls = tag_color_classes(opt.color);
                                        view! {
                                            <button
                                                type="button"
                                                on:click=move |_| {
                                                    selected_id.set(Some(oid));
                                                    open_dropdown.set(false);
                                                    save_cell.run(CellValue::Select(Some(oid)));
                                                }
                                                class="w-full text-left px-2 py-1 rounded hover:bg-slate-800 flex items-center"
                                            >
                                                <span class=format!("px-2 py-0.5 rounded-full border text-[11px] font-medium {cls}")>
                                                    {opt.label}
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
        // 4. `MultiSelect`: Colored pills with `×` remove + `+` toggle popover
        // ====================================================================
        ColumnPropertyType::MultiSelect => {
            let options: Vec<SelectOption> = match &column.property_config {
                ColumnPropertyConfig::MultiSelect { options } => options.clone(),
                _ => Vec::new(),
            };
            let options_store = StoredValue::new(options);

            let selected_ids = RwSignal::new(match &current_cell {
                Some(CellValue::MultiSelect(ids)) => ids.clone(),
                _ => Vec::new(),
            });
            let open_popover = RwSignal::new(false);

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

            view! {
                <div class="relative w-full h-full min-h-[34px] flex items-center flex-wrap gap-1 px-2 py-1">
                    {move || {
                        let opts = options_store.get_value();
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
                            on:click=move |_| open_popover.update(|o| *o = !*o)
                            class="px-1.5 py-0.5 rounded bg-slate-800 hover:bg-slate-700 text-[10px] text-slate-300 border border-[#2A2F37]"
                        >
                            "+"
                        </button>
                    </Show>

                    <Show when=move || open_popover.get() && can_edit>
                        <div class="absolute left-1 top-9 z-40 w-52 rounded-xl bg-[#1E2227] border border-[#2A2F37] shadow-2xl p-2 space-y-1">
                            <div class="flex items-center justify-between px-1 pb-1 border-b border-[#2A2F37] text-[10px] text-slate-400">
                                <span>"Toggle Multi-Select Tags"</span>
                                <button
                                    type="button"
                                    on:click=move |_| open_popover.set(false)
                                    class="text-slate-400 hover:text-white font-bold"
                                >
                                    "✕"
                                </button>
                            </div>
                            {move || {
                                let current = selected_ids.get();
                                options_store
                                    .get_value()
                                    .into_iter()
                                    .map(|opt| {
                                        let oid = opt.id;
                                        let is_checked = current.contains(&oid);
                                        let cls = tag_color_classes(opt.color);
                                        view! {
                                            <button
                                                type="button"
                                                on:click=move |_| toggle_option(oid)
                                                class="w-full text-left px-2 py-1 rounded hover:bg-slate-800 flex items-center justify-between"
                                            >
                                                <span class=format!("px-2 py-0.5 rounded-full border text-[11px] font-medium {cls}")>
                                                    {opt.label}
                                                </span>
                                                <span class="text-xs text-[#06B6D4]">
                                                    {if is_checked { "✓" } else { "" }}
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
        // 5. `Date`: Native HTML5 `<input type="date">` (ISO-8601)
        // ====================================================================
        ColumnPropertyType::Date => {
            let initial_date = match &current_cell {
                Some(CellValue::Date(Some(d))) => d.format("%Y-%m-%d").to_string(),
                _ => String::new(),
            };
            let date_str = RwSignal::new(initial_date);

            view! {
                <div class="w-full h-full min-h-[34px] flex items-center px-2 py-1">
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
                <div class="w-full h-full min-h-[34px] flex items-center px-2 py-1">
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
                <div class="w-full h-full min-h-[34px] flex items-center justify-center">
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
                <div class="relative w-full h-full min-h-[34px] flex items-center px-2.5 py-1">
                    <button
                        type="button"
                        on:click=move |_| open_checklist.update(|o| *o = !*o)
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
                        <div class="absolute left-1 top-9 z-40 w-64 rounded-xl bg-[#1E2227] border border-[#2A2F37] shadow-2xl p-3 space-y-2">
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
                <div class="w-full h-full min-h-[34px] flex items-center flex-wrap gap-1 px-2 py-1">
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
                <div class="relative w-full h-full min-h-[34px] flex items-center flex-wrap gap-1 px-2 py-1">
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
                            on:click=move |_| open_person_picker.update(|o| *o = !*o)
                            class="px-1.5 py-0.5 rounded bg-slate-800 hover:bg-slate-700 text-[10px] text-slate-300 border border-[#2A2F37]"
                        >
                            "+ 👤"
                        </button>
                    </Show>

                    <Show when=move || open_person_picker.get() && can_edit>
                        <div class="absolute left-1 top-9 z-40 w-56 rounded-xl bg-[#1E2227] border border-[#2A2F37] shadow-2xl p-2 space-y-1">
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
                <div class="w-full h-full min-h-[34px] flex items-center justify-between gap-1.5 px-2.5 py-1 text-[11px] text-slate-400 select-none">
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
    }
}

