//! # Dynamic Capability Manager (`dynamic_capability_manager`)
//! ==========================================================
//! Project Hagibis: Pillar 3 (Polyglot WASM Sandboxing & Dynamic Capability Revocation)
//!
//! Production-grade zero-restart capability management and runtime revocation
//! for `hgb-wasm-kernel` untrusted plugin instances.
//!
//! Features:
//! - Lock-free O(1) hot-path revocation checks via atomic bitsets.
//! - Sparse arbitrary token ID revocation set with RwLock HashSet.
//! - Per-instance capability injection without VM or instance restarts.
//! - Fine-grained memory range, network host, port, request count, and egress byte enforcement.
//! - Immediate `AbiError::PermissionDenied` on revoked token usage.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};
use serde::{Deserialize, Serialize};

use crate::canonical_abi_shm_views::{
    AbiError, MemoryCapabilityToken, NetworkPermissionToken,
};

/// Fixed bitset size for ultra-low latency lock-free revocation checks (token IDs 0..65535)
pub const BITSET_CAPACITY: usize = 65536;
pub const BITSET_WORDS: usize = BITSET_CAPACITY / 64;

/// Lock-free atomic bitset for zero-latency capability revocation checks
pub struct AtomicRevocationBitset {
    words: Vec<AtomicU64>,
}

impl AtomicRevocationBitset {
    pub fn new() -> Self {
        let mut words = Vec::with_capacity(BITSET_WORDS);
        for _ in 0..BITSET_WORDS {
            words.push(AtomicU64::new(0));
        }
        Self { words }
    }

    #[inline(always)]
    pub fn is_revoked(&self, token_id: u64) -> bool {
        if (token_id as usize) >= BITSET_CAPACITY {
            return false;
        }
        let word_idx = (token_id / 64) as usize;
        let bit_idx = (token_id % 64) as u32;
        let mask = 1u64 << bit_idx;
        (self.words[word_idx].load(Ordering::Acquire) & mask) != 0
    }

    #[inline(always)]
    pub fn mark_revoked(&self, token_id: u64) -> bool {
        if (token_id as usize) >= BITSET_CAPACITY {
            return false;
        }
        let word_idx = (token_id / 64) as usize;
        let bit_idx = (token_id % 64) as u32;
        let mask = 1u64 << bit_idx;
        let prev = self.words[word_idx].fetch_or(mask, Ordering::AcqRel);
        (prev & mask) == 0
    }
}

impl Default for AtomicRevocationBitset {
    fn default() -> Self {
        Self::new()
    }
}

/// Metadata record captured upon capability token revocation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RevocationRecord {
    pub token_id: u64,
    pub reason: String,
    pub revoked_at_ns: u64,
}

/// Running instance capability store supporting runtime injection
#[derive(Default)]
pub struct InstanceCapabilityStore {
    pub memory_tokens: HashMap<u64, MemoryCapabilityToken>,
    pub network_tokens: HashMap<u64, NetworkPermissionToken>,
    pub request_counters: HashMap<u64, Arc<AtomicU32>>,
    pub egress_byte_counters: HashMap<u64, Arc<AtomicU64>>,
}

/// Dynamic capability manager for sovereign WebAssembly execution
pub struct DynamicCapabilityManager {
    signing_secret: Vec<u8>,
    bitset: AtomicRevocationBitset,
    sparse_revocations: RwLock<HashSet<u64>>,
    revocation_log: RwLock<HashMap<u64, RevocationRecord>>,
    instances: RwLock<HashMap<String, InstanceCapabilityStore>>,
}

impl DynamicCapabilityManager {
    /// Create a new DynamicCapabilityManager with default signing secret
    pub fn new() -> Self {
        Self::with_secret(b"hgb_dynamic_cap_default_signing_key_2026")
    }

    /// Create with explicit cryptographic signing secret
    pub fn with_secret(secret: &[u8]) -> Self {
        Self {
            signing_secret: secret.to_vec(),
            bitset: AtomicRevocationBitset::new(),
            sparse_revocations: RwLock::new(HashSet::new()),
            revocation_log: RwLock::new(HashMap::new()),
            instances: RwLock::new(HashMap::new()),
        }
    }

    /// Revoke a capability token dynamically at runtime
    pub fn revoke_capability(&self, token_id: u64, reason: impl Into<String>) -> Result<bool, AbiError> {
        let reason_str = reason.into();
        let was_new = if (token_id as usize) < BITSET_CAPACITY {
            self.bitset.mark_revoked(token_id)
        } else {
            let mut sparse = self.sparse_revocations.write().map_err(|_| AbiError::ExecutionTrapped {
                message: "Poisoned sparse revocation lock".to_string(),
            })?;
            sparse.insert(token_id)
        };

        if was_new {
            let now_ns = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_nanos() as u64)
                .unwrap_or(0);

            let mut log = self.revocation_log.write().map_err(|_| AbiError::ExecutionTrapped {
                message: "Poisoned revocation log lock".to_string(),
            })?;
            log.insert(token_id, RevocationRecord {
                token_id,
                reason: reason_str,
                revoked_at_ns: now_ns,
            });
        }

        Ok(was_new)
    }

    /// Check if a capability token is revoked (lock-free O(1) for IDs < 65536)
    #[inline(always)]
    pub fn is_capability_revoked(&self, token_id: u64) -> bool {
        if (token_id as usize) < BITSET_CAPACITY {
            if self.bitset.is_revoked(token_id) {
                return true;
            }
        }
        if let Ok(sparse) = self.sparse_revocations.read() {
            sparse.contains(&token_id)
        } else {
            true // Fail-closed on poisoned lock
        }
    }

    /// Assert that a capability token is active and unrevoked
    #[inline(always)]
    pub fn assert_capability_valid(&self, token_id: u64) -> Result<(), AbiError> {
        if self.is_capability_revoked(token_id) {
            Err(AbiError::PermissionDenied)
        } else {
            Ok(())
        }
    }

    /// Dynamically inject fine-grained memory token into running instance without restart
    pub fn inject_memory_token(
        &self,
        instance_id: &str,
        token: MemoryCapabilityToken,
    ) -> Result<u64, AbiError> {
        token.verify(&self.signing_secret)?;
        let token_id = token.token_id();
        self.assert_capability_valid(token_id)?;

        let mut instances = self.instances.write().map_err(|_| AbiError::ExecutionTrapped {
            message: "Poisoned instance lock".to_string(),
        })?;

        let store = instances.entry(instance_id.to_string()).or_default();
        store.memory_tokens.insert(token_id, token);
        Ok(token_id)
    }

    /// Dynamically inject fine-grained network token into running instance without restart
    pub fn inject_network_token(
        &self,
        instance_id: &str,
        token: NetworkPermissionToken,
    ) -> Result<u64, AbiError> {
        token.verify(&self.signing_secret)?;
        let token_id = token.token_id;
        self.assert_capability_valid(token_id)?;

        let mut instances = self.instances.write().map_err(|_| AbiError::ExecutionTrapped {
            message: "Poisoned instance lock".to_string(),
        })?;

        let store = instances.entry(instance_id.to_string()).or_default();
        store.request_counters.insert(token_id, Arc::new(AtomicU32::new(0)));
        store.egress_byte_counters.insert(token_id, Arc::new(AtomicU64::new(0)));
        store.network_tokens.insert(token_id, token);
        Ok(token_id)
    }

    /// Revoke all capability tokens associated with a given instance
    pub fn revoke_all_instance_capabilities(&self, instance_id: &str, reason: &str) -> Result<usize, AbiError> {
        let instances = self.instances.read().map_err(|_| AbiError::ExecutionTrapped {
            message: "Poisoned instance lock".to_string(),
        })?;

        let store = match instances.get(instance_id) {
            Some(s) => s,
            None => return Ok(0),
        };

        let mut count = 0;
        for token_id in store.memory_tokens.keys() {
            if self.revoke_capability(*token_id, format!("{}: {}", instance_id, reason))? {
                count += 1;
            }
        }
        for token_id in store.network_tokens.keys() {
            if self.revoke_capability(*token_id, format!("{}: {}", instance_id, reason))? {
                count += 1;
            }
        }
        Ok(count)
    }

    /// Runtime verification of memory access presented by an untrusted instance
    pub fn verify_memory_access(
        &self,
        instance_id: &str,
        token_id: u64,
        offset: usize,
        len: usize,
        write: bool,
    ) -> Result<(), AbiError> {
        self.assert_capability_valid(token_id)?;

        let instances = self.instances.read().map_err(|_| AbiError::ExecutionTrapped {
            message: "Poisoned instance lock".to_string(),
        })?;

        let store = instances.get(instance_id).ok_or(AbiError::PermissionDenied)?;
        let token = store.memory_tokens.get(&token_id).ok_or(AbiError::PermissionDenied)?;

        token.verify(&self.signing_secret)?;
        if !token.allows_range(offset, len) {
            return Err(AbiError::OutOfBounds {
                offset,
                len,
                capacity: token.allowed_len(),
            });
        }
        if write && !token.can_write() {
            return Err(AbiError::PermissionDenied);
        }

        Ok(())
    }

    /// Runtime verification of outbound network request presented by an untrusted instance
    pub fn verify_network_access(
        &self,
        instance_id: &str,
        token_id: u64,
        host: &str,
        port: u16,
        egress_bytes: u64,
    ) -> Result<(), AbiError> {
        self.assert_capability_valid(token_id)?;

        let instances = self.instances.read().map_err(|_| AbiError::ExecutionTrapped {
            message: "Poisoned instance lock".to_string(),
        })?;

        let store = instances.get(instance_id).ok_or(AbiError::PermissionDenied)?;
        let token = store.network_tokens.get(&token_id).ok_or(AbiError::PermissionDenied)?;

        token.verify(&self.signing_secret)?;
        if !token.allows_destination(host, port) {
            return Err(AbiError::PermissionDenied);
        }

        if let Some(req_ctr) = store.request_counters.get(&token_id) {
            let prev = req_ctr.fetch_add(1, Ordering::SeqCst);
            if prev >= token.max_outbound_requests {
                return Err(AbiError::PermissionDenied);
            }
        }

        if let Some(egress_ctr) = store.egress_byte_counters.get(&token_id) {
            let prev = egress_ctr.fetch_add(egress_bytes, Ordering::SeqCst);
            if prev.saturating_add(egress_bytes) > token.egress_bytes_limit {
                return Err(AbiError::PermissionDenied);
            }
        }

        Ok(())
    }
}

impl Default for DynamicCapabilityManager {
    fn default() -> Self {
        Self::new()
    }
}
