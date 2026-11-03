pub mod api;
pub mod components;
pub mod pages;
pub mod state;

use api::ApiClient;
use components::{import_modal::ImportModal, search_modal::SearchModal, sidebar::Sidebar};
use leptos::{prelude::*, task::spawn_local};
use pages::{
    admin::{AdminDashboard, AdminTab},
    grid_view::GridViewPage,
    login::LoginPage,
};
use pyra_ocean_shared::ColumnPropertyType;
use state::AuthState;
use uuid::Uuid;

#[component]
pub fn App() -> impl IntoView {
    let auth = AuthState::init();
    provide_context(auth);

    let initial_pathname = web_sys::window()
        .and_then(|w| w.location().pathname().ok())
        .unwrap_or_else(|| "/".into());

    let current_path = RwSignal::new(initial_pathname.clone());
    let admin_tab = RwSignal::new(AdminTab::Spaces);
    let import_modal_space = RwSignal::new(Option::<Uuid>::None);
    let search_modal_open = RwSignal::new(false);

    let route_space_id = RwSignal::new(Uuid::nil());
    let route_grid_id = RwSignal::new(Uuid::nil());

    // Helper to parse `/spaces/{space_id}/grids/{grid_id}` routes
    let sync_route_params = move |path: &str| {
        let segments: Vec<&str> = path.trim_matches('/').split('/').collect();
        if segments.len() == 4 && segments[0] == "spaces" && segments[2] == "grids" {
            if let (Ok(sid), Ok(gid)) = (Uuid::parse_str(segments[1]), Uuid::parse_str(segments[3]))
            {
                route_space_id.set(sid);
                route_grid_id.set(gid);
                auth.active_space_id.set(Some(sid));
                auth.active_grid_id.set(Some(gid));
            }
        }
    };
    sync_route_params(&initial_pathname);

    // Sync HTML root `dark` class whenever `auth.dark_mode` changes
    Effect::new(move |_| {
        let is_dark = auth.dark_mode.get();
        if let Some(doc) = web_sys::window().and_then(|w| w.document()) {
            if let Some(el) = doc.document_element() {
                if is_dark {
                    let _ = el.set_attribute("class", "dark");
                } else {
                    let _ = el.set_attribute("class", "");
                }
            }
        }
    });

    // Validate stored token with `GET /api/auth/me` on mount
    Effect::new(move |_| {
        if auth.token.get_untracked().is_some() {
            spawn_local(async move {
                if let Ok(profile) = ApiClient::fetch_me(&auth).await {
                    auth.user.set(Some(profile));
                }
            });
        }
    });

    let navigate = Callback::new(move |path: String| {
        if let Some(window) = web_sys::window() {
            if let Ok(history) = window.history() {
                let _ = history.push_state_with_url(&wasm_bindgen::JsValue::NULL, "", Some(&path));
            }
        }
        match path.as_str() {
            "/admin/users" => admin_tab.set(AdminTab::Users),
            "/admin/spaces" => admin_tab.set(AdminTab::Spaces),
            "/admin/audit" => admin_tab.set(AdminTab::AuditLogs),
            _ => sync_route_params(&path),
        }
        current_path.set(path);
    });

    view! {
        <Show
            when=move || auth.is_authenticated() && current_path.get() != "/login"
            fallback=move || {
                view! {
                    <LoginPage
                        on_success=Callback::new(move |_| {
                            navigate.run("/".into());
                        })
                    />
                }
            }
        >
            <div
                tabindex="0"
                on:keydown=move |ev: web_sys::KeyboardEvent| {
                    if (ev.ctrl_key() || ev.meta_key()) && ev.key().eq_ignore_ascii_case("k") {
                        ev.prevent_default();
                        search_modal_open.update(|open| *open = !*open);
                    }
                }
                class=move || {
                    let surface = if auth.dark_mode.get() {
                        "bg-[#14171A] text-[#F1F5F9]"
                    } else {
                        "bg-[#F8FAFC] text-[#0F172A]"
                    };
                    format!("h-screen w-screen overflow-hidden flex font-sans focus:outline-none {surface}")
                }
            >
                <Sidebar
                    current_path=current_path
                    on_navigate=navigate
                    on_open_import_modal=Callback::new(move |space_id: Uuid| {
                        import_modal_space.set(Some(space_id));
                    })
                    on_open_search_modal=Callback::new(move |_| {
                        search_modal_open.set(true);
                    })
                />

                <main
                    class=move || {
                        let path = current_path.get();
                        if path.starts_with("/spaces/") && path.contains("/grids/") {
                            "flex-1 h-full flex flex-col overflow-hidden relative"
                        } else {
                            "flex-1 h-full flex flex-col overflow-y-auto overflow-x-auto relative"
                        }
                    }
                >
                    {move || {
                        let path = current_path.get();
                        if path.starts_with("/admin") {
                            view! { <AdminDashboard initial_tab=admin_tab /> }.into_any()
                        } else if path.starts_with("/spaces/") && path.contains("/grids/") {
                            view! {
                                <GridViewPage
                                    current_path=current_path
                                    space_id=route_space_id
                                    grid_id=route_grid_id
                                    on_navigate=navigate
                                    on_open_import=Callback::new(move |sid: Uuid| {
                                        import_modal_space.set(Some(sid));
                                    })
                                    on_open_search_modal=Callback::new(move |_| {
                                        search_modal_open.set(true);
                                    })
                                />
                            }
                            .into_any()
                        } else {
                            view! {
                                <WorkspaceHomeView
                                    current_path=current_path
                                    on_navigate=navigate
                                    on_open_import=Callback::new(move |sid: Uuid| {
                                        import_modal_space.set(Some(sid));
                                    })
                                    on_open_search_modal=Callback::new(move |_| {
                                        search_modal_open.set(true);
                                    })
                                />
                            }
                            .into_any()
                        }
                    }}
                </main>

                // Global Notion & AppFlowy Import Modal
                <ImportModal
                    space_id=import_modal_space
                    on_close=Callback::new(move |_| import_modal_space.set(None))
                    on_imported=Callback::new(move |(sid, gid): (Uuid, Uuid)| {
                        import_modal_space.set(None);
                        navigate.run(format!("/spaces/{sid}/grids/{gid}"));
                    })
                />

                // 3-Tier Scoped Search Modal (`This Grid` | `This Space` | `All Spaces`)
                <SearchModal
                    open=search_modal_open
                    on_navigate_hit=Callback::new(move |(sid, gid, rid): (Uuid, Uuid, Option<Uuid>)| {
                        auth.highlighted_row_id.set(rid);
                        navigate.run(format!("/spaces/{sid}/grids/{gid}"));
                    })
                />
            </div>
        </Show>
    }
}

#[component]
fn WorkspaceHomeView(
    current_path: RwSignal<String>,
    on_navigate: Callback<String>,
    on_open_import: Callback<Uuid>,
    on_open_search_modal: Callback<()>,
) -> impl IntoView {
    let auth = expect_context::<AuthState>();

    let property_types = [
        ColumnPropertyType::Text,
        ColumnPropertyType::Number,
        ColumnPropertyType::Select,
        ColumnPropertyType::MultiSelect,
        ColumnPropertyType::Date,
        ColumnPropertyType::Time,
        ColumnPropertyType::Files,
        ColumnPropertyType::Checkbox,
        ColumnPropertyType::Checklist,
        ColumnPropertyType::LastModified,
        ColumnPropertyType::Person,
    ];

    view! {
        <div class="p-8 max-w-6xl mx-auto space-y-6">
            <div class="p-6 rounded-2xl bg-[#1E2227] border border-[#2A2F37] flex flex-col sm:flex-row sm:items-center justify-between gap-4 shadow-xl">
                <div class="flex items-center gap-4">
                    <img
                        src=move || auth.active_logo_url()
                        alt="PyraOcean"
                        class="h-14 w-14 rounded-xl object-contain border border-[#06B6D4]/40"
                    />
                    <div>
                        <h1 class="text-xl font-bold text-white">
                            "Welcome to Pyra"<span class="text-[#06B6D4]">"Ocean"</span>
                        </h1>
                        <p class="text-xs text-slate-400 mt-0.5">
                            {move || format!("Active Route: {} · Select a Space or Grid from the left sidebar", current_path.get())}
                        </p>
                    </div>
                </div>

                <div class="flex items-center gap-2">
                    <button
                        type="button"
                        on:click=move |_| on_open_search_modal.run(())
                        class="px-3.5 py-2 rounded-lg bg-[#14171A] hover:bg-slate-800 text-slate-200 border border-[#2A2F37] text-xs font-semibold transition"
                    >
                        "🔍 Search Workspace (Ctrl+K)"
                    </button>
                    {move || {
                        auth.spaces.get().first().map(|first_space| {
                            let sid = first_space.space.id;
                            view! {
                                <button
                                    type="button"
                                    on:click=move |_| on_open_import.run(sid)
                                    class="px-3.5 py-2 rounded-lg bg-[#06B6D4]/15 hover:bg-[#06B6D4]/25 text-[#06B6D4] border border-[#06B6D4]/30 text-xs font-semibold transition"
                                >
                                    "⇄ Import Notion / AppFlowy"
                                </button>
                            }
                        })
                    }}
                    <Show when=move || auth.is_admin()>
                        <button
                            type="button"
                            on:click=move |_| on_navigate.run("/admin/spaces".into())
                            class="px-4 py-2 rounded-lg bg-[#F97316] hover:bg-[#EA580C] text-white text-xs font-semibold shadow transition"
                        >
                            "Open Admin Control Panel →"
                        </button>
                    </Show>
                </div>
            </div>

            <div class="p-6 rounded-2xl bg-[#1E2227] border border-[#2A2F37] space-y-3">
                <h2 class="text-xs font-semibold uppercase tracking-wider text-[#06B6D4]">
                    "11 Strongly-Typed Grid Column Property Engines Ready"
                </h2>
                <div class="grid grid-cols-2 sm:grid-cols-4 gap-2.5">
                    {property_types
                        .into_iter()
                        .map(|pt| {
                            view! {
                                <div class="px-3 py-2 rounded-lg bg-[#14171A] border border-[#2A2F37] text-xs font-mono text-slate-200">
                                    {format!("{pt:?}")}
                                </div>
                            }
                        })
                        .collect_view()}
                </div>
            </div>
        </div>
    }
}

fn main() {
    console_error_panic_hook::set_once();
    mount_to_body(App);
}
