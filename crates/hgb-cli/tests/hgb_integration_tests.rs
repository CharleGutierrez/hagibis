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

#[test]
fn test_agy_crud_view_and_binary_safety() {
    use hgb_core::{AgyCrud, ViewFileOptions};
    let temp = std::env::temp_dir().join(format!("hgb_test_view_{}.txt", std::process::id()));
    let text = "Line 1: Alpha\nLine 2: Beta\nLine 3: Gamma\nLine 4: Delta\nLine 5: Epsilon\n";
    std::fs::write(&temp, text).unwrap();

    // Slicing lines 2 to 4
    let res = AgyCrud::view_file(&temp, ViewFileOptions {
        start_line: Some(2),
        end_line: Some(4),
        content_offset: None,
        max_lines: Some(10),
        line_numbers: true,
    }).unwrap();

    assert_eq!(res.total_lines, 5);
    assert!(res.content.contains("2 | Line 2: Beta"));
    assert!(res.content.contains("4 | Line 4: Delta"));
    assert!(!res.content.contains("1 | Line 1: Alpha"));
    assert!(!res.content.contains("5 | Line 5: Epsilon"));

    // Binary safety check
    let bin_path = std::env::temp_dir().join(format!("hgb_test_bin_{}.png", std::process::id()));
    let bin_bytes = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x01];
    std::fs::write(&bin_path, bin_bytes).unwrap();

    let bin_res = AgyCrud::view_file(&bin_path, ViewFileOptions::default()).unwrap();
    assert!(bin_res.is_binary);
    assert!(bin_res.content.contains("Binary file detected"));

    let _ = std::fs::remove_file(&temp);
    let _ = std::fs::remove_file(&bin_path);
}

#[test]
fn test_agy_crud_write_and_overwrite_protection() {
    use hgb_core::AgyCrud;
    let temp = std::env::temp_dir().join(format!("hgb_nested_sub/test_write_{}.txt", std::process::id()));
    let _ = std::fs::remove_file(&temp);

    // Initial write creates parent directory
    let res = AgyCrud::write_to_file(&temp, "initial content", false, None).unwrap();
    assert!(res.contains("Successfully wrote 15 bytes"));

    // Overwrite=false should error
    let err = AgyCrud::write_to_file(&temp, "new content", false, None);
    assert!(err.is_err());
    assert!(err.unwrap_err().to_string().contains("Target file already exists"));

    // Overwrite=true should succeed
    let overwrite_res = AgyCrud::write_to_file(&temp, "overwritten content", true, None);
    assert!(overwrite_res.is_ok());
    assert_eq!(std::fs::read_to_string(&temp).unwrap(), "overwritten content");

    let _ = std::fs::remove_file(&temp);
}

#[test]
fn test_agy_crud_replace_surgical_and_ambiguity() {
    use hgb_core::{AgyCrud, ReplaceOptions};
    let temp = std::env::temp_dir().join(format!("hgb_test_replace_{}.txt", std::process::id()));
    let text = "first line\nrepeat token\nmiddle line\nrepeat token\nlast line\n";
    std::fs::write(&temp, text).unwrap();

    // Ambiguity error: 'repeat token' exists twice without allow_multiple
    let ambig_err = AgyCrud::replace_file_content(&temp, "repeat token", "new token", ReplaceOptions::default());
    assert!(ambig_err.is_err());

    // Line bounded replacement: lines 1 to 3 contains exactly ONE 'repeat token'
    let bounded = AgyCrud::replace_file_content(&temp, "repeat token", "first replaced", ReplaceOptions {
        start_line: Some(1),
        end_line: Some(3),
        allow_multiple: false,
        ..Default::default()
    });
    assert!(bounded.is_ok());

    let updated = std::fs::read_to_string(&temp).unwrap();
    assert!(updated.contains("first replaced"));
    assert!(updated.contains("repeat token")); // second one remains intact!

    let _ = std::fs::remove_file(&temp);
}

#[test]
fn test_agy_crud_list_and_grep() {
    use hgb_core::AgyCrud;
    let temp_dir = std::env::temp_dir().join(format!("hgb_crud_dir_{}", std::process::id()));
    std::fs::create_dir_all(&temp_dir).unwrap();
    let file1 = temp_dir.join("alpha.txt");
    let file2 = temp_dir.join("beta.rs");
    std::fs::write(&file1, "fn main() {\n    let query = 42;\n}\n").unwrap();
    std::fs::write(&file2, "pub struct QueryParser;\n").unwrap();

    // Test list_dir
    let entries = AgyCrud::list_dir(&temp_dir).unwrap();
    assert_eq!(entries.len(), 2);

    // Test grep_search
    let matches_val = AgyCrud::grep_search(&temp_dir, "query", false, true, true, &[]).unwrap();
    let matches = matches_val.as_array().unwrap();
    assert_eq!(matches.len(), 2);

    let _ = std::fs::remove_dir_all(&temp_dir);
}
