use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MigrationSafetyLevel {
    SafeAdditive,
    RequiresBackfill,
    DestructiveDrop,
}

impl std::fmt::Display for MigrationSafetyLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SafeAdditive => write!(f, "SAFE_ADDITIVE"),
            Self::RequiresBackfill => write!(f, "REQUIRES_BACKFILL"),
            Self::DestructiveDrop => write!(f, "DESTRUCTIVE_DROP"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ColumnSchema {
    pub name: String,
    pub data_type: String,
    pub is_nullable: bool,
    pub is_primary_key: bool,
    pub default_value: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TableSchema {
    pub table_name: String,
    pub columns: Vec<ColumnSchema>,
    pub primary_keys: Vec<String>,
    pub foreign_keys: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SchemaDriftItem {
    MissingTable {
        table_name: String,
    },
    ExtraTable {
        table_name: String,
    },
    MissingColumn {
        table_name: String,
        column: ColumnSchema,
    },
    ColumnTypeMismatch {
        table_name: String,
        column_name: String,
        expected_type: String,
        live_type: String,
    },
    NullabilityMismatch {
        table_name: String,
        column_name: String,
        expected_nullable: bool,
        live_nullable: bool,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SchemaDriftReport {
    pub database_path: String,
    pub is_in_sync: bool,
    pub drift_items: Vec<SchemaDriftItem>,
    pub overall_safety: MigrationSafetyLevel,
    pub generated_forward_sql: String,
    pub generated_rollback_sql: String,
}
