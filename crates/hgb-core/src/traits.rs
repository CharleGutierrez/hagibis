use async_trait::async_trait;
use crate::error::Result;

#[async_trait]
pub trait HgbTool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    async fn execute(&self, arguments: serde_json::Value) -> Result<serde_json::Value>;
}

#[async_trait]
pub trait HgbProvider: Send + Sync {
    fn name(&self) -> &str;
    async fn complete(&self, prompt: &str, model: Option<&str>) -> Result<String>;
}
