//! # Phantom Swarm Traffic Simulator (`hgb-nextgen`)
//!
//! Autonomous stress-testing & load simulation engine:
//! - Configurable concurrent virtual client bots (1 to 20 concurrent workers)
//! - Generates high-frequency HTTP GET/POST and WebSocket frames against endpoints
//! - Computes throughput (req/sec) and latency percentiles (min, avg, p50, p95, p99, max)
//! - Flags HTTP 4xx/5xx errors, latency spikes, and connection anomalies

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Latency statistics and percentiles (in milliseconds)
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LatencyPercentiles {
    pub min_ms: f64,
    pub avg_ms: f64,
    pub p50_ms: f64,
    pub p95_ms: f64,
    pub p99_ms: f64,
    pub max_ms: f64,
}

/// Configuration for a Phantom Swarm simulation run
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PhantomSwarmConfig {
    pub target_url: String,
    pub concurrency: usize, // 1 to 20 virtual bots
    pub request_count: usize,
    pub http_method: String,
    pub payload: Option<String>,
    pub timeout_ms: u64,
}

impl Default for PhantomSwarmConfig {
    fn default() -> Self {
        Self {
            target_url: "http://127.0.0.1:3000/api/health".to_string(),
            concurrency: 5,
            request_count: 100,
            http_method: "GET".to_string(),
            payload: None,
            timeout_ms: 1000,
        }
    }
}

/// Complete report returned after a phantom traffic run
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PhantomSwarmReport {
    pub target_url: String,
    pub concurrency: usize,
    pub total_requests: usize,
    pub successful_requests: usize,
    pub failed_requests: usize,
    pub requests_per_second: f64,
    pub duration_ms: u64,
    pub latency: LatencyPercentiles,
    pub status_distribution: HashMap<u16, usize>,
    pub anomalies: Vec<String>,
}

/// Phantom Swarm Load & Stress Simulator
pub struct PhantomSwarmEngine {
    client: reqwest::Client,
}

impl Default for PhantomSwarmEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl PhantomSwarmEngine {
    pub fn new() -> Self {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_millis(1500))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self { client }
    }

    /// Calculate exact latency percentiles from measured sample points
    pub fn calculate_percentiles(mut latencies: Vec<f64>) -> LatencyPercentiles {
        if latencies.is_empty() {
            return LatencyPercentiles {
                min_ms: 0.0,
                avg_ms: 0.0,
                p50_ms: 0.0,
                p95_ms: 0.0,
                p99_ms: 0.0,
                max_ms: 0.0,
            };
        }

        latencies.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let count = latencies.len();
        let sum: f64 = latencies.iter().sum();
        let avg = sum / count as f64;

        let p50_idx = ((count as f64 * 0.50).round() as usize).min(count - 1);
        let p95_idx = ((count as f64 * 0.95).round() as usize).min(count - 1);
        let p99_idx = ((count as f64 * 0.99).round() as usize).min(count - 1);

        LatencyPercentiles {
            min_ms: latencies[0],
            avg_ms: avg,
            p50_ms: latencies[p50_idx],
            p95_ms: latencies[p95_idx],
            p99_ms: latencies[p99_idx],
            max_ms: latencies[count - 1],
        }
    }

    /// Execute simulated concurrent swarm traffic against target
    pub async fn simulate_traffic(&self, config: PhantomSwarmConfig) -> PhantomSwarmReport {
        let start = std::time::Instant::now();
        let concurrency = config.concurrency.clamp(1, 20);
        let reqs_per_bot = config.request_count / concurrency;

        let mut handles = Vec::new();

        for _ in 0..concurrency {
            let client = self.client.clone();
            let target_url = config.target_url.clone();
            let timeout_ms = config.timeout_ms;
            let reqs = reqs_per_bot.max(1);
            let method = config.http_method.clone();
            let payload = config.payload.clone();

            let handle = tokio::spawn(async move {
                let mut latencies = Vec::with_capacity(reqs);
                let mut status_map: HashMap<u16, usize> = HashMap::new();
                let mut successful = 0;
                let mut failed = 0;

                for _ in 0..reqs {
                    let req_start = std::time::Instant::now();
                    
                    let mut builder = match method.as_str() {
                        "POST" => client.post(&target_url),
                        "PUT" => client.put(&target_url),
                        "DELETE" => client.delete(&target_url),
                        _ => client.get(&target_url),
                    };

                    if let Some(p) = &payload {
                        builder = builder.body(p.clone());
                    }

                    let res = builder
                        .timeout(std::time::Duration::from_millis(timeout_ms))
                        .send().await;
                        
                    let elapsed_ms = req_start.elapsed().as_secs_f64() * 1000.0;
                    latencies.push(elapsed_ms);

                    match res {
                        Ok(resp) => {
                            let status = resp.status().as_u16();
                            *status_map.entry(status).or_insert(0) += 1;
                            if status < 400 {
                                successful += 1;
                            } else {
                                failed += 1;
                            }
                        }
                        Err(_) => {
                            *status_map.entry(503).or_insert(0) += 1;
                            failed += 1;
                        }
                    }
                }
                (latencies, status_map, successful, failed)
            });
            handles.push(handle);
        }

        let mut all_latencies = Vec::with_capacity(config.request_count);
        let mut combined_status_map: HashMap<u16, usize> = HashMap::new();
        let mut total_successful = 0;
        let mut total_failed = 0;

        for handle in handles {
            if let Ok((latencies, status_map, successful, failed)) = handle.await {
                all_latencies.extend(latencies);
                for (status, count) in status_map {
                    *combined_status_map.entry(status).or_insert(0) += count;
                }
                total_successful += successful;
                total_failed += failed;
            }
        }

        let total_duration_ms = start.elapsed().as_millis().max(1) as u64;
        let rps = (config.request_count as f64) / (total_duration_ms as f64 / 1000.0);

        let latency_stats = Self::calculate_percentiles(all_latencies);

        // Detect anomalies
        let mut anomalies = Vec::new();
        if latency_stats.p95_ms > 200.0 {
            anomalies.push(format!("High p95 Latency Spike: {:.2}ms", latency_stats.p95_ms));
        }
        if let Some(&err_5xx) = combined_status_map.get(&500).or_else(|| combined_status_map.get(&503)) {
            if err_5xx > 0 {
                anomalies.push(format!("Server Errors Detected: {} requests failed with 5xx", err_5xx));
            }
        }

        PhantomSwarmReport {
            target_url: config.target_url,
            concurrency,
            total_requests: config.request_count,
            successful_requests: total_successful,
            failed_requests: total_failed,
            requests_per_second: rps,
            duration_ms: total_duration_ms,
            latency: latency_stats,
            status_distribution: combined_status_map,
            anomalies,
        }
    }

    /// Synchronously execute phantom traffic run using current or fresh tokio runtime
    pub fn simulate_traffic_sync(&self, config: PhantomSwarmConfig) -> PhantomSwarmReport {
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            tokio::task::block_in_place(|| {
                handle.block_on(self.simulate_traffic(config))
            })
        } else if let Ok(rt) = tokio::runtime::Builder::new_current_thread().enable_all().build() {
            rt.block_on(self.simulate_traffic(config))
        } else {
            unreachable!("Could not get or create tokio runtime")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_percentile_calculations() {
        let latencies = vec![10.0, 12.0, 15.0, 20.0, 25.0, 30.0, 50.0, 80.0, 120.0, 500.0];
        let p = PhantomSwarmEngine::calculate_percentiles(latencies);

        assert_eq!(p.min_ms, 10.0);
        assert_eq!(p.max_ms, 500.0);
        assert!(p.p50_ms >= 20.0 && p.p50_ms <= 30.0);
        assert!(p.p95_ms >= 120.0);
        assert!(p.avg_ms > 80.0);
    }

    #[tokio::test]
    async fn test_phantom_swarm_traffic_simulation() {
        let engine = PhantomSwarmEngine::new();
        let config = PhantomSwarmConfig {
            target_url: "http://127.0.0.1:9999/probe".to_string(),
            concurrency: 4,
            request_count: 8,
            http_method: "GET".to_string(),
            payload: None,
            timeout_ms: 50,
        };

        let report = engine.simulate_traffic(config).await;
        assert_eq!(report.concurrency, 4);
        assert_eq!(report.total_requests, 8);
        assert!(report.latency.max_ms >= 0.0);
        assert!(!report.anomalies.is_empty()); // 503 offline anomaly detected
    }
}
