//! # Enterprise Packaging, Cryptographic Licensing & SOC2 Audit Ledger
//!
//! Implements Strategy 5 of the Enterprise Modernization Blueprint:
//! 1. `HgbEdition`: Community ("Sovereign") vs Enterprise ("Fleet Orchestrator").
//! 2. `EnterpriseLicense`: Cryptographic signature verification, tenant isolation, seat quotas, and expiration.
//! 3. `EnterpriseAuditLedger`: Cryptographic append-only hash chain of all agent actions, model calls,
//!    and human approvals for SOC2/ISO 27001 compliance, exportable to JSON/CSV for Dataverse & Power BI.

use crate::error::{HgbError, Result};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};

/// Product edition for Hagibis runtime
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum HgbEdition {
    /// Community Edition: "Sovereign" developer experience (Local models, Ollama, privacy-first)
    Community,
    /// Enterprise Edition: "Fleet Orchestrator" (Azure OpenAI, Microsoft 365, Webhooks, SOC2 Ledger)
    Enterprise,
}

impl Default for HgbEdition {
    fn default() -> Self {
        Self::Community
    }
}

impl HgbEdition {
    pub fn name(&self) -> &'static str {
        match self {
            Self::Community => "Hagibis Sovereign Community Edition",
            Self::Enterprise => "Hagibis Fleet Orchestrator Enterprise Edition",
        }
    }

    pub fn detect(workspace_root: Option<&Path>) -> Self {
        if std::env::var("HGB_ENTERPRISE_TOKEN").is_ok() || std::env::var("HGB_LICENSE_KEY").is_ok() {
            return Self::Enterprise;
        }
        if let Some(ws) = workspace_root {
            if ws.join(".hgb").join("enterprise.lic").exists() {
                return Self::Enterprise;
            }
        }
        Self::Community
    }

    pub fn is_enterprise(&self) -> bool {
        matches!(self, Self::Enterprise)
    }

    pub fn allows_feature(&self, feature: &str) -> bool {
        match self {
            Self::Enterprise => true,
            Self::Community => match feature {
                "azure_openai" | "microsoft_mcp_bridge" | "webhook_gateway" | "soc2_audit_ledger" => false,
                _ => true,
            },
        }
    }

    pub fn can_use_azure_openai(&self) -> bool {
        self.allows_feature("azure_openai")
    }

    pub fn can_use_m365_tools(&self) -> bool {
        self.allows_feature("microsoft_mcp_bridge")
    }

    pub fn can_use_webhook_gateway(&self) -> bool {
        self.allows_feature("webhook_gateway")
    }

    pub fn can_use_audit_ledger(&self) -> bool {
        self.allows_feature("soc2_audit_ledger")
    }
}

/// Feature flags enabled by an enterprise license
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EnterpriseEntitlements {
    pub azure_openai: bool,
    pub microsoft_mcp_bridge: bool,
    pub webhook_gateway: bool,
    pub npu_acceleration: bool,
    pub soc2_audit_ledger: bool,
    pub fleet_orchestration: bool,
}

impl Default for EnterpriseEntitlements {
    fn default() -> Self {
        Self {
            azure_openai: true,
            microsoft_mcp_bridge: true,
            webhook_gateway: true,
            npu_acceleration: true,
            soc2_audit_ledger: true,
            fleet_orchestration: true,
        }
    }
}

/// Cryptographically signed enterprise license token
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EnterpriseLicense {
    pub tenant_id: String,
    pub organization_name: String,
    pub issued_at: String,
    pub expires_at: String,
    pub seats: u32,
    pub entitlements: EnterpriseEntitlements,
    pub signature: String,
}

impl EnterpriseLicense {
    pub const DEFAULT_MASTER_SECRET: &'static str = "hgb_enterprise_master_vault_key_2026_blake3";

    /// Issues an enterprise license using the default master secret
    pub fn issue_default(
        tenant_id: impl Into<String>,
        organization_name: impl Into<String>,
        seats: u32,
        validity_days: i64,
    ) -> (Self, String) {
        Self::generate(
            tenant_id,
            organization_name,
            seats,
            validity_days,
            EnterpriseEntitlements::default(),
            Self::DEFAULT_MASTER_SECRET,
        )
    }

    /// Verifies a license token using the default master secret
    pub fn verify_default(token: &str) -> Result<Self> {
        Self::parse_and_verify(token, Self::DEFAULT_MASTER_SECRET)
    }

    /// Generates a signed license and returns `(EnterpriseLicense, raw_license_token)`
    pub fn generate(
        tenant_id: impl Into<String>,
        organization_name: impl Into<String>,
        seats: u32,
        validity_days: i64,
        entitlements: EnterpriseEntitlements,
        master_secret: &str,
    ) -> (Self, String) {
        let t_id = tenant_id.into();
        let org = organization_name.into();
        let now = Utc::now();
        let issued_at = now.to_rfc3339();
        let expires_at = (now + Duration::days(validity_days)).to_rfc3339();

        let payload_to_sign = format!(
            "tenant={}|org={}|issued={}|expires={}|seats={}|entitlements={:?}",
            t_id, org, issued_at, expires_at, seats, entitlements
        );

        let signature = blake3::keyed_hash(
            blake3::hash(master_secret.as_bytes()).as_bytes(),
            payload_to_sign.as_bytes(),
        ).to_hex().to_string();

        let license = Self {
            tenant_id: t_id,
            organization_name: org,
            issued_at,
            expires_at,
            seats,
            entitlements,
            signature: signature.clone(),
        };

        let raw_json = serde_json::to_string(&license).unwrap_or_default();
        let b64 = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, raw_json.as_bytes());
        let token = format!("HGB-ENT-{}.{}", b64, signature);

        (license, token)
    }

    /// Verifies and decodes an enterprise license token
    pub fn parse_and_verify(token: &str, master_secret: &str) -> Result<Self> {
        let clean = token.trim();
        let payload_part = clean.strip_prefix("HGB-ENT-")
            .ok_or_else(|| HgbError::Authentication("Invalid license format: missing HGB-ENT- prefix".to_string()))?;

        let mut parts = payload_part.split('.');
        let b64_payload = parts.next()
            .ok_or_else(|| HgbError::Authentication("Missing license payload segment".to_string()))?;
        let expected_sig = parts.next()
            .ok_or_else(|| HgbError::Authentication("Missing license signature segment".to_string()))?;

        let decoded_bytes = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, b64_payload)
            .map_err(|e| HgbError::Authentication(format!("Corrupted license base64: {e}")))?;

        let license: EnterpriseLicense = serde_json::from_slice(&decoded_bytes)
            .map_err(|e| HgbError::Authentication(format!("Corrupted license JSON: {e}")))?;

        // Recompute signature
        let payload_to_sign = format!(
            "tenant={}|org={}|issued={}|expires={}|seats={}|entitlements={:?}",
            license.tenant_id, license.organization_name, license.issued_at, license.expires_at, license.seats, license.entitlements
        );

        let computed_sig = blake3::keyed_hash(
            blake3::hash(master_secret.as_bytes()).as_bytes(),
            payload_to_sign.as_bytes(),
        ).to_hex().to_string();

        if computed_sig != expected_sig || license.signature != computed_sig {
            return Err(HgbError::Security(
                "Cryptographic signature mismatch! License token has been tampered with or revoked.".to_string(),
            ));
        }

        // Expiration check
        if let Ok(exp) = DateTime::parse_from_rfc3339(&license.expires_at) {
            if Utc::now() > exp {
                return Err(HgbError::Authentication(format!(
                    "Enterprise license for tenant '{}' expired on {}.",
                    license.tenant_id, license.expires_at
                )));
            }
        }

        Ok(license)
    }

    pub fn is_valid(&self) -> bool {
        if let Ok(exp) = DateTime::parse_from_rfc3339(&self.expires_at) {
            Utc::now() <= exp
        } else {
            false
        }
    }
}

// -----------------------------------------------------------------------------
// Enterprise Cryptographic Audit Ledger (SOC2 / ISO 27001)
// -----------------------------------------------------------------------------

/// Individual immutable record in the cryptographic append-only audit chain
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EnterpriseAuditRecord {
    pub index: u64,
    pub timestamp: String,
    pub prev_hash: String,
    pub action_type: String,
    pub actor: String,
    pub details: serde_json::Value,
    pub block_hash: String,
}

impl EnterpriseAuditRecord {
    pub fn compute_hash(
        index: u64,
        timestamp: &str,
        prev_hash: &str,
        action_type: &str,
        actor: &str,
        details: &serde_json::Value,
    ) -> String {
        let mut hasher = blake3::Hasher::new();
        hasher.update(&index.to_be_bytes());
        hasher.update(timestamp.as_bytes());
        hasher.update(prev_hash.as_bytes());
        hasher.update(action_type.as_bytes());
        hasher.update(actor.as_bytes());
        hasher.update(details.to_string().as_bytes());
        hasher.finalize().to_hex().to_string()
    }
}

/// Cryptographically verifiable append-only SOC2 / ISO 27001 audit ledger
pub struct EnterpriseAuditLedger {
    records: Vec<EnterpriseAuditRecord>,
    storage_path: Option<PathBuf>,
}

impl EnterpriseAuditLedger {
    pub const GENESIS_PREV_HASH: &'static str = "0000000000000000000000000000000000000000000000000000000000000000";

    pub fn new(storage_path: Option<PathBuf>) -> Self {
        let mut ledger = Self {
            records: Vec::new(),
            storage_path,
        };

        // If storage file exists on disk, load it
        if let Some(ref path) = ledger.storage_path {
            if path.exists() {
                if let Ok(loaded) = Self::load_from_disk(path) {
                    ledger.records = loaded.records;
                    return ledger;
                }
            }
        }

        // Initialize with Genesis Block if empty
        let genesis_time = Utc::now().to_rfc3339();
        let genesis_details = serde_json::json!({
            "event": "GenesisBlock",
            "standard": "SOC2_Type_II_ISO_27001",
            "version": "1.0"
        });
        let genesis_hash = EnterpriseAuditRecord::compute_hash(
            0,
            &genesis_time,
            Self::GENESIS_PREV_HASH,
            "GENESIS",
            "HagibisKernel",
            &genesis_details,
        );

        ledger.records.push(EnterpriseAuditRecord {
            index: 0,
            timestamp: genesis_time,
            prev_hash: Self::GENESIS_PREV_HASH.to_string(),
            action_type: "GENESIS".to_string(),
            actor: "HagibisKernel".to_string(),
            details: genesis_details,
            block_hash: genesis_hash,
        });

        ledger
    }

    pub fn records(&self) -> &[EnterpriseAuditRecord] {
        &self.records
    }

    /// Appends a new immutable action to the cryptographic hash chain
    pub fn record(
        &mut self,
        action_type: &str,
        actor: &str,
        details: serde_json::Value,
    ) -> Result<String> {
        let index = self.records.len() as u64;
        let prev_hash = self.records.last().map(|r| r.block_hash.clone()).unwrap_or_else(|| Self::GENESIS_PREV_HASH.to_string());
        let timestamp = Utc::now().to_rfc3339();

        let block_hash = EnterpriseAuditRecord::compute_hash(
            index,
            &timestamp,
            &prev_hash,
            action_type,
            actor,
            &details,
        );

        let record = EnterpriseAuditRecord {
            index,
            timestamp,
            prev_hash,
            action_type: action_type.to_string(),
            actor: actor.to_string(),
            details,
            block_hash: block_hash.clone(),
        };

        self.records.push(record);
        let _ = self.persist();

        Ok(block_hash)
    }

    /// Verifies the cryptographic integrity of every link in the hash chain
    pub fn verify_integrity(&self) -> Result<bool> {
        if self.records.is_empty() {
            return Ok(true);
        }

        for (i, rec) in self.records.iter().enumerate() {
            if i == 0 {
                if rec.prev_hash != Self::GENESIS_PREV_HASH {
                    return Err(HgbError::Security("Corrupt genesis prev_hash".to_string()));
                }
            } else {
                let prev = &self.records[i - 1];
                if rec.prev_hash != prev.block_hash {
                    return Err(HgbError::Security(format!(
                        "Broken chain link at index {}: prev_hash {} != previous block_hash {}",
                        i, rec.prev_hash, prev.block_hash
                    )));
                }
            }

            // Verify recalculation of block_hash
            let computed = EnterpriseAuditRecord::compute_hash(
                rec.index,
                &rec.timestamp,
                &rec.prev_hash,
                &rec.action_type,
                &rec.actor,
                &rec.details,
            );

            if computed != rec.block_hash {
                return Err(HgbError::Security(format!(
                    "Tampered block detected at index {}: computed {} != stored {}",
                    i, computed, rec.block_hash
                )));
            }
        }

        Ok(true)
    }

    /// Export ledger records formatted for Microsoft Dataverse & Power Apps ingestion
    pub fn export_json(&self) -> Result<String> {
        serde_json::to_string_pretty(&self.records)
            .map_err(|e| HgbError::Serialization(e.to_string()))
    }

    /// Export ledger records formatted as CSV for Power BI dashboards
    pub fn export_csv(&self) -> Result<String> {
        let mut csv = String::new();
        csv.push_str("Index,Timestamp,PrevHash,ActionType,Actor,BlockHash,DetailsSnippet\n");

        for rec in &self.records {
            let details_snippet = rec.details.to_string().replace('"', "\"\"");
            csv.push_str(&format!(
                "{},\"{}\",\"{}\",\"{}\",\"{}\",\"{}\",\"{}\"\n",
                rec.index, rec.timestamp, rec.prev_hash, rec.action_type, rec.actor, rec.block_hash, details_snippet
            ));
        }

        Ok(csv)
    }

    /// Load ledger from persistent JSON file
    pub fn load_from_disk(path: impl AsRef<Path>) -> Result<Self> {
        let path_ref = path.as_ref();
        let content = fs::read_to_string(path_ref)
            .map_err(|e| HgbError::Storage(format!("Failed to read audit ledger from {:?}: {e}", path_ref)))?;
        let records: Vec<EnterpriseAuditRecord> = serde_json::from_str(&content)
            .map_err(|e| HgbError::Serialization(format!("Corrupt audit ledger JSON: {e}")))?;

        let ledger = Self {
            records,
            storage_path: Some(path_ref.to_path_buf()),
        };

        ledger.verify_integrity()?;
        Ok(ledger)
    }

    /// Persist ledger to disk if storage_path is configured
    pub fn persist(&self) -> Result<()> {
        if let Some(ref path) = self.storage_path {
            if let Some(parent) = path.parent() {
                let _ = fs::create_dir_all(parent);
            }
            let json = self.export_json()?;
            let mut file = File::create(path)
                .map_err(|e| HgbError::Storage(format!("Failed to create ledger file: {e}")))?;
            file.write_all(json.as_bytes())
                .map_err(|e| HgbError::Storage(format!("Failed to write ledger file: {e}")))?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_edition_feature_allowance() {
        let community = HgbEdition::Community;
        assert_eq!(community.is_enterprise(), false);
        assert_eq!(community.allows_feature("local_ollama"), true);
        assert_eq!(community.allows_feature("azure_openai"), false);
        assert_eq!(community.allows_feature("microsoft_mcp_bridge"), false);

        let enterprise = HgbEdition::Enterprise;
        assert_eq!(enterprise.is_enterprise(), true);
        assert_eq!(enterprise.allows_feature("azure_openai"), true);
        assert_eq!(enterprise.allows_feature("soc2_audit_ledger"), true);
    }

    #[test]
    fn test_license_generation_and_cryptographic_verification() {
        let secret = "contoso_enterprise_root_master_signing_key_99";
        let entitlements = EnterpriseEntitlements::default();

        let (license, token) = EnterpriseLicense::generate(
            "contoso-tenant-01",
            "Contoso Corporation",
            500,
            365,
            entitlements,
            secret,
        );

        assert_eq!(license.tenant_id, "contoso-tenant-01");
        assert_eq!(license.seats, 500);
        assert!(license.is_valid());

        // Verify valid token
        let verified = EnterpriseLicense::parse_and_verify(&token, secret).expect("Token must verify");
        assert_eq!(verified.tenant_id, "contoso-tenant-01");
        assert_eq!(verified.organization_name, "Contoso Corporation");

        // Tamper token payload
        let parts: Vec<&str> = token.split('.').collect();
        let tampered_payload = format!("HGB-ENT-AAAA{}", &parts[0][12..]);
        let tampered = format!("{}.{}", tampered_payload, parts[1]);
        assert!(EnterpriseLicense::parse_and_verify(&tampered, secret).is_err());

        // Tamper signature
        let tampered_sig = format!("{}.ffffffffffffffff", parts[0]);
        assert!(EnterpriseLicense::parse_and_verify(&tampered_sig, secret).is_err());

        // Wrong secret
        assert!(EnterpriseLicense::parse_and_verify(&token, "wrong_secret").is_err());
    }

    #[test]
    fn test_audit_ledger_hash_chain_and_tamper_detection() {
        let temp_dir = std::env::temp_dir().join(format!("ledger_test_{}", uuid::Uuid::new_v4().simple()));
        let ledger_path = temp_dir.join(".hgb").join("audit_ledger.json");
        let mut ledger = EnterpriseAuditLedger::new(Some(ledger_path.clone()));

        assert_eq!(ledger.records().len(), 1); // Genesis block
        assert!(ledger.verify_integrity().unwrap());

        // Record 1: Agent action
        let hash1 = ledger.record(
            "AgentToolCall",
            "autonomous_agent",
            serde_json::json!({ "tool": "sharepoint_read_spec", "target": "AUTH_SPEC.md" }),
        ).unwrap();
        assert!(!hash1.is_empty());

        // Record 2: Human approval
        let hash2 = ledger.record(
            "HumanApproval",
            "cto@contoso.com",
            serde_json::json!({ "decision": "Approved", "pr": 104 }),
        ).unwrap();
        assert!(!hash2.is_empty());

        assert_eq!(ledger.records().len(), 3);
        assert!(ledger.verify_integrity().unwrap());

        // Verify exports
        let json_export = ledger.export_json().unwrap();
        assert!(json_export.contains("sharepoint_read_spec"));

        let csv_export = ledger.export_csv().unwrap();
        assert!(csv_export.contains("HumanApproval"));

        // Tamper test: manually mutate a record in the chain
        ledger.records[1].actor = "malicious_actor".to_string();
        assert!(ledger.verify_integrity().is_err());

        let _ = fs::remove_dir_all(&temp_dir);
    }
}
