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
    #[serde(default)]
    #[cfg_attr(feature = "ssr", sqlx(default))]
    pub totp_enabled: bool,
    #[serde(default)]
    #[cfg_attr(feature = "ssr", sqlx(default))]
    pub password_reset_requested_at: Option<DateTime<Utc>>,
}

/// Client-side authenticated user profile (`UserProfile`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserProfile {
    pub id: Uuid,
    pub email: String,
    pub display_name: String,
    pub system_role: SystemRole,
    pub is_active: bool,
    #[serde(default)]
    pub totp_enabled: bool,
    #[serde(default)]
    pub password_reset_requested_at: Option<DateTime<Utc>>,
}

impl From<User> for UserProfile {
    fn from(u: User) -> Self {
        Self {
            id: u.id,
            email: u.email,
            display_name: u.display_name,
            system_role: u.system_role,
            is_active: u.is_active,
            totp_enabled: u.totp_enabled,
            password_reset_requested_at: u.password_reset_requested_at,
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
    #[serde(default)]
    #[cfg_attr(feature = "ssr", sqlx(default))]
    pub deleted_at: Option<DateTime<Utc>>,
    #[serde(default)]
    #[cfg_attr(feature = "ssr", sqlx(default))]
    pub deleted_by: Option<Uuid>,
}

/// Soft-deleted Grid item returned by `GET /api/trash/grids`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(FromRow))]
pub struct TrashedGridItem {
    pub id: Uuid,
    pub space_id: Uuid,
    pub space_name: String,
    pub name: String,
    pub icon: Option<String>,
    pub deleted_at: DateTime<Utc>,
    pub deleted_by_email: Option<String>,
    pub days_remaining: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmptyTrashResponse {
    pub purged_count: u64,
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
    #[serde(rename = "password")]
    Password,
}

pub type PropertyType = ColumnPropertyType;

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
    Password,
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
// 4. The 12 Dynamic Cell Values & Validation
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
    /// 12. `Password`: Masked secret string (`••••••••••••`) with per-cell reveal & copy.
    Password(String),
}

pub type PropertyValue = CellValue;

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
            CellValue::Password(_) => ColumnPropertyType::Password,
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
        let actual_pt = self.property_type();
        let is_compatible_text_password = matches!(
            (actual_pt, col.property_type),
            (ColumnPropertyType::Text, ColumnPropertyType::Password)
                | (ColumnPropertyType::Password, ColumnPropertyType::Text)
        );
        if actual_pt != col.property_type && !is_compatible_text_password {
            return Err(CellValidationError::TypeMismatch {
                column_id: col.id,
                expected: col.property_type,
                actual: actual_pt,
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
    #[serde(default)]
    pub totp_code: Option<String>,
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
            app_logo_dark: "/public/logo-dark.png".into(),
            app_logo_light: "/public/logo-light.png".into(),
            powered_by: "Powered by Pyramakerz".into(),
            powered_by_logo: "/public/pyramakerz.webp".into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoginResponse {
    #[serde(default)]
    pub requires_totp: bool,
    #[serde(default)]
    pub requires_2fa: bool,
    #[serde(default)]
    pub temp_2fa_token: Option<String>,
    pub token: String,
    pub user: User,
    pub branding: BrandingMetadata,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ForgotPasswordRequest {
    pub email: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ForgotPasswordResponse {
    pub message: String,
    pub totp_enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SelfServicePasswordResetRequest {
    pub email: String,
    pub verification_code: String,
    pub new_password: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChangeOwnPasswordRequest {
    pub current_password: String,
    pub new_password: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TotpSetupResponse {
    pub secret: String,
    pub otpauth_uri: String,
    pub recovery_code: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerifyTotpRequest {
    pub code: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DisableTotpRequest {
    pub password: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdminResetPasswordRequest {
    pub new_password: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdminResetPasswordResponse {
    pub user: User,
    pub generated_recovery_code: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdminManage2faRequest {
    /// `"setup"` (provisions/enforces 2FA and returns secret + recovery code), `"verify"` (tests/activates 6-digit code), or `"reset"` (disables 2FA)
    pub action: String,
    #[serde(default)]
    pub code: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdminManage2faResponse {
    pub user: User,
    pub secret: Option<String>,
    pub otpauth_uri: Option<String>,
    pub recovery_code: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "ssr", derive(FromRow))]
pub struct GoogleAuthSettings {
    pub google_oauth_enabled: bool,
    pub google_client_id: String,
    pub google_client_secret: String,
    pub google_allowed_domains: String,
    pub require_2fa_after_google: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct PublicAuthConfig {
    pub google_oauth_enabled: bool,
    pub google_client_id: String,
    #[serde(default)]
    pub require_2fa_after_google: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GoogleLoginRequest {
    pub credential: String,
    #[serde(default)]
    pub email: Option<String>,
    #[serde(default)]
    pub totp_code: Option<String>,
    #[serde(default)]
    pub temp_2fa_token: Option<String>,
}

// ============================================================================
// Phase 18: Local AI Copilot (`PyraAI` / Ollama) Models & Task Execution DTOs
// ============================================================================

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(FromRow))]
pub struct AiSettings {
    pub ai_enabled: bool,
    pub ollama_base_url: String,
    pub model_name: String,
    pub system_prompt: String,
}

impl Default for AiSettings {
    fn default() -> Self {
        Self {
            ai_enabled: true,
            ollama_base_url: "http://10.10.10.4:11434".into(),
            model_name: "qwen2.5:1.5b".into(),
            system_prompt: "You are PyraAI, the intelligent workspace assistant and task executor for PyraOcean.".into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AiAttachment {
    pub filename: String,
    pub mime_type: String,
    pub data_base64: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AiNavigationTarget {
    pub space_id: Uuid,
    pub space_name: String,
    pub grid_id: Uuid,
    pub grid_name: String,
    #[serde(default)]
    pub grid_icon: Option<String>,
    #[serde(default)]
    pub row_id: Option<Uuid>,
    #[serde(default)]
    pub row_number: Option<usize>,
    pub primary_label: String,
    pub matched_preview: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AiChatMessage {
    pub role: String,
    pub content: String,
    #[serde(default)]
    pub attachments: Vec<AiAttachment>,
    #[serde(default)]
    pub navigation_targets: Vec<AiNavigationTarget>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AiExecutedAction {
    pub action_type: String,
    pub summary: String,
    #[serde(default)]
    pub space_id: Option<Uuid>,
    #[serde(default)]
    pub grid_id: Option<Uuid>,
    #[serde(default)]
    pub row_id: Option<Uuid>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AiCopilotRequest {
    pub prompt: String,
    #[serde(default = "default_ai_mode")]
    pub mode: String,
    #[serde(default)]
    pub space_id: Option<Uuid>,
    #[serde(default)]
    pub grid_id: Option<Uuid>,
    #[serde(default)]
    pub history: Vec<AiChatMessage>,
    #[serde(default)]
    pub attachments: Vec<AiAttachment>,
}

fn default_ai_mode() -> String {
    "ask".into()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AiCopilotResponse {
    pub reply: String,
    pub model_used: String,
    #[serde(default)]
    pub executed_actions: Vec<AiExecutedAction>,
    #[serde(default)]
    pub matched_rows_count: usize,
    #[serde(default)]
    pub navigated_space_id: Option<Uuid>,
    #[serde(default)]
    pub navigated_grid_id: Option<Uuid>,
    #[serde(default)]
    pub navigated_row_id: Option<Uuid>,
    #[serde(default)]
    pub navigation_targets: Vec<AiNavigationTarget>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(FromRow))]
pub struct GridRowComment {
    pub id: Uuid,
    pub grid_id: Uuid,
    pub row_id: Uuid,
    pub author_id: Option<Uuid>,
    pub author_email: String,
    pub content: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateRowCommentRequest {
    pub content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(FromRow))]
pub struct UserBackupRecord {
    pub id: Uuid,
    pub email: String,
    pub display_name: String,
    pub password_hash: String,
    pub system_role: SystemRole,
    pub is_active: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    #[serde(default)]
    #[cfg_attr(feature = "ssr", sqlx(default))]
    pub totp_secret: Option<String>,
    #[serde(default)]
    #[cfg_attr(feature = "ssr", sqlx(default))]
    pub totp_enabled: bool,
    #[serde(default)]
    #[cfg_attr(feature = "ssr", sqlx(default))]
    pub recovery_code_hash: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceBackupPayload {
    pub version: u32,
    pub exported_at: DateTime<Utc>,
    pub exported_by_email: String,
    #[serde(default)]
    pub users: Vec<UserBackupRecord>,
    #[serde(default)]
    pub spaces: Vec<Space>,
    #[serde(default)]
    pub space_members: Vec<SpaceMember>,
    #[serde(default)]
    pub grids: Vec<PageGrid>,
    #[serde(default)]
    pub columns: Vec<GridColumn>,
    #[serde(default)]
    pub rows: Vec<GridRow>,
    #[serde(default)]
    pub comments: Vec<GridRowComment>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RestoreBackupResponse {
    pub users_restored: usize,
    pub spaces_restored: usize,
    pub grids_restored: usize,
    pub columns_restored: usize,
    pub rows_restored: usize,
    pub comments_restored: usize,
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

/// Request payload for `POST /api/grids/{grid_id}/duplicate`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DuplicateGridRequest {
    pub target_space_id: Option<Uuid>,
    pub new_name: Option<String>,
    #[serde(default = "default_true_flag")]
    pub include_rows: bool,
}

fn default_true_flag() -> bool {
    true
}

/// Request payload for `PATCH /api/grids/{grid_id}/move`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MoveGridRequest {
    pub target_space_id: Uuid,
}

/// Request payload for `POST /api/grids/{source_grid_id}/rows/copy-to/{target_grid_id}`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CopyRowsToGridRequest {
    pub row_ids: Vec<Uuid>,
    #[serde(default = "default_true_flag")]
    pub auto_create_missing_columns: bool,
}

/// Response returned by `POST /api/grids/{source_grid_id}/rows/copy-to/{target_grid_id}`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CopyRowsToGridResponse {
    pub copied_count: usize,
    pub created_columns_count: usize,
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
        (_, Some(CellValue::Password(s))) => s.clone(),
    }
}

// ============================================================================
// 6.5 Smart Currency & Number Parser, Column Calculations & Group-By Engine
// ============================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CurrencyKind {
    Egp,
    Usd,
    Percent,
    Plain,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CalcMode {
    None,
    CountAll,
    CountValues,
    CountEmpty,
    CountChecked,
    PercentChecked,
    Sum,
    Average,
    Min,
    Max,
}

impl CalcMode {
    pub fn label(&self) -> &'static str {
        match self {
            CalcMode::None => "Calculate ▾",
            CalcMode::CountAll => "Count All",
            CalcMode::CountValues => "Count Values",
            CalcMode::CountEmpty => "Count Empty",
            CalcMode::CountChecked => "Count Checked",
            CalcMode::PercentChecked => "% Checked",
            CalcMode::Sum => "Sum",
            CalcMode::Average => "Avg",
            CalcMode::Min => "Min",
            CalcMode::Max => "Max",
        }
    }

    pub fn short_prefix(&self) -> &'static str {
        match self {
            CalcMode::None => "",
            CalcMode::CountAll => "ALL",
            CalcMode::CountValues => "VALUES",
            CalcMode::CountEmpty => "EMPTY",
            CalcMode::CountChecked => "CHECKED",
            CalcMode::PercentChecked => "% DONE",
            CalcMode::Sum => "SUM",
            CalcMode::Average => "AVG",
            CalcMode::Min => "MIN",
            CalcMode::Max => "MAX",
        }
    }
}

/// Structured output of a column calculation (`SUM`, `AVG`, `MIN`, `MAX`, `COUNT`)
/// supporting simultaneous Dual-Currency (`EGP` + `USD`) accumulators.
#[derive(Debug, Clone, PartialEq)]
pub struct ColumnCalcSummary {
    pub mode: CalcMode,
    pub egp_value: Option<f64>,
    pub usd_value: Option<f64>,
    pub percent_value: Option<f64>,
    pub plain_value: Option<f64>,
    pub text_summary: Option<String>,
}

impl ColumnCalcSummary {
    pub fn is_empty(&self) -> bool {
        self.egp_value.is_none()
            && self.usd_value.is_none()
            && self.percent_value.is_none()
            && self.plain_value.is_none()
            && self.text_summary.is_none()
    }
}

/// Formats a floating-point number with thousands separators (e.g. `90410.0` -> `"90,410.00"`).
pub fn format_number_with_commas(val: f64, precision: usize) -> String {
    if !val.is_finite() {
        return "0.00".to_string();
    }
    let sign = if val < 0.0 { "-" } else { "" };
    let abs_val = val.abs();
    let formatted = format!("{:.prec$}", abs_val, prec = precision);
    let mut parts = formatted.splitn(2, '.');
    let int_part = parts.next().unwrap_or("0");
    let dec_part = parts.next();

    let mut with_commas = String::with_capacity(int_part.len() + int_part.len() / 3 + 4);
    let chars: Vec<char> = int_part.chars().collect();
    for (i, ch) in chars.iter().enumerate() {
        if i > 0 && (chars.len() - i) % 3 == 0 {
            with_commas.push(',');
        }
        with_commas.push(*ch);
    }

    match dec_part {
        Some(d) if precision > 0 => format!("{sign}{with_commas}.{d}"),
        _ => format!("{sign}{with_commas}"),
    }
}

/// Parses a cell string into `Option<(CurrencyKind, f64)>`:
/// - Detects `"EGP"` or `"LE"` (case-insensitive) -> `CurrencyKind::Egp` (e.g. `"EGP 10,940.00"` -> `10940.0`)
/// - Detects `"$"` or `"USD"` (case-insensitive) -> `CurrencyKind::Usd` (e.g. `"$ 692.00"` -> `692.0`)
/// - Detects `"%"` -> `CurrencyKind::Percent` (e.g. `"33.33%"` -> `33.33`)
/// - Strips commas `,` and whitespace before parsing `f64`, ignoring `"-"`, `"Null"`, or empty strings.
pub fn parse_numeric_and_currency(raw: &str) -> Option<(CurrencyKind, f64)> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    let lower = trimmed.to_lowercase();
    if matches!(
        lower.as_str(),
        "-" | "—" | "–" | "null" | "none" | "n/a" | "na" | "tbd"
    ) {
        return None;
    }

    let kind = if lower.contains("egp")
        || lower.starts_with("le ")
        || lower.ends_with(" le")
        || lower.starts_with("l.e")
    {
        CurrencyKind::Egp
    } else if lower.contains('$') || lower.contains("usd") {
        CurrencyKind::Usd
    } else if lower.contains('%') {
        CurrencyKind::Percent
    } else {
        CurrencyKind::Plain
    };

    // Strip currency symbols/codes, commas, percent signs, and whitespace
    let mut cleaned = lower
        .replace("egp", "")
        .replace("usd", "")
        .replace("l.e.", "")
        .replace("l.e", "")
        .replace('$', "")
        .replace('%', "")
        .replace(',', "");
    if let Some(rest) = cleaned.trim().strip_prefix("le") {
        cleaned = rest.to_string();
    } else if let Some(rest) = cleaned.trim().strip_suffix("le") {
        cleaned = rest.to_string();
    }

    let num_str = cleaned.trim();
    if num_str.is_empty() {
        return None;
    }

    let parsed = num_str.parse::<f64>().ok()?;
    if !parsed.is_finite() {
        return None;
    }

    Some((kind, parsed))
}

/// Extracts `(CurrencyKind, f64)` from a `CellValue` and its `GridColumn` schema.
pub fn extract_cell_numeric(cell: Option<&CellValue>, column: &GridColumn) -> Option<(CurrencyKind, f64)> {
    match cell? {
        CellValue::Number(n) if n.is_finite() => {
            let kind = match &column.property_config {
                ColumnPropertyConfig::Number {
                    format: NumberFormat::Currency { symbol, .. },
                } => {
                    let s = symbol.trim().to_lowercase();
                    if s.contains("egp") || s == "le" || s == "l.e" {
                        CurrencyKind::Egp
                    } else if s.contains('$') || s.contains("usd") {
                        CurrencyKind::Usd
                    } else {
                        CurrencyKind::Plain
                    }
                }
                ColumnPropertyConfig::Number {
                    format: NumberFormat::Percentage { .. },
                } => CurrencyKind::Percent,
                _ => CurrencyKind::Plain,
            };
            Some((kind, *n))
        }
        CellValue::Text(s) => parse_numeric_and_currency(s),
        _ => None,
    }
}

/// Checks whether a cell is considered non-empty for `CountValues` / `CountEmpty`.
pub fn is_cell_non_empty(cell: Option<&CellValue>, prop_type: ColumnPropertyType) -> bool {
    if prop_type == ColumnPropertyType::LastModified {
        return true;
    }
    match cell {
        None => false,
        Some(CellValue::Text(s)) | Some(CellValue::Password(s)) => {
            let t = s.trim();
            !t.is_empty() && !matches!(t.to_lowercase().as_str(), "-" | "—" | "null")
        }
        Some(CellValue::Number(n)) => n.is_finite(),
        Some(CellValue::Select(opt)) => opt.is_some(),
        Some(CellValue::MultiSelect(ids)) => !ids.is_empty(),
        Some(CellValue::Date(d)) => d.is_some(),
        Some(CellValue::Time {
            time_of_day,
            duration_seconds,
        }) => time_of_day.is_some() || duration_seconds.is_some(),
        Some(CellValue::Files(files)) => !files.is_empty(),
        Some(CellValue::Checkbox(_)) => true,
        Some(CellValue::Checklist(items)) => !items.is_empty(),
        Some(CellValue::LastModified(_)) => true,
        Some(CellValue::Person(uids)) => !uids.is_empty(),
    }
}

/// Detects whether a column across a slice of rows contains numeric or currency values
/// (so Group Subtotal rows and default footers automatically summarize numeric/cost columns).
pub fn column_has_numeric_or_currency_values(column: &GridColumn, rows: &[GridRow]) -> bool {
    if column.property_type == ColumnPropertyType::Number {
        return true;
    }
    if column.property_type != ColumnPropertyType::Text {
        return false;
    }
    let mut numeric_hits = 0usize;
    let mut non_empty_hits = 0usize;
    for r in rows {
        if let Some(CellValue::Text(s)) = r.cells.get(&column.id) {
            let t = s.trim();
            if t.is_empty() || matches!(t.to_lowercase().as_str(), "-" | "—" | "null" | "n/a") {
                continue;
            }
            non_empty_hits += 1;
            if parse_numeric_and_currency(t).is_some() {
                numeric_hits += 1;
            }
        }
    }
    numeric_hits > 0 && numeric_hits * 2 >= non_empty_hits.max(1)
}

/// Computes `ColumnCalcSummary` for a column over the given slice of `GridRow`s.
pub fn compute_column_calculation(
    column: &GridColumn,
    rows: &[GridRow],
    mode: CalcMode,
) -> ColumnCalcSummary {
    match mode {
        CalcMode::None => ColumnCalcSummary {
            mode,
            egp_value: None,
            usd_value: None,
            percent_value: None,
            plain_value: None,
            text_summary: None,
        },
        CalcMode::CountAll => ColumnCalcSummary {
            mode,
            egp_value: None,
            usd_value: None,
            percent_value: None,
            plain_value: None,
            text_summary: Some(format!("{}", rows.len())),
        },
        CalcMode::CountValues => {
            let count = rows
                .iter()
                .filter(|r| is_cell_non_empty(r.cells.get(&column.id), column.property_type))
                .count();
            ColumnCalcSummary {
                mode,
                egp_value: None,
                usd_value: None,
                percent_value: None,
                plain_value: None,
                text_summary: Some(format!("{count}")),
            }
        }
        CalcMode::CountEmpty => {
            let count = rows
                .iter()
                .filter(|r| !is_cell_non_empty(r.cells.get(&column.id), column.property_type))
                .count();
            ColumnCalcSummary {
                mode,
                egp_value: None,
                usd_value: None,
                percent_value: None,
                plain_value: None,
                text_summary: Some(format!("{count}")),
            }
        }
        CalcMode::CountChecked | CalcMode::PercentChecked => {
            let mut checked = 0usize;
            let mut total = 0usize;
            for r in rows {
                match r.cells.get(&column.id) {
                    Some(CellValue::Checkbox(b)) => {
                        total += 1;
                        if *b {
                            checked += 1;
                        }
                    }
                    Some(CellValue::Checklist(items)) => {
                        total += items.len();
                        checked += items.iter().filter(|i| i.completed).count();
                    }
                    None if column.property_type == ColumnPropertyType::Checkbox => {
                        total += 1;
                    }
                    _ => {}
                }
            }
            let text = if mode == CalcMode::CountChecked {
                format!("{checked} / {total}")
            } else if total > 0 {
                format!("{:.1}%", (checked as f64 / total as f64) * 100.0)
            } else {
                "0.0%".to_string()
            };
            ColumnCalcSummary {
                mode,
                egp_value: None,
                usd_value: None,
                percent_value: None,
                plain_value: None,
                text_summary: Some(text),
            }
        }
        CalcMode::Sum | CalcMode::Average | CalcMode::Min | CalcMode::Max => {
            let mut egp_vals: Vec<f64> = Vec::new();
            let mut usd_vals: Vec<f64> = Vec::new();
            let mut pct_vals: Vec<f64> = Vec::new();
            let mut plain_vals: Vec<f64> = Vec::new();

            for r in rows {
                if let Some((kind, val)) = extract_cell_numeric(r.cells.get(&column.id), column) {
                    match kind {
                        CurrencyKind::Egp => egp_vals.push(val),
                        CurrencyKind::Usd => usd_vals.push(val),
                        CurrencyKind::Percent => pct_vals.push(val),
                        CurrencyKind::Plain => plain_vals.push(val),
                    }
                }
            }

            // If a column has EGP or USD values alongside plain numbers (e.g. a cell where the user omitted the prefix),
            // keep plain numbers in their own bucket unless the column is exclusively EGP or USD.
            if !egp_vals.is_empty() && usd_vals.is_empty() && !plain_vals.is_empty() {
                egp_vals.extend(plain_vals.drain(..));
            } else if !usd_vals.is_empty() && egp_vals.is_empty() && !plain_vals.is_empty() {
                usd_vals.extend(plain_vals.drain(..));
            }

            let reduce_slice = |vals: &[f64]| -> Option<f64> {
                if vals.is_empty() {
                    return None;
                }
                match mode {
                    CalcMode::Sum => Some(vals.iter().sum()),
                    CalcMode::Average => Some(vals.iter().sum::<f64>() / (vals.len() as f64)),
                    CalcMode::Min => vals.iter().copied().reduce(f64::min),
                    CalcMode::Max => vals.iter().copied().reduce(f64::max),
                    _ => None,
                }
            };

            ColumnCalcSummary {
                mode,
                egp_value: reduce_slice(&egp_vals),
                usd_value: reduce_slice(&usd_vals),
                percent_value: reduce_slice(&pct_vals),
                plain_value: reduce_slice(&plain_vals),
                text_summary: None,
            }
        }
    }
}

/// Represents a single grouped section when `Group By` is active on a Grid.
#[derive(Debug, Clone)]
pub struct GridRowGroup {
    pub key: String,
    pub label: String,
    pub color: Option<TagColor>,
    pub option_id: Option<Uuid>,
    pub rows: Vec<GridRow>,
}

/// Partitions visible rows into ordered `GridRowGroup`s for `Select`, `MultiSelect`, `Text`, `Person`, or `Checkbox` columns.
/// For `MultiSelect` columns (e.g. a row tagged with `["Software", "B2C", "Innovation"]`), places the row into
/// each matching department/tag group so per-department subtotal breakdowns match financial allocation tables.
pub fn partition_rows_by_group(
    rows: &[GridRow],
    group_col: &GridColumn,
    user_display_map: &HashMap<Uuid, String>,
) -> Vec<GridRowGroup> {
    match (&group_col.property_type, &group_col.property_config) {
        (ColumnPropertyType::Select, ColumnPropertyConfig::Select { options }) => {
            let mut groups: Vec<GridRowGroup> = options
                .iter()
                .map(|opt| GridRowGroup {
                    key: opt.id.to_string(),
                    label: opt.label.clone(),
                    color: Some(opt.color),
                    option_id: Some(opt.id),
                    rows: Vec::new(),
                })
                .collect();
            let mut uncategorized: Vec<GridRow> = Vec::new();

            for r in rows {
                if let Some(CellValue::Select(Some(oid))) = r.cells.get(&group_col.id) {
                    if let Some(grp) = groups.iter_mut().find(|g| g.option_id == Some(*oid)) {
                        grp.rows.push(r.clone());
                        continue;
                    }
                }
                uncategorized.push(r.clone());
            }

            groups.retain(|g| !g.rows.is_empty());
            if !uncategorized.is_empty() {
                groups.push(GridRowGroup {
                    key: "__uncategorized__".to_string(),
                    label: "Uncategorized".to_string(),
                    color: Some(TagColor::Slate),
                    option_id: None,
                    rows: uncategorized,
                });
            }
            groups
        }
        (ColumnPropertyType::MultiSelect, ColumnPropertyConfig::MultiSelect { options }) => {
            let mut groups: Vec<GridRowGroup> = options
                .iter()
                .map(|opt| GridRowGroup {
                    key: opt.id.to_string(),
                    label: opt.label.clone(),
                    color: Some(opt.color),
                    option_id: Some(opt.id),
                    rows: Vec::new(),
                })
                .collect();
            let mut uncategorized: Vec<GridRow> = Vec::new();

            for r in rows {
                if let Some(CellValue::MultiSelect(oids)) = r.cells.get(&group_col.id) {
                    if !oids.is_empty() {
                        let mut matched_any = false;
                        for oid in oids {
                            if let Some(grp) =
                                groups.iter_mut().find(|g| g.option_id == Some(*oid))
                            {
                                grp.rows.push(r.clone());
                                matched_any = true;
                            }
                        }
                        if matched_any {
                            continue;
                        }
                    }
                }
                uncategorized.push(r.clone());
            }

            groups.retain(|g| !g.rows.is_empty());
            if !uncategorized.is_empty() {
                groups.push(GridRowGroup {
                    key: "__uncategorized__".to_string(),
                    label: "Uncategorized".to_string(),
                    color: Some(TagColor::Slate),
                    option_id: None,
                    rows: uncategorized,
                });
            }
            groups
        }
        _ => {
            let mut ordered_keys: Vec<String> = Vec::new();
            let mut map: HashMap<String, Vec<GridRow>> = HashMap::new();
            for r in rows {
                let raw_label = format_cell_for_export(
                    r.cells.get(&group_col.id),
                    group_col,
                    user_display_map,
                    r.last_modified,
                    false,
                );
                let clean = raw_label.trim();
                let label = if clean.is_empty() {
                    "Uncategorized".to_string()
                } else {
                    clean.to_string()
                };
                if !map.contains_key(&label) {
                    ordered_keys.push(label.clone());
                }
                map.entry(label).or_default().push(r.clone());
            }

            let palette = [
                TagColor::Cyan,
                TagColor::Emerald,
                TagColor::Amber,
                TagColor::Blue,
                TagColor::Purple,
                TagColor::Orange,
                TagColor::Rose,
                TagColor::Teal,
            ];
            ordered_keys
                .into_iter()
                .enumerate()
                .filter_map(|(idx, lbl)| {
                    let grp_rows = map.remove(&lbl)?;
                    let color = if lbl == "Uncategorized" {
                        TagColor::Slate
                    } else {
                        palette[idx % palette.len()]
                    };
                    Some(GridRowGroup {
                        key: lbl.clone(),
                        label: lbl,
                        color: Some(color),
                        option_id: None,
                        rows: grp_rows,
                    })
                })
                .collect()
        }
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

fn render_pdf_calc_summary(summary: &ColumnCalcSummary) -> String {
    if summary.is_empty() {
        return String::new();
    }
    let prefix = summary.mode.short_prefix();
    let prefix_html = if prefix.is_empty() || summary.mode == CalcMode::Sum {
        String::new()
    } else {
        format!(
            "<span style=\"font-size:9px;color:#64748b;font-weight:700;margin-right:4px;\">{prefix}</span>"
        )
    };

    if let Some(txt) = &summary.text_summary {
        return format!(
            "{prefix_html}<span style=\"font-family:ui-monospace,monospace;font-weight:700;color:#0f172a;\">{}</span>",
            escape_html(txt)
        );
    }

    let mut parts: Vec<String> = Vec::new();
    if let Some(egp) = summary.egp_value {
        parts.push(format!(
            "<span class=\"curr-egp\">EGP {}</span>",
            format_number_with_commas(egp, 2)
        ));
    }
    if let Some(usd) = summary.usd_value {
        parts.push(format!(
            "<span class=\"curr-usd\">$ {}</span>",
            format_number_with_commas(usd, 2)
        ));
    }
    if let Some(pct) = summary.percent_value {
        parts.push(format!(
            "<span style=\"font-family:ui-monospace,monospace;font-weight:700;color:#b45309;\">{:.2}%</span>",
            pct
        ));
    }
    if let Some(plain) = summary.plain_value {
        parts.push(format!(
            "<span style=\"font-family:ui-monospace,monospace;font-weight:700;color:#047857;\">{}</span>",
            format_number_with_commas(plain, 2)
        ));
    }

    if parts.is_empty() {
        String::new()
    } else {
        format!(
            "{prefix_html}{}",
            parts.join(" <span style=\"color:#64748b;font-weight:700;\">+</span> ")
        )
    }
}

fn render_pdf_body_row(
    html: &mut String,
    row_num: usize,
    row: &GridRow,
    visible_cols: &[&GridColumn],
    user_display_map: &HashMap<Uuid, String>,
    mask_passwords: bool,
) {
    html.push_str(&format!(
        "      <tr>\n        <td class=\"idx-cell\">{}</td>\n",
        row_num
    ));

    for col in visible_cols {
        let cell = row.cells.get(&col.id);
        match (&col.property_type, cell) {
            (ColumnPropertyType::Password, Some(CellValue::Password(s)))
            | (ColumnPropertyType::Password, Some(CellValue::Text(s))) => {
                if s.trim().is_empty() {
                    html.push_str("        <td></td>\n");
                } else if mask_passwords {
                    html.push_str(
                        "        <td><span class=\"pwd-mask\">••••••••••••</span></td>\n",
                    );
                } else {
                    html.push_str(&format!(
                        "        <td><span style=\"font-family:ui-monospace,monospace;color:#b45309;\">{}</span></td>\n",
                        escape_html(s)
                    ));
                }
            }
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
                let trimmed = plain.trim();
                if let Some((kind, _)) = parse_numeric_and_currency(trimmed) {
                    match kind {
                        CurrencyKind::Egp => {
                            html.push_str(&format!(
                                "        <td class=\"num-cell\"><span class=\"curr-egp\">{}</span></td>\n",
                                escape_html(trimmed)
                            ));
                        }
                        CurrencyKind::Usd => {
                            html.push_str(&format!(
                                "        <td class=\"num-cell\"><span class=\"curr-usd\">{}</span></td>\n",
                                escape_html(trimmed)
                            ));
                        }
                        _ => {
                            html.push_str(&format!(
                                "        <td>{}</td>\n",
                                escape_html(&plain).replace('\n', "<br/>")
                            ));
                        }
                    }
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

/// Generates a self-contained Executive A3 Landscape HTML/PDF print document with
/// `@page { size: A3 landscape; margin: 10mm; }`, exact color printing, branded PyraOcean / Pyramakerz
/// header, dark-slate table headers (`#0f172a`), colored tag badges, currency highlighting,
/// optional Group-By section banners & Department Subtotal rows, and the Grand Total footer row.
pub fn generate_grid_a3_pdf_html(
    grid_title: &str,
    space_name: &str,
    columns: &[GridColumn],
    rows: &[GridRow],
    user_display_map: &HashMap<Uuid, String>,
    exported_at: DateTime<Utc>,
) -> String {
    generate_grid_a3_pdf_html_with_options(
        grid_title,
        space_name,
        columns,
        rows,
        user_display_map,
        exported_at,
        None,
        &HashMap::new(),
        true,
    )
}

/// Generates the Executive A3 Landscape HTML/PDF print document preserving active Group-By sections,
/// per-department subtotals (`EGP` + `USD`), bottom calculation footers, and password masking preferences.
pub fn generate_grid_a3_pdf_html_with_options(
    grid_title: &str,
    space_name: &str,
    columns: &[GridColumn],
    rows: &[GridRow],
    user_display_map: &HashMap<Uuid, String>,
    exported_at: DateTime<Utc>,
    group_by_col_id: Option<Uuid>,
    column_calculations: &HashMap<Uuid, CalcMode>,
    mask_passwords: bool,
) -> String {
    let visible_cols: Vec<&GridColumn> = columns.iter().filter(|c| !c.is_hidden).collect();
    let ts_str = exported_at.format("%Y-%m-%d %H:%M:%S UTC").to_string();

    let mut html = String::with_capacity(24576);
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
  tbody tr, tfoot tr {
    page-break-inside: avoid;
    break-inside: avoid;
  }
  tbody tr:nth-child(even) {
    background: #f8fafc;
  }
  tbody td, tfoot td {
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
  .curr-egp {
    display: inline-block;
    padding: 1px 6px;
    border-radius: 4px;
    background: #ecfdf5;
    color: #047857;
    border: 1px solid #6ee7b7;
    font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
    font-weight: 700;
    font-size: 10px;
    white-space: nowrap;
  }
  .curr-usd {
    display: inline-block;
    padding: 1px 6px;
    border-radius: 4px;
    background: #ecfeff;
    color: #0e7490;
    border: 1px solid #67e8f9;
    font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
    font-weight: 700;
    font-size: 10px;
    white-space: nowrap;
  }
  .pwd-mask {
    font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
    letter-spacing: 0.14em;
    color: #64748b;
    font-size: 10px;
  }
  .group-banner td {
    background: #1e293b !important;
    color: #f8fafc !important;
    font-weight: 700;
    padding: 8px 12px;
    border: 1px solid #0f172a;
  }
  .subtotal-row td {
    background: #f1f5f9 !important;
    border-top: 2px solid #475569 !important;
    border-bottom: 2px solid #94a3b8 !important;
    font-weight: 700;
    color: #0f172a;
  }
  tfoot tr td {
    background: #0f172a !important;
    color: #f8fafc !important;
    border: 1px solid #1e293b;
    border-top: 2px solid #f97316 !important;
    font-weight: 700;
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
    <div style="display:flex;align-items:center;gap:12px;">
      <img src="/public/logo-light.png" alt="PyraOcean" style="height:40px;width:40px;object-fit:contain;border-radius:8px;border:1px solid #cbd5e1;" />
      <div>
        <div class="brand-title">PyraOcean — "#,
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

    let numeric_col_flags: HashMap<Uuid, bool> = visible_cols
        .iter()
        .map(|c| (c.id, column_has_numeric_or_currency_values(c, rows)))
        .collect();

    let maybe_group_col =
        group_by_col_id.and_then(|gid| columns.iter().find(|c| c.id == gid));

    if let Some(group_col) = maybe_group_col {
        let groups = partition_rows_by_group(rows, group_col, user_display_map);
        let total_colspan = visible_cols.len() + 1;

        for grp in &groups {
            let (bg, fg, bd) = tag_color_pdf_hex(grp.color.unwrap_or(TagColor::Cyan));
            html.push_str(&format!(
                "      <tr class=\"group-banner\">\n        <td colspan=\"{total_colspan}\"><span class=\"pill\" style=\"background:{bg};color:{fg};border:1px solid {bd};\">{}</span> <span style=\"color:#94a3b8;font-size:10px;margin-left:6px;\">({} items · Grouped by {})</span></td>\n      </tr>\n",
                escape_html(&grp.label),
                grp.rows.len(),
                escape_html(&group_col.name)
            ));

            for (idx, row) in grp.rows.iter().enumerate() {
                render_pdf_body_row(
                    &mut html,
                    idx + 1,
                    row,
                    &visible_cols,
                    user_display_map,
                    mask_passwords,
                );
            }

            // Per-Group Subtotal Row (`✓ Subtotal — {Group Name}`)
            html.push_str("      <tr class=\"subtotal-row\">\n        <td class=\"idx-cell\">∑</td>\n");
            for (c_idx, col) in visible_cols.iter().enumerate() {
                let effective_mode = column_calculations
                    .get(&col.id)
                    .copied()
                    .filter(|m| *m != CalcMode::None)
                    .unwrap_or_else(|| {
                        if numeric_col_flags.get(&col.id).copied().unwrap_or(false) {
                            CalcMode::Sum
                        } else {
                            CalcMode::None
                        }
                    });
                let summary = compute_column_calculation(col, &grp.rows, effective_mode);
                let rendered_calc = render_pdf_calc_summary(&summary);

                if c_idx == 0 && rendered_calc.is_empty() {
                    html.push_str(&format!(
                        "        <td>✓ Subtotal — {} <span style=\"color:#64748b;font-weight:600;font-size:10px;\">({} items)</span></td>\n",
                        escape_html(&grp.label),
                        grp.rows.len()
                    ));
                } else {
                    html.push_str(&format!(
                        "        <td style=\"text-align:right;\">{rendered_calc}</td>\n"
                    ));
                }
            }
            html.push_str("      </tr>\n");
        }
    } else {
        for (idx, row) in rows.iter().enumerate() {
            render_pdf_body_row(
                &mut html,
                idx + 1,
                row,
                &visible_cols,
                user_display_map,
                mask_passwords,
            );
        }
    }

    html.push_str("    </tbody>\n");

    // Render Grand Total `<tfoot>` row if any column has an active calculation or numeric/currency values
    let any_footer_calc = visible_cols.iter().any(|c| {
        column_calculations
            .get(&c.id)
            .copied()
            .filter(|m| *m != CalcMode::None)
            .is_some()
            || numeric_col_flags.get(&c.id).copied().unwrap_or(false)
    });

    if any_footer_calc && !rows.is_empty() {
        html.push_str("    <tfoot>\n      <tr>\n        <td class=\"idx-cell\" style=\"color:#f97316;\">∑</td>\n");
        for (c_idx, col) in visible_cols.iter().enumerate() {
            let effective_mode = column_calculations
                .get(&col.id)
                .copied()
                .filter(|m| *m != CalcMode::None)
                .unwrap_or_else(|| {
                    if numeric_col_flags.get(&col.id).copied().unwrap_or(false) {
                        CalcMode::Sum
                    } else {
                        CalcMode::None
                    }
                });
            let summary = compute_column_calculation(col, rows, effective_mode);
            let rendered_calc = render_pdf_calc_summary(&summary);

            if c_idx == 0 && rendered_calc.is_empty() {
                html.push_str(&format!(
                    "        <td>GRAND TOTAL ({} rows)</td>\n",
                    rows.len()
                ));
            } else {
                html.push_str(&format!(
                    "        <td style=\"text-align:right;\">{rendered_calc}</td>\n"
                ));
            }
        }
        html.push_str("      </tr>\n    </tfoot>\n");
    }

    html.push_str("  </table>\n</body>\n</html>");
    html
}

/// Pure-Rust ISO/IEC 18004 QR Code SVG Generator (Byte Mode, Error Correction Level L, Mask 0).
/// Encodes `otpauth://totp/...` URIs into a crisp, scannable inline `<svg>` for Google Authenticator.
pub fn generate_qr_code_svg(data: &str) -> String {
    let bytes = data.as_bytes();
    // Select QR Version (4..=9, Level L)
    // (version, size, total_data_cw, ec_cw_per_block, num_blocks, align_centers, version_info_bits)
    let (size, total_data_cw, ec_per_block, num_blocks, align_coords, ver_info): (
        usize,
        usize,
        usize,
        usize,
        &[usize],
        u32,
    ) = if bytes.len() <= 78 {
        (33, 80, 20, 1, &[6, 26], 0)
    } else if bytes.len() <= 106 {
        (37, 108, 26, 1, &[6, 30], 0)
    } else if bytes.len() <= 134 {
        (41, 136, 18, 2, &[6, 34], 0)
    } else if bytes.len() <= 154 {
        (45, 156, 20, 2, &[6, 22, 38], 0x07C94)
    } else if bytes.len() <= 192 {
        (49, 194, 24, 2, &[6, 24, 42], 0x085BC)
    } else {
        (53, 232, 30, 2, &[6, 26, 46], 0x09A99)
    };

    let payload_len = bytes.len().min(total_data_cw - 2);
    let payload = &bytes[..payload_len];

    // Build bit stream: 4-bit Byte Mode indicator (0100) + 8-bit character count + payload + 4-bit terminator
    let mut bits: Vec<bool> = Vec::with_capacity(total_data_cw * 8);
    // Mode 0100
    bits.extend_from_slice(&[false, true, false, false]);
    // 8-bit length (Versions 1..=9 use 8-bit length for Byte mode)
    let len_u8 = payload.len() as u8;
    for i in (0..8).rev() {
        bits.push(((len_u8 >> i) & 1) == 1);
    }
    for &b in payload {
        for i in (0..8).rev() {
            bits.push(((b >> i) & 1) == 1);
        }
    }
    // Terminator up to 4 zero bits
    for _ in 0..4 {
        if bits.len() < total_data_cw * 8 {
            bits.push(false);
        }
    }
    while bits.len() % 8 != 0 {
        bits.push(false);
    }
    let mut data_cw: Vec<u8> = bits
        .chunks(8)
        .map(|chunk| {
            chunk
                .iter()
                .fold(0u8, |acc, &bit| (acc << 1) | if bit { 1 } else { 0 })
        })
        .collect();
    let pad_bytes = [0xECu8, 0x11u8];
    let mut pad_idx = 0;
    while data_cw.len() < total_data_cw {
        data_cw.push(pad_bytes[pad_idx % 2]);
        pad_idx += 1;
    }

    // GF(256) tables with primitive polynomial 0x11D
    let mut gf_exp = [0u8; 512];
    let mut gf_log = [0u8; 256];
    let mut x = 1u16;
    for i in 0..255 {
        gf_exp[i] = x as u8;
        gf_log[x as usize] = i as u8;
        x <<= 1;
        if (x & 0x100) != 0 {
            x ^= 0x11D;
        }
    }
    for i in 255..512 {
        gf_exp[i] = gf_exp[i - 255];
    }
    let gf_mul = |a: u8, b: u8| -> u8 {
        if a == 0 || b == 0 {
            0
        } else {
            gf_exp[(gf_log[a as usize] as usize) + (gf_log[b as usize] as usize)]
        }
    };

    // Generator polynomial of degree `ec_per_block`
    let mut gen = vec![1u8];
    for i in 0..ec_per_block {
        let root = gf_exp[i];
        let mut next = vec![0u8; gen.len() + 1];
        for (j, &g) in gen.iter().enumerate() {
            next[j] ^= g;
            next[j + 1] ^= gf_mul(g, root);
        }
        gen = next;
    }

    let block_data_len = total_data_cw / num_blocks;
    let mut data_blocks: Vec<Vec<u8>> = Vec::with_capacity(num_blocks);
    let mut ec_blocks: Vec<Vec<u8>> = Vec::with_capacity(num_blocks);

    for b_idx in 0..num_blocks {
        let start = b_idx * block_data_len;
        let block = data_cw[start..start + block_data_len].to_vec();
        let mut rem = vec![0u8; ec_per_block];
        for &d in &block {
            let factor = d ^ rem[0];
            rem.remove(0);
            rem.push(0);
            for j in 0..ec_per_block {
                rem[j] ^= gf_mul(gen[j + 1], factor);
            }
        }
        data_blocks.push(block);
        ec_blocks.push(rem);
    }

    // Interleave blocks
    let mut final_cw: Vec<u8> = Vec::with_capacity(total_data_cw + ec_per_block * num_blocks);
    for i in 0..block_data_len {
        for b in &data_blocks {
            final_cw.push(b[i]);
        }
    }
    for i in 0..ec_per_block {
        for b in &ec_blocks {
            final_cw.push(b[i]);
        }
    }

    // Matrix allocation: None = unreserved, Some(val) = module
    let mut modules = vec![vec![false; size]; size];
    let mut is_function = vec![vec![false; size]; size];

    // Finder patterns + separators
    let draw_finder = |r0: isize,
                       c0: isize,
                       modules: &mut Vec<Vec<bool>>,
                       is_function: &mut Vec<Vec<bool>>| {
        for dr in -1..=7 {
            for dc in -1..=7 {
                let r = r0 + dr;
                let c = c0 + dc;
                if r >= 0 && (r as usize) < size && c >= 0 && (c as usize) < size {
                    let dark = if (0..=6).contains(&dr) && (0..=6).contains(&dc) {
                        dr == 0
                            || dr == 6
                            || dc == 0
                            || dc == 6
                            || ((2..=4).contains(&dr) && (2..=4).contains(&dc))
                    } else {
                        false
                    };
                    modules[r as usize][c as usize] = dark;
                    is_function[r as usize][c as usize] = true;
                }
            }
        }
    };
    draw_finder(0, 0, &mut modules, &mut is_function);
    draw_finder(0, (size - 7) as isize, &mut modules, &mut is_function);
    draw_finder((size - 7) as isize, 0, &mut modules, &mut is_function);

    // Alignment patterns
    for &ar in align_coords {
        for &ac in align_coords {
            // Skip if overlapping with finder patterns
            if (ar <= 8 && ac <= 8)
                || (ar <= 8 && ac >= size - 8)
                || (ar >= size - 8 && ac <= 8)
            {
                continue;
            }
            for dr in -2isize..=2 {
                for dc in -2isize..=2 {
                    let r = (ar as isize + dr) as usize;
                    let c = (ac as isize + dc) as usize;
                    let dark = dr.abs() == 2 || dc.abs() == 2 || (dr == 0 && dc == 0);
                    modules[r][c] = dark;
                    is_function[r][c] = true;
                }
            }
        }
    }

    // Timing patterns
    for i in 8..(size - 8) {
        if !is_function[6][i] {
            modules[6][i] = i % 2 == 0;
            is_function[6][i] = true;
        }
        if !is_function[i][6] {
            modules[i][6] = i % 2 == 0;
            is_function[i][6] = true;
        }
    }

    // Dark module & reserve format information areas
    modules[size - 8][8] = true;
    is_function[size - 8][8] = true;
    for i in 0..9 {
        if i != 6 {
            if !is_function[8][i] {
                modules[8][i] = false;
                is_function[8][i] = true;
            }
            if !is_function[i][8] {
                modules[i][8] = false;
                is_function[i][8] = true;
            }
        }
    }
    for i in 0..8 {
        if !is_function[8][size - 1 - i] {
            modules[8][size - 1 - i] = false;
            is_function[8][size - 1 - i] = true;
        }
        if !is_function[size - 1 - i][8] {
            modules[size - 1 - i][8] = false;
            is_function[size - 1 - i][8] = true;
        }
    }

    // Version info for Version >= 7
    if ver_info != 0 {
        for i in 0..18 {
            let bit = ((ver_info >> i) & 1) == 1;
            let a = size - 11 + (i % 3);
            let b = i / 3;
            modules[b][a] = bit;
            is_function[b][a] = true;
            modules[a][b] = bit;
            is_function[a][b] = true;
        }
    }

    // Place data bits in upward/downward 2-column zig-zag
    let mut bit_idx = 0usize;
    let total_bits = final_cw.len() * 8;
    let mut right = (size as isize) - 1;
    let mut upward = true;
    while right >= 1 {
        if right == 6 {
            right -= 1;
        }
        for vert in 0..size {
            let r = if upward { size - 1 - vert } else { vert };
            for j in 0..2 {
                let c = (right - j) as usize;
                if !is_function[r][c] {
                    let mut dark = false;
                    if bit_idx < total_bits {
                        let cw = final_cw[bit_idx / 8];
                        dark = ((cw >> (7 - (bit_idx % 8))) & 1) == 1;
                        bit_idx += 1;
                    }
                    // Apply Mask 0: (r + c) % 2 == 0
                    if (r + c) % 2 == 0 {
                        dark = !dark;
                    }
                    modules[r][c] = dark;
                }
            }
        }
        upward = !upward;
        right -= 2;
    }

    // Write Format Information for Level L (01) + Mask 0 (000) -> 0x77C4
    let fmt_bits: u16 = 0x77C4;
    for i in 0..15 {
        let bit = ((fmt_bits >> i) & 1) == 1;
        // Top-left copy
        if i < 6 {
            modules[i][8] = bit;
        } else if i < 8 {
            modules[i + 1][8] = bit;
        } else if i == 8 {
            modules[8][7] = bit;
        } else {
            modules[8][14 - i] = bit;
        }
        // Second copy
        if i < 8 {
            modules[8][size - 1 - i] = bit;
        } else {
            modules[size - 15 + i][8] = bit;
        }
    }
    modules[size - 8][8] = true;

    // Render SVG with a 4-module quiet zone
    let quiet = 4usize;
    let view_size = size + quiet * 2;
    let mut svg = format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 {view_size} {view_size}\" shape-rendering=\"crispEdges\" class=\"w-full h-full\">"
    );
    svg.push_str(&format!(
        "<rect width=\"{view_size}\" height=\"{view_size}\" fill=\"#FFFFFF\" rx=\"2\"/>"
    ));
    svg.push_str("<path fill=\"#000000\" d=\"");
    for (r, row) in modules.iter().enumerate() {
        for (c, &dark) in row.iter().enumerate() {
            if dark {
                let x = c + quiet;
                let y = r + quiet;
                svg.push_str(&format!("M{x},{y}h1v1h-1z"));
            }
        }
    }
    svg.push_str("\"/></svg>");
    svg
}
