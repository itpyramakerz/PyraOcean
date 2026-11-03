use chrono::{DateTime, NaiveDate, NaiveTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use uuid::Uuid;

#[cfg(feature = "ssr")]
use sqlx::{FromRow, Type};

// ============================================================================
// 1. RBAC Hierarchy & Permissions
// ============================================================================

/// Global 3-tier system role assigned at the user account level.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(Type))]
#[cfg_attr(
    feature = "ssr",
    sqlx(type_name = "system_role", rename_all = "snake_case")
)]
#[serde(rename_all = "snake_case")]
pub enum SystemRole {
    /// Standard member: can only view/edit explicitly granted Spaces and Pages.
    User,
    /// Workspace/Department manager: can manage assigned Spaces, grids, and imports.
    Admin,
    /// Single global account with full system visibility and god-mode privileges.
    SuperAdmin,
}

impl SystemRole {
    pub fn is_super_admin(&self) -> bool {
        matches!(self, SystemRole::SuperAdmin)
    }

    pub fn is_admin_or_above(&self) -> bool {
        matches!(self, SystemRole::Admin | SystemRole::SuperAdmin)
    }

    pub fn label(&self) -> &'static str {
        match self {
            SystemRole::SuperAdmin => "Super Admin",
            SystemRole::Admin => "Admin",
            SystemRole::User => "User",
        }
    }
}

/// Space-scoped role mapping for granular access control within a Workspace (`Space`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(Type))]
#[cfg_attr(
    feature = "ssr",
    sqlx(type_name = "space_role", rename_all = "snake_case")
)]
#[serde(rename_all = "snake_case")]
pub enum SpaceRole {
    /// Read-only access to explicitly permitted pages/grids.
    Viewer,
    /// Can read and edit cells/rows in permitted grids, but cannot alter schema or members.
    Editor,
    /// Full control over the Space: manage members, create/delete grids, alter columns, import data.
    SpaceAdmin,
}

impl SpaceRole {
    pub fn can_read(&self) -> bool {
        true
    }

    pub fn can_write(&self) -> bool {
        matches!(self, SpaceRole::Editor | SpaceRole::SpaceAdmin)
    }

    pub fn can_manage_space(&self) -> bool {
        matches!(self, SpaceRole::SpaceAdmin)
    }

    pub fn label(&self) -> &'static str {
        match self {
            SpaceRole::Viewer => "View Only",
            SpaceRole::Editor => "Can Edit",
            SpaceRole::SpaceAdmin => "Space Admin",
        }
    }
}

// ============================================================================
// 2. Core Domain Entities (Users, Spaces, Grids) & Shared API DTOs
// ============================================================================

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(FromRow))]
pub struct User {
    pub id: Uuid,
    pub email: String,
    pub display_name: String,
    #[serde(default, skip_serializing)]
    pub password_hash: String,
    pub system_role: SystemRole,
    pub is_active: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Client-side authenticated user profile (`UserProfile`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserProfile {
    pub id: Uuid,
    pub email: String,
    pub display_name: String,
    pub system_role: SystemRole,
    pub is_active: bool,
}

impl From<User> for UserProfile {
    fn from(u: User) -> Self {
        Self {
            id: u.id,
            email: u.email,
            display_name: u.display_name,
            system_role: u.system_role,
            is_active: u.is_active,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(FromRow))]
pub struct Space {
    pub id: Uuid,
    pub slug: String,
    pub name: String,
    pub description: Option<String>,
    pub icon: Option<String>,
    pub created_by: Uuid,
    pub is_archived: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Space returned by `GET /api/spaces` including the caller's effective `SpaceRole`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpaceWithRole {
    #[serde(flatten)]
    pub space: Space,
    pub effective_role: SpaceRole,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(FromRow))]
pub struct SpaceMember {
    pub space_id: Uuid,
    pub user_id: Uuid,
    pub space_role: SpaceRole,
    pub granted_by: Uuid,
    pub created_at: DateTime<Utc>,
}

/// Enriched Space member row returned by `GET /api/spaces/{space_id}/members`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(FromRow))]
pub struct SpaceMemberDetail {
    pub space_id: Uuid,
    pub user_id: Uuid,
    pub email: String,
    pub display_name: String,
    pub global_role: SystemRole,
    pub space_role: SpaceRole,
    pub granted_by: Uuid,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(FromRow))]
pub struct PageGrid {
    pub id: Uuid,
    pub space_id: Uuid,
    pub title: String,
    pub description: Option<String>,
    pub icon: Option<String>,
    pub position: i32,
    pub created_by: Uuid,
    pub is_archived: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(FromRow))]
pub struct AuditLogEntry {
    pub id: Uuid,
    pub actor_id: Option<Uuid>,
    pub actor_email: Option<String>,
    pub space_id: Option<Uuid>,
    pub grid_id: Option<Uuid>,
    pub action: String,
    pub metadata: serde_json::Value,
    pub ip_address: Option<String>,
    pub created_at: DateTime<Utc>,
}

// ============================================================================
// 3. The 11 Strict Column Property Types & Schema Configuration
// ============================================================================

/// Discriminator enum matching PostgreSQL `column_property_type`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(Type))]
#[cfg_attr(
    feature = "ssr",
    sqlx(type_name = "column_property_type", rename_all = "snake_case")
)]
#[serde(rename_all = "snake_case")]
pub enum ColumnPropertyType {
    Text,
    Number,
    Select,
    MultiSelect,
    Date,
    Time,
    Files,
    Checkbox,
    Checklist,
    LastModified,
    Person,
}

/// Standardized color palette for `Select` and `MultiSelect` option tags.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TagColor {
    Slate,
    Gray,
    Red,
    Orange,
    Amber,
    Green,
    Emerald,
    Teal,
    Cyan,
    Blue,
    Indigo,
    Violet,
    Purple,
    Pink,
    Rose,
}

/// Option metadata for `Select` and `MultiSelect` columns.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SelectOption {
    pub id: Uuid,
    pub label: String,
    pub color: TagColor,
}

/// Number formatting rules for `Number` columns.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NumberFormat {
    Integer,
    Decimal { precision: u8 },
    Percentage { precision: u8 },
    Currency { symbol: String, precision: u8 },
}

impl Default for NumberFormat {
    fn default() -> Self {
        Self::Decimal { precision: 2 }
    }
}

/// Type-specific column metadata stored in `grid_columns.property_config` (`JSONB`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "config_type", rename_all = "snake_case")]
pub enum ColumnPropertyConfig {
    Text {
        rich_text: bool,
        max_length: Option<u32>,
    },
    Number {
        format: NumberFormat,
    },
    Select {
        options: Vec<SelectOption>,
    },
    MultiSelect {
        options: Vec<SelectOption>,
    },
    Date {
        /// e.g. "%Y-%m-%d" or "%d/%m/%Y"
        display_format: String,
        include_time: bool,
    },
    Time {
        use_24_hour: bool,
        is_duration: bool,
    },
    Files {
        allowed_mime_prefixes: Vec<String>,
        max_file_size_bytes: u64,
    },
    Checkbox,
    Checklist {
        show_progress_bar: bool,
    },
    LastModified,
    Person {
        allow_multiple: bool,
    },
}

/// Database row representation of `grid_columns`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(FromRow))]
pub struct GridColumn {
    pub id: Uuid,
    pub grid_id: Uuid,
    pub name: String,
    pub property_type: ColumnPropertyType,
    #[cfg_attr(feature = "ssr", sqlx(json))]
    pub property_config: ColumnPropertyConfig,
    pub width_px: i32,
    pub position: i32,
    pub is_primary: bool,
    pub is_hidden: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

// ============================================================================
// 4. The 11 Dynamic Cell Values & Validation
// ============================================================================

/// Metadata for a single file attachment in a `Files` column cell.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileAttachment {
    pub id: Uuid,
    pub filename: String,
    pub mime_type: String,
    pub size_bytes: u64,
    /// Local storage path or S3 object URL.
    pub storage_url: String,
    pub uploaded_by: Uuid,
    pub uploaded_at: DateTime<Utc>,
}

/// Embedded checklist item inside a `Checklist` column cell.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChecklistItem {
    pub id: Uuid,
    pub item: String,
    pub completed: bool,
}

/// Strongly-typed value stored inside `grid_rows.cells` (`JSONB` map keyed by `column_id`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "t", content = "v", rename_all = "snake_case")]
pub enum CellValue {
    /// 1. `Text`: Rich or plain string.
    Text(String),
    /// 2. `Number`: IEEE-754 float (supports integer, decimal, and currency display via column config).
    Number(f64),
    /// 3. `Select`: Single `SelectOption.id` reference.
    Select(Option<Uuid>),
    /// 4. `MultiSelect`: Array of `SelectOption.id` references.
    MultiSelect(Vec<Uuid>),
    /// 5. `Date`: ISO-8601 date (`YYYY-MM-DD`).
    Date(Option<NaiveDate>),
    /// 6. `Time`: Time of day (`HH:MM:SS`) or duration in seconds.
    Time {
        time_of_day: Option<NaiveTime>,
        duration_seconds: Option<i64>,
    },
    /// 7. `Files`: Array of file metadata/URLs attached to the cell.
    Files(Vec<FileAttachment>),
    /// 8. `Checkbox`: Boolean true/false toggle.
    Checkbox(bool),
    /// 9. `Checklist`: Embedded array of `{ id, item, completed }` with computed progress helper.
    Checklist(Vec<ChecklistItem>),
    /// 10. `LastModified`: System-automated UTC timestamp updated on row edit.
    LastModified(DateTime<Utc>),
    /// 11. `Person`: Foreign key references to one or more `User` IDs.
    Person(Vec<Uuid>),
}

impl CellValue {
    /// Returns the `ColumnPropertyType` corresponding to this `CellValue`.
    pub fn property_type(&self) -> ColumnPropertyType {
        match self {
            CellValue::Text(_) => ColumnPropertyType::Text,
            CellValue::Number(_) => ColumnPropertyType::Number,
            CellValue::Select(_) => ColumnPropertyType::Select,
            CellValue::MultiSelect(_) => ColumnPropertyType::MultiSelect,
            CellValue::Date(_) => ColumnPropertyType::Date,
            CellValue::Time { .. } => ColumnPropertyType::Time,
            CellValue::Files(_) => ColumnPropertyType::Files,
            CellValue::Checkbox(_) => ColumnPropertyType::Checkbox,
            CellValue::Checklist(_) => ColumnPropertyType::Checklist,
            CellValue::LastModified(_) => ColumnPropertyType::LastModified,
            CellValue::Person(_) => ColumnPropertyType::Person,
        }
    }

    /// Computes completion progress (`0.0..=1.0`) if this cell is a `Checklist`.
    pub fn checklist_progress(&self) -> Option<f32> {
        if let CellValue::Checklist(items) = self {
            if items.is_empty() {
                return Some(0.0);
            }
            let completed = items.iter().filter(|i| i.completed).count() as f32;
            Some(completed / (items.len() as f32))
        } else {
            None
        }
    }

    /// Validates that the cell value conforms to the column's `ColumnPropertyType` and `ColumnPropertyConfig`.
    pub fn validate(&self, col: &GridColumn) -> Result<(), CellValidationError> {
        if self.property_type() != col.property_type {
            return Err(CellValidationError::TypeMismatch {
                column_id: col.id,
                expected: col.property_type,
                actual: self.property_type(),
            });
        }

        match (self, &col.property_config) {
            (CellValue::Text(s), ColumnPropertyConfig::Text { max_length: Some(max), .. }) => {
                if s.chars().count() > *max as usize {
                    return Err(CellValidationError::ConstraintViolation(format!(
                        "Text length exceeds max_length of {max}"
                    )));
                }
            }
            (CellValue::Number(n), _) if !n.is_finite() => {
                return Err(CellValidationError::ConstraintViolation(
                    "Number value must be finite".into(),
                ));
            }
            (CellValue::Select(Some(opt_id)), ColumnPropertyConfig::Select { options }) => {
                if !options.iter().any(|o| &o.id == opt_id) {
                    return Err(CellValidationError::InvalidOptionId(*opt_id));
                }
            }
            (CellValue::MultiSelect(opt_ids), ColumnPropertyConfig::MultiSelect { options }) => {
                for id in opt_ids {
                    if !options.iter().any(|o| &o.id == id) {
                        return Err(CellValidationError::InvalidOptionId(*id));
                    }
                }
            }
            (CellValue::Person(users), ColumnPropertyConfig::Person { allow_multiple: false }) => {
                if users.len() > 1 {
                    return Err(CellValidationError::ConstraintViolation(
                        "Column only allows a single Person reference".into(),
                    ));
                }
            }
            _ => {}
        }

        Ok(())
    }
}

#[derive(Debug, thiserror::Error, Serialize, Deserialize)]
pub enum CellValidationError {
    #[error("Column {column_id} expected type {expected:?}, got {actual:?}")]
    TypeMismatch {
        column_id: Uuid,
        expected: ColumnPropertyType,
        actual: ColumnPropertyType,
    },
    #[error("Select option ID {0} does not exist in column schema")]
    InvalidOptionId(Uuid),
    #[error("Cell constraint violation: {0}")]
    ConstraintViolation(String),
}

/// Map of `GridColumn.id -> CellValue` stored in `grid_rows.cells` (`JSONB`).
pub type RowCellMap = HashMap<Uuid, CellValue>;

/// Database row representation of `grid_rows`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(FromRow))]
pub struct GridRow {
    pub id: Uuid,
    pub grid_id: Uuid,
    #[serde(default)]
    #[cfg_attr(feature = "ssr", sqlx(default))]
    pub position: i32,
    pub row_order: f64,
    #[cfg_attr(feature = "ssr", sqlx(json))]
    pub cells: RowCellMap,
    pub created_by: Uuid,
    pub updated_by: Uuid,
    pub created_at: DateTime<Utc>,
    pub last_modified: DateTime<Utc>,
}

// ============================================================================
// 5. Query, Sort, Filter, Auth & Admin Request/Response DTOs
// ============================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SortDirection {
    Asc,
    Desc,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ColumnSortSpec {
    pub column_id: Uuid,
    pub direction: SortDirection,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FilterOperator {
    Equals,
    NotEquals,
    Contains,
    GreaterThan,
    LessThan,
    IsEmpty,
    IsNotEmpty,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ColumnFilterSpec {
    pub column_id: Uuid,
    pub operator: FilterOperator,
    pub value: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImportMode {
    /// Automatically infer the 11 property types from headers and sample rows.
    FormattedSchemaInference,
    /// Default all columns to `Text` and strip Markdown/HTML formatting.
    UnformattedRawText,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoginRequest {
    pub email: String,
    pub password: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BrandingMetadata {
    pub app_name: String,
    pub app_logo_dark: String,
    pub app_logo_light: String,
    pub powered_by: String,
    pub powered_by_logo: String,
}

impl Default for BrandingMetadata {
    fn default() -> Self {
        Self {
            app_name: "PyraOcean".into(),
            app_logo_dark: "/public/logo-dark.jpg".into(),
            app_logo_light: "/public/logo-light.jpg".into(),
            powered_by: "Powered by Pyramakerz".into(),
            powered_by_logo: "/public/pyramakerz.webp".into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoginResponse {
    pub token: String,
    pub user: User,
    pub branding: BrandingMetadata,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateUserRequest {
    pub email: String,
    pub display_name: String,
    pub password: String,
    pub system_role: SystemRole,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateUserRoleRequest {
    pub system_role: SystemRole,
    pub is_active: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateSpaceRequest {
    pub slug: String,
    pub name: String,
    pub description: Option<String>,
    pub icon: Option<String>,
    #[serde(default)]
    pub admin_manager_ids: Vec<Uuid>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GrantSpaceMemberRequest {
    pub user_id: Uuid,
    pub can_view: bool,
    pub can_edit: bool,
    #[serde(default)]
    pub is_space_admin: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateGridRequest {
    pub title: String,
    pub description: Option<String>,
    pub icon: Option<String>,
    pub position: Option<i32>,
}

/// Request payload for `PATCH /api/grids/{grid_id}` to rename a Grid Page or update its description.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UpdateGridRequest {
    /// Accepts `"name"` or `"title"` from JSON callers.
    #[serde(alias = "title")]
    pub name: Option<String>,
    pub description: Option<String>,
    pub icon: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GridSchemaResponse {
    pub grid: PageGrid,
    pub columns: Vec<GridColumn>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateColumnRequest {
    pub name: String,
    pub property_type: ColumnPropertyType,
    pub property_config: Option<ColumnPropertyConfig>,
    pub width_px: Option<i32>,
    pub position: Option<i32>,
    pub is_primary: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateColumnRequest {
    pub name: Option<String>,
    pub property_type: Option<ColumnPropertyType>,
    pub property_config: Option<ColumnPropertyConfig>,
    pub width_px: Option<i32>,
    pub position: Option<i32>,
    pub is_hidden: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReorderColumnsRequest {
    pub column_ids: Vec<Uuid>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SearchScope {
    Grid,
    Space,
    All,
}

impl SearchScope {
    pub fn as_str(&self) -> &'static str {
        match self {
            SearchScope::Grid => "grid",
            SearchScope::Space => "space",
            SearchScope::All => "all",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(FromRow))]
pub struct SearchHit {
    pub space_id: Uuid,
    pub space_name: String,
    pub grid_id: Uuid,
    pub grid_title: String,
    pub row_id: Option<Uuid>,
    pub match_type: String,
    pub snippet: String,
    pub last_modified: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateRowRequest {
    pub row_order: Option<f64>,
    pub cells: RowCellMap,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PatchRowRequest {
    pub row_order: Option<f64>,
    pub cells: RowCellMap,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BulkRowCellPatch {
    pub row_id: Uuid,
    pub cells: RowCellMap,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BulkPatchCellsRequest {
    pub updates: Vec<BulkRowCellPatch>,
}

/// Request payload for `PATCH /api/grids/{grid_id}/rows/reorder`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReorderRowsRequest {
    pub row_ids: Vec<Uuid>,
}

/// Request payload for `POST /api/grids/{grid_id}/rows/restore` (Undo deleted rows).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RestoreRowsRequest {
    pub rows: Vec<GridRow>,
}

/// Full JSONB snapshot stored in `grid_history_snapshots.snapshot_data`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GridSnapshotData {
    pub columns: Vec<GridColumn>,
    pub rows: Vec<GridRow>,
    #[serde(default)]
    pub summary: String,
}

/// Summary entry returned by `GET /api/grids/{grid_id}/history` (30-Day Grid History).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(FromRow))]
pub struct GridHistoryEntry {
    pub id: Uuid,
    pub grid_id: Uuid,
    pub action_type: String,
    pub actor_user_id: Option<Uuid>,
    pub actor_email: Option<String>,
    pub summary: String,
    pub column_count: i32,
    pub row_count: i32,
    pub created_at: DateTime<Utc>,
}

/// Response returned by `POST /api/grids/{grid_id}/history/{history_id}/restore`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RestoreGridHistoryResponse {
    pub grid: PageGrid,
    pub columns: Vec<GridColumn>,
    pub rows: Vec<GridRow>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BulkDeleteRowsRequest {
    pub row_ids: Vec<Uuid>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BulkDeleteRowsResponse {
    pub deleted_count: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImportGridResponse {
    pub grid: PageGrid,
    pub columns: Vec<GridColumn>,
    pub imported_row_count: usize,
    pub mode: ImportMode,
    pub detected_delimiter: Option<String>,
}
