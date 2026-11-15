//! # Pillar 4: Dynamic WebAssembly MCP Plugin Loader & Hot-Reloading Socket Subsystem
//!
//! Decouples static tool definitions in `hagibis.mcp.json` into a live dynamic plugin ecosystem
//! rooted in `plugins/wasm/`:
//! 1. Discovers and compiles `.wasm` / `.wat` modules placed in `plugins/wasm/`.
//! 2. Parses accompanying WebAssembly Interface Type (`.wit`) specifications to synthesize typed JSON schemas.
//! 3. Dynamically registers tools into thread-safe `DynamicSkillRegistry` without restarting the daemon.
//! 4. Emits `notifications/tools/list_changed` JSON-RPC notifications across active MCP SSE client sockets.
//! 5. Dispatches tool invocations directly to sandboxed WASM runners or sovereign native handlers.
//! 6. Atomically reflects updates into `hagibis.mcp.json`.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::error::{HgbError, Result};
use crate::dynamic_skill_mcp_hot_reloader::{
    DynamicSkillRegistry, DynamicSkillTool, WitFunction, WitParam, WitSpec,
};

/// Represents a dynamic WASM plugin discovered in `plugins/wasm/`
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DynamicWasmPluginDescriptor {
    pub plugin_name: String,
    pub wasm_file: PathBuf,
    pub wit_file: Option<PathBuf>,
    pub wasm_size_bytes: usize,
    pub blake3_hash: String,
    pub tools_synthesized: Vec<String>,
    pub loaded_at_epoch_ms: u64,
}

/// Dynamic WASM Plugin Loader for `plugins/wasm/`
pub struct DynamicWasmPluginLoader {
    plugins_dir: PathBuf,
    registry: Arc<DynamicSkillRegistry>,
    mcp_json_path: Option<PathBuf>,
    sync_mcp_json: bool,
    loaded_plugins: Arc<RwLock<HashMap<String, DynamicWasmPluginDescriptor>>>,
}

impl DynamicWasmPluginLoader {
    /// Construct a new plugin loader targeting `plugins/wasm/`
    pub fn new<P: AsRef<Path>>(
        plugins_dir: P,
        registry: Arc<DynamicSkillRegistry>,
        mcp_json_path: Option<PathBuf>,
        sync_mcp_json: bool,
    ) -> Self {
        Self {
            plugins_dir: plugins_dir.as_ref().to_path_buf(),
            registry,
            mcp_json_path,
            sync_mcp_json,
            loaded_plugins: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Access inner registry
    pub fn registry(&self) -> &Arc<DynamicSkillRegistry> {
        &self.registry
    }

    /// Scan `plugins/wasm/`, compile/validate bytecode, parse WIT, and register tools
    pub fn scan_and_register_plugins(&self) -> Result<Vec<DynamicWasmPluginDescriptor>> {
        let mut loaded = Vec::new();

        if !self.plugins_dir.exists() {
            fs::create_dir_all(&self.plugins_dir)?;
            return Ok(loaded);
        }

        let entries = fs::read_dir(&self.plugins_dir)?;
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file() {
                let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
                if ext == "wasm" || ext == "wat" {
                    match self.load_single_plugin(&path) {
                        Ok(desc) => loaded.push(desc),
                        Err(e) => {
                            tracing::warn!("Failed to load plugin at {}: {}", path.display(), e);
                        }
                    }
                }
            }
        }

        // Broadcast MCP tool list change notification over SSE
        self.registry.broadcast_tools_list_changed();

        // Sync to hagibis.mcp.json if enabled
        if self.sync_mcp_json {
            if let Some(ref mcp_path) = self.mcp_json_path {
                let _ = self.registry.reflect_to_mcp_json(mcp_path);
            }
        }

        Ok(loaded)
    }

    /// Load or reload a single `.wasm` or `.wat` plugin
    pub fn load_single_plugin(&self, wasm_path: &Path) -> Result<DynamicWasmPluginDescriptor> {
        let file_stem = wasm_path
            .file_stem()
            .and_then(|s| s.to_str())
            .ok_or_else(|| HgbError::Execution("Invalid plugin file path".into()))?;

        let plugin_name = file_stem.to_string();
        let raw_bytes = fs::read(wasm_path)?;

        // If file is WAT, translate to WASM bytecode representation
        let bytecode = if raw_bytes.starts_with(b"(module") || wasm_path.extension().and_then(|s| s.to_str()) == Some("wat") {
            let wat_str = std::str::from_utf8(&raw_bytes).map_err(|e| HgbError::Execution(e.to_string()))?;
            wat::parse_str(wat_str).map_err(|e| HgbError::Execution(format!("WAT parse error: {}", e)))?
        } else {
            raw_bytes
        };

        // Validate WASM binary header (\0asm) or WAT module
        if bytecode.len() < 4 || &bytecode[0..4] != b"\x00asm" {
            if !bytecode.starts_with(b"(module") {
                return Err(HgbError::Execution(format!(
                    "File '{}' is not a valid WebAssembly binary or WAT module",
                    wasm_path.display()
                )));
            }
        }

        let blake3_hash = blake3::hash(&bytecode).to_hex().to_string();
        let wasm_size_bytes = bytecode.len();

        // Check for accompanying .wit interface file
        let wit_candidate = wasm_path.with_extension("wit");
        let wit_file = if wit_candidate.exists() {
            Some(wit_candidate)
        } else {
            let wit_in_subdir = wasm_path
                .parent()
                .unwrap_or(Path::new("."))
                .join("wit")
                .join(format!("{}.wit", plugin_name));
            if wit_in_subdir.exists() {
                Some(wit_in_subdir)
            } else {
                None
            }
        };

        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);

        let mut synthesized_tools = Vec::new();

        if let Some(ref wit_path) = wit_file {
            let wit_content = fs::read_to_string(wit_path)?;
            let spec = WitSpec::parse(&wit_content);

            for func in &spec.functions {
                let func_slug = func.name.replace('-', "_");
                let tool_name = format!("plugin_{}_{}", plugin_name, func_slug);
                let description = func.doc.clone().unwrap_or_else(|| {
                    format!("WASM Plugin '{}' operation: {}", plugin_name, func.name)
                });
                let input_schema = spec.build_input_schema(func);

                let dynamic_tool = DynamicSkillTool {
                    tool_name: tool_name.clone(),
                    skill_name: format!("plugin_{}", plugin_name),
                    function_name: Some(func.name.clone()),
                    interface_name: func.interface.clone(),
                    description,
                    input_schema,
                    wit_path: Some(wit_path.clone()),
                    skill_dir: self.plugins_dir.clone(),
                    wasm_path: Some(wasm_path.to_path_buf()),
                    registered_at: now_ms,
                    is_active: true,
                };

                self.registry.register_tool(dynamic_tool);
                synthesized_tools.push(tool_name);
            }
        }

        // If no functions were synthesized from WIT, register default invocation entrypoint
        if synthesized_tools.is_empty() {
            let tool_name = format!("plugin_{}", plugin_name);
            let default_schema = json!({
                "type": "object",
                "properties": {
                    "input": {
                        "type": "string",
                        "description": "JSON arguments or string payload passed to WASM plugin"
                    }
                }
            });

            let dynamic_tool = DynamicSkillTool {
                tool_name: tool_name.clone(),
                skill_name: format!("plugin_{}", plugin_name),
                function_name: Some("hgb_tool_invoke".to_string()),
                interface_name: None,
                description: format!("Dynamic WebAssembly MCP plugin: {}", plugin_name),
                input_schema: default_schema,
                wit_path: None,
                skill_dir: self.plugins_dir.clone(),
                wasm_path: Some(wasm_path.to_path_buf()),
                registered_at: now_ms,
                is_active: true,
            };

            self.registry.register_tool(dynamic_tool);
            synthesized_tools.push(tool_name);
        }

        let descriptor = DynamicWasmPluginDescriptor {
            plugin_name: plugin_name.clone(),
            wasm_file: wasm_path.to_path_buf(),
            wit_file,
            wasm_size_bytes,
            blake3_hash,
            tools_synthesized: synthesized_tools,
            loaded_at_epoch_ms: now_ms,
        };

        self.loaded_plugins
            .write()
            .unwrap()
            .insert(plugin_name, descriptor.clone());

        Ok(descriptor)
    }

    /// List all currently loaded dynamic WASM plugins
    pub fn list_loaded_plugins(&self) -> Vec<DynamicWasmPluginDescriptor> {
        self.loaded_plugins.read().unwrap().values().cloned().collect()
    }
}

// ==============================================================================
// Unit Tests
// ==============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_dynamic_wasm_plugin_discovery_and_wit_synthesis() {
        let temp = tempdir().expect("tempdir");
        let plugins_dir = temp.path().join("plugins").join("wasm");
        fs::create_dir_all(&plugins_dir).expect("create plugins/wasm");

        let mcp_json = temp.path().join("hagibis.mcp.json");
        fs::write(&mcp_json, r#"{"tools": {}}"#).expect("write initial mcp.json");

        // 1. Create a WAT plugin
        let wat_content = r#"(module
  (func $add (export "add") (param $a i32) (param $b i32) (result i32)
    local.get $a
    local.get $b
    i32.add
  )
)"#;
        let wasm_file = plugins_dir.join("math_engine.wat");
        fs::write(&wasm_file, wat_content).expect("write wat");

        // 2. Create an accompanying WIT file
        let wit_content = r#"package hagibis:math@0.1.0;

interface math-ops {
    /// Add two signed 32-bit integers
    add: func(a: s32, b: s32) -> s32;
}

world math-engine {
    export math-ops;
}
"#;
        let wit_file = plugins_dir.join("math_engine.wit");
        fs::write(&wit_file, wit_content).expect("write wit");

        // 3. Initialize loader
        let registry = Arc::new(DynamicSkillRegistry::new());
        let loader = DynamicWasmPluginLoader::new(&plugins_dir, registry.clone(), Some(mcp_json.clone()), true);

        // 4. Scan and register
        let loaded = loader.scan_and_register_plugins().expect("scan plugins");
        assert_eq!(loaded.len(), 1, "Must discover math_engine plugin");

        let desc = &loaded[0];
        assert_eq!(desc.plugin_name, "math_engine");
        assert!(desc.tools_synthesized.contains(&"plugin_math_engine_add".to_string()));

        // 5. Verify registered in DynamicSkillRegistry
        let tool = registry.get_tool("plugin_math_engine_add").expect("registered tool");
        assert_eq!(tool.skill_name, "plugin_math_engine");
        assert_eq!(tool.function_name.as_deref(), Some("add"));
        assert!(tool.description.contains("Add two signed 32-bit integers"));

        // Verify JSON Schema matches parameters
        let schema = &tool.input_schema;
        assert_eq!(schema["type"], "object");
        let props = &schema["properties"];
        assert!(props.get("a").is_some(), "Property 'a' must exist");
        assert!(props.get("b").is_some(), "Property 'b' must exist");

        // 6. Verify reflected in hagibis.mcp.json
        let updated_mcp_str = fs::read_to_string(&mcp_json).expect("read mcp json");
        assert!(updated_mcp_str.contains("plugin_math_engine_add"), "mcp.json must include dynamic plugin");
    }

    #[test]
    fn test_dynamic_wasm_plugin_without_wit_fallback_schema() {
        let temp = tempdir().expect("tempdir");
        let plugins_dir = temp.path().join("plugins").join("wasm");
        fs::create_dir_all(&plugins_dir).expect("create plugins/wasm");

        // Plugin without WIT
        let wat_content = r#"(module
  (func $hgb_tool_invoke (export "hgb_tool_invoke") (result i32)
    i32.const 42
  )
)"#;
        let wasm_file = plugins_dir.join("standalone_kernel.wat");
        fs::write(&wasm_file, wat_content).expect("write wat");

        let registry = Arc::new(DynamicSkillRegistry::new());
        let loader = DynamicWasmPluginLoader::new(&plugins_dir, registry.clone(), None, false);

        let loaded = loader.scan_and_register_plugins().expect("scan");
        assert_eq!(loaded.len(), 1);

        let tool = registry.get_tool("plugin_standalone_kernel").expect("fallback tool");
        assert_eq!(tool.tool_name, "plugin_standalone_kernel");
        assert!(tool.input_schema["properties"].get("input").is_some());
    }
}
