use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum RolloutHealthVerdict {
    Greenlight,
    Warning,
    RollbackTriggered,
    HotfixDispatched,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TelemetrySample {
    pub timestamp_epoch_ms: u64,
    pub request_count: u64,
    pub error_count: u64,
    pub p50_latency_ms: f32,
    pub p95_latency_ms: f32,
    pub p99_latency_ms: f32,
    pub status_5xx_count: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RolloutWatchConfig {
    pub max_error_rate_pct: f32,       // e.g. 0.5% (0.005)
    pub max_p99_latency_ms: f32,       // e.g. 250.0 ms
    pub max_5xx_rate_pct: f32,         // e.g. 0.1%
    pub auto_rollback_enabled: bool,
}

impl Default for RolloutWatchConfig {
    fn default() -> Self {
        Self {
            max_error_rate_pct: 1.0,   // 1.0% threshold
            max_p99_latency_ms: 300.0,
            max_5xx_rate_pct: 0.5,
            auto_rollback_enabled: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RolloutWatchReport {
    pub deployment_id: String,
    pub sample_count: usize,
    pub current_error_rate_pct: f32,
    pub baseline_error_rate_pct: f32,
    pub current_p99_ms: f32,
    pub verdict: RolloutHealthVerdict,
    pub anomaly_detected: bool,
    pub remediation_action: String,
    pub rollback_command: Option<String>,
}

pub struct RolloutHealthWatch;

impl RolloutHealthWatch {
    pub fn new() -> Self {
        Self
    }

    /// Evaluates telemetry samples against baseline and returns health verdict
    pub fn evaluate(
        deployment_id: &str,
        samples: &[TelemetrySample],
        baseline_samples: &[TelemetrySample],
        config: Option<RolloutWatchConfig>,
    ) -> RolloutWatchReport {
        let cfg = config.unwrap_or_default();

        let (curr_reqs, curr_errs, max_p99, curr_5xx) = samples.iter().fold((0u64, 0u64, 0.0f32, 0u64), |acc, s| {
            (
                acc.0 + s.request_count,
                acc.1 + s.error_count,
                acc.2.max(s.p99_latency_ms),
                acc.3 + s.status_5xx_count,
            )
        });

        let current_error_rate_pct = if curr_reqs > 0 {
            (curr_errs as f32 / curr_reqs as f32) * 100.0
        } else {
            0.0
        };

        let current_5xx_rate_pct = if curr_reqs > 0 {
            (curr_5xx as f32 / curr_reqs as f32) * 100.0
        } else {
            0.0
        };

        let (base_reqs, base_errs) = baseline_samples.iter().fold((0u64, 0u64), |acc, s| {
            (acc.0 + s.request_count, acc.1 + s.error_count)
        });

        let baseline_error_rate_pct = if base_reqs > 0 {
            (base_errs as f32 / base_reqs as f32) * 100.0
        } else {
            0.0
        };

        let is_error_spike = current_error_rate_pct > cfg.max_error_rate_pct || current_error_rate_pct > (baseline_error_rate_pct * 3.0).max(1.0);
        let is_latency_spike = max_p99 > cfg.max_p99_latency_ms;
        let is_5xx_spike = current_5xx_rate_pct > cfg.max_5xx_rate_pct;

        let anomaly_detected = is_error_spike || is_latency_spike || is_5xx_spike;

        let (verdict, remediation, rollback_cmd) = if is_error_spike || is_5xx_spike {
            (
                RolloutHealthVerdict::RollbackTriggered,
                format!("CRITICAL: Error rate reached {:.2}% (threshold: {:.2}%). Automated rollback triggered.", current_error_rate_pct, cfg.max_error_rate_pct),
                Some(format!("git revert HEAD && git push origin main && hgb deploy --rollback {}", deployment_id)),
            )
        } else if is_latency_spike {
            (
                RolloutHealthVerdict::Warning,
                format!("WARNING: p99 latency spiked to {:.1}ms (threshold: {:.1}ms). Monitor connection pools and memory.", max_p99, cfg.max_p99_latency_ms),
                None,
            )
        } else {
            (
                RolloutHealthVerdict::Greenlight,
                "HEALTHY: Deployment telemetry matches healthy baseline parameters. All service metrics green.".to_string(),
                None,
            )
        };

        RolloutWatchReport {
            deployment_id: deployment_id.to_string(),
            sample_count: samples.len(),
            current_error_rate_pct,
            baseline_error_rate_pct,
            current_p99_ms: max_p99,
            verdict,
            anomaly_detected,
            remediation_action: remediation,
            rollback_command: rollback_cmd,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rollout_health_watch_healthy() {
        let samples = vec![
            TelemetrySample {
                timestamp_epoch_ms: 1000,
                request_count: 500,
                error_count: 1,
                p50_latency_ms: 12.0,
                p95_latency_ms: 35.0,
                p99_latency_ms: 60.0,
                status_5xx_count: 0,
            }
        ];
        let rep = RolloutHealthWatch::evaluate("dep-101", &samples, &samples, None);
        assert_eq!(rep.verdict, RolloutHealthVerdict::Greenlight);
        assert!(!rep.anomaly_detected);
        assert!(rep.rollback_command.is_none());
    }

    #[test]
    fn test_rollout_health_watch_critical_spike() {
        let baseline = vec![
            TelemetrySample {
                timestamp_epoch_ms: 1000,
                request_count: 1000,
                error_count: 2,
                p50_latency_ms: 10.0,
                p95_latency_ms: 25.0,
                p99_latency_ms: 50.0,
                status_5xx_count: 0,
            }
        ];
        let bad_samples = vec![
            TelemetrySample {
                timestamp_epoch_ms: 2000,
                request_count: 1000,
                error_count: 80, // 8% error rate!
                p50_latency_ms: 20.0,
                p95_latency_ms: 150.0,
                p99_latency_ms: 450.0,
                status_5xx_count: 40,
            }
        ];
        let rep = RolloutHealthWatch::evaluate("dep-102", &bad_samples, &baseline, None);
        assert_eq!(rep.verdict, RolloutHealthVerdict::RollbackTriggered);
        assert!(rep.anomaly_detected);
        assert!(rep.rollback_command.is_some());
    }
}
