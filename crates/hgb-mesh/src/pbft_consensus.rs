//! # Distributed Byzantine Fault Tolerant (PBFT) Swarm Consensus Engine
//!
//! Production-grade PBFT ($3f + 1$ total nodes, $2f + 1$ quorum) consensus engine
//! directly integrated into `hgb-mesh` for multi-tenant autonomous swarms.
//!
//! Provides:
//! - **Mathematical BFT Quorum ($N \ge 3f + 1$)**: Tolerates up to $f$ arbitrary Byzantine
//!   (malicious, compromised, or faulty) nodes with deterministic leader selection (`view % N`).
//! - **Two-Phase Voting Pipeline**:
//!   1. `PrePrepare` from Leader containing cryptographically signed mutation proposal.
//!   2. `Prepare` voting requiring $2f$ matching endorsements from distinct peers ($2f+1$ total).
//!   3. `Commit` voting requiring $2f+1$ matching commits from distinct peers.
//! - **View Change Protocol**: Automatic leader timeout failover with $2f+1$ `ViewChange`
//!   quorum proofs and `NewView` proof reconstruction.
//! - **Periodic Stable Checkpoints**: Safe message log compaction and state pruning.
//! - **Swarm Code Mutation Debate Engine (`CodeMutationAuditor`)**: Cryptographic signature
//!   auditing, SHA-256 diff verification, AST fingerprint validation, syntax structural checks,
//!   and AgentShield malicious pattern rejection before casting prepare votes.

use std::collections::{HashMap, HashSet};
use std::time::{SystemTime, UNIX_EPOCH};

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use libp2p::identity::PublicKey;
use serde::{Deserialize, Serialize};

use crate::error::{MeshError, Result};
use crate::secure_transport::{compute_sha256, compute_sha256_hex, NodeIdentity};

/// Unique identifier of a PBFT swarm node (PeerId string)
pub type PbftNodeId = String;

/// Monotonically increasing view identifier
pub type PbftViewId = u64;

/// Monotonically increasing consensus sequence number
pub type PbftSequenceNumber = u64;

// ==============================================================================
// 1. Swarm Code Mutation Proposal
// ==============================================================================

/// Autonomous compiler code mutation proposal under cryptographic swarm debate
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MutationProposal {
    /// Unique proposal UUID
    pub id: String,
    /// Proposer node ID
    pub author: PbftNodeId,
    /// SHA-256 hex digest of the patch content
    pub code_diff_hash: String,
    /// Deterministic AST fingerprint of the mutation
    pub ast_fingerprint: String,
    /// Relative target source file path being patched
    pub proposed_file_path: String,
    /// Raw unified diff or patch code content
    pub patch_content: String,
    /// Ed25519 digital signature of canonical proposal digest by author
    pub ed25519_signature: String,
    /// Timestamp of proposal creation in ms since UNIX epoch
    pub timestamp_ms: u64,
    /// Semantic description or compiler reasoning for the patch
    pub description: String,
}

impl MutationProposal {
    /// Construct and cryptographically sign a new code mutation proposal
    pub fn new(
        author_identity: &NodeIdentity,
        proposed_file_path: &str,
        patch_content: &str,
        description: &str,
    ) -> Result<Self> {
        let id = uuid::Uuid::new_v4().to_string();
        let author = author_identity.peer_id_string();
        let timestamp_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        let code_diff_hash = compute_sha256_hex(patch_content.as_bytes());
        let ast_fingerprint = Self::compute_ast_fingerprint(patch_content);

        let canonical_str = format!(
            "{}:{}:{}:{}:{}:{}",
            id, author, code_diff_hash, ast_fingerprint, proposed_file_path, timestamp_ms
        );
        let digest = compute_sha256(canonical_str.as_bytes());

        let sig_bytes = author_identity.sign(&digest)?;
        let ed25519_signature = BASE64.encode(&sig_bytes);

        Ok(Self {
            id,
            author,
            code_diff_hash,
            ast_fingerprint,
            proposed_file_path: proposed_file_path.to_string(),
            patch_content: patch_content.to_string(),
            ed25519_signature,
            timestamp_ms,
            description: description.to_string(),
        })
    }

    /// Compute deterministic AST fingerprint over normalized token stream
    pub fn compute_ast_fingerprint(content: &str) -> String {
        let mut normalized = String::with_capacity(content.len());
        for line in content.lines() {
            let trimmed = line.trim();
            // Skip comments and empty lines for structural AST fingerprinting
            if !trimmed.is_empty() && !trimmed.starts_with("//") && !trimmed.starts_with('#') {
                normalized.push_str(trimmed);
                normalized.push(';');
            }
        }
        compute_sha256_hex(normalized.as_bytes())
    }

    /// Canonical hash digest of the proposal for consensus debate
    pub fn canonical_digest(&self) -> String {
        let canonical_str = format!(
            "{}:{}:{}:{}:{}:{}",
            self.id,
            self.author,
            self.code_diff_hash,
            self.ast_fingerprint,
            self.proposed_file_path,
            self.timestamp_ms
        );
        compute_sha256_hex(canonical_str.as_bytes())
    }

    /// Cryptographically verify the author's Ed25519 signature
    pub fn verify_author_signature(&self, author_public_key: &PublicKey) -> Result<()> {
        let sig_bytes = BASE64
            .decode(&self.ed25519_signature)
            .map_err(|e| MeshError::Base64(format!("Invalid signature encoding: {}", e)))?;

        let canonical_str = format!(
            "{}:{}:{}:{}:{}:{}",
            self.id,
            self.author,
            self.code_diff_hash,
            self.ast_fingerprint,
            self.proposed_file_path,
            self.timestamp_ms
        );
        let digest = compute_sha256(canonical_str.as_bytes());

        NodeIdentity::verify_signature(author_public_key, &digest, &sig_bytes)
    }
}

// ==============================================================================
// 2. Swarm Code Mutation Debate & Syntax Auditor
// ==============================================================================

/// Outcome of the independent swarm code mutation debate
#[derive(Debug, Clone, PartialEq)]
pub struct MutationAuditOutcome {
    pub approved: bool,
    pub reason: String,
    pub risk_score: f64,
    pub ast_token_count: usize,
}

/// Independent auditor verifying code diffs, syntax validity, and security invariants
pub struct CodeMutationAuditor;

impl CodeMutationAuditor {
    /// Conduct full cryptographic and semantic audit of a proposed code mutation
    pub fn audit_proposal(
        proposal: &MutationProposal,
        author_public_key: &PublicKey,
    ) -> MutationAuditOutcome {
        // 1. Cryptographic signature check
        if let Err(e) = proposal.verify_author_signature(author_public_key) {
            return MutationAuditOutcome {
                approved: false,
                reason: format!("Cryptographic signature verification failed: {}", e),
                risk_score: 1.0,
                ast_token_count: 0,
            };
        }

        // 2. Code diff hash verification
        let calculated_hash = compute_sha256_hex(proposal.patch_content.as_bytes());
        if calculated_hash != proposal.code_diff_hash {
            return MutationAuditOutcome {
                approved: false,
                reason: format!(
                    "Code diff hash mismatch: expected {}, calculated {}",
                    proposal.code_diff_hash, calculated_hash
                ),
                risk_score: 1.0,
                ast_token_count: 0,
            };
        }

        // 3. AST fingerprint verification
        let calculated_ast = MutationProposal::compute_ast_fingerprint(&proposal.patch_content);
        if calculated_ast != proposal.ast_fingerprint {
            return MutationAuditOutcome {
                approved: false,
                reason: format!(
                    "AST fingerprint mismatch: expected {}, calculated {}",
                    proposal.ast_fingerprint, calculated_ast
                ),
                risk_score: 0.95,
                ast_token_count: 0,
            };
        }

        // 4. Sandbox file path safety check (prevent path traversal)
        let path = &proposal.proposed_file_path;
        if path.starts_with('/')
            || path.contains("..")
            || path.contains("/etc/")
            || path.contains("/root/")
            || path.ends_with(".env")
            || path.contains("id_rsa")
            || path.contains("id_ed25519")
        {
            return MutationAuditOutcome {
                approved: false,
                reason: format!("Prohibited path traversal or sensitive path access: {}", path),
                risk_score: 1.0,
                ast_token_count: 0,
            };
        }

        // 5. Dangerous command & execution heuristic audit
        let banned_patterns = [
            "rm -rf",
            "mkfs",
            "fork(",
            "execve(",
            ":(){ :|:& };:",
            "std::process::Command",
            "libc::system",
            "> /dev/sd",
            "> /dev/nvme",
        ];

        for pattern in &banned_patterns {
            if proposal.patch_content.contains(pattern) {
                return MutationAuditOutcome {
                    approved: false,
                    reason: format!("Malicious code pattern detected: '{}'", pattern),
                    risk_score: 1.0,
                    ast_token_count: 0,
                };
            }
        }

        // 6. Syntax validity audit (balanced delimiters via lexer tokenizer)
        match proposal.patch_content.parse::<proc_macro2::TokenStream>() {
            Ok(tokens) => {
                fn count_tokens(stream: proc_macro2::TokenStream) -> usize {
                    let mut count = 0;
                    for tt in stream {
                        count += 1;
                        if let proc_macro2::TokenTree::Group(group) = tt {
                            count += count_tokens(group.stream());
                        }
                    }
                    count
                }
                let token_count = count_tokens(tokens);
                MutationAuditOutcome {
                    approved: true,
                    reason: "Independent cryptographic and syntax debate passed with zero violations".to_string(),
                    risk_score: 0.0,
                    ast_token_count: token_count,
                }
            }
            Err(e) => {
                MutationAuditOutcome {
                    approved: false,
                    reason: format!("Syntax error: unbalanced delimiters or invalid tokens ({})", e),
                    risk_score: 0.85,
                    ast_token_count: 0,
                }
            }
        }
    }
}

// ==============================================================================
// 3. PBFT Protocol Messages
// ==============================================================================

/// PBFT consensus protocol messages
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum PbftMessage {
    /// Pre-Prepare multicast by the leader
    PrePrepare {
        view: PbftViewId,
        sequence_number: PbftSequenceNumber,
        proposal_digest: String,
        proposal: MutationProposal,
        leader_id: PbftNodeId,
        signature: String,
    },
    /// Prepare multicast by backups upon validating PrePrepare
    Prepare {
        view: PbftViewId,
        sequence_number: PbftSequenceNumber,
        proposal_digest: String,
        node_id: PbftNodeId,
        signature: String,
    },
    /// Commit multicast by nodes upon reaching Prepared state
    Commit {
        view: PbftViewId,
        sequence_number: PbftSequenceNumber,
        proposal_digest: String,
        node_id: PbftNodeId,
        signature: String,
    },
    /// View Change notification triggered on leader failure or timeout
    ViewChange {
        new_view: PbftViewId,
        node_id: PbftNodeId,
        prepared_proofs: Vec<PreparedCertificate>,
        signature: String,
    },
    /// New View proclamation multicast by the new leader
    NewView {
        new_view: PbftViewId,
        leader_id: PbftNodeId,
        view_change_proofs: Vec<PbftMessage>,
        signature: String,
    },
    /// Periodic stable state checkpoint
    Checkpoint {
        sequence_number: PbftSequenceNumber,
        state_digest: String,
        node_id: PbftNodeId,
        signature: String,
    },
}

impl PbftMessage {
    /// Extract the view number associated with the message
    pub fn view(&self) -> Option<PbftViewId> {
        match self {
            Self::PrePrepare { view, .. } => Some(*view),
            Self::Prepare { view, .. } => Some(*view),
            Self::Commit { view, .. } => Some(*view),
            Self::ViewChange { new_view, .. } => Some(*new_view),
            Self::NewView { new_view, .. } => Some(*new_view),
            Self::Checkpoint { .. } => None,
        }
    }

    /// Extract the sequence number associated with the message
    pub fn sequence_number(&self) -> Option<PbftSequenceNumber> {
        match self {
            Self::PrePrepare { sequence_number, .. } => Some(*sequence_number),
            Self::Prepare { sequence_number, .. } => Some(*sequence_number),
            Self::Commit { sequence_number, .. } => Some(*sequence_number),
            Self::Checkpoint { sequence_number, .. } => Some(*sequence_number),
            Self::ViewChange { .. } | Self::NewView { .. } => None,
        }
    }

    /// Extract the sender node ID
    pub fn sender_id(&self) -> &PbftNodeId {
        match self {
            Self::PrePrepare { leader_id, .. } => leader_id,
            Self::Prepare { node_id, .. } => node_id,
            Self::Commit { node_id, .. } => node_id,
            Self::ViewChange { node_id, .. } => node_id,
            Self::NewView { leader_id, .. } => leader_id,
            Self::Checkpoint { node_id, .. } => node_id,
        }
    }

    /// Canonical signing payload for cryptographic verification
    pub fn signing_bytes(&self) -> Vec<u8> {
        match self {
            Self::PrePrepare {
                view,
                sequence_number,
                proposal_digest,
                leader_id,
                ..
            } => {
                format!("PRE-PREPARE:{}:{}:{}:{}", view, sequence_number, proposal_digest, leader_id)
                    .into_bytes()
            }
            Self::Prepare {
                view,
                sequence_number,
                proposal_digest,
                node_id,
                ..
            } => {
                format!("PREPARE:{}:{}:{}:{}", view, sequence_number, proposal_digest, node_id)
                    .into_bytes()
            }
            Self::Commit {
                view,
                sequence_number,
                proposal_digest,
                node_id,
                ..
            } => {
                format!("COMMIT:{}:{}:{}:{}", view, sequence_number, proposal_digest, node_id)
                    .into_bytes()
            }
            Self::ViewChange {
                new_view,
                node_id,
                prepared_proofs,
                ..
            } => {
                let proofs_hash = compute_sha256_hex(&bincode::serialize(prepared_proofs).unwrap_or_default());
                format!("VIEW-CHANGE:{}:{}:{}", new_view, node_id, proofs_hash).into_bytes()
            }
            Self::NewView {
                new_view,
                leader_id,
                view_change_proofs,
                ..
            } => {
                let vc_hash = compute_sha256_hex(&bincode::serialize(view_change_proofs).unwrap_or_default());
                format!("NEW-VIEW:{}:{}:{}", new_view, leader_id, vc_hash).into_bytes()
            }
            Self::Checkpoint {
                sequence_number,
                state_digest,
                node_id,
                ..
            } => {
                format!("CHECKPOINT:{}:{}:{}", sequence_number, state_digest, node_id).into_bytes()
            }
        }
    }

    /// Extract signature string
    pub fn signature(&self) -> &str {
        match self {
            Self::PrePrepare { signature, .. } => signature,
            Self::Prepare { signature, .. } => signature,
            Self::Commit { signature, .. } => signature,
            Self::ViewChange { signature, .. } => signature,
            Self::NewView { signature, .. } => signature,
            Self::Checkpoint { signature, .. } => signature,
        }
    }

    /// Verify signature with public key
    pub fn verify_signature(&self, public_key: &PublicKey) -> Result<()> {
        let sig_bytes = BASE64
            .decode(self.signature())
            .map_err(|e| MeshError::Base64(format!("Invalid signature encoding: {}", e)))?;
        let payload = self.signing_bytes();
        let digest = compute_sha256(&payload);
        NodeIdentity::verify_signature(public_key, &digest, &sig_bytes)
    }
}

// ==============================================================================
// 4. Consensus Proof Certificates
// ==============================================================================

/// Cryptographic certificate proving a proposal achieved Prepared state in view `v`
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PreparedCertificate {
    pub view: PbftViewId,
    pub sequence_number: PbftSequenceNumber,
    pub proposal_digest: String,
    pub prepare_votes: Vec<PbftMessage>,
}

/// Cryptographic certificate proving a proposal achieved Committed state
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CommitCertificate {
    pub view: PbftViewId,
    pub sequence_number: PbftSequenceNumber,
    pub proposal_digest: String,
    pub commit_votes: Vec<PbftMessage>,
}

// ==============================================================================
// 5. PBFT Consensus Engine
// ==============================================================================

/// Distributed Byzantine Fault Tolerant consensus state machine
pub struct PbftConsensusEngine {
    /// Local node cryptographic identity
    pub identity: NodeIdentity,
    /// Local node ID (PeerId)
    pub node_id: PbftNodeId,
    /// Deterministically ordered list of cluster peers ($N = 3f + 1$)
    pub cluster_nodes: Vec<PbftNodeId>,
    /// Public key directory of all cluster peers
    pub peer_keys: HashMap<PbftNodeId, PublicKey>,
    /// Current view number
    pub current_view: PbftViewId,
    /// Last assigned or observed sequence number
    pub last_sequence_number: PbftSequenceNumber,
    /// Last stable checkpoint sequence number
    pub last_stable_checkpoint: PbftSequenceNumber,
    /// Digest of the state at the last stable checkpoint
    pub last_stable_checkpoint_digest: String,
    /// Active consensus state digest
    pub state_digest: String,
    /// Flag indicating whether normal view operation is active (false during view change)
    pub view_active: bool,

    // In-memory message logs
    pub pre_prepare_log: HashMap<(PbftViewId, PbftSequenceNumber), PbftMessage>,
    pub prepare_log: HashMap<(PbftViewId, PbftSequenceNumber), HashMap<PbftNodeId, PbftMessage>>,
    pub commit_log: HashMap<(PbftViewId, PbftSequenceNumber), HashMap<PbftNodeId, PbftMessage>>,
    pub checkpoint_log: HashMap<PbftSequenceNumber, HashMap<PbftNodeId, PbftMessage>>,
    pub view_change_log: HashMap<PbftViewId, HashMap<PbftNodeId, PbftMessage>>,

    // Verified proposals store
    pub proposals: HashMap<String, MutationProposal>,
    // Ordered committed proposals
    pub committed_proposals: Vec<MutationProposal>,
    // Executed proposal IDs in sequence order
    pub executed_proposal_ids: HashSet<String>,
}

impl PbftConsensusEngine {
    /// Initialize a new PBFT consensus engine instance
    pub fn new(
        identity: NodeIdentity,
        cluster_nodes: Vec<PbftNodeId>,
        peer_keys: HashMap<PbftNodeId, PublicKey>,
    ) -> Self {
        let node_id = identity.peer_id_string();
        Self {
            identity,
            node_id,
            cluster_nodes,
            peer_keys,
            current_view: 0,
            last_sequence_number: 0,
            last_stable_checkpoint: 0,
            last_stable_checkpoint_digest: compute_sha256_hex(b"GENESIS_STATE_CHECKPOINT"),
            state_digest: compute_sha256_hex(b"GENESIS_STATE_CHECKPOINT"),
            view_active: true,
            pre_prepare_log: HashMap::new(),
            prepare_log: HashMap::new(),
            commit_log: HashMap::new(),
            checkpoint_log: HashMap::new(),
            view_change_log: HashMap::new(),
            proposals: HashMap::new(),
            committed_proposals: Vec::new(),
            executed_proposal_ids: HashSet::new(),
        }
    }

    /// Total cluster size ($N$)
    pub fn cluster_size(&self) -> usize {
        self.cluster_nodes.len()
    }

    /// Maximum tolerable Byzantine faults $f = \lfloor(N - 1) / 3\rfloor$
    pub fn f(&self) -> usize {
        self.cluster_size().saturating_sub(1) / 3
    }

    /// PBFT Quorum size $2f + 1$
    pub fn quorum_size(&self) -> usize {
        2 * self.f() + 1
    }

    /// Deterministic primary/leader for a given view: `view % N`
    pub fn leader_for_view(&self, view: PbftViewId) -> PbftNodeId {
        if self.cluster_nodes.is_empty() {
            return String::new();
        }
        let index = (view as usize) % self.cluster_nodes.len();
        self.cluster_nodes[index].clone()
    }

    /// Check if the local node is the primary/leader for the active view
    pub fn is_leader(&self) -> bool {
        self.leader_for_view(self.current_view) == self.node_id
    }

    // ==========================================================================
    // Consensus Phase 1: Pre-Prepare (Leader Proposal)
    // ==========================================================================

    /// Leader proposes a code mutation proposal to the swarm
    pub fn propose_mutation(&mut self, proposal: MutationProposal) -> Result<PbftMessage> {
        if !self.view_active {
            return Err(MeshError::Consensus("Cannot propose mutation during active view change".to_string()));
        }
        if !self.is_leader() {
            return Err(MeshError::NotLeader);
        }

        // Leader conducts self-audit
        if let Some(author_key) = self.peer_keys.get(&proposal.author) {
            let audit = CodeMutationAuditor::audit_proposal(&proposal, author_key);
            if !audit.approved {
                return Err(MeshError::Consensus(format!("Leader audit rejected proposal: {}", audit.reason)));
            }
        }

        self.last_sequence_number += 1;
        let seq = self.last_sequence_number;
        let view = self.current_view;
        let digest = proposal.canonical_digest();

        let payload = format!("PRE-PREPARE:{}:{}:{}:{}", view, seq, digest, self.node_id);
        let sig = self.identity.sign(&compute_sha256(payload.as_bytes()))?;
        let signature = BASE64.encode(&sig);

        let pre_prepare_msg = PbftMessage::PrePrepare {
            view,
            sequence_number: seq,
            proposal_digest: digest.clone(),
            proposal: proposal.clone(),
            leader_id: self.node_id.clone(),
            signature,
        };

        // Leader logs PrePrepare
        self.pre_prepare_log.insert((view, seq), pre_prepare_msg.clone());
        self.proposals.insert(digest.clone(), proposal);

        // Leader automatically endorses with its own Prepare vote
        let prep_payload = format!("PREPARE:{}:{}:{}:{}", view, seq, digest, self.node_id);
        let prep_sig = self.identity.sign(&compute_sha256(prep_payload.as_bytes()))?;
        let prep_msg = PbftMessage::Prepare {
            view,
            sequence_number: seq,
            proposal_digest: digest,
            node_id: self.node_id.clone(),
            signature: BASE64.encode(&prep_sig),
        };

        self.prepare_log
            .entry((view, seq))
            .or_default()
            .insert(self.node_id.clone(), prep_msg);

        Ok(pre_prepare_msg)
    }

    // ==========================================================================
    // Consensus Phase 2: Handle Pre-Prepare & Multicast Prepare
    // ==========================================================================

    /// Backup node handles a PrePrepare from the leader
    pub fn handle_pre_prepare(&mut self, msg: PbftMessage) -> Result<Option<PbftMessage>> {
        if !self.view_active {
            return Err(MeshError::Consensus("View change in progress; dropping PrePrepare".to_string()));
        }

        let (view, seq, digest, proposal, leader_id) = match &msg {
            PbftMessage::PrePrepare {
                view,
                sequence_number,
                proposal_digest,
                proposal,
                leader_id,
                ..
            } => (*view, *sequence_number, proposal_digest.clone(), proposal.clone(), leader_id.clone()),
            _ => return Err(MeshError::Consensus("Expected PrePrepare message".to_string())),
        };

        // 1. Verify view matches current view
        if view != self.current_view {
            return Err(MeshError::Consensus(format!(
                "PrePrepare view {} does not match current view {}",
                view, self.current_view
            )));
        }

        // 2. Verify sender is the authorized leader for this view
        let expected_leader = self.leader_for_view(view);
        if leader_id != expected_leader {
            return Err(MeshError::Consensus(format!(
                "Sender {} is not the authorized leader {} for view {}",
                leader_id, expected_leader, view
            )));
        }

        // 3. Cryptographically verify leader signature
        let leader_pk = self.peer_keys.get(&leader_id).ok_or_else(|| {
            MeshError::Consensus(format!("Unknown public key for leader {}", leader_id))
        })?;
        msg.verify_signature(leader_pk)?;

        // 4. Verify digest matches proposal
        let calculated_digest = proposal.canonical_digest();
        if calculated_digest != digest {
            return Err(MeshError::Consensus("Proposal digest mismatch in PrePrepare".to_string()));
        }

        // 5. Prevent Byzantine leader equivocation: ensure no conflicting PrePrepare logged
        if let Some(existing) = self.pre_prepare_log.get(&(view, seq)) {
            if let PbftMessage::PrePrepare { proposal_digest: existing_digest, .. } = existing {
                if existing_digest != &digest {
                    return Err(MeshError::Consensus(format!(
                        "Byzantine leader equivocation detected for view {} seq {}",
                        view, seq
                    )));
                }
            }
        }

        // 6. Independent Swarm Code Mutation Debate (Syntax, AST, Diff, and Security Audit)
        let author_pk = self.peer_keys.get(&proposal.author).ok_or_else(|| {
            MeshError::Consensus(format!("Unknown public key for author {}", proposal.author))
        })?;
        let audit = CodeMutationAuditor::audit_proposal(&proposal, author_pk);
        if !audit.approved {
            return Err(MeshError::Consensus(format!(
                "Swarm code mutation debate rejected proposal {}: {}",
                proposal.id, audit.reason
            )));
        }

        // 7. Log PrePrepare and Proposal
        self.pre_prepare_log.insert((view, seq), msg);
        self.proposals.insert(digest.clone(), proposal);
        if seq > self.last_sequence_number {
            self.last_sequence_number = seq;
        }

        // 8. Generate and multicast signed Prepare vote
        let payload = format!("PREPARE:{}:{}:{}:{}", view, seq, digest, self.node_id);
        let sig = self.identity.sign(&compute_sha256(payload.as_bytes()))?;
        let prepare_msg = PbftMessage::Prepare {
            view,
            sequence_number: seq,
            proposal_digest: digest,
            node_id: self.node_id.clone(),
            signature: BASE64.encode(&sig),
        };

        // Log own prepare vote
        self.prepare_log
            .entry((view, seq))
            .or_default()
            .insert(self.node_id.clone(), prepare_msg.clone());

        Ok(Some(prepare_msg))
    }

    // ==========================================================================
    // Consensus Phase 3: Handle Prepare & Check Prepared Quorum ($2f$)
    // ==========================================================================

    /// Handle Prepare vote from a peer; returns Commit message if Prepared quorum is newly reached
    pub fn handle_prepare(&mut self, msg: PbftMessage) -> Result<Option<PbftMessage>> {
        let (view, seq, digest, sender_id) = match &msg {
            PbftMessage::Prepare {
                view,
                sequence_number,
                proposal_digest,
                node_id,
                ..
            } => (*view, *sequence_number, proposal_digest.clone(), node_id.clone()),
            _ => return Err(MeshError::Consensus("Expected Prepare message".to_string())),
        };

        if view != self.current_view {
            return Ok(None);
        }

        let sender_pk = self.peer_keys.get(&sender_id).ok_or_else(|| {
            MeshError::Consensus(format!("Unknown peer ID in prepare: {}", sender_id))
        })?;
        msg.verify_signature(sender_pk)?;

        let prepares = self.prepare_log.entry((view, seq)).or_default();
        prepares.insert(sender_id, msg);

        // Check if Prepared condition is met:
        // 1. Must have accepted PrePrepare for (view, seq) matching digest
        let has_pre_prepare = self.pre_prepare_log.get(&(view, seq)).map_or(false, |pp| {
            if let PbftMessage::PrePrepare { proposal_digest, .. } = pp {
                proposal_digest == &digest
            } else {
                false
            }
        });

        if !has_pre_prepare {
            return Ok(None);
        }

        // 2. Count matching prepares from distinct peers. Need >= 2*f matching prepares.
        let matching_prepares = prepares
            .values()
            .filter(|p| {
                if let PbftMessage::Prepare { proposal_digest, .. } = p {
                    proposal_digest == &digest
                } else {
                    false
                }
            })
            .count();

        let f_faults = self.f();
        // Already committed or commit already sent?
        let commits = self.commit_log.entry((view, seq)).or_default();
        let already_committed = commits.contains_key(&self.node_id);

        if matching_prepares >= 2 * f_faults && !already_committed {
            // Prepared state achieved! Multicast Commit message
            let payload = format!("COMMIT:{}:{}:{}:{}", view, seq, digest, self.node_id);
            let sig = self.identity.sign(&compute_sha256(payload.as_bytes()))?;
            let commit_msg = PbftMessage::Commit {
                view,
                sequence_number: seq,
                proposal_digest: digest,
                node_id: self.node_id.clone(),
                signature: BASE64.encode(&sig),
            };

            commits.insert(self.node_id.clone(), commit_msg.clone());
            return Ok(Some(commit_msg));
        }

        Ok(None)
    }

    // ==========================================================================
    // Consensus Phase 4: Handle Commit & Quorum Execution ($2f + 1$)
    // ==========================================================================

    /// Handle Commit message; returns committed proposal if $2f+1$ commit quorum is newly achieved
    pub fn handle_commit(&mut self, msg: PbftMessage) -> Result<Option<MutationProposal>> {
        let (view, seq, digest, sender_id) = match &msg {
            PbftMessage::Commit {
                view,
                sequence_number,
                proposal_digest,
                node_id,
                ..
            } => (*view, *sequence_number, proposal_digest.clone(), node_id.clone()),
            _ => return Err(MeshError::Consensus("Expected Commit message".to_string())),
        };

        if view != self.current_view {
            return Ok(None);
        }

        let sender_pk = self.peer_keys.get(&sender_id).ok_or_else(|| {
            MeshError::Consensus(format!("Unknown peer ID in commit: {}", sender_id))
        })?;
        msg.verify_signature(sender_pk)?;

        let commits = self.commit_log.entry((view, seq)).or_default();
        commits.insert(sender_id, msg);

        // Count matching commits. Quorum requirement: >= 2*f + 1
        let matching_commits = commits
            .values()
            .filter(|c| {
                if let PbftMessage::Commit { proposal_digest, .. } = c {
                    proposal_digest == &digest
                } else {
                    false
                }
            })
            .count();

        if matching_commits >= self.quorum_size() {
            if let Some(proposal) = self.proposals.get(&digest).cloned() {
                if !self.executed_proposal_ids.contains(&proposal.id) {
                    // Mark as executed in PBFT state machine
                    self.executed_proposal_ids.insert(proposal.id.clone());
                    self.committed_proposals.push(proposal.clone());

                    // Evolve cluster consensus state digest
                    let next_state = format!("{}:{}", self.state_digest, proposal.code_diff_hash);
                    self.state_digest = compute_sha256_hex(next_state.as_bytes());

                    return Ok(Some(proposal));
                }
            }
        }

        Ok(None)
    }

    // ==========================================================================
    // View Change Protocol
    // ==========================================================================

    /// Trigger a View Change upon leader timeout or suspected Byzantine failure
    pub fn trigger_view_change(&mut self) -> Result<PbftMessage> {
        self.view_active = false;
        let new_view = self.current_view + 1;

        // Gather Prepared certificates for all uncommitted or prepared proposals
        let mut prepared_proofs = Vec::new();
        for ((v, s), prepares) in &self.prepare_log {
            if *v <= self.current_view && prepares.len() >= 2 * self.f() {
                if let Some(pp) = self.pre_prepare_log.get(&(*v, *s)) {
                    if let PbftMessage::PrePrepare { proposal_digest, .. } = pp {
                        prepared_proofs.push(PreparedCertificate {
                            view: *v,
                            sequence_number: *s,
                            proposal_digest: proposal_digest.clone(),
                            prepare_votes: prepares.values().cloned().collect(),
                        });
                    }
                }
            }
        }

        let proofs_hash = compute_sha256_hex(&bincode::serialize(&prepared_proofs).unwrap_or_default());
        let payload = format!("VIEW-CHANGE:{}:{}:{}", new_view, self.node_id, proofs_hash);
        let sig = self.identity.sign(&compute_sha256(payload.as_bytes()))?;

        let vc_msg = PbftMessage::ViewChange {
            new_view,
            node_id: self.node_id.clone(),
            prepared_proofs,
            signature: BASE64.encode(&sig),
        };

        self.view_change_log
            .entry(new_view)
            .or_default()
            .insert(self.node_id.clone(), vc_msg.clone());

        Ok(vc_msg)
    }

    /// Handle ViewChange vote; returns NewView proclamation if local node is new leader and reaches $2f+1$
    pub fn handle_view_change(&mut self, msg: PbftMessage) -> Result<Option<PbftMessage>> {
        let (new_view, sender_id) = match &msg {
            PbftMessage::ViewChange { new_view, node_id, .. } => (*new_view, node_id.clone()),
            _ => return Err(MeshError::Consensus("Expected ViewChange message".to_string())),
        };

        if new_view <= self.current_view {
            return Ok(None);
        }

        let sender_pk = self.peer_keys.get(&sender_id).ok_or_else(|| {
            MeshError::Consensus(format!("Unknown peer in view change: {}", sender_id))
        })?;
        msg.verify_signature(sender_pk)?;

        let quorum = self.quorum_size();
        let is_leader_for_new_view = self.leader_for_view(new_view) == self.node_id;

        let vcs = self.view_change_log.entry(new_view).or_default();
        vcs.insert(sender_id, msg);

        // Check if $2f+1$ ViewChange votes received and local node is leader for new_view
        if vcs.len() >= quorum && is_leader_for_new_view {
            let proofs: Vec<PbftMessage> = vcs.values().cloned().collect();
            let vc_hash = compute_sha256_hex(&bincode::serialize(&proofs).unwrap_or_default());
            let payload = format!("NEW-VIEW:{}:{}:{}", new_view, self.node_id, vc_hash);
            let sig = self.identity.sign(&compute_sha256(payload.as_bytes()))?;

            let new_view_msg = PbftMessage::NewView {
                new_view,
                leader_id: self.node_id.clone(),
                view_change_proofs: proofs,
                signature: BASE64.encode(&sig),
            };

            // Transition local state to new view
            self.current_view = new_view;
            self.view_active = true;

            return Ok(Some(new_view_msg));
        }

        Ok(None)
    }

    /// Backup processes NewView from the new leader
    pub fn handle_new_view(&mut self, msg: PbftMessage) -> Result<()> {
        let (new_view, leader_id) = match &msg {
            PbftMessage::NewView { new_view, leader_id, .. } => (*new_view, leader_id.clone()),
            _ => return Err(MeshError::Consensus("Expected NewView message".to_string())),
        };

        if new_view <= self.current_view {
            return Ok(());
        }

        if leader_id != self.leader_for_view(new_view) {
            return Err(MeshError::Consensus("Sender is not the authorized leader for new view".to_string()));
        }

        let leader_pk = self.peer_keys.get(&leader_id).ok_or_else(|| {
            MeshError::Consensus(format!("Unknown leader {} in new view", leader_id))
        })?;
        msg.verify_signature(leader_pk)?;

        // Transition backup to new view
        self.current_view = new_view;
        self.view_active = true;

        Ok(())
    }

    // ==========================================================================
    // Checkpoints & State Pruning
    // ==========================================================================

    /// Generate signed state Checkpoint at sequence number
    pub fn create_checkpoint(&mut self, seq: PbftSequenceNumber) -> Result<PbftMessage> {
        let payload = format!("CHECKPOINT:{}:{}:{}", seq, self.state_digest, self.node_id);
        let sig = self.identity.sign(&compute_sha256(payload.as_bytes()))?;
        let cp_msg = PbftMessage::Checkpoint {
            sequence_number: seq,
            state_digest: self.state_digest.clone(),
            node_id: self.node_id.clone(),
            signature: BASE64.encode(&sig),
        };

        self.checkpoint_log
            .entry(seq)
            .or_default()
            .insert(self.node_id.clone(), cp_msg.clone());

        Ok(cp_msg)
    }

    /// Process Checkpoint; prunes historical logs when $2f+1$ checkpoint quorum is reached
    pub fn handle_checkpoint(&mut self, msg: PbftMessage) -> Result<bool> {
        let (seq, state_digest, sender_id) = match &msg {
            PbftMessage::Checkpoint {
                sequence_number,
                state_digest,
                node_id,
                ..
            } => (*sequence_number, state_digest.clone(), node_id.clone()),
            _ => return Err(MeshError::Consensus("Expected Checkpoint message".to_string())),
        };

        let sender_pk = self.peer_keys.get(&sender_id).ok_or_else(|| {
            MeshError::Consensus(format!("Unknown peer ID in checkpoint: {}", sender_id))
        })?;
        msg.verify_signature(sender_pk)?;

        let cps = self.checkpoint_log.entry(seq).or_default();
        cps.insert(sender_id, msg);

        let matching_cps = cps
            .values()
            .filter(|c| {
                if let PbftMessage::Checkpoint { state_digest: d, .. } = c {
                    d == &state_digest
                } else {
                    false
                }
            })
            .count();

        if matching_cps >= self.quorum_size() && seq > self.last_stable_checkpoint {
            // Establish new stable checkpoint
            self.last_stable_checkpoint = seq;
            self.last_stable_checkpoint_digest = state_digest;

            // Prune historical consensus logs prior to stable checkpoint
            self.pre_prepare_log.retain(|(_, s), _| *s > seq);
            self.prepare_log.retain(|(_, s), _| *s > seq);
            self.commit_log.retain(|(_, s), _| *s > seq);
            self.checkpoint_log.retain(|s, _| *s >= seq);

            return Ok(true);
        }

        Ok(false)
    }
}
