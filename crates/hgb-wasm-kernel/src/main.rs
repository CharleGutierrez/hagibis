//! # Hagibis WebAssembly Microkernel Native Runner (`hgb-wasm-kernel`)
//!
//! High-performance native runner binary for executing WebAssembly microkernel tools,
//! hardware-accelerated SIMD vector linear algebra routines, and agentic compute offloading.
//! Used in autonomous PGO and LLVM BOLT profile-guided optimization pipelines.

use std::env;
use std::time::Instant;
use hgb_wasm_kernel::{
    cosine_similarity, dot_product, euclidean_distance, hgb_simd_dot_product_f32,
    hgb_simd_relu_f32, PluginManifest,
};

fn print_help() {
    println!("Hagibis WebAssembly Microkernel Native Runner (hgb-wasm-kernel)");
    println!("Usage: hgb-wasm-kernel [OPTIONS]");
    println!();
    println!("Options:");
    println!("  --bench, -b [ITERS] Run realistic SIMD & tensor linear algebra benchmarks");
    println!("  --manifest, -m      Output default WASM guest plugin manifest JSON");
    println!("  --simd-test, -t     Execute verification test for SIMD acceleration primitives");
    println!("  --status, -s        Display runtime kernel capabilities and architecture");
    println!("  --version, -v       Print version information");
    println!("  --help, -h          Show this help message");
}

fn print_version() {
    println!("hgb-wasm-kernel 0.1.0 (Hagibis Sovereign WASM Native Runner)");
}

fn print_manifest() {
    let manifest = PluginManifest::default();
    match serde_json::to_string_pretty(&manifest) {
        Ok(json) => println!("{}", json),
        Err(e) => eprintln!("Error serializing manifest: {}", e),
    }
}

fn run_simd_test() -> bool {
    println!("Running SIMD verification tests...");
    let a = vec![1.0f32, 2.0, 3.0, 4.0];
    let b = vec![1.0f32, 2.0, 3.0, 4.0];
    let dot = hgb_simd_dot_product_f32(&a, &b);
    let expected_dot = 1.0 + 4.0 + 9.0 + 16.0; // 30.0
    if (dot - expected_dot).abs() > 1e-4 {
        eprintln!("SIMD dot product mismatch: got {}, expected {}", dot, expected_dot);
        return false;
    }

    let relu_in = vec![-2.0f32, -1.0, 0.0, 1.0, 2.0];
    let relu_out = hgb_simd_relu_f32(&relu_in);
    if relu_out != vec![0.0f32, 0.0, 0.0, 1.0, 2.0] {
        eprintln!("SIMD relu mismatch: got {:?}", relu_out);
        return false;
    }

    let cos = cosine_similarity(&a, &b);
    if (cos - 1.0f32).abs() > 1e-4 {
        eprintln!("Cosine similarity mismatch: got {}", cos);
        return false;
    }

    let dist = euclidean_distance(&a, &b);
    if dist.abs() > 1e-4 {
        eprintln!("Euclidean distance mismatch: got {}", dist);
        return false;
    }

    let normal_dot = dot_product(&a, &b);
    if (normal_dot - expected_dot).abs() > 1e-4 {
        eprintln!("Scalar dot product mismatch: got {}, expected {}", normal_dot, expected_dot);
        return false;
    }

    println!("All SIMD tests passed successfully (✔).");
    true
}

fn run_benchmarks(iterations: usize) {
    println!("Running Hagibis WASM Kernel SIMD Benchmarks ({} iterations)...", iterations);
    let dim = 512;
    let mut vec_a = Vec::with_capacity(dim);
    let mut vec_b = Vec::with_capacity(dim);
    for i in 0..dim {
        vec_a.push((i as f32) * 0.01);
        vec_b.push(((dim - i) as f32) * 0.01);
    }

    let start = Instant::now();
    let mut acc_dot = 0.0f32;
    let mut acc_cos = 0.0f32;
    let mut acc_dist = 0.0f32;

    for _ in 0..iterations {
        acc_dot += hgb_simd_dot_product_f32(&vec_a, &vec_b);
        acc_cos += cosine_similarity(&vec_a, &vec_b);
        acc_dist += euclidean_distance(&vec_a, &vec_b);
        let _ = hgb_simd_relu_f32(&vec_a);
    }

    let elapsed = start.elapsed();
    let total_ops = iterations * 4;
    let ops_per_sec = if elapsed.as_secs_f64() > 0.0 {
        (total_ops as f64) / elapsed.as_secs_f64()
    } else {
        0.0
    };

    println!("Completed {} operations in {:.2?}", total_ops, elapsed);
    println!("Throughput: {:.2} ops/sec (acc: {:.2}, {:.2}, {:.2})", ops_per_sec, acc_dot, acc_cos, acc_dist);
}

fn print_status() {
    println!("Hagibis WASM Kernel Native Runner Status:");
    println!("  Target Architecture: {}", env::consts::ARCH);
    println!("  OS: {}", env::consts::OS);
    println!("  SIMD Acceleration: Available (AVX2/AVX-512/Neon/WASM-SIMD128)");
    println!("  Zero-Copy SHM Support: Enabled");
}

pub fn main() {
    let args: Vec<String> = env::args().collect();
    if args.len() <= 1 {
        // Default execution: run quick verification & warmup benchmark
        print_version();
        let _ = run_simd_test();
        run_benchmarks(2500);
        return;
    }

    if args.len() > 1 {
        match args[1].as_str() {
            "--help" | "-h" => {
                print_help();
                return;
            }
            "--version" | "-v" => {
                print_version();
                return;
            }
            "--manifest" | "-m" => {
                print_manifest();
                return;
            }
            "--simd-test" | "-t" => {
                let ok = run_simd_test();
                if !ok {
                    std::process::exit(1);
                }
                return;
            }
            "--bench" | "-b" => {
                let iters = args.get(2)
                    .and_then(|s| s.parse::<usize>().ok())
                    .unwrap_or(5000);
                run_benchmarks(iters);
                return;
            }
            "--status" | "-s" => {
                print_status();
                return;
            }
            unknown => {
                eprintln!("Unknown option: {}", unknown);
                print_help();
                std::process::exit(1);
            }
        }
    }
}
