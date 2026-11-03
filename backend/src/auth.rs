use argon2::{
    password_hash::{rand_core::OsRng, PasswordHash, PasswordHasher, PasswordVerifier, SaltString},
    Argon2,
};
use axum::{
    extract::{FromRef, FromRequestParts, Path, Request, State},
    http::{header::AUTHORIZATION, request::Parts, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
    Json,
};
use chrono::{Duration, Utc};
use jsonwebtoken::{decode, encode, DecodingKey, EncodingKey, Header, Validation};
use pyra_ocean_shared::{GridColumn, GridSchemaResponse, PageGrid, SpaceRole, SystemRole};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, RwLock},
    time::{Duration as StdDuration, Instant},
};
use uuid::Uuid;

// ============================================================================
// 1. Application State, In-Memory Hot-Path Cache & Auth Configuration
// ============================================================================

const CACHE_TTL: StdDuration = StdDuration::from_secs(60);

struct CacheEntry<T> {
    value: T,
    expires_at: Instant,
}

/// High-speed in-memory TTL cache eliminating repetitive PostgreSQL roundtrips on hot paths:
/// - `user_status`: `(SystemRole, bool)` for `AuthUser` JWT extractor verification
/// - `space_access`: `SpaceRole` for `(user_id, space_id)` RBAC resolution
/// - `grid_space`: `space_id` for `grid_id` lookup
/// - `grid_access`: `SpaceRole` for `(user_id, grid_id)` RBAC resolution
/// - `grid_schema`: `(PageGrid, Vec<GridColumn>)` for `get_grid_schema` and cell validation
#[derive(Default)]
pub struct AppCache {
    user_status: RwLock<HashMap<Uuid, CacheEntry<(SystemRole, bool)>>>,
    space_access: RwLock<HashMap<(Uuid, Uuid), CacheEntry<SpaceRole>>>,
    grid_space: RwLock<HashMap<Uuid, CacheEntry<Uuid>>>,
    grid_access: RwLock<HashMap<(Uuid, Uuid), CacheEntry<SpaceRole>>>,
    grid_schema: RwLock<HashMap<Uuid, CacheEntry<GridSchemaResponse>>>,
    space_grids: RwLock<HashMap<Uuid, CacheEntry<Vec<PageGrid>>>>,
}

impl AppCache {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn get_user_status(&self, user_id: &Uuid) -> Option<(SystemRole, bool)> {
        let guard = self.user_status.read().ok()?;
        let entry = guard.get(user_id)?;
        if Instant::now() < entry.expires_at {
            Some(entry.value)
        } else {
            None
        }
    }

    pub fn set_user_status(&self, user_id: Uuid, status: (SystemRole, bool)) {
        if let Ok(mut guard) = self.user_status.write() {
            guard.insert(
                user_id,
                CacheEntry {
                    value: status,
                    expires_at: Instant::now() + CACHE_TTL,
                },
            );
        }
    }

    pub fn invalidate_user(&self, user_id: &Uuid) {
        if let Ok(mut guard) = self.user_status.write() {
            guard.remove(user_id);
        }
        if let Ok(mut guard) = self.space_access.write() {
            guard.retain(|(uid, _), _| uid != user_id);
        }
        if let Ok(mut guard) = self.grid_access.write() {
            guard.retain(|(uid, _), _| uid != user_id);
        }
    }

    pub fn get_space_access(&self, user_id: Uuid, space_id: Uuid) -> Option<SpaceRole> {
        let guard = self.space_access.read().ok()?;
        let entry = guard.get(&(user_id, space_id))?;
        if Instant::now() < entry.expires_at {
            Some(entry.value)
        } else {
            None
        }
    }

    pub fn set_space_access(&self, user_id: Uuid, space_id: Uuid, role: SpaceRole) {
        if let Ok(mut guard) = self.space_access.write() {
            guard.insert(
                (user_id, space_id),
                CacheEntry {
                    value: role,
                    expires_at: Instant::now() + CACHE_TTL,
                },
            );
        }
    }

    pub fn invalidate_space(&self, space_id: &Uuid) {
        if let Ok(mut guard) = self.space_access.write() {
            guard.retain(|(_, sid), _| sid != space_id);
        }
        if let Ok(mut guard) = self.grid_access.write() {
            guard.clear();
        }
        if let Ok(mut guard) = self.space_grids.write() {
            guard.remove(space_id);
        }
    }

    pub fn get_grid_space(&self, grid_id: &Uuid) -> Option<Uuid> {
        let guard = self.grid_space.read().ok()?;
        let entry = guard.get(grid_id)?;
        if Instant::now() < entry.expires_at {
            Some(entry.value)
        } else {
            None
        }
    }

    pub fn set_grid_space(&self, grid_id: Uuid, space_id: Uuid) {
        if let Ok(mut guard) = self.grid_space.write() {
            guard.insert(
                grid_id,
                CacheEntry {
                    value: space_id,
                    expires_at: Instant::now() + CACHE_TTL,
                },
            );
        }
    }

    pub fn get_grid_access(&self, user_id: Uuid, grid_id: Uuid) -> Option<SpaceRole> {
        let guard = self.grid_access.read().ok()?;
        let entry = guard.get(&(user_id, grid_id))?;
        if Instant::now() < entry.expires_at {
            Some(entry.value)
        } else {
            None
        }
    }

    pub fn set_grid_access(&self, user_id: Uuid, grid_id: Uuid, role: SpaceRole) {
        if let Ok(mut guard) = self.grid_access.write() {
            guard.insert(
                (user_id, grid_id),
                CacheEntry {
                    value: role,
                    expires_at: Instant::now() + CACHE_TTL,
                },
            );
        }
    }

    pub fn get_grid_schema(&self, grid_id: &Uuid) -> Option<GridSchemaResponse> {
        let guard = self.grid_schema.read().ok()?;
        let entry = guard.get(grid_id)?;
        if Instant::now() < entry.expires_at {
            Some(entry.value.clone())
        } else {
            None
        }
    }

    pub fn get_grid_columns(&self, grid_id: &Uuid) -> Option<Vec<GridColumn>> {
        self.get_grid_schema(grid_id).map(|s| s.columns)
    }

    pub fn set_grid_schema(&self, grid_id: Uuid, schema: GridSchemaResponse) {
        self.set_grid_space(grid_id, schema.grid.space_id);
        if let Ok(mut guard) = self.grid_schema.write() {
            guard.insert(
                grid_id,
                CacheEntry {
                    value: schema,
                    expires_at: Instant::now() + CACHE_TTL,
                },
            );
        }
    }

    pub fn invalidate_grid(&self, grid_id: &Uuid) {
        let maybe_sid = self.get_grid_space(grid_id);
        if let Ok(mut guard) = self.grid_schema.write() {
            guard.remove(grid_id);
        }
        if let Ok(mut guard) = self.grid_space.write() {
            guard.remove(grid_id);
        }
        if let Ok(mut guard) = self.grid_access.write() {
            guard.retain(|(_, gid), _| gid != grid_id);
        }
        if let Some(sid) = maybe_sid {
            if let Ok(mut guard) = self.space_grids.write() {
                guard.remove(&sid);
            }
        }
    }

    pub fn get_space_grids(&self, space_id: &Uuid) -> Option<Vec<PageGrid>> {
        let guard = self.space_grids.read().ok()?;
        let entry = guard.get(space_id)?;
        if Instant::now() < entry.expires_at {
            Some(entry.value.clone())
        } else {
            None
        }
    }

    pub fn set_space_grids(&self, space_id: Uuid, grids: Vec<PageGrid>) {
        if let Ok(mut guard) = self.space_grids.write() {
            guard.insert(
                space_id,
                CacheEntry {
                    value: grids,
                    expires_at: Instant::now() + CACHE_TTL,
                },
            );
        }
    }
}

#[derive(Clone)]
pub struct AppState {
    pub db: PgPool,
    pub jwt_config: Arc<JwtConfig>,
    pub uploads_dir: PathBuf,
    pub cache: Arc<AppCache>,
}

pub struct JwtConfig {
    pub encoding_key: EncodingKey,
    pub decoding_key: DecodingKey,
    pub expiration_hours: i64,
}

impl JwtConfig {
    pub fn from_secret(secret: &str, expiration_hours: i64) -> Self {
        Self {
            encoding_key: EncodingKey::from_secret(secret.as_bytes()),
            decoding_key: DecodingKey::from_secret(secret.as_bytes()),
            expiration_hours,
        }
    }
}

// ============================================================================
// 2. Argon2id Password Hashing Utilities
// ============================================================================

pub fn hash_password(plain_password: &str) -> Result<String, AuthError> {
    let salt = SaltString::generate(&mut OsRng);
    let argon2 = Argon2::default();
    argon2
        .hash_password(plain_password.as_bytes(), &salt)
        .map(|hash| hash.to_string())
        .map_err(|_| AuthError::InternalError("Failed to hash password".into()))
}

pub fn verify_password(plain_password: &str, password_hash: &str) -> Result<bool, AuthError> {
    let parsed_hash = PasswordHash::new(password_hash)
        .map_err(|_| AuthError::InternalError("Invalid stored password hash".into()))?;
    Ok(Argon2::default()
        .verify_password(plain_password.as_bytes(), &parsed_hash)
        .is_ok())
}

// ============================================================================
// 3. JWT Claims & Token Issuance
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Claims {
    /// Subject (`users.id`)
    pub sub: Uuid,
    pub email: String,
    pub role: SystemRole,
    pub iat: usize,
    pub exp: usize,
}

impl Claims {
    pub fn issue_token(
        user_id: Uuid,
        email: String,
        role: SystemRole,
        config: &JwtConfig,
    ) -> Result<String, AuthError> {
        let now = Utc::now();
        let exp = now + Duration::hours(config.expiration_hours);
        let claims = Self {
            sub: user_id,
            email,
            role,
            iat: now.timestamp() as usize,
            exp: exp.timestamp() as usize,
        };

        encode(&Header::default(), &claims, &config.encoding_key)
            .map_err(|_| AuthError::InternalError("Failed to sign JWT".into()))
    }
}

// ============================================================================
// 4. Structured API & Auth Errors
// ============================================================================

#[derive(Debug, Serialize)]
pub struct ErrorBody {
    pub error: String,
    pub code: &'static str,
}

#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    #[error("Missing or malformed Authorization Bearer token")]
    MissingToken,
    #[error("Invalid or expired JWT token")]
    InvalidToken,
    #[error("Invalid email or password")]
    InvalidCredentials,
    #[error("Account is deactivated or no longer exists")]
    AccountInactive,
    #[error("Insufficient permissions: requires {required}")]
    Forbidden { required: &'static str },
    #[error("Resource not found: {0}")]
    NotFound(String),
    #[error("Validation error: {0}")]
    BadRequest(String),
    #[error("Conflict error: {0}")]
    Conflict(String),
    #[error("Database or internal error: {0}")]
    InternalError(String),
}

impl From<sqlx::Error> for AuthError {
    fn from(err: sqlx::Error) -> Self {
        match err {
            sqlx::Error::RowNotFound => {
                AuthError::NotFound("Requested database record not found".into())
            }
            sqlx::Error::Database(db_err) if db_err.is_unique_violation() => {
                AuthError::Conflict(db_err.message().to_string())
            }
            other => AuthError::InternalError(other.to_string()),
        }
    }
}

impl IntoResponse for AuthError {
    fn into_response(self) -> Response {
        let (status, code) = match &self {
            AuthError::MissingToken => (StatusCode::UNAUTHORIZED, "AUTH_MISSING_TOKEN"),
            AuthError::InvalidToken => (StatusCode::UNAUTHORIZED, "AUTH_INVALID_TOKEN"),
            AuthError::InvalidCredentials => {
                (StatusCode::UNAUTHORIZED, "AUTH_INVALID_CREDENTIALS")
            }
            AuthError::AccountInactive => (StatusCode::FORBIDDEN, "AUTH_ACCOUNT_INACTIVE"),
            AuthError::Forbidden { .. } => (StatusCode::FORBIDDEN, "RBAC_FORBIDDEN"),
            AuthError::NotFound(_) => (StatusCode::NOT_FOUND, "RESOURCE_NOT_FOUND"),
            AuthError::BadRequest(_) => (StatusCode::BAD_REQUEST, "VALIDATION_ERROR"),
            AuthError::Conflict(_) => (StatusCode::CONFLICT, "RESOURCE_CONFLICT"),
            AuthError::InternalError(_) => (StatusCode::INTERNAL_SERVER_ERROR, "INTERNAL_ERR"),
        };

        let body = Json(ErrorBody {
            error: self.to_string(),
            code,
        });

        (status, body).into_response()
    }
}

// ============================================================================
// 5. Axum 0.8 Extractors for 3-Tier RBAC Hierarchy (Backed by In-Memory Cache)
// ============================================================================

/// Base authenticated principal extracted on any protected route.
/// Verifies both the JWT signature and live user active status / role via `AppCache` + PostgreSQL fallback.
#[derive(Debug, Clone)]
pub struct AuthUser {
    pub user_id: Uuid,
    pub email: String,
    pub system_role: SystemRole,
}

impl<S> FromRequestParts<S> for AuthUser
where
    AppState: FromRef<S>,
    S: Send + Sync,
{
    type Rejection = AuthError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let app_state = AppState::from_ref(state);

        let auth_header = parts
            .headers
            .get(AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .ok_or(AuthError::MissingToken)?;

        let token = auth_header
            .strip_prefix("Bearer ")
            .ok_or(AuthError::MissingToken)?;

        let token_data = decode::<Claims>(
            token,
            &app_state.jwt_config.decoding_key,
            &Validation::default(),
        )
        .map_err(|_| AuthError::InvalidToken)?;

        let uid = token_data.claims.sub;

        let status = if let Some(cached) = app_state.cache.get_user_status(&uid) {
            cached
        } else {
            let row: Option<(SystemRole, bool)> =
                sqlx::query_as("SELECT system_role, is_active FROM users WHERE id = $1")
                    .bind(uid)
                    .fetch_optional(&app_state.db)
                    .await
                    .map_err(|e| AuthError::InternalError(e.to_string()))?;

            match row {
                Some(st) => {
                    app_state.cache.set_user_status(uid, st);
                    st
                }
                None => return Err(AuthError::AccountInactive),
            }
        };

        match status {
            (live_role, true) => Ok(AuthUser {
                user_id: uid,
                email: token_data.claims.email,
                system_role: live_role,
            }),
            _ => Err(AuthError::AccountInactive),
        }
    }
}

/// Tier 1 Extractor: Enforces `SystemRole::SuperAdmin` (Global god-mode account).
#[derive(Debug, Clone)]
pub struct RequireSuperAdmin(pub AuthUser);

impl<S> FromRequestParts<S> for RequireSuperAdmin
where
    AppState: FromRef<S>,
    S: Send + Sync,
{
    type Rejection = AuthError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let user = AuthUser::from_request_parts(parts, state).await?;
        if user.system_role.is_super_admin() {
            Ok(RequireSuperAdmin(user))
        } else {
            Err(AuthError::Forbidden {
                required: "super_admin",
            })
        }
    }
}

/// Tier 2 Extractor: Enforces `SystemRole::Admin` or `SystemRole::SuperAdmin`.
#[derive(Debug, Clone)]
pub struct RequireAdmin(pub AuthUser);

impl<S> FromRequestParts<S> for RequireAdmin
where
    AppState: FromRef<S>,
    S: Send + Sync,
{
    type Rejection = AuthError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let user = AuthUser::from_request_parts(parts, state).await?;
        if user.system_role.is_admin_or_above() {
            Ok(RequireAdmin(user))
        } else {
            Err(AuthError::Forbidden {
                required: "admin or super_admin",
            })
        }
    }
}

/// Tier 3 / General Authenticated Extractor alias (`RequireSpaceAccess` for shared user endpoints).
#[derive(Debug, Clone)]
pub struct RequireSpaceAccess(pub AuthUser);

impl<S> FromRequestParts<S> for RequireSpaceAccess
where
    AppState: FromRef<S>,
    S: Send + Sync,
{
    type Rejection = AuthError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let user = AuthUser::from_request_parts(parts, state).await?;
        Ok(RequireSpaceAccess(user))
    }
}

/// Extractor for Space-Admin operations (SuperAdmin OR `SystemRole::Admin` / `SpaceRole::SpaceAdmin` on a Space).
#[derive(Debug, Clone)]
pub struct RequireSpaceAdmin(pub AuthUser);

impl<S> FromRequestParts<S> for RequireSpaceAdmin
where
    AppState: FromRef<S>,
    S: Send + Sync,
{
    type Rejection = AuthError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let user = AuthUser::from_request_parts(parts, state).await?;
        Ok(RequireSpaceAdmin(user))
    }
}

// ============================================================================
// 6. Cached + Database-Enforced Space & Grid Access Guard (`SpaceAccess`)
// ============================================================================

/// Resolved permission context for a specific Workspace (`Space`).
#[derive(Debug, Clone)]
pub struct SpaceAccess {
    pub user: AuthUser,
    pub space_id: Uuid,
    pub effective_space_role: SpaceRole,
}

impl SpaceAccess {
    /// Checks whether the caller can create/edit/delete rows in this Space.
    pub fn ensure_write(&self) -> Result<(), AuthError> {
        if self.effective_space_role.can_write() {
            Ok(())
        } else {
            Err(AuthError::Forbidden {
                required: "space editor or space_admin",
            })
        }
    }

    /// Checks whether the caller can alter columns, import datasets, or manage Space members.
    pub fn ensure_manage(&self) -> Result<(), AuthError> {
        if self.effective_space_role.can_manage_space() {
            Ok(())
        } else {
            Err(AuthError::Forbidden {
                required: "space_admin or super_admin",
            })
        }
    }
}

/// Resolves a user's effective `SpaceRole` for a given `space_id` using `AppCache` with PostgreSQL fallback.
pub async fn resolve_space_access_cached(
    state: &AppState,
    user: AuthUser,
    space_id: Uuid,
) -> Result<SpaceAccess, AuthError> {
    if user.system_role.is_super_admin() {
        return Ok(SpaceAccess {
            user,
            space_id,
            effective_space_role: SpaceRole::SpaceAdmin,
        });
    }

    if let Some(cached_role) = state.cache.get_space_access(user.user_id, space_id) {
        return Ok(SpaceAccess {
            user,
            space_id,
            effective_space_role: cached_role,
        });
    }

    let access = resolve_space_access(&state.db, user, space_id).await?;
    state.cache.set_space_access(
        access.user.user_id,
        space_id,
        access.effective_space_role,
    );
    Ok(access)
}

/// Resolves a user's effective `SpaceRole` for a given `space_id`.
pub async fn resolve_space_access(
    db: &PgPool,
    user: AuthUser,
    space_id: Uuid,
) -> Result<SpaceAccess, AuthError> {
    if user.system_role.is_super_admin() {
        return Ok(SpaceAccess {
            user,
            space_id,
            effective_space_role: SpaceRole::SpaceAdmin,
        });
    }

    let member_role: Option<(SpaceRole,)> = sqlx::query_as(
        r#"
        SELECT sm.space_role
        FROM space_members sm
        INNER JOIN spaces s ON s.id = sm.space_id
        WHERE sm.space_id = $1
          AND sm.user_id = $2
          AND s.is_archived = FALSE
        "#,
    )
    .bind(space_id)
    .bind(user.user_id)
    .fetch_optional(db)
    .await
    .map_err(|e| AuthError::InternalError(e.to_string()))?;

    match member_role {
        Some((role,)) => Ok(SpaceAccess {
            user,
            space_id,
            effective_space_role: role,
        }),
        None => Err(AuthError::Forbidden {
            required: "explicit space membership",
        }),
    }
}

/// Resolves a user's `SpaceAccess` via a `grid_id` lookup using `AppCache` with PostgreSQL fallback.
pub async fn resolve_grid_access_cached(
    state: &AppState,
    user: AuthUser,
    grid_id: Uuid,
) -> Result<SpaceAccess, AuthError> {
    if let (Some(space_id), Some(role)) = (
        state.cache.get_grid_space(&grid_id),
        if user.system_role.is_super_admin() {
            Some(SpaceRole::SpaceAdmin)
        } else {
            state.cache.get_grid_access(user.user_id, grid_id)
        },
    ) {
        return Ok(SpaceAccess {
            user,
            space_id,
            effective_space_role: role,
        });
    }

    let access = resolve_grid_access(&state.db, user, grid_id).await?;
    state.cache.set_grid_space(grid_id, access.space_id);
    state.cache.set_grid_access(
        access.user.user_id,
        grid_id,
        access.effective_space_role,
    );
    Ok(access)
}

/// Resolves a user's `SpaceAccess` via a `grid_id` lookup, verifying both grid existence
/// and space-level/grid-level permissions in a single query.
pub async fn resolve_grid_access(
    db: &PgPool,
    user: AuthUser,
    grid_id: Uuid,
) -> Result<SpaceAccess, AuthError> {
    let space_row: Option<(Uuid,)> = sqlx::query_as(
        "SELECT space_id FROM pages_grids WHERE id = $1 AND is_archived = FALSE",
    )
    .bind(grid_id)
    .fetch_optional(db)
    .await
    .map_err(|e| AuthError::InternalError(e.to_string()))?;

    let (space_id,) =
        space_row.ok_or_else(|| AuthError::NotFound(format!("Grid {grid_id} not found")))?;

    if user.system_role.is_super_admin() {
        return Ok(SpaceAccess {
            user,
            space_id,
            effective_space_role: SpaceRole::SpaceAdmin,
        });
    }

    let row: Option<(SpaceRole, Option<bool>)> = sqlx::query_as(
        r#"
        SELECT sm.space_role, pgp.can_edit
        FROM space_members sm
        INNER JOIN spaces s ON s.id = sm.space_id
        LEFT JOIN page_grid_permissions pgp ON pgp.grid_id = $1 AND pgp.user_id = $2
        WHERE sm.space_id = $3
          AND sm.user_id = $2
          AND s.is_archived = FALSE
        "#,
    )
    .bind(grid_id)
    .bind(user.user_id)
    .bind(space_id)
    .fetch_optional(db)
    .await
    .map_err(|e| AuthError::InternalError(e.to_string()))?;

    match row {
        Some((SpaceRole::SpaceAdmin, _)) => Ok(SpaceAccess {
            user,
            space_id,
            effective_space_role: SpaceRole::SpaceAdmin,
        }),
        Some((_, Some(true))) => Ok(SpaceAccess {
            user,
            space_id,
            effective_space_role: SpaceRole::Editor,
        }),
        Some((_, Some(false))) => Ok(SpaceAccess {
            user,
            space_id,
            effective_space_role: SpaceRole::Viewer,
        }),
        Some((role, None)) => Ok(SpaceAccess {
            user,
            space_id,
            effective_space_role: role,
        }),
        None => Err(AuthError::Forbidden {
            required: "explicit space or grid access",
        }),
    }
}

/// Asynchronously writes a structured audit log entry to `audit_logs` via `tokio::spawn`
/// so request latency is never blocked on non-critical audit logging.
pub async fn record_audit_log(
    db: &PgPool,
    actor_id: Uuid,
    space_id: Option<Uuid>,
    grid_id: Option<Uuid>,
    action: &str,
    metadata: serde_json::Value,
) -> Result<(), sqlx::Error> {
    let db_clone = db.clone();
    let action_owned = action.to_string();
    tokio::spawn(async move {
        let _ = sqlx::query(
            r#"
            INSERT INTO audit_logs (actor_id, space_id, grid_id, action, metadata)
            VALUES ($1, $2, $3, $4, $5)
            "#,
        )
        .bind(actor_id)
        .bind(space_id)
        .bind(grid_id)
        .bind(action_owned)
        .bind(metadata)
        .execute(&db_clone)
        .await;
    });
    Ok(())
}

/// Tower/Axum middleware function for routes scoped under `/api/spaces/{space_id}/...`.
pub async fn require_space_access_middleware(
    State(state): State<AppState>,
    Path(space_id): Path<Uuid>,
    mut req: Request,
    next: Next,
) -> Result<Response, AuthError> {
    let (mut parts, body) = req.into_parts();
    let user = AuthUser::from_request_parts(&mut parts, &state).await?;
    let access = resolve_space_access_cached(&state, user, space_id).await?;

    parts.extensions.insert(access);
    req = Request::from_parts(parts, body);
    Ok(next.run(req).await)
}
