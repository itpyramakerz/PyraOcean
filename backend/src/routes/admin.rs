use crate::auth::{
    hash_password, record_audit_log, verify_password, AppState, AuthError, AuthUser, Claims,
    RequireAdmin, RequireSuperAdmin,
};
use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    Json,
};
use pyra_ocean_shared::{
    AuditLogEntry, BrandingMetadata, CreateUserRequest, LoginRequest, LoginResponse, SystemRole,
    UpdateUserRoleRequest, User, UserProfile,
};
use serde::Deserialize;
use serde_json::json;
use uuid::Uuid;

#[derive(Debug, Deserialize)]
pub struct AuditLogQuery {
    pub limit: Option<i64>,
    pub offset: Option<i64>,
    pub actor_id: Option<Uuid>,
    pub space_id: Option<Uuid>,
}

// ============================================================================
// 1. Authentication Endpoints (`POST /api/auth/login`, `GET /api/auth/me`)
// ============================================================================

pub async fn login_handler(
    State(state): State<AppState>,
    Json(payload): Json<LoginRequest>,
) -> Result<Json<LoginResponse>, AuthError> {
    let email_norm = payload.email.trim().to_lowercase();

    let user: User = sqlx::query_as(
        r#"
        SELECT id, email, display_name, password_hash, system_role, is_active, created_at, updated_at
        FROM users
        WHERE LOWER(email) = $1
        "#,
    )
    .bind(&email_norm)
    .fetch_optional(&state.db)
    .await?
    .ok_or(AuthError::InvalidCredentials)?;

    if !user.is_active {
        return Err(AuthError::AccountInactive);
    }

    let valid = verify_password(&payload.password, &user.password_hash)?;
    if !valid {
        return Err(AuthError::InvalidCredentials);
    }

    let token = Claims::issue_token(
        user.id,
        user.email.clone(),
        user.system_role,
        &state.jwt_config,
    )?;

    let _ = record_audit_log(
        &state.db,
        user.id,
        None,
        None,
        "auth.login",
        json!({ "email": user.email, "role": user.system_role }),
    )
    .await;

    Ok(Json(LoginResponse {
        token,
        user,
        branding: BrandingMetadata::default(),
    }))
}

/// `GET /api/auth/me`
/// Returns the currently authenticated user's profile.
pub async fn current_user_profile(
    State(state): State<AppState>,
    auth_user: AuthUser,
) -> Result<Json<UserProfile>, AuthError> {
    let user: User = sqlx::query_as(
        r#"
        SELECT id, email, display_name, password_hash, system_role, is_active, created_at, updated_at
        FROM users
        WHERE id = $1
        "#,
    )
    .bind(auth_user.user_id)
    .fetch_optional(&state.db)
    .await?
    .ok_or(AuthError::AccountInactive)?;

    Ok(Json(UserProfile::from(user)))
}

// ============================================================================
// 2. User Directory & SuperAdmin User Management
// ============================================================================

/// `GET /api/admin/users`
/// Lists all registered users (accessible to `SuperAdmin` and `Admin` for Space member assignment).
pub async fn list_users(
    State(state): State<AppState>,
    RequireAdmin(_admin): RequireAdmin,
) -> Result<Json<Vec<User>>, AuthError> {
    let users: Vec<User> = sqlx::query_as(
        r#"
        SELECT id, email, display_name, password_hash, system_role, is_active, created_at, updated_at
        FROM users
        ORDER BY created_at ASC
        "#,
    )
    .fetch_all(&state.db)
    .await?;

    Ok(Json(users))
}

/// `POST /api/admin/users`
/// Creates a new user account and assigns their global `SystemRole`.
pub async fn create_user(
    State(state): State<AppState>,
    RequireSuperAdmin(admin): RequireSuperAdmin,
    Json(payload): Json<CreateUserRequest>,
) -> Result<(StatusCode, Json<User>), AuthError> {
    let email_norm = payload.email.trim().to_lowercase();
    if email_norm.is_empty() || !email_norm.contains('@') {
        return Err(AuthError::BadRequest("A valid email address is required".into()));
    }
    if payload.password.len() < 8 {
        return Err(AuthError::BadRequest(
            "Password must be at least 8 characters long".into(),
        ));
    }

    let password_hash = hash_password(&payload.password)?;

    let created_user: User = sqlx::query_as(
        r#"
        INSERT INTO users (email, display_name, password_hash, system_role, is_active)
        VALUES ($1, $2, $3, $4, TRUE)
        RETURNING id, email, display_name, password_hash, system_role, is_active, created_at, updated_at
        "#,
    )
    .bind(&email_norm)
    .bind(payload.display_name.trim())
    .bind(&password_hash)
    .bind(payload.system_role)
    .fetch_one(&state.db)
    .await?;

    state.cache.set_user_status(
        created_user.id,
        (created_user.system_role, created_user.is_active),
    );

    record_audit_log(
        &state.db,
        admin.user_id,
        None,
        None,
        "admin.user.create",
        json!({
            "target_user_id": created_user.id,
            "email": created_user.email,
            "assigned_role": created_user.system_role
        }),
    )
    .await?;

    Ok((StatusCode::CREATED, Json(created_user)))
}

/// `PATCH /api/admin/users/{id}/role`
/// Promotes or demotes a user's global role or toggles active status.
pub async fn update_user_role(
    State(state): State<AppState>,
    RequireSuperAdmin(admin): RequireSuperAdmin,
    Path(target_user_id): Path<Uuid>,
    Json(payload): Json<UpdateUserRoleRequest>,
) -> Result<Json<User>, AuthError> {
    if target_user_id == admin.user_id
        && (payload.system_role != SystemRole::SuperAdmin || payload.is_active == Some(false))
    {
        return Err(AuthError::BadRequest(
            "SuperAdmin cannot demote or deactivate their own global account".into(),
        ));
    }

    let updated_user: User = sqlx::query_as(
        r#"
        UPDATE users
        SET system_role = $2,
            is_active = COALESCE($3, is_active)
        WHERE id = $1
        RETURNING id, email, display_name, password_hash, system_role, is_active, created_at, updated_at
        "#,
    )
    .bind(target_user_id)
    .bind(payload.system_role)
    .bind(payload.is_active)
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(|| AuthError::NotFound(format!("User {target_user_id} not found")))?;

    state.cache.invalidate_user(&target_user_id);

    record_audit_log(
        &state.db,
        admin.user_id,
        None,
        None,
        "admin.user.role_updated",
        json!({
            "target_user_id": updated_user.id,
            "new_role": updated_user.system_role,
            "is_active": updated_user.is_active
        }),
    )
    .await?;

    Ok(Json(updated_user))
}

/// `GET /api/admin/audit-logs`
/// Retrieves system-wide activity logs for SuperAdmin inspection.
pub async fn list_audit_logs(
    State(state): State<AppState>,
    RequireSuperAdmin(_admin): RequireSuperAdmin,
    Query(params): Query<AuditLogQuery>,
) -> Result<Json<Vec<AuditLogEntry>>, AuthError> {
    let limit = params.limit.unwrap_or(100).clamp(1, 500);
    let offset = params.offset.unwrap_or(0).max(0);

    let logs: Vec<AuditLogEntry> = sqlx::query_as(
        r#"
        SELECT
            al.id,
            al.actor_id,
            u.email AS actor_email,
            al.space_id,
            al.grid_id,
            al.action,
            al.metadata,
            al.ip_address,
            al.created_at
        FROM audit_logs al
        LEFT JOIN users u ON u.id = al.actor_id
        WHERE ($1::uuid IS NULL OR al.actor_id = $1)
          AND ($2::uuid IS NULL OR al.space_id = $2)
        ORDER BY al.created_at DESC
        LIMIT $3 OFFSET $4
        "#,
    )
    .bind(params.actor_id)
    .bind(params.space_id)
    .bind(limit)
    .bind(offset)
    .fetch_all(&state.db)
    .await?;

    Ok(Json(logs))
}
