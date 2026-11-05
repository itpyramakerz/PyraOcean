use crate::{api::ApiClient, components::cell_editor::copy_to_clipboard, state::AuthState};
use leptos::{prelude::*, task::spawn_local};
use pyra_ocean_shared::{
    generate_qr_code_svg, AdminManage2faResponse, AdminResetPasswordResponse, AiSettings,
    AuditLogEntry, CreateSpaceRequest, CreateUserRequest, GoogleAuthSettings,
    GrantSpaceMemberRequest, SpaceMemberDetail, SpaceRole, SystemRole, TotpSetupResponse, User,
    WorkspaceBackupPayload,
};
use std::collections::HashSet;
use uuid::Uuid;
use wasm_bindgen::JsCast;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdminTab {
    Users,
    Spaces,
    AuditLogs,
    BackupRestore,
}

/// Trigger a browser file download for a UTF-8 string payload.
fn trigger_browser_download(filename: &str, content: &str, mime_type: &str) {
    let Some(window) = web_sys::window() else {
        return;
    };
    let Some(document) = window.document() else {
        return;
    };
    let parts = js_sys::Array::new();
    parts.push(&wasm_bindgen::JsValue::from_str(content));
    let bag = web_sys::BlobPropertyBag::new();
    bag.set_type(mime_type);
    let Ok(blob) = web_sys::Blob::new_with_str_sequence_and_options(&parts, &bag) else {
        return;
    };
    let Ok(url) = web_sys::Url::create_object_url_with_blob(&blob) else {
        return;
    };
    if let Ok(el) = document.create_element("a") {
        if let Ok(anchor) = el.dyn_into::<web_sys::HtmlAnchorElement>() {
            anchor.set_href(&url);
            anchor.set_download(filename);
            anchor.click();
        }
    }
    let _ = web_sys::Url::revoke_object_url(&url);
}

fn copy_text_to_clipboard(text: &str) {
    let _ = copy_to_clipboard(text);
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

    // Tab 1: Admin Reset User Password Modal state
    let reset_pwd_target_user = RwSignal::new(Option::<User>::None);
    let reset_pwd_new_password = RwSignal::new(String::new());
    let reset_pwd_result = RwSignal::new(Option::<AdminResetPasswordResponse>::None);

    // Tab 1: Admin 2FA Enforcement / Reset Modal state
    let manage_2fa_target_user = RwSignal::new(Option::<User>::None);
    let manage_2fa_result = RwSignal::new(Option::<AdminManage2faResponse>::None);
    let manage_2fa_verify_code = RwSignal::new(String::new());
    let manage_2fa_verify_status = RwSignal::new(Option::<(bool, String)>::None);

    // Tab 1: My Admin Security Card (Own Google Authenticator 2FA Enrollment)
    let own_totp_setup_data = RwSignal::new(Option::<TotpSetupResponse>::None);
    let own_totp_verify_code = RwSignal::new(String::new());
    let own_totp_disable_pwd = RwSignal::new(String::new());

    // Tab 1: Google OAuth2 & 2FA Policy Configuration (SuperAdmin)
    let google_oauth_enabled = RwSignal::new(false);
    let google_client_id = RwSignal::new(String::new());
    let google_client_secret = RwSignal::new(String::new());
    let google_allowed_domains = RwSignal::new(String::new());
    let require_2fa_after_google = RwSignal::new(false);
    let saving_auth_settings = RwSignal::new(false);

    // Tab 1: Phase 18 Local AI Copilot (PyraAI / Ollama) Configuration (SuperAdmin)
    let ai_enabled = RwSignal::new(true);
    let ollama_base_url = RwSignal::new(String::from("http://10.10.10.4:11434"));
    let ai_model_name = RwSignal::new(String::from("qwen2.5:1.5b"));
    let ai_system_prompt = RwSignal::new(String::from(
        "You are PyraAI, the intelligent workspace assistant and task executor for PyraOcean.",
    ));
    let saving_ai_settings = RwSignal::new(false);

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

    // Tab 4: Full Database Backup & Restore state (SuperAdmin)
    let backup_exporting = RwSignal::new(false);
    let restore_in_progress = RwSignal::new(false);
    let restore_json_text = RwSignal::new(String::new());
    let restore_file_name = RwSignal::new(Option::<String>::None);

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
                if auth.is_super_admin() {
                    if let Ok(cfg) = ApiClient::get_auth_settings(&auth).await {
                        google_oauth_enabled.set(cfg.google_oauth_enabled);
                        google_client_id.set(cfg.google_client_id);
                        google_client_secret.set(cfg.google_client_secret);
                        google_allowed_domains.set(cfg.google_allowed_domains);
                        require_2fa_after_google.set(cfg.require_2fa_after_google);
                    }
                    if let Ok(ai_cfg) = ApiClient::get_ai_settings(&auth).await {
                        ai_enabled.set(ai_cfg.ai_enabled);
                        ollama_base_url.set(ai_cfg.ollama_base_url);
                        ai_model_name.set(ai_cfg.model_name);
                        ai_system_prompt.set(ai_cfg.system_prompt);
                    }
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
    // Handlers for Tab 1: Global User & Role Management + Password / 2FA
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

    let handle_delete_user = move |target_id: Uuid, target_email: String| {
        spawn_local(async move {
            match ApiClient::delete_user(&auth, target_id).await {
                Ok(()) => {
                    users.update(|list| list.retain(|u| u.id != target_id));
                    status_banner.set(Some((
                        true,
                        format!("Deleted user account '{target_email}'."),
                    )));
                }
                Err(err) => status_banner.set(Some((false, err))),
            }
        });
    };

    let handle_admin_reset_password = move |ev: web_sys::SubmitEvent| {
        ev.prevent_default();
        let Some(target) = reset_pwd_target_user.get() else {
            return;
        };
        let new_pwd = reset_pwd_new_password.get();
        spawn_local(async move {
            match ApiClient::admin_reset_user_password(&auth, target.id, new_pwd).await {
                Ok(resp) => {
                    if let Ok(u_list) = ApiClient::list_users(&auth).await {
                        users.set(u_list);
                    }
                    reset_pwd_new_password.set(String::new());
                    status_banner.set(Some((
                        true,
                        format!("Password reset for '{}'.", resp.user.email),
                    )));
                    reset_pwd_result.set(Some(resp));
                }
                Err(err) => status_banner.set(Some((false, err))),
            }
        });
    };

    let handle_admin_2fa_action = move |action: &'static str| {
        let Some(target) = manage_2fa_target_user.get() else {
            return;
        };
        manage_2fa_verify_status.set(None);
        manage_2fa_verify_code.set(String::new());
        spawn_local(async move {
            match ApiClient::admin_manage_user_2fa(&auth, target.id, action.to_string()).await {
                Ok(resp) => {
                    if let Ok(u_list) = ApiClient::list_users(&auth).await {
                        if let Some(updated) = u_list.iter().find(|u| u.id == target.id).cloned() {
                            manage_2fa_target_user.set(Some(updated));
                        }
                        users.set(u_list);
                    }
                    if let Some(me) = auth.user.get_untracked() {
                        if me.id == resp.user.id {
                            if let Ok(profile) = ApiClient::fetch_me(&auth).await {
                                auth.user.set(Some(profile));
                            }
                        }
                    }
                    let msg = if resp.user.totp_enabled {
                        format!(
                            "Google Authenticator 2FA provisioned and enabled for '{}'.",
                            resp.user.email
                        )
                    } else {
                        format!("Reset / disabled 2FA for '{}'.", resp.user.email)
                    };
                    status_banner.set(Some((true, msg)));
                    manage_2fa_result.set(Some(resp));
                }
                Err(err) => status_banner.set(Some((false, err))),
            }
        });
    };

    let handle_admin_verify_2fa_code = move |ev: web_sys::SubmitEvent| {
        ev.prevent_default();
        let Some(target) = manage_2fa_target_user.get() else {
            return;
        };
        let code = manage_2fa_verify_code.get().trim().to_string();
        spawn_local(async move {
            match ApiClient::admin_verify_user_2fa(&auth, target.id, code).await {
                Ok(resp) => {
                    manage_2fa_verify_status.set(Some((
                        true,
                        "✓ 6-Digit Google Authenticator Code Verified! 2FA is synced and active."
                            .into(),
                    )));
                    manage_2fa_verify_code.set(String::new());
                    if let Ok(u_list) = ApiClient::list_users(&auth).await {
                        if let Some(updated) = u_list.iter().find(|u| u.id == target.id).cloned() {
                            manage_2fa_target_user.set(Some(updated));
                        }
                        users.set(u_list);
                    }
                    if let Some(me) = auth.user.get_untracked() {
                        if me.id == resp.user.id {
                            if let Ok(profile) = ApiClient::fetch_me(&auth).await {
                                auth.user.set(Some(profile));
                            }
                        }
                    }
                }
                Err(err) => {
                    manage_2fa_verify_status.set(Some((false, err)));
                }
            }
        });
    };

    let handle_own_2fa_setup = move |_| {
        own_totp_verify_code.set(String::new());
        spawn_local(async move {
            match ApiClient::setup_totp(&auth).await {
                Ok(res) => {
                    own_totp_setup_data.set(Some(res));
                }
                Err(err) => status_banner.set(Some((false, err))),
            }
        });
    };

    let handle_own_2fa_enable = move |ev: web_sys::SubmitEvent| {
        ev.prevent_default();
        let code = own_totp_verify_code.get().trim().to_string();
        spawn_local(async move {
            match ApiClient::enable_totp(&auth, code).await {
                Ok(profile) => {
                    auth.user.set(Some(profile));
                    own_totp_setup_data.set(None);
                    own_totp_verify_code.set(String::new());
                    if let Ok(u_list) = ApiClient::list_users(&auth).await {
                        users.set(u_list);
                    }
                    status_banner.set(Some((
                        true,
                        "Google Authenticator 2FA is now active on your Admin account.".into(),
                    )));
                }
                Err(err) => status_banner.set(Some((false, err))),
            }
        });
    };

    let handle_own_2fa_disable = move |ev: web_sys::SubmitEvent| {
        ev.prevent_default();
        let pwd = own_totp_disable_pwd.get();
        spawn_local(async move {
            match ApiClient::disable_totp(&auth, pwd).await {
                Ok(profile) => {
                    auth.user.set(Some(profile));
                    own_totp_disable_pwd.set(String::new());
                    own_totp_setup_data.set(None);
                    if let Ok(u_list) = ApiClient::list_users(&auth).await {
                        users.set(u_list);
                    }
                    status_banner.set(Some((
                        true,
                        "Two-Factor Authentication has been disabled on your account.".into(),
                    )));
                }
                Err(err) => status_banner.set(Some((false, err))),
            }
        });
    };

    let handle_save_auth_settings = move |ev: web_sys::SubmitEvent| {
        ev.prevent_default();
        saving_auth_settings.set(true);
        let payload = GoogleAuthSettings {
            google_oauth_enabled: google_oauth_enabled.get(),
            google_client_id: google_client_id.get().trim().to_string(),
            google_client_secret: google_client_secret.get().trim().to_string(),
            google_allowed_domains: google_allowed_domains.get().trim().to_string(),
            require_2fa_after_google: require_2fa_after_google.get(),
        };
        spawn_local(async move {
            match ApiClient::update_auth_settings(&auth, payload).await {
                Ok(saved) => {
                    google_oauth_enabled.set(saved.google_oauth_enabled);
                    google_client_id.set(saved.google_client_id);
                    google_client_secret.set(saved.google_client_secret);
                    google_allowed_domains.set(saved.google_allowed_domains);
                    require_2fa_after_google.set(saved.require_2fa_after_google);
                    status_banner.set(Some((
                        true,
                        "Saved Google Authentication & 2FA Policy settings.".into(),
                    )));
                }
                Err(err) => status_banner.set(Some((false, err))),
            }
            saving_auth_settings.set(false);
        });
    };

    let handle_save_ai_settings = move |ev: web_sys::SubmitEvent| {
        ev.prevent_default();
        saving_ai_settings.set(true);
        let payload = AiSettings {
            ai_enabled: ai_enabled.get(),
            ollama_base_url: ollama_base_url.get().trim().to_string(),
            model_name: ai_model_name.get().trim().to_string(),
            system_prompt: ai_system_prompt.get().trim().to_string(),
        };
        spawn_local(async move {
            match ApiClient::update_ai_settings(&auth, payload).await {
                Ok(saved) => {
                    ai_enabled.set(saved.ai_enabled);
                    ollama_base_url.set(saved.ollama_base_url);
                    ai_model_name.set(saved.model_name);
                    ai_system_prompt.set(saved.system_prompt);
                    status_banner.set(Some((
                        true,
                        "Saved Local AI Copilot (PyraAI / Ollama) settings.".into(),
                    )));
                }
                Err(err) => status_banner.set(Some((false, err))),
            }
            saving_ai_settings.set(false);
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

    // ========================================================================
    // Handlers for Tab 4: Full Database Backup & Restore
    // ========================================================================

    let handle_download_backup = move |_| {
        backup_exporting.set(true);
        spawn_local(async move {
            match ApiClient::export_workspace_backup(&auth).await {
                Ok(backup) => {
                    let filename = format!(
                        "pyraocean-backup-{}.json",
                        backup.exported_at.format("%Y-%m-%d-%H%M%S")
                    );
                    if let Ok(pretty_json) = serde_json::to_string_pretty(&backup) {
                        trigger_browser_download(&filename, &pretty_json, "application/json");
                        status_banner.set(Some((
                            true,
                            format!(
                                "Downloaded '{}' ({} users, {} spaces, {} grids, {} rows, {} comments).",
                                filename,
                                backup.users.len(),
                                backup.spaces.len(),
                                backup.grids.len(),
                                backup.rows.len(),
                                backup.comments.len()
                            ),
                        )));
                    }
                }
                Err(err) => status_banner.set(Some((false, err))),
            }
            backup_exporting.set(false);
        });
    };

    let handle_restore_file_select = move |ev: web_sys::Event| {
        let Some(target) = ev.target() else {
            return;
        };
        let Ok(input) = target.dyn_into::<web_sys::HtmlInputElement>() else {
            return;
        };
        let Some(files) = input.files() else {
            return;
        };
        let Some(file) = files.get(0) else {
            return;
        };
        restore_file_name.set(Some(file.name()));

        let Ok(reader) = web_sys::FileReader::new() else {
            return;
        };
        let reader_clone = reader.clone();
        let onload = wasm_bindgen::closure::Closure::wrap(Box::new(move |_: web_sys::Event| {
            if let Ok(res) = reader_clone.result() {
                if let Some(text) = res.as_string() {
                    restore_json_text.set(text);
                }
            }
        }) as Box<dyn FnMut(_)>);
        reader.set_onload(Some(onload.as_ref().unchecked_ref()));
        onload.forget();
        let _ = reader.read_as_text(&file);
    };

    let handle_execute_restore = move |_| {
        let raw = restore_json_text.get();
        if raw.trim().is_empty() {
            status_banner.set(Some((
                false,
                "Please select or paste a valid PyraOcean backup .json file first.".into(),
            )));
            return;
        }
        let payload: WorkspaceBackupPayload = match serde_json::from_str(&raw) {
            Ok(p) => p,
            Err(e) => {
                status_banner.set(Some((false, format!("Invalid Backup JSON format: {e}"))));
                return;
            }
        };

        restore_in_progress.set(true);
        spawn_local(async move {
            match ApiClient::restore_workspace_backup(&auth, payload).await {
                Ok(res) => {
                    if let Ok(u_list) = ApiClient::list_users(&auth).await {
                        users.set(u_list);
                    }
                    if let Ok(s_list) = ApiClient::list_spaces(&auth).await {
                        auth.spaces.set(s_list);
                    }
                    status_banner.set(Some((
                        true,
                        format!(
                            "Database Restore Complete: {} users, {} spaces, {} grids, {} columns, {} rows, and {} comments restored.",
                            res.users_restored,
                            res.spaces_restored,
                            res.grids_restored,
                            res.columns_restored,
                            res.rows_restored,
                            res.comments_restored
                        ),
                    )));
                }
                Err(err) => status_banner.set(Some((false, err))),
            }
            restore_in_progress.set(false);
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
                        "PyraOcean Enterprise Admin & RBAC Control Center"
                    </h1>
                    <p class="text-xs text-slate-400 mt-1">
                        "Manage 3-tier global roles, 2FA/TOTP enforcement, password resets, PyraOcean Workspace access matrices, and full database backups"
                    </p>
                </div>

                <div class="flex flex-wrap items-center gap-2">
                    <Show when=move || auth.is_admin()>
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

                        <button
                            type="button"
                            on:click=move |_| initial_tab.set(AdminTab::BackupRestore)
                            class=move || {
                                let active = initial_tab.get() == AdminTab::BackupRestore;
                                let cls = if active {
                                    "bg-[#06B6D4] text-slate-950 font-semibold shadow"
                                } else {
                                    "bg-[#1E2227] text-slate-300 hover:bg-slate-800 border border-[#2A2F37]"
                                };
                                format!("px-3.5 py-2 rounded-lg text-xs transition {cls}")
                            }
                        >
                            "4. Backup & Restore"
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
            // TAB 1: Global User & Role Management (`SuperAdmin` & `Admin`)
            // ================================================================
            <Show when=move || initial_tab.get() == AdminTab::Users && auth.is_admin()>
                <div class="grid grid-cols-1 lg:grid-cols-2 gap-6">
                    // Card A: My Admin Security Card (Google Authenticator 2FA Enrollment)
                    <section class="bg-[#1E2227] border border-[#2A2F37] rounded-2xl p-6 space-y-4 shadow-xl">
                        <div class="flex items-start justify-between gap-3">
                            <div>
                                <h2 class="text-base font-bold text-white flex items-center gap-2">
                                    <span>"🔐 Google Authenticator & Two-Factor Authentication (2FA)"</span>
                                </h2>
                                <p class="text-xs text-slate-400 mt-1">
                                    "Enroll your own Admin account in Google Authenticator 2FA (scan QR code + enter 6-digit code) directly inside the Admin Panel."
                                </p>
                            </div>
                            {move || {
                                let enabled = auth
                                    .user
                                    .get()
                                    .map(|u| u.totp_enabled)
                                    .unwrap_or(false);
                                if enabled {
                                    view! {
                                        <span class="px-2.5 py-1 rounded-lg bg-emerald-500/15 border border-emerald-500/40 text-emerald-300 text-xs font-bold shrink-0">
                                            "🔒 Google Auth 2FA Active"
                                        </span>
                                    }
                                    .into_any()
                                } else {
                                    view! {
                                        <span class="px-2.5 py-1 rounded-lg bg-amber-500/15 border border-amber-500/40 text-amber-300 text-xs font-semibold shrink-0">
                                            "2FA Not Enrolled"
                                        </span>
                                    }
                                    .into_any()
                                }
                            }}
                        </div>

                        {move || {
                            let is_enabled = auth
                                .user
                                .get()
                                .map(|u| u.totp_enabled)
                                .unwrap_or(false);
                            let setup_state = own_totp_setup_data.get();

                            if let Some(setup) = setup_state {
                                let qr_svg = generate_qr_code_svg(&setup.otpauth_uri);
                                let sec_display = setup.secret.clone();
                                let sec_copy = setup.secret.clone();
                                let rec_display = setup.recovery_code.clone();
                                let rec_copy = setup.recovery_code.clone();

                                view! {
                                    <div class="p-4 rounded-xl bg-[#14171A] border border-[#06B6D4]/40 space-y-4">
                                        <div class="flex flex-col sm:flex-row items-center gap-4">
                                            <div class="w-40 h-40 p-2 rounded-xl bg-white shadow-lg shrink-0 flex items-center justify-center">
                                                <div class="w-full h-full" inner_html=qr_svg />
                                            </div>
                                            <div class="flex-1 space-y-2.5 w-full">
                                                <div class="text-xs font-bold text-[#06B6D4]">
                                                    "1. Scan QR Code with Google Authenticator"
                                                </div>
                                                <p class="text-[11px] text-slate-300 leading-relaxed">
                                                    "Open the Google Authenticator app on your phone, tap '+' -> 'Scan a QR code', or enter the Base32 Setup Key manually:"
                                                </p>
                                                <div>
                                                    <div class="text-[10px] uppercase tracking-wider text-slate-400 mb-1">
                                                        "Manual Setup Key (Base32)"
                                                    </div>
                                                    <div class="flex items-center justify-between gap-2 p-2 rounded-lg bg-[#1E2227] border border-slate-700 font-mono text-xs text-white">
                                                        <span class="select-all break-all">{sec_display}</span>
                                                        <button
                                                            type="button"
                                                            on:click=move |_| copy_text_to_clipboard(&sec_copy)
                                                            class="px-2.5 py-1 rounded bg-[#06B6D4]/20 hover:bg-[#06B6D4]/30 border border-[#06B6D4]/40 text-[#06B6D4] text-[10px] font-sans font-semibold shrink-0"
                                                        >
                                                            "📋 Copy Setup Key"
                                                        </button>
                                                    </div>
                                                </div>
                                                <div>
                                                    <div class="text-[10px] uppercase tracking-wider text-amber-300 mb-1 font-semibold">
                                                        "Emergency Recovery Code"
                                                    </div>
                                                    <div class="flex items-center justify-between gap-2 p-2 rounded-lg bg-[#1E2227] border border-amber-500/40 font-mono text-xs text-amber-300">
                                                        <span class="select-all">{rec_display}</span>
                                                        <button
                                                            type="button"
                                                            on:click=move |_| copy_text_to_clipboard(&rec_copy)
                                                            class="px-2.5 py-1 rounded bg-amber-500/20 hover:bg-amber-500/30 border border-amber-500/40 text-amber-200 text-[10px] font-sans font-semibold shrink-0"
                                                        >
                                                            "📋 Copy Recovery Code"
                                                        </button>
                                                    </div>
                                                </div>
                                            </div>
                                        </div>

                                        <form class="pt-2 border-t border-[#2A2F37] flex flex-col sm:flex-row items-stretch sm:items-center gap-2" on:submit=handle_own_2fa_enable>
                                            <input
                                                type="text"
                                                maxlength="6"
                                                required
                                                placeholder="Enter 6-digit code from Google Authenticator..."
                                                class="flex-1 px-3 py-2 rounded-lg bg-[#1E2227] border border-[#2A2F37] text-xs font-mono tracking-widest text-white focus:outline-none focus:border-[#06B6D4]"
                                                prop:value=own_totp_verify_code
                                                on:input=move |ev| own_totp_verify_code.set(event_target_value(&ev))
                                            />
                                            <button
                                                type="submit"
                                                class="px-4 py-2 rounded-lg bg-[#06B6D4] hover:bg-[#0284C7] text-slate-950 font-bold text-xs shadow transition shrink-0"
                                            >
                                                "Verify & Activate 2FA"
                                            </button>
                                            <button
                                                type="button"
                                                on:click=move |_| own_totp_setup_data.set(None)
                                                class="px-3 py-2 rounded-lg border border-[#2A2F37] text-xs text-slate-300 hover:text-white"
                                            >
                                                "Cancel"
                                            </button>
                                        </form>
                                    </div>
                                }
                                .into_any()
                            } else if is_enabled {
                                view! {
                                    <div class="space-y-3">
                                        <div class="p-3.5 rounded-xl bg-emerald-500/10 border border-emerald-500/30 flex flex-col sm:flex-row sm:items-center justify-between gap-3">
                                            <div class="text-xs text-emerald-200">
                                                "Your account is protected with Google Authenticator 2FA. You can rotate your QR code or disable 2FA below."
                                            </div>
                                            <button
                                                type="button"
                                                on:click=handle_own_2fa_setup
                                                class="px-3 py-1.5 rounded-lg bg-[#06B6D4]/20 hover:bg-[#06B6D4]/30 border border-[#06B6D4]/40 text-[#06B6D4] text-xs font-semibold shrink-0"
                                            >
                                                "🔄 Show / Rotate QR Code"
                                            </button>
                                        </div>
                                        <form class="flex items-center gap-2" on:submit=handle_own_2fa_disable>
                                            <input
                                                type="password"
                                                required
                                                placeholder="Confirm current password to disable 2FA..."
                                                class="flex-1 px-3 py-1.5 rounded-lg bg-[#14171A] border border-[#2A2F37] text-xs text-white"
                                                prop:value=own_totp_disable_pwd
                                                on:input=move |ev| own_totp_disable_pwd.set(event_target_value(&ev))
                                            />
                                            <button
                                                type="submit"
                                                class="px-3 py-1.5 rounded-lg bg-red-500/15 hover:bg-red-500/25 border border-red-500/30 text-red-300 text-xs font-semibold shrink-0"
                                            >
                                                "Disable My 2FA"
                                            </button>
                                        </form>
                                    </div>
                                }
                                .into_any()
                            } else {
                                view! {
                                    <div class="p-4 rounded-xl bg-[#14171A] border border-[#2A2F37] flex flex-col sm:flex-row sm:items-center justify-between gap-3">
                                        <div class="text-xs text-slate-300">
                                            "Protect your Admin account with a 6-digit time-based code from the Google Authenticator app."
                                        </div>
                                        <button
                                            type="button"
                                            on:click=handle_own_2fa_setup
                                            class="px-4 py-2 rounded-xl bg-[#06B6D4] hover:bg-[#0284C7] text-slate-950 font-bold text-xs shadow transition shrink-0"
                                        >
                                            "📲 Enable 2FA / Google Authenticator"
                                        </button>
                                    </div>
                                }
                                .into_any()
                            }
                        }}
                    </section>

                    // Card B: Google Authentication & 2FA Policy Configuration (`SuperAdmin` Only)
                    <Show
                        when=move || auth.is_super_admin()
                        fallback=move || {
                            view! {
                                <section class="bg-[#1E2227] border border-[#2A2F37] rounded-2xl p-6 space-y-3 shadow-xl flex flex-col justify-between">
                                    <div class="space-y-2">
                                        <h2 class="text-base font-bold text-white">
                                            "🌐 Google Authentication & 2FA Security Policy"
                                        </h2>
                                        <p class="text-xs text-slate-400">
                                            "SuperAdmin controls global Google Workspace OAuth 2.0 Sign-In settings. As an Admin, you can manage 2FA / Google Authenticator for standard Users in the directory below."
                                        </p>
                                    </div>
                                    <div class="p-3.5 rounded-xl bg-[#14171A] border border-[#2A2F37] text-xs text-slate-300">
                                        "Tip: Click 'Enable 2FA / Google Authenticator' on any standard User row below to generate a scannable QR code and test their 6-digit code."
                                    </div>
                                </section>
                            }
                        }
                    >
                        <section class="bg-[#1E2227] border border-[#2A2F37] rounded-2xl p-6 space-y-4 shadow-xl">
                            <div>
                                <h2 class="text-base font-bold text-white">
                                    "🌐 Google Authentication & 2FA Policy"
                                </h2>
                                <p class="text-xs text-slate-400 mt-0.5">
                                    "Configure Sign in with Google (OAuth 2.0) and enforce Google Authenticator 2FA after Google login."
                                </p>
                            </div>

                            <form class="space-y-3" on:submit=handle_save_auth_settings>
                                <label class="flex items-center gap-2.5 p-2.5 rounded-xl bg-[#14171A] border border-[#2A2F37] cursor-pointer">
                                    <input
                                        type="checkbox"
                                        class="rounded accent-[#F97316] w-4 h-4"
                                        prop:checked=google_oauth_enabled
                                        on:change=move |ev| google_oauth_enabled.set(event_target_checked(&ev))
                                    />
                                    <span class="text-xs font-semibold text-white">
                                        "Enable Sign in with Google (OAuth 2.0)"
                                    </span>
                                </label>

                                <div class="grid grid-cols-1 sm:grid-cols-2 gap-3">
                                    <div>
                                        <label class="block text-[11px] text-slate-400 mb-1">
                                            "Google OAuth Client ID"
                                        </label>
                                        <input
                                            type="text"
                                            placeholder="1234567890-abc.apps.googleusercontent.com"
                                            class="w-full px-3 py-2 rounded-lg bg-[#14171A] border border-[#2A2F37] text-xs text-white font-mono focus:outline-none focus:border-[#06B6D4]"
                                            prop:value=google_client_id
                                            on:input=move |ev| google_client_id.set(event_target_value(&ev))
                                        />
                                    </div>
                                    <div>
                                        <label class="block text-[11px] text-slate-400 mb-1">
                                            "Google OAuth Client Secret"
                                        </label>
                                        <input
                                            type="password"
                                            placeholder="GOCSPX-••••••••••••••••"
                                            class="w-full px-3 py-2 rounded-lg bg-[#14171A] border border-[#2A2F37] text-xs text-white font-mono focus:outline-none focus:border-[#06B6D4]"
                                            prop:value=google_client_secret
                                            on:input=move |ev| google_client_secret.set(event_target_value(&ev))
                                        />
                                    </div>
                                </div>

                                <div>
                                    <label class="block text-[11px] text-slate-400 mb-1">
                                        "Allowed Email Domains (comma-separated, optional filter)"
                                    </label>
                                    <input
                                        type="text"
                                        placeholder="pyramakerz.com, gmail.com"
                                        class="w-full px-3 py-2 rounded-lg bg-[#14171A] border border-[#2A2F37] text-xs text-white font-mono focus:outline-none focus:border-[#06B6D4]"
                                        prop:value=google_allowed_domains
                                        on:input=move |ev| google_allowed_domains.set(event_target_value(&ev))
                                    />
                                </div>

                                <label class="flex items-center gap-2.5 p-2.5 rounded-xl bg-[#14171A] border border-[#2A2F37] cursor-pointer">
                                    <input
                                        type="checkbox"
                                        class="rounded accent-[#06B6D4] w-4 h-4"
                                        prop:checked=require_2fa_after_google
                                        on:change=move |ev| require_2fa_after_google.set(event_target_checked(&ev))
                                    />
                                    <span class="text-xs font-semibold text-slate-200">
                                        "Require 2FA (Google Authenticator) even when signing in with Google"
                                    </span>
                                </label>

                                <div class="pt-1 flex justify-end">
                                    <button
                                        type="submit"
                                        disabled=move || saving_auth_settings.get()
                                        class="px-4 py-2 rounded-xl bg-[#F97316] hover:bg-[#EA580C] disabled:opacity-50 text-white text-xs font-bold shadow transition"
                                    >
                                        {move || {
                                            if saving_auth_settings.get() {
                                                "Saving..."
                                            } else {
                                                "💾 Save Authentication Settings"
                                            }
                                        }}
                                    </button>
                                </div>
                            </form>
                        </section>
                    </Show>
                </div>

                // Phase 18: Local AI Copilot (`PyraAI` / Ollama) Configuration Section (`SuperAdmin` Only)
                <Show when=move || auth.is_super_admin()>
                    <section class="bg-[#1E2227] border border-[#2A2F37] rounded-2xl p-6 space-y-4 shadow-xl">
                        <div class="flex flex-col sm:flex-row sm:items-center justify-between gap-2">
                            <div>
                                <h2 class="text-base font-bold text-white flex items-center gap-2">
                                    <span>"🤖 Local AI Copilot (PyraAI / Ollama) Configuration"</span>
                                </h2>
                                <p class="text-xs text-slate-400 mt-0.5">
                                    "Configure the self-hosted Ollama endpoint, LLM model (e.g. qwen2.5:1.5b), and system prompt for Natural Language Search, Executive Reports, and Autonomous Grid Task Execution."
                                </p>
                            </div>
                            <span class=move || {
                                if ai_enabled.get() {
                                    "px-2.5 py-1 rounded-lg bg-emerald-500/15 border border-emerald-500/40 text-emerald-300 text-xs font-bold shrink-0"
                                } else {
                                    "px-2.5 py-1 rounded-lg bg-slate-800 border border-slate-700 text-slate-400 text-xs shrink-0"
                                }
                            }>
                                {move || if ai_enabled.get() { "✨ PyraAI Enabled" } else { "PyraAI Disabled" }}
                            </span>
                        </div>

                        <form class="space-y-3" on:submit=handle_save_ai_settings>
                            <div class="grid grid-cols-1 sm:grid-cols-3 gap-3 items-center">
                                <label class="flex items-center gap-2.5 p-2.5 rounded-xl bg-[#14171A] border border-[#2A2F37] cursor-pointer">
                                    <input
                                        type="checkbox"
                                        class="rounded accent-[#06B6D4] w-4 h-4"
                                        prop:checked=ai_enabled
                                        on:change=move |ev| ai_enabled.set(event_target_checked(&ev))
                                    />
                                    <span class="text-xs font-semibold text-white">
                                        "Enable PyraAI Copilot"
                                    </span>
                                </label>

                                <div>
                                    <label class="block text-[11px] text-slate-400 mb-1">
                                        "Ollama Base URL"
                                    </label>
                                    <input
                                        type="text"
                                        placeholder="http://10.10.10.4:11434"
                                        class="w-full px-3 py-2 rounded-lg bg-[#14171A] border border-[#2A2F37] text-xs text-white font-mono focus:outline-none focus:border-[#06B6D4]"
                                        prop:value=ollama_base_url
                                        on:input=move |ev| ollama_base_url.set(event_target_value(&ev))
                                    />
                                </div>

                                <div>
                                    <label class="block text-[11px] text-slate-400 mb-1">
                                        "Ollama Model Name"
                                    </label>
                                    <input
                                        type="text"
                                        placeholder="qwen2.5:1.5b"
                                        class="w-full px-3 py-2 rounded-lg bg-[#14171A] border border-[#2A2F37] text-xs text-[#06B6D4] font-mono focus:outline-none focus:border-[#06B6D4]"
                                        prop:value=ai_model_name
                                        on:input=move |ev| ai_model_name.set(event_target_value(&ev))
                                    />
                                </div>
                            </div>

                            <div>
                                <label class="block text-[11px] text-slate-400 mb-1">
                                    "PyraAI System Prompt"
                                </label>
                                <textarea
                                    rows="2"
                                    placeholder="You are PyraAI, the intelligent workspace assistant and task executor for PyraOcean."
                                    class="w-full px-3 py-2 rounded-lg bg-[#14171A] border border-[#2A2F37] text-xs text-slate-200 focus:outline-none focus:border-[#06B6D4]"
                                    prop:value=ai_system_prompt
                                    on:input=move |ev| ai_system_prompt.set(event_target_value(&ev))
                                />
                            </div>

                            <div class="flex justify-end">
                                <button
                                    type="submit"
                                    disabled=move || saving_ai_settings.get()
                                    class="px-4 py-2 rounded-xl bg-[#06B6D4] hover:bg-[#0284C7] disabled:opacity-50 text-slate-950 text-xs font-bold shadow transition"
                                >
                                    {move || {
                                        if saving_ai_settings.get() {
                                            "Saving PyraAI..."
                                        } else {
                                            "💾 Save PyraAI Settings"
                                        }
                                    }}
                                </button>
                            </div>
                        </form>
                    </section>
                </Show>

                <section class="bg-[#1E2227] border border-[#2A2F37] rounded-2xl p-6 space-y-4 shadow-xl">
                    <div class="flex items-center justify-between">
                        <div>
                            <h2 class="text-base font-bold text-white">"Global User Directory & Security Controls"</h2>
                            <p class="text-xs text-slate-400">
                                "Manage global roles, reset user passwords, generate recovery codes, and enforce or reset Google Authenticator 2FA"
                            </p>
                        </div>
                        <Show when=move || auth.is_super_admin()>
                            <button
                                type="button"
                                on:click=move |_| show_create_user_modal.set(true)
                                class="px-4 py-2 rounded-lg bg-[#F97316] hover:bg-[#EA580C] text-white text-xs font-semibold shadow transition"
                            >
                                "+ Create User"
                            </button>
                        </Show>
                    </div>

                    <div class="overflow-x-auto">
                        <table class="w-full text-left border-collapse text-xs">
                            <thead>
                                <tr class="border-b border-[#2A2F37] text-slate-400 uppercase text-[11px]">
                                    <th class="py-3 px-3">"Display Name"</th>
                                    <th class="py-3 px-3">"Email"</th>
                                    <th class="py-3 px-3">"Global Role"</th>
                                    <th class="py-3 px-3">"2FA / Security Status"</th>
                                    <th class="py-3 px-3">"Role Hierarchy"</th>
                                    <th class="py-3 px-3 text-right">"Password, 2FA & Account Actions"</th>
                                </tr>
                            </thead>
                            <tbody class="divide-y divide-[#2A2F37]/60">
                                {move || {
                                    let is_caller_sa = auth.is_super_admin();
                                    let is_caller_admin = auth.is_admin();
                                    let caller_uid = auth.user.get().map(|u| u.id);

                                    users
                                        .get()
                                        .into_iter()
                                        .map(|u| {
                                            let uid = u.id;
                                            let u_email = u.email.clone();
                                            let u_email_del = u.email.clone();
                                            let is_target_sa = u.system_role == SystemRole::SuperAdmin;
                                            let is_target_admin = u.system_role == SystemRole::Admin;
                                            let is_target_user = u.system_role == SystemRole::User;
                                            let is_self = caller_uid == Some(uid);

                                            // Hierarchy check:
                                            // - SuperAdmin can manage Admin & User (and reset own password/2FA, but NEVER demote/delete SuperAdmin).
                                            // - Admin can ONLY manage standard User accounts (and enroll their own 2FA).
                                            let can_manage_credentials = (is_caller_sa && !is_target_sa)
                                                || (is_caller_admin && !is_caller_sa && is_target_user);

                                            let badge_cls = match u.system_role {
                                                SystemRole::SuperAdmin => "bg-[#F97316]/20 text-[#F97316] border-[#F97316]/40",
                                                SystemRole::Admin => "bg-[#06B6D4]/20 text-[#06B6D4] border-[#06B6D4]/40",
                                                SystemRole::User => "bg-slate-700/40 text-slate-300 border-slate-600",
                                            };
                                            let user_for_pwd = u.clone();
                                            let user_for_2fa = u.clone();
                                            let user_for_self_2fa = u.clone();
                                            let totp_on = u.totp_enabled;
                                            let reset_req = u.password_reset_requested_at;

                                            view! {
                                                <tr class="hover:bg-slate-800/30">
                                                    <td class="py-3 px-3 font-medium text-white">
                                                        <div>{u.display_name.clone()}</div>
                                                        <div class="text-[10px] text-slate-500">
                                                            "Joined " {u.created_at.format("%Y-%m-%d").to_string()}
                                                        </div>
                                                    </td>
                                                    <td class="py-3 px-3 text-slate-300 font-mono">{u_email}</td>
                                                    <td class="py-3 px-3">
                                                        <span class=format!("px-2 py-0.5 rounded border text-[11px] font-semibold {badge_cls}")>
                                                            {u.system_role.label()}
                                                        </span>
                                                    </td>
                                                    <td class="py-3 px-3">
                                                        <div class="flex flex-wrap items-center gap-1.5">
                                                            {if totp_on {
                                                                view! {
                                                                    <span class="px-2 py-0.5 rounded bg-emerald-500/15 border border-emerald-500/40 text-emerald-300 text-[10px] font-semibold">
                                                                        "🔒 Google Auth 2FA Active"
                                                                    </span>
                                                                }
                                                                .into_any()
                                                            } else {
                                                                view! {
                                                                    <span class="px-2 py-0.5 rounded bg-slate-800 border border-slate-700 text-slate-400 text-[10px]">
                                                                        "2FA Off"
                                                                    </span>
                                                                }
                                                                .into_any()
                                                            }}
                                                            {reset_req.map(|ts| {
                                                                view! {
                                                                    <span
                                                                        title=format!("Requested at {}", ts.format("%Y-%m-%d %H:%M UTC"))
                                                                        class="px-2 py-0.5 rounded bg-amber-500/20 border border-amber-500/40 text-amber-300 text-[10px] font-semibold animate-pulse"
                                                                    >
                                                                        "🔑 Reset Requested"
                                                                    </span>
                                                                }
                                                            })}
                                                        </div>
                                                    </td>
                                                    <td class="py-3 px-3">
                                                        {if is_target_sa {
                                                            view! {
                                                                <span class="inline-flex items-center gap-1 px-2.5 py-1 rounded-lg bg-[#F97316]/15 border border-[#F97316]/40 text-[#F97316] text-[11px] font-semibold">
                                                                    "🛡️ Protected Super Admin"
                                                                </span>
                                                            }
                                                            .into_any()
                                                        } else if is_caller_sa {
                                                            view! {
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
                                                            }
                                                            .into_any()
                                                        } else if is_target_admin {
                                                            view! {
                                                                <span class="inline-flex items-center gap-1 px-2 py-0.5 rounded bg-slate-800 border border-slate-700 text-slate-400 text-[11px]">
                                                                    "🔒 Protected Admin"
                                                                </span>
                                                            }
                                                            .into_any()
                                                        } else {
                                                            view! {
                                                                <span class="text-[11px] text-slate-400">
                                                                    "Standard User"
                                                                </span>
                                                            }
                                                            .into_any()
                                                        }}
                                                    </td>
                                                    <td class="py-3 px-3 text-right">
                                                        {if is_target_sa {
                                                            view! {
                                                                <div class="inline-flex items-center gap-1.5">
                                                                    <span class="inline-flex items-center gap-1 px-2.5 py-1 rounded-lg bg-[#F97316]/10 border border-[#F97316]/30 text-[#F97316] text-[11px] font-semibold">
                                                                        "🛡️ Protected Super Admin"
                                                                    </span>
                                                                    {if is_self {
                                                                        let self_user = user_for_self_2fa.clone();
                                                                        view! {
                                                                            <button
                                                                                type="button"
                                                                                on:click=move |_| {
                                                                                    manage_2fa_result.set(None);
                                                                                    manage_2fa_verify_status.set(None);
                                                                                    manage_2fa_verify_code.set(String::new());
                                                                                    manage_2fa_target_user.set(Some(self_user.clone()));
                                                                                }
                                                                                class="px-2.5 py-1 rounded bg-[#06B6D4]/15 hover:bg-[#06B6D4]/25 border border-[#06B6D4]/40 text-[#06B6D4] text-[11px] font-semibold transition"
                                                                            >
                                                                                "📲 Enable 2FA / Google Authenticator"
                                                                            </button>
                                                                        }
                                                                        .into_any()
                                                                    } else {
                                                                        view! { <span /> }.into_any()
                                                                    }}
                                                                </div>
                                                            }
                                                            .into_any()
                                                        } else if can_manage_credentials {
                                                            let email_for_del = u_email_del.clone();
                                                            view! {
                                                                <div class="inline-flex items-center gap-1.5">
                                                                    <button
                                                                        type="button"
                                                                        on:click=move |_| {
                                                                            reset_pwd_new_password.set(String::new());
                                                                            reset_pwd_result.set(None);
                                                                            reset_pwd_target_user.set(Some(user_for_pwd.clone()));
                                                                        }
                                                                        class="px-2.5 py-1 rounded bg-[#F97316]/15 hover:bg-[#F97316]/25 border border-[#F97316]/40 text-[#F97316] text-[11px] font-semibold transition"
                                                                    >
                                                                        "🔑 Reset Password"
                                                                    </button>
                                                                    <button
                                                                        type="button"
                                                                        on:click=move |_| {
                                                                            manage_2fa_result.set(None);
                                                                            manage_2fa_verify_status.set(None);
                                                                            manage_2fa_verify_code.set(String::new());
                                                                            manage_2fa_target_user.set(Some(user_for_2fa.clone()));
                                                                        }
                                                                        class="px-2.5 py-1 rounded bg-[#06B6D4]/15 hover:bg-[#06B6D4]/25 border border-[#06B6D4]/40 text-[#06B6D4] text-[11px] font-semibold transition"
                                                                    >
                                                                        "📲 Enable 2FA / Google Authenticator"
                                                                    </button>
                                                                    <Show when=move || is_caller_sa>
                                                                        {
                                                                            let del_email = email_for_del.clone();
                                                                            view! {
                                                                                <button
                                                                                    type="button"
                                                                                    on:click=move |_| handle_delete_user(uid, del_email.clone())
                                                                                    class="px-2 py-1 rounded bg-red-500/10 hover:bg-red-500/20 border border-red-500/30 text-red-400 text-[11px] font-semibold transition"
                                                                                >
                                                                                    "Delete"
                                                                                </button>
                                                                            }
                                                                        }
                                                                    </Show>
                                                                </div>
                                                            }
                                                            .into_any()
                                                        } else if is_self {
                                                            let self_user = user_for_self_2fa.clone();
                                                            view! {
                                                                <button
                                                                    type="button"
                                                                    on:click=move |_| {
                                                                        manage_2fa_result.set(None);
                                                                        manage_2fa_verify_status.set(None);
                                                                        manage_2fa_verify_code.set(String::new());
                                                                        manage_2fa_target_user.set(Some(self_user.clone()));
                                                                    }
                                                                    class="px-2.5 py-1 rounded bg-[#06B6D4]/15 hover:bg-[#06B6D4]/25 border border-[#06B6D4]/40 text-[#06B6D4] text-[11px] font-semibold transition"
                                                                >
                                                                    "📲 Enable 2FA / Google Authenticator"
                                                                </button>
                                                            }
                                                            .into_any()
                                                        } else {
                                                            view! {
                                                                <span class="text-[11px] text-slate-500 italic">
                                                                    "Protected Peer Admin"
                                                                </span>
                                                            }
                                                            .into_any()
                                                        }}
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

                // Create User Modal (SuperAdmin)
                <Show when=move || show_create_user_modal.get() && auth.is_super_admin()>
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

                // Reset User Password Modal
                <Show when=move || reset_pwd_target_user.get().is_some()>
                    <div class="fixed inset-0 z-50 bg-black/75 flex items-center justify-center p-4">
                        <div class="w-full max-w-md bg-[#1E2227] border border-[#2A2F37] rounded-2xl p-6 shadow-2xl space-y-4">
                            {move || {
                                let Some(target) = reset_pwd_target_user.get() else {
                                    return view! { <div /> }.into_any();
                                };
                                view! {
                                    <div class="space-y-4">
                                        <div class="flex items-center justify-between">
                                            <div>
                                                <h3 class="text-base font-bold text-white">
                                                    "🔑 Reset Password — " {target.display_name.clone()}
                                                </h3>
                                                <p class="text-xs text-slate-400 font-mono">{target.email.clone()}</p>
                                            </div>
                                            <button
                                                type="button"
                                                on:click=move |_| {
                                                    reset_pwd_target_user.set(None);
                                                    reset_pwd_result.set(None);
                                                }
                                                class="text-slate-400 hover:text-white"
                                            >
                                                "✕"
                                            </button>
                                        </div>

                                        <form class="space-y-3" on:submit=handle_admin_reset_password>
                                            <div>
                                                <label class="block text-xs text-slate-400 mb-1">
                                                    "New Password (min 8 characters)"
                                                </label>
                                                <input
                                                    type="password"
                                                    required
                                                    placeholder="Enter new password for user..."
                                                    class="w-full px-3 py-2 rounded-lg bg-[#14171A] border border-[#2A2F37] text-xs text-white focus:outline-none focus:border-[#F97316]"
                                                    prop:value=reset_pwd_new_password
                                                    on:input=move |ev| reset_pwd_new_password.set(event_target_value(&ev))
                                                />
                                            </div>

                                            <div class="flex justify-end gap-2">
                                                <button
                                                    type="button"
                                                    on:click=move |_| {
                                                        reset_pwd_target_user.set(None);
                                                        reset_pwd_result.set(None);
                                                    }
                                                    class="px-3 py-1.5 rounded-lg border border-[#2A2F37] text-xs text-slate-300"
                                                >
                                                    "Close"
                                                </button>
                                                <button
                                                    type="submit"
                                                    class="px-4 py-1.5 rounded-lg bg-[#F97316] hover:bg-[#EA580C] text-white text-xs font-semibold shadow"
                                                >
                                                    "Set Password & Generate Recovery Code"
                                                </button>
                                            </div>
                                        </form>

                                        {move || {
                                            reset_pwd_result.get().map(|res| {
                                                let code = res.generated_recovery_code.clone();
                                                let code_copy = code.clone();
                                                view! {
                                                    <div class="p-3.5 rounded-xl bg-emerald-500/10 border border-emerald-500/40 space-y-2">
                                                        <div class="text-xs font-bold text-emerald-300">
                                                            "✓ Password Reset Complete"
                                                        </div>
                                                        <p class="text-[11px] text-slate-300">
                                                            "Share this one-time Emergency Recovery Code with the user so they can reset their password or pass 2FA recovery in the future:"
                                                        </p>
                                                        <div class="flex items-center justify-between gap-2 p-2 rounded-lg bg-[#14171A] border border-emerald-500/30 font-mono text-xs text-[#06B6D4]">
                                                            <span>{code}</span>
                                                            <button
                                                                type="button"
                                                                on:click=move |_| copy_text_to_clipboard(&code_copy)
                                                                class="px-2 py-1 rounded bg-slate-800 hover:bg-slate-700 text-slate-200 text-[10px] font-sans font-semibold"
                                                            >
                                                                "Copy"
                                                            </button>
                                                        </div>
                                                    </div>
                                                }
                                            })
                                        }}
                                    </div>
                                }
                                .into_any()
                            }}
                        </div>
                    </div>
                </Show>

                // Google Authenticator 2FA Setup & Enforcement Modal
                <Show when=move || manage_2fa_target_user.get().is_some()>
                    <div class="fixed inset-0 z-50 bg-black/75 flex items-center justify-center p-4 overflow-y-auto">
                        <div class="w-full max-w-xl bg-[#1E2227] border border-[#2A2F37] rounded-2xl p-6 shadow-2xl space-y-4 my-8">
                            {move || {
                                let Some(target) = manage_2fa_target_user.get() else {
                                    return view! { <div /> }.into_any();
                                };
                                let is_enabled = target.totp_enabled;
                                view! {
                                    <div class="space-y-4">
                                        <div class="flex items-center justify-between">
                                            <div>
                                                <h3 class="text-base font-bold text-white">
                                                    "📲 Google Authenticator 2FA Setup — " {target.display_name.clone()}
                                                </h3>
                                                <p class="text-xs text-slate-400 font-mono">{target.email.clone()}</p>
                                            </div>
                                            <button
                                                type="button"
                                                on:click=move |_| {
                                                    manage_2fa_target_user.set(None);
                                                    manage_2fa_result.set(None);
                                                    manage_2fa_verify_status.set(None);
                                                    manage_2fa_verify_code.set(String::new());
                                                }
                                                class="text-slate-400 hover:text-white"
                                            >
                                                "✕"
                                            </button>
                                        </div>

                                        <div class="p-3.5 rounded-xl bg-[#14171A] border border-[#2A2F37] flex items-center justify-between">
                                            <div>
                                                <div class="text-xs font-semibold text-white">
                                                    "Current Google Authenticator 2FA Status"
                                                </div>
                                                <div class="text-[11px] text-slate-400">
                                                    {if is_enabled {
                                                        "Google Authenticator 6-digit code is REQUIRED at login for this user."
                                                    } else {
                                                        "Google Authenticator 2FA is not currently active for this user."
                                                    }}
                                                </div>
                                            </div>
                                            <span class=if is_enabled {
                                                "px-2.5 py-1 rounded bg-emerald-500/15 border border-emerald-500/40 text-emerald-300 text-xs font-bold"
                                            } else {
                                                "px-2.5 py-1 rounded bg-slate-800 border border-slate-700 text-slate-400 text-xs"
                                            }>
                                                {if is_enabled { "🔒 Google Auth 2FA Active" } else { "Disabled" }}
                                            </span>
                                        </div>

                                        <div class="grid grid-cols-1 sm:grid-cols-2 gap-3">
                                            <button
                                                type="button"
                                                on:click=move |_| handle_admin_2fa_action("setup")
                                                class="p-3 rounded-xl bg-[#06B6D4]/15 hover:bg-[#06B6D4]/25 border border-[#06B6D4]/40 text-left transition space-y-1"
                                            >
                                                <div class="text-xs font-bold text-[#06B6D4]">
                                                    "📲 Generate Google Authenticator QR Code"
                                                </div>
                                                <p class="text-[11px] text-slate-300">
                                                    "Provision a scannable QR code, Base32 Setup Key, and Emergency Recovery Code for Google Authenticator."
                                                </p>
                                            </button>

                                            <button
                                                type="button"
                                                on:click=move |_| handle_admin_2fa_action("reset")
                                                class="p-3 rounded-xl bg-red-500/10 hover:bg-red-500/20 border border-red-500/30 text-left transition space-y-1"
                                            >
                                                <div class="text-xs font-bold text-red-400">
                                                    "🔓 Reset / Disable 2FA"
                                                </div>
                                                <p class="text-[11px] text-slate-300">
                                                    "Clear the user's Google Authenticator secret if they lost their phone or authenticator app."
                                                </p>
                                            </button>
                                        </div>

                                        {move || {
                                            manage_2fa_result.get().map(|res| {
                                                let status_text = if res.user.totp_enabled {
                                                    format!("Google Authenticator 2FA provisioned for {}", res.user.email)
                                                } else {
                                                    format!("2FA reset and disabled for {}", res.user.email)
                                                };
                                                let qr_svg_opt = res.otpauth_uri.as_ref().map(|uri| generate_qr_code_svg(uri));

                                                view! {
                                                    <div class="p-4 rounded-xl bg-slate-900/90 border border-[#06B6D4]/40 space-y-4">
                                                        <div class="text-xs font-bold text-[#06B6D4]">
                                                            {status_text}
                                                        </div>

                                                        {qr_svg_opt.map(|qr_svg| {
                                                            view! {
                                                                <div class="flex flex-col sm:flex-row items-center gap-4 p-3 rounded-xl bg-[#14171A] border border-[#2A2F37]">
                                                                    <div class="w-44 h-44 p-2.5 rounded-xl bg-white shadow-xl shrink-0 flex items-center justify-center">
                                                                        <div class="w-full h-full" inner_html=qr_svg />
                                                                    </div>
                                                                    <div class="space-y-2 text-xs text-slate-300">
                                                                        <div class="font-bold text-white">
                                                                            "Scan with Google Authenticator App"
                                                                        </div>
                                                                        <ol class="list-decimal list-inside space-y-1 text-[11px] text-slate-300">
                                                                            <li>"Open Google Authenticator on iOS or Android."</li>
                                                                            <li>"Tap '+' and choose 'Scan a QR code'."</li>
                                                                            <li>"Point the camera at this QR code, then test the 6-digit code below."</li>
                                                                        </ol>
                                                                    </div>
                                                                </div>
                                                            }
                                                        })}

                                                        {res.secret.map(|sec| {
                                                            let sec_copy = sec.clone();
                                                            view! {
                                                                <div>
                                                                    <div class="text-[11px] text-slate-400 mb-1 font-semibold">
                                                                        "Manual Setup Key (Base32 — Google Authenticator / Authy / 1Password):"
                                                                    </div>
                                                                    <div class="flex items-center justify-between gap-2 p-2.5 rounded-lg bg-[#14171A] border border-slate-700 font-mono text-xs text-white">
                                                                        <span class="select-all break-all">{sec}</span>
                                                                        <button
                                                                            type="button"
                                                                            on:click=move |_| copy_text_to_clipboard(&sec_copy)
                                                                            class="px-2.5 py-1 rounded bg-[#06B6D4]/20 hover:bg-[#06B6D4]/30 border border-[#06B6D4]/40 text-[#06B6D4] text-[10px] font-sans font-semibold shrink-0"
                                                                        >
                                                                            "📋 Copy Setup Key"
                                                                        </button>
                                                                    </div>
                                                                </div>
                                                            }
                                                        })}

                                                        {res.recovery_code.map(|code| {
                                                            let code_copy = code.clone();
                                                            view! {
                                                                <div>
                                                                    <div class="text-[11px] text-amber-300 mb-1 font-semibold">
                                                                        "Emergency Recovery Code (One-Time Backup):"
                                                                    </div>
                                                                    <div class="flex items-center justify-between gap-2 p-2.5 rounded-lg bg-[#14171A] border border-amber-500/40 font-mono text-xs text-amber-300">
                                                                        <span class="select-all">{code}</span>
                                                                        <button
                                                                            type="button"
                                                                            on:click=move |_| copy_text_to_clipboard(&code_copy)
                                                                            class="px-2.5 py-1 rounded bg-amber-500/20 hover:bg-amber-500/30 border border-amber-500/40 text-amber-200 text-[10px] font-sans font-semibold shrink-0"
                                                                        >
                                                                            "📋 Copy Recovery Code"
                                                                        </button>
                                                                    </div>
                                                                </div>
                                                            }
                                                        })}

                                                        // Live 6-Digit Verification Test Box
                                                        <Show when=move || res.user.totp_enabled>
                                                            <div class="pt-3 border-t border-[#2A2F37] space-y-2">
                                                                <label class="block text-xs font-bold text-white">
                                                                    "Verify Google Authenticator Code (Live 6-Digit Sync Test)"
                                                                </label>
                                                                <form class="flex flex-col sm:flex-row items-stretch sm:items-center gap-2" on:submit=handle_admin_verify_2fa_code>
                                                                    <input
                                                                        type="text"
                                                                        maxlength="6"
                                                                        required
                                                                        placeholder="Enter 6-digit code from app (e.g. 123456)..."
                                                                        class="flex-1 px-3 py-2 rounded-lg bg-[#14171A] border border-[#2A2F37] text-xs font-mono tracking-widest text-white focus:outline-none focus:border-[#06B6D4]"
                                                                        prop:value=manage_2fa_verify_code
                                                                        on:input=move |ev| manage_2fa_verify_code.set(event_target_value(&ev))
                                                                    />
                                                                    <button
                                                                        type="submit"
                                                                        class="px-4 py-2 rounded-lg bg-[#06B6D4] hover:bg-[#0284C7] text-slate-950 font-bold text-xs shadow transition shrink-0"
                                                                    >
                                                                        "✓ Verify Google Authenticator Code"
                                                                    </button>
                                                                </form>
                                                                {move || {
                                                                    manage_2fa_verify_status.get().map(|(ok, msg)| {
                                                                        let cls = if ok {
                                                                            "bg-emerald-500/15 border-emerald-500/40 text-emerald-300"
                                                                        } else {
                                                                            "bg-red-500/15 border-red-500/40 text-red-300"
                                                                        };
                                                                        view! {
                                                                            <div class=format!("p-2.5 rounded-lg border text-xs font-semibold {cls}")>
                                                                                {msg}
                                                                            </div>
                                                                        }
                                                                    })
                                                                }}
                                                            </div>
                                                        </Show>
                                                    </div>
                                                }
                                            })
                                        }}
                                    </div>
                                }
                                .into_any()
                            }}
                        </div>
                    </div>
                </Show>
            </Show>

            // ================================================================
            // TAB 2: Spaces & Granular Access Control (`SuperAdmin` & `Admin`)
            // ================================================================
            <Show when=move || initial_tab.get() == AdminTab::Spaces>
                <div class="grid grid-cols-1 lg:grid-cols-3 gap-6">
                    // Left Column: Spaces List + SuperAdmin/Admin Create Space
                    <div class="bg-[#1E2227] border border-[#2A2F37] rounded-2xl p-5 space-y-4 shadow-xl">
                        <div class="flex items-center justify-between">
                            <h2 class="text-sm font-bold text-white">"Workspaces (Spaces)"</h2>
                            <Show when=move || auth.is_admin()>
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

                            let is_caller_sa = auth.is_super_admin();
                            let is_caller_admin = auth.is_admin();

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

                                    // Grant User Access Form (SuperAdmin & Admin)
                                    <Show when=move || is_caller_admin>
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
                                                    let sa_caller = auth.is_super_admin();
                                                    users
                                                        .get()
                                                        .into_iter()
                                                        .filter(move |u| {
                                                            if sa_caller {
                                                                u.system_role != SystemRole::SuperAdmin
                                                            } else {
                                                                u.system_role == SystemRole::User
                                                            }
                                                        })
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
                                                <Show when=move || is_caller_sa>
                                                    <option value="space_admin">"Space Admin"</option>
                                                </Show>
                                            </select>

                                            <button
                                                type="submit"
                                                class="px-4 py-2 rounded-lg bg-[#06B6D4] hover:bg-[#0284C7] text-slate-950 font-semibold text-xs transition shrink-0"
                                            >
                                                "+ Grant Access"
                                            </button>
                                        </form>
                                    </Show>

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
                                                    let caller_sa = auth.is_super_admin();
                                                    let caller_admin = auth.is_admin();

                                                    space_members
                                                        .get()
                                                        .into_iter()
                                                        .map(|m| {
                                                            let uid = m.user_id;
                                                            let current_role = m.space_role;
                                                            let is_member_sa = m.global_role == SystemRole::SuperAdmin;
                                                            let is_member_admin = m.global_role == SystemRole::Admin
                                                                || m.space_role == SpaceRole::SpaceAdmin;

                                                            // Strict UI Hierarchy Enforcement:
                                                            // 1. SuperAdmin row: always immutable `🛡️ Protected Super Admin`
                                                            // 2. Logged in as SuperAdmin: full controls on all Admin & User rows
                                                            // 3. Logged in as Admin: controls ONLY on standard User rows (`!is_member_sa && !is_member_admin`)
                                                            // 4. Logged in as User: no controls
                                                            let can_modify_member = !is_member_sa
                                                                && (caller_sa || (caller_admin && !is_member_admin));

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
                                                                        {if is_member_sa {
                                                                            view! {
                                                                                <span class="inline-flex items-center gap-1 px-2.5 py-1 rounded-lg bg-[#F97316]/15 border border-[#F97316]/40 text-[#F97316] text-[11px] font-semibold">
                                                                                    "🛡️ Protected Super Admin"
                                                                                </span>
                                                                            }
                                                                            .into_any()
                                                                        } else if can_modify_member {
                                                                            view! {
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
                                                                                    <Show when=move || caller_sa>
                                                                                        <option value="space_admin" selected=current_role == SpaceRole::SpaceAdmin>
                                                                                            "Space Admin"
                                                                                        </option>
                                                                                    </Show>
                                                                                </select>
                                                                            }
                                                                            .into_any()
                                                                        } else {
                                                                            view! {
                                                                                <span class="px-2.5 py-1 rounded bg-slate-800 border border-slate-700 text-slate-300 text-[11px] font-semibold">
                                                                                    {current_role.label()}
                                                                                </span>
                                                                            }
                                                                            .into_any()
                                                                        }}
                                                                    </td>
                                                                    <td class="py-2.5 px-3 text-right">
                                                                        {if is_member_sa {
                                                                            view! {
                                                                                <span class="inline-flex items-center gap-1 px-2.5 py-1 rounded-lg bg-[#F97316]/10 border border-[#F97316]/30 text-[#F97316] text-[11px] font-semibold">
                                                                                    "🛡️ Protected Super Admin"
                                                                                </span>
                                                                            }
                                                                            .into_any()
                                                                        } else if can_modify_member {
                                                                            view! {
                                                                                <button
                                                                                    type="button"
                                                                                    on:click=move |_| handle_revoke_member(active_sid, uid)
                                                                                    class="px-2.5 py-1 rounded bg-red-500/10 hover:bg-red-500/20 text-red-400 text-[11px] font-semibold"
                                                                                >
                                                                                    "Revoke"
                                                                                </button>
                                                                            }
                                                                            .into_any()
                                                                        } else {
                                                                            view! {
                                                                                <span class="text-[11px] text-slate-500 italic">
                                                                                    "🔒 Protected Admin"
                                                                                </span>
                                                                            }
                                                                            .into_any()
                                                                        }}
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

                // Create Space Modal (SuperAdmin & Admin)
                <Show when=move || show_create_space_modal.get() && auth.is_admin()>
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
                                "Immutable trail of user logins, 2FA events, password resets, role changes, Space mutations, and database backups"
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

            // ================================================================
            // TAB 4: Full Database Backup & Restore (`SuperAdmin` Only)
            // ================================================================
            <Show when=move || initial_tab.get() == AdminTab::BackupRestore && auth.is_super_admin()>
                <div class="grid grid-cols-1 lg:grid-cols-2 gap-6">
                    // Card 1: Export & Download Full Backup
                    <section class="bg-[#1E2227] border border-[#2A2F37] rounded-2xl p-6 space-y-4 shadow-xl flex flex-col justify-between">
                        <div class="space-y-2">
                            <div class="flex items-center gap-2">
                                <span class="text-lg">"💾"</span>
                                <h2 class="text-base font-bold text-white">
                                    "Export Full Workspace Backup (.json)"
                                </h2>
                            </div>
                            <p class="text-xs text-slate-400 leading-relaxed">
                                "Generate a complete, lossless JSON snapshot of your entire PyraOcean PostgreSQL database—including all Users, Password Hashes, 2FA Configurations, Spaces, Access Matrices, Grids, Columns, Rows, and Row Comments."
                            </p>
                            <ul class="text-[11px] text-slate-300 space-y-1 pt-2 font-mono">
                                <li>"• users & 2FA credentials"</li>
                                <li>"• spaces & space_members RBAC matrix"</li>
                                <li>"• pages_grids (active + trashed)"</li>
                                <li>"• grid_columns, grid_rows & grid_row_comments"</li>
                            </ul>
                        </div>

                        <div class="pt-4">
                            <button
                                type="button"
                                disabled=move || backup_exporting.get()
                                on:click=handle_download_backup
                                class="w-full py-2.5 px-4 rounded-xl bg-[#06B6D4] hover:bg-[#0284C7] disabled:opacity-50 text-slate-950 font-bold text-xs shadow-lg transition flex items-center justify-center gap-2"
                            >
                                <span>"⬇"</span>
                                <span>
                                    {move || {
                                        if backup_exporting.get() {
                                            "Exporting Full Database Snapshot..."
                                        } else {
                                            "Download Full Backup (.json)"
                                        }
                                    }}
                                </span>
                            </button>
                        </div>
                    </section>

                    // Card 2: Restore Workspace from Backup JSON
                    <section class="bg-[#1E2227] border border-[#2A2F37] rounded-2xl p-6 space-y-4 shadow-xl">
                        <div class="space-y-1.5">
                            <div class="flex items-center gap-2">
                                <span class="text-lg">"♻️"</span>
                                <h2 class="text-base font-bold text-white">
                                    "Restore Workspace from Backup (.json)"
                                </h2>
                            </div>
                            <p class="text-xs text-slate-400 leading-relaxed">
                                "Upload a previously exported PyraOcean `.json` backup file to idempotently upsert and restore all Users, Spaces, Grids, Columns, Rows, and Comments in a single atomic transaction."
                            </p>
                        </div>

                        <div class="space-y-3">
                            <label class="block p-3.5 rounded-xl bg-[#14171A] border border-dashed border-[#2A2F37] hover:border-[#F97316] cursor-pointer text-center transition">
                                <div class="text-xs font-semibold text-slate-200">
                                    {move || {
                                        restore_file_name
                                            .get()
                                            .map(|n| format!("📄 Loaded file: {n}"))
                                            .unwrap_or_else(|| "Click to select a pyraocean-backup-*.json file".into())
                                    }}
                                </div>
                                <input
                                    type="file"
                                    accept=".json,application/json"
                                    class="hidden"
                                    on:change=handle_restore_file_select
                                />
                            </label>

                            <textarea
                                rows="5"
                                placeholder="Or paste raw backup JSON payload here..."
                                class="w-full p-3 rounded-xl bg-[#14171A] border border-[#2A2F37] text-[11px] font-mono text-slate-200 focus:outline-none focus:border-[#F97316]"
                                prop:value=restore_json_text
                                on:input=move |ev| restore_json_text.set(event_target_value(&ev))
                            />

                            <button
                                type="button"
                                disabled=move || restore_in_progress.get()
                                on:click=handle_execute_restore
                                class="w-full py-2.5 px-4 rounded-xl bg-[#F97316] hover:bg-[#EA580C] disabled:opacity-50 text-white font-bold text-xs shadow-lg transition flex items-center justify-center gap-2"
                            >
                                <span>"⬆"</span>
                                <span>
                                    {move || {
                                        if restore_in_progress.get() {
                                            "Restoring Database Snapshot..."
                                        } else {
                                            "Restore Database from Backup JSON"
                                        }
                                    }}
                                </span>
                            </button>
                        </div>
                    </section>
                </div>
            </Show>
        </div>
    }
}
