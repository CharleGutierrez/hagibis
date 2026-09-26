use chrono::Utc;
use hgb_core::browser_snoop::{
    BrowserHealthReport, BrowserHealthVerdict, BrowserIncident, BrowserIncidentKind,
    BrowserSeverity,
};
use hgb_core::trace::TraceRingBuffer;
use hgb_core::Result;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, RwLock};

#[derive(Clone)]
pub struct BrowserSnoopEngine {
    inspected_url: String,
    incidents: Arc<RwLock<Vec<BrowserIncident>>>,
    trace_buffer: Option<Arc<TraceRingBuffer>>,
    incident_counter: Arc<AtomicUsize>,
}

impl BrowserSnoopEngine {
    pub fn new(inspected_url: String, trace_buffer: Option<Arc<TraceRingBuffer>>) -> Self {
        Self {
            inspected_url,
            incidents: Arc::new(RwLock::new(Vec::new())),
            trace_buffer,
            incident_counter: Arc::new(AtomicUsize::new(1)),
        }
    }

    pub fn record_incident(&self, kind: BrowserIncidentKind, severity: BrowserSeverity) -> BrowserIncident {
        let seq = self.incident_counter.fetch_add(1, Ordering::SeqCst);
        let id = format!("inc-{}", seq);
        let incident = BrowserIncident {
            id,
            timestamp_rfc3339: Utc::now().to_rfc3339(),
            severity,
            kind: kind.clone(),
        };

        if let Ok(mut lock) = self.incidents.write() {
            lock.push(incident.clone());
        }

        if let Some(ref tb) = self.trace_buffer {
            let msg = match &kind {
                BrowserIncidentKind::ConsoleError { message, .. } => format!("Console error: {}", message),
                BrowserIncidentKind::UnhandledDomException { exception_type, message, .. } => {
                    format!("DOM Exception ({}): {}", exception_type, message)
                }
                BrowserIncidentKind::NetworkFailure { url, status_code, .. } => {
                    format!("HTTP {} failed for {}", status_code, url)
                }
                BrowserIncidentKind::HmrCompileError { compiler_message, .. } => {
                    format!("HMR compilation error: {}", compiler_message)
                }
            };
            tb.record_browser_incident(&severity.to_string(), &msg);
        }

        incident
    }

    pub fn clear(&self) {
        if let Ok(mut lock) = self.incidents.write() {
            lock.clear();
        }
    }

    pub fn generate_health_report(&self) -> BrowserHealthReport {
        let incidents = self.incidents.read().map(|l| l.clone()).unwrap_or_default();
        let total_incidents = incidents.len();

        let mut console_errors_count = 0;
        let mut network_failures_count = 0;
        let mut hmr_errors_count = 0;
        let mut suggested_root_causes = Vec::new();

        for inc in &incidents {
            match &inc.kind {
                BrowserIncidentKind::ConsoleError { message, .. } => {
                    console_errors_count += 1;
                    if message.contains("Cannot read properties of undefined") || message.contains("is not a function") {
                        suggested_root_causes.push(format!("Null pointer dereference: {}", message));
                    } else if message.contains("Hydration failed") {
                        suggested_root_causes.push("SSR/Client DOM hydration mismatch in component tree".to_string());
                    }
                }
                BrowserIncidentKind::UnhandledDomException { message, .. } => {
                    suggested_root_causes.push(format!("DOM layout/rendering fault: {}", message));
                }
                BrowserIncidentKind::NetworkFailure { url, status_code, .. } => {
                    network_failures_count += 1;
                    suggested_root_causes.push(format!("API endpoint {} returned HTTP {}", url, status_code));
                }
                BrowserIncidentKind::HmrCompileError { compiler_message, .. } => {
                    hmr_errors_count += 1;
                    suggested_root_causes.push(format!("DevServer bundler compilation failed: {}", compiler_message));
                }
            }
        }

        suggested_root_causes.dedup();

        let verdict = if hmr_errors_count > 0 || console_errors_count >= 3 {
            BrowserHealthVerdict::Broken
        } else if console_errors_count > 0 || network_failures_count > 0 {
            BrowserHealthVerdict::Degraded
        } else {
            BrowserHealthVerdict::Healthy
        };

        BrowserHealthReport {
            verdict,
            inspected_url: self.inspected_url.clone(),
            total_incidents,
            console_errors_count,
            network_failures_count,
            hmr_errors_count,
            incidents,
            suggested_root_causes,
            generated_at_rfc3339: Utc::now().to_rfc3339(),
        }
    }

    /// Actively probe a local devserver endpoint via HTTP and evaluate status
    pub async fn probe_endpoint(&self, target_url: &str) -> Result<BrowserHealthReport> {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_millis(1500))
            .build()
            .unwrap_or_default();

        match client.get(target_url).send().await {
            Ok(resp) => {
                let status = resp.status().as_u16();
                if status >= 400 {
                    self.record_incident(
                        BrowserIncidentKind::NetworkFailure {
                            url: target_url.to_string(),
                            method: "GET".to_string(),
                            status_code: status,
                            error_text: Some(format!("HTTP error status {}", status)),
                            duration_ms: 10,
                        },
                        BrowserSeverity::Error,
                    );
                } else if let Ok(body) = resp.text().await {
                    // Check for embedded compiler errors or runtime stack traces in HTML body
                    if body.contains("Failed to compile") || body.contains("Build Error") {
                        self.record_incident(
                            BrowserIncidentKind::HmrCompileError {
                                compiler_message: "Detected compilation error overlay in dev server HTML response".to_string(),
                                affected_file: None,
                            },
                            BrowserSeverity::Fatal,
                        );
                    }
                }
            }
            Err(e) => {
                self.record_incident(
                    BrowserIncidentKind::NetworkFailure {
                        url: target_url.to_string(),
                        method: "GET".to_string(),
                        status_code: 0,
                        error_text: Some(e.to_string()),
                        duration_ms: 0,
                    },
                    BrowserSeverity::Error,
                );
            }
        }

        Ok(self.generate_health_report())
    }
}
