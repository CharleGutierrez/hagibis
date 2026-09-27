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
    // 7. AST-Aware Visual Patch Arbiter
    AstPatchParse {
        original: String,
        modified: String,
        file_ext: String,
    },
    AstPatchApply {
        original: String,
        hunks: Vec<crate::ast_arbiter::AstHunk>,
        file_ext: String,
    },
    // 8. Instant P2P Mobile QR Live-Sync & Ephemeral Preview Tunnel
    LiveTunnelCreate {
        local_port: u16,
        session_id: Option<String>,
    },
    LiveTunnelTelemetry {
        payload: String,
    },
    // 9. Autonomous Speculative TDD Loop
    TddCycleRun {
        intent: String,
        target_fn: String,
        file_ext: String,
    },
    // 10. Ephemeral Micro-WASM & Capability Sandbox
    MicroSandboxRun {
        command: String,
        args: Vec<String>,
        timeout_ms: Option<u64>,
    },
    // 11. Ambient Audio Earcons & Voice Flow Bridge
    AudioCuePlay {
        cue: crate::ambient_audio::AudioCueKind,
    },
    VoiceIntentParse {
        transcript: String,
    },
    // 12. Hot-Module CDP Live Patching
    CdpLivePatch {
        patch_kind: String,
        target: String,
        payload: String,
    },
    // 13. AST Skeleton Lens & Context Token Budgeter
    SkeletonLensProject {
        source_code: String,
        target_symbol: String,
        file_ext: String,
    },
    // 14. Lakandiwa Triple-Model Consensus Swarm
    LakandiwaSwarmRace {
        prompt: String,
        target_symbol: String,
        file_ext: String,
    },
    // 15. Instant Database CoW Time Machine
    DbCowSnapshotCreate {
        db_path: String,
        description: String,
    },
    DbCowSnapshotRollback {
        snapshot_file: String,
        source_path: String,
        blake3_hash: String,
    },
    // 16. Supply-Chain & Slopsquatting Hallucination Firewall
    SlopsquattingAudit {
        packages: Vec<String>,
        ecosystem: String,
    },
    // 17. Zero-Ops Cloud Launchpad & Ephemeral Edge Deployer
    CloudLaunchpadDeploy {
        workspace_path: Option<String>,
        project_name: String,
    },
    // 18. Living Architecture Flight Simulator
    FlightSimulatorTrace {
        workspace_path: Option<String>,
        endpoint_name: String,
    },
    // 19. Sub-Millisecond Predictive Shadow Synthesizer
    ShadowSynthesize {
        prefix: String,
    },
    // 20. Universal Offline API Mirage
    ApiMirageSimulate {
        endpoint: String,
        method: String,
    },
    // 21. In-Process Chaos Monkey & UI Invariant Fuzzer
    ChaosExperimentRun {
        target_component: String,
    },
    ChaosIdempotencyFuzz {
        key: String,
        runs: usize,
    },
    // 22. Autonomous Night-Shift Swarm Worktree Pipeline
    NightShiftDispatch {
        goal: String,
        base_branch: String,
    },
    // 23. Kernel-Level Memory-Only Ghost Envs
    VaultSeal {
        secrets: Vec<(String, String)>,
        passphrase: String,
    },
    VaultAuditDisk {
        disk_content: String,
    },
    // 24. Zero-Drift Polyglot Type Lock
    TypeLockSync {
        rust_source: String,
        existing_ts: Option<String>,
    },
    // 25. Spatial Cockpit Radar & Semantic Zoom
    SpatialRadarQuery {
        tier: crate::spatial_radar::ZoomTier,
    },
    // 26. Click-to-Source CDP Teleport
    CdpTeleportResolve {
        selector: String,
    },
    // 27. Full-Duplex Voice Flow Co-Pilot
    VoiceFlowProcess {
        transcript: String,
    },
    // 28. Headless Screenplay & PR Loom Tape
    PrTapeRecord {
        url: String,
        scenario_name: String,
    },
    // 29. Token FinOps & Dynamic Latency Arbitrage
    FinOpsRoute {
        prompt: String,
    },
    // 30. Zero-Knowledge Airgap Cloak & PII Sanitizer
    AirgapCloakText {
        text: String,
    },
    AirgapRehydrateText {
        response_text: String,
    },
    // 31. Active SQL Interceptor & Shadow Transaction Jail
    SqlGuardInspect {
        sql: String,
    },
    // 32. Deterministic Execution Replay & Rewind-Exec
    ExecutionReplayScrub {
        target_frame: Option<usize>,
    },
    // 33. Two-Way Visual Canvas & Live CSS/Tailwind Bi-Directional Mirror
    CanvasApplyTweak {
        source_code: String,
        target_file: String,
        symbol_name: String,
        mutation: crate::visual_canvas::CanvasStyleMutation,
    },
    // 34. Multi-Repo Swarm & Monorepo Mesh Federator
    MultiRepoFederate {
        goal: String,
    },
    // 35. Relational Time-Warp Data Synthesizer
    TimeWarpGenerate {
        #[serde(default)]
        config: Option<crate::time_warp_data::TimeWarpConfig>,
    },
    // 36. Structural Invariant Guardrails & Anti-Spaghetti Linter
    StructuralGuardrailsAudit {
        #[serde(default)]
        workspace_path: Option<String>,
    },
    // 37. Production Crash Auto-Triage & Reproduction Pipeline
    CrashTriageTrace {
        raw_trace: String,
    },
    // 38. Flaky Test Exterminator & Deterministic Stress Fuzzer
    FlakyDeflake {
        test_name: String,
        #[serde(default)]
        test_code: Option<String>,
    },
    // 39. Associative Neural Context & Infinite Cross-Session Memory
    ContextAnchorGenerate,
    ContextAnchorRecord {
        category: crate::neural_context_anchor::AnchorCategory,
        key: String,
        statement: String,
    },
    // 40. Universal LSP Ghost Daemon Bridge & Inline Prediction
    LspGhostComplete {
        params: crate::lsp_ghost_bridge::LspInlineCompletionParams,
    },
    // 41. Automated Rolling Context Compactor & Semantic Tree Pruner
    RollingCompactSession {
        session_id: String,
        turns: Vec<crate::rolling_compactor::ConversationTurn>,
        #[serde(default)]
        max_tokens: Option<usize>,
    },
    // 42. Atomic Conventional Git Micro-Commit Mirror
    GitMicroCommit {
        files: Vec<String>,
        intent: String,
        diff_preview: String,
    },
    // 43. Declarative Vibe Recipes & Runbook Engine
    VibeRecipeList,
    VibeRecipeRun {
        recipe_name: String,
    },
    // 44. Pre-Flight Behavioral Contract Matrix Generator
    BehaviorMatrixGenerate {
        symbol_name: String,
        intent_desc: String,
    },
    // 45. Live Agent Flight-Graph & Real-Time Task DAG Visualizer
    FlightGraphQuery {
        goal: String,
        #[serde(default)]
        active_step: usize,
    },
    // 46. Tree-sitter PageRank Symbol Graph & Token Density Repo-Map
    RepoMapRank {
        #[serde(default)]
        extensions: Vec<String>,
        #[serde(default)]
        token_budget: Option<usize>,
    },
    // 47. Cursor-Style Silent Pre-Flight Shadow Workspace & Speculative Repair
    ShadowPreflight {
        relative_path: String,
        candidate_content: String,
    },
    // 48. Claude Code-Style Terminal Stream Squeezer & High-Signal Digest
    StreamSqueeze {
        raw_output: String,
        #[serde(default)]
        max_tokens: Option<usize>,
    },
    // 49. Qodo-Style Test Integrity & Anti-Placebo Mutation Testing
    MutationAudit {
        source_code: String,
        file_name: String,
    },
    // 50. Bolt.new-Style Visual Click-to-Code DOM Telemetry & Inspector
    DomInspect {
        template_content: String,
        file_name: String,
        #[serde(default)]
        click_coords: Option<(f64, f64)>,
        #[serde(default)]
        css_selector: Option<String>,
    },
    // 51. Goose-Style Universal MCP Host Orchestrator & Tool Namespace Hub
    McpOrchestrate {
        action: String, // "discover", "start", "stop", "list", "call"
        #[serde(default)]
        server_name: Option<String>,
        #[serde(default)]
        tool_name: Option<String>,
        #[serde(default)]
        arguments: Option<serde_json::Value>,
    },
    // 52. Augment Code-Style Live Graph Watcher & Incremental In-Memory Index
    LiveGraphSync {
        #[serde(default)]
        extensions: Vec<String>,
    },
    // 53. Warp Terminal-Style Shell Panic Interceptor & 1-Key Auto-Repair
    ShellPanicDiagnose {
        command: String,
        exit_code: i32,
        stderr: String,
    },
    // 54. Copilot Workspace-Style Spec -> Plan -> Diff Task Decomposer
    SpecDecompose {
        intent: String,
        #[serde(default)]
        workspace_files: Vec<String>,
    },
    // 55. Continue.dev & Roo Code-Style Dynamic @Context Expander
    DynamicContextExpand {
        prompt: String,
    },
    // 56. Devin & Replit-Style Visual DOM Layout Regression Sentry
    VisualRegressionAudit {
        baseline_nodes: Vec<crate::visual_regression_sentry::VisualNodeSnapshot>,
        current_nodes: Vec<crate::visual_regression_sentry::VisualNodeSnapshot>,
    },
    // 57. Meta SapFix & Qodo-Style Continuous Autonomous Healing Loop
    ContinuousHealWatch {
        #[serde(default)]
        workspace_errors: Vec<String>,
        #[serde(default)]
        flaky_tests: Vec<String>,
    },
    // 58. Windsurf Cascade & Supermaven Next-Edit Anticipator
    AmbientPredict {
        file_path: String,
        symbol_name: String,
        change_kind: crate::ambient_predictor::EditKind,
        #[serde(default)]
        old_snippet: Option<String>,
        #[serde(default)]
        new_snippet: Option<String>,
    },
    // 59. Bolt.new & Devin Bidirectional DevTools Click-to-Source Sync
    CdpTweakSync {
        event: crate::cdp_tweak_mirror::DomTweakEvent,
        #[serde(default)]
        apply_to_disk: bool,
    },
    // 60. Continue.dev & Roo Code Composable Modes & Live Docs Harvester
    PromptModeHarvest {
        mode: crate::prompt_mode_docs_harvester::VibePromptMode,
        user_prompt: String,
        #[serde(default)]
        doc_targets: Vec<String>,
        #[serde(default)]
        raw_doc_content: Option<String>,
    },
    // 61. Replit Agent & WebContainers Zero-Config Ephemeral Stack Sandbox
    EphemeralSandboxSpinUp {
        stack_name: String,
        #[serde(default)]
        tables_to_seed: Vec<String>,
    },
    // 62. Qodo & Meta SapFix Anti-Placebo Mutation Testing Gatekeeper
    AntiPlaceboAudit {
        source_code: String,
        test_code: String,
    },
    // 63. Circular Loop Circuit Breaker & Anti-Thrashing Gate
    CircuitBreakerCheck {
        turn_number: usize,
        files: Vec<(String, String)>,
        #[serde(default)]
        error_output: Option<String>,
        intent: String,
    },
    // 64. Autonomous AppSec Sentinel & Pre-Apply Vulnerability Gate
    AppSecAudit {
        file_path: String,
        content: String,
    },
    // 65. Cognitive Walkthrough & Invariant Diff Explainer
    CognitiveWalkthroughExplain {
        intent: String,
        file_diffs: Vec<(String, String)>,
    },
    // 66. Click-to-Logic DevTools Teleport & Reactive State Sync
    LogicTeleport {
        event: crate::logic_teleport_mirror::DomInteractionEvent,
    },
    // 67. Instant Relational Mock API & Webhook Replay Fabric
    MockApiReplay {
        service: crate::relational_mock_api_replayer::MockServiceKind,
        endpoint: String,
        #[serde(default)]
        method: Option<String>,
        #[serde(default)]
        payload: Option<serde_json::Value>,
    },
    // 68. Embedded Visual Live-Preview Sidecar
    LivePreviewStart {
        #[serde(default)]
        port: Option<u16>,
        #[serde(default)]
        proxy_devserver_port: Option<u16>,
    },
    // 69. Multimodal Vision Ingestion & Clipboard Capture
    MultimodalVisionCapture {
        prompt: String,
        #[serde(default)]
        base64_image: Option<String>,
    },
    // 70. One-Click Public Share & Instant Tunneling
    ShareTunnelCreate {
        local_port: u16,
        #[serde(default)]
        custom_slug: Option<String>,
    },
    // 71. BaaS Auto-Graduation ("Mock-to-Real")
    BaasGraduate {
        resource_name: String,
        sample_json: serde_json::Value,
        target: crate::baas_graduate::BaasTarget,
    },
    // 72. Vibe-to-Spec Intent Expander
    VibeIntentExpand {
        prompt: String,
    },
    // 73. Invisible Dependency & Package Auto-Healing
    AutoDependencyHeal {
        compiler_log: String,
    },
    // 74. Embedded Webview HUD & Live Canvas Sidecar
    VisualCanvasHudStart {
        #[serde(default)]
        port: Option<u16>,
        #[serde(default)]
        preferred_model: Option<String>,
    },
    // 75. Zero-Config 1-Click Public Edge Deployer
    EdgeDeploy {
        provider: crate::edge_deployer::EdgeProvider,
        project_slug: String,
        #[serde(default)]
        write_configs: Option<bool>,
        #[serde(default)]
        custom_domain: Option<String>,
    },
    // 76. Visual Screenshot Annotation & Clipboard Xerox Engine
    VisualAnnotate {
        raw_annotation: String,
    },
    // 77. Collaborative Real-Time Multiplayer Vibe Swarm
    MultiplayerSwarmAction {
        session_id: String,
        action: String,
        payload: serde_json::Value,
    },
    // 78. Universal Companion Editor & LSP Sidecar Bridge
    CompanionBridgeSetup {
        editor: crate::companion_bridge::CompanionEditorKind,
        #[serde(default)]
        install: Option<bool>,
    },
    // 79. Instant Monetization & Auth Fabric
    SaasScaffold {
        config: crate::saas_monetization::SaasScaffoldConfig,
    },
    SaasWebhookVerify {
        provider: crate::saas_monetization::SaasProvider,
        payload: String,
        signature: String,
        secret: String,
    },
    // 80. Full-Duplex Ambient Conversational Voice Loop
    ContinuousVoiceTurn {
        speaker: String,
        transcript: String,
        #[serde(default)]
        intent_action: Option<String>,
        #[serde(default)]
        energy: Option<f32>,
    },
    // 81. Bi-Directional Figma & Design Token Synchronization
    FigmaSync {
        file_key: String,
        #[serde(default)]
        raw_json: Option<String>,
    },
    FigmaExport {
        component_name: String,
        markup: String,
    },
    // 82. Autonomous Production Database Shadow Simulator & Load Tester
    ShadowDbStress {
        profile: crate::shadow_db_stress::StressProfile,
        #[serde(default)]
        schema_sql: Option<String>,
    },
    // 83. Viral Social Graph & Dynamic OpenGraph Engine
    ViralOgGenerate {
        config: crate::viral_social_og::OgCardConfig,
    },
    // 84. Instant Mobile QR Teleport & PWA Matrix
    MobileQrTeleportGenerate {
        target_url: String,
        #[serde(default)]
        config: Option<crate::mobile_qr_teleport::MobilePwaConfig>,
    },
    // 85. Live Production Telemetry Ingest & Auto-Hotfixer
    ProductionHotfixTriage {
        payload: crate::production_hotfix_sentinel::ProductionErrorPayload,
    },
    // 86. AI Semantic Cost Gateway & Model Arbitrage
    LlmCostRoute {
        request: crate::llm_cost_gateway::LlmPromptRequest,
    },
    // 87. Zero-Cookie Privacy Funnel Analytics
    PrivacyFunnelQuery {
        #[serde(default)]
        event_to_record: Option<crate::privacy_funnel_analytics::AnalyticsEvent>,
    },
    PrivacyAnalyticsScaffold,
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
    AstPatchReport(crate::ast_arbiter::AstPatchReport),
    AstPatchApplied {
        code: String,
    },
    LiveTunnelSessionReport(crate::live_tunnel::LiveTunnelSession),
    MobileTelemetryReport(crate::live_tunnel::MobileTelemetryEvent),
    TddCycleReport(crate::tdd_loop::TddReport),
    MicroSandboxExecutionReport(crate::micro_sandbox::MicroSandboxReport),
    AudioCuePlayed {
        cue: crate::ambient_audio::AudioCueKind,
    },
    VoiceIntentReport(Option<crate::ambient_audio::VoiceDiffIntent>),
    // 12. Hot-Module CDP Live Patching
    CdpPatchResult(crate::cdp_patcher::CdpPatchReport),
    // 13. AST Skeleton Lens & Context Token Budgeter
    SkeletonLensResult(crate::skeleton_lens::SkeletonLensReport),
    // 14. Lakandiwa Triple-Model Consensus Swarm
    LakandiwaSwarmResult(crate::lakandiwa_swarm::SwarmConsensusReport),
    // 15. Instant Database CoW Time Machine
    DbCowSnapshotCreated(crate::db_cow_time_machine::DbSnapshotRecord),
    DbCowSnapshotRestored { bytes_restored: u64 },
    // 16. Supply-Chain & Slopsquatting Hallucination Firewall
    SlopsquattingReport(crate::slopsquatting_firewall::FirewallAuditReport),
    // 17. Zero-Ops Cloud Launchpad & Ephemeral Edge Deployer
    CloudLaunchpadReport(crate::cloud_launchpad::LaunchpadDeploymentReport),
    // 18. Living Architecture Flight Simulator
    FlightSimulatorResult(crate::flight_simulator::FlightSimulatorReport),
    // 19. Sub-Millisecond Predictive Shadow Synthesizer
    ShadowSynthesizerResult(crate::shadow_synthesizer::ShadowSynthesisReport),
    // 20. Universal Offline API Mirage
    ApiMirageResult(crate::api_mirage::MirageExecutionReport),
    // 21. In-Process Chaos Monkey & UI Invariant Fuzzer
    ChaosMonkeyResult(crate::chaos_monkey::ChaosMonkeyReport),
    ChaosTrialResult(crate::chaos_monkey::ChaosTrialResult),
    // 22. Autonomous Night-Shift Swarm Worktree Pipeline
    NightShiftResult(crate::nightshift_pipeline::NightShiftPipelineReport),
    // 23. Kernel-Level Memory-Only Ghost Envs
    VaultSealResult(crate::vault_ghost_envs::GhostVaultSeal),
    VaultAuditResult(crate::vault_ghost_envs::GhostEnvAuditReport),
    // 24. Zero-Drift Polyglot Type Lock
    TypeLockResult(crate::polyglot_typelock::TypeLockSyncReport),
    // 25. Spatial Cockpit Radar & Semantic Zoom
    SpatialRadarResult(crate::spatial_radar::SpatialRadarReport),
    // 26. Click-to-Source CDP Teleport
    CdpTeleportResult(crate::cdp_teleport::TeleportTargetReport),
    // 27. Full-Duplex Voice Flow Co-Pilot
    VoiceFlowResult(crate::voice_flow::VoiceFlowSessionReport),
    // 28. Headless Screenplay & PR Loom Tape
    PrTapeResult(crate::pr_tape::PrTapeReport),
    // 29. Token FinOps & Dynamic Latency Arbitrage
    FinOpsResult(crate::finops_arbitrage::FinOpsReport),
    // 30. Zero-Knowledge Airgap Cloak & PII Sanitizer
    AirgapCloakResult(crate::airgap_cloak::CloakAuditReport),
    AirgapRehydrateResult { rehydrated_text: String },
    // 31. Active SQL Interceptor & Shadow Transaction Jail
    SqlGuardResult(crate::sql_guard::SqlGuardReport),
    // 32. Deterministic Execution Replay & Rewind-Exec
    ExecutionReplayResult(crate::execution_replay::ExecutionTraceReport),
    // 33. Two-Way Visual Canvas & Live CSS/Tailwind Bi-Directional Mirror
    CanvasMutationResult(crate::visual_canvas::CanvasMutationReport),
    // 34. Multi-Repo Swarm & Monorepo Mesh Federator
    MultiRepoFederateResult(crate::multi_repo_federator::FederatedSyncReport),
    // 35. Relational Time-Warp Data Synthesizer
    TimeWarpResult(crate::time_warp_data::TimeWarpReport),
    // 36. Structural Invariant Guardrails & Anti-Spaghetti Linter
    StructuralGuardrailsResult(crate::structural_guardrails::GuardrailReport),
    // 37. Production Crash Auto-Triage & Reproduction Pipeline
    CrashTriageResult(crate::crash_triage::CrashTriageReport),
    // 38. Flaky Test Exterminator & Deterministic Stress Fuzzer
    FlakyDeflakeResult(crate::flaky_exterminator::FlakyAnalysisReport),
    // 39. Associative Neural Context & Infinite Cross-Session Memory
    ContextAnchorResult(crate::neural_context_anchor::ContextAnchorReport),
    ContextAnchorRecorded { id: String },
    // 40. Universal LSP Ghost Daemon Bridge & Inline Prediction
    LspGhostResult(crate::lsp_ghost_bridge::LspGhostReport),
    // 41. Automated Rolling Context Compactor & Semantic Tree Pruner
    RollingCompactResult(crate::rolling_compactor::CompactionReport),
    // 42. Atomic Conventional Git Micro-Commit Mirror
    GitMicroCommitResult(crate::git_micro_commit::MicroCommitReport),
    // 43. Declarative Vibe Recipes & Runbook Engine
    VibeRecipeListResult(Vec<crate::vibe_recipe::VibeRecipe>),
    VibeRecipeRunResult(crate::vibe_recipe::RecipeExecutionReport),
    // 44. Pre-Flight Behavioral Contract Matrix Generator
    BehaviorMatrixResult(crate::behavior_matrix::BehaviorMatrixReport),
    // 45. Live Agent Flight-Graph & Real-Time Task DAG Visualizer
    FlightGraphResult(crate::flight_graph::FlightGraphReport),
    // 46. Tree-sitter PageRank Symbol Graph & Token Density Repo-Map
    RepoMapRankResult(String),
    // 47. Cursor-Style Silent Pre-Flight Shadow Workspace & Speculative Repair
    ShadowPreflightResult(crate::shadow_workspace::PreflightResult),
    // 48. Claude Code-Style Terminal Stream Squeezer & High-Signal Digest
    StreamSqueezeResult(crate::stream_squeezer::SqueezedDigest),
    // 49. Qodo-Style Test Integrity & Anti-Placebo Mutation Testing
    MutationAuditResult(crate::mutation_fuzzer::MutationReport),
    // 50. Bolt.new-Style Visual Click-to-Code DOM Telemetry & Inspector
    DomInspectResult {
        elements: Vec<crate::dom_preview_bridge::DomElement>,
        hierarchy_map: String,
        target_element: Option<crate::dom_preview_bridge::DomElement>,
    },
    // 51. Goose-Style Universal MCP Host Orchestrator & Tool Namespace Hub
    McpOrchestrateResult {
        active_servers: std::collections::HashMap<String, crate::mcp_host_orchestrator::McpServerStatus>,
        tools: Vec<crate::mcp_host_orchestrator::NamespacedMcpTool>,
        tool_output: Option<serde_json::Value>,
    },
    // 52. Augment Code-Style Live Graph Watcher & Incremental In-Memory Index
    LiveGraphSyncResult(crate::live_graph_watcher::LiveGraphSummary),
    // 53. Warp Terminal-Style Shell Panic Interceptor & 1-Key Auto-Repair
    ShellPanicDiagnosisResult(crate::shell_panic_hook::ShellPanicDiagnosis),
    // 54. Copilot Workspace-Style Spec -> Plan -> Diff Task Decomposer
    SpecDecomposeResult(crate::spec_decomposer::SpecDecompositionReport),
    // 55. Continue.dev & Roo Code-Style Dynamic @Context Expander
    DynamicContextExpandResult(crate::dynamic_at_context::ExpandedPromptResult),
    // 56. Devin & Replit-Style Visual DOM Layout Regression Sentry
    VisualRegressionResult(crate::visual_regression_sentry::VisualRegressionReport),
    // 57. Meta SapFix & Qodo-Style Continuous Autonomous Healing Loop
    ContinuousHealResult(crate::continuous_flaky_watchdog::WatchdogReport),
    // 58. Windsurf Cascade & Supermaven Next-Edit Anticipator
    AmbientPredictResult(crate::ambient_predictor::PredictionBatchReport),
    // 59. Bolt.new & Devin Bidirectional DevTools Click-to-Source Sync
    CdpTweakSyncResult(crate::cdp_tweak_mirror::TweakSyncReport),
    // 60. Continue.dev & Roo Code Composable Modes & Live Docs Harvester
    PromptModeHarvestResult(crate::prompt_mode_docs_harvester::ModeHarvesterReport),
    // 61. Replit Agent & WebContainers Zero-Config Ephemeral Stack Sandbox
    EphemeralSandboxResult(crate::ephemeral_stack_sandbox::SandboxReport),
    // 62. Qodo & Meta SapFix Anti-Placebo Mutation Testing Gatekeeper
    AntiPlaceboAuditResult(crate::anti_placebo_gatekeeper::AntiPlaceboReport),
    // 63. Circular Loop Circuit Breaker & Anti-Thrashing Gate
    CircuitBreakerResult(crate::circular_circuit_breaker::CircuitBreakerReport),
    // 64. Autonomous AppSec Sentinel & Pre-Apply Vulnerability Gate
    AppSecAuditResult(crate::appsec_sentinel::AppSecReport),
    // 65. Cognitive Walkthrough & Invariant Diff Explainer
    CognitiveWalkthroughResult(crate::cognitive_walkthrough::WalkthroughReport),
    // 66. Click-to-Logic DevTools Teleport & Reactive State Sync
    LogicTeleportResult(crate::logic_teleport_mirror::LogicTeleportReport),
    // 67. Instant Relational Mock API & Webhook Replay Fabric
    MockApiReplayResult(crate::relational_mock_api_replayer::MockReplayReport),
    // 68. Embedded Visual Live-Preview Sidecar
    LivePreviewResult {
        port: u16,
        base_url: String,
        status: String,
    },
    // 69. Multimodal Vision Ingestion & Clipboard Capture
    MultimodalVisionResult(crate::multimodal_vision::MultimodalPromptPayload),
    // 70. One-Click Public Share & Instant Tunneling
    ShareTunnelResult(crate::share_tunnel::ShareTunnelSession),
    // 71. BaaS Auto-Graduation ("Mock-to-Real")
    BaasGraduationResult(crate::baas_graduate::BaasGraduationReport),
    // 72. Vibe-to-Spec Intent Expander
    VibeIntentExpandResult(crate::vibe_intent_expander::ExpandedVibeSpec),
    // 73. Invisible Dependency & Package Auto-Healing
    AutoDependencyHealResult(crate::auto_dependency_healer::DependencyHealingReport),
    // 74. Embedded Webview HUD & Live Canvas Sidecar
    VisualCanvasHudResult(crate::visual_canvas_hud::CanvasHudReport),
    // 75. Zero-Config 1-Click Public Edge Deployer
    EdgeDeployResult(crate::edge_deployer::EdgeDeployReport),
    // 76. Visual Screenshot Annotation & Clipboard Xerox Engine
    VisualAnnotateResult(crate::visual_annotation::VisualAnnotationReport),
    // 77. Collaborative Real-Time Multiplayer Vibe Swarm
    MultiplayerSwarmResult(crate::multiplayer_swarm::MultiplayerSessionReport),
    // 78. Universal Companion Editor & LSP Sidecar Bridge
    CompanionBridgeResult(crate::companion_bridge::CompanionBridgeConfigReport),
    // 79. Instant Monetization & Auth Fabric
    SaasScaffoldResult(crate::saas_monetization::SaasScaffoldReport),
    SaasWebhookVerifyResult(crate::saas_monetization::WebhookVerificationResult),
    // 80. Full-Duplex Ambient Conversational Voice Loop
    ContinuousVoiceResult(crate::continuous_voice::VoiceSessionReport),
    // 81. Bi-Directional Figma & Design Token Synchronization
    FigmaSyncResult(crate::figma_bridge::FigmaSyncReport),
    FigmaExportResult(crate::figma_bridge::VectorCanvasExportReport),
    // 82. Autonomous Production Database Shadow Simulator & Load Tester
    ShadowDbStressResult(crate::shadow_db_stress::ShadowDbStressReport),
    // 83. Viral Social Graph & Dynamic OpenGraph Engine
    ViralOgResult(crate::viral_social_og::ViralOgReport),
    // 84. Instant Mobile QR Teleport & PWA Matrix
    MobileQrTeleportResult(crate::mobile_qr_teleport::QrDisplayReport),
    // 85. Live Production Telemetry Ingest & Auto-Hotfixer
    ProductionHotfixResult(crate::production_hotfix_sentinel::HotfixReproductionReport),
    // 86. AI Semantic Cost Gateway & Model Arbitrage
    LlmCostResult(crate::llm_cost_gateway::LlmCostReport),
    // 87. Zero-Cookie Privacy Funnel Analytics
    PrivacyFunnelResult(crate::privacy_funnel_analytics::FunnelReport),
    PrivacyAnalyticsScaffoldResult(crate::privacy_funnel_analytics::AnalyticsScaffoldReport),
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
