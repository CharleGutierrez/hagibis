pub trait VectorScaffolder {
    fn scaffold(&self) -> String;
}
pub struct AiNativeRag;
impl VectorScaffolder for AiNativeRag {
    fn scaffold(&self) -> String {
        "Scaffolding AI-native RAG stack".to_string()
    }
}
