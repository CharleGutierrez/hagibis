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
