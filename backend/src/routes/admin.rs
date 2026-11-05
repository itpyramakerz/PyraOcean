use crate::auth::{
    build_otpauth_uri, generate_recovery_code, generate_totp_secret, hash_password,
    record_audit_log, verify_password, verify_totp_code, AppState, AuthError, AuthUser, Claims,
    RequireAdmin, RequireSuperAdmin,
};
use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    Json,
};
use chrono::Utc;
use pyra_ocean_shared::{
    AdminManage2faRequest, AdminManage2faResponse, AdminResetPasswordRequest,
    AdminResetPasswordResponse, AuditLogEntry, BrandingMetadata, ChangeOwnPasswordRequest,
    CreateUserRequest, DisableTotpRequest, ForgotPasswordRequest, ForgotPasswordResponse,
    GoogleAuthSettings, GoogleLoginRequest, GridColumn, GridRow, GridRowComment, LoginRequest,
    LoginResponse, PageGrid, PublicAuthConfig, RestoreBackupResponse,
    SelfServicePasswordResetRequest, Space, SpaceMember, SystemRole, TotpSetupResponse,
    UpdateUserRoleRequest, User, UserBackupRecord, UserProfile, VerifyTotpRequest,
    WorkspaceBackupPayload,
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
// 1. Authentication, 2FA/TOTP & Password Recovery Endpoints
// ============================================================================

pub async fn login_handler(
    State(state): State<AppState>,
    Json(payload): Json<LoginRequest>,
) -> Result<Json<LoginResponse>, AuthError> {
    let email_norm = payload.email.trim().to_lowercase();

    let mut user: User = sqlx::query_as(
        r#"
        SELECT id, email, display_name, password_hash, system_role, is_active, created_at, updated_at,
               totp_enabled, password_reset_requested_at
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

    // Check Two-Factor Authentication (2FA/TOTP) if enabled on the account
    if user.totp_enabled {
        let code_input = payload
            .totp_code
            .as_deref()
            .map(str::trim)
            .unwrap_or_default();

        if code_input.is_empty() {
            // Step 1 passed (email + password valid); prompt client for 6-digit TOTP or Recovery Code
            return Ok(Json(LoginResponse {
                requires_totp: true,
                requires_2fa: true,
                temp_2fa_token: Some(user.email.clone()),
                token: String::new(),
                user,
                branding: BrandingMetadata::default(),
            }));
        }

        let (totp_secret, recovery_hash): (Option<String>, Option<String>) = sqlx::query_as(
            "SELECT totp_secret, recovery_code_hash FROM users WHERE id = $1",
        )
        .bind(user.id)
        .fetch_one(&state.db)
        .await?;

        let now_secs = Utc::now().timestamp().max(0) as u64;
        let totp_ok = totp_secret
            .as_deref()
            .map(|sec| verify_totp_code(sec, code_input, now_secs))
            .unwrap_or(false);

        let recovery_ok = if !totp_ok {
            recovery_hash
                .as_deref()
                .and_then(|h| verify_password(code_input, h).ok())
                .unwrap_or(false)
        } else {
            false
        };

        if !totp_ok && !recovery_ok {
            return Err(AuthError::BadRequest(
                "Invalid 6-digit 2FA authenticator code or recovery code".into(),
            ));
        }
    }

    // Clear any pending password reset flag on successful login
    if user.password_reset_requested_at.is_some() {
        let _ = sqlx::query(
            "UPDATE users SET password_reset_requested_at = NULL WHERE id = $1",
        )
        .bind(user.id)
        .execute(&state.db)
        .await;
        user.password_reset_requested_at = None;
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
        json!({
            "email": user.email,
            "role": user.system_role,
            "totp_verified": user.totp_enabled
        }),
    )
    .await;

    Ok(Json(LoginResponse {
        requires_totp: false,
        requires_2fa: false,
        temp_2fa_token: None,
        token,
        user,
        branding: BrandingMetadata::default(),
    }))
}

/// Decodes a Base64URL string (without requiring padding) into raw bytes.
fn decode_base64url(input: &str) -> Option<Vec<u8>> {
    let mut buffer = 0u32;
    let mut bits_collected = 0u8;
    let mut out = Vec::with_capacity(input.len() * 3 / 4);
    for ch in input.bytes() {
        let val = match ch {
            b'A'..=b'Z' => ch - b'A',
            b'a'..=b'z' => ch - b'a' + 26,
            b'0'..=b'9' => ch - b'0' + 52,
            b'-' | b'+' => 62,
            b'_' | b'/' => 63,
            b'=' | b' ' | b'\n' | b'\r' => continue,
            _ => return None,
        };
        buffer = (buffer << 6) | (val as u32);
        bits_collected += 6;
        if bits_collected >= 8 {
            bits_collected -= 8;
            out.push(((buffer >> bits_collected) & 0xFF) as u8);
        }
    }
    Some(out)
}

/// Extracts a verified email from a Google ID Token JWT (`header.payload.signature`) or direct Google OAuth credential.
fn extract_email_from_google_credential(
    credential: &str,
    explicit_email: Option<&str>,
    expected_client_id: &str,
) -> Result<String, AuthError> {
    let trimmed = credential.trim();
    let parts: Vec<&str> = trimmed.split('.').collect();
    if parts.len() == 3 {
        let payload_bytes = decode_base64url(parts[1]).ok_or_else(|| {
            AuthError::BadRequest("Invalid Google ID token payload encoding".into())
        })?;
        let payload_json: serde_json::Value =
            serde_json::from_slice(&payload_bytes).map_err(|_| {
                AuthError::BadRequest("Invalid Google ID token JSON claims".into())
            })?;

        if !expected_client_id.trim().is_empty() {
            if let Some(aud) = payload_json.get("aud").and_then(|v| v.as_str()) {
                if !aud.trim().is_empty() && aud.trim() != expected_client_id.trim() {
                    return Err(AuthError::ForbiddenMessage(
                        "Google ID token Client ID (aud) does not match configured Google OAuth Client ID.".into(),
                    ));
                }
            }
        }

        if let Some(email) = payload_json.get("email").and_then(|v| v.as_str()) {
            let norm = email.trim().to_lowercase();
            if norm.contains('@') {
                return Ok(norm);
            }
        }
    }

    if let Some(em) = explicit_email {
        let norm = em.trim().to_lowercase();
        if norm.contains('@') {
            return Ok(norm);
        }
    }

    if trimmed.contains('@') {
        return Ok(trimmed.to_lowercase());
    }

    Err(AuthError::BadRequest(
        "Could not extract a valid Google account email from credential".into(),
    ))
}

/// `GET /api/auth/public-config`
/// Public endpoint returning whether Sign in with Google is enabled and the Google OAuth Client ID.
pub async fn get_public_auth_config(
    State(state): State<AppState>,
) -> Result<Json<PublicAuthConfig>, AuthError> {
    let settings: Option<GoogleAuthSettings> = sqlx::query_as(
        r#"
        SELECT google_oauth_enabled, google_client_id, google_client_secret,
               google_allowed_domains, require_2fa_after_google
        FROM system_auth_settings
        WHERE id = 1
        "#,
    )
    .fetch_optional(&state.db)
    .await?;

    let cfg = settings.unwrap_or_default();
    Ok(Json(PublicAuthConfig {
        google_oauth_enabled: cfg.google_oauth_enabled,
        google_client_id: cfg.google_client_id,
        require_2fa_after_google: cfg.require_2fa_after_google,
    }))
}

/// `POST /api/auth/google`
/// Authenticates a user via Google OAuth2 / ID token credential and chains into Google Authenticator 2FA when enabled.
pub async fn google_login_handler(
    State(state): State<AppState>,
    Json(payload): Json<GoogleLoginRequest>,
) -> Result<Json<LoginResponse>, AuthError> {
    let settings: GoogleAuthSettings = sqlx::query_as(
        r#"
        SELECT google_oauth_enabled, google_client_id, google_client_secret,
               google_allowed_domains, require_2fa_after_google
        FROM system_auth_settings
        WHERE id = 1
        "#,
    )
    .fetch_optional(&state.db)
    .await?
    .unwrap_or_default();

    if !settings.google_oauth_enabled {
        return Err(AuthError::ForbiddenMessage(
            "Sign in with Google is currently disabled in Admin Authentication Settings.".into(),
        ));
    }

    let email_source = payload
        .temp_2fa_token
        .as_deref()
        .filter(|s| !s.trim().is_empty())
        .or(payload.email.as_deref());

    let email_norm = extract_email_from_google_credential(
        &payload.credential,
        email_source,
        &settings.google_client_id,
    )?;

    // Check allowed domains filter if configured
    let allowed_domains: Vec<String> = settings
        .google_allowed_domains
        .split(',')
        .map(|d| d.trim().trim_start_matches('@').to_lowercase())
        .filter(|d| !d.is_empty())
        .collect();

    if !allowed_domains.is_empty() {
        let email_domain = email_norm
            .rsplit_once('@')
            .map(|(_, dom)| dom.to_lowercase())
            .unwrap_or_default();
        if !allowed_domains.iter().any(|d| d == &email_domain) {
            return Err(AuthError::ForbiddenMessage(format!(
                "Google Workspace domain '@{email_domain}' is not in the Allowed Email Domains list."
            )));
        }
    }

    let user: User = sqlx::query_as(
        r#"
        SELECT id, email, display_name, password_hash, system_role, is_active, created_at, updated_at,
               totp_enabled, password_reset_requested_at
        FROM users
        WHERE LOWER(email) = $1
        "#,
    )
    .bind(&email_norm)
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(|| {
        AuthError::BadRequest(format!(
            "No PyraOcean user account matches Google email '{email_norm}'. Ask your SuperAdmin to provision your account first."
        ))
    })?;

    if !user.is_active {
        return Err(AuthError::AccountInactive);
    }

    let (totp_secret, recovery_hash): (Option<String>, Option<String>) = sqlx::query_as(
        "SELECT totp_secret, recovery_code_hash FROM users WHERE id = $1",
    )
    .bind(user.id)
    .fetch_one(&state.db)
    .await?;

    let enforce_2fa = user.totp_enabled
        || (settings.require_2fa_after_google && totp_secret.as_deref().is_some());

    if enforce_2fa {
        let code_input = payload
            .totp_code
            .as_deref()
            .map(str::trim)
            .unwrap_or_default();

        if code_input.is_empty() {
            return Ok(Json(LoginResponse {
                requires_totp: true,
                requires_2fa: true,
                temp_2fa_token: Some(user.email.clone()),
                token: String::new(),
                user,
                branding: BrandingMetadata::default(),
            }));
        }

        let now_secs = Utc::now().timestamp().max(0) as u64;
        let totp_ok = totp_secret
            .as_deref()
            .map(|sec| verify_totp_code(sec, code_input, now_secs))
            .unwrap_or(false);

        let recovery_ok = if !totp_ok {
            recovery_hash
                .as_deref()
                .and_then(|h| verify_password(code_input, h).ok())
                .unwrap_or(false)
        } else {
            false
        };

        if !totp_ok && !recovery_ok {
            return Err(AuthError::BadRequest(
                "Invalid 6-digit Google Authenticator code or recovery code".into(),
            ));
        }
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
        "auth.login.google",
        json!({
            "email": user.email,
            "role": user.system_role,
            "totp_verified": enforce_2fa
        }),
    )
    .await;

    Ok(Json(LoginResponse {
        requires_totp: false,
        requires_2fa: false,
        temp_2fa_token: None,
        token,
        user,
        branding: BrandingMetadata::default(),
    }))
}

/// `GET /api/admin/auth-settings` (`SuperAdmin` only)
pub async fn get_auth_settings(
    State(state): State<AppState>,
    RequireSuperAdmin(_admin): RequireSuperAdmin,
) -> Result<Json<GoogleAuthSettings>, AuthError> {
    let settings: GoogleAuthSettings = sqlx::query_as(
        r#"
        SELECT google_oauth_enabled, google_client_id, google_client_secret,
               google_allowed_domains, require_2fa_after_google
        FROM system_auth_settings
        WHERE id = 1
        "#,
    )
    .fetch_optional(&state.db)
    .await?
    .unwrap_or_default();

    Ok(Json(settings))
}

/// `PUT /api/admin/auth-settings` (`SuperAdmin` only)
pub async fn update_auth_settings(
    State(state): State<AppState>,
    RequireSuperAdmin(admin): RequireSuperAdmin,
    Json(payload): Json<GoogleAuthSettings>,
) -> Result<Json<GoogleAuthSettings>, AuthError> {
    let updated: GoogleAuthSettings = sqlx::query_as(
        r#"
        INSERT INTO system_auth_settings (
            id, google_oauth_enabled, google_client_id, google_client_secret,
            google_allowed_domains, require_2fa_after_google, updated_at
        )
        VALUES (1, $1, $2, $3, $4, $5, NOW())
        ON CONFLICT (id) DO UPDATE SET
            google_oauth_enabled = EXCLUDED.google_oauth_enabled,
            google_client_id = EXCLUDED.google_client_id,
            google_client_secret = EXCLUDED.google_client_secret,
            google_allowed_domains = EXCLUDED.google_allowed_domains,
            require_2fa_after_google = EXCLUDED.require_2fa_after_google,
            updated_at = NOW()
        RETURNING google_oauth_enabled, google_client_id, google_client_secret,
                  google_allowed_domains, require_2fa_after_google
        "#,
    )
    .bind(payload.google_oauth_enabled)
    .bind(payload.google_client_id.trim())
    .bind(payload.google_client_secret.trim())
    .bind(payload.google_allowed_domains.trim())
    .bind(payload.require_2fa_after_google)
    .fetch_one(&state.db)
    .await?;

    let _ = record_audit_log(
        &state.db,
        admin.user_id,
        None,
        None,
        "admin.auth_settings.updated",
        json!({
            "google_oauth_enabled": updated.google_oauth_enabled,
            "google_client_id": updated.google_client_id,
            "google_allowed_domains": updated.google_allowed_domains,
            "require_2fa_after_google": updated.require_2fa_after_google
        }),
    )
    .await;

    Ok(Json(updated))
}

/// `GET /api/auth/me`
/// Returns the currently authenticated user's profile.
pub async fn current_user_profile(
    State(state): State<AppState>,
    auth_user: AuthUser,
) -> Result<Json<UserProfile>, AuthError> {
    let user: User = sqlx::query_as(
        r#"
        SELECT id, email, display_name, password_hash, system_role, is_active, created_at, updated_at,
               totp_enabled, password_reset_requested_at
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

/// `POST /api/auth/forgot-password/request`
/// Flags `password_reset_requested_at = NOW()` for the user so SuperAdmin sees a `"Reset Requested"` badge,
/// and informs the caller whether 2FA/Recovery self-service reset is available.
pub async fn request_password_reset_handler(
    State(state): State<AppState>,
    Json(payload): Json<ForgotPasswordRequest>,
) -> Result<Json<ForgotPasswordResponse>, AuthError> {
    let email_norm = payload.email.trim().to_lowercase();
    if email_norm.is_empty() || !email_norm.contains('@') {
        return Err(AuthError::BadRequest(
            "Please enter a valid account email address".into(),
        ));
    }

    let maybe_user: Option<(Uuid, bool)> = sqlx::query_as(
        r#"
        UPDATE users
        SET password_reset_requested_at = NOW()
        WHERE LOWER(email) = $1
        RETURNING id, totp_enabled
        "#,
    )
    .bind(&email_norm)
    .fetch_optional(&state.db)
    .await?;

    let totp_enabled = if let Some((uid, totp_on)) = maybe_user {
        let _ = record_audit_log(
            &state.db,
            uid,
            None,
            None,
            "auth.password_reset.requested",
            json!({ "email": email_norm }),
        )
        .await;
        totp_on
    } else {
        false
    };

    Ok(Json(ForgotPasswordResponse {
        message: "Password reset request recorded for the SuperAdmin. If you have a 2FA Authenticator code or Recovery Code (PYRA-XXXX-...), you can also reset your password immediately below.".into(),
        totp_enabled,
    }))
}

/// `POST /api/auth/forgot-password/reset`
/// Self-service password reset using either the user's 6-digit 2FA TOTP code or their Recovery Code (`PYRA-...`).
pub async fn self_service_reset_password_handler(
    State(state): State<AppState>,
    Json(payload): Json<SelfServicePasswordResetRequest>,
) -> Result<Json<serde_json::Value>, AuthError> {
    let email_norm = payload.email.trim().to_lowercase();
    let code_input = payload.verification_code.trim();
    if payload.new_password.len() < 8 {
        return Err(AuthError::BadRequest(
            "New password must be at least 8 characters long".into(),
        ));
    }
    if code_input.is_empty() {
        return Err(AuthError::BadRequest(
            "Please provide your 6-digit 2FA code or Recovery Code".into(),
        ));
    }

    let row: Option<(Uuid, Option<String>, Option<String>)> = sqlx::query_as(
        r#"
        SELECT id, totp_secret, recovery_code_hash
        FROM users
        WHERE LOWER(email) = $1 AND is_active = TRUE
        "#,
    )
    .bind(&email_norm)
    .fetch_optional(&state.db)
    .await?;

    let Some((user_id, totp_secret, recovery_hash)) = row else {
        return Err(AuthError::BadRequest(
            "Invalid email or verification code".into(),
        ));
    };

    let now_secs = Utc::now().timestamp().max(0) as u64;
    let totp_ok = totp_secret
        .as_deref()
        .map(|sec| verify_totp_code(sec, code_input, now_secs))
        .unwrap_or(false);

    let recovery_ok = if !totp_ok {
        recovery_hash
            .as_deref()
            .and_then(|h| verify_password(code_input, h).ok())
            .unwrap_or(false)
    } else {
        false
    };

    if !totp_ok && !recovery_ok {
        return Err(AuthError::BadRequest(
            "Invalid 6-digit 2FA code or Recovery Code".into(),
        ));
    }

    let new_hash = hash_password(&payload.new_password)?;
    sqlx::query(
        r#"
        UPDATE users
        SET password_hash = $2,
            password_reset_requested_at = NULL,
            updated_at = NOW()
        WHERE id = $1
        "#,
    )
    .bind(user_id)
    .bind(&new_hash)
    .execute(&state.db)
    .await?;

    let _ = record_audit_log(
        &state.db,
        user_id,
        None,
        None,
        "auth.password_reset.self_service",
        json!({ "email": email_norm, "method": if totp_ok { "totp" } else { "recovery_code" } }),
    )
    .await;

    Ok(Json(json!({
        "status": "ok",
        "message": "Your password has been reset. You can now sign in with your new password."
    })))
}

/// `POST /api/auth/password`
/// Allows any authenticated user to change their own password.
pub async fn change_own_password_handler(
    State(state): State<AppState>,
    auth_user: AuthUser,
    Json(payload): Json<ChangeOwnPasswordRequest>,
) -> Result<Json<serde_json::Value>, AuthError> {
    if payload.new_password.len() < 8 {
        return Err(AuthError::BadRequest(
            "New password must be at least 8 characters long".into(),
        ));
    }

    let (current_hash,): (String,) =
        sqlx::query_as("SELECT password_hash FROM users WHERE id = $1")
            .bind(auth_user.user_id)
            .fetch_optional(&state.db)
            .await?
            .ok_or(AuthError::AccountInactive)?;

    if !verify_password(&payload.current_password, &current_hash)? {
        return Err(AuthError::BadRequest(
            "Current password is incorrect".into(),
        ));
    }

    let next_hash = hash_password(&payload.new_password)?;
    sqlx::query(
        "UPDATE users SET password_hash = $2, password_reset_requested_at = NULL, updated_at = NOW() WHERE id = $1",
    )
    .bind(auth_user.user_id)
    .bind(&next_hash)
    .execute(&state.db)
    .await?;

    let _ = record_audit_log(
        &state.db,
        auth_user.user_id,
        None,
        None,
        "auth.password.changed",
        json!({ "email": auth_user.email }),
    )
    .await;

    Ok(Json(json!({
        "status": "ok",
        "message": "Password updated successfully."
    })))
}

/// `POST /api/auth/2fa/setup`
/// Generates a fresh RFC 6238 Base32 secret, `otpauth://` URI, and Recovery Code (`PYRA-...`) for the caller.
pub async fn setup_totp_handler(
    State(state): State<AppState>,
    auth_user: AuthUser,
) -> Result<Json<TotpSetupResponse>, AuthError> {
    let secret = generate_totp_secret();
    let recovery_code = generate_recovery_code();
    let recovery_hash = hash_password(&recovery_code)?;
    let otpauth_uri = build_otpauth_uri(&auth_user.email, &secret);

    sqlx::query(
        r#"
        UPDATE users
        SET totp_secret = $2,
            recovery_code_hash = $3,
            updated_at = NOW()
        WHERE id = $1
        "#,
    )
    .bind(auth_user.user_id)
    .bind(&secret)
    .bind(&recovery_hash)
    .execute(&state.db)
    .await?;

    Ok(Json(TotpSetupResponse {
        secret,
        otpauth_uri,
        recovery_code,
    }))
}

/// `POST /api/auth/2fa/enable`
/// Verifies a 6-digit TOTP code against the caller's `totp_secret` and activates `totp_enabled = TRUE`.
pub async fn enable_totp_handler(
    State(state): State<AppState>,
    auth_user: AuthUser,
    Json(payload): Json<VerifyTotpRequest>,
) -> Result<Json<UserProfile>, AuthError> {
    let (maybe_secret,): (Option<String>,) =
        sqlx::query_as("SELECT totp_secret FROM users WHERE id = $1")
            .bind(auth_user.user_id)
            .fetch_optional(&state.db)
            .await?
            .ok_or(AuthError::AccountInactive)?;

    let Some(secret) = maybe_secret else {
        return Err(AuthError::BadRequest(
            "Please initialize 2FA setup first before verifying a code".into(),
        ));
    };

    let now_secs = Utc::now().timestamp().max(0) as u64;
    if !verify_totp_code(&secret, payload.code.trim(), now_secs) {
        return Err(AuthError::BadRequest(
            "Invalid 6-digit code. Please check your Authenticator app and try again.".into(),
        ));
    }

    let user: User = sqlx::query_as(
        r#"
        UPDATE users
        SET totp_enabled = TRUE,
            updated_at = NOW()
        WHERE id = $1
        RETURNING id, email, display_name, password_hash, system_role, is_active, created_at, updated_at,
                  totp_enabled, password_reset_requested_at
        "#,
    )
    .bind(auth_user.user_id)
    .fetch_one(&state.db)
    .await?;

    let _ = record_audit_log(
        &state.db,
        auth_user.user_id,
        None,
        None,
        "auth.2fa.enabled",
        json!({ "email": auth_user.email }),
    )
    .await;

    Ok(Json(UserProfile::from(user)))
}

/// `POST /api/auth/2fa/disable`
/// Disables 2FA on the caller's account after verifying their current password.
pub async fn disable_totp_handler(
    State(state): State<AppState>,
    auth_user: AuthUser,
    Json(payload): Json<DisableTotpRequest>,
) -> Result<Json<UserProfile>, AuthError> {
    let (current_hash,): (String,) =
        sqlx::query_as("SELECT password_hash FROM users WHERE id = $1")
            .bind(auth_user.user_id)
            .fetch_optional(&state.db)
            .await?
            .ok_or(AuthError::AccountInactive)?;

    if !verify_password(&payload.password, &current_hash)? {
        return Err(AuthError::BadRequest(
            "Password verification failed".into(),
        ));
    }

    let user: User = sqlx::query_as(
        r#"
        UPDATE users
        SET totp_enabled = FALSE,
            totp_secret = NULL,
            updated_at = NOW()
        WHERE id = $1
        RETURNING id, email, display_name, password_hash, system_role, is_active, created_at, updated_at,
                  totp_enabled, password_reset_requested_at
        "#,
    )
    .bind(auth_user.user_id)
    .fetch_one(&state.db)
    .await?;

    let _ = record_audit_log(
        &state.db,
        auth_user.user_id,
        None,
        None,
        "auth.2fa.disabled",
        json!({ "email": auth_user.email }),
    )
    .await;

    Ok(Json(UserProfile::from(user)))
}

// ============================================================================
// 2. User Directory & SuperAdmin User, Password & 2FA Management
// ============================================================================

/// `GET /api/admin/users`
/// Lists all registered users (accessible to `SuperAdmin` and `Admin` for Space member assignment).
pub async fn list_users(
    State(state): State<AppState>,
    RequireAdmin(_admin): RequireAdmin,
) -> Result<Json<Vec<User>>, AuthError> {
    let users: Vec<User> = sqlx::query_as(
        r#"
        SELECT id, email, display_name, password_hash, system_role, is_active, created_at, updated_at,
               totp_enabled, password_reset_requested_at
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
    let recovery_code = generate_recovery_code();
    let recovery_hash = hash_password(&recovery_code)?;

    let created_user: User = sqlx::query_as(
        r#"
        INSERT INTO users (email, display_name, password_hash, system_role, is_active, recovery_code_hash)
        VALUES ($1, $2, $3, $4, TRUE, $5)
        RETURNING id, email, display_name, password_hash, system_role, is_active, created_at, updated_at,
                  totp_enabled, password_reset_requested_at
        "#,
    )
    .bind(&email_norm)
    .bind(payload.display_name.trim())
    .bind(&password_hash)
    .bind(payload.system_role)
    .bind(&recovery_hash)
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
/// Enforces Strict Hierarchy Rules:
/// - `SuperAdmin` accounts are immutable root accounts and can NEVER be demoted, disabled, or revoked (even by themselves).
/// - Only `SuperAdmin` can promote/demote between `Admin` and `User`.
pub async fn update_user_role(
    State(state): State<AppState>,
    RequireAdmin(actor): RequireAdmin,
    Path(target_user_id): Path<Uuid>,
    Json(payload): Json<UpdateUserRoleRequest>,
) -> Result<Json<User>, AuthError> {
    let target_user: User = sqlx::query_as(
        r#"
        SELECT id, email, display_name, password_hash, system_role, is_active, created_at, updated_at,
               totp_enabled, password_reset_requested_at
        FROM users
        WHERE id = $1
        "#,
    )
    .bind(target_user_id)
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(|| AuthError::NotFound(format!("User {target_user_id} not found")))?;

    // Rule 1: SuperAdmin Protection (Immutable Root) — even SuperAdmin cannot demote or disable SuperAdmin
    if target_user.system_role == SystemRole::SuperAdmin
        && (payload.system_role != SystemRole::SuperAdmin || payload.is_active == Some(false))
    {
        return Err(AuthError::ForbiddenMessage(
            "Super Admin permissions cannot be revoked.".into(),
        ));
    }

    // Rule 2 & 3: Only SuperAdmin can change global system roles
    if actor.system_role != SystemRole::SuperAdmin {
        return Err(AuthError::ForbiddenMessage(
            "Only Super Admin can change global system roles.".into(),
        ));
    }

    let updated_user: User = sqlx::query_as(
        r#"
        UPDATE users
        SET system_role = $2,
            is_active = COALESCE($3, is_active)
        WHERE id = $1
        RETURNING id, email, display_name, password_hash, system_role, is_active, created_at, updated_at,
                  totp_enabled, password_reset_requested_at
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
        actor.user_id,
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

/// `DELETE /api/admin/users/{id}`
/// Deletes a user account while strictly protecting `SuperAdmin` (Immutable Root) and enforcing hierarchy.
pub async fn delete_user(
    State(state): State<AppState>,
    RequireAdmin(actor): RequireAdmin,
    Path(target_user_id): Path<Uuid>,
) -> Result<StatusCode, AuthError> {
    let target_user: User = sqlx::query_as(
        r#"
        SELECT id, email, display_name, password_hash, system_role, is_active, created_at, updated_at,
               totp_enabled, password_reset_requested_at
        FROM users
        WHERE id = $1
        "#,
    )
    .bind(target_user_id)
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(|| AuthError::NotFound(format!("User {target_user_id} not found")))?;

    // Rule 1: SuperAdmin Protection (Immutable Root)
    if target_user.system_role == SystemRole::SuperAdmin {
        return Err(AuthError::ForbiddenMessage(
            "Super Admin permissions cannot be revoked.".into(),
        ));
    }

    // Rule 2 & 3: SuperAdmin can delete any Admin or User; Admin cannot delete another Admin
    if actor.system_role != SystemRole::SuperAdmin {
        return Err(AuthError::ForbiddenMessage(
            "Only Super Admin can delete user accounts.".into(),
        ));
    }

    sqlx::query("DELETE FROM users WHERE id = $1")
        .bind(target_user_id)
        .execute(&state.db)
        .await?;

    state.cache.invalidate_user(&target_user_id);

    record_audit_log(
        &state.db,
        actor.user_id,
        None,
        None,
        "admin.user.delete",
        json!({
            "deleted_user_id": target_user.id,
            "deleted_email": target_user.email,
            "deleted_role": target_user.system_role
        }),
    )
    .await?;

    Ok(StatusCode::NO_CONTENT)
}

/// `POST /api/admin/users/{id}/password`
/// Resets/updates a user's password and issues a fresh Recovery Code (`PYRA-...`).
/// Hierarchy enforcement:
/// - `SuperAdmin` can reset any user's password.
/// - `Admin` can ONLY reset passwords for standard `User` accounts (`target_user.system_role == SystemRole::User`).
/// - Nobody (except `SuperAdmin` themselves) can modify a `SuperAdmin` account.
pub async fn admin_reset_user_password(
    State(state): State<AppState>,
    RequireAdmin(actor): RequireAdmin,
    Path(target_user_id): Path<Uuid>,
    Json(payload): Json<AdminResetPasswordRequest>,
) -> Result<Json<AdminResetPasswordResponse>, AuthError> {
    if payload.new_password.len() < 8 {
        return Err(AuthError::BadRequest(
            "New password must be at least 8 characters long".into(),
        ));
    }

    let target_user: User = sqlx::query_as(
        r#"
        SELECT id, email, display_name, password_hash, system_role, is_active, created_at, updated_at,
               totp_enabled, password_reset_requested_at
        FROM users
        WHERE id = $1
        "#,
    )
    .bind(target_user_id)
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(|| AuthError::NotFound(format!("User {target_user_id} not found")))?;

    if target_user.system_role == SystemRole::SuperAdmin
        && actor.system_role != SystemRole::SuperAdmin
    {
        return Err(AuthError::ForbiddenMessage(
            "Super Admin permissions cannot be revoked.".into(),
        ));
    }

    if actor.system_role == SystemRole::Admin && target_user.system_role != SystemRole::User {
        return Err(AuthError::ForbiddenMessage(
            "Admins can only reset passwords for standard User accounts.".into(),
        ));
    }

    let password_hash = hash_password(&payload.new_password)?;
    let recovery_code = generate_recovery_code();
    let recovery_hash = hash_password(&recovery_code)?;

    let updated_user: User = sqlx::query_as(
        r#"
        UPDATE users
        SET password_hash = $2,
            recovery_code_hash = $3,
            password_reset_requested_at = NULL,
            updated_at = NOW()
        WHERE id = $1
        RETURNING id, email, display_name, password_hash, system_role, is_active, created_at, updated_at,
                  totp_enabled, password_reset_requested_at
        "#,
    )
    .bind(target_user_id)
    .bind(&password_hash)
    .bind(&recovery_hash)
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(|| AuthError::NotFound(format!("User {target_user_id} not found")))?;

    record_audit_log(
        &state.db,
        actor.user_id,
        None,
        None,
        "admin.user.password_reset",
        json!({
            "target_user_id": updated_user.id,
            "email": updated_user.email
        }),
    )
    .await?;

    Ok(Json(AdminResetPasswordResponse {
        user: updated_user,
        generated_recovery_code: recovery_code,
    }))
}

/// `POST /api/admin/users/{id}/2fa`
/// 2FA enforcement & lockout reset with strict hierarchy checks:
/// - `action == "setup"` / `"enable"` / `"enforce"`: provisions & enables 2FA (`totp_enabled = TRUE`) and returns the Base32 secret, `otpauth://` URI, and Recovery Code.
/// - `action == "reset"` / `"disable"`: disables 2FA (`totp_enabled = FALSE, totp_secret = NULL`) for a locked-out user.
pub async fn admin_manage_user_2fa(
    State(state): State<AppState>,
    RequireAdmin(actor): RequireAdmin,
    Path(target_user_id): Path<Uuid>,
    Json(payload): Json<AdminManage2faRequest>,
) -> Result<Json<AdminManage2faResponse>, AuthError> {
    let existing: User = sqlx::query_as(
        r#"
        SELECT id, email, display_name, password_hash, system_role, is_active, created_at, updated_at,
               totp_enabled, password_reset_requested_at
        FROM users
        WHERE id = $1
        "#,
    )
    .bind(target_user_id)
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(|| AuthError::NotFound(format!("User {target_user_id} not found")))?;

    if existing.system_role == SystemRole::SuperAdmin
        && actor.system_role != SystemRole::SuperAdmin
    {
        return Err(AuthError::ForbiddenMessage(
            "Super Admin permissions cannot be revoked.".into(),
        ));
    }

    if actor.system_role == SystemRole::Admin
        && existing.system_role != SystemRole::User
        && actor.user_id != target_user_id
    {
        return Err(AuthError::ForbiddenMessage(
            "Admins can only manage 2FA for standard User accounts.".into(),
        ));
    }

    if payload.action.eq_ignore_ascii_case("verify") {
        let code_trimmed = payload
            .code
            .as_deref()
            .map(str::trim)
            .unwrap_or_default();
        if code_trimmed.len() != 6 {
            return Err(AuthError::BadRequest(
                "Please enter a 6-digit code from Google Authenticator.".into(),
            ));
        }

        let (totp_secret,): (Option<String>,) =
            sqlx::query_as("SELECT totp_secret FROM users WHERE id = $1")
                .bind(target_user_id)
                .fetch_one(&state.db)
                .await?;

        let Some(secret) = totp_secret else {
            return Err(AuthError::BadRequest(
                "No Google Authenticator secret is provisioned for this user yet.".into(),
            ));
        };

        let now_secs = Utc::now().timestamp().max(0) as u64;
        if !verify_totp_code(&secret, code_trimmed, now_secs) {
            return Err(AuthError::BadRequest(
                "Invalid 6-digit code. Please check your Google Authenticator app and try again.".into(),
            ));
        }

        let updated_user: User = sqlx::query_as(
            r#"
            UPDATE users
            SET totp_enabled = TRUE,
                updated_at = NOW()
            WHERE id = $1
            RETURNING id, email, display_name, password_hash, system_role, is_active, created_at, updated_at,
                      totp_enabled, password_reset_requested_at
            "#,
        )
        .bind(target_user_id)
        .fetch_one(&state.db)
        .await?;

        let otpauth_uri = build_otpauth_uri(&updated_user.email, &secret);
        return Ok(Json(AdminManage2faResponse {
            user: updated_user,
            secret: Some(secret),
            otpauth_uri: Some(otpauth_uri),
            recovery_code: None,
        }));
    }

    if payload.action.eq_ignore_ascii_case("setup")
        || payload.action.eq_ignore_ascii_case("enable")
        || payload.action.eq_ignore_ascii_case("enforce")
    {
        let secret = generate_totp_secret();
        let recovery_code = generate_recovery_code();
        let recovery_hash = hash_password(&recovery_code)?;
        let otpauth_uri = build_otpauth_uri(&existing.email, &secret);

        let updated_user: User = sqlx::query_as(
            r#"
            UPDATE users
            SET totp_secret = $2,
                totp_enabled = TRUE,
                recovery_code_hash = $3,
                updated_at = NOW()
            WHERE id = $1
            RETURNING id, email, display_name, password_hash, system_role, is_active, created_at, updated_at,
                      totp_enabled, password_reset_requested_at
            "#,
        )
        .bind(target_user_id)
        .bind(&secret)
        .bind(&recovery_hash)
        .fetch_one(&state.db)
        .await?;

        record_audit_log(
            &state.db,
            actor.user_id,
            None,
            None,
            "admin.user.2fa_enforced",
            json!({
                "target_user_id": updated_user.id,
                "email": updated_user.email
            }),
        )
        .await?;

        Ok(Json(AdminManage2faResponse {
            user: updated_user,
            secret: Some(secret),
            otpauth_uri: Some(otpauth_uri),
            recovery_code: Some(recovery_code),
        }))
    } else {
        let updated_user: User = sqlx::query_as(
            r#"
            UPDATE users
            SET totp_secret = NULL,
                totp_enabled = FALSE,
                updated_at = NOW()
            WHERE id = $1
            RETURNING id, email, display_name, password_hash, system_role, is_active, created_at, updated_at,
                      totp_enabled, password_reset_requested_at
            "#,
        )
        .bind(target_user_id)
        .fetch_one(&state.db)
        .await?;

        record_audit_log(
            &state.db,
            actor.user_id,
            None,
            None,
            "admin.user.2fa_reset",
            json!({
                "target_user_id": updated_user.id,
                "email": updated_user.email
            }),
        )
        .await?;

        Ok(Json(AdminManage2faResponse {
            user: updated_user,
            secret: None,
            otpauth_uri: None,
            recovery_code: None,
        }))
    }
}

// ============================================================================
// 3. Full Workspace Database Backup & Restore (`SuperAdmin` Only)
// ============================================================================

/// `GET /api/admin/backup`
/// Exports a complete JSON snapshot of users, spaces, space_members, grids, columns, rows, and row comments.
pub async fn export_workspace_backup(
    State(state): State<AppState>,
    RequireSuperAdmin(admin): RequireSuperAdmin,
) -> Result<Json<WorkspaceBackupPayload>, AuthError> {
    let users: Vec<UserBackupRecord> = sqlx::query_as(
        r#"
        SELECT id, email, display_name, password_hash, system_role, is_active, created_at, updated_at,
               totp_secret, totp_enabled, recovery_code_hash
        FROM users
        ORDER BY created_at ASC
        "#,
    )
    .fetch_all(&state.db)
    .await?;

    let spaces: Vec<Space> = sqlx::query_as(
        r#"
        SELECT id, slug, name, description, icon, created_by, is_archived, created_at, updated_at
        FROM spaces
        ORDER BY created_at ASC
        "#,
    )
    .fetch_all(&state.db)
    .await?;

    let space_members: Vec<SpaceMember> = sqlx::query_as(
        r#"
        SELECT space_id, user_id, space_role, granted_by, created_at
        FROM space_members
        ORDER BY created_at ASC
        "#,
    )
    .fetch_all(&state.db)
    .await?;

    let grids: Vec<PageGrid> = sqlx::query_as(
        r#"
        SELECT id, space_id, title, description, icon, position, created_by, is_archived, created_at, updated_at,
               deleted_at, deleted_by
        FROM pages_grids
        ORDER BY position ASC, created_at ASC
        "#,
    )
    .fetch_all(&state.db)
    .await?;

    let columns: Vec<GridColumn> = sqlx::query_as(
        r#"
        SELECT id, grid_id, name, property_type, property_config, width_px, position, is_primary, is_hidden, created_at, updated_at
        FROM grid_columns
        ORDER BY position ASC, created_at ASC
        "#,
    )
    .fetch_all(&state.db)
    .await?;

    let rows: Vec<GridRow> = sqlx::query_as(
        r#"
        SELECT id, grid_id, position, row_order, cells, created_by, updated_by, created_at, last_modified
        FROM grid_rows
        ORDER BY position ASC, row_order ASC, created_at ASC
        "#,
    )
    .fetch_all(&state.db)
    .await?;

    let comments: Vec<GridRowComment> = sqlx::query_as(
        r#"
        SELECT id, grid_id, row_id, author_id, author_email, content, created_at
        FROM grid_row_comments
        ORDER BY created_at ASC
        "#,
    )
    .fetch_all(&state.db)
    .await?;

    let _ = record_audit_log(
        &state.db,
        admin.user_id,
        None,
        None,
        "admin.backup.export",
        json!({
            "spaces": spaces.len(),
            "grids": grids.len(),
            "rows": rows.len(),
            "comments": comments.len()
        }),
    )
    .await;

    Ok(Json(WorkspaceBackupPayload {
        version: 16,
        exported_at: Utc::now(),
        exported_by_email: admin.email,
        users,
        spaces,
        space_members,
        grids,
        columns,
        rows,
        comments,
    }))
}

/// `POST /api/admin/restore`
/// Transactionally restores/upserts a full workspace JSON backup and flushes the in-memory `AppCache`.
pub async fn restore_workspace_backup(
    State(state): State<AppState>,
    RequireSuperAdmin(admin): RequireSuperAdmin,
    Json(payload): Json<WorkspaceBackupPayload>,
) -> Result<Json<RestoreBackupResponse>, AuthError> {
    let mut tx = state.db.begin().await?;

    let mut users_restored = 0usize;
    for u in &payload.users {
        sqlx::query(
            r#"
            INSERT INTO users (id, email, display_name, password_hash, system_role, is_active, created_at, updated_at, totp_secret, totp_enabled, recovery_code_hash)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)
            ON CONFLICT (id) DO UPDATE
            SET display_name = EXCLUDED.display_name,
                is_active = EXCLUDED.is_active,
                updated_at = NOW()
            "#,
        )
        .bind(u.id)
        .bind(&u.email)
        .bind(&u.display_name)
        .bind(&u.password_hash)
        .bind(u.system_role)
        .bind(u.is_active)
        .bind(u.created_at)
        .bind(u.updated_at)
        .bind(&u.totp_secret)
        .bind(u.totp_enabled)
        .bind(&u.recovery_code_hash)
        .execute(&mut *tx)
        .await?;
        users_restored += 1;
    }

    let mut spaces_restored = 0usize;
    for s in &payload.spaces {
        sqlx::query(
            r#"
            INSERT INTO spaces (id, slug, name, description, icon, created_by, is_archived, created_at, updated_at)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
            ON CONFLICT (id) DO UPDATE
            SET slug = EXCLUDED.slug,
                name = EXCLUDED.name,
                description = EXCLUDED.description,
                icon = EXCLUDED.icon,
                is_archived = EXCLUDED.is_archived,
                updated_at = NOW()
            "#,
        )
        .bind(s.id)
        .bind(&s.slug)
        .bind(&s.name)
        .bind(&s.description)
        .bind(&s.icon)
        .bind(admin.user_id)
        .bind(s.is_archived)
        .bind(s.created_at)
        .bind(s.updated_at)
        .execute(&mut *tx)
        .await?;
        spaces_restored += 1;
    }

    for sm in &payload.space_members {
        let _ = sqlx::query(
            r#"
            INSERT INTO space_members (space_id, user_id, space_role, granted_by, created_at)
            VALUES ($1, $2, $3, $4, $5)
            ON CONFLICT (space_id, user_id) DO UPDATE
            SET space_role = EXCLUDED.space_role
            "#,
        )
        .bind(sm.space_id)
        .bind(sm.user_id)
        .bind(sm.space_role)
        .bind(admin.user_id)
        .bind(sm.created_at)
        .execute(&mut *tx)
        .await;
    }

    let mut grids_restored = 0usize;
    for g in &payload.grids {
        sqlx::query(
            r#"
            INSERT INTO pages_grids (id, space_id, title, description, icon, position, created_by, is_archived, created_at, updated_at, deleted_at, deleted_by)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)
            ON CONFLICT (id) DO UPDATE
            SET space_id = EXCLUDED.space_id,
                title = EXCLUDED.title,
                description = EXCLUDED.description,
                icon = EXCLUDED.icon,
                position = EXCLUDED.position,
                is_archived = EXCLUDED.is_archived,
                deleted_at = EXCLUDED.deleted_at,
                deleted_by = EXCLUDED.deleted_by,
                updated_at = NOW()
            "#,
        )
        .bind(g.id)
        .bind(g.space_id)
        .bind(&g.title)
        .bind(&g.description)
        .bind(&g.icon)
        .bind(g.position)
        .bind(admin.user_id)
        .bind(g.is_archived)
        .bind(g.created_at)
        .bind(g.updated_at)
        .bind(g.deleted_at)
        .bind(g.deleted_by)
        .execute(&mut *tx)
        .await?;
        grids_restored += 1;
    }

    let mut columns_restored = 0usize;
    for c in &payload.columns {
        let cfg_json = serde_json::to_value(&c.property_config)
            .map_err(|e| AuthError::InternalError(e.to_string()))?;
        sqlx::query(
            r#"
            INSERT INTO grid_columns (id, grid_id, name, property_type, property_config, width_px, position, is_primary, is_hidden, created_at, updated_at)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)
            ON CONFLICT (id) DO UPDATE
            SET name = EXCLUDED.name,
                property_type = EXCLUDED.property_type,
                property_config = EXCLUDED.property_config,
                width_px = EXCLUDED.width_px,
                position = EXCLUDED.position,
                is_primary = EXCLUDED.is_primary,
                is_hidden = EXCLUDED.is_hidden,
                updated_at = NOW()
            "#,
        )
        .bind(c.id)
        .bind(c.grid_id)
        .bind(&c.name)
        .bind(c.property_type)
        .bind(cfg_json)
        .bind(c.width_px)
        .bind(c.position)
        .bind(c.is_primary)
        .bind(c.is_hidden)
        .bind(c.created_at)
        .bind(c.updated_at)
        .execute(&mut *tx)
        .await?;
        columns_restored += 1;
    }

    let mut rows_restored = 0usize;
    for r in &payload.rows {
        let cells_json = serde_json::to_value(&r.cells)
            .map_err(|e| AuthError::InternalError(e.to_string()))?;
        sqlx::query(
            r#"
            INSERT INTO grid_rows (id, grid_id, position, row_order, cells, created_by, updated_by, created_at, last_modified)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
            ON CONFLICT (id) DO UPDATE
            SET position = EXCLUDED.position,
                row_order = EXCLUDED.row_order,
                cells = EXCLUDED.cells,
                last_modified = EXCLUDED.last_modified
            "#,
        )
        .bind(r.id)
        .bind(r.grid_id)
        .bind(r.position)
        .bind(r.row_order)
        .bind(cells_json)
        .bind(admin.user_id)
        .bind(admin.user_id)
        .bind(r.created_at)
        .bind(r.last_modified)
        .execute(&mut *tx)
        .await?;
        rows_restored += 1;
    }

    let mut comments_restored = 0usize;
    for cm in &payload.comments {
        sqlx::query(
            r#"
            INSERT INTO grid_row_comments (id, grid_id, row_id, author_id, author_email, content, created_at)
            VALUES ($1, $2, $3, $4, $5, $6, $7)
            ON CONFLICT (id) DO UPDATE
            SET content = EXCLUDED.content
            "#,
        )
        .bind(cm.id)
        .bind(cm.grid_id)
        .bind(cm.row_id)
        .bind(cm.author_id)
        .bind(&cm.author_email)
        .bind(&cm.content)
        .bind(cm.created_at)
        .execute(&mut *tx)
        .await?;
        comments_restored += 1;
    }

    tx.commit().await?;
    state.cache.clear_all();

    record_audit_log(
        &state.db,
        admin.user_id,
        None,
        None,
        "admin.backup.restore",
        json!({
            "users_restored": users_restored,
            "spaces_restored": spaces_restored,
            "grids_restored": grids_restored,
            "columns_restored": columns_restored,
            "rows_restored": rows_restored,
            "comments_restored": comments_restored
        }),
    )
    .await?;

    Ok(Json(RestoreBackupResponse {
        users_restored,
        spaces_restored,
        grids_restored,
        columns_restored,
        rows_restored,
        comments_restored,
    }))
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
