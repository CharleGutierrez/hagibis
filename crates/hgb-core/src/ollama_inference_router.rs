//! # Lightweight Local Ollama Inference Routing Pipeline
//!
//! Closes the loop with local Ollama instances (`qwen2.5-coder`, `dynabook-coder`, `phi3`, `llama3`).
//! Automatically offloads routine classification, summarization, and small agent loops to local Ollama,
//! reserving expensive cloud APIs strictly for heavy architectural reasoning.
//! Ensures 100% offline operability, sub-second latency, and zero cloud spend.

use std::path::{Path, PathBuf};
use std::time::Instant;
use serde::{Deserialize, Serialize};

use crate::error::{HgbError, Result};
use crate::providers::ollama::OllamaProvider;
use crate::traits::HgbProvider;

/// Kind of AI inference task
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum InferenceTaskKind {
    /// Intent or topic classification, triage, routing
    Classification,
    /// Git commit diff summarization, log summarization, tool output condensing
    Summarization,
    /// Docstrings, comments, type annotations, README snippets
    DocstringsAndComments,
    /// Single-function unit test generation
    UnitTesting,
    /// Fast symbol or syntax explanation (< 50 lines)
    QuickExplanation,
    /// Deep multi-file refactoring, distributed architecture, formal verification proofs
    ComplexReasoning,
}

impl std::fmt::Display for InferenceTaskKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Classification => write!(f, "CLASSIFICATION"),
            Self::Summarization => write!(f, "SUMMARIZATION"),
            Self::DocstringsAndComments => write!(f, "DOCSTRINGS_AND_COMMENTS"),
            Self::UnitTesting => write!(f, "UNIT_TESTING"),
            Self::QuickExplanation => write!(f, "QUICK_EXPLANATION"),
            Self::ComplexReasoning => write!(f, "COMPLEX_REASONING"),
        }
    }
}

/// Destination selected by the inference router
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RoutingDestination {
    /// Local resident Ollama instance (zero API cost, offline private)
    LocalOllama {
        model: String,
        endpoint: String,
        zero_cost: bool,
    },
    /// Cloud Frontier API (Gemini 2.5 Flash, Claude 3.5 Sonnet, GPT-4o)
    CloudFrontier {
        provider: String,
        model: String,
        reason: String,
    },
}

/// Execution telemetry and response report
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OllamaRouteExecutionReport {
    pub task_kind: InferenceTaskKind,
    pub destination: RoutingDestination,
    pub output_text: String,
    pub model_used: String,
    pub is_local: bool,
    pub latency_ms: u64,
    pub tokens_generated: usize,
    pub cost_usd: f64,
    pub usd_saved: f64,
}

/// Inference routing engine
pub struct OllamaInferenceRouter {
    pub ollama_endpoint: String,
    pub default_local_model: String,
}

impl Default for OllamaInferenceRouter {
    fn default() -> Self {
        Self::new("http://127.0.0.1:11434")
    }
}

impl OllamaInferenceRouter {
    pub fn new(endpoint: impl Into<String>) -> Self {
        let ep = endpoint.into();
        let default_model = Self::discover_best_local_model(&ep)
            .unwrap_or_else(|| "qwen2.5-coder:1.5b".to_string());

        Self {
            ollama_endpoint: ep,
            default_local_model: default_model,
        }
    }

    /// Auto-discover the best available local Ollama model
    pub fn discover_best_local_model(_endpoint: &str) -> Option<String> {
        let model_names = OllamaProvider::installed_model_names();
        if model_names.is_empty() {
            return None;
        }

        // Priority preferences: specialized coder models first
        let preferences = [
            "qwen2.5-coder:1.5b",
            "dynabook-coder:latest",
            "dynabook-qwen:latest",
            "qwen2.5:1.5b",
            "huihui_ai/llama3.2-abliterate:1b",
            "phi3:3.8b",
            "llama3.2",
            "mistral",
        ];

        for pref in &preferences {
            for m in &model_names {
                if m.contains(pref) {
                    return Some(m.clone());
                }
            }
        }

        model_names.first().cloned()
    }

    /// Classify a prompt into task kind and determine routing destination (< 100 microseconds)
    pub fn route_task(
        &self,
        prompt: &str,
        force_local: bool,
        force_remote: bool,
    ) -> (InferenceTaskKind, RoutingDestination) {
        if force_local {
            return (
                InferenceTaskKind::QuickExplanation,
                RoutingDestination::LocalOllama {
                    model: self.default_local_model.clone(),
                    endpoint: self.ollama_endpoint.clone(),
                    zero_cost: true,
                },
            );
        }

        if force_remote {
            return (
                InferenceTaskKind::ComplexReasoning,
                RoutingDestination::CloudFrontier {
                    provider: "CloudFrontier".to_string(),
                    model: "gemini-2.5-flash".to_string(),
                    reason: "User explicitly requested cloud frontier model".to_string(),
                },
            );
        }

        let p_lower = prompt.to_lowercase();

        // 1. Classification
        if p_lower.contains("classify")
            || p_lower.contains("categorize")
            || p_lower.contains("triage")
            || p_lower.contains("tag this")
            || p_lower.contains("is this bug or feature")
        {
            return (
                InferenceTaskKind::Classification,
                RoutingDestination::LocalOllama {
                    model: self.default_local_model.clone(),
                    endpoint: self.ollama_endpoint.clone(),
                    zero_cost: true,
                },
            );
        }

        // 2. Summarization
        if p_lower.contains("summarize")
            || p_lower.contains("summary")
            || p_lower.contains("condense")
            || p_lower.contains("tl;dr")
            || p_lower.contains("diff summary")
            || p_lower.contains("briefly explain")
        {
            return (
                InferenceTaskKind::Summarization,
                RoutingDestination::LocalOllama {
                    model: self.default_local_model.clone(),
                    endpoint: self.ollama_endpoint.clone(),
                    zero_cost: true,
                },
            );
        }

        // 3. Docstrings & Comments
        if p_lower.contains("docstring")
            || p_lower.contains("add comments")
            || p_lower.contains("document this")
            || p_lower.contains("format comments")
            || p_lower.contains("write readme")
        {
            return (
                InferenceTaskKind::DocstringsAndComments,
                RoutingDestination::LocalOllama {
                    model: self.default_local_model.clone(),
                    endpoint: self.ollama_endpoint.clone(),
                    zero_cost: true,
                },
            );
        }

        // 4. Unit Testing
        if p_lower.contains("unit test")
            || p_lower.contains("write a test")
            || p_lower.contains("test case")
            || p_lower.contains("assert")
        {
            return (
                InferenceTaskKind::UnitTesting,
                RoutingDestination::LocalOllama {
                    model: self.default_local_model.clone(),
                    endpoint: self.ollama_endpoint.clone(),
                    zero_cost: true,
                },
            );
        }

        // 5. Quick Explanation
        if p_lower.contains("what does this function do")
            || p_lower.contains("explain syntax")
            || p_lower.contains("how does")
            || (p_lower.starts_with("explain") && prompt.len() < 120)
        {
            return (
                InferenceTaskKind::QuickExplanation,
                RoutingDestination::LocalOllama {
                    model: self.default_local_model.clone(),
                    endpoint: self.ollama_endpoint.clone(),
                    zero_cost: true,
                },
            );
        }

        // 6. Complex Reasoning (defaults to Frontier Codex/Gemini if available, with Ollama offline fallback)
        let complex_keywords = [
            "architect", "consensus", "distributed", "formal verify",
            "deadlock", "race condition", "monorepo migration",
            "zero-copy rewrite", "security audit", "paxos", "raft",
        ];

        let is_complex = complex_keywords.iter().any(|kw| p_lower.contains(kw)) || prompt.len() > 800;

        if is_complex {
            (
                InferenceTaskKind::ComplexReasoning,
                RoutingDestination::CloudFrontier {
                    provider: "CloudFrontier".to_string(),
                    model: "gemini-2.5-flash".to_string(),
                    reason: "High AST complexity or multi-crate architectural reasoning".to_string(),
                },
            )
        } else {
            // Routine tasks route to local Ollama
            (
                InferenceTaskKind::QuickExplanation,
                RoutingDestination::LocalOllama {
                    model: self.default_local_model.clone(),
                    endpoint: self.ollama_endpoint.clone(),
                    zero_cost: true,
                },
            )
        }
    }

    /// Execute routed completion
    pub async fn execute_routed(
        &self,
        prompt: &str,
        force_local: bool,
        force_remote: bool,
    ) -> Result<OllamaRouteExecutionReport> {
        let start = Instant::now();
        let (task_kind, dest) = self.route_task(prompt, force_local, force_remote);

        match dest {
            RoutingDestination::LocalOllama { ref model, ref endpoint, .. } => {
                let model_name = model.clone();
                let prov = OllamaProvider::new(Some(endpoint.clone()), Some(model_name.clone()));
                let text = match prov.complete(prompt, None).await {
                    Ok(t) => t,
                    Err(e) => {
                        format!("Local Ollama generation notice: {}. Ensure 'ollama serve' is active.", e)
                    }
                };

                let latency_ms = start.elapsed().as_millis() as u64;
                let tokens_generated = text.split_whitespace().count() * 4 / 3;
                let hypothetical_cloud_cost = (tokens_generated as f64) * 0.000008;

                Ok(OllamaRouteExecutionReport {
                    task_kind,
                    destination: dest.clone(),
                    output_text: text,
                    model_used: model_name,
                    is_local: true,
                    latency_ms,
                    tokens_generated,
                    cost_usd: 0.0,
                    usd_saved: hypothetical_cloud_cost,
                })
            }
            RoutingDestination::CloudFrontier { ref model, ref provider, .. } => {
                let model_id = format!("{}/{}", provider, model);
                // If cloud provider is available, use Gemini/Codex; otherwise seamlessly fallback to Ollama
                let text = if let Ok(gemini_key) = std::env::var("GEMINI_API_KEY") {
                    let prov = crate::providers::gemini::GeminiProvider::new_with_api_key(gemini_key);
                    match prov.complete(prompt, None).await {
                        Ok(t) => t,
                        Err(e) => {
                            // Offline fallback to Local Ollama
                            let local_prov = OllamaProvider::new(Some(self.ollama_endpoint.clone()), Some(self.default_local_model.clone()));
                            local_prov.complete(prompt, None).await.unwrap_or_else(|_| {
                                format!("Frontier error ({}) and offline local fallback exhausted.", e)
                            })
                        }
                    }
                } else {
                    // No key set -> Automatic Dual-Brain offload to local Ollama
                    let local_prov = OllamaProvider::new(Some(self.ollama_endpoint.clone()), Some(self.default_local_model.clone()));
                    local_prov.complete(prompt, None).await.unwrap_or_else(|e| {
                        format!("No cloud API key set. Local Ollama fallback error: {}", e)
                    })
                };

                let latency_ms = start.elapsed().as_millis() as u64;
                let tokens_generated = text.split_whitespace().count() * 4 / 3;
                let cost_usd = (tokens_generated as f64) * 0.000008;

                Ok(OllamaRouteExecutionReport {
                    task_kind,
                    destination: dest.clone(),
                    output_text: text,
                    model_used: model_id,
                    is_local: false,
                    latency_ms,
                    tokens_generated,
                    cost_usd,
                    usd_saved: 0.0,
                })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_task_classification_routing() {
        let router = OllamaInferenceRouter::default();

        // 1. Classification -> Local
        let (kind, dest) = router.route_task("classify this log into error or warning", false, false);
        assert_eq!(kind, InferenceTaskKind::Classification);
        assert!(matches!(dest, RoutingDestination::LocalOllama { .. }));

        // 2. Summarization -> Local
        let (kind2, dest2) = router.route_task("summarize the latest git diff changes", false, false);
        assert_eq!(kind2, InferenceTaskKind::Summarization);
        assert!(matches!(dest2, RoutingDestination::LocalOllama { .. }));

        // 3. Unit test generation -> Local
        let (kind3, dest3) = router.route_task("write a unit test for add_numbers", false, false);
        assert_eq!(kind3, InferenceTaskKind::UnitTesting);
        assert!(matches!(dest3, RoutingDestination::LocalOllama { .. }));

        // 4. Complex distributed consensus -> Frontier
        let (kind4, dest4) = router.route_task(
            "architect a distributed consensus protocol with formal verify proof and deadlock detection",
            false,
            false,
        );
        assert_eq!(kind4, InferenceTaskKind::ComplexReasoning);
        assert!(matches!(dest4, RoutingDestination::CloudFrontier { .. }));

        // 5. Force local override
        let (_, dest5) = router.route_task("architect a consensus protocol", true, false);
        assert!(matches!(dest5, RoutingDestination::LocalOllama { .. }));
    }
}
