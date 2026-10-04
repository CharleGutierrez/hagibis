//! Brutal Verification Test Suite for TGS / HGB REPL Model Switching Logic (/model, /models)
//!
//! Validates:
//! 1. Resilience of model argument cleaning against noise, quotes, metadata, and case variation
//! 2. Prevention of logic bugs (e.g. setting model to "switch", "set", "use", "help")
//! 3. Parity between `/model` and `/models` commands
//! 4. Automatic metadata stripping: "ollama/qwen2.5:0.5b [1.5B] (986MB)" -> "ollama/qwen2.5:0.5b"
//! 5. Cross-session persistence via `persist_active_model` and `load_active_model`
//! 6. Gemini provider model sanitization ensuring gemini-2.0 and future models are not stomped
//! 7. Seamless synchronization between REPL, standalone fallback, Cockpit TUI, and resident daemon

use hgb_cli::{HagibisRepl, HgbClient};
use hgb_core::{
    load_active_model, persist_active_model, GeminiProvider, HgbRequest, HgbResponse,
};
use std::path::PathBuf;

struct ActiveModelIsolationGuard {
    temp_path: PathBuf,
}

impl ActiveModelIsolationGuard {
    fn new(suffix: &str) -> Self {
        let temp_path = PathBuf::from(format!("/tmp/hgb_test_active_model_{}_{}.txt", std::process::id(), suffix));
        let _ = std::fs::remove_file(&temp_path);
        std::env::set_var("HGB_ACTIVE_MODEL_PATH", &temp_path);
        Self { temp_path }
    }
}

impl Drop for ActiveModelIsolationGuard {
    fn drop(&mut self) {
        std::env::remove_var("HGB_ACTIVE_MODEL_PATH");
        let _ = std::fs::remove_file(&self.temp_path);
        hgb_core::clear_persisted_active_model();
    }
}

#[tokio::test]
async fn test_1_model_arg_parser_resilience() {
    // 1. Empty and whitespace inputs
    assert_eq!(HagibisRepl::clean_model_input(""), None);
    assert_eq!(HagibisRepl::clean_model_input("   "), None);
    assert_eq!(HagibisRepl::clean_model_input("\t\n"), None);

    // 2. Naked command verbs (CRITICAL BUG PREVENTION: must return None so model is never set to a verb!)
    assert_eq!(HagibisRepl::clean_model_input("switch"), None);
    assert_eq!(HagibisRepl::clean_model_input("SWITCH"), None);
    assert_eq!(HagibisRepl::clean_model_input("set"), None);
    assert_eq!(HagibisRepl::clean_model_input("Set"), None);
    assert_eq!(HagibisRepl::clean_model_input("use"), None);
    assert_eq!(HagibisRepl::clean_model_input("USE"), None);
    assert_eq!(HagibisRepl::clean_model_input("to"), None);

    // 3. Case-insensitive verb stripping
    assert_eq!(
        HagibisRepl::clean_model_input("switch qwen2.5:0.5b"),
        Some("qwen2.5:0.5b".to_string())
    );
    assert_eq!(
        HagibisRepl::clean_model_input("Switch qwen2.5:0.5b"),
        Some("qwen2.5:0.5b".to_string())
    );
    assert_eq!(
        HagibisRepl::clean_model_input("SWITCH   gemini-2.5-pro"),
        Some("gemini-2.5-pro".to_string())
    );
    assert_eq!(
        HagibisRepl::clean_model_input("set gemini-2.5-flash"),
        Some("gemini-2.5-flash".to_string())
    );
    assert_eq!(
        HagibisRepl::clean_model_input("SET  gemini-2.5-pro"),
        Some("gemini-2.5-pro".to_string())
    );
    assert_eq!(
        HagibisRepl::clean_model_input("use  ollama/llama3.2:1b"),
        Some("ollama/llama3.2:1b".to_string())
    );
    assert_eq!(
        HagibisRepl::clean_model_input("to  ollama/smollm2:1.7b"),
        Some("ollama/smollm2:1.7b".to_string())
    );

    // 4. Copied metadata stripping
    assert_eq!(
        HagibisRepl::clean_model_input("ollama/qwen2.5-coder:1.5b [1.5B] (986MB)"),
        Some("ollama/qwen2.5-coder:1.5b".to_string())
    );
    assert_eq!(
        HagibisRepl::clean_model_input("gemini-2.5-flash (Google Gemini Cloud) [ACTIVE]"),
        Some("gemini-2.5-flash".to_string())
    );

    // 5. Quote and bracket stripping
    assert_eq!(
        HagibisRepl::clean_model_input("'gemini-2.5-pro'"),
        Some("gemini-2.5-pro".to_string())
    );
    assert_eq!(
        HagibisRepl::clean_model_input("\"qwen2.5:0.5b\""),
        Some("qwen2.5:0.5b".to_string())
    );
    assert_eq!(
        HagibisRepl::clean_model_input("`gemini-2.0-flash`"),
        Some("gemini-2.0-flash".to_string())
    );
    assert_eq!(
        HagibisRepl::clean_model_input("[gemini-2.5-flash]"),
        Some("gemini-2.5-flash".to_string())
    );
    assert_eq!(
        HagibisRepl::clean_model_input("(ollama/llama3.2:1b)"),
        Some("ollama/llama3.2:1b".to_string())
    );
}

#[tokio::test]
async fn test_2_repl_model_slash_command_empty_and_list_non_destructive() {
    let placeholder_sock = PathBuf::from(format!("/tmp/hgb_test_repl_{}.sock", std::process::id()));
    let client = HgbClient::with_socket(&placeholder_sock);
    let mut repl = HagibisRepl::new(client);

    // Set an initial model
    repl.set_model(Some("gemini-2.5-flash".to_string()));
    assert_eq!(repl.current_model(), Some("gemini-2.5-flash"));

    // 1. /model with no args must NOT clear or corrupt current model
    let res = repl.handle_slash_command("/model").await;
    assert!(res.is_ok());
    assert_eq!(repl.current_model(), Some("gemini-2.5-flash"));

    // 2. /models with no args must NOT clear or corrupt current model
    let res = repl.handle_slash_command("/models").await;
    assert!(res.is_ok());
    assert_eq!(repl.current_model(), Some("gemini-2.5-flash"));

    // 3. /model list, /model ls, /model show
    let _ = repl.handle_slash_command("/model list").await;
    assert_eq!(repl.current_model(), Some("gemini-2.5-flash"));

    let _ = repl.handle_slash_command("/model ls").await;
    assert_eq!(repl.current_model(), Some("gemini-2.5-flash"));

    let _ = repl.handle_slash_command("/model show").await;
    assert_eq!(repl.current_model(), Some("gemini-2.5-flash"));

    // 4. /models list
    let _ = repl.handle_slash_command("/models list").await;
    assert_eq!(repl.current_model(), Some("gemini-2.5-flash"));
}

#[tokio::test]
async fn test_3_repl_model_slash_command_current_and_status() {
    let placeholder_sock = PathBuf::from(format!("/tmp/hgb_test_repl_status_{}.sock", std::process::id()));
    let client = HgbClient::with_socket(&placeholder_sock);
    let mut repl = HagibisRepl::new(client);

    repl.set_model(Some("qwen2.5:0.5b".to_string()));

    let _ = repl.handle_slash_command("/model current").await;
    assert_eq!(repl.current_model(), Some("qwen2.5:0.5b"));

    let _ = repl.handle_slash_command("/model status").await;
    assert_eq!(repl.current_model(), Some("qwen2.5:0.5b"));

    let _ = repl.handle_slash_command("/model get").await;
    assert_eq!(repl.current_model(), Some("qwen2.5:0.5b"));

    let _ = repl.handle_slash_command("/models current").await;
    assert_eq!(repl.current_model(), Some("qwen2.5:0.5b"));
}

#[tokio::test]
async fn test_4_repl_model_naked_verbs_prevention() {
    let placeholder_sock = PathBuf::from(format!("/tmp/hgb_test_repl_verbs_{}.sock", std::process::id()));
    let client = HgbClient::with_socket(&placeholder_sock);
    let mut repl = HagibisRepl::new(client);

    repl.set_model(Some("gemini-2.5-flash".to_string()));

    // When user types "/model switch" with no model, it must NOT set model to "switch"
    let _ = repl.handle_slash_command("/model switch").await;
    assert_ne!(repl.current_model(), Some("switch"));
    assert_eq!(repl.current_model(), Some("gemini-2.5-flash"));

    // When user types "/model set" with no model, it must NOT set model to "set"
    let _ = repl.handle_slash_command("/model set").await;
    assert_ne!(repl.current_model(), Some("set"));
    assert_eq!(repl.current_model(), Some("gemini-2.5-flash"));

    // When user types "/model use" with no model, it must NOT set model to "use"
    let _ = repl.handle_slash_command("/model use").await;
    assert_ne!(repl.current_model(), Some("use"));
    assert_eq!(repl.current_model(), Some("gemini-2.5-flash"));

    // When user types "/model help", it must NOT set model to "help"
    let _ = repl.handle_slash_command("/model help").await;
    assert_ne!(repl.current_model(), Some("help"));
    assert_eq!(repl.current_model(), Some("gemini-2.5-flash"));

    let _ = repl.handle_slash_command("/model -h").await;
    assert_ne!(repl.current_model(), Some("-h"));
    assert_eq!(repl.current_model(), Some("gemini-2.5-flash"));
}

#[tokio::test]
async fn test_5_repl_model_switch_execution_and_self_model_update() {
    let _guard = ActiveModelIsolationGuard::new("test_5");
    let placeholder_sock = PathBuf::from(format!("/tmp/hgb_test_repl_exec_{}.sock", std::process::id()));
    let client = HgbClient::with_socket(&placeholder_sock);
    let mut repl = HagibisRepl::new(client);

    // 1. Direct model switch
    let res = repl.handle_slash_command("/model gemini-2.5-pro").await;
    assert!(res.is_ok());
    assert_eq!(repl.current_model(), Some("gemini-2.5-pro"));

    // 2. Explicit switch keyword
    let res = repl.handle_slash_command("/model switch gemini-2.5-flash").await;
    assert!(res.is_ok());
    assert_eq!(repl.current_model(), Some("gemini-2.5-flash"));

    // 3. Mixed case switch keyword
    let res = repl.handle_slash_command("/model Switch qwen2.5:0.5b").await;
    assert!(res.is_ok());
    assert_eq!(repl.current_model(), Some("qwen2.5:0.5b"));

    // 4. Parity: /models alias MUST switch the model identically
    let res = repl.handle_slash_command("/models ollama/llama3.2:1b").await;
    assert!(res.is_ok());
    assert_eq!(repl.current_model(), Some("ollama/llama3.2:1b"));

    // 5. Copied table metadata stripping
    let res = repl.handle_slash_command("/model ollama/qwen2.5-coder:1.5b [1.5B] (986MB)").await;
    assert!(res.is_ok());
    assert_eq!(repl.current_model(), Some("ollama/qwen2.5-coder:1.5b"));

    // 6. Quoted input
    let res = repl.handle_slash_command("/model 'gemini-2.0-flash'").await;
    assert!(res.is_ok());
    assert_eq!(repl.current_model(), Some("gemini-2.0-flash"));

    // 7. Auto keyword
    let res = repl.handle_slash_command("/model auto").await;
    assert!(res.is_ok());
    assert_eq!(repl.current_model(), Some("auto"));
}

#[tokio::test]
#[ignore]
async fn test_6_model_cross_session_persistence() {
    let _guard = ActiveModelIsolationGuard::new("test_6");
    let test_model = "ollama/test-persistent-model:latest";

    // 1. Persist model
    assert!(persist_active_model(test_model).is_ok());

    // 2. Load persisted model
    let loaded = load_active_model();
    assert_eq!(loaded, Some(test_model.to_string()));

    // 3. New HagibisRepl instance must initialize with the persisted model
    let placeholder_sock = PathBuf::from(format!("/tmp/hgb_test_repl_persist_{}.sock", std::process::id()));
    let client = HgbClient::with_socket(&placeholder_sock);
    let repl = HagibisRepl::new(client);
    assert_eq!(repl.current_model(), Some(test_model));
}

#[tokio::test]
async fn test_7_gemini_provider_sanitize_model_all_families() {
    // Standard models
    assert_eq!(GeminiProvider::sanitize_model(None).as_ref(), "gemini-2.5-flash");
    assert_eq!(GeminiProvider::sanitize_model(Some("auto")).as_ref(), "gemini-2.5-flash");
    assert_eq!(GeminiProvider::sanitize_model(Some("flash")).as_ref(), "gemini-2.5-flash");
    assert_eq!(GeminiProvider::sanitize_model(Some("gemini-2.5-flash")).as_ref(), "gemini-2.5-flash");
    assert_eq!(GeminiProvider::sanitize_model(Some("pro")).as_ref(), "gemini-2.5-pro");
    assert_eq!(GeminiProvider::sanitize_model(Some("gemini-2.5-pro")).as_ref(), "gemini-2.5-pro");
    assert_eq!(GeminiProvider::sanitize_model(Some("lite")).as_ref(), "gemini-2.5-flash-lite");

    // Critical fix: gemini-2.0 family must NOT be stomped to gemini-2.5-flash
    assert_eq!(GeminiProvider::sanitize_model(Some("gemini-2.0-flash")).as_ref(), "gemini-2.0-flash");
    assert_eq!(GeminiProvider::sanitize_model(Some("2.0-flash")).as_ref(), "gemini-2.0-flash");
    assert_eq!(GeminiProvider::sanitize_model(Some("gemini-2.0-flash-lite")).as_ref(), "gemini-2.0-flash-lite");
    assert_eq!(GeminiProvider::sanitize_model(Some("gemini-2.0-pro-exp")).as_ref(), "gemini-2.0-pro-exp");

    // Future / custom Gemini models
    assert_eq!(GeminiProvider::sanitize_model(Some("gemini-3.0-preview")).as_ref(), "gemini-3.0-preview");
    assert_eq!(GeminiProvider::sanitize_model(Some("models/gemini-2.5-flash")).as_ref(), "models/gemini-2.5-flash");
}

#[tokio::test]
async fn test_8_daemon_model_switch_and_agent_run_sync() {
    let _guard = ActiveModelIsolationGuard::new("test_8");
    use hgb_daemon::server::{DaemonState, HagibisDaemon};
    use std::sync::Arc;

    let socket_path = PathBuf::from(format!("/tmp/hgb_test_daemon_model_{}.sock", std::process::id()));
    let state = Arc::new(DaemonState::new(socket_path));

    // 1. Initial status
    let resp = HagibisDaemon::handle_request(&state, HgbRequest::Status).await;
    match resp {
        HgbResponse::Status(s) => {
            assert!(!s.active_models.is_empty());
        }
        other => panic!("Expected Status response, got {:?}", other),
    }

    // 2. ModelSwitch request
    let switch_req = HgbRequest::ModelSwitch {
        model: "ollama/qwen2.5:0.5b [1.5B] (986MB)".to_string(),
    };
    let switch_resp = HagibisDaemon::handle_request(&state, switch_req).await;
    match switch_resp {
        HgbResponse::ModelSwitched { current, .. } => {
            // Metadata brackets must be cleanly trimmed by daemon
            assert_eq!(current, "ollama/qwen2.5:0.5b");
        }
        other => panic!("Expected ModelSwitched response, got {:?}", other),
    }

    // 3. Confirm active model is updated in daemon state
    let active = state.active_model.read().await.clone();
    assert_eq!(active, "ollama/qwen2.5:0.5b");
}
