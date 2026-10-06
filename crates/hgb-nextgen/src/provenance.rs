use blake3::Hasher;
use serde::{Deserialize, Serialize};

pub const HAGIBIS_PROVENANCE_GENESIS: &str = "HAGIBIS_SOVEREIGN_GENESIS_ROOT_2026";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MerkleProof {
    pub leaf_hash: String,
    pub hops: Vec<MerkleHop>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MerkleHop {
    pub is_right: bool,
    pub sibling_hash: String,
}

pub struct MerkleTree {
    pub leaves: Vec<String>,
    pub layers: Vec<Vec<String>>,
}

impl MerkleTree {
    pub fn new(leaves: Vec<String>) -> Self {
        if leaves.is_empty() {
            let zero = Self::hash_leaf(b"EMPTY_MERKLE_TREE");
            return Self {
                leaves: vec![zero.clone()],
                layers: vec![vec![zero]],
            };
        }
        let mut layers = Vec::new();
        layers.push(leaves.clone());
        let mut current = leaves.clone();
        while current.len() > 1 {
            let mut next = Vec::new();
            for chunk in current.chunks(2) {
                if chunk.len() == 2 {
                    next.push(Self::hash_pair(&chunk[0], &chunk[1]));
                } else {
                    next.push(Self::hash_pair(&chunk[0], &chunk[0]));
                }
            }
            layers.push(next.clone());
            current = next;
        }
        Self { leaves, layers }
    }

    pub fn root(&self) -> String {
        self.layers.last().and_then(|l| l.first()).cloned().unwrap_or_default()
    }

    pub fn hash_leaf(data: &[u8]) -> String {
        let mut h = Hasher::new();
        h.update(b"HGB_MERKLE_LEAF:");
        h.update(data);
        h.finalize().to_hex().to_string()
    }

    pub fn hash_pair(left: &str, right: &str) -> String {
        hgb_core::zig_accelerate::blake3_node_pair_hex(left, right)
    }

    pub fn generate_proof(&self, index: usize) -> Option<MerkleProof> {
        if index >= self.leaves.len() {
            return None;
        }
        let leaf_hash = self.leaves[index].clone();
        let mut hops = Vec::new();
        let mut idx = index;
        for layer in &self.layers[..self.layers.len() - 1] {
            let is_right = idx % 2 == 1;
            let sibling_idx = if is_right { idx - 1 } else { (idx + 1).min(layer.len() - 1) };
            hops.push(MerkleHop {
                is_right,
                sibling_hash: layer[sibling_idx].clone(),
            });
            idx /= 2;
        }
        Some(MerkleProof { leaf_hash, hops })
    }

    pub fn verify_proof(proof: &MerkleProof, expected_root: &str) -> bool {
        let mut current = proof.leaf_hash.clone();
        for hop in &proof.hops {
            if hop.is_right {
                current = Self::hash_pair(&hop.sibling_hash, &current);
            } else {
                current = Self::hash_pair(&current, &hop.sibling_hash);
            }
        }
        current == expected_root
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProvenanceEntry {
    pub sequence: u64,
    pub timestamp_utc: String,
    pub actor: String,
    pub action: String,
    pub payload_hash: String,
    pub prev_hash: String,
    pub statutory_attestation: Option<String>,
}

pub struct ProvenanceLedger {
    entries: Vec<ProvenanceEntry>,
    tree: MerkleTree,
}

impl ProvenanceLedger {
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
            tree: MerkleTree::new(Vec::new()),
        }
    }

    pub fn append(&mut self, actor: &str, action: &str, payload: &[u8], statutory_rule: Option<&str>) -> ProvenanceEntry {
        let sequence = (self.entries.len() + 1) as u64;
        let timestamp_utc = chrono::Utc::now().to_rfc3339();
        let payload_hash = MerkleTree::hash_leaf(payload);
        let prev_hash = self.entries.last().map(|e| e.payload_hash.clone()).unwrap_or_else(|| HAGIBIS_PROVENANCE_GENESIS.to_string());
        
        let entry = ProvenanceEntry {
            sequence,
            timestamp_utc,
            actor: actor.to_string(),
            action: action.to_string(),
            payload_hash: payload_hash.clone(),
            prev_hash,
            statutory_attestation: statutory_rule.map(|s| s.to_string()),
        };

        self.entries.push(entry.clone());
        let all_hashes: Vec<String> = self.entries.iter().map(|e| e.payload_hash.clone()).collect();
        self.tree = MerkleTree::new(all_hashes);
        entry
    }

    pub fn root(&self) -> String {
        self.tree.root()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }
}
