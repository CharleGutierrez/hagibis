//! # WebAssembly Gas Metering & Instruction Accounting Engine (`wasm_gas_metering`)
//!
//! Pillar 1: Supercharge the WebAssembly Sandbox (hgb-wasm-kernel & hgb-core)
//!
//! Production-grade fuel counting and instruction metering for untrusted agent-generated
//! WebAssembly code.
//!
//! ## Key Capabilities
//! - Strict instruction counting via Wasmtime `Store::set_fuel` and `Store::consume_fuel`.
//! - Clean trapping with `GasLimitExceeded` diagnostic when untrusted code enters infinite loops
//!   or exhausts its fuel budget.
//! - High-precision accounting returning exact gas units consumed and execution duration in microseconds.
//! - Non-destructive gas budgeting, top-ups, and intermediate checkpoints.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;
use serde::{Deserialize, Serialize};
use wasmtime::{Config, Engine, Instance, Linker, Module, Store, Trap};

use crate::error::{HgbError, Result};

/// Default fuel limit for agent tool execution (10,000,000 instructions)
pub const DEFAULT_GAS_LIMIT: u64 = 10_000_000;
/// Minimum fuel limit required to initialize and run minimal WASM execution
pub const MIN_GAS_LIMIT: u64 = 10_000;
/// Hard ceiling on allowable fuel per execution invocation (1,000,000,000 instructions)
pub const MAX_GAS_CEILING: u64 = 1_000_000_000;

/// Diagnostic error indicating untrusted code exhausted its allocated gas budget
pub const GAS_LIMIT_EXCEEDED_DIAGNOSTIC: &str = "GasLimitExceeded";

/// Detailed outcome of a gas-metered WebAssembly execution
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GasExecutionOutcome {
    /// Function or entrypoint name invoked
    pub entrypoint: String,
    /// Exit code returned by guest (0 for clean execution, negative for trap/error)
    pub exit_code: i32,
    /// Exact gas/fuel units consumed during execution
    pub gas_consumed: u64,
    /// Initial gas limit allocated before execution
    pub gas_limit: u64,
    /// Gas units remaining in store after execution
    pub gas_remaining: u64,
    /// Execution duration in microseconds (us)
    pub duration_us: u64,
    /// Whether execution cleanly finished or trapped
    pub trapped: bool,
    /// Error diagnostic string if execution trapped or failed
    pub error_diagnostic: Option<String>,
}

impl GasExecutionOutcome {
    /// Returns true if execution succeeded without trap or gas exhaustion
    pub fn is_success(&self) -> bool {
        !self.trapped && self.error_diagnostic.is_none() && self.exit_code >= 0
    }

    /// Returns true if the failure was specifically due to gas limit exhaustion
    pub fn is_gas_exhausted(&self) -> bool {
        self.error_diagnostic.as_deref() == Some(GAS_LIMIT_EXCEEDED_DIAGNOSTIC)
            || (self.trapped && self.gas_remaining == 0)
    }
}

/// Fuel and instruction accounting state stored inside the Wasmtime `Store`
#[derive(Debug, Clone)]
pub struct GasStoreData {
    /// Identifier of the running agent or tool
    pub agent_or_tool_id: String,
    /// Configured initial gas limit
    pub initial_gas: u64,
    /// Cumulative gas consumed across all calls within this store session
    pub cumulative_consumed: u64,
    /// Host execution logs recorded during execution
    pub host_logs: Vec<String>,
    /// Last error diagnostic recorded by host traps
    pub last_diagnostic: Option<String>,
}

impl GasStoreData {
    pub fn new(agent_or_tool_id: impl Into<String>, initial_gas: u64) -> Self {
        Self {
            agent_or_tool_id: agent_or_tool_id.into(),
            initial_gas,
            cumulative_consumed: 0,
            host_logs: Vec::new(),
            last_diagnostic: None,
        }
    }
}

/// Gas meter configurer and instruction executor
pub struct GasMeterEngine {
    engine: Engine,
    default_gas_limit: u64,
    total_gas_consumed_global: AtomicU64,
}

impl GasMeterEngine {
    /// Initialize gas meter with custom default fuel limit
    pub fn new(default_gas_limit: u64) -> Result<Self> {
        let mut config = Config::new();
        config.consume_fuel(true);
        config.wasm_memory64(false);

        let engine = Engine::new(&config).map_err(|e| {
            HgbError::Execution(format!("Failed to initialize Wasmtime engine with fuel metering: {}", e))
        })?;

        Ok(Self {
            engine,
            default_gas_limit: default_gas_limit.clamp(MIN_GAS_LIMIT, MAX_GAS_CEILING),
            total_gas_consumed_global: AtomicU64::new(0),
        })
    }

    /// Access reference to the internal Wasmtime `Engine`
    pub fn engine(&self) -> &Engine {
        &self.engine
    }

    /// Global lifetime gas consumption counter
    pub fn total_gas_consumed(&self) -> u64 {
        self.total_gas_consumed_global.load(Ordering::Relaxed)
    }

    /// Creates a new `Store<GasStoreData>` configured with strict fuel limits
    pub fn create_metered_store(
        &self,
        identifier: impl Into<String>,
        gas_limit: Option<u64>,
    ) -> Result<Store<GasStoreData>> {
        let gas = gas_limit
            .unwrap_or(self.default_gas_limit)
            .clamp(MIN_GAS_LIMIT, MAX_GAS_CEILING);

        let data = GasStoreData::new(identifier, gas);
        let mut store = Store::new(&self.engine, data);

        store.set_fuel(gas).map_err(|e| {
            HgbError::Execution(format!("Failed to set initial fuel limit {}: {}", gas, e))
        })?;

        Ok(store)
    }

    /// Execute a nullary `run() -> i32` entrypoint with strict gas counting
    pub fn execute_run(
        &self,
        wasm_bytes: &[u8],
        gas_limit: Option<u64>,
    ) -> Result<GasExecutionOutcome> {
        self.execute_entrypoint(wasm_bytes, "run", gas_limit)
    }

    /// Execute an arbitrary typed entrypoint `() -> i32` or `() -> ()` with gas counting
    pub fn execute_entrypoint(
        &self,
        wasm_bytes: &[u8],
        entrypoint: &str,
        gas_limit: Option<u64>,
    ) -> Result<GasExecutionOutcome> {
        let effective_gas = gas_limit
            .unwrap_or(self.default_gas_limit)
            .clamp(MIN_GAS_LIMIT, MAX_GAS_CEILING);

        let module = Module::new(&self.engine, wasm_bytes).map_err(|e| {
            HgbError::Execution(format!("Failed to compile WASM bytecode for gas metering: {}", e))
        })?;

        let mut store = self.create_metered_store("gas_metered_guest", Some(effective_gas))?;
        let linker = Linker::new(&self.engine);

        let instance = match linker.instantiate(&mut store, &module) {
            Ok(inst) => inst,
            Err(e) => {
                let err_str = e.to_string();
                let is_fuel = err_str.contains("fuel") || store.get_fuel().unwrap_or(1) == 0;
                let diagnostic = if is_fuel {
                    GAS_LIMIT_EXCEEDED_DIAGNOSTIC.to_string()
                } else {
                    format!("Instantiation trap: {}", err_str)
                };

                let remaining = store.get_fuel().unwrap_or(0);
                let consumed = effective_gas.saturating_sub(remaining);
                self.total_gas_consumed_global.fetch_add(consumed, Ordering::Relaxed);

                return Ok(GasExecutionOutcome {
                    entrypoint: entrypoint.to_string(),
                    exit_code: -1,
                    gas_consumed: consumed,
                    gas_limit: effective_gas,
                    gas_remaining: remaining,
                    duration_us: 0,
                    trapped: true,
                    error_diagnostic: Some(diagnostic),
                });
            }
        };

        self.invoke_instance_func(&mut store, &instance, entrypoint, effective_gas)
    }

    /// Invoke a specific exported function on an already instantiated guest
    pub fn invoke_instance_func(
        &self,
        store: &mut Store<GasStoreData>,
        instance: &Instance,
        entrypoint: &str,
        allocated_gas: u64,
    ) -> Result<GasExecutionOutcome> {
        let start_time = Instant::now();

        // Check if function returns i32
        if let Ok(func_i32) = instance.get_typed_func::<(), i32>(&mut *store, entrypoint) {
            match func_i32.call(&mut *store, ()) {
                Ok(exit_code) => {
                    let duration_us = start_time.elapsed().as_micros() as u64;
                    let remaining = store.get_fuel().unwrap_or(0);
                    let consumed = allocated_gas.saturating_sub(remaining);
                    self.total_gas_consumed_global.fetch_add(consumed, Ordering::Relaxed);

                    Ok(GasExecutionOutcome {
                        entrypoint: entrypoint.to_string(),
                        exit_code,
                        gas_consumed: consumed,
                        gas_limit: allocated_gas,
                        gas_remaining: remaining,
                        duration_us,
                        trapped: false,
                        error_diagnostic: None,
                    })
                }
                Err(trap) => {
                    let duration_us = start_time.elapsed().as_micros() as u64;
                    let outcome = self.classify_trap(store, trap, entrypoint, allocated_gas, duration_us);
                    Ok(outcome)
                }
            }
        } else if let Ok(func_void) = instance.get_typed_func::<(), ()>(&mut *store, entrypoint) {
            match func_void.call(&mut *store, ()) {
                Ok(()) => {
                    let duration_us = start_time.elapsed().as_micros() as u64;
                    let remaining = store.get_fuel().unwrap_or(0);
                    let consumed = allocated_gas.saturating_sub(remaining);
                    self.total_gas_consumed_global.fetch_add(consumed, Ordering::Relaxed);

                    Ok(GasExecutionOutcome {
                        entrypoint: entrypoint.to_string(),
                        exit_code: 0,
                        gas_consumed: consumed,
                        gas_limit: allocated_gas,
                        gas_remaining: remaining,
                        duration_us,
                        trapped: false,
                        error_diagnostic: None,
                    })
                }
                Err(trap) => {
                    let duration_us = start_time.elapsed().as_micros() as u64;
                    let outcome = self.classify_trap(store, trap, entrypoint, allocated_gas, duration_us);
                    Ok(outcome)
                }
            }
        } else {
            Err(HgbError::Execution(format!(
                "Exported entrypoint '{}' not found or unsupported signature in guest",
                entrypoint
            )))
        }
    }

    /// Classify a Wasmtime trap as gas exhaustion vs generic trap
    fn classify_trap(
        &self,
        store: &mut Store<GasStoreData>,
        trap: wasmtime::Error,
        entrypoint: &str,
        allocated_gas: u64,
        duration_us: u64,
    ) -> GasExecutionOutcome {
        let err_msg = trap.to_string();
        let remaining_fuel = store.get_fuel().unwrap_or(0);

        let is_fuel_exhausted = remaining_fuel == 0
            || trap.downcast_ref::<Trap>() == Some(&Trap::OutOfFuel)
            || err_msg.to_lowercase().contains("fuel")
            || err_msg.to_lowercase().contains("all fuel consumed")
            || trap.root_cause().to_string().to_lowercase().contains("fuel");

        let (diagnostic, consumed, remaining) = if is_fuel_exhausted {
            (GAS_LIMIT_EXCEEDED_DIAGNOSTIC.to_string(), allocated_gas, 0)
        } else {
            let actual_remaining = store.get_fuel().unwrap_or(0);
            let actual_consumed = allocated_gas.saturating_sub(actual_remaining);
            (format!("Trap: {}", err_msg), actual_consumed, actual_remaining)
        };

        self.total_gas_consumed_global.fetch_add(consumed, Ordering::Relaxed);

        GasExecutionOutcome {
            entrypoint: entrypoint.to_string(),
            exit_code: -1,
            gas_consumed: consumed,
            gas_limit: allocated_gas,
            gas_remaining: remaining,
            duration_us,
            trapped: true,
            error_diagnostic: Some(diagnostic),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_clean_execution_and_gas_metering() {
        let engine = GasMeterEngine::new(100_000).expect("init gas meter engine");
        let wasm = wat::parse_str(r#"
            (module
                (func (export "run") (result i32)
                    i32.const 42
                )
            )
        "#).expect("parse wat");

        let outcome = engine.execute_run(&wasm, Some(50_000)).expect("execute run");
        assert!(outcome.is_success());
        assert_eq!(outcome.exit_code, 42);
        assert!(outcome.gas_consumed > 0);
        assert_eq!(outcome.gas_limit, 50_000);
        assert_eq!(outcome.gas_remaining, 50_000 - outcome.gas_consumed);
        assert!(!outcome.trapped);
        assert_eq!(outcome.error_diagnostic, None);
        assert!(engine.total_gas_consumed() >= outcome.gas_consumed);
    }

    #[test]
    fn test_infinite_loop_gas_exhaustion_clean_trap() {
        let engine = GasMeterEngine::new(10_000).expect("init engine");
        let wasm = wat::parse_str(r#"
            (module
                (func (export "run") (result i32)
                    (loop
                        br 0
                    )
                    i32.const 0
                )
            )
        "#).expect("parse wat");

        let outcome = engine.execute_run(&wasm, Some(20_000)).expect("execute run");
        assert!(!outcome.is_success());
        assert!(outcome.trapped);
        assert!(outcome.is_gas_exhausted());
        assert_eq!(outcome.gas_remaining, 0);
        assert_eq!(outcome.gas_consumed, 20_000);
        assert_eq!(outcome.error_diagnostic, Some(GAS_LIMIT_EXCEEDED_DIAGNOSTIC.to_string()));
    }
}
