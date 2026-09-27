//! # Superpower 77: MultiplayerSwarmHub
//!
//! Collaborative Real-Time Multiplayer Vibe Swarm session coordinator over hgbd,
//! synchronizing peer presence, shared speculative racing, and flight graph updates.

use crate::error::{HgbError, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, OnceLock, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};

/// Role assigned to a peer in the multiplayer session
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SwarmPeerRole {
    Driver,
    Navigator,
    Reviewer,
    Spectator,
}

impl std::fmt::Display for SwarmPeerRole {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SwarmPeerRole::Driver => write!(f, "Driver (Lead Prompt Engineer)"),
            SwarmPeerRole::Navigator => write!(f, "Navigator (Spec Arbiter)"),
            SwarmPeerRole::Reviewer => write!(f, "Reviewer (Diff Gatekeeper)"),
            SwarmPeerRole::Spectator => write!(f, "Spectator"),
        }
    }
}

/// Swarm peer metadata and live cursor presence
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SwarmPeer {
    pub peer_id: String,
    pub username: String,
    pub role: SwarmPeerRole,
    pub cursor_file: Option<String>,
    pub cursor_line: Option<usize>,
    pub active_model: String,
    pub connected_at_epoch: u64,
    pub last_heartbeat_epoch: u64,
}

impl SwarmPeer {
    pub fn new(peer_id: &str, username: &str, role: SwarmPeerRole, model: &str) -> Self {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        Self {
            peer_id: peer_id.to_string(),
            username: username.to_string(),
            role,
            cursor_file: None,
            cursor_line: None,
            active_model: model.to_string(),
            connected_at_epoch: now,
            last_heartbeat_epoch: now,
        }
    }
}

/// Active multiplayer session state
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MultiplayerSession {
    pub session_id: String,
    pub session_name: String,
    pub host_peer_id: String,
    pub peers: HashMap<String, SwarmPeer>,
    pub active_race_id: Option<String>,
    pub active_race_prompt: Option<String>,
    pub active_race_winner: Option<String>,
    pub flight_graph_snapshot: Option<serde_json::Value>,
    pub flight_graph_version: u64,
    pub created_at_epoch: u64,
}

/// Public report summarizing a multiplayer session
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MultiplayerSessionReport {
    pub session_id: String,
    pub session_name: String,
    pub host_peer_id: String,
    pub peer_count: usize,
    pub peers: Vec<SwarmPeer>,
    pub active_race_id: Option<String>,
    pub active_race_prompt: Option<String>,
    pub active_race_winner: Option<String>,
    pub flight_graph_version: u64,
    pub status: String,
}

/// Presence update result
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PresenceUpdateReport {
    pub session_id: String,
    pub peer_id: String,
    pub cursor_file: Option<String>,
    pub cursor_line: Option<usize>,
    pub active_peers: usize,
}

/// Shared speculative race synchronization result
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SharedRaceSyncReport {
    pub session_id: String,
    pub race_id: String,
    pub prompt: String,
    pub consensus_winner: Option<String>,
    pub synced_peers: usize,
}

/// Flight Graph DAG synchronization report
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FlightGraphSyncReport {
    pub session_id: String,
    pub version: u64,
    pub node_count: usize,
    pub broadcast_success: bool,
}

/// Real-Time Multiplayer Vibe Swarm Session Hub
pub struct MultiplayerSwarmHub {
    sessions: Arc<RwLock<HashMap<String, MultiplayerSession>>>,
}

impl Default for MultiplayerSwarmHub {
    fn default() -> Self {
        Self::new()
    }
}

impl MultiplayerSwarmHub {
    pub fn new() -> Self {
        Self {
            sessions: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Global singleton instance for resident daemon coordination
    pub fn global() -> &'static MultiplayerSwarmHub {
        static HUB: OnceLock<MultiplayerSwarmHub> = OnceLock::new();
        HUB.get_or_init(MultiplayerSwarmHub::new)
    }

    /// Create or initialize a collaborative multiplayer session
    pub fn create_or_join_session(
        &self,
        session_id: &str,
        session_name: &str,
        peer: SwarmPeer,
    ) -> MultiplayerSessionReport {
        let mut map = self.sessions.write().unwrap();
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();

        let session = map.entry(session_id.to_string()).or_insert_with(|| MultiplayerSession {
            session_id: session_id.to_string(),
            session_name: session_name.to_string(),
            host_peer_id: peer.peer_id.clone(),
            peers: HashMap::new(),
            active_race_id: None,
            active_race_prompt: None,
            active_race_winner: None,
            flight_graph_snapshot: None,
            flight_graph_version: 1,
            created_at_epoch: now,
        });

        session.peers.insert(peer.peer_id.clone(), peer);
        session_to_report(session)
    }

    /// Update peer cursor presence (file, line number, active model)
    pub fn update_presence(
        &self,
        session_id: &str,
        peer_id: &str,
        cursor_file: Option<String>,
        cursor_line: Option<usize>,
    ) -> Result<PresenceUpdateReport> {
        let mut map = self.sessions.write().unwrap();
        let session = map.get_mut(session_id).ok_or_else(|| {
            HgbError::InvalidInput(format!("Multiplayer session '{}' not found", session_id))
        })?;

        let peer = session.peers.get_mut(peer_id).ok_or_else(|| {
            HgbError::InvalidInput(format!("Peer '{}' not in session '{}'", peer_id, session_id))
        })?;

        peer.cursor_file = cursor_file.clone();
        peer.cursor_line = cursor_line;
        peer.last_heartbeat_epoch = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();

        Ok(PresenceUpdateReport {
            session_id: session_id.to_string(),
            peer_id: peer_id.to_string(),
            cursor_file,
            cursor_line,
            active_peers: session.peers.len(),
        })
    }

    /// Synchronize a speculative dual-draft / tri-model race across all connected peers
    pub fn sync_race(
        &self,
        session_id: &str,
        race_id: &str,
        prompt: &str,
        winner: Option<String>,
    ) -> Result<SharedRaceSyncReport> {
        let mut map = self.sessions.write().unwrap();
        let session = map.get_mut(session_id).ok_or_else(|| {
            HgbError::InvalidInput(format!("Multiplayer session '{}' not found", session_id))
        })?;

        session.active_race_id = Some(race_id.to_string());
        session.active_race_prompt = Some(prompt.to_string());
        session.active_race_winner = winner.clone();

        Ok(SharedRaceSyncReport {
            session_id: session_id.to_string(),
            race_id: race_id.to_string(),
            prompt: prompt.to_string(),
            consensus_winner: winner,
            synced_peers: session.peers.len(),
        })
    }

    /// Broadcast real-time Flight Graph DAG updates to all swarm participants
    pub fn broadcast_flight_graph(
        &self,
        session_id: &str,
        graph_snapshot: serde_json::Value,
    ) -> Result<FlightGraphSyncReport> {
        let mut map = self.sessions.write().unwrap();
        let session = map.get_mut(session_id).ok_or_else(|| {
            HgbError::InvalidInput(format!("Multiplayer session '{}' not found", session_id))
        })?;

        session.flight_graph_version += 1;
        let node_count = graph_snapshot
            .get("nodes")
            .and_then(|n| n.as_array())
            .map(|a| a.len())
            .unwrap_or(0);
        session.flight_graph_snapshot = Some(graph_snapshot);

        Ok(FlightGraphSyncReport {
            session_id: session_id.to_string(),
            version: session.flight_graph_version,
            node_count,
            broadcast_success: true,
        })
    }

    /// Retrieve session status report
    pub fn get_session_report(&self, session_id: &str) -> Result<MultiplayerSessionReport> {
        let map = self.sessions.read().unwrap();
        let session = map.get(session_id).ok_or_else(|| {
            HgbError::InvalidInput(format!("Multiplayer session '{}' not found", session_id))
        })?;
        Ok(session_to_report(session))
    }

    /// Disconnect and remove peer from session
    pub fn leave_session(&self, session_id: &str, peer_id: &str) -> Result<bool> {
        let mut map = self.sessions.write().unwrap();
        if let Some(session) = map.get_mut(session_id) {
            let removed = session.peers.remove(peer_id).is_some();
            return Ok(removed);
        }
        Ok(false)
    }

    /// Prune peers whose heartbeat exceeded timeout
    pub fn prune_stale_peers(&self, session_id: &str, timeout_secs: u64) -> Result<usize> {
        let mut map = self.sessions.write().unwrap();
        let session = map.get_mut(session_id).ok_or_else(|| {
            HgbError::InvalidInput(format!("Multiplayer session '{}' not found", session_id))
        })?;

        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
        let initial_count = session.peers.len();
        session.peers.retain(|_, peer| {
            now.saturating_sub(peer.last_heartbeat_epoch) <= timeout_secs
        });

        Ok(initial_count - session.peers.len())
    }
}

fn session_to_report(s: &MultiplayerSession) -> MultiplayerSessionReport {
    let mut peer_list: Vec<SwarmPeer> = s.peers.values().cloned().collect();
    peer_list.sort_by(|a, b| a.username.cmp(&b.username));

    MultiplayerSessionReport {
        session_id: s.session_id.clone(),
        session_name: s.session_name.clone(),
        host_peer_id: s.host_peer_id.clone(),
        peer_count: peer_list.len(),
        peers: peer_list,
        active_race_id: s.active_race_id.clone(),
        active_race_prompt: s.active_race_prompt.clone(),
        active_race_winner: s.active_race_winner.clone(),
        flight_graph_version: s.flight_graph_version,
        status: "active".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_multiplayer_session_lifecycle() {
        let hub = MultiplayerSwarmHub::new();

        // 1. Host creates session
        let host = SwarmPeer::new("p1", "alice", SwarmPeerRole::Driver, "gemini-2.5-pro");
        let rep = hub.create_or_join_session("sess-alpha", "Frontend Swarm", host);
        assert_eq!(rep.session_id, "sess-alpha");
        assert_eq!(rep.peer_count, 1);
        assert_eq!(rep.host_peer_id, "p1");

        // 2. Peer joins session
        let bob = SwarmPeer::new("p2", "bob", SwarmPeerRole::Navigator, "qwen2.5-coder:7b");
        let rep2 = hub.create_or_join_session("sess-alpha", "Frontend Swarm", bob);
        assert_eq!(rep2.peer_count, 2);

        // 3. Update presence
        let pres = hub.update_presence("sess-alpha", "p2", Some("src/App.tsx".to_string()), Some(42)).unwrap();
        assert_eq!(pres.cursor_line, Some(42));
        assert_eq!(pres.cursor_file.as_deref(), Some("src/App.tsx"));

        // 4. Sync race
        let race = hub.sync_race("sess-alpha", "race-99", "Build payment checkout button", Some("gemini-2.5-pro".to_string())).unwrap();
        assert_eq!(race.race_id, "race-99");
        assert_eq!(race.consensus_winner.as_deref(), Some("gemini-2.5-pro"));

        // 5. Broadcast Flight Graph
        let graph = serde_json::json!({
            "nodes": [
                { "id": "step1", "label": "Tokenize" },
                { "id": "step2", "label": "Generate AST" }
            ]
        });
        let f_rep = hub.broadcast_flight_graph("sess-alpha", graph).unwrap();
        assert_eq!(f_rep.node_count, 2);
        assert_eq!(f_rep.version, 2);

        // 6. Leave
        let left = hub.leave_session("sess-alpha", "p2").unwrap();
        assert!(left);
        let final_rep = hub.get_session_report("sess-alpha").unwrap();
        assert_eq!(final_rep.peer_count, 1);
    }
}
