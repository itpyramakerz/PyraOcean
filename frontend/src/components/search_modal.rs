use crate::{api::ApiClient, state::AuthState};
use leptos::{prelude::*, task::spawn_local};
use pyra_ocean_shared::{SearchHit, SearchScope};
use std::collections::BTreeMap;
use uuid::Uuid;

#[component]
pub fn SearchModal(
    open: RwSignal<bool>,
    on_navigate_hit: Callback<(Uuid, Uuid, Option<Uuid>)>,
) -> impl IntoView {
    let auth = expect_context::<AuthState>();

    let query = RwSignal::new(String::new());
    let scope = RwSignal::new(SearchScope::All);
    let results = RwSignal::new(Vec::<SearchHit>::new());
    let is_searching = RwSignal::new(false);
    let error_msg = RwSignal::new(Option::<String>::None);

    let run_search = move |q_str: String, active_scope: SearchScope| {
        let trimmed = q_str.trim().to_string();
        if trimmed.is_empty() {
            results.set(Vec::new());
            return;
        }
        is_searching.set(true);
        error_msg.set(None);

        let cur_space = auth.active_space_id.get_untracked();
        let cur_grid = auth.active_grid_id.get_untracked();

        spawn_local(async move {
            match ApiClient::search_workspace(&auth, &trimmed, active_scope, cur_space, cur_grid)
                .await
            {
                Ok(hits) => {
                    is_searching.set(false);
                    results.set(hits);
                }
                Err(err) => {
                    is_searching.set(false);
                    error_msg.set(Some(err));
                }
            }
        });
    };

    view! {
        <Show when=move || open.get()>
            <div
                on:click=move |_| open.set(false)
                class="fixed inset-0 z-50 bg-black/75 flex items-start justify-center pt-16 p-4"
            >
                <div
                    on:click=move |ev| ev.stop_propagation()
                    class="w-full max-w-2xl bg-[#1E2227] border border-[#2A2F37] rounded-2xl shadow-2xl overflow-hidden text-[#F1F5F9]"
                >
                    // Top Search Input Bar + 3-Way Scope Pill Selector
                    <div class="p-4 border-b border-[#2A2F37] space-y-3 bg-[#14171A]/70">
                        <div class="flex items-center gap-3">
                            <span class="text-[#06B6D4] text-base font-bold">"🔍"</span>
                            <input
                                type="search"
                                autofocus
                                placeholder="Search across all 12 property types (Select, MultiSelect tags, Text, Numbers, Files, Checklists)..."
                                class="flex-1 bg-transparent text-sm text-white focus:outline-none"
                                prop:value=query
                                on:input=move |ev| {
                                    let next_q = event_target_value(&ev);
                                    query.set(next_q.clone());
                                    run_search(next_q, scope.get_untracked());
                                }
                                on:keydown=move |ev: web_sys::KeyboardEvent| {
                                    if ev.key() == "Escape" {
                                        open.set(false);
                                    }
                                }
                            />
                            <button
                                type="button"
                                on:click=move |_| open.set(false)
                                class="px-2 py-0.5 rounded bg-slate-800 text-[10px] font-mono text-slate-400 hover:text-white"
                            >
                                "ESC"
                            </button>
                        </div>

                        // 3-Tier Scope Selector Pills: `[This Grid]` | `[This Space]` | `[All Spaces]`
                        <div class="flex items-center justify-between gap-2 pt-1">
                            <div class="flex items-center gap-1.5">
                                <span class="text-[11px] text-slate-400 mr-1">"Search Scope:"</span>
                                <button
                                    type="button"
                                    disabled=move || auth.active_grid_id.get().is_none()
                                    on:click=move |_| {
                                        scope.set(SearchScope::Grid);
                                        run_search(query.get_untracked(), SearchScope::Grid);
                                    }
                                    class=move || {
                                        let active = scope.get() == SearchScope::Grid;
                                        let cls = if active {
                                            "bg-[#F97316] text-white font-semibold"
                                        } else {
                                            "bg-[#1E2227] text-slate-300 hover:bg-slate-800 border border-[#2A2F37]"
                                        };
                                        format!("px-2.5 py-1 rounded-full text-[11px] transition disabled:opacity-40 {cls}")
                                    }
                                >
                                    "This Grid"
                                </button>

                                <button
                                    type="button"
                                    disabled=move || auth.active_space_id.get().is_none()
                                    on:click=move |_| {
                                        scope.set(SearchScope::Space);
                                        run_search(query.get_untracked(), SearchScope::Space);
                                    }
                                    class=move || {
                                        let active = scope.get() == SearchScope::Space;
                                        let cls = if active {
                                            "bg-[#06B6D4] text-slate-950 font-semibold"
                                        } else {
                                            "bg-[#1E2227] text-slate-300 hover:bg-slate-800 border border-[#2A2F37]"
                                        };
                                        format!("px-2.5 py-1 rounded-full text-[11px] transition disabled:opacity-40 {cls}")
                                    }
                                >
                                    "This Space"
                                </button>

                                <button
                                    type="button"
                                    on:click=move |_| {
                                        scope.set(SearchScope::All);
                                        run_search(query.get_untracked(), SearchScope::All);
                                    }
                                    class=move || {
                                        let active = scope.get() == SearchScope::All;
                                        let cls = if active {
                                            "bg-[#F97316] text-white font-semibold"
                                        } else {
                                            "bg-[#1E2227] text-slate-300 hover:bg-slate-800 border border-[#2A2F37]"
                                        };
                                        format!("px-2.5 py-1 rounded-full text-[11px] transition {cls}")
                                    }
                                >
                                    "All Spaces"
                                </button>
                            </div>

                            <span class="text-[11px] text-slate-400">
                                {move || {
                                    if is_searching.get() {
                                        "Searching...".to_string()
                                    } else {
                                        format!("{} matches", results.get().len())
                                    }
                                }}
                            </span>
                        </div>
                    </div>

                    // Grouped Results List (Space -> Grid Page -> Matching Row Snippet)
                    <div class="max-h-[60vh] overflow-y-auto p-4 space-y-4">
                        {move || {
                            if let Some(err) = error_msg.get() {
                                return view! {
                                    <div class="p-3 rounded-xl bg-red-500/15 border border-red-500/40 text-xs text-red-300">
                                        {err}
                                    </div>
                                }
                                .into_any();
                            }

                            let hits = results.get();
                            if hits.is_empty() {
                                return view! {
                                    <div class="py-10 text-center text-xs text-slate-400">
                                        {if query.get().trim().is_empty() {
                                            "Type a keyword above to search across your permitted Grids and Spaces."
                                        } else {
                                            "No matching rows or Grids found for this scope."
                                        }}
                                    </div>
                                }
                                .into_any();
                            }

                            // Group by `(space_name, grid_title)`
                            let mut grouped: BTreeMap<(String, String), Vec<SearchHit>> =
                                BTreeMap::new();
                            for hit in hits {
                                grouped
                                    .entry((hit.space_name.clone(), hit.grid_title.clone()))
                                    .or_default()
                                    .push(hit);
                            }

                            grouped
                                .into_iter()
                                .map(|((space_name, grid_title), group_hits)| {
                                    view! {
                                        <div class="space-y-1.5">
                                            <div class="px-2 flex items-center gap-1.5 text-[11px] font-semibold uppercase tracking-wider text-slate-400">
                                                <span class="text-[#F97316]">{space_name}</span>
                                                <span>"/"</span>
                                                <span class="text-[#06B6D4]">{grid_title}</span>
                                            </div>

                                            <div class="space-y-1">
                                                {group_hits
                                                    .into_iter()
                                                    .map(|hit| {
                                                        let sid = hit.space_id;
                                                        let gid = hit.grid_id;
                                                        let rid = hit.row_id;
                                                        let is_grid = hit.match_type == "grid_title";
                                                        view! {
                                                            <button
                                                                type="button"
                                                                on:click=move |_| {
                                                                    open.set(false);
                                                                    on_navigate_hit.run((sid, gid, rid));
                                                                }
                                                                class="w-full text-left p-2.5 rounded-xl bg-[#14171A]/90 hover:bg-[#06B6D4]/15 border border-[#2A2F37] hover:border-[#06B6D4]/50 flex items-center justify-between gap-3 transition"
                                                            >
                                                                <div class="flex items-center gap-2.5 truncate">
                                                                    <span class="text-xs text-[#06B6D4]">
                                                                        {if is_grid { "⊞" } else { "↳" }}
                                                                    </span>
                                                                    <span class="text-xs text-slate-100 truncate">
                                                                        {hit.snippet}
                                                                    </span>
                                                                </div>
                                                                <span class="text-[10px] font-mono text-slate-500 shrink-0">
                                                                    {hit.last_modified.format("%Y-%m-%d %H:%M").to_string()}
                                                                </span>
                                                            </button>
                                                        }
                                                    })
                                                    .collect_view()}
                                            </div>
                                        </div>
                                    }
                                })
                                .collect_view()
                                .into_any()
                        }}
                    </div>
                </div>
            </div>
        </Show>
    }
}

