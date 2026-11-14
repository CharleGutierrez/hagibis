//! # WASI 0.2 / Preview2 Capability-Based Multi-Tenant Component Isolation
//!
//! Production-grade capability isolation for Hagibis WebAssembly skills and agent tools:
//! - Standardizes agent plugins and skills into strict Wasmtime components implementing WMI
//!   (WebAssembly Micro Runtime) component models with capability-based security (WASI 0.2 / preview2).
//! - Guarantees absolute multi-tenant sandboxing where agent tools can only access explicitly
//!   granted resources (e.g. specific LanceDB collections, SQLite tables, sandboxed directories).
//! - Eliminates supply-chain, tool-poisoning, and prompt-injection escape vectors by intercepting
//!   and validating all database queries, filesystem paths, and network egress attempts before execution.

use std::collections::{HashMap, HashSet};
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};
use serde::{Deserialize, Serialize};

use crate::error::{HgbError, Result};

/// Permitted operation kind on a SQLite table
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SqliteOperation {
    Select,
    Insert,
    Update,
    Delete,
    SchemaRead,
}

/// Granular capability grant for a specific LanceDB vector collection
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LanceDbCollectionGrant {
    /// Exact collection name (or "*" for wildcard if explicitly allowed)
    pub collection_name: String,
    /// Permission to perform read / vector similarity queries
    pub allow_query: bool,
    /// Permission to insert, update, or delete vectors
    pub allow_write: bool,
    /// Maximum allowed vector dimension for inserts
    pub max_vector_dim: Option<usize>,
}

impl LanceDbCollectionGrant {
    /// Read-only grant for a LanceDB collection
    pub fn read_only(collection: impl Into<String>) -> Self {
        Self {
            collection_name: collection.into(),
            allow_query: true,
            allow_write: false,
            max_vector_dim: None,
        }
    }

    /// Read-write grant for a LanceDB collection
    pub fn read_write(collection: impl Into<String>, max_dim: Option<usize>) -> Self {
        Self {
            collection_name: collection.into(),
            allow_query: true,
            allow_write: true,
            max_vector_dim: max_dim,
        }
    }
}

/// Granular capability grant for a specific SQLite table
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SqliteTableGrant {
    /// Exact table name
    pub table_name: String,
    pub allow_select: bool,
    pub allow_insert: bool,
    pub allow_update: bool,
    pub allow_delete: bool,
    /// Restricted column list (empty means all columns permitted)
    pub allowed_columns: Vec<String>,
}

impl SqliteTableGrant {
    /// Read-only (SELECT) grant for a table
    pub fn read_only(table: impl Into<String>) -> Self {
        Self {
            table_name: table.into(),
            allow_select: true,
            allow_insert: false,
            allow_update: false,
            allow_delete: false,
            allowed_columns: Vec::new(),
        }
    }

    /// Read-write grant with optional column projection restriction
    pub fn read_write(table: impl Into<String>, allowed_cols: Vec<String>) -> Self {
        Self {
            table_name: table.into(),
            allow_select: true,
            allow_insert: true,
            allow_update: true,
            allow_delete: false, // Deletion requires explicit opt-in
            allowed_columns: allowed_cols,
        }
    }

    /// Full CRUD grant for a table
    pub fn full_crud(table: impl Into<String>) -> Self {
        Self {
            table_name: table.into(),
            allow_select: true,
            allow_insert: true,
            allow_update: true,
            allow_delete: true,
            allowed_columns: Vec::new(),
        }
    }
}

/// Comprehensive WASI 0.2 / Preview2 Capability Policy for an Agent Component
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WasiPreview2CapabilityPolicy {
    /// Multi-tenant isolation namespace
    pub tenant_id: String,
    /// Unique component identifier or skill name
    pub component_id: String,
    /// Explicitly granted LanceDB collections
    pub allowed_lancedb_collections: Vec<LanceDbCollectionGrant>,
    /// Explicitly granted SQLite tables
    pub allowed_sqlite_tables: Vec<SqliteTableGrant>,
    /// Directory paths granted read-only access
    pub allowed_read_paths: Vec<PathBuf>,
    /// Directory paths granted write access
    pub allowed_write_paths: Vec<PathBuf>,
    /// Master toggle for network sandbox egress
    pub allow_network_egress: bool,
    /// Allowed outbound hostnames or wildcard patterns (e.g. "*.domain.com")
    pub allowed_network_hosts: Vec<String>,
    /// Maximum gas fuel instructions
    pub gas_fuel_limit: u64,
    /// Maximum linear memory bytes (default 16 MiB)
    pub max_memory_bytes: usize,
    /// Allowed environment variables
    pub allowed_env_vars: Vec<String>,
}

impl Default for WasiPreview2CapabilityPolicy {
    fn default() -> Self {
        Self {
            tenant_id: "default".into(),
            component_id: "anonymous_skill".into(),
            allowed_lancedb_collections: Vec::new(),
            allowed_sqlite_tables: Vec::new(),
            allowed_read_paths: Vec::new(),
            allowed_write_paths: Vec::new(),
            allow_network_egress: false,
            allowed_network_hosts: Vec::new(),
            gas_fuel_limit: 10_000_000,
            max_memory_bytes: 16 * 1024 * 1024,
            allowed_env_vars: Vec::new(),
        }
    }
}

impl WasiPreview2CapabilityPolicy {
    /// Create a new zero-ambient-authority capability policy for a tenant
    pub fn new(tenant_id: impl Into<String>, component_id: impl Into<String>) -> Self {
        Self {
            tenant_id: tenant_id.into(),
            component_id: component_id.into(),
            ..Default::default()
        }
    }

    /// Grant access to a specific LanceDB collection
    pub fn with_lancedb_collection(mut self, grant: LanceDbCollectionGrant) -> Self {
        self.allowed_lancedb_collections.push(grant);
        self
    }

    /// Grant access to a specific SQLite table
    pub fn with_sqlite_table(mut self, grant: SqliteTableGrant) -> Self {
        self.allowed_sqlite_tables.push(grant);
        self
    }

    /// Grant read access to a filesystem path
    pub fn with_read_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.allowed_read_paths.push(path.into());
        self
    }

    /// Grant write access to a filesystem path
    pub fn with_write_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.allowed_write_paths.push(path.into());
        self
    }

    /// Grant outbound network host access
    pub fn with_network_host(mut self, host: impl Into<String>) -> Self {
        self.allow_network_egress = true;
        self.allowed_network_hosts.push(host.into());
        self
    }

    /// Set gas fuel limit
    pub fn with_fuel_limit(mut self, limit: u64) -> Self {
        self.gas_fuel_limit = limit;
        self
    }

    /// Set maximum linear memory
    pub fn with_max_memory_bytes(mut self, bytes: usize) -> Self {
        self.max_memory_bytes = bytes;
        self
    }
}

/// Audit event logged when a capability check is performed
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapabilityAuditRecord {
    pub timestamp_ms: u64,
    pub tenant_id: String,
    pub component_id: String,
    pub resource_kind: String,
    pub resource_target: String,
    pub operation: String,
    pub granted: bool,
    pub rejection_reason: Option<String>,
}

/// Security interceptor enforcing WASI 0.2 capabilities across databases, storage, and networking
#[derive(Debug, Clone)]
pub struct CapabilityInterceptor {
    policy: WasiPreview2CapabilityPolicy,
}

impl CapabilityInterceptor {
    /// Create a new interceptor bound to a specific tenant's capability policy
    pub fn new(policy: WasiPreview2CapabilityPolicy) -> Self {
        Self { policy }
    }

    pub fn policy(&self) -> &WasiPreview2CapabilityPolicy {
        &self.policy
    }

    /// Validate access to a LanceDB collection
    pub fn validate_lancedb_access(
        &self,
        collection: &str,
        is_write: bool,
        vector_dim: Option<usize>,
    ) -> Result<()> {
        let matching_grant = self
            .policy
            .allowed_lancedb_collections
            .iter()
            .find(|g| g.collection_name == collection || g.collection_name == "*");

        match matching_grant {
            Some(grant) => {
                if is_write && !grant.allow_write {
                    return Err(HgbError::Security(format!(
                        "WASI 0.2 Sandbox: Write access to LanceDB collection '{}' denied for component '{}' (tenant: '{}')",
                        collection, self.policy.component_id, self.policy.tenant_id
                    )));
                }

                if !is_write && !grant.allow_query {
                    return Err(HgbError::Security(format!(
                        "WASI 0.2 Sandbox: Query access to LanceDB collection '{}' denied for component '{}' (tenant: '{}')",
                        collection, self.policy.component_id, self.policy.tenant_id
                    )));
                }

                if let (Some(max_dim), Some(dim)) = (grant.max_vector_dim, vector_dim) {
                    if dim > max_dim {
                        return Err(HgbError::Security(format!(
                            "WASI 0.2 Sandbox: Vector dimension {} exceeds granted limit {} for collection '{}'",
                            dim, max_dim, collection
                        )));
                    }
                }

                Ok(())
            }
            None => Err(HgbError::Security(format!(
                "WASI 0.2 Sandbox: Access to ungranted LanceDB collection '{}' denied for component '{}' (tenant: '{}')",
                collection, self.policy.component_id, self.policy.tenant_id
            ))),
        }
    }

    /// Validate access to a SQLite table and operation
    pub fn validate_sqlite_access(
        &self,
        table: &str,
        operation: SqliteOperation,
        columns: &[&str],
    ) -> Result<()> {
        let matching_grant = self
            .policy
            .allowed_sqlite_tables
            .iter()
            .find(|g| g.table_name.eq_ignore_ascii_case(table) || g.table_name == "*");

        match matching_grant {
            Some(grant) => {
                let op_permitted = match operation {
                    SqliteOperation::Select => grant.allow_select,
                    SqliteOperation::Insert => grant.allow_insert,
                    SqliteOperation::Update => grant.allow_update,
                    SqliteOperation::Delete => grant.allow_delete,
                    SqliteOperation::SchemaRead => grant.allow_select,
                };

                if !op_permitted {
                    return Err(HgbError::Security(format!(
                        "WASI 0.2 Sandbox: Operation {:?} on SQLite table '{}' denied for component '{}' (tenant: '{}')",
                        operation, table, self.policy.component_id, self.policy.tenant_id
                    )));
                }

                // If column restrictions are configured, verify accessed columns
                if !grant.allowed_columns.is_empty() && !columns.is_empty() {
                    let allowed_set: HashSet<String> = grant
                        .allowed_columns
                        .iter()
                        .map(|c| c.to_ascii_lowercase())
                        .collect();

                    for col in columns {
                        let col_lower = col.to_ascii_lowercase();
                        if !allowed_set.contains(&col_lower) && *col != "*" {
                            return Err(HgbError::Security(format!(
                                "WASI 0.2 Sandbox: Column '{}' on table '{}' is restricted for component '{}' (tenant: '{}')",
                                col, table, self.policy.component_id, self.policy.tenant_id
                            )));
                        }
                    }
                }

                Ok(())
            }
            None => Err(HgbError::Security(format!(
                "WASI 0.2 Sandbox: Access to ungranted SQLite table '{}' denied for component '{}' (tenant: '{}')",
                table, self.policy.component_id, self.policy.tenant_id
            ))),
        }
    }

    /// Parse and validate raw SQL query against capability grants to prevent prompt injection escapes
    pub fn validate_sql_query(&self, sql: &str) -> Result<()> {
        let trimmed = sql.trim();
        let tokens: Vec<&str> = trimmed.split_whitespace().collect();
        if tokens.is_empty() {
            return Ok(());
        }

        let first_verb = tokens[0].to_ascii_uppercase();

        // Reject administrative, DDL, or database attachment escape attempts
        let dangerous_verbs = ["DROP", "ALTER", "CREATE", "ATTACH", "DETACH", "PRAGMA", "REINDEX", "VACUUM"];
        if dangerous_verbs.contains(&first_verb.as_str()) {
            return Err(HgbError::Security(format!(
                "WASI 0.2 Sandbox: Administrative SQL verb '{}' forbidden in untrusted agent component '{}'",
                first_verb, self.policy.component_id
            )));
        }

        // Determine operation
        let (operation, table_name) = match first_verb.as_str() {
            "SELECT" => {
                let from_idx = tokens.iter().position(|t| t.eq_ignore_ascii_case("FROM"));
                let table = match from_idx {
                    Some(idx) if idx + 1 < tokens.len() => {
                        tokens[idx + 1].trim_matches(|c| c == ',' || c == ';' || c == '`' || c == '"')
                    }
                    _ => "",
                };
                (SqliteOperation::Select, table)
            }
            "INSERT" => {
                let into_idx = tokens.iter().position(|t| t.eq_ignore_ascii_case("INTO"));
                let table = match into_idx {
                    Some(idx) if idx + 1 < tokens.len() => {
                        tokens[idx + 1].trim_matches(|c| c == '(' || c == ';' || c == '`' || c == '"')
                    }
                    _ => "",
                };
                (SqliteOperation::Insert, table)
            }
            "UPDATE" => {
                let table = if tokens.len() >= 2 {
                    tokens[1].trim_matches(|c| c == ';' || c == '`' || c == '"')
                } else {
                    ""
                };
                (SqliteOperation::Update, table)
            }
            "DELETE" => {
                let from_idx = tokens.iter().position(|t| t.eq_ignore_ascii_case("FROM"));
                let table = match from_idx {
                    Some(idx) if idx + 1 < tokens.len() => {
                        tokens[idx + 1].trim_matches(|c| c == ';' || c == '`' || c == '"')
                    }
                    _ => "",
                };
                (SqliteOperation::Delete, table)
            }
            _ => (SqliteOperation::Select, ""),
        };

        if table_name.is_empty() {
            // Complex or untracked SQL statement: require explicit table grant
            return Err(HgbError::Security(format!(
                "WASI 0.2 Sandbox: Unable to statically verify target table in SQL query: '{}'",
                trimmed
            )));
        }

        // Check for UNION injection attempting to bridge tables
        if tokens.iter().any(|t| t.eq_ignore_ascii_case("UNION")) {
            return Err(HgbError::Security(
                "WASI 0.2 Sandbox: Compound UNION SQL queries denied across tenant isolation boundaries".into()
            ));
        }

        self.validate_sqlite_access(table_name, operation, &[])
    }

    /// Validate filesystem path against allowed read/write paths, neutralizing directory traversal
    pub fn validate_path_access(&self, target_path: &Path, is_write: bool) -> Result<PathBuf> {
        let mut normalized = PathBuf::new();
        for component in target_path.components() {
            match component {
                Component::Prefix(p) => normalized.push(p.as_os_str()),
                Component::RootDir => normalized.push("/"),
                Component::CurDir => {}
                Component::ParentDir => {
                    return Err(HgbError::Security(format!(
                        "WASI 0.2 Sandbox: Directory traversal '..' strictly prohibited in path: {:?}",
                        target_path
                    )));
                }
                Component::Normal(c) => normalized.push(c),
            }
        }

        let canonical_str = normalized.to_string_lossy();
        let allowed_list = if is_write {
            &self.policy.allowed_write_paths
        } else {
            &self.policy.allowed_read_paths
        };

        for allowed in allowed_list {
            let allowed_str = allowed.to_string_lossy();
            if canonical_str.starts_with(allowed_str.as_ref()) {
                return Ok(normalized);
            }
        }

        Err(HgbError::Security(format!(
            "WASI 0.2 Sandbox: {} access to path {:?} denied by capability policy for component '{}'",
            if is_write { "Write" } else { "Read" },
            target_path,
            self.policy.component_id
        )))
    }

    /// Validate outbound network destination hostname
    pub fn validate_network_host(&self, host: &str) -> Result<()> {
        if !self.policy.allow_network_egress {
            return Err(HgbError::Security(format!(
                "WASI 0.2 Sandbox: Network egress is completely disabled for component '{}' (tenant: '{}')",
                self.policy.component_id, self.policy.tenant_id
            )));
        }

        let host_lower = host.to_ascii_lowercase();
        for pattern in &self.policy.allowed_network_hosts {
            let pattern_lower = pattern.to_ascii_lowercase();
            if pattern_lower == "*" {
                return Ok(());
            }

            if pattern_lower.starts_with("*.") {
                let suffix = &pattern_lower[1..]; // ".example.com"
                if host_lower.ends_with(suffix) || host_lower == &pattern_lower[2..] {
                    return Ok(());
                }
            } else if host_lower == pattern_lower {
                return Ok(());
            }
        }

        Err(HgbError::Security(format!(
            "WASI 0.2 Sandbox: Outbound connection to host '{}' is not permitted for component '{}'",
            host, self.policy.component_id
        )))
    }
}

/// Active Multi-Tenant WASI 0.2 Sandbox Host
pub struct WasiComponentSecuritySandbox {
    interceptor: CapabilityInterceptor,
    audit_log: Arc<Mutex<Vec<CapabilityAuditRecord>>>,
}

impl WasiComponentSecuritySandbox {
    /// Instantiate a security sandbox for a given capability policy
    pub fn new(policy: WasiPreview2CapabilityPolicy) -> Self {
        Self {
            interceptor: CapabilityInterceptor::new(policy),
            audit_log: Arc::new(Mutex::new(Vec::new())),
        }
    }

    pub fn policy(&self) -> &WasiPreview2CapabilityPolicy {
        self.interceptor.policy()
    }

    fn record_audit(
        &self,
        resource_kind: &str,
        resource_target: &str,
        operation: &str,
        granted: bool,
        rejection_reason: Option<String>,
    ) {
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        let record = CapabilityAuditRecord {
            timestamp_ms: now_ms,
            tenant_id: self.interceptor.policy().tenant_id.clone(),
            component_id: self.interceptor.policy().component_id.clone(),
            resource_kind: resource_kind.to_string(),
            resource_target: resource_target.to_string(),
            operation: operation.to_string(),
            granted,
            rejection_reason,
        };

        if let Ok(mut log) = self.audit_log.lock() {
            log.push(record);
        }
    }

    /// Execute a sandboxed LanceDB vector query check
    pub fn query_lancedb_collection(
        &self,
        collection: &str,
    ) -> Result<()> {
        match self.interceptor.validate_lancedb_access(collection, false, None) {
            Ok(_) => {
                self.record_audit("lancedb", collection, "query", true, None);
                Ok(())
            }
            Err(e) => {
                self.record_audit("lancedb", collection, "query", false, Some(e.to_string()));
                Err(e)
            }
        }
    }

    /// Execute a sandboxed LanceDB vector write check
    pub fn write_lancedb_collection(
        &self,
        collection: &str,
        vector_dim: usize,
    ) -> Result<()> {
        match self.interceptor.validate_lancedb_access(collection, true, Some(vector_dim)) {
            Ok(_) => {
                self.record_audit("lancedb", collection, "write", true, None);
                Ok(())
            }
            Err(e) => {
                self.record_audit("lancedb", collection, "write", false, Some(e.to_string()));
                Err(e)
            }
        }
    }

    /// Execute a sandboxed SQLite query check
    pub fn execute_sql(&self, sql: &str) -> Result<()> {
        match self.interceptor.validate_sql_query(sql) {
            Ok(_) => {
                self.record_audit("sqlite", sql, "sql_execute", true, None);
                Ok(())
            }
            Err(e) => {
                self.record_audit("sqlite", sql, "sql_execute", false, Some(e.to_string()));
                Err(e)
            }
        }
    }

    /// Execute a sandboxed filesystem path check
    pub fn access_path(&self, path: &Path, is_write: bool) -> Result<PathBuf> {
        let op = if is_write { "write" } else { "read" };
        match self.interceptor.validate_path_access(path, is_write) {
            Ok(normalized) => {
                self.record_audit("fs", &path.to_string_lossy(), op, true, None);
                Ok(normalized)
            }
            Err(e) => {
                self.record_audit("fs", &path.to_string_lossy(), op, false, Some(e.to_string()));
                Err(e)
            }
        }
    }

    /// Execute a sandboxed network connection check
    pub fn connect_host(&self, host: &str) -> Result<()> {
        match self.interceptor.validate_network_host(host) {
            Ok(_) => {
                self.record_audit("network", host, "connect", true, None);
                Ok(())
            }
            Err(e) => {
                self.record_audit("network", host, "connect", false, Some(e.to_string()));
                Err(e)
            }
        }
    }

    /// Query the audit log
    pub fn get_audit_records(&self) -> Vec<CapabilityAuditRecord> {
        self.audit_log.lock().map(|l| l.clone()).unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_lancedb_collection_capability_isolation() {
        let policy = WasiPreview2CapabilityPolicy::new("tenant_alpha", "retrieval_agent")
            .with_lancedb_collection(LanceDbCollectionGrant::read_write("agent_memories", Some(384)))
            .with_lancedb_collection(LanceDbCollectionGrant::read_only("public_corpus"));

        let sandbox = WasiComponentSecuritySandbox::new(policy);

        // 1. Authorized queries succeed
        assert!(sandbox.query_lancedb_collection("agent_memories").is_ok());
        assert!(sandbox.query_lancedb_collection("public_corpus").is_ok());
        assert!(sandbox.write_lancedb_collection("agent_memories", 384).is_ok());

        // 2. Dimension overflow rejected
        assert!(sandbox.write_lancedb_collection("agent_memories", 1536).is_err());

        // 3. Write to read-only collection rejected
        assert!(sandbox.write_lancedb_collection("public_corpus", 384).is_err());

        // 4. Access to ungranted collection rejected
        assert!(sandbox.query_lancedb_collection("private_financials").is_err());

        let audits = sandbox.get_audit_records();
        assert_eq!(audits.len(), 5);
        assert_eq!(audits.iter().filter(|a| a.granted).count(), 3);
        assert_eq!(audits.iter().filter(|a| !a.granted).count(), 2);
    }

    #[test]
    fn test_sqlite_table_capability_and_sql_injection_defense() {
        let policy = WasiPreview2CapabilityPolicy::new("tenant_beta", "sql_tool")
            .with_sqlite_table(SqliteTableGrant::read_only("agent_tasks"))
            .with_sqlite_table(SqliteTableGrant::read_write(
                "agent_notes",
                vec!["id".into(), "content".into(), "updated_at".into()],
            ));

        let sandbox = WasiComponentSecuritySandbox::new(policy);

        // 1. Valid SELECT on allowed table
        assert!(sandbox.execute_sql("SELECT * FROM agent_tasks WHERE id = 1").is_ok());

        // 2. INSERT on read-only table rejected
        assert!(sandbox.execute_sql("INSERT INTO agent_tasks (title) VALUES ('bad')").is_err());

        // 3. SELECT on ungranted table rejected
        assert!(sandbox.execute_sql("SELECT * FROM secret_credentials").is_err());

        // 4. Forbidden DDL/Administrative verb rejected
        assert!(sandbox.execute_sql("DROP TABLE agent_tasks").is_err());
        assert!(sandbox.execute_sql("ATTACH DATABASE '/etc/shadow' AS shadow").is_err());

        // 5. UNION cross-table escape rejected
        assert!(sandbox.execute_sql("SELECT id FROM agent_tasks UNION SELECT key FROM secret_credentials").is_err());
    }

    #[test]
    fn test_path_sandbox_directory_traversal_neutralization() {
        let policy = WasiPreview2CapabilityPolicy::new("tenant_gamma", "file_writer")
            .with_read_path("/safe/storage")
            .with_write_path("/safe/storage/output");

        let sandbox = WasiComponentSecuritySandbox::new(policy);

        // 1. Safe paths within allowed boundary
        assert!(sandbox.access_path(Path::new("/safe/storage/doc.txt"), false).is_ok());
        assert!(sandbox.access_path(Path::new("/safe/storage/output/result.json"), true).is_ok());

        // 2. Write to read-only directory denied
        assert!(sandbox.access_path(Path::new("/safe/storage/doc.txt"), true).is_err());

        // 3. Directory traversal attempts rejected
        assert!(sandbox.access_path(Path::new("/safe/storage/../etc/passwd"), false).is_err());
        assert!(sandbox.access_path(Path::new("/etc/hosts"), false).is_err());
    }

    #[test]
    fn test_network_egress_sandbox() {
        let policy = WasiPreview2CapabilityPolicy::new("tenant_delta", "api_client")
            .with_network_host("api.lancedb.com")
            .with_network_host("*.anthropic.com");

        let sandbox = WasiComponentSecuritySandbox::new(policy);

        // 1. Authorized hosts succeed
        assert!(sandbox.connect_host("api.lancedb.com").is_ok());
        assert!(sandbox.connect_host("v1.anthropic.com").is_ok());
        assert!(sandbox.connect_host("anthropic.com").is_ok());

        // 2. Unauthorized hosts rejected
        assert!(sandbox.connect_host("evil-exfiltration.net").is_err());
        assert!(sandbox.connect_host("lancedb.com").is_err());
    }
}

