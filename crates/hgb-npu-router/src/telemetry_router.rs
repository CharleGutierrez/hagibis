//! # Dynamic Telemetry-Aware Workload Offloader Router
//!
//! Intelligently schedules tensor batch embeddings, LanceDB vector queries,
//! and SLM inference across heterogeneous accelerators.

use std::sync::{Arc, RwLock};
use tracing::info;

use crate::accelerator_backend::{
    AcceleratorComputeBackend, OffloadDecision, OffloadReceipt, OffloadTarget,
    WorkloadRequest, WorkloadType,
};
use crate::error::{NpuRouterError, Result};
use crate::governor_bridge::GovernorBridge;
use crate::hardware_discovery::HardwareInventory;

#[derive(Debug, Clone)]
pub struct RouterConfig {
    pub min_cpu_headroom_c: f32,
    pub high_queue_threshold: usize,
    pub allow_gpu_offload: bool,
    pub allow_npu_offload: bool,
}

impl Default for RouterConfig {
    fn default() -> Self {
        Self {
            min_cpu_headroom_c: 25.0,
            high_queue_threshold: 4,
            allow_gpu_offload: true,
            allow_npu_offload: true,
        }
    }
}

pub struct NpuOffloadRouter {
    pub inventory: HardwareInventory,
    pub governor: GovernorBridge,
    pub backend: AcceleratorComputeBackend,
    pub config: RouterConfig,
    pub receipts: Arc<RwLock<Vec<OffloadReceipt>>>,
}

impl NpuOffloadRouter {
    pub fn new(inventory: HardwareInventory, governor: GovernorBridge, config: RouterConfig) -> Self {
        Self {
            inventory,
            governor,
            backend: AcceleratorComputeBackend::default(),
            config,
            receipts: Arc::new(RwLock::new(Vec::new())),
        }
    }

    /// Evaluates live thermal & power state and decides the optimal compute target
    pub fn route_workload(&self, req: &WorkloadRequest, queue_depth: usize) -> OffloadDecision {
        let telemetry = self.governor.poll_telemetry();

        // 1. Thermal Throttling or low headroom (<15°C) -> Offload to GPU/NPU
        if telemetry.is_thermal_throttling || telemetry.thermal_headroom_c < 15.0 {
            if self.config.allow_npu_offload && self.inventory.has_npu() {
                return OffloadDecision {
                    target: OffloadTarget::IntelNpu,
                    device_name: "Intel AI Boost / NPU (IVPU)".to_string(),
                    rationale: format!(
                        "THERMAL MITIGATION: CPU headroom is {:.1}°C (< 15°C). Offloading to dedicated low-TDP NPU.",
                        telemetry.thermal_headroom_c
                    ),
                    expected_latency_us: 1200,
                };
            }
            if self.config.allow_gpu_offload && self.inventory.has_gpu() {
                return OffloadDecision {
                    target: OffloadTarget::VulkanCompute,
                    device_name: "Vulkan DRI GPU Device".to_string(),
                    rationale: format!(
                        "THERMAL MITIGATION: CPU headroom is {:.1}°C (< 15°C). Offloading compute to GPU.",
                        telemetry.thermal_headroom_c
                    ),
                    expected_latency_us: 1500,
                };
            }
        }

        // 2. Battery Operation -> Conserve energy with ultra-low power NPU or SIMD
        if !telemetry.ac_connected {
            if self.config.allow_npu_offload && self.inventory.has_npu() {
                return OffloadDecision {
                    target: OffloadTarget::IntelNpu,
                    device_name: "Intel AI Boost / NPU (IVPU)".to_string(),
                    rationale: "BATTERY PRESERVATION: Directing tensor ops to ultra-low power NPU (< 3W TDP).".to_string(),
                    expected_latency_us: 1400,
                };
            }
        }

        // 3. High CPU thermal headroom (>25°C) & AC Connected & Light Queue -> Route to CPU SIMD
        if telemetry.thermal_headroom_c >= self.config.min_cpu_headroom_c
            && telemetry.ac_connected
            && queue_depth <= self.config.high_queue_threshold
        {
            return OffloadDecision {
                target: OffloadTarget::CpuSimd,
                device_name: format!("Host CPU SIMD ({}b lanes)", self.inventory.cpu_simd.vector_width_bits),
                rationale: format!(
                    "OPTIMAL CPU HEADROOM ({:.1}°C >= 25°C on AC): Zero DMA transfer overhead, AVX2/AVX-512 SIMD optimal.",
                    telemetry.thermal_headroom_c
                ),
                expected_latency_us: 900,
            };
        }

        // 4. Heavy Queue Congestion -> Offload to GPU
        if queue_depth > self.config.high_queue_threshold && self.inventory.has_gpu() {
            return OffloadDecision {
                target: OffloadTarget::VulkanCompute,
                device_name: "Vulkan DRI GPU Device".to_string(),
                rationale: format!(
                    "QUEUE CONGESTION DRAIN: Pending queue depth {} exceeds threshold {}. Bursting to GPU.",
                    queue_depth, self.config.high_queue_threshold
                ),
                expected_latency_us: 1300,
            };
        }

        // Fallback default
        OffloadDecision {
            target: OffloadTarget::CpuSimd,
            device_name: "Host CPU SIMD".to_string(),
            rationale: "DEFAULT DISPATCH: Parallel CPU execution.".to_string(),
            expected_latency_us: 1000,
        }
    }

    /// Dispatches batch vector search and archives the signed receipt
    pub fn dispatch_vector_search(
        &self,
        query: &[f32],
        candidates: &[f32],
        dim: usize,
        top_k: usize,
        queue_depth: usize,
    ) -> Result<Vec<(usize, f32)>> {
        let req = WorkloadRequest {
            id: uuid::Uuid::new_v4().to_string(),
            workload_type: WorkloadType::LanceDbVectorQuery,
            batch_size: candidates.len() / dim,
            dimension: dim,
            payload_bytes: (candidates.len() + query.len()) * 4,
        };
        let decision = self.route_workload(&req, queue_depth);
        let (results, receipt) = self.backend.batch_cosine_search(query, candidates, dim, top_k, decision.target)?;

        if let Ok(mut r) = self.receipts.write() {
            r.push(receipt);
        }
        Ok(results)
    }

    /// Dispatches matrix multiplication and archives the signed receipt
    pub fn dispatch_gemm(
        &self,
        a: &[f32],
        b: &[f32],
        m: usize,
        k: usize,
        n: usize,
        queue_depth: usize,
    ) -> Result<Vec<f32>> {
        let req = WorkloadRequest {
            id: uuid::Uuid::new_v4().to_string(),
            workload_type: WorkloadType::MatrixMultiplication,
            batch_size: m,
            dimension: n,
            payload_bytes: (a.len() + b.len()) * 4,
        };
        let decision = self.route_workload(&req, queue_depth);
        let (out, receipt) = self.backend.matrix_multiply(a, b, m, k, n, decision.target)?;

        if let Ok(mut r) = self.receipts.write() {
            r.push(receipt);
        }
        Ok(out)
    }

    /// Dispatches SLM forward layer pass and archives the signed receipt
    pub fn dispatch_slm_layer(
        &self,
        hidden_states: &[f32],
        weights: &[f32],
        dim: usize,
        queue_depth: usize,
    ) -> Result<Vec<f32>> {
        let req = WorkloadRequest {
            id: uuid::Uuid::new_v4().to_string(),
            workload_type: WorkloadType::SlmForwardPass,
            batch_size: hidden_states.len() / dim,
            dimension: dim,
            payload_bytes: (hidden_states.len() + weights.len()) * 4,
        };
        let decision = self.route_workload(&req, queue_depth);
        let (out, receipt) = self.backend.slm_layer_forward(hidden_states, weights, dim, decision.target)?;

        if let Ok(mut r) = self.receipts.write() {
            r.push(receipt);
        }
        Ok(out)
    }

    pub fn total_receipts_recorded(&self) -> usize {
        self.receipts.read().map(|r| r.len()).unwrap_or(0)
    }
}