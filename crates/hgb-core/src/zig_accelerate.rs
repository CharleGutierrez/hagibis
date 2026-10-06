use std::ffi::c_void;

#[link(name = "hgb_accelerate", kind = "static")]
extern "C" {
    fn hgb_zig_simd_dot_product(a: *const f32, b: *const f32, len: usize) -> f32;
    fn hgb_zig_simd_cosine_similarity(a: *const f32, b: *const f32, len: usize) -> f32;
    fn hgb_zig_simd_normalize(vec: *mut f32, len: usize);
    fn hgb_zig_dsp_analyze_frame(samples: *const i16, len: usize, out_rms: *mut f32, out_zcr: *mut f32);
    fn hgb_zig_dsp_synthesize_tone(freq_hz: f32, sample_rate: u32, duration_samples: usize, out_buf: *mut i16, buf_len: usize) -> usize;
    fn hgb_zig_arena_create(capacity: usize) -> *mut c_void;
    fn hgb_zig_arena_alloc(arena: *mut c_void, size: usize, alignment: usize) -> *mut u8;
    fn hgb_zig_arena_reset(arena: *mut c_void);
    fn hgb_zig_arena_destroy(arena: *mut c_void);
    fn hgb_zig_raster_rgb_to_halfblocks(rgb_pixels: *const u8, width: usize, height: usize, out_buf: *mut u8, out_cap: usize, out_len: *mut usize) -> bool;
}

pub fn simd_dot_product(a: &[f32], b: &[f32]) -> f32 {
    assert_eq!(a.len(), b.len(), "Vectors must have same length");
    unsafe { hgb_zig_simd_dot_product(a.as_ptr(), b.as_ptr(), a.len()) }
}

pub fn simd_cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    assert_eq!(a.len(), b.len(), "Vectors must have same length");
    unsafe { hgb_zig_simd_cosine_similarity(a.as_ptr(), b.as_ptr(), a.len()) }
}

pub fn simd_normalize(vec: &mut [f32]) {
    unsafe { hgb_zig_simd_normalize(vec.as_mut_ptr(), vec.len()) }
}

pub fn dsp_analyze_frame(samples: &[i16]) -> (f32, f32) {
    let mut rms = 0.0;
    let mut zcr = 0.0;
    unsafe { hgb_zig_dsp_analyze_frame(samples.as_ptr(), samples.len(), &mut rms, &mut zcr) };
    (rms, zcr)
}

pub fn dsp_synthesize_tone(freq_hz: f32, sample_rate: u32, duration_samples: usize, out_buf: &mut [i16]) -> usize {
    unsafe { hgb_zig_dsp_synthesize_tone(freq_hz, sample_rate, duration_samples, out_buf.as_mut_ptr(), out_buf.len()) }
}

pub struct ZigArena {
    ptr: *mut c_void,
}

impl ZigArena {
    pub fn new(capacity: usize) -> Self {
        let ptr = unsafe { hgb_zig_arena_create(capacity) };
        assert!(!ptr.is_null(), "Failed to create Zig arena");
        Self { ptr }
    }

    pub fn alloc(&self, size: usize, alignment: usize) -> Option<*mut u8> {
        let ptr = unsafe { hgb_zig_arena_alloc(self.ptr, size, alignment) };
        if ptr.is_null() { None } else { Some(ptr) }
    }

    pub fn reset(&self) {
        unsafe { hgb_zig_arena_reset(self.ptr) }
    }
}

impl Drop for ZigArena {
    fn drop(&mut self) {
        unsafe { hgb_zig_arena_destroy(self.ptr) }
    }
}

pub fn raster_rgb_to_halfblocks(rgb_pixels: &[u8], width: usize, height: usize, out_buf: &mut [u8]) -> Option<usize> {
    assert_eq!(rgb_pixels.len(), width * height * 3, "Invalid RGB pixel array length");
    let mut out_len = 0;
    let success = unsafe {
        hgb_zig_raster_rgb_to_halfblocks(rgb_pixels.as_ptr(), width, height, out_buf.as_mut_ptr(), out_buf.len(), &mut out_len)
    };
    if success { Some(out_len) } else { None }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_dot_product() {
        let a = [1.0, 2.0, 3.0, 4.0];
        let b = [5.0, 6.0, 7.0, 8.0];
        assert_eq!(simd_dot_product(&a, &b), 70.0);
    }

    #[test]
    fn test_cosine_similarity() {
        let a = [1.0, 0.0, 0.0];
        let b = [1.0, 0.0, 0.0];
        assert_eq!(simd_cosine_similarity(&a, &b), 1.0);
    }

    #[test]
    fn test_normalize() {
        let mut a = [3.0, 4.0];
        simd_normalize(&mut a);
        assert!((a[0] - 0.6).abs() < 1e-6);
        assert!((a[1] - 0.8).abs() < 1e-6);
    }

    #[test]
    fn test_arena() {
        let arena = ZigArena::new(1024);
        let ptr1 = arena.alloc(16, 8);
        assert!(ptr1.is_some());
        arena.reset();
        let ptr2 = arena.alloc(32, 8);
        assert!(ptr2.is_some());
    }

    #[test]
    fn test_dsp() {
        let mut buf = vec![0; 44100];
        let written = dsp_synthesize_tone(440.0, 44100, 44100, &mut buf);
        assert_eq!(written, 44100);
        let (rms, zcr) = dsp_analyze_frame(&buf);
        assert!(rms > 0.0 && rms < 1.0);
        assert!(zcr > 0.0 && zcr < 1.0);
    }
}

// --- Phase 2: Lock-Free POSIX Shared Memory Ring Buffer ---

#[link(name = "hgb_accelerate", kind = "static")]
extern "C" {
    fn hgb_zig_shm_init_header(buf_ptr: *mut c_void, capacity: usize);
    fn hgb_zig_shm_push(buf_ptr: *mut c_void, data: *const u8, len: usize) -> bool;
    fn hgb_zig_shm_pop(buf_ptr: *mut c_void, out_buf: *mut u8, max_len: usize, out_len: *mut usize) -> bool;
    fn hgb_zig_shm_available(buf_ptr: *mut c_void) -> usize;

    fn hgb_zig_myers_diff_distance(hashes_a: *const u64, len_a: usize, hashes_b: *const u64, len_b: usize) -> usize;
    fn hgb_zig_myers_lcs_similarity(hashes_a: *const u64, len_a: usize, hashes_b: *const u64, len_b: usize) -> f32;

    fn hgb_zig_gf256_poly_mul(p1: *const u8, len1: usize, p2: *const u8, len2: usize, out: *mut u8) -> usize;
    fn hgb_zig_gf256_rs_encode(data: *const u8, data_len: usize, ec_len: usize, out_ec: *mut u8);
}

pub struct ShmRingBuffer {
    ptr: *mut c_void,
}

impl ShmRingBuffer {
    pub fn new(ptr: *mut c_void, capacity: usize) -> Self {
        unsafe { hgb_zig_shm_init_header(ptr, capacity) };
        Self { ptr }
    }

    pub fn push(&self, data: &[u8]) -> bool {
        unsafe { hgb_zig_shm_push(self.ptr, data.as_ptr(), data.len()) }
    }

    pub fn pop(&self, out_buf: &mut [u8]) -> Option<usize> {
        let mut out_len = 0;
        let success = unsafe { hgb_zig_shm_pop(self.ptr, out_buf.as_mut_ptr(), out_buf.len(), &mut out_len) };
        if success { Some(out_len) } else { None }
    }

    pub fn available(&self) -> usize {
        unsafe { hgb_zig_shm_available(self.ptr) }
    }
}

pub fn myers_diff_distance(hashes_a: &[u64], hashes_b: &[u64]) -> usize {
    unsafe { hgb_zig_myers_diff_distance(hashes_a.as_ptr(), hashes_a.len(), hashes_b.as_ptr(), hashes_b.len()) }
}

pub fn myers_lcs_similarity(hashes_a: &[u64], hashes_b: &[u64]) -> f32 {
    unsafe { hgb_zig_myers_lcs_similarity(hashes_a.as_ptr(), hashes_a.len(), hashes_b.as_ptr(), hashes_b.len()) }
}

pub fn gf256_poly_mul(p1: &[u8], p2: &[u8]) -> Vec<u8> {
    if p1.is_empty() || p2.is_empty() {
        return Vec::new();
    }
    let mut out = vec![0u8; p1.len() + p2.len() - 1];
    let written = unsafe {
        hgb_zig_gf256_poly_mul(p1.as_ptr(), p1.len(), p2.as_ptr(), p2.len(), out.as_mut_ptr())
    };
    out.truncate(written);
    out
}

pub fn gf256_rs_encode(data: &[u8], ec_len: usize) -> Vec<u8> {
    let mut out = vec![0u8; ec_len];
    unsafe { hgb_zig_gf256_rs_encode(data.as_ptr(), data.len(), ec_len, out.as_mut_ptr()) };
    out
}

#[cfg(test)]
mod phase2_tests {
    use super::*;

    #[test]
    fn test_shm_ring() {
        let mut mem = vec![0u8; 1024];
        let ring = ShmRingBuffer::new(mem.as_mut_ptr() as *mut c_void, 512);
        
        assert_eq!(ring.available(), 0);
        let pushed = ring.push(b"hello");
        assert!(pushed);
        assert_eq!(ring.available(), 5);
        
        let mut out = vec![0u8; 10];
        let popped = ring.pop(&mut out).unwrap();
        assert_eq!(popped, 5);
        assert_eq!(&out[..5], b"hello");
    }

    #[test]
    fn test_myers() {
        let a = vec![1, 2, 3];
        let b = vec![1, 4, 3];
        let dist = myers_diff_distance(&a, &b);
        assert_eq!(dist, 2);
        
        let sim = myers_lcs_similarity(&a, &b);
        assert!(sim > 0.0);
    }

    #[test]
    fn test_gf256() {
        let data = vec![0x40, 0xd2, 0x75, 0x47, 0x76, 0x17, 0x32, 0x06, 0x27, 0x26, 0x96, 0xc6, 0xc6, 0x96, 0x70, 0xec];
        let ec = gf256_rs_encode(&data, 10);
        assert_eq!(ec.len(), 10);

        let p1 = [1, 2];
        let p2 = [3, 4];
        let prod = gf256_poly_mul(&p1, &p2);
        assert_eq!(prod.len(), 3);
    }
}

// --- Phase 3 & 4: Safe Rust FFI Wrappers ---

#[link(name = "hgb_accelerate", kind = "static")]
extern "C" {
    fn hgb_zig_pagerank_csr(num_nodes: usize, row_offsets: *const u32, col_indices: *const u32, iterations: usize, damping: f64, out_scores: *mut f64);
    fn hgb_zig_strip_ansi(input: *const u8, in_len: usize, out_buf: *mut u8, out_cap: usize, out_len: *mut usize) -> bool;
    fn hgb_zig_levenshtein_distance(s1: *const u8, len1: usize, s2: *const u8, len2: usize) -> usize;
    fn hgb_zig_dsp_fft_magnitude(samples: *const f32, n: usize, out_magnitudes: *mut f32);
    fn hgb_zig_sandbox_check_support() -> bool;
    fn hgb_zig_sandbox_apply_landlock(allowed_dir: *const u8, len: usize) -> i32;
    fn hgb_zig_merkle_root(leaf_hashes_ptr: *const u8, leaf_count: usize, out_root: *mut u8);
    fn hgb_zig_blake3_node_pair(left: *const u8, left_len: usize, right: *const u8, right_len: usize, out_hex: *mut u8);
    fn hgb_zig_dsp_resample(in_samples: *const i16, in_len: usize, in_rate: u32, out_samples: *mut i16, out_cap: usize, out_rate: u32) -> usize;
}

pub fn pagerank_csr(num_nodes: usize, row_offsets: &[u32], col_indices: &[u32], iterations: usize, damping: f64) -> Vec<f64> {
    let mut out = vec![0.0; num_nodes];
    if num_nodes == 0 { return out; }
    unsafe {
        hgb_zig_pagerank_csr(num_nodes, row_offsets.as_ptr(), col_indices.as_ptr(), iterations, damping, out.as_mut_ptr());
    }
    out
}

pub fn strip_ansi(input: &str) -> String {
    let mut out = vec![0u8; input.len()];
    let mut out_len = 0;
    unsafe {
        hgb_zig_strip_ansi(input.as_ptr(), input.len(), out.as_mut_ptr(), out.len(), &mut out_len);
    }
    out.truncate(out_len);
    String::from_utf8(out).unwrap_or_else(|_| String::new())
}

pub fn levenshtein_distance(s1: &str, s2: &str) -> usize {
    unsafe {
        hgb_zig_levenshtein_distance(s1.as_ptr(), s1.len(), s2.as_ptr(), s2.len())
    }
}

pub fn dsp_fft_magnitude(samples: &[f32]) -> Vec<f32> {
    let n = samples.len();
    let mut out = vec![0.0; n];
    if n == 0 || (n & (n - 1)) != 0 || n > 4096 {
        return out;
    }
    unsafe {
        hgb_zig_dsp_fft_magnitude(samples.as_ptr(), n, out.as_mut_ptr());
    }
    out
}

pub fn sandbox_check_support() -> bool {
    unsafe { hgb_zig_sandbox_check_support() }
}

pub fn sandbox_apply_landlock(allowed_dir: &str) -> Result<(), i32> {
    let c_str = allowed_dir.as_bytes();
    let ret = unsafe { hgb_zig_sandbox_apply_landlock(c_str.as_ptr(), c_str.len()) };
    if ret == 0 { Ok(()) } else { Err(ret) }
}

pub fn merkle_root_bytes(leaf_hashes: &[[u8; 32]]) -> [u8; 32] {
    let mut out = [0u8; 32];
    if leaf_hashes.is_empty() {
        unsafe {
            hgb_zig_merkle_root(std::ptr::null(), 0, out.as_mut_ptr());
        }
    } else {
        unsafe {
            hgb_zig_merkle_root(leaf_hashes.as_ptr() as *const u8, leaf_hashes.len(), out.as_mut_ptr());
        }
    }
    out
}

pub fn blake3_node_pair_hex(left: &str, right: &str) -> String {
    let mut out = vec![0u8; 64];
    unsafe {
        hgb_zig_blake3_node_pair(left.as_ptr(), left.len(), right.as_ptr(), right.len(), out.as_mut_ptr());
    }
    String::from_utf8(out).unwrap_or_default()
}

pub fn dsp_resample(samples: &[i16], in_rate: u32, out_rate: u32) -> Vec<i16> {
    if samples.is_empty() || in_rate == 0 || out_rate == 0 {
        return Vec::new();
    }
    let target_len = ((samples.len() as f64) * (out_rate as f64) / (in_rate as f64)).ceil() as usize + 16;
    let mut out = vec![0i16; target_len];
    let actual_len = unsafe {
        hgb_zig_dsp_resample(samples.as_ptr(), samples.len(), in_rate, out.as_mut_ptr(), out.len(), out_rate)
    };
    out.truncate(actual_len);
    out
}

#[cfg(test)]
mod phase3_4_tests {
    use super::*;

    #[test]
    fn test_pagerank() {
        let num_nodes = 3;
        let row_offsets = vec![0, 2, 3, 4];
        let col_indices = vec![1, 2, 0, 1];
        let scores = pagerank_csr(num_nodes, &row_offsets, &col_indices, 10, 0.85);
        assert_eq!(scores.len(), 3);
        assert!(scores[0] > 0.0);
    }

    #[test]
    fn test_strip_ansi() {
        let input = "\x1b[31mHello\x1b[0m World";
        let clean = strip_ansi(input);
        assert_eq!(clean, "Hello World");
    }

    #[test]
    fn test_levenshtein() {
        let dist = levenshtein_distance("kitten", "sitting");
        assert_eq!(dist, 3);
    }

    #[test]
    fn test_fft() {
        let samples = vec![1.0, 1.0, 1.0, 1.0];
        let mags = dsp_fft_magnitude(&samples);
        assert_eq!(mags.len(), 4);
        assert!(mags[0] > 1.0); // DC component is strong
    }

    #[test]
    fn test_sandbox_check() {
        // Can be false on systems without landlock
        let _ = sandbox_check_support(); 
    }

    #[test]
    fn test_merkle_root_bytes() {
        let mut leaves = Vec::new();
        for i in 0..4u8 {
            leaves.push([i; 32]);
        }
        let root = merkle_root_bytes(&leaves);
        assert_ne!(root, [0u8; 32]);

        // Empty root check
        let empty_root = merkle_root_bytes(&[]);
        assert_ne!(empty_root, [0u8; 32]);
    }

    #[test]
    fn test_blake3_node_pair() {
        let hex = blake3_node_pair_hex("left_node", "right_node");
        assert_eq!(hex.len(), 64);
    }

    #[test]
    fn test_dsp_resample() {
        let audio = vec![1000i16, 2000, 3000, 4000, 5000, 6000];
        let resampled = dsp_resample(&audio, 48000, 16000);
        assert_eq!(resampled.len(), 2);
        assert!(resampled[0] > 0);
    }
}

// --- Phase 5: MCP FFI Wrappers ---

#[link(name = "hgb_accelerate", kind = "static")]
extern "C" {
    fn hgb_zig_mcp_parse_request(
        json_ptr: *const u8,
        json_len: usize,
        out_id: *mut i64,
        out_has_id: *mut bool,
        out_method_ptr: *mut *const u8,
        out_method_len: *mut usize,
        out_params_ptr: *mut *const u8,
        out_params_len: *mut usize,
    ) -> bool;

    fn hgb_zig_mcp_extract_tool_call(
        params_ptr: *const u8,
        params_len: usize,
        out_name_ptr: *mut *const u8,
        out_name_len: *mut usize,
        out_args_ptr: *mut *const u8,
        out_args_len: *mut usize,
    ) -> bool;

    fn hgb_zig_mcp_format_initialize_response(
        id: i64,
        has_id: bool,
        server_name_ptr: *const u8,
        server_name_len: usize,
        version_ptr: *const u8,
        version_len: usize,
        out_buf_ptr: *mut u8,
        out_cap: usize,
        out_len: *mut usize,
    ) -> bool;

    fn hgb_zig_mcp_format_tool_result(
        id: i64,
        has_id: bool,
        content_ptr: *const u8,
        content_len: usize,
        is_error: bool,
        out_buf_ptr: *mut u8,
        out_cap: usize,
        out_len: *mut usize,
    ) -> bool;

    fn hgb_zig_mcp_arena_create(capacity: usize) -> *mut std::ffi::c_void;
    fn hgb_zig_mcp_arena_alloc(arena: *mut std::ffi::c_void, size: usize) -> *mut u8;
    fn hgb_zig_mcp_arena_reset(arena: *mut std::ffi::c_void);
    fn hgb_zig_mcp_arena_destroy(arena: *mut std::ffi::c_void);
}

pub struct McpParsedRequest<'a> {
    pub id: Option<i64>,
    pub method: &'a str,
    pub params: Option<&'a str>,
}

pub fn parse_mcp_request(json: &str) -> Option<McpParsedRequest<'_>> {
    let mut out_id: i64 = 0;
    let mut out_has_id: bool = false;
    let mut out_method_ptr: *const u8 = std::ptr::null();
    let mut out_method_len: usize = 0;
    let mut out_params_ptr: *const u8 = std::ptr::null();
    let mut out_params_len: usize = 0;

    let ok = unsafe {
        hgb_zig_mcp_parse_request(
            json.as_ptr(),
            json.len(),
            &mut out_id,
            &mut out_has_id,
            &mut out_method_ptr,
            &mut out_method_len,
            &mut out_params_ptr,
            &mut out_params_len,
        )
    };

    if !ok {
        return None;
    }

    let method = unsafe {
        let slice = std::slice::from_raw_parts(out_method_ptr, out_method_len);
        std::str::from_utf8_unchecked(slice)
    };

    let params = if out_params_len > 0 {
        unsafe {
            let slice = std::slice::from_raw_parts(out_params_ptr, out_params_len);
            Some(std::str::from_utf8_unchecked(slice))
        }
    } else {
        None
    };

    Some(McpParsedRequest {
        id: if out_has_id { Some(out_id) } else { None },
        method,
        params,
    })
}

pub fn extract_mcp_tool_call(params: &str) -> Option<(&str, &str)> {
    let mut out_name_ptr: *const u8 = std::ptr::null();
    let mut out_name_len: usize = 0;
    let mut out_args_ptr: *const u8 = std::ptr::null();
    let mut out_args_len: usize = 0;

    let ok = unsafe {
        hgb_zig_mcp_extract_tool_call(
            params.as_ptr(),
            params.len(),
            &mut out_name_ptr,
            &mut out_name_len,
            &mut out_args_ptr,
            &mut out_args_len,
        )
    };

    if !ok {
        return None;
    }

    let name = unsafe {
        let slice = std::slice::from_raw_parts(out_name_ptr, out_name_len);
        std::str::from_utf8_unchecked(slice)
    };

    let args = unsafe {
        let slice = std::slice::from_raw_parts(out_args_ptr, out_args_len);
        std::str::from_utf8_unchecked(slice)
    };

    Some((name, args))
}

pub fn format_mcp_initialize(id: Option<i64>, server_name: &str, version: &str) -> Option<String> {
    let mut out = vec![0u8; 1024 + server_name.len() + version.len()];
    let mut out_len = 0;
    let has_id = id.is_some();
    let id_val = id.unwrap_or(0);

    let ok = unsafe {
        hgb_zig_mcp_format_initialize_response(
            id_val,
            has_id,
            server_name.as_ptr(),
            server_name.len(),
            version.as_ptr(),
            version.len(),
            out.as_mut_ptr(),
            out.len(),
            &mut out_len,
        )
    };

    if ok {
        out.truncate(out_len);
        String::from_utf8(out).ok()
    } else {
        None
    }
}

pub fn format_mcp_tool_result(id: Option<i64>, content: &str, is_error: bool) -> Option<String> {
    let mut out = vec![0u8; content.len() * 2 + 512]; // ample space for escaping
    let mut out_len = 0;
    let has_id = id.is_some();
    let id_val = id.unwrap_or(0);

    let ok = unsafe {
        hgb_zig_mcp_format_tool_result(
            id_val,
            has_id,
            content.as_ptr(),
            content.len(),
            is_error,
            out.as_mut_ptr(),
            out.len(),
            &mut out_len,
        )
    };

    if ok {
        out.truncate(out_len);
        String::from_utf8(out).ok()
    } else {
        None
    }
}

pub struct McpArena {
    ptr: *mut std::ffi::c_void,
}

impl McpArena {
    pub fn new(capacity: usize) -> Option<Self> {
        let ptr = unsafe { hgb_zig_mcp_arena_create(capacity) };
        if ptr.is_null() {
            None
        } else {
            Some(Self { ptr })
        }
    }

    pub fn alloc(&self, size: usize) -> Option<*mut u8> {
        let ptr = unsafe { hgb_zig_mcp_arena_alloc(self.ptr, size) };
        if ptr.is_null() {
            None
        } else {
            Some(ptr)
        }
    }

    pub fn reset(&self) {
        unsafe { hgb_zig_mcp_arena_reset(self.ptr) }
    }
}

impl Drop for McpArena {
    fn drop(&mut self) {
        unsafe { hgb_zig_mcp_arena_destroy(self.ptr) }
    }
}

#[cfg(test)]
mod phase5_tests {
    use super::*;

    #[test]
    fn test_mcp_parse_request() {
        let req = r#"{"jsonrpc":"2.0","id":123,"method":"initialize","params":{"version":"1.0"}}"#;
        let parsed = parse_mcp_request(req).unwrap();
        assert_eq!(parsed.id, Some(123));
        assert_eq!(parsed.method, "initialize");
        assert_eq!(parsed.params, Some(r#"{"version":"1.0"}"#));

        let req_no_id = r#"{"jsonrpc":"2.0","method":"notifications/test","params":[]}"#;
        let parsed2 = parse_mcp_request(req_no_id).unwrap();
        assert_eq!(parsed2.id, None);
        assert_eq!(parsed2.method, "notifications/test");
        assert_eq!(parsed2.params, Some("[]"));
    }

    #[test]
    fn test_mcp_extract_tool_call() {
        let params = r#"{"name":"test_tool","arguments":{"arg1":"val1"}}"#;
        let ext = extract_mcp_tool_call(params).unwrap();
        assert_eq!(ext.0, "test_tool");
        assert_eq!(ext.1, r#"{"arg1":"val1"}"#);
    }

    #[test]
    fn test_mcp_format_initialize() {
        let init = format_mcp_initialize(Some(1), "my_server", "1.0.0").unwrap();
        assert!(init.contains("\"protocolVersion\":\"2024-11-05\""));
        assert!(init.contains("\"my_server\""));
        assert!(init.contains("\"id\":1"));
    }

    #[test]
    fn test_mcp_format_tool_result() {
        let res = format_mcp_tool_result(Some(2), "result\n\"text\"", false).unwrap();
        assert!(res.contains("result\\n\\\"text\\\""));
    }

    #[test]
    fn test_mcp_arena() {
        let arena = McpArena::new(1024).unwrap();
        let ptr1 = arena.alloc(100);
        assert!(ptr1.is_some());
        arena.reset();
        let ptr2 = arena.alloc(1000);
        assert!(ptr2.is_some());
    }
}
