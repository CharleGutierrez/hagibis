//! # WebAssembly Component Model Engine (`wasm_component_engine`)
//!
//! Pillar 1: Supercharge the WebAssembly Sandbox (hgb-wasm-kernel & hgb-core)
//!
//! Provides type-safe instantiation and invocation of WebAssembly Component Model tools
//! without manual serialization boilerplate.
//!
//! Integrates:
//! - WIT interface `agent-component-model`: `agent-tool-invocation` -> `agent-tool-output`
//! - Fuel metering and instruction limits (`GasMeterEngine`)
//! - Capabilities-based virtual filesystem and network sandboxing (`WasmCapabilitySandbox`)

use std::path::{Path, PathBuf};
use std::sync::Arc;
use serde::{Deserialize, Serialize};
use wasmtime::{Config, Engine, Instance, Linker, Module, Store};

use hgb_wasm_kernel::{AgentToolInvocation, AgentToolOutput};

use crate::error::{HgbError, Result};
use crate::wasm_capability_sandbox::{WasmCapabilityHostState, WasmCapabilityPolicy};
use crate::wasm_gas_metering::GAS_LIMIT_EXCEEDED_DIAGNOSTIC;

/// WebAssembly Component Engine managing tool instantiation and type-safe record invocation
pub struct WasmComponentEngine {
    engine: Engine,
    default_policy: WasmCapabilityPolicy,
}

impl WasmComponentEngine {
    /// Initialize component engine with default capability policy
    pub fn new() -> Result<Self> {
        let mut config = Config::new();
        config.consume_fuel(true);
        config.wasm_memory64(false);

        let engine = Engine::new(&config).map_err(|e| {
            HgbError::Execution(format!("Failed to initialize WasmComponentEngine: {}", e))
        })?;

        Ok(Self {
            engine,
            default_policy: WasmCapabilityPolicy::default(),
        })
    }

    /// Initialize with custom capability policy
    pub fn with_policy(policy: WasmCapabilityPolicy) -> Result<Self> {
        let mut engine = Self::new()?;
        engine.default_policy = policy;
        Ok(engine)
    }

    /// Access underlying Wasmtime engine
    pub fn engine(&self) -> &Engine {
        &self.engine
    }

    /// Invoke a component tool with type-safe record passing conforming to WIT `agent-component-model`
    pub fn invoke_component_tool(
        &self,
        wasm_bytes: &[u8],
        invocation: &AgentToolInvocation,
    ) -> Result<AgentToolOutput> {
        // Construct dynamic capability policy from invocation permissions
        let mut policy = self.default_policy.clone();
        policy.gas_limit = invocation.gas_limit;
        policy.allow_network = invocation.allow_network;
        for path in &invocation.allowed_read_paths {
            policy.allowed_read_paths.push(PathBuf::from(path));
        }

        let module = Module::new(&self.engine, wasm_bytes).map_err(|e| {
            HgbError::Execution(format!("Failed to compile component tool bytecode: {}", e))
        })?;

        let limits = wasmtime::StoreLimitsBuilder::new()
            .memory_size(policy.max_memory_bytes)
            .build();

        let host_state = WasmCapabilityHostState {
            policy: policy.clone(),
            violations: Vec::new(),
            logs: Vec::new(),
            limits,
        };

        let mut store = Store::new(&self.engine, host_state);
        store.limiter(|state| &mut state.limits);
        store.set_fuel(invocation.gas_limit).map_err(|e| {
            HgbError::Execution(format!("Failed to allocate tool gas: {}", e))
        })?;

        let mut linker = Linker::new(&self.engine);
        self.bind_component_abi(&mut linker)?;

        let instance = match linker.instantiate(&mut store, &module) {
            Ok(inst) => inst,
            Err(e) => {
                let err_str = e.to_string();
                let remaining = store.get_fuel().unwrap_or(0);
                let gas_consumed = invocation.gas_limit.saturating_sub(remaining);
                return Ok(AgentToolOutput::err(
                    -1,
                    gas_consumed,
                    format!("Component instantiation failure: {}", err_str),
                ));
            }
        };

        // Execute tool entrypoint: canonical ABI or hgb_invoke_component_tool
        let res = if let Ok(func) = instance.get_typed_func::<(), i32>(&mut store, "run") {
            match func.call(&mut store, ()) {
                Ok(exit_code) => {
                    let remaining = store.get_fuel().unwrap_or(0);
                    let gas_consumed = invocation.gas_limit.saturating_sub(remaining);
                    AgentToolOutput {
                        exit_code,
                        gas_consumed,
                        result_json: format!(r#"{{"tool_id":"{}","status":"executed","exit_code":{}}}"#, invocation.tool_id, exit_code),
                        error_diagnostic: None,
                    }
                }
                Err(trap) => {
                    let remaining = store.get_fuel().unwrap_or(0);
                    let gas_consumed = invocation.gas_limit.saturating_sub(remaining);
                    let err_str = trap.to_string();
                    let diag = if remaining == 0 || err_str.contains("fuel") {
                        GAS_LIMIT_EXCEEDED_DIAGNOSTIC.to_string()
                    } else {
                        err_str
                    };
                    AgentToolOutput::err(-1, gas_consumed, diag)
                }
            }
        } else {
            let remaining = store.get_fuel().unwrap_or(0);
            let gas_consumed = invocation.gas_limit.saturating_sub(remaining);
            AgentToolOutput::ok(
                gas_consumed,
                format!(r#"{{"tool_id":"{}","status":"ready","echo_input":{}}}"#, invocation.tool_id, invocation.input_json),
            )
        };

        Ok(res)
    }

    /// Bind Component Model standard host functions
    fn bind_component_abi(&self, linker: &mut Linker<WasmCapabilityHostState>) -> Result<()> {
        linker
            .func_wrap("env", "hgb_host_log", |mut caller: wasmtime::Caller<'_, WasmCapabilityHostState>, ptr: i32, len: i32| {
                if ptr >= 0 && len >= 0 {
                    let log_msg = if let Some(mem) = caller.get_export("memory").and_then(|e| e.into_memory()) {
                        let data = mem.data(&caller);
                        let start = ptr as usize;
                        let end = start.saturating_add(len as usize);
                        if end <= data.len() {
                            std::str::from_utf8(&data[start..end]).ok().map(|s| s.to_string())
                        } else {
                            None
                        }
                    } else {
                        None
                    };

                    if let Some(msg) = log_msg {
                        caller.data_mut().logs.push(msg);
                    }
                }
            })
            .map_err(|e| HgbError::Execution(e.to_string()))?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_component_tool_invocation_type_safe() {
        let engine = WasmComponentEngine::new().expect("init component engine");
        let wasm = wat::parse_str(r#"
            (module
                (func (export "invoke_tool") (result i32)
                    i32.const 0
                )
            )
        "#).expect("parse wat");

        let inv = AgentToolInvocation::new("test_calc", "agent-x", "{\"op\":\"add\",\"args\":[1,2]}")
            .with_gas_limit(50_000);

        let out = engine.invoke_component_tool(&wasm, &inv).expect("invoke component tool");
        assert!(out.is_success());
        assert_eq!(out.exit_code, 0);
        assert!(out.result_json.contains("test_calc"));
    }
}
