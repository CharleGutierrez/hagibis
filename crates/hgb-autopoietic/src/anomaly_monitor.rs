//! # OpenTelemetry Anomaly & Regression Detector (`anomaly_monitor`)
//!
//! Continuous live telemetry ingestion, percentile tracking (P50, P90, P95, P99),
//! panic rate calculation, CPU thermal runaway guarding, instruction/cache thrashing
//! detection, and deterministic source-code frame tracing.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

use hgb_core::error::{HgbError, Result};

pub const DEFAULT_P99_LATENCY_THRESHOLD_MS: f64 = 200.0;
pub const DEFAULT_PANIC_RATE_THRESHOLD: f64 = 0.01;
pub const DEFAULT_CPU_THERMAL_THRESHOLD_C: f64 = 78.0;
pub const DEFAULT_CACHE_MISS_RATIO_THRESHOLD: f64 = 0.35;
pub const DEFAULT_MIN_WINDOW_SAMPLES: usize = 10;
pub const DEFAULT_MAX_WINDOW_SAMPLES: usize = 10000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum AnomalySeverity {
    Low,
    Medium,
    High,
    Critical,
}

impl std::fmt::Display for AnomalySeverity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Low => write!(f, "LOW"),
            Self::Medium => write!(f, "MEDIUM"),
            Self::High => write!(f, "HIGH"),
            Self::Critical => write!(f, "CRITICAL"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum AnomalyKind {
    LatencyDegradation { p99_ms: f64, threshold_ms: f64 },
    PanicRateSpike { panic_rate: f64, threshold: f64, total_panics: u64 },
    CpuThermalRunaway { temp_celsius: f64, threshold_celsius: f64 },
    CacheThrashing { miss_ratio: f64, threshold: f64 },
}

impl AnomalyKind {
    pub fn name(&self) -> &'static str {
        match self {
            Self::LatencyDegradation { .. } => "LatencyDegradation",
            Self::PanicRateSpike { .. } => "PanicRateSpike",
            Self::CpuThermalRunaway { .. } => "CpuThermalRunaway",
            Self::CacheThrashing { .. } => "CacheThrashing",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnomalyThresholds {
    pub p99_latency_threshold_ms: f64,
    pub panic_rate_threshold: f64,
    pub cpu_thermal_threshold_c: f64,
    pub cache_miss_ratio_threshold: f64,
    pub min_samples_for_evaluation: usize,
    pub max_samples_buffer: usize,
}

impl Default for AnomalyThresholds {
    fn default() -> Self {
        Self {
            p99_latency_threshold_ms: DEFAULT_P99_LATENCY_THRESHOLD_MS,
            panic_rate_threshold: DEFAULT_PANIC_RATE_THRESHOLD,
            cpu_thermal_threshold_c: DEFAULT_CPU_THERMAL_THRESHOLD_C,
            cache_miss_ratio_threshold: DEFAULT_CACHE_MISS_RATIO_THRESHOLD,
            min_samples_for_evaluation: DEFAULT_MIN_WINDOW_SAMPLES,
            max_samples_buffer: DEFAULT_MAX_WINDOW_SAMPLES,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct OtelMetricSample {
    pub timestamp_epoch_ms: u64,
    pub service_name: String,
    pub span_name: String,
    pub duration_ms: f64,
    pub is_panic: bool,
    pub cpu_temp_celsius: Option<f64>,
    pub cache_miss_rate: Option<f64>,
    pub stack_trace: Option<String>,
    pub source_file: Option<String>,
    pub function_name: Option<String>,
    pub line_number: Option<usize>,
    pub attributes: HashMap<String, String>,
}

impl OtelMetricSample {
    pub fn new(span_name: impl Into<String>, duration_ms: f64) -> Self {
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        Self {
            timestamp_epoch_ms: now_ms,
            service_name: "hgb-daemon".to_string(),
            span_name: span_name.into(),
            duration_ms,
            is_panic: false,
            cpu_temp_celsius: None,
            cache_miss_rate: None,
            stack_trace: None,
            source_file: None,
            function_name: None,
            line_number: None,
            attributes: HashMap::new(),
        }
    }

    pub fn with_panic(mut self, is_panic: bool) -> Self {
        self.is_panic = is_panic;
        self
    }

    pub fn with_cpu_temp(mut self, temp_c: f64) -> Self {
        self.cpu_temp_celsius = Some(temp_c);
        self
    }

    pub fn with_cache_miss_rate(mut self, rate: f64) -> Self {
        self.cache_miss_rate = Some(rate);
        self
    }

    pub fn with_source(
        mut self,
        file: impl Into<String>,
        func: impl Into<String>,
        line: usize,
    ) -> Self {
        self.source_file = Some(file.into());
        self.function_name = Some(func.into());
        self.line_number = Some(line);
        self
    }

    pub fn with_stack_trace(mut self, trace: impl Into<String>) -> Self {
        self.stack_trace = Some(trace.into());
        self
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AnomalyAlert {
    pub alert_id: String,
    pub timestamp_ms: u64,
    pub severity: AnomalySeverity,
    pub kind: AnomalyKind,
    pub target_file: PathBuf,
    pub target_function: String,
    pub line_number: Option<usize>,
    pub observed_value: f64,
    pub threshold_value: f64,
    pub sample_count: usize,
    pub stack_trace: Option<String>,
    pub recommended_strategy: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TracedSourceLocation {
    pub file_path: PathBuf,
    pub function_name: String,
    pub line_number: Option<usize>,
}

pub struct TelemetryAnomalyMonitor {
    thresholds: AnomalyThresholds,
    sample_buffer: Arc<RwLock<VecDeque<OtelMetricSample>>>,
    total_samples_ingested: AtomicU64,
    workspace_root: PathBuf,
}

impl TelemetryAnomalyMonitor {
    pub fn new(workspace_root: impl Into<PathBuf>) -> Self {
        Self::with_thresholds(workspace_root, AnomalyThresholds::default())
    }

    pub fn with_thresholds(
        workspace_root: impl Into<PathBuf>,
        thresholds: AnomalyThresholds,
    ) -> Self {
        Self {
            thresholds,
            sample_buffer: Arc::new(RwLock::new(VecDeque::new())),
            total_samples_ingested: AtomicU64::new(0),
            workspace_root: workspace_root.into(),
        }
    }

    pub async fn ingest_sample(&self, sample: OtelMetricSample) {
        self.total_samples_ingested.fetch_add(1, Ordering::Relaxed);
        let mut buffer = self.sample_buffer.write().await;
        if buffer.len() >= self.thresholds.max_samples_buffer {
            buffer.pop_front();
        }
        buffer.push_back(sample);
    }

    pub async fn ingest_batch(&self, samples: Vec<OtelMetricSample>) {
        let count = samples.len() as u64;
        self.total_samples_ingested.fetch_add(count, Ordering::Relaxed);
        let mut buffer = self.sample_buffer.write().await;
        for s in samples {
            if buffer.len() >= self.thresholds.max_samples_buffer {
                buffer.pop_front();
            }
            buffer.push_back(s);
        }
    }

    pub fn total_samples_ingested(&self) -> u64 {
        self.total_samples_ingested.load(Ordering::Relaxed)
    }

    pub async fn calculate_latency_percentiles(&self) -> (f64, f64, f64, f64) {
        let buffer = self.sample_buffer.read().await;
        if buffer.is_empty() {
            return (0.0, 0.0, 0.0, 0.0);
        }

        let mut latencies: Vec<f64> = buffer.iter().map(|s| s.duration_ms).collect();
        latencies.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

        let len = latencies.len();
        let p50 = latencies[((len as f64 * 0.50) as usize).min(len - 1)];
        let p90 = latencies[((len as f64 * 0.90) as usize).min(len - 1)];
        let p95 = latencies[((len as f64 * 0.95) as usize).min(len - 1)];
        let p99 = latencies[((len as f64 * 0.99) as usize).min(len - 1)];

        (p50, p90, p95, p99)
    }

    pub async fn calculate_panic_rate(&self) -> (f64, u64) {
        let buffer = self.sample_buffer.read().await;
        if buffer.is_empty() {
            return (0.0, 0);
        }
        let total = buffer.len() as f64;
        let panic_count = buffer.iter().filter(|s| s.is_panic).count() as u64;
        (panic_count as f64 / total, panic_count)
    }

    pub async fn average_cpu_temperature(&self) -> Option<f64> {
        let buffer = self.sample_buffer.read().await;
        let temps: Vec<f64> = buffer.iter().filter_map(|s| s.cpu_temp_celsius).collect();
        if temps.is_empty() {
            None
        } else {
            Some(temps.iter().sum::<f64>() / temps.len() as f64)
        }
    }

    pub async fn average_cache_miss_rate(&self) -> Option<f64> {
        let buffer = self.sample_buffer.read().await;
        let rates: Vec<f64> = buffer.iter().filter_map(|s| s.cache_miss_rate).collect();
        if rates.is_empty() {
            None
        } else {
            Some(rates.iter().sum::<f64>() / rates.len() as f64)
        }
    }

    pub fn parse_stack_frame(frame: &str) -> Option<TracedSourceLocation> {
        let clean = frame.trim();
        if clean.is_empty() {
            return None;
        }

        if let Some(at_idx) = clean.rfind(" at ") {
            let func_part = &clean[..at_idx].trim();
            let path_part = &clean[at_idx + 4..].trim();

            let func_name = func_part
                .split_whitespace()
                .last()
                .unwrap_or(func_part)
                .rsplit("::")
                .next()
                .unwrap_or(func_part)
                .to_string();

            let path_tokens: Vec<&str> = path_part.split(':').collect();
            if !path_tokens.is_empty() {
                let file_path = PathBuf::from(path_tokens[0]);
                let line_number = path_tokens.get(1).and_then(|s| s.parse::<usize>().ok());
                return Some(TracedSourceLocation {
                    file_path,
                    function_name: func_name,
                    line_number,
                });
            }
        }

        if let (Some(open), Some(close)) = (clean.find('('), clean.rfind(')')) {
            if open < close {
                let func_part = clean[..open].trim();
                let inner = clean[open + 1..close].trim();

                let func_name = func_part
                    .rsplit("::")
                    .next()
                    .unwrap_or(func_part)
                    .to_string();

                let inner_tokens: Vec<&str> = inner.split(':').collect();
                if !inner_tokens.is_empty() {
                    let file_path = PathBuf::from(inner_tokens[0]);
                    let line_number = inner_tokens.get(1).and_then(|s| s.parse::<usize>().ok());
                    return Some(TracedSourceLocation {
                        file_path,
                        function_name: func_name,
                        line_number,
                    });
                }
            }
        }

        let parts: Vec<&str> = clean.split(':').collect();
        if parts.len() >= 2 && (parts[0].ends_with(".rs") || parts[0].contains('/')) {
            let file_path = PathBuf::from(parts[0]);
            let line_number = parts[1].parse::<usize>().ok();
            return Some(TracedSourceLocation {
                file_path,
                function_name: "unknown_fn".to_string(),
                line_number,
            });
        }

        None
    }

    pub fn trace_offending_symbol(
        &self,
        samples: &[OtelMetricSample],
    ) -> Option<TracedSourceLocation> {
        for s in samples.iter().rev() {
            if let (Some(f), Some(func)) = (&s.source_file, &s.function_name) {
                let p = PathBuf::from(f);
                let full = if p.is_absolute() { p } else { self.workspace_root.join(&p) };
                return Some(TracedSourceLocation {
                    file_path: full,
                    function_name: func.clone(),
                    line_number: s.line_number,
                });
            }
        }

        for s in samples.iter().rev() {
            if let Some(ref trace) = s.stack_trace {
                for line in trace.lines() {
                    if let Some(loc) = Self::parse_stack_frame(line) {
                        let full = if loc.file_path.is_absolute() { loc.file_path } else { self.workspace_root.join(&loc.file_path) };
                        return Some(TracedSourceLocation {
                            file_path: full,
                            function_name: loc.function_name,
                            line_number: loc.line_number,
                        });
                    }
                }
            }
        }

        if let Some(s) = samples.last() {
            if !s.span_name.is_empty() {
                let func = s.span_name.rsplit("::").next().unwrap_or(&s.span_name);
                let default_file = self.workspace_root.join("src/lib.rs");
                return Some(TracedSourceLocation {
                    file_path: default_file,
                    function_name: func.to_string(),
                    line_number: None,
                });
            }
        }

        None
    }

    pub async fn evaluate_anomalies(&self) -> Vec<AnomalyAlert> {
        let buffer = self.sample_buffer.read().await;
        if buffer.len() < self.thresholds.min_samples_for_evaluation {
            return Vec::new();
        }

        let samples_vec: Vec<OtelMetricSample> = buffer.iter().cloned().collect();
        drop(buffer);

        let mut alerts = Vec::new();
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        let (_p50, _p90, _p95, p99) = self.calculate_latency_percentiles().await;
        if p99 > self.thresholds.p99_latency_threshold_ms {
            let offending = self
                .trace_offending_symbol(&samples_vec)
                .unwrap_or_else(|| TracedSourceLocation {
                    file_path: self.workspace_root.join("crates/hgb-daemon/src/server.rs"),
                    function_name: "handle_request".to_string(),
                    line_number: None,
                });

            let severity = if p99 > self.thresholds.p99_latency_threshold_ms * 3.0 {
                AnomalySeverity::Critical
            } else if p99 > self.thresholds.p99_latency_threshold_ms * 1.5 {
                AnomalySeverity::High
            } else {
                AnomalySeverity::Medium
            };

            alerts.push(AnomalyAlert {
                alert_id: uuid::Uuid::new_v4().to_string(),
                timestamp_ms: now_ms,
                severity,
                kind: AnomalyKind::LatencyDegradation {
                    p99_ms: p99,
                    threshold_ms: self.thresholds.p99_latency_threshold_ms,
                },
                target_file: offending.file_path,
                target_function: offending.function_name,
                line_number: offending.line_number,
                observed_value: p99,
                threshold_value: self.thresholds.p99_latency_threshold_ms,
                sample_count: samples_vec.len(),
                stack_trace: samples_vec.iter().rev().find_map(|s| s.stack_trace.clone()),
                recommended_strategy: "PreallocateVectorsOrIntroduceAtomicCache".to_string(),
            });
        }

        let (panic_rate, total_panics) = self.calculate_panic_rate().await;
        if panic_rate > self.thresholds.panic_rate_threshold {
            let panic_samples: Vec<OtelMetricSample> = samples_vec
                .iter()
                .filter(|s| s.is_panic)
                .cloned()
                .collect();

            let offending = self
                .trace_offending_symbol(&panic_samples)
                .unwrap_or_else(|| TracedSourceLocation {
                    file_path: self.workspace_root.join("crates/hgb-daemon/src/lib.rs"),
                    function_name: "dispatch".to_string(),
                    line_number: None,
                });

            alerts.push(AnomalyAlert {
                alert_id: uuid::Uuid::new_v4().to_string(),
                timestamp_ms: now_ms,
                severity: AnomalySeverity::Critical,
                kind: AnomalyKind::PanicRateSpike {
                    panic_rate,
                    threshold: self.thresholds.panic_rate_threshold,
                    total_panics,
                },
                target_file: offending.file_path,
                target_function: offending.function_name,
                line_number: offending.line_number,
                observed_value: panic_rate,
                threshold_value: self.thresholds.panic_rate_threshold,
                sample_count: samples_vec.len(),
                stack_trace: panic_samples.iter().rev().find_map(|s| s.stack_trace.clone()),
                recommended_strategy: "BoundTimeoutsAndHandleUnwrap".to_string(),
            });
        }

        if let Some(avg_temp) = self.average_cpu_temperature().await {
            if avg_temp >= self.thresholds.cpu_thermal_threshold_c {
                let offending = self
                    .trace_offending_symbol(&samples_vec)
                    .unwrap_or_else(|| TracedSourceLocation {
                        file_path: self.workspace_root.join("crates/hgb-core/src/dynatune_bridge.rs"),
                        function_name: "tune_concurrency".to_string(),
                        line_number: None,
                    });

                let severity = if avg_temp >= 85.0 { AnomalySeverity::Critical } else { AnomalySeverity::High };

                alerts.push(AnomalyAlert {
                    alert_id: uuid::Uuid::new_v4().to_string(),
                    timestamp_ms: now_ms,
                    severity,
                    kind: AnomalyKind::CpuThermalRunaway {
                        temp_celsius: avg_temp,
                        threshold_celsius: self.thresholds.cpu_thermal_threshold_c,
                    },
                    target_file: offending.file_path,
                    target_function: offending.function_name,
                    line_number: offending.line_number,
                    observed_value: avg_temp,
                    threshold_value: self.thresholds.cpu_thermal_threshold_c,
                    sample_count: samples_vec.len(),
                    stack_trace: None,
                    recommended_strategy: "UnrollLoopsAndYieldCpu".to_string(),
                });
            }
        }

        if let Some(avg_miss) = self.average_cache_miss_rate().await {
            if avg_miss > self.thresholds.cache_miss_ratio_threshold {
                let offending = self
                    .trace_offending_symbol(&samples_vec)
                    .unwrap_or_else(|| TracedSourceLocation {
                        file_path: self.workspace_root.join("crates/hgb-core/src/zero_copy_memory_bus.rs"),
                        function_name: "copy_buffer".to_string(),
                        line_number: None,
                    });

                alerts.push(AnomalyAlert {
                    alert_id: uuid::Uuid::new_v4().to_string(),
                    timestamp_ms: now_ms,
                    severity: AnomalySeverity::High,
                    kind: AnomalyKind::CacheThrashing {
                        miss_ratio: avg_miss,
                        threshold: self.thresholds.cache_miss_ratio_threshold,
                    },
                    target_file: offending.file_path,
                    target_function: offending.function_name,
                    line_number: offending.line_number,
                    observed_value: avg_miss,
                    threshold_value: self.thresholds.cache_miss_ratio_threshold,
                    sample_count: samples_vec.len(),
                    stack_trace: None,
                    recommended_strategy: "EliminateCloneAndOptimizeLayout".to_string(),
                });
            }
        }

        alerts
    }

    pub async fn clear_buffer(&self) {
        let mut buffer = self.sample_buffer.write().await;
        buffer.clear();
    }
}