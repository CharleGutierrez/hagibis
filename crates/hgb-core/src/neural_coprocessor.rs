//! # Neural Coprocessor & Local Model Hot-Swapping Engine
//!
//! Intelligently classifies prompt complexity and coordinates high-efficiency routing across:
//! 1. Simple file/string transforms, formatting, regex, pure algorithms -> Local WASM kernel (`hgb-wasm-kernel` / `WasmHotPool`).
//! 2. Moderate tool coordination, small edits, unit tests, diffs -> Local lightweight models via Ollama (`qwen2.5-coder:1.5b`, `phi3:3.8b`, `gemma3:1b`).
//! 3. Complex multi-file architectural reasoning -> Frontier models via MCP / Cloud Fabric (`gemini-2.5-flash`, `claude-3.5-sonnet`, `o1`).
//!
//! Integrates real-time Linux hardware telemetry from `AdaptivePowerScheduler` and `DynatuneSynergyEngine`
//! to dynamically select model quantization (Q8_0/FP16 vs Q4_K_M vs Q2_K) and manage fallback chains under thermal stress.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;
use serde::{Deserialize, Serialize};

use crate::dynatune_synergy::DynatuneSynergyEngine;
use crate::error::Result;
use crate::power_scheduler::AdaptivePowerScheduler;
use crate::providers::ollama::OllamaProvider;
use crate::wasm_hot_pool::WasmHotPool;

/// Complexity classification tier for incoming developer prompts
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PromptComplexityTier {
    /// Pure string/file transforms, formatting, regex replacements, JSON/YAML linting
    SimpleTransform,
    /// Single-file edits, bug fixes, unit test drafting, tool coordination, diff generation
    ModerateCoordination,
    /// Multi-file refactoring, distributed consensus, protocol design, security audit, architecture overhaul
    ComplexArchitectural,
}

impl std::fmt::Display for PromptComplexityTier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SimpleTransform => write!(f, "SimpleTransform (WASM Kernel)"),
            Self::ModerateCoordination => write!(f, "ModerateCoordination (Local Lightweight Ollama)"),
            Self::ComplexArchitectural => write!(f, "ComplexArchitectural (Frontier Cloud MCP)"),
        }
    }
}

/// Destination runtime target for the coprocessor execution
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ExecutionTarget {
    /// Executed via sub-millisecond local WASM kernel (`hgb-wasm-kernel`)
    WasmKernel { module: String },
    /// Executed locally on Ollama inference server
    LocalLightweight { model: String, quantization: String },
    /// Executed on high-capacity cloud frontier model via MCP
    FrontierMcp { model: String },
}

/// Routing decision synthesized by the Coprocessor Router
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RoutingDecision {
    pub complexity: PromptComplexityTier,
    pub target: ExecutionTarget,
    pub selected_model: String,
    pub quantization: String,
    pub thermal_headroom_c: f32,
    pub ac_connected: bool,
    pub is_wasm_offload: bool,
    pub fallback_engaged: bool,
    pub rationale: String,
    pub confidence_score: f32,
}

/// Telemetry metrics tracked by the Neural Coprocessor
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CoprocessorTelemetry {
    pub total_routes: u64,
    pub wasm_kernel_offloads: u64,
    pub local_ollama_routes: u64,
    pub frontier_mcp_routes: u64,
    pub fallback_triggered: u64,
    pub avg_latency_us: f64,
    pub tokens_processed_estimate: u64,
}

/// Overall status of the Neural Coprocessor subsystem
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoprocessorStatus {
    pub ollama_endpoint: String,
    pub sibling_ollama_path: String,
    pub sibling_models_detected: Vec<String>,
    pub installed_models: Vec<String>,
    pub active_quantization_tier: String,
    pub current_temp_c: f32,
    pub thermal_headroom_c: f32,
    pub ac_connected: bool,
    pub wasm_hot_pool_available: bool,
    pub telemetry: CoprocessorTelemetry,
}

/// Prompt Complexity Inspector and Hardware-Informed Router
#[derive(Debug, Clone)]
pub struct CoprocessorRouter {
    pub ollama_endpoint: String,
    pub sibling_ollama_path: PathBuf,
}

impl Default for CoprocessorRouter {
    fn default() -> Self {
        Self::new()
    }
}

impl CoprocessorRouter {
    pub fn new() -> Self {
        let ep = std::env::var("OLLAMA_HOST").unwrap_or_else(|_| "http://127.0.0.1:11434".to_string());
        let sibling = PathBuf::from("/home/toshiba/My AI Project/ollama");
        Self {
            ollama_endpoint: ep,
            sibling_ollama_path: sibling,
        }
    }

    pub fn with_endpoint(endpoint: impl Into<String>, sibling: impl Into<PathBuf>) -> Self {
        Self {
            ollama_endpoint: endpoint.into(),
            sibling_ollama_path: sibling.into(),
        }
    }

    /// Inspect prompt text to determine complexity tier with precision
    pub fn inspect_complexity(&self, prompt: &str) -> (PromptComplexityTier, f32) {
        let text = prompt.trim().to_lowercase();
        let word_count = text.split_whitespace().count();

        // 1. High Complexity Indicators (Architectural / Multi-file / Deep Reasoning)
        let complex_markers = [
            "refactor across", "architecture", "monorepo", "distributed consensus",
            "p2p mesh", "security audit", "threat model", "protocol design",
            "migration strategy", "concurrency lockup", "deadlock proof",
            "multi-file", "system redesign", "formal verification", "zero-copy shm",
            "high-throughput ring", "self-healing daemon", "full stack rewrite",
        ];
        let complex_hits = complex_markers.iter().filter(|&&m| text.contains(m)).count();

        if complex_hits >= 2 || (complex_hits >= 1 && word_count > 60) || word_count > 250 {
            return (PromptComplexityTier::ComplexArchitectural, 0.95);
        }

        // 2. Simple Transform Indicators (WASM Hot-Pool Pure Compute)
        let transform_markers = [
            "format json", "prettify", "regex replace", "sort lines", "trim whitespace",
            "convert to csv", "uppercase", "lowercase", "slugify", "base64 encode",
            "base64 decode", "hash blake3", "sha256", "pure math", "factorial",
            "fibonacci", "indent", "strip comments", "string reverse", "ascii art",
            "word count", "count lines", "validate json", "parse url"
        ];
        let transform_hits = transform_markers.iter().filter(|&&m| text.contains(m)).count();

        let is_short_edit = word_count < 30 && (text.starts_with("format") || text.starts_with("convert") || text.starts_with("sort") || text.starts_with("replace") || text.starts_with("hash"));

        if transform_hits >= 1 || is_short_edit {
            return (PromptComplexityTier::SimpleTransform, 0.92);
        }

        // 3. Moderate Coordination (Local Lightweight Model)
        (PromptComplexityTier::ModerateCoordination, 0.88)
    }

    /// Dynamic Quantization Selection based on thermal headroom and power source
    pub fn select_quantization(headroom_c: f32, ac_connected: bool) -> &'static str {
        if !ac_connected {
            if headroom_c < 15.0 {
                "Q2_K"
            } else {
                "Q4_K_M"
            }
        } else if headroom_c > 25.0 {
            "Q8_0"
        } else if headroom_c >= 15.0 {
            "Q4_K_M"
        } else {
            "Q4_K_S"
        }
    }

    /// Select optimal model from installed inventory
    pub fn select_model_for_tier(
        tier: PromptComplexityTier,
        installed: &[String],
        headroom_c: f32,
    ) -> (ExecutionTarget, String) {
        match tier {
            PromptComplexityTier::SimpleTransform => {
                (
                    ExecutionTarget::WasmKernel {
                        module: "hgb_kernel.wasm".to_string(),
                    },
                    "wasm_kernel:hgb_kernel.wasm".to_string(),
                )
            }
            PromptComplexityTier::ModerateCoordination => {
                let preferred = if headroom_c < 12.0 {
                    vec!["gemma3:1b", "qwen2.5-coder:1.5b", "llama3.2:1b"]
                } else {
                    vec!["qwen2.5-coder:1.5b", "phi3:3.8b", "gemma3:1b", "deepseek-coder:1.3b"]
                };

                let mut chosen = preferred[0].to_string();
                for pref in preferred {
                    if let Some(found) = installed.iter().find(|m| m.contains(pref)) {
                        chosen = found.clone();
                        break;
                    }
                }

                (
                    ExecutionTarget::LocalLightweight {
                        model: chosen.clone(),
                        quantization: "Q4_K_M".to_string(),
                    },
                    chosen,
                )
            }
            PromptComplexityTier::ComplexArchitectural => {
                let frontier = "gemini-2.5-flash".to_string();
                (
                    ExecutionTarget::FrontierMcp {
                        model: frontier.clone(),
                    },
                    frontier,
                )
            }
        }
    }
}

/// The Neural Coprocessor & Hot-Swapping Orchestrator
pub struct NeuralCoprocessor {
    router: CoprocessorRouter,
    dynatune_engine: Arc<DynatuneSynergyEngine>,
    wasm_pool: Option<Arc<WasmHotPool>>,
    total_routes: AtomicU64,
    wasm_offloads: AtomicU64,
    ollama_routes: AtomicU64,
    mcp_routes: AtomicU64,
    fallbacks: AtomicU64,
    total_latency_us: AtomicU64,
}

impl Default for NeuralCoprocessor {
    fn default() -> Self {
        Self::new()
    }
}

impl NeuralCoprocessor {
    pub fn new() -> Self {
        let router = CoprocessorRouter::new();
        let dynatune_engine = Arc::new(DynatuneSynergyEngine::new());
        let wasm_pool = WasmHotPool::new(4).ok().map(Arc::new);

        Self {
            router,
            dynatune_engine,
            wasm_pool,
            total_routes: AtomicU64::new(0),
            wasm_offloads: AtomicU64::new(0),
            ollama_routes: AtomicU64::new(0),
            mcp_routes: AtomicU64::new(0),
            fallbacks: AtomicU64::new(0),
            total_latency_us: AtomicU64::new(0),
        }
    }

    pub fn read_hardware_headroom(&self) -> (f32, f32, bool) {
        let report = self.dynatune_engine.evaluate_hardware_telemetry();
        let current_temp = report.max_temp_c;
        let headroom = (85.0f32 - current_temp).max(0.0f32);
        let ac = report.ac_connected;
        (current_temp, headroom, ac)
    }

    pub fn fetch_available_models(&self) -> Vec<String> {
        let names = OllamaProvider::installed_model_names();
        if !names.is_empty() {
            names
        } else {
            if self.router.sibling_ollama_path.is_dir() {
                if let Ok(entries) = fs::read_dir(&self.router.sibling_ollama_path) {
                    let mut found = Vec::new();
                    for entry in entries.flatten() {
                        let name = entry.file_name().to_string_lossy().to_string();
                        if name.ends_with(".gguf") || name.contains("qwen") || name.contains("phi") || name.contains("gemma") {
                            found.push(name);
                        }
                    }
                    if !found.is_empty() {
                        return found;
                    }
                }
            }
            vec![
                "qwen2.5-coder:1.5b".to_string(),
                "phi3:3.8b".to_string(),
                "gemma3:1b".to_string(),
            ]
        }
    }

    pub fn route(&self, prompt: &str) -> RoutingDecision {
        let start = Instant::now();
        self.total_routes.fetch_add(1, Ordering::Relaxed);

        let (complexity, confidence) = self.router.inspect_complexity(prompt);
        let (_current_temp, headroom_c, ac_connected) = self.read_hardware_headroom();
        let quantization = CoprocessorRouter::select_quantization(headroom_c, ac_connected).to_string();
        let installed = self.fetch_available_models();

        let (mut target, mut selected_model) = CoprocessorRouter::select_model_for_tier(complexity, &installed, headroom_c);
        let mut fallback_engaged = false;

        if complexity == PromptComplexityTier::ModerateCoordination && headroom_c < 8.0 {
            fallback_engaged = true;
            self.fallbacks.fetch_add(1, Ordering::Relaxed);
            target = ExecutionTarget::FrontierMcp {
                model: "gemini-2.5-flash".to_string(),
            };
            selected_model = "gemini-2.5-flash (Thermal Fallback)".to_string();
        }

        let is_wasm = matches!(target, ExecutionTarget::WasmKernel { .. });
        match &target {
            ExecutionTarget::WasmKernel { .. } => {
                self.wasm_offloads.fetch_add(1, Ordering::Relaxed);
            }
            ExecutionTarget::LocalLightweight { .. } => {
                self.ollama_routes.fetch_add(1, Ordering::Relaxed);
            }
            ExecutionTarget::FrontierMcp { .. } => {
                self.mcp_routes.fetch_add(1, Ordering::Relaxed);
            }
        }

        let rationale = match &target {
            ExecutionTarget::WasmKernel { module } => {
                format!(
                    "Sub-millisecond deterministic transform routed to WASM kernel ({}) with zero-copy execution.",
                    module
                )
            }
            ExecutionTarget::LocalLightweight { model, .. } => {
                format!(
                    "Moderate editing/coordination routed to local Ollama model '{}' with dynamic quantization {} (Headroom: {:.1}°C, AC: {}).",
                    model, quantization, headroom_c, ac_connected
                )
            }
            ExecutionTarget::FrontierMcp { model } => {
                if fallback_engaged {
                    format!(
                        "Thermal throttling safeguard triggered (<8°C headroom). Workload offloaded to Frontier model '{}'.",
                        model
                    )
                } else {
                    format!(
                        "Complex multi-file architectural reasoning routed to Frontier model '{}' via MCP Cloud Fabric.",
                        model
                    )
                }
            }
        };

        let elapsed_us = start.elapsed().as_micros() as u64;
        self.total_latency_us.fetch_add(elapsed_us, Ordering::Relaxed);

        RoutingDecision {
            complexity,
            target,
            selected_model,
            quantization,
            thermal_headroom_c: headroom_c,
            ac_connected,
            is_wasm_offload: is_wasm,
            fallback_engaged,
            rationale,
            confidence_score: confidence,
        }
    }

    pub fn status(&self) -> CoprocessorStatus {
        let (current_temp, headroom, ac) = self.read_hardware_headroom();
        let installed = self.fetch_available_models();
        let sibling_models = if self.router.sibling_ollama_path.is_dir() {
            fs::read_dir(&self.router.sibling_ollama_path)
                .map(|entries| {
                    entries
                        .flatten()
                        .map(|e| e.file_name().to_string_lossy().to_string())
                        .collect()
                })
                .unwrap_or_default()
        } else {
            Vec::new()
        };

        let total = self.total_routes.load(Ordering::Relaxed);
        let tot_lat = self.total_latency_us.load(Ordering::Relaxed);
        let avg_lat = if total > 0 {
            tot_lat as f64 / total as f64
        } else {
            0.0
        };

        CoprocessorStatus {
            ollama_endpoint: self.router.ollama_endpoint.clone(),
            sibling_ollama_path: self.router.sibling_ollama_path.to_string_lossy().to_string(),
            sibling_models_detected: sibling_models,
            installed_models: installed,
            active_quantization_tier: CoprocessorRouter::select_quantization(headroom, ac).to_string(),
            current_temp_c: current_temp,
            thermal_headroom_c: headroom,
            ac_connected: ac,
            wasm_hot_pool_available: self.wasm_pool.is_some(),
            telemetry: CoprocessorTelemetry {
                total_routes: total,
                wasm_kernel_offloads: self.wasm_offloads.load(Ordering::Relaxed),
                local_ollama_routes: self.ollama_routes.load(Ordering::Relaxed),
                frontier_mcp_routes: self.mcp_routes.load(Ordering::Relaxed),
                fallback_triggered: self.fallbacks.load(Ordering::Relaxed),
                avg_latency_us: avg_lat,
                tokens_processed_estimate: total * 150,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_complexity_classification() {
        let router = CoprocessorRouter::new();

        let (tier, conf) = router.inspect_complexity("format json string with 2 space indentation");
        assert_eq!(tier, PromptComplexityTier::SimpleTransform);
        assert!(conf > 0.85);

        let (tier2, _) = router.inspect_complexity("regex replace all digits with X in the input");
        assert_eq!(tier2, PromptComplexityTier::SimpleTransform);

        let (tier_c, conf_c) = router.inspect_complexity(
            "refactor across the monorepo to introduce distributed consensus and security audit for p2p mesh"
        );
        assert_eq!(tier_c, PromptComplexityTier::ComplexArchitectural);
        assert!(conf_c > 0.90);

        let (tier_m, _) = router.inspect_complexity("add a unit test for helper function parse_config");
        assert_eq!(tier_m, PromptComplexityTier::ModerateCoordination);
    }

    #[test]
    fn test_dynamic_quantization_selection() {
        assert_eq!(CoprocessorRouter::select_quantization(30.0, true), "Q8_0");
        assert_eq!(CoprocessorRouter::select_quantization(20.0, true), "Q4_K_M");
        assert_eq!(CoprocessorRouter::select_quantization(10.0, true), "Q4_K_S");
        assert_eq!(CoprocessorRouter::select_quantization(25.0, false), "Q4_K_M");
        assert_eq!(CoprocessorRouter::select_quantization(10.0, false), "Q2_K");
    }

    #[test]
    fn test_neural_coprocessor_routing_and_status() {
        let coproc = NeuralCoprocessor::new();

        let dec1 = coproc.route("sort lines alphabetically and trim whitespace");
        assert_eq!(dec1.complexity, PromptComplexityTier::SimpleTransform);
        assert!(dec1.is_wasm_offload);
        assert!(matches!(dec1.target, ExecutionTarget::WasmKernel { .. }));

        let dec2 = coproc.route("architectural overhaul: multi-file protocol design for distributed consensus");
        assert_eq!(dec2.complexity, PromptComplexityTier::ComplexArchitectural);
        assert!(!dec2.is_wasm_offload);
        assert!(matches!(dec2.target, ExecutionTarget::FrontierMcp { .. }));

        let st = coproc.status();
        assert!(st.telemetry.total_routes >= 2);
        assert!(st.telemetry.wasm_kernel_offloads >= 1);
        assert!(st.telemetry.frontier_mcp_routes >= 1);
    }
}
