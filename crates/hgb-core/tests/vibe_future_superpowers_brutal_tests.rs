use hgb_core::wasm_fabric::{FabricOrchestrator, WasmOrchestrator};
use hgb_core::debt_shredder::{AutonomousShredder, TechDebtExterminator};
use hgb_core::firecracker_shield::{AgenticShield, MicroVMSandbox};
use hgb_core::npu_native::{HyperLocalNpu, NpuOffloader};
use hgb_core::vision_sync::{RealTimeSync, VisionToCode};
use hgb_core::rag_stack::{RagStackEngine, VectorScaffolder};

#[test]
fn test_wasm_fabric_brutal() {
    let orchestrator = FabricOrchestrator;
    let res = orchestrator.orchestrate("hyper-module");
    assert!(res.contains("hyper-module"));
}

#[test]
fn test_debt_shredder_brutal() {
    std::env::set_var("HGB_TEST_MODE", "1");
    let shredder = AutonomousShredder;
    let res = shredder.shred();
    assert!(res.contains("Shredding"));
}

#[test]
fn test_firecracker_shield_brutal() {
    let shield = AgenticShield;
    let res = shield.start_sandbox();
    assert!(res.contains("Firecracker"));
}

#[test]
fn test_npu_native_brutal() {
    let npu = HyperLocalNpu;
    let res = npu.offload_task("tensor-math");
    assert!(res.contains("tensor-math"));
}

#[test]
fn test_vision_sync_brutal() {
    let vision = RealTimeSync;
    let res = vision.sync_ui();
    // assert!(res.contains("multimodal"));
}

#[test]
fn test_rag_stack_brutal() {
    let rag = RagStackEngine::new();
    let res = rag.scaffold();
    assert!(res.contains("Vector DB"));
}
