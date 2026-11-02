use gloo_storage::{LocalStorage, Storage};
use leptos::prelude::*;
use pyra_ocean_shared::{PageGrid, SpaceRole, SpaceWithRole, SystemRole, UserProfile};
use std::collections::HashMap;
use uuid::Uuid;

pub const TOKEN_STORAGE_KEY: &str = "pyra_ocean_token";
pub const PROFILE_STORAGE_KEY: &str = "pyra_ocean_user_profile";
pub const THEME_STORAGE_KEY: &str = "pyra_ocean_dark_mode";

/// Global reactive state for authentication, RBAC permissions, theme mode, and active workspace tree.
#[derive(Clone, Copy)]
pub struct AuthState {
    pub token: RwSignal<Option<String>>,
    pub user: RwSignal<Option<UserProfile>>,
    pub spaces: RwSignal<Vec<SpaceWithRole>>,
    pub space_grids: RwSignal<HashMap<Uuid, Vec<PageGrid>>>,
    pub active_space_id: RwSignal<Option<Uuid>>,
    pub active_grid_id: RwSignal<Option<Uuid>>,
    pub dark_mode: RwSignal<bool>,
    pub sidebar_collapsed: RwSignal<bool>,
}

impl AuthState {
    pub fn init() -> Self {
        let stored_token: Option<String> = LocalStorage::get(TOKEN_STORAGE_KEY).ok();
        let stored_user: Option<UserProfile> = LocalStorage::get(PROFILE_STORAGE_KEY).ok();
        let stored_dark: bool = LocalStorage::get(THEME_STORAGE_KEY).unwrap_or(true);

        Self {
            token: RwSignal::new(stored_token),
            user: RwSignal::new(stored_user),
            spaces: RwSignal::new(Vec::new()),
            space_grids: RwSignal::new(HashMap::new()),
            active_space_id: RwSignal::new(None),
            active_grid_id: RwSignal::new(None),
            dark_mode: RwSignal::new(stored_dark),
            sidebar_collapsed: RwSignal::new(false),
        }
    }

    /// Persists the JWT token and `UserProfile` into `localStorage` and updates reactive signals.
    pub fn set_session(&self, jwt: String, profile: UserProfile) {
        let _ = LocalStorage::set(TOKEN_STORAGE_KEY, &jwt);
        let _ = LocalStorage::set(PROFILE_STORAGE_KEY, &profile);
        self.token.set(Some(jwt));
        self.user.set(Some(profile));
    }

    /// Clears `localStorage` credentials and resets workspace state (used on logout or `401 Unauthorized`).
    pub fn clear_session(&self) {
        LocalStorage::delete(TOKEN_STORAGE_KEY);
        LocalStorage::delete(PROFILE_STORAGE_KEY);
        self.token.set(None);
        self.user.set(None);
        self.spaces.set(Vec::new());
        self.space_grids.set(HashMap::new());
        self.active_space_id.set(None);
        self.active_grid_id.set(None);
    }

    pub fn is_authenticated(&self) -> bool {
        self.token.get().is_some() && self.user.get().is_some()
    }

    /// Reactive check: returns `true` if the current user is the global `SuperAdmin`.
    pub fn is_super_admin(&self) -> bool {
        self.user
            .get()
            .map(|u| u.system_role == SystemRole::SuperAdmin)
            .unwrap_or(false)
    }

    /// Reactive check: returns `true` if the current user is `SuperAdmin` or `Admin`.
    pub fn is_admin(&self) -> bool {
        self.user
            .get()
            .map(|u| matches!(u.system_role, SystemRole::SuperAdmin | SystemRole::Admin))
            .unwrap_or(false)
    }

    /// Reactive check: returns `true` if the user can create/edit grids or rows in `space_id`.
    pub fn can_edit_space(&self, space_id: Uuid) -> bool {
        if self.is_super_admin() {
            return true;
        }
        self.spaces
            .get()
            .iter()
            .find(|s| s.space.id == space_id)
            .map(|s| matches!(s.effective_role, SpaceRole::Editor | SpaceRole::SpaceAdmin))
            .unwrap_or(false)
    }

    /// Reactive check: returns `true` if the user can manage members/columns/imports in `space_id`.
    pub fn can_manage_space(&self, space_id: Uuid) -> bool {
        if self.is_super_admin() {
            return true;
        }
        self.spaces
            .get()
            .iter()
            .find(|s| s.space.id == space_id)
            .map(|s| s.effective_role == SpaceRole::SpaceAdmin)
            .unwrap_or(false)
    }

    /// Toggles Dark/Light mode and persists preference to `localStorage`.
    pub fn toggle_theme(&self) {
        let next = !self.dark_mode.get();
        let _ = LocalStorage::set(THEME_STORAGE_KEY, next);
        self.dark_mode.set(next);
    }

    /// Returns the appropriate PyraOcean brand logo path based on active theme mode.
    pub fn active_logo_url(&self) -> &'static str {
        if self.dark_mode.get() {
            "/public/logo-dark.jpg"
        } else {
            "/public/logo-light.jpg"
        }
    }
}

