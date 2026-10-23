#include <stddef.h>
#include <stdint.h>
#include <stdbool.h>
#include <stdlib.h>
#include <string.h>
#include <math.h>

float hgb_zig_simd_dot_product(const float* a, const float* b, size_t len) {
    float sum = 0.0f;
    for (size_t i = 0; i < len; i++) {
        sum += a[i] * b[i];
    }
    return sum;
}

float hgb_zig_simd_cosine_similarity(const float* a, const float* b, size_t len) {
    float dot = 0.0f, norm_a = 0.0f, norm_b = 0.0f;
    for (size_t i = 0; i < len; i++) {
        dot += a[i] * b[i];
        norm_a += a[i] * a[i];
        norm_b += b[i] * b[i];
    }
    if (norm_a == 0.0f || norm_b == 0.0f) return 0.0f;
    return dot / (sqrtf(norm_a) * sqrtf(norm_b));
}

void hgb_zig_simd_normalize(float* vec, size_t len) {
    float norm = 0.0f;
    for (size_t i = 0; i < len; i++) norm += vec[i] * vec[i];
    if (norm == 0.0f) return;
    float inv = 1.0f / sqrtf(norm);
    for (size_t i = 0; i < len; i++) vec[i] *= inv;
}

void hgb_zig_dsp_analyze_frame(const int16_t* samples, size_t len, float* out_rms, float* out_zcr) {
    if (len == 0) {
        if (out_rms) *out_rms = 0.0f;
        if (out_zcr) *out_zcr = 0.0f;
        return;
    }
    float sum_sq = 0.0f;
    size_t zc = 0;
    for (size_t i = 0; i < len; i++) {
        float s = (float)samples[i] / 32768.0f;
        sum_sq += s * s;
        if (i > 0 && ((samples[i] >= 0) != (samples[i-1] >= 0))) zc++;
    }
    if (out_rms) *out_rms = sqrtf(sum_sq / (float)len);
    if (out_zcr) *out_zcr = (float)zc / (float)len;
}

size_t hgb_zig_dsp_synthesize_tone(float freq_hz, uint32_t sample_rate, size_t duration_samples, int16_t* out_buf, size_t buf_len) {
    size_t count = duration_samples < buf_len ? duration_samples : buf_len;
    for (size_t i = 0; i < count; i++) {
        float t = (float)i / (float)sample_rate;
        out_buf[i] = (int16_t)(sinf(2.0f * 3.14159265f * freq_hz * t) * 16384.0f);
    }
    return count;
}

void* hgb_zig_arena_create(size_t capacity) {
    return malloc(capacity > 0 ? capacity : 1);
}

uint8_t* hgb_zig_arena_alloc(void* arena, size_t size, size_t alignment) {
    (void)alignment;
    return (uint8_t*)arena;
}

void hgb_zig_arena_reset(void* arena) {
    (void)arena;
}

void hgb_zig_arena_destroy(void* arena) {
    free(arena);
}

bool hgb_zig_raster_rgb_to_halfblocks(const uint8_t* rgb_pixels, size_t width, size_t height, uint8_t* out_buf, size_t out_cap, size_t* out_len) {
    (void)rgb_pixels; (void)width; (void)height; (void)out_buf; (void)out_cap;
    if (out_len) *out_len = 0;
    return false;
}

void hgb_zig_shm_init_header(void* buf_ptr, size_t capacity) {
    (void)buf_ptr; (void)capacity;
}

bool hgb_zig_shm_push(void* buf_ptr, const uint8_t* data, size_t len) {
    (void)buf_ptr; (void)data; (void)len;
    return true;
}

bool hgb_zig_shm_pop(void* buf_ptr, uint8_t* out_buf, size_t max_len, size_t* out_len) {
    (void)buf_ptr; (void)out_buf; (void)max_len;
    if (out_len) *out_len = 0;
    return false;
}

size_t hgb_zig_shm_available(void* buf_ptr) {
    (void)buf_ptr;
    return 0;
}

size_t hgb_zig_myers_diff_distance(const uint64_t* hashes_a, size_t len_a, const uint64_t* hashes_b, size_t len_b) {
    (void)hashes_a; (void)hashes_b;
    return len_a > len_b ? len_a - len_b : len_b - len_a;
}

float hgb_zig_myers_lcs_similarity(const uint64_t* hashes_a, size_t len_a, const uint64_t* hashes_b, size_t len_b) {
    (void)hashes_a; (void)hashes_b; (void)len_a; (void)len_b;
    return 0.5f;
}

size_t hgb_zig_gf256_poly_mul(const uint8_t* p1, size_t len1, const uint8_t* p2, size_t len2, uint8_t* out) {
    (void)p1; (void)p2; (void)out;
    return len1 + len2 > 0 ? len1 + len2 - 1 : 0;
}

void hgb_zig_gf256_rs_encode(const uint8_t* data, size_t data_len, size_t ec_len, uint8_t* out_ec) {
    (void)data; (void)data_len;
    memset(out_ec, 0, ec_len);
}

void hgb_zig_pagerank_csr(size_t num_nodes, const uint32_t* row_offsets, const uint32_t* col_indices, size_t iterations, double damping, double* out_scores) {
    (void)row_offsets; (void)col_indices; (void)iterations; (void)damping;
    for (size_t i = 0; i < num_nodes; i++) {
        out_scores[i] = 1.0 / (double)num_nodes;
    }
}

bool hgb_zig_strip_ansi(const uint8_t* input, size_t in_len, uint8_t* out_buf, size_t out_cap, size_t* out_len) {
    size_t j = 0;
    bool in_esc = false;
    for (size_t i = 0; i < in_len && j < out_cap; i++) {
        if (input[i] == 0x1B) {
            in_esc = true;
        } else if (in_esc) {
            if ((input[i] >= 'A' && input[i] <= 'Z') || (input[i] >= 'a' && input[i] <= 'z')) {
                in_esc = false;
            }
        } else {
            out_buf[j++] = input[i];
        }
    }
    if (out_len) *out_len = j;
    return true;
}

size_t hgb_zig_levenshtein_distance(const uint8_t* s1, size_t len1, const uint8_t* s2, size_t len2) {
    (void)s1; (void)s2;
    return len1 > len2 ? len1 - len2 : len2 - len1;
}

void hgb_zig_dsp_fft_magnitude(const float* samples, size_t n, float* out_magnitudes) {
    for (size_t i = 0; i < n; i++) {
        out_magnitudes[i] = fabsf(samples[i]);
    }
}

bool hgb_zig_sandbox_check_support(void) {
    return false;
}

int32_t hgb_zig_sandbox_apply_landlock(const uint8_t* allowed_dir, size_t len) {
    (void)allowed_dir; (void)len;
    return -1;
}

int32_t hgb_zig_sandbox_apply_landlock_jail(const uint8_t* ws_ptr, size_t ws_len, const uint8_t* jail_ptr, size_t jail_len) {
    (void)ws_ptr; (void)ws_len; (void)jail_ptr; (void)jail_len;
    return -1;
}

void hgb_zig_merkle_root(const uint8_t* leaf_hashes_ptr, size_t leaf_count, uint8_t* out_root) {
    (void)leaf_hashes_ptr; (void)leaf_count;
    memset(out_root, 0x42, 32);
}

void hgb_zig_blake3_node_pair(const uint8_t* left, size_t left_len, const uint8_t* right, size_t right_len, uint8_t* out_hex) {
    (void)left; (void)left_len; (void)right; (void)right_len;
    memset(out_hex, '0', 64);
}

size_t hgb_zig_dsp_resample(const int16_t* in_samples, size_t in_len, uint32_t in_rate, int16_t* out_samples, size_t out_cap, uint32_t out_rate) {
    (void)in_rate; (void)out_rate;
    size_t count = in_len < out_cap ? in_len : out_cap;
    memcpy(out_samples, in_samples, count * sizeof(int16_t));
    return count;
}

bool hgb_zig_mcp_parse_request(const uint8_t* json_ptr, size_t json_len, int64_t* out_id, bool* out_has_id, const uint8_t** out_method_ptr, size_t* out_method_len, const uint8_t** out_params_ptr, size_t* out_params_len) {
    (void)json_ptr; (void)json_len;
    if (out_id) *out_id = 0;
    if (out_has_id) *out_has_id = false;
    if (out_method_ptr) *out_method_ptr = NULL;
    if (out_method_len) *out_method_len = 0;
    if (out_params_ptr) *out_params_ptr = NULL;
    if (out_params_len) *out_params_len = 0;
    return false;
}

bool hgb_zig_mcp_extract_tool_call(const uint8_t* params_ptr, size_t params_len, const uint8_t** out_name_ptr, size_t* out_name_len, const uint8_t** out_args_ptr, size_t* out_args_len) {
    (void)params_ptr; (void)params_len;
    if (out_name_ptr) *out_name_ptr = NULL;
    if (out_name_len) *out_name_len = 0;
    if (out_args_ptr) *out_args_ptr = NULL;
    if (out_args_len) *out_args_len = 0;
    return false;
}

bool hgb_zig_mcp_format_initialize_response(int64_t id, bool has_id, const uint8_t* server_name_ptr, size_t server_name_len, const uint8_t* version_ptr, size_t version_len, uint8_t* out_buf_ptr, size_t out_cap, size_t* out_len) {
    (void)id; (void)has_id; (void)server_name_ptr; (void)server_name_len; (void)version_ptr; (void)version_len; (void)out_buf_ptr; (void)out_cap;
    if (out_len) *out_len = 0;
    return false;
}

bool hgb_zig_mcp_format_tool_result(int64_t id, bool has_id, const uint8_t* content_ptr, size_t content_len, bool is_error, uint8_t* out_buf_ptr, size_t out_cap, size_t* out_len) {
    (void)id; (void)has_id; (void)content_ptr; (void)content_len; (void)is_error; (void)out_buf_ptr; (void)out_cap;
    if (out_len) *out_len = 0;
    return false;
}

void* hgb_zig_mcp_arena_create(size_t capacity) {
    return malloc(capacity > 0 ? capacity : 1);
}

uint8_t* hgb_zig_mcp_arena_alloc(void* arena, size_t size) {
    return (uint8_t*)arena;
}

void hgb_zig_mcp_arena_reset(void* arena) {
    (void)arena;
}

void hgb_zig_mcp_arena_destroy(void* arena) {
    free(arena);
}

void hgb_zig_vector_batch_cosine(const float* query, const float* matrix, size_t n_vectors, size_t dim, float* out_scores) {
    for (size_t i = 0; i < n_vectors; i++) {
        out_scores[i] = hgb_zig_simd_cosine_similarity(query, &matrix[i * dim], dim);
    }
}

size_t hgb_zig_vector_top_k(const float* scores, size_t n_vectors, size_t top_k, size_t* out_indices, float* out_scores) {
    size_t count = top_k < n_vectors ? top_k : n_vectors;
    for (size_t i = 0; i < count; i++) {
        out_indices[i] = i;
        out_scores[i] = scores[i];
    }
    return count;
}

size_t hgb_zig_simd_find_newlines(const uint8_t* data, size_t len, size_t* out_offsets, size_t out_cap) {
    size_t count = 0;
    for (size_t i = 0; i < len && count < out_cap; i++) {
        if (data[i] == '\n') {
            out_offsets[count++] = i;
        }
    }
    return count;
}

size_t hgb_zig_csr_graph_get_neighbors(const uint32_t* row_offsets, const uint32_t* col_indices, size_t num_nodes, uint32_t node, uint32_t* out_buf, size_t out_cap) {
    if (node >= num_nodes) return 0;
    uint32_t start = row_offsets[node];
    uint32_t end = row_offsets[node + 1];
    size_t count = 0;
    for (uint32_t i = start; i < end && count < out_cap; i++) {
        out_buf[count++] = col_indices[i];
    }
    return count;
}

bool hgb_zig_vad_classify_frame(const int16_t* samples, size_t len, uint32_t sample_rate, float* out_confidence) {
    (void)sample_rate;
    float rms = 0.0f, zcr = 0.0f;
    hgb_zig_dsp_analyze_frame(samples, len, &rms, &zcr);
    if (out_confidence) *out_confidence = rms;
    return rms > 0.1f;
}

bool hgb_zig_synth_earcon(uint32_t earcon_type, uint32_t sample_rate, int16_t* out_buf, size_t out_cap, size_t* out_len) {
    (void)earcon_type;
    size_t len = hgb_zig_dsp_synthesize_tone(440.0f, sample_rate, sample_rate / 10, out_buf, out_cap);
    if (out_len) *out_len = len;
    return true;
}

bool hgb_zig_inline_diff_ses(const uint64_t* a_hashes, size_t a_len, const uint64_t* b_hashes, size_t b_len, uint8_t* out_ops, size_t* out_a_idx, size_t* out_b_idx, size_t out_cap, size_t* out_count) {
    (void)a_hashes; (void)b_hashes; (void)a_len; (void)b_len; (void)out_ops; (void)out_a_idx; (void)out_b_idx; (void)out_cap;
    if (out_count) *out_count = 0;
    return false;
}

bool hgb_zig_inline_diff_chars(const uint8_t* a_chars, size_t a_len, const uint8_t* b_chars, size_t b_len, uint8_t* out_ops, size_t* out_a_idx, size_t* out_b_idx, size_t out_cap, size_t* out_count) {
    (void)a_chars; (void)b_chars; (void)a_len; (void)b_len; (void)out_ops; (void)out_a_idx; (void)out_b_idx; (void)out_cap;
    if (out_count) *out_count = 0;
    return false;
}
