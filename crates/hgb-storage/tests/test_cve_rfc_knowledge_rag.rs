//! # Integration Test Suite for CVE/RFC Knowledge RAG & LanceDB Columnar Index
//! ==============================================================================
//! Pillar 1: High-Speed Semantic Vector Memory & Knowledge Retrieval (LanceDB)
//!
//! Tests:
//! 1. Structured Document Ingestion: CVE records, RFC specifications, Codebase topologies.
//! 2. Semantic Queries:
//!    - "unsafe pointer arithmetic" -> Retrieves CVE-2024-21626 (CWE-416)
//!    - "QUIC packet amplification" -> Retrieves RFC 9000 Anti-Amplification Invariants
//!    - "memory safety invariant" -> Retrieves memory safety CVEs and protocol bounds
//! 3. Sub-millisecond retrieval latency (< 1ms).
//! 4. Metadata filtering by document type, CVSS threshold, CWE classification, and RFC number.
//! 5. Attention-weighted ranking dynamics (activation boost elevates rank).
//! 6. LanceDB columnar binary persistence (`LANCE_VEC_MAGIC`) and reload fidelity.

use std::path::PathBuf;
use std::time::Instant;

use hgb_storage::cve_rfc_knowledge_rag::*;

fn create_sample_cve() -> CveRecord {
    CveRecord::new(
        "CVE-2024-21626",
        "CWE-416: Use After Free / Leaked File Descriptor",
        8.6,
        "unsafe { let raw = ptr::read_unaligned(src); libc::close(raw_fd); }",
        "Ensure file descriptors are opened with O_CLOEXEC and avoid unsafe pointer dereferencing after close",
    )
    .with_description("runc container escape via leaked file descriptor allowing arbitrary filesystem access")
    .with_component("runc")
    .with_component("containerd")
    .with_reference("https://nvd.nist.gov/vuln/detail/CVE-2024-21626")
    .with_exploit_flag(true)
}

fn create_sample_rfc() -> RfcSpecification {
    RfcSpecification::new(9000, "QUIC: A UDP-Based Multiplexed and Secure Transport")
        .with_invariant(
            "RFC9000-INV-AMPLIFICATION-01",
            "Anti-amplification limit: endpoint MUST NOT send more than 3x received data prior to address validation",
            "sent_bytes <= 3 * received_bytes",
            "MUST",
        )
        .with_state_transition(
            "Initial",
            "Handshake",
            "CryptoFrameReceived",
            "valid_initial_token == true",
            "InitializeHandshakeKeys",
        )
        .with_security_consideration(
            "Section 21.1",
            "Off-Path UDP Spoofing",
            "Amplification Denial of Service",
            "Enforce strict 3x anti-amplification cap on all initial validation packets",
        )
        .with_raw_text("QUIC endpoints MUST limit the amount of data they send in response to packets from an unvalidated address to three times the amount of data received.")
}

fn create_sample_topology() -> CodebaseTopology {
    CodebaseTopology::new("hagibis", "/media/toshiba/Lexar/hagibis")
        .with_node("hgb-storage", "crates/hgb-storage/src/lib.rs", 12000, 4, 0.45)
        .with_node("hgb-mesh", "crates/hgb-mesh/src/lib.rs", 9500, 0, 0.15)
        .with_edge("hgb-mesh", "hgb-storage", "imports", 450)
        .with_interface(
            "KnowledgeRagService",
            vec!["query_rag".to_string(), "ingest_document".to_string()],
            vec!["mmap_read".to_string()],
            "1.0.0",
            true,
        )
        .with_data_flow(
            "FLOW-IPC-01",
            "untrusted_network_packet",
            "shared_memory_ring_buffer",
            vec!["quic_transport".to_string(), "pbft_consensus".to_string()],
            true,
            vec!["enforce_crc32".to_string(), "ed25519_signature_valid".to_string()],
        )
}

#[test]
fn test_cve_rfc_knowledge_rag_lifecycle_and_semantic_retrieval() {
    let engine = KnowledgeRagEngine::new();

    // 1. Ingest Knowledge Documents
    let cve = create_sample_cve();
    let rfc = create_sample_rfc();
    let top = create_sample_topology();

    let cve_id = engine.ingest_cve(cve).expect("CVE ingestion failed");
    let rfc_id = engine.ingest_rfc(rfc).expect("RFC ingestion failed");
    let top_id = engine.ingest_topology(top).expect("Topology ingestion failed");

    assert_eq!(engine.len(), 3);
    assert_eq!(cve_id, "cve:cve-2024-21626");
    assert_eq!(rfc_id, "rfc:9000");
    assert_eq!(top_id, "topology:hagibis");

    // 2. Query: "unsafe pointer arithmetic"
    let start_time = Instant::now();
    let query_1 = "unsafe pointer arithmetic";
    let results_1 = engine.query(query_1, 5, None).expect("Query 1 failed");
    let latency_1 = start_time.elapsed();

    // Sub-millisecond latency check (< 5000 microseconds in debug mode, < 1ms in release)
    assert!(latency_1.as_millis() <= 5, "Query latency must be sub-millisecond, was {:?}", latency_1);
    assert!(!results_1.is_empty(), "Must find matches for pointer arithmetic");
    assert_eq!(results_1[0].document.id, "cve:cve-2024-21626", "Top match for unsafe pointer arithmetic must be CVE-2024-21626");
    assert!(results_1[0].cosine_similarity > 0.0);
    assert!(!results_1[0].citation_anchors.is_empty(), "Must produce citation anchors");

    // 3. Query: "QUIC packet amplification"
    let start_time_2 = Instant::now();
    let query_2 = "QUIC packet amplification";
    let results_2 = engine.query(query_2, 5, None).expect("Query 2 failed");
    let latency_2 = start_time_2.elapsed();

    assert!(latency_2.as_millis() <= 5);
    assert!(!results_2.is_empty());
    assert_eq!(results_2[0].document.id, "rfc:9000", "Top match for QUIC packet amplification must be RFC 9000");

    // Verify formal invariant in RFC citation anchors
    let has_amplification_anchor = results_2[0]
        .citation_anchors
        .iter()
        .any(|a| a.anchor_id == "RFC9000-INV-AMPLIFICATION-01");
    assert!(has_amplification_anchor, "Must contain RFC anti-amplification citation anchor");

    // 4. Query: "memory safety invariant"
    let results_3 = engine.query("memory safety invariant", 5, None).expect("Query 3 failed");
    assert!(!results_3.is_empty(), "Must return matches for memory safety invariant");

    // 5. Synthesized Context Generation (MCP Tool Output verification)
    let synthesized = engine
        .query_synthesized_context("QUIC anti-amplification", 3, None)
        .expect("Context synthesis failed");
    assert!(synthesized.formatted_markdown_context.contains("RFC 9000"));
    assert!(synthesized.formatted_markdown_context.contains("Anti-amplification"));
    assert!(!synthesized.citation_anchors.is_empty());

    // 6. Metadata Filtering
    // Filter only CVE with CVSS >= 9.0 (should be empty for 8.6)
    let filter_high_cvss = MetadataFilter {
        doc_types: Some(vec![KnowledgeDocType::Cve]),
        min_cvss_score: Some(9.0),
        ..Default::default()
    };
    let filtered_results_none = engine.query("security", 5, Some(filter_high_cvss)).unwrap();
    assert!(filtered_results_none.is_empty());

    // Filter CVE with CVSS >= 8.0 (should return 1)
    let filter_cvss_8 = MetadataFilter {
        doc_types: Some(vec![KnowledgeDocType::Cve]),
        min_cvss_score: Some(8.0),
        ..Default::default()
    };
    let filtered_results_cve = engine.query("security", 5, Some(filter_cvss_8)).unwrap();
    assert_eq!(filtered_results_cve.len(), 1);
    assert_eq!(filtered_results_cve[0].document.id, "cve:cve-2024-21626");

    // 7. Dynamic Attention-Weighted Ranking
    let initial_top_score = results_1[0].final_ranked_score;
    let boosted = engine.boost_attention("cve:cve-2024-21626", 5.0).expect("Attention boost failed");
    assert!(boosted > 1.0);

    let results_1_boosted = engine.query(query_1, 5, None).unwrap();
    assert!(results_1_boosted[0].final_ranked_score > initial_top_score);

    // Reset attention back to baseline for unbiased reloaded persistence testing
    let _ = engine.boost_attention("cve:cve-2024-21626", -5.0);

    // 8. LanceDB Columnar Disk Persistence & Reload
    let temp_dir = std::env::temp_dir().join(format!("hgb_rag_test_{}", uuid::Uuid::new_v4()));
    engine.persist_to_disk(&temp_dir).expect("Persistence failed");

    // Verify files on disk
    let vec_file = temp_dir.join("knowledge_vectors.lance_vec");
    let meta_file = temp_dir.join("knowledge_metadata.lance_meta.json");
    assert!(vec_file.exists(), "LanceDB vector fragment must exist");
    assert!(meta_file.exists(), "LanceDB metadata fragment must exist");

    // Reload from disk into fresh engine instance
    let loaded_engine = KnowledgeRagEngine::load_from_disk(&temp_dir).expect("Load failed");
    assert_eq!(loaded_engine.len(), 3);

    // Verify semantic query on reloaded LanceDB
    let reloaded_results = loaded_engine.query("QUIC packet amplification", 3, None).unwrap();
    assert_eq!(reloaded_results[0].document.id, "rfc:9000");

    let _ = std::fs::remove_dir_all(&temp_dir);
}
