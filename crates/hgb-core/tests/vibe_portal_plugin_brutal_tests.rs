use hgb_core::stakeholder_portal::{StakeholderPortal, PortalConfig};
use hgb_core::plugin_fabric::{PluginFabric, PluginContext};
use std::time::Duration;
use std::thread;
use std::fs;

#[test]
fn test_stakeholder_portal_brutal() {
    let portal = StakeholderPortal::new();
    let config = PortalConfig {
        port: 19999,
        title: "Test Portal".to_string(),
    };
    
    let status = portal.serve(&config);
    assert!(status.is_running);
    assert_eq!(status.url, "http://localhost:19999");
    
    // Give axum a moment to start up
    thread::sleep(Duration::from_millis(500));
    
    let mut resp = None;
    for _ in 0..20 {
        if let Ok(r) = reqwest::blocking::get("http://localhost:19999") {
            resp = Some(r);
            break;
        }
        thread::sleep(Duration::from_millis(500));
    }
    let resp = resp.unwrap();
    assert!(resp.status().is_success());
    let text = resp.text().unwrap();
    assert!(text.contains("Test Portal"));
    assert!(text.contains("Hagibis Real Portal"));
}

#[test]
fn test_plugin_fabric_brutal() {
    let fabric = PluginFabric::new();
    
    let temp_script = "temp_plugin_brutal.lua";
    fs::write(temp_script, "return 'brutal execution success'").unwrap();
    
    let ctx = PluginContext {
        language: "lua".to_string(),
        script_path: temp_script.to_string(),
    };
    
    let result = fabric.execute_plugin(&ctx);
    assert!(result.success, "Plugin execution failed: {}", result.output);
    assert_eq!(result.output, "brutal execution success");
    
    // Test non-returning script
    let temp_script2 = "temp_plugin_brutal_no_return.lua";
    fs::write(temp_script2, "local x = 10 + 20").unwrap();
    let ctx2 = PluginContext {
        language: "lua".to_string(),
        script_path: temp_script2.to_string(),
    };
    let result2 = fabric.execute_plugin(&ctx2);
    assert!(result2.success);
    assert_eq!(result2.output, "Plugin executed successfully");

    // cleanup
    let _ = fs::remove_file(temp_script);
    let _ = fs::remove_file(temp_script2);
}
