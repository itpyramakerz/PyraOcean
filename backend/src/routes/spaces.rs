use crate::auth::{
    record_audit_log, resolve_space_access_cached, AppState, AuthError, RequireSpaceAccess,
    RequireSpaceAdmin, RequireSuperAdmin,
};
use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use pyra_ocean_shared::{
    CreateSpaceRequest, GrantSpaceMemberRequest, Space, SpaceMember, SpaceMemberDetail, SpaceRole,
    SpaceWithRole, SystemRole,
};
use serde_json::json;
use uuid::Uuid;

// ============================================================================
// 1. Space Lifecycle Endpoints (`SuperAdmin` & `Admin`)
// ============================================================================

/// `POST /api/spaces`
/// Creates a new top-level Workspace (Space) and assigns the creator (and any requested `Admin` managers) as `SpaceAdmin`.
pub async fn create_space(
    State(state): State<AppState>,
    RequireSpaceAccess(actor): RequireSpaceAccess,
    Json(payload): Json<CreateSpaceRequest>,
) -> Result<(StatusCode, Json<Space>), AuthError> {
    let is_system_admin = matches!(
        actor.system_role,
        SystemRole::SuperAdmin | SystemRole::Admin
    );
    if !is_system_admin {
        let has_space_admin: Option<(i32,)> = sqlx::query_as(
            "SELECT 1 FROM space_members WHERE user_id = $1 AND space_role = 'space_admin' LIMIT 1",
        )
        .bind(actor.user_id)
        .fetch_optional(&state.db)
        .await?;
        if has_space_admin.is_none() {
            return Err(AuthError::Forbidden {
                required: "admin or super_admin",
            });
        }
    }

    let slug = payload
        .slug
        .trim()
        .to_lowercase()
        .replace(' ', "-");

    if slug.is_empty() || payload.name.trim().is_empty() {
        return Err(AuthError::BadRequest(
            "Space name and slug cannot be empty".into(),
        ));
    }

    let mut tx = state.db.begin().await?;

    let space: Space = sqlx::query_as(
        r#"
        INSERT INTO spaces (slug, name, description, icon, created_by, is_archived)
        VALUES ($1, $2, $3, $4, $5, FALSE)
        RETURNING id, slug, name, description, icon, created_by, is_archived, created_at, updated_at
        "#,
    )
    .bind(&slug)
    .bind(payload.name.trim())
    .bind(&payload.description)
    .bind(&payload.icon)
    .bind(actor.user_id)
    .fetch_one(&mut *tx)
    .await?;

    // Automatically insert the creator into `space_members` as `space_admin` so they immediately have full Admin access
    sqlx::query(
        r#"
        INSERT INTO space_members (space_id, user_id, space_role, granted_by)
        VALUES ($1, $2, 'space_admin', $2)
        ON CONFLICT (space_id, user_id)
        DO UPDATE SET space_role = 'space_admin', granted_by = EXCLUDED.granted_by
        "#,
    )
    .bind(space.id)
    .bind(actor.user_id)
    .execute(&mut *tx)
    .await?;

    // Also ensure all global SuperAdmins are enrolled as `space_admin` on the new Space
    sqlx::query(
        r#"
        INSERT INTO space_members (space_id, user_id, space_role, granted_by)
        SELECT $1, id, 'space_admin', $2 FROM users WHERE system_role = 'super_admin'
        ON CONFLICT (space_id, user_id)
        DO UPDATE SET space_role = 'space_admin'
        "#,
    )
    .bind(space.id)
    .bind(actor.user_id)
    .execute(&mut *tx)
    .await?;

    for manager_id in &payload.admin_manager_ids {
        if actor.system_role == SystemRole::SuperAdmin {
            sqlx::query(
                r#"
                UPDATE users
                SET system_role = 'admin'
                WHERE id = $1 AND system_role = 'user'
                "#,
            )
            .bind(manager_id)
            .execute(&mut *tx)
            .await?;
        }

        sqlx::query(
            r#"
            INSERT INTO space_members (space_id, user_id, space_role, granted_by)
            VALUES ($1, $2, 'space_admin', $3)
            ON CONFLICT (space_id, user_id)
            DO UPDATE SET space_role = 'space_admin', granted_by = EXCLUDED.granted_by
            "#,
        )
        .bind(space.id)
        .bind(manager_id)
        .bind(actor.user_id)
        .execute(&mut *tx)
        .await?;
    }

    tx.commit().await?;

    state.cache.invalidate_space(&space.id);

    record_audit_log(
        &state.db,
        actor.user_id,
        Some(space.id),
        None,
        "space.create",
        json!({
            "slug": space.slug,
            "name": space.name,
            "admin_manager_ids": payload.admin_manager_ids
        }),
    )
    .await?;

    Ok((StatusCode::CREATED, Json(space)))
}

/// `DELETE /api/spaces/{space_id}`
/// Hard-deletes a Space and cascade-deletes all of its Pages/Grids, Columns, and Rows.
pub async fn delete_space(
    State(state): State<AppState>,
    RequireSuperAdmin(super_admin): RequireSuperAdmin,
    Path(space_id): Path<Uuid>,
) -> Result<StatusCode, AuthError> {
    let result = sqlx::query("DELETE FROM spaces WHERE id = $1")
        .bind(space_id)
        .execute(&state.db)
        .await?;

    if result.rows_affected() == 0 {
        return Err(AuthError::NotFound(format!("Space {space_id} not found")));
    }

    state.cache.invalidate_space(&space_id);

    record_audit_log(
        &state.db,
        super_admin.user_id,
        None,
        None,
        "space.hard_delete",
        json!({ "deleted_space_id": space_id }),
    )
    .await?;

    Ok(StatusCode::NO_CONTENT)
}

// ============================================================================
// 2. Space Admin Membership Endpoints (`RequireSpaceAdmin`)
// ============================================================================

/// `GET /api/spaces/{space_id}/members`
/// Lists all members of a Space with their email, display name, and space role.
pub async fn list_space_members(
    State(state): State<AppState>,
    RequireSpaceAccess(actor): RequireSpaceAccess,
    Path(space_id): Path<Uuid>,
) -> Result<Json<Vec<SpaceMemberDetail>>, AuthError> {
    let _access = resolve_space_access_cached(&state, actor, space_id).await?;

    let members: Vec<SpaceMemberDetail> = sqlx::query_as(
        r#"
        SELECT
            sm.space_id,
            sm.user_id,
            u.email,
            u.display_name,
            u.system_role AS global_role,
            sm.space_role,
            sm.granted_by,
            sm.created_at
        FROM space_members sm
        INNER JOIN users u ON u.id = sm.user_id
        WHERE sm.space_id = $1
        ORDER BY sm.created_at ASC
        "#,
    )
    .bind(space_id)
    .fetch_all(&state.db)
    .await?;

    Ok(Json(members))
}

/// `POST /api/spaces/{space_id}/members`
/// Grants a `User` access to a specific Space with granular permissions (`can_view`, `can_edit`).
/// Enforces Strict Hierarchy Rules:
/// - `SuperAdmin` permissions cannot be downgraded or revoked (`StatusCode::FORBIDDEN`).
/// - `Admin` (System `Admin` or Space `Admin`) can ONLY modify or revoke Space permissions for standard `User` accounts (`target_user.system_role == SystemRole::User` and `target_space_role != SpaceRole::SpaceAdmin`), and cannot grant/modify `SpaceAdmin` or modify another `Admin`.
pub async fn grant_space_member(
    State(state): State<AppState>,
    RequireSpaceAdmin(actor): RequireSpaceAdmin,
    Path(space_id): Path<Uuid>,
    Json(payload): Json<GrantSpaceMemberRequest>,
) -> Result<(StatusCode, Json<SpaceMember>), AuthError> {
    let access = resolve_space_access_cached(&state, actor.clone(), space_id).await?;
    access.ensure_manage()?;

    if !payload.can_view && !payload.can_edit && !payload.is_space_admin {
        return Err(AuthError::BadRequest(
            "At least can_view must be true when granting space membership".into(),
        ));
    }

    let target_role = if payload.is_space_admin {
        SpaceRole::SpaceAdmin
    } else if payload.can_edit {
        SpaceRole::Editor
    } else {
        SpaceRole::Viewer
    };

    let (target_global_role,): (SystemRole,) = sqlx::query_as(
        "SELECT system_role FROM users WHERE id = $1",
    )
    .bind(payload.user_id)
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(|| AuthError::NotFound(format!("User {} not found", payload.user_id)))?;

    let existing_space_role: Option<(SpaceRole,)> = sqlx::query_as(
        "SELECT space_role FROM space_members WHERE space_id = $1 AND user_id = $2",
    )
    .bind(space_id)
    .bind(payload.user_id)
    .fetch_optional(&state.db)
    .await?;

    // Rule 1: SuperAdmin Protection (Immutable Root)
    if target_global_role == SystemRole::SuperAdmin && target_role != SpaceRole::SpaceAdmin {
        return Err(AuthError::ForbiddenMessage(
            "Super Admin permissions cannot be revoked.".into(),
        ));
    }

    // Rule 3: Admin Authority — an Admin can ONLY modify standard User accounts (and not another Admin or SpaceAdmin)
    if actor.system_role != SystemRole::SuperAdmin {
        if target_global_role == SystemRole::SuperAdmin {
            return Err(AuthError::ForbiddenMessage(
                "Super Admin permissions cannot be revoked.".into(),
            ));
        }
        if target_global_role != SystemRole::User
            || existing_space_role.map(|(r,)| r == SpaceRole::SpaceAdmin).unwrap_or(false)
            || target_role == SpaceRole::SpaceAdmin
        {
            return Err(AuthError::ForbiddenMessage(
                "Admins can only manage Space permissions for standard User accounts.".into(),
            ));
        }
    }

    let member: SpaceMember = sqlx::query_as(
        r#"
        INSERT INTO space_members (space_id, user_id, space_role, granted_by)
        VALUES ($1, $2, $3, $4)
        ON CONFLICT (space_id, user_id)
        DO UPDATE SET space_role = EXCLUDED.space_role, granted_by = EXCLUDED.granted_by
        RETURNING space_id, user_id, space_role, granted_by, created_at
        "#,
    )
    .bind(space_id)
    .bind(payload.user_id)
    .bind(target_role)
    .bind(actor.user_id)
    .fetch_one(&state.db)
    .await?;

    state.cache.invalidate_user(&payload.user_id);
    state.cache.invalidate_space(&space_id);

    record_audit_log(
        &state.db,
        actor.user_id,
        Some(space_id),
        None,
        "space.member.grant",
        json!({
            "target_user_id": payload.user_id,
            "space_role": target_role
        }),
    )
    .await?;

    Ok((StatusCode::CREATED, Json(member)))
}

/// `DELETE /api/spaces/{space_id}/members/{user_id}`
/// Revokes a user's access from a Space with strict hierarchical permission checks:
/// - `SuperAdmin` can NEVER be removed or revoked from any Space (`StatusCode::FORBIDDEN`).
/// - `SuperAdmin` can revoke any `Admin` or `User`.
/// - `Admin` can ONLY revoke standard `User` accounts (`target_user.system_role == SystemRole::User` and `target_space_role != SpaceRole::SpaceAdmin`).
pub async fn revoke_space_member(
    State(state): State<AppState>,
    RequireSpaceAdmin(actor): RequireSpaceAdmin,
    Path((space_id, target_user_id)): Path<(Uuid, Uuid)>,
) -> Result<StatusCode, AuthError> {
    let access = resolve_space_access_cached(&state, actor.clone(), space_id).await?;
    access.ensure_manage()?;

    let (target_global_role,): (SystemRole,) = sqlx::query_as(
        "SELECT system_role FROM users WHERE id = $1",
    )
    .bind(target_user_id)
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(|| AuthError::NotFound(format!("User {target_user_id} not found")))?;

    // Rule 1: SuperAdmin Protection (Immutable Root)
    if target_global_role == SystemRole::SuperAdmin {
        return Err(AuthError::ForbiddenMessage(
            "Super Admin permissions cannot be revoked.".into(),
        ));
    }

    let existing_space_role: Option<(SpaceRole,)> = sqlx::query_as(
        "SELECT space_role FROM space_members WHERE space_id = $1 AND user_id = $2",
    )
    .bind(space_id)
    .bind(target_user_id)
    .fetch_optional(&state.db)
    .await?;

    let Some((target_space_role,)) = existing_space_role else {
        return Err(AuthError::NotFound(
            "User is not a member of this Space".into(),
        ));
    };

    // Rule 3: Admin Authority — an Admin cannot revoke another Admin or SpaceAdmin
    if actor.system_role != SystemRole::SuperAdmin
        && (target_global_role != SystemRole::User || target_space_role == SpaceRole::SpaceAdmin)
    {
        return Err(AuthError::ForbiddenMessage(
            "Admins can only revoke Space access for standard User accounts.".into(),
        ));
    }

    let deleted = sqlx::query(
        "DELETE FROM space_members WHERE space_id = $1 AND user_id = $2",
    )
    .bind(space_id)
    .bind(target_user_id)
    .execute(&state.db)
    .await?;

    if deleted.rows_affected() == 0 {
        return Err(AuthError::NotFound(
            "User is not a member of this Space".into(),
        ));
    }

    state.cache.invalidate_user(&target_user_id);
    state.cache.invalidate_space(&space_id);

    record_audit_log(
        &state.db,
        actor.user_id,
        Some(space_id),
        None,
        "space.member.revoke",
        json!({ "revoked_user_id": target_user_id }),
    )
    .await?;

    Ok(StatusCode::NO_CONTENT)
}

// ============================================================================
// 3. Shared Space Discovery Endpoint (`RequireSpaceAccess`)
// ============================================================================

/// `GET /api/spaces`
/// SuperAdmin sees all active Spaces (`effective_role = SpaceAdmin`);
/// Admins and Standard Users only see Spaces they are explicitly assigned to in `space_members`.
pub async fn list_spaces(
    State(state): State<AppState>,
    RequireSpaceAccess(user): RequireSpaceAccess,
) -> Result<Json<Vec<SpaceWithRole>>, AuthError> {
    if user.system_role == SystemRole::SuperAdmin {
        let spaces: Vec<Space> = sqlx::query_as(
            r#"
            SELECT id, slug, name, description, icon, created_by, is_archived, created_at, updated_at
            FROM spaces
            WHERE is_archived = FALSE
            ORDER BY created_at DESC
            "#,
        )
        .fetch_all(&state.db)
        .await?;

        let response = spaces
            .into_iter()
            .map(|space| SpaceWithRole {
                space,
                effective_role: SpaceRole::SpaceAdmin,
            })
            .collect();

        return Ok(Json(response));
    }

    #[derive(sqlx::FromRow)]
    struct JoinedSpaceRow {
        id: Uuid,
        slug: String,
        name: String,
        description: Option<String>,
        icon: Option<String>,
        created_by: Uuid,
        is_archived: bool,
        created_at: chrono::DateTime<chrono::Utc>,
        updated_at: chrono::DateTime<chrono::Utc>,
        space_role: SpaceRole,
    }

    let rows: Vec<JoinedSpaceRow> = sqlx::query_as(
        r#"
        SELECT
            s.id, s.slug, s.name, s.description, s.icon, s.created_by,
            s.is_archived, s.created_at, s.updated_at,
            sm.space_role
        FROM spaces s
        INNER JOIN space_members sm ON sm.space_id = s.id
        WHERE sm.user_id = $1
          AND s.is_archived = FALSE
        ORDER BY s.created_at DESC
        "#,
    )
    .bind(user.user_id)
    .fetch_all(&state.db)
    .await?;

    let response = rows
        .into_iter()
        .map(|r| SpaceWithRole {
            space: Space {
                id: r.id,
                slug: r.slug,
                name: r.name,
                description: r.description,
                icon: r.icon,
                created_by: r.created_by,
                is_archived: r.is_archived,
                created_at: r.created_at,
                updated_at: r.updated_at,
            },
            effective_role: r.space_role,
        })
        .collect();

    Ok(Json(response))
}
