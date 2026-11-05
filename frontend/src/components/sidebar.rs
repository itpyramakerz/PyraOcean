use crate::{
    api::{
        handle_clipboard_paste_to_ai_attachments, read_file_list_to_ai_attachments, ApiClient,
    },
    components::cell_editor::copy_to_clipboard,
    state::AuthState,
};
use leptos::{prelude::*, task::spawn_local};
use pyra_ocean_shared::{
    generate_qr_code_svg, AiAttachment, AiChatMessage, AiExecutedAction, AiNavigationTarget,
    CreateSpaceRequest, SpaceRole, SpaceWithRole, SystemRole, TotpSetupResponse,
};
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
    let creating_new_space = RwSignal::new(false);
    let new_space_title = RwSignal::new(String::new());
    let creating_grid_for_space = RwSignal::new(Option::<Uuid>::None);
    let new_grid_title = RwSignal::new(String::new());
    let sidebar_error = RwSignal::new(Option::<String>::None);
    let selected_sidebar_grids = RwSignal::new(HashSet::<Uuid>::new());
    let confirming_delete_grid_id = RwSignal::new(Option::<Uuid>::None);
    let confirming_bulk_delete_space_id = RwSignal::new(Option::<Uuid>::None);

    // Self-Service Account Security & 2FA Modal state
    let show_security_modal = RwSignal::new(false);
    let sec_current_password = RwSignal::new(String::new());
    let sec_new_password = RwSignal::new(String::new());
    let sec_totp_setup = RwSignal::new(Option::<TotpSetupResponse>::None);
    let sec_totp_verify_code = RwSignal::new(String::new());
    let sec_disable_password = RwSignal::new(String::new());
    let sec_feedback = RwSignal::new(Option::<(bool, String)>::None);

    // Phase 17: Sidebar Duplicate Grid & Move Grid Modals
    // (source_space_id, source_grid_id, source_grid_title)
    let dup_modal_grid = RwSignal::new(Option::<(Uuid, Uuid, String)>::None);
    let dup_new_name = RwSignal::new(String::new());
    let dup_target_space_id = RwSignal::new(Option::<Uuid>::None);
    let dup_include_rows = RwSignal::new(true);

    // (source_space_id, source_grid_id, source_grid_title)
    let move_modal_grid = RwSignal::new(Option::<(Uuid, Uuid, String)>::None);
    let move_target_space_id = RwSignal::new(Option::<Uuid>::None);

    // Phase 18: Sidebar PyraAI Copilot Modal state
    let show_ai_modal = RwSignal::new(false);
    let ai_prompt_input = RwSignal::new(String::new());
    let ai_mode = RwSignal::new(String::from("ask"));
    let ai_loading = RwSignal::new(false);
    let ai_history = RwSignal::new(Vec::<AiChatMessage>::new());
    let ai_actions_log = RwSignal::new(Vec::<AiExecutedAction>::new());
    let ai_model_badge = RwSignal::new(String::from("qwen2.5:1.5b"));
    let ai_attachments = RwSignal::new(Vec::<AiAttachment>::new());
    let ai_drag_over = RwSignal::new(false);
    let ai_file_input_ref = NodeRef::<leptos::html::Input>::new();

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
                if let Ok(trashed) = ApiClient::list_trashed_grids(&auth).await {
                    auth.trash_count.set(trashed.len());
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

    let submit_new_space = move || {
        let name = new_space_title.get().trim().to_string();
        if name.is_empty() {
            return;
        }
        let slug = name
            .to_lowercase()
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
            .collect::<String>()
            .trim_matches('-')
            .to_string();
        let clean_slug = if slug.is_empty() {
            format!("space-{}", &Uuid::new_v4().to_string()[..8])
        } else {
            slug
        };
        let payload = CreateSpaceRequest {
            slug: clean_slug,
            name,
            description: None,
            icon: Some("folder".into()),
            admin_manager_ids: Vec::new(),
        };
        spawn_local(async move {
            match ApiClient::create_space(&auth, payload).await {
                Ok(created_space) => {
                    creating_new_space.set(false);
                    new_space_title.set(String::new());
                    expanded_spaces.update(|set| {
                        set.insert(created_space.id);
                    });
                    if let Ok(spaces) = ApiClient::list_spaces(&auth).await {
                        auth.spaces.set(spaces);
                    }
                    auth.active_space_id.set(Some(created_space.id));
                }
                Err(e) => sidebar_error.set(Some(e)),
            }
        });
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
                Ok(_) => {
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
                    auth.trash_count.update(|c| *c += 1);

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
                    auth.trash_count.update(|c| *c += deleted_set.len());

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
                                <div class="font-bold text-sm tracking-tight text-[#06B6D4]">
                                    "PyraOcean"
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
                // 1.5 Global Search Trigger Button (Ctrl+K / Cmd+K) + PyraAI Copilot
                // ============================================================
                <div class="px-3 pt-3 space-y-1.5">
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

                    <button
                        type="button"
                        title="PyraAI Copilot (Natural Language Search, Reports & Task Execution)"
                        on:click=move |_| show_ai_modal.set(true)
                        class="w-full px-2.5 py-2 rounded-xl bg-gradient-to-r from-[#06B6D4]/15 to-[#F97316]/15 hover:from-[#06B6D4]/25 hover:to-[#F97316]/25 border border-[#06B6D4]/40 text-xs text-white flex items-center justify-between gap-2 transition shadow-sm"
                    >
                        <div class="flex items-center gap-2 truncate">
                            <span>"✨"</span>
                            <Show when=move || !auth.sidebar_collapsed.get()>
                                <span class="font-semibold truncate">"PyraAI Copilot"</span>
                            </Show>
                        </div>
                        <Show when=move || !auth.sidebar_collapsed.get()>
                            <span class="px-1.5 py-0.5 rounded bg-[#06B6D4]/20 border border-[#06B6D4]/40 text-[9px] font-mono text-[#06B6D4] shrink-0">
                                "AI"
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
                            <Show when=move || auth.is_admin()>
                                <button
                                    type="button"
                                    on:click=move |_| {
                                        creating_new_space.update(|v| *v = !*v);
                                        new_space_title.set(String::new());
                                    }
                                    class="text-[11px] font-semibold text-[#F97316] hover:text-[#EA580C]"
                                >
                                    "+ New Space"
                                </button>
                            </Show>
                        </div>

                        <Show when=move || creating_new_space.get() && auth.is_admin()>
                            <div class="mb-2 p-1.5 rounded-lg bg-slate-900/90 border border-[#2A2F37] flex items-center gap-1">
                                <input
                                    type="text"
                                    placeholder="New Space name..."
                                    class="w-full px-2 py-1 rounded bg-slate-950 text-xs text-white border border-slate-700 focus:outline-none focus:border-[#06B6D4]"
                                    prop:value=new_space_title
                                    on:input=move |ev| new_space_title.set(event_target_value(&ev))
                                    on:keydown=move |ev: web_sys::KeyboardEvent| {
                                        if ev.key() == "Enter" {
                                            ev.prevent_default();
                                            submit_new_space();
                                        } else if ev.key() == "Escape" {
                                            creating_new_space.set(false);
                                        }
                                    }
                                />
                                <button
                                    type="button"
                                    on:click=move |_| submit_new_space()
                                    class="px-2 py-1 rounded bg-[#F97316] hover:bg-[#EA580C] text-slate-950 font-bold text-xs shrink-0"
                                >
                                    "Add"
                                </button>
                            </div>
                        </Show>

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
                                                                    let gtitle_dup = grid.title.clone();
                                                                    let gtitle_move = grid.title.clone();
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
                                                                                {
                                                                                    let dup_title = gtitle_dup.clone();
                                                                                    let move_title = gtitle_move.clone();
                                                                                    move || {
                                                                                        let dt = dup_title.clone();
                                                                                        let mt = move_title.clone();
                                                                                        if confirming_delete_grid_id.get() == Some(gid) {
                                                                                            view! {
                                                                                                <div
                                                                                                    class="flex items-center gap-1 shrink-0"
                                                                                                    on:click=move |ev| ev.stop_propagation()
                                                                                                >
                                                                                                    <button
                                                                                                        type="button"
                                                                                                        title="Move grid to Trash? You can restore it within 30 days."
                                                                                                        on:click=move |ev| {
                                                                                                            ev.stop_propagation();
                                                                                                            handle_single_delete_grid(space_id, gid);
                                                                                                        }
                                                                                                        class="px-1.5 py-0.5 rounded bg-red-600 hover:bg-red-500 text-white text-[10px] font-bold"
                                                                                                    >
                                                                                                        "Trash"
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
                                                                                                <div class="opacity-0 group-hover/grid:opacity-100 flex items-center gap-0.5 transition shrink-0">
                                                                                                    <button
                                                                                                        type="button"
                                                                                                        title="Duplicate Grid"
                                                                                                        on:click=move |ev| {
                                                                                                            ev.stop_propagation();
                                                                                                            dup_new_name.set(format!("{} (Copy)", dt));
                                                                                                            dup_target_space_id.set(Some(space_id));
                                                                                                            dup_include_rows.set(true);
                                                                                                            dup_modal_grid.set(Some((space_id, gid, dt.clone())));
                                                                                                        }
                                                                                                        class="p-0.5 rounded hover:bg-[#06B6D4]/20 text-slate-400 hover:text-[#06B6D4] text-[11px]"
                                                                                                    >
                                                                                                        "📑"
                                                                                                    </button>
                                                                                                    <button
                                                                                                        type="button"
                                                                                                        title="Move to Space..."
                                                                                                        on:click=move |ev| {
                                                                                                            ev.stop_propagation();
                                                                                                            move_target_space_id.set(Some(space_id));
                                                                                                            move_modal_grid.set(Some((space_id, gid, mt.clone())));
                                                                                                        }
                                                                                                        class="p-0.5 rounded hover:bg-[#F97316]/20 text-slate-400 hover:text-[#F97316] text-[11px]"
                                                                                                    >
                                                                                                        "📂"
                                                                                                    </button>
                                                                                                    <button
                                                                                                        type="button"
                                                                                                        title="Delete Grid"
                                                                                                        on:click=move |ev| {
                                                                                                            ev.stop_propagation();
                                                                                                            confirming_delete_grid_id.set(Some(gid));
                                                                                                        }
                                                                                                        class="p-0.5 rounded hover:bg-red-500/20 text-slate-400 hover:text-red-400 text-[11px]"
                                                                                                    >
                                                                                                        "🗑️"
                                                                                                    </button>
                                                                                                </div>
                                                                                            }
                                                                                            .into_any()
                                                                                        }
                                                                                    }
                                                                                }
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
                                                                                                <div class="space-y-1.5">
                                                                                                    <p class="text-[10px] text-red-200 leading-tight">
                                                                                                        "Move grid to Trash? You can restore it within 30 days."
                                                                                                    </p>
                                                                                                    <div class="flex items-center gap-1">
                                                                                                        <button
                                                                                                            type="button"
                                                                                                            on:click=move |_| {
                                                                                                                handle_bulk_delete_space_grids(space_id, ids_to_del.clone());
                                                                                                            }
                                                                                                            class="flex-1 py-1 rounded bg-red-600 hover:bg-red-500 text-white text-[11px] font-bold transition"
                                                                                                        >
                                                                                                            "Move to Trash"
                                                                                                        </button>
                                                                                                        <button
                                                                                                            type="button"
                                                                                                            on:click=move |_| confirming_bulk_delete_space_id.set(None)
                                                                                                            class="px-2 py-1 rounded bg-slate-800 hover:bg-slate-700 text-slate-300 text-[11px]"
                                                                                                        >
                                                                                                            "Cancel"
                                                                                                        </button>
                                                                                                    </div>
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
                    // 2.5 Notion-Style Trash Navigation Link (`/trash`)
                    // ========================================================
                    <div class="px-3 pt-3 border-t border-[#2A2F37]/60">
                        <button
                            type="button"
                            on:click=move |_| {
                                auth.active_grid_id.set(None);
                                on_navigate.run("/trash".into());
                            }
                            class=move || {
                                let active = current_path.get() == "/trash";
                                let cls = if active {
                                    "bg-red-500/15 text-red-300 border-l-2 border-red-500 font-semibold"
                                } else {
                                    "text-slate-400 hover:text-slate-200 hover:bg-slate-500/10"
                                };
                                format!("w-full text-left px-2.5 py-2 rounded-lg text-xs flex items-center justify-between gap-2 transition {cls}")
                            }
                        >
                            <div class="flex items-center gap-2.5">
                                <span>"🗑️"</span>
                                <span>"Trash"</span>
                            </div>
                            {move || {
                                let count = auth.trash_count.get();
                                if count > 0 {
                                    view! {
                                        <span class="px-1.5 py-0.5 rounded-full bg-red-500/20 border border-red-500/40 text-red-300 font-mono text-[10px] font-bold">
                                            {count}
                                        </span>
                                    }
                                    .into_any()
                                } else {
                                    view! { <span /> }.into_any()
                                }
                            }}
                        </button>
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
                                // `/admin/users` (SuperAdmin & Admin)
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

                                    <button
                                        type="button"
                                        on:click=move |_| on_navigate.run("/admin/backup".into())
                                        class=move || {
                                            let active = current_path.get() == "/admin/backup";
                                            let cls = if active {
                                                "bg-[#06B6D4]/15 text-[#06B6D4] border-l-2 border-[#06B6D4] font-semibold"
                                            } else {
                                                "text-slate-400 hover:text-slate-200 hover:bg-slate-500/10"
                                            };
                                            format!("w-full text-left px-2.5 py-2 rounded text-xs flex items-center gap-2.5 transition {cls}")
                                        }
                                    >
                                        <span>"💾"</span>
                                        <span>"Backup & Restore"</span>
                                    </button>
                                </Show>
                            </nav>
                        </div>
                    </Show>
                </Show>
            </div>

            // ================================================================
            // 4. Bottom User Profile, Security & 2FA Settings & Sign Out Bar
            // ================================================================
            <div class="p-3 border-t border-[#2A2F37]/60 space-y-2">
                <Show
                    when=move || !auth.sidebar_collapsed.get()
                    fallback=move || {
                        view! {
                            <div class="space-y-1">
                                <button
                                    type="button"
                                    title="Account Security & 2FA"
                                    on:click=move |_| {
                                        sec_feedback.set(None);
                                        sec_totp_setup.set(None);
                                        show_security_modal.set(true);
                                    }
                                    class="w-full py-1.5 rounded bg-[#06B6D4]/15 text-[#06B6D4] text-xs font-bold"
                                >
                                    "🛡️"
                                </button>
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
                            </div>
                        }
                    }
                >
                    <div class="space-y-2">
                        <div class="flex items-center justify-between gap-2">
                            <div class="truncate">
                                <div class="text-xs font-semibold truncate flex items-center gap-1.5">
                                    <span>
                                        {move || auth.user.get().map(|u| u.display_name).unwrap_or_else(|| "Guest".into())}
                                    </span>
                                    {move || {
                                        if auth.user.get().map(|u| u.totp_enabled).unwrap_or(false) {
                                            view! {
                                                <span
                                                    title="Two-Factor Authentication (2FA) Active"
                                                    class="px-1 py-0.2 rounded bg-emerald-500/15 border border-emerald-500/40 text-emerald-300 text-[9px] font-bold"
                                                >
                                                    "2FA"
                                                </span>
                                            }
                                            .into_any()
                                        } else {
                                            view! { <span /> }.into_any()
                                        }
                                    }}
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

                        <button
                            type="button"
                            on:click=move |_| {
                                sec_feedback.set(None);
                                sec_totp_setup.set(None);
                                show_security_modal.set(true);
                            }
                            class="w-full py-1.5 px-2.5 rounded-lg bg-[#1E2227] hover:bg-slate-800 border border-[#2A2F37] text-[11px] text-slate-300 flex items-center justify-between transition"
                        >
                            <span>"🛡️ Password & 2FA Security"</span>
                            <span class="text-[#06B6D4]">"Configure →"</span>
                        </button>
                    </div>
                </Show>

                // Self-Service Account Security & 2FA Modal
                <Show when=move || show_security_modal.get()>
                    <div class="fixed inset-0 z-50 bg-black/75 flex items-center justify-center p-4">
                        <div class="w-full max-w-lg max-h-[90vh] overflow-y-auto bg-[#1E2227] border border-[#2A2F37] rounded-2xl p-6 shadow-2xl space-y-5 text-left">
                            <div class="flex items-center justify-between border-b border-[#2A2F37] pb-3">
                                <div>
                                    <h3 class="text-base font-bold text-white">
                                        "🛡️ Account Password & Two-Factor Authentication"
                                    </h3>
                                    <p class="text-xs text-slate-400">
                                        "Manage your password and TOTP Authenticator (Google Authenticator / Authy / 1Password)"
                                    </p>
                                </div>
                                <button
                                    type="button"
                                    on:click=move |_| show_security_modal.set(false)
                                    class="text-slate-400 hover:text-white"
                                >
                                    "✕"
                                </button>
                            </div>

                            {move || {
                                sec_feedback.get().map(|(ok, msg)| {
                                    let cls = if ok {
                                        "bg-emerald-500/15 border-emerald-500/40 text-emerald-300"
                                    } else {
                                        "bg-red-500/15 border-red-500/40 text-red-300"
                                    };
                                    view! {
                                        <div class=format!("p-3 rounded-xl border text-xs {cls}")>
                                            {msg}
                                        </div>
                                    }
                                })
                            }}

                            // Section 1: Change Own Password
                            <form
                                class="p-4 rounded-xl bg-[#14171A] border border-[#2A2F37] space-y-3"
                                on:submit=move |ev: web_sys::SubmitEvent| {
                                    ev.prevent_default();
                                    let cur = sec_current_password.get();
                                    let next = sec_new_password.get();
                                    spawn_local(async move {
                                        match ApiClient::change_own_password(&auth, cur, next).await {
                                            Ok(_) => {
                                                sec_current_password.set(String::new());
                                                sec_new_password.set(String::new());
                                                sec_feedback.set(Some((true, "Password updated successfully.".into())));
                                            }
                                            Err(e) => sec_feedback.set(Some((false, e))),
                                        }
                                    });
                                }
                            >
                                <div class="text-xs font-bold text-white">"🔑 Change Account Password"</div>
                                <div class="grid grid-cols-1 sm:grid-cols-2 gap-2.5">
                                    <input
                                        type="password"
                                        required
                                        placeholder="Current password"
                                        class="px-3 py-2 rounded-lg bg-[#1E2227] border border-[#2A2F37] text-xs text-white"
                                        prop:value=sec_current_password
                                        on:input=move |ev| sec_current_password.set(event_target_value(&ev))
                                    />
                                    <input
                                        type="password"
                                        required
                                        placeholder="New password (min 8 chars)"
                                        class="px-3 py-2 rounded-lg bg-[#1E2227] border border-[#2A2F37] text-xs text-white"
                                        prop:value=sec_new_password
                                        on:input=move |ev| sec_new_password.set(event_target_value(&ev))
                                    />
                                </div>
                                <div class="flex justify-end">
                                    <button
                                        type="submit"
                                        class="px-3.5 py-1.5 rounded-lg bg-[#F97316] hover:bg-[#EA580C] text-white text-xs font-semibold"
                                    >
                                        "Update Password"
                                    </button>
                                </div>
                            </form>

                            // Section 2: Two-Factor Authentication (TOTP)
                            <div class="p-4 rounded-xl bg-[#14171A] border border-[#2A2F37] space-y-3">
                                <div class="flex items-center justify-between">
                                    <div>
                                        <div class="text-xs font-bold text-white">
                                            "🔒 Two-Factor Authentication (2FA / TOTP)"
                                        </div>
                                        <div class="text-[11px] text-slate-400">
                                            "Protect your account and enable instant self-service password recovery"
                                        </div>
                                    </div>
                                    {move || {
                                        let enabled = auth.user.get().map(|u| u.totp_enabled).unwrap_or(false);
                                        if enabled {
                                            view! {
                                                <span class="px-2 py-0.5 rounded bg-emerald-500/15 border border-emerald-500/40 text-emerald-300 text-[11px] font-bold">
                                                    "Active"
                                                </span>
                                            }
                                            .into_any()
                                        } else {
                                            view! {
                                                <span class="px-2 py-0.5 rounded bg-slate-800 border border-slate-700 text-slate-400 text-[11px]">
                                                    "Disabled"
                                                </span>
                                            }
                                            .into_any()
                                        }
                                    }}
                                </div>

                                {move || {
                                    let is_enabled = auth.user.get().map(|u| u.totp_enabled).unwrap_or(false);
                                    if is_enabled {
                                        view! {
                                            <form
                                                class="space-y-2.5 pt-2 border-t border-[#2A2F37]"
                                                on:submit=move |ev: web_sys::SubmitEvent| {
                                                    ev.prevent_default();
                                                    let pwd = sec_disable_password.get();
                                                    spawn_local(async move {
                                                        match ApiClient::disable_totp(&auth, pwd).await {
                                                            Ok(me) => {
                                                                sec_disable_password.set(String::new());
                                                                auth.user.set(Some(me));
                                                                sec_feedback.set(Some((true, "Two-Factor Authentication disabled.".into())));
                                                            }
                                                            Err(e) => sec_feedback.set(Some((false, e))),
                                                        }
                                                    });
                                                }
                                            >
                                                <div class="text-[11px] text-slate-300">
                                                    "Enter your current password to disable 2FA:"
                                                </div>
                                                <div class="flex items-center gap-2">
                                                    <input
                                                        type="password"
                                                        required
                                                        placeholder="Current password..."
                                                        class="flex-1 px-3 py-1.5 rounded-lg bg-[#1E2227] border border-[#2A2F37] text-xs text-white"
                                                        prop:value=sec_disable_password
                                                        on:input=move |ev| sec_disable_password.set(event_target_value(&ev))
                                                    />
                                                    <button
                                                        type="submit"
                                                        class="px-3 py-1.5 rounded-lg bg-red-500/20 hover:bg-red-500/30 border border-red-500/40 text-red-300 text-xs font-semibold"
                                                    >
                                                        "Disable 2FA"
                                                    </button>
                                                </div>
                                            </form>
                                        }
                                        .into_any()
                                    } else {
                                        view! {
                                            <div class="space-y-3 pt-2">
                                                <button
                                                    type="button"
                                                    on:click=move |_| {
                                                        spawn_local(async move {
                                                            match ApiClient::setup_totp(&auth).await {
                                                                Ok(res) => sec_totp_setup.set(Some(res)),
                                                                Err(e) => sec_feedback.set(Some((false, e))),
                                                            }
                                                        });
                                                    }
                                                    class="px-3.5 py-1.5 rounded-lg bg-[#06B6D4] hover:bg-[#0284C7] text-slate-950 text-xs font-bold transition"
                                                >
                                                    "Generate 2FA Secret & Recovery Code"
                                                </button>

                                                {move || {
                                                    sec_totp_setup.get().map(|setup| {
                                                        let qr_svg = generate_qr_code_svg(&setup.otpauth_uri);
                                                        let sec_copy = setup.secret.clone();
                                                        let rec_copy = setup.recovery_code.clone();
                                                        view! {
                                                            <div class="p-3 rounded-xl bg-[#1E2227] border border-[#06B6D4]/40 space-y-3">
                                                                <div class="flex flex-col sm:flex-row items-center gap-3 p-2.5 rounded-lg bg-[#14171A] border border-[#2A2F37]">
                                                                    <div class="w-36 h-36 p-2 rounded-lg bg-white shadow shrink-0 flex items-center justify-center">
                                                                        <div class="w-full h-full" inner_html=qr_svg />
                                                                    </div>
                                                                    <div class="text-[11px] text-slate-300 space-y-1">
                                                                        <div class="font-bold text-white">
                                                                            "Scan with Google Authenticator"
                                                                        </div>
                                                                        <p>
                                                                            "Open Google Authenticator on your phone, tap '+' -> 'Scan a QR code', or copy the manual Base32 setup key below."
                                                                        </p>
                                                                    </div>
                                                                </div>

                                                                <div>
                                                                    <div class="text-[11px] text-slate-400">
                                                                        "1. Base32 Secret Key (Manual Setup):"
                                                                    </div>
                                                                    <div class="mt-1 flex items-center justify-between gap-2 p-2 rounded bg-[#14171A] border border-slate-700 font-mono text-xs text-[#06B6D4] select-all">
                                                                        <span class="break-all">{setup.secret}</span>
                                                                        <button
                                                                            type="button"
                                                                            on:click=move |_| {
                                                                                let _ = copy_to_clipboard(&sec_copy);
                                                                            }
                                                                            class="px-2 py-0.5 rounded bg-slate-800 hover:bg-slate-700 text-slate-200 text-[10px] font-sans shrink-0"
                                                                        >
                                                                            "📋 Copy Setup Key"
                                                                        </button>
                                                                    </div>
                                                                </div>

                                                                <div>
                                                                    <div class="text-[11px] text-amber-300 font-semibold">
                                                                        "2. Save your Emergency Recovery Code (for self-service password reset):"
                                                                    </div>
                                                                    <div class="mt-1 flex items-center justify-between gap-2 p-2 rounded bg-[#14171A] border border-amber-500/40 font-mono text-xs text-amber-300 select-all">
                                                                        <span>{setup.recovery_code}</span>
                                                                        <button
                                                                            type="button"
                                                                            on:click=move |_| {
                                                                                let _ = copy_to_clipboard(&rec_copy);
                                                                            }
                                                                            class="px-2 py-0.5 rounded bg-slate-800 hover:bg-slate-700 text-slate-200 text-[10px] font-sans shrink-0"
                                                                        >
                                                                            "📋 Copy Recovery Code"
                                                                        </button>
                                                                    </div>
                                                                </div>

                                                                <form
                                                                    class="space-y-2 pt-2 border-t border-[#2A2F37]"
                                                                    on:submit=move |ev: web_sys::SubmitEvent| {
                                                                        ev.prevent_default();
                                                                        let code = sec_totp_verify_code.get();
                                                                        spawn_local(async move {
                                                                            match ApiClient::enable_totp(&auth, code).await {
                                                                                Ok(me) => {
                                                                                    sec_totp_verify_code.set(String::new());
                                                                                    sec_totp_setup.set(None);
                                                                                    auth.user.set(Some(me));
                                                                                    sec_feedback.set(Some((
                                                                                        true,
                                                                                        "Two-Factor Authentication (2FA) is now active!".into(),
                                                                                    )));
                                                                                }
                                                                                Err(e) => sec_feedback.set(Some((false, e))),
                                                                            }
                                                                        });
                                                                    }
                                                                >
                                                                    <label class="block text-[11px] text-slate-300">
                                                                        "3. Enter the 6-digit code from your Authenticator app to activate:"
                                                                    </label>
                                                                    <div class="flex items-center gap-2">
                                                                        <input
                                                                            type="text"
                                                                            required
                                                                            placeholder="123456"
                                                                            class="flex-1 px-3 py-1.5 rounded-lg bg-[#14171A] border border-[#2A2F37] text-xs font-mono text-white"
                                                                            prop:value=sec_totp_verify_code
                                                                            on:input=move |ev| sec_totp_verify_code.set(event_target_value(&ev))
                                                                        />
                                                                        <button
                                                                            type="submit"
                                                                            class="px-4 py-1.5 rounded-lg bg-emerald-500 hover:bg-emerald-600 text-slate-950 text-xs font-bold"
                                                                        >
                                                                            "Verify & Enable 2FA"
                                                                        </button>
                                                                    </div>
                                                                </form>
                                                            </div>
                                                        }
                                                    })
                                                }}
                                            </div>
                                        }
                                        .into_any()
                                    }
                                }}
                            </div>
                        </div>
                    </div>
                </Show>

                // Phase 17: Sidebar Duplicate Grid Modal (`📑 Duplicate Grid`)
                <Show when=move || dup_modal_grid.get().is_some()>
                    <div class="fixed inset-0 z-50 bg-black/75 flex items-center justify-center p-4">
                        <div class="w-full max-w-md bg-[#1E2227] border border-[#2A2F37] rounded-2xl p-6 shadow-2xl space-y-4 text-left">
                            {move || {
                                let Some((src_sid, src_gid, src_title)) = dup_modal_grid.get() else {
                                    return view! { <div /> }.into_any();
                                };
                                view! {
                                    <div class="space-y-4">
                                        <div class="flex items-center justify-between border-b border-[#2A2F37] pb-3">
                                            <div>
                                                <h3 class="text-sm font-bold text-white">
                                                    "📑 Duplicate Grid"
                                                </h3>
                                                <p class="text-[11px] text-slate-400 truncate">
                                                    "Source: " {src_title}
                                                </p>
                                            </div>
                                            <button
                                                type="button"
                                                on:click=move |_| dup_modal_grid.set(None)
                                                class="text-slate-400 hover:text-white"
                                            >
                                                "✕"
                                            </button>
                                        </div>

                                        <form
                                            class="space-y-3"
                                            on:submit=move |ev: web_sys::SubmitEvent| {
                                                ev.prevent_default();
                                                let name_opt = {
                                                    let n = dup_new_name.get().trim().to_string();
                                                    if n.is_empty() { None } else { Some(n) }
                                                };
                                                let dst_sid = dup_target_space_id.get().unwrap_or(src_sid);
                                                let inc_rows = dup_include_rows.get();

                                                spawn_local(async move {
                                                    match ApiClient::duplicate_grid(&auth, src_gid, Some(dst_sid), name_opt, inc_rows).await {
                                                        Ok(created) => {
                                                            let new_gid = created.id;
                                                            expanded_spaces.update(|s| {
                                                                s.insert(dst_sid);
                                                            });
                                                            if let Ok(grids) = ApiClient::list_space_grids(&auth, dst_sid).await {
                                                                auth.space_grids.update(|map| {
                                                                    map.insert(dst_sid, grids);
                                                                });
                                                            }
                                                            dup_modal_grid.set(None);
                                                            auth.active_space_id.set(Some(dst_sid));
                                                            auth.active_grid_id.set(Some(new_gid));
                                                            on_navigate.run(format!("/spaces/{dst_sid}/grids/{new_gid}"));
                                                        }
                                                        Err(e) => sidebar_error.set(Some(e)),
                                                    }
                                                });
                                            }
                                        >
                                            <div>
                                                <label class="block text-xs text-slate-300 mb-1">"New Grid Name"</label>
                                                <input
                                                    type="text"
                                                    required
                                                    class="w-full px-3 py-2 rounded-lg bg-[#14171A] border border-[#2A2F37] text-xs text-white focus:outline-none focus:border-[#06B6D4]"
                                                    prop:value=dup_new_name
                                                    on:input=move |ev| dup_new_name.set(event_target_value(&ev))
                                                />
                                            </div>

                                            <div>
                                                <label class="block text-xs text-slate-300 mb-1">"Destination Space"</label>
                                                <select
                                                    class="w-full px-3 py-2 rounded-lg bg-[#14171A] border border-[#2A2F37] text-xs text-white focus:outline-none focus:border-[#06B6D4]"
                                                    on:change=move |ev| {
                                                        if let Ok(parsed) = Uuid::parse_str(&event_target_value(&ev)) {
                                                            dup_target_space_id.set(Some(parsed));
                                                        }
                                                    }
                                                >
                                                    {move || {
                                                        let current_dst = dup_target_space_id.get().unwrap_or(src_sid);
                                                        auth.spaces
                                                            .get()
                                                            .into_iter()
                                                            .filter(|s| {
                                                                auth.is_super_admin()
                                                                    || matches!(s.effective_role, SpaceRole::SpaceAdmin | SpaceRole::Editor)
                                                            })
                                                            .map(|s| {
                                                                let sid = s.space.id;
                                                                view! {
                                                                    <option value=sid.to_string() selected=sid == current_dst>
                                                                        {s.space.name}
                                                                    </option>
                                                                }
                                                            })
                                                            .collect_view()
                                                    }}
                                                </select>
                                            </div>

                                            <label class="flex items-center gap-2 text-xs text-slate-200 cursor-pointer pt-1">
                                                <input
                                                    type="checkbox"
                                                    class="rounded accent-[#06B6D4]"
                                                    prop:checked=move || dup_include_rows.get()
                                                    on:change=move |ev| dup_include_rows.set(event_target_checked(&ev))
                                                />
                                                <span>"Include all existing rows & cell values"</span>
                                            </label>

                                            <div class="pt-3 flex justify-end gap-2">
                                                <button
                                                    type="button"
                                                    on:click=move |_| dup_modal_grid.set(None)
                                                    class="px-3 py-1.5 rounded-lg border border-[#2A2F37] text-xs text-slate-300"
                                                >
                                                    "Cancel"
                                                </button>
                                                <button
                                                    type="submit"
                                                    class="px-4 py-1.5 rounded-lg bg-[#06B6D4] hover:bg-[#0284C7] text-slate-950 font-bold text-xs shadow"
                                                >
                                                    "📑 Duplicate Grid"
                                                </button>
                                            </div>
                                        </form>
                                    </div>
                                }
                                .into_any()
                            }}
                        </div>
                    </div>
                </Show>

                // Phase 17: Sidebar Move Grid to Space Modal (`📂 Move to Space...`)
                <Show when=move || move_modal_grid.get().is_some()>
                    <div class="fixed inset-0 z-50 bg-black/75 flex items-center justify-center p-4">
                        <div class="w-full max-w-md bg-[#1E2227] border border-[#2A2F37] rounded-2xl p-6 shadow-2xl space-y-4 text-left">
                            {move || {
                                let Some((src_sid, src_gid, src_title)) = move_modal_grid.get() else {
                                    return view! { <div /> }.into_any();
                                };
                                view! {
                                    <div class="space-y-4">
                                        <div class="flex items-center justify-between border-b border-[#2A2F37] pb-3">
                                            <div>
                                                <h3 class="text-sm font-bold text-white">
                                                    "📂 Move Grid to Another Space"
                                                </h3>
                                                <p class="text-[11px] text-slate-400 truncate">
                                                    "Grid: " {src_title}
                                                </p>
                                            </div>
                                            <button
                                                type="button"
                                                on:click=move |_| move_modal_grid.set(None)
                                                class="text-slate-400 hover:text-white"
                                            >
                                                "✕"
                                            </button>
                                        </div>

                                        <form
                                            class="space-y-3"
                                            on:submit=move |ev: web_sys::SubmitEvent| {
                                                ev.prevent_default();
                                                let Some(dst_sid) = move_target_space_id.get() else {
                                                    return;
                                                };
                                                if dst_sid == src_sid {
                                                    move_modal_grid.set(None);
                                                    return;
                                                }
                                                spawn_local(async move {
                                                    match ApiClient::move_grid(&auth, src_gid, dst_sid).await {
                                                        Ok(moved) => {
                                                            expanded_spaces.update(|s| {
                                                                s.insert(dst_sid);
                                                            });
                                                            if let Ok(src_grids) = ApiClient::list_space_grids(&auth, src_sid).await {
                                                                auth.space_grids.update(|map| {
                                                                    map.insert(src_sid, src_grids);
                                                                });
                                                            }
                                                            if let Ok(dst_grids) = ApiClient::list_space_grids(&auth, dst_sid).await {
                                                                auth.space_grids.update(|map| {
                                                                    map.insert(dst_sid, dst_grids);
                                                                });
                                                            }
                                                            move_modal_grid.set(None);
                                                            auth.active_space_id.set(Some(dst_sid));
                                                            auth.active_grid_id.set(Some(moved.id));
                                                            on_navigate.run(format!("/spaces/{dst_sid}/grids/{}", moved.id));
                                                        }
                                                        Err(e) => sidebar_error.set(Some(e)),
                                                    }
                                                });
                                            }
                                        >
                                            <div>
                                                <label class="block text-xs text-slate-300 mb-1">
                                                    "Select Destination Space"
                                                </label>
                                                <select
                                                    class="w-full px-3 py-2 rounded-lg bg-[#14171A] border border-[#2A2F37] text-xs text-white focus:outline-none focus:border-[#F97316]"
                                                    on:change=move |ev| {
                                                        if let Ok(parsed) = Uuid::parse_str(&event_target_value(&ev)) {
                                                            move_target_space_id.set(Some(parsed));
                                                        }
                                                    }
                                                >
                                                    {move || {
                                                        let current_dst = move_target_space_id.get().unwrap_or(src_sid);
                                                        auth.spaces
                                                            .get()
                                                            .into_iter()
                                                            .filter(|s| {
                                                                auth.is_super_admin()
                                                                    || matches!(s.effective_role, SpaceRole::SpaceAdmin | SpaceRole::Editor)
                                                            })
                                                            .map(|s| {
                                                                let sid = s.space.id;
                                                                view! {
                                                                    <option value=sid.to_string() selected=sid == current_dst>
                                                                        {format!("{}{}", s.space.name, if sid == src_sid { " (Current Space)" } else { "" })}
                                                                    </option>
                                                                }
                                                            })
                                                            .collect_view()
                                                    }}
                                                </select>
                                            </div>

                                            <div class="pt-3 flex justify-end gap-2">
                                                <button
                                                    type="button"
                                                    on:click=move |_| move_modal_grid.set(None)
                                                    class="px-3 py-1.5 rounded-lg border border-[#2A2F37] text-xs text-slate-300"
                                                >
                                                    "Cancel"
                                                </button>
                                                <button
                                                    type="submit"
                                                    class="px-4 py-1.5 rounded-lg bg-[#F97316] hover:bg-[#EA580C] text-white font-bold text-xs shadow"
                                                >
                                                    "📂 Move Grid"
                                                </button>
                                            </div>
                                        </form>
                                    </div>
                                }
                                .into_any()
                            }}
                        </div>
                    </div>
                </Show>

                // Phase 18: Workspace PyraAI Copilot Modal
                <Show when=move || show_ai_modal.get()>
                    <div class="fixed inset-0 z-50 bg-black/75 flex items-center justify-center p-4">
                        <div
                            on:dragover=move |ev: web_sys::DragEvent| {
                                ev.prevent_default();
                                ai_drag_over.set(true);
                            }
                            on:dragleave=move |ev: web_sys::DragEvent| {
                                ev.prevent_default();
                                ai_drag_over.set(false);
                            }
                            on:drop=move |ev: web_sys::DragEvent| {
                                ev.prevent_default();
                                ai_drag_over.set(false);
                                if let Some(dt) = ev.data_transfer() {
                                    read_file_list_to_ai_attachments(dt.files(), ai_attachments);
                                }
                            }
                            on:paste=move |ev: web_sys::Event| {
                                let _ = handle_clipboard_paste_to_ai_attachments(&ev, ai_attachments);
                            }
                            class=move || {
                                let ring_cls = if ai_drag_over.get() {
                                    "border-[#06B6D4] ring-2 ring-[#06B6D4]/50"
                                } else {
                                    "border-[#2A2F37]"
                                };
                                format!("w-full max-w-2xl bg-[#1E2227] border {ring_cls} rounded-2xl p-6 shadow-2xl space-y-4 text-left flex flex-col max-h-[85vh]")
                            }
                        >
                            <div class="flex items-center justify-between border-b border-[#2A2F37] pb-3 shrink-0">
                                <div class="flex items-center gap-2.5">
                                    <span class="text-lg">"✨"</span>
                                    <div>
                                        <h3 class="text-sm font-bold text-white flex flex-wrap items-center gap-2">
                                            <span>"PyraAI Workspace Copilot"</span>
                                            <span class="px-2 py-0.5 rounded bg-[#06B6D4]/15 border border-[#06B6D4]/40 text-[10px] font-mono text-[#06B6D4]">
                                                {move || ai_model_badge.get()}
                                            </span>
                                            {move || {
                                                if ai_loading.get() {
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
                                        </h3>
                                        <p class="text-[11px] text-slate-400">
                                            "Natural language search, executive reports, and autonomous Grid & Row task execution"
                                        </p>
                                    </div>
                                </div>
                                <button
                                    type="button"
                                    on:click=move |_| show_ai_modal.set(false)
                                    class="text-slate-400 hover:text-white"
                                >
                                    "✕"
                                </button>
                            </div>

                            // Mode Selector Pills
                            <div class="flex flex-wrap items-center gap-2 shrink-0">
                                <button
                                    type="button"
                                    on:click=move |_| ai_mode.set("ask".into())
                                    class=move || {
                                        let active = ai_mode.get() == "ask";
                                        let cls = if active {
                                            "bg-[#06B6D4] text-slate-950 font-bold"
                                        } else {
                                            "bg-[#14171A] text-slate-300 border border-[#2A2F37]"
                                        };
                                        format!("px-3 py-1.5 rounded-lg text-xs transition {cls}")
                                    }
                                >
                                    "💬 Ask & Search"
                                </button>
                                <button
                                    type="button"
                                    on:click=move |_| ai_mode.set("report".into())
                                    class=move || {
                                        let active = ai_mode.get() == "report";
                                        let cls = if active {
                                            "bg-[#06B6D4] text-slate-950 font-bold"
                                        } else {
                                            "bg-[#14171A] text-slate-300 border border-[#2A2F37]"
                                        };
                                        format!("px-3 py-1.5 rounded-lg text-xs transition {cls}")
                                    }
                                >
                                    "📊 Executive Report"
                                </button>
                                <button
                                    type="button"
                                    on:click=move |_| ai_mode.set("execute".into())
                                    class=move || {
                                        let active = ai_mode.get() == "execute";
                                        let cls = if active {
                                            "bg-[#F97316] text-white font-bold"
                                        } else {
                                            "bg-[#14171A] text-slate-300 border border-[#2A2F37]"
                                        };
                                        format!("px-3 py-1.5 rounded-lg text-xs transition {cls}")
                                    }
                                >
                                    "⚡ Execute Task (Create Grid / Add Row / Column)"
                                </button>
                            </div>

                            // Quick Prompt Suggestions
                            <div class="flex flex-wrap gap-1.5 shrink-0">
                                <button
                                    type="button"
                                    on:click=move |_| {
                                        ai_mode.set("execute".into());
                                        ai_prompt_input.set("Create a grid called Q4 Operations Budget".into());
                                    }
                                    class="px-2.5 py-1 rounded-lg bg-[#14171A] hover:bg-slate-800 border border-[#2A2F37] text-[11px] text-slate-300"
                                >
                                    "🗂️ Create a grid called Q4 Operations Budget"
                                </button>
                                <button
                                    type="button"
                                    on:click=move |_| {
                                        ai_mode.set("report".into());
                                        ai_prompt_input.set("Generate an executive summary with EGP/USD totals and department breakdown".into());
                                    }
                                    class="px-2.5 py-1 rounded-lg bg-[#14171A] hover:bg-slate-800 border border-[#2A2F37] text-[11px] text-slate-300"
                                >
                                    "📊 Executive Summary & Currency Totals"
                                </button>
                            </div>

                            // Conversation Transcript & Live Working Card
                            <div class="flex-1 overflow-y-auto space-y-3 p-3 rounded-xl bg-[#14171A] border border-[#2A2F37] min-h-[220px]">
                                {move || {
                                    let msgs = ai_history.get();
                                    let is_working = ai_loading.get();
                                    if msgs.is_empty() && !is_working {
                                        return view! {
                                            <div class="py-10 text-center text-xs text-slate-400">
                                                "Ask PyraAI anything about your Grids, generate an Executive Report, or instruct it to create Grids, Columns, and Rows."
                                            </div>
                                        }
                                        .into_any();
                                    }
                                    msgs.into_iter()
                                        .map(|m| {
                                            let is_user = m.role == "user";
                                            let box_cls = if is_user {
                                                "bg-[#F97316]/15 border-[#F97316]/40 text-white ml-8"
                                            } else {
                                                "bg-[#1E2227] border-[#06B6D4]/40 text-slate-200 mr-4"
                                            };
                                            let msg_atts = m.attachments.clone();
                                            let nav_targets = m.navigation_targets.clone();
                                            view! {
                                                <div class=format!("p-3 rounded-xl border text-xs whitespace-pre-wrap leading-relaxed {box_cls}")>
                                                    <div class="text-[10px] font-bold uppercase tracking-wider text-slate-400 mb-1">
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
                                                                                    <span class="text-[10px] text-slate-300 max-w-[130px] truncate">{att.filename}</span>
                                                                                </div>
                                                                            }
                                                                            .into_any()
                                                                        } else {
                                                                            view! {
                                                                                <div class="px-2 py-1 rounded-lg bg-slate-900/80 border border-slate-700/70 text-[10px] text-cyan-300 flex items-center gap-1">
                                                                                    <span>"📄"</span>
                                                                                    <span class="max-w-[160px] truncate">{att.filename}</span>
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
                                                                    {format!("🧭 Matched Locations Across Workspace ({}) — Click to Open:", nav_targets.len())}
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
                                                                                        expanded_spaces.update(|set| {
                                                                                            set.insert(t_sid);
                                                                                        });
                                                                                        auth.active_space_id.set(Some(t_sid));
                                                                                        auth.active_grid_id.set(Some(t_gid));
                                                                                        auth.highlighted_row_id.set(t_rid);
                                                                                        show_ai_modal.set(false);
                                                                                        on_navigate.run(format!("/spaces/{t_sid}/grids/{t_gid}"));
                                                                                    }
                                                                                    class="w-full text-left p-2 rounded-lg bg-[#14171A] hover:bg-cyan-500/15 border border-cyan-500/30 hover:border-cyan-400 transition flex items-center justify-between gap-2 group"
                                                                                >
                                                                                    <div class="min-w-0 flex-1">
                                                                                        <div class="flex items-center gap-1.5 text-[11px] font-bold text-white truncate">
                                                                                            <span>{target.grid_icon.unwrap_or_else(|| "📄".into())}</span>
                                                                                            <span class="text-cyan-300">{target.primary_label}</span>
                                                                                            <span class="px-1.5 py-0.5 rounded bg-slate-800 border border-slate-700 text-[10px] font-normal text-slate-300">
                                                                                                {badge_text}
                                                                                            </span>
                                                                                        </div>
                                                                                        <div class="text-[10px] text-slate-400 truncate mt-0.5">
                                                                                            {target.matched_preview}
                                                                                        </div>
                                                                                    </div>
                                                                                    <span class="text-[10px] font-bold text-[#F97316] group-hover:translate-x-0.5 transition shrink-0">
                                                                                        "Open →"
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

                                <Show when=move || ai_loading.get()>
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
                                                "🔍 Scanning grid columns, tags & EGP/USD totals..."
                                            </div>
                                            <div class="text-cyan-400/90 font-medium">
                                                "🧠 Generating response & executing workspace actions..."
                                            </div>
                                        </div>

                                        <div class="w-full h-1.5 bg-slate-800 rounded-full overflow-hidden">
                                            <div class="h-full w-2/3 bg-gradient-to-r from-cyan-400 via-orange-500 to-cyan-400 rounded-full animate-pulse" />
                                        </div>
                                    </div>
                                </Show>

                                {move || {
                                    let acts = ai_actions_log.get();
                                    if acts.is_empty() {
                                        view! { <span /> }.into_any()
                                    } else {
                                        view! {
                                            <div class="space-y-1.5 pt-2">
                                                {acts
                                                    .into_iter()
                                                    .map(|a| {
                                                        view! {
                                                            <div class="px-3 py-1.5 rounded-lg bg-emerald-500/15 border border-emerald-500/40 text-emerald-300 text-[11px] font-semibold">
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

                            <Show when=move || !ai_attachments.get().is_empty()>
                                <div class="flex flex-wrap gap-1.5 p-2 rounded-xl bg-[#14171A] border border-cyan-500/30 shrink-0">
                                    {move || {
                                        ai_attachments
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
                                                        <span class="max-w-[140px] truncate font-medium">{att.filename}</span>
                                                        <button
                                                            type="button"
                                                            on:click=move |_| {
                                                                ai_attachments.update(|list| {
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
                                node_ref=ai_file_input_ref
                                type="file"
                                multiple=true
                                accept="image/*,.csv,.tsv,.txt,.md,.json,.pdf"
                                class="hidden"
                                on:change=move |ev| {
                                    let input: web_sys::HtmlInputElement = event_target(&ev);
                                    read_file_list_to_ai_attachments(input.files(), ai_attachments);
                                    input.set_value("");
                                }
                            />

                            <form
                                class="flex items-center gap-2 shrink-0"
                                on:submit=move |ev: web_sys::SubmitEvent| {
                                    ev.prevent_default();
                                    let prompt = ai_prompt_input.get().trim().to_string();
                                    let pending_atts = ai_attachments.get_untracked();
                                    if (prompt.is_empty() && pending_atts.is_empty()) || ai_loading.get() {
                                        return;
                                    }
                                    let mode = ai_mode.get();
                                    let hist = ai_history.get();
                                    let sid = auth.active_space_id.get_untracked().or_else(|| {
                                        auth.spaces.get_untracked().first().map(|s| s.space.id)
                                    });
                                    let gid = auth.active_grid_id.get_untracked();
                                    let display_prompt = if prompt.is_empty() {
                                        format!("Attached {} file(s)", pending_atts.len())
                                    } else {
                                        prompt.clone()
                                    };

                                    ai_history.update(|list| {
                                        list.push(AiChatMessage {
                                            role: "user".into(),
                                            content: display_prompt,
                                            attachments: pending_atts.clone(),
                                            navigation_targets: Vec::new(),
                                        });
                                    });
                                    ai_prompt_input.set(String::new());
                                    ai_attachments.set(Vec::new());
                                    ai_loading.set(true);

                                    spawn_local(async move {
                                        match ApiClient::run_ai_copilot(&auth, prompt, mode, sid, gid, hist, pending_atts).await {
                                            Ok(resp) => {
                                                ai_model_badge.set(resp.model_used);
                                                if !resp.executed_actions.is_empty() {
                                                    ai_actions_log.set(resp.executed_actions);
                                                }
                                                let nav_targets = resp.navigation_targets.clone();
                                                ai_history.update(|list| {
                                                    list.push(AiChatMessage {
                                                        role: "assistant".into(),
                                                        content: resp.reply,
                                                        attachments: Vec::new(),
                                                        navigation_targets: nav_targets,
                                                    });
                                                });
                                                if let Some(new_gid) = resp.navigated_grid_id {
                                                    let resolved_sid = resp
                                                        .navigated_space_id
                                                        .or(sid)
                                                        .or_else(|| {
                                                            resp.navigation_targets
                                                                .first()
                                                                .map(|t| t.space_id)
                                                        });
                                                    if let Some(target_sid) = resolved_sid {
                                                        expanded_spaces.update(|set| {
                                                            set.insert(target_sid);
                                                        });
                                                        if let Ok(grids) = ApiClient::list_space_grids(&auth, target_sid).await {
                                                            auth.space_grids.update(|map| {
                                                                map.insert(target_sid, grids);
                                                            });
                                                        }
                                                        auth.active_space_id.set(Some(target_sid));
                                                        auth.active_grid_id.set(Some(new_gid));
                                                        auth.highlighted_row_id.set(resp.navigated_row_id);
                                                        on_navigate.run(format!("/spaces/{target_sid}/grids/{new_gid}"));
                                                    }
                                                } else if let Some(target_rid) = resp.navigated_row_id {
                                                    auth.highlighted_row_id.set(Some(target_rid));
                                                }
                                            }
                                            Err(err) => {
                                                ai_history.update(|list| {
                                                    list.push(AiChatMessage {
                                                        role: "assistant".into(),
                                                        content: format!("⚠️ Error: {err}"),
                                                        attachments: Vec::new(),
                                                        navigation_targets: Vec::new(),
                                                    });
                                                });
                                            }
                                        }
                                        ai_loading.set(false);
                                    });
                                }
                            >
                                <button
                                    type="button"
                                    disabled=move || ai_loading.get()
                                    on:click=move |_| {
                                        if let Some(el) = ai_file_input_ref.get_untracked() {
                                            el.click();
                                        }
                                    }
                                    title="Attach image or file (or Drag & Drop / Ctrl+V screenshot)"
                                    class="px-3 py-2.5 rounded-xl bg-[#14171A] hover:bg-slate-800 border border-[#2A2F37] disabled:opacity-50 text-xs text-cyan-400 transition shrink-0"
                                >
                                    "📎"
                                </button>
                                <input
                                    type="text"
                                    disabled=move || ai_loading.get()
                                    placeholder=move || {
                                        if ai_loading.get() {
                                            "PyraAI is processing your request..."
                                        } else {
                                            "Ask PyraAI, paste screenshot (Ctrl+V), attach CSV/JSON, or type a task..."
                                        }
                                    }
                                    class="flex-1 px-3.5 py-2.5 rounded-xl bg-[#14171A] border border-[#2A2F37] disabled:opacity-60 disabled:cursor-wait text-xs text-white focus:outline-none focus:border-[#06B6D4]"
                                    prop:value=ai_prompt_input
                                    on:input=move |ev| ai_prompt_input.set(event_target_value(&ev))
                                />
                                <button
                                    type="submit"
                                    disabled=move || ai_loading.get()
                                    class=move || {
                                        if ai_loading.get() {
                                            "px-4 py-2.5 rounded-xl bg-cyan-600/50 text-cyan-200 cursor-wait font-bold text-xs shadow transition shrink-0 flex items-center gap-1.5"
                                        } else {
                                            "px-4 py-2.5 rounded-xl bg-[#06B6D4] hover:bg-[#0284C7] text-slate-950 font-bold text-xs shadow transition shrink-0 flex items-center gap-1.5"
                                        }
                                    }
                                >
                                    {move || {
                                        if ai_loading.get() {
                                            view! {
                                                <>
                                                    <span class="inline-block animate-spin h-3.5 w-3.5 border-2 border-cyan-200 border-t-orange-400 rounded-full" />
                                                    <span>"⏳ Working..."</span>
                                                </>
                                            }
                                            .into_any()
                                        } else {
                                            view! { <span>"Send →"</span> }.into_any()
                                        }
                                    }}
                                </button>
                            </form>
                        </div>
                    </div>
                </Show>
            </div>
        </aside>
    }
}

