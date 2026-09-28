pub mod agent;
pub mod ambient_ast;
pub mod ast_pruner;
pub mod audio;
pub mod auth;
pub mod auto_spec;
pub mod browser_snoop;
pub mod clipboard;
pub mod crud;
pub mod db_sentinel;
pub mod drift_lock;
pub mod env_sentinel;
pub mod error;
pub mod forge;
pub mod glance;
pub mod impact;
pub mod memory;
pub mod package_guard;
pub mod protocol;
pub mod providers;
pub mod repomap;
pub mod rescue;
pub mod rules;
pub mod security;
pub mod style;
pub mod ast_rewind;
pub mod blueprint;
pub mod browser_hud;
pub mod seed_engine;
pub mod syntax_slicer;
pub mod trace;
pub mod traits;
pub mod variant_race;
pub mod web_browser;
pub mod mcp;
pub mod timeline;
pub mod verification_gate;
pub mod shell_hook;
pub mod ambient_vibe;
pub mod ast_arbiter;
pub mod live_tunnel;
pub mod tdd_loop;
pub mod micro_sandbox;
pub mod ambient_audio;
pub mod cdp_patcher;
pub mod skeleton_lens;
pub mod lakandiwa_swarm;
pub mod db_cow_time_machine;
pub mod slopsquatting_firewall;
pub mod cloud_launchpad;
pub mod flight_simulator;
pub mod shadow_synthesizer;
pub mod api_mirage;
pub mod chaos_monkey;
pub mod nightshift_pipeline;
pub mod vault_ghost_envs;
pub mod polyglot_typelock;
pub mod spatial_radar;
pub mod cdp_teleport;
pub mod voice_flow;
pub mod pr_tape;
pub mod finops_arbitrage;
pub mod airgap_cloak;
pub mod sql_guard;
pub mod execution_replay;
pub mod visual_canvas;
pub mod multi_repo_federator;
pub mod time_warp_data;
pub mod structural_guardrails;
pub mod crash_triage;
pub mod flaky_exterminator;
pub mod neural_context_anchor;
pub mod lsp_ghost_bridge;
pub mod rolling_compactor;
pub mod git_micro_commit;
pub mod vibe_recipe;
pub mod behavior_matrix;
pub mod flight_graph;
pub mod repo_map_ranker;
pub mod shadow_workspace;
pub mod stream_squeezer;
pub mod mutation_fuzzer;
pub mod dom_preview_bridge;
pub mod mcp_host_orchestrator;
pub mod live_graph_watcher;
pub mod shell_panic_hook;
pub mod spec_decomposer;
pub mod dynamic_at_context;
pub mod visual_regression_sentry;
pub mod continuous_flaky_watchdog;
pub mod ambient_predictor;
pub mod cdp_tweak_mirror;
pub mod prompt_mode_docs_harvester;
pub mod ephemeral_stack_sandbox;
pub mod anti_placebo_gatekeeper;
pub mod circular_circuit_breaker;
pub mod appsec_sentinel;
pub mod cognitive_walkthrough;
pub mod logic_teleport_mirror;
pub mod relational_mock_api_replayer;
pub mod visual_live_preview;
pub mod multimodal_vision;
pub mod share_tunnel;
pub mod baas_graduate;
pub mod vibe_intent_expander;
pub mod auto_dependency_healer;
pub mod visual_canvas_hud;
pub mod edge_deployer;
pub mod visual_annotation;
pub mod multiplayer_swarm;
pub mod companion_bridge;
pub mod saas_monetization;
pub mod continuous_voice;
pub mod figma_bridge;
pub mod shadow_db_stress;
pub mod viral_social_og;
pub mod mobile_qr_teleport;
pub mod production_hotfix_sentinel;
pub mod llm_cost_gateway;
pub mod privacy_funnel_analytics;
pub mod rails_engine;
pub mod project_coordinator;
pub mod tiered_rules_engine;
pub mod autopilot_pipeline;
pub mod decision_explainer;
pub mod merkle_smart_index;
pub mod rollout_health_watch;
pub mod ai_pr_security_audit;
pub mod preview_cloud_deployer;
pub mod multi_dev_collab;
pub mod prompt_lab_workspace;
pub mod language_intelligence_pack;
pub mod native_mobile_matrix;
pub mod session_budget_envelope;
pub mod vscode_extension_bridge;
pub mod ci_streamer;
pub mod plan_mode;
pub mod ticket_ingest;
pub mod profile_manager;
pub mod git_hook_guard;
pub mod conventions_harvester;
pub mod worktree_queue;
pub mod agentic_reviewer;
pub mod swe_bench_harness;
pub mod quickstart_synthesizer;
pub mod agent_registry;
pub mod provenance_ledger;
pub mod remote_tunnel;
pub mod observation_bus;
pub mod stack_preset_fabric;

pub use ci_streamer::{
    CiEventKind, CiExecutionConfig, CiExecutionSummary, CiOutputFormat, CiStreamerEngine,
};
pub use plan_mode::{
    ExecutionPlan, PlanEngine, PlanStep, PlanStepStatus,
};
pub use ticket_ingest::{
    AcceptanceCriterion, ParsedTicketContext, TicketIngestEngine, TicketProvider,
};
pub use profile_manager::{
    ProfileEngine, ProjectCodingConventions, ProjectProfile,
};
pub use git_hook_guard::{
    GitHookEngine, GitHookType, HookInstallReport, HookScanResult,
};
pub use conventions_harvester::{
    ConventionRule, ConventionsDna, ConventionsHarvester,
};
pub use worktree_queue::{
    WorktreeJob, WorktreeJobStatus, WorktreeQueueEngine, WorktreeQueueReport,
};
pub use agentic_reviewer::{
    AgenticReviewerEngine, InlineReviewComment, PrReviewReport, ReviewSeverity,
};
pub use swe_bench_harness::{
    BenchmarkRunReport, SweBenchEngine, TestCaseResult,
};
pub use quickstart_synthesizer::{
    QuickstartReport, QuickstartSpec, QuickstartSynthesizer,
};
pub use agent_registry::{
    AgentPluginManifest, AgentRegistryEngine, RegistrySearchReport,
};
pub use provenance_ledger::{
    AuthorshipAuditReport, AuthorshipSpan, ProvenanceEngine,
};
pub use remote_tunnel::{
    RemoteTunnelConfig, RemoteTunnelEngine, RemoteTunnelReport,
};
pub use observation_bus::{
    ObservationBusEngine, ObservationChannel, ObservationEvent, ObservationStreamReport,
};
pub use stack_preset_fabric::{
    ManagedService, StackPresetEngine, StackWireupConfig, StackWireupReport,
};

pub use rails_engine::{
    MigrationHazard, MigrationRiskLevel, MigrationSafetyReport, NPlusOneIssue, NPlusOneReport,
    ParsedRoute, RailsAppInfo, RailsDbAdapter, RailsEngine, RailsScaffoldResult, ScaffoldField,
};
pub use project_coordinator::{
    AdrRecord, ProjectAdrStatus, ProjectCoordinator, ProjectCoordinatorSnapshot, ProjectCoordinatorState,
    ProjectTask, TaskPriority, TaskStatus,
};
pub use tiered_rules_engine::{
    EvaluatedRuleset, RuleTier, TieredRule, TieredRulesEngine,
};
pub use autopilot_pipeline::{
    AutopilotPatchItem, AutopilotPipeline, AutopilotReport, AutopilotStage, ImpactPlan,
    IngestedTicket, PullRequestMetadata, VerificationSummary,
};
pub use decision_explainer::{
    DecisionExplanationReport, DecisionExplainer, DecisionTradeoff,
};
pub use merkle_smart_index::{
    FileLeafNode, MerkleCodebaseSnapshot, MerkleDiffReport, MerkleSmartIndex,
};
pub use rollout_health_watch::{
    RolloutHealthVerdict, RolloutHealthWatch, RolloutWatchConfig, RolloutWatchReport, TelemetrySample,
};
pub use ai_pr_security_audit::{
    AiPrSecurityAudit, PrSecurityReport, VulnerabilityFinding, VulnSeverity,
};
pub use preview_cloud_deployer::{
    PreviewCloudDeployer, PreviewCloudProvider, PreviewDeploymentConfig, PreviewDeploymentReport,
};
pub use multi_dev_collab::{
    CollabConflictWarning, CollabPatchIntent, CollabPeer, CollabSessionState, MultiDevCollabEngine,
};
pub use prompt_lab_workspace::{
    PromptBenchmarkResult, PromptLabReport, PromptLabWorkspace, PromptVariant,
};
pub use language_intelligence_pack::{
    FrameworkDiagnosisReport, LanguageIntelligencePack, LanguagePackInfo, SupportedFramework,
};
pub use native_mobile_matrix::{
    MobileCrashDiagnosis, MobileEnvironmentReport, MobilePlatformKind, NativeMobileMatrix,
    SymbolicatedCrashFrame,
};
pub use session_budget_envelope::{
    BudgetCallRecord, BudgetGovernorState, BudgetStatusReport, BudgetTier, SessionBudgetEnvelope,
};
pub use vscode_extension_bridge::{
    VsCodeBridgeConfig, VsCodeExtensionBridge, VsCodeExtensionScaffoldReport,
};

pub use saas_monetization::{
    PricingTier, SaasMonetizationFabric, SaasProvider, SaasScaffoldConfig, SaasScaffoldReport,
    WebhookVerificationResult,
};
pub use continuous_voice::{
    AcousticEarcon, ContinuousVoiceConfig, ContinuousVoiceDuplex, VoiceDuplexState,
    VoiceSessionReport, VoiceTurnEvent,
};
pub use figma_bridge::{
    ColorToken, FigmaComponentNode, FigmaDesignBridge, FigmaSyncReport, FigmaTokenSet,
    SpacingToken, TypographyToken, VectorCanvasExportReport,
};
pub use shadow_db_stress::{
    DbWorkloadMetrics, IndexRecommendation, ShadowDbStressFuzzer, ShadowDbStressReport,
    StressProfile,
};
pub use viral_social_og::{
    OgCardConfig, SocialMetaTag, ViralAuditScorecard, ViralOgReport, ViralSocialOgEngine,
};
pub use mobile_qr_teleport::{
    MobilePwaConfig, MobileQrTeleport, PwaScaffoldReport, QrDisplayReport,
};
pub use production_hotfix_sentinel::{
    HotfixPatch, HotfixReproductionReport, ProductionErrorPayload, ProductionHotfixSentinel,
};
pub use llm_cost_gateway::{
    GatewaySpendMetrics, LlmCostGateway, LlmCostReport, LlmGatewayRoutingDecision,
    LlmPromptRequest,
};
pub use privacy_funnel_analytics::{
    AnalyticsEvent, AnalyticsScaffoldReport, FunnelReport, FunnelStageMetric,
    PrivacyFunnelAnalytics,
};

pub use visual_canvas_hud::{
    AstComponentMapping, CanvasHudConfig, CanvasHudHandle, CanvasHudReport,
    CssLiveTweak, HudBoundingBox, HudElementSelection, VisualCanvasHud,
};
pub use edge_deployer::{
    DetectedFramework, EdgeDeployConfig, EdgeDeployReport, EdgeDeployer, EdgeProvider,
};
pub use visual_annotation::{
    AnnotationKind, ComponentSpatialBinding, SpatialCoords, VisualAnnotationItem,
    VisualAnnotationParser, VisualAnnotationReport,
};
pub use multiplayer_swarm::{
    FlightGraphSyncReport, MultiplayerSession, MultiplayerSessionReport, MultiplayerSwarmHub,
    PresenceUpdateReport, SharedRaceSyncReport, SwarmPeer, SwarmPeerRole,
};
pub use companion_bridge::{
    CompanionBridgeConfig, CompanionBridgeConfigReport, CompanionBridgeInstallReport,
    CompanionConfigFile, CompanionEditorBridge, CompanionEditorKind,
};

pub use visual_live_preview::{LivePreviewConfig, LivePreviewHandle, PreviewDomClickEvent, VisualLivePreview};
pub use multimodal_vision::{ImageFormat, MultimodalPromptPayload, MultimodalVisionEngine, VisionImage};
pub use share_tunnel::{ShareTunnelEngine, ShareTunnelSession};
pub use baas_graduate::{BaasGraduateEngine, BaasGraduationReport, BaasTarget, InferredColumn};
pub use vibe_intent_expander::{DesignTokens, ExpandedVibeSpec, MotionTokens, VibeIntentExpander};
pub use auto_dependency_healer::{AutoDependencyHealer, DependencyHealingReport, HealingAction, MissingPackage};

pub use ambient_predictor::{AmbientPredictor, EditEvent, EditKind, PredictedNextEdit, PredictionBatchReport};
pub use cdp_tweak_mirror::{CdpTweakMirror, DomTweakEvent, TweakSyncReport};
pub use prompt_mode_docs_harvester::{DocsHarvestResult, ModeHarvesterReport, PromptModeDocsHarvester, VibePromptMode};
pub use ephemeral_stack_sandbox::{EphemeralStackSandbox, SandboxReport, SandboxSession};
pub use anti_placebo_gatekeeper::{AntiPlaceboGatekeeper, AntiPlaceboReport, CodeMutant, MutationKind, PlaceboMutantStatus};
pub use circular_circuit_breaker::{CircuitBreakerReport, CircularCircuitBreaker, CodeStateSnapshot, LoopPatternKind};
pub use appsec_sentinel::{AppSecReport, AppSecSentinel, SecurityFinding, SecuritySeverity, VulnerabilityCategory};
pub use cognitive_walkthrough::{CognitiveCard, CognitiveWalkthrough, WalkthroughReport};
pub use logic_teleport_mirror::{DomInteractionEvent, LogicTarget, LogicTeleportMirror, LogicTeleportReport};
pub use relational_mock_api_replayer::{MockReplayReport, MockServiceKind, RelationalMockApiReplayer};

pub use lsp_ghost_bridge::{LspGhostBridge, LspGhostReport, LspInlineCompletionItem, LspInlineCompletionParams};
pub use rolling_compactor::{CompactionReport, ConversationTurn, RollingCompactor};
pub use git_micro_commit::{GitMicroCommitMirror, MicroCommitPlan, MicroCommitReport};
pub use vibe_recipe::{RecipeExecutionReport, RecipeStep, StepExecutionLog, VibeRecipe, VibeRecipeEngine};
pub use behavior_matrix::{BehaviorMatrixEngine, BehaviorMatrixReport, BehavioralContractItem, ContractDimension};
pub use flight_graph::{FlightGraphReport, FlightGraphVisualizer, FlightNode, FlightNodeStatus};
pub use repo_map_ranker::{RankedSymbol, RankedSymbolKind, RepoMapRanker, RepoSymbolGraph};
pub use shadow_workspace::{BuildRunner, PreflightResult, ShadowDiagnostic, ShadowWorkspace};
pub use stream_squeezer::{SqueezedDigest, StreamSqueezer};
pub use mutation_fuzzer::{MutantCandidate, MutantStatus, MutationFuzzer, MutationOperator, MutationReport};
pub use dom_preview_bridge::{BoundingBox, DomElement, DomPreviewBridge};
pub use mcp_host_orchestrator::{McpHostOrchestrator, McpServerStatus, NamespacedMcpTool};
pub use live_graph_watcher::{IndexedSymbol, LiveGraphSummary, LiveGraphWatcher};
pub use shell_panic_hook::{ShellFailureCategory, ShellIncident, ShellPanicDiagnosis, ShellPanicHook};
pub use spec_decomposer::{DecomposedStep, SpecDecomposer, SpecDecompositionReport, StepStatus};
pub use dynamic_at_context::{AtDirectiveKind, ContextAttachment, DynamicAtContext, ExpandedPromptResult};
pub use visual_regression_sentry::{VisualDeltaType, VisualNodeSnapshot, VisualRegressionDelta, VisualRegressionReport, VisualRegressionSentry};
pub use continuous_flaky_watchdog::{AutonomousHealingAction, ContinuousFlakyWatchdog, WatchdogReport};

pub use visual_canvas::{CanvasMutationReport, CanvasStyleMutation, VisualCanvasEngine};
pub use multi_repo_federator::{FederatedRepoTask, FederatedSyncReport, MultiRepoFederator};
pub use time_warp_data::{SyntheticEntity, TableSummary, TimeWarpConfig, TimeWarpDataEngine, TimeWarpReport};
pub use structural_guardrails::{GuardrailReport, GuardrailViolation, StructuralGuardrails, ViolationSeverity};
pub use crash_triage::{CrashTriagePipeline, CrashTriageReport};
pub use flaky_exterminator::{FlakyAnalysisReport, FlakyExterminator, FlakyTestRun};
pub use neural_context_anchor::{AnchorCategory, AnchorItem, ContextAnchorReport, NeuralContextAnchor};

pub use cdp_teleport::{CdpTeleportEngine, TeleportTarget, TeleportTargetReport};
pub use voice_flow::{VoiceActionDispatch, VoiceCommandKind, VoiceFlowEngine, VoiceFlowSessionReport};
pub use pr_tape::{PrTapeEngine, PrTapeReport, ScreenplayStep};
pub use finops_arbitrage::{ArbitrageRouteDecision, FinOpsArbitrageEngine, FinOpsReport, ModelTier};
pub use airgap_cloak::{AirgapCloakEngine, CloakAuditReport, CloakedEntity};
pub use sql_guard::{SafetyVerdict, SqlGuardEngine, SqlGuardReport};
pub use execution_replay::{ExecutionReplayEngine, ExecutionTraceReport, ReplayFrame};

pub use shadow_synthesizer::{ShadowPrediction, ShadowSynthesisReport, ShadowSynthesizer};
pub use api_mirage::{ApiMirageEngine, MirageEndpoint, MirageExecutionReport};
pub use chaos_monkey::{ChaosMonkeyEngine, ChaosMonkeyReport, ChaosTrialResult, FuzzVector, FuzzVectorKind};
pub use nightshift_pipeline::{NightShiftPipeline, NightShiftPipelineReport, NightShiftStage, StageLog};
pub use vault_ghost_envs::{GhostEnvAuditReport, GhostVaultSeal, VaultGhostEnvs};
pub use polyglot_typelock::{PolyglotField, PolyglotModel, PolyglotTypeLock, TypeDriftItem, TypeLockSyncReport};
pub use spatial_radar::{RadarNode, SpatialCockpitRadar, SpatialRadarReport, ZoomTier};
pub use cdp_patcher::{CdpLivePatcher, CdpPatchKind, CdpPatchReport};
pub use skeleton_lens::{AstSkeletonLens, SkeletonLensReport};
pub use lakandiwa_swarm::{LakandiwaSwarmArbiter, SwarmCandidate, SwarmConsensusReport};
pub use db_cow_time_machine::{DbCowTimeMachine, DbSnapshotRecord};
pub use slopsquatting_firewall::{FirewallAuditReport, PackageAuditItem, PackageRiskLevel, SlopsquattingFirewall};
pub use cloud_launchpad::{CloudLaunchpad, LaunchpadDeploymentReport};
pub use flight_simulator::{ArchitectureFlightSimulator, FlightHop, FlightSimulatorReport};
pub use ast_arbiter::{AstHunk, AstHunkDecision, AstHunkKind, AstPatchArbiter, AstPatchReport};
pub use live_tunnel::{LiveTunnelManager, LiveTunnelSession, MobileTelemetryEvent};
pub use tdd_loop::{RedGreenTddEngine, TddPhase, TddReport, TddSpec};
pub use micro_sandbox::{MicroSandboxConfig, MicroSandboxEngine, MicroSandboxReport, SandboxCapability};
pub use ambient_audio::{AmbientAudioEngine, AudioCueKind, VoiceDiffIntent};
pub use agent::{AgentEvent, AgentLoopConfig, AgentMessage, AgentRole, AgentSessionReport, AgentStepInfo, AgentToolCall, AgentToolResult, ReActAgentEngine};
pub use ambient_vibe::{AmbientVibeConfig, AmbientVibeEngine, TestSuiteConfig, VibeWatchEvent};
pub use verification_gate::{GoldenInvariant, VerificationCertificate, VerificationCheckKind, VerificationGate, VerificationStatus, VerificationStepResult};
pub use shell_hook::{CrashCategory, CrashDiagnosis, CrashInterceptor, CrashRecord, ShellHookGenerator, SupportedShell};
pub use ambient_ast::{AmbientAstFollower, AmbientContext};
pub use ast_pruner::{AstPruner, PrunedAstResult};
pub use ast_rewind::{AstRewindTimeline, SymbolRevision};
pub use audio::play_vibe_chime;
pub use auth::{GeminiOAuthManager, GeminiOAuthTokens};
pub use auto_spec::{GoldenSpec, InvariantType, SpecExecutionReport, SpecViolation, TestVector};
pub use blueprint::{ArchitectureBlueprint, ComponentKind, ComponentNode, DependencyEdge};
pub use browser_hud::{BrowserIncident as HudBrowserIncident, BrowserIncidentKind as HudIncidentKind, BrowserIncidentSeverity as HudIncidentSeverity, BrowserLiveHud, BrowserTelemetry as HudTelemetry};
pub use browser_snoop::{BrowserHealthReport, BrowserHealthVerdict, BrowserIncident, BrowserIncidentKind, BrowserSeverity};
pub use clipboard::ClipboardHelper;
pub use crud::{AgyCrud, CommandOptions, CommandResult, DirEntryInfo, FindEntry, GrepMatch, ReplaceOptions, ViewFileOptions, ViewFileResult};
pub use db_sentinel::{ColumnSchema, MigrationSafetyLevel, SchemaDriftItem, SchemaDriftReport, TableSchema};
pub use drift_lock::{ArchitecturalDna, ComplianceAuditReport, DnaPillars, DriftSeverity, DriftViolation};
pub use env_sentinel::{EnvAuditReport, EnvReconciliationItem, EnvSentinel, EnvVarStatus, EnvVarUsage, SecretLeakFinding};
pub use error::{HgbError, Result};
pub use forge::{ForgeEngine, ForgeReport, ForgeStack};
pub use glance::{ComponentSynthesisResult, CssSuggestion, DefectCategory, DefectSeverity, ImageMimeType, ImagePayload, LayoutDefect, VisualInspectionReport, synthesize_component};
pub use impact::{ImpactRadar, ImpactReport, RiskLevel, SymbolCallsite};
pub use mcp::{McpClient, McpConfigFile, McpServerConfig, McpTool, McpToolAdapter};
pub use memory::{
    AdrStatus, ArchitecturalDecision, DebtSeverity, MemoryDocument, ProjectMemoryLedger,
    ProjectRoadmapMilestone, TechDebtEntry,
};
pub use package_guard::{PackageEcosystem, PackageGuard, PackageStatus, PackageVerificationReport};
pub use protocol::{DaemonStatus, DevServerEndpointInfo, DoctorPillar, HgbRequest, HgbResponse, SwarmPodResult};
pub use providers::{GeminiProvider, OllamaModelDetails, OllamaModelTag, OllamaProvider};
pub use repomap::{CodeSymbol, FileSymbols, RepoMap, RepoMapReport, SymbolKind};
pub use rescue::{CorrectiveAction, FailureCategory, RescueReport, TerminalRescue};
pub use rules::{RuleDocument, RuleSourceType, WorkspaceRulesManager};
pub use security::AgentShieldLight;
pub use seed_engine::{EntityKind, PersonaSeedEngine, SeedBatch, SeedRecord};
pub use style::{StyleFeedbackRecord, StyleMemoryVault};
pub use syntax_slicer::{CallGraphNode, SyntaxSliceResult, TargetLanguage, TokenReductionMetrics};
pub use timeline::{TimelineDiff, TimelineInfo, TimelineManager, TimelineMergeReport};
pub use trace::{TraceEvent, TraceEventKind, TraceRingBuffer};
pub use traits::{HgbProvider, HgbTool};
pub use variant_race::{DesignArchetype, VariantCandidate, VariantRaceManifest, VariantRaceStatus};
pub use web_browser::{WebBrowserEngine, WebPageContent, WebSearchResult};

/// Persist active model selection across CLI, REPL, and TUI sessions
pub fn persist_active_model(model: &str) -> std::io::Result<()> {
    let clean = model.trim();
    if clean.is_empty() {
        return Ok(());
    }
    // 0. Support explicit override for tests and sandbox environments
    if let Ok(override_path) = std::env::var("HGB_ACTIVE_MODEL_PATH") {
        let p = std::path::PathBuf::from(override_path);
        if let Some(parent) = p.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        return std::fs::write(p, clean);
    }
    // 1. Try home directory .tagisan and .hgb
    if let Ok(home) = std::env::var("HOME") {
        let tgs_dir = std::path::PathBuf::from(&home).join(".tagisan");
        if tgs_dir.exists() {
            let _ = std::fs::write(tgs_dir.join("active_model"), clean);
        }
        let hgb_dir = std::path::PathBuf::from(&home).join(".hgb");
        let _ = std::fs::create_dir_all(&hgb_dir);
        let _ = std::fs::write(hgb_dir.join("active_model"), clean);
    }
    // 2. Try current workspace .hgb
    let ws_hgb = std::path::PathBuf::from(".hgb");
    if ws_hgb.exists() {
        let _ = std::fs::write(ws_hgb.join("active_model"), clean);
    }
    Ok(())
}

/// Load previously persisted active model if present
pub fn load_active_model() -> Option<String> {
    // 0. Check explicit override for tests and sandbox environments
    if let Ok(override_path) = std::env::var("HGB_ACTIVE_MODEL_PATH") {
        if let Ok(m) = std::fs::read_to_string(&override_path) {
            let trimmed = m.trim().to_string();
            if !trimmed.is_empty() {
                return Some(trimmed);
            }
        }
        return None;
    }
    // 1. Check workspace .hgb/active_model
    let ws_file = std::path::PathBuf::from(".hgb").join("active_model");
    if let Ok(m) = std::fs::read_to_string(&ws_file) {
        let trimmed = m.trim().to_string();
        if !trimmed.is_empty() {
            return Some(trimmed);
        }
    }
    // 2. Check ~/.hgb/active_model or ~/.tagisan/active_model
    if let Ok(home) = std::env::var("HOME") {
        let hgb_file = std::path::PathBuf::from(&home).join(".hgb").join("active_model");
        if let Ok(m) = std::fs::read_to_string(&hgb_file) {
            let trimmed = m.trim().to_string();
            if !trimmed.is_empty() {
                return Some(trimmed);
            }
        }
        let tgs_file = std::path::PathBuf::from(&home).join(".tagisan").join("active_model");
        if let Ok(m) = std::fs::read_to_string(&tgs_file) {
            let trimmed = m.trim().to_string();
            if !trimmed.is_empty() {
                return Some(trimmed);
            }
        }
    }
    None
}

/// Clear persisted active model from workspace and home directories
pub fn clear_persisted_active_model() {
    if let Ok(override_path) = std::env::var("HGB_ACTIVE_MODEL_PATH") {
        let _ = std::fs::remove_file(override_path);
    }
    if let Ok(home) = std::env::var("HOME") {
        let _ = std::fs::remove_file(std::path::PathBuf::from(&home).join(".hgb").join("active_model"));
        let _ = std::fs::remove_file(std::path::PathBuf::from(&home).join(".tagisan").join("active_model"));
    }
    let _ = std::fs::remove_file(std::path::PathBuf::from(".hgb").join("active_model"));
}

/// Validate and dynamically resolve an active model preference against actual installed and available providers.
/// - If a local Ollama model is requested but not installed, auto-resolves to the best available installed Ollama model
///   (prioritizing coder models like `qwen2.5-coder:7b`).
/// - If Ollama has no models or is offline, seamlessly resolves to `gemini-2.5-flash` if Google credentials are ready.
/// - Automatically repairs `.hgb/active_model` to prevent subsequent failures.
pub fn validate_and_resolve_active_model(raw_model: Option<&str>) -> Option<String> {
    let raw = match raw_model {
        Some(m) if !m.trim().is_empty() => m.trim(),
        _ => return None,
    };

    // If it's a cloud model (Gemini), it's valid
    if raw.contains("gemini") || raw.contains("flash") || raw.contains("pro") {
        return Some(raw.to_string());
    }

    // If it's a local / Ollama model
    if crate::providers::OllamaProvider::is_ollama_model(raw) {
        if crate::providers::OllamaProvider::is_available() {
            let installed = crate::providers::OllamaProvider::installed_model_names();
            if !installed.is_empty() {
                // If the exact model is installed, use it
                if crate::providers::OllamaProvider::is_installed_model(raw) {
                    return Some(raw.to_string());
                }
                // If not installed, look for best installed alternative
                let clean = raw
                    .strip_prefix("ollama/")
                    .or_else(|| raw.strip_prefix("local/"))
                    .unwrap_or(raw);

                // Check for match in same family (e.g. qwen2.5-coder)
                let base_family = clean.split(':').next().unwrap_or(clean);
                let fallback = installed.iter()
                    .find(|m| m.contains(base_family))
                    .or_else(|| installed.iter().find(|m| m.contains("coder")))
                    .or_else(|| installed.first())
                    .cloned();

                if let Some(ref alt) = fallback {
                    let resolved = if raw.starts_with("ollama/") {
                        format!("ollama/{}", alt)
                    } else {
                        alt.clone()
                    };
                    let _ = persist_active_model(&resolved);
                    return Some(resolved);
                }
            }
        }
        // If Ollama is offline or has no models, check if Gemini is available
        if crate::providers::GeminiProvider::is_available() {
            let _ = persist_active_model("gemini-2.5-flash");
            return Some("gemini-2.5-flash".to_string());
        }
    }

    Some(raw.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validate_and_resolve_cloud_models() {
        assert_eq!(
            validate_and_resolve_active_model(Some("gemini-2.5-flash")),
            Some("gemini-2.5-flash".to_string())
        );
        assert_eq!(
            validate_and_resolve_active_model(Some("gemini-2.5-pro")),
            Some("gemini-2.5-pro".to_string())
        );
        assert_eq!(validate_and_resolve_active_model(None), None);
        assert_eq!(validate_and_resolve_active_model(Some("   ")), None);
    }

    #[test]
    fn test_validate_and_resolve_uninstalled_ollama_model() {
        if providers::OllamaProvider::is_available() {
            let installed = providers::OllamaProvider::installed_model_names();
            if !installed.is_empty() {
                // Request a model that definitely does NOT exist on the machine
                let resolved = validate_and_resolve_active_model(Some("qwen2.5-coder:1.5b"));
                assert!(resolved.is_some());
                let res_str = resolved.unwrap();
                // Must resolve to one of the actually installed models!
                assert!(
                    installed.contains(&res_str),
                    "Expected resolved model '{}' to be one of installed: {:?}",
                    res_str,
                    installed
                );
            }
        }
    }
}
