use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DesignArchetype {
    MinimalistClean,
    BentoGridModern,
    DenseDashboard,
}

impl std::fmt::Display for DesignArchetype {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MinimalistClean => write!(f, "MinimalistClean"),
            Self::BentoGridModern => write!(f, "BentoGridModern"),
            Self::DenseDashboard => write!(f, "DenseDashboard"),
        }
    }
}

impl std::str::FromStr for DesignArchetype {
    type Err = String;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s.to_lowercase().replace(['-', '_', ' '], "").as_str() {
            "minimalistclean" | "minimalist" | "minimal" => Ok(Self::MinimalistClean),
            "bentogridmodern" | "bento" | "bentogrid" => Ok(Self::BentoGridModern),
            "densedashboard" | "dense" | "dashboard" => Ok(Self::DenseDashboard),
            other => Err(format!("Unknown design archetype: {}", other)),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VariantCandidate {
    pub candidate_id: String,
    pub archetype: DesignArchetype,
    pub branch_name: String,
    pub worktree_path: PathBuf,
    pub preview_port: u16,
    pub preview_url: String,
    pub patch_preview: String,
    pub passes_syntax_check: bool,
    pub passes_visual_check: bool,
    pub synthesis_duration_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum VariantRaceStatus {
    Synthesizing,
    ActivePreviewsReady,
    WinnerSelected { winner_id: String },
    Cancelled,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VariantRaceManifest {
    pub race_id: String,
    pub prompt: String,
    pub candidates: Vec<VariantCandidate>,
    pub status: VariantRaceStatus,
}
