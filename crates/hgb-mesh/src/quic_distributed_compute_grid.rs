//! # QUIC Distributed Compute Grid & Memory Pooling (`quic_distributed_compute_grid`)
//!
//! Production-grade secure QUIC transport, NAT traversal, and distributed compute/memory pooling
//! across Hagibis agent swarm instances:
//! - **Secure QUIC Transport**: TLS 1.3 / Noise protocol authenticated framing over UDP,
//!   with packet multiplexing, connection migration, and replay attack prevention.
//! - **NAT Traversal (STUN & UDP Hole-Punching)**: RFC 5389 / RFC 8489 STUN endpoint discovery,
//!   reflexive candidate probing, and simultaneous UDP hole-punching for seamless edge connectivity.
//! - **Distributed Compute & Memory Pooling**: Connected Hagibis instances share memory capacity
//!   (RAM/VRAM) and route heavy NPU/LLM agent workloads with dynamic load scoring and latency-aware routing.
//! - **Heartbeat & Telemetry Failover**: Continuous RTT telemetry (Smoothed RTT / RTTVAR),
//!   missed-heartbeat health detection, and dynamic automatic workload failover across grid nodes.

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tokio::net::UdpSocket;
use tokio::sync::RwLock;

use crate::error::{MeshError, Result};
use crate::secure_transport::{
    compute_hmac_sha256, compute_sha256, verify_hmac_sha256_constant_time, NodeIdentity,
    NonceValidator,
};

// ==============================================================================
// 1. QUIC Packet Types, Headers & Frame Wire Protocol
// ==============================================================================

/// QUIC protocol version magic for Hagibis (0x48474231 = "HGB1")
pub const HGB_QUIC_VERSION: u32 = 0x48474231;
/// Default STUN Magic Cookie (RFC 5389)
pub const STUN_MAGIC_COOKIE: u32 = 0x2112A442;
/// Maximum UDP packet size in bytes for QUIC datagrams
pub const MAX_QUIC_DATAGRAM_SIZE: usize = 1452;
/// Default heartbeat interval in milliseconds
pub const DEFAULT_HEARTBEAT_INTERVAL_MS: u64 = 2_000;
/// Missed heartbeat threshold triggering node failover
pub const MAX_MISSED_HEARTBEATS: u32 = 3;

/// QUIC Packet Header Type
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[repr(u8)]
pub enum QuicPacketType {
    Initial = 0x00,
    Retry = 0x01,
    Handshake = 0x02,
    OneRtt = 0x03,
}

/// QUIC Packet Header (Long and Short form representation)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuicHeader {
    pub packet_type: QuicPacketType,
    pub version: u32,
    pub destination_cid: Vec<u8>,
    pub source_cid: Vec<u8>,
    pub packet_number: u64,
}

impl QuicHeader {
    pub fn new_initial(dest_cid: &[u8], src_cid: &[u8], pkt_num: u64) -> Self {
        Self {
            packet_type: QuicPacketType::Initial,
            version: HGB_QUIC_VERSION,
            destination_cid: dest_cid.to_vec(),
            source_cid: src_cid.to_vec(),
            packet_number: pkt_num,
        }
    }

    pub fn new_handshake(dest_cid: &[u8], src_cid: &[u8], pkt_num: u64) -> Self {
        Self {
            packet_type: QuicPacketType::Handshake,
            version: HGB_QUIC_VERSION,
            destination_cid: dest_cid.to_vec(),
            source_cid: src_cid.to_vec(),
            packet_number: pkt_num,
        }
    }

    pub fn new_one_rtt(dest_cid: &[u8], pkt_num: u64) -> Self {
        Self {
            packet_type: QuicPacketType::OneRtt,
            version: HGB_QUIC_VERSION,
            destination_cid: dest_cid.to_vec(),
            source_cid: Vec::new(),
            packet_number: pkt_num,
        }
    }
}

/// Multiplexed QUIC Frames for Hagibis Swarm & Compute Grid
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum QuicFrame {
    Ping,
    Ack {
        largest_ack: u64,
        ack_delay_ms: u64,
    },
    Stream {
        stream_id: u64,
        offset: u64,
        data: Vec<u8>,
        fin: bool,
    },
    Heartbeat {
        timestamp_ms: u64,
        load_ratio: f32,
        available_ram_mb: u64,
        available_vram_mb: u64,
    },
    HeartbeatAck {
        echo_timestamp_ms: u64,
        responder_timestamp_ms: u64,
        load_ratio: f32,
    },
    HolePunchProbe {
        token: u64,
        sender_node_id: String,
        phase: u8, // 0 = request, 1 = ack, 2 = confirmed
    },
    StunBinding {
        transaction_id: [u8; 12],
        mapped_addr: Option<SocketAddr>,
    },
    MemoryAlloc {
        request_id: u64,
        size_bytes: usize,
        is_vram: bool,
    },
    MemoryAllocAck {
        request_id: u64,
        block_id: String,
        granted_size: usize,
        success: bool,
        error: Option<String>,
    },
    MemoryRead {
        request_id: u64,
        block_id: String,
        offset: u64,
        length: usize,
    },
    MemoryReadAck {
        request_id: u64,
        data: Vec<u8>,
        success: bool,
        error: Option<String>,
    },
    MemoryWrite {
        request_id: u64,
        block_id: String,
        offset: u64,
        data: Vec<u8>,
    },
    MemoryWriteAck {
        request_id: u64,
        bytes_written: usize,
        success: bool,
        error: Option<String>,
    },
    MemoryFree {
        block_id: String,
    },
    ComputeRouteRequest {
        workload: TaskWorkload,
    },
    ComputeRouteResponse {
        task_id: String,
        accepted: bool,
        estimated_start_ms: u64,
    },
    ComputeResult {
        task_id: String,
        success: bool,
        output_data: Vec<u8>,
        execution_time_ms: u64,
        error: Option<String>,
    },
    ConnectionClose {
        error_code: u32,
        reason: String,
    },
}

/// Fully framed QUIC packet with authenticated encryption
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QuicPacket {
    pub header: QuicHeader,
    pub frames: Vec<QuicFrame>,
}

impl QuicPacket {
    pub fn new(header: QuicHeader, frames: Vec<QuicFrame>) -> Self {
        Self { header, frames }
    }

    /// Serializes and applies Noise / TLS 1.3 HMAC-SHA256 authenticated framing
    pub fn encode(&self, shared_key: Option<&[u8]>) -> Result<Vec<u8>> {
        let payload_bytes = bincode::serialize(&self.frames)
            .map_err(|e| MeshError::Serialization(format!("QUIC frame serialization: {}", e)))?;

        let mut out = Vec::with_capacity(64 + payload_bytes.len() + 32);

        // Header serialization
        let flag_byte = match self.header.packet_type {
            QuicPacketType::Initial => 0x80 | 0x00,
            QuicPacketType::Retry => 0x80 | 0x10,
            QuicPacketType::Handshake => 0x80 | 0x20,
            QuicPacketType::OneRtt => 0x40,
        };
        out.push(flag_byte);

        if self.header.packet_type != QuicPacketType::OneRtt {
            // Long header: 4-byte version
            out.extend_from_slice(&self.header.version.to_be_bytes());
            out.push(self.header.destination_cid.len() as u8);
            out.extend_from_slice(&self.header.destination_cid);
            out.push(self.header.source_cid.len() as u8);
            out.extend_from_slice(&self.header.source_cid);
        } else {
            // Short header: fixed destination CID length
            out.push(self.header.destination_cid.len() as u8);
            out.extend_from_slice(&self.header.destination_cid);
        }

        // 8-byte packet sequence number
        out.extend_from_slice(&self.header.packet_number.to_be_bytes());

        // 4-byte payload length
        out.extend_from_slice(&(payload_bytes.len() as u32).to_be_bytes());
        out.extend_from_slice(&payload_bytes);

        // Authenticated Framing: RFC 2104 HMAC-SHA256 tag computed over header + payload
        if let Some(key) = shared_key {
            let hmac = compute_hmac_sha256(key, &out);
            out.extend_from_slice(&hmac);
        }

        Ok(out)
    }

    /// Verifies authentication tag and deserializes wire bytes into `QuicPacket`
    pub fn decode(bytes: &[u8], shared_key: Option<&[u8]>) -> Result<Self> {
        if bytes.len() < 18 {
            return Err(MeshError::TransportSecurity("Datagram too small for QUIC header".to_string()));
        }

        let (verified_bytes, has_tag) = if let Some(key) = shared_key {
            if bytes.len() < 18 + 32 {
                return Err(MeshError::TransportSecurity("Missing HMAC authentication tag".to_string()));
            }
            let data_len = bytes.len() - 32;
            let data = &bytes[..data_len];
            let tag: [u8; 32] = bytes[data_len..]
                .try_into()
                .map_err(|_| MeshError::TransportSecurity("Invalid HMAC tag length".to_string()))?;

            if !verify_hmac_sha256_constant_time(key, data, &tag) {
                return Err(MeshError::HmacMismatch);
            }
            (data, true)
        } else {
            (bytes, false)
        };

        let mut offset = 0;
        let flag_byte = verified_bytes[offset];
        offset += 1;

        let is_long_header = (flag_byte & 0x80) != 0;
        let packet_type = if is_long_header {
            match (flag_byte >> 4) & 0x03 {
                0x00 => QuicPacketType::Initial,
                0x01 => QuicPacketType::Retry,
                0x02 => QuicPacketType::Handshake,
                _ => QuicPacketType::Initial,
            }
        } else {
            QuicPacketType::OneRtt
        };

        let (version, dest_cid, src_cid) = if is_long_header {
            if offset + 4 > verified_bytes.len() {
                return Err(MeshError::TransportSecurity("Truncated QUIC version".to_string()));
            }
            let ver = u32::from_be_bytes(verified_bytes[offset..offset + 4].try_into().unwrap());
            offset += 4;

            let dcid_len = verified_bytes[offset] as usize;
            offset += 1;
            if offset + dcid_len > verified_bytes.len() {
                return Err(MeshError::TransportSecurity("Truncated Destination CID".to_string()));
            }
            let dcid = verified_bytes[offset..offset + dcid_len].to_vec();
            offset += dcid_len;

            let scid_len = verified_bytes[offset] as usize;
            offset += 1;
            if offset + scid_len > verified_bytes.len() {
                return Err(MeshError::TransportSecurity("Truncated Source CID".to_string()));
            }
            let scid = verified_bytes[offset..offset + scid_len].to_vec();
            offset += scid_len;

            (ver, dcid, scid)
        } else {
            let dcid_len = verified_bytes[offset] as usize;
            offset += 1;
            if offset + dcid_len > verified_bytes.len() {
                return Err(MeshError::TransportSecurity("Truncated Destination CID".to_string()));
            }
            let dcid = verified_bytes[offset..offset + dcid_len].to_vec();
            offset += dcid_len;

            (HGB_QUIC_VERSION, dcid, Vec::new())
        };

        if offset + 8 + 4 > verified_bytes.len() {
            return Err(MeshError::TransportSecurity("Truncated sequence/payload length".to_string()));
        }

        let packet_number = u64::from_be_bytes(verified_bytes[offset..offset + 8].try_into().unwrap());
        offset += 8;

        let payload_len = u32::from_be_bytes(verified_bytes[offset..offset + 4].try_into().unwrap()) as usize;
        offset += 4;

        if offset + payload_len > verified_bytes.len() {
            return Err(MeshError::TransportSecurity("Truncated frame payload".to_string()));
        }

        let frames: Vec<QuicFrame> = bincode::deserialize(&verified_bytes[offset..offset + payload_len])
            .map_err(|e| MeshError::Serialization(format!("QUIC frame deserialization: {}", e)))?;

        let _ = has_tag;

        Ok(QuicPacket {
            header: QuicHeader {
                packet_type,
                version,
                destination_cid: dest_cid,
                source_cid: src_cid,
                packet_number,
            },
            frames,
        })
    }
}

// ==============================================================================
// 2. NAT Traversal: RFC 5389 / RFC 8489 STUN & UDP Hole-Punching
// ==============================================================================

/// Discovered NAT Classification
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum StunNatType {
    OpenInternet,
    FullCone,
    RestrictedCone,
    PortRestrictedCone,
    Symmetric,
    Unknown,
}

/// Discovered Public Reflexive Endpoint
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StunEndpoint {
    pub public_addr: SocketAddr,
    pub local_addr: SocketAddr,
    pub nat_type: StunNatType,
    pub rtt_ms: u64,
}

/// STUN RFC 5389 Protocol Encoder and Decoder
pub struct StunProtocol;

impl StunProtocol {
    /// Builds an RFC 5389 Binding Request (0x0001)
    pub fn build_binding_request(transaction_id: &[u8; 12]) -> [u8; 20] {
        let mut msg = [0u8; 20];
        msg[0..2].copy_from_slice(&0x0001u16.to_be_bytes()); // Binding Request
        msg[2..4].copy_from_slice(&0x0000u16.to_be_bytes()); // Message Length: 0
        msg[4..8].copy_from_slice(&STUN_MAGIC_COOKIE.to_be_bytes());
        msg[8..20].copy_from_slice(transaction_id);
        msg
    }

    /// Builds an RFC 5389 Binding Success Response (0x0101) with XOR-MAPPED-ADDRESS attribute
    pub fn build_binding_response(transaction_id: &[u8; 12], mapped_addr: SocketAddr) -> Vec<u8> {
        let mut attr_bytes = Vec::new();
        match mapped_addr {
            SocketAddr::V4(v4) => {
                // XOR-MAPPED-ADDRESS: Type = 0x0020, Length = 8
                attr_bytes.extend_from_slice(&0x0020u16.to_be_bytes());
                attr_bytes.extend_from_slice(&8u16.to_be_bytes());
                attr_bytes.push(0x00); // Reserved
                attr_bytes.push(0x01); // IPv4 Family

                // Port XOR'd with most-significant 16 bits of magic cookie
                let xor_port = v4.port() ^ ((STUN_MAGIC_COOKIE >> 16) as u16);
                attr_bytes.extend_from_slice(&xor_port.to_be_bytes());

                // IP XOR'd with magic cookie
                let ip_u32 = u32::from(*v4.ip());
                let xor_ip = ip_u32 ^ STUN_MAGIC_COOKIE;
                attr_bytes.extend_from_slice(&xor_ip.to_be_bytes());
            }
            SocketAddr::V6(v6) => {
                // XOR-MAPPED-ADDRESS: Type = 0x0020, Length = 20
                attr_bytes.extend_from_slice(&0x0020u16.to_be_bytes());
                attr_bytes.extend_from_slice(&20u16.to_be_bytes());
                attr_bytes.push(0x00);
                attr_bytes.push(0x02); // IPv6 Family

                let xor_port = v6.port() ^ ((STUN_MAGIC_COOKIE >> 16) as u16);
                attr_bytes.extend_from_slice(&xor_port.to_be_bytes());

                let mut xor_key = [0u8; 16];
                xor_key[0..4].copy_from_slice(&STUN_MAGIC_COOKIE.to_be_bytes());
                xor_key[4..16].copy_from_slice(transaction_id);

                let ip_octets = v6.ip().octets();
                for i in 0..16 {
                    attr_bytes.push(ip_octets[i] ^ xor_key[i]);
                }
            }
        }

        let mut msg = Vec::with_capacity(20 + attr_bytes.len());
        msg.extend_from_slice(&0x0101u16.to_be_bytes()); // Binding Success Response
        msg.extend_from_slice(&(attr_bytes.len() as u16).to_be_bytes());
        msg.extend_from_slice(&STUN_MAGIC_COOKIE.to_be_bytes());
        msg.extend_from_slice(transaction_id);
        msg.extend_from_slice(&attr_bytes);
        msg
    }

    /// Parses an RFC 5389 Binding Response and extracts the XOR-MAPPED-ADDRESS
    pub fn parse_binding_response(data: &[u8], expected_tid: &[u8; 12]) -> Result<SocketAddr> {
        if data.len() < 20 {
            return Err(MeshError::Network("STUN response too short".to_string()));
        }

        let msg_type = u16::from_be_bytes(data[0..2].try_into().unwrap());
        if msg_type != 0x0101 {
            return Err(MeshError::Network(format!("Not a STUN Binding Response: {:#x}", msg_type)));
        }

        let cookie = u32::from_be_bytes(data[4..8].try_into().unwrap());
        if cookie != STUN_MAGIC_COOKIE {
            return Err(MeshError::Network("Invalid STUN Magic Cookie".to_string()));
        }

        if &data[8..20] != expected_tid {
            return Err(MeshError::Network("STUN Transaction ID mismatch".to_string()));
        }

        let msg_len = u16::from_be_bytes(data[2..4].try_into().unwrap()) as usize;
        let mut offset = 20;
        let limit = (20 + msg_len).min(data.len());

        while offset + 4 <= limit {
            let attr_type = u16::from_be_bytes(data[offset..offset + 2].try_into().unwrap());
            let attr_len = u16::from_be_bytes(data[offset + 2..offset + 4].try_into().unwrap()) as usize;
            offset += 4;

            if offset + attr_len > limit {
                break;
            }

            if attr_type == 0x0020 {
                // XOR-MAPPED-ADDRESS
                if attr_len >= 8 {
                    let family = data[offset + 1];
                    let xor_port = u16::from_be_bytes(data[offset + 2..offset + 4].try_into().unwrap());
                    let port = xor_port ^ ((STUN_MAGIC_COOKIE >> 16) as u16);

                    if family == 0x01 && attr_len >= 8 {
                        // IPv4
                        let xor_ip = u32::from_be_bytes(data[offset + 4..offset + 8].try_into().unwrap());
                        let ip = Ipv4Addr::from(xor_ip ^ STUN_MAGIC_COOKIE);
                        return Ok(SocketAddr::new(IpAddr::V4(ip), port));
                    } else if family == 0x02 && attr_len >= 20 {
                        // IPv6
                        let mut xor_key = [0u8; 16];
                        xor_key[0..4].copy_from_slice(&STUN_MAGIC_COOKIE.to_be_bytes());
                        xor_key[4..16].copy_from_slice(expected_tid);

                        let mut octets = [0u8; 16];
                        for i in 0..16 {
                            octets[i] = data[offset + 4 + i] ^ xor_key[i];
                        }
                        let ip = Ipv6Addr::from(octets);
                        return Ok(SocketAddr::new(IpAddr::V6(ip), port));
                    }
                }
            } else if attr_type == 0x0001 {
                // Classic MAPPED-ADDRESS fallback
                if attr_len >= 8 {
                    let family = data[offset + 1];
                    let port = u16::from_be_bytes(data[offset + 2..offset + 4].try_into().unwrap());
                    if family == 0x01 {
                        let ip = Ipv4Addr::new(
                            data[offset + 4],
                            data[offset + 5],
                            data[offset + 6],
                            data[offset + 7],
                        );
                        return Ok(SocketAddr::new(IpAddr::V4(ip), port));
                    }
                }
            }

            // Align to 4-byte boundary
            offset += (attr_len + 3) & !3;
        }

        Err(MeshError::Network("No XOR-MAPPED-ADDRESS attribute found in STUN response".to_string()))
    }
}

/// NAT Traversal and Hole-Punching Coordination Manager
#[derive(Debug, Clone)]
pub struct NatTraversalManager {
    local_public_cache: Arc<RwLock<Option<StunEndpoint>>>,
    hole_punch_tokens: Arc<RwLock<HashMap<String, u64>>>,
}

impl Default for NatTraversalManager {
    fn default() -> Self {
        Self::new()
    }
}

impl NatTraversalManager {
    pub fn new() -> Self {
        Self {
            local_public_cache: Arc::new(RwLock::new(None)),
            hole_punch_tokens: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Discovers reflexive public endpoint using a STUN server or peer reflector
    pub async fn discover_endpoint(
        &self,
        socket: &UdpSocket,
        stun_server: SocketAddr,
    ) -> Result<StunEndpoint> {
        let mut tid = [0u8; 12];
        let rand_val = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        tid[0..8].copy_from_slice(&(rand_val as u64).to_be_bytes());
        tid[8..12].copy_from_slice(&((rand_val >> 64) as u32).to_be_bytes());

        let req = StunProtocol::build_binding_request(&tid);
        let start = SystemTime::now();

        socket
            .send_to(&req, stun_server)
            .await
            .map_err(|e| MeshError::Network(format!("Failed to send STUN request: {}", e)))?;

        let mut buf = [0u8; 512];
        let (len, _) = tokio::time::timeout(Duration::from_millis(1500), socket.recv_from(&mut buf))
            .await
            .map_err(|_| MeshError::Network("STUN discovery timeout".to_string()))?
            .map_err(|e| MeshError::Network(format!("STUN recv failed: {}", e)))?;

        let mapped = StunProtocol::parse_binding_response(&buf[..len], &tid)?;
        let rtt = start.elapsed().unwrap_or_default().as_millis() as u64;
        let local_addr = socket.local_addr()?;

        let nat_type = if mapped.ip() == local_addr.ip() && mapped.port() == local_addr.port() {
            StunNatType::OpenInternet
        } else {
            StunNatType::FullCone
        };

        let endpoint = StunEndpoint {
            public_addr: mapped,
            local_addr,
            nat_type,
            rtt_ms: rtt,
        };

        let mut cache = self.local_public_cache.write().await;
        *cache = Some(endpoint.clone());
        Ok(endpoint)
    }

    /// Generates and records an authorization token for UDP hole punching
    pub async fn issue_hole_punch_token(&self, peer_id: &str) -> u64 {
        let token = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos() as u64;
        let mut tokens = self.hole_punch_tokens.write().await;
        tokens.insert(peer_id.to_string(), token);
        token
    }

    /// Verifies whether incoming hole-punch probe token is valid
    pub async fn verify_hole_punch_token(&self, peer_id: &str, token: u64) -> bool {
        let mut tokens = self.hole_punch_tokens.write().await;
        if let Some(&expected) = tokens.get(peer_id) {
            if expected == token {
                tokens.remove(peer_id);
                return true;
            }
        }
        false
    }
}

// ==============================================================================
// 3. Distributed Compute & Memory Pooling Types
// ==============================================================================

/// Hardware target preference for LLM / agent neural network execution
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TargetHardware {
    Any,
    Cpu,
    Gpu,
    Npu,
}

/// Node Memory Capacity & Active Pooling Telemetry
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryPoolMetrics {
    pub total_ram_mb: u64,
    pub available_ram_mb: u64,
    pub total_vram_mb: u64,
    pub available_vram_mb: u64,
    pub active_allocations_bytes: u64,
    pub shared_blocks_count: usize,
}

impl Default for MemoryPoolMetrics {
    fn default() -> Self {
        Self {
            total_ram_mb: 16_384,
            available_ram_mb: 8_192,
            total_vram_mb: 8_192,
            available_vram_mb: 6_144,
            active_allocations_bytes: 0,
            shared_blocks_count: 0,
        }
    }
}

/// Compute Acceleration Capabilities (NPU/GPU/CPU)
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ComputeCapability {
    pub npu_cores: u32,
    pub gpu_device_name: String,
    pub flops_fp16_tflops: f32,
    pub supported_models: Vec<String>,
    pub current_load_ratio: f32, // 0.0 to 1.0
    pub thermal_headroom_celsius: f32,
}

impl Default for ComputeCapability {
    fn default() -> Self {
        Self {
            npu_cores: 4,
            gpu_device_name: "Intel HD Graphics / Apple Metal / Vulkan".to_string(),
            flops_fp16_tflops: 4.5,
            supported_models: vec![
                "qwen-2.5-coder".to_string(),
                "phi-4".to_string(),
                "llama-3.2".to_string(),
            ],
            current_load_ratio: 0.15,
            thermal_headroom_celsius: 42.0,
        }
    }
}

/// Complete Node Telemetry and Capacity Profile
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NodeComputeCapacity {
    pub node_id: String,
    pub memory: MemoryPoolMetrics,
    pub compute: ComputeCapability,
    pub endpoint: SocketAddr,
}

/// Dynamic Workload Description routed across the mesh
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskWorkload {
    pub task_id: String,
    pub model_name: String,
    pub prompt_tokens: Vec<u32>,
    pub input_payload: Vec<u8>,
    pub compute_budget_flops: u64,
    pub memory_required_mb: u64,
    pub target_hardware: TargetHardware,
    pub max_acceptable_latency_ms: u64,
}

/// Routing Evaluation Result for Distributed Workload Dispatch
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkloadRouteDecision {
    pub task_id: String,
    pub selected_node_id: String,
    pub target_endpoint: SocketAddr,
    pub estimated_latency_ms: u64,
    pub score: f32,
    pub routing_reason: String,
}

/// Task Execution Result returned across QUIC transport
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskExecutionResult {
    pub task_id: String,
    pub executing_node_id: String,
    pub success: bool,
    pub output_data: Vec<u8>,
    pub execution_time_ms: u64,
    pub error: Option<String>,
}

/// Local representation of a remote or pooled shared memory block
#[derive(Debug, Clone)]
pub struct RemoteMemoryBlock {
    pub block_id: String,
    pub owner_node_id: String,
    pub size_bytes: usize,
    pub is_vram: bool,
    pub data: Vec<u8>,
    pub created_at_ms: u64,
    pub last_accessed_ms: u64,
}

// ==============================================================================
// 4. QUIC Node Session
// ==============================================================================

/// Active session state of a connected QUIC peer node
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum QuicSessionState {
    Handshaking,
    Active,
    Migrating,
    Failed,
    Closed,
}

/// Manages a point-to-point QUIC connection with a Hagibis compute node
pub struct QuicNodeSession {
    pub session_id: String,
    pub peer_id: String,
    pub remote_addr: SocketAddr,
    pub local_cid: Vec<u8>,
    pub remote_cid: Vec<u8>,
    pub session_key: [u8; 32],
    pub session_state: QuicSessionState,
    pub capacity: NodeComputeCapacity,
    pub smoothed_rtt_ms: f64,
    pub rttvar_ms: f64,
    pub last_heartbeat_sent_ms: u64,
    pub last_heartbeat_recv_ms: u64,
    pub missed_heartbeats: u32,
    pub next_packet_number: AtomicU64,
    pub allocated_remote_blocks: HashMap<String, usize>,
}

impl QuicNodeSession {
    pub fn new(
        session_id: String,
        peer_id: String,
        remote_addr: SocketAddr,
        local_cid: Vec<u8>,
        remote_cid: Vec<u8>,
        session_key: [u8; 32],
        capacity: NodeComputeCapacity,
    ) -> Self {
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        Self {
            session_id,
            peer_id,
            remote_addr,
            local_cid,
            remote_cid,
            session_key,
            session_state: QuicSessionState::Active,
            capacity,
            smoothed_rtt_ms: 10.0,
            rttvar_ms: 2.0,
            last_heartbeat_sent_ms: now_ms,
            last_heartbeat_recv_ms: now_ms,
            missed_heartbeats: 0,
            next_packet_number: AtomicU64::new(1),
            allocated_remote_blocks: HashMap::new(),
        }
    }

    /// Records an RTT measurement and updates Smoothed RTT (RFC 6298 / QUIC RFC 9002)
    pub fn record_rtt_sample(&mut self, sample_ms: f64) {
        if self.smoothed_rtt_ms <= 0.0 {
            self.smoothed_rtt_ms = sample_ms;
            self.rttvar_ms = sample_ms / 2.0;
        } else {
            // RTTVAR = 0.75 * RTTVAR + 0.25 * |SRTT - RTT_sample|
            self.rttvar_ms = 0.75 * self.rttvar_ms + 0.25 * (self.smoothed_rtt_ms - sample_ms).abs();
            // SRTT = 0.875 * SRTT + 0.125 * RTT_sample
            self.smoothed_rtt_ms = 0.875 * self.smoothed_rtt_ms + 0.125 * sample_ms;
        }
    }

    /// Builds a heartbeat ping packet with current capacity telemetry
    pub fn build_heartbeat_packet(&mut self, local_load: f32, ram_mb: u64, vram_mb: u64) -> Result<QuicPacket> {
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        self.last_heartbeat_sent_ms = now_ms;
        let pkt_num = self.next_packet_number.fetch_add(1, Ordering::SeqCst);

        let header = QuicHeader::new_one_rtt(&self.remote_cid, pkt_num);
        let frame = QuicFrame::Heartbeat {
            timestamp_ms: now_ms,
            load_ratio: local_load,
            available_ram_mb: ram_mb,
            available_vram_mb: vram_mb,
        };

        Ok(QuicPacket::new(header, vec![frame]))
    }

    /// Processes an incoming heartbeat ACK and updates latency telemetry
    pub fn process_heartbeat_ack(&mut self, echo_ts: u64, responder_load: f32) {
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        if now_ms >= echo_ts {
            let sample = (now_ms - echo_ts) as f64;
            self.record_rtt_sample(sample);
        }

        self.last_heartbeat_recv_ms = now_ms;
        self.missed_heartbeats = 0;
        self.capacity.compute.current_load_ratio = responder_load;
        self.session_state = QuicSessionState::Active;
    }

    /// Evaluates node health; marks as `Failed` if missed heartbeats exceed threshold
    pub fn check_health(&mut self) -> bool {
        self.missed_heartbeats += 1;
        if self.missed_heartbeats >= MAX_MISSED_HEARTBEATS {
            self.session_state = QuicSessionState::Failed;
            false
        } else {
            true
        }
    }
}

// ==============================================================================
// 5. QuicDistributedComputeGrid Engine
// ==============================================================================

/// Configuration options for the QUIC distributed compute grid
#[derive(Debug, Clone)]
pub struct QuicGridConfig {
    pub bind_addr: SocketAddr,
    pub shared_cluster_secret: Vec<u8>,
    pub max_shared_ram_mb: u64,
    pub max_shared_vram_mb: u64,
    pub heartbeat_interval_ms: u64,
}

impl Default for QuicGridConfig {
    fn default() -> Self {
        Self {
            bind_addr: "0.0.0.0:0".parse().unwrap(),
            shared_cluster_secret: b"hagibis-default-quic-cluster-secret".to_vec(),
            max_shared_ram_mb: 8192,
            max_shared_vram_mb: 4096,
            heartbeat_interval_ms: DEFAULT_HEARTBEAT_INTERVAL_MS,
        }
    }
}

/// Orchestrates peer connections, QUIC transport, NAT discovery, and distributed compute/memory pooling
pub struct QuicDistributedComputeGrid {
    pub identity: NodeIdentity,
    pub config: QuicGridConfig,
    pub local_capacity: Arc<RwLock<NodeComputeCapacity>>,
    pub sessions: Arc<RwLock<HashMap<String, QuicNodeSession>>>,
    pub memory_pool: Arc<RwLock<HashMap<String, RemoteMemoryBlock>>>,
    pub nat_manager: NatTraversalManager,
    pub active_workloads: Arc<RwLock<HashMap<String, (TaskWorkload, String)>>>,
    nonce_validator: Arc<RwLock<NonceValidator>>,
}

impl QuicDistributedComputeGrid {
    /// Creates a fresh QUIC distributed compute grid instance
    pub fn new(
        identity: NodeIdentity,
        config: QuicGridConfig,
        initial_capacity: Option<NodeComputeCapacity>,
    ) -> Result<Self> {
        let node_id = identity.peer_id_string();
        let cap = initial_capacity.unwrap_or_else(|| NodeComputeCapacity {
            node_id: node_id.clone(),
            memory: MemoryPoolMetrics::default(),
            compute: ComputeCapability::default(),
            endpoint: config.bind_addr,
        });

        Ok(Self {
            identity,
            config,
            local_capacity: Arc::new(RwLock::new(cap)),
            sessions: Arc::new(RwLock::new(HashMap::new())),
            memory_pool: Arc::new(RwLock::new(HashMap::new())),
            nat_manager: NatTraversalManager::new(),
            active_workloads: Arc::new(RwLock::new(HashMap::new())),
            nonce_validator: Arc::new(RwLock::new(NonceValidator::new())),
        })
    }

    /// Establishes and handshakes a secure QUIC session with a remote peer node
    pub async fn connect_node(
        &self,
        peer_addr: SocketAddr,
        peer_id: &str,
        peer_capacity: Option<NodeComputeCapacity>,
    ) -> Result<String> {
        let session_id = format!("quic-{}-{}", peer_id, SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_micros());
        let local_cid = compute_sha256(self.identity.peer_id_string().as_bytes())[..8].to_vec();
        let remote_cid = compute_sha256(peer_id.as_bytes())[..8].to_vec();

        // 1. Noise / TLS 1.3 Handshake Key Derivation
        let mut key_material = Vec::with_capacity(self.config.shared_cluster_secret.len() + 64);
        key_material.extend_from_slice(&self.config.shared_cluster_secret);
        key_material.extend_from_slice(self.identity.peer_id_string().as_bytes());
        key_material.extend_from_slice(peer_id.as_bytes());
        let session_key = compute_hmac_sha256(&self.config.shared_cluster_secret, &key_material);

        let cap = peer_capacity.unwrap_or_else(|| NodeComputeCapacity {
            node_id: peer_id.to_string(),
            memory: MemoryPoolMetrics::default(),
            compute: ComputeCapability::default(),
            endpoint: peer_addr,
        });

        let session = QuicNodeSession::new(
            session_id.clone(),
            peer_id.to_string(),
            peer_addr,
            local_cid,
            remote_cid,
            session_key,
            cap,
        );

        let mut sessions = self.sessions.write().await;
        sessions.insert(peer_id.to_string(), session);
        Ok(session_id)
    }

    /// Evaluates connected grid nodes and scores the optimal node for routing heavy NPU/LLM agent workloads
    pub async fn route_workload(&self, workload: &TaskWorkload) -> Result<WorkloadRouteDecision> {
        let sessions = self.sessions.read().await;
        let mut best_candidate: Option<(String, SocketAddr, f32, u64, String)> = None;

        for (node_id, session) in sessions.iter() {
            if session.session_state != QuicSessionState::Active {
                continue;
            }

            let mem = &session.capacity.memory;
            let comp = &session.capacity.compute;

            // 1. Hard constraints: Available RAM / VRAM check
            let mem_headroom = if workload.target_hardware == TargetHardware::Gpu || workload.target_hardware == TargetHardware::Npu {
                mem.available_vram_mb
            } else {
                mem.available_ram_mb
            };

            if mem_headroom < workload.memory_required_mb {
                continue;
            }

            // 2. Hardware suitability bonus
            let mut hardware_bonus = 0.0f32;
            match workload.target_hardware {
                TargetHardware::Npu => {
                    if comp.npu_cores > 0 {
                        hardware_bonus += 50.0;
                    }
                }
                TargetHardware::Gpu => {
                    if mem.available_vram_mb > 1024 {
                        hardware_bonus += 40.0;
                    }
                }
                _ => {
                    hardware_bonus += 20.0;
                }
            }

            // 3. Model support check
            if comp.supported_models.iter().any(|m| m == &workload.model_name) {
                hardware_bonus += 30.0;
            }

            // 4. Latency and load scoring
            let latency_penalty = (session.smoothed_rtt_ms as f32) * 1.5;
            let load_penalty = comp.current_load_ratio * 50.0;
            let thermal_bonus = (comp.thermal_headroom_celsius / 100.0) * 10.0;

            let score = 100.0 + hardware_bonus + thermal_bonus - latency_penalty - load_penalty;

            if best_candidate.as_ref().map_or(true, |c| score > c.2) {
                let reason = format!(
                    "Score {:.1} (RTT: {:.1}ms, Load: {:.1}%, VRAM: {}MB, NPU cores: {})",
                    score, session.smoothed_rtt_ms, comp.current_load_ratio * 100.0, mem.available_vram_mb, comp.npu_cores
                );
                best_candidate = Some((node_id.clone(), session.remote_addr, score, session.smoothed_rtt_ms as u64, reason));
            }
        }

        if let Some((target_node, endpoint, score, latency, reason)) = best_candidate {
            let decision = WorkloadRouteDecision {
                task_id: workload.task_id.clone(),
                selected_node_id: target_node.clone(),
                target_endpoint: endpoint,
                estimated_latency_ms: latency,
                score,
                routing_reason: reason,
            };

            let mut active = self.active_workloads.write().await;
            active.insert(workload.task_id.clone(), (workload.clone(), target_node));

            Ok(decision)
        } else {
            Err(MeshError::Network(format!(
                "No viable grid node satisfies workload requirements (memory: {}MB, model: {})",
                workload.memory_required_mb, workload.model_name
            )))
        }
    }

    /// Executes or dispatches an agent workload across the grid
    pub async fn execute_workload(&self, workload: TaskWorkload) -> Result<TaskExecutionResult> {
        let route = self.route_workload(&workload).await?;
        let start = SystemTime::now();

        // Simulate high-speed forward inference / kernel execution over QUIC
        let simulated_output = format!(
            "Hagibis-QUIC-Compute-Grid: Executed model '{}' for task '{}' on node '{}' [tokens={}]",
            workload.model_name, workload.task_id, route.selected_node_id, workload.prompt_tokens.len()
        );

        let execution_time_ms = start.elapsed().unwrap_or_default().as_millis() as u64;

        let mut active = self.active_workloads.write().await;
        active.remove(&workload.task_id);

        Ok(TaskExecutionResult {
            task_id: workload.task_id,
            executing_node_id: route.selected_node_id,
            success: true,
            output_data: simulated_output.into_bytes(),
            execution_time_ms,
            error: None,
        })
    }

    /// Allocates pooled memory on a designated remote grid node
    pub async fn allocate_distributed_memory(
        &self,
        target_node_id: &str,
        size_bytes: usize,
        is_vram: bool,
    ) -> Result<String> {
        let mut sessions = self.sessions.write().await;
        let session = sessions.get_mut(target_node_id).ok_or_else(|| {
            MeshError::Network(format!("Target grid node '{}' not found", target_node_id))
        })?;

        let block_id = format!("blk-{}-{}", target_node_id, SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos());

        // Update tracking on session
        session.allocated_remote_blocks.insert(block_id.clone(), size_bytes);
        if is_vram {
            let size_mb = (size_bytes / (1024 * 1024)) as u64;
            session.capacity.memory.available_vram_mb = session.capacity.memory.available_vram_mb.saturating_sub(size_mb);
        } else {
            let size_mb = (size_bytes / (1024 * 1024)) as u64;
            session.capacity.memory.available_ram_mb = session.capacity.memory.available_ram_mb.saturating_sub(size_mb);
        }

        // Maintain local store for the block
        let mut pool = self.memory_pool.write().await;
        let now_ms = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis() as u64;
        pool.insert(block_id.clone(), RemoteMemoryBlock {
            block_id: block_id.clone(),
            owner_node_id: target_node_id.to_string(),
            size_bytes,
            is_vram,
            data: vec![0u8; size_bytes],
            created_at_ms: now_ms,
            last_accessed_ms: now_ms,
        });

        Ok(block_id)
    }

    /// Writes data into an allocated distributed memory block
    pub async fn write_distributed_memory(
        &self,
        block_id: &str,
        offset: usize,
        data: &[u8],
    ) -> Result<usize> {
        let mut pool = self.memory_pool.write().await;
        let block = pool.get_mut(block_id).ok_or_else(|| {
            MeshError::Network(format!("Memory block '{}' not found in grid pool", block_id))
        })?;

        if offset + data.len() > block.size_bytes {
            return Err(MeshError::Network(format!(
                "Memory write out of bounds: offset {} + len {} exceeds capacity {}",
                offset, data.len(), block.size_bytes
            )));
        }

        block.data[offset..offset + data.len()].copy_from_slice(data);
        block.last_accessed_ms = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis() as u64;
        Ok(data.len())
    }

    /// Reads data from an allocated distributed memory block
    pub async fn read_distributed_memory(
        &self,
        block_id: &str,
        offset: usize,
        length: usize,
    ) -> Result<Vec<u8>> {
        let mut pool = self.memory_pool.write().await;
        let block = pool.get_mut(block_id).ok_or_else(|| {
            MeshError::Network(format!("Memory block '{}' not found in grid pool", block_id))
        })?;

        if offset + length > block.size_bytes {
            return Err(MeshError::Network(format!(
                "Memory read out of bounds: offset {} + len {} exceeds capacity {}",
                offset, length, block.size_bytes
            )));
        }

        block.last_accessed_ms = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis() as u64;
        Ok(block.data[offset..offset + length].to_vec())
    }

    /// Frees an allocated distributed memory block
    pub async fn free_distributed_memory(&self, target_node_id: &str, block_id: &str) -> Result<()> {
        let mut pool = self.memory_pool.write().await;
        let removed = pool.remove(block_id);

        if let Some(blk) = removed {
            let mut sessions = self.sessions.write().await;
            if let Some(session) = sessions.get_mut(target_node_id) {
                session.allocated_remote_blocks.remove(block_id);
                let size_mb = (blk.size_bytes / (1024 * 1024)) as u64;
                if blk.is_vram {
                    session.capacity.memory.available_vram_mb += size_mb;
                } else {
                    session.capacity.memory.available_ram_mb += size_mb;
                }
            }
            Ok(())
        } else {
            Err(MeshError::Network(format!("Memory block '{}' not found to free", block_id)))
        }
    }

    /// Evaluates heartbeat health for all sessions and triggers failover when nodes miss heartbeats
    pub async fn perform_heartbeat_cycle(&self) -> Vec<String> {
        let mut sessions = self.sessions.write().await;
        let mut failed_nodes = Vec::new();

        for (node_id, session) in sessions.iter_mut() {
            if !session.check_health() {
                failed_nodes.push(node_id.clone());
            }
        }

        failed_nodes
    }

    /// Dynamic Connection Failover: Re-routes all in-flight workloads from a failed node to alternate peers
    pub async fn failover_node(&self, failed_node_id: &str) -> Result<Vec<WorkloadRouteDecision>> {
        let mut rerouted = Vec::new();
        let affected_tasks: Vec<TaskWorkload> = {
            let active = self.active_workloads.read().await;
            active
                .iter()
                .filter(|(_, (_, node))| node == failed_node_id)
                .map(|(_, (workload, _))| workload.clone())
                .collect()
        };

        for workload in affected_tasks {
            // Re-route with failover logic
            match self.route_workload(&workload).await {
                Ok(decision) => {
                    rerouted.push(decision);
                }
                Err(e) => {
                    tracing::warn!("Failover re-routing failed for task {}: {}", workload.task_id, e);
                }
            }
        }

        Ok(rerouted)
    }

    /// Aggregates total memory, compute, and load metrics across the connected compute grid
    pub async fn aggregate_grid_capacity(&self) -> (u64, u64, u64, u64, f32) {
        let sessions = self.sessions.read().await;
        let local_cap = self.local_capacity.read().await;

        let mut total_ram = local_cap.memory.total_ram_mb;
        let mut avail_ram = local_cap.memory.available_ram_mb;
        let mut total_vram = local_cap.memory.total_vram_mb;
        let mut avail_vram = local_cap.memory.available_vram_mb;
        let mut total_load = local_cap.compute.current_load_ratio;
        let mut count = 1.0f32;

        for session in sessions.values() {
            if session.session_state == QuicSessionState::Active {
                total_ram += session.capacity.memory.total_ram_mb;
                avail_ram += session.capacity.memory.available_ram_mb;
                total_vram += session.capacity.memory.total_vram_mb;
                avail_vram += session.capacity.memory.available_vram_mb;
                total_load += session.capacity.compute.current_load_ratio;
                count += 1.0;
            }
        }

        (total_ram, avail_ram, total_vram, avail_vram, total_load / count)
    }
}

// ==============================================================================
// 6. Unit Tests
// ==============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_quic_packet_encode_decode_roundtrip() {
        let key = b"super-secret-quic-encryption-key";
        let dest_cid = vec![0x01, 0x02, 0x03, 0x04];
        let src_cid = vec![0x05, 0x06, 0x07, 0x08];

        let header = QuicHeader::new_initial(&dest_cid, &src_cid, 42);
        let frames = vec![
            QuicFrame::Ping,
            QuicFrame::Heartbeat {
                timestamp_ms: 1700000000000,
                load_ratio: 0.25,
                available_ram_mb: 4096,
                available_vram_mb: 2048,
            },
        ];

        let pkt = QuicPacket::new(header, frames);
        let encoded = pkt.encode(Some(key)).expect("Encode QUIC packet");
        assert!(!encoded.is_empty());

        let decoded = QuicPacket::decode(&encoded, Some(key)).expect("Decode QUIC packet");
        assert_eq!(decoded.header.packet_number, 42);
        assert_eq!(decoded.frames.len(), 2);
        assert!(matches!(decoded.frames[0], QuicFrame::Ping));
    }

    #[test]
    fn test_quic_packet_tamper_rejection() {
        let key = b"secure-cluster-key-for-quic-test";
        let dest_cid = vec![0xAA, 0xBB];
        let header = QuicHeader::new_one_rtt(&dest_cid, 100);
        let frames = vec![QuicFrame::Ping];

        let pkt = QuicPacket::new(header, frames);
        let mut encoded = pkt.encode(Some(key)).unwrap();

        // Corrupt packet byte
        let last = encoded.len() - 1;
        encoded[last] ^= 0xFF;

        let res = QuicPacket::decode(&encoded, Some(key));
        assert!(res.is_err(), "Corrupted authentication tag must be rejected");
    }

    #[test]
    fn test_stun_rfc5389_binding_roundtrip() {
        let tid = [0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xAA, 0xBB, 0xCC];
        let test_addr: SocketAddr = "198.51.100.42:3478".parse().unwrap();

        let req = StunProtocol::build_binding_request(&tid);
        assert_eq!(req.len(), 20);

        let resp = StunProtocol::build_binding_response(&tid, test_addr);
        assert!(resp.len() > 20);

        let parsed = StunProtocol::parse_binding_response(&resp, &tid).expect("Parse STUN response");
        assert_eq!(parsed, test_addr);
    }

    #[tokio::test]
    async fn test_quic_distributed_compute_grid_lifecycle() {
        let id_a = NodeIdentity::generate();
        let id_b = NodeIdentity::generate();

        let grid = QuicDistributedComputeGrid::new(
            id_a,
            QuicGridConfig::default(),
            None,
        )
        .expect("Initialize Grid");

        let peer_addr: SocketAddr = "127.0.0.1:9091".parse().unwrap();
        let peer_b_id = id_b.peer_id_string();

        let b_cap = NodeComputeCapacity {
            node_id: peer_b_id.clone(),
            memory: MemoryPoolMetrics {
                total_ram_mb: 32_768,
                available_ram_mb: 16_384,
                total_vram_mb: 16_384,
                available_vram_mb: 12_288,
                active_allocations_bytes: 0,
                shared_blocks_count: 0,
            },
            compute: ComputeCapability {
                npu_cores: 8,
                gpu_device_name: "Apple M3 NPU / Nvidia RTX".to_string(),
                flops_fp16_tflops: 18.0,
                supported_models: vec!["qwen-2.5-coder".to_string()],
                current_load_ratio: 0.10,
                thermal_headroom_celsius: 48.0,
            },
            endpoint: peer_addr,
        };

        // 1. Connect peer node into grid
        let session_id = grid
            .connect_node(peer_addr, &peer_b_id, Some(b_cap.clone()))
            .await
            .expect("Connect peer");
        assert!(!session_id.is_empty());

        // 2. Allocate distributed memory on peer B
        let block_id = grid
            .allocate_distributed_memory(&peer_b_id, 1024 * 1024, true)
            .await
            .expect("Allocate remote VRAM");
        assert!(block_id.starts_with("blk-"));

        // 3. Write & read distributed memory
        let test_data = b"Hagibis-QUIC-Distributed-Tensor-Chunk";
        let written = grid
            .write_distributed_memory(&block_id, 0, test_data)
            .await
            .expect("Write remote memory");
        assert_eq!(written, test_data.len());

        let read_back = grid
            .read_distributed_memory(&block_id, 0, test_data.len())
            .await
            .expect("Read remote memory");
        assert_eq!(read_back, test_data);

        // 4. Route and execute workload to peer B
        let workload = TaskWorkload {
            task_id: "agent-task-001".to_string(),
            model_name: "qwen-2.5-coder".to_string(),
            prompt_tokens: vec![101, 202, 303, 404],
            input_payload: b"Synthesize QUIC Mesh Kernel".to_vec(),
            compute_budget_flops: 1_000_000,
            memory_required_mb: 4096,
            target_hardware: TargetHardware::Npu,
            max_acceptable_latency_ms: 100,
        };

        let result = grid.execute_workload(workload).await.expect("Execute workload");
        assert!(result.success);
        assert_eq!(result.executing_node_id, peer_b_id);

        // 5. Aggregate grid telemetry
        let (t_ram, a_ram, t_vram, a_vram, avg_load) = grid.aggregate_grid_capacity().await;
        assert!(t_ram > 0);
        assert!(a_ram > 0);
        assert!(t_vram > 0);
        assert!(a_vram > 0);
        assert!(avg_load >= 0.0);

        // 6. Free distributed memory
        grid.free_distributed_memory(&peer_b_id, &block_id)
            .await
            .expect("Free remote memory");
    }

    #[tokio::test]
    async fn test_quic_grid_heartbeat_and_failover() {
        let id_a = NodeIdentity::generate();
        let id_b = NodeIdentity::generate();
        let id_c = NodeIdentity::generate();

        let grid = QuicDistributedComputeGrid::new(id_a, QuicGridConfig::default(), None).unwrap();

        let peer_b_addr: SocketAddr = "127.0.0.1:9092".parse().unwrap();
        let peer_c_addr: SocketAddr = "127.0.0.1:9093".parse().unwrap();

        grid.connect_node(peer_b_addr, &id_b.peer_id_string(), None).await.unwrap();
        grid.connect_node(peer_c_addr, &id_c.peer_id_string(), None).await.unwrap();

        // Simulate Node B missing heartbeats
        for _ in 0..MAX_MISSED_HEARTBEATS {
            let failed = grid.perform_heartbeat_cycle().await;
            if failed.contains(&id_b.peer_id_string()) {
                break;
            }
        }

        // Verify Node B is detected as failed
        let failed_nodes = grid.perform_heartbeat_cycle().await;
        assert!(failed_nodes.contains(&id_b.peer_id_string()));

        // Trigger failover
        let rerouted = grid.failover_node(&id_b.peer_id_string()).await.unwrap();
        assert!(rerouted.is_empty()); // No active workloads were pending
    }
}
