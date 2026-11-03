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
    #[serde(default)]
    pub insert_at_position: Option<i32>,
    #[serde(default)]
    pub target_row_id: Option<Uuid>,
    #[serde(default)]
    pub placement: Option<String>,
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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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

/// Detailed point-in-time snapshot returned by `GET /api/grids/{grid_id}/history/{history_id}`
/// for the Full-Page History Preview Modal.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GridHistoryDetail {
    pub id: Uuid,
    pub grid_id: Uuid,
    pub action_type: String,
    pub actor_user_id: Option<Uuid>,
    pub actor_email: Option<String>,
    pub summary: String,
    pub created_at: DateTime<Utc>,
    pub snapshot_data: GridSnapshotData,
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
pub struct BulkDeleteGridsRequest {
    pub grid_ids: Vec<Uuid>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BulkDeleteGridsResponse {
    pub deleted_count: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreviewColumnInfo {
    pub name: String,
    pub inferred_type: ColumnPropertyType,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreviewImportResponse {
    pub suggested_title: String,
    pub columns: Vec<PreviewColumnInfo>,
    pub sample_rows: Vec<Vec<String>>,
    pub total_row_count: usize,
    pub detected_delimiter: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImportGridResponse {
    pub grid: PageGrid,
    pub columns: Vec<GridColumn>,
    pub imported_row_count: usize,
    pub mode: ImportMode,
    pub detected_delimiter: Option<String>,
}

// ============================================================================
// 6. Universal Property Value Serializer & Multi-Format Export Generators
// ============================================================================

/// Supported export formats for `GET /api/grids/{grid_id}/export?format=csv|md` and client-side exports.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExportFormat {
    Csv,
    #[serde(alias = "md")]
    Markdown,
    Pdf,
}

/// Generates a clean, URL/filesystem-safe slug filename with the given extension (e.g. `q4-roadmap.csv`).
pub fn slugify_grid_filename(title: &str, ext: &str) -> String {
    let mut slug = String::with_capacity(title.len());
    let mut last_dash = false;
    for ch in title.chars() {
        if ch.is_alphanumeric() {
            for lc in ch.to_lowercase() {
                slug.push(lc);
            }
            last_dash = false;
        } else if !last_dash && !slug.is_empty() {
            slug.push('-');
            last_dash = true;
        }
    }
    let clean = slug.trim_matches('-');
    let base = if clean.is_empty() { "pyraocean-grid" } else { clean };
    format!("{base}.{ext}")
}

/// Deterministic universal serializer that converts any of the **11 Property Types** into
/// human-readable export text for CSV and GitHub-Flavored Markdown tables.
pub fn format_cell_for_export(
    cell: Option<&CellValue>,
    column: &GridColumn,
    user_display_map: &HashMap<Uuid, String>,
    row_last_modified: DateTime<Utc>,
    is_markdown: bool,
) -> String {
    match (&column.property_type, cell) {
        (ColumnPropertyType::LastModified, Some(CellValue::LastModified(dt))) => {
            dt.format("%Y-%m-%d %H:%M:%S UTC").to_string()
        }
        (ColumnPropertyType::LastModified, _) => {
            row_last_modified.format("%Y-%m-%d %H:%M:%S UTC").to_string()
        }
        (ColumnPropertyType::Checkbox, Some(CellValue::Checkbox(b))) => {
            if is_markdown {
                if *b { "[x]".into() } else { "[ ]".into() }
            } else if *b {
                "true".into()
            } else {
                "false".into()
            }
        }
        (ColumnPropertyType::Checkbox, None) => {
            if is_markdown {
                "[ ]".into()
            } else {
                "false".into()
            }
        }
        (_, None) => String::new(),
        (_, Some(CellValue::Text(s))) => s.clone(),
        (_, Some(CellValue::Number(n))) => match &column.property_config {
            ColumnPropertyConfig::Number { format } => match format {
                NumberFormat::Integer => format!("{:.0}", n),
                NumberFormat::Decimal { precision } => {
                    format!("{:.prec$}", n, prec = *precision as usize)
                }
                NumberFormat::Percentage { precision } => {
                    format!("{:.prec$}%", n, prec = *precision as usize)
                }
                NumberFormat::Currency { symbol, precision } => {
                    format!("{symbol} {:.prec$}", n, prec = *precision as usize)
                }
            },
            _ => n.to_string(),
        },
        (_, Some(CellValue::Select(Some(oid)))) => {
            if let ColumnPropertyConfig::Select { options } = &column.property_config {
                options
                    .iter()
                    .find(|o| &o.id == oid)
                    .map(|o| o.label.clone())
                    .unwrap_or_default()
            } else {
                String::new()
            }
        }
        (_, Some(CellValue::Select(None))) => String::new(),
        (_, Some(CellValue::MultiSelect(oids))) => {
            if let ColumnPropertyConfig::MultiSelect { options } = &column.property_config {
                oids.iter()
                    .filter_map(|id| options.iter().find(|o| &o.id == id).map(|o| o.label.clone()))
                    .collect::<Vec<_>>()
                    .join(", ")
            } else {
                String::new()
            }
        }
        (_, Some(CellValue::Date(Some(d)))) => d.format("%Y-%m-%d").to_string(),
        (_, Some(CellValue::Date(None))) => String::new(),
        (
            _,
            Some(CellValue::Time {
                time_of_day: Some(t),
                ..
            }),
        ) => t.format("%H:%M").to_string(),
        (_, Some(CellValue::Time { .. })) => String::new(),
        (_, Some(CellValue::Checkbox(b))) => {
            if is_markdown {
                if *b { "[x]".into() } else { "[ ]".into() }
            } else if *b {
                "true".into()
            } else {
                "false".into()
            }
        }
        (_, Some(CellValue::Checklist(items))) => items
            .iter()
            .map(|i| {
                if i.completed {
                    format!("[x] {}", i.item)
                } else {
                    format!("[ ] {}", i.item)
                }
            })
            .collect::<Vec<_>>()
            .join("; "),
        (_, Some(CellValue::Files(files))) => files
            .iter()
            .map(|f| {
                if !f.filename.trim().is_empty() {
                    f.filename.clone()
                } else {
                    f.storage_url.clone()
                }
            })
            .collect::<Vec<_>>()
            .join(", "),
        (_, Some(CellValue::Person(uids))) => uids
            .iter()
            .map(|uid| {
                user_display_map
                    .get(uid)
                    .cloned()
                    .unwrap_or_else(|| uid.to_string())
            })
            .collect::<Vec<_>>()
            .join(", "),
        (_, Some(CellValue::LastModified(dt))) => dt.format("%Y-%m-%d %H:%M:%S UTC").to_string(),
    }
}

/// Escapes a single CSV field per RFC 4180 (wrapping in `"` and doubling internal `"` as `""`
/// whenever the field contains commas, quotes, or newlines).
pub fn escape_csv_field(raw: &str) -> String {
    if raw.contains(',') || raw.contains('"') || raw.contains('\n') || raw.contains('\r') {
        let escaped = raw.replace('"', "\"\"");
        format!("\"{escaped}\"")
    } else {
        raw.to_string()
    }
}

/// Builds a complete RFC 4180 CSV document prepended with UTF-8 BOM (`\u{FEFF}`) for Excel/Calc compatibility.
pub fn generate_grid_csv(
    columns: &[GridColumn],
    rows: &[GridRow],
    user_display_map: &HashMap<Uuid, String>,
) -> String {
    let visible_cols: Vec<&GridColumn> = columns.iter().filter(|c| !c.is_hidden).collect();
    let mut out = String::from("\u{FEFF}");

    // Header row
    let header_line: Vec<String> = visible_cols
        .iter()
        .map(|c| escape_csv_field(&c.name))
        .collect();
    out.push_str(&header_line.join(","));
    out.push_str("\r\n");

    // Data rows
    for row in rows {
        let fields: Vec<String> = visible_cols
            .iter()
            .map(|col| {
                let text = format_cell_for_export(
                    row.cells.get(&col.id),
                    col,
                    user_display_map,
                    row.last_modified,
                    false,
                );
                escape_csv_field(&text)
            })
            .collect();
        out.push_str(&fields.join(","));
        out.push_str("\r\n");
    }

    out
}

/// Escapes a Markdown table cell (escaping literal `|` as `\|` and converting newlines to `<br>`).
pub fn escape_markdown_cell(raw: &str) -> String {
    raw.replace('|', "\\|")
        .replace("\r\n", "<br>")
        .replace('\n', "<br>")
}

/// Builds a GitHub-Flavored Markdown (`.md`) document with `# {Grid Title}`, timestamp metadata, and a pipe table.
pub fn generate_grid_markdown(
    grid_title: &str,
    space_name: &str,
    columns: &[GridColumn],
    rows: &[GridRow],
    user_display_map: &HashMap<Uuid, String>,
    exported_at: DateTime<Utc>,
) -> String {
    let visible_cols: Vec<&GridColumn> = columns.iter().filter(|c| !c.is_hidden).collect();
    let mut out = String::new();

    out.push_str(&format!("# {}\n\n", grid_title.trim()));
    out.push_str(&format!(
        "> **Workspace:** {} · **Exported:** {} · **Total Rows:** {}\n\n",
        space_name.trim(),
        exported_at.format("%Y-%m-%d %H:%M:%S UTC"),
        rows.len()
    ));

    // Table Header
    out.push_str("| # |");
    for col in &visible_cols {
        out.push(' ');
        out.push_str(&escape_markdown_cell(&col.name));
        out.push_str(" |");
    }
    out.push('\n');

    // Alignment Separator
    out.push_str("| --- |");
    for col in &visible_cols {
        if col.property_type == ColumnPropertyType::Number {
            out.push_str(" ---: |");
        } else {
            out.push_str(" --- |");
        }
    }
    out.push('\n');

    // Data Rows
    for (idx, row) in rows.iter().enumerate() {
        out.push_str(&format!("| {} |", idx + 1));
        for col in &visible_cols {
            let text = format_cell_for_export(
                row.cells.get(&col.id),
                col,
                user_display_map,
                row.last_modified,
                true,
            );
            out.push(' ');
            out.push_str(&escape_markdown_cell(&text));
            out.push_str(" |");
        }
        out.push('\n');
    }

    out
}

fn escape_html(raw: &str) -> String {
    raw.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

fn tag_color_pdf_hex(color: TagColor) -> (&'static str, &'static str, &'static str) {
    match color {
        TagColor::Blue => ("#dbeafe", "#1d4ed8", "#93c5fd"),
        TagColor::Cyan => ("#cffafe", "#0e7490", "#67e8f9"),
        TagColor::Teal => ("#ccfbf1", "#0f766e", "#5eead4"),
        TagColor::Emerald | TagColor::Green => ("#d1fae5", "#047857", "#6ee7b7"),
        TagColor::Amber => ("#fef3c7", "#b45309", "#fcd34d"),
        TagColor::Orange => ("#ffedd5", "#c2410c", "#fdba74"),
        TagColor::Red | TagColor::Rose => ("#ffe4e6", "#be123c", "#fda4af"),
        TagColor::Pink => ("#fce7f3", "#be185d", "#f9a8d4"),
        TagColor::Purple | TagColor::Violet => ("#f3e8ff", "#6d28d9", "#d8b4fe"),
        TagColor::Indigo => ("#e0e7ff", "#4338ca", "#a5b4fc"),
        TagColor::Slate | TagColor::Gray => ("#f1f5f9", "#334155", "#cbd5e1"),
    }
}

/// Generates a self-contained Executive A3 Landscape HTML/PDF print document with
/// `@page { size: A3 landscape; margin: 10mm; }`, exact color printing, branded PyraOcean / Pyramakerz
/// header, dark-slate table headers (`#0f172a`), colored tag badges, and currency highlighting.
pub fn generate_grid_a3_pdf_html(
    grid_title: &str,
    space_name: &str,
    columns: &[GridColumn],
    rows: &[GridRow],
    user_display_map: &HashMap<Uuid, String>,
    exported_at: DateTime<Utc>,
) -> String {
    let visible_cols: Vec<&GridColumn> = columns.iter().filter(|c| !c.is_hidden).collect();
    let ts_str = exported_at.format("%Y-%m-%d %H:%M:%S UTC").to_string();

    let mut html = String::with_capacity(16384);
    html.push_str(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8" />
<title>"#,
    );
    html.push_str(&escape_html(grid_title));
    html.push_str(
        r#" — PyraOcean Export</title>
<style>
  @page {
    size: A3 landscape;
    margin: 10mm;
  }
  * {
    box-sizing: border-box;
    -webkit-print-color-adjust: exact !important;
    print-color-adjust: exact !important;
  }
  body {
    margin: 0;
    padding: 0;
    font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, Inter, sans-serif;
    color: #0f172a;
    background: #ffffff;
    font-size: 11px;
  }
  .exec-header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 14px 18px;
    margin-bottom: 14px;
    border-radius: 10px;
    background: #0f172a;
    color: #f8fafc;
    border-bottom: 3px solid #f97316;
  }
  .brand-title {
    font-size: 18px;
    font-weight: 800;
    letter-spacing: -0.02em;
  }
  .brand-title .accent {
    color: #06b6d4;
  }
  .brand-sub {
    font-size: 11px;
    color: #94a3b8;
    margin-top: 3px;
  }
  .meta-right {
    text-align: right;
    font-size: 11px;
    color: #cbd5e1;
    line-height: 1.5;
  }
  .meta-badge {
    display: inline-block;
    padding: 2px 8px;
    border-radius: 999px;
    background: #f97316;
    color: #ffffff;
    font-weight: 700;
    font-size: 10px;
    text-transform: uppercase;
  }
  table {
    width: 100%;
    border-collapse: collapse;
    table-layout: auto;
  }
  thead th {
    background: #0f172a;
    color: #f8fafc;
    font-weight: 700;
    font-size: 10.5px;
    text-transform: uppercase;
    letter-spacing: 0.04em;
    padding: 8px 10px;
    border: 1px solid #1e293b;
    text-align: left;
  }
  tbody tr {
    page-break-inside: avoid;
    break-inside: avoid;
  }
  tbody tr:nth-child(even) {
    background: #f8fafc;
  }
  tbody td {
    padding: 7px 10px;
    border: 1px solid #cbd5e1;
    vertical-align: top;
    color: #1e293b;
    line-height: 1.4;
  }
  .idx-cell {
    width: 36px;
    text-align: center;
    font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
    color: #64748b;
    font-size: 10px;
  }
  .num-cell {
    text-align: right;
    font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
    font-weight: 700;
    color: #047857;
    white-space: nowrap;
  }
  .pill {
    display: inline-block;
    padding: 2px 8px;
    margin: 1px 3px 1px 0;
    border-radius: 999px;
    font-size: 10px;
    font-weight: 600;
    white-space: nowrap;
  }
  .chk-yes {
    display: inline-block;
    padding: 1px 6px;
    border-radius: 4px;
    background: #dcfce7;
    color: #15803d;
    border: 1px solid #86efac;
    font-weight: 700;
    font-size: 10px;
  }
  .chk-no {
    display: inline-block;
    padding: 1px 6px;
    border-radius: 4px;
    background: #f1f5f9;
    color: #64748b;
    border: 1px solid #cbd5e1;
    font-size: 10px;
  }
  .cl-item {
    font-size: 10px;
    margin-bottom: 2px;
  }
  .cl-done {
    color: #64748b;
    text-decoration: line-through;
  }
</style>
</head>
<body>
  <div class="exec-header">
    <div>
      <div class="brand-title">Pyra<span class="accent">Ocean</span> — "#,
    );
    html.push_str(&escape_html(grid_title));
    html.push_str(
        r#"</div>
      <div class="brand-sub">Workspace: <strong>"#,
    );
    html.push_str(&escape_html(space_name));
    html.push_str(
        r#"</strong> · Powered by Pyramakerz</div>
    </div>
    <div class="meta-right">
      <div><span class="meta-badge">A3 Landscape Executive Report</span></div>
      <div>Exported: "#,
    );
    html.push_str(&escape_html(&ts_str));
    html.push_str(&format!(
        " · {} Columns · {} Rows</div>\n    </div>\n  </div>\n  <table>\n    <thead>\n      <tr>\n        <th class=\"idx-cell\">#</th>\n",
        visible_cols.len(),
        rows.len()
    ));

    for col in &visible_cols {
        html.push_str(&format!("        <th>{}</th>\n", escape_html(&col.name)));
    }
    html.push_str("      </tr>\n    </thead>\n    <tbody>\n");

    for (idx, row) in rows.iter().enumerate() {
        html.push_str(&format!(
            "      <tr>\n        <td class=\"idx-cell\">{}</td>\n",
            idx + 1
        ));

        for col in &visible_cols {
            let cell = row.cells.get(&col.id);
            match (&col.property_type, cell) {
                (ColumnPropertyType::Number, Some(CellValue::Number(_))) => {
                    let formatted = format_cell_for_export(
                        cell,
                        col,
                        user_display_map,
                        row.last_modified,
                        false,
                    );
                    html.push_str(&format!(
                        "        <td class=\"num-cell\">{}</td>\n",
                        escape_html(&formatted)
                    ));
                }
                (ColumnPropertyType::Select, Some(CellValue::Select(Some(oid)))) => {
                    let rendered =
                        if let ColumnPropertyConfig::Select { options } = &col.property_config {
                            if let Some(opt) = options.iter().find(|o| &o.id == oid) {
                                let (bg, fg, bd) = tag_color_pdf_hex(opt.color);
                                format!(
                                    "<span class=\"pill\" style=\"background:{bg};color:{fg};border:1px solid {bd};\">&bull; {}</span>",
                                    escape_html(&opt.label)
                                )
                            } else {
                                String::new()
                            }
                        } else {
                            String::new()
                        };
                    html.push_str(&format!("        <td>{rendered}</td>\n"));
                }
                (ColumnPropertyType::MultiSelect, Some(CellValue::MultiSelect(oids))) => {
                    let mut badges = String::new();
                    if let ColumnPropertyConfig::MultiSelect { options } = &col.property_config {
                        for id in oids {
                            if let Some(opt) = options.iter().find(|o| &o.id == id) {
                                let (bg, fg, bd) = tag_color_pdf_hex(opt.color);
                                badges.push_str(&format!(
                                    "<span class=\"pill\" style=\"background:{bg};color:{fg};border:1px solid {bd};\">{}</span>",
                                    escape_html(&opt.label)
                                ));
                            }
                        }
                    }
                    html.push_str(&format!("        <td>{badges}</td>\n"));
                }
                (ColumnPropertyType::Checkbox, Some(CellValue::Checkbox(b))) => {
                    if *b {
                        html.push_str("        <td><span class=\"chk-yes\">☑ True</span></td>\n");
                    } else {
                        html.push_str("        <td><span class=\"chk-no\">☐ False</span></td>\n");
                    }
                }
                (ColumnPropertyType::Checkbox, None) => {
                    html.push_str("        <td><span class=\"chk-no\">☐ False</span></td>\n");
                }
                (ColumnPropertyType::Checklist, Some(CellValue::Checklist(items))) => {
                    let mut cl_html = String::new();
                    for item in items {
                        if item.completed {
                            cl_html.push_str(&format!(
                                "<div class=\"cl-item cl-done\">☑ {}</div>",
                                escape_html(&item.item)
                            ));
                        } else {
                            cl_html.push_str(&format!(
                                "<div class=\"cl-item\">☐ {}</div>",
                                escape_html(&item.item)
                            ));
                        }
                    }
                    html.push_str(&format!("        <td>{cl_html}</td>\n"));
                }
                _ => {
                    let plain = format_cell_for_export(
                        cell,
                        col,
                        user_display_map,
                        row.last_modified,
                        false,
                    );
                    // Highlight Text cells that start with currency symbols (e.g. "EGP 1,000" or "$ 75.00")
                    let trimmed = plain.trim();
                    let is_currency_text = (trimmed.starts_with("EGP ")
                        || trimmed.starts_with("USD ")
                        || trimmed.starts_with("$ ")
                        || trimmed.starts_with("€ "))
                        && trimmed.chars().any(|c| c.is_ascii_digit());
                    if is_currency_text {
                        html.push_str(&format!(
                            "        <td class=\"num-cell\">{}</td>\n",
                            escape_html(trimmed)
                        ));
                    } else {
                        html.push_str(&format!(
                            "        <td>{}</td>\n",
                            escape_html(&plain).replace('\n', "<br/>")
                        ));
                    }
                }
            }
        }

        html.push_str("      </tr>\n");
    }

    html.push_str("    </tbody>\n  </table>\n</body>\n</html>");
    html
}
