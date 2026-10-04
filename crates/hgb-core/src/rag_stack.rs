use std::collections::HashMap;

/// A simple structure representing an embedded vector
#[derive(Debug, Clone)]
pub struct RagVector {
    pub id: String,
    pub values: Vec<f32>,
    pub metadata: HashMap<String, String>,
}

impl RagVector {
    pub fn new(id: impl Into<String>, values: Vec<f32>) -> Self {
        Self {
            id: id.into(),
            values,
            metadata: HashMap::new(),
        }
    }

    pub fn with_metadata(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.metadata.insert(key.into(), value.into());
        self
    }
}

/// Calculate cosine similarity between two vectors
pub fn cosine_similarity(v1: &[f32], v2: &[f32]) -> f32 {
    if v1.is_empty() || v2.is_empty() || v1.len() != v2.len() {
        return 0.0;
    }

    let dot_product: f32 = v1.iter().zip(v2.iter()).map(|(a, b)| a * b).sum();
    let norm_v1: f32 = v1.iter().map(|a| a * a).sum::<f32>().sqrt();
    let norm_v2: f32 = v2.iter().map(|b| b * b).sum::<f32>().sqrt();

    if norm_v1 == 0.0 || norm_v2 == 0.0 {
        0.0
    } else {
        dot_product / (norm_v1 * norm_v2)
    }
}

/// A simulated connection to a Vector DB (e.g., Qdrant, Pinecone, Milvus)
pub struct VectorDbConn {
    store: HashMap<String, RagVector>,
    dimension: usize,
}

impl VectorDbConn {
    pub fn new(dimension: usize) -> Self {
        Self {
            store: HashMap::new(),
            dimension,
        }
    }

    /// Insert or update a vector in the store
    pub fn upsert(&mut self, vector: RagVector) -> Result<(), String> {
        if vector.values.len() != self.dimension {
            return Err(format!(
                "Dimension mismatch. Expected {}, got {}",
                self.dimension,
                vector.values.len()
            ));
        }
        self.store.insert(vector.id.clone(), vector);
        Ok(())
    }

    /// Search for the top K most similar vectors using cosine similarity
    pub fn search(&self, query_vector: &[f32], top_k: usize) -> Result<Vec<(f32, RagVector)>, String> {
        if query_vector.len() != self.dimension {
            return Err(format!(
                "Dimension mismatch. Expected {}, got {}",
                self.dimension,
                query_vector.len()
            ));
        }

        let mut results: Vec<(f32, RagVector)> = self
            .store
            .values()
            .map(|v| {
                let sim = cosine_similarity(query_vector, &v.values);
                (sim, v.clone())
            })
            .collect();

        // Sort descending by similarity
        results.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        
        Ok(results.into_iter().take(top_k).collect())
    }
}

pub trait VectorScaffolder {
    fn scaffold(&self) -> String;
}

pub struct RagStackEngine;

impl RagStackEngine {
    pub fn new() -> Self {
        Self
    }
    
    pub fn run_action(&self, action: &str) -> String {
        let mut db = VectorDbConn::new(3);
        match action {
            "scaffold" | "init" => {
                let _ = db.upsert(RagVector::new("vec1", vec![1.0, 0.0, 0.0]).with_metadata("type", "test"));
                "Successfully scaffolded Vector DB connection with sample vectors.".to_string()
            }
            "search" => {
                let _ = db.upsert(RagVector::new("vec_target", vec![0.9, 0.1, 0.0]));
                let _ = db.upsert(RagVector::new("vec_other", vec![0.0, 1.0, 0.0]));
                let query = vec![1.0, 0.0, 0.0];
                match db.search(&query, 1) {
                    Ok(res) => format!("Search completed. Top match: {} with score {:.2}", res[0].1.id, res[0].0),
                    Err(e) => format!("Search failed: {}", e),
                }
            }
            _ => format!("RagStackEngine performed unknown action: {}", action),
        }
    }
}

impl VectorScaffolder for RagStackEngine {
    fn scaffold(&self) -> String {
        self.run_action("scaffold")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cosine_similarity() {
        let v1 = vec![1.0, 0.0, 0.0];
        let v2 = vec![1.0, 0.0, 0.0];
        assert_eq!(cosine_similarity(&v1, &v2), 1.0);

        let v3 = vec![0.0, 1.0, 0.0];
        assert_eq!(cosine_similarity(&v1, &v3), 0.0);
        
        let v4 = vec![0.5, 0.5, 0.0];
        assert!((cosine_similarity(&v1, &v4) - 0.707).abs() < 0.01);
    }

    #[test]
    fn test_vector_db_search() {
        let mut db = VectorDbConn::new(2);
        assert!(db.upsert(RagVector::new("a", vec![1.0, 0.0])).is_ok());
        assert!(db.upsert(RagVector::new("b", vec![0.0, 1.0])).is_ok());

        let res = db.search(&[1.0, 0.0], 1).unwrap();
        assert_eq!(res.len(), 1);
        assert_eq!(res[0].1.id, "a");
        assert_eq!(res[0].0, 1.0);
    }

    #[test]
    fn test_rag_engine_actions() {
        let engine = RagStackEngine::new();
        assert!(engine.run_action("scaffold").contains("Successfully"));
        assert!(engine.run_action("search").contains("vec_target"));
    }
}
