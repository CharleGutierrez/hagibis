use serde::{Deserialize, Serialize};
use mlua::Lua;
use std::fs;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginContext {
    pub language: String,
    pub script_path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginExecutionResult {
    pub success: bool,
    pub output: String,
    pub execution_time_ms: u64,
}

pub struct PluginFabric;

impl PluginFabric {
    pub fn new() -> Self {
        Self
    }

    pub fn execute_plugin(&self, ctx: &PluginContext) -> PluginExecutionResult {
        let start = std::time::Instant::now();
        
        if ctx.language.to_lowercase() != "lua" {
            return PluginExecutionResult {
                success: false,
                output: format!("Unsupported language: {}", ctx.language),
                execution_time_ms: start.elapsed().as_millis() as u64,
            };
        }

        let script = match fs::read_to_string(&ctx.script_path) {
            Ok(s) => s,
            Err(e) => return PluginExecutionResult {
                success: false,
                output: format!("Failed to read script {}: {}", ctx.script_path, e),
                execution_time_ms: start.elapsed().as_millis() as u64,
            },
        };

        let lua = Lua::new();
        
        let result: mlua::Result<String> = lua.load(&script).eval();

        let (success, output) = match result {
            Ok(s) => (true, s),
            Err(e) => {
                // If it doesn't return a string, let's just execute as chunk
                match lua.load(&script).exec() {
                    Ok(_) => (true, "Plugin executed successfully".to_string()),
                    Err(exec_err) => (false, format!("Lua Error: {}", exec_err)),
                }
            },
        };

        PluginExecutionResult {
            success,
            output,
            execution_time_ms: start.elapsed().as_millis() as u64,
        }
    }
}
