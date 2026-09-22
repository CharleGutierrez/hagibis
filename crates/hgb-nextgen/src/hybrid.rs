use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CandidateToken {
    pub token: String,
    pub probability: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DraftDistribution {
    pub candidates: Vec<CandidateToken>,
}

impl DraftDistribution {
    pub fn new(mut candidates: Vec<CandidateToken>) -> Self {
        candidates.sort_by(|a, b| b.probability.partial_cmp(&a.probability).unwrap_or(std::cmp::Ordering::Equal));
        Self { candidates }
    }

    pub fn shannon_entropy(&self) -> f64 {
        let mut entropy = 0.0;
        for c in &self.candidates {
            if c.probability > 1e-9 {
                entropy -= c.probability * c.probability.log2();
            }
        }
        entropy
    }

    pub fn top_margin(&self) -> f64 {
        if self.candidates.len() >= 2 {
            self.candidates[0].probability - self.candidates[1].probability
        } else if self.candidates.len() == 1 {
            self.candidates[0].probability
        } else {
            0.0
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum LakandiwaVerdict {
    AcceptLocalDraft { token: String, confidence: f64 },
    EscalateToFrontier { reason: String, entropy: f64 },
}

pub struct SpeculativeHybridEngine {
    pub max_entropy: f64,
    pub min_margin: f64,
}

impl SpeculativeHybridEngine {
    pub fn new(max_entropy: f64, min_margin: f64) -> Self {
        Self { max_entropy, min_margin }
    }

    pub fn evaluate_draft(&self, dist: &DraftDistribution) -> LakandiwaVerdict {
        let entropy = dist.shannon_entropy();
        let margin = dist.top_margin();
        if dist.candidates.is_empty() {
            return LakandiwaVerdict::EscalateToFrontier {
                reason: "Empty distribution".to_string(),
                entropy: 0.0,
            };
        }
        let top = &dist.candidates[0];
        if entropy <= self.max_entropy && margin >= self.min_margin {
            LakandiwaVerdict::AcceptLocalDraft {
                token: top.token.clone(),
                confidence: top.probability,
            }
        } else {
            LakandiwaVerdict::EscalateToFrontier {
                reason: format!("High entropy ({:.2}) or low margin ({:.2})", entropy, margin),
                entropy,
            }
        }
    }
}
