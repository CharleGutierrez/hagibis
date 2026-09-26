use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BrowserSeverity {
    Info,
    Warning,
    Error,
    Fatal,
}

impl std::fmt::Display for BrowserSeverity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Info => write!(f, "INFO"),
            Self::Warning => write!(f, "WARN"),
            Self::Error => write!(f, "ERROR"),
            Self::Fatal => write!(f, "FATAL"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum BrowserIncidentKind {
    ConsoleError {
        message: String,
        stack_trace: Option<String>,
        source_url: Option<String>,
        line: Option<u32>,
        col: Option<u32>,
    },
    UnhandledDomException {
        exception_type: String,
        message: String,
        stack_trace: String,
    },
    NetworkFailure {
        url: String,
        method: String,
        status_code: u16,
        error_text: Option<String>,
        duration_ms: u64,
    },
    HmrCompileError {
        compiler_message: String,
        affected_file: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrowserIncident {
    pub id: String,
    pub timestamp_rfc3339: String,
    pub severity: BrowserSeverity,
    pub kind: BrowserIncidentKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BrowserHealthVerdict {
    Healthy,
    Degraded,
    Broken,
}

impl std::fmt::Display for BrowserHealthVerdict {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Healthy => write!(f, "HEALTHY"),
            Self::Degraded => write!(f, "DEGRADED"),
            Self::Broken => write!(f, "BROKEN"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BrowserHealthReport {
    pub verdict: BrowserHealthVerdict,
    pub inspected_url: String,
    pub total_incidents: usize,
    pub console_errors_count: usize,
    pub network_failures_count: usize,
    pub hmr_errors_count: usize,
    pub incidents: Vec<BrowserIncident>,
    pub suggested_root_causes: Vec<String>,
    pub generated_at_rfc3339: String,
}

impl BrowserHealthReport {
    pub fn empty(inspected_url: String) -> Self {
        Self {
            verdict: BrowserHealthVerdict::Healthy,
            inspected_url,
            total_incidents: 0,
            console_errors_count: 0,
            network_failures_count: 0,
            hmr_errors_count: 0,
            incidents: Vec::new(),
            suggested_root_causes: Vec::new(),
            generated_at_rfc3339: chrono::Utc::now().to_rfc3339(),
        }
    }
}
