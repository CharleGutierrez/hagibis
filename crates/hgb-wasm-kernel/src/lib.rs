//! # Hagibis WebAssembly Guest Kernel SDK (`hgb-wasm-kernel`)
//!
//! Guest runtime SDK and standard ABI bindings for compiling sandboxed agent tools
//! and capability-restricted plugins to WebAssembly.
//!
//! ## Architecture
//! - Standard JSON-RPC ABI: `hgb_tool_manifest` and `hgb_tool_invoke`
//! - Capability-safe host syscall wrappers:
//!   * `fs_read(path)` -> bounded filesystem read
//!   * `fs_write(path, data)` -> bounded filesystem write
//!   * `net_fetch(url)` -> capability-checked network access
//!   * `host_log(message)` -> safe structured host logging
//! - Memory management helpers: `alloc`, `dealloc`
//! - Declarative tool export macro: `export_hgb_tool!`

use serde::{Deserialize, Serialize};

/// Tool input/output manifest exported by the WASM guest
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PluginManifest {
    pub name: String,
    pub version: String,
    pub description: String,
    pub author: Option<String>,
    pub capabilities_required: Vec<String>,
    pub input_schema: serde_json::Value,
}

impl Default for PluginManifest {
    fn default() -> Self {
        Self {
            name: "unnamed_wasm_tool".to_string(),
            version: "0.1.0".to_string(),
            description: "Hagibis Sandboxed WebAssembly Tool".to_string(),
            author: None,
            capabilities_required: Vec::new(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "input": { "type": "string" }
                }
            }),
        }
    }
}

/// Dynamic Tool Invocation Request received from host
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolInvocationRequest {
    pub tool_name: String,
    pub arguments: serde_json::Value,
}

/// Dynamic Tool Invocation Response returned to host
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolInvocationResponse {
    pub success: bool,
    pub result: serde_json::Value,
    pub error: Option<String>,
}

impl ToolInvocationResponse {
    pub fn ok(result: serde_json::Value) -> Self {
        Self {
            success: true,
            result,
            error: None,
        }
    }

    pub fn err(message: impl Into<String>) -> Self {
        Self {
            success: false,
            result: serde_json::Value::Null,
            error: Some(message.into()),
        }
    }
}

#[cfg(target_arch = "wasm32")]
mod host_ffi {
    extern "C" {
        pub fn hgb_host_log(ptr: i32, len: i32);
        pub fn hgb_fs_read(path_ptr: i32, path_len: i32, out_ptr: i32, max_out: i32) -> i32;
        pub fn hgb_fs_write(path_ptr: i32, path_len: i32, data_ptr: i32, data_len: i32) -> i32;
        pub fn hgb_net_fetch(url_ptr: i32, url_len: i32, out_ptr: i32, max_out: i32) -> i32;
    }
}

/// Safe host log wrapper
pub fn host_log(message: &str) {
    #[cfg(target_arch = "wasm32")]
    unsafe {
        host_ffi::hgb_host_log(message.as_ptr() as i32, message.len() as i32);
    }
    #[cfg(not(target_arch = "wasm32"))]
    let _ = message;
}

/// Safe capability-checked filesystem read
pub fn fs_read(path: &str, max_bytes: usize) -> Result<Vec<u8>, String> {
    #[cfg(target_arch = "wasm32")]
    {
        let mut buffer = vec![0u8; max_bytes];
        let bytes_read = unsafe {
            host_ffi::hgb_fs_read(
                path.as_ptr() as i32,
                path.len() as i32,
                buffer.as_mut_ptr() as i32,
                max_bytes as i32,
            )
        };
        if bytes_read < 0 {
            Err(format!("Filesystem read of '{}' failed or denied by sandbox", path))
        } else {
            buffer.truncate(bytes_read as usize);
            Ok(buffer)
        }
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = max_bytes;
        std::fs::read(path).map_err(|e| e.to_string())
    }
}

/// Safe capability-checked filesystem write
pub fn fs_write(path: &str, data: &[u8]) -> Result<(), String> {
    #[cfg(target_arch = "wasm32")]
    {
        let res = unsafe {
            host_ffi::hgb_fs_write(
                path.as_ptr() as i32,
                path.len() as i32,
                data.as_ptr() as i32,
                data.len() as i32,
            )
        };
        if res == 0 {
            Ok(())
        } else {
            Err(format!("Filesystem write to '{}' failed or denied by sandbox", path))
        }
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        std::fs::write(path, data).map_err(|e| e.to_string())
    }
}

/// Safe capability-checked outbound network request
pub fn net_fetch(url: &str) -> Result<Vec<u8>, String> {
    #[cfg(target_arch = "wasm32")]
    {
        let mut buffer = vec![0u8; 64 * 1024];
        let res = unsafe {
            host_ffi::hgb_net_fetch(
                url.as_ptr() as i32,
                url.len() as i32,
                buffer.as_mut_ptr() as i32,
                buffer.len() as i32,
            )
        };
        if res < 0 {
            Err(format!("Network fetch to '{}' failed or denied by sandbox", url))
        } else {
            buffer.truncate(res as usize);
            Ok(buffer)
        }
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = url;
        Ok(Vec::new())
    }
}

/// Standard guest linear memory allocation for host string exchange
#[no_mangle]
pub extern "C" fn hgb_alloc(size: usize) -> *mut u8 {
    let mut buf = Vec::with_capacity(size);
    let ptr = buf.as_mut_ptr();
    std::mem::forget(buf);
    ptr
}

/// Standard guest linear memory deallocation
#[no_mangle]
pub extern "C" fn hgb_dealloc(ptr: *mut u8, size: usize) {
    if !ptr.is_null() {
        unsafe {
            drop(Vec::from_raw_parts(ptr, 0, size));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_plugin_manifest_serialization() {
        let manifest = PluginManifest {
            name: "security_scanner".to_string(),
            version: "1.2.0".to_string(),
            description: "Deep AST vulnerability scanner".to_string(),
            author: Some("Charle Gutierrez".to_string()),
            capabilities_required: vec!["fs_read".to_string(), "network".to_string()],
            input_schema: serde_json::json!({ "type": "object" }),
        };

        let json = serde_json::to_string(&manifest).expect("serialize");
        let deserialized: PluginManifest = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(manifest, deserialized);
    }

    #[test]
    fn test_invocation_response_construction() {
        let ok_res = ToolInvocationResponse::ok(serde_json::json!({ "status": "clean" }));
        assert!(ok_res.success);
        assert!(ok_res.error.is_none());

        let err_res = ToolInvocationResponse::err("Unauthorized read");
        assert!(!err_res.success);
        assert_eq!(err_res.error.as_deref(), Some("Unauthorized read"));
    }

    #[test]
    fn test_memory_allocation_roundtrip() {
        let ptr = hgb_alloc(1024);
        assert!(!ptr.is_null());
        hgb_dealloc(ptr, 1024);
    }
}
