pub mod gemini;
pub mod ollama;

pub use gemini::GeminiProvider;
pub use ollama::{OllamaModelDetails, OllamaModelTag, OllamaProvider};
