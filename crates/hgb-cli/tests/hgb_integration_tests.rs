use hgb_core::AgentShieldLight;
use hgb_nextgen::{
    AgenticFuzzEngine, CandidateToken, DraftDistribution, MerkleTree,
    MeshNode, P2pSwarmMesh, ProvenanceLedger, SpeculativeHybridEngine, SwarmCheckpointManager,
};
use hgb_storage::{SkillRecord, SkillStore, VectorEntry, VectorIndex};
use std::collections::HashMap;

#[test]
fn test_merkle_provenance_and_tamper_detection() {
    let mut ledger = ProvenanceLedger::new();
    let entry1 = ledger.append("admin", "file_intake", b"case_data_001", Some("Rule 141"));
    let entry2 = ledger.append("raffle_officer", "branch_raffle", b"branch_007", Some("SC A.M. No. 03-8-02-SC"));
    
    assert_eq!(entry1.sequence, 1);
    assert_eq!(entry2.sequence, 2);
    assert!(!ledger.root().is_empty());

    let tree = MerkleTree::new(vec![entry1.payload_hash.clone(), entry2.payload_hash.clone()]);
    let proof = tree.generate_proof(0).expect("Proof should exist");
    assert!(MerkleTree::verify_proof(&proof, &tree.root()));

    // Tamper detection
    let mut tampered_proof = proof.clone();
    tampered_proof.leaf_hash = MerkleTree::hash_leaf(b"TAMPERED_DATA");
    assert!(!MerkleTree::verify_proof(&tampered_proof, &tree.root()));
}

#[test]
fn test_swarm_checkpoint_and_time_travel() {
    let mut mgr = SwarmCheckpointManager::new();
    mgr.record_event("TASK_START", r#"{"task": "ingest"}"#);
    
    let mut states = HashMap::new();
    states.insert("node_1".to_string(), "RUNNING".to_string());
    let mut mem = HashMap::new();
    mem.insert("step".to_string(), "1".to_string());

    let ckpt1 = mgr.create_checkpoint("step1", states.clone(), mem.clone());
    assert_eq!(ckpt1.sequence, 1);

    // Rollback
    let restored = mgr.rollback_to_checkpoint(&ckpt1.checkpoint_id).expect("Checkpoint should exist");
    assert_eq!(restored.label, "step1");
    assert_eq!(restored.memory.get("step").unwrap(), "1");
}

#[test]
fn test_differential_fuzzing() {
    let violations = AgenticFuzzEngine::fuzz_target("sample_input", |inp| {
        if inp.contains("' OR 1=1 --") {
            Err(hgb_core::HgbError::Security("SQL injection caught".to_string()))
        } else {
            Ok(())
        }
    });

    assert!(!violations.is_empty());
}

#[test]
fn test_speculative_entropy_gate() {
    let engine = SpeculativeHybridEngine::new(1.75, 0.20);
    let dist = DraftDistribution::new(vec![
        CandidateToken { token: "fn".to_string(), probability: 0.90 },
        CandidateToken { token: "pub".to_string(), probability: 0.08 },
    ]);

    let verdict = engine.evaluate_draft(&dist);
    assert!(matches!(verdict, hgb_nextgen::LakandiwaVerdict::AcceptLocalDraft { .. }));
}

#[test]
fn test_decoupled_sqlite_skill_store() {
    let temp_db = std::env::temp_dir().join(format!("hgb_test_skills_{}.db", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
    let store = SkillStore::new(&temp_db).expect("Store init failed");

    let skill = SkillRecord {
        name: "test-skill".to_string(),
        description: "Test skill description".to_string(),
        content: "Detailed markdown content".to_string(),
        tier: "tier1".to_string(),
        triggers: vec!["test".to_string(), "sample".to_string()],
    };

    store.insert_skill(&skill).expect("Insert failed");
    let fetched = store.get_skill("test-skill").expect("Get failed").expect("Skill not found");
    assert_eq!(fetched.name, "test-skill");
    assert_eq!(fetched.triggers.len(), 2);
    assert_eq!(store.count_skills().unwrap(), 1);

    let _ = std::fs::remove_file(&temp_db);
}

#[test]
fn test_vector_index() {
    let mut index = VectorIndex::new();
    index.insert(VectorEntry {
        id: "doc1".to_string(),
        text: "hello world".to_string(),
        embedding: vec![1.0, 0.0, 0.0],
    });
    index.insert(VectorEntry {
        id: "doc2".to_string(),
        text: "goodbye world".to_string(),
        embedding: vec![0.0, 1.0, 0.0],
    });

    let results = index.search(&[1.0, 0.0, 0.0], 1);
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].0.id, "doc1");
    assert!((results[0].1 - 1.0).abs() < 1e-5);
}

#[test]
fn test_p2p_mesh_queue() {
    let mut mesh = P2pSwarmMesh::new();
    mesh.register_node(MeshNode {
        node_id: "node_alpha".to_string(),
        compute_tier: "Tier3EdgeLocal".to_string(),
        vram_mb: 8192,
        active_models: vec!["qwen2.5-coder-1.5b".to_string()],
        max_concurrency: 4,
    });

    assert_eq!(mesh.nodes_count(), 1);
    mesh.enqueue_task("task_001");
    assert_eq!(mesh.steal_task(), Some("task_001".to_string()));
    assert_eq!(mesh.steal_task(), None);
}

#[test]
fn test_agent_shield_light() {
    assert!(AgentShieldLight::audit_command("ls -la").is_ok());
    assert!(AgentShieldLight::audit_command("rm -rf /").is_err());
}
