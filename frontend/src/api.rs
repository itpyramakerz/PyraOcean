use crate::state::AuthState;
use gloo_net::http::{Request, RequestBuilder};
use leptos::prelude::GetUntracked;
use pyra_ocean_shared::{
    AuditLogEntry, BulkDeleteRowsRequest, BulkDeleteRowsResponse, CreateColumnRequest,
    CreateGridRequest, CreateRowRequest, CreateSpaceRequest, CreateUserRequest,
    GrantSpaceMemberRequest, GridColumn, GridRow, GridSchemaResponse, ImportGridResponse,
    ImportMode, LoginRequest, LoginResponse, PageGrid, PatchRowRequest, RowCellMap, Space,
    SpaceMember, SpaceMemberDetail, SpaceWithRole, SystemRole, UpdateColumnRequest,
    UpdateUserRoleRequest, User, UserProfile,
};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Deserialize)]
struct ApiErrorPayload {
    pub error: String,
}

/// Centralized WASM HTTP client that automatically attaches `Authorization: Bearer <token>`
/// and handles `401 Unauthorized` by clearing `AuthState` and redirecting to `/login`.
pub struct ApiClient;

impl ApiClient {
    fn attach_auth(builder: RequestBuilder, auth: &AuthState) -> RequestBuilder {
        if let Some(token) = auth.token.get_untracked() {
            builder.header("Authorization", &format!("Bearer {token}"))
        } else {
            builder
        }
    }

    fn handle_unauthorized(auth: &AuthState) {
        auth.clear_session();
        if let Some(window) = web_sys::window() {
            let _ = window.location().set_pathname("/login");
        }
    }

    async fn send_json<T: DeserializeOwned>(
        req: Request,
        auth: &AuthState,
    ) -> Result<T, String> {
        let resp = req
            .send()
            .await
            .map_err(|e| format!("Network request failed: {e}"))?;

        if resp.status() == 401 {
            Self::handle_unauthorized(auth);
            return Err("Session expired or unauthorized. Please sign in again.".into());
        }

        if !resp.ok() {
            let err_msg = resp
                .json::<ApiErrorPayload>()
                .await
                .map(|p| p.error)
                .unwrap_or_else(|_| format!("HTTP error {}", resp.status()));
            return Err(err_msg);
        }

        resp.json::<T>()
            .await
            .map_err(|e| format!("Failed to deserialize server response: {e}"))
    }

    async fn send_empty(req: Request, auth: &AuthState) -> Result<(), String> {
        let resp = req
            .send()
            .await
            .map_err(|e| format!("Network request failed: {e}"))?;

        if resp.status() == 401 {
            Self::handle_unauthorized(auth);
            return Err("Session expired or unauthorized. Please sign in again.".into());
        }

        if !resp.ok() {
            let err_msg = resp
                .json::<ApiErrorPayload>()
                .await
                .map(|p| p.error)
                .unwrap_or_else(|_| format!("HTTP error {}", resp.status()));
            return Err(err_msg);
        }

        Ok(())
    }

    fn get_req(url: &str, auth: &AuthState) -> Result<Request, String> {
        Self::attach_auth(Request::get(url), auth)
            .build()
            .map_err(|e| e.to_string())
    }

    fn post_json_req<B: Serialize>(
        url: &str,
        body: &B,
        auth: &AuthState,
    ) -> Result<Request, String> {
        Self::attach_auth(Request::post(url), auth)
            .header("Content-Type", "application/json")
            .json(body)
            .map_err(|e| e.to_string())
    }

    fn patch_json_req<B: Serialize>(
        url: &str,
        body: &B,
        auth: &AuthState,
    ) -> Result<Request, String> {
        Self::attach_auth(Request::patch(url), auth)
            .header("Content-Type", "application/json")
            .json(body)
            .map_err(|e| e.to_string())
    }

    fn delete_json_req<B: Serialize>(
        url: &str,
        body: &B,
        auth: &AuthState,
    ) -> Result<Request, String> {
        Self::attach_auth(Request::delete(url), auth)
            .header("Content-Type", "application/json")
            .json(body)
            .map_err(|e| e.to_string())
    }

    fn delete_req(url: &str, auth: &AuthState) -> Result<Request, String> {
        Self::attach_auth(Request::delete(url), auth)
            .build()
            .map_err(|e| e.to_string())
    }

    // ========================================================================
    // Auth Endpoints
    // ========================================================================

    pub async fn login(email: String, password: String) -> Result<LoginResponse, String> {
        let payload = LoginRequest { email, password };
        let req = Request::post("/api/auth/login")
            .header("Content-Type", "application/json")
            .json(&payload)
            .map_err(|e| e.to_string())?;

        let resp = req
            .send()
            .await
            .map_err(|e| format!("Network error: {e}"))?;

        if !resp.ok() {
            let msg = resp
                .json::<ApiErrorPayload>()
                .await
                .map(|p| p.error)
                .unwrap_or_else(|_| "Invalid email or password".into());
            return Err(msg);
        }

        resp.json::<LoginResponse>()
            .await
            .map_err(|e| format!("Invalid login response: {e}"))
    }

    pub async fn fetch_me(auth: &AuthState) -> Result<UserProfile, String> {
        let req = Self::get_req("/api/auth/me", auth)?;
        Self::send_json(req, auth).await
    }

    // ========================================================================
    // Spaces & Sidebar Tree Endpoints
    // ========================================================================

    pub async fn list_spaces(auth: &AuthState) -> Result<Vec<SpaceWithRole>, String> {
        let req = Self::get_req("/api/spaces", auth)?;
        Self::send_json(req, auth).await
    }

    pub async fn create_space(
        auth: &AuthState,
        payload: CreateSpaceRequest,
    ) -> Result<Space, String> {
        let req = Self::post_json_req("/api/spaces", &payload, auth)?;
        Self::send_json(req, auth).await
    }

    pub async fn delete_space(auth: &AuthState, space_id: Uuid) -> Result<(), String> {
        let url = format!("/api/spaces/{space_id}");
        let req = Self::delete_req(&url, auth)?;
        Self::send_empty(req, auth).await
    }

    pub async fn list_space_grids(
        auth: &AuthState,
        space_id: Uuid,
    ) -> Result<Vec<PageGrid>, String> {
        let url = format!("/api/spaces/{space_id}/grids");
        let req = Self::get_req(&url, auth)?;
        Self::send_json(req, auth).await
    }

    pub async fn create_grid(
        auth: &AuthState,
        space_id: Uuid,
        title: String,
    ) -> Result<GridSchemaResponse, String> {
        let url = format!("/api/spaces/{space_id}/grids");
        let payload = CreateGridRequest {
            title,
            description: None,
            icon: Some("table".into()),
            position: None,
        };
        let req = Self::post_json_req(&url, &payload, auth)?;
        Self::send_json(req, auth).await
    }

    // ========================================================================
    // Space Member Access Matrix Endpoints
    // ========================================================================

    pub async fn list_space_members(
        auth: &AuthState,
        space_id: Uuid,
    ) -> Result<Vec<SpaceMemberDetail>, String> {
        let url = format!("/api/spaces/{space_id}/members");
        let req = Self::get_req(&url, auth)?;
        Self::send_json(req, auth).await
    }

    pub async fn grant_space_member(
        auth: &AuthState,
        space_id: Uuid,
        payload: GrantSpaceMemberRequest,
    ) -> Result<SpaceMember, String> {
        let url = format!("/api/spaces/{space_id}/members");
        let req = Self::post_json_req(&url, &payload, auth)?;
        Self::send_json(req, auth).await
    }

    pub async fn revoke_space_member(
        auth: &AuthState,
        space_id: Uuid,
        user_id: Uuid,
    ) -> Result<(), String> {
        let url = format!("/api/spaces/{space_id}/members/{user_id}");
        let req = Self::delete_req(&url, auth)?;
        Self::send_empty(req, auth).await
    }

    // ========================================================================
    // Super Admin Users & Audit Log Endpoints
    // ========================================================================

    pub async fn list_users(auth: &AuthState) -> Result<Vec<User>, String> {
        let req = Self::get_req("/api/admin/users", auth)?;
        Self::send_json(req, auth).await
    }

    pub async fn create_user(
        auth: &AuthState,
        payload: CreateUserRequest,
    ) -> Result<User, String> {
        let req = Self::post_json_req("/api/admin/users", &payload, auth)?;
        Self::send_json(req, auth).await
    }

    pub async fn update_user_role(
        auth: &AuthState,
        user_id: Uuid,
        system_role: SystemRole,
        is_active: Option<bool>,
    ) -> Result<User, String> {
        let url = format!("/api/admin/users/{user_id}/role");
        let payload = UpdateUserRoleRequest {
            system_role,
            is_active,
        };
        let req = Self::patch_json_req(&url, &payload, auth)?;
        Self::send_json(req, auth).await
    }

    pub async fn list_audit_logs(auth: &AuthState) -> Result<Vec<AuditLogEntry>, String> {
        let req = Self::get_req("/api/admin/audit-logs?limit=200", auth)?;
        Self::send_json(req, auth).await
    }

    // ========================================================================
    // Phase 4: Dynamic Grid Schema, Rows, Bulk Delete, Multipart Upload & Import
    // ========================================================================

    pub async fn get_grid_schema(
        auth: &AuthState,
        grid_id: Uuid,
    ) -> Result<GridSchemaResponse, String> {
        let url = format!("/api/grids/{grid_id}");
        let req = Self::get_req(&url, auth)?;
        Self::send_json(req, auth).await
    }

    pub async fn create_column(
        auth: &AuthState,
        grid_id: Uuid,
        payload: CreateColumnRequest,
    ) -> Result<GridColumn, String> {
        let url = format!("/api/grids/{grid_id}/columns");
        let req = Self::post_json_req(&url, &payload, auth)?;
        Self::send_json(req, auth).await
    }

    pub async fn update_column(
        auth: &AuthState,
        grid_id: Uuid,
        col_id: Uuid,
        payload: UpdateColumnRequest,
    ) -> Result<GridColumn, String> {
        let url = format!("/api/grids/{grid_id}/columns/{col_id}");
        let req = Self::patch_json_req(&url, &payload, auth)?;
        Self::send_json(req, auth).await
    }

    pub async fn list_rows(
        auth: &AuthState,
        grid_id: Uuid,
        sorts_query: Option<String>,
        filter_col: Option<Uuid>,
        filter_op: Option<&str>,
        filter_val: Option<&str>,
    ) -> Result<Vec<GridRow>, String> {
        let mut params: Vec<String> = Vec::new();
        if let Some(s) = sorts_query.filter(|s| !s.is_empty()) {
            params.push(format!("sorts={s}"));
        }
        if let (Some(col), Some(op)) = (filter_col, filter_op) {
            params.push(format!("filter_col={col}"));
            params.push(format!("filter_op={op}"));
            if let Some(val) = filter_val {
                params.push(format!("filter_val={val}"));
            }
        }
        let query_str = if params.is_empty() {
            String::new()
        } else {
            format!("?{}", params.join("&"))
        };
        let url = format!("/api/grids/{grid_id}/rows{query_str}");
        let req = Self::get_req(&url, auth)?;
        Self::send_json(req, auth).await
    }

    pub async fn create_row(
        auth: &AuthState,
        grid_id: Uuid,
        cells: RowCellMap,
    ) -> Result<GridRow, String> {
        let url = format!("/api/grids/{grid_id}/rows");
        let payload = CreateRowRequest {
            row_order: None,
            cells,
        };
        let req = Self::post_json_req(&url, &payload, auth)?;
        Self::send_json(req, auth).await
    }

    pub async fn patch_row(
        auth: &AuthState,
        grid_id: Uuid,
        row_id: Uuid,
        cells: RowCellMap,
    ) -> Result<GridRow, String> {
        let url = format!("/api/grids/{grid_id}/rows/{row_id}");
        let payload = PatchRowRequest {
            row_order: None,
            cells,
        };
        let req = Self::patch_json_req(&url, &payload, auth)?;
        Self::send_json(req, auth).await
    }

    pub async fn bulk_delete_rows(
        auth: &AuthState,
        grid_id: Uuid,
        row_ids: Vec<Uuid>,
    ) -> Result<BulkDeleteRowsResponse, String> {
        let url = format!("/api/grids/{grid_id}/rows/bulk");
        let payload = BulkDeleteRowsRequest { row_ids };
        let req = Self::delete_json_req(&url, &payload, auth)?;
        Self::send_json(req, auth).await
    }

    pub async fn upload_cell_files(
        auth: &AuthState,
        grid_id: Uuid,
        row_id: Uuid,
        column_id: Uuid,
        files: Vec<web_sys::File>,
    ) -> Result<GridRow, String> {
        let form = web_sys::FormData::new().map_err(|_| "Failed to create FormData".to_string())?;
        form.append_with_str("column_id", &column_id.to_string())
            .map_err(|_| "Failed to append column_id".to_string())?;

        for file in files {
            form.append_with_blob_and_filename("file", &file, &file.name())
                .map_err(|_| "Failed to append file to FormData".to_string())?;
        }

        let url = format!("/api/grids/{grid_id}/rows/{row_id}/upload");
        let req = Self::attach_auth(Request::post(&url), auth)
            .body(form)
            .map_err(|e| e.to_string())?;

        Self::send_json(req, auth).await
    }

    pub async fn import_grid_file(
        auth: &AuthState,
        space_id: Uuid,
        title: Option<String>,
        mode: ImportMode,
        file: web_sys::File,
    ) -> Result<ImportGridResponse, String> {
        let form = web_sys::FormData::new().map_err(|_| "Failed to create FormData".to_string())?;
        if let Some(t) = title.filter(|s| !s.trim().is_empty()) {
            form.append_with_str("title", t.trim())
                .map_err(|_| "Failed to append title".to_string())?;
        }

        let mode_str = match mode {
            ImportMode::FormattedSchemaInference => "formatted_schema_inference",
            ImportMode::UnformattedRawText => "unformatted_raw_text",
        };
        form.append_with_str("mode", mode_str)
            .map_err(|_| "Failed to append import mode".to_string())?;

        form.append_with_blob_and_filename("file", &file, &file.name())
            .map_err(|_| "Failed to append import file".to_string())?;

        let url = format!("/api/spaces/{space_id}/grids/import");
        let req = Self::attach_auth(Request::post(&url), auth)
            .body(form)
            .map_err(|e| e.to_string())?;

        Self::send_json(req, auth).await
    }
}
