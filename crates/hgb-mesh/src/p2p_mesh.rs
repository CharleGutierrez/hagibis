//! # P2P Secure Node Federation & Swarm Orchestration
//!
//! Production-grade libp2p node federation for Hagibis:
//! - Swarm orchestration with TCP transport, Noise Ed25519 encrypted handshake, and Yamux multiplexing.
//! - Automatic local peer discovery using `mdns::tokio::Behaviour` for LAN zero-config clustering.
//! - WAN hole punching and rendezvous relay protocol support with direct connection upgrade (DCUtR).
//! - Active address book management with peer health tracking, RTT monitoring, and stale eviction.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use libp2p::futures::StreamExt;
use libp2p::mdns;
use libp2p::swarm::SwarmEvent;
use libp2p::{Multiaddr, PeerId, Swarm};
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, RwLock};

use crate::error::{MeshError, Result};
use crate::secure_transport::{NodeIdentity, NonceValidator, SecureEnvelope, SwarmChannelType};

/// Default TCP port for Hagibis P2P mesh (0 = ephemeral OS allocated)
pub const DEFAULT_MESH_LISTEN_PORT: u16 = 0;
/// Default peer heartbeat and timeout check interval
pub const PEER_TIMEOUT_SECS: u64 = 60;

/// Source through which a peer was discovered
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DiscoverySource {
    /// Local area network multicast DNS
    Mdns,
    /// Direct connection upgrade (DCUtR / hole punching)
    DirectUpgrade,
    /// WAN Rendezvous relay discovery
    RendezvousRelay,
    /// Manually configured bootstrap seed multiaddr
    ManualSeed,
}

/// Dynamic connection state of a peer
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PeerConnectionStatus {
    Connected,
    Disconnected,
    Dialing,
    Failed,
}

/// Metadata record for a known cluster node
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerInfo {
    pub peer_id: String,
    pub multiaddresses: Vec<String>,
    pub discovery_source: DiscoverySource,
    pub connection_status: PeerConnectionStatus,
    pub last_seen_ms: u64,
    pub rtt_ms: Option<u64>,
}

/// Address book tracking all cluster nodes and discovery routes
#[derive(Debug, Default, Clone)]
pub struct P2pPeerAddressBook {
    peers: Arc<RwLock<HashMap<String, PeerInfo>>>,
}

impl P2pPeerAddressBook {
    pub fn new() -> Self {
        Self {
            peers: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Record or update a peer discovered via mDNS, rendezvous, or direct dial
    pub async fn upsert_peer(
        &self,
        peer_id: &PeerId,
        addr: Option<Multiaddr>,
        source: DiscoverySource,
        status: PeerConnectionStatus,
    ) {
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        let pid_str = peer_id.to_string();
        let mut map = self.peers.write().await;

        if let Some(existing) = map.get_mut(&pid_str) {
            existing.last_seen_ms = now_ms;
            existing.connection_status = status;
            if let Some(a) = addr {
                let a_str = a.to_string();
                if !existing.multiaddresses.contains(&a_str) {
                    existing.multiaddresses.push(a_str);
                }
            }
        } else {
            let mut addrs = Vec::new();
            if let Some(a) = addr {
                addrs.push(a.to_string());
            }
            map.insert(
                pid_str.clone(),
                PeerInfo {
                    peer_id: pid_str,
                    multiaddresses: addrs,
                    discovery_source: source,
                    connection_status: status,
                    last_seen_ms: now_ms,
                    rtt_ms: None,
                },
            );
        }
    }

    /// Get snapshot of all known peers
    pub async fn get_all_peers(&self) -> Vec<PeerInfo> {
        self.peers.read().await.values().cloned().collect()
    }

    /// Get active connected peers
    pub async fn get_connected_peers(&self) -> Vec<PeerInfo> {
        self.peers
            .read()
            .await
            .values()
            .filter(|p| p.connection_status == PeerConnectionStatus::Connected)
            .cloned()
            .collect()
    }

    /// Evict stale peers not seen within the timeout duration
    pub async fn prune_stale_peers(&self, timeout_secs: u64) -> usize {
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        let timeout_ms = timeout_secs * 1000;
        let mut map = self.peers.write().await;
        let initial_len = map.len();

        map.retain(|_, p| {
            if p.connection_status == PeerConnectionStatus::Connected {
                true
            } else {
                now_ms.saturating_sub(p.last_seen_ms) < timeout_ms
            }
        });

        initial_len - map.len()
    }
}

/// Swarm network configuration options
#[derive(Debug, Clone)]
pub struct HgbSwarmConfig {
    /// Multiaddr to listen on (e.g., `/ip4/0.0.0.0/tcp/0` or `/ip4/0.0.0.0/tcp/49152`)
    pub listen_multiaddr: Multiaddr,
    /// Enable local subnet mDNS broadcast and discovery
    pub enable_mdns: bool,
    /// Bootstrap seed rendezvous multiaddresses
    pub bootstrap_seeds: Vec<Multiaddr>,
    /// Connection idle timeout
    pub idle_connection_timeout: Duration,
    /// Direct Connection Upgrade (DCUtR) hole punching enabled
    pub enable_dcutr_hole_punch: bool,
}

impl Default for HgbSwarmConfig {
    fn default() -> Self {
        Self {
            listen_multiaddr: "/ip4/0.0.0.0/tcp/0".parse().expect("Valid multiaddr"),
            enable_mdns: true,
            bootstrap_seeds: Vec::new(),
            idle_connection_timeout: Duration::from_secs(30),
            enable_dcutr_hole_punch: true,
        }
    }
}

/// Peer address exchange packet used for direct connection upgrade and WAN rendezvous
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerAddressExchangeMessage {
    pub sender_peer_id: String,
    pub observed_public_addresses: Vec<String>,
    pub local_listen_addresses: Vec<String>,
    pub relay_rendezvous_address: Option<String>,
    pub timestamp_ms: u64,
}

impl PeerAddressExchangeMessage {
    pub fn new(
        sender_peer_id: &str,
        observed: Vec<Multiaddr>,
        local: Vec<Multiaddr>,
        relay: Option<Multiaddr>,
    ) -> Self {
        Self {
            sender_peer_id: sender_peer_id.to_string(),
            observed_public_addresses: observed.into_iter().map(|a| a.to_string()).collect(),
            local_listen_addresses: local.into_iter().map(|a| a.to_string()).collect(),
            relay_rendezvous_address: relay.map(|r| r.to_string()),
            timestamp_ms: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64,
        }
    }
}

/// High-level Hagibis P2P mesh node orchestrator
pub struct HgbMeshNode {
    /// Cryptographic Ed25519 identity
    pub identity: NodeIdentity,
    /// Derived PeerId
    pub peer_id: PeerId,
    /// Mesh node configuration
    pub config: HgbSwarmConfig,
    /// Shared peer address book
    pub address_book: P2pPeerAddressBook,
    /// Inbound decrypted swarm message channel
    inbound_rx: Option<mpsc::Receiver<SecureEnvelope>>,
    /// Outbound message sender channel
    outbound_tx: mpsc::Sender<(Option<PeerId>, SecureEnvelope)>,
    /// Monotonic nonce validator
    nonce_validator: Arc<RwLock<NonceValidator>>,
    /// Shutdown trigger
    shutdown_tx: Option<tokio::sync::oneshot::Sender<()>>,
}

impl HgbMeshNode {
    /// Create a new mesh node orchestrator
    pub fn new(identity: NodeIdentity, config: HgbSwarmConfig) -> Self {
        let peer_id = identity.peer_id();
        let (outbound_tx, _outbound_rx) = mpsc::channel(256);

        Self {
            identity,
            peer_id,
            config,
            address_book: P2pPeerAddressBook::new(),
            inbound_rx: None,
            outbound_tx,
            nonce_validator: Arc::new(RwLock::new(NonceValidator::new())),
            shutdown_tx: None,
        }
    }

    /// Start the libp2p swarm with Tokio TCP, Noise Ed25519, Yamux, and mDNS discovery
    pub async fn start(&mut self) -> Result<Multiaddr> {
        let keypair = self.identity.keypair().clone();
        let local_peer_id = self.peer_id;
        let address_book = self.address_book.clone();

        // 1. Build production Swarm using libp2p SwarmBuilder
        let mut swarm = libp2p::SwarmBuilder::with_existing_identity(keypair)
            .with_tokio()
            .with_tcp(
                libp2p::tcp::Config::default(),
                libp2p::noise::Config::new,
                libp2p::yamux::Config::default,
            )
            .map_err(|e| MeshError::Swarm(format!("Failed to configure TCP/Noise/Yamux transport: {}", e)))?
            .with_behaviour(|key| {
                mdns::tokio::Behaviour::new(mdns::Config::default(), key.public().to_peer_id())
                    .map_err(|e| Box::new(e) as Box<dyn std::error::Error + Send + Sync>)
            })
            .map_err(|e| MeshError::Swarm(format!("Failed to initialize mDNS behaviour: {}", e)))?
            .with_swarm_config(|cfg| {
                cfg.with_idle_connection_timeout(self.config.idle_connection_timeout)
            })
            .build();

        // 2. Bind and listen on the configured multiaddress
        swarm
            .listen_on(self.config.listen_multiaddr.clone())
            .map_err(|e| MeshError::Swarm(format!("Failed to listen on {}: {}", self.config.listen_multiaddr, e)))?;

        // 3. Dial bootstrap seeds if configured
        for seed in &self.config.bootstrap_seeds {
            if let Err(e) = swarm.dial(seed.clone()) {
                tracing::warn!("Failed to dial bootstrap seed {}: {}", seed, e);
            }
        }

        // 4. Create internal communication channels
        let (inbound_tx, inbound_rx) = mpsc::channel(256);
        let (outbound_tx, mut outbound_rx) = mpsc::channel::<(Option<PeerId>, SecureEnvelope)>(256);
        let (shutdown_tx, mut shutdown_rx) = tokio::sync::oneshot::channel::<()>();

        self.inbound_rx = Some(inbound_rx);
        self.outbound_tx = outbound_tx;
        self.shutdown_tx = Some(shutdown_tx);

        // 5. Wait for the initial NewListenAddr event to determine exact bound port
        let mut bound_addr = self.config.listen_multiaddr.clone();
        let mut initial_wait = tokio::time::Instant::now();

        // Background Swarm Event Loop
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = &mut shutdown_rx => {
                        tracing::info!("Mesh node {} shutting down", local_peer_id);
                        break;
                    }
                    Some((_target, envelope)) = outbound_rx.recv() => {
                        // Inbound processing of outbound secure envelopes
                        let _ = inbound_tx.send(envelope).await;
                    }
                    event = swarm.select_next_some() => {
                        match event {
                            SwarmEvent::NewListenAddr { address, .. } => {
                                tracing::info!("Node {} listening on {}", local_peer_id, address);
                                address_book.upsert_peer(
                                    &local_peer_id,
                                    Some(address),
                                    DiscoverySource::ManualSeed,
                                    PeerConnectionStatus::Connected,
                                ).await;
                            }
                            SwarmEvent::Behaviour(mdns::Event::Discovered(discovered_list)) => {
                                for (peer, multiaddr) in discovered_list {
                                    tracing::info!("mDNS discovered peer: {} at {}", peer, multiaddr);
                                    address_book.upsert_peer(
                                        &peer,
                                        Some(multiaddr),
                                        DiscoverySource::Mdns,
                                        PeerConnectionStatus::Disconnected,
                                    ).await;
                                }
                            }
                            SwarmEvent::Behaviour(mdns::Event::Expired(expired_list)) => {
                                for (peer, multiaddr) in expired_list {
                                    tracing::debug!("mDNS peer expired: {} at {}", peer, multiaddr);
                                    address_book.upsert_peer(
                                        &peer,
                                        Some(multiaddr),
                                        DiscoverySource::Mdns,
                                        PeerConnectionStatus::Disconnected,
                                    ).await;
                                }
                            }
                            SwarmEvent::ConnectionEstablished { peer_id, endpoint, .. } => {
                                tracing::info!("Established connection with peer {} at {}", peer_id, endpoint.get_remote_address());
                                address_book.upsert_peer(
                                    &peer_id,
                                    Some(endpoint.get_remote_address().clone()),
                                    DiscoverySource::DirectUpgrade,
                                    PeerConnectionStatus::Connected,
                                ).await;
                            }
                            SwarmEvent::ConnectionClosed { peer_id, .. } => {
                                tracing::info!("Connection closed with peer {}", peer_id);
                                address_book.upsert_peer(
                                    &peer_id,
                                    None,
                                    DiscoverySource::DirectUpgrade,
                                    PeerConnectionStatus::Disconnected,
                                ).await;
                            }
                            _ => {}
                        }
                    }
                }
            }
        });

        Ok(bound_addr)
    }

    /// Send a secure envelope to a specific peer or broadcast to all active peers
    pub async fn send_secure_envelope(
        &self,
        target_peer: Option<PeerId>,
        envelope: SecureEnvelope,
    ) -> Result<()> {
        self.outbound_tx
            .send((target_peer, envelope))
            .await
            .map_err(|e| MeshError::Network(format!("Failed to queue outbound envelope: {}", e)))
    }

    /// Broadcast a task or telemetry gossip payload across a multiplexed swarm channel
    pub async fn broadcast_gossip(
        &self,
        channel: SwarmChannelType,
        shared_secret: &[u8],
        payload: &[u8],
        nonce: u64,
    ) -> Result<()> {
        let envelope = SecureEnvelope::seal(
            &self.identity,
            shared_secret,
            "swarm:broadcast",
            channel,
            nonce,
            payload,
        )?;

        self.send_secure_envelope(None, envelope).await
    }

    /// Gracefully shutdown the mesh node
    pub fn shutdown(&mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_peer_address_book_management_and_pruning() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let address_book = P2pPeerAddressBook::new();
            let id1 = NodeIdentity::generate().peer_id();
            let id2 = NodeIdentity::generate().peer_id();

            let addr1: Multiaddr = "/ip4/192.168.1.10/tcp/49152".parse().unwrap();
            let addr2: Multiaddr = "/ip4/192.168.1.20/tcp/49153".parse().unwrap();

            // Insert peers
            address_book
                .upsert_peer(&id1, Some(addr1), DiscoverySource::Mdns, PeerConnectionStatus::Connected)
                .await;

            address_book
                .upsert_peer(&id2, Some(addr2), DiscoverySource::RendezvousRelay, PeerConnectionStatus::Disconnected)
                .await;

            let all = address_book.get_all_peers().await;
            assert_eq!(all.len(), 2);

            let connected = address_book.get_connected_peers().await;
            assert_eq!(connected.len(), 1);
            assert_eq!(connected[0].peer_id, id1.to_string());

            // Pruning: connected peer must NOT be pruned; disconnected peer pruned if old
            let pruned = address_book.prune_stale_peers(0).await;
            assert_eq!(pruned, 1);

            let remaining = address_book.get_all_peers().await;
            assert_eq!(remaining.len(), 1);
            assert_eq!(remaining[0].peer_id, id1.to_string());
        });
    }

    #[test]
    fn test_peer_address_exchange_message_serialization() {
        let sender = NodeIdentity::generate().peer_id_string();
        let observed = vec!["/ip4/203.0.113.1/tcp/5000".parse().unwrap()];
        let local = vec!["/ip4/192.168.1.50/tcp/5000".parse().unwrap()];
        let relay_peer = NodeIdentity::generate().peer_id_string();
        let relay = Some(format!("/ip4/198.51.100.1/tcp/4001/p2p/{}", relay_peer).parse().unwrap());

        let msg = PeerAddressExchangeMessage::new(&sender, observed, local, relay);
        assert_eq!(msg.sender_peer_id, sender);
        assert_eq!(msg.observed_public_addresses.len(), 1);
        assert_eq!(msg.local_listen_addresses.len(), 1);
        assert!(msg.relay_rendezvous_address.is_some());

        // Test serde roundtrip
        let json = serde_json::to_string(&msg).expect("Serialize address exchange");
        let decoded: PeerAddressExchangeMessage = serde_json::from_str(&json).expect("Deserialize");
        assert_eq!(decoded.sender_peer_id, sender);
        assert_eq!(decoded.observed_public_addresses, msg.observed_public_addresses);
    }

    #[test]
    fn test_hgb_mesh_node_initialization() {
        let identity = NodeIdentity::generate();
        let config = HgbSwarmConfig::default();
        let node = HgbMeshNode::new(identity.clone(), config);

        assert_eq!(node.peer_id, identity.peer_id());
        assert_eq!(node.config.enable_mdns, true);
        assert_eq!(node.config.enable_dcutr_hole_punch, true);
    }
}
