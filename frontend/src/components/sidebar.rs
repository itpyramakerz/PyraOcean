use crate::{api::ApiClient, state::AuthState};
use leptos::{prelude::*, task::spawn_local};
use pyra_ocean_shared::{SpaceRole, SpaceWithRole, SystemRole};
use std::collections::HashSet;
use uuid::Uuid;

#[component]
pub fn Sidebar(
    current_path: RwSignal<String>,
    on_navigate: Callback<String>,
    on_open_import_modal: Callback<Uuid>,
    on_open_search_modal: Callback<()>,
) -> impl IntoView {
    let auth = expect_context::<AuthState>();
    let expanded_spaces = RwSignal::new(HashSet::<Uuid>::new());
    let creating_grid_for_space = RwSignal::new(Option::<Uuid>::None);
    let new_grid_title = RwSignal::new(String::new());
    let sidebar_error = RwSignal::new(Option::<String>::None);
    let selected_sidebar_grids = RwSignal::new(HashSet::<Uuid>::new());
    let confirming_delete_grid_id = RwSignal::new(Option::<Uuid>::None);
    let confirming_bulk_delete_space_id = RwSignal::new(Option::<Uuid>::None);

    // Fetch accessible Spaces whenever the user session becomes active
    Effect::new(move |_| {
        if auth.token.get().is_some() {
            spawn_local(async move {
                if let Ok(spaces) = ApiClient::list_spaces(&auth).await {
                    // Auto-expand the first space if none expanded yet
                    if let Some(first) = spaces.first() {
                        let first_id = first.space.id;
                        expanded_spaces.update(|set| {
                            set.insert(first_id);
                        });
                        if let Ok(grids) = ApiClient::list_space_grids(&auth, first_id).await {
                            auth.space_grids.update(|map| {
                                map.insert(first_id, grids);
                            });
                        }
                    }
                    auth.spaces.set(spaces);
                }
            });
        }
    });

    let toggle_space_accordion = move |space_id: Uuid| {
        let mut should_fetch = false;
        expanded_spaces.update(|set| {
            if set.contains(&space_id) {
                set.remove(&space_id);
            } else {
                set.insert(space_id);
                should_fetch = true;
            }
        });

        if should_fetch {
            spawn_local(async move {
                if let Ok(grids) = ApiClient::list_space_grids(&auth, space_id).await {
                    auth.space_grids.update(|map| {
                        map.insert(space_id, grids);
                    });
                }
            });
        }
    };

    let submit_new_grid = move |space_id: Uuid| {
        let title = new_grid_title.get().trim().to_string();
        if title.is_empty() {
            return;
        }
        spawn_local(async move {
            match ApiClient::create_grid(&auth, space_id, title).await {
                Ok(created) => {
                    let grid_id = created.grid.id;
                    auth.space_grids.update(|map| {
                        map.entry(space_id).or_default().push(created.grid);
                    });
                    creating_grid_for_space.set(None);
                    new_grid_title.set(String::new());
                    auth.active_space_id.set(Some(space_id));
                    auth.active_grid_id.set(Some(grid_id));
                    on_navigate.run(format!("/spaces/{space_id}/grids/{grid_id}"));
                }
                Err(e) => sidebar_error.set(Some(e)),
            }
        });
    };

    let handle_single_delete_grid = move |space_id: Uuid, grid_id: Uuid| {
        spawn_local(async move {
            match ApiClient::delete_grid(&auth, grid_id).await {
                Ok(()) => {
                    let mut next_grid_id: Option<Uuid> = None;
                    auth.space_grids.update(|map| {
                        if let Some(list) = map.get_mut(&space_id) {
                            list.retain(|g| g.id != grid_id);
                            next_grid_id = list.first().map(|g| g.id);
                        }
                    });
                    selected_sidebar_grids.update(|set| {
                        set.remove(&grid_id);
                    });
                    confirming_delete_grid_id.set(None);

                    let active_gid = auth.active_grid_id.get_untracked();
                    let cur_path = current_path.get_untracked();
                    if active_gid == Some(grid_id) || cur_path.ends_with(&grid_id.to_string()) {
                        if let Some(next_id) = next_grid_id {
                            auth.active_space_id.set(Some(space_id));
                            auth.active_grid_id.set(Some(next_id));
                            on_navigate.run(format!("/spaces/{space_id}/grids/{next_id}"));
                        } else {
                            auth.active_grid_id.set(None);
                            on_navigate.run("/".into());
                        }
                    }
                }
                Err(e) => sidebar_error.set(Some(e)),
            }
        });
    };

    let handle_bulk_delete_space_grids = move |space_id: Uuid, grid_ids: Vec<Uuid>| {
        if grid_ids.is_empty() {
            return;
        }
        let deleted_set: HashSet<Uuid> = grid_ids.iter().copied().collect();
        spawn_local(async move {
            match ApiClient::bulk_delete_grids(&auth, space_id, grid_ids).await {
                Ok(_) => {
                    let mut next_grid_id: Option<Uuid> = None;
                    auth.space_grids.update(|map| {
                        if let Some(list) = map.get_mut(&space_id) {
                            list.retain(|g| !deleted_set.contains(&g.id));
                            next_grid_id = list.first().map(|g| g.id);
                        }
                    });
                    selected_sidebar_grids.update(|set| {
                        for id in &deleted_set {
                            set.remove(id);
                        }
                    });
                    confirming_bulk_delete_space_id.set(None);

                    let active_gid = auth.active_grid_id.get_untracked();
                    let cur_path = current_path.get_untracked();
                    let viewing_deleted = active_gid
                        .map(|id| deleted_set.contains(&id))
                        .unwrap_or(false)
                        || deleted_set
                            .iter()
                            .any(|id| cur_path.ends_with(&id.to_string()));
                    if viewing_deleted {
                        if let Some(next_id) = next_grid_id {
                            auth.active_space_id.set(Some(space_id));
                            auth.active_grid_id.set(Some(next_id));
                            on_navigate.run(format!("/spaces/{space_id}/grids/{next_id}"));
                        } else {
                            auth.active_grid_id.set(None);
                            on_navigate.run("/".into());
                        }
                    }
                }
                Err(e) => sidebar_error.set(Some(e)),
            }
        });
    };

    view! {
        <aside
            class=move || {
                let width_cls = if auth.sidebar_collapsed.get() { "w-16" } else { "w-72" };
                let surface_cls = if auth.dark_mode.get() {
                    "bg-[#111315] border-[#2A2F37] text-[#F1F5F9]"
                } else {
                    "bg-[#F1F5F9] border-[#E2E8F0] text-[#0F172A]"
                };
                format!("{width_cls} {surface_cls} h-screen border-r flex flex-col justify-between transition-all duration-200 select-none shrink-0")
            }
        >
            // ================================================================
            // 1. Top Bar: PyraOcean Brand Logo, Role Badge & Theme Toggle
            // ================================================================
            <div>
                <div class="p-3.5 border-b border-[#2A2F37]/60 flex items-center justify-between gap-2">
                    <div class="flex items-center gap-2.5 overflow-hidden">
                        <img
                            src=move || auth.active_logo_url()
                            alt="PyraOcean"
                            class="h-9 w-9 rounded-lg object-contain border border-[#06B6D4]/40 shrink-0 shadow-sm"
                        />
                        <Show when=move || !auth.sidebar_collapsed.get()>
                            <div class="truncate">
                                <div class="font-bold text-sm tracking-tight flex items-center gap-1">
                                    <span>"Pyra"</span>
                                    <span class="text-[#06B6D4]">"Ocean"</span>
                                </div>
                                {move || {
                                    auth.user.get().map(|u| {
                                        let badge_cls = match u.system_role {
                                            SystemRole::SuperAdmin => "bg-[#F97316]/15 text-[#F97316] border-[#F97316]/40",
                                            SystemRole::Admin => "bg-[#06B6D4]/15 text-[#06B6D4] border-[#06B6D4]/40",
                                            SystemRole::User => "bg-slate-500/15 text-slate-400 border-slate-500/30",
                                        };
                                        view! {
                                            <span class=format!("inline-block mt-0.5 px-1.5 py-0.2 rounded text-[10px] font-semibold uppercase tracking-wider border {badge_cls}")>
                                                {u.system_role.label()}
                                            </span>
                                        }
                                    })
                                }}
                            </div>
                        </Show>
                    </div>

                    <div class="flex items-center gap-1 shrink-0">
                        <button
                            type="button"
                            title="Toggle Dark / Light Mode"
                            on:click=move |_| auth.toggle_theme()
                            class="p-1.5 rounded-lg hover:bg-slate-500/15 text-xs transition"
                        >
                            {move || if auth.dark_mode.get() { "☀" } else { "☾" }}
                        </button>
                        <button
                            type="button"
                            title="Collapse / Expand Sidebar"
                            on:click=move |_| auth.sidebar_collapsed.update(|c| *c = !*c)
                            class="p-1.5 rounded-lg hover:bg-slate-500/15 text-xs transition"
                        >
                            {move || if auth.sidebar_collapsed.get() { "»" } else { "«" }}
                        </button>
                    </div>
                </div>

                // Error toast inside sidebar if any action fails
                {move || {
                    sidebar_error.get().map(|err| {
                        view! {
                            <div class="mx-3 mt-2 p-2 rounded bg-red-500/15 border border-red-500/40 text-[11px] text-red-300 flex justify-between items-center">
                                <span class="truncate">{err}</span>
                                <button
                                    type="button"
                                    class="ml-2 font-bold"
                                    on:click=move |_| sidebar_error.set(None)
                                >
                                    "×"
                                </button>
                            </div>
                        }
                    })
                }}

                // ============================================================
                // 1.5 Global Search Trigger Button (Ctrl+K / Cmd+K)
                // ============================================================
                <div class="px-3 pt-3">
                    <button
                        type="button"
                        title="Global Scoped Search (Ctrl+K)"
                        on:click=move |_| on_open_search_modal.run(())
                        class="w-full px-2.5 py-2 rounded-xl bg-[#14171A] hover:bg-[#1E2227] border border-[#2A2F37] hover:border-[#06B6D4]/50 text-xs text-slate-300 flex items-center justify-between gap-2 transition"
                    >
                        <div class="flex items-center gap-2 truncate">
                            <span class="text-[#06B6D4]">"🔍"</span>
                            <Show when=move || !auth.sidebar_collapsed.get()>
                                <span class="truncate">"Search Workspace..."</span>
                            </Show>
                        </div>
                        <Show when=move || !auth.sidebar_collapsed.get()>
                            <span class="px-1.5 py-0.5 rounded bg-slate-800/80 border border-slate-700 text-[10px] font-mono text-slate-400 shrink-0">
                                "Ctrl+K"
                            </span>
                        </Show>
                    </button>
                </div>

                // ============================================================
                // 2. Spaces Tree Navigation (Accordion + Grids + Actions)
                // ============================================================
                <Show when=move || !auth.sidebar_collapsed.get()>
                    <div class="px-3 pt-4 pb-2">
                        <div class="flex items-center justify-between px-1 mb-2">
                            <span class="text-[11px] font-semibold uppercase tracking-wider text-slate-400">
                                "Workspaces (Spaces)"
                            </span>
                            <Show when=move || auth.is_super_admin()>
                                <button
                                    type="button"
                                    on:click=move |_| on_navigate.run("/admin/spaces".into())
                                    class="text-[11px] font-semibold text-[#F97316] hover:text-[#EA580C]"
                                >
                                    "+ Space"
                                </button>
                            </Show>
                        </div>

                        <div class="space-y-1.5 max-h-[52vh] overflow-y-auto pr-1">
                            {move || {
                                let spaces = auth.spaces.get();
                                if spaces.is_empty() {
                                    return view! {
                                        <div class="px-2.5 py-3 rounded-lg border border-dashed border-slate-700/60 text-xs text-slate-400">
                                            "No Spaces assigned yet."
                                        </div>
                                    }
                                    .into_any();
                                }

                                spaces
                                    .into_iter()
                                    .map(|space_item: SpaceWithRole| {
                                        let space_id = space_item.space.id;
                                        let space_name = space_item.space.name.clone();
                                        let eff_role = space_item.effective_role;
                                        let can_create_or_edit = auth.is_super_admin()
                                            || matches!(eff_role, SpaceRole::SpaceAdmin | SpaceRole::Editor);

                                        view! {
                                            <div class="rounded-lg border border-transparent hover:border-[#2A2F37]/60">
                                                // Space Accordion Header
                                                <div
                                                    on:click=move |_| toggle_space_accordion(space_id)
                                                    class=move || {
                                                        let active = auth.active_space_id.get() == Some(space_id);
                                                        let highlight = if active {
                                                            "bg-[#06B6D4]/10 text-[#06B6D4] border-l-2 border-[#06B6D4]"
                                                        } else {
                                                            "hover:bg-slate-500/10"
                                                        };
                                                        format!("w-full px-2.5 py-2 rounded-md flex items-center justify-between cursor-pointer text-xs font-semibold transition {highlight}")
                                                    }
                                                >
                                                    <div class="flex items-center gap-2 truncate">
                                                        <span class="text-[10px] text-slate-400">
                                                            {move || if expanded_spaces.get().contains(&space_id) { "▼" } else { "▶" }}
                                                        </span>
                                                        <span class="truncate">{space_name}</span>
                                                    </div>
                                                    <span class="text-[10px] px-1.5 py-0.5 rounded bg-slate-500/10 text-slate-400">
                                                        {eff_role.label()}
                                                    </span>
                                                </div>

                                                // Expanded Space Accordion Body: Grids List + Action Buttons
                                                <Show when=move || expanded_spaces.get().contains(&space_id)>
                                                    <div class="pl-5 pr-2 py-1.5 space-y-1">
                                                        // Grid Pages inside this Space
                                                        {move || {
                                                            let grids = auth
                                                                .space_grids
                                                                .get()
                                                                .get(&space_id)
                                                                .cloned()
                                                                .unwrap_or_default();

                                                            let space_grid_ids: Vec<Uuid> =
                                                                grids.iter().map(|g| g.id).collect();
                                                            let selected_in_space: Vec<Uuid> = {
                                                                let sel = selected_sidebar_grids.get();
                                                                space_grid_ids
                                                                    .iter()
                                                                    .copied()
                                                                    .filter(|id| sel.contains(id))
                                                                    .collect()
                                                            };
                                                            let has_selection_in_space = !selected_in_space.is_empty();
                                                            let all_selected_in_space = !space_grid_ids.is_empty()
                                                                && selected_in_space.len() == space_grid_ids.len();

                                                            let grid_rows_view = grids
                                                                .into_iter()
                                                                .map(|grid| {
                                                                    let gid = grid.id;
                                                                    let gtitle = grid.title.clone();
                                                                    let target_route = format!("/spaces/{space_id}/grids/{gid}");
                                                                    let target_route_click = target_route.clone();
                                                                    let is_checked = selected_sidebar_grids.get().contains(&gid);

                                                                    view! {
                                                                        <div
                                                                            class=move || {
                                                                                let is_selected = auth.active_grid_id.get() == Some(gid)
                                                                                    || current_path.get() == target_route;
                                                                                let cls = if is_selected {
                                                                                    "bg-[#0284C7]/20 text-[#06B6D4] border-l-2 border-[#06B6D4] font-semibold"
                                                                                } else {
                                                                                    "text-slate-400 hover:text-slate-200 hover:bg-slate-500/10"
                                                                                };
                                                                                format!("group/grid w-full px-2 py-1.5 rounded text-xs flex items-center justify-between gap-1.5 transition {cls}")
                                                                            }
                                                                        >
                                                                            <div class="flex items-center gap-1.5 min-w-0 flex-1">
                                                                                <Show when=move || can_create_or_edit>
                                                                                    <input
                                                                                        type="checkbox"
                                                                                        prop:checked=is_checked
                                                                                        on:click=move |ev| {
                                                                                            ev.stop_propagation();
                                                                                            selected_sidebar_grids.update(|set| {
                                                                                                if set.contains(&gid) {
                                                                                                    set.remove(&gid);
                                                                                                } else {
                                                                                                    set.insert(gid);
                                                                                                }
                                                                                            });
                                                                                        }
                                                                                        class=if has_selection_in_space || is_checked {
                                                                                            "h-3.5 w-3.5 rounded accent-[#F97316] cursor-pointer shrink-0 opacity-100 transition-opacity"
                                                                                        } else {
                                                                                            "h-3.5 w-3.5 rounded accent-[#F97316] cursor-pointer shrink-0 opacity-0 group-hover/grid:opacity-100 transition-opacity"
                                                                                        }
                                                                                    />
                                                                                </Show>
                                                                                <button
                                                                                    type="button"
                                                                                    on:click=move |_| {
                                                                                        auth.active_space_id.set(Some(space_id));
                                                                                        auth.active_grid_id.set(Some(gid));
                                                                                        on_navigate.run(target_route_click.clone());
                                                                                    }
                                                                                    class="flex items-center gap-1.5 min-w-0 flex-1 text-left truncate"
                                                                                >
                                                                                    <span class="text-[#06B6D4] shrink-0">"⊞"</span>
                                                                                    <span class="truncate">{gtitle}</span>
                                                                                </button>
                                                                            </div>

                                                                            <Show when=move || can_create_or_edit>
                                                                                {move || {
                                                                                    if confirming_delete_grid_id.get() == Some(gid) {
                                                                                        view! {
                                                                                            <div
                                                                                                class="flex items-center gap-1 shrink-0"
                                                                                                on:click=move |ev| ev.stop_propagation()
                                                                                            >
                                                                                                <button
                                                                                                    type="button"
                                                                                                    title="Confirm Delete Grid"
                                                                                                    on:click=move |ev| {
                                                                                                        ev.stop_propagation();
                                                                                                        handle_single_delete_grid(space_id, gid);
                                                                                                    }
                                                                                                    class="px-1.5 py-0.5 rounded bg-red-600 hover:bg-red-500 text-white text-[10px] font-bold"
                                                                                                >
                                                                                                    "Del"
                                                                                                </button>
                                                                                                <button
                                                                                                    type="button"
                                                                                                    title="Cancel"
                                                                                                    on:click=move |ev| {
                                                                                                        ev.stop_propagation();
                                                                                                        confirming_delete_grid_id.set(None);
                                                                                                    }
                                                                                                    class="px-1 py-0.5 rounded bg-slate-800 hover:bg-slate-700 text-slate-300 text-[10px]"
                                                                                                >
                                                                                                    "×"
                                                                                                </button>
                                                                                            </div>
                                                                                        }
                                                                                        .into_any()
                                                                                    } else {
                                                                                        view! {
                                                                                            <button
                                                                                                type="button"
                                                                                                title="Delete Grid"
                                                                                                on:click=move |ev| {
                                                                                                    ev.stop_propagation();
                                                                                                    confirming_delete_grid_id.set(Some(gid));
                                                                                                }
                                                                                                class="opacity-0 group-hover/grid:opacity-100 p-0.5 rounded hover:bg-red-500/20 text-slate-400 hover:text-red-400 text-[11px] transition shrink-0"
                                                                                            >
                                                                                                "🗑️"
                                                                                            </button>
                                                                                        }
                                                                                        .into_any()
                                                                                    }
                                                                                }}
                                                                            </Show>
                                                                        </div>
                                                                    }
                                                                })
                                                                .collect_view();

                                                            let selected_for_bar = selected_in_space.clone();
                                                            let space_ids_for_toggle = space_grid_ids.clone();

                                                            view! {
                                                                <div class="space-y-1">
                                                                    {grid_rows_view}

                                                                    // Compact Bulk Selection Bar when 1+ Grids are selected in this Space
                                                                    <Show when=move || can_create_or_edit && has_selection_in_space>
                                                                        {
                                                                            let sel_ids = selected_for_bar.clone();
                                                                            let all_ids = space_ids_for_toggle.clone();
                                                                            let count = sel_ids.len();
                                                                            view! {
                                                                                <div class="mt-1.5 p-2 rounded-lg bg-red-950/30 border border-red-500/30 flex flex-col gap-1.5">
                                                                                    <div class="flex items-center justify-between text-[11px]">
                                                                                        <span class="font-semibold text-red-300">
                                                                                            {format!("{count} grid{} selected", if count == 1 { "" } else { "s" })}
                                                                                        </span>
                                                                                        <button
                                                                                            type="button"
                                                                                            on:click=move |_| {
                                                                                                let ids = all_ids.clone();
                                                                                                selected_sidebar_grids.update(|set| {
                                                                                                    if all_selected_in_space {
                                                                                                        for id in &ids {
                                                                                                            set.remove(id);
                                                                                                        }
                                                                                                    } else {
                                                                                                        for id in ids {
                                                                                                            set.insert(id);
                                                                                                        }
                                                                                                    }
                                                                                                });
                                                                                            }
                                                                                            class="text-[10px] font-semibold text-[#06B6D4] hover:underline"
                                                                                        >
                                                                                            {if all_selected_in_space { "Clear" } else { "Select All" }}
                                                                                        </button>
                                                                                    </div>

                                                                                    {move || {
                                                                                        let ids_to_del = sel_ids.clone();
                                                                                        if confirming_bulk_delete_space_id.get() == Some(space_id) {
                                                                                            view! {
                                                                                                <div class="flex items-center gap-1">
                                                                                                    <button
                                                                                                        type="button"
                                                                                                        on:click=move |_| {
                                                                                                            handle_bulk_delete_space_grids(space_id, ids_to_del.clone());
                                                                                                        }
                                                                                                        class="flex-1 py-1 rounded bg-red-600 hover:bg-red-500 text-white text-[11px] font-bold transition"
                                                                                                    >
                                                                                                        "Confirm Delete"
                                                                                                    </button>
                                                                                                    <button
                                                                                                        type="button"
                                                                                                        on:click=move |_| confirming_bulk_delete_space_id.set(None)
                                                                                                        class="px-2 py-1 rounded bg-slate-800 hover:bg-slate-700 text-slate-300 text-[11px]"
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
                                                                                                    on:click=move |_| confirming_bulk_delete_space_id.set(Some(space_id))
                                                                                                    class="w-full py-1 px-2 rounded bg-red-500/20 hover:bg-red-500/30 border border-red-500/40 text-red-300 text-[11px] font-semibold flex items-center justify-center gap-1 transition"
                                                                                                >
                                                                                                    <span>"🗑️"</span>
                                                                                                    <span>"Delete Selected"</span>
                                                                                                </button>
                                                                                            }
                                                                                            .into_any()
                                                                                        }
                                                                                    }}
                                                                                </div>
                                                                            }
                                                                        }
                                                                    </Show>
                                                                </div>
                                                            }
                                                        }}

                                                        // Conditional `+ New Grid` & `Import from Notion / AppFlowy` buttons
                                                        <Show when=move || can_create_or_edit>
                                                            <div class="pt-1 flex flex-col gap-1">
                                                                <div class="flex items-center gap-1.5">
                                                                    <button
                                                                        type="button"
                                                                        on:click=move |_| {
                                                                            creating_grid_for_space.set(Some(space_id));
                                                                            new_grid_title.set(String::new());
                                                                        }
                                                                        class="flex-1 px-2 py-1 rounded text-[11px] font-medium bg-[#F97316]/15 text-[#F97316] hover:bg-[#F97316]/25 border border-[#F97316]/30 transition text-left"
                                                                    >
                                                                        "+ New Grid"
                                                                    </button>
                                                                    <button
                                                                        type="button"
                                                                        title="Import CSV, JSON, or Markdown from Notion / AppFlowy"
                                                                        on:click=move |_| on_open_import_modal.run(space_id)
                                                                        class="px-2 py-1 rounded text-[11px] font-medium bg-[#06B6D4]/15 text-[#06B6D4] hover:bg-[#06B6D4]/25 border border-[#06B6D4]/30 transition"
                                                                    >
                                                                        "Import"
                                                                    </button>
                                                                </div>

                                                                // Inline New Grid Title Input
                                                                <Show when=move || creating_grid_for_space.get() == Some(space_id)>
                                                                    <div class="p-1.5 rounded bg-slate-900/80 border border-[#2A2F37] flex items-center gap-1">
                                                                        <input
                                                                            type="text"
                                                                            placeholder="Grid title..."
                                                                            class="w-full px-2 py-1 rounded bg-slate-950 text-xs text-white border border-slate-700 focus:outline-none focus:border-[#06B6D4]"
                                                                            prop:value=new_grid_title
                                                                            on:input=move |ev| new_grid_title.set(event_target_value(&ev))
                                                                        />
                                                                        <button
                                                                            type="button"
                                                                            on:click=move |_| submit_new_grid(space_id)
                                                                            class="px-2 py-1 rounded bg-[#F97316] text-slate-950 font-bold text-xs"
                                                                        >
                                                                            "Add"
                                                                        </button>
                                                                    </div>
                                                                </Show>
                                                            </div>
                                                        </Show>
                                                    </div>
                                                </Show>
                                            </div>
                                        }
                                    })
                                    .collect_view()
                                    .into_any()
                            }}
                        </div>
                    </div>

                    // ========================================================
                    // 3. Admin Navigation Section (Conditional Render)
                    // ========================================================
                    <Show when=move || auth.is_admin()>
                        <div class="px-3 pt-4 border-t border-[#2A2F37]/60">
                            <span class="px-1 text-[11px] font-semibold uppercase tracking-wider text-[#F97316]">
                                "Enterprise Admin"
                            </span>
                            <nav class="mt-2 space-y-1">
                                // `/admin/users` (SuperAdmin Only)
                                <Show when=move || auth.is_super_admin()>
                                    <button
                                        type="button"
                                        on:click=move |_| on_navigate.run("/admin/users".into())
                                        class=move || {
                                            let active = current_path.get() == "/admin/users";
                                            let cls = if active {
                                                "bg-[#F97316]/15 text-[#F97316] border-l-2 border-[#F97316] font-semibold"
                                            } else {
                                                "text-slate-400 hover:text-slate-200 hover:bg-slate-500/10"
                                            };
                                            format!("w-full text-left px-2.5 py-2 rounded text-xs flex items-center gap-2.5 transition {cls}")
                                        }
                                    >
                                        <span>"👥"</span>
                                        <span>"Global Users & Roles"</span>
                                    </button>
                                </Show>

                                // `/admin/spaces` (SuperAdmin & Space Admins)
                                <button
                                    type="button"
                                    on:click=move |_| on_navigate.run("/admin/spaces".into())
                                    class=move || {
                                        let active = current_path.get() == "/admin/spaces";
                                        let cls = if active {
                                            "bg-[#06B6D4]/15 text-[#06B6D4] border-l-2 border-[#06B6D4] font-semibold"
                                        } else {
                                            "text-slate-400 hover:text-slate-200 hover:bg-slate-500/10"
                                        };
                                        format!("w-full text-left px-2.5 py-2 rounded text-xs flex items-center gap-2.5 transition {cls}")
                                    }
                                >
                                    <span>"🔐"</span>
                                    <span>"Spaces & Access Matrix"</span>
                                </button>

                                // `/admin/audit` (SuperAdmin Only)
                                <Show when=move || auth.is_super_admin()>
                                    <button
                                        type="button"
                                        on:click=move |_| on_navigate.run("/admin/audit".into())
                                        class=move || {
                                            let active = current_path.get() == "/admin/audit";
                                            let cls = if active {
                                                "bg-[#F97316]/15 text-[#F97316] border-l-2 border-[#F97316] font-semibold"
                                            } else {
                                                "text-slate-400 hover:text-slate-200 hover:bg-slate-500/10"
                                            };
                                            format!("w-full text-left px-2.5 py-2 rounded text-xs flex items-center gap-2.5 transition {cls}")
                                        }
                                    >
                                        <span>"📜"</span>
                                        <span>"System Audit Logs"</span>
                                    </button>
                                </Show>
                            </nav>
                        </div>
                    </Show>
                </Show>
            </div>

            // ================================================================
            // 4. Bottom User Profile & Sign Out Bar
            // ================================================================
            <div class="p-3 border-t border-[#2A2F37]/60">
                <Show
                    when=move || !auth.sidebar_collapsed.get()
                    fallback=move || {
                        view! {
                            <button
                                type="button"
                                title="Sign Out"
                                on:click=move |_| {
                                    auth.clear_session();
                                    on_navigate.run("/login".into());
                                }
                                class="w-full py-1.5 rounded bg-red-500/10 text-red-400 text-xs font-bold"
                            >
                                "⏻"
                            </button>
                        }
                    }
                >
                    <div class="flex items-center justify-between gap-2">
                        <div class="truncate">
                            <div class="text-xs font-semibold truncate">
                                {move || auth.user.get().map(|u| u.display_name).unwrap_or_else(|| "Guest".into())}
                            </div>
                            <div class="text-[11px] text-slate-400 truncate">
                                {move || auth.user.get().map(|u| u.email).unwrap_or_default()}
                            </div>
                        </div>
                        <button
                            type="button"
                            on:click=move |_| {
                                auth.clear_session();
                                on_navigate.run("/login".into());
                            }
                            class="px-2.5 py-1 rounded text-[11px] font-semibold bg-red-500/10 hover:bg-red-500/20 text-red-400 border border-red-500/30 transition shrink-0"
                        >
                            "Sign Out"
                        </button>
                    </div>
                </Show>
            </div>
        </aside>
    }
}

