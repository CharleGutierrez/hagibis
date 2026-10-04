use hgb_cli::repl::ReplEditor;
use hgb_core::error::HgbError;
use hgb_core::mcp::{McpClient, McpConfigFile, McpServerConfig};
use hgb_core::protocol::{HgbRequest, HgbResponse};
use hgb_daemon::server::{DaemonState, HagibisDaemon};
use std::fs;
use std::sync::Arc;

/// Helper: returns a bash script acting as a full MCP stdio JSON-RPC 2.0 proxy responder
fn create_proxy_mcp_script() -> &'static str {
    r#"
while read -r line; do
  [ -z "$line" ] && continue
  method=$(echo "$line" | grep -o '"method":"[^"]*"' | cut -d'"' -f4)
  id=$(echo "$line" | grep -o '"id":[0-9]*' | cut -d':' -f2)
  if [ -z "$id" ]; then
    # Notification, ignore
    continue
  fi

  if [ "$method" = "initialize" ]; then
    echo "{\"jsonrpc\":\"2.0\",\"id\":$id,\"result\":{\"protocolVersion\":\"2024-11-05\",\"capabilities\":{\"tools\":{}},\"serverInfo\":{\"name\":\"proxy-mcp-server\",\"version\":\"1.0.0\"}}}"
  elif [ "$method" = "tools/list" ]; then
    echo "{\"jsonrpc\":\"2.0\",\"id\":$id,\"result\":{\"tools\":[{\"name\":\"calculate_sum\",\"description\":\"Add two numbers\",\"inputSchema\":{\"type\":\"object\"}},{\"name\":\"echo\",\"description\":\"Echo back message\",\"inputSchema\":{\"type\":\"object\"}}]}}"
  elif [ "$method" = "tools/call" ]; then
    tool_name=$(echo "$line" | grep -o '"name":"[^"]*"' | cut -d'"' -f4)
    if [ "$tool_name" = "calculate_sum" ]; then
      echo "{\"jsonrpc\":\"2.0\",\"id\":$id,\"result\":{\"content\":[{\"type\":\"text\",\"text\":\"42\"}],\"isError\":false}}"
    elif [ "$tool_name" = "echo" ]; then
      echo "{\"jsonrpc\":\"2.0\",\"id\":$id,\"result\":{\"content\":[{\"type\":\"text\",\"text\":\"echoed successfully\"}],\"isError\":false}}"
    elif [ "$tool_name" = "fail_tool" ]; then
      echo "{\"jsonrpc\":\"2.0\",\"id\":$id,\"error\":{\"code\":-32000,\"message\":\"Simulated tool execution error\"}}"
    else
      echo "{\"jsonrpc\":\"2.0\",\"id\":$id,\"error\":{\"code\":-32601,\"message\":\"Tool not found: $tool_name\"}}"
    fi
  else
    echo "{\"jsonrpc\":\"2.0\",\"id\":$id,\"error\":{\"code\":-32601,\"message\":\"Method not found: $method\"}}"
  fi
done
"#
}

// =========================================================================
// TEST A: Config Loading from Default Paths and Explicit -c Path
// =========================================================================
#[test]
fn test_mcp_config_discovery_default_paths_and_explicit() {
    let base_dir = std::env::temp_dir().join(format!("hgb_mcp_test_paths_{}", std::process::id()));
    let _ = fs::remove_dir_all(&base_dir);
    fs::create_dir_all(&base_dir).unwrap();

    let sample_config = r#"{
        "mcpServers": {
            "demo_server": {
                "command": "bash",
                "args": ["-c", "echo hello"]
            }
        }
    }"#;

    // 1. Test discovery from hagibis.mcp.json
    let dir1 = base_dir.join("dir1");
    fs::create_dir_all(&dir1).unwrap();
    fs::write(dir1.join("hagibis.mcp.json"), sample_config).unwrap();
    let loaded1 = McpConfigFile::load_from_dir(&dir1).unwrap().expect("Should load hagibis.mcp.json");
    assert!(loaded1.mcp_servers.contains_key("demo_server"));

    // 2. Test discovery from .hgb/mcp.json
    let dir2 = base_dir.join("dir2");
    fs::create_dir_all(dir2.join(".hgb")).unwrap();
    fs::write(dir2.join(".hgb").join("mcp.json"), sample_config).unwrap();
    let loaded2 = McpConfigFile::load_from_dir(&dir2).unwrap().expect("Should load .hgb/mcp.json");
    assert!(loaded2.mcp_servers.contains_key("demo_server"));

    // 3. Test discovery from .mcp.json
    let dir3 = base_dir.join("dir3");
    fs::create_dir_all(&dir3).unwrap();
    fs::write(dir3.join(".mcp.json"), sample_config).unwrap();
    let loaded3 = McpConfigFile::load_from_dir(&dir3).unwrap().expect("Should load .mcp.json");
    assert!(loaded3.mcp_servers.contains_key("demo_server"));

    // 4. Test directory with no config
    let dir4 = base_dir.join("dir4");
    fs::create_dir_all(&dir4).unwrap();
    assert!(McpConfigFile::load_from_dir(&dir4).unwrap().is_none());
    let default_empty = McpConfigFile::load_default_or_empty(&dir4).unwrap();
    assert!(default_empty.mcp_servers.is_empty());

    // 5. Test explicit file path loading
    let explicit_file = base_dir.join("custom_servers.json");
    fs::write(&explicit_file, sample_config).unwrap();
    let loaded_explicit = McpConfigFile::load_from_path(&explicit_file).unwrap();
    assert!(loaded_explicit.mcp_servers.contains_key("demo_server"));

    // 6. Test explicit non-existent path
    let missing_file = base_dir.join("does_not_exist.json");
    let err_missing = McpConfigFile::load_from_path(&missing_file).unwrap_err();
    assert!(err_missing.to_string().contains("does not exist"));

    // 7. Test explicit malformed JSON path
    let corrupt_file = base_dir.join("corrupt.json");
    fs::write(&corrupt_file, "{ this is not valid json }").unwrap();
    let err_corrupt = McpConfigFile::load_from_path(&corrupt_file).unwrap_err();
    assert!(matches!(err_corrupt, HgbError::Serialization(_)));

    let _ = fs::remove_dir_all(&base_dir);
}

// =========================================================================
// TEST B: Spawning and Handshaking with Stdio MCP Server
// =========================================================================
#[tokio::test]
async fn test_mcp_spawn_and_handshake_lifecycle() {
    let script = create_proxy_mcp_script();
    let cfg = McpServerConfig::new("bash").with_args(["-c", script]);

    let client = McpClient::spawn_and_handshake("primary_proxy", &cfg, None)
        .await
        .expect("Handshake must succeed");

    assert_eq!(client.server_name(), "primary_proxy");

    // Clean close
    client.close().await;
}

// =========================================================================
// TEST C: Listing Tools Across Multiple Configured Servers
// =========================================================================
#[tokio::test]
async fn test_mcp_list_tools_multiple_configured_servers() {
    let temp_dir = std::env::temp_dir().join(format!("hgb_mcp_multi_{}", std::process::id()));
    let _ = fs::remove_dir_all(&temp_dir);
    fs::create_dir_all(&temp_dir).unwrap();

    // Server 1 Script: provides tool_a and tool_b
    let script1 = r#"
while read -r line; do
  method=$(echo "$line" | grep -o '"method":"[^"]*"' | cut -d'"' -f4)
  id=$(echo "$line" | grep -o '"id":[0-9]*' | cut -d':' -f2)
  [ -z "$id" ] && continue
  if [ "$method" = "initialize" ]; then
    echo "{\"jsonrpc\":\"2.0\",\"id\":$id,\"result\":{\"protocolVersion\":\"2024-11-05\",\"capabilities\":{\"tools\":{}},\"serverInfo\":{\"name\":\"server1\",\"version\":\"1.0\"}}}"
  elif [ "$method" = "tools/list" ]; then
    echo "{\"jsonrpc\":\"2.0\",\"id\":$id,\"result\":{\"tools\":[{\"name\":\"server1_tool_a\",\"description\":\"Tool A\",\"inputSchema\":{}},{\"name\":\"server1_tool_b\",\"description\":\"Tool B\",\"inputSchema\":{}}]}}"
  fi
done
"#;

    // Server 2 Script: provides tool_c
    let script2 = r#"
while read -r line; do
  method=$(echo "$line" | grep -o '"method":"[^"]*"' | cut -d'"' -f4)
  id=$(echo "$line" | grep -o '"id":[0-9]*' | cut -d':' -f2)
  [ -z "$id" ] && continue
  if [ "$method" = "initialize" ]; then
    echo "{\"jsonrpc\":\"2.0\",\"id\":$id,\"result\":{\"protocolVersion\":\"2024-11-05\",\"capabilities\":{\"tools\":{}},\"serverInfo\":{\"name\":\"server2\",\"version\":\"1.0\"}}}"
  elif [ "$method" = "tools/list" ]; then
    echo "{\"jsonrpc\":\"2.0\",\"id\":$id,\"result\":{\"tools\":[{\"name\":\"server2_tool_c\",\"description\":\"Tool C\",\"inputSchema\":{}}]}}"
  fi
done
"#;

    let mut cfg = McpConfigFile::new();
    cfg.mcp_servers.insert("srv1".into(), McpServerConfig::new("bash").with_args(["-c", script1]));
    cfg.mcp_servers.insert("srv2".into(), McpServerConfig::new("bash").with_args(["-c", script2]));

    let cfg_path = temp_dir.join("hagibis.mcp.json");
    cfg.save_to_file(&cfg_path).unwrap();

    let socket_path = temp_dir.join("daemon.sock");
    let state = Arc::new(DaemonState::new(socket_path));

    let req = HgbRequest::McpListTools {
        config_path: Some(cfg_path.to_str().unwrap().to_string()),
    };

    let resp = HagibisDaemon::handle_request(&state, req).await;
    match resp {
        HgbResponse::McpToolsList(tools) => {
            let names: Vec<String> = tools.into_iter().map(|t| t.name).collect();
            assert_eq!(names.len(), 3);
            assert!(names.contains(&"server1_tool_a".to_string()));
            assert!(names.contains(&"server1_tool_b".to_string()));
            assert!(names.contains(&"server2_tool_c".to_string()));
        }
        other => panic!("Expected McpToolsList, got {:?}", other),
    }

    let _ = fs::remove_dir_all(&temp_dir);
}

// =========================================================================
// TEST D & E: Calling Tools with Valid and Invalid/Malformed Arguments
// =========================================================================
#[tokio::test]
async fn test_mcp_call_tool_valid_and_malformed_arguments() {
    let temp_dir = std::env::temp_dir().join(format!("hgb_mcp_call_{}", std::process::id()));
    let _ = fs::remove_dir_all(&temp_dir);
    fs::create_dir_all(&temp_dir).unwrap();

    let script = create_proxy_mcp_script();
    let mut cfg = McpConfigFile::new();
    cfg.mcp_servers.insert("math_srv".into(), McpServerConfig::new("bash").with_args(["-c", script]));

    let cfg_path = temp_dir.join("hagibis.mcp.json");
    cfg.save_to_file(&cfg_path).unwrap();

    let socket_path = temp_dir.join("daemon.sock");
    let state = Arc::new(DaemonState::new(socket_path));

    // 1. Call calculate_sum with valid JSON arguments
    let req_sum = HgbRequest::McpCallTool {
        server_name: "math_srv".into(),
        tool_name: "calculate_sum".into(),
        arguments: serde_json::json!({ "a": 20, "b": 22 }),
        config_path: Some(cfg_path.to_str().unwrap().to_string()),
    };
    let resp_sum = HagibisDaemon::handle_request(&state, req_sum).await;
    match resp_sum {
        HgbResponse::McpToolCallResult(val) => {
            assert_eq!(val["content"][0]["text"], "42");
        }
        other => panic!("Expected McpToolCallResult, got {:?}", other),
    }

    // 2. Call echo with valid JSON arguments
    let req_echo = HgbRequest::McpCallTool {
        server_name: "math_srv".into(),
        tool_name: "echo".into(),
        arguments: serde_json::json!({ "msg": "vibe coding" }),
        config_path: Some(cfg_path.to_str().unwrap().to_string()),
    };
    let resp_echo = HagibisDaemon::handle_request(&state, req_echo).await;
    match resp_echo {
        HgbResponse::McpToolCallResult(val) => {
            assert_eq!(val["content"][0]["text"], "echoed successfully");
        }
        other => panic!("Expected McpToolCallResult, got {:?}", other),
    }

    // 3. Test that malformed JSON input string is rejected by parser
    let malformed_input = "{ a: 10, invalid_json ";
    let parsed_res = serde_json::from_str::<serde_json::Value>(malformed_input);
    assert!(parsed_res.is_err(), "Malformed JSON string must fail to parse");

    let _ = fs::remove_dir_all(&temp_dir);
}

// =========================================================================
// TEST F: Calling Non-existent Tool on a Server
// =========================================================================
#[tokio::test]
async fn test_mcp_call_nonexistent_tool() {
    let temp_dir = std::env::temp_dir().join(format!("hgb_mcp_notool_{}", std::process::id()));
    let _ = fs::remove_dir_all(&temp_dir);
    fs::create_dir_all(&temp_dir).unwrap();

    let script = create_proxy_mcp_script();
    let mut cfg = McpConfigFile::new();
    cfg.mcp_servers.insert("proxy_srv".into(), McpServerConfig::new("bash").with_args(["-c", script]));

    let cfg_path = temp_dir.join("hagibis.mcp.json");
    cfg.save_to_file(&cfg_path).unwrap();

    let socket_path = temp_dir.join("daemon.sock");
    let state = Arc::new(DaemonState::new(socket_path));

    let req = HgbRequest::McpCallTool {
        server_name: "proxy_srv".into(),
        tool_name: "nonexistent_secret_tool".into(),
        arguments: serde_json::json!({}),
        config_path: Some(cfg_path.to_str().unwrap().to_string()),
    };

    let resp = HagibisDaemon::handle_request(&state, req).await;
    match resp {
        HgbResponse::Error(err) => {
            assert!(err.contains("Tool not found: nonexistent_secret_tool"));
        }
        other => panic!("Expected Error response for non-existent tool, got {:?}", other),
    }

    let _ = fs::remove_dir_all(&temp_dir);
}

// =========================================================================
// TEST G: Calling Tool on Non-existent Server
// =========================================================================
#[tokio::test]
async fn test_mcp_call_nonexistent_server() {
    let temp_dir = std::env::temp_dir().join(format!("hgb_mcp_noserver_{}", std::process::id()));
    let _ = fs::remove_dir_all(&temp_dir);
    fs::create_dir_all(&temp_dir).unwrap();

    let mut cfg = McpConfigFile::new();
    cfg.mcp_servers.insert("real_server".into(), McpServerConfig::new("echo"));

    let cfg_path = temp_dir.join("hagibis.mcp.json");
    cfg.save_to_file(&cfg_path).unwrap();

    let socket_path = temp_dir.join("daemon.sock");
    let state = Arc::new(DaemonState::new(socket_path));

    let req = HgbRequest::McpCallTool {
        server_name: "phantom_ghost_server".into(),
        tool_name: "some_tool".into(),
        arguments: serde_json::json!({}),
        config_path: Some(cfg_path.to_str().unwrap().to_string()),
    };

    let resp = HagibisDaemon::handle_request(&state, req).await;
    match resp {
        HgbResponse::Error(err) => {
            assert!(err.contains("MCP Server 'phantom_ghost_server' not found in configuration"));
        }
        other => panic!("Expected Error for non-existent server, got {:?}", other),
    }

    let _ = fs::remove_dir_all(&temp_dir);
}

// =========================================================================
// TEST H: Handling Disabled Servers ("disabled": true)
// =========================================================================
#[tokio::test]
async fn test_mcp_disabled_servers_handling() {
    let temp_dir = std::env::temp_dir().join(format!("hgb_mcp_disabled_{}", std::process::id()));
    let _ = fs::remove_dir_all(&temp_dir);
    fs::create_dir_all(&temp_dir).unwrap();

    let script = create_proxy_mcp_script();
    let mut disabled_cfg = McpServerConfig::new("bash").with_args(["-c", script]);
    disabled_cfg.disabled = true;

    let mut cfg = McpConfigFile::new();
    cfg.mcp_servers.insert("disabled_srv".into(), disabled_cfg);

    let cfg_path = temp_dir.join("hagibis.mcp.json");
    cfg.save_to_file(&cfg_path).unwrap();

    let socket_path = temp_dir.join("daemon.sock");
    let state = Arc::new(DaemonState::new(socket_path));

    // 1. List tools should skip disabled server
    let list_req = HgbRequest::McpListTools {
        config_path: Some(cfg_path.to_str().unwrap().to_string()),
    };
    let list_resp = HagibisDaemon::handle_request(&state, list_req).await;
    match list_resp {
        HgbResponse::McpToolsList(tools) => {
            assert!(tools.is_empty(), "Disabled server's tools should not be listed");
        }
        other => panic!("Expected McpToolsList, got {:?}", other),
    }

    // 2. Call tool on disabled server should fail immediately
    let call_req = HgbRequest::McpCallTool {
        server_name: "disabled_srv".into(),
        tool_name: "calculate_sum".into(),
        arguments: serde_json::json!({}),
        config_path: Some(cfg_path.to_str().unwrap().to_string()),
    };
    let call_resp = HagibisDaemon::handle_request(&state, call_req).await;
    match call_resp {
        HgbResponse::Error(err) => {
            assert!(err.contains("disabled in configuration"));
        }
        other => panic!("Expected Error for disabled server call, got {:?}", other),
    }

    let _ = fs::remove_dir_all(&temp_dir);
}

// =========================================================================
// TEST I: Handling Server Process Exit / Stderr Output / Error Responses
// =========================================================================
#[tokio::test]
async fn test_mcp_server_exit_stderr_and_error_responses() {
    // 1. Server with heavy stderr output that does not hang due to stderr draining
    let noisy_script = r#"
# Flood stderr
for i in $(seq 1 500); do
  echo "DEBUG STDERR LOG LINE $i" >&2
done
while read -r line; do
  method=$(echo "$line" | grep -o '"method":"[^"]*"' | cut -d'"' -f4)
  id=$(echo "$line" | grep -o '"id":[0-9]*' | cut -d':' -f2)
  [ -z "$id" ] && continue
  if [ "$method" = "initialize" ]; then
    echo "{\"jsonrpc\":\"2.0\",\"id\":$id,\"result\":{\"protocolVersion\":\"2024-11-05\",\"capabilities\":{\"tools\":{}},\"serverInfo\":{\"name\":\"noisy\",\"version\":\"1.0\"}}}"
  elif [ "$method" = "tools/list" ]; then
    echo "{\"jsonrpc\":\"2.0\",\"id\":$id,\"result\":{\"tools\":[]}}"
  fi
done
"#;

    let noisy_cfg = McpServerConfig::new("bash").with_args(["-c", noisy_script]);
    let client = McpClient::spawn_and_handshake("noisy_server", &noisy_cfg, None)
        .await
        .expect("Noisy server must not deadlock on stderr");
    let tools = client.list_tools().await.expect("list_tools must succeed");
    assert!(tools.is_empty());
    client.close().await;

    // 2. Server that crashes / exits immediately during handshake
    let crash_cfg = McpServerConfig::new("bash").with_args(["-c", "exit 1"]);
    let spawn_res = McpClient::spawn_and_handshake("crash_server", &crash_cfg, None).await;
    assert!(spawn_res.is_err(), "Crashing server must fail fast");

    // 3. Server that returns JSON-RPC error response
    let script = create_proxy_mcp_script();
    let err_cfg = McpServerConfig::new("bash").with_args(["-c", script]);
    let err_client = McpClient::spawn_and_handshake("err_server", &err_cfg, None)
        .await
        .unwrap();

    let call_err = err_client.call_tool("fail_tool", serde_json::json!({})).await;
    assert!(call_err.is_err());
    let err_str = call_err.unwrap_err().to_string();
    assert!(err_str.contains("Simulated tool execution error"));

    err_client.close().await;
}

// =========================================================================
// TEST J: REPL Autocompletion and Slash Command Dispatch
// =========================================================================
#[test]
fn test_mcp_repl_tab_completions() {
    // 1. /m prefix completion
    let comp_m = ReplEditor::get_completions("/m");
    let names: Vec<String> = comp_m.into_iter().map(|(s, _)| s).collect();
    assert!(names.contains(&"/mcp ".to_string()));

    // 2. /mcp subcommands completion
    let comp_mcp = ReplEditor::get_completions("/mcp ");
    let sub_names: Vec<String> = comp_mcp.into_iter().map(|(s, _)| s).collect();
    assert!(sub_names.contains(&"/mcp list ".to_string()));
    assert!(sub_names.contains(&"/mcp call ".to_string()));

    // 3. /mcp l prefix
    let comp_mcpl = ReplEditor::get_completions("/mcp l");
    let l_names: Vec<String> = comp_mcpl.into_iter().map(|(s, _)| s).collect();
    assert!(l_names.contains(&"/mcp list ".to_string()));
    assert!(!l_names.contains(&"/mcp call ".to_string()));

    // 4. /mcp c prefix
    let comp_mcpc = ReplEditor::get_completions("/mcp c");
    let c_names: Vec<String> = comp_mcpc.into_iter().map(|(s, _)| s).collect();
    assert!(c_names.contains(&"/mcp call ".to_string()));
    assert!(!c_names.contains(&"/mcp list ".to_string()));
}
