use crate::{api::ApiClient, state::AuthState};
use leptos::{prelude::*, task::spawn_local};
use pyra_ocean_shared::{
    AuditLogEntry, CreateSpaceRequest, CreateUserRequest, GrantSpaceMemberRequest,
    SpaceMemberDetail, SpaceRole, SystemRole, User,
};
use std::collections::HashSet;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdminTab {
    Users,
    Spaces,
    AuditLogs,
}

#[component]
pub fn AdminDashboard(initial_tab: RwSignal<AdminTab>) -> impl IntoView {
    let auth = expect_context::<AuthState>();

    // Shared reactive data across Admin tabs
    let users = RwSignal::new(Vec::<User>::new());
    let audit_logs = RwSignal::new(Vec::<AuditLogEntry>::new());
    let selected_space_id = RwSignal::new(Option::<Uuid>::None);
    let space_members = RwSignal::new(Vec::<SpaceMemberDetail>::new());
    let status_banner = RwSignal::new(Option::<(bool, String)>::None);

    // Tab 1: Create User Modal state
    let show_create_user_modal = RwSignal::new(false);
    let new_user_email = RwSignal::new(String::new());
    let new_user_name = RwSignal::new(String::new());
    let new_user_password = RwSignal::new(String::new());
    let new_user_role = RwSignal::new(SystemRole::User);

    // Tab 2: Create Space Modal state (SuperAdmin)
    let show_create_space_modal = RwSignal::new(false);
    let new_space_name = RwSignal::new(String::new());
    let new_space_slug = RwSignal::new(String::new());
    let new_space_desc = RwSignal::new(String::new());
    let selected_space_admins = RwSignal::new(HashSet::<Uuid>::new());

    // Tab 2: Add Member to Space form state
    let member_user_id_to_add = RwSignal::new(String::new());
    let member_permission_level = RwSignal::new(String::from("editor"));

    // Tab 3: Audit search filter
    let audit_search_query = RwSignal::new(String::new());

    // Load initial data when AdminDashboard mounts or tab changes
    Effect::new(move |_| {
        let tab = initial_tab.get();
        if auth.is_admin() {
            spawn_local(async move {
                if let Ok(u_list) = ApiClient::list_users(&auth).await {
                    users.set(u_list);
                }
                if let Ok(s_list) = ApiClient::list_spaces(&auth).await {
                    if selected_space_id.get_untracked().is_none() {
                        if let Some(first) = s_list.first() {
                            selected_space_id.set(Some(first.space.id));
                        }
                    }
                    auth.spaces.set(s_list);
                }
                if tab == AdminTab::AuditLogs && auth.is_super_admin() {
                    if let Ok(logs) = ApiClient::list_audit_logs(&auth).await {
                        audit_logs.set(logs);
                    }
                }
            });
        }
    });

    // Fetch Space Member Access Matrix whenever `selected_space_id` changes
    Effect::new(move |_| {
        if let Some(sid) = selected_space_id.get() {
            spawn_local(async move {
                if let Ok(members) = ApiClient::list_space_members(&auth, sid).await {
                    space_members.set(members);
                }
            });
        } else {
            space_members.set(Vec::new());
        }
    });

    // ========================================================================
    // Handlers for Tab 1: Global User & Role Management
    // ========================================================================

    let handle_create_user = move |ev: web_sys::SubmitEvent| {
        ev.prevent_default();
        let payload = CreateUserRequest {
            email: new_user_email.get().trim().to_string(),
            display_name: new_user_name.get().trim().to_string(),
            password: new_user_password.get(),
            system_role: new_user_role.get(),
        };

        spawn_local(async move {
            match ApiClient::create_user(&auth, payload).await {
                Ok(created) => {
                    users.update(|list| list.push(created.clone()));
                    show_create_user_modal.set(false);
                    new_user_email.set(String::new());
                    new_user_name.set(String::new());
                    new_user_password.set(String::new());
                    status_banner.set(Some((
                        true,
                        format!("Created user '{}' ({:?})", created.email, created.system_role),
                    )));
                }
                Err(err) => status_banner.set(Some((false, err))),
            }
        });
    };

    let handle_role_change = move |target_id: Uuid, next_role: SystemRole| {
        spawn_local(async move {
            match ApiClient::update_user_role(&auth, target_id, next_role, None).await {
                Ok(updated) => {
                    users.update(|list| {
                        if let Some(u) = list.iter_mut().find(|u| u.id == target_id) {
                            *u = updated.clone();
                        }
                    });
                    status_banner.set(Some((
                        true,
                        format!(
                            "Updated '{}' global role to {}",
                            updated.email,
                            updated.system_role.label()
                        ),
                    )));
                }
                Err(err) => status_banner.set(Some((false, err))),
            }
        });
    };

    // ========================================================================
    // Handlers for Tab 2: Spaces & Member Access Matrix
    // ========================================================================

    let handle_create_space = move |ev: web_sys::SubmitEvent| {
        ev.prevent_default();
        let name = new_space_name.get().trim().to_string();
        let slug = {
            let raw = new_space_slug.get().trim().to_string();
            if raw.is_empty() {
                name.to_lowercase().replace(' ', "-")
            } else {
                raw.to_lowercase().replace(' ', "-")
            }
        };
        let desc = {
            let d = new_space_desc.get().trim().to_string();
            if d.is_empty() { None } else { Some(d) }
        };
        let admin_manager_ids: Vec<Uuid> = selected_space_admins.get().into_iter().collect();

        let payload = CreateSpaceRequest {
            slug,
            name,
            description: desc,
            icon: Some("folder".into()),
            admin_manager_ids,
        };

        spawn_local(async move {
            match ApiClient::create_space(&auth, payload).await {
                Ok(space) => {
                    if let Ok(updated_spaces) = ApiClient::list_spaces(&auth).await {
                        auth.spaces.set(updated_spaces);
                    }
                    selected_space_id.set(Some(space.id));
                    show_create_space_modal.set(false);
                    new_space_name.set(String::new());
                    new_space_slug.set(String::new());
                    new_space_desc.set(String::new());
                    selected_space_admins.set(HashSet::new());
                    status_banner.set(Some((
                        true,
                        format!("Workspace '{}' created.", space.name),
                    )));
                }
                Err(err) => status_banner.set(Some((false, err))),
            }
        });
    };

    let handle_delete_space = move |space_id: Uuid, space_name: String| {
        spawn_local(async move {
            match ApiClient::delete_space(&auth, space_id).await {
                Ok(()) => {
                    auth.spaces.update(|list| list.retain(|s| s.space.id != space_id));
                    if selected_space_id.get_untracked() == Some(space_id) {
                        let next_id = auth.spaces.get_untracked().first().map(|s| s.space.id);
                        selected_space_id.set(next_id);
                    }
                    status_banner.set(Some((
                        true,
                        format!("Deleted Workspace '{space_name}' and all its Grids."),
                    )));
                }
                Err(err) => status_banner.set(Some((false, err))),
            }
        });
    };

    let handle_grant_member = move |ev: web_sys::SubmitEvent| {
        ev.prevent_default();
        let Some(space_id) = selected_space_id.get() else {
            return;
        };
        let Ok(uid) = Uuid::parse_str(member_user_id_to_add.get().trim()) else {
            status_banner.set(Some((false, "Please select a user to grant access.".into())));
            return;
        };

        let perm = member_permission_level.get();
        let (can_view, can_edit, is_space_admin) = match perm.as_str() {
            "space_admin" => (true, true, true),
            "editor" => (true, true, false),
            _ => (true, false, false),
        };

        let payload = GrantSpaceMemberRequest {
            user_id: uid,
            can_view,
            can_edit,
            is_space_admin,
        };

        spawn_local(async move {
            match ApiClient::grant_space_member(&auth, space_id, payload).await {
                Ok(_) => {
                    if let Ok(members) = ApiClient::list_space_members(&auth, space_id).await {
                        space_members.set(members);
                    }
                    status_banner.set(Some((true, "Updated Space member permissions.".into())));
                }
                Err(err) => status_banner.set(Some((false, err))),
            }
        });
    };

    let handle_inline_space_role = move |space_id: Uuid, user_id: Uuid, next_role: SpaceRole| {
        let payload = GrantSpaceMemberRequest {
            user_id,
            can_view: true,
            can_edit: matches!(next_role, SpaceRole::Editor | SpaceRole::SpaceAdmin),
            is_space_admin: next_role == SpaceRole::SpaceAdmin,
        };
        spawn_local(async move {
            match ApiClient::grant_space_member(&auth, space_id, payload).await {
                Ok(_) => {
                    if let Ok(members) = ApiClient::list_space_members(&auth, space_id).await {
                        space_members.set(members);
                    }
                    status_banner.set(Some((
                        true,
                        format!("Updated Space permission to '{}'.", next_role.label()),
                    )));
                }
                Err(err) => status_banner.set(Some((false, err))),
            }
        });
    };

    let handle_revoke_member = move |space_id: Uuid, user_id: Uuid| {
        spawn_local(async move {
            match ApiClient::revoke_space_member(&auth, space_id, user_id).await {
                Ok(()) => {
                    space_members.update(|list| list.retain(|m| m.user_id != user_id));
                    status_banner.set(Some((true, "Revoked user access from Space.".into())));
                }
                Err(err) => status_banner.set(Some((false, err))),
            }
        });
    };

    view! {
        <div class="p-6 max-w-7xl mx-auto space-y-6">
            // ================================================================
            // Header & Role-Aware Tab Bar
            // ================================================================
            <div class="flex flex-col sm:flex-row sm:items-center justify-between gap-4 border-b border-[#2A2F37] pb-4">
                <div>
                    <h1 class="text-2xl font-bold tracking-tight">
                        "Enterprise Admin & RBAC Control Center"
                    </h1>
                    <p class="text-xs text-slate-400 mt-1">
                        "Manage 3-tier global roles, Workspace (Space) access matrices, and system-wide audit trails"
                    </p>
                </div>

                <div class="flex items-center gap-2">
                    <Show when=move || auth.is_super_admin()>
                        <button
                            type="button"
                            on:click=move |_| initial_tab.set(AdminTab::Users)
                            class=move || {
                                let active = initial_tab.get() == AdminTab::Users;
                                let cls = if active {
                                    "bg-[#F97316] text-white font-semibold shadow"
                                } else {
                                    "bg-[#1E2227] text-slate-300 hover:bg-slate-800 border border-[#2A2F37]"
                                };
                                format!("px-3.5 py-2 rounded-lg text-xs transition {cls}")
                            }
                        >
                            "1. Global Users & Roles"
                        </button>
                    </Show>

                    <button
                        type="button"
                        on:click=move |_| initial_tab.set(AdminTab::Spaces)
                        class=move || {
                            let active = initial_tab.get() == AdminTab::Spaces;
                            let cls = if active {
                                "bg-[#06B6D4] text-slate-950 font-semibold shadow"
                            } else {
                                "bg-[#1E2227] text-slate-300 hover:bg-slate-800 border border-[#2A2F37]"
                            };
                            format!("px-3.5 py-2 rounded-lg text-xs transition {cls}")
                        }
                    >
                        "2. Spaces & Access Matrix"
                    </button>

                    <Show when=move || auth.is_super_admin()>
                        <button
                            type="button"
                            on:click=move |_| initial_tab.set(AdminTab::AuditLogs)
                            class=move || {
                                let active = initial_tab.get() == AdminTab::AuditLogs;
                                let cls = if active {
                                    "bg-[#F97316] text-white font-semibold shadow"
                                } else {
                                    "bg-[#1E2227] text-slate-300 hover:bg-slate-800 border border-[#2A2F37]"
                                };
                                format!("px-3.5 py-2 rounded-lg text-xs transition {cls}")
                            }
                        >
                            "3. System Audit Logs"
                        </button>
                    </Show>
                </div>
            </div>

            // Status / Feedback Banner
            {move || {
                status_banner.get().map(|(ok, msg)| {
                    let cls = if ok {
                        "bg-emerald-500/15 border-emerald-500/40 text-emerald-300"
                    } else {
                        "bg-red-500/15 border-red-500/40 text-red-300"
                    };
                    view! {
                        <div class=format!("p-3 rounded-xl border text-xs flex items-center justify-between {cls}")>
                            <span>{msg}</span>
                            <button
                                type="button"
                                class="font-bold ml-4"
                                on:click=move |_| status_banner.set(None)
                            >
                                "×"
                            </button>
                        </div>
                    }
                })
            }}

            // ================================================================
            // TAB 1: Global User & Role Management (`SuperAdmin` Only)
            // ================================================================
            <Show when=move || initial_tab.get() == AdminTab::Users && auth.is_super_admin()>
                <section class="bg-[#1E2227] border border-[#2A2F37] rounded-2xl p-6 space-y-4 shadow-xl">
                    <div class="flex items-center justify-between">
                        <div>
                            <h2 class="text-base font-bold text-white">"Global User Directory"</h2>
                            <p class="text-xs text-slate-400">
                                "Promote or demote accounts between Standard User and Department Admin"
                            </p>
                        </div>
                        <button
                            type="button"
                            on:click=move |_| show_create_user_modal.set(true)
                            class="px-4 py-2 rounded-lg bg-[#F97316] hover:bg-[#EA580C] text-white text-xs font-semibold shadow transition"
                        >
                            "+ Create User"
                        </button>
                    </div>

                    <div class="overflow-x-auto">
                        <table class="w-full text-left border-collapse text-xs">
                            <thead>
                                <tr class="border-b border-[#2A2F37] text-slate-400 uppercase text-[11px]">
                                    <th class="py-3 px-3">"Display Name"</th>
                                    <th class="py-3 px-3">"Email"</th>
                                    <th class="py-3 px-3">"Global Role"</th>
                                    <th class="py-3 px-3">"Created At"</th>
                                    <th class="py-3 px-3 text-right">"Promote / Demote"</th>
                                </tr>
                            </thead>
                            <tbody class="divide-y divide-[#2A2F37]/60">
                                {move || {
                                    users
                                        .get()
                                        .into_iter()
                                        .map(|u| {
                                            let uid = u.id;
                                            let is_sa = u.system_role == SystemRole::SuperAdmin;
                                            let badge_cls = match u.system_role {
                                                SystemRole::SuperAdmin => "bg-[#F97316]/20 text-[#F97316] border-[#F97316]/40",
                                                SystemRole::Admin => "bg-[#06B6D4]/20 text-[#06B6D4] border-[#06B6D4]/40",
                                                SystemRole::User => "bg-slate-700/40 text-slate-300 border-slate-600",
                                            };
                                            view! {
                                                <tr class="hover:bg-slate-800/30">
                                                    <td class="py-3 px-3 font-medium text-white">{u.display_name}</td>
                                                    <td class="py-3 px-3 text-slate-300 font-mono">{u.email}</td>
                                                    <td class="py-3 px-3">
                                                        <span class=format!("px-2 py-0.5 rounded border text-[11px] font-semibold {badge_cls}")>
                                                            {u.system_role.label()}
                                                        </span>
                                                    </td>
                                                    <td class="py-3 px-3 text-slate-400">
                                                        {u.created_at.format("%Y-%m-%d %H:%M").to_string()}
                                                    </td>
                                                    <td class="py-3 px-3 text-right">
                                                        <Show
                                                            when=move || !is_sa
                                                            fallback=|| view! {
                                                                <span class="text-[11px] text-slate-500 italic">"Global Singleton"</span>
                                                            }
                                                        >
                                                            <select
                                                                class="px-2.5 py-1 rounded bg-[#14171A] border border-[#2A2F37] text-xs text-slate-200 focus:border-[#06B6D4]"
                                                                on:change=move |ev| {
                                                                    let val = event_target_value(&ev);
                                                                    let next_role = if val == "admin" {
                                                                        SystemRole::Admin
                                                                    } else {
                                                                        SystemRole::User
                                                                    };
                                                                    handle_role_change(uid, next_role);
                                                                }
                                                            >
                                                                <option value="user" selected=u.system_role == SystemRole::User>
                                                                    "User (Standard Member)"
                                                                </option>
                                                                <option value="admin" selected=u.system_role == SystemRole::Admin>
                                                                    "Admin (Workspace Manager)"
                                                                </option>
                                                            </select>
                                                        </Show>
                                                    </td>
                                                </tr>
                                            }
                                        })
                                        .collect_view()
                                }}
                            </tbody>
                        </table>
                    </div>
                </section>

                // Create User Modal
                <Show when=move || show_create_user_modal.get()>
                    <div class="fixed inset-0 z-50 bg-black/70 flex items-center justify-center p-4">
                        <div class="w-full max-w-md bg-[#1E2227] border border-[#2A2F37] rounded-2xl p-6 shadow-2xl space-y-4">
                            <div class="flex items-center justify-between">
                                <h3 class="text-base font-bold text-white">"Create New User Account"</h3>
                                <button
                                    type="button"
                                    on:click=move |_| show_create_user_modal.set(false)
                                    class="text-slate-400 hover:text-white"
                                >
                                    "✕"
                                </button>
                            </div>

                            <form class="space-y-3" on:submit=handle_create_user>
                                <div>
                                    <label class="block text-xs text-slate-400 mb-1">"Display Name"</label>
                                    <input
                                        type="text"
                                        required
                                        placeholder="Sara Hassan"
                                        class="w-full px-3 py-2 rounded-lg bg-[#14171A] border border-[#2A2F37] text-xs text-white"
                                        prop:value=new_user_name
                                        on:input=move |ev| new_user_name.set(event_target_value(&ev))
                                    />
                                </div>
                                <div>
                                    <label class="block text-xs text-slate-400 mb-1">"Work Email"</label>
                                    <input
                                        type="email"
                                        required
                                        placeholder="sara@pyramakerz.com"
                                        class="w-full px-3 py-2 rounded-lg bg-[#14171A] border border-[#2A2F37] text-xs text-white"
                                        prop:value=new_user_email
                                        on:input=move |ev| new_user_email.set(event_target_value(&ev))
                                    />
                                </div>
                                <div>
                                    <label class="block text-xs text-slate-400 mb-1">"Initial Password (min 8 chars)"</label>
                                    <input
                                        type="password"
                                        required
                                        placeholder="••••••••••••"
                                        class="w-full px-3 py-2 rounded-lg bg-[#14171A] border border-[#2A2F37] text-xs text-white"
                                        prop:value=new_user_password
                                        on:input=move |ev| new_user_password.set(event_target_value(&ev))
                                    />
                                </div>
                                <div>
                                    <label class="block text-xs text-slate-400 mb-1">"Global Role Tier"</label>
                                    <select
                                        class="w-full px-3 py-2 rounded-lg bg-[#14171A] border border-[#2A2F37] text-xs text-white"
                                        on:change=move |ev| {
                                            let val = event_target_value(&ev);
                                            if val == "admin" {
                                                new_user_role.set(SystemRole::Admin);
                                            } else {
                                                new_user_role.set(SystemRole::User);
                                            }
                                        }
                                    >
                                        <option value="user">"User (Standard Member)"</option>
                                        <option value="admin">"Admin (Department / Space Manager)"</option>
                                    </select>
                                </div>

                                <div class="pt-3 flex justify-end gap-2">
                                    <button
                                        type="button"
                                        on:click=move |_| show_create_user_modal.set(false)
                                        class="px-3 py-1.5 rounded-lg border border-[#2A2F37] text-xs text-slate-300"
                                    >
                                        "Cancel"
                                    </button>
                                    <button
                                        type="submit"
                                        class="px-4 py-1.5 rounded-lg bg-[#F97316] hover:bg-[#EA580C] text-white text-xs font-semibold"
                                    >
                                        "Create Account"
                                    </button>
                                </div>
                            </form>
                        </div>
                    </div>
                </Show>
            </Show>

            // ================================================================
            // TAB 2: Spaces & Granular Access Control (`SuperAdmin` & `Admin`)
            // ================================================================
            <Show when=move || initial_tab.get() == AdminTab::Spaces>
                <div class="grid grid-cols-1 lg:grid-cols-3 gap-6">
                    // Left Column: Spaces List + SuperAdmin Create/Delete Space
                    <div class="bg-[#1E2227] border border-[#2A2F37] rounded-2xl p-5 space-y-4 shadow-xl">
                        <div class="flex items-center justify-between">
                            <h2 class="text-sm font-bold text-white">"Workspaces (Spaces)"</h2>
                            <Show when=move || auth.is_super_admin()>
                                <button
                                    type="button"
                                    on:click=move |_| show_create_space_modal.set(true)
                                    class="px-3 py-1.5 rounded-lg bg-[#F97316] hover:bg-[#EA580C] text-white text-xs font-semibold transition"
                                >
                                    "+ Create New Space"
                                </button>
                            </Show>
                        </div>

                        <div class="space-y-2">
                            {move || {
                                auth.spaces
                                    .get()
                                    .into_iter()
                                    .map(|item| {
                                        let sid = item.space.id;
                                        let sname = item.space.name.clone();
                                        let sname_del = sname.clone();
                                        let sslug = item.space.slug.clone();
                                        let role = item.effective_role;

                                        view! {
                                            <div
                                                on:click=move |_| selected_space_id.set(Some(sid))
                                                class=move || {
                                                    let selected = selected_space_id.get() == Some(sid);
                                                    let cls = if selected {
                                                        "border-[#06B6D4] bg-[#06B6D4]/10"
                                                    } else {
                                                        "border-[#2A2F37] bg-[#14171A]/60 hover:border-slate-600"
                                                    };
                                                    format!("p-3 rounded-xl border cursor-pointer flex items-center justify-between transition {cls}")
                                                }
                                            >
                                                <div class="truncate">
                                                    <div class="text-xs font-bold text-white truncate">{sname}</div>
                                                    <div class="text-[11px] text-slate-400 font-mono">
                                                        "/" {sslug} " · " {role.label()}
                                                    </div>
                                                </div>

                                                <Show when=move || auth.is_super_admin()>
                                                    {
                                                        let sname_for_btn = sname_del.clone();
                                                        view! {
                                                            <button
                                                                type="button"
                                                                title="Hard Delete Space & Cascade Grids"
                                                                on:click=move |ev| {
                                                                    ev.stop_propagation();
                                                                    handle_delete_space(sid, sname_for_btn.clone());
                                                                }
                                                                class="px-2 py-1 rounded bg-red-500/10 hover:bg-red-500/25 text-red-400 text-[11px] font-semibold"
                                                            >
                                                                "Delete"
                                                            </button>
                                                        }
                                                    }
                                                </Show>
                                            </div>
                                        }
                                    })
                                    .collect_view()
                            }}
                        </div>
                    </div>

                    // Right Column (2 cols wide): Space Member Access Matrix
                    <div class="lg:col-span-2 bg-[#1E2227] border border-[#2A2F37] rounded-2xl p-6 space-y-5 shadow-xl">
                        {move || {
                            let Some(active_sid) = selected_space_id.get() else {
                                return view! {
                                    <div class="py-12 text-center text-xs text-slate-400">
                                        "Select a Space on the left to configure its Member Access Matrix."
                                    </div>
                                }
                                .into_any();
                            };

                            let space_title = auth
                                .spaces
                                .get()
                                .into_iter()
                                .find(|s| s.space.id == active_sid)
                                .map(|s| s.space.name)
                                .unwrap_or_else(|| "Selected Space".into());

                            view! {
                                <div class="space-y-5">
                                    <div class="flex flex-col sm:flex-row sm:items-center justify-between gap-2 border-b border-[#2A2F37] pb-3">
                                        <div>
                                            <h3 class="text-base font-bold text-white">
                                                "Member Access Matrix — "
                                                <span class="text-[#06B6D4]">{space_title}</span>
                                            </h3>
                                            <p class="text-xs text-slate-400">
                                                "Grant users access and toggle between View Only, Can Edit, or Space Admin"
                                            </p>
                                        </div>
                                    </div>

                                    // Grant User Access Form
                                    <form
                                        class="p-3.5 rounded-xl bg-[#14171A] border border-[#2A2F37] flex flex-col sm:flex-row items-stretch sm:items-center gap-2.5"
                                        on:submit=handle_grant_member
                                    >
                                        <select
                                            class="flex-1 px-3 py-2 rounded-lg bg-[#1E2227] border border-[#2A2F37] text-xs text-white"
                                            on:change=move |ev| member_user_id_to_add.set(event_target_value(&ev))
                                        >
                                            <option value="">"Select user to grant access..."</option>
                                            {move || {
                                                users
                                                    .get()
                                                    .into_iter()
                                                    .map(|u| {
                                                        view! {
                                                            <option value=u.id.to_string()>
                                                                {format!("{} ({}) — {}", u.display_name, u.email, u.system_role.label())}
                                                            </option>
                                                        }
                                                    })
                                                    .collect_view()
                                            }}
                                        </select>

                                        <select
                                            class="px-3 py-2 rounded-lg bg-[#1E2227] border border-[#2A2F37] text-xs text-white"
                                            on:change=move |ev| member_permission_level.set(event_target_value(&ev))
                                        >
                                            <option value="viewer">"View Only"</option>
                                            <option value="editor" selected>"Can Edit"</option>
                                            <option value="space_admin">"Space Admin"</option>
                                        </select>

                                        <button
                                            type="submit"
                                            class="px-4 py-2 rounded-lg bg-[#06B6D4] hover:bg-[#0284C7] text-slate-950 font-semibold text-xs transition shrink-0"
                                        >
                                            "+ Grant Access"
                                        </button>
                                    </form>

                                    // Member Access Matrix Table
                                    <div class="overflow-x-auto">
                                        <table class="w-full text-left border-collapse text-xs">
                                            <thead>
                                                <tr class="border-b border-[#2A2F37] text-slate-400 uppercase text-[11px]">
                                                    <th class="py-2.5 px-3">"Member"</th>
                                                    <th class="py-2.5 px-3">"Global Role"</th>
                                                    <th class="py-2.5 px-3">"Space Permission"</th>
                                                    <th class="py-2.5 px-3 text-right">"Actions"</th>
                                                </tr>
                                            </thead>
                                            <tbody class="divide-y divide-[#2A2F37]/60">
                                                {move || {
                                                    space_members
                                                        .get()
                                                        .into_iter()
                                                        .map(|m| {
                                                            let uid = m.user_id;
                                                            let current_role = m.space_role;
                                                            view! {
                                                                <tr class="hover:bg-slate-800/30">
                                                                    <td class="py-2.5 px-3">
                                                                        <div class="font-semibold text-white">{m.display_name}</div>
                                                                        <div class="text-[11px] text-slate-400 font-mono">{m.email}</div>
                                                                    </td>
                                                                    <td class="py-2.5 px-3 text-slate-300">
                                                                        {m.global_role.label()}
                                                                    </td>
                                                                    <td class="py-2.5 px-3">
                                                                        <select
                                                                            class="px-2.5 py-1 rounded bg-[#14171A] border border-[#2A2F37] text-xs text-[#06B6D4] font-semibold"
                                                                            on:change=move |ev| {
                                                                                let val = event_target_value(&ev);
                                                                                let next = match val.as_str() {
                                                                                    "space_admin" => SpaceRole::SpaceAdmin,
                                                                                    "editor" => SpaceRole::Editor,
                                                                                    _ => SpaceRole::Viewer,
                                                                                };
                                                                                handle_inline_space_role(active_sid, uid, next);
                                                                            }
                                                                        >
                                                                            <option value="viewer" selected=current_role == SpaceRole::Viewer>
                                                                                "View Only"
                                                                            </option>
                                                                            <option value="editor" selected=current_role == SpaceRole::Editor>
                                                                                "Can Edit"
                                                                            </option>
                                                                            <option value="space_admin" selected=current_role == SpaceRole::SpaceAdmin>
                                                                                "Space Admin"
                                                                            </option>
                                                                        </select>
                                                                    </td>
                                                                    <td class="py-2.5 px-3 text-right">
                                                                        <button
                                                                            type="button"
                                                                            on:click=move |_| handle_revoke_member(active_sid, uid)
                                                                            class="px-2.5 py-1 rounded bg-red-500/10 hover:bg-red-500/20 text-red-400 text-[11px] font-semibold"
                                                                        >
                                                                            "Revoke"
                                                                        </button>
                                                                    </td>
                                                                </tr>
                                                            }
                                                        })
                                                        .collect_view()
                                                }}
                                            </tbody>
                                        </table>
                                    </div>
                                </div>
                            }
                            .into_any()
                        }}
                    </div>
                </div>

                // SuperAdmin Create Space Modal
                <Show when=move || show_create_space_modal.get()>
                    <div class="fixed inset-0 z-50 bg-black/70 flex items-center justify-center p-4">
                        <div class="w-full max-w-lg bg-[#1E2227] border border-[#2A2F37] rounded-2xl p-6 shadow-2xl space-y-4">
                            <div class="flex items-center justify-between">
                                <h3 class="text-base font-bold text-white">"Create New Workspace (Space)"</h3>
                                <button
                                    type="button"
                                    on:click=move |_| show_create_space_modal.set(false)
                                    class="text-slate-400 hover:text-white"
                                >
                                    "✕"
                                </button>
                            </div>

                            <form class="space-y-3" on:submit=handle_create_space>
                                <div>
                                    <label class="block text-xs text-slate-400 mb-1">"Space Name"</label>
                                    <input
                                        type="text"
                                        required
                                        placeholder="Engineering & Product"
                                        class="w-full px-3 py-2 rounded-lg bg-[#14171A] border border-[#2A2F37] text-xs text-white"
                                        prop:value=new_space_name
                                        on:input=move |ev| new_space_name.set(event_target_value(&ev))
                                    />
                                </div>
                                <div>
                                    <label class="block text-xs text-slate-400 mb-1">"URL Slug (optional)"</label>
                                    <input
                                        type="text"
                                        placeholder="engineering-product"
                                        class="w-full px-3 py-2 rounded-lg bg-[#14171A] border border-[#2A2F37] text-xs text-white font-mono"
                                        prop:value=new_space_slug
                                        on:input=move |ev| new_space_slug.set(event_target_value(&ev))
                                    />
                                </div>
                                <div>
                                    <label class="block text-xs text-slate-400 mb-1">"Description"</label>
                                    <input
                                        type="text"
                                        placeholder="Core R&D roadmaps, sprints, and QA grids"
                                        class="w-full px-3 py-2 rounded-lg bg-[#14171A] border border-[#2A2F37] text-xs text-white"
                                        prop:value=new_space_desc
                                        on:input=move |ev| new_space_desc.set(event_target_value(&ev))
                                    />
                                </div>

                                <div>
                                    <label class="block text-xs text-slate-400 mb-1.5">
                                        "Assign Initial Space Admins"
                                    </label>
                                    <div class="max-h-36 overflow-y-auto p-2 rounded-lg bg-[#14171A] border border-[#2A2F37] space-y-1.5">
                                        {move || {
                                            users
                                                .get()
                                                .into_iter()
                                                .map(|u| {
                                                    let uid = u.id;
                                                    view! {
                                                        <label class="flex items-center gap-2 text-xs text-slate-200 cursor-pointer">
                                                            <input
                                                                type="checkbox"
                                                                class="rounded accent-[#F97316]"
                                                                prop:checked=move || selected_space_admins.get().contains(&uid)
                                                                on:change=move |_| {
                                                                    selected_space_admins.update(|set| {
                                                                        if set.contains(&uid) {
                                                                            set.remove(&uid);
                                                                        } else {
                                                                            set.insert(uid);
                                                                        }
                                                                    });
                                                                }
                                                            />
                                                            <span>{u.display_name}</span>
                                                            <span class="text-slate-500 font-mono text-[11px]">
                                                                "(" {u.email} ")"
                                                            </span>
                                                        </label>
                                                    }
                                                })
                                                .collect_view()
                                        }}
                                    </div>
                                </div>

                                <div class="pt-3 flex justify-end gap-2">
                                    <button
                                        type="button"
                                        on:click=move |_| show_create_space_modal.set(false)
                                        class="px-3 py-1.5 rounded-lg border border-[#2A2F37] text-xs text-slate-300"
                                    >
                                        "Cancel"
                                    </button>
                                    <button
                                        type="submit"
                                        class="px-4 py-1.5 rounded-lg bg-[#F97316] hover:bg-[#EA580C] text-white text-xs font-semibold"
                                    >
                                        "Create Space"
                                    </button>
                                </div>
                            </form>
                        </div>
                    </div>
                </Show>
            </Show>

            // ================================================================
            // TAB 3: System Audit Logs (`SuperAdmin` Only)
            // ================================================================
            <Show when=move || initial_tab.get() == AdminTab::AuditLogs && auth.is_super_admin()>
                <section class="bg-[#1E2227] border border-[#2A2F37] rounded-2xl p-6 space-y-4 shadow-xl">
                    <div class="flex flex-col sm:flex-row sm:items-center justify-between gap-3">
                        <div>
                            <h2 class="text-base font-bold text-white">"System-Wide Security & Activity Audit Logs"</h2>
                            <p class="text-xs text-slate-400">
                                "Immutable trail of user logins, role changes, Space mutations, Grid imports, and bulk deletions"
                            </p>
                        </div>
                        <input
                            type="search"
                            placeholder="Filter by action, email, or metadata..."
                            class="w-full sm:w-72 px-3 py-2 rounded-lg bg-[#14171A] border border-[#2A2F37] text-xs text-white focus:outline-none focus:border-[#06B6D4]"
                            prop:value=audit_search_query
                            on:input=move |ev| audit_search_query.set(event_target_value(&ev))
                        />
                    </div>

                    <div class="overflow-x-auto">
                        <table class="w-full text-left border-collapse text-xs">
                            <thead>
                                <tr class="border-b border-[#2A2F37] text-slate-400 uppercase text-[11px]">
                                    <th class="py-2.5 px-3">"Timestamp (UTC)"</th>
                                    <th class="py-2.5 px-3">"Actor"</th>
                                    <th class="py-2.5 px-3">"Action"</th>
                                    <th class="py-2.5 px-3">"Metadata (JSONB)"</th>
                                </tr>
                            </thead>
                            <tbody class="divide-y divide-[#2A2F37]/60">
                                {move || {
                                    let q = audit_search_query.get().trim().to_lowercase();
                                    audit_logs
                                        .get()
                                        .into_iter()
                                        .filter(|entry| {
                                            if q.is_empty() {
                                                return true;
                                            }
                                            entry.action.to_lowercase().contains(&q)
                                                || entry
                                                    .actor_email
                                                    .as_deref()
                                                    .unwrap_or_default()
                                                    .to_lowercase()
                                                    .contains(&q)
                                                || entry.metadata.to_string().to_lowercase().contains(&q)
                                        })
                                        .map(|entry| {
                                            view! {
                                                <tr class="hover:bg-slate-800/30">
                                                    <td class="py-2.5 px-3 font-mono text-slate-400 whitespace-nowrap">
                                                        {entry.created_at.format("%Y-%m-%d %H:%M:%S").to_string()}
                                                    </td>
                                                    <td class="py-2.5 px-3 font-mono text-slate-200">
                                                        {entry.actor_email.unwrap_or_else(|| "system".into())}
                                                    </td>
                                                    <td class="py-2.5 px-3">
                                                        <span class="px-2 py-0.5 rounded bg-[#06B6D4]/15 text-[#06B6D4] border border-[#06B6D4]/30 font-mono text-[11px]">
                                                            {entry.action}
                                                        </span>
                                                    </td>
                                                    <td class="py-2.5 px-3 font-mono text-[11px] text-slate-300 max-w-xl truncate">
                                                        {entry.metadata.to_string()}
                                                    </td>
                                                </tr>
                                            }
                                        })
                                        .collect_view()
                                }}
                            </tbody>
                        </table>
                    </div>
                </section>
            </Show>
        </div>
    }
}

