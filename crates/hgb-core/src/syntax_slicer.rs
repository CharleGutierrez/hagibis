use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TargetLanguage {
    Rust,
    TypeScript,
    JavaScript,
    Python,
    Go,
}

impl std::fmt::Display for TargetLanguage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Rust => write!(f, "Rust"),
            Self::TypeScript => write!(f, "TypeScript"),
            Self::JavaScript => write!(f, "JavaScript"),
            Self::Python => write!(f, "Python"),
            Self::Go => write!(f, "Go"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CallGraphNode {
    pub symbol_name: String,
    pub defining_file: PathBuf,
    pub line_number: usize,
    pub signature: String,
    pub is_focal_target: bool,
    pub skeletonized_body: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenReductionMetrics {
    pub raw_characters: usize,
    pub sliced_characters: usize,
    pub estimated_raw_tokens: usize,
    pub estimated_sliced_tokens: usize,
    pub reduction_percentage: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyntaxSliceResult {
    pub focal_symbol: String,
    pub language: TargetLanguage,
    pub nodes_included: Vec<CallGraphNode>,
    pub rendered_surgical_prompt: String,
    pub token_metrics: TokenReductionMetrics,
}
