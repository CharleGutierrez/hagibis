//! # Pillar III: MicroVM CRDT State Sync Engine (`microvm_crdt_sync`)
//!
//! Production-grade decentralized CRDT (Conflict-free Replicated Data Type) state synchronizer
//! designed specifically for multi-agent workflows executing concurrently across Firecracker microVM sandboxes.
//!
//! ## Problem Solved
//! When autonomous sub-agents execute in parallel microVM sandboxes (e.g. `vm-frontend`, `vm-backend`, `vm-tester`),
//! writing shared episodic memory, tool artifacts, and scratchpad thoughts to a single SQLite database or file lock
//! causes severe lock contention (`database is locked`, `SQLITE_BUSY`).
//!
//! ## Solution Architecture
//! `MicroVmCrdtStateSync` gives each microVM replica an in-memory, lock-free CRDT state representation:
//! 1. `OrSetMemory<EpisodicFact>`: Observed-Remove Set for agent episodic discoveries and facts.
//! 2. `OrSetMemory<ScratchpadThought>`: Observed-Remove Set for multi-step reasoning thoughts.
//! 3. `OrSetMemory<ToolArtifactRecord>`: Observed-Remove Set for generated patches, logs, and binaries.
//! 4. `SharedStateVariable`: Last-Write-Wins (LWW) register with Lamport timestamp tie-breaking for shared workspace variables.
//! 5. `LwwVectorRegister`: LWW semantic embedding store from `distributed_crdt_mesh`.
//!
//! ## Deterministic Convergence
//! All CRDT merge operations are mathematically associative, commutative, and idempotent:
//! Any sequence or interleaving of replica merges yields the exact same global state across all microVMs.

use blake3::Hasher;
use hgb_core::{HgbError, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::distributed_crdt_mesh::{
    ClockRelation, LwwVectorRegister, MemoryTag, OrSetMemory, VectorClock, VectorEmbeddingRecord,
};

// -----------------------------------------------------------------------------
// 1. MicroVM Domain Data Types
// -----------------------------------------------------------------------------

/// Episodic fact discovered by an agent running in a microVM sandbox
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct EpisodicFact {
    pub fact_id: String,
    pub vm_id: String,
    pub agent_role: String,
    pub category: String,
    pub content: String,
    pub confidence: u32,
    pub created_at_ms: u64,
}

impl EpisodicFact {
    pub fn new(
        fact_id: impl Into<String>,
        vm_id: impl Into<String>,
        agent_role: impl Into<String>,
        category: impl Into<String>,
        content: impl Into<String>,
        confidence: u32,
    ) -> Self {
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        Self {
            fact_id: fact_id.into(),
            vm_id: vm_id.into(),
            agent_role: agent_role.into(),
            category: category.into(),
            content: content.into(),
            confidence,
            created_at_ms: now_ms,
        }
    }
}

/// Scratchpad reasoning step recorded by an agent within its microVM
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ScratchpadThought {
    pub thought_id: String,
    pub vm_id: String,
    pub agent_role: String,
    pub step_index: usize,
    pub thought: String,
    pub proposed_action: Option<String>,
    pub status: String,
    pub created_at_ms: u64,
}

impl ScratchpadThought {
    pub fn new(
        thought_id: impl Into<String>,
        vm_id: impl Into<String>,
        agent_role: impl Into<String>,
        step_index: usize,
        thought: impl Into<String>,
        proposed_action: Option<String>,
        status: impl Into<String>,
    ) -> Self {
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        Self {
            thought_id: thought_id.into(),
            vm_id: vm_id.into(),
            agent_role: agent_role.into(),
            step_index,
            thought: thought.into(),
            proposed_action,
            status: status.into(),
            created_at_ms: now_ms,
        }
    }
}

/// Tool execution artifact (e.g. patch diff, test log, compiled binary)
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ToolArtifactRecord {
    pub artifact_id: String,
    pub vm_id: String,
    pub path: String,
    pub content_hash: String,
    pub artifact_type: String,
    pub size_bytes: usize,
    pub created_at_ms: u64,
}

impl ToolArtifactRecord {
    pub fn new(
        artifact_id: impl Into<String>,
        vm_id: impl Into<String>,
        path: impl Into<String>,
        content: &[u8],
        artifact_type: impl Into<String>,
    ) -> Self {
        let mut hasher = Hasher::new();
        hasher.update(content);
        let content_hash = hasher.finalize().to_hex().to_string();

        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        Self {
            artifact_id: artifact_id.into(),
            vm_id: vm_id.into(),
            path: path.into(),
            content_hash,
            artifact_type: artifact_type.into(),
            size_bytes: content.len(),
            created_at_ms: now_ms,
        }
    }
}

/// Last-Write-Wins (LWW) register for shared workspace state variables
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SharedStateVariable {
    pub key: String,
    pub value: String,
    pub lamport_ts: u64,
    pub wall_clock_ms: u64,
    pub writer_vm_id: String,
    pub is_tombstone: bool,
}

impl SharedStateVariable {
    pub fn new(
        key: impl Into<String>,
        value: impl Into<String>,
        writer_vm_id: impl Into<String>,
        lamport_ts: u64,
    ) -> Self {
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        Self {
            key: key.into(),
            value: value.into(),
            lamport_ts,
            wall_clock_ms: now_ms,
            writer_vm_id: writer_vm_id.into(),
            is_tombstone: false,
        }
    }

    pub fn tombstone(
        key: impl Into<String>,
        writer_vm_id: impl Into<String>,
        lamport_ts: u64,
    ) -> Self {
        let mut var = Self::new(key, "", writer_vm_id, lamport_ts);
        var.is_tombstone = true;
        var
    }

    /// Reconciles with remote variable. Deterministic tie-breaking on:
    /// (1) Lamport timestamp, (2) wall clock, (3) writer VM ID.
    pub fn merge(&mut self, remote: &SharedStateVariable) -> bool {
        let remote_wins = if remote.lamport_ts > self.lamport_ts {
            true
        } else if remote.lamport_ts == self.lamport_ts {
            if remote.wall_clock_ms > self.wall_clock_ms {
                true
            } else if remote.wall_clock_ms == self.wall_clock_ms {
                remote.writer_vm_id > self.writer_vm_id
            } else {
                false
            }
        } else {
            false
        };

        if remote_wins {
            self.value = remote.value.clone();
            self.lamport_ts = remote.lamport_ts;
            self.wall_clock_ms = remote.wall_clock_ms;
            self.writer_vm_id = remote.writer_vm_id.clone();
            self.is_tombstone = remote.is_tombstone;
            true
        } else {
            false
        }
    }
}

// -----------------------------------------------------------------------------
// 2. MicroVM Delta & Synchronization Payloads
// -----------------------------------------------------------------------------

/// Delta payload exchanged between microVM replicas and the coordinator
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MicroVmSyncDelta {
    pub origin_vm_id: String,
    pub origin_role: String,
    pub baseline_clock: VectorClock,
    pub current_clock: VectorClock,
    pub facts_delta: OrSetMemory<EpisodicFact>,
    pub thoughts_delta: OrSetMemory<ScratchpadThought>,
    pub artifacts_delta: OrSetMemory<ToolArtifactRecord>,
    pub shared_vars_delta: Vec<SharedStateVariable>,
    pub vector_registers_delta: Vec<LwwVectorRegister>,
    pub timestamp_ms: u64,
    pub blake3_checksum: String,
}

impl MicroVmSyncDelta {
    /// Compute cryptographic Blake3 checksum over the serialized delta contents
    pub fn compute_checksum(
        origin_vm_id: &str,
        origin_role: &str,
        current_clock: &VectorClock,
        facts: &OrSetMemory<EpisodicFact>,
        thoughts: &OrSetMemory<ScratchpadThought>,
        artifacts: &OrSetMemory<ToolArtifactRecord>,
        vars: &[SharedStateVariable],
        vectors: &[LwwVectorRegister],
        timestamp_ms: u64,
    ) -> String {
        let mut hasher = Hasher::new();
        hasher.update(origin_vm_id.as_bytes());
        hasher.update(origin_role.as_bytes());
        hasher.update(&timestamp_ms.to_le_bytes());
        if let Ok(clock_bytes) = serde_json::to_vec(current_clock) {
            hasher.update(&clock_bytes);
        }
        if let Ok(f_bytes) = serde_json::to_vec(facts) {
            hasher.update(&f_bytes);
        }
        if let Ok(t_bytes) = serde_json::to_vec(thoughts) {
            hasher.update(&t_bytes);
        }
        if let Ok(a_bytes) = serde_json::to_vec(artifacts) {
            hasher.update(&a_bytes);
        }
        if let Ok(v_bytes) = serde_json::to_vec(vars) {
            hasher.update(&v_bytes);
        }
        if let Ok(vec_bytes) = serde_json::to_vec(vectors) {
            hasher.update(&vec_bytes);
        }
        hasher.finalize().to_hex().to_string()
    }

    /// Verify integrity of the received delta
    pub fn verify_checksum(&self) -> Result<()> {
        let computed = Self::compute_checksum(
            &self.origin_vm_id,
            &self.origin_role,
            &self.current_clock,
            &self.facts_delta,
            &self.thoughts_delta,
            &self.artifacts_delta,
            &self.shared_vars_delta,
            &self.vector_registers_delta,
            self.timestamp_ms,
        );

        if computed == self.blake3_checksum {
            Ok(())
        } else {
            Err(HgbError::Storage(format!(
                "MicroVM delta checksum mismatch from '{}': expected {}, computed {}",
                self.origin_vm_id, self.blake3_checksum, computed
            )))
        }
    }

    pub fn to_json(&self) -> Result<String> {
        serde_json::to_string(self).map_err(|e| HgbError::Serialization(e.to_string()))
    }

    pub fn from_json(json_str: &str) -> Result<Self> {
        let delta: Self = serde_json::from_str(json_str)
            .map_err(|e| HgbError::Serialization(e.to_string()))?;
        delta.verify_checksum()?;
        Ok(delta)
    }
}

/// Detailed report generated after a CRDT merge
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MicroVmSyncReport {
    pub local_vm_id: String,
    pub remote_vm_id: String,
    pub clock_relation: ClockRelation,
    pub facts_added: usize,
    pub facts_removed: usize,
    pub thoughts_added: usize,
    pub thoughts_removed: usize,
    pub artifacts_added: usize,
    pub artifacts_removed: usize,
    pub shared_vars_updated: usize,
    pub vectors_updated: usize,
    pub total_active_facts: usize,
    pub total_active_thoughts: usize,
    pub total_active_artifacts: usize,
    pub total_active_vars: usize,
    pub total_active_vectors: usize,
    pub merged_clock: BTreeMap<String, u64>,
}

// -----------------------------------------------------------------------------
// 3. MicroVM Replica
// -----------------------------------------------------------------------------

/// In-memory state replica for a single Firecracker microVM instance
pub struct MicroVmReplica {
    pub vm_id: String,
    pub role: String,
    pub vector_clock: VectorClock,
    pub lamport_counter: AtomicU64,
    pub facts: OrSetMemory<EpisodicFact>,
    pub thoughts: OrSetMemory<ScratchpadThought>,
    pub artifacts: OrSetMemory<ToolArtifactRecord>,
    pub shared_vars: HashMap<String, SharedStateVariable>,
    pub vector_registers: HashMap<String, LwwVectorRegister>,
    pub last_synced_ms: u64,
}

impl MicroVmReplica {
    pub fn new(vm_id: impl Into<String>, role: impl Into<String>) -> Self {
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        Self {
            vm_id: vm_id.into(),
            role: role.into(),
            vector_clock: VectorClock::new(),
            lamport_counter: AtomicU64::new(0),
            facts: OrSetMemory::new(),
            thoughts: OrSetMemory::new(),
            artifacts: OrSetMemory::new(),
            shared_vars: HashMap::new(),
            vector_registers: HashMap::new(),
            last_synced_ms: now_ms,
        }
    }

    pub fn next_lamport(&self) -> u64 {
        self.lamport_counter.fetch_add(1, Ordering::SeqCst) + 1
    }

    pub fn observe_lamport(&self, remote_ts: u64) {
        let _ = self.lamport_counter.fetch_max(remote_ts, Ordering::SeqCst);
    }

    /// Record episodic discovery without DB lock
    pub fn record_fact(&mut self, fact: EpisodicFact) -> MemoryTag {
        let lamport = self.next_lamport();
        self.vector_clock.increment(&self.vm_id);
        self.facts.add(fact, &self.vm_id, lamport)
    }

    /// Record scratchpad thought without DB lock
    pub fn record_thought(&mut self, thought: ScratchpadThought) -> MemoryTag {
        let lamport = self.next_lamport();
        self.vector_clock.increment(&self.vm_id);
        self.thoughts.add(thought, &self.vm_id, lamport)
    }

    /// Record tool execution artifact without DB lock
    pub fn record_artifact(&mut self, artifact: ToolArtifactRecord) -> MemoryTag {
        let lamport = self.next_lamport();
        self.vector_clock.increment(&self.vm_id);
        self.artifacts.add(artifact, &self.vm_id, lamport)
    }

    /// Remove an artifact by path using OR-Set tombstoning
    pub fn remove_artifact_by_path(&mut self, path: &str) -> usize {
        self.vector_clock.increment(&self.vm_id);
        self.artifacts.remove_by_predicate(|a| a.path == path)
    }

    /// Set shared workspace variable with Last-Write-Wins semantics
    pub fn set_shared_var(&mut self, key: &str, value: &str) -> SharedStateVariable {
        let lamport = self.next_lamport();
        self.vector_clock.increment(&self.vm_id);
        let var = SharedStateVariable::new(key, value, &self.vm_id, lamport);
        self.shared_vars.insert(key.to_string(), var.clone());
        var
    }

    /// Delete shared workspace variable via tombstone
    pub fn delete_shared_var(&mut self, key: &str) -> bool {
        if self.shared_vars.contains_key(key) {
            let lamport = self.next_lamport();
            self.vector_clock.increment(&self.vm_id);
            let tombstone = SharedStateVariable::tombstone(key, &self.vm_id, lamport);
            self.shared_vars.insert(key.to_string(), tombstone);
            true
        } else {
            false
        }
    }

    /// Upsert semantic vector embedding
    pub fn upsert_vector(
        &mut self,
        id: &str,
        collection: &str,
        embedding: Vec<f32>,
        metadata: &str,
    ) -> LwwVectorRegister {
        let lamport = self.next_lamport();
        self.vector_clock.increment(&self.vm_id);
        let record = VectorEmbeddingRecord::new(id, collection, embedding, metadata);
        let reg = LwwVectorRegister::new(record, &self.vm_id, lamport);
        self.vector_registers.insert(id.to_string(), reg.clone());
        reg
    }

    /// Active episodic facts (excluding deleted)
    pub fn active_facts(&self) -> Vec<EpisodicFact> {
        self.facts.elements()
    }

    /// Active scratchpad thoughts
    pub fn active_thoughts(&self) -> Vec<ScratchpadThought> {
        self.thoughts.elements()
    }

    /// Active tool artifacts
    pub fn active_artifacts(&self) -> Vec<ToolArtifactRecord> {
        self.artifacts.elements()
    }

    /// Active shared variables (excluding tombstones)
    pub fn active_shared_vars(&self) -> HashMap<String, String> {
        self.shared_vars
            .iter()
            .filter(|(_, v)| !v.is_tombstone)
            .map(|(k, v)| (k.clone(), v.value.clone()))
            .collect()
    }

    /// Active vector embeddings (excluding tombstones)
    pub fn active_vectors(&self) -> Vec<VectorEmbeddingRecord> {
        self.vector_registers
            .values()
            .filter(|r| !r.is_tombstone)
            .map(|r| r.record.clone())
            .collect()
    }

    /// Generate delta difference beyond baseline clock
    pub fn generate_delta(&self, baseline: &VectorClock) -> MicroVmSyncDelta {
        let mut delta_vars = Vec::new();
        for var in self.shared_vars.values() {
            let base_ts = baseline.get(&var.writer_vm_id);
            if var.lamport_ts > base_ts {
                delta_vars.push(var.clone());
            }
        }

        let mut delta_vectors = Vec::new();
        for reg in self.vector_registers.values() {
            let base_ts = baseline.get(&reg.node_id);
            if reg.lamport_ts > base_ts {
                delta_vectors.push(reg.clone());
            }
        }

        let facts_delta = self.facts.delta_since(baseline);
        let thoughts_delta = self.thoughts.delta_since(baseline);
        let artifacts_delta = self.artifacts.delta_since(baseline);

        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        let checksum = MicroVmSyncDelta::compute_checksum(
            &self.vm_id,
            &self.role,
            &self.vector_clock,
            &facts_delta,
            &thoughts_delta,
            &artifacts_delta,
            &delta_vars,
            &delta_vectors,
            now_ms,
        );

        MicroVmSyncDelta {
            origin_vm_id: self.vm_id.clone(),
            origin_role: self.role.clone(),
            baseline_clock: baseline.clone(),
            current_clock: self.vector_clock.clone(),
            facts_delta,
            thoughts_delta,
            artifacts_delta,
            shared_vars_delta: delta_vars,
            vector_registers_delta: delta_vectors,
            timestamp_ms: now_ms,
            blake3_checksum: checksum,
        }
    }

    /// Apply delta payload deterministically with zero data loss
    pub fn apply_delta(&mut self, delta: &MicroVmSyncDelta) -> Result<MicroVmSyncReport> {
        delta.verify_checksum()?;

        let relation = self.vector_clock.relation(&delta.current_clock);
        self.vector_clock.merge(&delta.current_clock);

        // Update local Lamport clock
        let mut max_incoming_lamport = 0u64;
        for var in &delta.shared_vars_delta {
            max_incoming_lamport = max_incoming_lamport.max(var.lamport_ts);
        }
        for vec in &delta.vector_registers_delta {
            max_incoming_lamport = max_incoming_lamport.max(vec.lamport_ts);
        }
        for (tag, _) in &delta.facts_delta.add_set {
            max_incoming_lamport = max_incoming_lamport.max(tag.lamport_ts);
        }
        for (tag, _) in &delta.thoughts_delta.add_set {
            max_incoming_lamport = max_incoming_lamport.max(tag.lamport_ts);
        }
        for (tag, _) in &delta.artifacts_delta.add_set {
            max_incoming_lamport = max_incoming_lamport.max(tag.lamport_ts);
        }
        if max_incoming_lamport > 0 {
            self.observe_lamport(max_incoming_lamport);
        }

        // Merge OR-Sets
        let (facts_added, facts_removed) = self.facts.merge(&delta.facts_delta);
        let (thoughts_added, thoughts_removed) = self.thoughts.merge(&delta.thoughts_delta);
        let (artifacts_added, artifacts_removed) = self.artifacts.merge(&delta.artifacts_delta);

        // Merge LWW shared variables
        let mut shared_vars_updated = 0;
        for remote_var in &delta.shared_vars_delta {
            match self.shared_vars.get_mut(&remote_var.key) {
                Some(local_var) => {
                    if local_var.merge(remote_var) {
                        shared_vars_updated += 1;
                    }
                }
                None => {
                    self.shared_vars
                        .insert(remote_var.key.clone(), remote_var.clone());
                    shared_vars_updated += 1;
                }
            }
        }

        // Merge LWW vector registers
        let mut vectors_updated = 0;
        for remote_reg in &delta.vector_registers_delta {
            match self.vector_registers.get_mut(&remote_reg.record.id) {
                Some(local_reg) => {
                    if local_reg.merge(remote_reg) {
                        vectors_updated += 1;
                    }
                }
                None => {
                    self.vector_registers
                        .insert(remote_reg.record.id.clone(), remote_reg.clone());
                    vectors_updated += 1;
                }
            }
        }

        self.last_synced_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        let total_active_facts = self.active_facts().len();
        let total_active_thoughts = self.active_thoughts().len();
        let total_active_artifacts = self.active_artifacts().len();
        let total_active_vars = self.active_shared_vars().len();
        let total_active_vectors = self.active_vectors().len();

        Ok(MicroVmSyncReport {
            local_vm_id: self.vm_id.clone(),
            remote_vm_id: delta.origin_vm_id.clone(),
            clock_relation: relation,
            facts_added,
            facts_removed,
            thoughts_added,
            thoughts_removed,
            artifacts_added,
            artifacts_removed,
            shared_vars_updated,
            vectors_updated,
            total_active_facts,
            total_active_thoughts,
            total_active_artifacts,
            total_active_vars,
            total_active_vectors,
            merged_clock: self.vector_clock.clock.clone(),
        })
    }
}

// -----------------------------------------------------------------------------
// 4. MicroVmCrdtStateSync Coordinator
// -----------------------------------------------------------------------------

/// Production-grade CRDT state synchronizer for concurrent Firecracker microVMs
pub struct MicroVmCrdtStateSync {
    pub coordinator_id: String,
    pub coordinator: Arc<RwLock<MicroVmReplica>>,
    pub replicas: Arc<RwLock<HashMap<String, Arc<RwLock<MicroVmReplica>>>>>,
}

impl MicroVmCrdtStateSync {
    /// Initialize synchronizer with coordinator ID (e.g. `host-coordinator`)
    pub fn new(coordinator_id: impl Into<String>) -> Self {
        let cid = coordinator_id.into();
        let coord_replica = MicroVmReplica::new(&cid, "orchestrator");

        Self {
            coordinator_id: cid,
            coordinator: Arc::new(RwLock::new(coord_replica)),
            replicas: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Register a new microVM replica with its dedicated agent role
    pub fn register_microvm(&self, vm_id: &str, role: &str) -> Result<()> {
        let mut guard = self.replicas.write().map_err(|_| {
            HgbError::Storage("Failed to acquire write lock on replicas".to_string())
        })?;

        if guard.contains_key(vm_id) {
            return Ok(());
        }

        let replica = MicroVmReplica::new(vm_id, role);
        guard.insert(vm_id.to_string(), Arc::new(RwLock::new(replica)));
        Ok(())
    }

    /// Check if a microVM replica is registered
    pub fn has_replica(&self, vm_id: &str) -> bool {
        self.replicas
            .read()
            .map(|r| r.contains_key(vm_id))
            .unwrap_or(false)
    }

    /// Get reference to a microVM replica
    pub fn get_replica(&self, vm_id: &str) -> Result<Arc<RwLock<MicroVmReplica>>> {
        let guard = self.replicas.read().map_err(|_| {
            HgbError::Storage("Failed to acquire read lock on replicas".to_string())
        })?;

        guard
            .get(vm_id)
            .cloned()
            .ok_or_else(|| HgbError::NotFound(format!("MicroVM replica '{}' not found", vm_id)))
    }

    /// List all registered microVM IDs
    pub fn list_vm_ids(&self) -> Vec<String> {
        self.replicas
            .read()
            .map(|r| r.keys().cloned().collect())
            .unwrap_or_default()
    }

    /// Count registered microVMs
    pub fn active_vm_count(&self) -> usize {
        self.replicas.read().map(|r| r.len()).unwrap_or(0)
    }

    /// Record episodic fact directly into a specific microVM's state
    pub fn record_fact(&self, vm_id: &str, fact: EpisodicFact) -> Result<MemoryTag> {
        let replica_arc = self.get_replica(vm_id)?;
        let mut replica = replica_arc.write().map_err(|_| {
            HgbError::Storage(format!("Write lock poisoned for replica '{}'", vm_id))
        })?;
        Ok(replica.record_fact(fact))
    }

    /// Record scratchpad thought directly into a specific microVM's state
    pub fn record_thought(
        &self,
        vm_id: &str,
        thought: ScratchpadThought,
    ) -> Result<MemoryTag> {
        let replica_arc = self.get_replica(vm_id)?;
        let mut replica = replica_arc.write().map_err(|_| {
            HgbError::Storage(format!("Write lock poisoned for replica '{}'", vm_id))
        })?;
        Ok(replica.record_thought(thought))
    }

    /// Record tool execution artifact into a specific microVM's state
    pub fn record_artifact(
        &self,
        vm_id: &str,
        artifact: ToolArtifactRecord,
    ) -> Result<MemoryTag> {
        let replica_arc = self.get_replica(vm_id)?;
        let mut replica = replica_arc.write().map_err(|_| {
            HgbError::Storage(format!("Write lock poisoned for replica '{}'", vm_id))
        })?;
        Ok(replica.record_artifact(artifact))
    }

    /// Remove tool artifact by path from a specific microVM's state
    pub fn remove_artifact_by_path(&self, vm_id: &str, path: &str) -> Result<usize> {
        let replica_arc = self.get_replica(vm_id)?;
        let mut replica = replica_arc.write().map_err(|_| {
            HgbError::Storage(format!("Write lock poisoned for replica '{}'", vm_id))
        })?;
        Ok(replica.remove_artifact_by_path(path))
    }

    /// Set shared workspace variable in a microVM's state
    pub fn set_shared_var(&self, vm_id: &str, key: &str, value: &str) -> Result<()> {
        let replica_arc = self.get_replica(vm_id)?;
        let mut replica = replica_arc.write().map_err(|_| {
            HgbError::Storage(format!("Write lock poisoned for replica '{}'", vm_id))
        })?;
        replica.set_shared_var(key, value);
        Ok(())
    }

    /// Get shared variable value from a microVM's state
    pub fn get_shared_var(&self, vm_id: &str, key: &str) -> Result<Option<String>> {
        let replica_arc = self.get_replica(vm_id)?;
        let replica = replica_arc.read().map_err(|_| {
            HgbError::Storage(format!("Read lock poisoned for replica '{}'", vm_id))
        })?;
        Ok(replica.shared_vars.get(key).and_then(|v| {
            if v.is_tombstone {
                None
            } else {
                Some(v.value.clone())
            }
        }))
    }

    /// Upsert semantic vector embedding into a microVM's state
    pub fn upsert_vector(
        &self,
        vm_id: &str,
        id: &str,
        collection: &str,
        embedding: Vec<f32>,
        metadata: &str,
    ) -> Result<LwwVectorRegister> {
        let replica_arc = self.get_replica(vm_id)?;
        let mut replica = replica_arc.write().map_err(|_| {
            HgbError::Storage(format!("Write lock poisoned for replica '{}'", vm_id))
        })?;
        Ok(replica.upsert_vector(id, collection, embedding, metadata))
    }

    /// Sync changes from a microVM replica into the coordinator
    pub fn sync_vm_to_coordinator(&self, vm_id: &str) -> Result<MicroVmSyncReport> {
        let replica_arc = self.get_replica(vm_id)?;
        let (delta, _origin) = {
            let replica = replica_arc.read().map_err(|_| {
                HgbError::Storage(format!("Read lock poisoned for replica '{}'", vm_id))
            })?;
            let coord = self.coordinator.read().map_err(|_| {
                HgbError::Storage("Read lock poisoned for coordinator".to_string())
            })?;
            (replica.generate_delta(&coord.vector_clock), replica.vm_id.clone())
        };

        let mut coord = self.coordinator.write().map_err(|_| {
            HgbError::Storage("Write lock poisoned for coordinator".to_string())
        })?;
        coord.apply_delta(&delta)
    }

    /// Sync changes from coordinator into a microVM replica
    pub fn sync_coordinator_to_vm(&self, vm_id: &str) -> Result<MicroVmSyncReport> {
        let replica_arc = self.get_replica(vm_id)?;
        let delta = {
            let coord = self.coordinator.read().map_err(|_| {
                HgbError::Storage("Read lock poisoned for coordinator".to_string())
            })?;
            let replica = replica_arc.read().map_err(|_| {
                HgbError::Storage(format!("Read lock poisoned for replica '{}'", vm_id))
            })?;
            coord.generate_delta(&replica.vector_clock)
        };

        let mut replica = replica_arc.write().map_err(|_| {
            HgbError::Storage(format!("Write lock poisoned for replica '{}'", vm_id))
        })?;
        replica.apply_delta(&delta)
    }

    /// Cross-synchronize directly between two microVM replicas without host intervention
    pub fn cross_sync_vms(
        &self,
        vm_a: &str,
        vm_b: &str,
    ) -> Result<(MicroVmSyncReport, MicroVmSyncReport)> {
        let rep_a_arc = self.get_replica(vm_a)?;
        let rep_b_arc = self.get_replica(vm_b)?;

        // Phase 1: delta from A to B
        let delta_a = {
            let rep_a = rep_a_arc.read().map_err(|_| {
                HgbError::Storage(format!("Read lock poisoned for replica '{}'", vm_a))
            })?;
            let rep_b = rep_b_arc.read().map_err(|_| {
                HgbError::Storage(format!("Read lock poisoned for replica '{}'", vm_b))
            })?;
            rep_a.generate_delta(&rep_b.vector_clock)
        };

        let report_b = {
            let mut rep_b = rep_b_arc.write().map_err(|_| {
                HgbError::Storage(format!("Write lock poisoned for replica '{}'", vm_b))
            })?;
            rep_b.apply_delta(&delta_a)?
        };

        // Phase 2: delta from B back to A
        let delta_b = {
            let rep_b = rep_b_arc.read().map_err(|_| {
                HgbError::Storage(format!("Read lock poisoned for replica '{}'", vm_b))
            })?;
            let rep_a = rep_a_arc.read().map_err(|_| {
                HgbError::Storage(format!("Read lock poisoned for replica '{}'", vm_a))
            })?;
            rep_b.generate_delta(&rep_a.vector_clock)
        };

        let report_a = {
            let mut rep_a = rep_a_arc.write().map_err(|_| {
                HgbError::Storage(format!("Write lock poisoned for replica '{}'", vm_a))
            })?;
            rep_a.apply_delta(&delta_b)?
        };

        Ok((report_b, report_a))
    }

    /// Synchronize all registered microVM replicas with the coordinator
    /// (Phase 1: all VMs push to coordinator; Phase 2: coordinator pushes to all VMs)
    pub fn sync_all(&self) -> Result<Vec<MicroVmSyncReport>> {
        let vm_ids = self.list_vm_ids();
        let mut reports = Vec::with_capacity(vm_ids.len() * 2);

        // Phase 1: Push from all microVMs to coordinator
        for vm_id in &vm_ids {
            let rep = self.sync_vm_to_coordinator(vm_id)?;
            reports.push(rep);
        }

        // Phase 2: Distribute merged state back to all microVMs
        for vm_id in &vm_ids {
            let rep = self.sync_coordinator_to_vm(vm_id)?;
            reports.push(rep);
        }

        Ok(reports)
    }

    /// Verifies deterministic mathematical convergence across all replicas and coordinator.
    /// Returns Ok(true) if active facts, thoughts, artifacts, and shared variables are identical.
    pub fn verify_convergence(&self) -> Result<bool> {
        let coord = self.coordinator.read().map_err(|_| {
            HgbError::Storage("Read lock poisoned for coordinator".to_string())
        })?;

        let coord_facts: HashSet<EpisodicFact> = coord.active_facts().into_iter().collect();
        let coord_thoughts: HashSet<ScratchpadThought> =
            coord.active_thoughts().into_iter().collect();
        let coord_artifacts: HashSet<ToolArtifactRecord> =
            coord.active_artifacts().into_iter().collect();
        let coord_vars = coord.active_shared_vars();

        let vm_ids = self.list_vm_ids();
        for vm_id in vm_ids {
            let rep_arc = self.get_replica(&vm_id)?;
            let rep = rep_arc.read().map_err(|_| {
                HgbError::Storage(format!("Read lock poisoned for replica '{}'", vm_id))
            })?;

            let rep_facts: HashSet<EpisodicFact> = rep.active_facts().into_iter().collect();
            if rep_facts != coord_facts {
                return Ok(false);
            }

            let rep_thoughts: HashSet<ScratchpadThought> =
                rep.active_thoughts().into_iter().collect();
            if rep_thoughts != coord_thoughts {
                return Ok(false);
            }

            let rep_artifacts: HashSet<ToolArtifactRecord> =
                rep.active_artifacts().into_iter().collect();
            if rep_artifacts != coord_artifacts {
                return Ok(false);
            }

            let rep_vars = rep.active_shared_vars();
            if rep_vars != coord_vars {
                return Ok(false);
            }
        }

        Ok(true)
    }

    /// Export a global snapshot delta representing the complete coordinator state
    pub fn export_global_snapshot(&self) -> Result<MicroVmSyncDelta> {
        let coord = self.coordinator.read().map_err(|_| {
            HgbError::Storage("Read lock poisoned for coordinator".to_string())
        })?;
        Ok(coord.generate_delta(&VectorClock::new()))
    }

    /// Import an external snapshot delta into the coordinator
    pub fn import_global_snapshot(&self, delta: &MicroVmSyncDelta) -> Result<MicroVmSyncReport> {
        let mut coord = self.coordinator.write().map_err(|_| {
            HgbError::Storage("Write lock poisoned for coordinator".to_string())
        })?;
        coord.apply_delta(delta)
    }
}

// -----------------------------------------------------------------------------
// 5. Unit Tests
// -----------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_microvm_replica_local_writes_and_delta_generation() {
        let mut replica = MicroVmReplica::new("vm-frontend-01", "frontend_engineer");

        let fact = EpisodicFact::new(
            "fact_1",
            "vm-frontend-01",
            "frontend_engineer",
            "ui_layout",
            "Tailwind 4.0 requires container-query plugin",
            95,
        );
        replica.record_fact(fact.clone());

        let thought = ScratchpadThought::new(
            "thought_1",
            "vm-frontend-01",
            "frontend_engineer",
            1,
            "Inspect flexbox overflow in cockpit HUD",
            Some("Apply overflow-hidden".to_string()),
            "executing",
        );
        replica.record_thought(thought.clone());

        let artifact = ToolArtifactRecord::new(
            "art_1",
            "vm-frontend-01",
            "src/components/Cockpit.tsx",
            b"export const Cockpit = () => <div/>;",
            "tsx_component",
        );
        replica.record_artifact(artifact.clone());

        replica.set_shared_var("frontend.status", "ready_for_test");
        replica.upsert_vector(
            "vec_fe_01",
            "ui_embeddings",
            vec![0.1, 0.2, 0.3],
            "cockpit layout",
        );

        assert_eq!(replica.active_facts().len(), 1);
        assert_eq!(replica.active_thoughts().len(), 1);
        assert_eq!(replica.active_artifacts().len(), 1);
        assert_eq!(
            replica.active_shared_vars().get("frontend.status"),
            Some(&"ready_for_test".to_string())
        );

        let delta = replica.generate_delta(&VectorClock::new());
        assert_eq!(delta.origin_vm_id, "vm-frontend-01");
        assert_eq!(delta.origin_role, "frontend_engineer");
        assert_eq!(delta.facts_delta.elements().len(), 1);
        assert_eq!(delta.thoughts_delta.elements().len(), 1);
        assert_eq!(delta.artifacts_delta.elements().len(), 1);
        assert_eq!(delta.shared_vars_delta.len(), 1);
        assert_eq!(delta.vector_registers_delta.len(), 1);
        assert!(delta.verify_checksum().is_ok());
    }

    #[test]
    fn test_multi_agent_concurrent_microvm_writes_and_conflict_free_convergence() {
        let sync = MicroVmCrdtStateSync::new("host-coordinator");
        sync.register_microvm("vm-frontend", "frontend_engineer")
            .unwrap();
        sync.register_microvm("vm-backend", "backend_engineer")
            .unwrap();
        sync.register_microvm("vm-tester", "test_runner").unwrap();

        // VM 1: Frontend agent writes UI facts and artifacts
        sync.record_fact(
            "vm-frontend",
            EpisodicFact::new(
                "f_ui",
                "vm-frontend",
                "frontend_engineer",
                "ui",
                "React 19 server actions enabled",
                90,
            ),
        )
        .unwrap();
        sync.record_artifact(
            "vm-frontend",
            ToolArtifactRecord::new(
                "art_ui",
                "vm-frontend",
                "src/app.tsx",
                b"import React from 'react';",
                "source",
            ),
        )
        .unwrap();

        // VM 2: Backend agent writes API facts and artifacts
        sync.record_fact(
            "vm-backend",
            EpisodicFact::new(
                "f_api",
                "vm-backend",
                "backend_engineer",
                "api",
                "Axum router on 127.0.0.1:4000",
                98,
            ),
        )
        .unwrap();
        sync.record_artifact(
            "vm-backend",
            ToolArtifactRecord::new(
                "art_api",
                "vm-backend",
                "src/server.rs",
                b"fn main() {}",
                "source",
            ),
        )
        .unwrap();

        // VM 3: Tester agent writes test report
        sync.record_thought(
            "vm-tester",
            ScratchpadThought::new(
                "th_test",
                "vm-tester",
                "test_runner",
                1,
                "Running cargo test across workspace",
                Some("cargo test".to_string()),
                "passed",
            ),
        )
        .unwrap();
        sync.record_artifact(
            "vm-tester",
            ToolArtifactRecord::new(
                "art_test",
                "vm-tester",
                "target/test_report.json",
                b"{\"passed\": 42}",
                "report",
            ),
        )
        .unwrap();

        // Prior to sync: Replicas do not yet have each other's data
        let rep_fe = sync.get_replica("vm-frontend").unwrap();
        assert_eq!(rep_fe.read().unwrap().active_facts().len(), 1);

        // Execute sync across all microVMs and coordinator
        let reports = sync.sync_all().unwrap();
        assert_eq!(reports.len(), 6); // 3 push to coord + 3 pull from coord

        // Post-sync: Deterministic convergence guaranteed!
        assert!(sync.verify_convergence().unwrap());

        // Every replica has all 2 facts, 1 thought, and 3 artifacts!
        let rep_fe = sync.get_replica("vm-frontend").unwrap();
        let rep_be = sync.get_replica("vm-backend").unwrap();
        let rep_te = sync.get_replica("vm-tester").unwrap();

        assert_eq!(rep_fe.read().unwrap().active_facts().len(), 2);
        assert_eq!(rep_be.read().unwrap().active_facts().len(), 2);
        assert_eq!(rep_te.read().unwrap().active_facts().len(), 2);

        assert_eq!(rep_fe.read().unwrap().active_thoughts().len(), 1);
        assert_eq!(rep_be.read().unwrap().active_thoughts().len(), 1);
        assert_eq!(rep_te.read().unwrap().active_thoughts().len(), 1);

        assert_eq!(rep_fe.read().unwrap().active_artifacts().len(), 3);
        assert_eq!(rep_be.read().unwrap().active_artifacts().len(), 3);
        assert_eq!(rep_te.read().unwrap().active_artifacts().len(), 3);
    }

    #[test]
    fn test_lamport_timestamp_tie_breaking_on_concurrent_variable_writes() {
        let sync = MicroVmCrdtStateSync::new("host-coordinator");
        sync.register_microvm("vm-node-a", "worker").unwrap();
        sync.register_microvm("vm-node-b", "worker").unwrap();

        // VM A writes variable with lamport = 1
        sync.set_shared_var("vm-node-a", "build_mode", "debug")
            .unwrap();

        // VM B writes variable with lamport = 1 (higher node ID "vm-node-b" > "vm-node-a")
        sync.set_shared_var("vm-node-b", "build_mode", "release")
            .unwrap();

        // Test direct cross-sync between microVMs
        let (rep_b, rep_a) = sync.cross_sync_vms("vm-node-a", "vm-node-b").unwrap();
        assert_eq!(rep_b.local_vm_id, "vm-node-b");
        assert_eq!(rep_a.local_vm_id, "vm-node-a");

        // Both replicas must converge to the exact same value!
        let val_a = sync.get_shared_var("vm-node-a", "build_mode").unwrap();
        let val_b = sync.get_shared_var("vm-node-b", "build_mode").unwrap();
        assert_eq!(val_a, val_b);

        // "vm-node-b" wins the deterministic tie-break due to higher node_id
        assert_eq!(val_a, Some("release".to_string()));

        // Now VM A does an update with higher lamport timestamp
        sync.set_shared_var("vm-node-a", "build_mode", "profile")
            .unwrap();
        sync.cross_sync_vms("vm-node-a", "vm-node-b").unwrap();

        let val_a_new = sync.get_shared_var("vm-node-a", "build_mode").unwrap();
        let val_b_new = sync.get_shared_var("vm-node-b", "build_mode").unwrap();
        assert_eq!(val_a_new, Some("profile".to_string()));
        assert_eq!(val_b_new, Some("profile".to_string()));
    }

    #[test]
    fn test_or_set_artifact_deletion_and_concurrent_addition() {
        let sync = MicroVmCrdtStateSync::new("host-coordinator");
        sync.register_microvm("vm-1", "worker").unwrap();
        sync.register_microvm("vm-2", "worker").unwrap();

        let art1 = ToolArtifactRecord::new(
            "art_100",
            "vm-1",
            "src/temp.rs",
            b"fn temp() {}",
            "source",
        );
        sync.record_artifact("vm-1", art1.clone()).unwrap();

        // Sync to VM 2
        sync.cross_sync_vms("vm-1", "vm-2").unwrap();

        let rep2 = sync.get_replica("vm-2").unwrap();
        assert_eq!(rep2.read().unwrap().active_artifacts().len(), 1);

        // VM 2 deletes art1 by path
        sync.remove_artifact_by_path("vm-2", "src/temp.rs").unwrap();
        assert_eq!(rep2.read().unwrap().active_artifacts().len(), 0);

        // VM 1 concurrently adds a new artifact
        let art2 = ToolArtifactRecord::new(
            "art_200",
            "vm-1",
            "src/stable.rs",
            b"fn stable() {}",
            "source",
        );
        sync.record_artifact("vm-1", art2.clone()).unwrap();

        // Cross sync again
        sync.cross_sync_vms("vm-1", "vm-2").unwrap();

        let rep1 = sync.get_replica("vm-1").unwrap();
        let rep2 = sync.get_replica("vm-2").unwrap();

        // Both replicas must only have art2!
        assert_eq!(rep1.read().unwrap().active_artifacts().len(), 1);
        assert_eq!(rep2.read().unwrap().active_artifacts().len(), 1);
        assert_eq!(
            rep1.read().unwrap().active_artifacts()[0].path,
            "src/stable.rs"
        );
        assert_eq!(
            rep2.read().unwrap().active_artifacts()[0].path,
            "src/stable.rs"
        );
    }

    #[test]
    fn test_microvm_delta_serialization_and_checksum_verification() {
        let mut replica = MicroVmReplica::new("vm-alpha", "planner");
        replica.record_fact(EpisodicFact::new(
            "f1",
            "vm-alpha",
            "planner",
            "plan",
            "Task 1 completed",
            100,
        ));
        replica.set_shared_var("stage", "verification");

        let delta = replica.generate_delta(&VectorClock::new());
        let json_str = delta.to_json().expect("to_json");

        // Parse valid delta
        let parsed = MicroVmSyncDelta::from_json(&json_str).expect("from_json");
        assert_eq!(parsed.origin_vm_id, "vm-alpha");
        assert_eq!(parsed.blake3_checksum, delta.blake3_checksum);

        // Tamper with checksum -> should fail verification
        let mut tampered = parsed.clone();
        tampered.blake3_checksum = "00000000000000000000000000000000".to_string();
        assert!(tampered.verify_checksum().is_err());
    }
}
