//! Comprehensive Test Suite: Pillar 4 Full-Codebase Vector RAG & Refactoring (LanceDB)
//! =========================================================================

use std::fs;
use std::path::Path;
use tempfile::tempdir;

use hgb_storage::codebase_vector_rag::{
    CodeChunkKind, CodebaseChunker, CodebaseVectorEmbedder, CodebaseVectorRagEngine,
    RefactoringCategory, RefactoringSeverity, CODEBASE_VECTOR_DIM,
};

#[test]
fn test_codebase_chunker_rust() {
    let rust_code = r#"
/// Adaptive PID Controller for system load balancing
pub struct AdaptivePidController {
    kp: f32,
    ki: f32,
    kd: f32,
}

impl AdaptivePidController {
    /// Create a new controller
    pub fn new(kp: f32, ki: f32, kd: f32) -> Self {
        Self { kp, ki, kd }
    }

    /// Compute control output
    pub fn compute(&self, error: f32) -> f32 {
        self.kp * error
    }
}

pub fn standalone_helper() -> bool {
    unsafe {
        // Fast pointer dereference
        let ptr = std::ptr::null::<u8>();
        let _ = ptr;
    }
    true
}
"#;

    let chunks = CodebaseChunker::chunk_file("crates/hgb-core/src/pid.rs", rust_code);
    assert!(!chunks.is_empty(), "Expected chunks to be extracted from Rust code");

    // Check struct chunk
    let struct_chunk = chunks.iter().find(|c| c.kind == CodeChunkKind::Struct);
    assert!(struct_chunk.is_some(), "Expected Struct chunk");
    let sc = struct_chunk.unwrap();
    assert_eq!(sc.symbol_name, "AdaptivePidController");
    assert!(sc.start_line > 0 && sc.end_line >= sc.start_line);
    assert!(!sc.blake3_hash.is_empty());
    assert_eq!(sc.partition, "crates");
    assert_eq!(sc.language, "rust");

    // Check function chunks
    let fn_chunks: Vec<_> = chunks.iter().filter(|c| c.kind == CodeChunkKind::Function).collect();
    assert!(fn_chunks.len() >= 2, "Expected at least 2 functions (new, compute, standalone_helper)");

    // Check unsafe detection
    let unsafe_fn = chunks.iter().find(|c| c.symbol_name == "standalone_helper");
    assert!(unsafe_fn.is_some(), "Expected standalone_helper function");
    assert!(unsafe_fn.unwrap().has_unsafe, "Expected has_unsafe to be true for standalone_helper");
}

#[test]
fn test_codebase_chunker_polyglot() {
    // 1. Python chunking
    let py_code = r#"
class CircuitBreakerManager:
    def __init__(self, threshold=5):
        self.threshold = threshold

    def record_failure(self):
        self.threshold -= 1
        return self.threshold <= 0

def calculate_backoff(retries):
    return 2 ** retries
"#;
    let py_chunks = CodebaseChunker::chunk_file("scripts/manager.py", py_code);
    assert!(!py_chunks.is_empty());
    assert!(py_chunks.iter().any(|c| c.kind == CodeChunkKind::Class && c.symbol_name == "CircuitBreakerManager"));
    assert!(py_chunks.iter().any(|c| c.kind == CodeChunkKind::Function && c.symbol_name == "calculate_backoff"));
    assert_eq!(py_chunks[0].partition, "scripts");

    // 2. Shell script chunking
    let sh_code = r#"
#!/usr/bin/env bash
setup_firecracker_jail() {
    echo "Configuring volatile jail in /dev/shm..."
    mkdir -p /dev/shm/test_jail
}

function run_security_fuzz() {
    echo "Running AFL/libFuzzer in sandbox..."
}
"#;
    let sh_chunks = CodebaseChunker::chunk_file("scripts/microvm_setup.sh", sh_code);
    assert!(!sh_chunks.is_empty());
    assert!(sh_chunks.iter().any(|c| c.kind == CodeChunkKind::ShellFunction && c.symbol_name == "setup_firecracker_jail"));
    assert!(sh_chunks.iter().any(|c| c.kind == CodeChunkKind::ShellFunction && c.symbol_name == "run_security_fuzz"));

    // 3. Markdown chunking
    let md_content = r#"
# Firecracker MicroVM Architecture
Overview of hardware virtualization.

## KVM and cgroups v2
Details on CPU and memory limits.

## Landlock LSM Fallback
Unprivileged Linux sandbox guarantees.
"#;
    let md_chunks = CodebaseChunker::chunk_file("docs/architecture.md", md_content);
    assert_eq!(md_chunks.len(), 3);
    assert_eq!(md_chunks[0].symbol_name, "Firecracker MicroVM Architecture");
    assert_eq!(md_chunks[1].symbol_name, "KVM and cgroups v2");
    assert_eq!(md_chunks[2].symbol_name, "Landlock LSM Fallback");
    assert_eq!(md_chunks[0].partition, "docs");
}

#[test]
fn test_codebase_vector_embedder() {
    let embedder = CodebaseVectorEmbedder::new(CODEBASE_VECTOR_DIM);

    let v1 = embedder.embed("Where are circuit breakers defined in hagibis?");
    assert_eq!(v1.len(), CODEBASE_VECTOR_DIM);

    // Verify L2 unit normalization (norm ≈ 1.0)
    let norm1: f32 = v1.iter().map(|x| x * x).sum::<f32>().sqrt();
    assert!((norm1 - 1.0).abs() < 1e-4, "Expected vector to be unit-normalized, got {}", norm1);

    let v2 = embedder.embed("Adaptive PID Circuit Breaker trip and half-open failure threshold");
    let v3 = embedder.embed("PostgreSQL relational schema migration table indexes");

    // Cosine similarity between circuit breaker queries should be significantly higher than unrelated queries
    let dot1_2: f32 = v1.iter().zip(v2.iter()).map(|(a, b)| a * b).sum();
    let dot1_3: f32 = v1.iter().zip(v3.iter()).map(|(a, b)| a * b).sum();

    assert!(
        dot1_2 > dot1_3,
        "Circuit breaker query should match circuit breaker code (sim: {}) closer than unrelated (sim: {})",
        dot1_2,
        dot1_3
    );
}

#[test]
fn test_codebase_indexing_and_semantic_retrieval() {
    let temp = tempdir().expect("tempdir");
    let root = temp.path();

    // Create realistic codebase subdirectories
    let crates_dir = root.join("crates/hgb-core/src");
    let scripts_dir = root.join("scripts");
    let skills_dir = root.join("skills/microvm");
    let docs_dir = root.join("docs");

    fs::create_dir_all(&crates_dir).unwrap();
    fs::create_dir_all(&scripts_dir).unwrap();
    fs::create_dir_all(&skills_dir).unwrap();
    fs::create_dir_all(&docs_dir).unwrap();

    // 1. Circuit breaker Rust file
    fs::write(
        crates_dir.join("circuit_breaker.rs"),
        r#"
//! # Adaptive PID Circuit Breaker
//! Automatic tripping and backoff under high error rates.

pub struct AdaptivePidCircuitBreaker {
    pub failure_threshold: u32,
    pub trip_state: bool,
}

impl AdaptivePidCircuitBreaker {
    pub fn new() -> Self {
        Self { failure_threshold: 5, trip_state: false }
    }

    pub fn record_success(&mut self) {
        self.trip_state = false;
    }
}
"#,
    ).unwrap();

    // 2. Unsafe memory and race condition Rust file
    fs::write(
        crates_dir.join("memory_unsafe.rs"),
        r#"
//! # Memory Mapped Unsafe Operations
pub struct MmapBuffer {
    ptr: *mut u8,
    len: usize,
}

impl MmapBuffer {
    pub fn read_byte(&self, offset: usize) -> u8 {
        unsafe {
            // Memory leak and race condition risk on raw pointer
            *self.ptr.add(offset)
        }
    }
}
"#,
    ).unwrap();

    // 3. Documentation file
    fs::write(
        docs_dir.join("firecracker_microvm.md"),
        r#"
# Firecracker MicroVM Hardware Virtualization
This document explains how Firecracker microVMs are spawned using `/dev/kvm`.
Hardware isolation guarantees zero leakage into host directories.

## Landlock LSM Unprivileged Containment
When KVM is unavailable, Landlock LSM enforces safe volatile RAM CoW overlays in `/dev/shm`.
"#,
    ).unwrap();

    // 4. Shell script file
    fs::write(
        scripts_dir.join("fuzz_runner.sh"),
        r#"
#!/usr/bin/env bash
run_fuzzing_campaign() {
    echo "Starting coverage-guided security fuzzing inside ephemeral microVM sandbox..."
}
"#,
    ).unwrap();

    // Initialize LanceDB vector engine in test directory
    let lance_storage = root.join(".hgb_test");
    let engine = CodebaseVectorRagEngine::open_or_create(&lance_storage, CODEBASE_VECTOR_DIM)
        .expect("engine initialization");

    // Index workspace
    let report = engine.index_workspace(root, true).expect("workspace indexing");
    assert!(report.total_files_scanned >= 4, "Expected at least 4 files scanned");
    assert!(report.total_chunks_indexed >= 4, "Expected at least 4 chunks indexed");
    assert!(report.total_unsafe_blocks >= 1, "Expected unsafe block in memory_unsafe.rs");

    // Search 1: Circuit breaker
    let cb_hits = engine.search("Where are circuit breakers defined?", 5).expect("search");
    assert!(!cb_hits.is_empty(), "Expected search results for circuit breaker");
    let top_cb = &cb_hits[0];
    assert!(
        top_cb.file_path.contains("circuit_breaker.rs") || top_cb.symbol_name.contains("CircuitBreaker"),
        "Top hit should be circuit breaker, got file: {}, symbol: {}",
        top_cb.file_path,
        top_cb.symbol_name
    );
    assert!(top_cb.score > 0.0);

    // Search 2: Memory leak & race conditions
    let mem_hits = engine.search("Locate memory leaks and race conditions", 5).expect("search");
    assert!(!mem_hits.is_empty(), "Expected search results for memory leak");
    let top_mem = &mem_hits[0];
    assert!(
        top_mem.file_path.contains("memory_unsafe.rs") || top_mem.has_unsafe,
        "Top hit should point to memory_unsafe or unsafe block, got file: {}",
        top_mem.file_path
    );

    // Search 3: Firecracker microVM isolation
    let vm_hits = engine.search("Firecracker microVM isolation", 5).expect("search");
    assert!(!vm_hits.is_empty(), "Expected search results for microvm");
    let top_vm = &vm_hits[0];
    assert!(
        top_vm.file_path.contains("firecracker_microvm.md") || top_vm.symbol_name.contains("Firecracker"),
        "Top hit should point to Firecracker doc, got file: {}",
        top_vm.file_path
    );
}

#[test]
fn test_autonomous_refactoring_audit() {
    let temp = tempdir().expect("tempdir");
    let root = temp.path();

    let crates_dir = root.join("crates/test-crate/src");
    fs::create_dir_all(&crates_dir).unwrap();

    // Create file with undocumented unsafe block
    fs::write(
        crates_dir.join("vulnerable.rs"),
        r#"
pub fn unprotected_deref(raw: *const u32) -> u32 {
    unsafe {
        *raw
    }
}
"#,
    ).unwrap();

    let lance_storage = root.join(".hgb_refactor_test");
    let engine = CodebaseVectorRagEngine::open_or_create(&lance_storage, CODEBASE_VECTOR_DIM).unwrap();
    let _ = engine.index_workspace(root, true).unwrap();

    let audit_rep = engine.autonomous_refactor_audit(None).expect("refactor audit");
    assert!(audit_rep.total_chunks_analyzed > 0);

    // Should flag the undocumented unsafe block
    let unsafe_prop = audit_rep.proposals.iter().find(|p| p.category == RefactoringCategory::SecurityVulnerability);
    assert!(unsafe_prop.is_some(), "Expected SecurityVulnerability proposal for undocumented unsafe");
    let prop = unsafe_prop.unwrap();
    assert_eq!(prop.severity, RefactoringSeverity::High);
    assert!(prop.diagnosis.contains("unsafe code"));
    assert!(!prop.prescriptive_patch.is_empty());
}
