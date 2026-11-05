use crate::{api::ApiClient, state::AuthState};
use leptos::{prelude::*, task::spawn_local};
use pyra_ocean_shared::TrashedGridItem;
use std::collections::HashSet;
use uuid::Uuid;

#[component]
pub fn TrashViewPage(on_navigate: Callback<String>) -> impl IntoView {
    let auth = expect_context::<AuthState>();

    let trashed_items = RwSignal::new(Vec::<TrashedGridItem>::new());
    let is_loading = RwSignal::new(false);
    let error_banner = RwSignal::new(Option::<String>::None);
    let success_banner = RwSignal::new(Option::<String>::None);

    // Search & Space filter state
    let search_query = RwSignal::new(String::new());
    let selected_space_filter = RwSignal::new(String::from("all"));

    // Multi-checkbox selection & confirmation states
    let selected_ids = RwSignal::new(HashSet::<Uuid>::new());
    let confirming_permanent_id = RwSignal::new(Option::<Uuid>::None);
    let confirming_bulk_permanent = RwSignal::new(false);
    let show_empty_trash_modal = RwSignal::new(false);

    // Helper to refresh a Space's active grids in `auth.space_grids` so the Sidebar updates in real time
    let refresh_space_sidebar = move |space_id: Uuid| {
        spawn_local(async move {
            if let Ok(grids) = ApiClient::list_space_grids(&auth, space_id).await {
                auth.space_grids.update(|map| {
                    map.insert(space_id, grids);
                });
            }
        });
    };

    // Fetch all trashed grids accessible to the user
    let load_trashed_grids = move || {
        is_loading.set(true);
        error_banner.set(None);
        spawn_local(async move {
            match ApiClient::list_trashed_grids(&auth).await {
                Ok(items) => {
                    auth.trash_count.set(items.len());
                    trashed_items.set(items);
                }
                Err(e) => error_banner.set(Some(e)),
            }
            is_loading.set(false);
        });
    };

    Effect::new(move |_| {
        if auth.token.get().is_some() {
            load_trashed_grids();
        }
    });

    // Filtered items memo
    let filtered_items = Memo::new(move |_| {
        let q = search_query.get().trim().to_lowercase();
        let space_filter = selected_space_filter.get();
        trashed_items
            .get()
            .into_iter()
            .filter(|item| {
                let matches_space =
                    space_filter == "all" || item.space_id.to_string() == space_filter;
                if !matches_space {
                    return false;
                }
                if q.is_empty() {
                    return true;
                }
                item.name.to_lowercase().contains(&q)
                    || item.space_name.to_lowercase().contains(&q)
                    || item
                        .deleted_by_email
                        .as_deref()
                        .unwrap_or("")
                        .to_lowercase()
                        .contains(&q)
            })
            .collect::<Vec<TrashedGridItem>>()
    });

    // Single Grid Restore (`POST /api/trash/grids/{grid_id}/restore`)
    let handle_restore_single = move |item: TrashedGridItem| {
        let gid = item.id;
        let sid = item.space_id;
        let gname = item.name.clone();
        error_banner.set(None);
        spawn_local(async move {
            match ApiClient::restore_trashed_grid(&auth, gid).await {
                Ok(_) => {
                    trashed_items.update(|list| list.retain(|i| i.id != gid));
                    selected_ids.update(|set| {
                        set.remove(&gid);
                    });
                    auth.trash_count.set(trashed_items.get_untracked().len());
                    refresh_space_sidebar(sid);
                    success_banner.set(Some(format!(
                        "Restored grid \"{}\" back to its Space.",
                        gname
                    )));
                }
                Err(e) => error_banner.set(Some(e)),
            }
        });
    };

    // Single Grid Permanent Delete (`DELETE /api/trash/grids/{grid_id}/permanent`)
    let handle_permanent_delete_single = move |gid: Uuid, gname: String| {
        error_banner.set(None);
        spawn_local(async move {
            match ApiClient::permanently_delete_trashed_grid(&auth, gid).await {
                Ok(()) => {
                    trashed_items.update(|list| list.retain(|i| i.id != gid));
                    selected_ids.update(|set| {
                        set.remove(&gid);
                    });
                    confirming_permanent_id.set(None);
                    auth.trash_count.set(trashed_items.get_untracked().len());
                    success_banner.set(Some(format!("Permanently deleted grid \"{}\".", gname)));
                }
                Err(e) => error_banner.set(Some(e)),
            }
        });
    };

    // Bulk Restore Selected Grids
    let handle_bulk_restore = move |_| {
        let ids: Vec<Uuid> = selected_ids.get_untracked().into_iter().collect();
        if ids.is_empty() {
            return;
        }
        let items_map: Vec<TrashedGridItem> = trashed_items
            .get_untracked()
            .into_iter()
            .filter(|i| ids.contains(&i.id))
            .collect();

        error_banner.set(None);
        spawn_local(async move {
            let mut restored_count = 0usize;
            let mut affected_spaces = HashSet::<Uuid>::new();

            for item in items_map {
                if ApiClient::restore_trashed_grid(&auth, item.id).await.is_ok() {
                    restored_count += 1;
                    affected_spaces.insert(item.space_id);
                    let id_removed = item.id;
                    trashed_items.update(|list| list.retain(|i| i.id != id_removed));
                    selected_ids.update(|set| {
                        set.remove(&id_removed);
                    });
                }
            }

            auth.trash_count.set(trashed_items.get_untracked().len());
            for sid in affected_spaces {
                refresh_space_sidebar(sid);
            }
            if restored_count > 0 {
                success_banner.set(Some(format!(
                    "Restored {} grid{} to their original Spaces.",
                    restored_count,
                    if restored_count == 1 { "" } else { "s" }
                )));
            }
        });
    };

    // Bulk Permanent Delete Selected Grids
    let handle_bulk_permanent_delete = move |_| {
        let ids: Vec<Uuid> = selected_ids.get_untracked().into_iter().collect();
        if ids.is_empty() {
            return;
        }
        error_banner.set(None);
        spawn_local(async move {
            let mut deleted_count = 0usize;
            for gid in ids {
                if ApiClient::permanently_delete_trashed_grid(&auth, gid)
                    .await
                    .is_ok()
                {
                    deleted_count += 1;
                    trashed_items.update(|list| list.retain(|i| i.id != gid));
                    selected_ids.update(|set| {
                        set.remove(&gid);
                    });
                }
            }
            confirming_bulk_permanent.set(false);
            auth.trash_count.set(trashed_items.get_untracked().len());
            if deleted_count > 0 {
                success_banner.set(Some(format!(
                    "Permanently erased {} grid{}.",
                    deleted_count,
                    if deleted_count == 1 { "" } else { "s" }
                )));
            }
        });
    };

    // Empty Trash (`DELETE /api/trash/empty?space_id=...`)
    let handle_confirm_empty_trash = move |_| {
        let filter_val = selected_space_filter.get_untracked();
        let maybe_space_id = if filter_val == "all" {
            None
        } else {
            Uuid::parse_str(&filter_val).ok()
        };

        error_banner.set(None);
        spawn_local(async move {
            match ApiClient::empty_trash(&auth, maybe_space_id).await {
                Ok(resp) => {
                    show_empty_trash_modal.set(false);
                    selected_ids.set(HashSet::new());
                    load_trashed_grids();
                    success_banner.set(Some(format!(
                        "Emptied Trash ({} grid{} permanently purged).",
                        resp.purged_count,
                        if resp.purged_count == 1 { "" } else { "s" }
                    )));
                }
                Err(e) => {
                    show_empty_trash_modal.set(false);
                    error_banner.set(Some(e));
                }
            }
        });
    };

    view! {
        <div class="p-8 max-w-6xl mx-auto w-full space-y-6">
            // ================================================================
            // 1. Header Bar: Title, Subtitle, Search, Space Filter & Empty Trash
            // ================================================================
            <div class="p-6 rounded-2xl bg-[#1E2227] border border-[#2A2F37] flex flex-col lg:flex-row lg:items-center justify-between gap-4 shadow-xl">
                <div class="flex items-center gap-4">
                    <div class="h-12 w-12 rounded-2xl bg-red-500/15 border border-red-500/30 flex items-center justify-center text-2xl shrink-0">
                        "🗑️"
                    </div>
                    <div>
                        <div class="flex items-center gap-2.5">
                            <h1 class="text-xl font-bold text-white tracking-tight">
                                "🗑️ Trash"
                            </h1>
                            <span class="px-2.5 py-0.5 rounded-full bg-slate-800 border border-[#2A2F37] text-xs font-mono text-[#06B6D4]">
                                {move || format!("{} items", trashed_items.get().len())}
                            </span>
                        </div>
                        <p class="text-xs text-slate-400 mt-1">
                            "Grids in Trash are automatically deleted after 30 days. Restore any grid to return it to its original Space."
                        </p>
                    </div>
                </div>

                <div class="flex flex-wrap items-center gap-2.5">
                    // Search by Grid Name or Space Name
                    <input
                        type="search"
                        placeholder="Search trashed grids or spaces..."
                        class="w-56 px-3.5 py-2 rounded-xl bg-[#14171A] border border-[#2A2F37] text-xs text-white focus:outline-none focus:border-[#06B6D4]"
                        prop:value=search_query
                        on:input=move |ev| search_query.set(event_target_value(&ev))
                    />

                    // Space Filter Dropdown
                    <select
                        class="px-3 py-2 rounded-xl bg-[#14171A] border border-[#2A2F37] text-xs text-slate-200 focus:outline-none focus:border-[#06B6D4]"
                        prop:value=selected_space_filter
                        on:change=move |ev| selected_space_filter.set(event_target_value(&ev))
                    >
                        <option value="all">"All Spaces"</option>
                        {move || {
                            auth.spaces
                                .get()
                                .into_iter()
                                .map(|s| {
                                    let sid = s.space.id.to_string();
                                    let sname = s.space.name;
                                    view! {
                                        <option value=sid>{sname}</option>
                                    }
                                })
                                .collect_view()
                        }}
                    </select>

                    // Refresh Button
                    <button
                        type="button"
                        on:click=move |_| load_trashed_grids()
                        class="px-3 py-2 rounded-xl bg-[#14171A] hover:bg-slate-800 border border-[#2A2F37] text-xs font-medium text-slate-300 transition"
                    >
                        "↻ Refresh"
                    </button>

                    // Red Empty Trash Button
                    <button
                        type="button"
                        disabled=move || trashed_items.get().is_empty()
                        on:click=move |_| show_empty_trash_modal.set(true)
                        class=move || {
                            if trashed_items.get().is_empty() {
                                "px-4 py-2 rounded-xl bg-red-500/10 border border-red-500/20 text-xs font-semibold text-red-400/40 cursor-not-allowed"
                            } else {
                                "px-4 py-2 rounded-xl bg-red-600 hover:bg-red-500 text-white text-xs font-bold shadow-lg transition cursor-pointer"
                            }
                        }
                    >
                        "Empty Trash"
                    </button>
                </div>
            </div>

            // Status / Feedback Banners
            {move || {
                error_banner.get().map(|err| {
                    view! {
                        <div class="p-3.5 rounded-xl bg-red-500/15 border border-red-500/40 text-xs text-red-300 flex items-center justify-between">
                            <span>{err}</span>
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

            {move || {
                success_banner.get().map(|msg| {
                    view! {
                        <div class="p-3.5 rounded-xl bg-emerald-500/15 border border-emerald-500/40 text-xs text-emerald-300 flex items-center justify-between">
                            <span>{msg}</span>
                            <button
                                type="button"
                                on:click=move |_| success_banner.set(None)
                                class="font-bold ml-4"
                            >
                                "✕"
                            </button>
                        </div>
                    }
                })
            }}

            // ================================================================
            // 2. Bulk Selection Action Bar (Bulk Restore / Bulk Permanent Delete)
            // ================================================================
            <Show when=move || !selected_ids.get().is_empty()>
                <div class="p-4 rounded-2xl bg-[#1E2227] border border-[#06B6D4]/50 shadow-xl flex flex-wrap items-center justify-between gap-4">
                    <div class="flex items-center gap-3">
                        <span class="px-2.5 py-1 rounded-lg bg-[#06B6D4]/15 text-[#06B6D4] font-mono text-xs font-bold">
                            {move || format!("{} selected", selected_ids.get().len())}
                        </span>
                        <button
                            type="button"
                            on:click=move |_| {
                                selected_ids.set(HashSet::new());
                                confirming_bulk_permanent.set(false);
                            }
                            class="text-xs text-slate-400 hover:text-white underline"
                        >
                            "Clear Selection"
                        </button>
                    </div>

                    <div class="flex items-center gap-2.5">
                        <button
                            type="button"
                            on:click=handle_bulk_restore
                            class="px-4 py-2 rounded-xl bg-[#06B6D4] hover:bg-[#0284C7] text-slate-950 hover:text-white text-xs font-bold shadow transition cursor-pointer"
                        >
                            "↩️ Restore Selected"
                        </button>

                        <Show
                            when=move || confirming_bulk_permanent.get()
                            fallback=move || {
                                view! {
                                    <button
                                        type="button"
                                        on:click=move |_| confirming_bulk_permanent.set(true)
                                        class="px-4 py-2 rounded-xl bg-red-500/20 hover:bg-red-500/30 border border-red-500/40 text-red-300 text-xs font-bold transition cursor-pointer"
                                    >
                                        "✕ Delete Selected Permanently"
                                    </button>
                                }
                            }
                        >
                            <div class="flex items-center gap-2 bg-red-950/50 border border-red-500/50 px-3 py-1.5 rounded-xl">
                                <span class="text-xs text-red-200 font-semibold">
                                    "Erase permanently?"
                                </span>
                                <button
                                    type="button"
                                    on:click=handle_bulk_permanent_delete
                                    class="px-3 py-1 rounded-lg bg-red-600 hover:bg-red-500 text-white text-xs font-bold"
                                >
                                    "Confirm Erase"
                                </button>
                                <button
                                    type="button"
                                    on:click=move |_| confirming_bulk_permanent.set(false)
                                    class="px-2.5 py-1 rounded-lg bg-slate-800 text-slate-300 text-xs"
                                >
                                    "Cancel"
                                </button>
                            </div>
                        </Show>
                    </div>
                </div>
            </Show>

            // ================================================================
            // 3. Trash Table / Cards
            // ================================================================
            <div class="rounded-2xl bg-[#1E2227] border border-[#2A2F37] overflow-hidden shadow-xl">
                <Show
                    when=move || !is_loading.get()
                    fallback=move || {
                        view! {
                            <div class="p-12 text-center text-xs text-slate-400">
                                "Loading Trash..."
                            </div>
                        }
                    }
                >
                    {move || {
                        let items = filtered_items.get();
                        if items.is_empty() {
                            return view! {
                                <div class="p-14 text-center space-y-3">
                                    <div class="text-3xl opacity-60">"🗑️"</div>
                                    <h2 class="text-sm font-bold text-white">
                                        "Trash is Empty"
                                    </h2>
                                    <p class="text-xs text-slate-400 max-w-md mx-auto">
                                        "Deleted Grids appear here for 30 days before being permanently purged. You can restore any trashed Grid back to its Space in one click."
                                    </p>
                                    <div class="pt-2">
                                        <button
                                            type="button"
                                            on:click=move |_| on_navigate.run("/".into())
                                            class="px-4 py-2 rounded-xl bg-[#14171A] hover:bg-slate-800 border border-[#2A2F37] text-xs font-semibold text-[#06B6D4] transition"
                                        >
                                            "← Back to Workspace"
                                        </button>
                                    </div>
                                </div>
                            }
                            .into_any();
                        }

                        let all_filtered_ids: Vec<Uuid> = items.iter().map(|i| i.id).collect();
                        let all_ids_for_header = all_filtered_ids.clone();

                        view! {
                            <div class="overflow-x-auto">
                                <table class="w-full text-left border-collapse">
                                    <thead>
                                        <tr class="border-b border-[#2A2F37] bg-[#14171A] text-[11px] uppercase tracking-wider text-slate-400">
                                            <th class="w-10 px-4 py-3 text-center">
                                                <input
                                                    type="checkbox"
                                                    class="h-3.5 w-3.5 rounded accent-[#06B6D4] cursor-pointer"
                                                    prop:checked=move || {
                                                        let sel = selected_ids.get();
                                                        !all_ids_for_header.is_empty()
                                                            && all_ids_for_header.iter().all(|id| sel.contains(id))
                                                    }
                                                    on:change=move |ev| {
                                                        let checked = event_target_checked(&ev);
                                                        let ids = all_filtered_ids.clone();
                                                        selected_ids.update(|set| {
                                                            if checked {
                                                                for id in ids {
                                                                    set.insert(id);
                                                                }
                                                            } else {
                                                                for id in &ids {
                                                                    set.remove(id);
                                                                }
                                                            }
                                                        });
                                                    }
                                                />
                                            </th>
                                            <th class="px-4 py-3 font-semibold">"Grid Name"</th>
                                            <th class="px-4 py-3 font-semibold">"Original Space"</th>
                                            <th class="px-4 py-3 font-semibold">"Deleted By"</th>
                                            <th class="px-4 py-3 font-semibold">"Deleted Time & Expiry"</th>
                                            <th class="px-4 py-3 font-semibold text-right">"Actions"</th>
                                        </tr>
                                    </thead>
                                    <tbody class="divide-y divide-[#2A2F37]/70 text-xs">
                                        {items
                                            .into_iter()
                                            .map(|item| {
                                                let gid = item.id;
                                                let gname = item.name.clone();
                                                let gname_for_del = item.name.clone();
                                                let space_name = item.space_name.clone();
                                                let deleted_by = item
                                                    .deleted_by_email
                                                    .clone()
                                                    .unwrap_or_else(|| "Workspace User".into());
                                                let deleted_ts = item
                                                    .deleted_at
                                                    .format("%Y-%m-%d %H:%M UTC")
                                                    .to_string();
                                                let days_left = item.days_remaining.max(0);
                                                let item_for_restore = item.clone();

                                                let days_badge_cls = if days_left <= 5 {
                                                    "bg-red-500/15 text-red-300 border-red-500/30"
                                                } else if days_left <= 14 {
                                                    "bg-amber-500/15 text-amber-300 border-amber-500/30"
                                                } else {
                                                    "bg-slate-800 text-slate-300 border-slate-700"
                                                };

                                                view! {
                                                    <tr class="hover:bg-slate-800/40 transition-colors">
                                                        <td class="px-4 py-3.5 text-center">
                                                            <input
                                                                type="checkbox"
                                                                class="h-3.5 w-3.5 rounded accent-[#06B6D4] cursor-pointer"
                                                                prop:checked=move || selected_ids.get().contains(&gid)
                                                                on:change=move |_| {
                                                                    selected_ids.update(|set| {
                                                                        if set.contains(&gid) {
                                                                            set.remove(&gid);
                                                                        } else {
                                                                            set.insert(gid);
                                                                        }
                                                                    });
                                                                }
                                                            />
                                                        </td>

                                                        // Grid Name & Icon
                                                        <td class="px-4 py-3.5">
                                                            <div class="flex items-center gap-2.5">
                                                                <span class="h-7 w-7 rounded-lg bg-[#06B6D4]/15 border border-[#06B6D4]/30 flex items-center justify-center text-[#06B6D4] font-bold shrink-0">
                                                                    "⊞"
                                                                </span>
                                                                <div>
                                                                    <div class="font-semibold text-white">
                                                                        {gname}
                                                                    </div>
                                                                    <div class="text-[10px] font-mono text-slate-500">
                                                                        {gid.to_string()}
                                                                    </div>
                                                                </div>
                                                            </div>
                                                        </td>

                                                        // Original Space Badge
                                                        <td class="px-4 py-3.5">
                                                            <span class="px-2.5 py-1 rounded-lg bg-[#14171A] border border-[#2A2F37] text-slate-200 font-medium">
                                                                {space_name}
                                                            </span>
                                                        </td>

                                                        // Deleted By
                                                        <td class="px-4 py-3.5 text-slate-300">
                                                            {deleted_by}
                                                        </td>

                                                        // Deleted Time & Days Remaining Badge
                                                        <td class="px-4 py-3.5">
                                                            <div class="flex flex-col gap-1">
                                                                <span class="text-slate-300 font-mono text-[11px]">
                                                                    {deleted_ts}
                                                                </span>
                                                                <div>
                                                                    <span class=format!("inline-block px-2 py-0.5 rounded-full text-[10px] font-semibold border {days_badge_cls}")>
                                                                        {format!("{} day{} left", days_left, if days_left == 1 { "" } else { "s" })}
                                                                    </span>
                                                                </div>
                                                            </div>
                                                        </td>

                                                        // Action Buttons: Restore & Delete Permanently
                                                        <td class="px-4 py-3.5 text-right">
                                                            <div class="flex items-center justify-end gap-2">
                                                                <button
                                                                    type="button"
                                                                    on:click={
                                                                        let item_restore = item_for_restore.clone();
                                                                        move |_| handle_restore_single(item_restore.clone())
                                                                    }
                                                                    class="px-3 py-1.5 rounded-lg bg-[#06B6D4]/15 hover:bg-[#06B6D4] text-[#06B6D4] hover:text-slate-950 border border-[#06B6D4]/40 font-semibold transition cursor-pointer"
                                                                >
                                                                    "↩️ Restore"
                                                                </button>

                                                                {move || {
                                                                    let gname_del = gname_for_del.clone();
                                                                    if confirming_permanent_id.get() == Some(gid) {
                                                                        view! {
                                                                            <div class="flex items-center gap-1.5 bg-red-950/50 border border-red-500/50 px-2 py-1 rounded-lg">
                                                                                <button
                                                                                    type="button"
                                                                                    on:click=move |_| {
                                                                                        handle_permanent_delete_single(gid, gname_del.clone());
                                                                                    }
                                                                                    class="px-2.5 py-1 rounded bg-red-600 hover:bg-red-500 text-white text-[11px] font-bold"
                                                                                >
                                                                                    "Confirm Erase"
                                                                                </button>
                                                                                <button
                                                                                    type="button"
                                                                                    on:click=move |_| confirming_permanent_id.set(None)
                                                                                    class="px-2 py-1 rounded bg-slate-800 text-slate-300 text-[11px]"
                                                                                >
                                                                                    "Cancel"
                                                                                </button>
                                                                            </div>
                                                                        }
                                                                        .into_any()
                                                                    } else {
                                                                        view! {
                                                                            <button
                                                                                type="button"
                                                                                on:click=move |_| confirming_permanent_id.set(Some(gid))
                                                                                class="px-3 py-1.5 rounded-lg bg-red-500/15 hover:bg-red-600 text-red-400 hover:text-white border border-red-500/30 font-semibold transition cursor-pointer"
                                                                            >
                                                                                "✕ Delete Permanently"
                                                                            </button>
                                                                        }
                                                                        .into_any()
                                                                    }
                                                                }}
                                                            </div>
                                                        </td>
                                                    </tr>
                                                }
                                            })
                                            .collect_view()}
                                    </tbody>
                                </table>
                            </div>
                        }
                        .into_any()
                    }}
                </Show>
            </div>

            // ================================================================
            // 4. Empty Trash Confirmation Modal
            // ================================================================
            <Show when=move || show_empty_trash_modal.get()>
                <div
                    on:click=move |_| show_empty_trash_modal.set(false)
                    class="fixed inset-0 z-50 bg-black/70 backdrop-blur-sm flex items-center justify-center p-4"
                >
                    <div
                        on:click=move |ev| ev.stop_propagation()
                        class="w-full max-w-md rounded-2xl bg-[#1E2227] border border-red-500/40 shadow-2xl p-6 space-y-4"
                    >
                        <div class="flex items-center gap-3">
                            <div class="h-10 w-10 rounded-xl bg-red-500/20 border border-red-500/40 flex items-center justify-center text-lg">
                                "⚠️"
                            </div>
                            <div>
                                <h3 class="text-base font-bold text-white">
                                    "Empty Trash Permanently?"
                                </h3>
                                <p class="text-xs text-slate-400">
                                    "This action cannot be undone."
                                </p>
                            </div>
                        </div>

                        <p class="text-xs text-slate-300 leading-relaxed">
                            "All grids currently in Trash (along with their columns, rows, and 30-day history snapshots) will be permanently erased from PostgreSQL."
                        </p>

                        <div class="flex items-center justify-end gap-2.5 pt-2">
                            <button
                                type="button"
                                on:click=move |_| show_empty_trash_modal.set(false)
                                class="px-4 py-2 rounded-xl bg-[#14171A] hover:bg-slate-800 border border-[#2A2F37] text-xs font-semibold text-slate-300"
                            >
                                "Cancel"
                            </button>
                            <button
                                type="button"
                                on:click=handle_confirm_empty_trash
                                class="px-4 py-2 rounded-xl bg-red-600 hover:bg-red-500 text-white text-xs font-bold shadow-lg transition"
                            >
                                "✕ Permanently Empty Trash"
                            </button>
                        </div>
                    </div>
                </div>
            </Show>
        </div>
    }
}

