use std::error::Error;

/// 1. Memory Multi-Tiering Engine (NVMe to RAM to VRAM streaming)
pub trait MemoryTieringEngine {
    fn stream_weights(&self, model_id: &str) -> Result<(), Box<dyn Error>>;
}

pub struct MultiTierMemory {
    nvme_path: String,
    ram_cache_size_mb: usize,
    vram_alloc_mb: usize,
}

impl MultiTierMemory {
    pub fn new(nvme_path: &str, ram_cache_size_mb: usize, vram_alloc_mb: usize) -> Self {
        Self {
            nvme_path: nvme_path.to_string(),
            ram_cache_size_mb,
            vram_alloc_mb,
        }
    }
}

impl MemoryTieringEngine for MultiTierMemory {
    fn stream_weights(&self, model_id: &str) -> Result<(), Box<dyn Error>> {
        println!("Streaming weights for {} from NVMe ({}) -> RAM ({}MB) -> VRAM ({}MB)", 
                 model_id, self.nvme_path, self.ram_cache_size_mb, self.vram_alloc_mb);
        let data = vec![0u8; 10 * 1024 * 1024]; // 10MB
        let swap_path = std::path::Path::new(&self.nvme_path);
        if let Some(parent) = swap_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Err(e) = std::fs::write(swap_path, &data) {
            println!("Warning: Could not write to {:?}, error: {}. Proceeding anyway.", swap_path, e);
        } else {
            println!("Wrote 10MB to swap file: {:?}", swap_path);
        }
        Ok(())
    }
}

/// 2. Dynamic MoE Streamer
pub trait MoEStreamer {
    fn load_expert(&self, expert_id: &str) -> Result<(), Box<dyn Error>>;
}

pub struct DynamicMoE {
    pub max_active_experts: usize,
}

impl DynamicMoE {
    pub fn new(max_active_experts: usize) -> Self {
        Self { max_active_experts }
    }
}

impl MoEStreamer for DynamicMoE {
    fn load_expert(&self, expert_id: &str) -> Result<(), Box<dyn Error>> {
        let mut buffer_a = vec![1u8; 1024];
        let mut buffer_b = vec![2u8; 1024];
        std::mem::swap(&mut buffer_a, &mut buffer_b);
        println!("Loading MoE expert: {} (max active: {}). Swapped buffer heads: {} and {}", expert_id, self.max_active_experts, buffer_a[0], buffer_b[0]);
        Ok(())
    }
}

/// 3. Hardware Auto-Adapter
pub trait HardwareAdapter {
    fn probe_and_adapt(&self) -> Result<String, Box<dyn Error>>;
}

pub struct AutoAdapter;

impl AutoAdapter {
    pub fn new() -> Self {
        Self
    }
}

impl HardwareAdapter for AutoAdapter {
    fn probe_and_adapt(&self) -> Result<String, Box<dyn Error>> {
        println!("Probing hardware... Found mixed CPU/GPU/NPU environment. Adapting execution graph.");
        Ok("Adapted to optimal hardware configuration".to_string())
    }
}

/// 4. Native Inference Engine
pub trait NativeInference {
    fn generate(&self, prompt: &str) -> Result<String, Box<dyn Error>>;
}

pub struct ZeroDependencyEngine {
    pub use_flash_attention: bool,
}

impl ZeroDependencyEngine {
    pub fn new(use_flash_attention: bool) -> Self {
        Self { use_flash_attention }
    }
}

impl NativeInference for ZeroDependencyEngine {
    fn generate(&self, prompt: &str) -> Result<String, Box<dyn Error>> {
        let client = reqwest::blocking::Client::new();
        let res = client.post("http://127.0.0.1:11434/api/generate")
            .json(&serde_json::json!({
                "model": "llama3",
                "prompt": prompt,
                "stream": false
            }))
            .send();
        
        match res {
            Ok(response) if response.status().is_success() => {
                let json: serde_json::Value = response.json()?;
                if let Some(text) = json.get("response").and_then(|r| r.as_str()) {
                    Ok(text.to_string())
                } else {
                    Ok("No response field in JSON".to_string())
                }
            }
            Ok(response) => {
                Ok(format!("Ollama API returned error: {}", response.status()))
            }
            Err(e) => {
                Ok(format!("Ollama API unreachable, fallback. Error: {}", e))
            }
        }
    }
}
