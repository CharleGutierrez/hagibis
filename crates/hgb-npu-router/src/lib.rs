//! # Hagibis Heterogeneous Hardware Accelerator & NPU Offloader Router
//!
//! Production-grade hardware acceleration, telemetry ingestion, and dynamic offloading
//! for tensor batch embeddings, SLM inference, and LanceDB vector queries.

pub mod error;
pub mod hardware_discovery;
pub mod governor_bridge;
pub mod accelerator_backend;
pub mod telemetry_router;

pub use error::{NpuRouterError, Result};
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

#[cfg(test)]
mod integration_tests {
    use super::*;
    use tempfile::tempdir;
    use std::fs;

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
}