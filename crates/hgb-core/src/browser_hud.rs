use serde::{Deserialize, Serialize};

/// Type of browser runtime incident captured from Chrome DevTools Protocol or console
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum BrowserIncidentKind {
    ConsoleError,
    UnhandledPromiseRejection,
    HydrationMismatch,
    NetworkFailure,
    UncaughtException,
    CssLayoutDefect,
}

pub type IncidentKind = BrowserIncidentKind;

impl std::fmt::Display for BrowserIncidentKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ConsoleError => write!(f, "CONSOLE_ERROR"),
            Self::UnhandledPromiseRejection => write!(f, "PROMISE_REJECTION"),
            Self::HydrationMismatch => write!(f, "HYDRATION_MISMATCH"),
            Self::NetworkFailure => write!(f, "NETWORK_FAILURE"),
            Self::UncaughtException => write!(f, "UNCAUGHT_EXCEPTION"),
            Self::CssLayoutDefect => write!(f, "CSS_LAYOUT_DEFECT"),
        }
    }
}

/// Incident severity classification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BrowserIncidentSeverity {
    Critical,
    High,
    Medium,
    Low,
    Info,
}

impl BrowserIncidentSeverity {
    pub fn badge(&self) -> &'static str {
        match self {
            Self::Critical => "🚨 CRITICAL",
            Self::High => "🔴 HIGH",
            Self::Medium => "🟡 MEDIUM",
            Self::Low => "🔵 LOW",
            Self::Info => "ℹ️ INFO",
        }
    }
}

/// Structured incident captured live from browser runtime
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrowserIncident {
    pub id: String,
    pub kind: BrowserIncidentKind,
    pub severity: BrowserIncidentSeverity,
    pub message: String,
    pub source_url: String,
    pub line_number: Option<usize>,
    pub stack_trace: Option<String>,
    pub suggested_fix: Option<String>,
    pub healed: bool,
}

/// Telemetry metrics for HUD
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BrowserTelemetry {
    pub total_incidents: usize,
    pub unresolved_count: usize,
    pub hydration_mismatches: usize,
    pub uncaught_exceptions: usize,
    pub network_errors: usize,
}

/// Live Browser Incident Streamer and Console HUD
#[derive(Debug, Clone)]
pub struct BrowserLiveHud {
    pub incidents: Vec<BrowserIncident>,
    pub max_history: usize,
    counter: usize,
}

impl Default for BrowserLiveHud {
    fn default() -> Self {
        Self::new(100)
    }
}

impl BrowserLiveHud {
    pub fn new(max_history: usize) -> Self {
        Self {
            incidents: Vec::new(),
            max_history,
            counter: 0,
        }
    }

    /// Ingest a full raw CDP JSON string containing {"method": "...", "params": {...}}
    pub fn ingest_cdp_event(&mut self, raw_cdp_json: &str) -> Option<BrowserIncident> {
        let v: serde_json::Value = serde_json::from_str(raw_cdp_json).ok()?;
        let method = v.get("method").and_then(|m| m.as_str())?;
        let params_val = v.get("params")?;
        let params_str = params_val.to_string();
        self.record_cdp_event(method, &params_str)
    }

    /// Decode raw Chrome DevTools Protocol (CDP) console or runtime event
    pub fn record_cdp_event(&mut self, method: &str, params_json: &str) -> Option<BrowserIncident> {
        let v: serde_json::Value = serde_json::from_str(params_json).ok()?;

        let (kind, severity, message, source_url, line_no, stack) = match method {
            "Runtime.consoleAPICalled" => {
                let msg_type = v.get("type").and_then(|t| t.as_str()).unwrap_or("log");
                if msg_type != "error" && msg_type != "warning" {
                    return None;
                }
                let raw_msg = v.get("args")
                    .and_then(|a| a.as_array())
                    .and_then(|arr| arr.first())
                    .and_then(|arg| arg.get("value").and_then(|val| val.as_str()).or_else(|| arg.get("description").and_then(|d| d.as_str())))
                    .unwrap_or("Unknown console error");

                let mut stack_url = String::new();
                let mut stack_line = None;
                let stack = v.get("stackTrace")
                    .and_then(|s| s.get("callFrames"))
                    .and_then(|f| f.as_array())
                    .map(|frames| {
                        frames.iter().map(|fr| {
                            let fn_name = fr.get("functionName").and_then(|n| n.as_str()).unwrap_or("anonymous");
                            let url = fr.get("url").and_then(|u| u.as_str()).unwrap_or("");
                            let line = fr.get("lineNumber").and_then(|l| l.as_u64()).unwrap_or(0);
                            if stack_url.is_empty() && !url.is_empty() {
                                stack_url = url.to_string();
                                stack_line = Some(line as usize);
                            }
                            format!("  at {} ({}:{})", fn_name, url, line)
                        }).collect::<Vec<_>>().join("\n")
                    });

                let (kind, severity) = if raw_msg.contains("Hydration failed") || raw_msg.contains("server HTML did not match") || raw_msg.contains("does not match what was rendered on the server") {
                    (BrowserIncidentKind::HydrationMismatch, BrowserIncidentSeverity::High)
                } else if raw_msg.contains("overflow") || raw_msg.contains("clientWidth") {
                    (BrowserIncidentKind::CssLayoutDefect, BrowserIncidentSeverity::Medium)
                } else {
                    (BrowserIncidentKind::ConsoleError, BrowserIncidentSeverity::High)
                };

                let src = if !stack_url.is_empty() { stack_url } else { "http://localhost:3000".to_string() };
                (kind, severity, raw_msg.to_string(), src, stack_line.or(Some(1)), stack)
            }
            "Runtime.exceptionThrown" => {
                let exc_details = v.get("exceptionDetails");
                let desc = exc_details
                    .and_then(|e| e.get("text").and_then(|t| t.as_str()).or_else(|| e.get("exception").and_then(|ex| ex.get("description").and_then(|d| d.as_str()))))
                    .unwrap_or("Unhandled runtime exception");
                let url = exc_details
                    .and_then(|e| e.get("url"))
                    .and_then(|u| u.as_str())
                    .unwrap_or("app.js");
                let line = exc_details
                    .and_then(|e| e.get("lineNumber"))
                    .and_then(|l| l.as_u64())
                    .map(|l| l as usize);

                let (kind, severity) = if desc.contains("Unhandled Promise Rejection") {
                    (BrowserIncidentKind::UnhandledPromiseRejection, BrowserIncidentSeverity::High)
                } else {
                    (BrowserIncidentKind::UncaughtException, BrowserIncidentSeverity::Critical)
                };

                (kind, severity, desc.to_string(), url.to_string(), line, None)
            }
            "Network.responseReceived" => {
                let status = v.get("response").and_then(|r| r.get("status")).and_then(|s| s.as_u64()).unwrap_or(200);
                if status >= 400 {
                    let url = v.get("response").and_then(|r| r.get("url")).and_then(|u| u.as_str()).unwrap_or("");
                    let msg = format!("HTTP {} fetching {}", status, url);
                    (BrowserIncidentKind::NetworkFailure, BrowserIncidentSeverity::High, msg, url.to_string(), None, None)
                } else {
                    return None;
                }
            }
            _ => return None,
        };

        self.counter += 1;
        let id = format!("inc-{}", self.counter);
        let suggested_fix = Some(Self::synthesize_fix_action_internal(&kind, &message, source_url.as_str()));

        let incident = BrowserIncident {
            id,
            kind,
            severity,
            message,
            source_url,
            line_number: line_no,
            stack_trace: stack,
            suggested_fix,
            healed: false,
        };

        self.incidents.push(incident.clone());
        if self.incidents.len() > self.max_history {
            self.incidents.remove(0);
        }

        Some(incident)
    }

    /// Internal heuristic to synthesize immediate surgical fixes
    fn synthesize_fix_action_internal(kind: &BrowserIncidentKind, msg: &str, url: &str) -> String {
        match kind {
            BrowserIncidentKind::HydrationMismatch => {
                "Wrap client-only component with dynamic ssr: false or useEffect / suppressHydrationWarning".to_string()
            }
            BrowserIncidentKind::UnhandledPromiseRejection => {
                if msg.contains("Failed to fetch") {
                    "Verify backend devserver is running on expected port or configure CORS headers".to_string()
                } else {
                    "Attach .catch(err => console.error(err)) or try/catch around async dispatch".to_string()
                }
            }
            BrowserIncidentKind::NetworkFailure => {
                format!("Check route handler for endpoint '{}' or add mock route in MockFabric", url)
            }
            BrowserIncidentKind::CssLayoutDefect => {
                "Apply overflow-x: hidden or max-w-full to prevent horizontal layout blowout".to_string()
            }
            BrowserIncidentKind::ConsoleError | BrowserIncidentKind::UncaughtException => {
                if msg.contains("Cannot read properties of undefined") || msg.contains("Cannot read property") {
                    "Add optional chaining (?.) or null check before property access".to_string()
                } else {
                    "Verify symbol declaration and check import specifier".to_string()
                }
            }
        }
    }

    /// Synthesize surgical fix action for an existing incident
    pub fn synthesize_fix_action(&self, incident: &BrowserIncident) -> String {
        Self::synthesize_fix_action_internal(&incident.kind, &incident.message, &incident.source_url)
    }

    /// Mark an incident as healed / resolved
    pub fn mark_healed(&mut self, id: &str) -> bool {
        if let Some(inc) = self.incidents.iter_mut().find(|i| i.id == id) {
            inc.healed = true;
            true
        } else {
            false
        }
    }

    /// Alias for mark_healed (1-click heal via F8)
    pub fn resolve_incident(&mut self, id: &str) -> bool {
        self.mark_healed(id)
    }

    /// Get all unresolved incidents
    pub fn get_unresolved_incidents(&self) -> Vec<&BrowserIncident> {
        self.incidents.iter().filter(|i| !i.healed).collect()
    }

    /// Compute telemetry statistics
    pub fn telemetry(&self) -> BrowserTelemetry {
        let mut t = BrowserTelemetry {
            total_incidents: self.incidents.len(),
            ..Default::default()
        };
        for inc in &self.incidents {
            if !inc.healed {
                t.unresolved_count += 1;
            }
            match inc.kind {
                BrowserIncidentKind::HydrationMismatch => t.hydration_mismatches += 1,
                BrowserIncidentKind::UncaughtException => t.uncaught_exceptions += 1,
                BrowserIncidentKind::NetworkFailure => t.network_errors += 1,
                _ => {}
            }
        }
        t
    }
}
