pub mod gemini;
pub mod ollama;
pub mod openai_compatible;
pub mod azure_openai;
pub mod router;

pub use gemini::GeminiProvider;
pub use ollama::{OllamaModelDetails, OllamaModelTag, OllamaProvider};
pub use openai_compatible::{AnthropicProvider, OpenAiCompatibleFlavor, OpenAiCompatibleProvider};
pub use azure_openai::{AzureAuth, AzureOpenAiProvider, AzureTokenUsage, DEFAULT_AZURE_API_VERSION};
pub use router::SharedProviderRouter;
