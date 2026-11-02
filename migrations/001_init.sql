-- ============================================================================
-- PyraOcean Phase 1: Core Infrastructure, RBAC & Dynamic Grid Database Schema
-- PostgreSQL 16+
-- ============================================================================

CREATE EXTENSION IF NOT EXISTS "pgcrypto";
CREATE EXTENSION IF NOT EXISTS "btree_gin";

-- ----------------------------------------------------------------------------
-- 1. Custom Enum Types
-- ----------------------------------------------------------------------------

-- Global 3-tier RBAC hierarchy
CREATE TYPE system_role AS ENUM (
    'user',
    'admin',
    'super_admin'
);

-- Space-level granular permission roles
CREATE TYPE space_role AS ENUM (
    'viewer',
    'editor',
    'space_admin'
);

-- The 11 strict dynamic column property types
CREATE TYPE column_property_type AS ENUM (
    'text',
    'number',
    'select',
    'multi_select',
    'date',
    'time',
    'files',
    'checkbox',
    'checklist',
    'last_modified',
    'person'
);

-- ----------------------------------------------------------------------------
-- 2. Users & Roles Table
-- ----------------------------------------------------------------------------

CREATE TABLE roles (
    role_name       system_role PRIMARY KEY,
    description     TEXT NOT NULL,
    can_manage_all  BOOLEAN NOT NULL DEFAULT FALSE,
    can_create_space BOOLEAN NOT NULL DEFAULT FALSE,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

INSERT INTO roles (role_name, description, can_manage_all, can_create_space) VALUES
    ('super_admin', 'Global god-mode account (singleton enforced). Full system & space access.', TRUE, TRUE),
    ('admin',       'Workspace / Department Manager. Manages assigned spaces, grids, and imports.', FALSE, TRUE),
    ('user',        'Standard member. Views and edits explicitly granted spaces and grids.', FALSE, FALSE);

CREATE TABLE users (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    email           TEXT NOT NULL UNIQUE,
    display_name    TEXT NOT NULL,
    password_hash   TEXT NOT NULL,
    system_role     system_role NOT NULL DEFAULT 'user' REFERENCES roles(role_name),
    is_active       BOOLEAN NOT NULL DEFAULT TRUE,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- Strictly enforce the "1 Global Super Admin Account" rule at the database level
CREATE UNIQUE INDEX idx_users_singleton_super_admin
    ON users (system_role)
    WHERE system_role = 'super_admin';

CREATE INDEX idx_users_email ON users (email);
CREATE INDEX idx_users_system_role ON users (system_role);

-- ----------------------------------------------------------------------------
-- 3. Workspaces (Spaces) & RBAC Mapping (space_members)
-- ----------------------------------------------------------------------------

CREATE TABLE spaces (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    slug            TEXT NOT NULL UNIQUE,
    name            TEXT NOT NULL,
    description     TEXT,
    icon            TEXT,
    created_by      UUID NOT NULL REFERENCES users(id) ON DELETE RESTRICT,
    is_archived     BOOLEAN NOT NULL DEFAULT FALSE,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX idx_spaces_slug ON spaces (slug) WHERE is_archived = FALSE;

CREATE TABLE space_members (
    space_id        UUID NOT NULL REFERENCES spaces(id) ON DELETE CASCADE,
    user_id         UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    space_role      space_role NOT NULL DEFAULT 'viewer',
    granted_by      UUID NOT NULL REFERENCES users(id) ON DELETE RESTRICT,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (space_id, user_id)
);

CREATE INDEX idx_space_members_user_lookup ON space_members (user_id, space_id, space_role);

-- ----------------------------------------------------------------------------
-- 4. Pages / Grids (pages_grids) & Page-Level Member Grants
-- ----------------------------------------------------------------------------

CREATE TABLE pages_grids (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    space_id        UUID NOT NULL REFERENCES spaces(id) ON DELETE CASCADE,
    title           TEXT NOT NULL,
    description     TEXT,
    icon            TEXT,
    position        INTEGER NOT NULL DEFAULT 0,
    created_by      UUID NOT NULL REFERENCES users(id) ON DELETE RESTRICT,
    is_archived     BOOLEAN NOT NULL DEFAULT FALSE,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX idx_pages_grids_space_position ON pages_grids (space_id, position) WHERE is_archived = FALSE;

-- Optional granular Page/Grid-level override for Standard Users
CREATE TABLE page_grid_permissions (
    grid_id         UUID NOT NULL REFERENCES pages_grids(id) ON DELETE CASCADE,
    user_id         UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    can_edit        BOOLEAN NOT NULL DEFAULT FALSE,
    granted_by      UUID NOT NULL REFERENCES users(id) ON DELETE RESTRICT,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (grid_id, user_id)
);

-- ----------------------------------------------------------------------------
-- 5. Dynamic Grid Columns (grid_columns)
-- ----------------------------------------------------------------------------

CREATE TABLE grid_columns (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    grid_id         UUID NOT NULL REFERENCES pages_grids(id) ON DELETE CASCADE,
    name            TEXT NOT NULL,
    property_type   column_property_type NOT NULL,
    -- Stores select/multi_select option tags, number formatting, date format, etc.
    property_config JSONB NOT NULL DEFAULT '{}'::jsonb,
    width_px        INTEGER NOT NULL DEFAULT 180 CHECK (width_px >= 60 AND width_px <= 1200),
    position        INTEGER NOT NULL DEFAULT 0,
    is_primary      BOOLEAN NOT NULL DEFAULT FALSE,
    is_hidden       BOOLEAN NOT NULL DEFAULT FALSE,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX idx_grid_columns_grid_pos ON grid_columns (grid_id, position);
CREATE INDEX idx_grid_columns_config_gin ON grid_columns USING GIN (property_config jsonb_path_ops);

-- ----------------------------------------------------------------------------
-- 6. High-Speed JSONB Grid Rows (grid_rows)
-- ----------------------------------------------------------------------------

CREATE TABLE grid_rows (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    grid_id         UUID NOT NULL REFERENCES pages_grids(id) ON DELETE CASCADE,
    -- Fractional indexing float for O(1) row reordering without renumbering the entire table
    row_order       DOUBLE PRECISION NOT NULL DEFAULT 1000.0,
    -- Dynamic cell map keyed by column UUID -> {"t": "<property_type>", "v": <typed_value>}
    cells           JSONB NOT NULL DEFAULT '{}'::jsonb,
    created_by      UUID NOT NULL REFERENCES users(id) ON DELETE RESTRICT,
    updated_by      UUID NOT NULL REFERENCES users(id) ON DELETE RESTRICT,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    last_modified   TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- B-Tree composite index for fast pagination and default ordering within a grid
CREATE INDEX idx_grid_rows_grid_order ON grid_rows (grid_id, row_order ASC, id ASC);

-- B-Tree index for sorting/filtering by LastModified property
CREATE INDEX idx_grid_rows_last_modified ON grid_rows (grid_id, last_modified DESC);

-- GIN index with jsonb_path_ops for sub-millisecond containment queries (@>, @?) on dynamic cells
CREATE INDEX idx_grid_rows_cells_gin ON grid_rows USING GIN (cells jsonb_path_ops);

-- ----------------------------------------------------------------------------
-- 7. System Audit Logs (Super Admin Visibility)
-- ----------------------------------------------------------------------------

CREATE TABLE audit_logs (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    actor_id        UUID REFERENCES users(id) ON DELETE SET NULL,
    space_id        UUID REFERENCES spaces(id) ON DELETE SET NULL,
    grid_id         UUID REFERENCES pages_grids(id) ON DELETE SET NULL,
    action          TEXT NOT NULL,
    metadata        JSONB NOT NULL DEFAULT '{}'::jsonb,
    ip_address      TEXT,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX idx_audit_logs_created_at ON audit_logs (created_at DESC);
CREATE INDEX idx_audit_logs_actor ON audit_logs (actor_id, created_at DESC);

-- ----------------------------------------------------------------------------
-- 8. Automated Triggers for `updated_at` and `LastModified` Column Sync
-- ----------------------------------------------------------------------------

CREATE OR REPLACE FUNCTION trigger_set_updated_at()
RETURNS TRIGGER AS $$
BEGIN
    NEW.updated_at = NOW();
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER trg_users_updated_at
    BEFORE UPDATE ON users FOR EACH ROW EXECUTE FUNCTION trigger_set_updated_at();

CREATE TRIGGER trg_spaces_updated_at
    BEFORE UPDATE ON spaces FOR EACH ROW EXECUTE FUNCTION trigger_set_updated_at();

CREATE TRIGGER trg_pages_grids_updated_at
    BEFORE UPDATE ON pages_grids FOR EACH ROW EXECUTE FUNCTION trigger_set_updated_at();

CREATE TRIGGER trg_grid_columns_updated_at
    BEFORE UPDATE ON grid_columns FOR EACH ROW EXECUTE FUNCTION trigger_set_updated_at();

-- Automatically updates `grid_rows.last_modified` and injects the timestamp into any
-- `last_modified` typed column cells belonging to the row's grid.
CREATE OR REPLACE FUNCTION trigger_sync_grid_row_last_modified()
RETURNS TRIGGER AS $$
DECLARE
    col_id UUID;
    now_ts TIMESTAMPTZ := NOW();
BEGIN
    NEW.last_modified := now_ts;

    FOR col_id IN
        SELECT id FROM grid_columns
        WHERE grid_id = NEW.grid_id AND property_type = 'last_modified'
    LOOP
        NEW.cells := jsonb_set(
            NEW.cells,
            ARRAY[col_id::text],
            jsonb_build_object('t', 'last_modified', 'v', to_jsonb(now_ts)),
            TRUE
        );
    END LOOP;

    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER trg_grid_rows_last_modified
    BEFORE INSERT OR UPDATE ON grid_rows
    FOR EACH ROW EXECUTE FUNCTION trigger_sync_grid_row_last_modified();

