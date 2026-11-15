//! # Hagibis Heterogeneous Hardware Accelerator & NPU Offloader Router
//!
//! Production-grade hardware acceleration, telemetry ingestion, dynamic offloading,
//! and Pareto-optimal hardware auto-tuning for tensor batch embeddings, SLM inference,
//! and LanceDB vector queries.

pub mod error;
pub mod hardware_discovery;
pub mod governor_bridge;
pub mod accelerator_backend;
pub mod dynamic_benchmarker;
pub mod shard_allocator;
pub mod telemetry_router;
pub mod pareto_frontier_autotuner;
pub mod hardware_aware_dynamic_router;
pub mod realtime_telemetry_load_balancer;

pub use error::{NpuRouterError, Result};
pub use hardware_aware_dynamic_router::{
    DynamicOffloadReceipt, DynamicRouteTarget, HardwareAwareDynamicRouter, LatencySla,
    LiveHardwareCounters, SlaRoutingDecision, VectorSearchResult, VectorShardPlan,
    LANCEDB_EMBEDDING_DIM,
};
pub use hardware_discovery::{
    AcceleratorDevice, AcceleratorType, ComputeCapability, GpuVendor,
    HardwareInventory, NpuVendor, SimdFeatureSet,
};
pub use governor_bridge::{
    GovernorBridge, GovernorProfile, LiveTelemetrySample,
};
pub use accelerator_backend::{
    AcceleratorComputeBackend, OffloadDecision, OffloadReceipt, OffloadTarget,
    WorkloadRequest, WorkloadType,
};
pub use telemetry_router::{
    NpuOffloadRouter, RouterConfig,
};
pub use dynamic_benchmarker::{
    AcceleratorBenchmarkReport, BenchmarkMetricResult, BenchmarkTarget, BenchmarkWorkload,
    DynamicHardwareBenchmarker,
};
pub use shard_allocator::{
    AgentToolCall, AgentToolCallOutput, PowerAwareShardAllocator, ShardAllocatorConfig,
    ShardPartitionPlan, ShardTarget, ShardedToolExecutionResult, ShardedVectorSearchResult,
    WorkloadShard,
};
pub use pareto_frontier_autotuner::{
    AcceleratorBackendTarget, HardwareTelemetrySnapshot, ParetoAnalysisReport,
    ParetoAutoDispatcher, ParetoAutoTunerConfig, ParetoDispatchEvent,
    ParetoOperatingPoint,
};
pub use realtime_telemetry_load_balancer::{
    AcceleratorNodeTelemetry, AcceleratorTargetKind, CongestionWeights,
    DynamicInferenceResult, ExecutionReceipt,
    LlmInferenceJob, LoadBalancerConfig, MigrationReceipt, MigrationTriggerReason,
    PcieSaturationMetrics, RealtimeTelemetryLoadBalancer, ThermalTelemetry,
    VectorQueryJob, VramTelemetry,
};

#[cfg(test)]
mod integration_tests {
    use super::*;

    #[test]
    fn test_end_to_end_router_dispatch() {
        let inventory = HardwareInventory::discover().unwrap();
        let governor = GovernorBridge::default();
        let router = NpuOffloadRouter::new(inventory, governor, RouterConfig::default());

        // Vector search test
        let query = vec![0.5f32; 64];
        let mut candidates = vec![0.0f32; 10 * 64];
        for i in 0..candidates.len() {
            candidates[i] = (i % 7) as f32 * 0.1;
        }

        let hits = router.dispatch_vector_search(&query, &candidates, 64, 3, 0).unwrap();
        assert_eq!(hits.len(), 3);
        assert!(router.total_receipts_recorded() >= 1);

        // GEMM test
        let a = vec![1.0f32; 4 * 8];
        let b = vec![2.0f32; 8 * 4];
        let c = router.dispatch_gemm(&a, &b, 4, 8, 4, 0).unwrap();
        assert_eq!(c.len(), 16);
        assert_eq!(c[0], 16.0); // 8 * (1.0 * 2.0)

        // SLM forward pass test
        let hidden = vec![1.0f32; 2 * 32];
        let weights = vec![0.05f32; 32 * 32];
        let slm_out = router.dispatch_slm_layer(&hidden, &weights, 32, 0).unwrap();
        assert_eq!(slm_out.len(), 64);
    }

    #[test]
    fn test_cryptographic_receipt_verification() {
        let backend = AcceleratorComputeBackend::default();
        let q = vec![1.0f32; 16];
        let db = vec![1.0f32; 4 * 16];
        let (_, receipt) = backend.batch_cosine_search(&q, &db, 16, 2, OffloadTarget::CpuSimd).unwrap();

        assert!(!receipt.sha256_signature.is_empty());
        assert!(receipt.verify_signature(&backend.secret_key));

        let wrong_key = b"wrong_secret_key";
        assert!(!receipt.verify_signature(wrong_key));
    }

    #[test]
    fn test_pareto_auto_dispatcher_integration() {
        let dispatcher = ParetoAutoDispatcher::auto_discover(ParetoAutoTunerConfig::default()).unwrap();

        // Dispatch vector query through Pareto dispatcher
        let query = vec![0.5f32; 32];
        let candidates = vec![0.1f32; 20 * 32];
        let (hits, receipt, event) = dispatcher.dispatch_vector_search(&query, &candidates, 32, 5).unwrap();

        assert_eq!(hits.len(), 5);
        assert!(!receipt.receipt_id.is_empty());
        assert!(!receipt.sha256_signature.is_empty());
        assert!(event.evaluated_points.len() >= 2);
        assert!(!event.rationale.is_empty());
    }

    #[tokio::test]
    async fn test_hardware_aware_dynamic_router_integration() {
        let router = HardwareAwareDynamicRouter::new(b"production-test-key-32b-secret!");
        let dim = 768;
        let query = vec![0.1f32; dim];
        let candidates = vec![0.2f32; 16 * dim];

        let (hits, receipt) = router
            .dispatch_vector_search(&query, &candidates, dim, 3, LatencySla::SubMillisecond)
            .await
            .unwrap();

        assert_eq!(hits.len(), 3);
        assert_eq!(receipt.dimension, 768);
        assert!(receipt.verify(b"production-test-key-32b-secret!"));
    }
}
