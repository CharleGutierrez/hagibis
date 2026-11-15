//! # Autopoietic BLAKE3 Cryptographic Evolution Ledger (`evolution_ledger`)
//!
//! Produces immutable, tamper-evident cryptographic evolution receipts binding
//! pre-patch AST hash, post-patch AST hash, OTel verification metrics, and sandbox logs
//! into an append-only BLAKE3 Merkle chain.

use std::collections::VecDeque;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

use hgb_core::error::{HgbError, Result};

pub const GENESIS_PARENT_HASH: &str = "0000000000000000000000000000000000000000000000000000000000000000";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EvolutionReceipt {
    pub receipt_id: String,
    pub prev_receipt_hash: String,
    pub cycle_timestamp_ms: u64,
    pub target_crate: String,
    pub target_file: String,
    pub target_function: String,
    pub strategy: String,
    pub anomaly_kind: String,
    pub pre_patch_ast_hash: String,
    pub post_patch_ast_hash: String,
    pub patch_diff: String,
    pub observed_metric_before: f64,
    pub observed_metric_after: f64,
    pub speedup_percentage: f64,
    pub sandbox_run_id: String,
    pub sandbox_exit_code: i32,
    pub sandbox_logs_hash: String,
    pub pgo_profile_applied: bool,
    pub hot_reloaded: bool,
    pub blake3_provenance_hash: String,
}

impl EvolutionReceipt {
    pub fn compute_canonical_hash(&self) -> String {
        let mut hasher = blake3::Hasher::new();
        hasher.update(self.receipt_id.as_bytes());
        hasher.update(self.prev_receipt_hash.as_bytes());
        hasher.update(&self.cycle_timestamp_ms.to_le_bytes());
        hasher.update(self.target_crate.as_bytes());
        hasher.update(self.target_file.as_bytes());
        hasher.update(self.target_function.as_bytes());
        hasher.update(self.strategy.as_bytes());
        hasher.update(self.anomaly_kind.as_bytes());
        hasher.update(self.pre_patch_ast_hash.as_bytes());
        hasher.update(self.post_patch_ast_hash.as_bytes());
        hasher.update(self.patch_diff.as_bytes());
        hasher.update(&self.observed_metric_before.to_le_bytes());
        hasher.update(&self.observed_metric_after.to_le_bytes());
        hasher.update(&self.speedup_percentage.to_le_bytes());
        hasher.update(self.sandbox_run_id.as_bytes());
        hasher.update(&self.sandbox_exit_code.to_le_bytes());
        hasher.update(self.sandbox_logs_hash.as_bytes());
        hasher.update(&[self.pgo_profile_applied as u8]);
        hasher.update(&[self.hot_reloaded as u8]);
        hasher.finalize().to_hex().to_string()
    }
}

pub struct EvolutionLedger {
    ledger_file_path: PathBuf,
    receipts: Arc<RwLock<Vec<EvolutionReceipt>>>,
}

impl EvolutionLedger {
    pub fn new(ledger_path: impl Into<PathBuf>) -> Self {
        Self {
            ledger_file_path: ledger_path.into(),
            receipts: Arc::new(RwLock::new(Vec::new())),
        }
    }

    pub async fn append_receipt(&self, mut receipt: EvolutionReceipt) -> Result<String> {
        let mut chain = self.receipts.write().await;
        let prev_hash = chain
            .last()
            .map(|r| r.blake3_provenance_hash.clone())
            .unwrap_or_else(|| GENESIS_PARENT_HASH.to_string());

        receipt.prev_receipt_hash = prev_hash;
        receipt.blake3_provenance_hash = receipt.compute_canonical_hash();

        if let Some(parent) = self.ledger_file_path.parent() {
            let _ = fs::create_dir_all(parent);
        }

        if let Ok(json) = serde_json::to_string(&receipt) {
            if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(&self.ledger_file_path) {
                let _ = writeln!(f, "{}", json);
            }
        }

        let hash = receipt.blake3_provenance_hash.clone();
        chain.push(receipt);
        Ok(hash)
    }

    pub async fn verify_chain(&self) -> Result<bool> {
        let chain = self.receipts.read().await;
        if chain.is_empty() { return Ok(true); }

        let mut expected_prev = GENESIS_PARENT_HASH.to_string();
        for r in chain.iter() {
            if r.prev_receipt_hash != expected_prev { return Ok(false); }
            if r.compute_canonical_hash() != r.blake3_provenance_hash { return Ok(false); }
            expected_prev = r.blake3_provenance_hash.clone();
        }
        Ok(true)
    }

    pub async fn total_evolutions(&self) -> usize {
        self.receipts.read().await.len()
    }

    pub async fn latest_receipt(&self) -> Option<EvolutionReceipt> {
        self.receipts.read().await.last().cloned()
    }
}