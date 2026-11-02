#![recursion_limit = "512"]
// [Self-Patch 20261020_141157] Verified autonomous stability anchor
//! Frontier Codex Architecture Solution
//! Task: add a docstring and comment
//! Context: <project_memory aggregate_hash="468bacb9" project="Hagibis Project">

pub mod colibri;
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
#[path = "providers/openai_compatible.rs"]
pub mod openai_compatible;
#[path = "providers/azure_openai.rs"]
pub mod azure_openai;
pub mod session;
pub mod doctor;
pub mod mcp_microsoft;
pub mod webhook_gateway;
pub mod onnx_runtime;
pub mod enterprise;
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
pub mod code_skeleton;
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
pub mod relational_webhook_replayer;
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
pub mod self_evolution_engine;
pub mod desktop_computer_use;
pub mod formal_verification_engine;
pub mod monorepo_hypergraph;
pub mod embedded_firmware_lab;
pub mod store_release_orchestrator;
pub mod speech_synthesis_engine;
pub mod visual_canvas_studio;

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
pub type ContinuousVoiceLoop = ContinuousVoiceDuplex;
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

pub use self_evolution_engine::{
    DpoPreferencePair, EvolutionGeneration, SelfEvolutionConfig, SelfEvolutionEngine,
    SelfEvolutionReport,
};
pub use desktop_computer_use::{
    DesktopAction, DesktopActionResult, DesktopComputerUseEngine, DesktopInspectionReport,
    DesktopTargetWindow,
};
pub use formal_verification_engine::{
    FormalProofStatus, FormalVerificationConfig, FormalVerificationEngine,
    FormalVerificationReport, SmtSolverKind, VerificationProperty, VerificationPropertyKind,
};
pub use monorepo_hypergraph::{
    BlastRadiusReport, MonorepoHypergraphEngine, MonorepoHypergraphReport, MonorepoPackageNode,
};
pub use embedded_firmware_lab::{
    EmbeddedCheckConfig, EmbeddedCheckReport, EmbeddedFirmwareEngine, HdlLanguage,
    TargetMcuArchitecture,
};
pub use store_release_orchestrator::{
    AppStorePlatform, ReleaseTrack, StoreReleaseConfig, StoreReleaseEngine, StoreReleaseReport,
};
pub use speech_synthesis_engine::{
    SpeechSynthesisEngine, SynthesisConfig, SynthesisReport, VoiceProfile,
};
pub use visual_canvas_studio::{
    VisualCanvasStudioEngine, VisualComponentNode, VisualStudioSessionReport,
    VisualStudioSyncPatch,
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
pub use relational_webhook_replayer::{ProxyReplayReport, ProxyServiceKind, RelationalWebhookReplayer};

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
pub use live_graph_watcher::{CsrSymbolGraph, IndexedSymbol, LiveGraphSummary, LiveGraphWatcher};
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
pub use audio::{play_vibe_chime, play_vibe_cue, VibeAudioCue};
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
pub use protocol::{DaemonStatus, DevServerEndpointInfo, DoctorPillar, HgbRequest, HgbResponse, SwarmPodResult, WorkspaceIdentity};
pub use providers::{GeminiProvider, OllamaModelDetails, OllamaModelTag, OllamaProvider, SharedProviderRouter};
pub use openai_compatible::{
    AnthropicMessage, AnthropicProvider, ChatMessage, OpenAiCompatibleFlavor,
    OpenAiCompatibleProvider, DEFAULT_ANTHROPIC_MODEL,
};
pub use session::{
    CockpitSessionData, SessionArtifactDiff, SessionChatMessage, SessionDagNode,
    SessionSummary, SessionToolCall,
};
pub use doctor::{
    DiagnosticCategory, DiagnosticCheck, DiagnosticStatus, DoctorReport,
    DoctorSummary, HgbDoctor,
};
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
pub mod wasm_fabric;
pub mod debt_shredder;
pub mod firecracker_shield;
pub mod npu_native;
pub mod vision_sync;
pub mod rag_stack;
pub mod cloud_swarm;
pub mod plugin_fabric;
pub mod stakeholder_portal;
pub mod enterprise_gateway;
pub use rag_stack::{RagStackEngine, RagVector, VectorDbConn};
pub use cloud_swarm::{CloudSwarm, CloudSwarmConfig, CloudSwarmStatus};
pub use plugin_fabric::{PluginFabric, PluginContext, PluginExecutionResult};
pub use stakeholder_portal::{StakeholderPortal, PortalConfig, PortalStatus};
pub use enterprise_gateway::{EnterpriseGateway, EnterpriseCompliance, GatewayAuthResult};
pub mod visionary;
pub mod zig_accelerate;

pub mod path_jail;
pub use path_jail::PathJail;

pub mod speculative_sandbox;
pub mod speculative_draft;
pub mod submarine;
pub mod blast_radius;
pub mod adversarial_tournament;
pub mod checkpoint_time_machine;
pub mod durable_checkpoint;
pub mod agentic_bisect;

pub use speculative_sandbox::{
    CompilationResult, HealingReport, HealingRound, SandboxPatch, SpeculativeSandbox,
};
pub use speculative_draft::{
    DraftProposal, SpeculativeDraftEngine, SpeculativeMetrics, VerificationResult,
};
pub use submarine::{
    AirgapStatus, NetworkAuditReport, SubmarineGuard,
};
pub use blast_radius::{
    AffectedSymbol, ArchitecturalBlastRadiusReport, BlastRadiusRadar, BlastSeverity, CallsiteReference,
};
pub use adversarial_tournament::{
    AdversarialTournament, RemediationAction, SecurityCertificate, TournamentConfig,
    TournamentOutcome, TournamentRound, VulnCategory,
    VulnSeverity as TournamentVulnSeverity,
    VulnerabilityFinding as TournamentVulnerabilityFinding,
};
pub use checkpoint_time_machine::{
    CheckpointMetadata, CheckpointTimeMachine, RollbackReport,
};
pub use durable_checkpoint::{
    DurableCheckpointManager, DurableCheckpointMeta, DurableFileEntry, DurableRollbackReport,
};
pub use agentic_bisect::{
    AgenticBisectEngine, BisectConfig, BisectOutcome, BisectStep, CulpritAnalysis,
};

// ---------------------------------------------------------------------------
// 4-PHASE RE-ENGINEERING CORE MODULES
// ---------------------------------------------------------------------------
pub mod embedded_infer;
pub mod overlayfs_sandbox;
pub mod graph_of_thought;
pub mod ambient_dream;

pub use embedded_infer::{
    EmbeddedInferenceEngine, EmbeddedTokenizer, GgufModel, GgufTensorInfo, GgufValue,
    InferenceConfig, InferenceResult, InferenceTelemetry, LogitsProcessor,
};
pub use overlayfs_sandbox::{
    CommitReport as SandboxCommitReport, OverlaySandbox, RollbackReport as SandboxRollbackReport,
    SandboxBackend, SandboxDiff, SandboxExecutionReport,
};
pub use graph_of_thought::{
    BranchExecutionResult, EdgeType, FitnessScore, GoTArbiter, ThoughtCertificate,
    ThoughtEdge, ThoughtGraph, ThoughtNode, ThoughtNodeId, ThoughtState, ThoughtStrategy,
};
pub use ambient_dream::{
    AmbientDreamEngine, DpoMetadata, DpoPreferencePair as DistillDpoPair, DreamCycleReport, GitCommitRecord,
    LoRAConfig, PowerState, SystemHeadroom,
};

// ---------------------------------------------------------------------------
// ROADMAP PRIORITIES P1 - P6 MODULES
// ---------------------------------------------------------------------------
pub mod unified_driver;
pub mod github_action_runner;
pub mod gpu_accelerate;

pub use unified_driver::{
    UnifiedAutonomousDriver, UnifiedExecutionReport, VerificationStages,
};
pub use github_action_runner::{
    ActionConfig as GitHubActionConfig, ActionReport as GitHubActionReport, GitHubActionRunner,
};
pub use gpu_accelerate::{
    ComputeBackend, GgufInspectionReport, GgufModelEntry, GgufModelManager, GpuBenchmarkReport,
    GpuComputeEngine,
};

// ---------------------------------------------------------------------------
// FREE CLAUDE CODE (FCC) TRANSCENDENCE MODULES
// ---------------------------------------------------------------------------
pub mod token_shredder;
pub mod copilot_oauth;
pub mod codex_oauth;
pub mod cloud_fabric;

pub use token_shredder::{
    PolicyExecutionMetrics, ShredResult, TokenPolicyConfig, TokenPolicyEngine, TokenShredder,
};
pub use copilot_oauth::{
    CopilotAuthEngine, CopilotChatMessage, CopilotChatRequest, CopilotChatResponse,
    CopilotCredentials, DeviceCodeResponse, GitHubTokenResponse,
};
pub use codex_oauth::{
    CodexAuthEngine, CodexCredentials, CodexModel,
};
pub use cloud_fabric::{
    FabricChatRequest, FabricChatResponse, FabricMessage, FailoverEvent,
    ProviderDescriptor, ProviderHealthReport, ProviderId, UniversalCloudFabric,
};




// ===========================================================================
// ADDITIONS FOR crates/hgb-core/src/lib.rs
// ===========================================================================

pub mod voice_loop;
pub mod visual_loop;
pub mod time_machine;
pub mod ast_query;
pub mod finops_budget;

// Re-export core dependencies for daemon and CLI
pub use blake3;
pub use qrcode;

// Voice-to-Intent Engine exports
pub use voice_loop::{
    compute_frame_rms, encode_wav_pcm16, parse_wav, ParsedAudio, SpeechSegment,
    VadConfig, VoiceActivityDetector, VoiceIntent, VoiceIntentClassifier,
    VoiceTranscriptionClient, WavHeader,
};

// Visual Multimodal Debugger via CDP exports
pub use visual_loop::{
    CdpClient, CdpConsoleMessage, CdpRequest, CdpResponse, ConsoleLevel,
    VisualDebuggerReport,
};

// ChronoWarp Time-Machine Ledger exports
pub use time_machine::{
    SnapshotOutcome, TimeMachineEngine, TimelineSnapshot,
};

// Deterministic AST Query & Refactor Engine exports
pub use ast_query::{
    AstQueryEngine, AstQueryFilter, AstSymbol, SyntaxInvariantValidator,
};

// Smart FinOps & Token Budget Envelope exports
pub use finops_budget::{
    BudgetEnvelope, FinOpsEngine, ModelPricing, ModelPricingCatalog, RoutingDecision,
    UsageRecord,
};

// Folder & Network Share Inspection and Auto-Grounding exports
pub mod folder_inspector;
pub use folder_inspector::{
    BuildBloatItem, ComprehensiveSystemAnalysisReport, DirectoryTreeSummary, FileEntryMetric,
    FileNodeInfo, FileTypeKind, FolderAnalysisReport, FolderInspectionConfig,
    FolderInspectorEngine, InspectedTargetReport, NetworkAnalysisReport, NetworkComputerInfo,
    NetworkHostAnalysis, NetworkShareInfo, SecurityWarning, StorageAnalysisReport,
    StorageDriveAnalysis, StorageDriveInfo, TargetKind,
};

// Universal Prompt Auto-Grounding & Analytical Context Engine exports
pub mod prompt_grounding;
pub use prompt_grounding::{
    GroundingContextReport, InspectedFileTarget, SystemSpecsSummary, UniversalPromptGrounder,
};

// Hybrid Dual-Brain Orchestration Engine exports
pub mod hybrid_orchestrator;
pub use hybrid_orchestrator::{
    BenchmarkRecord, EscalationHandoffPackage, HybridExecutionReport, HybridOrchestrationEngine,
    HybridVerificationResult, PerformanceBenchmarkLedger, PerformanceScorecard, PrivacyAuditOutcome,
    PrivacyFirewallEngine, SharedProjectContext, TaskRoutingDecision, TaskRoutingTier,
};

// Native Model Context Protocol (MCP) Server exports
pub mod mcp_server;
pub use mcp_server::{HagibisMcpServer, HgbMcpProfile, McpStdioServer, CORE_VIBE_TOOLS};

// Lightweight Local Ollama Inference Routing Pipeline exports
pub mod ollama_inference_router;
pub use ollama_inference_router::{
    InferenceTaskKind, OllamaInferenceRouter, OllamaRouteExecutionReport, RoutingDestination,
};

// Dynatune & Linux Kernel Synergy exports
pub mod dynatune_synergy;
pub use dynatune_synergy::{
    DynatuneSynergyEngine, HardwareInferencePolicy, HardwareSafeguardReport, ThermalSeverity,
    ThermalZoneMetric,
};

// Dual-Layer Secure Execution Engine (Wasm + Firecracker MicroVM)
pub mod dual_layer_sandbox;
pub use dual_layer_sandbox::{
    DualLayerExecutionReport, DualLayerSandboxEngine, ExecutionTier,
};

// Masterclass & Documentation Pipeline exports
pub mod masterclass_pipeline;
pub use masterclass_pipeline::{
    MasterclassGenerationReport, MasterclassPipeline,
};

// WebAssembly (`hgb-wasm-kernel`) Plugin System exports
pub mod wasm_plugin_kernel;
pub use wasm_plugin_kernel::{
    WasmPluginHostState, WasmPluginInstance, WasmPluginManager, WasmPluginManifest, WasmToolAdapter,
};

// WebAssembly Component Model & Capability Security Sandbox (Pillar 4)
pub mod wasm_plugin_sandbox;
pub use wasm_plugin_sandbox::{
    PluginCapabilities, ReloadReport, SandboxedExecutionResult, SandboxedWasmHostState,
    SandboxedWasmPlugin, SandboxedWasmToolAdapter, WasmCapabilityViolation,
    WasmPluginCatalog, WasmPluginRegistry, WasmPluginSandboxEngine,
};

// Sibling Ollama Pipeline Accelerator & Mutation Vector Mesh exports
pub mod ollama_sibling_accelerator;
pub use ollama_sibling_accelerator::{
    MutationVectorChunk, MutationVectorMatch, SiblingOllamaAccelerator, SiblingOllamaMetadata,
    VectorSyncReport,
};

// Stateful WebAssembly Distributed Sync & Edge Migration exports
pub mod wasm_distributed_sync;
pub use wasm_distributed_sync::{
    CloudflareEdgeMigrator, StatefulWasmComponent, WasmExecutionState, WasmMemoryPage,
    WasmMigrationEnvelope, WasmMigrationReceipt, WasmNodeTier, WasmSyncStatus, WranglerConfig,
    WASM_PAGE_SIZE,
};

// Fully Autonomous Vibe Coding Agent Loop Engine exports
pub mod vibe_autonomous_loop;
pub use vibe_autonomous_loop::{
    VibeAutonomousLoopEngine, VibeCycleReport, VibeLoopConfig, VibePatchCandidate,
};

// Zero-Copy WASM Execution Hot-Pool exports
pub mod wasm_hot_pool;
pub use wasm_hot_pool::{
    CachedModuleInfo, ModulePrewarmReport, WasmHotExecReport, WasmHotPool, WasmHotPoolStatus,
    WasmShmTensorReport, WasmZeroCopyContextReport, DEFAULT_WASM_FUEL,
};

// Adaptive Power-Aware Scheduler (Ollama + Dynatune) exports
pub mod power_scheduler;
pub use power_scheduler::{
    AdaptivePowerScheduler, OllamaPowerProfile, PowerSchedulerStatus, ScheduledInferenceConfig,
};

// High-Performance Unified Distributed Event Bus exports
pub mod event_bus;
pub use event_bus::{
    DaemonEvent, DistributedEventBus, EventBusStatus, DEFAULT_EVENT_BUS_CAPACITY,
};

// Superpower 1: Native Ollama Integration & Hardware-Aware Routing
pub mod hardware_aware_router;
pub use hardware_aware_router::{
    ComplexityTier, HardwareAwareRouter, HardwareRouterStatus, HardwareRoutingDecision,
};

// Superpower 2: Distributed MicroVM Swarms via Firecracker & WASM
pub mod microvm_swarm;
pub use microvm_swarm::{
    EphemeralSandboxSwarm, IsolatedExecutionReport, IsolatedExecutionSpec, MicroVmSwarmStatus,
};

// Superpower 4: Autonomous Self-Patching & Continuous Learning Loop
pub mod autonomous_self_patcher;
pub use autonomous_self_patcher::{
    AutonomousSelfPatcher, DistillationPair, SelfPatchRecord, SelfPatchStatusReport,
};

// Autonomous Self-Healing Compilation Loops
pub mod compiler_interceptor;
pub use compiler_interceptor::{
    CompilerDiagnostic, CompilerInterceptor, CompilerTool, DiagnosticSeverity, SelfHealingReport,
};

// JIT GPU/NPU Offloading via Dynatune Integration
pub mod jit_resource_allocator;
pub use jit_resource_allocator::{
    GpuTelemetry, HeavyProcessInfo, JitAllocationGuard, JitAllocationStatus, JitResourceAllocator,
};

// Zero-Copy WASM Shared Memory Pipelines
pub mod wasm_shared_memory;
pub use wasm_shared_memory::{
    WasmSharedMemoryPipeline, WasmShmBenchReport, WasmShmStatus, DEFAULT_SHM_PAGES,
};

// Pillar 1: Neural Coprocessor & Local Model Hot-Swapping
pub mod neural_coprocessor;
pub use neural_coprocessor::{
    CoprocessorRouter, CoprocessorStatus, CoprocessorTelemetry, ExecutionTarget,
    NeuralCoprocessor, PromptComplexityTier, RoutingDecision as CoprocessorRoutingDecision,
};

// Superpower 1: Zero-Overhead IPC via Shared Memory & Unix Domain Sockets
pub mod shm_ipc_channel;
pub use shm_ipc_channel::{
    run_shm_ipc_benchmark, ShmIpcBenchReport, ShmIpcChannel, ShmIpcHeader, ShmIpcMessage,
    ShmMessageType, ShmRingPool, DEFAULT_SHM_FILE_NAME, DEFAULT_SHM_POOL_CAPACITY,
    HGB_IPC_MAGIC, SHM_THRESHOLD_BYTES,
};

// Superpower 2: Autonomous Multi-Agent Swarm Orchestration
pub mod actor_swarm;
pub use actor_swarm::{
    ActorVote, CandidatePatch, CoderActor, DebateTurn, DpoTrainingPair, PlannerActor,
    ReviewVerdict, ReviewerActor, SandboxExecutionVerdict, SandboxExecutorActor, SwarmActor,
    SwarmConsensusCertificate, SwarmCoordinator, SwarmRole, WorkPlan, WorkUnit,
};

// Superpower 3: Distributed Edge-to-Cloud State Synchronization (CRDT Engine)
pub mod crdt_edge_sync;
pub use crdt_edge_sync::{
    AdrRecord as CrdtAdrRecord, ClockRelation, CrdtSyncDocument, EdgeSyncPayload, ElementTag, LwwRegister,
    MergeReport, OrSet, SkillRecord, StylePreferences, VectorClock, VectorEmbeddingRef,
};

// Superpower 4: Hardware-Aware Dynamic CPU Governor Integration
pub mod dynamic_governor;
pub use dynamic_governor::{
    CpuBaselineState, DynamicGovernor, GovernorGuard, GovernorTelemetry,
    WorkloadKind as DynamicGovernorWorkloadKind,
    THERMAL_RELAX_THRESHOLD_C, THERMAL_THROTTLE_THRESHOLD_C,
};

// Powerhouse Pillar 1: Hardware Telemetry Optimizer
pub mod hardware_telemetry_optimizer;
pub use hardware_telemetry_optimizer::{
    BaselineHardwareProfile, HardwareOptimizationGuard, HardwareOptimizationTelemetry,
    HardwareTelemetryOptimizer, PSI_SOME_WARN_THRESHOLD, THERMAL_CRITICAL_THRESHOLD_C,
    THERMAL_SAFE_MARGIN_C,
};

// Powerhouse Pillar 2: Ollama Speculative Orchestrator
pub mod ollama_speculative_orchestrator;
pub use ollama_speculative_orchestrator::{
    OllamaRoutingDecision, OllamaSpeculativeOrchestrator, OllamaSpeculativeTelemetry,
    DEFAULT_DRAFT_MODEL, DEFAULT_OLLAMA_ENDPOINT, DEFAULT_TARGET_MODEL,
};

// Powerhouse Pillar 3: WebAssembly Plugin Marketplace
pub mod wasm_skill_marketplace;
pub use wasm_skill_marketplace::{
    WasmSkillExecutionResult, WasmSkillInfo, WasmSkillMarketplace,
};

// Powerhouse Pillar 4: Ephemeral Code Execution Sandboxes
pub mod microvm_sandbox_executor;
pub use microvm_sandbox_executor::{
    EphemeralExecResult, EphemeralExecSpec, FirecrackerEphemeralSandbox,
    FirecrackerJailDestroyReport, FirecrackerJailExecReport, FirecrackerJailExecSpec,
    FirecrackerJailInfo, FirecrackerSpawnConfig,
};

// Autonomous Expansion Pillar 1: Hybrid Kernel-Level Acceleration (dynatune & kernel-optimizer)
pub mod hybrid_kernel_accelerator;
pub use hybrid_kernel_accelerator::{
    BaselineKernelProfile, HybridKernelAccelerator, HybridKernelTelemetry, KernelAccelerationGuard,
    BATTERY_LOW_THRESHOLD_PCT, THERMAL_CRITICAL_HEADROOM_C,
};

// Autonomous Expansion Pillar 2: Distributed P2P WASM Mesh Networking
pub mod wasm_mesh_network;
pub use wasm_mesh_network::{
    WasmComputeMesh, WasmMeshExecutionResult, WasmMeshNode, WasmMeshPackage, WasmMeshStatusReport,
};

// Strategic Upgrade 1: Native eBPF & Kernel-Level Telemetry
pub mod ebpf_telemetry_probe;
pub use ebpf_telemetry_probe::{
    CpuThermalMetrics, EbpfGovernorGuard, EbpfProbeEngine, EbpfTelemetryReport, GovernorState,
    MemorySpikeMetrics, SyscallLatencyMetrics, ThermalZoneReading, DYNATUNE_PROJECT_DIR,
    KERNEL_OPTIMIZER_CTL,
};

// Strategic Upgrade 3: Ephemeral GPU-Accelerated WASM Sandboxing & WASI-NN
pub mod wasm_gpu_wasi_nn;
pub use wasm_gpu_wasi_nn::{
    ExecutionContext, ExecutionTarget as WasiExecutionTarget, GraphEncoding, ModelGraph, Tensor, TensorType,
    WasiNnBenchmarkReport, WasiNnError, WasiNnExecutionResult, WasiNnHostState,
    WasiNnStatusReport, WasmGpuWasiNnEngine, INTEL_GPU_RENDER_NODE,
};

// Strategic Upgrade 3: Hardware-Aware Autonomous Throttling (dynatune FFI throttler)
pub mod dynatune_ffi_throttler;
pub use dynatune_ffi_throttler::{
    CDynatuneHardwareReport, DynatuneSysfsReader, HardwareSensorSnapshot, HardwareThrottleArbiter,
    HardwareThrottleReport, DEFAULT_CRITICAL_WORKERS, DEFAULT_MAX_WORKERS,
    DEFAULT_MODERATE_WORKERS, MAX_COOLDOWN_MS, MIN_COOLDOWN_MS, THERMAL_COOL_THRESHOLD_C,
    THERMAL_HOT_THRESHOLD_C,
};

// Strategic Upgrade 1: Unified Shared-Memory IPC & WASM Linear Memory Bridge
pub mod unified_shm_wasm_bridge;
pub use unified_shm_wasm_bridge::{
    UnifiedRingControl, UnifiedShmBenchReport, UnifiedShmStatus, UnifiedShmWasmBridge,
    UnifiedShmWasmHeader, CONTROL_HEADER_SIZE, DEFAULT_UNIFIED_RING_CAPACITY,
    DEFAULT_UNIFIED_SHM_PATH, FLAG_AST_REPRESENTATION, FLAG_CONTEXT_BUFFER,
    FLAG_DIRECT_POINTER_MAP, FLAG_MODEL_TENSOR_WEIGHTS, FLAG_STREAM_CHUNK, FLAG_STREAM_FINAL,
    FLAG_URGENT, FLAG_ZERO_COPY_VERIFIED, UNIFIED_HEADER_SIZE, UNIFIED_RING_CONTROL_MAGIC,
    UNIFIED_SHM_MAGIC,
};

// Strategic Enhancement 1: Zero-Copy Shared-Memory & WIT WebAssembly Bridge
pub mod wasm_shm_wit_bridge;
pub use wasm_shm_wit_bridge::{
    RawTensorHeader, ShmBenchLatencyReport, ShmBridgeError, ShmTensorDType, ShmTensorMeta,
    WasmShmFrameHeader, WasmShmWitBridge, FRAME_TYPE_CONTEXT, FRAME_TYPE_RAW_BYTES,
    FRAME_TYPE_TENSOR, FRAME_TYPE_TOKENS, HGB_WSHM_MAGIC, MAX_TENSOR_DIMS, MAX_TENSOR_NAME_LEN,
    RAW_TENSOR_HEADER_SIZE, SHM_FRAME_HEADER_SIZE, TENSOR_METADATA_MAGIC,
};

pub use wasmtime;

// Frictionless Vibe Prompting & Ambient Hardware Synergy
pub mod vibe_hardware_governor;
pub use vibe_hardware_governor::{
    VibeHardwareGovernor, VibeHardwareGuard, VibeThermalState,
    VIBE_THERMAL_HEADROOM_THRESHOLD_C,
};

pub mod vibe_prompt_runner;
pub use vibe_prompt_runner::{
    format_ambient_status_card, VibePromptResult, VibePromptRunner, VibePromptSpec,
};
pub use code_skeleton::{CodeSkeletonizer, SkeletonReport};

// Pillar 4: Unified Autonomous Loop Mastery (Masterclass Ch8 Native Rust)
pub mod autonomous_state_machine;
pub use autonomous_state_machine::{
    AutonomousCycleInfo, AutonomousState, AutonomousStateMachine, AutonomousStateStatusSummary,
    CrashRecoveryReport, CycleStatus, DpoDistillationRecord, StateTransitionRecord,
};

// Pillar 3: Host-Aware Dynamic Scaling (Dynatune & Kernel Optimizer Bridge)
pub mod dynatune_bridge;
pub use dynatune_bridge::{
    BatteryTelemetry, CompleteHardwareTelemetry, DynatuneBridge, HostPowerState,
    PressureRecordTelemetry, PsiCpuTelemetry, PsiMemoryTelemetry, ScalingGuard, ScalingMode,
    ScalingShiftReport, ThermalZoneTelemetry, WorkloadCategory,
    BATTERY_CRITICAL_THRESHOLD_PCT, BURST_MGLRU_MIN_TTL_MS,
    THERMAL_CRITICAL_TEMP_C, THERMAL_SAFE_HEADROOM_C,
    compute_concurrency_limit,
};

// Strategic Upgrade 1: Unified Shared-Memory IPC (Zero-Copy Ring Buffers)
pub mod shm_ring_buffer;
pub use shm_ring_buffer::{
    resolve_shm_path, resolve_ring_path_for_pid, resolve_ring_path,
    ShmHeader, ShmRingBenchmarkReport, ShmRingBuffer,
    ShmRingConsumer, ShmRingProducer, DEFAULT_SHM_CAPACITY, HEADER_SIZE,
    SHM_MAGIC, SHM_VERSION,
};

pub mod shm_ring_channel;
pub use shm_ring_channel::{
    align_up_64, ShmFrameHeader, ShmRingChannel, ShmRingChannelConsumer, ShmRingChannelProducer,
    ShmRingFrame, ShmTaskDispatch, ShmTaskExecutionReport, ShmTaskType, ShmTelemetryFrame,
    FRAME_HEADER_SIZE, FRAME_MAGIC, FRAME_TAG_RAW, FRAME_TAG_REQUEST, FRAME_TAG_RESPONSE,
    FRAME_TAG_TASK_DISPATCH, FRAME_TAG_TASK_REPORT, FRAME_TAG_TELEMETRY, FRAME_TAG_TELEMETRY_FRAME,
    FRAME_TAG_TOKEN_CHUNK,
};

// Strategic Upgrade 2: Distributed MicroVM Swarm Intelligence (Firecracker + WASM Hybrid)
pub mod microvm_swarm_orchestrator;
pub use microvm_swarm_orchestrator::{
    IsolationTier, MicroVmSwarmOrchestrator, SwarmExecutionOutcome, SwarmOrchestratorStatus,
    SwarmTask, SwarmWorkerType, TaskComplexity,
};

// Strategic Upgrade 3: Integrated Native GPU / NPU Offloading (dynatune synergy)
pub mod gpu_npu_offloader;
pub use gpu_npu_offloader::{
    DriRenderNodeInfo, HardwareProbeReport, HardwareTarget, HardwareTelemetrySnapshot,
    NpuDeviceInfo, NvidiaDriverInfo, OffloadEmbeddingResult, OffloadSynthesisResult,
    OllamaModelTelemetry, RocmKfdInfo, SchedulingDecision, SmartHardwareScheduler,
};

/// Enterprise high-capacity implementation
pub fn enterprise_architecture_dispatch() -> bool {
    true
}

// Upgrade 1: Linux io_uring & Zero-Copy SHM IPC Engine
pub mod io_uring_ipc;
pub use io_uring_ipc::{
    probe_kernel_io_uring, ContextChunk, IpcBackendKind, IpcEngineStats, IpcMessageFrame,
    IpcMessageKind, IpcRpcCommand, IpcRpcResponse, IoUringIpcConsumer, IoUringIpcEngine,
    IoUringIpcProducer, NotificationSignaler, TokenStreamChunk, IPC_FRAME_MAGIC,
};

// Memory-Mapped Crash Dump Recorder
pub mod shm_panic_dumper;
pub use shm_panic_dumper::{
    clear_panic_dump, install_shm_panic_hook, read_last_panic_dump, record_panic_hook_info,
    resolve_panic_dump_path, write_panic_dump, ShmPanicDump, ShmPanicHeader,
    SHM_PANIC_FILENAME, SHM_PANIC_HEADER_SIZE, SHM_PANIC_MAGIC, SHM_PANIC_VERSION,
};

// Skill 4: Speculative Dry-Run Diff Viewer
pub mod speculative_diff_viewer;
pub use speculative_diff_viewer::{
    CherryPickCommitReport, DiffHunk, DiffLine, DiffLineKind, FileDiffStatus,
    HunkStatus, SpeculativeDiffSession, SpeculativeDiffSummary, SpeculativeFileDiff,
};

// Pillar 2: Distributed MicroVM Mesh & WASM Swarm Federation
pub mod p2p_swarm_federation;
pub use p2p_swarm_federation::{
    OptimalNodeDecision, SwarmFederationEngine, SwarmFederationResult, SwarmFederationStatusReport,
    SwarmFederationTask, SwarmPacket, SwarmTaskKind, WorkerNodeAdvertisement, SWARM_FRAME_MAGIC,
};

// Pillar 4: Autonomous Self-Healing Agent Loops (Autonomous Watchdog, Snapshotting, and Self-Correction)
pub mod autonomous_watchdog;
pub use autonomous_watchdog::{
    AgentContextSnapshot, AgentGenerationParams, FailurePattern, RecoveryStory, WatchdogStatusReport,
};

// Superpower 1: Autonomous Self-Healing Loop via the WASM Kernel
pub mod wasm_autonomous_self_healer;
pub use wasm_autonomous_self_healer::{
    ToolInvocationResponse, WasmAutonomousSelfHealer, WasmFailureDiagnostic, WasmHealerConfig,
    WasmRepairIteration, WasmSelfHealingReport, WasmTrapKind,
};

// Superpower 2: Dynamic Hardware-Aware Routing with Dynatune Integration
pub mod hardware_aware_workload_router;
pub use hardware_aware_workload_router::{
    decide_profile, ExecutionProfile, HardwareAwareWorkloadRouter, WorkloadAdaptiveGuard,
    WorkloadExecutionDecision, WorkloadExecutionProfile, WorkloadKind,
};

// Superpower 3: Distributed P2P Agent Swarms via Model Context Protocol (MCP)
pub mod p2p_mcp_swarm;
pub use p2p_mcp_swarm::{
    P2pContentCache, P2pMcpNodeAdvertisement, P2pMcpPacket,
    P2pMcpSwarmEngine, P2pMcpSwarmStatusReport, P2pMcpToolAdvertisement, P2pSubtaskResult,
    P2pSubtaskSpec, P2pWorkflowResult, P2pWorkflowSpec, SubtaskDelegationTarget,
    SubtaskNegotiationPlan, DEFAULT_P2P_MCP_PORT,
};

// Superpower 4: Profile-Guided WebAssembly JIT Caching
pub mod wasm_pgo_jit;
pub use wasm_pgo_jit::{
    WasmPgoJitEngine, WasmPgoProfile, DEFAULT_PGO_CALL_THRESHOLD, DEFAULT_PGO_DURATION_THRESHOLD_US,
};

// Superpower 1: Direct Local Ollama Pipeline Acceleration
pub mod ollama_pipeline_accelerator;
pub use ollama_pipeline_accelerator::{
    blob_to_embedding, chunk_source_code, deterministic_semantic_embedding,
    embedding_to_blob, simd_cosine_similarity, CodeChunk, FileMutationVectorPipeline,
    MutationSearchResult, MutationSyncReport, OllamaHostConfig,
    OllamaPipelineAccelerator, OllamaRunnerStatus, SpeculativeDraftResult, VectorDbStats,
    DEFAULT_FAST_DRAFT_MODEL, DEFAULT_HEAVY_TARGET_MODEL, DEFAULT_MUTATION_VECTORS_DB,
    DEFAULT_OLLAMA_SOCKET, SIBLING_OLLAMA_DIR,
};

// Pillar 3: Edge-Native Synchronization via Cloudflare Workers & Wrangler
pub mod cloudflare_edge_mesh;
pub use cloudflare_edge_mesh::{
    glob_match, BufferFlushReport, BufferedItem, CloudflareEdgeMeshConfig,
    CloudflareEdgeMeshEngine, CloudflareEdgeMeshStatus, CloudflareRoutesConfig,
    EdgePublishReceipt, EdgeStorageBackend, EdgeTelemetryPayload, EdgeVectorChunk,
    EdgeVectorSyncBatch, EdgeVectorSyncReceipt, FullMeshSyncResult, HardwareTelemetry,
    RemoteTrigger, RemoteTriggerAck, RemoteTriggerKind, SwarmPeerTelemetry,
    DEFAULT_COMPATIBILITY_DATE, DEFAULT_EDGE_BUFFER_DB, DEFAULT_EDGE_TIMEOUT_MS,
};

// Pillar 1: WASM-Zig Sandbox with Autonomous Agent Loops
pub mod zig_wasm_agent_compiler;
pub use zig_wasm_agent_compiler::{
    AgentInvocationResult, AutonomousLoopReport, AutonomousZigAgentLoop, CompilerStats,
    HealingAttempt, SandboxedWasmAgentRunner, ZigCompilerConfig, ZigCompilationResult,
    ZigOptimization, ZigWasmAgentCompiler, ZigWasmTarget,
};

// Vector 3: Hardware-Aware Dynamic Profiling (Synergy with dynatune & kernel-optimizer)
pub mod dynamic_hardware_profiler;
pub use dynamic_hardware_profiler::{
    compute_ollama_batch_size, compute_task_concurrency, compute_workload_affinity,
    discover_topology_from_sysfs, evaluate_mglru_pressure,
    get_current_thread_affinity, set_current_thread_affinity_raw,
    CoreAffinityGuard, CoreClusterKind, CpuCoreInfo, CpuTopology,
    DynamicHardwareProfiler, HardwareGovernorProfile, HardwareProfileTelemetry,
    MglruPressureLevel, ProfileSwitchReport, ProfilerGovernorGuard, TaskIntensity,
    ThermalZoneInfo, BALANCED_MGLRU_MIN_TTL_MS, POWERSAVE_MGLRU_MIN_TTL_MS,
    THERMAL_HEADROOM_SAFE_C, THERMAL_THROTTLE_LIMIT_C,
};

// Vector 2: Universal WASM Plugin Hot-Reloading
pub mod wasm_plugin_hot_reloader;
pub use wasm_plugin_hot_reloader::{
    HotReloadEvent, HotReloadReport, HotReloadedPluginInfo, WasmHotReloadConfig,
    WasmPluginHotReloader, DEFAULT_HOT_RELOAD_POLL_MS, DEFAULT_WASM_PLUGIN_DIR,
    WASM_MAGIC_HEADER, WASM_VERSION_1,
};

// Pillar 3: Deep Ollama Ecosystem Bridge
pub mod ollama_ecosystem_bridge;
pub use ollama_ecosystem_bridge::{
    BridgeModelTier, BridgeRoutingDecision, CachedTensorView, DynabookOptimizationProfile,
    OllamaBridgeConfig, OllamaEcosystemBridge, OllamaEndpointConnector, OllamaModelInfo,
    OllamaMultiModelRouter, OllamaSpeculativeDecoder, OllamaStatusReport, OllamaTransport,
    PromptComplexityAnalysis, ShmTensorCache, ShmTensorFileHeader, ShmTensorMetadata,
    SpeculativeConfig, SpeculativeDecodingResult, SpeculativeVerificationStats, StreamToken,
    StreamTokenStats, TaskCategory, TensorDtype, TensorKind, TokenStreamDispatcher,
    TokenStreamReceiver, DEFAULT_OLLAMA_HTTP, DEFAULT_SHM_CACHE_DIR,
    DEFAULT_SOVEREIGN_TARGET_MODEL,
};

// Pillar 3: True Zero-Copy Shared Memory for AI Inference Pipelines
pub mod shm_inference_pipeline;
pub use shm_inference_pipeline::{
    resolve_infer_stream_path, InferControlHeader, InferDataType, TensorFrameHeader,
    TensorStreamBenchReport, TensorStreamStatus, WasmDirectMemoryBridge,
    ZeroCopyTensorConsumer, ZeroCopyTensorProducer, ZeroCopyTensorStream,
    DEFAULT_INFER_STREAM_CAPACITY, FLAG_CHECKSUMMED, FLAG_INTERMEDIATE_ACTIVATION,
    FLAG_IS_FINAL, FLAG_KV_CACHE, INFER_CONTROL_HEADER_SIZE, INFER_FRAME_HEADER_SIZE,
    INFER_FRAME_MAGIC, INFER_STREAM_VERSION,
};


// ==============================================================================
// Pillar 2: Distributed Multi-Tier Sandboxing via Firecracker & WASM
// ==============================================================================
pub mod multi_tier_sandbox {
    //! # Pillar 2: Distributed Multi-Tier Sandboxing via Firecracker & WASM
    //!
    //! Provides a unified, production-grade Multi-Tier Execution Sandbox:
    //! - **Tier 1 (Fast / Light)**: Sub-millisecond startup (<2ms), capability-restricted,
    //!   instruction-fuel-metered execution inside Wasmtime / WASM-Zig microkernel runtime.
    //!   Ideal for fast untrusted calculations, text/AST processing, and autonomous agent tools.
    //! - **Tier 2 (Heavy / Secure)**: Hardware-isolated KVM virtualization inside lightweight
    //!   Firecracker microVMs with seamless zero-failure fallback to Linux Landlock LSM + volatile
    //!   RAM CoW overlay sandboxes (`/dev/shm`).
    //!   Ideal for heavy autonomous agent code loops, full compiler toolchains, package installations,
    //!   and shell script executions.
    //! - **Automated Complexity Routing**: `MultiTierSandboxRouter` inspects syntax, system call
    //!   indicators, network access, languages, and binary requirements to dynamically direct workloads
    //!   to the optimal execution tier if no explicit tier preference is requested.

    use std::path::{Path, PathBuf};
    use std::sync::Arc;
    use std::time::Instant;
    use serde::{Deserialize, Serialize};
    use wasmtime::*;

    use crate::dynatune_bridge::DynatuneBridge;
    use crate::error::{HgbError, Result};
    use crate::microvm_sandbox_executor::{EphemeralExecSpec, FirecrackerEphemeralSandbox};
    use crate::zig_wasm_agent_compiler::{ZigCompilerConfig, ZigWasmAgentCompiler};

    /// Sandboxing execution tiers supported by Hagibis
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
    pub enum SandboxExecutionTier {
        /// Tier 1: Sub-millisecond Wasmtime / WASM-Zig runtime with deterministic fuel metering
        Tier1WasmZig,
        /// Tier 2: Heavyweight hardware virtualization via Firecracker microVM with Landlock CoW fallback
        Tier2FirecrackerMicroVm,
    }

    impl std::fmt::Display for SandboxExecutionTier {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            match self {
                Self::Tier1WasmZig => write!(f, "Tier1WasmZig"),
                Self::Tier2FirecrackerMicroVm => write!(f, "Tier2FirecrackerMicroVm"),
            }
        }
    }

    /// Unified request for sandboxed execution
    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct SandboxExecutionRequest {
        /// Source code, script text, or WebAssembly payload (WAT text or encoded WASM)
        pub code: String,
        /// Programming language identifier (e.g. "wasm", "wat", "zig", "bash", "python", "javascript")
        pub language: String,
        /// Optional explicit tier preference overriding automatic complexity routing
        pub tier_preference: Option<SandboxExecutionTier>,
        /// CPU instruction fuel budget for Tier 1 WASM sandboxing (default 10,000,000 instructions)
        pub fuel_limit: Option<u64>,
        /// Linear / virtual memory ceiling in megabytes
        pub memory_limit_mb: Option<usize>,
        /// Execution timeout in milliseconds (default 10,000 ms)
        pub timeout_ms: u64,
    }

    impl SandboxExecutionRequest {
        pub fn new(code: impl Into<String>, language: impl Into<String>) -> Self {
            Self {
                code: code.into(),
                language: language.into(),
                tier_preference: None,
                fuel_limit: None,
                memory_limit_mb: None,
                timeout_ms: 10_000,
            }
        }

        pub fn with_tier_preference(mut self, tier: SandboxExecutionTier) -> Self {
            self.tier_preference = Some(tier);
            self
        }

        pub fn with_fuel_limit(mut self, fuel_limit: u64) -> Self {
            self.fuel_limit = Some(fuel_limit);
            self
        }

        pub fn with_memory_limit_mb(mut self, mb: usize) -> Self {
            self.memory_limit_mb = Some(mb);
            self
        }

        pub fn with_timeout_ms(mut self, ms: u64) -> Self {
            self.timeout_ms = ms;
            self
        }
    }

    /// Unified execution outcome returned across all sandbox tiers
    #[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
    pub struct SandboxExecutionOutcome {
        pub stdout: String,
        pub stderr: String,
        pub exit_code: i32,
        pub tier_used: SandboxExecutionTier,
        pub duration_ms: u64,
        pub memory_bytes_used: u64,
        pub fuel_consumed: Option<u64>,
    }

    /// Host state for Tier 1 WASM execution store
    struct Tier1HostState {
        stdout_buf: Vec<u8>,
        stderr_buf: Vec<u8>,
        limits: StoreLimits,
    }

    /// Multi-Tier Execution Sandbox Router
    pub struct MultiTierSandboxRouter {
        workspace_root: PathBuf,
        microvm_sandbox: Arc<FirecrackerEphemeralSandbox>,
        dynatune_bridge: Arc<DynatuneBridge>,
        zig_compiler: Arc<ZigWasmAgentCompiler>,
        wasm_engine: Engine,
    }

    impl Default for MultiTierSandboxRouter {
        fn default() -> Self {
            let ws = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
            Self::new(ws)
        }
    }

    impl MultiTierSandboxRouter {
        /// Instantiate a new `MultiTierSandboxRouter`
        pub fn new(workspace_root: impl Into<PathBuf>) -> Self {
            let bridge = Arc::new(DynatuneBridge::new());
            Self::with_bridge(workspace_root, bridge)
        }

        /// Instantiate with custom Dynatune hardware bridge
        pub fn with_bridge(workspace_root: impl Into<PathBuf>, bridge: Arc<DynatuneBridge>) -> Self {
            let ws = workspace_root.into();
            let microvm = Arc::new(FirecrackerEphemeralSandbox::with_bridge(&ws, Arc::clone(&bridge)));

            let compiler_cfg = ZigCompilerConfig::default();
            let zig_compiler = Arc::new(ZigWasmAgentCompiler::with_config(compiler_cfg));

            let mut config = Config::new();
            config.consume_fuel(true);
            let wasm_engine = Engine::new(&config).expect("Failed to initialize Wasmtime engine for Tier 1 sandbox");

            Self {
                workspace_root: ws,
                microvm_sandbox: microvm,
                dynatune_bridge: bridge,
                zig_compiler,
                wasm_engine,
            }
        }

        /// Access workspace root path
        pub fn workspace_root(&self) -> &Path {
            &self.workspace_root
        }

        /// Access dynatune bridge
        pub fn dynatune_bridge(&self) -> &Arc<DynatuneBridge> {
            &self.dynatune_bridge
        }

        /// Access internal Zig compiler
        pub fn zig_compiler(&self) -> &Arc<ZigWasmAgentCompiler> {
            &self.zig_compiler
        }

        /// Access internal Firecracker microVM executor
        pub fn microvm_sandbox(&self) -> &Arc<FirecrackerEphemeralSandbox> {
            &self.microvm_sandbox
        }

        /// Analyzes request complexity to automatically route between Tier 1 (WASM-Zig)
        /// and Tier 2 (Firecracker / Landlock microVM).
        ///
        /// Complexity Analysis Rules:
        /// 1. If explicit `tier_preference` is provided, always honor it.
        /// 2. If language requires native OS execution (`bash`, `sh`, `zsh`, `python`, `node`, `javascript`, `typescript`, `ruby`, `rust`, `c`, `cpp`):
        ///    => Route to `Tier2FirecrackerMicroVm`.
        /// 3. If code contains system calls, filesystem writes, network operations, or external commands:
        ///    (`curl`, `wget`, `http://`, `https://`, `socket`, `fork(`, `exec(`, `system(`, `/proc/`, `/sys/`, `/dev/`, `rm `, `chmod `, etc.)
        ///    => Route to `Tier2FirecrackerMicroVm`.
        /// 4. If code length is heavy (> 16 KB) or compilation dependencies are required:
        ///    => Route to `Tier2FirecrackerMicroVm`.
        /// 5. Quick, untrusted code snippets in `wasm`, `wat`, or pure algorithm / math snippets in `zig`:
        ///    => Route to `Tier1WasmZig` (<2ms startup, sub-millisecond isolation, fuel metering).
        pub fn analyze_complexity(&self, request: &SandboxExecutionRequest) -> SandboxExecutionTier {
            if let Some(tier) = request.tier_preference {
                return tier;
            }

            let lang = request.language.trim().to_lowercase();

            // OS script languages requiring full Linux userspace
            match lang.as_str() {
                "bash" | "sh" | "zsh" | "shell" | "python" | "python3" | "py" | "node"
                | "javascript" | "js" | "typescript" | "ts" | "ruby" | "rb" | "perl"
                | "c" | "cpp" | "rust" | "rs" => {
                    return SandboxExecutionTier::Tier2FirecrackerMicroVm;
                }
                _ => {}
            }

            // Detect system call, filesystem, or network keywords
            let code_lower = request.code.to_lowercase();
            let syscall_signatures = [
                "curl ", "wget ", "http://", "https://", "socket(", "connect(", "bind(",
                "listen(", "fork(", "exec(", "system(", "popen(", "/etc/", "/proc/",
                "/sys/", "/dev/", "rm -rf", "mkdir ", "chmod ", "sudo ", "apt ", "cargo ",
                "npm ", "docker ",
            ];

            for sig in syscall_signatures {
                if code_lower.contains(sig) {
                    return SandboxExecutionTier::Tier2FirecrackerMicroVm;
                }
            }

            // Heavy code length threshold
            if request.code.len() > 16 * 1024 {
                return SandboxExecutionTier::Tier2FirecrackerMicroVm;
            }

            // WebAssembly Text format, binary wasm, or freestanding Zig math snippets
            if lang == "wat" || lang == "wasm" || lang == "wasm32" {
                return SandboxExecutionTier::Tier1WasmZig;
            }

            if lang == "zig" {
                // Freestanding Zig code without OS dependencies runs in Tier 1 WASM runtime
                if !code_lower.contains("std.os") && !code_lower.contains("std.fs") && !code_lower.contains("std.net") {
                    return SandboxExecutionTier::Tier1WasmZig;
                }
            }

            // Default to Tier 1 for small self-contained snippets, Tier 2 otherwise
            if request.code.len() < 4096 {
                SandboxExecutionTier::Tier1WasmZig
            } else {
                SandboxExecutionTier::Tier2FirecrackerMicroVm
            }
        }

        /// Execute a request across the automatically or explicitly selected sandbox tier
        pub fn execute(&self, request: &SandboxExecutionRequest) -> Result<SandboxExecutionOutcome> {
            let tier = self.analyze_complexity(request);
            match tier {
                SandboxExecutionTier::Tier1WasmZig => self.execute_tier1(request),
                SandboxExecutionTier::Tier2FirecrackerMicroVm => self.execute_tier2(request),
            }
        }

        /// Execute Tier 1 (Fast/Light): Quick, untrusted code snippets inside WASM Zig kernel
        /// (<2ms startup, fuel metering, capability enforcement).
        pub fn execute_tier1(&self, request: &SandboxExecutionRequest) -> Result<SandboxExecutionOutcome> {
            let start = Instant::now();
            let fuel_limit = request.fuel_limit.unwrap_or(10_000_000);
            let memory_mb = request.memory_limit_mb.unwrap_or(16);
            let max_memory_bytes = memory_mb * 1024 * 1024;

            // 1. Prepare WASM bytecode
            let wasm_bytes = self.prepare_wasm_bytes(request)?;

            // 2. Configure Store with fuel and memory limiter
            let limits = StoreLimitsBuilder::new()
                .memory_size(max_memory_bytes)
                .build();

            let host_state = Tier1HostState {
                stdout_buf: Vec::new(),
                stderr_buf: Vec::new(),
                limits,
            };

            let mut store = Store::new(&self.wasm_engine, host_state);
            store.limiter(|state| &mut state.limits);
            store.set_fuel(fuel_limit).map_err(|e| {
                HgbError::Execution(format!("Failed to set WASM fuel limit {}: {}", fuel_limit, e))
            })?;

            // 3. Compile Module
            let module = Module::new(&self.wasm_engine, &wasm_bytes).map_err(|e| {
                HgbError::Execution(format!("WASM compilation failed: {}", e))
            })?;

            // 4. Build Linker with WASI / host output capture
            let mut linker = Linker::new(&self.wasm_engine);
            self.bind_tier1_host_functions(&mut linker)?;

            // 5. Instantiate Module
            let instance = match linker.instantiate(&mut store, &module) {
                Ok(inst) => inst,
                Err(e) => {
                    let duration_ms = start.elapsed().as_millis() as u64;
                    let fuel_consumed = fuel_limit.saturating_sub(store.get_fuel().unwrap_or(0));
                    return Ok(SandboxExecutionOutcome {
                        stdout: String::new(),
                        stderr: format!("WASM instantiation error: {}", e),
                        exit_code: -1,
                        tier_used: SandboxExecutionTier::Tier1WasmZig,
                        duration_ms,
                        memory_bytes_used: 0,
                        fuel_consumed: Some(fuel_consumed),
                    });
                }
            };

            // 6. Invoke entry point: check for run, _start, main, or exported function
            let mut exit_code = 0;
            let mut trap_error = None;

            if let Ok(func) = instance.get_typed_func::<(), i32>(&mut store, "run") {
                match func.call(&mut store, ()) {
                    Ok(code) => exit_code = code,
                    Err(e) => trap_error = Some(e),
                }
            } else if let Ok(func) = instance.get_typed_func::<(), ()>(&mut store, "run") {
                if let Err(e) = func.call(&mut store, ()) {
                    trap_error = Some(e);
                }
            } else if let Ok(func) = instance.get_typed_func::<(), ()>(&mut store, "_start") {
                if let Err(e) = func.call(&mut store, ()) {
                    trap_error = Some(e);
                }
            } else if let Ok(func) = instance.get_typed_func::<(), i32>(&mut store, "main") {
                match func.call(&mut store, ()) {
                    Ok(code) => exit_code = code,
                    Err(e) => trap_error = Some(e),
                }
            } else if let Ok(func) = instance.get_typed_func::<(), ()>(&mut store, "main") {
                if let Err(e) = func.call(&mut store, ()) {
                    trap_error = Some(e);
                }
            }

            let duration_ms = start.elapsed().as_millis() as u64;
            let remaining_fuel = store.get_fuel().unwrap_or(0);
            let fuel_consumed = fuel_limit.saturating_sub(remaining_fuel);

            // Linear memory bytes used
            let memory_bytes_used = instance
                .get_memory(&mut store, "memory")
                .map(|mem| mem.data_size(&store) as u64)
                .unwrap_or(65536);

            let data = store.data();
            let mut stdout = String::from_utf8_lossy(&data.stdout_buf).to_string();
            let mut stderr = String::from_utf8_lossy(&data.stderr_buf).to_string();

            if let Some(trap) = trap_error {
                let trap_msg = trap.to_string();
                let is_out_of_fuel = remaining_fuel == 0
                    || trap_msg.contains("fuel")
                    || trap.downcast_ref::<Trap>() == Some(&Trap::OutOfFuel);

                exit_code = if is_out_of_fuel { -2 } else { -1 };
                if is_out_of_fuel {
                    if !stderr.is_empty() {
                        stderr.push('\n');
                    }
                    stderr.push_str("Execution trap: CPU fuel limit exhausted (OutOfFuel)");
                } else {
                    if !stderr.is_empty() {
                        stderr.push('\n');
                    }
                    stderr.push_str(&format!("Execution trap: {}", trap_msg));
                }
            } else if stdout.is_empty() && stderr.is_empty() {
                stdout = format!("WASM execution succeeded with exit code {}", exit_code);
            }

            Ok(SandboxExecutionOutcome {
                stdout,
                stderr,
                exit_code,
                tier_used: SandboxExecutionTier::Tier1WasmZig,
                duration_ms,
                memory_bytes_used,
                fuel_consumed: Some(fuel_consumed),
            })
        }

        /// Prepare WASM bytecode from code/language in request
        fn prepare_wasm_bytes(&self, request: &SandboxExecutionRequest) -> Result<Vec<u8>> {
            let lang = request.language.trim().to_lowercase();
            let trimmed_code = request.code.trim();

            if lang == "wat" || trimmed_code.starts_with("(module") {
                // WebAssembly Text format - Wasmtime compiles WAT text bytes directly into module bytecode
                Ok(request.code.as_bytes().to_vec())
            } else if lang == "wasm" || lang == "wasm32" {
                // Check if binary header \0asm
                if trimmed_code.as_bytes().starts_with(b"\0asm") {
                    Ok(trimmed_code.as_bytes().to_vec())
                } else if trimmed_code.starts_with("0x") || trimmed_code.chars().all(|c| c.is_ascii_hexdigit()) {
                    // Hex format
                    let hex_str = trimmed_code.strip_prefix("0x").unwrap_or(trimmed_code);
                    hex::decode(hex_str).map_err(|e| {
                        HgbError::Execution(format!("Failed to decode hex WASM bytes: {}", e))
                    })
                } else if let Ok(decoded) = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, trimmed_code) {
                    // Base64 format
                    Ok(decoded)
                } else {
                    // Fallback: try raw bytes
                    Ok(request.code.as_bytes().to_vec())
                }
            } else if lang == "zig" {
                // JIT compile Zig source code into wasm32-freestanding via ZigWasmAgentCompiler
                let comp = self.zig_compiler.compile(&request.code)?;
                Ok(comp.wasm_bytes)
            } else {
                // Fallback: raw bytes
                Ok(request.code.as_bytes().to_vec())
            }
        }

        /// Bind output capture and minimal WASI/host functions to Linker for Tier 1
        fn bind_tier1_host_functions(&self, linker: &mut Linker<Tier1HostState>) -> Result<()> {
            // Minimal WASI preview1 fd_write for capturing stdout and stderr
            linker.func_wrap(
                "wasi_snapshot_preview1",
                "fd_write",
                |mut caller: Caller<'_, Tier1HostState>, fd: i32, iovs_ptr: i32, iovs_len: i32, nwritten_ptr: i32| -> i32 {
                    let memory = match caller.get_export("memory").and_then(|e| e.into_memory()) {
                        Some(m) => m,
                        None => return 28, // EBADF
                    };

                    let mut total_written: u32 = 0;
                    for i in 0..iovs_len {
                        let iov_offset = (iovs_ptr as usize) + (i as usize) * 8;
                        let mem_data = memory.data(&caller);
                        if iov_offset + 8 > mem_data.len() {
                            return 21; // EFAULT
                        }

                        let buf_ptr = u32::from_le_bytes(mem_data[iov_offset..iov_offset + 4].try_into().unwrap()) as usize;
                        let buf_len = u32::from_le_bytes(mem_data[iov_offset + 4..iov_offset + 8].try_into().unwrap()) as usize;

                        if buf_ptr + buf_len > mem_data.len() {
                            return 21; // EFAULT
                        }

                        let bytes = mem_data[buf_ptr..buf_ptr + buf_len].to_vec();
                        let state = caller.data_mut();
                        if fd == 1 {
                            state.stdout_buf.extend_from_slice(&bytes);
                        } else if fd == 2 {
                            state.stderr_buf.extend_from_slice(&bytes);
                        }
                        total_written += buf_len as u32;
                    }

                    let mem_mut = memory.data_mut(&mut caller);
                    let nw_offset = nwritten_ptr as usize;
                    if nw_offset + 4 <= mem_mut.len() {
                        mem_mut[nw_offset..nw_offset + 4].copy_from_slice(&total_written.to_le_bytes());
                    }

                    0 // SUCCESS
                },
            ).map_err(|e| HgbError::Execution(format!("Failed to bind WASI fd_write: {}", e)))?;

            // Minimal WASI proc_exit
            linker.func_wrap(
                "wasi_snapshot_preview1",
                "proc_exit",
                |_caller: Caller<'_, Tier1HostState>, code: i32| {
                    let _ = code;
                },
            ).map_err(|e| HgbError::Execution(format!("Failed to bind WASI proc_exit: {}", e)))?;

            // Host output logging function for freestanding kernels: env.log(ptr, len)
            linker.func_wrap(
                "env",
                "log",
                |mut caller: Caller<'_, Tier1HostState>, ptr: i32, len: i32| {
                    let memory = match caller.get_export("memory").and_then(|e| e.into_memory()) {
                        Some(m) => m,
                        None => return,
                    };
                    let mem_data = memory.data(&caller);
                    let start = ptr as usize;
                    let end = start + (len as usize);
                    if end <= mem_data.len() {
                        let bytes = mem_data[start..end].to_vec();
                        caller.data_mut().stdout_buf.extend_from_slice(&bytes);
                    }
                },
            ).map_err(|e| HgbError::Execution(format!("Failed to bind env log: {}", e)))?;

            Ok(())
        }

        /// Execute Tier 2 (Heavy/Secure): Autonomous agent code execution inside lightweight
        /// Firecracker microVMs (hardware KVM virtualization with Landlock CoW fallback).
        pub fn execute_tier2(&self, request: &SandboxExecutionRequest) -> Result<SandboxExecutionOutcome> {
            let spec = EphemeralExecSpec {
                code: request.code.clone(),
                language: request.language.clone(),
                patch_content: None,
                timeout_ms: request.timeout_ms,
                memory_mb: request.memory_limit_mb.unwrap_or(128) as u32,
                vcpu: 2,
            };

            let result = self.microvm_sandbox.execute(&spec)?;

            let memory_bytes_used = (result.memory_consumed_mb as u64) * 1024 * 1024;

            Ok(SandboxExecutionOutcome {
                stdout: result.stdout,
                stderr: result.stderr,
                exit_code: result.exit_code,
                tier_used: SandboxExecutionTier::Tier2FirecrackerMicroVm,
                duration_ms: result.duration_ms,
                memory_bytes_used,
                fuel_consumed: None,
            })
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn test_automated_complexity_routing_tier1_wat() {
            let router = MultiTierSandboxRouter::default();
            let req = SandboxExecutionRequest::new(
                "(module (func (export \"run\") (result i32) i32.const 42))",
                "wat",
            );
            let tier = router.analyze_complexity(&req);
            assert_eq!(tier, SandboxExecutionTier::Tier1WasmZig);
        }

        #[test]
        fn test_automated_complexity_routing_tier2_bash() {
            let router = MultiTierSandboxRouter::default();
            let req = SandboxExecutionRequest::new("echo 'hello from bash'", "bash");
            let tier = router.analyze_complexity(&req);
            assert_eq!(tier, SandboxExecutionTier::Tier2FirecrackerMicroVm);
        }

        #[test]
        fn test_automated_complexity_routing_tier2_syscall_presence() {
            let router = MultiTierSandboxRouter::default();
            let req = SandboxExecutionRequest::new("curl https://api.github.com/zen", "wasm");
            let tier = router.analyze_complexity(&req);
            assert_eq!(tier, SandboxExecutionTier::Tier2FirecrackerMicroVm);
        }

        #[test]
        fn test_tier_preference_override() {
            let router = MultiTierSandboxRouter::default();
            let req = SandboxExecutionRequest::new("echo 'override'", "bash")
                .with_tier_preference(SandboxExecutionTier::Tier1WasmZig);
            let tier = router.analyze_complexity(&req);
            assert_eq!(tier, SandboxExecutionTier::Tier1WasmZig);
        }

        #[test]
        fn test_tier1_wasm_execution_wat_success() {
            let router = MultiTierSandboxRouter::default();
            let wat_code = r#"
            (module
                (func (export "run") (result i32)
                    i32.const 1337
                )
            )
            "#;
            let req = SandboxExecutionRequest::new(wat_code, "wat")
                .with_fuel_limit(1_000_000);

            let outcome = router.execute(&req).expect("tier 1 execution");
            assert_eq!(outcome.tier_used, SandboxExecutionTier::Tier1WasmZig);
            assert_eq!(outcome.exit_code, 1337);
            assert!(outcome.fuel_consumed.is_some());
            assert!(outcome.fuel_consumed.unwrap() > 0);
        }

        #[test]
        fn test_tier1_wasm_execution_fuel_metering_trap() {
            let router = MultiTierSandboxRouter::default();
            // Infinite loop WAT requiring endless fuel
            let wat_code = r#"
            (module
                (func (export "run") (result i32)
                    (loop $infinite
                        br $infinite
                    )
                    i32.const 0
                )
            )
            "#;
            let req = SandboxExecutionRequest::new(wat_code, "wat")
                .with_fuel_limit(500); // Tiny fuel limit to trigger trap

            let outcome = router.execute(&req).expect("tier 1 execution with trap");
            assert_eq!(outcome.tier_used, SandboxExecutionTier::Tier1WasmZig);
            assert_eq!(outcome.exit_code, -2);
            assert!(outcome.stderr.contains("fuel"));
        }

        #[test]
        fn test_tier2_firecracker_landlock_execution() {
            let router = MultiTierSandboxRouter::default();
            let req = SandboxExecutionRequest::new("echo 'Hello Multi-Tier Sandboxing'", "bash")
                .with_timeout_ms(5000);

            let outcome = router.execute(&req).expect("tier 2 execution");
            assert_eq!(outcome.tier_used, SandboxExecutionTier::Tier2FirecrackerMicroVm);
            assert_eq!(outcome.exit_code, 0);
            assert!(outcome.stdout.contains("Hello Multi-Tier Sandboxing"));
            assert!(outcome.duration_ms > 0);
            assert!(outcome.fuel_consumed.is_none());
        }
    }
}

pub use multi_tier_sandbox::{
    MultiTierSandboxRouter, SandboxExecutionOutcome, SandboxExecutionRequest,
    SandboxExecutionTier,
};

pub mod wasm_edge_mesh;
pub use wasm_edge_mesh::{
    MeshNodeAdvertisement, MeshPacket, WasmComputationGraph, WasmEdgeMeshEngine,
    WasmExecutionFragment, WasmFragmentOutcome, WasmGraphNode, WasmMeshExecutionReport,
    WasmMeshShardRouter, WasmNodeAdvertisement,
};
pub mod wasm_mesh {
    pub use super::wasm_edge_mesh::*;
}

pub mod mcp_mesh_router;
pub use mcp_mesh_router::{
    BackendHealthStatus, DistributedMcpMeshRouter, McpContextRequest, McpMeshCapability,
    McpMeshRouteTarget, McpMeshRouterConfig, McpMeshRoutingDecision, McpMeshRoutingOutcome,
    PrivacyLevel,
};

// Pillar 1: Native Subsystem Integration with dynatune & kernel-optimizer
pub mod dynatune_subsystem_fuse;
pub use dynatune_subsystem_fuse::{
    BatteryTelemetryRecord, CpuGovernorRecord, DynatuneSubsystemFuse,
    DynatuneWorkloadBoostGuard, FuseGovernorMode, MglruTelemetryRecord,
    PreviousSubsystemState, SafeguardState, SubsystemPaths,
    SubsystemTelemetrySnapshot, ThermalZoneRecord,
    DEFAULT_DYNATUNE_BIN, DEFAULT_DYNATUNE_SOCK, DEFAULT_KERNEL_CTL_BIN,
    IDLE_MGLRU_MIN_TTL_MS, SIBLING_KERNEL_CTL_BIN,
};

// Pillar 2: Distributed Micro-VM & WASM Edge Mesh Scheduler
pub mod distributed_microvm_scheduler;
pub use distributed_microvm_scheduler::{
    read_free_ram_mb as scheduler_read_free_ram_mb,
    write_framed_packet, read_framed_packet,
    DistributedExecutionReport, DistributedMicroVmScheduler, DistributedNodeDescriptor,
    DistributedTaskGraph, SchedulerPacket, ShardExecutionOutcome, TaskShard,
    DEFAULT_SCHEDULER_TCP_PORT, DEFAULT_SCHEDULER_UDP_PORT, DEFAULT_SHARD_FUEL_LIMIT,
    DEFAULT_STALE_NODE_TIMEOUT_SECS, DISTRIBUTED_FRAME_MAGIC, MAX_SCHEDULER_FRAME_BYTES,
};

// Pillar 4: Autonomous Loop Self-Healing & Hot-Reloading Plugins
pub mod autonomous_wasm_plugin_healer;
pub use autonomous_wasm_plugin_healer::{
    AutonomousHealingReport, AutonomousWasmPluginHealer, HotspotProfiler,
    PgoHotspotsReport, SynthesizedToolCode, ToolExecutionError, ToolHotspotMetric,
    ToolRequirement, WasmModuleCertificate, inspect_wasm_memory_limits,
    MAX_PLUGIN_MEMORY_PAGES_16MB, MAX_PLUGIN_MEMORY_PAGES_32MB,
};












