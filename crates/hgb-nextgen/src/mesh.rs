use serde::{Deserialize, Serialize};
use std::collections::VecDeque;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MeshNode {
    pub node_id: String,
    pub compute_tier: String,
    pub vram_mb: usize,
    pub active_models: Vec<String>,
    pub max_concurrency: usize,
}

pub struct P2pSwarmMesh {
    nodes: Vec<MeshNode>,
    task_queue: VecDeque<String>,
}

impl P2pSwarmMesh {
    pub fn new() -> Self {
        Self {
            nodes: Vec::new(),
            task_queue: VecDeque::new(),
        }
    }

    pub fn register_node(&mut self, node: MeshNode) {
        self.nodes.retain(|n| n.node_id != node.node_id);
        self.nodes.push(node);
    }

    pub fn enqueue_task(&mut self, task_id: &str) {
        self.task_queue.push_back(task_id.to_string());
    }

    pub fn steal_task(&mut self) -> Option<String> {
        self.task_queue.pop_front()
    }

    pub fn nodes_count(&self) -> usize {
        self.nodes.len()
    }
}
