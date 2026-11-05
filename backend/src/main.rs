pub mod auth;
pub mod routes;
pub mod services;

use auth::{hash_password, AppCache, AppState, JwtConfig};
use axum::{
    extract::{DefaultBodyLimit, State},
    routing::{delete, get, patch, post},
    Json, Router,
};
use pyra_ocean_shared::SystemRole;
use serde_json::{json, Value};
use sqlx::{postgres::PgPoolOptions, PgPool};
use std::{net::SocketAddr, path::PathBuf, sync::Arc};
use tower_http::{
    cors::CorsLayer,
    services::{ServeDir, ServeFile},
    trace::TraceLayer,
};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    dotenvy::dotenv().ok();

    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "pyra_ocean_backend=debug,tower_http=info".into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    let database_url = std::env::var("DATABASE_URL")
        .expect("DATABASE_URL must be set in environment or .env");
    let jwt_secret = std::env::var("JWT_SECRET")
        .expect("JWT_SECRET must be set in environment or .env");

    let uploads_dir = PathBuf::from(
        std::env::var("STORAGE_DIR").unwrap_or_else(|_| "/data/pyraocean_uploads".into()),
    );
    tokio::fs::create_dir_all(&uploads_dir).await?;

    let db = PgPoolOptions::new()
        .max_connections(25)
        .connect(&database_url)
        .await?;

    // Run embedded SQL migrations automatically on startup
    sqlx::migrate!("../migrations").run(&db).await?;

    // Non-destructive startup migration: add `'password'` to PostgreSQL `column_property_type` enum
    sqlx::query("ALTER TYPE column_property_type ADD VALUE IF NOT EXISTS 'password'")
        .execute(&db)
        .await?;

    // Ensure `grid_columns.width_px` defaults to 180px for interactive column resizing
    sqlx::query("ALTER TABLE grid_columns ADD COLUMN IF NOT EXISTS width_px INT DEFAULT 180")
        .execute(&db)
        .await?;

    // Ensure `grid_rows.position` exists for drag-and-drop row reordering
    sqlx::query(
        "ALTER TABLE grid_rows ADD COLUMN IF NOT EXISTS position INT NOT NULL DEFAULT 0",
    )
    .execute(&db)
    .await?;

    // Ensure `grid_history_snapshots` table exists for 30-Day Grid Version History & Point-in-Time Restore
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS grid_history_snapshots (
            id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
            grid_id UUID NOT NULL REFERENCES pages_grids(id) ON DELETE CASCADE,
            action_type TEXT NOT NULL,
            actor_user_id UUID,
            actor_email TEXT,
            snapshot_data JSONB NOT NULL DEFAULT '{}'::jsonb,
            created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
        )
        "#,
    )
    .execute(&db)
    .await?;

    sqlx::query(
        "CREATE INDEX IF NOT EXISTS idx_grid_history_snapshots_grid_created ON grid_history_snapshots (grid_id, created_at DESC)",
    )
    .execute(&db)
    .await?;

    // Purge any snapshots older than 30 days on startup
    sqlx::query(
        "DELETE FROM grid_history_snapshots WHERE created_at < NOW() - INTERVAL '30 days'",
    )
    .execute(&db)
    .await?;

    // Non-destructive startup migration for Notion-Style Soft-Delete / Trash on `pages_grids`
    sqlx::query(
        "ALTER TABLE pages_grids ADD COLUMN IF NOT EXISTS deleted_at TIMESTAMPTZ DEFAULT NULL",
    )
    .execute(&db)
    .await?;

    sqlx::query(
        "ALTER TABLE pages_grids ADD COLUMN IF NOT EXISTS deleted_by UUID REFERENCES users(id) ON DELETE SET NULL",
    )
    .execute(&db)
    .await?;

    sqlx::query(
        "CREATE INDEX IF NOT EXISTS idx_pages_grids_deleted_at ON pages_grids (space_id, deleted_at)",
    )
    .execute(&db)
    .await?;

    // Auto-purge any soft-deleted grids older than 30 days on startup
    sqlx::query(
        "DELETE FROM pages_grids WHERE deleted_at IS NOT NULL AND deleted_at < NOW() - INTERVAL '30 days'",
    )
    .execute(&db)
    .await?;

    // Phase 16: 2FA/TOTP & Password Recovery columns on `users`
    sqlx::query("ALTER TABLE users ADD COLUMN IF NOT EXISTS totp_secret TEXT DEFAULT NULL")
        .execute(&db)
        .await?;
    sqlx::query(
        "ALTER TABLE users ADD COLUMN IF NOT EXISTS totp_enabled BOOLEAN NOT NULL DEFAULT FALSE",
    )
    .execute(&db)
    .await?;
    sqlx::query("ALTER TABLE users ADD COLUMN IF NOT EXISTS recovery_code_hash TEXT DEFAULT NULL")
        .execute(&db)
        .await?;
    sqlx::query(
        "ALTER TABLE users ADD COLUMN IF NOT EXISTS password_reset_requested_at TIMESTAMPTZ DEFAULT NULL",
    )
    .execute(&db)
    .await?;

    // Phase 16: Row Comments table (`grid_row_comments`)
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS grid_row_comments (
            id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
            grid_id UUID NOT NULL REFERENCES pages_grids(id) ON DELETE CASCADE,
            row_id UUID NOT NULL REFERENCES grid_rows(id) ON DELETE CASCADE,
            author_id UUID REFERENCES users(id) ON DELETE SET NULL,
            author_email TEXT NOT NULL,
            content TEXT NOT NULL,
            created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
        )
        "#,
    )
    .execute(&db)
    .await?;

    sqlx::query(
        "CREATE INDEX IF NOT EXISTS idx_row_comments_row_id ON grid_row_comments (row_id, created_at ASC)",
    )
    .execute(&db)
    .await?;

    sqlx::query(
        "CREATE INDEX IF NOT EXISTS idx_row_comments_grid_id ON grid_row_comments (grid_id)",
    )
    .execute(&db)
    .await?;

    // Google OAuth2 & 2FA Authentication Settings singleton table
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS system_auth_settings (
            id INT PRIMARY KEY DEFAULT 1,
            google_oauth_enabled BOOLEAN NOT NULL DEFAULT FALSE,
            google_client_id TEXT NOT NULL DEFAULT '',
            google_client_secret TEXT NOT NULL DEFAULT '',
            google_allowed_domains TEXT NOT NULL DEFAULT '',
            require_2fa_after_google BOOLEAN NOT NULL DEFAULT FALSE,
            updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
        )
        "#,
    )
    .execute(&db)
    .await?;

    sqlx::query("INSERT INTO system_auth_settings (id) VALUES (1) ON CONFLICT (id) DO NOTHING")
        .execute(&db)
        .await?;

    // Phase 18: Local AI Copilot (`PyraAI` / Ollama) Settings singleton table
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS system_ai_settings (
            id INT PRIMARY KEY DEFAULT 1,
            ai_enabled BOOLEAN NOT NULL DEFAULT TRUE,
            ollama_base_url TEXT NOT NULL DEFAULT 'http://10.10.10.4:11434',
            model_name TEXT NOT NULL DEFAULT 'qwen2.5:1.5b',
            system_prompt TEXT NOT NULL DEFAULT 'You are PyraAI, the intelligent workspace assistant and task executor for PyraOcean.',
            updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
        )
        "#,
    )
    .execute(&db)
    .await?;

    sqlx::query("INSERT INTO system_ai_settings (id) VALUES (1) ON CONFLICT (id) DO NOTHING")
        .execute(&db)
        .await?;

    // Seed singleton SuperAdmin account if none exists
    bootstrap_super_admin(&db).await?;

    let state = AppState {
        db,
        jwt_config: Arc::new(JwtConfig::from_secret(&jwt_secret, 24)),
        uploads_dir: uploads_dir.clone(),
        cache: Arc::new(AppCache::new()),
    };

    // Serve `/public` and `/assets` from compiled Trunk `/app/dist/public` in Docker,
    // automatically falling back to `frontend/public` during local development.
    let public_service =
        ServeDir::new("dist/public").fallback(ServeDir::new("frontend/public"));
    let assets_service =
        ServeDir::new("dist/public/assets").fallback(ServeDir::new("frontend/public/assets"));

    let app = Router::new()
        // Health & Branding
        .route("/api/health", get(health_check))
        .route("/api/v1/health", get(health_check))
        // Authentication, 2FA/TOTP & Password Recovery
        .route("/api/auth/login", post(routes::admin::login_handler))
        .route(
            "/api/auth/public-config",
            get(routes::admin::get_public_auth_config),
        )
        .route("/api/auth/google", post(routes::admin::google_login_handler))
        .route("/api/auth/me", get(routes::admin::current_user_profile))
        .route(
            "/api/auth/forgot-password/request",
            post(routes::admin::request_password_reset_handler),
        )
        .route(
            "/api/auth/forgot-password/reset",
            post(routes::admin::self_service_reset_password_handler),
        )
        .route(
            "/api/auth/password",
            post(routes::admin::change_own_password_handler),
        )
        .route("/api/auth/2fa/setup", post(routes::admin::setup_totp_handler))
        .route(
            "/api/auth/2fa/enable",
            post(routes::admin::enable_totp_handler),
        )
        .route(
            "/api/auth/2fa/disable",
            post(routes::admin::disable_totp_handler),
        )
        // Module 1: SuperAdmin, Backup/Restore, AI Settings & Space RBAC Management API
        .route(
            "/api/admin/auth-settings",
            get(routes::admin::get_auth_settings).put(routes::admin::update_auth_settings),
        )
        .route(
            "/api/admin/ai-settings",
            get(routes::ai::get_ai_settings).put(routes::ai::update_ai_settings),
        )
        .route("/api/ai/status", get(routes::ai::get_ai_status))
        .route("/api/ai/copilot", post(routes::ai::run_ai_copilot))
        .route(
            "/api/admin/users",
            get(routes::admin::list_users).post(routes::admin::create_user),
        )
        .route(
            "/api/admin/users/{id}",
            delete(routes::admin::delete_user),
        )
        .route(
            "/api/admin/users/{id}/role",
            patch(routes::admin::update_user_role),
        )
        .route(
            "/api/admin/users/{id}/password",
            post(routes::admin::admin_reset_user_password),
        )
        .route(
            "/api/admin/users/{id}/2fa",
            post(routes::admin::admin_manage_user_2fa),
        )
        .route(
            "/api/admin/backup",
            get(routes::admin::export_workspace_backup),
        )
        .route(
            "/api/admin/restore",
            post(routes::admin::restore_workspace_backup),
        )
        .route("/api/admin/audit-logs", get(routes::admin::list_audit_logs))
        .route(
            "/api/spaces",
            post(routes::spaces::create_space).get(routes::spaces::list_spaces),
        )
        .route(
            "/api/spaces/{space_id}",
            delete(routes::spaces::delete_space),
        )
        .route(
            "/api/spaces/{space_id}/members",
            get(routes::spaces::list_space_members).post(routes::spaces::grant_space_member),
        )
        .route(
            "/api/spaces/{space_id}/members/{user_id}",
            delete(routes::spaces::revoke_space_member),
        )
        // Module 2: Dynamic Grid Engine, Drag-and-Drop Reorder, Type Conversion & Bulk Delete
        .route(
            "/api/spaces/{space_id}/grids",
            get(routes::grids::list_space_grids).post(routes::grids::create_grid),
        )
        .route(
            "/api/spaces/{space_id}/grids/bulk",
            delete(routes::grids::bulk_delete_grids),
        )
        // Notion-Style Trash & Soft-Delete Lifecycle Endpoints
        .route("/api/trash/grids", get(routes::grids::list_trashed_grids))
        .route(
            "/api/trash/grids/{grid_id}/restore",
            post(routes::grids::restore_trashed_grid),
        )
        .route(
            "/api/trash/grids/{grid_id}/permanent",
            delete(routes::grids::permanently_delete_trashed_grid),
        )
        .route("/api/trash/empty", delete(routes::grids::empty_trash))
        .route(
            "/api/grids/{grid_id}",
            get(routes::grids::get_grid_schema)
                .patch(routes::grids::update_grid)
                .delete(routes::grids::delete_grid),
        )
        .route(
            "/api/grids/{grid_id}/duplicate",
            post(routes::grids::duplicate_grid),
        )
        .route(
            "/api/grids/{grid_id}/move",
            patch(routes::grids::move_grid),
        )
        .route(
            "/api/grids/{source_grid_id}/rows/copy-to/{target_grid_id}",
            post(routes::grids::copy_rows_to_grid),
        )
        .route(
            "/api/grids/{grid_id}/reset-layout",
            post(routes::grids::reset_grid_layout),
        )
        .route(
            "/api/grids/{grid_id}/export",
            get(services::exporter::export_grid_handler),
        )
        .route(
            "/api/grids/{grid_id}/history",
            get(routes::grids::list_grid_history),
        )
        .route(
            "/api/grids/{grid_id}/history/{history_id}",
            get(routes::grids::get_grid_history_detail),
        )
        .route(
            "/api/grids/{grid_id}/history/{history_id}/restore",
            post(routes::grids::restore_grid_history),
        )
        .route(
            "/api/grids/{grid_id}/columns",
            post(routes::grids::create_column),
        )
        .route(
            "/api/grids/{grid_id}/columns/reorder",
            patch(routes::grids::reorder_columns),
        )
        .route(
            "/api/grids/{grid_id}/columns/{col_id}",
            patch(routes::grids::update_column).delete(routes::grids::delete_column),
        )
        .route(
            "/api/grids/{grid_id}/rows",
            get(routes::grids::list_rows).post(routes::grids::create_row),
        )
        .route(
            "/api/grids/{grid_id}/rows/reorder",
            patch(routes::grids::reorder_rows),
        )
        .route(
            "/api/grids/{grid_id}/rows/restore",
            post(routes::grids::restore_rows),
        )
        .route(
            "/api/grids/{grid_id}/rows/bulk",
            delete(routes::grids::bulk_delete_rows),
        )
        .route(
            "/api/grids/{grid_id}/rows/bulk-cells",
            patch(routes::grids::bulk_update_row_cells),
        )
        .route(
            "/api/grids/{grid_id}/rows/{row_id}",
            patch(routes::grids::update_row),
        )
        .route(
            "/api/grids/{grid_id}/rows/{row_id}/upload",
            post(routes::grids::upload_cell_file),
        )
        // Row Comments Endpoints (`grid_row_comments`)
        .route(
            "/api/grids/{grid_id}/comments/counts",
            get(routes::grids::list_grid_comment_counts),
        )
        .route(
            "/api/grids/{grid_id}/rows/{row_id}/comments",
            get(routes::grids::list_row_comments).post(routes::grids::create_row_comment),
        )
        .route(
            "/api/grids/{grid_id}/rows/{row_id}/comments/{comment_id}",
            delete(routes::grids::delete_row_comment),
        )
        // Module 3: Notion, AppFlowy & PDF Data Import Engine
        .route(
            "/api/spaces/{space_id}/grids/import",
            post(services::importer::import_grid_handler),
        )
        .route(
            "/api/spaces/{space_id}/grids/import/preview",
            post(services::importer::preview_import_handler),
        )
        // Module 4: 3-Tier Scoped Search Engine (Current Grid, Space, All Spaces)
        .route("/api/search", get(routes::search::search_handler))
        // Static file serving for uploaded cell attachments, brand logos, and WASM SPA
        .nest_service("/uploads", ServeDir::new(uploads_dir))
        .nest_service("/public", public_service)
        .nest_service("/assets", assets_service)
        .fallback_service(
            ServeDir::new("dist").not_found_service(ServeFile::new("dist/index.html")),
        )
        // Allow up to 50 MB multipart uploads for Notion/AppFlowy exports & cell attachments
        .layer(DefaultBodyLimit::max(50 * 1024 * 1024))
        .layer(TraceLayer::new_for_http())
        .layer(CorsLayer::permissive())
        .with_state(state);

    let host = std::env::var("APP_HOST").unwrap_or_else(|_| "0.0.0.0".into());
    let port: u16 = std::env::var("APP_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(8080);
    let addr: SocketAddr = format!("{host}:{port}").parse()?;

    tracing::info!("PyraOcean (pyra_ocean_backend) listening on http://{addr}");
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}

async fn bootstrap_super_admin(db: &PgPool) -> Result<(), Box<dyn std::error::Error>> {
    let existing: Option<(uuid::Uuid,)> =
        sqlx::query_as("SELECT id FROM users WHERE system_role = 'super_admin' LIMIT 1")
            .fetch_optional(db)
            .await?;

    if existing.is_some() {
        tracing::info!("Global SuperAdmin account already exists; skipping bootstrap seed.");
        return Ok(());
    }

    let email = std::env::var("PYRAOCEAN_SUPERADMIN_EMAIL")
        .unwrap_or_else(|_| "superadmin@pyraocean.local".into())
        .trim()
        .to_lowercase();
    let password = std::env::var("PYRAOCEAN_SUPERADMIN_PASSWORD")
        .unwrap_or_else(|_| "PyraOcean#SuperAdmin2026!".into());

    let password_hash = hash_password(&password)
        .map_err(|e| format!("Failed to hash bootstrap SuperAdmin password: {e}"))?;

    let (seeded_id,): (uuid::Uuid,) = sqlx::query_as(
        r#"
        INSERT INTO users (email, display_name, password_hash, system_role, is_active)
        VALUES ($1, 'PyraOcean Super Admin', $2, $3, TRUE)
        RETURNING id
        "#,
    )
    .bind(&email)
    .bind(&password_hash)
    .bind(SystemRole::SuperAdmin)
    .fetch_one(db)
    .await?;

    tracing::info!(
        "Seeded singleton SuperAdmin account (id = {seeded_id}, email = {email})"
    );

    Ok(())
}

async fn health_check(State(state): State<AppState>) -> Json<Value> {
    let db_ok = sqlx::query("SELECT 1").execute(&state.db).await.is_ok();
    Json(json!({
        "status": if db_ok { "healthy" } else { "degraded" },
        "service": "pyra_ocean_backend",
        "branding": {
            "app_name": "PyraOcean",
            "logo_dark": "/public/logo-dark.png",
            "logo_light": "/public/logo-light.png",
            "powered_by": "Powered by Pyramakerz",
            "powered_by_logo": "/public/pyramakerz.webp"
        },
        "phase": 14
    }))
}
