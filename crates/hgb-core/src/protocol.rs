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
        #[serde(default)]
        start_line: Option<usize>,
        #[serde(default)]
        end_line: Option<usize>,
        #[serde(default)]
        offset: Option<usize>,
    },
    CrudWrite {
        path: String,
        content: String,
        overwrite: bool,
        #[serde(default)]
        artifact_summary: Option<String>,
    },
    CrudEdit {
        path: String,
        target: String,
        replacement: String,
        #[serde(default)]
        start_line: Option<usize>,
        #[serde(default)]
        end_line: Option<usize>,
        #[serde(default)]
        allow_multiple: bool,
        #[serde(default)]
        instruction: Option<String>,
        #[serde(default)]
        description: Option<String>,
        #[serde(default)]
        target_lint_error_ids: Vec<String>,
    },
    CrudList {
        path: String,
    },
    CrudGrep {
        pattern: String,
        #[serde(default)]
        path: Option<String>,
        #[serde(default)]
        is_regex: bool,
        #[serde(default)]
        case_insensitive: bool,
        #[serde(default = "default_true")]
        match_per_line: bool,
        #[serde(default)]
        includes: Vec<String>,
    },
    CrudFind {
        search_directory: String,
        #[serde(default)]
        pattern: Option<String>,
        #[serde(default)]
        extensions: Vec<String>,
        #[serde(default)]
        excludes: Vec<String>,
        #[serde(default)]
        max_depth: Option<usize>,
        #[serde(default)]
        target_type: Option<String>,
    },
}

fn default_true() -> bool {
    true
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
