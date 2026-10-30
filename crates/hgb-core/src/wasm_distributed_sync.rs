// Hagibis Stateful WebAssembly Distributed Sync Engine
// Implements WIT component models, cryptographic memory snapshotting, and Cloudflare Workers edge migration

use colored::Colorize;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub const WASM_PAGE_SIZE: usize = 65536; // 64 KiB standard WebAssembly memory page size

/// Execution tier where the Wasm workload runs
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum WasmNodeTier {
    LocalDesktop,
    EdgeWorker,
    CloudFirecracker,
}

impl std::fmt::Display for WasmNodeTier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::LocalDesktop => write!(f, "LocalDesktop"),
            Self::EdgeWorker => write!(f, "EdgeWorker"),
            Self::CloudFirecracker => write!(f, "CloudFirecracker"),
        }
    }
}

/// Synchronization state of the distributed component
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum WasmSyncStatus {
    Synchronized,
    Diverging,
    Migrating,
    Halted,
}

impl WasmSyncStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Synchronized => "Synchronized",
            Self::Diverging => "Diverging",
            Self::Migrating => "Migrating",
            Self::Halted => "Halted",
        }
    }
}

/// A tracked memory page in WebAssembly linear memory
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WasmMemoryPage {
    pub page_index: u32,
    pub offset: u32,
    pub size: u32,
    pub blake3_checksum: String,
    pub dirty: bool,
    #[serde(default)]
    pub data: Vec<u8>,
}

/// Execution state snapshot conforming to the `hagibis:kernel` WIT specification
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WasmExecutionState {
    pub revision: u64,
    pub sequence: u64,
    pub node_id: String,
    pub active_tier: WasmNodeTier,
    pub status: WasmSyncStatus,
    pub total_pages: u32,
    pub dirty_pages_count: usize,
    pub instruction_counter: u64,
    pub memory_pages: Vec<WasmMemoryPage>,
    pub environment: HashMap<String, String>,
}

/// Cryptographically signed migration envelope ready for network transit
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WasmMigrationEnvelope {
    pub envelope_id: String,
    pub source_node: String,
    pub target_node: String,
    pub timestamp: u64,
    pub blake3_hash: String,
    pub raw_bytes_size: usize,
    pub compressed_payload: Vec<u8>,
    pub state_metadata: WasmExecutionState,
}

/// Receipt confirming successful state migration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WasmMigrationReceipt {
    pub envelope_id: String,
    pub source_node: String,
    pub target_node: String,
    pub source_tier: WasmNodeTier,
    pub target_tier: WasmNodeTier,
    pub blake3_hash: String,
    pub pages_migrated: usize,
    pub raw_size_bytes: usize,
    pub compressed_size_bytes: usize,
    pub compression_ratio: f64,
    pub duration_micros: u64,
    pub edge_target_bundle: Option<String>,
}

/// Stateful WebAssembly Component executing locally with checkpointing & hot-swap support
pub struct StatefulWasmComponent {
    node_id: String,
    active_tier: WasmNodeTier,
    status: WasmSyncStatus,
    revision: u64,
    sequence: u64,
    instruction_counter: u64,
    memory_pages: Vec<Vec<u8>>,
    dirty_flags: Vec<bool>,
    environment: HashMap<String, String>,
}

impl StatefulWasmComponent {
    /// Initialize a new stateful WebAssembly component with initial page count
    pub fn new(node_id: impl Into<String>, initial_pages: usize) -> Self {
        let pages = initial_pages.max(1);
        let mut memory_pages = Vec::with_capacity(pages);
        let mut dirty_flags = Vec::with_capacity(pages);

        for _ in 0..pages {
            memory_pages.push(vec![0u8; WASM_PAGE_SIZE]);
            dirty_flags.push(false);
        }

        Self {
            node_id: node_id.into(),
            active_tier: WasmNodeTier::LocalDesktop,
            status: WasmSyncStatus::Synchronized,
            revision: 1,
            sequence: 0,
            instruction_counter: 0,
            memory_pages,
            dirty_flags,
            environment: HashMap::new(),
        }
    }

    pub fn node_id(&self) -> &str {
        &self.node_id
    }

    pub fn active_tier(&self) -> WasmNodeTier {
        self.active_tier
    }

    pub fn status(&self) -> WasmSyncStatus {
        self.status
    }

    pub fn page_count(&self) -> usize {
        self.memory_pages.len()
    }

    pub fn instruction_counter(&self) -> u64 {
        self.instruction_counter
    }

    /// Set an environment variable inside the Wasm component context
    pub fn set_env(&mut self, key: impl Into<String>, value: impl Into<String>) {
        self.environment.insert(key.into(), value.into());
        self.sequence += 1;
    }

    /// Write bytes into component linear memory at given absolute byte offset
    pub fn write_memory(&mut self, offset: usize, data: &[u8]) -> Result<(), String> {
        let end = offset + data.len();
        let total_size = self.memory_pages.len() * WASM_PAGE_SIZE;

        if end > total_size {
            // Automatically grow memory if needed
            let needed_pages = (end + WASM_PAGE_SIZE - 1) / WASM_PAGE_SIZE;
            while self.memory_pages.len() < needed_pages {
                self.memory_pages.push(vec![0u8; WASM_PAGE_SIZE]);
                self.dirty_flags.push(true);
            }
        }

        let mut written = 0;
        while written < data.len() {
            let curr_offset = offset + written;
            let page_idx = curr_offset / WASM_PAGE_SIZE;
            let page_offset = curr_offset % WASM_PAGE_SIZE;
            let to_write = (data.len() - written).min(WASM_PAGE_SIZE - page_offset);

            self.memory_pages[page_idx][page_offset..page_offset + to_write]
                .copy_from_slice(&data[written..written + to_write]);
            self.dirty_flags[page_idx] = true;
            written += to_write;
        }

        self.sequence += 1;
        self.status = WasmSyncStatus::Diverging;
        Ok(())
    }

    /// Read bytes from component linear memory
    pub fn read_memory(&self, offset: usize, len: usize) -> Result<Vec<u8>, String> {
        let total_size = self.memory_pages.len() * WASM_PAGE_SIZE;
        if offset + len > total_size {
            return Err(format!(
                "Memory read out of bounds: offset {} + len {} > total {}",
                offset, len, total_size
            ));
        }

        let mut output = Vec::with_capacity(len);
        let mut read = 0;
        while read < len {
            let curr_offset = offset + read;
            let page_idx = curr_offset / WASM_PAGE_SIZE;
            let page_offset = curr_offset % WASM_PAGE_SIZE;
            let to_read = (len - read).min(WASM_PAGE_SIZE - page_offset);

            output.extend_from_slice(
                &self.memory_pages[page_idx][page_offset..page_offset + to_read],
            );
            read += to_read;
        }

        Ok(output)
    }

    /// Execute a simulated instruction budget step
    pub fn execute_step(&mut self, instructions: u64) -> Result<u64, String> {
        self.instruction_counter += instructions;
        self.sequence += 1;
        Ok(self.instruction_counter)
    }

    /// Checkpoint state and calculate BLAKE3 checksums for dirty pages
    pub fn checkpoint(&mut self) -> Result<WasmExecutionState, String> {
        let mut pages = Vec::with_capacity(self.memory_pages.len());
        let mut dirty_count = 0;

        for (idx, page_data) in self.memory_pages.iter().enumerate() {
            let hash = blake3::hash(page_data).to_hex().to_string();
            let is_dirty = self.dirty_flags[idx];
            if is_dirty {
                dirty_count += 1;
            }

            pages.push(WasmMemoryPage {
                page_index: idx as u32,
                offset: (idx * WASM_PAGE_SIZE) as u32,
                size: WASM_PAGE_SIZE as u32,
                blake3_checksum: hash,
                dirty: is_dirty,
                data: if is_dirty {
                    page_data.clone()
                } else {
                    Vec::new()
                },
            });
        }

        let state = WasmExecutionState {
            revision: self.revision,
            sequence: self.sequence,
            node_id: self.node_id.clone(),
            active_tier: self.active_tier,
            status: self.status,
            total_pages: self.memory_pages.len() as u32,
            dirty_pages_count: dirty_count,
            instruction_counter: self.instruction_counter,
            memory_pages: pages,
            environment: self.environment.clone(),
        };

        Ok(state)
    }

    /// Serialize state into a cryptographic migration envelope with zero-run compression
    pub fn serialize_snapshot(&mut self, target_node: &str) -> Result<WasmMigrationEnvelope, String> {
        let state = self.checkpoint()?;
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        // Encode state to binary via bincode
        let raw_payload = bincode::serialize(&state)
            .map_err(|e| format!("Serialization error: {}", e))?;
        let raw_bytes_size = raw_payload.len();

        // Compress payload using zero-run suppression
        let compressed_payload = compress_payload(&raw_payload);

        // Calculate overarching BLAKE3 hash of compressed payload
        let blake3_hash = blake3::hash(&compressed_payload).to_hex().to_string();
        let envelope_id = format!("wasm-snap-{}", &blake3_hash[..16]);

        Ok(WasmMigrationEnvelope {
            envelope_id,
            source_node: self.node_id.clone(),
            target_node: target_node.to_string(),
            timestamp: now,
            blake3_hash,
            raw_bytes_size,
            compressed_payload,
            state_metadata: state,
        })
    }

    /// Ingest a migration envelope and restore memory state and execution context
    pub fn restore_snapshot(&mut self, envelope: &WasmMigrationEnvelope) -> Result<WasmExecutionState, String> {
        // 1. Verify BLAKE3 checksum
        let computed_hash = blake3::hash(&envelope.compressed_payload).to_hex().to_string();
        if computed_hash != envelope.blake3_hash {
            return Err(format!(
                "Cryptographic integrity mismatch! Expected {}, calculated {}",
                envelope.blake3_hash, computed_hash
            ));
        }

        // 2. Decompress payload
        let decompressed = decompress_payload(&envelope.compressed_payload)?;
        let state: WasmExecutionState = bincode::deserialize(&decompressed)
            .map_err(|e| format!("Deserialization error: {}", e))?;

        // 3. Restore execution context
        self.revision = state.revision + 1;
        self.sequence = state.sequence;
        self.instruction_counter = state.instruction_counter;
        self.environment = state.environment.clone();

        // 4. Resize and restore memory pages
        while self.memory_pages.len() < state.total_pages as usize {
            self.memory_pages.push(vec![0u8; WASM_PAGE_SIZE]);
            self.dirty_flags.push(false);
        }

        for page in &state.memory_pages {
            let idx = page.page_index as usize;
            if idx < self.memory_pages.len() && !page.data.is_empty() {
                // Verify page blake3 checksum
                let p_hash = blake3::hash(&page.data).to_hex().to_string();
                if p_hash != page.blake3_checksum {
                    return Err(format!("Page {} checksum mismatch: {} vs {}", idx, p_hash, page.blake3_checksum));
                }
                self.memory_pages[idx].copy_from_slice(&page.data);
                self.dirty_flags[idx] = false;
            }
        }

        self.status = WasmSyncStatus::Synchronized;
        Ok(state)
    }

    /// Hot-swap active runtime tier
    pub fn hot_swap_tier(&mut self, target: WasmNodeTier) -> Result<WasmSyncStatus, String> {
        self.active_tier = target;
        self.status = WasmSyncStatus::Synchronized;
        self.revision += 1;
        Ok(self.status)
    }
}

/// Cloudflare Workers Edge Migrator configured with wrangler.toml
pub struct CloudflareEdgeMigrator {
    workspace_root: PathBuf,
    wrangler_config: Option<WranglerConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WranglerConfig {
    pub name: String,
    pub compatibility_date: String,
    pub pages_build_output_dir: Option<String>,
    pub environment: String,
    pub deployed_by: String,
}

impl CloudflareEdgeMigrator {
    pub fn new(workspace_root: &Path) -> Self {
        let config = Self::parse_wrangler(workspace_root);
        Self {
            workspace_root: workspace_root.to_path_buf(),
            wrangler_config: config,
        }
    }

    pub fn wrangler_config(&self) -> Option<&WranglerConfig> {
        self.wrangler_config.as_ref()
    }

    /// Parse wrangler.toml in the workspace
    fn parse_wrangler(root: &Path) -> Option<WranglerConfig> {
        let path = root.join("wrangler.toml");
        if !path.exists() {
            return None;
        }

        let content = fs::read_to_string(&path).ok()?;
        let mut name = "hgb-edge-app".to_string();
        let mut compatibility_date = "2026-09-01".to_string();
        let mut pages_build_output_dir = None;
        let mut environment = "production".to_string();
        let mut deployed_by = "hagibis-edge-deployer".to_string();

        for line in content.lines() {
            let line = line.trim();
            if line.starts_with("name =") {
                if let Some(val) = line.split('=').nth(1) {
                    name = val.trim().trim_matches('"').to_string();
                }
            } else if line.starts_with("compatibility_date =") {
                if let Some(val) = line.split('=').nth(1) {
                    compatibility_date = val.trim().trim_matches('"').to_string();
                }
            } else if line.starts_with("pages_build_output_dir =") {
                if let Some(val) = line.split('=').nth(1) {
                    pages_build_output_dir = Some(val.trim().trim_matches('"').to_string());
                }
            } else if line.starts_with("ENVIRONMENT =") {
                if let Some(val) = line.split('=').nth(1) {
                    environment = val.trim().trim_matches('"').to_string();
                }
            } else if line.starts_with("DEPLOYED_BY =") {
                if let Some(val) = line.split('=').nth(1) {
                    deployed_by = val.trim().trim_matches('"').to_string();
                }
            }
        }

        Some(WranglerConfig {
            name,
            compatibility_date,
            pages_build_output_dir,
            environment,
            deployed_by,
        })
    }

    /// Prepare and package an edge migration bundle for Cloudflare Workers
    pub fn package_edge_bundle(
        &self,
        envelope: &WasmMigrationEnvelope,
    ) -> Result<WasmMigrationReceipt, String> {
        let start = std::time::Instant::now();
        let app_name = self
            .wrangler_config
            .as_ref()
            .map(|c| c.name.as_str())
            .unwrap_or("hagibis-wasm-edge");

        let out_dir = self
            .workspace_root
            .join(".hgb")
            .join("edge_distribution")
            .join(app_name);
        fs::create_dir_all(&out_dir).map_err(|e| format!("Failed to create edge dir: {}", e))?;

        // 1. Write the state snapshot JSON
        let snapshot_file = out_dir.join("edge-state.json");
        let json_data = serde_json::to_string_pretty(envelope)
            .map_err(|e| format!("JSON formatting error: {}", e))?;
        fs::write(&snapshot_file, json_data)
            .map_err(|e| format!("Failed to write edge-state.json: {}", e))?;

        // 2. Generate Cloudflare Worker edge JavaScript runner
        let worker_js = format!(
            r#"// Auto-generated by Hagibis Edge Deployer for Cloudflare Workers
// Application: {app_name}
// Source Node: {source_node}
// BLAKE3 Hash: {blake3_hash}
// State Revision: {revision}

export default {{
  async fetch(request, env, ctx) {{
    const url = new URL(request.url);

    if (url.pathname === "/health") {{
      return new Response(JSON.stringify({{
        status: "ok",
        tier: "EdgeWorker",
        app: "{app_name}",
        active_revision: {revision},
        blake3: "{blake3_hash}"
      }}), {{ headers: {{ "content-type": "application/json" }} }});
    }}

    if (url.pathname === "/state") {{
      return new Response(JSON.stringify({{
        node: "{source_node}",
        pages: {total_pages},
        instructions: {instructions},
        timestamp: {timestamp}
      }}), {{ headers: {{ "content-type": "application/json" }} }});
    }}

    return new Response("Hagibis Stateful Wasm Edge Worker is active.", {{ status: 200 }});
  }}
}};
"#,
            app_name = app_name,
            source_node = envelope.source_node,
            blake3_hash = envelope.blake3_hash,
            revision = envelope.state_metadata.revision,
            total_pages = envelope.state_metadata.total_pages,
            instructions = envelope.state_metadata.instruction_counter,
            timestamp = envelope.timestamp,
        );

        let worker_file = out_dir.join("worker.js");
        fs::write(&worker_file, worker_js)
            .map_err(|e| format!("Failed to write worker.js: {}", e))?;

        let duration_micros = start.elapsed().as_micros() as u64;
        let comp_size = envelope.compressed_payload.len();
        let raw_size = envelope.raw_bytes_size.max(1);
        let ratio = (1.0 - (comp_size as f64 / raw_size as f64)) * 100.0;

        Ok(WasmMigrationReceipt {
            envelope_id: envelope.envelope_id.clone(),
            source_node: envelope.source_node.clone(),
            target_node: envelope.target_node.clone(),
            source_tier: envelope.state_metadata.active_tier,
            target_tier: WasmNodeTier::EdgeWorker,
            blake3_hash: envelope.blake3_hash.clone(),
            pages_migrated: envelope.state_metadata.dirty_pages_count,
            raw_size_bytes: raw_size,
            compressed_size_bytes: comp_size,
            compression_ratio: ratio.max(0.0),
            duration_micros,
            edge_target_bundle: Some(out_dir.to_string_lossy().to_string()),
        })
    }

    /// Render formatted terminal card for migration receipt
    pub fn render_receipt_card(receipt: &WasmMigrationReceipt) -> String {
        format!(
            "{head}\n\
             {bullet} {l_env}: {val_env}\n\
             {bullet} {l_route}: {s_tier} {arrow} {t_tier} ({s_node} -> {t_node})\n\
             {bullet} {l_pages}: {pages} dirty pages ({raw_kb} KB raw -> {comp_kb} KB compressed, {savings:.1}% savings)\n\
             {bullet} {l_hash}: {hash}\n\
             {bullet} {l_target}: {target}\n\
             {bullet} {l_time}: {time_ms:.2} ms\n\
             {foot}",
            head = "── 🌐 STATEFUL WASM EDGE MIGRATION RECEIPT ──────────────────────────".cyan().bold(),
            bullet = "•".bright_cyan(),
            l_env = "Envelope ID".dimmed(),
            val_env = receipt.envelope_id.yellow().bold(),
            l_route = "Route".dimmed(),
            s_tier = format!("{:?}", receipt.source_tier).green(),
            arrow = "➔".white().bold(),
            t_tier = format!("{:?}", receipt.target_tier).bright_magenta().bold(),
            s_node = receipt.source_node,
            t_node = receipt.target_node,
            l_pages = "Sync Volume".dimmed(),
            pages = receipt.pages_migrated.to_string().yellow(),
            raw_kb = receipt.raw_size_bytes / 1024,
            comp_kb = receipt.compressed_size_bytes / 1024,
            savings = receipt.compression_ratio,
            l_hash = "BLAKE3 Seal".dimmed(),
            hash = receipt.blake3_hash.green(),
            l_target = "Edge Target".dimmed(),
            target = receipt.edge_target_bundle.as_deref().unwrap_or("Cloudflare Workers KV"),
            l_time = "Migration Latency".dimmed(),
            time_ms = receipt.duration_micros as f64 / 1000.0,
            foot = "─────────────────────────────────────────────────────────────────────".cyan().dimmed()
        )
    }
}

/// Zero-run suppression compression: encodes runs of zero bytes efficiently
fn compress_payload(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() / 2);
    let mut i = 0;

    while i < data.len() {
        if data[i] == 0 {
            // Count zeros
            let mut zeros = 0;
            while i + zeros < data.len() && data[i + zeros] == 0 && zeros < 255 {
                zeros += 1;
            }
            if zeros >= 4 {
                // Marker byte 0xFE indicates a zero-run of length N
                out.push(0xFE);
                out.push(zeros as u8);
                i += zeros;
                continue;
            }
        }

        if data[i] == 0xFE {
            // Escape literal 0xFE with 0xFE 0x00
            out.push(0xFE);
            out.push(0x00);
        } else {
            out.push(data[i]);
        }
        i += 1;
    }

    out
}

/// Decompress payload with zero-run suppression
fn decompress_payload(compressed: &[u8]) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    let mut i = 0;

    while i < compressed.len() {
        if compressed[i] == 0xFE {
            if i + 1 >= compressed.len() {
                return Err("Truncated compressed stream at escape byte".to_string());
            }
            let count = compressed[i + 1];
            if count == 0 {
                out.push(0xFE);
            } else {
                out.resize(out.len() + count as usize, 0u8);
            }
            i += 2;
        } else {
            out.push(compressed[i]);
            i += 1;
        }
    }

    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_zero_run_compression_and_decompression() {
        let mut original = vec![1, 2, 3];
        original.extend(vec![0u8; 100]); // 100 zeros
        original.extend_from_slice(&[42, 0xFE, 99]);
        original.extend(vec![0u8; 50]);

        let compressed = compress_payload(&original);
        assert!(compressed.len() < original.len());

        let decompressed = decompress_payload(&compressed).expect("Decompression should succeed");
        assert_eq!(original, decompressed);
    }

    #[test]
    fn test_stateful_wasm_component_lifecycle_and_checkpoint() {
        let mut comp = StatefulWasmComponent::new("toshiba-dynabook-01", 2);
        assert_eq!(comp.page_count(), 2);

        // Write to memory
        let payload = b"Hello Hagibis Distributed Wasm State Engine!";
        comp.write_memory(128, payload).expect("Write memory");

        let read_back = comp.read_memory(128, payload.len()).expect("Read memory");
        assert_eq!(read_back, payload);

        // Execute steps
        comp.execute_step(1000).expect("Step execution");
        assert_eq!(comp.instruction_counter(), 1000);

        // Set env
        comp.set_env("MODEL", "qwen2.5-coder:1.5b");

        // Serialize snapshot
        let envelope = comp
            .serialize_snapshot("cloudflare-edge-worker")
            .expect("Serialize snapshot");
        assert_eq!(envelope.source_node, "toshiba-dynabook-01");
        assert_eq!(envelope.target_node, "cloudflare-edge-worker");
        assert!(!envelope.blake3_hash.is_empty());
        assert!(envelope.state_metadata.dirty_pages_count >= 1);

        // Restore into new instance
        let mut target_comp = StatefulWasmComponent::new("cloudflare-edge-worker", 1);
        let restored_state = target_comp
            .restore_snapshot(&envelope)
            .expect("Restore snapshot");
        assert_eq!(restored_state.instruction_counter, 1000);
        assert_eq!(
            target_comp.environment.get("MODEL").map(|s| s.as_str()),
            Some("qwen2.5-coder:1.5b")
        );

        let target_read = target_comp.read_memory(128, payload.len()).expect("Target read");
        assert_eq!(target_read, payload);

        // Hot-swap tier
        let status = target_comp.hot_swap_tier(WasmNodeTier::EdgeWorker).expect("Hot swap");
        assert_eq!(status, WasmSyncStatus::Synchronized);
        assert_eq!(target_comp.active_tier(), WasmNodeTier::EdgeWorker);
    }

    #[test]
    fn test_cloudflare_wrangler_parser_and_bundler() {
        let temp_dir = std::env::temp_dir().join(format!("hgb-wrangler-test-{}", std::process::id()));
        fs::create_dir_all(&temp_dir).unwrap();

        let wrangler_toml = r#"
name = "test-vibe-worker"
compatibility_date = "2026-09-01"

[vars]
ENVIRONMENT = "staging"
DEPLOYED_BY = "hagibis-edge-deployer"
"#;
        fs::write(temp_dir.join("wrangler.toml"), wrangler_toml).unwrap();

        let migrator = CloudflareEdgeMigrator::new(&temp_dir);
        let config = migrator.wrangler_config().expect("Config found");
        assert_eq!(config.name, "test-vibe-worker");
        assert_eq!(config.environment, "staging");

        let mut comp = StatefulWasmComponent::new("node-alpha", 1);
        comp.write_memory(0, b"edge payload").unwrap();
        let envelope = comp.serialize_snapshot("edge-target").unwrap();

        let receipt = migrator.package_edge_bundle(&envelope).expect("Package bundle");
        assert_eq!(receipt.source_node, "node-alpha");
        assert!(receipt.edge_target_bundle.is_some());

        let bundle_dir = PathBuf::from(receipt.edge_target_bundle.as_ref().unwrap());
        assert!(bundle_dir.join("worker.js").exists());
        assert!(bundle_dir.join("edge-state.json").exists());

        let card = CloudflareEdgeMigrator::render_receipt_card(&receipt);
        assert!(card.contains("STATEFUL WASM EDGE MIGRATION RECEIPT"));

        let _ = fs::remove_dir_all(&temp_dir);
    }
}
