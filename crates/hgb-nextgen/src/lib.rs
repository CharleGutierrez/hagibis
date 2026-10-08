pub mod auto_spec;
pub mod browser_snoop;
pub mod checkpoint;
pub mod chrono_warp;
pub mod clipboard_xerox;
pub mod cockpit;
pub mod compactor;
pub mod council;
pub mod crash_interceptor;
pub mod diff_hud;
pub mod drift_lock;
pub mod fuzz;
pub mod ghost_engine;
pub mod glance_engine;
pub mod green_light;
pub mod guardian;
pub mod hallucination_sentry;
pub mod heal;
pub mod hybrid;
pub mod mesh;
pub mod local_proxy_fabric;
pub mod passive_sentinel;
pub mod phantom_swarm;
pub mod pixel_radar;
pub mod pod;
pub mod port_multiplexer;
pub mod pr_storyteller;
pub mod provenance;
pub mod qr;
pub mod race;
pub mod redteam;
pub mod self_validating;
pub mod sentinel;
pub mod storyteller;
pub mod syntax_slicer;
pub mod terminal_graphics;
pub mod variant_race;
pub mod voice_hook;
pub mod wattage_governor;
pub mod wiretap;
pub mod worktree;
pub mod dom_teleport;
pub mod stream_steer;
pub mod rootless_sandbox;
pub mod arena_mode;
pub mod context_gc;
pub mod vibe_deployer;
pub mod type_harmonizer;
pub mod shadow_exec;
pub mod db_migration_synthesizer;
pub mod zero_proxy;
pub mod ghost_typing;
pub mod invariant_shield;
pub mod architecture_dag;
pub mod voice_stream;
pub mod inline_diff_engine;

pub use inline_diff_engine::{
    DiffOpKind, DualBufferOverlay, InlineDiffEngine, InlineDiffLine, InlineDiffMetrics,
    InlineDiffSpan,
};
pub use auto_spec::AutoSpecEngine;
pub use browser_snoop::BrowserSnoopEngine;
pub use checkpoint::{FileSnapshotState, RollbackReport, SwarmCheckpoint, SwarmCheckpointManager, SwarmEvent, SwarmWal};
pub use chrono_warp::{ChronoWarpEngine, ChronoWarpRollbackReport, ChronoWarpSnapshot, ProcessStatus, TrackedProcess};
pub use clipboard_xerox::{ClipboardXeroxEngine, ColorRole, DominantColor, LayoutRegion, XeroxSynthesisResult};
pub use cockpit::{
    ChronoWarpCardItem, ClipboardXeroxCardItem, CockpitActiveTab, CockpitApp,
    CockpitArtifactDiff, CockpitBackgroundTask, CockpitChatItem, CockpitChatSender,
    CockpitDagNode, CockpitInputMode, CockpitItem, CockpitNodeStatus, CockpitOverlay,
    CockpitState, CockpitTelemetry, CockpitToolCall, CockpitVibeManager, CockpitViewMode,
    CouncilCardItem, CrashBannerItem, DbTimeMachineCardItem, DiffAction, DiffActionHitbox,
    DiffCardItem, DiffCardStatus, GhostCardItem, GreenLightCardItem,
    HallucinationSentryCardItem, HealHitbox, ImagePreviewItem, PhantomSwarmCardItem,
    PixelRadarCardItem, SteeringAction, TelepathyCardItem, TunnelCardItem,
    WattageGovernorCardItem, WiretapCardItem, WebBrowseCardItem, WebSearchCardItem,
    MemoryCardItem, AmbientCardItem, ValidationCardItem, ForgeCardItem, PruneCardItem,
    PortCardItem, ShipCardItem,
    BrowserIncidentCardItem, SeedCardItem, RewindCardItem,
    RedTeamCardItem, BlueprintCardItem, PassiveSentinelCardItem,
};
pub use compactor::{CompactionReport, SmartAutoCompactor};
pub use council::{CouncilDebate, CouncilEngine, CouncilRound, CouncilVerdict};
pub use crash_interceptor::{CrashDetector, CrashSourceType, InterceptedCrash};
pub use diff_hud::{DiffHunk, DiffLine, DiffLineKind, SelectivePatcher};
pub use drift_lock::DriftLockEngine;
pub use fuzz::{AgenticFuzzEngine, FuzzViolation};
pub use ghost_engine::{CursorContext, GhostCandidate, GhostEngine};
pub use glance_engine::{
    BoundingBox, CssSuggestion, DefectCategory, DefectSeverity, GlanceEngine, ImagePayload,
    LayoutDefect, PatchTarget, VisualInspectionReport,
};
pub use green_light::{GreenLightEngine, GreenLightReport, SpecRequirement};
pub use guardian::{GhostFix, GhostFixStatus, GuardianConfig, GuardianEngine, GuardianEvent};
pub use heal::{CompilerDiagnostic, HealAttempt, HealEngine, HealReport};
pub use hybrid::{CandidateToken, DraftDistribution, LakandiwaVerdict, SpeculativeHybridEngine};
pub use mesh::{MeshNode, P2pSwarmMesh};
pub use local_proxy_fabric::{LocalProxyFabric, LocalProxyFabricConfig, LocalProxyFabricServer};
pub use phantom_swarm::{LatencyPercentiles, PhantomSwarmConfig, PhantomSwarmEngine, PhantomSwarmReport};
pub use pixel_radar::{CssOverflowDefect, DomSnapshot, PixelDiffRadar, PixelDiffReport};
pub use pod::{RoleOutput, SwarmPod, SwarmPodResult, SwarmRole};
pub use port_multiplexer::{GatewayRoute, PortCollision, PortMultiplexer, PortMultiplexerReport};
pub use pr_storyteller::{AtomicCommit, PrStoryReport, PrStorytellerEngine};
pub use provenance::{MerkleHop, MerkleProof, MerkleTree, ProvenanceEntry, ProvenanceLedger, HAGIBIS_PROVENANCE_GENESIS};
pub use qr::{detect_local_ip, render_mobile_test_card, QrMatrix};
pub use race::{RaceCandidateResult, SpeculativeRaceRunner};
pub use self_validating::{CompilerHealer, SelfValidatingEngine, ValidationError, ValidationReport};
pub type SelfValidatingLoop = SelfValidatingEngine;
pub use sentinel::{DevServerEndpoint, DevServerFramework, DevServerSentinel, RuntimeDiagnostic};
pub use storyteller::{PrReport, PrStoryteller};
pub use syntax_slicer::SyntaxSlicer;
pub use terminal_graphics::{
    detect_graphics_protocol, ImageBuffer, RgbPixel, TerminalGraphicsProtocol,
};
pub use variant_race::VariantRaceEngine;
pub use voice_hook::{AudioPromptEngine, VoicePromptSession};
pub use wattage_governor::{GovernorConfig, RoutingDecision, SessionExpenditure, WattageGovernor};
pub use wiretap::{ContractDrift, ContractPatch, DriftKind, ExpectedField, WiretapEngine};
pub use worktree::{AtmosphericWorktreeHandle, SemanticStash, SemanticStashManager};
pub use redteam::{RedTeamAuditor, RedTeamCategory, RedTeamFinding, RedTeamReport, RedTeamSeverity, RedTeamVerdict};
pub use passive_sentinel::{PassiveSentinel, SentinelEvent, SentinelEventKind, SentinelHealthStatus, SentinelTelemetry};
pub use hallucination_sentry::{HallucinationSentry, ManifestAuditReport, ManifestEcosystem, PackageAuditEntry};
pub use dom_teleport::{DomElementClickEvent, DomToAstTeleporter, TeleportResult};
pub use stream_steer::{NudgeReport, SteeringStatus, StreamSteeringController};
pub use rootless_sandbox::{RootlessSandboxEngine, SandboxConfig, SandboxExecutionReport};
pub use arena_mode::{ArenaCandidate, ArenaManifest, ArenaMergeReport, ArenaSwarmEngine};
pub use context_gc::{ContextAntiRotGc, GcReport, TurnRecord};
pub use vibe_deployer::{DeployReport, DeployTarget, VibeDeployerEngine};
pub use type_harmonizer::{FieldDefinition, HarmonizeReport, ModelDefinition, TypeDriftHarmonizer};
pub use shadow_exec::{ShadowExecutionEngine, ShadowExecutionReport, ShadowTestResult};
pub use db_migration_synthesizer::{ColumnSpec, DbMigrationSynthesizer, MigrationPlan, TableSpec};
pub use zero_proxy::{InterceptedCall, ProxyResponse, ZeroLocalProxyFabric, ZeroProxyReport};
pub use ghost_typing::{GhostPrediction, GhostTypingEngine};
pub use invariant_shield::{HazardKind, InvariantAuditReport, InvariantShieldEngine, PanicHazard};
pub use architecture_dag::{ArchitectureDagVisualizer, ArchitectureTopology, DagEdge, DagNode};
pub use voice_stream::{VoicePacket, VoiceStreamCoPilot, VoiceTranscriptionEvent};



