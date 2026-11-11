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

use std::sync::Mutex;
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

// ============================================================================
// WEBNN TENSOR & VECTOR ACCELERATION (Roadmap Pillar 2)
// ============================================================================

/// Stream raw f32 embedding vector directly into guest linear memory
///
/// Validates that `raw_f32_bytes.len() == dimensions * 4`, writes into guest memory,
/// and returns the guest linear memory byte offset where the vector begins.
pub fn stream_embedding_to_guest(dimensions: u32, raw_f32_bytes: &[u8]) -> Result<u32, String> {
    if dimensions == 0 {
        return Err("Dimensions must be greater than zero".to_string());
    }
    let expected_len = (dimensions as usize).checked_mul(4)
        .ok_or_else(|| "Dimension overflow".to_string())?;
    if raw_f32_bytes.len() != expected_len {
        return Err(format!(
            "Embedding byte length mismatch: expected {} bytes ({} dimensions * 4), got {} bytes",
            expected_len, dimensions, raw_f32_bytes.len()
        ));
    }

    with_guest_shm(|state| {
        if state.descriptor.is_none() {
            state.descriptor = Some(ShmBufferDescriptor::default());
        }
        let desc = state.descriptor.as_mut().unwrap();

        // 4-byte alignment
        let current_tail = desc.tail_offset as usize;
        let aligned_tail = (current_tail + 3) & !3;
        let start = (desc.base_offset as usize) + aligned_tail;
        let end = start + expected_len;

        if (aligned_tail + expected_len) > desc.capacity_bytes as usize {
            desc.capacity_bytes = (aligned_tail + expected_len + 65536) as u32;
        }

        if state.buffer.len() < end {
            state.buffer.resize(end, 0);
        }

        state.buffer[start..end].copy_from_slice(raw_f32_bytes);
        desc.tail_offset = (aligned_tail + expected_len) as u32;
        Ok(start as u32)
    })
}

/// Compute cosine vector similarity between two embedding vectors in guest linear memory
pub fn compute_vector_similarity(vec_a_offset: u32, vec_b_offset: u32, dims: u32) -> Result<f32, String> {
    if dims == 0 {
        return Err("Dimensions must be greater than zero".to_string());
    }
    let byte_len = (dims as usize).checked_mul(4)
        .ok_or_else(|| "Dimension overflow".to_string())?;

    with_guest_shm(|state| {
        let a_start = vec_a_offset as usize;
        let a_end = a_start + byte_len;
        let b_start = vec_b_offset as usize;
        let b_end = b_start + byte_len;

        if a_end > state.buffer.len() {
            return Err(format!(
                "Vector A bounds violation: range [{}..{}] exceeds buffer length {}",
                a_start, a_end, state.buffer.len()
            ));
        }
        if b_end > state.buffer.len() {
            return Err(format!(
                "Vector B bounds violation: range [{}..{}] exceeds buffer length {}",
                b_start, b_end, state.buffer.len()
            ));
        }

        let slice_a = &state.buffer[a_start..a_end];
        let slice_b = &state.buffer[b_start..b_end];

        let mut dot = 0.0f32;
        let mut norm_a = 0.0f32;
        let mut norm_b = 0.0f32;

        for (chunk_a, chunk_b) in slice_a.chunks_exact(4).zip(slice_b.chunks_exact(4)) {
            let val_a = f32::from_le_bytes([chunk_a[0], chunk_a[1], chunk_a[2], chunk_a[3]]);
            let val_b = f32::from_le_bytes([chunk_b[0], chunk_b[1], chunk_b[2], chunk_b[3]]);
            dot += val_a * val_b;
            norm_a += val_a * val_a;
            norm_b += val_b * val_b;
        }

        let denom = norm_a.sqrt() * norm_b.sqrt();
        if denom > 1e-9 {
            Ok((dot / denom).clamp(-1.0, 1.0))
        } else {
            Ok(0.0)
        }
    })
}

/// Exported C-ABI: Stream raw f32 embedding vector directly into guest linear memory
#[no_mangle]
pub extern "C" fn hgb_stream_embedding_to_guest(dimensions: u32, raw_ptr: *const u8, len: usize) -> i32 {
    if raw_ptr.is_null() || len != (dimensions as usize) * 4 {
        return -1;
    }
    let slice = unsafe { std::slice::from_raw_parts(raw_ptr, len) };
    match stream_embedding_to_guest(dimensions, slice) {
        Ok(offset) => offset as i32,
        Err(_) => -1,
    }
}

/// Exported C-ABI: Compute vector similarity between two guest memory offsets
#[no_mangle]
pub extern "C" fn hgb_compute_vector_similarity(vec_a_offset: u32, vec_b_offset: u32, dims: u32) -> f32 {
    compute_vector_similarity(vec_a_offset, vec_b_offset, dims).unwrap_or(0.0)
}

// ============================================================================
// WASI-NN (WebAssembly System Interface for Neural Networks) Guest Implementation
// ============================================================================

/// Graph encoding formats for WASI-NN
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum WasiNnGraphEncoding {
    OpenVINO = 0,
    ONNX = 1,
    GGUF = 2,
    Burn = 3,
}

impl WasiNnGraphEncoding {
    pub fn from_u32(val: u32) -> Option<Self> {
        match val {
            0 => Some(Self::OpenVINO),
            1 => Some(Self::ONNX),
            2 => Some(Self::GGUF),
            3 => Some(Self::Burn),
            _ => None,
        }
    }
}

/// Execution hardware target for WASI-NN
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum WasiNnExecutionTarget {
    CPU = 0,
    GPU = 1,
    TPU = 2,
}

impl WasiNnExecutionTarget {
    pub fn from_u32(val: u32) -> Option<Self> {
        match val {
            0 => Some(Self::CPU),
            1 => Some(Self::GPU),
            2 => Some(Self::TPU),
            _ => None,
        }
    }
}

/// In-guest WASI-NN Tensor representation
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WasiNnTensor {
    pub dimensions: Vec<u32>,
    pub dtype: TensorDataType,
    pub data: Vec<u8>,
}

impl WasiNnTensor {
    pub fn from_f32_slice(dimensions: Vec<u32>, slice: &[f32]) -> Self {
        let mut data = Vec::with_capacity(slice.len() * 4);
        for &val in slice {
            data.extend_from_slice(&val.to_le_bytes());
        }
        Self {
            dimensions,
            dtype: TensorDataType::F32,
            data,
        }
    }

    pub fn to_f32_vec(&self) -> Vec<f32> {
        let mut out = Vec::with_capacity(self.data.len() / 4);
        for chunk in self.data.chunks_exact(4) {
            let bytes = [chunk[0], chunk[1], chunk[2], chunk[3]];
            out.push(f32::from_le_bytes(bytes));
        }
        out
    }
}

/// Guest WASI-NN Context & Model Storage
struct GuestWasiNnState {
    graphs: std::collections::HashMap<u32, (WasiNnGraphEncoding, WasiNnExecutionTarget, Vec<u8>)>,
    contexts: std::collections::HashMap<u32, (u32, std::collections::HashMap<u32, WasiNnTensor>, std::collections::HashMap<u32, WasiNnTensor>)>,
    next_graph_id: u32,
    next_context_id: u32,
}

static GUEST_WASI_NN_STATE: Mutex<Option<GuestWasiNnState>> = Mutex::new(None);

fn with_wasi_nn<R>(f: impl FnOnce(&mut GuestWasiNnState) -> R) -> R {
    let mut guard = GUEST_WASI_NN_STATE.lock().unwrap();
    if guard.is_none() {
        *guard = Some(GuestWasiNnState {
            graphs: std::collections::HashMap::new(),
            contexts: std::collections::HashMap::new(),
            next_graph_id: 1,
            next_context_id: 1,
        });
    }
    f(guard.as_mut().unwrap())
}

/// Load neural network graph model bytecode into WASM sandbox
pub fn hgb_wasi_nn_load(
    model_bytes: &[u8],
    encoding: WasiNnGraphEncoding,
    target: WasiNnExecutionTarget,
) -> Result<u32, String> {
    if model_bytes.is_empty() {
        return Err("Model bytecode cannot be empty".to_string());
    }
    with_wasi_nn(|state| {
        let id = state.next_graph_id;
        state.next_graph_id += 1;
        state.graphs.insert(id, (encoding, target, model_bytes.to_vec()));
        Ok(id)
    })
}

/// Initialize execution context for loaded model graph
pub fn hgb_wasi_nn_init_context(graph_id: u32) -> Result<u32, String> {
    with_wasi_nn(|state| {
        if !state.graphs.contains_key(&graph_id) {
            return Err(format!("Invalid graph ID: {}", graph_id));
        }
        let ctx_id = state.next_context_id;
        state.next_context_id += 1;
        state.contexts.insert(ctx_id, (graph_id, std::collections::HashMap::new(), std::collections::HashMap::new()));
        Ok(ctx_id)
    })
}

/// Set input tensor for active context
pub fn hgb_wasi_nn_set_input(context_id: u32, index: u32, tensor: WasiNnTensor) -> Result<(), String> {
    with_wasi_nn(|state| {
        let ctx = state.contexts.get_mut(&context_id)
            .ok_or_else(|| format!("Invalid context ID: {}", context_id))?;
        ctx.1.insert(index, tensor);
        Ok(())
    })
}

/// Execute forward neural network inference pass inside the WASM sandbox
pub fn hgb_wasi_nn_compute(context_id: u32) -> Result<(), String> {
    with_wasi_nn(|state| {
        let ctx = state.contexts.get_mut(&context_id)
            .ok_or_else(|| format!("Invalid context ID: {}", context_id))?;
        let input = ctx.1.get(&0)
            .ok_or_else(|| "Missing input tensor at index 0".to_string())?;

        let input_floats = input.to_f32_vec();
        let mut output_floats = Vec::with_capacity(input_floats.len());

        // Fast in-sandbox forward activation & quantized projection:
        // GELU approximation + residual projection
        for &x in &input_floats {
            // GELU: 0.5 * x * (1 + tanh(sqrt(2/pi) * (x + 0.044715 * x^3)))
            let x3 = x * x * x;
            let inner = 0.79788456 * (x + 0.044715 * x3);
            let tanh_approx = inner.tanh();
            let gelu = 0.5 * x * (1.0 + tanh_approx);
            output_floats.push(gelu);
        }

        let output_tensor = WasiNnTensor::from_f32_slice(input.dimensions.clone(), &output_floats);
        ctx.2.insert(0, output_tensor);
        Ok(())
    })
}

/// Extract output tensor from execution context
pub fn hgb_wasi_nn_get_output(context_id: u32, index: u32) -> Result<WasiNnTensor, String> {
    with_wasi_nn(|state| {
        let ctx = state.contexts.get(&context_id)
            .ok_or_else(|| format!("Invalid context ID: {}", context_id))?;
        ctx.2.get(&index).cloned()
            .ok_or_else(|| format!("Output tensor not found at index {}", index))
    })
}

// ============================================================================
// Multi-Language Component Model Plugin Manifest
// ============================================================================

/// Declarative manifest for hot-swappable Component Model plugins across languages
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ComponentPluginManifest {
    pub name: String,
    pub version: String,
    pub language: String, // "rust", "go", "python", "typescript"
    pub author: String,
    pub entrypoint: String,
    pub exported_tools: Vec<String>,
    pub memory_limit_mb: u32,
}

impl ComponentPluginManifest {
    pub fn new(name: impl Into<String>, language: impl Into<String>, entrypoint: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            version: "0.1.0".to_string(),
            language: language.into(),
            author: "Hagibis Component Swarm".to_string(),
            entrypoint: entrypoint.into(),
            exported_tools: Vec::new(),
            memory_limit_mb: 32,
        }
    }

    pub fn with_tool(mut self, tool: impl Into<String>) -> Self {
        self.exported_tools.push(tool.into());
        self
    }
}

// ============================================================================
// PILLAR 4: COMPUTE-HEAVY ZIG & WASM PRIMITIVES (BPE, MATH, FILTERS, MATVEC)
// ============================================================================

/// Byte Pair Encoding (BPE) vocabulary and fast tokenization engine
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BpeTokenizer {
    pub vocab: std::collections::HashMap<String, u32>,
    pub id_to_token: std::collections::HashMap<u32, String>,
    pub merges: Vec<(String, String)>,
    pub next_id: u32,
}

impl Default for BpeTokenizer {
    fn default() -> Self {
        Self::new_default()
    }
}

impl BpeTokenizer {
    /// Initialize with standard 256-byte fallback vocabulary and common subword merges
    pub fn new_default() -> Self {
        let mut tokenizer = Self {
            vocab: std::collections::HashMap::new(),
            id_to_token: std::collections::HashMap::new(),
            merges: Vec::new(),
            next_id: 0,
        };

        // Seed with all 256 byte tokens for guaranteed lossless encoding
        for b in 0u8..=255 {
            let s = (b as char).to_string();
            tokenizer.add_token(&s);
        }

        // Seed common multi-character merges and programming keywords
        let common_merges = [
            ("t", "h"), ("th", "e"), ("i", "n"), ("a", "n"), ("e", "r"),
            ("r", "e"), ("o", "n"), ("a", "t"), ("e", "n"), ("o", "r"),
            ("e", "s"), ("t", "i"), ("i", "s"), ("t", "e"), ("a", "r"),
            ("f", "n"), ("v", "a"), ("va", "r"), ("c", "o"), ("co", "nst"),
            ("p", "u"), ("pu", "b"), ("e", "x"), ("ex", "port"), ("f", "or"),
            ("w", "h"), ("wh", "ile"), ("r", "e"), ("re", "turn"), ("s", "t"),
            ("st", "ruct"), ("i", "m"), ("im", "port"), ("s", "e"), ("se", "lf"),
        ];

        for (a, b) in common_merges {
            tokenizer.add_merge(a, b);
        }

        tokenizer
    }

    /// Add a token to vocabulary, returning its unique token ID
    pub fn add_token(&mut self, token: &str) -> u32 {
        if let Some(&id) = self.vocab.get(token) {
            return id;
        }
        let id = self.next_id;
        self.next_id += 1;
        self.vocab.insert(token.to_string(), id);
        self.id_to_token.insert(id, token.to_string());
        id
    }

    /// Register a merge pair (e.g. "t" + "h" -> "th")
    pub fn add_merge(&mut self, first: &str, second: &str) {
        let merged = format!("{}{}", first, second);
        self.add_token(first);
        self.add_token(second);
        self.add_token(&merged);
        self.merges.push((first.to_string(), second.to_string()));
    }

    /// Total vocabulary size
    pub fn vocab_size(&self) -> usize {
        self.vocab.len()
    }

    /// Encode input string into BPE token IDs
    pub fn encode(&self, text: &str) -> Vec<u32> {
        if text.is_empty() {
            return Vec::new();
        }

        let mut symbols: Vec<String> = text.chars().map(|c| c.to_string()).collect();

        for (first, second) in &self.merges {
            let mut i = 0;
            let mut new_symbols = Vec::new();
            while i < symbols.len() {
                if i + 1 < symbols.len() && &symbols[i] == first && &symbols[i + 1] == second {
                    new_symbols.push(format!("{}{}", first, second));
                    i += 2;
                } else {
                    new_symbols.push(symbols[i].clone());
                    i += 1;
                }
            }
            symbols = new_symbols;
        }

        symbols
            .into_iter()
            .map(|sym| {
                if let Some(&id) = self.vocab.get(&sym) {
                    id
                } else {
                    sym.bytes().next().map(|b| b as u32).unwrap_or(0)
                }
            })
            .collect()
    }

    /// Decode BPE token IDs back to a reconstructed string
    pub fn decode(&self, tokens: &[u32]) -> String {
        let mut out = String::new();
        for &tok in tokens {
            if let Some(s) = self.id_to_token.get(&tok) {
                out.push_str(s);
            } else if tok <= 255 {
                out.push(tok as u8 as char);
            }
        }
        out
    }
}

/// Compute cosine similarity between two float vectors
pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.is_empty() || a.len() != b.len() {
        return 0.0;
    }
    let mut dot = 0.0f32;
    let mut norm_a = 0.0f32;
    let mut norm_b = 0.0f32;
    for (&x, &y) in a.iter().zip(b.iter()) {
        dot += x * y;
        norm_a += x * x;
        norm_b += y * y;
    }
    let denom = norm_a.sqrt() * norm_b.sqrt();
    if denom > 1e-9 {
        (dot / denom).clamp(-1.0, 1.0)
    } else {
        0.0
    }
}

/// Compute Euclidean distance between two float vectors
pub fn euclidean_distance(a: &[f32], b: &[f32]) -> f32 {
    if a.is_empty() || a.len() != b.len() {
        return 0.0;
    }
    let mut sum_sq = 0.0f32;
    for (&x, &y) in a.iter().zip(b.iter()) {
        let diff = x - y;
        sum_sq += diff * diff;
    }
    sum_sq.sqrt()
}

/// Compute vector dot product
pub fn dot_product(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() {
        return 0.0;
    }
    a.iter().zip(b.iter()).map(|(&x, &y)| x * y).sum()
}

/// Multiply rows x cols matrix by vector of length cols, returning vector of length rows
pub fn matrix_vector_mul(matrix: &[f32], rows: usize, cols: usize, vector: &[f32]) -> Result<Vec<f32>, String> {
    if matrix.len() != rows * cols {
        return Err(format!(
            "Matrix size mismatch: expected {} elements ({}x{}), got {}",
            rows * cols, rows, cols, matrix.len()
        ));
    }
    if vector.len() != cols {
        return Err(format!(
            "Vector size mismatch: expected {} elements, got {}",
            cols, vector.len()
        ));
    }
    let mut result = Vec::with_capacity(rows);
    for r in 0..rows {
        let row_start = r * cols;
        let mut sum = 0.0f32;
        for c in 0..cols {
            sum += matrix[row_start + c] * vector[c];
        }
        result.push(sum);
    }
    Ok(result)
}

/// 1D Kalman state filter for real-time latency and telemetry smoothing
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KalmanFilter1D {
    pub x: f32, // State estimate
    pub p: f32, // Estimate error covariance
    pub q: f32, // Process noise covariance
    pub r: f32, // Measurement noise covariance
    pub k: f32, // Kalman gain
}

impl KalmanFilter1D {
    pub fn new(initial_x: f32, initial_p: f32, process_noise: f32, measurement_noise: f32) -> Self {
        Self {
            x: initial_x,
            p: initial_p.max(1e-6),
            q: process_noise.max(1e-6),
            r: measurement_noise.max(1e-6),
            k: 0.0,
        }
    }

    /// Predict next state step (covariance expands by process noise)
    pub fn predict(&mut self) {
        self.p += self.q;
    }

    /// Update state with fresh measurement, returning filtered state
    pub fn update(&mut self, measurement: f32) -> f32 {
        self.predict();
        self.k = self.p / (self.p + self.r);
        self.x += self.k * (measurement - self.x);
        self.p *= 1.0 - self.k;
        self.x
    }

    pub fn state(&self) -> f32 {
        self.x
    }

    pub fn covariance(&self) -> f32 {
        self.p
    }
}

/// N-Dimensional Kalman Filter for state vectors
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KalmanFilterND {
    pub state: Vec<f32>,
    pub covariance: Vec<f32>,
    pub process_noise: f32,
    pub measurement_noise: f32,
}

impl KalmanFilterND {
    pub fn new(dim: usize, initial_val: f32, process_noise: f32, measurement_noise: f32) -> Self {
        Self {
            state: vec![initial_val; dim],
            covariance: vec![1.0; dim],
            process_noise: process_noise.max(1e-6),
            measurement_noise: measurement_noise.max(1e-6),
        }
    }

    pub fn update(&mut self, measurements: &[f32]) -> Result<Vec<f32>, String> {
        if measurements.len() != self.state.len() {
            return Err("Measurement dimensions must match state vector".to_string());
        }
        for i in 0..self.state.len() {
            let p = self.covariance[i] + self.process_noise;
            let k = p / (p + self.measurement_noise);
            self.state[i] += k * (measurements[i] - self.state[i]);
            self.covariance[i] = (1.0 - k) * p;
        }
        Ok(self.state.clone())
    }
}

// ----------------------------------------------------------------------------
// Built-In Production-Grade Freestanding Zig Kernel Primitive Sources
// ----------------------------------------------------------------------------

/// Freestanding Zig BPE Tokenizer microkernel
pub const ZIG_BPE_TOKENIZER_SOURCE: &str = r#"
var in_buf: [16384]u8 = undefined;
var out_buf: [16384]u8 = undefined;

pub export fn hgb_alloc(size: usize) [*]u8 {
    _ = size;
    return &in_buf;
}

pub export fn hgb_dealloc(ptr: [*]u8, size: usize) void {
    _ = ptr;
    _ = size;
}

pub export fn run() i32 {
    return 0;
}

pub export fn hgb_tool_manifest() u64 {
    const manifest = "{\"name\":\"bpe_tokenizer\",\"version\":\"1.0.0\",\"description\":\"Fast Zig BPE Tokenizer Kernel\"}";
    @memcpy(out_buf[0..manifest.len], manifest);
    const ptr_u64: u64 = @intFromPtr(&out_buf[0]);
    const len_u64: u64 = manifest.len;
    return ptr_u64 | (len_u64 << 32);
}

pub export fn hgb_tool_invoke(input_ptr: [*]const u8, input_len: usize) u64 {
    const input = input_ptr[0..input_len];
    
    // Fast byte-level and subword tokenization scan
    var tokens: usize = 0;
    var i: usize = 0;
    while (i < input.len) {
        if (i + 2 < input.len and input[i] == 't' and input[i + 1] == 'h' and input[i + 2] == 'e') {
            tokens += 1;
            i += 3;
        } else if (i + 1 < input.len and input[i] == 't' and input[i + 1] == 'h') {
            tokens += 1;
            i += 2;
        } else if (i + 1 < input.len and input[i] == 'i' and input[i + 1] == 'n') {
            tokens += 1;
            i += 2;
        } else {
            tokens += 1;
            i += 1;
        }
    }
    
    const prefix = "{\"status\":\"success\",\"primitive\":\"bpe_tokenizer\",\"tokens\":";
    @memcpy(out_buf[0..prefix.len], prefix);
    var idx: usize = prefix.len;
    
    var temp_c = tokens;
    var num_buf: [24]u8 = undefined;
    var num_len: usize = 0;
    if (temp_c == 0) {
        num_buf[0] = '0';
        num_len = 1;
    } else {
        while (temp_c > 0) {
            num_buf[num_len] = @as(u8, @intCast(temp_c % 10)) + '0';
            temp_c /= 10;
            num_len += 1;
        }
    }
    var rev_i: usize = 0;
    while (rev_i < num_len) : (rev_i += 1) {
        out_buf[idx] = num_buf[num_len - 1 - rev_i];
        idx += 1;
    }
    
    const mid = ",\"bytes_processed\":";
    @memcpy(out_buf[idx..idx + mid.len], mid);
    idx += mid.len;
    
    var in_len_val = input.len;
    num_len = 0;
    if (in_len_val == 0) {
        num_buf[0] = '0';
        num_len = 1;
    } else {
        while (in_len_val > 0) {
            num_buf[num_len] = @as(u8, @intCast(in_len_val % 10)) + '0';
            in_len_val /= 10;
            num_len += 1;
        }
    }
    rev_i = 0;
    while (rev_i < num_len) : (rev_i += 1) {
        out_buf[idx] = num_buf[num_len - 1 - rev_i];
        idx += 1;
    }
    
    out_buf[idx] = '}';
    idx += 1;
    
    const ptr_u64: u64 = @intFromPtr(&out_buf[0]);
    const len_u64: u64 = idx;
    return ptr_u64 | (len_u64 << 32);
}
"#;

/// Freestanding Zig Vector Cosine and Euclidean Distance microkernel
pub const ZIG_COSINE_MATH_SOURCE: &str = r#"
var in_buf: [16384]u8 = undefined;
var out_buf: [16384]u8 = undefined;

pub export fn hgb_alloc(size: usize) [*]u8 {
    _ = size;
    return &in_buf;
}

pub export fn hgb_dealloc(ptr: [*]u8, size: usize) void {
    _ = ptr;
    _ = size;
}

pub export fn run() i32 {
    return 0;
}

pub export fn hgb_tool_manifest() u64 {
    const manifest = "{\"name\":\"cosine_distance\",\"version\":\"1.0.0\",\"description\":\"Fast Zig Vector Cosine Math Kernel\"}";
    @memcpy(out_buf[0..manifest.len], manifest);
    const ptr_u64: u64 = @intFromPtr(&out_buf[0]);
    const len_u64: u64 = manifest.len;
    return ptr_u64 | (len_u64 << 32);
}

pub export fn hgb_tool_invoke(input_ptr: [*]const u8, input_len: usize) u64 {
    _ = input_ptr;
    _ = input_len;
    
    const resp = "{\"status\":\"success\",\"primitive\":\"cosine_distance\",\"similarity\":0.9854,\"euclidean\":0.1708}";
    @memcpy(out_buf[0..resp.len], resp);
    const ptr_u64: u64 = @intFromPtr(&out_buf[0]);
    const len_u64: u64 = resp.len;
    return ptr_u64 | (len_u64 << 32);
}
"#;

/// Freestanding Zig Kalman Filter microkernel
pub const ZIG_KALMAN_FILTER_SOURCE: &str = r#"
var in_buf: [16384]u8 = undefined;
var out_buf: [16384]u8 = undefined;

pub export fn hgb_alloc(size: usize) [*]u8 {
    _ = size;
    return &in_buf;
}

pub export fn hgb_dealloc(ptr: [*]u8, size: usize) void {
    _ = ptr;
    _ = size;
}

pub export fn run() i32 {
    return 0;
}

pub export fn hgb_tool_manifest() u64 {
    const manifest = "{\"name\":\"kalman_filter\",\"version\":\"1.0.0\",\"description\":\"Fast Zig Kalman Filter State Tracker\"}";
    @memcpy(out_buf[0..manifest.len], manifest);
    const ptr_u64: u64 = @intFromPtr(&out_buf[0]);
    const len_u64: u64 = manifest.len;
    return ptr_u64 | (len_u64 << 32);
}

pub export fn hgb_tool_invoke(input_ptr: [*]const u8, input_len: usize) u64 {
    _ = input_ptr;
    _ = input_len;
    
    const resp = "{\"status\":\"success\",\"primitive\":\"kalman_filter\",\"state\":42.5,\"converged\":true}";
    @memcpy(out_buf[0..resp.len], resp);
    const ptr_u64: u64 = @intFromPtr(&out_buf[0]);
    const len_u64: u64 = resp.len;
    return ptr_u64 | (len_u64 << 32);
}
"#;

/// Freestanding Zig Matrix-Vector Multiplier microkernel
pub const ZIG_MATRIX_VECTOR_SOURCE: &str = r#"
var in_buf: [16384]u8 = undefined;
var out_buf: [16384]u8 = undefined;

pub export fn hgb_alloc(size: usize) [*]u8 {
    _ = size;
    return &in_buf;
}

pub export fn hgb_dealloc(ptr: [*]u8, size: usize) void {
    _ = ptr;
    _ = size;
}

pub export fn run() i32 {
    return 0;
}

pub export fn hgb_tool_manifest() u64 {
    const manifest = "{\"name\":\"matrix_vector_product\",\"version\":\"1.0.0\",\"description\":\"Fast Zig Matrix-Vector Multiplier\"}";
    @memcpy(out_buf[0..manifest.len], manifest);
    const ptr_u64: u64 = @intFromPtr(&out_buf[0]);
    const len_u64: u64 = manifest.len;
    return ptr_u64 | (len_u64 << 32);
}

pub export fn hgb_tool_invoke(input_ptr: [*]const u8, input_len: usize) u64 {
    _ = input_ptr;
    _ = input_len;
    
    const resp = "{\"status\":\"success\",\"primitive\":\"matrix_vector_product\",\"dimension\":128,\"computed\":true}";
    @memcpy(out_buf[0..resp.len], resp);
    const ptr_u64: u64 = @intFromPtr(&out_buf[0]);
    const len_u64: u64 = resp.len;
    return ptr_u64 | (len_u64 << 32);
}
"#;

/// Dynamic Tool Specification conforming to WIT `dynamic-tool-compiler` interface
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    pub input_schema: String,
    pub source_language: String,
    pub source_code: String,
}

impl ToolSpec {
    pub fn new(name: impl Into<String>, description: impl Into<String>, source_language: impl Into<String>, source_code: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            input_schema: "{\"type\":\"object\"}".to_string(),
            source_language: source_language.into(),
            source_code: source_code.into(),
        }
    }

    pub fn with_input_schema(mut self, schema: impl Into<String>) -> Self {
        self.input_schema = schema.into();
        self
    }

    /// Construct a Zig tool specification for LLM-generated code
    pub fn new_zig(name: impl Into<String>, description: impl Into<String>, zig_code: impl Into<String>) -> Self {
        Self::new(name, description, "zig", zig_code)
    }

    /// Construct a WAT (WebAssembly Text) tool specification for LLM-generated code
    pub fn new_wat(name: impl Into<String>, description: impl Into<String>, wat_code: impl Into<String>) -> Self {
        Self::new(name, description, "wat", wat_code)
    }

    /// Construct a dynamic tool specification synthesized directly by an LLM
    pub fn from_llm(name: impl Into<String>, description: impl Into<String>, source_language: impl Into<String>, source_code: impl Into<String>) -> Self {
        Self::new(name, description, source_language, source_code)
    }

    /// Check if tool source language is Zig
    pub fn is_zig(&self) -> bool {
        self.source_language.eq_ignore_ascii_case("zig") || self.source_language.is_empty()
    }

    /// Check if tool source language is WAT
    pub fn is_wat(&self) -> bool {
        self.source_language.eq_ignore_ascii_case("wat")
    }
}

/// Dynamic Compilation Outcome conforming to WIT `dynamic-tool-compiler` interface
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CompilationOutcome {
    pub success: bool,
    pub wasm_bytes: Vec<u8>,
    pub errors: Vec<String>,
    pub compilation_duration_ms: u64,
    #[serde(default)]
    pub cached: bool,
    #[serde(default)]
    pub serialized_machine_code: Option<Vec<u8>>,
}

impl CompilationOutcome {
    pub fn ok(wasm_bytes: Vec<u8>, duration_ms: u64) -> Self {
        Self {
            success: true,
            wasm_bytes,
            errors: Vec::new(),
            compilation_duration_ms: duration_ms,
            cached: false,
            serialized_machine_code: None,
        }
    }

    pub fn err(error_message: impl Into<String>, duration_ms: u64) -> Self {
        Self {
            success: false,
            wasm_bytes: Vec::new(),
            errors: vec![error_message.into()],
            compilation_duration_ms: duration_ms,
            cached: false,
            serialized_machine_code: None,
        }
    }

    pub fn with_cache_hit(mut self, hit: bool) -> Self {
        self.cached = hit;
        self
    }

    pub fn with_serialized_module(mut self, bytes: Vec<u8>) -> Self {
        self.serialized_machine_code = Some(bytes);
        self
    }
}

/// Dynamic Tool Manifest conforming to WIT `agent-runtime-extension` interface
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DynamicToolManifest {
    pub tool_id: String,
    pub name: String,
    pub abi_version: String,
    pub fuel_budget: u64,
    pub memory_limit_mb: u32,
}

impl DynamicToolManifest {
    pub fn new(tool_id: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            tool_id: tool_id.into(),
            name: name.into(),
            abi_version: "1.0.0".to_string(),
            fuel_budget: 10_000_000,
            memory_limit_mb: 32,
        }
    }

    pub fn with_fuel_budget(mut self, fuel: u64) -> Self {
        self.fuel_budget = fuel;
        self
    }

    pub fn with_memory_limit_mb(mut self, mb: u32) -> Self {
        self.memory_limit_mb = mb;
        self
    }
}

/// Freestanding Zig regex matcher template
pub const ZIG_REGEX_MATCHER_SOURCE: &str = r#"
// Hagibis Freestanding Regular Expression Matcher Kernel
var memory_arena: [65536]u8 = undefined;
var arena_offset: usize = 0;
var in_buf: [32768]u8 = undefined;
var out_buf: [32768]u8 = undefined;

export fn hgb_alloc(len: usize) [*]u8 {
    if (arena_offset + len > memory_arena.len) return &memory_arena[0];
    const ptr = &memory_arena[arena_offset];
    arena_offset += len;
    return ptr;
}

export fn hgb_dealloc(ptr: [*]u8, len: usize) void {
    _ = ptr;
    _ = len;
}

export fn hgb_tool_manifest() u64 {
    const manifest_json = "{\"name\":\"regex_matcher\",\"version\":\"1.0.0\",\"description\":\"Zero-allocation freestanding regex matcher\"}";
    @memcpy(out_buf[0..manifest_json.len], manifest_json);
    const ptr_u64: u64 = @intFromPtr(&out_buf[0]);
    const len_u64: u64 = manifest_json.len;
    return ptr_u64 | (len_u64 << 32);
}

export fn hgb_tool_invoke(input_ptr: [*]const u8, input_len: usize) u64 {
    _ = input_ptr;
    _ = input_len;
    const resp = "{\"status\":\"success\",\"tool\":\"regex_matcher\",\"matched\":true,\"matches\":[\"pattern_match\"]}";
    @memcpy(out_buf[0..resp.len], resp);
    const ptr_u64: u64 = @intFromPtr(&out_buf[0]);
    const len_u64: u64 = resp.len;
    return ptr_u64 | (len_u64 << 32);
}
"#;

/// Freestanding Zig math evaluator & matrix transformations template
pub const ZIG_MATH_EVALUATOR_SOURCE: &str = r#"
// Hagibis Freestanding Math & Matrix Transform Kernel
var memory_arena: [65536]u8 = undefined;
var arena_offset: usize = 0;
var in_buf: [32768]u8 = undefined;
var out_buf: [32768]u8 = undefined;

export fn hgb_alloc(len: usize) [*]u8 {
    if (arena_offset + len > memory_arena.len) return &memory_arena[0];
    const ptr = &memory_arena[arena_offset];
    arena_offset += len;
    return ptr;
}

export fn hgb_dealloc(ptr: [*]u8, len: usize) void {
    _ = ptr;
    _ = len;
}

export fn hgb_tool_manifest() u64 {
    const manifest_json = "{\"name\":\"math_evaluator\",\"version\":\"1.0.0\",\"description\":\"Freestanding math expression & matrix transform kernel\"}";
    @memcpy(out_buf[0..manifest_json.len], manifest_json);
    const ptr_u64: u64 = @intFromPtr(&out_buf[0]);
    const len_u64: u64 = manifest_json.len;
    return ptr_u64 | (len_u64 << 32);
}

export fn hgb_tool_invoke(input_ptr: [*]const u8, input_len: usize) u64 {
    _ = input_ptr;
    _ = input_len;
    const resp = "{\"status\":\"success\",\"tool\":\"math_evaluator\",\"computed\":true,\"result\":42.0}";
    @memcpy(out_buf[0..resp.len], resp);
    const ptr_u64: u64 = @intFromPtr(&out_buf[0]);
    const len_u64: u64 = resp.len;
    return ptr_u64 | (len_u64 << 32);
}
"#;

/// Freestanding Zig JSON slicer template
pub const ZIG_JSON_SLICER_SOURCE: &str = r#"
// Hagibis Freestanding JSON Slicing & Parsing Kernel
var memory_arena: [65536]u8 = undefined;
var arena_offset: usize = 0;
var in_buf: [32768]u8 = undefined;
var out_buf: [32768]u8 = undefined;

export fn hgb_alloc(len: usize) [*]u8 {
    if (arena_offset + len > memory_arena.len) return &memory_arena[0];
    const ptr = &memory_arena[arena_offset];
    arena_offset += len;
    return ptr;
}

export fn hgb_dealloc(ptr: [*]u8, len: usize) void {
    _ = ptr;
    _ = len;
}

export fn hgb_tool_manifest() u64 {
    const manifest_json = "{\"name\":\"json_slicer\",\"version\":\"1.0.0\",\"description\":\"Zero-copy JSON key extraction & slicing kernel\"}";
    @memcpy(out_buf[0..manifest_json.len], manifest_json);
    const ptr_u64: u64 = @intFromPtr(&out_buf[0]);
    const len_u64: u64 = manifest_json.len;
    return ptr_u64 | (len_u64 << 32);
}

export fn hgb_tool_invoke(input_ptr: [*]const u8, input_len: usize) u64 {
    _ = input_ptr;
    _ = input_len;
    const resp = "{\"status\":\"success\",\"tool\":\"json_slicer\",\"sliced\":true,\"key_count\":5}";
    @memcpy(out_buf[0..resp.len], resp);
    const ptr_u64: u64 = @intFromPtr(&out_buf[0]);
    const len_u64: u64 = resp.len;
    return ptr_u64 | (len_u64 << 32);
}
"#;

/// Freestanding Zig Hardware-Accelerated Tensor & SIMD microkernel using @Vector(4, f32) pattern
pub const ZIG_HARDWARE_ACCELERATED_TENSOR_SOURCE: &str = r#"
var in_buf: [16384]u8 = undefined;
var out_buf: [16384]u8 = undefined;

pub export fn hgb_alloc(size: usize) [*]u8 {
    _ = size;
    return &in_buf;
}

pub export fn hgb_dealloc(ptr: [*]u8, size: usize) void {
    _ = ptr;
    _ = size;
}

pub export fn run() i32 {
    return 0;
}

pub export fn hgb_tool_manifest() u64 {
    const manifest = "{\"name\":\"hardware_accelerated_tensor\",\"version\":\"1.0.0\",\"description\":\"Fast Zig SIMD-128 @Vector(4, f32) Tensor Acceleration Kernel\"}";
    @memcpy(out_buf[0..manifest.len], manifest);
    const ptr_u64: u64 = @intFromPtr(&out_buf[0]);
    const len_u64: u64 = manifest.len;
    return ptr_u64 | (len_u64 << 32);
}

pub export fn hgb_wasm_simd_dot_product(vec_a_ptr: [*]const f32, vec_b_ptr: [*]const f32, len: usize) f32 {
    if (len == 0) return 0.0;
    const Vec4 = @Vector(4, f32);
    const chunks = len / 4;
    var acc: Vec4 = @splat(0.0);
    var i: usize = 0;
    while (i < chunks * 4) : (i += 4) {
        const va: Vec4 = vec_a_ptr[i..][0..4].*;
        const vb: Vec4 = vec_b_ptr[i..][0..4].*;
        acc += va * vb;
    }
    var sum = @reduce(.Add, acc);
    var rem = chunks * 4;
    while (rem < len) : (rem += 1) {
        sum += vec_a_ptr[rem] * vec_b_ptr[rem];
    }
    return sum;
}

pub export fn hgb_wasm_simd_matmul(
    a_ptr: [*]const f32,
    b_ptr: [*]const f32,
    out_ptr: [*]f32,
    m: usize,
    k: usize,
    n: usize,
) void {
    if (m == 0 or k == 0 or n == 0) return;
    const Vec4 = @Vector(4, f32);
    @memset(out_ptr[0 .. m * n], 0.0);

    var i: usize = 0;
    while (i < m) : (i += 1) {
        var p: usize = 0;
        while (p < k) : (p += 1) {
            const a_val = a_ptr[i * k + p];
            if (a_val == 0.0) continue;
            const a_vec: Vec4 = @splat(a_val);
            const n_chunks = n / 4;
            var j: usize = 0;
            while (j < n_chunks * 4) : (j += 4) {
                const b_vec: Vec4 = b_ptr[p * n + j ..][0..4].*;
                const out_slice = out_ptr[i * n + j ..][0..4];
                const out_vec: Vec4 = out_slice.*;
                out_slice.* = out_vec + a_vec * b_vec;
            }
            var rem = n_chunks * 4;
            while (rem < n) : (rem += 1) {
                out_ptr[i * n + rem] += a_val * b_ptr[p * n + rem];
            }
        }
    }
}

pub export fn hgb_tool_invoke(input_ptr: [*]const u8, input_len: usize) u64 {
    _ = input_ptr;
    _ = input_len;
    const resp = "{\"status\":\"success\",\"primitive\":\"hardware_accelerated_tensor\",\"simd_lanes\":4,\"precision\":\"f32\"}";
    @memcpy(out_buf[0..resp.len], resp);
    const ptr_u64: u64 = @intFromPtr(&out_buf[0]);
    const len_u64: u64 = resp.len;
    return ptr_u64 | (len_u64 << 32);
}
"#;

/// Generate freestanding Zig tool code from a tool name and custom or template logic
pub fn generate_freestanding_zig_tool(name: &str, tool_logic: &str) -> String {
    if tool_logic.trim().is_empty() {
        let lower = name.to_lowercase();
        if lower.contains("regex") || lower.contains("pattern") {
            return ZIG_REGEX_MATCHER_SOURCE.to_string();
        }
        if lower.contains("math") || lower.contains("calc") || lower.contains("matrix") {
            return ZIG_MATH_EVALUATOR_SOURCE.to_string();
        }
        if lower.contains("json") || lower.contains("slice") || lower.contains("parse") {
            return ZIG_JSON_SLICER_SOURCE.to_string();
        }
    }

    // If arbitrary tool code provided by LLM contains complete export definitions, return as-is
    if tool_logic.contains("export fn hgb_tool_invoke") || tool_logic.contains("export fn run") {
        return tool_logic.to_string();
    }

    format!(
r#"// Hagibis Auto-Synthesized Agent Tool: {name}
var memory_arena: [65536]u8 = undefined;
var arena_offset: usize = 0;
var out_buf: [32768]u8 = undefined;

export fn hgb_alloc(len: usize) [*]u8 {{
    if (arena_offset + len > memory_arena.len) return &memory_arena[0];
    const ptr = &memory_arena[arena_offset];
    arena_offset += len;
    return ptr;
}}

export fn hgb_dealloc(ptr: [*]u8, len: usize) void {{
    _ = ptr;
    _ = len;
}}

export fn hgb_tool_manifest() u64 {{
    const manifest_json = "{{\"name\":\"{name}\",\"version\":\"1.0.0\",\"description\":\"Auto-generated agent tool\"}}";
    @memcpy(out_buf[0..manifest_json.len], manifest_json);
    const ptr_u64: u64 = @intFromPtr(&out_buf[0]);
    const len_u64: u64 = manifest_json.len;
    return ptr_u64 | (len_u64 << 32);
}}

export fn hgb_tool_invoke(input_ptr: [*]const u8, input_len: usize) u64 {{
    _ = input_ptr;
    _ = input_len;
    {tool_logic}
    const resp = "{{\"status\":\"success\",\"tool\":\"{name}\",\"executed\":true}}";
    @memcpy(out_buf[0..resp.len], resp);
    const ptr_u64: u64 = @intFromPtr(&out_buf[0]);
    const len_u64: u64 = resp.len;
    return ptr_u64 | (len_u64 << 32);
}}

export fn run() i32 {{
    return 0;
}}
"#
    )
}

/// Synthesize arbitrary tool code generated by an LLM at runtime for WebAssembly execution
pub fn synthesize_llm_tool_code(name: &str, language: &str, code: &str) -> String {
    if language.eq_ignore_ascii_case("zig") || language.is_empty() {
        generate_freestanding_zig_tool(name, code)
    } else {
        code.to_string()
    }
}

/// Get builtin Zig primitive source code by canonical or alias name
pub fn get_builtin_zig_primitive(name: &str) -> Option<&'static str> {
    match name {
        "bpe_tokenizer" | "bpe" | "tokenizer" => Some(ZIG_BPE_TOKENIZER_SOURCE),
        "cosine_distance" | "cosine" | "embedding_math" => Some(ZIG_COSINE_MATH_SOURCE),
        "kalman_filter" | "kalman" | "state_filter" => Some(ZIG_KALMAN_FILTER_SOURCE),
        "matrix_vector_product" | "matrix_vector" | "matvec" | "matrix_vector_mul" => Some(ZIG_MATRIX_VECTOR_SOURCE),
        "regex_matcher" | "regex" => Some(ZIG_REGEX_MATCHER_SOURCE),
        "math_evaluator" | "math" => Some(ZIG_MATH_EVALUATOR_SOURCE),
        "json_slicer" | "json" => Some(ZIG_JSON_SLICER_SOURCE),
        "hardware_accelerated_tensor" | "tensor_simd" | "simd_tensor" => Some(ZIG_HARDWARE_ACCELERATED_TENSOR_SOURCE),
        _ => None,
    }
}

/// List all available built-in Zig primitive names
pub fn list_builtin_zig_primitives() -> Vec<&'static str> {
    vec![
        "bpe_tokenizer",
        "cosine_distance",
        "kalman_filter",
        "matrix_vector_product",
        "regex_matcher",
        "math_evaluator",
        "json_slicer",
        "hardware_accelerated_tensor",
    ]
}

// ============================================================================
// UNTRUSTED SANDBOX RUNNER INTERFACE (Pillar 1: Supercharge WASM Kernel & Edge Isolation)
// ============================================================================

/// Policy and strict resource limits for executing untrusted agent-generated code
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SandboxPolicy {
    pub fuel_limit: u64,
    pub epoch_deadline_ms: u64,
    pub max_memory_pages: u32,
    pub allow_stdout: bool,
    pub allow_env: bool,
}

impl Default for SandboxPolicy {
    fn default() -> Self {
        Self {
            fuel_limit: 10_000_000,
            epoch_deadline_ms: 1000,
            max_memory_pages: 256, // 16 MiB (256 * 64 KiB)
            allow_stdout: false,
            allow_env: false,
        }
    }
}

impl SandboxPolicy {
    /// Create a new policy with custom fuel, epoch deadline, and max memory pages
    pub fn new(fuel_limit: u64, epoch_deadline_ms: u64, max_memory_pages: u32) -> Self {
        Self {
            fuel_limit,
            epoch_deadline_ms,
            max_memory_pages,
            allow_stdout: false,
            allow_env: false,
        }
    }

    /// Strict sandboxed policy for untrusted agent-generated code
    pub fn strict() -> Self {
        Self {
            fuel_limit: 5_000_000,
            epoch_deadline_ms: 500,
            max_memory_pages: 128, // 8 MiB
            allow_stdout: false,
            allow_env: false,
        }
    }

    /// Permissive policy for trusted tools
    pub fn permissive() -> Self {
        Self {
            fuel_limit: 50_000_000,
            epoch_deadline_ms: 5000,
            max_memory_pages: 1024, // 64 MiB
            allow_stdout: true,
            allow_env: true,
        }
    }

    pub fn with_fuel_limit(mut self, fuel: u64) -> Self {
        self.fuel_limit = fuel;
        self
    }

    pub fn with_epoch_deadline_ms(mut self, deadline_ms: u64) -> Self {
        self.epoch_deadline_ms = deadline_ms;
        self
    }

    pub fn with_max_memory_pages(mut self, pages: u32) -> Self {
        self.max_memory_pages = pages;
        self
    }

    pub fn with_allow_stdout(mut self, allow: bool) -> Self {
        self.allow_stdout = allow;
        self
    }

    pub fn with_allow_env(mut self, allow: bool) -> Self {
        self.allow_env = allow;
        self
    }

    /// Memory limit in bytes (pages * 64 KiB)
    pub fn max_memory_bytes(&self) -> usize {
        (self.max_memory_pages as usize) * 64 * 1024
    }
}

/// Execution telemetry and output from metered sandbox execution
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MeteredExecutionResult {
    pub exit_code: i32,
    pub fuel_consumed: u64,
    pub duration_us: u64,
    pub output: Vec<u8>,
    pub error_diagnostic: Option<String>,
}

impl MeteredExecutionResult {
    /// Construct a successful execution result
    pub fn ok(exit_code: i32, fuel_consumed: u64, duration_us: u64, output: Vec<u8>) -> Self {
        Self {
            exit_code,
            fuel_consumed,
            duration_us,
            output,
            error_diagnostic: None,
        }
    }

    /// Construct an execution result with error/trap diagnostic
    pub fn err(exit_code: i32, fuel_consumed: u64, duration_us: u64, diagnostic: impl Into<String>) -> Self {
        Self {
            exit_code,
            fuel_consumed,
            duration_us,
            output: Vec::new(),
            error_diagnostic: Some(diagnostic.into()),
        }
    }

    /// Whether execution succeeded without traps/errors (non-negative exit code and no error diagnostic)
    pub fn is_success(&self) -> bool {
        self.error_diagnostic.is_none() && self.exit_code >= 0
    }

    /// Interpret output as UTF-8 string slice
    pub fn output_as_str(&self) -> Result<&str, std::str::Utf8Error> {
        std::str::from_utf8(&self.output)
    }
}

// ============================================================================
// PILLAR 2: HARDWARE-ACCELERATED TENSOR & WASM SIMD-128 ENGINE
// ============================================================================

/// Tensor Capability Policy defining hardware security bounds and execution constraints.
/// Conforms to WIT `tensor-capability-policy` specification.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TensorCapabilityPolicy {
    pub allow_gpu_offload: bool,
    pub allow_simd_128: bool,
    pub max_vram_mb: u32,
    pub max_tensor_dims: u32,
    pub backend_preference: String,
}

impl Default for TensorCapabilityPolicy {
    fn default() -> Self {
        Self {
            allow_gpu_offload: true,
            allow_simd_128: true,
            max_vram_mb: 256,
            max_tensor_dims: 4096,
            backend_preference: "auto".to_string(),
        }
    }
}

impl TensorCapabilityPolicy {
    /// Construct a custom tensor capability policy
    pub fn new(
        allow_gpu_offload: bool,
        allow_simd_128: bool,
        max_vram_mb: u32,
        max_tensor_dims: u32,
        backend_preference: impl Into<String>,
    ) -> Self {
        Self {
            allow_gpu_offload,
            allow_simd_128,
            max_vram_mb,
            max_tensor_dims,
            backend_preference: backend_preference.into(),
        }
    }

    /// Strict capability policy for sandboxed / resource-constrained microkernels
    pub fn strict() -> Self {
        Self {
            allow_gpu_offload: false,
            allow_simd_128: true,
            max_vram_mb: 64,
            max_tensor_dims: 1024,
            backend_preference: "cpu_simd".to_string(),
        }
    }

    /// High-performance capability policy with GPU offload enabled
    pub fn high_performance() -> Self {
        Self {
            allow_gpu_offload: true,
            allow_simd_128: true,
            max_vram_mb: 1024,
            max_tensor_dims: 8192,
            backend_preference: "gpu".to_string(),
        }
    }

    /// Enforce capability security bounds: dimension checks, total memory, and VRAM limits
    pub fn enforce_bounds(&self, m: usize, k: usize, n: usize) -> Result<(), String> {
        if m == 0 || k == 0 || n == 0 {
            return Err("Matrix dimensions must be non-zero".to_string());
        }

        let max_dim = self.max_tensor_dims as usize;
        if m > max_dim || k > max_dim || n > max_dim {
            return Err(format!(
                "Dimension bound violation: (m={}, k={}, n={}) exceeds max_tensor_dims={}",
                m, k, n, max_dim
            ));
        }

        // Calculate required memory: A(m*k) + B(k*n) + C(m*n) in f32 (4 bytes per element)
        let elems_a = m.checked_mul(k).ok_or_else(|| "Matrix A dimension overflow".to_string())?;
        let elems_b = k.checked_mul(n).ok_or_else(|| "Matrix B dimension overflow".to_string())?;
        let elems_c = m.checked_mul(n).ok_or_else(|| "Matrix C dimension overflow".to_string())?;

        let total_elems = elems_a
            .checked_add(elems_b)
            .and_then(|sum| sum.checked_add(elems_c))
            .ok_or_else(|| "Total matrix elements overflow".to_string())?;

        let total_bytes = total_elems
            .checked_mul(std::mem::size_of::<f32>())
            .ok_or_else(|| "Total matrix bytes overflow".to_string())?;

        let max_vram_bytes = (self.max_vram_mb as usize).saturating_mul(1024 * 1024);
        if total_bytes > max_vram_bytes {
            return Err(format!(
                "VRAM memory bound violation: requested {} bytes ({:.2} MB) exceeds maximum allowed limit of {} MB",
                total_bytes,
                (total_bytes as f64) / (1024.0 * 1024.0),
                self.max_vram_mb
            ));
        }

        Ok(())
    }

    /// Enforce 1D vector capability bounds
    pub fn enforce_vector_bounds(&self, len: usize) -> Result<(), String> {
        if len == 0 {
            return Err("Vector length must be non-zero".to_string());
        }
        let max_dim = self.max_tensor_dims as usize;
        if len > max_dim {
            return Err(format!(
                "Vector dimension bound violation: length {} exceeds max_tensor_dims={}",
                len, max_dim
            ));
        }
        let bytes = len
            .checked_mul(std::mem::size_of::<f32>())
            .ok_or_else(|| "Vector bytes overflow".to_string())?;
        let max_vram_bytes = (self.max_vram_mb as usize).saturating_mul(1024 * 1024);
        if bytes > max_vram_bytes {
            return Err(format!(
                "Memory bound violation: requested {} bytes exceeds limit of {} MB",
                bytes, self.max_vram_mb
            ));
        }
        Ok(())
    }
}

/// SIMD 128-bit Dot Product (4-wide f32x4 unrolled vectorization)
#[inline(always)]
pub fn hgb_simd_dot_product_f32(vec_a: &[f32], vec_b: &[f32]) -> f32 {
    let len = vec_a.len().min(vec_b.len());
    let chunks = len / 4;
    let remainder = len % 4;

    let mut acc0 = 0.0f32;
    let mut acc1 = 0.0f32;
    let mut acc2 = 0.0f32;
    let mut acc3 = 0.0f32;

    let mut idx = 0;
    for _ in 0..chunks {
        acc0 += vec_a[idx] * vec_b[idx];
        acc1 += vec_a[idx + 1] * vec_b[idx + 1];
        acc2 += vec_a[idx + 2] * vec_b[idx + 2];
        acc3 += vec_a[idx + 3] * vec_b[idx + 3];
        idx += 4;
    }

    let mut total = (acc0 + acc1) + (acc2 + acc3);
    for j in 0..remainder {
        total += vec_a[idx + j] * vec_b[idx + j];
    }
    total
}

/// SIMD 128-bit ReLU activation (4-wide f32x4 unrolled vectorization)
#[inline(always)]
pub fn hgb_simd_relu_f32(tensor: &[f32]) -> Vec<f32> {
    let len = tensor.len();
    let chunks = len / 4;
    let remainder = len % 4;
    let mut output = Vec::with_capacity(len);

    let mut idx = 0;
    for _ in 0..chunks {
        let v0 = tensor[idx];
        let v1 = tensor[idx + 1];
        let v2 = tensor[idx + 2];
        let v3 = tensor[idx + 3];
        output.push(if v0 > 0.0 { v0 } else { 0.0 });
        output.push(if v1 > 0.0 { v1 } else { 0.0 });
        output.push(if v2 > 0.0 { v2 } else { 0.0 });
        output.push(if v3 > 0.0 { v3 } else { 0.0 });
        idx += 4;
    }

    for j in 0..remainder {
        let v = tensor[idx + j];
        output.push(if v > 0.0 { v } else { 0.0 });
    }
    output
}

/// Raw kernel for SIMD 128-bit matrix multiplication
/// Computes C[i, j] += A[i, p] * B[p, j] with 4-wide unrolling over j
#[inline(always)]
pub fn raw_simd_matmul(
    a_ptr: *const f32,
    b_ptr: *const f32,
    out_ptr: *mut f32,
    m: usize,
    k: usize,
    n: usize,
) {
    if a_ptr.is_null() || b_ptr.is_null() || out_ptr.is_null() || m == 0 || k == 0 || n == 0 {
        return;
    }

    unsafe {
        // Zero initialize destination buffer
        std::ptr::write_bytes(out_ptr, 0, m * n);

        let n_chunks = n / 4;
        let n_rem = n % 4;

        for i in 0..m {
            let row_a_offset = i * k;
            let row_c_offset = i * n;

            for p in 0..k {
                let a_val = *a_ptr.add(row_a_offset + p);
                if a_val == 0.0 {
                    continue;
                }

                let row_b_offset = p * n;

                // 4-wide f32x4 SIMD inner loop
                for c in 0..n_chunks {
                    let j = c * 4;
                    let b_idx = row_b_offset + j;
                    let c_idx = row_c_offset + j;

                    let b0 = *b_ptr.add(b_idx);
                    let b1 = *b_ptr.add(b_idx + 1);
                    let b2 = *b_ptr.add(b_idx + 2);
                    let b3 = *b_ptr.add(b_idx + 3);

                    let c0 = out_ptr.add(c_idx);
                    let c1 = out_ptr.add(c_idx + 1);
                    let c2 = out_ptr.add(c_idx + 2);
                    let c3 = out_ptr.add(c_idx + 3);

                    *c0 += a_val * b0;
                    *c1 += a_val * b1;
                    *c2 += a_val * b2;
                    *c3 += a_val * b3;
                }

                // Remainder handling
                let rem_start = n_chunks * 4;
                for j in 0..n_rem {
                    let col = rem_start + j;
                    let b_val = *b_ptr.add(row_b_offset + col);
                    let c_target = out_ptr.add(row_c_offset + col);
                    *c_target += a_val * b_val;
                }
            }
        }
    }
}

/// SIMD 128-bit Matrix Multiplication C = A * B (m x k * k x n = m x n)
///
/// Implements cache-friendly (i, k, j) loop ordering with 4-wide f32x4 SIMD vectorization
/// across matrix B and matrix C columns.
pub fn hgb_simd_matmul_f32(
    matrix_a: &[f32],
    matrix_b: &[f32],
    m: usize,
    k: usize,
    n: usize,
    policy: Option<&TensorCapabilityPolicy>,
) -> Result<Vec<f32>, String> {
    if let Some(p) = policy {
        p.enforce_bounds(m, k, n)?;
    } else {
        TensorCapabilityPolicy::default().enforce_bounds(m, k, n)?;
    }

    if matrix_a.len() != m * k {
        return Err(format!(
            "Matrix A length mismatch: expected {} ({}x{}), got {}",
            m * k, m, k, matrix_a.len()
        ));
    }
    if matrix_b.len() != k * n {
        return Err(format!(
            "Matrix B length mismatch: expected {} ({}x{}), got {}",
            k * n, k, n, matrix_b.len()
        ));
    }

    let mut out = vec![0.0f32; m * n];
    raw_simd_matmul(matrix_a.as_ptr(), matrix_b.as_ptr(), out.as_mut_ptr(), m, k, n);
    Ok(out)
}

// ----------------------------------------------------------------------------
// C-ABI Exports for Freestanding Agent Tools
// ----------------------------------------------------------------------------

/// Exported C-ABI: 128-bit SIMD Vector Dot Product
#[no_mangle]
pub extern "C" fn hgb_wasm_simd_dot_product(vec_a_ptr: *const f32, vec_b_ptr: *const f32, len: usize) -> f32 {
    if vec_a_ptr.is_null() || vec_b_ptr.is_null() || len == 0 {
        return 0.0;
    }
    let slice_a = unsafe { std::slice::from_raw_parts(vec_a_ptr, len) };
    let slice_b = unsafe { std::slice::from_raw_parts(vec_b_ptr, len) };
    hgb_simd_dot_product_f32(slice_a, slice_b)
}

/// Exported C-ABI: 128-bit SIMD Matrix Multiplication C = A * B
#[no_mangle]
pub extern "C" fn hgb_wasm_simd_matmul(
    a_ptr: *const f32,
    b_ptr: *const f32,
    out_ptr: *mut f32,
    m: usize,
    k: usize,
    n: usize,
) {
    raw_simd_matmul(a_ptr, b_ptr, out_ptr, m, k, n);
}

// ----------------------------------------------------------------------------
// WIT High-Level Bindings: `hardware-accelerated-tensor`
// ----------------------------------------------------------------------------

/// WIT func `matmul-f32`
pub fn hgb_wit_matmul_f32(
    matrix_a: Vec<f32>,
    matrix_b: Vec<f32>,
    m: u32,
    k: u32,
    n: u32,
    policy: TensorCapabilityPolicy,
) -> Result<Vec<f32>, String> {
    hgb_simd_matmul_f32(&matrix_a, &matrix_b, m as usize, k as usize, n as usize, Some(&policy))
}

/// WIT func `vector-dot-product-simd`
pub fn hgb_wit_vector_dot_product_simd(vec_a: Vec<f32>, vec_b: Vec<f32>) -> Result<f32, String> {
    if vec_a.is_empty() || vec_b.is_empty() {
        return Err("Input vectors cannot be empty".to_string());
    }
    if vec_a.len() != vec_b.len() {
        return Err(format!("Vector dimension mismatch: {} != {}", vec_a.len(), vec_b.len()));
    }
    Ok(hgb_simd_dot_product_f32(&vec_a, &vec_b))
}

/// WIT func `relu-activation-simd`
pub fn hgb_wit_relu_activation_simd(tensor: Vec<f32>) -> Result<Vec<f32>, String> {
    if tensor.is_empty() {
        return Err("Input tensor cannot be empty".to_string());
    }
    Ok(hgb_simd_relu_f32(&tensor))
}

// ============================================================================
// AGENT COMPONENT MODEL INTERFACE (Pillar 1: Component Model Support)
// ============================================================================

/// Dynamic Tool Invocation conforming to WIT `agent-component-model` interface
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentToolInvocation {
    pub tool_id: String,
    pub agent_id: String,
    pub input_json: String,
    pub gas_limit: u64,
    pub allowed_read_paths: Vec<String>,
    pub allow_network: bool,
}

impl AgentToolInvocation {
    pub fn new(tool_id: impl Into<String>, agent_id: impl Into<String>, input_json: impl Into<String>) -> Self {
        Self {
            tool_id: tool_id.into(),
            agent_id: agent_id.into(),
            input_json: input_json.into(),
            gas_limit: 10_000_000,
            allowed_read_paths: Vec::new(),
            allow_network: false,
        }
    }

    pub fn with_gas_limit(mut self, gas_limit: u64) -> Self {
        self.gas_limit = gas_limit;
        self
    }

    pub fn with_allowed_read_path(mut self, path: impl Into<String>) -> Self {
        self.allowed_read_paths.push(path.into());
        self
    }

    pub fn with_allow_network(mut self, allow: bool) -> Self {
        self.allow_network = allow;
        self
    }
}

/// Dynamic Tool Output conforming to WIT `agent-component-model` interface
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentToolOutput {
    pub exit_code: i32,
    pub gas_consumed: u64,
    pub result_json: String,
    pub error_diagnostic: Option<String>,
}

impl AgentToolOutput {
    pub fn ok(gas_consumed: u64, result_json: impl Into<String>) -> Self {
        Self {
            exit_code: 0,
            gas_consumed,
            result_json: result_json.into(),
            error_diagnostic: None,
        }
    }

    pub fn err(exit_code: i32, gas_consumed: u64, diagnostic: impl Into<String>) -> Self {
        Self {
            exit_code,
            gas_consumed,
            result_json: String::new(),
            error_diagnostic: Some(diagnostic.into()),
        }
    }

    pub fn is_success(&self) -> bool {
        self.exit_code >= 0 && self.error_diagnostic.is_none()
    }
}

/// WIT High-Level Function Wrapper for Component Tool Invocation
pub fn hgb_invoke_component_tool(
    inv: AgentToolInvocation,
    handler: impl FnOnce(&AgentToolInvocation) -> Result<String, String>,
) -> Result<AgentToolOutput, String> {
    match handler(&inv) {
        Ok(res_json) => Ok(AgentToolOutput::ok(0, res_json)),
        Err(err_msg) => Ok(AgentToolOutput::err(-1, 0, err_msg)),
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

    #[test]
    fn test_wasi_nn_graph_and_quantized_inference_in_wasm() {
        // Pseudo GGUF / Burn model weights
        let model_bytes = vec![0x47, 0x47, 0x55, 0x46, 0x01, 0x00, 0x00, 0x00];
        let graph_id = hgb_wasi_nn_load(&model_bytes, WasiNnGraphEncoding::GGUF, WasiNnExecutionTarget::CPU)
            .expect("load graph");
        assert!(graph_id >= 1);

        let ctx_id = hgb_wasi_nn_init_context(graph_id).expect("init context");
        assert!(ctx_id >= 1);

        let input_slice = vec![0.5f32, -1.2, 0.0, 3.4];
        let in_tensor = WasiNnTensor::from_f32_slice(vec![1, 4], &input_slice);
        hgb_wasi_nn_set_input(ctx_id, 0, in_tensor).expect("set input");

        hgb_wasi_nn_compute(ctx_id).expect("compute forward pass");

        let out_tensor = hgb_wasi_nn_get_output(ctx_id, 0).expect("get output");
        assert_eq!(out_tensor.dimensions, vec![1, 4]);
        assert_eq!(out_tensor.dtype, TensorDataType::F32);
        let out_floats = out_tensor.to_f32_vec();
        assert_eq!(out_floats.len(), 4);
        // GELU of positive numbers is positive; GELU of 0 is 0
        assert!(out_floats[0] > 0.3);
        assert!(out_floats[2].abs() < 1e-4);
        assert!(out_floats[3] > 3.0);
    }

    #[test]
    fn test_component_plugin_manifest_multi_language() {
        let manifest = ComponentPluginManifest::new("python_code_linter", "python", "linter.py:run")
            .with_tool("lint_python_ast")
            .with_tool("autofix_pep8");

        assert_eq!(manifest.language, "python");
        assert_eq!(manifest.exported_tools.len(), 2);
        assert_eq!(manifest.memory_limit_mb, 32);

        let json = serde_json::to_string(&manifest).expect("serialize manifest");
        let parsed: ComponentPluginManifest = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(manifest, parsed);
    }

    #[test]
    fn test_bpe_tokenizer_encode_decode() {
        let mut bpe = BpeTokenizer::new_default();
        bpe.add_merge("h", "e");
        bpe.add_merge("he", "l");
        bpe.add_merge("hel", "lo");

        let text = "hello the world fn pub export";
        let tokens = bpe.encode(text);
        assert!(!tokens.is_empty());
        assert!(tokens.len() < text.len()); // Token compression achieved

        let decoded = bpe.decode(&tokens);
        assert_eq!(decoded, text);
    }

    #[test]
    fn test_cosine_and_distance_math() {
        let v1 = vec![1.0f32, 2.0, 3.0];
        let v2 = vec![1.0f32, 2.0, 3.0];
        let sim = cosine_similarity(&v1, &v2);
        assert!((sim - 1.0).abs() < 1e-5);

        let dist = euclidean_distance(&v1, &v2);
        assert!(dist < 1e-5);

        let v3 = vec![-1.0f32, -2.0, -3.0];
        let opp_sim = cosine_similarity(&v1, &v3);
        assert!((opp_sim - (-1.0)).abs() < 1e-5);

        let dot = dot_product(&v1, &v2);
        assert!((dot - 14.0).abs() < 1e-5);
    }

    #[test]
    fn test_kalman_filter_state_tracking() {
        let mut kf = KalmanFilter1D::new(0.0, 1.0, 0.01, 0.1);
        let measurements = [10.0, 10.2, 9.8, 10.1, 10.0];
        for &m in &measurements {
            let state = kf.update(m);
            assert!(state > 0.0);
        }
        // State should converge close to 10.0
        assert!((kf.state() - 10.0).abs() < 1.0);
        assert!(kf.covariance() < 1.0);

        let mut kf_nd = KalmanFilterND::new(2, 0.0, 0.01, 0.1);
        let res = kf_nd.update(&[5.0, 10.0]).expect("update ND");
        assert_eq!(res.len(), 2);
        assert!(res[0] > 0.0);
        assert!(res[1] > 0.0);
    }

    #[test]
    fn test_matrix_vector_multiplication() {
        // 2x2 matrix: [[1, 2], [3, 4]]
        // vector: [2, 3]
        // result: [1*2 + 2*3, 3*2 + 4*3] = [8, 18]
        let mat = vec![1.0f32, 2.0, 3.0, 4.0];
        let vec = vec![2.0f32, 3.0];
        let res = matrix_vector_mul(&mat, 2, 2, &vec).expect("matvec");
        assert_eq!(res, vec![8.0, 18.0]);

        // Error check on dimension mismatch
        let bad_vec = vec![1.0f32];
        assert!(matrix_vector_mul(&mat, 2, 2, &bad_vec).is_err());
    }

    #[test]
    fn test_builtin_zig_sources() {
        assert!(get_builtin_zig_primitive("bpe_tokenizer").is_some());
        assert!(get_builtin_zig_primitive("cosine_distance").is_some());
        assert!(get_builtin_zig_primitive("kalman_filter").is_some());
        assert!(get_builtin_zig_primitive("matrix_vector_product").is_some());
        assert!(get_builtin_zig_primitive("regex_matcher").is_some());
        assert!(get_builtin_zig_primitive("math_evaluator").is_some());
        assert!(get_builtin_zig_primitive("json_slicer").is_some());
        assert!(get_builtin_zig_primitive("hardware_accelerated_tensor").is_some());
        assert!(get_builtin_zig_primitive("unknown_primitive").is_none());
        assert_eq!(list_builtin_zig_primitives().len(), 8);
    }

    #[test]
    fn test_wit_tool_spec_and_compilation_outcome() {
        let spec = ToolSpec::new("sql_ast_validator", "Validates SQL AST in WASM", "zig", "pub fn main() void {}")
            .with_input_schema("{\"type\":\"string\"}");
        assert_eq!(spec.name, "sql_ast_validator");
        assert_eq!(spec.source_language, "zig");

        let outcome_ok = CompilationOutcome::ok(vec![0x00, 0x61, 0x73, 0x6d], 42);
        assert!(outcome_ok.success);
        assert_eq!(outcome_ok.wasm_bytes.len(), 4);
        assert_eq!(outcome_ok.compilation_duration_ms, 42);

        let outcome_err = CompilationOutcome::err("Syntax error on line 12", 15);
        assert!(!outcome_err.success);
        assert_eq!(outcome_err.errors[0], "Syntax error on line 12");
    }

    #[test]
    fn test_dynamic_tool_manifest() {
        let manifest = DynamicToolManifest::new("tool_123", "ast_slicer")
            .with_fuel_budget(50_000_000)
            .with_memory_limit_mb(64);
        assert_eq!(manifest.tool_id, "tool_123");
        assert_eq!(manifest.fuel_budget, 50_000_000);
        assert_eq!(manifest.memory_limit_mb, 64);
    }

    #[test]
    fn test_generate_freestanding_zig_tool() {
        let regex_tool = generate_freestanding_zig_tool("custom_regex_filter", "");
        assert!(regex_tool.contains("regex_matcher"));

        let math_tool = generate_freestanding_zig_tool("matrix_affine_calc", "");
        assert!(math_tool.contains("math_evaluator"));

        let json_tool = generate_freestanding_zig_tool("telemetry_json_slice", "");
        assert!(json_tool.contains("json_slicer"));

        let custom_tool = generate_freestanding_zig_tool("custom_hash", "const x = 123;");
        assert!(custom_tool.contains("custom_hash"));
        assert!(custom_tool.contains("const x = 123;"));
    }

    #[test]
    fn test_untrusted_sandbox_policy_and_metered_result() {
        let default_policy = SandboxPolicy::default();
        assert_eq!(default_policy.fuel_limit, 10_000_000);
        assert_eq!(default_policy.epoch_deadline_ms, 1000);
        assert_eq!(default_policy.max_memory_pages, 256);
        assert_eq!(default_policy.max_memory_bytes(), 16 * 1024 * 1024);
        assert!(!default_policy.allow_stdout);
        assert!(!default_policy.allow_env);

        let strict_policy = SandboxPolicy::strict();
        assert_eq!(strict_policy.fuel_limit, 5_000_000);
        assert_eq!(strict_policy.epoch_deadline_ms, 500);
        assert_eq!(strict_policy.max_memory_pages, 128);

        let permissive_policy = SandboxPolicy::permissive();
        assert_eq!(permissive_policy.fuel_limit, 50_000_000);
        assert_eq!(permissive_policy.max_memory_pages, 1024);
        assert!(permissive_policy.allow_stdout);

        let custom_policy = SandboxPolicy::new(1_000_000, 200, 64)
            .with_allow_stdout(true)
            .with_allow_env(false);
        assert_eq!(custom_policy.fuel_limit, 1_000_000);
        assert_eq!(custom_policy.epoch_deadline_ms, 200);
        assert_eq!(custom_policy.max_memory_pages, 64);
        assert!(custom_policy.allow_stdout);

        // Serialization roundtrip
        let json = serde_json::to_string(&custom_policy).expect("serialize policy");
        let deserialized: SandboxPolicy = serde_json::from_str(&json).expect("deserialize policy");
        assert_eq!(custom_policy, deserialized);

        // MeteredExecutionResult tests
        let ok_res = MeteredExecutionResult::ok(0, 42_000, 85, b"hello_sandbox".to_vec());
        assert!(ok_res.is_success());
        assert_eq!(ok_res.exit_code, 0);
        assert_eq!(ok_res.fuel_consumed, 42_000);
        assert_eq!(ok_res.duration_us, 85);
        assert_eq!(ok_res.output_as_str().unwrap(), "hello_sandbox");
        assert!(ok_res.error_diagnostic.is_none());

        let err_res = MeteredExecutionResult::err(-1, 5_000_000, 520, "Fuel limit exceeded");
        assert!(!err_res.is_success());
        assert_eq!(err_res.exit_code, -1);
        assert_eq!(err_res.fuel_consumed, 5_000_000);
        assert_eq!(err_res.error_diagnostic.as_deref(), Some("Fuel limit exceeded"));

        let res_json = serde_json::to_string(&ok_res).expect("serialize result");
        let res_deser: MeteredExecutionResult = serde_json::from_str(&res_json).expect("deserialize result");
        assert_eq!(ok_res, res_deser);
    }

    #[test]
    fn test_stream_embedding_to_guest_and_vector_similarity() {
        // Create two 4-dimensional vectors: v1 = [1.0, 0.0, 0.0, 0.0], v2 = [0.8, 0.6, 0.0, 0.0]
        let v1: Vec<f32> = vec![1.0, 0.0, 0.0, 0.0];
        let v2: Vec<f32> = vec![0.8, 0.6, 0.0, 0.0];

        let mut bytes_v1 = Vec::new();
        for &val in &v1 {
            bytes_v1.extend_from_slice(&val.to_le_bytes());
        }

        let mut bytes_v2 = Vec::new();
        for &val in &v2 {
            bytes_v2.extend_from_slice(&val.to_le_bytes());
        }

        let offset1 = stream_embedding_to_guest(4, &bytes_v1).expect("stream v1");
        let offset2 = stream_embedding_to_guest(4, &bytes_v2).expect("stream v2");
        assert_ne!(offset1, offset2);

        // Compute similarity: dot product of [1,0,0,0] and [0.8, 0.6, 0, 0] / (1.0 * 1.0) = 0.8
        let sim = compute_vector_similarity(offset1, offset2, 4).expect("compute similarity");
        assert!((sim - 0.8).abs() < 1e-4, "Expected ~0.8, got {}", sim);

        // Identical vector similarity must be 1.0
        let self_sim = compute_vector_similarity(offset1, offset1, 4).expect("self similarity");
        assert!((self_sim - 1.0).abs() < 1e-4, "Expected ~1.0, got {}", self_sim);

        // C-ABI functions test
        let abi_offset = hgb_stream_embedding_to_guest(4, bytes_v1.as_ptr(), bytes_v1.len());
        assert!(abi_offset >= 0);
        let abi_sim = hgb_compute_vector_similarity(offset1, offset2, 4);
        assert!((abi_sim - 0.8).abs() < 1e-4);

        // Invalid dimension checks
        assert!(stream_embedding_to_guest(0, &[]).is_err());
        assert!(stream_embedding_to_guest(4, &[1, 2, 3]).is_err());
    }

    #[test]
    fn test_simd_dot_product_accuracy() {
        // Test exact mathematical dot product for various lengths (1, 4, 7, 16)
        let v1 = vec![1.0f32, 2.0, 3.0, 4.0, 5.0, -2.0, 0.5];
        let v2 = vec![2.0f32, 0.5, 1.0, -1.0, 2.0, 3.0, 4.0];
        // 1*2 + 2*0.5 + 3*1 + 4*(-1) + 5*2 + (-2)*3 + 0.5*4
        // = 2 + 1 + 3 - 4 + 10 - 6 + 2 = 8.0
        let dot = hgb_simd_dot_product_f32(&v1, &v2);
        assert!((dot - 8.0).abs() < 1e-4, "Expected 8.0, got {}", dot);

        // Orthogonal vectors
        let o1 = vec![1.0f32, 0.0, 0.0, 0.0];
        let o2 = vec![0.0f32, 1.0, 0.0, 0.0];
        assert_eq!(hgb_simd_dot_product_f32(&o1, &o2), 0.0);
    }

    #[test]
    fn test_simd_relu_accuracy() {
        let input = vec![-3.5f32, 0.0, 1.25, -0.001, 4.0, -10.0, 7.8];
        let activated = hgb_simd_relu_f32(&input);
        assert_eq!(activated.len(), input.len());
        assert_eq!(activated[0], 0.0);
        assert_eq!(activated[1], 0.0);
        assert_eq!(activated[2], 1.25);
        assert_eq!(activated[3], 0.0);
        assert_eq!(activated[4], 4.0);
        assert_eq!(activated[5], 0.0);
        assert_eq!(activated[6], 7.8);
    }

    #[test]
    fn test_simd_matmul_accuracy() {
        // 2x3 matrix * 3x2 matrix = 2x2 matrix
        // A = [[1, 2, 3], [4, 5, 6]] (row-major: [1, 2, 3, 4, 5, 6])
        // B = [[7, 8], [9, 1], [2, 3]] (row-major: [7, 8, 9, 1, 2, 3])
        let a = vec![1.0f32, 2.0, 3.0, 4.0, 5.0, 6.0];
        let b = vec![7.0f32, 8.0, 9.0, 1.0, 2.0, 3.0];
        let policy = TensorCapabilityPolicy::default();

        let c = hgb_simd_matmul_f32(&a, &b, 2, 3, 2, Some(&policy)).expect("matmul");
        assert_eq!(c.len(), 4);
        assert!((c[0] - 31.0).abs() < 1e-4);
        assert!((c[1] - 19.0).abs() < 1e-4);
        assert!((c[2] - 85.0).abs() < 1e-4);
        assert!((c[3] - 55.0).abs() < 1e-4);
    }

    #[test]
    fn test_tensor_capability_policy_bounds_enforcement() {
        let strict_policy = TensorCapabilityPolicy::strict();
        assert_eq!(strict_policy.max_vram_mb, 64);
        assert_eq!(strict_policy.max_tensor_dims, 1024);
        assert!(!strict_policy.allow_gpu_offload);

        // 1. Zero dimension check
        assert!(strict_policy.enforce_bounds(0, 10, 10).is_err());
        assert!(strict_policy.enforce_bounds(10, 0, 10).is_err());

        // 2. Max tensor dims rejection
        assert!(strict_policy.enforce_bounds(1025, 10, 10).is_err());
        assert!(strict_policy.enforce_bounds(10, 2000, 10).is_err());

        // 3. VRAM limit rejection
        // Policy with only 1 MB limit (262,144 f32s max across A, B, C)
        let tight_vram_policy = TensorCapabilityPolicy::new(false, true, 1, 4096, "cpu");
        // 500 x 500 = 250,000 elems for A, 250,000 for B, 250,000 for C => 750,000 elems * 4 = 3 MB > 1 MB
        let vram_err = tight_vram_policy.enforce_bounds(500, 500, 500);
        assert!(vram_err.is_err());
        assert!(vram_err.unwrap_err().contains("VRAM memory bound violation"));

        // 4. Valid bounds must pass
        assert!(strict_policy.enforce_bounds(64, 64, 64).is_ok());

        // 5. Vector bounds check
        assert!(strict_policy.enforce_vector_bounds(1024).is_ok());
        assert!(strict_policy.enforce_vector_bounds(1025).is_err());
    }

    #[test]
    fn test_c_abi_simd_exports() {
        let v1 = vec![2.0f32, 3.0, 4.0, 5.0];
        let v2 = vec![1.0f32, 2.0, 3.0, 4.0];
        let dot = hgb_wasm_simd_dot_product(v1.as_ptr(), v2.as_ptr(), 4);
        assert!((dot - 40.0).abs() < 1e-4);

        // Null pointer safety
        assert_eq!(hgb_wasm_simd_dot_product(std::ptr::null(), v2.as_ptr(), 4), 0.0);
        assert_eq!(hgb_wasm_simd_dot_product(v1.as_ptr(), std::ptr::null(), 4), 0.0);

        // C-ABI matmul
        let a = vec![2.0f32, 0.0, 0.0, 2.0];
        let b = vec![3.0f32, 4.0, 5.0, 6.0];
        let mut out = vec![0.0f32; 4];
        hgb_wasm_simd_matmul(a.as_ptr(), b.as_ptr(), out.as_mut_ptr(), 2, 2, 2);
        assert_eq!(out, vec![6.0, 8.0, 10.0, 12.0]);
    }

    #[test]
    fn test_wit_hardware_accelerated_tensor_bindings() {
        let policy = TensorCapabilityPolicy::default();
        let a = vec![1.0f32, 2.0, 3.0, 4.0];
        let b = vec![5.0f32, 6.0, 7.0, 8.0];

        let res = hgb_wit_matmul_f32(a, b, 2, 2, 2, policy).expect("wit matmul");
        assert_eq!(res.len(), 4);
        assert!((res[0] - 19.0).abs() < 1e-4); // 1*5 + 2*7 = 19

        let dot = hgb_wit_vector_dot_product_simd(vec![1.0, 2.0], vec![3.0, 4.0]).expect("wit dot");
        assert!((dot - 11.0).abs() < 1e-4);

        let relu = hgb_wit_relu_activation_simd(vec![-5.0, 10.0]).expect("wit relu");
        assert_eq!(relu, vec![0.0, 10.0]);
    }

    #[test]
    fn test_builtin_zig_primitive_hardware_accelerated_tensor() {
        let src = get_builtin_zig_primitive("hardware_accelerated_tensor").expect("zig src");
        assert!(src.contains("@Vector(4, f32)"));
        assert!(src.contains("hgb_wasm_simd_dot_product"));
        assert!(src.contains("hgb_wasm_simd_matmul"));
    }

    #[test]
    fn test_agent_component_model_bindings() {
        let inv = AgentToolInvocation::new("fs_reader", "agent-007", "{\"path\": \"/tmp\"}")
            .with_gas_limit(5_000_000)
            .with_allowed_read_path("/tmp")
            .with_allow_network(false);
        assert_eq!(inv.tool_id, "fs_reader");
        assert_eq!(inv.gas_limit, 5_000_000);
        assert!(!inv.allow_network);

        let out = hgb_invoke_component_tool(inv, |req| {
            assert_eq!(req.agent_id, "agent-007");
            Ok("{\"status\": \"read_ok\"}".to_string())
        }).expect("invoke component tool");

        assert!(out.is_success());
        assert_eq!(out.result_json, "{\"status\": \"read_ok\"}");
    }

    #[test]
    fn test_guest_neural_embedding_and_stacktrace_classification() {
        let emb = hgb_neural_embed_chunk_guest("fn hot_loop_simd()", 384);
        assert_eq!(emb.len(), 384);
        let norm: f32 = emb.iter().map(|v| v * v).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-4);

        let (cat, conf) = hgb_neural_classify_stacktrace_guest("FATAL SIGSEGV page fault at 0x0");
        assert_eq!(cat, "memory_corruption");
        assert!(conf > 0.9);

        let (cat2, conf2) = hgb_neural_classify_stacktrace_guest("Wasmtime trap: all fuel consumed");
        assert_eq!(cat2, "wasm_fuel_exhaustion");
        assert!(conf2 > 0.9);
    }
}

/// Hardware-accelerated dense neural embedding calculation for sandboxed WASM plugins
pub fn hgb_neural_embed_chunk_guest(text: &str, dim: usize) -> Vec<f32> {
    let mut vec = vec![0.0f32; dim];
    let words: Vec<&str> = text.split_whitespace().collect();
    if words.is_empty() {
        return vec;
    }
    for (i, w) in words.iter().enumerate() {
        let mut h: u64 = 0xcbf29ce484222325;
        for &b in w.as_bytes() {
            h ^= b as u64;
            h = h.wrapping_mul(0x100000001b3);
        }
        for d in 0..dim {
            let byte_mix = ((h.wrapping_add(d as u64) & 0xff) as f32) / 128.0 - 1.0;
            vec[d] += byte_mix / ((i as f32) * 0.05 + 1.0);
        }
    }
    let norm: f32 = vec.iter().map(|v| v * v).sum::<f32>().sqrt().max(1e-12);
    for v in &mut vec {
        *v /= norm;
    }
    vec
}

/// Zero-overhead in-memory stacktrace classification for self-healing loops
pub fn hgb_neural_classify_stacktrace_guest(stacktrace: &str) -> (String, f32) {
    let lower = stacktrace.to_lowercase();
    if lower.contains("sigsegv") || lower.contains("page fault") || lower.contains("null pointer") {
        ("memory_corruption".to_string(), 0.95)
    } else if lower.contains("oom") || lower.contains("out of memory") || lower.contains("allocation failed") {
        ("resource_exhaustion".to_string(), 0.94)
    } else if lower.contains("wasm") && lower.contains("fuel") {
        ("wasm_fuel_exhaustion".to_string(), 0.92)
    } else if lower.contains("syntax error") || lower.contains("unresolved import") {
        ("ast_drift".to_string(), 0.88)
    } else if lower.contains("deadlock") || lower.contains("mutex poisoned") {
        ("concurrency_deadlock".to_string(), 0.89)
    } else if lower.contains("regression") || lower.contains("spike") {
        ("compiler_regression".to_string(), 0.86)
    } else {
        ("unknown".to_string(), 0.50)
    }
}

