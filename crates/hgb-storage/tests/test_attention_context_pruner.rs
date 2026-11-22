//! # `test_attention_context_pruner.rs` - Integration Test Suite for Dynamic Context Pruning
//! ==========================================================================================
//! Pillar 3: Dynamic Multi-Modal Context Pruning Engine Verification
//!
//! Comprehensive, zero-mock integration test suite testing:
//! 1. Multi-Modal Context Ingestion (ConversationTurn, AstSyntaxFragment, ToolReceipt, TerminalLog, VectorEmbeddingChunk)
//! 2. Attention Scoring & Temporal Decay Dynamics ($W(t) = importance \times e^{-\lambda \Delta t} \times multiplier \times (1 + \alpha \cdot deps)$)
//! 3. 3-Tier Working Memory Hierarchy (L1 Active Window -> L2 Semantic Summary Cache -> L3 Cold Persistent Store)
//! 4. Causal Dependency Invariant Protection (ancestors of active L1 items never evicted to L3)
//! 5. Token Budget Adherence & Automated Garbage Collection Compaction Telemetry
//! 6. Persistent File-Backed SQLite L3 Store with Zstandard Compression and Indexed Recall
//! 7. Multi-Threaded Concurrency, State Consistency, and Thread Safety

#[path = "../src/attention_context_pruner.rs"]
mod attention_context_pruner;

use std::sync::Arc;
use std::thread;
use std::time::Duration;

use attention_context_pruner::*;
use serde_json::json;

// ============================================================================
// 1. Test Multi-Modal Context Ingestion
// ============================================================================

#[test]
fn test_multi_modal_context_ingestion() {
    let config = ContextPrunerConfig {
        max_token_budget: 10_000,
        auto_prune_on_ingest: false,
        ..Default::default()
    };
    let pruner = DynamicAttentionContextPruner::new(config).expect("Pruner initialization failed");

    // 1. ConversationTurn (User & System)
    let sys_item = ContextItem::new_conversation(
        "turn-sys-1",
        ConversationRole::System,
        "ArchitectGovernor",
        "Maintain zero-cost abstractions and sub-millisecond query latency across all memory tiers.",
        1.0,
        vec![],
    )
    .with_metadata("phase", "bootstrap");

    let user_item = ContextItem::new_conversation(
        "turn-user-1",
        ConversationRole::User,
        "PrincipalDev",
        "Execute distributed consensus check across edge MicroVM instances.",
        0.85,
        vec!["turn-sys-1".to_string()],
    );

    // 2. AstSyntaxFragment (active anchor)
    let ast_anchor = ContextItem::new_ast_fragment(
        "ast-anchor-1",
        "crates/hgb-core/src/consensus.rs",
        "rust",
        "pub struct RaftLeaderNode {\n    pub term: u64,\n    pub state: NodeState,\n}\nimpl RaftLeaderNode {\n    pub fn heartbeat(&mut self) -> bool { true }\n}",
        "struct",
        true, // is_anchor
        0.95,
        vec!["turn-user-1".to_string()],
    );

    // 3. ToolReceipt
    let tool_receipt = ContextItem::new_tool_receipt(
        "tool-rcpt-1",
        "microvm_exec",
        "call-7788",
        json!({ "node_id": "replica-node-alpha", "timeout_ms": 500 }),
        "Execution successful: raft term 42 committed, sync delta applied.",
        ToolExecutionStatus::Success,
        0.80,
        vec!["ast-anchor-1".to_string()],
    );

    // 4. TerminalLog (stdout/stderr)
    let term_log = ContextItem::new_terminal_log(
        "term-log-1",
        TerminalStream::Stdout,
        "cargo check --workspace",
        Some(0),
        "   Compiling hgb-storage v0.1.0\n    Finished dev [unoptimized + debuginfo] target(s) in 0.42s\n",
        0.50,
        vec!["tool-rcpt-1".to_string()],
    );

    // 5. VectorEmbeddingChunk
    let fake_vector = vec![0.15f32; 64];
    let vec_chunk = ContextItem::new_vector_chunk(
        "vec-chunk-1",
        "doc-spec-crdt-v2",
        "LWW-Register conflict resolution leverages monotonic physical clocks and Lamport vector registers.",
        fake_vector,
        0.92,
        0.75,
        vec![],
    );

    // Ingest all items
    pruner.ingest(sys_item.clone()).unwrap();
    pruner.ingest(user_item.clone()).unwrap();
    pruner.ingest(ast_anchor.clone()).unwrap();
    pruner.ingest(tool_receipt.clone()).unwrap();
    pruner.ingest(term_log.clone()).unwrap();
    pruner.ingest(vec_chunk.clone()).unwrap();

    let metrics = pruner.metrics();
    assert_eq!(metrics.l1_items_count, 6);
    assert_eq!(metrics.l2_items_count, 0);
    assert_eq!(metrics.l3_items_count, 0);
    assert!(metrics.total_active_tokens > 0);

    // Verify item modalities and retrieve fidelity
    let (retrieved, tier) = pruner.get_item("ast-anchor-1").unwrap().expect("ast-anchor-1 found");
    assert_eq!(tier, MemoryTier::L1Active);
    assert_eq!(retrieved.modality(), ModalityKind::AstSyntaxFragment);
    if let ContextPayload::AstFragment(ast) = retrieved.payload {
        assert!(ast.is_anchor);
        assert_eq!(ast.node_type, "struct");
    } else {
        panic!("Expected AstFragment payload");
    }

    // Verify metadata preserved
    let (sys_ret, _) = pruner.get_item("turn-sys-1").unwrap().expect("sys turn found");
    assert_eq!(sys_ret.metadata.get("phase"), Some(&"bootstrap".to_string()));
}

// ============================================================================
// 2. Test Attention Scoring & Temporal Decay Dynamics
// ============================================================================

#[test]
fn test_attention_decay_scoring_dynamics() {
    let config = AttentionConfig::default();
    let base_ts = 1_700_000_000_000u64;

    // Create 3 items with different modalities and priorities
    let sys_item = ContextItem::new_conversation(
        "sys-inst",
        ConversationRole::System,
        "System",
        "Core agent execution invariant: never drop uncommitted transactions.",
        1.0,
        vec![],
    )
    .with_timestamp_ms(base_ts);

    let ast_anchor = ContextItem::new_ast_fragment(
        "anchor-main",
        "src/main.rs",
        "rust",
        "pub fn main() { bootstrap_storage(); }",
        "fn",
        true,
        1.0,
        vec![],
    )
    .with_timestamp_ms(base_ts);

    let terminal_log = ContextItem::new_terminal_log(
        "term-build",
        TerminalStream::Stderr,
        "cargo test",
        Some(101),
        "test failed in sub-crate",
        1.0,
        vec![],
    )
    .with_timestamp_ms(base_ts);

    // A: Initial scores at t = 0 (base_ts)
    let score_sys_0 = config.compute_attention(&sys_item, base_ts, 0);
    let score_ast_0 = config.compute_attention(&ast_anchor, base_ts, 0);
    let score_term_0 = config.compute_attention(&terminal_log, base_ts, 0);

    assert!(score_sys_0 > score_ast_0, "System instructions have highest multiplier");
    assert!(score_ast_0 > score_term_0, "AST anchor outranks transient terminal log");

    // B: Scores after 300 seconds of elapsed time (300,000 ms)
    let eval_ts = base_ts + 300_000;
    let score_sys_300 = config.compute_attention(&sys_item, eval_ts, 0);
    let score_ast_300 = config.compute_attention(&ast_anchor, eval_ts, 0);
    let score_term_300 = config.compute_attention(&terminal_log, eval_ts, 0);

    // Verify decay is strictly monotonic
    assert!(score_sys_300 < score_sys_0);
    assert!(score_ast_300 < score_ast_0);
    assert!(score_term_300 < score_term_0);

    // Ratio of retention: sys and ast_anchor must retain vastly more attention than transient logs
    let retention_sys = score_sys_300 / score_sys_0;
    let retention_ast = score_ast_300 / score_ast_0;
    let retention_term = score_term_300 / score_term_0;

    assert!(
        retention_sys > 0.95,
        "System instruction should have minimal decay after 300s, retention={}",
        retention_sys
    );
    assert!(
        retention_ast > 0.95,
        "AST anchor should have minimal decay after 300s, retention={}",
        retention_ast
    );
    assert!(
        retention_term < 0.05,
        "Transient terminal log should decay rapidly after 300s, retention={}",
        retention_term
    );
    assert!(
        retention_ast > retention_term * 15.0,
        "AST anchor must retain at least 15x more attention than compiler log"
    );

    // C: Downstream causal dependency boost
    // (1.0 + alpha * downstream_dependencies)
    let score_sys_with_deps = config.compute_attention(&sys_item, eval_ts, 4);
    let expected_boost = 1.0 + config.alpha * 4.0;
    let ratio = score_sys_with_deps / score_sys_300;
    assert!(
        (ratio - expected_boost).abs() < 1e-4,
        "Downstream dependency boost ratio must match (1 + alpha * deps)"
    );
}

// ============================================================================
// 3. Test Token Budget Adherence & Automated Garbage Collection
// ============================================================================

#[test]
fn test_token_budget_adherence_and_auto_gc() {
    // Set a strict token budget: 200 tokens total, L1 budget ratio = 0.5 (100 tokens max for L1)
    let config = ContextPrunerConfig {
        max_token_budget: 200,
        l1_budget_ratio: 0.50,
        auto_prune_on_ingest: true,
        protect_causal_dependencies: true,
        min_attention_l1: 0.1,
        min_attention_l2: 0.01,
        ..Default::default()
    };
    let pruner = DynamicAttentionContextPruner::new(config).expect("Pruner init");

    // Ingest 6 large items, each ~60-80 tokens, totaling ~400+ tokens (well over 200 token budget)
    for i in 1..=6 {
        let text = format!(
            "Operational turn {}: Executing comprehensive verification suite across modules with transaction logs, diagnostic spans, and AST integrity assertions for resilient microvm deployment.",
            i
        );
        let item = ContextItem::new_conversation(
            format!("heavy-turn-{}", i),
            ConversationRole::User,
            "Developer",
            text,
            0.5 + (i as f64 * 0.08), // Later items have higher importance
            vec![],
        )
        .with_timestamp_ms(1_000_000 + i * 50_000);

        pruner.ingest(item).unwrap();
    }

    let metrics = pruner.metrics();

    // 1. Invariant: Total active tokens must strictly adhere to the configured budget!
    assert!(
        metrics.total_active_tokens <= 200,
        "Total active tokens ({}) must not exceed max budget (200)",
        metrics.total_active_tokens
    );

    // 2. Invariant: Active working memory items are partitioned into L1 and L2, with low-attention evicted to L3
    assert!(
        metrics.l1_token_count <= 120, // Approx 100 token L1 budget with single-item boundary
        "L1 tokens ({}) should stay near L1 budget",
        metrics.l1_token_count
    );
    assert!(metrics.l2_items_count > 0, "Some items must be compacted into L2 summaries");
    assert!(metrics.l3_items_count > 0, "Excess items must be evicted into L3 cold store");

    // 3. Prompt assembly generates coherent, budget-compliant context
    let assembled = pruner.assemble_prompt();
    assert!(assembled.prompt_text.contains("# AGENT WORKING MEMORY CONTEXT"));
    assert!(assembled.prompt_text.contains("## L1 Active Working Memory Window"));
    assert!(assembled.prompt_text.contains("## L2 Semantic Summary Cache"));
    assert!(assembled.metrics.total_active_tokens <= 200);
}

// ============================================================================
// 4. Test Causal Dependency Protection Invariant
// ============================================================================

#[test]
fn test_causal_dependency_protection() {
    // Configure pruner with strict 300 token budget
    let config = ContextPrunerConfig {
        max_token_budget: 300,
        l1_budget_ratio: 0.60,
        auto_prune_on_ingest: false, // Explicit manual pruning for deterministic checking
        protect_causal_dependencies: true,
        min_attention_l2: 0.25,
        ..Default::default()
    };
    let pruner = DynamicAttentionContextPruner::new(config).expect("Pruner init");

    let t0 = 1_000_000u64;

    // Item A: Old item with low intrinsic importance (candidate for eviction)
    let parent_item = ContextItem::new_ast_fragment(
        "parent-ast-core",
        "src/core.rs",
        "rust",
        "pub struct CoreState { pub epoch: u64, pub peers: Vec<String> }",
        "struct",
        false,
        0.20, // Low intrinsic importance
        vec![],
    )
    .with_timestamp_ms(t0);

    // Item B: Fresh item with high intrinsic importance, causally dependent on Item A
    let child_item = ContextItem::new_tool_receipt(
        "child-tool-validator",
        "raft_sync",
        "call-9911",
        json!({ "epoch": 10 }),
        "Validation successful for CoreState replication across all peers.",
        ToolExecutionStatus::Success,
        0.98, // High intrinsic importance
        vec!["parent-ast-core".to_string()], // Explicit causal parent link!
    )
    .with_timestamp_ms(t0 + 100_000);

    // Item C, D, E, F: Several other heavy items without dependencies
    pruner.ingest(parent_item).unwrap();
    pruner.ingest(child_item).unwrap();

    for i in 1..=5 {
        let noise_item = ContextItem::new_terminal_log(
            format!("noise-term-{}", i),
            TerminalStream::Stderr,
            format!("build-step-{}", i),
            Some(1),
            format!(
                "Compiler diagnostic warning line {}: unused parameter in non-critical auxiliary crate test harness.",
                i
            ),
            0.60,
            vec![],
        )
        .with_timestamp_ms(t0 + 150_000);
        pruner.ingest(noise_item).unwrap();
    }

    // Trigger pruning at t0 + 200_000 ms
    let report = pruner.prune_at_timestamp(t0 + 200_000).unwrap();

    assert!(report.evicted_to_l3_count > 0, "Should have evicted items to L3");

    // CRITICAL CAUSAL INVARIANT VERIFICATION:
    // Even though parent-ast-core had low intrinsic importance and older timestamp,
    // because child_item is retained in active L1 memory and depends on it,
    // parent-ast-core MUST NOT be evicted to L3!
    let parent_check = pruner.get_item("parent-ast-core").unwrap();
    assert!(
        parent_check.is_some(),
        "parent-ast-core must remain accessible in working memory"
    );
    let (_, tier) = parent_check.unwrap();
    assert_ne!(
        tier,
        MemoryTier::L3ColdPersistent,
        "parent-ast-core was causal-protected and MUST NOT be evicted to L3!"
    );

    // Child item must be in L1
    let child_check = pruner.get_item("child-tool-validator").unwrap();
    assert!(child_check.is_some());
    assert_eq!(child_check.unwrap().1, MemoryTier::L1Active);
}

// ============================================================================
// 5. Test 3-Tier Working Memory Hierarchy (L1 -> L2 -> L3 -> Reconstitute)
// ============================================================================

#[test]
fn test_3_tier_working_memory_hierarchy_and_reconstitution() {
    let config = ContextPrunerConfig {
        max_token_budget: 120, // very small budget to force rapid tiering
        l1_budget_ratio: 0.50,
        auto_prune_on_ingest: false,
        protect_causal_dependencies: false, // allow eviction to test L3
        min_attention_l2: 0.16,
        ..Default::default()
    };
    let pruner = DynamicAttentionContextPruner::new(config).expect("Pruner init");

    let t0 = 1_000_000u64;

    // Item 1: Large code fragment
    let item1 = ContextItem::new_ast_fragment(
        "tier-item-1",
        "src/kernel.rs",
        "rust",
        "pub fn execute_kernel(args: &Args) -> Result<()> {\n    let ctx = init_context()?;\n    ctx.run_pipeline()?;\n    Ok(())\n}",
        "fn",
        false,
        0.3,
        vec![],
    )
    .with_timestamp_ms(t0);

    // Ingest: starts in L1 Active
    pruner.ingest(item1.clone()).unwrap();
    let (_, tier1) = pruner.get_item("tier-item-1").unwrap().unwrap();
    assert_eq!(tier1, MemoryTier::L1Active);

    // Ingest Item 2 (High importance)
    let item2 = ContextItem::new_conversation(
        "tier-item-2",
        ConversationRole::System,
        "System",
        "Active high-priority prompt directive for immediate reasoning pass.",
        1.0,
        vec![],
    )
    .with_timestamp_ms(t0 + 10_000);
    pruner.ingest(item2).unwrap();

    // Ingest Item 3 (Large high-priority tool receipt)
    let item3 = ContextItem::new_tool_receipt(
        "tier-item-3",
        "synapse_query",
        "call-100",
        json!({ "query": "select * from nodes" }),
        "Returned 10,000 nodes with full metadata payload across distributed shard cluster.",
        ToolExecutionStatus::Success,
        0.95,
        vec![],
    )
    .with_timestamp_ms(t0 + 490_000);
    pruner.ingest(item3).unwrap();

    // First pruning pass: Item 1 should demote to L2 (Semantic Summary)
    pruner.prune_at_timestamp(t0 + 30_000).unwrap();
    let (_, tier_after_prune1) = pruner.get_item("tier-item-1").unwrap().unwrap();
    assert_eq!(
        tier_after_prune1,
        MemoryTier::L2SemanticSummary,
        "Item 1 should be demoted to L2"
    );

    // Ingest Item 4 (Another huge item) and prune with severe time decay on Item 1
    let item4 = ContextItem::new_terminal_log(
        "tier-item-4",
        TerminalStream::Stdout,
        "cargo run",
        Some(0),
        "Running comprehensive stress benchmark pass 1000 iterations without allocation failures.",
        0.90,
        vec![],
    )
    .with_timestamp_ms(t0 + 495_000);
    pruner.ingest(item4).unwrap();

    // Second pruning pass at t0 + 500_000 ms: Item 1 attention decayed to near 0, should evict to L3
    pruner.prune_at_timestamp(t0 + 500_000).unwrap();
    let item1_lookup = pruner.get_item("tier-item-1").unwrap().unwrap();
    assert_eq!(
        item1_lookup.1,
        MemoryTier::L3ColdPersistent,
        "Item 1 must be evicted to L3 cold store"
    );

    // Verify L3 storage count
    assert_eq!(pruner.l3_store().count().unwrap(), 1);

    // Reconstitute Item 1 from L3 back into L1 Active Memory!
    let reconstituted = pruner.reconstitute_to_l1("tier-item-1").unwrap();
    assert!(reconstituted, "Reconstitution must succeed");

    // Item 1 is now back in L1 with original full content preserved!
    let (restored, tier_restored) = pruner.get_item("tier-item-1").unwrap().unwrap();
    assert_eq!(tier_restored, MemoryTier::L1Active);
    assert_eq!(restored.raw_content, item1.raw_content);
    assert_eq!(pruner.l3_store().count().unwrap(), 0, "L3 entry deleted on restoration");
}

// ============================================================================
// 6. Test Persistent File-Backed SQLite L3 Store with Zstandard Compression
// ============================================================================

#[test]
fn test_persistent_file_backed_l3_sqlite_store() {
    let temp_dir = tempfile::tempdir().expect("tempdir creation failed");
    let db_path = temp_dir.path().join("l3_cold_store.db");

    let config = ContextPrunerConfig {
        max_token_budget: 100,
        l1_budget_ratio: 0.5,
        auto_prune_on_ingest: false,
        ..Default::default()
    };

    // Scope 1: Create pruner with file-backed DB and evict item
    {
        let pruner = DynamicAttentionContextPruner::with_db_path(config.clone(), &db_path)
            .expect("Pruner creation with disk path");

        let cold_item = ContextItem::new_terminal_log(
            "cold-crash-dump",
            TerminalStream::Stderr,
            "hgb-daemon --verify",
            Some(139),
            "SIGSEGV segmentation fault encountered at virtual address 0xdeadbeef during zero-copy remap",
            0.15,
            vec![],
        );

        pruner.ingest(cold_item).unwrap();
        // Force eviction to L3
        pruner.l3_store().persist(&pruner.get_item("cold-crash-dump").unwrap().unwrap().0, 0.05, 1_000_000).unwrap();
        assert_eq!(pruner.l3_store().count().unwrap(), 1);
    } // Pruner dropped here, SQLite connection closed

    // Verify database file exists on disk
    assert!(db_path.exists(), "SQLite DB file must exist on disk");

    // Scope 2: Open a fresh pruner instance pointing to the same disk file
    {
        let pruner_reopened = DynamicAttentionContextPruner::with_db_path(config, &db_path)
            .expect("Reopening pruner with existing DB");

        assert_eq!(
            pruner_reopened.l3_store().count().unwrap(),
            1,
            "Persisted cold item count must match"
        );

        // Query by text substring
        let query_filter = ColdQueryFilter {
            text_query: Some("SIGSEGV".to_string()),
            limit: 10,
            ..Default::default()
        };
        let matches = pruner_reopened.l3_store().query(query_filter).unwrap();
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].id, "cold-crash-dump");
        assert!(matches[0].raw_content.contains("0xdeadbeef"));

        // Reconstitute into active working memory
        let ok = pruner_reopened.reconstitute_to_l1("cold-crash-dump").unwrap();
        assert!(ok);
        assert_eq!(pruner_reopened.metrics().l1_items_count, 1);
    }
}

// ============================================================================
// 7. Test Multi-Threaded Concurrency, State Consistency, and Thread Safety
// ============================================================================

#[test]
fn test_multithreaded_concurrency_and_thread_safety() {
    let config = ContextPrunerConfig {
        max_token_budget: 1500,
        l1_budget_ratio: 0.60,
        auto_prune_on_ingest: true,
        protect_causal_dependencies: true,
        ..Default::default()
    };

    let pruner = Arc::new(DynamicAttentionContextPruner::new(config).expect("Pruner init"));
    let mut handles = Vec::new();

    // 8 concurrent threads executing mixed operations
    // Threads 0..2: Ingesting turns and AST fragments
    for t_idx in 0..3 {
        let p = Arc::clone(&pruner);
        handles.push(thread::spawn(move || {
            for i in 0..30 {
                let id = format!("thread-{}-turn-{}", t_idx, i);
                let item = ContextItem::new_conversation(
                    id,
                    ConversationRole::Assistant,
                    format!("Agent-{}", t_idx),
                    format!("Parallel multi-threaded ingestion payload iteration {} with execution telemetry", i),
                    0.75,
                    vec![],
                );
                p.ingest(item).unwrap();
                thread::sleep(Duration::from_millis(1));
            }
        }));
    }

    // Threads 3..4: Ingesting tool receipts with causal dependencies
    for t_idx in 3..5 {
        let p = Arc::clone(&pruner);
        handles.push(thread::spawn(move || {
            for i in 0..20 {
                let id = format!("thread-{}-tool-{}", t_idx, i);
                let parent_id = format!("thread-0-turn-{}", i.min(10));
                let item = ContextItem::new_tool_receipt(
                    id,
                    "bash_run",
                    format!("call-{}", i),
                    json!({ "idx": i }),
                    format!("Output for command invocation iteration {}", i),
                    ToolExecutionStatus::Success,
                    0.80,
                    vec![parent_id],
                );
                p.ingest(item).unwrap();
                thread::sleep(Duration::from_millis(2));
            }
        }));
    }

    // Thread 5: Periodic pruning sweeps
    {
        let p = Arc::clone(&pruner);
        handles.push(thread::spawn(move || {
            for _ in 0..15 {
                let _ = p.prune();
                thread::sleep(Duration::from_millis(5));
            }
        }));
    }

    // Thread 6: Periodic prompt assembly and telemetry inspection
    {
        let p = Arc::clone(&pruner);
        handles.push(thread::spawn(move || {
            for _ in 0..15 {
                let assembled = p.assemble_prompt();
                assert!(!assembled.prompt_text.is_empty());
                let m = p.metrics();
                assert!(m.max_token_budget == 1500);
                thread::sleep(Duration::from_millis(4));
            }
        }));
    }

    // Thread 7: Periodic score querying and reconstitution
    {
        let p = Arc::clone(&pruner);
        handles.push(thread::spawn(move || {
            for i in 0..15 {
                let id = format!("thread-0-turn-{}", i);
                let _ = p.score_item(&id, 1_700_000_000_000);
                let _ = p.reconstitute_to_l1(&id);
                thread::sleep(Duration::from_millis(3));
            }
        }));
    }

    for handle in handles {
        handle.join().expect("Worker thread panicked");
    }

    // Final state consistency verification
    let final_metrics = pruner.metrics();
    assert!(
        final_metrics.total_active_tokens <= 1500,
        "Total active tokens ({}) must adhere to budget",
        final_metrics.total_active_tokens
    );
    assert!(final_metrics.l1_items_count + final_metrics.l2_items_count > 0);
}
