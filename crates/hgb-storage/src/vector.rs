use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VectorEntry {
    pub id: String,
    pub text: String,
    pub embedding: Vec<f32>,
}

/// In-Process Fast Vector Index with Cosine Similarity
pub struct VectorIndex {
    entries: Vec<VectorEntry>,
}

impl VectorIndex {
    pub fn new() -> Self {
        Self { entries: Vec::new() }
    }

    pub fn insert(&mut self, entry: VectorEntry) {
        self.entries.push(entry);
    }

    pub fn search(&self, query: &[f32], top_k: usize) -> Vec<(&VectorEntry, f32)> {
        let mut scored: Vec<(&VectorEntry, f32)> = self.entries
            .iter()
            .map(|e| {
                let sim = cosine_similarity(&e.embedding, query);
                (e, sim)
            })
            .collect();
        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        scored.truncate(top_k);
        scored
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }
}

pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let mut dot = 0.0;
    let mut norm_a = 0.0;
    let mut norm_b = 0.0;
    for (x, y) in a.iter().zip(b.iter()) {
        dot += x * y;
        norm_a += x * x;
        norm_b += y * y;
    }
    if norm_a <= 0.0 || norm_b <= 0.0 {
        return 0.0;
    }
    dot / (norm_a.sqrt() * norm_b.sqrt())
}
