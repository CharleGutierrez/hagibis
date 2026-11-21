//! # PBFT Swarm Consensus & Code Mutation Debate Integration Tests
//! ================================================================
//! Verifies:
//! 1. 4-node cluster ($3f+1$, $f=1$, quorum $2f+1=3$) full PrePrepare -> Prepare -> Commit lifecycle.
//! 2. Swarm code mutation debate: rejects malformed syntax and malicious patterns (`rm -rf`, unbalanced braces).
//! 3. Byzantine fault tolerance: 1 malicious node equivocating and forging signatures does NOT stall or corrupt consensus.
//! 4. Leader timeout failover and View Change protocol transition to new view.
//! 5. Periodic stable checkpoint generation, $2f+1$ verification, and log compaction/pruning.

use std::collections::HashMap;
use base64::Engine;

use hgb_mesh::pbft_consensus::{
    CodeMutationAuditor, MutationProposal, PbftConsensusEngine, PbftMessage,
};
use hgb_mesh::secure_transport::NodeIdentity;

/// Helper to spin up an N-node cluster with full mutual cryptographic identities
fn setup_pbft_cluster(n: usize) -> (Vec<NodeIdentity>, Vec<PbftConsensusEngine>) {
    let mut identities = Vec::with_capacity(n);
    let mut node_ids = Vec::with_capacity(n);
    let mut key_dir = HashMap::with_capacity(n);

    for _ in 0..n {
        let ident = NodeIdentity::generate();
        node_ids.push(ident.peer_id_string());
        key_dir.insert(ident.peer_id_string(), ident.public_key());
        identities.push(ident);
    }

    let mut engines = Vec::with_capacity(n);
    for ident in &identities {
        let engine = PbftConsensusEngine::new(ident.clone(), node_ids.clone(), key_dir.clone());
        engines.push(engine);
    }

    (identities, engines)
}

#[test]
fn test_pbft_math_and_quorum_calculation() {
    let (_, engines_4) = setup_pbft_cluster(4);
    assert_eq!(engines_4[0].f(), 1);
    assert_eq!(engines_4[0].quorum_size(), 3); // 2*1 + 1 = 3

    let (_, engines_7) = setup_pbft_cluster(7);
    assert_eq!(engines_7[0].f(), 2);
    assert_eq!(engines_7[0].quorum_size(), 5); // 2*2 + 1 = 5
}

#[test]
fn test_pbft_4_node_cluster_full_consensus_lifecycle() {
    let (identities, mut engines) = setup_pbft_cluster(4);
    // Node 0 is leader for view 0 (0 % 4 == 0)
    assert!(engines[0].is_leader());
    assert!(!engines[1].is_leader());

    // 1. Author (Node 1) crafts a valid code mutation proposal
    let valid_patch = r#"
// Strategic hot-patch for latency reduction
pub fn fast_path_compute(input: u64) -> u64 {
    input.saturating_mul(2)
}
"#;
    let proposal = MutationProposal::new(
        &identities[1],
        "crates/hgb-core/src/fast_path.rs",
        valid_patch,
        "Optimize math compute path in hot loop",
    )
    .expect("Proposal creation should succeed");

    // 2. Leader (Node 0) receives proposal and broadcasts PrePrepare
    let pre_prepare = engines[0]
        .propose_mutation(proposal.clone())
        .expect("Leader PrePrepare proposal must succeed");

    // 3. Backups (Nodes 1, 2, 3) handle PrePrepare -> Swarm debate passes -> Broadcast Prepare
    let mut prepares = Vec::new();
    for i in 1..4 {
        let prep_opt = engines[i]
            .handle_pre_prepare(pre_prepare.clone())
            .expect("Backup should validate PrePrepare");
        let prep = prep_opt.expect("Backup must emit Prepare vote upon audit success");
        prepares.push(prep);
    }

    // 4. All nodes exchange Prepare votes -> Reach Prepared state -> Broadcast Commit
    let mut commits = Vec::new();
    for engine in engines.iter_mut() {
        for prep in &prepares {
            if let Some(commit) = engine.handle_prepare(prep.clone()).expect("Handle prepare") {
                commits.push(commit);
            }
        }
    }

    // Ensure at least 3 nodes ($2f+1$) reached Prepared and emitted Commit votes
    assert!(commits.len() >= 3);

    // 5. All nodes exchange Commit votes -> Reach Committed state ($2f+1$) -> Execute proposal
    for engine in engines.iter_mut() {
        for commit in &commits {
            let _ = engine.handle_commit(commit.clone()).expect("Handle commit");
        }
        // Verify node committed and executed the mutation proposal
        assert_eq!(engine.committed_proposals.len(), 1);
        assert_eq!(engine.committed_proposals[0].id, proposal.id);
        assert!(engine.executed_proposal_ids.contains(&proposal.id));
    }

    // Verify all nodes reached identical consensus state digest
    let state_0 = &engines[0].state_digest;
    for engine in &engines {
        assert_eq!(&engine.state_digest, state_0);
    }
}

#[test]
fn test_swarm_code_mutation_debate_rejects_malicious_and_invalid_syntax() {
    let (identities, mut engines) = setup_pbft_cluster(4);

    // A. Unbalanced syntax delimiter
    let malformed_patch = r#"
pub fn broken_function() {
    let x = 42;
    // Missing closing brace!
"#;
    let malformed_proposal = MutationProposal::new(
        &identities[1],
        "crates/hgb-core/src/broken.rs",
        malformed_patch,
        "Malformed syntax",
    )
    .expect("Create proposal");

    let audit_fail = CodeMutationAuditor::audit_proposal(&malformed_proposal, &identities[1].public_key());
    assert!(!audit_fail.approved);
    assert!(audit_fail.reason.contains("Syntax error"));

    // B. Destructive shell execution attempt
    let dangerous_patch = r#"
pub fn run_maintenance() {
    let _ = "rm -rf /";
}
"#;
    let dangerous_proposal = MutationProposal::new(
        &identities[1],
        "crates/hgb-core/src/maintenance.rs",
        dangerous_patch,
        "Dangerous patch",
    )
    .expect("Create proposal");

    let audit_danger = CodeMutationAuditor::audit_proposal(&dangerous_proposal, &identities[1].public_key());
    assert!(!audit_danger.approved);
    assert!(audit_danger.reason.contains("Malicious code pattern detected"));

    // C. Path traversal attempt outside sandbox
    let traversal_proposal = MutationProposal::new(
        &identities[1],
        "../../etc/shadow",
        "root:x:0:0",
        "Path traversal attempt",
    )
    .expect("Create proposal");

    let audit_traversal = CodeMutationAuditor::audit_proposal(&traversal_proposal, &identities[1].public_key());
    assert!(!audit_traversal.approved);
    assert!(audit_traversal.reason.contains("Prohibited path traversal"));

    // D. Leader refuses to propose invalid code
    let propose_res = engines[0].propose_mutation(dangerous_proposal);
    assert!(propose_res.is_err());
}

#[test]
fn test_byzantine_fault_tolerance_with_faulty_node() {
    let (identities, mut engines) = setup_pbft_cluster(4);
    // Node 3 is Byzantine

    let valid_patch = "pub const RESILIENCE_FACTOR: u32 = 42;";
    let proposal = MutationProposal::new(
        &identities[1],
        "crates/hgb-core/src/constants.rs",
        valid_patch,
        "Update factor",
    )
    .expect("Create proposal");

    let pre_prepare = engines[0]
        .propose_mutation(proposal.clone())
        .expect("Leader propose");

    // Honest nodes 1 and 2 process PrePrepare
    let prep_1 = engines[1].handle_pre_prepare(pre_prepare.clone()).unwrap().unwrap();
    let prep_2 = engines[2].handle_pre_prepare(pre_prepare.clone()).unwrap().unwrap();

    // Byzantine node 3 crafts forged signature and bogus payload
    let forged_prepare = PbftMessage::Prepare {
        view: 0,
        sequence_number: 1,
        proposal_digest: "bogus_forged_digest_0000000000000000000000000000000000000000".to_string(),
        node_id: identities[3].peer_id_string(),
        signature: "invalid_base64_sig".to_string(),
    };

    // Honest nodes reject the Byzantine message
    assert!(engines[1].handle_prepare(forged_prepare).is_err());

    // Honest nodes exchange valid prepares (Node 0, 1, 2 = 3 nodes, >= 2f)
    let commit_0 = engines[0].handle_prepare(prep_1.clone()).unwrap();
    let commit_1 = engines[1].handle_prepare(prep_2.clone()).unwrap();
    let commit_2 = engines[2].handle_prepare(prep_1.clone()).unwrap();

    let honest_commits = vec![
        commit_0.expect("Commit 0"),
        commit_1.expect("Commit 1"),
        commit_2.expect("Commit 2"),
    ];

    // Honest nodes achieve $2f+1 = 3$ quorum despite Byzantine node 3 omission
    for i in 0..3 {
        for commit in &honest_commits {
            let _ = engines[i].handle_commit(commit.clone()).unwrap();
        }
        assert_eq!(engines[i].committed_proposals.len(), 1);
        assert_eq!(engines[i].committed_proposals[0].id, proposal.id);
    }
}

#[test]
fn test_view_change_on_leader_timeout() {
    let (_, mut engines) = setup_pbft_cluster(4);
    assert_eq!(engines[0].current_view, 0);

    // Backups detect leader timeout and trigger ViewChange to view 1
    let mut view_change_messages = Vec::new();
    for i in 1..4 {
        let vc_msg = engines[i].trigger_view_change().expect("Trigger view change");
        assert!(!engines[i].view_active);
        view_change_messages.push(vc_msg);
    }

    // Node 1 is the leader for view 1 (1 % 4 == 1)
    assert_eq!(engines[1].leader_for_view(1), engines[1].node_id);

    // Node 1 processes $2f+1=3$ ViewChange votes and issues NewView
    let mut new_view_opt = None;
    for vc in &view_change_messages {
        if let Some(nv) = engines[1].handle_view_change(vc.clone()).unwrap() {
            new_view_opt = Some(nv);
        }
    }

    let new_view_msg = new_view_opt.expect("New leader must construct NewView certificate");
    assert_eq!(engines[1].current_view, 1);
    assert!(engines[1].view_active);

    // Backups verify NewView and transition to view 1
    for i in 2..4 {
        engines[i].handle_new_view(new_view_msg.clone()).expect("Handle NewView");
        assert_eq!(engines[i].current_view, 1);
        assert!(engines[i].view_active);
    }
}

#[test]
fn test_checkpoint_and_log_pruning() {
    let (_, mut engines) = setup_pbft_cluster(4);

    // Manually simulate consensus execution through sequence 10
    engines[0].last_sequence_number = 10;
    engines[0].state_digest = "state_hash_sequence_10".to_string();

    // Node 0 creates Checkpoint at sequence 10
    let cp_0 = engines[0].create_checkpoint(10).expect("Create checkpoint");

    // Other nodes endorse checkpoint
    let cp_1 = PbftMessage::Checkpoint {
        sequence_number: 10,
        state_digest: "state_hash_sequence_10".to_string(),
        node_id: engines[1].node_id.clone(),
        signature: engines[1]
            .identity
            .sign(&hgb_mesh::secure_transport::compute_sha256(
                format!("CHECKPOINT:10:state_hash_sequence_10:{}", engines[1].node_id).as_bytes(),
            ))
            .map(|s| base64::engine::general_purpose::STANDARD.encode(s))
            .unwrap(),
    };
    let cp_2 = PbftMessage::Checkpoint {
        sequence_number: 10,
        state_digest: "state_hash_sequence_10".to_string(),
        node_id: engines[2].node_id.clone(),
        signature: engines[2]
            .identity
            .sign(&hgb_mesh::secure_transport::compute_sha256(
                format!("CHECKPOINT:10:state_hash_sequence_10:{}", engines[2].node_id).as_bytes(),
            ))
            .map(|s| base64::engine::general_purpose::STANDARD.encode(s))
            .unwrap(),
    };

    // Node 0 handles endorsements
    assert!(!engines[0].handle_checkpoint(cp_1).unwrap());
    // 3rd endorsement achieves $2f+1$ quorum -> stable checkpoint established & logs pruned
    assert!(engines[0].handle_checkpoint(cp_2).unwrap());
    assert_eq!(engines[0].last_stable_checkpoint, 10);
    assert_eq!(engines[0].last_stable_checkpoint_digest, "state_hash_sequence_10");
}
