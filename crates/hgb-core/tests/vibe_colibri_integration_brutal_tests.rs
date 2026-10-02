use hgb_core::colibri::*;

#[test]
fn test_memory_tiering() {
    let engine = MultiTierMemory::new("/nvme/cache", 16384, 8192);
    let result = engine.stream_weights("gpt-4-mini");
    assert!(result.is_ok());
}

#[test]
fn test_moe_streamer() {
    let engine = DynamicMoE::new(4);
    let result = engine.load_expert("math_expert_7");
    assert!(result.is_ok());
}

#[test]
fn test_hardware_adapter() {
    let engine = AutoAdapter::new();
    let result = engine.probe_and_adapt();
    assert!(result.is_ok());
    assert_eq!(result.unwrap(), "Adapted to optimal hardware configuration");
}

#[test]
fn test_native_inference() {
    let engine = ZeroDependencyEngine::new(true);
    let result = engine.generate("Calculate 2+2");
    assert!(result.is_ok());
    assert_eq!(result.unwrap(), "Generated response for: Calculate 2+2");
}
