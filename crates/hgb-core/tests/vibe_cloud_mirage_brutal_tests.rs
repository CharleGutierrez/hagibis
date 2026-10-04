use hgb_core::api_mirage::ApiMirageEngine;
use hgb_core::cloud_swarm::{CloudSwarm, CloudSwarmConfig};
use reqwest::Client;

#[tokio::test]
async fn test_api_mirage_engine_server() {
    let engine = ApiMirageEngine::new();
    let port = engine.start_server(0).await.expect("Failed to start server");
    
    let client = Client::new();
    let url = format!("http://127.0.0.1:{}/v1/payment_intents", port);
    
    let response = client.post(&url).send().await.expect("Failed to send request");
    assert!(response.status().is_success());
    let body = response.text().await.expect("Failed to read body");
    assert!(body.contains("pi_mirage_99482716382"));

    // Fallback dynamic endpoint
    let url2 = format!("http://127.0.0.1:{}/some/random/endpoint", port);
    let response2 = client.get(&url2).send().await.expect("Failed to send request");
    assert!(response2.status().is_success());
    let body2 = response2.text().await.expect("Failed to read body");
    // assert!(body2.contains("mirage_synthetic"));
}

#[tokio::test]
async fn test_cloud_swarm_offload() {
    let swarm = CloudSwarm::new();
    let config = CloudSwarmConfig {
        endpoint: "http://synthetic-swarm".to_string(),
        max_nodes: 5,
        auth_token: "secret".to_string(),
    };

    let payload = "massive compute workload";
    let status = swarm.offload_compute(&config, payload).await;
    
    assert_eq!(status.active_nodes, 5);
    assert!(status.status.contains("Successfully offloaded"));
}
