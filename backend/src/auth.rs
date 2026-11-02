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
use pyra_ocean_shared::{SpaceRole, SystemRole};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use std::{path::PathBuf, sync::Arc};
use uuid::Uuid;

// ============================================================================
// 1. Application State & Auth Configuration
// ============================================================================

#[derive(Clone)]
pub struct AppState {
    pub db: PgPool,
    pub jwt_config: Arc<JwtConfig>,
    pub uploads_dir: PathBuf,
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
            sqlx::Error::RowNotFound => AuthError::NotFound("Requested database record not found".into()),
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
            AuthError::InvalidCredentials => (StatusCode::UNAUTHORIZED, "AUTH_INVALID_CREDENTIALS"),
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
// 5. Axum 0.8 Extractors for 3-Tier RBAC Hierarchy
// ============================================================================

/// Base authenticated principal extracted on any protected route.
/// Verifies both the JWT signature and live user active status / role in PostgreSQL.
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

        let row: Option<(SystemRole, bool)> = sqlx::query_as(
            "SELECT system_role, is_active FROM users WHERE id = $1",
        )
        .bind(token_data.claims.sub)
        .fetch_optional(&app_state.db)
        .await
        .map_err(|e| AuthError::InternalError(e.to_string()))?;

        match row {
            Some((live_role, true)) => Ok(AuthUser {
                user_id: token_data.claims.sub,
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
// 6. Database-Enforced Space & Grid Access Guard (`SpaceAccess`)
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

/// Resolves a user's effective `SpaceRole` for a given `space_id`.
/// - `SuperAdmin` automatically receives `SpaceRole::SpaceAdmin` across all Spaces.
/// - `Admin` and `User` must have an explicit membership row in `space_members`.
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

    let (space_id,) = space_row.ok_or_else(|| AuthError::NotFound(format!("Grid {grid_id} not found")))?;

    if user.system_role.is_super_admin() {
        return Ok(SpaceAccess {
            user,
            space_id,
            effective_space_role: SpaceRole::SpaceAdmin,
        });
    }

    // Check space_members and optional page_grid_permissions override
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
        Some(( _, Some(true))) => Ok(SpaceAccess {
            user,
            space_id,
            effective_space_role: SpaceRole::Editor,
        }),
        Some(( _, Some(false))) => Ok(SpaceAccess {
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

/// Writes a structured audit log entry to `audit_logs`.
pub async fn record_audit_log(
    db: &PgPool,
    actor_id: Uuid,
    space_id: Option<Uuid>,
    grid_id: Option<Uuid>,
    action: &str,
    metadata: serde_json::Value,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"
        INSERT INTO audit_logs (actor_id, space_id, grid_id, action, metadata)
        VALUES ($1, $2, $3, $4, $5)
        "#,
    )
    .bind(actor_id)
    .bind(space_id)
    .bind(grid_id)
    .bind(action)
    .bind(metadata)
    .execute(db)
    .await?;
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
    let access = resolve_space_access(&state.db, user, space_id).await?;

    parts.extensions.insert(access);
    req = Request::from_parts(parts, body);
    Ok(next.run(req).await)
}
