use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;
use hgb_core::{HgbError, HgbProvider, Result};
pub use hgb_core::protocol::SwarmPodResult;
use crate::cockpit::{CockpitDagNode, CockpitNodeStatus};

/// Specialized agent persona in the Swarm Pod
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SwarmRole {
    Architect,
    Coder,
    Reviewer,
    QaTester,
}

impl SwarmRole {
    pub fn title(&self) -> &'static str {
        match self {
            Self::Architect => "🏛️ Lead Architect",
            Self::Coder => "💻 Surgical Coder",
            Self::Reviewer => "🛡️ Security & Style Reviewer",
            Self::QaTester => "🧪 Brutal QA Engineer",
        }
    }

    pub fn system_prompt(&self) -> &'static str {
        match self {
            Self::Architect => "You are the Lead Systems Rust Architect. Analyze requirements, break them down into data contracts and DAG steps, and enforce clean module boundaries.",
            Self::Coder => "You are the Surgical Coder. Write memory-safe, thread-safe, zero-placeholder, panic-free Rust implementation matching the Architect's contracts.",
            Self::Reviewer => "You are the Security & Style Reviewer. Inspect code for secret leaks, anti-patterns, OWASP risks, and AgentShieldLight violations. Reject sloppy code.",
            Self::QaTester => "You are the Brutal QA Engineer. Formulate brutal test matrices, boundary values, concurrency races, and negative test cases. Verify that all invariants hold.",
        }
    }
}

/// Output payload from a specialist agent
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoleOutput {
    pub role: SwarmRole,
    pub content: String,
    pub tokens_used: usize,
    pub approved: bool,
    pub feedback_notes: Vec<String>,
}

pub struct SwarmPod {
    provider: Arc<dyn HgbProvider>,
    workspace_root: PathBuf,
    dag_nodes: Arc<RwLock<HashMap<SwarmRole, CockpitDagNode>>>,
}

impl SwarmPod {
    pub fn new(provider: Arc<dyn HgbProvider>, workspace_root: PathBuf) -> Self {
        let mut nodes = HashMap::new();
        nodes.insert(SwarmRole::Architect, CockpitDagNode::new("pod-arch", "Architect Plan", "gemini-2.5-pro"));
        nodes.insert(SwarmRole::Coder, CockpitDagNode::new("pod-code", "Surgical Coder", "qwen2.5-coder:1.5b").with_parent("pod-arch"));
        nodes.insert(SwarmRole::Reviewer, CockpitDagNode::new("pod-rev", "Security Audit", "gemini-2.5-flash").with_parent("pod-code"));
        nodes.insert(SwarmRole::QaTester, CockpitDagNode::new("pod-qa", "Brutal QA", "qwen2.5-coder:1.5b").with_parent("pod-code"));

        Self {
            provider,
            workspace_root,
            dag_nodes: Arc::new(RwLock::new(nodes)),
        }
    }

    pub fn workspace_root(&self) -> &PathBuf {
        &self.workspace_root
    }

    /// Execute the collaborative 4-step pipeline: Architect -> Coder -> [Reviewer + QA]
    pub async fn execute_task(&self, task: &str) -> Result<SwarmPodResult> {
        let start = std::time::Instant::now();
        let mut total_tokens = 0;

        // 1. Step 1: Architect Plan
        self.update_node(SwarmRole::Architect, CockpitNodeStatus::Running { progress_pct: 20 }, "Synthesizing architectural contracts...").await;
        let arch_prompt = format!("{}\n\nTask: {}\nSynthesize architectural specification:", SwarmRole::Architect.system_prompt(), task);
        let arch_plan = self.provider.complete(&arch_prompt, None).await?;
        total_tokens += arch_plan.len().max(4) / 4;
        self.update_node(SwarmRole::Architect, CockpitNodeStatus::Succeeded { duration_ms: 120 }, &arch_plan[..100.min(arch_plan.len())]).await;

        // 2. Step 2: Coder Implementation
        self.update_node(SwarmRole::Coder, CockpitNodeStatus::Running { progress_pct: 50 }, "Generating surgical implementation...").await;
        let code_prompt = format!("{}\n\nArchitect Plan:\n{}\n\nWrite implementation:", SwarmRole::Coder.system_prompt(), arch_plan);
        let code_solution = self.provider.complete(&code_prompt, None).await?;
        total_tokens += code_solution.len().max(4) / 4;
        self.update_node(SwarmRole::Coder, CockpitNodeStatus::Succeeded { duration_ms: 250 }, &code_solution[..100.min(code_solution.len())]).await;

        // 3. Step 3: Concurrent Reviewer and QA
        self.update_node(SwarmRole::Reviewer, CockpitNodeStatus::Running { progress_pct: 75 }, "Auditing security and style...").await;
        self.update_node(SwarmRole::QaTester, CockpitNodeStatus::Running { progress_pct: 75 }, "Formulating brutal test matrix...").await;

        let rev_provider = self.provider.clone();
        let code_clone1 = code_solution.clone();
        let rev_handle = tokio::spawn(async move {
            let prompt = format!("{}\n\nReview this code:\n{}\nEmit: [APPROVED] or [REJECTED: reason]", SwarmRole::Reviewer.system_prompt(), code_clone1);
            rev_provider.complete(&prompt, None).await
        });

        let qa_provider = self.provider.clone();
        let code_clone2 = code_solution.clone();
        let qa_handle = tokio::spawn(async move {
            let prompt = format!("{}\n\nGenerate brutal test suite for this code:\n{}", SwarmRole::QaTester.system_prompt(), code_clone2);
            qa_provider.complete(&prompt, None).await
        });

        let (rev_res, qa_res) = tokio::join!(rev_handle, qa_handle);
        let review_text = rev_res.map_err(|e| HgbError::Execution(e.to_string()))??;
        let qa_text = qa_res.map_err(|e| HgbError::Execution(e.to_string()))??;

        total_tokens += (review_text.len() + qa_text.len()).max(4) / 4;
        let review_passed = review_text.contains("[APPROVED]") || !review_text.contains("[REJECTED");

        self.update_node(SwarmRole::Reviewer, CockpitNodeStatus::Succeeded { duration_ms: 180 }, if review_passed { "Review Passed: Clean" } else { "Review Flagged Issues" }).await;
        self.update_node(SwarmRole::QaTester, CockpitNodeStatus::Succeeded { duration_ms: 210 }, "Test suite synthesized").await;

        Ok(SwarmPodResult {
            task: task.to_string(),
            architect_plan: arch_plan,
            code_solution,
            review_status: review_passed,
            review_notes: vec![review_text],
            test_coverage: qa_text,
            total_tokens,
            duration_ms: start.elapsed().as_millis() as u64,
        })
    }

    async fn update_node(&self, role: SwarmRole, status: CockpitNodeStatus, scratchpad: &str) {
        let mut nodes = self.dag_nodes.write().await;
        if let Some(node) = nodes.get_mut(&role) {
            node.status = status;
            node.scratchpad = scratchpad.to_string();
        }
    }

    pub async fn get_dag_nodes(&self) -> Vec<CockpitDagNode> {
        let nodes = self.dag_nodes.read().await;
        let mut list: Vec<CockpitDagNode> = nodes.values().cloned().collect();
        list.sort_by(|a, b| a.id.cmp(&b.id));
        list
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;

    struct MockSwarmProvider {
        approval: bool,
    }

    #[async_trait]
    impl HgbProvider for MockSwarmProvider {
        fn name(&self) -> &str {
            "mock-swarm"
        }

        async fn complete(&self, prompt: &str, _model: Option<&str>) -> Result<String> {
            if prompt.contains("Lead Systems Rust Architect") {
                Ok("Architect plan: Implement TokenBucket rate limiter struct with atomic CAS".into())
            } else if prompt.contains("Surgical Coder") {
                Ok("pub struct TokenBucket { tokens: AtomicU64 }".into())
            } else if prompt.contains("Security & Style Reviewer") {
                if self.approval {
                    Ok("[APPROVED] Clean implementation, no leaks".into())
                } else {
                    Ok("[REJECTED: overflow risk in CAS loop]".into())
                }
            } else {
                Ok("#[test] fn test_token_bucket() { ... }".into())
            }
        }
    }

    #[tokio::test]
    async fn test_swarm_pod_successful_consensus() {
        let provider = Arc::new(MockSwarmProvider { approval: true });
        let pod = SwarmPod::new(provider, PathBuf::from("/tmp"));

        let res = pod.execute_task("Implement rate limiter").await.expect("pod execution");
        assert_eq!(res.task, "Implement rate limiter");
        assert!(res.architect_plan.contains("TokenBucket"));
        assert!(res.code_solution.contains("pub struct TokenBucket"));
        assert!(res.review_status);
        assert!(res.test_coverage.contains("test_token_bucket"));
        assert!(res.total_tokens > 0);

        let nodes = pod.get_dag_nodes().await;
        assert_eq!(nodes.len(), 4);
        for node in nodes {
            assert!(matches!(node.status, CockpitNodeStatus::Succeeded { .. }));
        }
    }

    #[tokio::test]
    async fn test_swarm_pod_rejection_consensus() {
        let provider = Arc::new(MockSwarmProvider { approval: false });
        let pod = SwarmPod::new(provider, PathBuf::from("/tmp"));

        let res = pod.execute_task("Implement buggy component").await.expect("pod execution");
        assert!(!res.review_status);
        assert!(res.review_notes[0].contains("REJECTED"));
    }
}
