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
    AuthStatus,
    Login,
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
    // --- Dynamic Model Hot-Swapping ---
    ModelSwitch {
        model: String,
    },
    ModelList,
    // --- Speculative Dual-Draft Race ---
    VibeRace {
        prompt: String,
        #[serde(default)]
        target_dir: Option<String>,
    },
    // --- Style Memory & Reject-Learner Vault ---
    RecordStyleFeedback {
        snippet: String,
        accepted: bool,
    },
    GetStyleGuidance,
    // --- Autonomous PR Storyteller ---
    Ship {
        #[serde(default)]
        dry_run: bool,
    },
    // --- Vibe Coding Next Sprint Capabilities ---
    RunCommand {
        command: String,
        #[serde(default)]
        cwd: Option<String>,
        #[serde(default)]
        timeout_ms: Option<u64>,
    },
    AgentRun {
        prompt: String,
        #[serde(default)]
        model: Option<String>,
        #[serde(default)]
        max_turns: Option<usize>,
        #[serde(default)]
        workspace_root: Option<String>,
    },
    Heal {
        #[serde(default)]
        check_command: Option<String>,
        #[serde(default)]
        workspace_root: Option<String>,
    },
    Undo {
        #[serde(default)]
        checkpoint_id: Option<String>,
    },
    GetRules {
        #[serde(default)]
        workspace_root: Option<String>,
    },
    InitRules {
        #[serde(default)]
        workspace_root: Option<String>,
    },
    GetRepoMap {
        #[serde(default)]
        workspace_root: Option<String>,
        #[serde(default)]
        max_files: Option<usize>,
    },
    // --- Next-Gen Vibe Coding Requests ---
    GuardianStart {
        #[serde(default)]
        check_command: Option<String>,
        #[serde(default)]
        debounce_ms: Option<u64>,
    },
    GuardianStatus,
    GuardianApplyFix {
        fix_id: String,
    },
    DevServerScan,
    ImpactAnalyze {
        symbol: String,
        originating_file: String,
    },
    SwarmPodRun {
        task: String,
    },
    RescueDiagnose {
        failed_command: String,
        exit_code: i32,
        stderr: String,
        stdout: String,
    },
    MemoryGetAnchor {
        #[serde(default)]
        max_tokens: Option<usize>,
    },
    MemoryRecordDecision {
        title: String,
        decision: String,
        context: String,
    },
    // --- Pillar 1: GlanceEngine ---
    GlanceInspect {
        image_path: String,
        #[serde(default)]
        context_path: Option<String>,
    },
    // --- Pillar 2: PackageGuard ---
    PackageVerify {
        ecosystem: String,
        name: String,
        #[serde(default)]
        version: Option<String>,
    },
    // --- Pillar 3: EnvSentinel ---
    EnvScan {
        #[serde(default)]
        workspace_root: Option<String>,
        #[serde(default)]
        env_file: Option<String>,
    },
    EnvExampleGenerate {
        #[serde(default)]
        workspace_root: Option<String>,
    },
    EnvShred {
        content: String,
    },
    // --- Pillar 4: MockFabric ---
    MockServerStart {
        resource_name: String,
        #[serde(default)]
        schema_json: Option<String>,
        #[serde(default)]
        port: Option<u16>,
        #[serde(default = "default_five")]
        seed_count: usize,
    },
    MockServerStop {
        port: u16,
    },
    // --- Pillar 5: TraceRingBuffer ---
    TraceGetContext {
        #[serde(default = "default_twenty")]
        last_n: usize,
    },
    // --- Pillar 6: AtmosphericWorktree & Semantic Stash ---
    WorktreeCreate {
        branch: String,
        #[serde(default)]
        base_commit: Option<String>,
    },
    WorktreeCleanup {
        branch: String,
    },
    SemanticStash {
        action: String,
        #[serde(default)]
        tag: Option<String>,
        #[serde(default)]
        description: Option<String>,
    },
    // --- Pillar 1: BrowserSnoop ---
    BrowserSnoopReport {
        #[serde(default)]
        target_url: Option<String>,
    },
    BrowserSnoopClear,
    // --- Pillar 2: VariantRace ---
    VariantRaceStart {
        prompt: String,
        #[serde(default)]
        archetypes: Option<Vec<crate::variant_race::DesignArchetype>>,
    },
    VariantRacePick {
        race_id: String,
        winner_id: String,
    },
    VariantRaceAbort {
        race_id: String,
    },
    // --- Pillar 3: DbSentinel ---
    DbSentinelScan {
        #[serde(default)]
        db_path: Option<String>,
        #[serde(default)]
        ddl_path: Option<String>,
    },
    DbSentinelDryRun {
        db_path: String,
        migration_sql: String,
    },
    DbSentinelApply {
        db_path: String,
        migration_sql: String,
        migration_name: String,
    },
    // --- Pillar 4: SyntaxSlicer ---
    SyntaxSlice {
        file_path: String,
        focal_symbol: String,
        #[serde(default = "default_two")]
        depth: usize,
    },
    // --- Pillar 5: AutoSpec ---
    AutoSpecSynthesize {
        target_function: String,
        file_path: String,
    },
    AutoSpecRun {
        #[serde(default)]
        strict: bool,
    },
    AutoSpecList,
    // --- Pillar 6: DriftLock ---
    DriftLockScan {
        #[serde(default)]
        workspace_root: Option<String>,
    },
    DriftLockAuditPatch {
        patch_content: String,
    },
    // --- Superpowers Vibe Coding Requests ---
    // 1. Universal MCP Client
    McpListTools {
        #[serde(default)]
        config_path: Option<String>,
    },
    McpCallTool {
        server_name: String,
        tool_name: String,
        arguments: serde_json::Value,
        #[serde(default)]
        config_path: Option<String>,
    },
    // 2. Ephemeral Worktree "What-If" Timelines
    TimelineCreate {
        name: String,
        #[serde(default)]
        base_branch: Option<String>,
        #[serde(default)]
        workspace_root: Option<String>,
    },
    TimelineList {
        #[serde(default)]
        workspace_root: Option<String>,
    },
    TimelineDiff {
        name: String,
        #[serde(default)]
        workspace_root: Option<String>,
    },
    TimelineMerge {
        name: String,
        #[serde(default)]
        workspace_root: Option<String>,
    },
    TimelineDiscard {
        name: String,
        #[serde(default)]
        workspace_root: Option<String>,
    },
    // 3. Verification Gate & Golden Invariant Guard
    VerificationGateRun {
        #[serde(default)]
        workspace_root: Option<String>,
        #[serde(default)]
        auto_heal: bool,
    },
    // 4. Shell Companion & Crash Interceptor
    ShellInit {
        shell: String,
    },
    ShellCrashRecord {
        record: crate::shell_hook::CrashRecord,
    },
    ShellCrashFix {
        #[serde(default)]
        workspace_root: Option<String>,
    },
    // 5. Ambient Watch-and-Vibe Autonomous Loop
    AmbientVibeRunOnce {
        #[serde(default)]
        workspace_root: Option<String>,
    },
    // 6. Visual Ingestion Component Synthesis
    GlanceSynthesize {
        image_path: String,
        target_framework: String,
    },
}

fn default_two() -> usize {
    2
}

fn default_true() -> bool {
    true
}

fn default_five() -> usize {
    5
}

fn default_twenty() -> usize {
    20
}

/// Endpoint summary for discovered local dev servers
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DevServerEndpointInfo {
    pub port: u16,
    pub url: String,
    pub framework: String,
    pub http_status: Option<u16>,
    pub response_time_ms: u64,
    pub is_healthy: bool,
}

/// Completed result from a 4-role concurrent Swarm Pod execution
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SwarmPodResult {
    pub task: String,
    pub architect_plan: String,
    pub code_solution: String,
    pub review_status: bool,
    pub review_notes: Vec<String>,
    pub test_coverage: String,
    pub total_tokens: usize,
    pub duration_ms: u64,
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
    // --- Dynamic Model Hot-Swapping ---
    ModelSwitched {
        previous: String,
        current: String,
        duration_ms: u64,
    },
    ModelList(Vec<String>),
    // --- Speculative Dual-Draft Race ---
    RaceResult {
        winner: String,
        duration_ms: u64,
        patch: String,
        passed_checks: bool,
    },
    // --- Style Memory & Reject-Learner Vault ---
    StyleFeedbackRecorded,
    StyleGuidance(String),
    // --- Autonomous PR Storyteller ---
    ShipReport {
        pr_title: String,
        pr_body: String,
        commits: Vec<String>,
        security_passed: bool,
    },
    // --- Vibe Coding Next Sprint Responses ---
    CommandOutput {
        stdout: String,
        stderr: String,
        exit_code: i32,
        duration_ms: u64,
        timed_out: bool,
    },
    AgentSession {
        output: String,
        turns: usize,
        duration_ms: u64,
        steps_count: usize,
    },
    HealResult {
        check_command: String,
        initial_errors: usize,
        final_errors: usize,
        fully_healed: bool,
        duration_ms: u64,
        attempts_count: usize,
    },
    UndoResult {
        checkpoint_id: String,
        files_restored: Vec<String>,
        files_deleted: Vec<String>,
        duration_ms: u64,
    },
    RulesReport {
        rules_count: usize,
        aggregate_hash: String,
        aggregated_content: String,
    },
    RepoMapReport {
        content: String,
        symbol_count: usize,
        file_count: usize,
    },
    // --- Next-Gen Vibe Coding Responses ---
    GuardianReport {
        is_active: bool,
        staged_ghost_fixes: usize,
        last_check_passed: bool,
    },
    GhostFixApplied {
        report: String,
    },
    DevServerEndpoints(Vec<DevServerEndpointInfo>),
    ImpactReport(crate::impact::ImpactReport),
    SwarmPodCompleted(SwarmPodResult),
    RescueReport(crate::rescue::RescueReport),
    MemoryAnchor(String),
    MemoryRecorded {
        id: String,
    },
    // --- Sprint Vibe Coding Responses ---
    GlanceReport(crate::glance::VisualInspectionReport),
    PackageVerified(crate::package_guard::PackageVerificationReport),
    EnvAudit(crate::env_sentinel::EnvAuditReport),
    EnvExample(String),
    EnvShredded {
        sanitized_content: String,
        leaks_detected: usize,
    },
    MockServerStarted {
        url: String,
        port: u16,
        resource: String,
        seed_count: usize,
    },
    MockServerStopped {
        port: u16,
    },
    TraceContext(String),
    WorktreeCreated {
        branch: String,
        path: String,
    },
    WorktreeCleaned {
        branch: String,
    },
    SemanticStashResult {
        output: String,
    },
    // --- Next-Gen Vibe Coding Responses ---
    BrowserHealth(crate::browser_snoop::BrowserHealthReport),
    BrowserSnoopCleared,
    VariantRaceManifestReport(crate::variant_race::VariantRaceManifest),
    VariantWinnerCherryPicked {
        commit_hash_or_patch: String,
    },
    VariantRaceAborted,
    DbDriftReport(crate::db_sentinel::SchemaDriftReport),
    DbDryRunResult {
        success: bool,
        error: Option<String>,
    },
    DbMigrationApplied {
        migration_file: String,
    },
    SyntaxSliceReport(crate::syntax_slicer::SyntaxSliceResult),
    AutoSpecSynthesized(crate::auto_spec::GoldenSpec),
    AutoSpecRunReport(Vec<crate::auto_spec::SpecExecutionReport>),
    AutoSpecListReport(Vec<crate::auto_spec::GoldenSpec>),
    DriftDnaReport(crate::drift_lock::ArchitecturalDna),
    ComplianceAuditResult(crate::drift_lock::ComplianceAuditReport),
    // --- Superpowers Vibe Coding Responses ---
    McpToolsList(Vec<crate::mcp::McpTool>),
    McpToolCallResult(serde_json::Value),
    TimelineCreated(crate::timeline::TimelineInfo),
    TimelineListReport(Vec<crate::timeline::TimelineInfo>),
    TimelineDiffReport(crate::timeline::TimelineDiff),
    TimelineMergeReport(crate::timeline::TimelineMergeReport),
    TimelineDiscarded { name: String },
    VerificationGateCertificate(crate::verification_gate::VerificationCertificate),
    ShellInitScript(String),
    ShellCrashRecorded { crash_id: String },
    ShellCrashDiagnosis(crate::shell_hook::CrashDiagnosis),
    AmbientVibeReport(Vec<crate::ambient_vibe::VibeWatchEvent>),
    GlanceSynthesizedCode {
        framework: String,
        code: String,
        css: Option<String>,
    },
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
