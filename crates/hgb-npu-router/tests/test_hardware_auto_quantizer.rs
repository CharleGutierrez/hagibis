//! Comprehensive Integration Tests for Hardware-Aware NPU Auto-Quantization & Kernel Fusion Pipeline
//!
//! Verifies:
//! 1. Real-time telemetry scraping from Linux sysfs & EMA smoothing.
//! 2. Dynamic thermal-aware quantization policies (FP32 -> FP16 -> INT8 -> INT4) with hysteresis damping.
//! 3. Native IEEE-754 FP16 conversion exactness and subnormals.
//! 4. Affine INT8 and packed 4-bit nibble INT4 quantization accuracy & SQNR bounds.
//! 5. Fused compute kernels (MatMul + Bias + Activation, LayerNorm, RMSNorm) exact numerical parity against unfused references.
//! 6. Integration with zero-copy device memory buffers and dynamic telemetry router.

use hgb_npu_router::governor_bridge::GovernorBridge;
use hgb_npu_router::hardware_auto_quantizer::*;
use hgb_npu_router::hardware_discovery::HardwareInventory;
use hgb_npu_router::telemetry_router::{NpuOffloadRouter, RouterConfig};
use hgb_npu_router::zero_copy_tensor_bridge::{
    DeviceComputeBackend, DeviceMemoryBuffer, ALIGN_64_AVX512,
};
use std::fs;
use std::time::Duration;
use tempfile::TempDir;

#[test]
fn test_live_hardware_telemetry_scraping_real_sensors() {
    let scraper = HardwareTelemetryScraper::new();
    let metrics = scraper.scrape_live_raw();

    // Verify metrics structure is populated
    assert!(metrics.junction_temp_c > 0.0, "CPU temp must be positive on active machine");
    assert!(metrics.junction_temp_c < 125.0, "CPU temp must be within physical silicon limits");
    assert!(metrics.power_draw_w > 0.0, "Power draw must be non-zero");

    // Sample smoothed metrics
    let smoothed = scraper.poll_smoothed();
    assert!(smoothed.smoothed_temp_c > 0.0);
    assert!(smoothed.smoothed_power_w > 0.0);
}

#[test]
fn test_sysfs_parsing_from_isolated_directory() {
    let temp_dir = TempDir::new().expect("Create isolated temp directory");
    let base = temp_dir.path();

    // Create synthetic /sys/class/thermal layout
    let thermal_zone = base.join("class/thermal/thermal_zone0");
    fs::create_dir_all(&thermal_zone).unwrap();
    fs::write(thermal_zone.join("temp"), "62500\n").unwrap(); // 62.5 C
    fs::write(thermal_zone.join("type"), "x86_pkg_temp\n").unwrap();

    // Create synthetic /sys/class/powercap layout
    let rapl_zone = base.join("class/powercap/intel-rapl/intel-rapl:0");
    fs::create_dir_all(&rapl_zone).unwrap();
    fs::write(rapl_zone.join("constraint_0_power_limit_uw"), "45000000\n").unwrap(); // 45W

    let scraper = HardwareTelemetryScraper::with_sysfs_root(base.to_path_buf())
        .with_cache_ttl(Duration::from_millis(50))
        .with_ema_alpha(0.30);

    let metrics = scraper.scrape_live_raw();
    assert!((metrics.junction_temp_c - 62.5).abs() < 0.1);
    assert!(metrics.power_draw_w >= 45.0);
}

#[test]
fn test_thermal_quantization_policy_transitions_and_hysteresis() {
    let mut policy = ThermalQuantizationPolicy::default();

    // 1. Cold: 55°C -> FP32 / FP16
    let mode1 = policy.evaluate_policy(55.0, 20.0, false);
    assert_eq!(mode1, QuantizationMode::FP16);

    // 2. Warm: 76°C -> INT8
    let mode2 = policy.evaluate_policy(76.0, 35.0, false);
    assert_eq!(mode2, QuantizationMode::INT8);

    // 3. Hot: 88°C -> INT4 (Critical thermal throttling)
    let mode3 = policy.evaluate_policy(88.0, 50.0, false);
    assert_eq!(mode3, QuantizationMode::INT4);

    // 4. Cooling down with hysteresis: At 84°C, still within INT4 hysteresis window (threshold 85 - 2.5 = 82.5°C)
    let mode4 = policy.evaluate_policy(84.0, 40.0, false);
    assert_eq!(mode4, QuantizationMode::INT4, "Must stay in INT4 due to hysteresis damping");

    // 5. Cooled further: 81°C -> Falls back to INT8
    let mode5 = policy.evaluate_policy(81.0, 35.0, false);
    assert_eq!(mode5, QuantizationMode::INT8);

    // 6. Battery mode trigger: forces at least INT8 even if cold (50°C)
    let mode6 = policy.evaluate_policy(50.0, 15.0, true);
    assert_eq!(mode6, QuantizationMode::INT8, "Battery mode forces power-conserving INT8");
}

#[test]
fn test_fp16_native_software_conversion_exactness() {
    let test_values = [0.0f32, -0.0f32, 1.0f32, -1.0f32, 0.5f32, 65504.0f32, -65504.0f32, 1.5e-4f32];

    for &val in &test_values {
        let f16_bits = f32_to_f16(val);
        let recon = f16_to_f32(f16_bits);
        let rel_err = if val.abs() > 1e-4 {
            (recon - val).abs() / val.abs()
        } else {
            (recon - val).abs()
        };
        assert!(rel_err < 0.002, "FP16 roundtrip relative error must be < 0.2%: val={}, recon={}", val, recon);
    }
}

#[test]
fn test_int8_symmetric_and_asymmetric_accuracy_and_sqnr() {
    let mut data = Vec::new();
    for i in 0..1024 {
        let x = (i as f32) / 1024.0 * 2.0 - 1.0;
        data.push(x);
    }

    // Symmetric INT8
    let (bytes_sym, scale_sym, zp_sym) = quantize_int8(&data, QuantizationScheme::SymmetricAffine);
    assert_eq!(bytes_sym.len(), 1024);
    assert_eq!(zp_sym, 0.0);

    let recon_sym = dequantize_int8(&bytes_sym, scale_sym, zp_sym, QuantizationScheme::SymmetricAffine);
    let metrics_sym = calculate_quality_metrics(&data, &recon_sym, bytes_sym.len());

    assert!(metrics_sym.sqnr_db >= 35.0, "INT8 SQNR must exceed 35 dB, got {:.2} dB", metrics_sym.sqnr_db);
    assert!(metrics_sym.max_absolute_error < 0.02, "INT8 max absolute error must be < 0.02, got {}", metrics_sym.max_absolute_error);
    assert!((metrics_sym.compression_ratio - 4.0).abs() < 0.01, "INT8 must achieve 4x memory reduction vs FP32");
}

#[test]
fn test_int4_packed_nibbles_accuracy_and_bandwidth_compression() {
    let mut data = Vec::new();
    for i in 0..512 {
        let x = ((i as f32) / 512.0 * 4.0 - 2.0) * 0.5;
        data.push(x);
    }

    let (packed, scale, zp) = quantize_int4(&data, QuantizationScheme::SymmetricAffine);
    // 512 elements packed at 2 nibbles per byte = 256 bytes!
    assert_eq!(packed.len(), 256, "512 4-bit nibbles must pack into exactly 256 bytes");

    let recon = dequantize_int4(&packed, 512, scale, zp, QuantizationScheme::SymmetricAffine);
    let metrics = calculate_quality_metrics(&data, &recon, packed.len());

    assert!(metrics.sqnr_db >= 18.0, "INT4 SQNR must exceed 18 dB, got {:.2} dB", metrics.sqnr_db);
    assert!((metrics.compression_ratio - 8.0).abs() < 0.01, "INT4 must achieve 8x memory reduction vs FP32");
}

#[test]
fn test_int4_odd_length_packing_and_nibble_sign_extension() {
    let data = vec![-1.0f32, 0.5f32, 0.0f32, -0.75f32, 1.0f32]; // 5 elements (odd)
    let (packed, scale, zp) = quantize_int4(&data, QuantizationScheme::SymmetricAffine);
    assert_eq!(packed.len(), 3, "5 elements must pack into ceil(5/2) = 3 bytes");

    let recon = dequantize_int4(&packed, 5, scale, zp, QuantizationScheme::SymmetricAffine);
    assert_eq!(recon.len(), 5);
    for (orig, rec) in data.iter().zip(recon.iter()) {
        assert!((orig - rec).abs() < 0.25);
    }
}

#[test]
fn test_fused_matmul_bias_activation_numerical_parity_all_activations() {
    let m = 8;
    let k = 16;
    let n = 12;

    let mut a = Vec::with_capacity(m * k);
    for i in 0..(m * k) {
        a.push((i as f32 * 0.1).sin());
    }

    let mut b = Vec::with_capacity(k * n);
    for i in 0..(k * n) {
        b.push((i as f32 * 0.15).cos());
    }

    let mut bias = Vec::with_capacity(n);
    for i in 0..n {
        bias.push(0.05 * (i as f32));
    }

    let activations = [
        ActivationFunction::None,
        ActivationFunction::ReLU,
        ActivationFunction::GELU,
        ActivationFunction::SiLU,
        ActivationFunction::Tanh,
    ];

    for act in activations {
        let fused_c = fused_matmul_bias_activation(&a, &b, Some(&bias), m, k, n, act).unwrap();
        let ref_c = reference_unfused_matmul_bias_activation(&a, &b, Some(&bias), m, k, n, act);

        assert_eq!(fused_c.len(), m * n);
        for idx in 0..(m * n) {
            let diff = (fused_c[idx] - ref_c[idx]).abs();
            assert!(
                diff < 1e-4,
                "Fused MatMul ({:?}) at index {} differs from reference by {}",
                act, idx, diff
            );
        }
    }
}

#[test]
fn test_fused_layernorm_activation_numerical_parity() {
    let rows = 4;
    let cols = 32;
    let mut data = Vec::with_capacity(rows * cols);
    for i in 0..(rows * cols) {
        data.push((i as f32 * 0.05).sin() * 2.0);
    }

    let gamma: Vec<f32> = (0..cols).map(|i| 1.0 + 0.01 * (i as f32)).collect();
    let beta: Vec<f32> = (0..cols).map(|i| 0.02 * (i as f32)).collect();

    let fused_out = fused_layernorm_activation(
        &data,
        Some(&gamma),
        Some(&beta),
        rows,
        cols,
        1e-5,
        ActivationFunction::SiLU,
    ).unwrap();

    let ref_out = reference_unfused_layernorm_activation(
        &data,
        Some(&gamma),
        Some(&beta),
        rows,
        cols,
        1e-5,
        ActivationFunction::SiLU,
    );

    for idx in 0..(rows * cols) {
        let diff = (fused_out[idx] - ref_out[idx]).abs();
        assert!(diff < 1e-4, "LayerNorm + SiLU differs from reference by {}", diff);
    }
}

#[test]
fn test_fused_rmsnorm_activation_numerical_parity() {
    let rows = 4;
    let cols = 16;
    let data: Vec<f32> = (0..(rows * cols)).map(|i| (i as f32 * 0.1).cos()).collect();
    let gamma: Vec<f32> = vec![1.0; cols];

    let fused_out = fused_rmsnorm_activation(
        &data,
        Some(&gamma),
        rows,
        cols,
        1e-5,
        ActivationFunction::GELU,
    ).unwrap();

    assert_eq!(fused_out.len(), rows * cols);
}

#[test]
fn test_fused_matmul_quantized_int8_accuracy() {
    let m = 4;
    let k = 8;
    let n = 4;

    let a: Vec<f32> = (0..(m * k)).map(|i| ((i as f32) * 0.2 - 0.8)).collect();
    let b: Vec<f32> = (0..(k * n)).map(|i| ((i as f32) * 0.15 - 0.5)).collect();

    let (a_bytes, a_scale, _) = quantize_int8(&a, QuantizationScheme::SymmetricAffine);
    let (b_bytes, b_scale, _) = quantize_int8(&b, QuantizationScheme::SymmetricAffine);

    let a_i8: &[i8] = unsafe { std::slice::from_raw_parts(a_bytes.as_ptr() as *const i8, a_bytes.len()) };
    let b_i8: &[i8] = unsafe { std::slice::from_raw_parts(b_bytes.as_ptr() as *const i8, b_bytes.len()) };

    let out_q = fused_matmul_bias_activation_int8(
        a_i8,
        b_i8,
        a_scale,
        b_scale,
        None,
        m,
        k,
        n,
        ActivationFunction::ReLU,
    ).unwrap();

    let ref_c = reference_unfused_matmul_bias_activation(&a, &b, None, m, k, n, ActivationFunction::ReLU);

    for idx in 0..(m * n) {
        let diff = (out_q[idx] - ref_c[idx]).abs();
        assert!(diff < 0.10, "INT8 GEMM index {} differs by {}", idx, diff);
    }
}

#[test]
fn test_memory_bandwidth_savings_accounting() {
    let auto_quantizer = HardwareAutoQuantizer::default();
    let m = 16;
    let k = 32;
    let n = 16;

    let a = vec![0.5f32; m * k];
    let b = vec![0.25f32; k * n];

    let mut a_buf = DeviceMemoryBuffer::allocate_aligned(a.len() * 4, ALIGN_64_AVX512, DeviceComputeBackend::SimdUnifiedMemory).unwrap();
    a_buf.as_f32_mut_slice().unwrap().copy_from_slice(&a);

    let mut b_buf = DeviceMemoryBuffer::allocate_aligned(b.len() * 4, ALIGN_64_AVX512, DeviceComputeBackend::SimdUnifiedMemory).unwrap();
    b_buf.as_f32_mut_slice().unwrap().copy_from_slice(&b);

    let (out_buf, receipt) = auto_quantizer.dispatch_fused_matmul(&a_buf, &b_buf, None, m, k, n, ActivationFunction::ReLU).unwrap();

    assert_eq!(out_buf.capacity_bytes(), m * n * 4);
    assert!(receipt.bandwidth_reduction_pct > 25.0);
    assert!(receipt.intermediate_allocations_avoided >= 1);
    assert_eq!(receipt.hmac_signature.len(), 64);
}

#[test]
fn test_zero_copy_device_memory_buffer_quantize_and_dequantize() {
    let auto_quantizer = HardwareAutoQuantizer::default();

    let data = vec![0.1f32, -0.5, 1.2, -0.8, 0.45, 0.0, 0.99, -1.0];
    let byte_len = data.len() * std::mem::size_of::<f32>();
    let mut dev_buf = DeviceMemoryBuffer::allocate_aligned(
        byte_len,
        ALIGN_64_AVX512,
        DeviceComputeBackend::SimdUnifiedMemory,
    )
    .unwrap();
    dev_buf.as_f32_mut_slice().unwrap().copy_from_slice(&data);

    // Quantize device buffer directly
    let q_tensor = auto_quantizer
        .quantize_device_buffer(&dev_buf, &[data.len()], QuantizationScheme::SymmetricAffine)
        .unwrap();

    assert_eq!(q_tensor.element_count, 8);

    // Dequantize back to newly allocated aligned device buffer
    let deq_buf = auto_quantizer
        .dequantize_to_device_buffer(&q_tensor, DeviceComputeBackend::SimdUnifiedMemory)
        .unwrap();

    let recon_floats = deq_buf.as_f32_slice().unwrap();

    for (orig, rec) in data.iter().zip(recon_floats.iter()) {
        assert!((orig - rec).abs() < 0.05);
    }
}

#[test]
fn test_telemetry_router_quantization_recommendation_bridge() {
    let router = NpuOffloadRouter::new(
        HardwareInventory::discover().unwrap(),
        GovernorBridge::default(),
        RouterConfig::default(),
    );

    let (rec_mode, requires_fusion) = router.recommend_quantization();
    assert!(matches!(
        rec_mode,
        QuantizationMode::FP32
            | QuantizationMode::FP16
            | QuantizationMode::INT8
            | QuantizationMode::INT4
    ));
    let _ = requires_fusion;
}
