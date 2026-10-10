use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::RwLock;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodeChunk {
    pub file_path: String,
    pub start_line: usize,
    pub end_line: usize,
    pub title: String,
    pub content: String,
    pub term_frequencies: HashMap<String, f32>,
    pub vector_magnitude: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SemanticSearchResult {
    pub file_path: String,
    pub start_line: usize,
    pub end_line: usize,
    pub title: String,
    pub score: f32,
    pub snippet: String,
}

pub struct SemanticCodebaseIndex {
    workspace_root: PathBuf,
    chunks: Arc<RwLock<Vec<CodeChunk>>>,
}

impl SemanticCodebaseIndex {
    pub fn new(workspace_root: PathBuf) -> Self {
        Self {
            workspace_root,
            chunks: Arc::new(RwLock::new(Vec::new())),
        }
    }

    /// Recursively indexes source code files in the workspace
    pub async fn index_workspace(&self) -> usize {
        let mut all_chunks = Vec::new();
        let mut dirs = vec![self.workspace_root.clone()];

        while let Some(current_dir) = dirs.pop() {
            if let Ok(entries) = std::fs::read_dir(&current_dir) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.is_dir() {
                        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
                        if name != "target" && name != ".git" && name != "node_modules" && !name.starts_with('.') {
                            dirs.push(path);
                        }
                    } else if path.is_file() {
                        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
                        if matches!(ext, "rs" | "zig" | "toml" | "json" | "js" | "ts" | "py" | "md" | "c" | "cpp" | "h") {
                            if let Ok(content) = std::fs::read_to_string(&path) {
                                let rel = path.strip_prefix(&self.workspace_root).unwrap_or(&path);
                                let file_chunks = chunk_file_content(&rel.to_string_lossy(), &content);
                                all_chunks.extend(file_chunks);
                            }
                        }
                    }
                }
            }
        }

        let count = all_chunks.len();
        let mut write_guard = self.chunks.write().await;
        *write_guard = all_chunks;
        count
    }

    /// Performs semantic cosine similarity search against indexed codebase chunks
    pub async fn search(&self, query: &str, limit: usize) -> Vec<SemanticSearchResult> {
        let query_terms = tokenize_and_weight(query);
        let query_magnitude: f32 = query_terms.values().map(|v| v * v).sum::<f32>().sqrt();

        if query_magnitude == 0.0 {
            return Vec::new();
        }

        let read_guard = self.chunks.read().await;
        let mut scored_results: Vec<(f32, &CodeChunk)> = Vec::new();

        for chunk in read_guard.iter() {
            if chunk.vector_magnitude == 0.0 {
                continue;
            }

            let mut dot_product = 0.0;
            for (term, q_weight) in &query_terms {
                if let Some(c_weight) = chunk.term_frequencies.get(term) {
                    dot_product += q_weight * c_weight;
                }
            }

            let cosine_sim = dot_product / (query_magnitude * chunk.vector_magnitude);
            if cosine_sim > 0.05 {
                scored_results.push((cosine_sim, chunk));
            }
        }

        scored_results.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));

        scored_results
            .into_iter()
            .take(limit)
            .map(|(score, chunk)| SemanticSearchResult {
                file_path: chunk.file_path.clone(),
                start_line: chunk.start_line,
                end_line: chunk.end_line,
                title: chunk.title.clone(),
                score,
                snippet: chunk.content.clone(),
            })
            .collect()
    }
}

/// Chunks content into semantic units (functions, structs, headers, or line blocks)
fn chunk_file_content(file_path: &str, content: &str) -> Vec<CodeChunk> {
    let mut chunks = Vec::new();
    let lines: Vec<&str> = content.lines().collect();

    if lines.is_empty() {
        return chunks;
    }

    // Chunk size between 20 and 50 lines with overlap
    let chunk_size = 35;
    let overlap = 8;
    let mut i = 0;

    while i < lines.len() {
        let end = (i + chunk_size).min(lines.len());
        let slice = &lines[i..end];
        let chunk_text = slice.join("\n");

        let first_line = slice.first().unwrap_or(&"").trim();
        let title = if first_line.starts_with("fn ") || first_line.starts_with("pub fn ") {
            first_line.to_string()
        } else if first_line.starts_with("struct ") || first_line.starts_with("pub struct ") {
            first_line.to_string()
        } else if first_line.starts_with("impl ") {
            first_line.to_string()
        } else {
            format!("{}:{}-{}", file_path, i + 1, end)
        };

        let term_freqs = tokenize_and_weight(&chunk_text);
        let magnitude: f32 = term_freqs.values().map(|v| v * v).sum::<f32>().sqrt();

        chunks.push(CodeChunk {
            file_path: file_path.to_string(),
            start_line: i + 1,
            end_line: end,
            title,
            content: chunk_text,
            term_frequencies: term_freqs,
            vector_magnitude: magnitude,
        });

        if end == lines.len() {
            break;
        }
        i += chunk_size.saturating_sub(overlap);
    }

    chunks
}

/// Tokenizes text into normalized term frequencies with sub-word identifier splitting
fn tokenize_and_weight(text: &str) -> HashMap<String, f32> {
    let mut counts: HashMap<String, f32> = HashMap::new();

    for raw_word in text.split(|c: char| !c.is_alphanumeric() && c != '_') {
        let lower = raw_word.to_lowercase();
        if lower.len() >= 2 && !is_stopword(&lower) {
            *counts.entry(lower.clone()).or_insert(0.0) += 1.0;

            // Split camelCase and snake_case tokens
            for part in raw_word.split('_') {
                let part_lower = part.to_lowercase();
                if part_lower.len() >= 2 && !is_stopword(&part_lower) && part_lower != lower {
                    *counts.entry(part_lower).or_insert(0.0) += 0.8;
                }
            }
        }
    }

    // Apply logarithm scaling
    for val in counts.values_mut() {
        *val = (1.0 + *val).ln();
    }

    counts
}

fn is_stopword(w: &str) -> bool {
    matches!(
        w,
        "the" | "and" | "for" | "with" | "this" | "that" | "from" | "are" | "was" | "were"
            | "has" | "have" | "had" | "will" | "would" | "can" | "could" | "should" | "not"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_semantic_index_and_search() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let sample_file = temp_dir.path().join("calculator.rs");
        std::fs::write(&sample_file, "pub fn add_numbers(a: i32, b: i32) -> i32 {\n    a + b\n}\n\npub fn multiply_numbers(x: i32, y: i32) -> i32 {\n    x * y\n}").expect("write");

        let index = SemanticCodebaseIndex::new(temp_dir.path().to_path_buf());
        let indexed_count = index.index_workspace().await;
        assert!(indexed_count > 0);

        let results = index.search("add_numbers sum addition", 5).await;
        assert!(!results.is_empty());
        assert_eq!(results[0].file_path, "calculator.rs");
        assert!(results[0].score > 0.0);
    }
}

