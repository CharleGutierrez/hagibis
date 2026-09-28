use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FileLeafNode {
    pub path: String,
    pub blake3_hash: String,
    pub size_bytes: u64,
    pub last_modified_epoch_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MerkleCodebaseSnapshot {
    pub root_hash: String,
    pub file_count: usize,
    pub total_bytes: u64,
    pub leaves: BTreeMap<String, FileLeafNode>,
    pub created_at_utc: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MerkleDiffReport {
    pub prev_root: String,
    pub new_root: String,
    pub added_files: Vec<String>,
    pub modified_files: Vec<String>,
    pub deleted_files: Vec<String>,
    pub is_identical: bool,
}

pub struct MerkleSmartIndex;

impl MerkleSmartIndex {
    pub fn new() -> Self {
        Self
    }

    /// Computes a blake3 cryptographic Merkle tree of a workspace directory
    pub fn build_snapshot(root_dir: &Path) -> MerkleCodebaseSnapshot {
        let mut leaves = BTreeMap::new();
        let mut total_bytes = 0;

        Self::walk_and_hash(root_dir, root_dir, &mut leaves, &mut total_bytes);

        // Compute Root Merkle Hash from sorted leaf hashes
        let mut hasher = blake3::Hasher::new();
        for (rel_path, node) in &leaves {
            hasher.update(rel_path.as_bytes());
            hasher.update(node.blake3_hash.as_bytes());
        }
        let root_hash = hasher.finalize().to_hex().to_string();

        MerkleCodebaseSnapshot {
            root_hash,
            file_count: leaves.len(),
            total_bytes,
            leaves,
            created_at_utc: chrono::Utc::now().to_rfc3339(),
        }
    }

    fn walk_and_hash(
        base_dir: &Path,
        current_dir: &Path,
        leaves: &mut BTreeMap<String, FileLeafNode>,
        total_bytes: &mut u64,
    ) {
        if let Ok(entries) = std::fs::read_dir(current_dir) {
            for entry in entries.filter_map(std::result::Result::ok) {
                let path = entry.path();
                let file_name = entry.file_name().to_string_lossy().to_string();

                // Skip hidden dirs, git, target, node_modules
                if file_name.starts_with('.') || file_name == "target" || file_name == "node_modules" || file_name == "tmp" {
                    continue;
                }

                if path.is_dir() {
                    Self::walk_and_hash(base_dir, &path, leaves, total_bytes);
                } else if path.is_file() {
                    if let Ok(metadata) = entry.metadata() {
                        let size = metadata.len();
                        let modified = metadata
                            .modified()
                            .ok()
                            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                            .map(|d| d.as_millis() as u64)
                            .unwrap_or(0);

                        let rel_path = path
                            .strip_prefix(base_dir)
                            .map(|p| p.to_string_lossy().to_string())
                            .unwrap_or_else(|_| file_name);

                        // Fast Blake3 content hash
                        let content_hash = if let Ok(bytes) = std::fs::read(&path) {
                            blake3::hash(&bytes).to_hex().to_string()
                        } else {
                            blake3::hash(b"").to_hex().to_string()
                        };

                        *total_bytes += size;
                        leaves.insert(
                            rel_path.clone(),
                            FileLeafNode {
                                path: rel_path,
                                blake3_hash: content_hash,
                                size_bytes: size,
                                last_modified_epoch_ms: modified,
                            },
                        );
                    }
                }
            }
        }
    }

    /// O(k log N) differential comparison between two Merkle snapshots
    pub fn diff(prev: &MerkleCodebaseSnapshot, current: &MerkleCodebaseSnapshot) -> MerkleDiffReport {
        if prev.root_hash == current.root_hash {
            return MerkleDiffReport {
                prev_root: prev.root_hash.clone(),
                new_root: current.root_hash.clone(),
                added_files: Vec::new(),
                modified_files: Vec::new(),
                deleted_files: Vec::new(),
                is_identical: true,
            };
        }

        let mut added = Vec::new();
        let mut modified = Vec::new();
        let mut deleted = Vec::new();

        for (path, curr_node) in &current.leaves {
            if let Some(prev_node) = prev.leaves.get(path) {
                if prev_node.blake3_hash != curr_node.blake3_hash {
                    modified.push(path.clone());
                }
            } else {
                added.push(path.clone());
            }
        }

        for path in prev.leaves.keys() {
            if !current.leaves.contains_key(path) {
                deleted.push(path.clone());
            }
        }

        MerkleDiffReport {
            prev_root: prev.root_hash.clone(),
            new_root: current.root_hash.clone(),
            added_files: added,
            modified_files: modified,
            deleted_files: deleted,
            is_identical: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_merkle_index_and_diff() {
        let tmp = std::env::temp_dir().join("hgb_merkle_test");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();

        std::fs::write(tmp.join("file1.txt"), "hello").unwrap();
        std::fs::write(tmp.join("file2.txt"), "world").unwrap();

        let snap1 = MerkleSmartIndex::build_snapshot(&tmp);
        assert_eq!(snap1.file_count, 2);
        assert!(!snap1.root_hash.is_empty());

        // Identical diff
        let diff_ident = MerkleSmartIndex::diff(&snap1, &snap1);
        assert!(diff_ident.is_identical);

        // Modify file1
        std::fs::write(tmp.join("file1.txt"), "hello modified").unwrap();
        // Add file3
        std::fs::write(tmp.join("file3.txt"), "new file").unwrap();

        let snap2 = MerkleSmartIndex::build_snapshot(&tmp);
        assert_ne!(snap1.root_hash, snap2.root_hash);

        let diff = MerkleSmartIndex::diff(&snap1, &snap2);
        assert!(!diff.is_identical);
        assert_eq!(diff.modified_files, vec!["file1.txt".to_string()]);
        assert_eq!(diff.added_files, vec!["file3.txt".to_string()]);
        assert!(diff.deleted_files.is_empty());

        let _ = std::fs::remove_dir_all(&tmp);
    }
}
