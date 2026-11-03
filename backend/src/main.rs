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
        // Authentication
        .route("/api/auth/login", post(routes::admin::login_handler))
        .route("/api/auth/me", get(routes::admin::current_user_profile))
        // Module 1: SuperAdmin & Space RBAC Management API
        .route(
            "/api/admin/users",
            get(routes::admin::list_users).post(routes::admin::create_user),
        )
        .route(
            "/api/admin/users/{id}/role",
            patch(routes::admin::update_user_role),
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
        .route(
            "/api/grids/{grid_id}",
            get(routes::grids::get_grid_schema)
                .patch(routes::grids::update_grid)
                .delete(routes::grids::delete_grid),
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
            "logo_dark": "/public/logo-dark.jpg",
            "logo_light": "/public/logo-light.jpg",
            "powered_by": "Powered by Pyramakerz",
            "powered_by_logo": "/public/pyramakerz.webp"
        },
        "phase": 12
    }))
}
