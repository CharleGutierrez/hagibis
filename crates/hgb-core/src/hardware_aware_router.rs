//! # Superpower 1: Native Ollama Integration & Hardware-Aware Routing
//!
//! Connects directly to local Ollama engine (`http://127.0.0.1:11434`) and sibling `/home/toshiba/My AI Project/ollama`.
//! Evaluates task complexity (`ComplexityTier`), reads real hardware state via `AdaptivePowerScheduler`,
//! Linux `/sys/class/thermal`, `/sys/class/power_supply`, and dynamically routes:
//! - Cool & AC mains (>25°C headroom): Heavy reasoning to high-parameter models (`phi3:3.8b`, `hermes-3`).
//! - Battery / Warm (<15°C headroom): Scales down to lightweight models (`qwen2.5-coder:1.5b`, `llama3.2-abliterate:1b`, `gemma3:1b`).
//! - Thermal throttling / Critical (<8°C headroom): Offloads deterministic compute to `hgb-wasm-kernel` hot-pool (`WasmHotPool`).

use std::fmt;
use std::fs;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use serde::{Deserialize, Serialize};

use crate::error::{HgbError, Result};
use crate::power_scheduler::AdaptivePowerScheduler;
use crate::providers::ollama::OllamaProvider;
use crate::wasm_hot_pool::WasmHotPool;

/// Complexity tier of incoming AI tasks
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ComplexityTier {
    /// Deep multi-file refactoring, formal architecture, logic proofs
    HeavyReasoning,
    /// Standard code generation, function implementation, unit test writing
    StandardCode,
    /// Fast docstrings, linting, comment cleanup, syntax checks
    FastLint,
    /// Purely deterministic tasks: AST walks, regex transforms, formatting
    DeterministicCompute,
}

impl fmt::Display for ComplexityTier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::HeavyReasoning => write!(f, "HEAVY_REASONING"),
            Self::StandardCode => write!(f, "STANDARD_CODE"),
            Self::FastLint => write!(f, "FAST_LINT"),
            Self::DeterministicCompute => write!(f, "DETERMINISTIC_COMPUTE"),
        }
    }
}

impl FromStr for ComplexityTier {
    type Err = HgbError;

    fn from_str(s: &str) -> Result<Self> {
        let clean = s.trim().to_lowercase();
        if clean.contains("heavy") || clean.contains("reason") || clean.contains("arch") {
            Ok(Self::HeavyReasoning)
        } else if clean.contains("lint") || clean.contains("fast") || clean.contains("doc") {
            Ok(Self::FastLint)
        } else if clean.contains("determ") || clean.contains("wasm") || clean.contains("compute") {
            Ok(Self::DeterministicCompute)
        } else {
            Ok(Self::StandardCode)
        }
    }
}

/// Routing decision output from the hardware-aware router
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HardwareRoutingDecision {
    pub selected_model: String,
    pub complexity: ComplexityTier,
    pub thermal_headroom_c: f32,
    pub ac_connected: bool,
    pub is_wasm_offload: bool,
    pub reasoning: String,
    pub installed_models_count: usize,
    pub endpoint: String,
}

/// Global status telemetry of the HardwareAwareRouter
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HardwareRouterStatus {
    pub current_temp_c: f32,
    pub thermal_headroom_c: f32,
    pub ac_connected: bool,
    pub installed_models: Vec<String>,
    pub ollama_endpoint: String,
    pub sibling_ollama_path: String,
    pub sibling_ollama_exists: bool,
    pub recommended_heavy_model: String,
    pub recommended_light_model: String,
    pub total_routed_tasks: u64,
}

/// Hardware-Aware Local Inference & WASM Router
pub struct HardwareAwareRouter {
    ollama_endpoint: String,
    sibling_ollama_path: PathBuf,
    power_scheduler: AdaptivePowerScheduler,
    wasm_hot_pool: Option<Arc<WasmHotPool>>,
    total_routed: AtomicU64,
}

impl Default for HardwareAwareRouter {
    fn default() -> Self {
        Self::new()
    }
}

impl HardwareAwareRouter {
    /// Initialize the router with defaults
    pub fn new() -> Self {
        let ep = std::env::var("OLLAMA_HOST").unwrap_or_else(|_| "http://127.0.0.1:11434".to_string());
        let sibling = PathBuf::from("/home/toshiba/My AI Project/ollama");
        Self {
            ollama_endpoint: ep,
            sibling_ollama_path: sibling,
            power_scheduler: AdaptivePowerScheduler::new(),
            wasm_hot_pool: WasmHotPool::new(4).ok().map(Arc::new),
            total_routed: AtomicU64::new(0),
        }
    }

    /// Construct router with explicit configuration
    pub fn with_config(
        endpoint: impl Into<String>,
        sibling_path: impl Into<PathBuf>,
        wasm_pool: Option<Arc<WasmHotPool>>,
    ) -> Self {
        Self {
            ollama_endpoint: endpoint.into(),
            sibling_ollama_path: sibling_path.into(),
            power_scheduler: AdaptivePowerScheduler::new(),
            wasm_hot_pool: wasm_pool,
            total_routed: AtomicU64::new(0),
        }
    }

    /// Inspect real hardware thermal state and power supply (AC vs Battery)
    pub fn evaluate_hardware_headroom(&self) -> (f32, f32, bool) {
        let profile = self.power_scheduler.evaluate_live_profile();
        let current_temp = profile.current_temp_c;
        let headroom = (85.0 - current_temp).max(0.0);
        let ac = profile.ac_connected;
        (current_temp, headroom, ac)
    }

    /// Fetch installed models directly from Ollama /api/tags
    pub fn fetch_installed_models(&self) -> Vec<String> {
        let names = OllamaProvider::installed_model_names();
        if !names.is_empty() {
            names
        } else {
            // Sibling directory inspection fallback
            if self.sibling_ollama_path.is_dir() {
                if let Ok(entries) = fs::read_dir(&self.sibling_ollama_path) {
                    let mut found = Vec::new();
                    for entry in entries.flatten() {
                        let name = entry.file_name().to_string_lossy().to_string();
                        if name.ends_with(".gguf") || name.contains("qwen") || name.contains("phi") {
                            found.push(name);
                        }
                    }
                    if !found.is_empty() {
                        return found;
                    }
                }
            }
            // Baseline known models
            vec![
                "qwen2.5-coder:1.5b".to_string(),
                "phi3:3.8b".to_string(),
                "gemma3:1b".to_string(),
            ]
        }
    }

    /// Resolve optimal heavy model from installed inventory
    pub fn resolve_heavy_model(installed: &[String]) -> String {
        // Priority: hermes-3, phi3:3.8b, qwen3.5, qwen2.5-coder:7b
        for pattern in &["hermes-3", "phi3:3.8b", "phi3", "qwen3.5", "qwen2.5-coder:7b", "llama3:8b"] {
            if let Some(m) = installed.iter().find(|m| m.contains(pattern)) {
                return m.clone();
            }
        }
        installed.first().cloned().unwrap_or_else(|| "phi3:3.8b".to_string())
    }

    /// Resolve optimal lightweight model from installed inventory
    pub fn resolve_lightweight_model(installed: &[String]) -> String {
        // Priority: qwen2.5-coder:1.5b, llama3.2-abliterate:1b, gemma3:1b, 0.5b
        for pattern in &["qwen2.5-coder:1.5b", "llama3.2-abliterate", "gemma3", "1.5b", "1b", "2b", "0.5b"] {
            if let Some(m) = installed.iter().find(|m| m.contains(pattern)) {
                return m.clone();
            }
        }
        installed.last().cloned().unwrap_or_else(|| "qwen2.5-coder:1.5b".to_string())
    }

    /// Dynamically route task according to complexity and real-time hardware headroom
    pub fn route(&self, complexity: ComplexityTier) -> HardwareRoutingDecision {
        self.total_routed.fetch_add(1, Ordering::Relaxed);
        let (temp, headroom, ac) = self.evaluate_hardware_headroom();
        let installed = self.fetch_installed_models();

        let heavy_model = Self::resolve_heavy_model(&installed);
        let light_model = Self::resolve_lightweight_model(&installed);

        // Case 1: Deterministic compute OR thermal throttling / critical (<8°C headroom)
        if complexity == ComplexityTier::DeterministicCompute || headroom < 8.0 {
            let reason = if headroom < 8.0 {
                format!(
                    "THERMAL THROTTLE MITIGATION ({:.1}°C, headroom {:.1}°C < 8°C): Offloaded to WasmHotPool to conserve CPU cycles.",
                    temp, headroom
                )
            } else {
                "DETERMINISTIC COMPUTE: Routing to WasmHotPool zero-copy sandboxed execution.".to_string()
            };

            return HardwareRoutingDecision {
                selected_model: "hgb-wasm-kernel:hot_pool".to_string(),
                complexity,
                thermal_headroom_c: headroom,
                ac_connected: ac,
                is_wasm_offload: true,
                reasoning: reason,
                installed_models_count: installed.len(),
                endpoint: self.ollama_endpoint.clone(),
            };
        }

        // Case 2: Cool & AC Connected (>25°C headroom)
        if headroom > 25.0 && ac {
            match complexity {
                ComplexityTier::HeavyReasoning => HardwareRoutingDecision {
                    selected_model: heavy_model.clone(),
                    complexity,
                    thermal_headroom_c: headroom,
                    ac_connected: true,
                    is_wasm_offload: false,
                    reasoning: format!(
                        "OPTIMAL HEADROOM ({:.1}°C headroom, AC Connected): Selected heavy parameter model '{}'.",
                        headroom, heavy_model
                    ),
                    installed_models_count: installed.len(),
                    endpoint: self.ollama_endpoint.clone(),
                },
                ComplexityTier::StandardCode => HardwareRoutingDecision {
                    selected_model: if installed.iter().any(|m| m.contains("coder")) {
                        installed.iter().find(|m| m.contains("coder")).cloned().unwrap_or(heavy_model)
                    } else {
                        heavy_model
                    },
                    complexity,
                    thermal_headroom_c: headroom,
                    ac_connected: true,
                    is_wasm_offload: false,
                    reasoning: format!("OPTIMAL HEADROOM: High-throughput code synthesis via '{}'.", light_model),
                    installed_models_count: installed.len(),
                    endpoint: self.ollama_endpoint.clone(),
                },
                ComplexityTier::FastLint => HardwareRoutingDecision {
                    selected_model: light_model.clone(),
                    complexity,
                    thermal_headroom_c: headroom,
                    ac_connected: true,
                    is_wasm_offload: false,
                    reasoning: format!("FAST LINT: Sub-second linting via lightweight model '{}'.", light_model),
                    installed_models_count: installed.len(),
                    endpoint: self.ollama_endpoint.clone(),
                },
                ComplexityTier::DeterministicCompute => unreachable!(),
            }
        } else {
            // Case 3: Battery power or warm system (<15°C headroom) -> Scale down to lightweight model
            let reason = if !ac {
                format!(
                    "BATTERY POWER PRESERVATION ({:.1}°C): Scaled down to lightweight model '{}' to reduce drain.",
                    temp, light_model
                )
            } else {
                format!(
                    "WARM HEADROOM CONSTRAINT ({:.1}°C headroom < 15°C): Scaled down to lightweight model '{}' to prevent fan noise.",
                    headroom, light_model
                )
            };

            HardwareRoutingDecision {
                selected_model: light_model,
                complexity,
                thermal_headroom_c: headroom,
                ac_connected: ac,
                is_wasm_offload: false,
                reasoning: reason,
                installed_models_count: installed.len(),
                endpoint: self.ollama_endpoint.clone(),
            }
        }
    }

    /// Read comprehensive status report of the router
    pub fn status(&self) -> HardwareRouterStatus {
        let (temp, headroom, ac) = self.evaluate_hardware_headroom();
        let installed = self.fetch_installed_models();
        let heavy = Self::resolve_heavy_model(&installed);
        let light = Self::resolve_lightweight_model(&installed);
        let sibling_exists = self.sibling_ollama_path.is_dir();

        HardwareRouterStatus {
            current_temp_c: temp,
            thermal_headroom_c: headroom,
            ac_connected: ac,
            installed_models: installed,
            ollama_endpoint: self.ollama_endpoint.clone(),
            sibling_ollama_path: self.sibling_ollama_path.to_string_lossy().to_string(),
            sibling_ollama_exists: sibling_exists,
            recommended_heavy_model: heavy,
            recommended_light_model: light,
            total_routed_tasks: self.total_routed.load(Ordering::Relaxed),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_complexity_tier_from_str() {
        assert_eq!(ComplexityTier::from_str("heavy").unwrap(), ComplexityTier::HeavyReasoning);
        assert_eq!(ComplexityTier::from_str("reasoning").unwrap(), ComplexityTier::HeavyReasoning);
        assert_eq!(ComplexityTier::from_str("lint").unwrap(), ComplexityTier::FastLint);
        assert_eq!(ComplexityTier::from_str("deterministic").unwrap(), ComplexityTier::DeterministicCompute);
        assert_eq!(ComplexityTier::from_str("code").unwrap(), ComplexityTier::StandardCode);
        assert_eq!(ComplexityTier::from_str("anything").unwrap(), ComplexityTier::StandardCode);
    }

    #[test]
    fn test_model_resolution() {
        let models = vec![
            "phi3:3.8b".to_string(),
            "qwen2.5-coder:1.5b".to_string(),
            "frankarenakc/hermes-3-uncensored:latest".to_string(),
            "gemma3:1b".to_string(),
        ];
        let heavy = HardwareAwareRouter::resolve_heavy_model(&models);
        assert!(heavy.contains("hermes-3") || heavy.contains("phi3"));

        let light = HardwareAwareRouter::resolve_lightweight_model(&models);
        assert!(light.contains("coder:1.5b") || light.contains("gemma3"));
    }

    #[test]
    fn test_hardware_router_decision_and_status() {
        let router = HardwareAwareRouter::new();
        let status = router.status();
        assert!(status.current_temp_c >= 0.0);
        assert!(!status.ollama_endpoint.is_empty());

        let decision = router.route(ComplexityTier::DeterministicCompute);
        assert!(decision.is_wasm_offload);
        assert_eq!(decision.selected_model, "hgb-wasm-kernel:hot_pool");

        let code_decision = router.route(ComplexityTier::StandardCode);
        assert!(!code_decision.selected_model.is_empty());
        assert!(!code_decision.reasoning.is_empty());
    }
}
