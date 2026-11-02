use crate::{
    api::ApiClient,
    components::cell_editor::CellEditor,
    state::AuthState,
};
use leptos::{prelude::*, task::spawn_local};
use pyra_ocean_shared::{
    CellValue, ColumnPropertyConfig, ColumnPropertyType, CreateColumnRequest, GridColumn, GridRow,
    NumberFormat, PageGrid, RowCellMap, SelectOption, SortDirection, SpaceMemberDetail, TagColor,
};
use std::collections::{HashMap, HashSet};
use uuid::Uuid;

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

#[component]
pub fn GridViewPage(
    space_id: RwSignal<Uuid>,
    grid_id: RwSignal<Uuid>,
    on_open_import: Callback<Uuid>,
) -> impl IntoView {
    let auth = expect_context::<AuthState>();

    let grid_meta = RwSignal::new(Option::<PageGrid>::None);
    let columns = RwSignal::new(Vec::<GridColumn>::new());
    let rows = RwSignal::new(Vec::<GridRow>::new());
    let space_members = RwSignal::new(Vec::<SpaceMemberDetail>::new());
    let is_loading = RwSignal::new(false);
    let error_banner = RwSignal::new(Option::<String>::None);

    // Sorting & Filtering state
    let sort_specs = RwSignal::new(Vec::<(Uuid, SortDirection)>::new());
    let show_sort_popover = RwSignal::new(false);
    let quick_search = RwSignal::new(String::new());
    let server_filter_col = RwSignal::new(Option::<Uuid>::None);
    let server_filter_val = RwSignal::new(String::new());

    // Bulk Row Selection state
    let selected_row_ids = RwSignal::new(HashSet::<Uuid>::new());

    // `+ Add Column` Popover state
    let show_add_col_popover = RwSignal::new(false);
    let new_col_name = RwSignal::new(String::new());
    let new_col_type = RwSignal::new(ColumnPropertyType::Text);
    let new_col_options_csv = RwSignal::new(String::from("Todo, In Progress, Done"));
    let new_col_currency = RwSignal::new(String::new());

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
        let gid = grid_id.get_untracked();
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

    // Load Grid Schema, Rows, and Space Members whenever `grid_id` or `space_id` changes
    Effect::new(move |_| {
        let sid = space_id.get();
        let gid = grid_id.get();
        selected_row_ids.set(HashSet::new());
        sort_specs.set(Vec::new());
        is_loading.set(true);

        spawn_local(async move {
            if let Ok(schema) = ApiClient::get_grid_schema(&auth, gid).await {
                grid_meta.set(Some(schema.grid));
                columns.set(schema.columns);
            }
            if let Ok(fetched_rows) = ApiClient::list_rows(&auth, gid, None, None, None, None).await
            {
                rows.set(fetched_rows);
            }
            if let Ok(members) = ApiClient::list_space_members(&auth, sid).await {
                space_members.set(members);
            }
            is_loading.set(false);
        });
    });

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

    // Handler for `+ New Row` button
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
                Ok(new_row) => rows.update(|list| list.push(new_row)),
                Err(e) => error_banner.set(Some(e)),
            }
        });
    };

    // Handler for `+ Add Column` popover submit
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
            width_px: Some(200),
            position: None,
            is_primary: Some(false),
        };

        spawn_local(async move {
            match ApiClient::create_column(&auth, gid, payload).await {
                Ok(col) => {
                    columns.update(|list| list.push(col));
                    new_col_name.set(String::new());
                    show_add_col_popover.set(false);
                }
                Err(e) => error_banner.set(Some(e)),
            }
        });
    };

    // Handler for Native Bulk Delete (`DELETE /api/grids/{grid_id}/rows/bulk`)
    let handle_bulk_delete = move |_| {
        let gid = grid_id.get();
        let ids_to_delete: Vec<Uuid> = selected_row_ids.get().into_iter().collect();
        if ids_to_delete.is_empty() {
            return;
        }

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

    view! {
        <div class="h-full flex flex-col justify-between relative">
            // ================================================================
            // 1. Grid Toolbar: Breadcrumb, Role Pill, Search, Sort, +Column, +Row
            // ================================================================
            <div>
                <header class="px-6 py-4 border-b border-[#2A2F37] bg-[#1E2227]/60 flex flex-col lg:flex-row lg:items-center justify-between gap-4">
                    // Space Breadcrumb & Grid Title
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
                                <h1 class="text-lg font-bold tracking-tight">
                                    {move || {
                                        grid_meta
                                            .get()
                                            .map(|g| g.title)
                                            .unwrap_or_else(|| "Dynamic Grid".into())
                                    }}
                                </h1>
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

                    // Right Toolbar Controls: Search, Multi-Sort, Import, +Add Column, +New Row
                    <div class="flex flex-wrap items-center gap-2">
                        // Quick Filter / Search Bar
                        <input
                            type="search"
                            placeholder="Quick filter cells..."
                            class="w-48 px-3 py-1.5 rounded-lg bg-[#14171A] border border-[#2A2F37] text-xs text-white focus:outline-none focus:border-[#06B6D4]"
                            prop:value=quick_search
                            on:input=move |ev| quick_search.set(event_target_value(&ev))
                        />

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
                                    on:click=move |_| show_add_col_popover.update(|v| *v = !*v)
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

                                            // Conditional Option Tags builder for Select / MultiSelect
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

                                            // Conditional Currency Symbol input for Number
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

                            <button
                                type="button"
                                on:click=handle_add_row
                                class="px-3.5 py-1.5 rounded-lg bg-[#F97316] hover:bg-[#EA580C] text-white text-xs font-semibold shadow transition"
                            >
                                "+ New Row"
                            </button>
                        </Show>
                    </div>
                </header>

                // Error toast banner if any cell validation or network call fails
                {move || {
                    error_banner.get().map(|msg| {
                        view! {
                            <div class="mx-6 mt-3 p-3 rounded-xl bg-red-500/15 border border-red-500/40 text-xs text-red-300 flex items-center justify-between">
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
                // 2. Excel-Style Dynamic Spreadsheet Table
                // ============================================================
                <div class="overflow-x-auto">
                    <table class="w-full border-collapse text-left">
                        <thead>
                            <tr class="border-b border-[#2A2F37] bg-[#111315] text-[11px] uppercase tracking-wider text-slate-400 select-none">
                                // Leftmost Sticky Bulk-Selection Header Checkbox
                                <th class="sticky left-0 z-20 bg-[#111315] w-10 px-3 py-2.5 border-r border-[#2A2F37] text-center">
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

                                // Row Number Index Header
                                <th class="w-12 px-2 py-2.5 border-r border-[#2A2F37] text-center font-mono text-[10px]">
                                    "#"
                                </th>

                                // Dynamic Typed Column Headers (Click to toggle ASC/DESC sort)
                                {move || {
                                    columns
                                        .get()
                                        .into_iter()
                                        .filter(|c| !c.is_hidden)
                                        .map(|col| {
                                            let cid = col.id;
                                            let cname = col.name.clone();
                                            let ptype = col.property_type;
                                            let width_style = format!("min-width: {}px;", col.width_px);

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
                                                    style=width_style
                                                    on:click=move |_| toggle_column_sort(cid)
                                                    class="px-3 py-2.5 border-r border-[#2A2F37] hover:bg-slate-800/50 cursor-pointer transition"
                                                >
                                                    <div class="flex items-center justify-between gap-2">
                                                        <div class="flex items-center gap-1.5 truncate">
                                                            <span class="text-[#06B6D4] font-mono text-[10px]">
                                                                {property_type_icon(ptype)}
                                                            </span>
                                                            <span class="font-semibold text-slate-200 truncate">
                                                                {cname}
                                                            </span>
                                                        </div>
                                                        <span class="text-[#F97316] font-bold text-xs">
                                                            {sort_badge}
                                                        </span>
                                                    </div>
                                                </th>
                                            }
                                        })
                                        .collect_view()
                                }}
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
                                    .map(|(idx, row)| {
                                        let rid = row.id;
                                        let cols_for_row = cols.clone();

                                        view! {
                                            <tr
                                                class=move || {
                                                    let is_sel = selected_row_ids.get().contains(&rid);
                                                    if is_sel {
                                                        "bg-[#06B6D4]/10 hover:bg-[#06B6D4]/15 transition"
                                                    } else {
                                                        "hover:bg-slate-800/30 transition"
                                                    }
                                                }
                                            >
                                                // Sticky Left Checkbox Cell
                                                <td class="sticky left-0 z-10 bg-[#14171A] px-3 py-1 border-r border-[#2A2F37] text-center">
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

                                                // Row Index Number
                                                <td class="px-2 py-1 border-r border-[#2A2F37] text-center font-mono text-[11px] text-slate-500">
                                                    {idx + 1}
                                                </td>

                                                // Dynamic Typed Cells (All 11 Property Types)
                                                {cols_for_row
                                                    .into_iter()
                                                    .filter(|c| !c.is_hidden)
                                                    .map(|col| {
                                                        let row_clone = row.clone();
                                                        view! {
                                                            <td class="p-0 border-r border-[#2A2F37] align-middle focus-within:ring-1 focus-within:ring-[#06B6D4]">
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
                                                                    on_error=Callback::new(move |err: String| {
                                                                        error_banner.set(Some(err));
                                                                    })
                                                                />
                                                            </td>
                                                        }
                                                    })
                                                    .collect_view()}
                                            </tr>
                                        }
                                    })
                                    .collect_view()
                            }}
                        </tbody>
                    </table>
                </div>

                // Bottom Add Row Strip inside Spreadsheet
                <Show when=move || auth.can_edit_space(space_id.get())>
                    <div class="px-6 py-2.5 border-b border-[#2A2F37] bg-[#14171A]/60">
                        <button
                            type="button"
                            on:click=handle_add_row
                            class="text-xs font-medium text-slate-400 hover:text-[#F97316] flex items-center gap-1.5 transition"
                        >
                            <span>"+"</span>
                            <span>"New Row"</span>
                        </button>
                    </div>
                </Show>
            </div>

            // ================================================================
            // 3. Floating Bulk Action Bar (`DELETE /api/grids/{grid_id}/rows/bulk`)
            // ================================================================
            <Show when=move || !selected_row_ids.get().is_empty()>
                <div class="fixed bottom-6 left-1/2 -translate-x-1/2 z-40 px-5 py-3 rounded-2xl bg-[#1E2227] border border-[#06B6D4]/50 shadow-2xl flex items-center gap-5">
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

