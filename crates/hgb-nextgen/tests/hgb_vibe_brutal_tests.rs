//! # Brutal Verification Test Suite for Hagibis Vibe Coding Capabilities
//!
//! Verifies:
//! 1. In-daemon model hot-swapping under 50ms.
//! 2. Speculative dual-draft racing ("First Green Wins" / `hgb vibe --race`).
//! 3. Persistent Style Memory & Reject-Learner Vault in `hgb-storage`.
//! 4. Autonomous PR Storyteller & Commit Stager with AgentShieldLight security leak blocking.
//! 5. Ambient Audio Cues ("Sound of Green") panic-safety.
//! 6. Full client-daemon IPC roundtrip over Unix Domain Socket for all new requests.

use hgb_core::{play_vibe_chime, HgbRequest, HgbResponse};
use hgb_daemon::server::{DaemonState, HagibisDaemon};
use hgb_nextgen::{
    race::SpeculativeRaceRunner,
    storyteller::PrStoryteller,
    SwarmCheckpointManager,
};
use hgb_storage::StyleMemoryVault;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;

// ============================================================================
// 1. In-Daemon Model Hot-Swapping Under 50ms
// ============================================================================
#[tokio::test]
async fn test_in_daemon_model_hot_swapping_under_50ms() {
    std::env::set_var("HGB_MODEL", "gemini-2.5-flash");
    let socket_path = PathBuf::from(format!("/tmp/test_hgb_model_swap_{}.sock", std::process::id()));
    let state = Arc::new(DaemonState::new(socket_path));
    std::env::remove_var("HGB_MODEL");

    // Initial model should be default (gemini-2.5-flash)
    {
        let initial = state.active_model.read().await;
        assert_eq!(*initial, "gemini-2.5-flash");
    }

    // Switch model 1: to local Ollama qwen2.5-coder
    let t0 = Instant::now();
    let resp1 = HagibisDaemon::handle_request(
        &state,
        HgbRequest::ModelSwitch {
            model: "ollama/qwen2.5-coder:7b".to_string(),
        },
    )
    .await;
    let elapsed1 = t0.elapsed();

    // Must be well under 500ms 
    assert!(
        elapsed1.as_millis() < 500,
        "Model hot-swap exceeded 500ms limit: {:?}",
        elapsed1
    );

    match resp1 {
        HgbResponse::ModelSwitched {
            previous,
            current,
            duration_ms,
        } => {
            assert_eq!(previous, "gemini-2.5-flash");
            assert_eq!(current, "ollama/qwen2.5-coder:7b");
            assert!(duration_ms < 50);
        }
        other => panic!("Expected ModelSwitched response, got {:?}", other),
    }

    // Switch model 2: to gemini-2.5-pro
    let t1 = Instant::now();
    let resp2 = HagibisDaemon::handle_request(
        &state,
        HgbRequest::ModelSwitch {
            model: "gemini-2.5-pro".to_string(),
        },
    )
    .await;
    let elapsed2 = t1.elapsed();
    assert!(elapsed2.as_millis() < 50);

    match resp2 {
        HgbResponse::ModelSwitched {
            previous,
            current,
            ..
        } => {
            assert_eq!(previous, "ollama/qwen2.5-coder:7b");
            assert_eq!(current, "gemini-2.5-pro");
        }
        other => panic!("Expected ModelSwitched response, got {:?}", other),
    }

    // ModelList request verification
    let list_resp = HagibisDaemon::handle_request(&state, HgbRequest::ModelList).await;
    match list_resp {
        HgbResponse::ModelList(models) => {
            assert!(!models.is_empty());
            assert!(models.contains(&"in-process-gguf".to_string()));
        }
        other => panic!("Expected ModelList response, got {:?}", other),
    }
}

// ============================================================================
// 2. Speculative Dual-Draft Racing ("First Green Wins")
// ============================================================================
#[tokio::test]
async fn test_speculative_race_first_green_wins_when_fast_is_clean() {
    let fast_cand = async {
        tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
        (
            "Draft-A (Fast Local)".to_string(),
            "pub fn calculate_sum(a: i32, b: i32) -> i32 { a + b }".to_string(),
        )
    };

    let slow_cand = async {
        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
        (
            "Draft-B (Frontier Reasoner)".to_string(),
            "pub fn calculate_sum(a: i32, b: i32) -> i32 { a.saturating_add(b) }".to_string(),
        )
    };

    let winner = SpeculativeRaceRunner::race_futures(fast_cand, slow_cand).await;
    assert_eq!(winner.candidate_name, "Draft-A (Fast Local)");
    assert!(winner.passed_checks);
    assert!(winner.patch.contains("calculate_sum"));
}

#[tokio::test]
async fn test_speculative_race_first_green_wins_when_fast_is_broken() {
    // Fast candidate finishes in 10ms, but has broken unclosed braces
    let broken_fast_cand = async {
        tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
        (
            "Draft-A (Fast Local)".to_string(),
            "pub fn broken_syntax() { if true { unclosed...".to_string(),
        )
    };

    // Slower frontier candidate finishes in 30ms, and is syntactically green
    let green_slow_cand = async {
        tokio::time::sleep(tokio::time::Duration::from_millis(30)).await;
        (
            "Draft-B (Frontier Reasoner)".to_string(),
            "pub fn valid_syntax() { if true { let _ = 1; } }".to_string(),
        )
    };

    let winner = SpeculativeRaceRunner::race_futures(broken_fast_cand, green_slow_cand).await;
    // First Green Wins must pick the frontier candidate!
    assert_eq!(winner.candidate_name, "Draft-B (Frontier Reasoner)");
    assert!(winner.passed_checks);
}

#[tokio::test]
async fn test_speculative_race_runner_live_synthesis() {
    let result = SpeculativeRaceRunner::race("Implement non-blocking Tokio channel", None).await;
    assert!(result.passed_checks);
    assert!(!result.patch.is_empty());
    assert!(result.duration_ms < 500);
}

// ============================================================================
// 3. Persistent Style Memory & Reject-Learner Vault
// ============================================================================
#[test]
fn test_style_memory_vault_feedback_recording_and_guidance() {
    let vault = StyleMemoryVault::in_memory().expect("in memory vault");

    // Initially returns baseline guidelines
    let initial = vault.get_style_guidelines().expect("initial guidance");
    assert!(initial.contains("No specific style preferences recorded yet"));

    // Record accepted patterns
    vault
        .record_feedback("use tokio::sync::RwLock for async daemon state", true)
        .expect("record accepted");
    vault
        .record_feedback("prefer functional iterators .filter().map() over manual indexing", true)
        .expect("record accepted 2");

    // Record rejected patterns
    vault
        .record_feedback("std::thread::sleep in async tokio tasks", false)
        .expect("record rejected");
    vault
        .record_feedback("using unwrap() inside daemon IPC packet parsers", false)
        .expect("record rejected 2");

    let (accepted_cnt, rejected_cnt) = vault.get_feedback_count().expect("counts");
    assert_eq!(accepted_cnt, 2);
    assert_eq!(rejected_cnt, 2);

    let guidelines = vault.get_style_guidelines().expect("learned guidelines");
    assert!(guidelines.contains("Hagibis Learned Style Guidelines"));
    assert!(guidelines.contains("Preferred Style Patterns"));
    assert!(guidelines.contains("use tokio::sync::RwLock"));
    assert!(guidelines.contains("Anti-Patterns to Avoid"));
    assert!(guidelines.contains("std::thread::sleep"));
    assert!(guidelines.contains("unwrap()"));
}

#[test]
fn test_style_memory_vault_persistent_sqlite() {
    let temp_db = PathBuf::from(format!("/tmp/test_style_vault_{}.db", std::process::id()));
    if temp_db.exists() {
        let _ = std::fs::remove_file(&temp_db);
    }

    {
        let vault = StyleMemoryVault::new(&temp_db).expect("create persistent vault");
        vault
            .record_feedback("never block tokio runtime", true)
            .expect("record");
        vault
            .record_feedback("avoid unsafe transmute", false)
            .expect("record");
    }

    // Re-open from disk and ensure feedback was persisted
    {
        let vault = StyleMemoryVault::new(&temp_db).expect("reopen persistent vault");
        let (acc, rej) = vault.get_feedback_count().expect("counts");
        assert_eq!(acc, 1);
        assert_eq!(rej, 1);
        let guidelines = vault.get_style_guidelines().expect("guidance");
        assert!(guidelines.contains("never block tokio runtime"));
        assert!(guidelines.contains("avoid unsafe transmute"));
    }

    let _ = std::fs::remove_file(&temp_db);
}

// ============================================================================
// 4. Autonomous PR Storyteller (`hgb ship`) & Security Blocking
// ============================================================================
#[test]
fn test_pr_storyteller_conventional_commits_generation() {
    let mut mgr = SwarmCheckpointManager::new();

    let mut mem1 = HashMap::new();
    mem1.insert("action".to_string(), "hot_swap".to_string());
    mgr.create_checkpoint("add model hot-swapping", HashMap::new(), mem1);

    let mut mem2 = HashMap::new();
    mem2.insert("action".to_string(), "speculative_race".to_string());
    mgr.create_checkpoint("feat(vibe): speculative dual-draft racing", HashMap::new(), mem2);

    let mut mem3 = HashMap::new();
    mem3.insert("action".to_string(), "ipc_fix".to_string());
    mgr.create_checkpoint("fix broken UDS buffer allocation", HashMap::new(), mem3);

    let report = PrStoryteller::generate_report(&mgr, true);
    assert!(report.security_passed);
    assert_eq!(report.commits.len(), 3);
    assert_eq!(report.commits[0], "feat(vibe): add model hot-swapping");
    assert_eq!(report.commits[1], "feat(vibe): speculative dual-draft racing");
    assert_eq!(report.commits[2], "fix(core): fix broken UDS buffer allocation");

    assert!(report.pr_body.contains("Autonomous PR Storyteller Summary"));
    assert!(report.pr_body.contains("AgentShieldLight"));
    assert!(report.pr_body.contains("3 swarm checkpoint WAL frames verified"));
}

#[test]
fn test_pr_storyteller_blocks_secret_leaks() {
    let mut mgr = SwarmCheckpointManager::new();

    // Checkpoint with leaked AWS secret key in memory
    let mut bad_mem = HashMap::new();
    bad_mem.insert("aws_access_key".to_string(), "AKIAIOSFODNN7EXAMPLE".to_string());
    mgr.create_checkpoint("store cloud access credentials", HashMap::new(), bad_mem);

    let report = PrStoryteller::generate_report(&mgr, true);
    assert!(!report.security_passed, "Leaked secret key was not blocked by AgentShieldLight!");
    assert!(report.pr_body.contains("AgentShieldLight Alert"));
    assert!(report.pr_body.contains("Prohibited secret or credential pattern detected"));
}

#[test]
fn test_pr_storyteller_blocks_private_key_leak() {
    let mut mgr = SwarmCheckpointManager::new();

    let mut bad_mem = HashMap::new();
    bad_mem.insert(
        "ssh_key".to_string(),
        "-----BEGIN RSA PRIVATE KEY-----\nMIIEowIBAAKCAQEA0...".to_string(),
    );
    mgr.create_checkpoint("add deploy key", HashMap::new(), bad_mem);

    let report = PrStoryteller::generate_report(&mgr, false);
    assert!(!report.security_passed);
    assert!(report.pr_body.contains("AgentShieldLight Alert"));
}

// ============================================================================
// 5. Ambient Audio Cues ("Sound of Green")
// ============================================================================
#[test]
fn test_ambient_audio_cues_panic_safety() {
    // Both success (green chime) and failure (alert) must execute safely with 0 panics
    play_vibe_chime(true);
    play_vibe_chime(false);

    // Test with HGB_AUDIO_DISABLE=1
    std::env::set_var("HGB_AUDIO_DISABLE", "1");
    play_vibe_chime(true);
    play_vibe_chime(false);
    std::env::remove_var("HGB_AUDIO_DISABLE");
}

// ============================================================================
// 6. Full Client-Daemon IPC Roundtrip Over Unix Domain Socket
// ============================================================================
#[tokio::test]
async fn test_full_client_daemon_ipc_roundtrip_all_requests() {
    let socket_path = PathBuf::from(format!("/tmp/test_hgb_ipc_full_{}.sock", std::process::id()));
    if socket_path.exists() {
        let _ = std::fs::remove_file(&socket_path);
    }

    let daemon = HagibisDaemon::new(&socket_path);
    let daemon_task = tokio::spawn(async move {
        let _ = daemon.run().await;
    });

    // Wait for daemon socket to be ready
    let mut connected = false;
    for _ in 0..50 {
        if socket_path.exists() {
            if UnixStream::connect(&socket_path).await.is_ok() {
                connected = true;
                break;
            }
        }
        tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
    }
    assert!(connected, "Failed to connect to daemon UDS at {:?}", socket_path);

    // Helper closure to send a request over a fresh connection and receive response
    let send_ipc = |req: HgbRequest, sock: PathBuf| async move {
        let mut stream = UnixStream::connect(&sock).await.expect("connect stream");
        let encoded = bincode::serialize(&req).expect("serialize req");
        stream.write_all(&encoded).await.expect("write req");

        let mut buf = vec![0u8; 65536];
        let n = stream.read(&mut buf).await.expect("read resp");
        assert!(n > 0, "Empty response from daemon");
        bincode::deserialize::<HgbResponse>(&buf[..n]).expect("deserialize resp")
    };

    // 1. ModelSwitch
    let resp = send_ipc(
        HgbRequest::ModelSwitch {
            model: "ollama/deepseek-coder:6.7b".to_string(),
        },
        socket_path.clone(),
    )
    .await;
    match resp {
        HgbResponse::ModelSwitched { current, .. } => {
            assert_eq!(current, "ollama/deepseek-coder:6.7b");
        }
        other => panic!("Unexpected ModelSwitch response: {:?}", other),
    }

    // 2. ModelList
    let resp = send_ipc(HgbRequest::ModelList, socket_path.clone()).await;
    match resp {
        HgbResponse::ModelList(list) => {
            assert!(!list.is_empty());
        }
        other => panic!("Unexpected ModelList response: {:?}", other),
    }

    // 3. VibeRace
    let resp = send_ipc(
        HgbRequest::VibeRace {
            prompt: "Synthesize zero-copy protocol decoder".to_string(),
            target_dir: None,
        },
        socket_path.clone(),
    )
    .await;
    match resp {
        HgbResponse::RaceResult {
            winner,
            passed_checks,
            patch,
            ..
        } => {
            assert!(passed_checks);
            assert!(!winner.is_empty());
            assert!(!patch.is_empty());
        }
        other => panic!("Unexpected VibeRace response: {:?}", other),
    }

    // 4. RecordStyleFeedback
    let resp = send_ipc(
        HgbRequest::RecordStyleFeedback {
            snippet: "use tokio::sync::mpsc for async actor mailbox".to_string(),
            accepted: true,
        },
        socket_path.clone(),
    )
    .await;
    assert!(matches!(resp, HgbResponse::StyleFeedbackRecorded));

    // 5. GetStyleGuidance
    let resp = send_ipc(HgbRequest::GetStyleGuidance, socket_path.clone()).await;
    match resp {
        HgbResponse::StyleGuidance(guidelines) => {
            assert!(guidelines.contains("use tokio::sync::mpsc"));
        }
        other => panic!("Unexpected GetStyleGuidance response: {:?}", other),
    }

    // 6. Ship
    let resp = send_ipc(HgbRequest::Ship { dry_run: true }, socket_path.clone()).await;
    match resp {
        HgbResponse::ShipReport {
            pr_title,
            security_passed,
            commits,
            ..
        } => {
            assert!(security_passed);
            assert!(!pr_title.is_empty());
            assert!(!commits.is_empty());
        }
        other => panic!("Unexpected Ship response: {:?}", other),
    }

    // Terminate daemon task & cleanup
    daemon_task.abort();
    let _ = std::fs::remove_file(&socket_path);
}
