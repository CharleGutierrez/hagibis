use serde::{Deserialize, Serialize};

/// Request sent from hgb CLI to hgbd Daemon over Unix Domain Socket
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum HgbRequest {
    Ping,
    Status,
    Doctor,
    Prompt {
        prompt: String,
        model: Option<String>,
        provider: Option<String>,
        stream: bool,
    },
    Verify {
        target: String,
        invariant: String,
    },
    Checkpoint {
        action: String,
        label: Option<String>,
    },
    Provenance {
        action: String,
    },
    Fuzz {
        target: String,
        iterations: usize,
    },
    MeshStatus,
    // --- AGY Surgical CRUD Requests ---
    CrudView {
        path: String,
        start_line: Option<usize>,
        end_line: Option<usize>,
        offset: Option<usize>,
    },
    CrudWrite {
        path: String,
        content: String,
        overwrite: bool,
    },
    CrudEdit {
        path: String,
        target: String,
        replacement: String,
        start_line: Option<usize>,
        end_line: Option<usize>,
        allow_multiple: bool,
    },
    CrudList {
        path: String,
    },
    CrudGrep {
        pattern: String,
        path: Option<String>,
        case_insensitive: bool,
    },
}

/// Response returned from hgbd Daemon to hgb CLI
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum HgbResponse {
    Pong { latency_us: u64 },
    Status(DaemonStatus),
    DoctorReport(Vec<DoctorPillar>),
    TextChunk(String),
    Complete { output: String, tokens_used: usize, duration_ms: u64 },
    Error(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DaemonStatus {
    pub version: String,
    pub uptime_secs: u64,
    pub active_models: Vec<String>,
    pub memory_rss_mb: f64,
    pub active_peers: usize,
    pub socket_path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DoctorPillar {
    pub name: String,
    pub status: String,
    pub message: String,
}
