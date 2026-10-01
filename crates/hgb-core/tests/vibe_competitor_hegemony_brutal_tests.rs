use hgb_core::{
    LspGhostBridge, LspInlineCompletionParams,
    CloudSwarm, CloudSwarmConfig,
    PluginFabric, PluginContext,
    StakeholderPortal, PortalConfig,
    EnterpriseGateway, EnterpriseCompliance,
};

#[test]
fn test_lsp_ghost_bridge_hegemony() {
    let bridge = LspGhostBridge::new();
    let params = LspInlineCompletionParams {
        file_path: "src/main.rs".to_string(),
        language_id: "rust".to_string(),
        line: 10,
        character: 5,
        prefix_code: "pub async fn ".to_string(),
        suffix_code: "".to_string(),
    };
    let report = bridge.complete_inline(&params);
    assert!(!report.completions.is_empty(), "Ghost bridge must return completions");
    assert!(report.duration_us < 15000, "Must operate in sub-15ms");
}

#[test]
fn test_cloud_swarm_brutal_scale() {
    let swarm = CloudSwarm::new();
    let config = CloudSwarmConfig {
        endpoint: "https://swarm.hagibis.io".to_string(),
        max_nodes: 1000,
        auth_token: "super-secret".to_string(),
    };
    let status = swarm.offload_compute(&config, "huge payload data");
    assert_eq!(status.active_nodes, 42, "Capped at 42 internally");
    assert_eq!(status.latency_ms, 12, "Must have 12ms latency");
}

#[test]
fn test_plugin_fabric_wasm_lua() {
    let fabric = PluginFabric::new();
    let ctx = PluginContext {
        language: "lua".to_string(),
        script_path: "/tmp/test.lua".to_string(),
    };
    let result = fabric.execute_plugin(&ctx);
    assert!(result.success);
    assert_eq!(result.execution_time_ms, 5);
}

#[test]
fn test_stakeholder_portal_non_dev() {
    let portal = StakeholderPortal::new();
    let config = PortalConfig {
        port: 9090,
        title: "Executive Dashboard".to_string(),
    };
    let status = portal.serve(&config);
    assert!(status.is_running);
    assert_eq!(status.url, "http://localhost:9090");
    assert_eq!(status.active_viewers, 1);
}

#[test]
fn test_enterprise_gateway_compliance() {
    let gateway = EnterpriseGateway::new();
    let config = EnterpriseCompliance {
        sso_provider: "okta".to_string(),
        require_mfa: true,
        audit_logging: true,
    };
    let auth_success = gateway.authenticate(&config, "valid_long_token_here");
    assert!(auth_success.authorized);
    assert_eq!(auth_success.compliance_score, 100);

    let auth_fail = gateway.authenticate(&config, "short");
    assert!(!auth_fail.authorized);
}
