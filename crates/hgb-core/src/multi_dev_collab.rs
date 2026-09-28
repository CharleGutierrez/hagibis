use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CollabPeer {
    pub peer_id: String,
    pub username: String,
    pub active_file: Option<String>,
    pub cursor_line: Option<usize>,
    pub last_seen_epoch_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CollabPatchIntent {
    pub author_peer_id: String,
    pub target_file: String,
    pub start_line: usize,
    pub end_line: usize,
    pub patch_summary: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CollabConflictWarning {
    pub target_file: String,
    pub peer_a: String,
    pub peer_b: String,
    pub overlapping_lines: (usize, usize),
    pub resolution_strategy: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CollabSessionState {
    pub room_id: String,
    pub host_id: String,
    pub connected_peers: HashMap<String, CollabPeer>,
    pub active_intents: Vec<CollabPatchIntent>,
    pub detected_conflicts: Vec<CollabConflictWarning>,
}

pub struct MultiDevCollabEngine {
    state: CollabSessionState,
}

impl MultiDevCollabEngine {
    pub fn new(room_id: &str, host_username: &str) -> Self {
        let host_id = format!("peer_{:04}", 1);
        let mut peers = HashMap::new();
        peers.insert(
            host_id.clone(),
            CollabPeer {
                peer_id: host_id.clone(),
                username: host_username.to_string(),
                active_file: None,
                cursor_line: None,
                last_seen_epoch_ms: chrono::Utc::now().timestamp_millis() as u64,
            },
        );

        Self {
            state: CollabSessionState {
                room_id: room_id.to_string(),
                host_id,
                connected_peers: peers,
                active_intents: Vec::new(),
                detected_conflicts: Vec::new(),
            },
        }
    }

    pub fn join_peer(&mut self, username: &str) -> CollabPeer {
        let peer_id = format!("peer_{:04}", self.state.connected_peers.len() + 1);
        let peer = CollabPeer {
            peer_id: peer_id.clone(),
            username: username.to_string(),
            active_file: None,
            cursor_line: None,
            last_seen_epoch_ms: chrono::Utc::now().timestamp_millis() as u64,
        };
        self.state.connected_peers.insert(peer_id, peer.clone());
        peer
    }

    pub fn update_cursor(&mut self, peer_id: &str, file: &str, line: usize) {
        if let Some(peer) = self.state.connected_peers.get_mut(peer_id) {
            peer.active_file = Some(file.to_string());
            peer.cursor_line = Some(line);
            peer.last_seen_epoch_ms = chrono::Utc::now().timestamp_millis() as u64;
        }
    }

    pub fn submit_intent(&mut self, intent: CollabPatchIntent) -> Vec<CollabConflictWarning> {
        let mut conflicts = Vec::new();

        for existing in &self.state.active_intents {
            if existing.target_file == intent.target_file && existing.author_peer_id != intent.author_peer_id {
                // Check line overlap: max(start1, start2) <= min(end1, end2)
                let overlap_start = existing.start_line.max(intent.start_line);
                let overlap_end = existing.end_line.min(intent.end_line);

                if overlap_start <= overlap_end {
                    conflicts.push(CollabConflictWarning {
                        target_file: intent.target_file.clone(),
                        peer_a: existing.author_peer_id.clone(),
                        peer_b: intent.author_peer_id.clone(),
                        overlapping_lines: (overlap_start, overlap_end),
                        resolution_strategy: "3-way diff lock: Queue Peer B patch after Peer A finishes".to_string(),
                    });
                }
            }
        }

        self.state.detected_conflicts.extend(conflicts.clone());
        self.state.active_intents.push(intent);
        conflicts
    }

    pub fn session_snapshot(&self) -> CollabSessionState {
        self.state.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_collab_peer_joining_and_conflict_detection() {
        let mut collab = MultiDevCollabEngine::new("room-vibe-42", "alice");
        let bob = collab.join_peer("bob");
        assert_eq!(collab.state.connected_peers.len(), 2);

        collab.update_cursor(&bob.peer_id, "src/lib.rs", 42);
        assert_eq!(collab.state.connected_peers.get(&bob.peer_id).unwrap().cursor_line, Some(42));

        // Alice intends to edit lines 10..30
        let conflicts1 = collab.submit_intent(CollabPatchIntent {
            author_peer_id: "peer_0001".to_string(),
            target_file: "src/lib.rs".to_string(),
            start_line: 10,
            end_line: 30,
            patch_summary: "Refactor core loop".to_string(),
        });
        assert!(conflicts1.is_empty());

        // Bob intends to edit lines 25..50 (overlap at 25..30)
        let conflicts2 = collab.submit_intent(CollabPatchIntent {
            author_peer_id: bob.peer_id.clone(),
            target_file: "src/lib.rs".to_string(),
            start_line: 25,
            end_line: 50,
            patch_summary: "Inject logging".to_string(),
        });
        assert_eq!(conflicts2.len(), 1);
        assert_eq!(conflicts2[0].overlapping_lines, (25, 30));
    }
}
