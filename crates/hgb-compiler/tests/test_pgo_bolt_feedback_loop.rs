//! # Integration Tests for Autonomous PGO/BOLT Continuous Feedback Loop
//!
//! Validates:
//! 1. Telemetry & profile metric parsing (JSON, Linux perf stat, Telemetry Bridge).
//! 2. Hot-path detection and optimization pass classification.
//! 3. Compiler flag adjustment and aggressive optimization pass synthesis.
//! 4. Nightly benchmark automation runner and `pgo_optimization_manifest.json` emission.

use tempfile::tempdir;

use hgb_compiler::pgo_bolt_feedback::{
    BenchmarkScore, HotPathDetector, HotPathThresholds, NightlyBenchmarkConfig,
    NightlyBenchmarkRunner, OptimizationAction, OptimizationPassSynthesizer, PerformanceDelta,
    PgoOptimizationManifest, ProfiledFunctionMetric, WorkloadMetricParser, WorkloadProfileMetrics,
};

#[test]
fn test_metric_parsing_json() {
    let json_data = r#"{
        "timestamp_epoch_secs": 1732145000,
        "workload_name": "agent_actor_mesh_throughput",
        "target_binary": "hgbd",
        "total_samples": 50000,
        "total_cycles": 120000000,
        "total_instructions": 180000000,
        "ipc": 1.5,
        "l1_icache_miss_rate_pct": 14.2,
        "l1_dcache_miss_rate_pct": 3.8,
        "branch_miss_rate_pct": 7.5,
        "p99_latency_ms": 42.5,
        "throughput_ops_per_sec": 18500.0,
        "hot_functions": [
            {
                "function_name": "hgb_mesh::quic_poll_events",
                "call_count": 12000,
                "cpu_cycles": 25000000,
                "branch_mispredictions": 950,
                "branch_count": 15000,
                "icache_misses": 3400,
                "dcache_misses": 500,
                "is_loop_heavy": false,
                "is_simd_candidate": false,
                "hotness_score": 15450.0
            },
            {
                "function_name": "hgb_compiler::simd_vector_dot",
                "call_count": 8000,
                "cpu_cycles": 45000000,
                "branch_mispredictions": 120,
                "branch_count": 25000,
                "icache_misses": 400,
                "dcache_misses": 200,
                "is_loop_heavy": true,
                "is_simd_candidate": true,
                "hotness_score": 21380.0
            }
        ]
    }"#;

    let metrics = WorkloadMetricParser::parse_json(json_data).expect("Must parse valid JSON metrics");
    assert_eq!(metrics.workload_name, "agent_actor_mesh_throughput");
    assert_eq!(metrics.target_binary, "hgbd");
    assert_eq!(metrics.total_samples, 50000);
    assert!((metrics.ipc - 1.5).abs() < 1e-4);
    assert!((metrics.l1_icache_miss_rate_pct - 14.2).abs() < 1e-4);
    assert!((metrics.branch_miss_rate_pct - 7.5).abs() < 1e-4);
    assert_eq!(metrics.hot_functions.len(), 2);

    let quic_fn = &metrics.hot_functions[0];
    assert_eq!(quic_fn.function_name, "hgb_mesh::quic_poll_events");
    assert_eq!(quic_fn.call_count, 12000);
    assert!(quic_fn.branch_misprediction_rate_pct() > 6.0);
}

#[test]
fn test_metric_parsing_perf_stat() {
    let perf_output = r#"
 Performance counter stats for 'hgbd --bench':

     1,842,501,230      cycles                    #    3.245 GHz
     2,763,751,845      instructions              #    1.50  insn per cycle
        54,231,100      branches                  #   95.421 M/sec
         4,120,560      branch-misses             #    7.60% of all branches
        18,450,200      L1-icache-load-misses     #   13.25% of all L1-icache hits
       139,246,792      L1-icache-loads
         5,120,400      L1-dcache-load-misses     #    2.80% of all L1-dcache hits
       182,871,428      L1-dcache-loads

       0.567823451 seconds time elapsed
    "#;

    let metrics = WorkloadMetricParser::parse_perf_stat(perf_output, "nightly_perf", "hgbd")
        .expect("Must parse perf stat output");

    assert_eq!(metrics.total_cycles, 1_842_501_230);
    assert_eq!(metrics.total_instructions, 2_763_751_845);
    assert!((metrics.ipc - 1.50).abs() < 0.05);
    assert!((metrics.branch_miss_rate_pct - 7.60).abs() < 0.1);
    assert!((metrics.l1_icache_miss_rate_pct - 13.25).abs() < 0.2);
    assert!((metrics.l1_dcache_miss_rate_pct - 2.80).abs() < 0.2);
}

#[test]
fn test_metric_parsing_telemetry_bridge() {
    let bridge_json = r#"{
        "cpu_governor": "performance",
        "p99_latency_ms": 35.8,
        "queue_depth": 5,
        "active_workers": 8,
        "icache_miss_rate_pct": 15.4,
        "branch_miss_rate_pct": 6.2,
        "top_functions": [
            {
                "function_name": "hgb_compiler::matrix_multiply",
                "call_count": 1500,
                "total_cpu_cycles": 18000000,
                "branch_mispredictions": 420
            },
            {
                "function_name": "hgb_storage::lsm_tree_search",
                "call_count": 6000,
                "total_cpu_cycles": 8000000,
                "branch_mispredictions": 890
            }
        ]
    }"#;

    let metrics = WorkloadMetricParser::parse_telemetry_bridge(bridge_json, "bridge_live", "hgbd")
        .expect("Must parse telemetry bridge JSON");

    assert_eq!(metrics.workload_name, "bridge_live");
    assert!((metrics.l1_icache_miss_rate_pct - 15.4).abs() < 1e-4);
    assert!((metrics.branch_miss_rate_pct - 6.2).abs() < 1e-4);
    assert_eq!(metrics.hot_functions.len(), 2);
    assert_eq!(metrics.hot_functions[0].function_name, "hgb_compiler::matrix_multiply");
    assert!(metrics.hot_functions[0].is_simd_candidate);
}

#[test]
fn test_hot_path_detection_and_classification() {
    let mut metrics = WorkloadProfileMetrics {
        timestamp_epoch_secs: 1732145000,
        workload_name: "test_workload".to_string(),
        target_binary: "hgbd".to_string(),
        total_samples: 10000,
        total_cycles: 50_000_000,
        total_instructions: 80_000_000,
        ipc: 1.6,
        l1_icache_miss_rate_pct: 14.5, // > 10% threshold -> ColdSplit
        l1_dcache_miss_rate_pct: 2.5,
        branch_miss_rate_pct: 6.8,     // > 5% threshold -> BoltLayoutReorder
        p99_latency_ms: 22.0,
        throughput_ops_per_sec: 14000.0,
        hot_functions: Vec::new(),
    };

    // 1. Function A: Inlining candidate (High calls, low cycle per call)
    let mut fn_inline = ProfiledFunctionMetric::new("hgb_core::small_hash_lookup", 10_000, 500_000);
    fn_inline.branch_count = 10_000;
    fn_inline.branch_mispredictions = 50;
    fn_inline.recompute_hotness_score();
    metrics.hot_functions.push(fn_inline);

    // 2. Function B: SIMD vectorization candidate (Loop heavy, matrix multiplication)
    let mut fn_simd = ProfiledFunctionMetric::new("hgb_compiler::vector_matrix_multiply_f32", 500, 30_000_000);
    fn_simd.is_simd_candidate = true;
    fn_simd.branch_count = 50_000;
    fn_simd.branch_mispredictions = 120;
    fn_simd.recompute_hotness_score();
    metrics.hot_functions.push(fn_simd);

    // 3. Function C: High branch misprediction routine -> BOLT layout reordering
    let mut fn_branch = ProfiledFunctionMetric::new("hgb_mesh::packet_dispatch_branchy", 2_000, 4_000_000);
    fn_branch.branch_count = 10_000;
    fn_branch.branch_mispredictions = 1_200; // 12% misprediction
    fn_branch.icache_misses = 800;
    fn_branch.recompute_hotness_score();
    metrics.hot_functions.push(fn_branch);

    let thresholds = HotPathThresholds::default();
    let classified = HotPathDetector::detect_and_classify(&metrics, &thresholds);

    assert_eq!(classified.len(), 3);

    // Verify ordering: highest hotness score first
    assert!(classified[0].hotness_score >= classified[1].hotness_score);
    assert!(classified[1].hotness_score >= classified[2].hotness_score);

    // Verify Inlining recommendation
    let lookup_res = classified
        .iter()
        .find(|c| c.function_name == "hgb_core::small_hash_lookup")
        .expect("Must find small_hash_lookup");
    assert!(lookup_res.recommended_actions.contains(&OptimizationAction::AggressiveInlining));

    // Verify SIMD recommendation
    let simd_res = classified
        .iter()
        .find(|c| c.function_name == "hgb_compiler::vector_matrix_multiply_f32")
        .expect("Must find vector_matrix_multiply_f32");
    assert!(simd_res.recommended_actions.contains(&OptimizationAction::TargetVectorization));

    // Verify BOLT reorder recommendation
    let branch_res = classified
        .iter()
        .find(|c| c.function_name == "hgb_mesh::packet_dispatch_branchy")
        .expect("Must find packet_dispatch_branchy");
    assert!(branch_res.recommended_actions.contains(&OptimizationAction::BoltLayoutReorder));
    assert!(branch_res.recommended_actions.contains(&OptimizationAction::ColdSplit));
}

#[test]
fn test_compiler_flag_adjustment_and_aggressive_synthesis() {
    let mut classified = Vec::new();

    // Create 7 hot paths to trigger 750 inline threshold
    for i in 0..7 {
        classified.push(hgb_compiler::pgo_bolt_feedback::ClassifiedHotPath {
            function_name: format!("hgb_worker::task_{}", i),
            hotness_score: 500.0 + (i as f64) * 10.0,
            call_count: 5000,
            cpu_cycles: 2000000,
            recommended_actions: vec![
                OptimizationAction::AggressiveInlining,
                OptimizationAction::TargetVectorization,
                OptimizationAction::BoltLayoutReorder,
            ],
            rationale: "hot agent task routing".to_string(),
        });
    }

    let config = OptimizationPassSynthesizer::synthesize(&classified, None, None);

    // Verify Inlining threshold scaled to 750
    assert_eq!(config.inline_threshold, 750);
    assert!(config.rustc_flags.contains(&"-C inline-threshold=750".to_string()));

    // Verify Target CPU Native
    assert_eq!(config.target_cpu, "native");
    assert!(config.rustc_flags.contains(&"-C target-cpu=native".to_string()));

    // Verify LTO and codegen units
    assert!(config.rustc_flags.contains(&"-C lto=fat".to_string()));
    assert!(config.rustc_flags.contains(&"-C codegen-units=1".to_string()));

    // Verify Linker flags for BOLT relocations
    assert!(config.rustc_flags.contains(&"-C link-arg=-Wl,-q".to_string()));

    // Verify LLVM vectorizer args
    assert!(config.llvm_args.contains(&"-vectorize-loops=true".to_string()));
    assert!(config.llvm_args.contains(&"-vectorize-slp=true".to_string()));

    // Verify BOLT passes
    assert!(config.bolt_flags.contains(&"-reorder-blocks=ext-tsp".to_string()));
    assert!(config.bolt_flags.contains(&"-reorder-functions=cdsort".to_string()));
    assert!(config.bolt_flags.contains(&"-split-functions".to_string()));
    assert!(config.bolt_flags.contains(&"-split-all-cold".to_string()));
    assert!(config.bolt_flags.contains(&"-dyno-stats".to_string()));

    // Verify RUSTFLAGS env serialization
    let rustflags_env = config.to_rustflags_env();
    assert!(rustflags_env.contains("-C inline-threshold=750"));
    assert!(rustflags_env.contains("-C target-cpu=native"));
    assert!(rustflags_env.contains("-C llvm-args=-vectorize-loops=true"));
    assert!(rustflags_env.contains("-C link-arg=-Wl,-q"));

    // Verify Candidates Tracking
    assert_eq!(config.inlining_candidate_functions.len(), 7);
    assert_eq!(config.simd_candidate_functions.len(), 7);
    assert_eq!(config.bolt_reordered_functions.len(), 7);
}

#[test]
fn test_nightly_benchmark_runner_and_manifest_emission() {
    let temp = tempdir().expect("Must create temp directory");
    let manifest_path = temp.path().join("pgo_optimization_manifest.json");

    let bench_config = NightlyBenchmarkConfig {
        iterations: 30,
        matrix_dim: 32,
        vector_dim: 256,
        token_text_size: 2048,
        target_binary: "hgbd".to_string(),
        output_manifest_path: manifest_path.clone(),
    };

    let manifest = NightlyBenchmarkRunner::run_nightly_benchmarks(&bench_config)
        .expect("Nightly benchmark suite execution must succeed");

    // 1. Verify Manifest Content
    assert_eq!(manifest.schema_version, "v1");
    assert_eq!(manifest.target_binary, "hgbd");
    assert!(manifest.speedup_multiplier > 1.0, "Expected speedup > 1.0, got {}", manifest.speedup_multiplier);
    assert!(manifest.verified_speedup);
    assert!(manifest.performance_delta.latency_p50_reduction_pct >= 0.0);
    assert!(manifest.detected_hot_paths.len() >= 2);

    // 2. Verify File Was Written to Disk
    assert!(manifest_path.exists(), "Manifest file must exist at {}", manifest_path.display());

    // 3. Verify Manifest File Deserialization Round-Trip
    let loaded_manifest = PgoOptimizationManifest::load_from_file(&manifest_path)
        .expect("Must read and deserialize manifest from disk");

    assert_eq!(loaded_manifest.schema_version, manifest.schema_version);
    assert_eq!(loaded_manifest.target_binary, manifest.target_binary);
    assert_eq!(loaded_manifest.speedup_multiplier, manifest.speedup_multiplier);
    assert_eq!(loaded_manifest.compiler_config.inline_threshold, manifest.compiler_config.inline_threshold);
    assert_eq!(loaded_manifest.detected_hot_paths.len(), manifest.detected_hot_paths.len());
}

#[test]
fn test_manifest_performance_delta_computation() {
    let baseline = BenchmarkScore {
        latency_p50_us: 150.0,
        latency_p99_us: 520.0,
        throughput_mops: 10.0,
        icache_miss_rate_pct: 15.0,
        branch_miss_rate_pct: 8.0,
        total_cpu_cycles: 480_000_000,
    };

    let optimized = BenchmarkScore {
        latency_p50_us: 75.0,
        latency_p99_us: 210.0,
        throughput_mops: 22.0,
        icache_miss_rate_pct: 3.5,
        branch_miss_rate_pct: 1.6,
        total_cpu_cycles: 220_000_000,
    };

    let delta = PerformanceDelta::compute(&baseline, &optimized);

    // Latency cut in half -> 50% reduction, speedup ~ 2.0x
    assert!((delta.latency_p50_reduction_pct - 50.0).abs() < 1e-2);
    assert!((delta.speedup_multiplier - 2.0).abs() < 1e-2);
    assert!(delta.throughput_gain_pct > 100.0);
    assert!(delta.icache_miss_reduction_pct > 70.0);
    assert!(delta.branch_miss_reduction_pct > 75.0);
}
