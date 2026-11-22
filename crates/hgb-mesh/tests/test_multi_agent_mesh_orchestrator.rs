//! # Integration Test Suite for Multi-Agent Swarm Orchestrator & PBFT Consensus
//! ==============================================================================
//! Pillar 2: Distributed Multi-Agent Swarm Orchestration (`hgb-mesh`)
//!
//! Tests:
//! 1. 4-node cluster orchestration ($3f+1$, $f=1$, quorum $2f+1=3$).
//! 2. Role specialization: `CodeAuditor`, `PerformanceTuner`, `TestGenerator`.
//! 3. Cryptographically signed `SwarmTask` dispatch, priority scheduling, and assignment.
//! 4. WASM Component Model execution logic for each agent role.
//! 5. PBFT consensus debate: multi-agent voting on code patches and state transitions.
//! 6. Quorum enforcement: requires $2f+1$ matching endorsements to commit.
//! 7. Malicious code rejection: adversarial payload rejected by swarm debate.

use base64::Engine;
use hgb_mesh::multi_agent_mesh_orchestrator::*;
use hgb_mesh::pbft_consensus::MutationProposal;
use hgb_mesh::secure_transport::NodeIdentity;

fn setup_swarm_cluster() -> (
    NodeIdentity,
    Vec<(NodeIdentity, Vec<SwarmAgentRole>)>,
    DistributedAgentOrchestrator,
) {
    let leader_ident = NodeIdentity::generate();
    let auditor_ident = NodeIdentity::generate();
    let tuner_ident = NodeIdentity::generate();
    let tester_ident = NodeIdentity::generate();

    let peers = vec![
        (auditor_ident.clone(), vec![SwarmAgentRole::CodeAuditor]),
        (tuner_ident.clone(), vec![SwarmAgentRole::PerformanceTuner]),
        (tester_ident.clone(), vec![SwarmAgentRole::TestGenerator]),
    ];

    let orchestrator = DistributedAgentOrchestrator::new(leader_ident.clone(), peers.clone());
    (leader_ident, peers, orchestrator)
}

#[test]
fn test_multi_agent_orchestrator_task_lifecycle_and_wasm_execution() {
    let (leader, peers, orchestrator) = setup_swarm_cluster();
    let auditor_id = peers[0].0.peer_id_string();
    let tuner_id = peers[1].0.peer_id_string();
    let tester_id = peers[2].0.peer_id_string();

    // 1. Submit CodeAuditor Task
    let audit_payload = SwarmTaskPayload::AuditCode {
        file_path: "crates/hgb-mesh/src/quic_zero_copy_transport.rs".to_string(),
        code_content: r#"
            pub fn receive_packet(raw: *const u8, len: usize) -> Result<(), &'static str> {
                unsafe {
                    let slice = std::slice::from_raw_parts(raw, len);
                    println!("Read {} bytes", slice.len());
                }
                Ok(())
            }
        "#
        .to_string(),
        security_rules: vec!["no_naked_unsafe".to_string()],
    };

    let audit_task = SwarmTask::new(
        &leader,
        SwarmAgentRole::CodeAuditor,
        audit_payload,
        30_000,
        TaskPriority::High,
    )
    .expect("Failed to create audit task");

    let audit_task_id = orchestrator.submit_task(audit_task).expect("Task submission failed");

    // 2. Submit PerformanceTuner Task
    let tuner_payload = SwarmTaskPayload::TunePerformance {
        component_name: "quic_zero_copy_ring".to_string(),
        execution_trace: vec![350, 420, 610, 890, 750],
        target_bandwidth_gbps: 25.0,
        enable_npu_quantization: true,
    };

    let tuner_task = SwarmTask::new(
        &leader,
        SwarmAgentRole::PerformanceTuner,
        tuner_payload,
        30_000,
        TaskPriority::Normal,
    )
    .expect("Failed to create tuner task");

    let tuner_task_id = orchestrator.submit_task(tuner_task).expect("Task submission failed");

    // 3. Submit TestGenerator Task
    let tester_payload = SwarmTaskPayload::GenerateTests {
        function_name: "verify_pbft_commit".to_string(),
        signature: "fn verify_pbft_commit(proof: &[u8]) -> bool".to_string(),
        spec_invariants: vec!["proof.len() >= 64".to_string()],
    };

    let tester_task = SwarmTask::new(
        &leader,
        SwarmAgentRole::TestGenerator,
        tester_payload,
        30_000,
        TaskPriority::Critical,
    )
    .expect("Failed to create tester task");

    let tester_task_id = orchestrator.submit_task(tester_task).expect("Task submission failed");

    // 4. Verify Task Assignment based on Role Matching
    // TestGenerator is Critical priority, so requesting node matching TestGenerator gets it first!
    let assigned_tester = orchestrator.assign_next_task(&tester_id).unwrap().expect("Must assign to tester");
    assert_eq!(assigned_tester.id, tester_task_id);
    assert_eq!(assigned_tester.target_role, SwarmAgentRole::TestGenerator);

    let assigned_auditor = orchestrator.assign_next_task(&auditor_id).unwrap().expect("Must assign to auditor");
    assert_eq!(assigned_auditor.id, audit_task_id);
    assert_eq!(assigned_auditor.target_role, SwarmAgentRole::CodeAuditor);

    let assigned_tuner = orchestrator.assign_next_task(&tuner_id).unwrap().expect("Must assign to tuner");
    assert_eq!(assigned_tuner.id, tuner_task_id);
    assert_eq!(assigned_tuner.target_role, SwarmAgentRole::PerformanceTuner);

    // 5. Execute Tasks via WASM Component Model Guest Runner
    let audit_result = orchestrator.execute_and_complete_task(&audit_task_id).expect("Audit execution failed");
    if let SwarmTaskResult::Audit(report) = audit_result {
        assert_eq!(report.unsafe_block_count, 1);
        assert!(!report.findings.is_empty());
        assert!(report.cwe_classifications.contains(&"CWE-242".to_string()));
    } else {
        panic!("Expected Audit result");
    }

    let tuner_result = orchestrator.execute_and_complete_task(&tuner_task_id).expect("Tuning execution failed");
    if let SwarmTaskResult::Tuning(report) = tuner_result {
        assert!(report.npu_auto_quantization_triggered, "Quantization should trigger for hot cycle trace");
        assert!(report.latency_reduction_pct > 20.0);
    } else {
        panic!("Expected Tuning result");
    }

    let tester_result = orchestrator.execute_and_complete_task(&tester_task_id).expect("Test gen execution failed");
    if let SwarmTaskResult::Tests(report) = tester_result {
        assert!(!report.fuzzing_inputs.is_empty(), "Must synthesize fuzzing vectors");
        assert!(!report.regression_tests.is_empty(), "Must generate regression test cases");
        assert!(report.branch_coverage_pct > 90.0);
    } else {
        panic!("Expected Tests result");
    }
}

#[test]
fn test_multi_agent_pbft_consensus_debate_and_quorum_commit() {
    let (leader, peers, orchestrator) = setup_swarm_cluster();
    let auditor = &peers[0].0;
    let tuner = &peers[1].0;
    let tester = &peers[2].0;

    // 1. Leader proposes a code patch for high-throughput zero-copy serialization
    let valid_patch = r#"
// Performance optimization: zero-copy SIMD serialization
pub fn fast_serialize(input: &[u64]) -> Vec<u8> {
    let mut out = Vec::with_capacity(input.len() * 8);
    for val in input {
        out.extend_from_slice(&val.to_le_bytes());
    }
    out
}
"#;

    let proposal = MutationProposal::new(
        &leader,
        "crates/hgb-mesh/src/zero_copy_patch.rs",
        valid_patch,
        "Zero-copy SIMD serialization hot-patch",
    )
    .expect("Proposal creation failed");

    let proposal_id = orchestrator.propose_code_patch(proposal).expect("Proposal debate initiation failed");

    // 2. Swarm Agents debate and cast PBFT consensus votes
    // CodeAuditor checks syntax and CWE safety -> votes Accept
    let vote_auditor = SwarmConsensusVote::new(
        auditor,
        &proposal_id,
        1,
        0,
        SwarmAgentRole::CodeAuditor,
        VoteDecision::Accept,
        "Zero unsafe blocks, no banned patterns, AST syntax valid",
        0.05,
    )
    .expect("Auditor vote failed");

    // PerformanceTuner evaluates latency reduction -> votes Accept
    let vote_tuner = SwarmConsensusVote::new(
        tuner,
        &proposal_id,
        1,
        0,
        SwarmAgentRole::PerformanceTuner,
        VoteDecision::Accept,
        "Memory bandwidth improved to 28.5 GB/s, latency reduced by 18%",
        0.02,
    )
    .expect("Tuner vote failed");

    // TestGenerator verifies boundary coverage -> votes Accept
    let vote_tester = SwarmConsensusVote::new(
        tester,
        &proposal_id,
        1,
        0,
        SwarmAgentRole::TestGenerator,
        VoteDecision::Accept,
        "All boundary assertions verified across 1024 fuzz iterations",
        0.01,
    )
    .expect("Tester vote failed");

    // 3. Cast votes into Orchestrator
    orchestrator.cast_consensus_vote(vote_auditor).unwrap();
    orchestrator.cast_consensus_vote(vote_tuner).unwrap();
    orchestrator.cast_consensus_vote(vote_tester).unwrap();

    // 4. Evaluate Consensus: 3 Accept votes out of 4 nodes satisfies PBFT quorum ($2f+1 = 3$)
    let report = orchestrator.evaluate_consensus(&proposal_id).expect("Consensus evaluation failed");
    assert_eq!(report.total_votes, 3);
    assert_eq!(report.accept_votes, 3);
    assert_eq!(report.reject_votes, 0);
    assert!(report.quorum_reached, "PBFT quorum (2f+1 = 3) must be reached");
    assert!(report.committed, "Proposal must be committed upon reaching PBFT quorum");
}

#[test]
fn test_multi_agent_rejects_malicious_code_proposal() {
    let (leader, peers, orchestrator) = setup_swarm_cluster();
    let auditor = &peers[0].0;

    // Craft malicious patch containing banned Command pattern
    let malicious_patch = r#"
pub fn backdoor() {
    std::process::Command::new("rm").arg("-rf").arg("/").spawn().ok();
}
"#;

    let proposal = MutationProposal::new(
        &leader,
        "crates/hgb-mesh/src/backdoor.rs",
        malicious_patch,
        "Malicious backdoor patch",
    );

    // Leader PBFT engine self-audit rejects proposal containing malicious command patterns
    assert!(proposal.is_ok());
    let res = orchestrator.propose_code_patch(proposal.unwrap());
    assert!(res.is_err(), "Leader PBFT engine must reject proposal with malicious patterns");
}
