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

// ============================================================================
// ZERO-COPY SHARED MEMORY BUFFERS & WIT BRIDGING
// ============================================================================

/// Shared Memory Access Flags
pub const SHM_FLAG_READONLY: u32 = 1;
pub const SHM_FLAG_READWRITE: u32 = 2;
pub const SHM_FLAG_RING_BUFFER: u32 = 4;

/// 64-bit Magic for Binary Tensor Serialization ("HGBTENS1")
pub const TENSOR_HEADER_MAGIC: u64 = 0x48474254454E5331;

/// Zero-Copy Shared Linear Memory Buffer Descriptor
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShmBufferDescriptor {
    pub shm_id: String,
    pub base_offset: u32,
    pub capacity_bytes: u32,
    pub head_offset: u32,
    pub tail_offset: u32,
    pub flags: u32,
}

impl Default for ShmBufferDescriptor {
    fn default() -> Self {
        Self {
            shm_id: "default_shm".to_string(),
            base_offset: 0,
            capacity_bytes: 1024 * 1024,
            head_offset: 0,
            tail_offset: 0,
            flags: SHM_FLAG_READWRITE,
        }
    }
}

/// Multi-Modal Tensor Data Type
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[repr(u32)]
pub enum TensorDataType {
    F32 = 0,
    F16 = 1,
    BF16 = 2,
    Q8_0 = 3,
}

impl TensorDataType {
    pub fn element_size(&self) -> usize {
        match self {
            TensorDataType::F32 => 4,
            TensorDataType::F16 => 2,
            TensorDataType::BF16 => 2,
            TensorDataType::Q8_0 => 1,
        }
    }
}

/// Multi-Modal Tensor Metadata Header
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TensorHeader {
    pub name: String,
    pub dtype: TensorDataType,
    pub shape: Vec<u32>,
    pub byte_length: u32,
    pub data_offset: u32,
}

impl TensorHeader {
    pub fn new(name: impl Into<String>, dtype: TensorDataType, shape: Vec<u32>, data_offset: u32) -> Self {
        let element_count: usize = shape.iter().map(|&d| d as usize).product();
        let byte_length = (element_count * dtype.element_size()) as u32;
        Self {
            name: name.into(),
            dtype,
            shape,
            byte_length,
            data_offset,
        }
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(32 + self.shape.len() * 4 + self.name.len());
        buf.extend_from_slice(&TENSOR_HEADER_MAGIC.to_le_bytes());
        buf.extend_from_slice(&(self.dtype as u32).to_le_bytes());
        buf.extend_from_slice(&(self.shape.len() as u32).to_le_bytes());
        for &dim in &self.shape {
            buf.extend_from_slice(&dim.to_le_bytes());
        }
        buf.extend_from_slice(&self.byte_length.to_le_bytes());
        buf.extend_from_slice(&self.data_offset.to_le_bytes());
        let name_bytes = self.name.as_bytes();
        buf.extend_from_slice(&(name_bytes.len() as u32).to_le_bytes());
        buf.extend_from_slice(name_bytes);
        buf
    }

    pub fn from_bytes(slice: &[u8]) -> Result<Self, String> {
        if slice.len() < 24 {
            return Err("Buffer too small for TensorHeader".to_string());
        }
        let magic = u64::from_le_bytes(slice[0..8].try_into().unwrap());
        if magic != TENSOR_HEADER_MAGIC {
            if slice[0] == b'{' {
                return serde_json::from_slice(slice).map_err(|e| e.to_string());
            }
            return Err(format!("Invalid TensorHeader magic: {:#x}", magic));
        }
        let dtype_u32 = u32::from_le_bytes(slice[8..12].try_into().unwrap());
        let dtype = match dtype_u32 {
            0 => TensorDataType::F32,
            1 => TensorDataType::F16,
            2 => TensorDataType::BF16,
            3 => TensorDataType::Q8_0,
            _ => return Err(format!("Unknown TensorDataType code {}", dtype_u32)),
        };
        let shape_len = u32::from_le_bytes(slice[12..16].try_into().unwrap()) as usize;
        let mut offset = 16;
        if slice.len() < offset + shape_len * 4 + 12 {
            return Err("Buffer truncated for tensor shape".to_string());
        }
        let mut shape = Vec::with_capacity(shape_len);
        for _ in 0..shape_len {
            shape.push(u32::from_le_bytes(slice[offset..offset + 4].try_into().unwrap()));
            offset += 4;
        }
        let byte_length = u32::from_le_bytes(slice[offset..offset + 4].try_into().unwrap());
        offset += 4;
        let data_offset = u32::from_le_bytes(slice[offset..offset + 4].try_into().unwrap());
        offset += 4;
        let name_len = u32::from_le_bytes(slice[offset..offset + 4].try_into().unwrap()) as usize;
        offset += 4;
        if slice.len() < offset + name_len {
            return Err("Buffer truncated for tensor name".to_string());
        }
        let name = String::from_utf8(slice[offset..offset + name_len].to_vec())
            .map_err(|e| format!("Invalid utf8 tensor name: {}", e))?;
        Ok(Self {
            name,
            dtype,
            shape,
            byte_length,
            data_offset,
        })
    }
}

pub const GUEST_SHM_DEFAULT_CAPACITY: usize = 1024 * 1024; // 1 MiB

struct GuestShmState {
    buffer: Vec<u8>,
    descriptor: Option<ShmBufferDescriptor>,
}

static GUEST_SHM: std::sync::Mutex<Option<GuestShmState>> = std::sync::Mutex::new(None);

fn with_guest_shm<R>(f: impl FnOnce(&mut GuestShmState) -> R) -> R {
    let mut guard = GUEST_SHM.lock().unwrap();
    let state = guard.get_or_insert_with(|| GuestShmState {
        buffer: vec![0u8; GUEST_SHM_DEFAULT_CAPACITY],
        descriptor: None,
    });
    f(state)
}

/// Attach shared linear memory buffer in guest space
pub fn hgb_shm_attach(descriptor: &ShmBufferDescriptor) -> Result<(), String> {
    with_guest_shm(|state| {
        let needed_size = (descriptor.base_offset as usize).saturating_add(descriptor.capacity_bytes as usize);
        if state.buffer.len() < needed_size {
            state.buffer.resize(needed_size, 0);
        }
        state.descriptor = Some(descriptor.clone());
        Ok(())
    })
}

/// Read tensor header directly from attached shared memory
pub fn hgb_shm_read_tensor_header() -> Result<TensorHeader, String> {
    with_guest_shm(|state| {
        let desc = state
            .descriptor
            .as_ref()
            .ok_or_else(|| "No SHM buffer attached".to_string())?;
        let start = (desc.base_offset + desc.head_offset) as usize;
        if start >= state.buffer.len() {
            return Err("SHM head offset out of buffer bounds".to_string());
        }
        TensorHeader::from_bytes(&state.buffer[start..])
    })
}

/// Stream prompt token array directly to shared memory
pub fn hgb_shm_write_token_stream(tokens: &[u32]) -> Result<u32, String> {
    with_guest_shm(|state| {
        let desc = state
            .descriptor
            .as_mut()
            .ok_or_else(|| "No SHM buffer attached".to_string())?;
        let bytes_to_write = tokens.len() * 4;
        let start = (desc.base_offset + desc.tail_offset) as usize;
        let end = start + bytes_to_write;
        if desc.tail_offset as usize + bytes_to_write > desc.capacity_bytes as usize {
            return Err(format!(
                "Token stream overflow: need {} bytes, capacity remaining {} bytes",
                bytes_to_write,
                desc.capacity_bytes.saturating_sub(desc.tail_offset)
            ));
        }
        if state.buffer.len() < end {
            state.buffer.resize(end, 0);
        }
        for (i, &tok) in tokens.iter().enumerate() {
            let offset = start + i * 4;
            state.buffer[offset..offset + 4].copy_from_slice(&tok.to_le_bytes());
        }
        desc.tail_offset += bytes_to_write as u32;
        Ok(tokens.len() as u32)
    })
}

/// Exported C-ABI: Get raw pointer to guest shared memory buffer
#[no_mangle]
pub extern "C" fn hgb_shm_get_buffer_ptr() -> *mut u8 {
    with_guest_shm(|state| state.buffer.as_mut_ptr())
}

/// Exported C-ABI: Get capacity of guest shared memory buffer
#[no_mangle]
pub extern "C" fn hgb_shm_get_buffer_capacity() -> usize {
    with_guest_shm(|state| state.buffer.len())
}

/// Exported C-ABI: Commit bytes written to guest shared memory buffer
#[no_mangle]
pub extern "C" fn hgb_shm_commit_write(bytes_written: usize) -> usize {
    with_guest_shm(|state| {
        if let Some(desc) = state.descriptor.as_mut() {
            desc.tail_offset = desc.tail_offset.saturating_add(bytes_written as u32);
            desc.tail_offset as usize
        } else {
            bytes_written
        }
    })
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

    #[test]
    fn test_shm_descriptor_and_attachment() {
        let desc = ShmBufferDescriptor {
            shm_id: "test_shm_001".to_string(),
            base_offset: 1024,
            capacity_bytes: 65536,
            head_offset: 0,
            tail_offset: 0,
            flags: SHM_FLAG_READWRITE | SHM_FLAG_RING_BUFFER,
        };
        let attach_res = hgb_shm_attach(&desc);
        assert!(attach_res.is_ok());

        let ptr = hgb_shm_get_buffer_ptr();
        assert!(!ptr.is_null());
        let cap = hgb_shm_get_buffer_capacity();
        assert!(cap >= (1024 + 65536));
    }

    #[test]
    fn test_tensor_header_binary_serde_and_read() {
        let tensor = TensorHeader::new("vision_latent_features", TensorDataType::BF16, vec![1, 16, 768], 512);
        let bytes = tensor.to_bytes();
        let parsed = TensorHeader::from_bytes(&bytes).expect("parse tensor");
        assert_eq!(tensor, parsed);
        assert_eq!(parsed.byte_length, (1 * 16 * 768 * 2) as u32);

        // Attach buffer and write header at head
        let desc = ShmBufferDescriptor {
            shm_id: "tensor_shm".to_string(),
            base_offset: 0,
            capacity_bytes: 65536,
            head_offset: 0,
            tail_offset: 0,
            flags: SHM_FLAG_READWRITE,
        };
        hgb_shm_attach(&desc).expect("attach");
        let ptr = hgb_shm_get_buffer_ptr();
        unsafe {
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr, bytes.len());
        }

        let read_tensor = hgb_shm_read_tensor_header().expect("read tensor header");
        assert_eq!(read_tensor, tensor);
    }

    #[test]
    fn test_token_stream_write_and_commit() {
        let desc = ShmBufferDescriptor {
            shm_id: "tokens_shm".to_string(),
            base_offset: 2048,
            capacity_bytes: 128, // can hold 32 u32 tokens
            head_offset: 0,
            tail_offset: 0,
            flags: SHM_FLAG_READWRITE,
        };
        hgb_shm_attach(&desc).expect("attach");

        let tokens = vec![101u32, 2054, 2003, 1037, 3835, 102];
        let written = hgb_shm_write_token_stream(&tokens).expect("write tokens");
        assert_eq!(written, 6);

        // Commit extra bytes
        let tail = hgb_shm_commit_write(16);
        assert_eq!(tail, (6 * 4) + 16);

        // Overflow test: capacity is 128 bytes, currently at 40 bytes.
        // Trying to write 30 tokens = 120 bytes -> 40 + 120 = 160 > 128 -> must fail!
        let big_tokens = vec![42u32; 30];
        let overflow_res = hgb_shm_write_token_stream(&big_tokens);
        assert!(overflow_res.is_err());
    }
}
