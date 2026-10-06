const std = @import("std");

export fn hgb_zig_simd_dot_product(a: [*]const f32, b: [*]const f32, len: usize) callconv(.C) f32 {
    var sum: f32 = 0.0;
    var i: usize = 0;
    
    while (i + 8 <= len) : (i += 8) {
        const va = @as(*const [8]f32, @ptrCast(a + i)).*;
        const vb = @as(*const [8]f32, @ptrCast(b + i)).*;
        const v_va: @Vector(8, f32) = va;
        const v_vb: @Vector(8, f32) = vb;
        sum += @reduce(.Add, v_va * v_vb);
    }
    
    while (i < len) : (i += 1) {
        sum += a[i] * b[i];
    }
    
    return sum;
}

export fn hgb_zig_simd_cosine_similarity(a: [*]const f32, b: [*]const f32, len: usize) callconv(.C) f32 {
    const dot = hgb_zig_simd_dot_product(a, b, len);
    const norm_a = @sqrt(hgb_zig_simd_dot_product(a, a, len));
    const norm_b = @sqrt(hgb_zig_simd_dot_product(b, b, len));
    
    if (norm_a == 0.0 or norm_b == 0.0) return 0.0;
    return dot / (norm_a * norm_b);
}

export fn hgb_zig_simd_normalize(vec: [*]f32, len: usize) callconv(.C) void {
    const dot = hgb_zig_simd_dot_product(vec, vec, len);
    if (dot == 0.0) return;
    
    const inv_norm = 1.0 / @sqrt(dot);
    var i: usize = 0;
    
    const vinv: @Vector(8, f32) = @splat(inv_norm);
    while (i + 8 <= len) : (i += 8) {
        const va = @as(*const [8]f32, @ptrCast(vec + i)).*;
        var v: @Vector(8, f32) = va;
        v = v * vinv;
        @as(*[8]f32, @ptrCast(vec + i)).* = v;
    }
    
    while (i < len) : (i += 1) {
        vec[i] *= inv_norm;
    }
}

export fn hgb_zig_dsp_analyze_frame(samples: [*]const i16, len: usize, out_rms: *f32, out_zcr: *f32) callconv(.C) void {
    if (len == 0) {
        out_rms.* = 0.0;
        out_zcr.* = 0.0;
        return;
    }
    
    var sum_sq: f64 = 0.0;
    var zcr_count: usize = 0;
    var prev_sign: bool = samples[0] >= 0;
    
    for (0..len) |i| {
        const sample_f = @as(f32, @floatFromInt(samples[i])) / 32768.0;
        sum_sq += @as(f64, @floatCast(sample_f * sample_f));
        
        const current_sign = samples[i] >= 0;
        if (current_sign != prev_sign) {
            zcr_count += 1;
            prev_sign = current_sign;
        }
    }
    
    out_rms.* = @floatCast(@sqrt(sum_sq / @as(f64, @floatFromInt(len))));
    out_zcr.* = @as(f32, @floatFromInt(zcr_count)) / @as(f32, @floatFromInt(len));
}

export fn hgb_zig_dsp_synthesize_tone(freq_hz: f32, sample_rate: u32, duration_samples: usize, out_buf: [*]i16, buf_len: usize) callconv(.C) usize {
    const to_write = if (duration_samples < buf_len) duration_samples else buf_len;
    const dt = 1.0 / @as(f32, @floatFromInt(sample_rate));
    const phase_inc = 2.0 * std.math.pi * freq_hz * dt;
    
    for (0..to_write) |i| {
        const phase = @as(f32, @floatFromInt(i)) * phase_inc;
        const sample_f = @sin(phase);
        out_buf[i] = @as(i16, @intFromFloat(sample_f * 32767.0));
    }
    
    return to_write;
}

const HgbArena = struct {
    allocator: std.heap.ArenaAllocator,
};

export fn hgb_zig_arena_create(capacity: usize) callconv(.C) ?*anyopaque {
    _ = capacity; // We'll just use the system allocator with ArenaAllocator
    const arena_ptr = std.heap.page_allocator.create(HgbArena) catch return null;
    arena_ptr.* = HgbArena{
        .allocator = std.heap.ArenaAllocator.init(std.heap.page_allocator),
    };
    return @ptrCast(arena_ptr);
}

export fn hgb_zig_arena_alloc(arena: *anyopaque, size: usize, alignment: usize) callconv(.C) ?[*]u8 {
    const arena_ptr = @as(*HgbArena, @ptrCast(@alignCast(arena)));
    
    const log2_align = std.math.log2_int(usize, alignment);
    _ = log2_align;
    
    const slice = arena_ptr.allocator.allocator().alloc(u8, size) catch return null;
    return slice.ptr;
}

export fn hgb_zig_arena_reset(arena: *anyopaque) callconv(.C) void {
    const arena_ptr = @as(*HgbArena, @ptrCast(@alignCast(arena)));
    _ = arena_ptr.allocator.reset(.retain_capacity);
}

export fn hgb_zig_arena_destroy(arena: *anyopaque) callconv(.C) void {
    const arena_ptr = @as(*HgbArena, @ptrCast(@alignCast(arena)));
    arena_ptr.allocator.deinit();
    std.heap.page_allocator.destroy(arena_ptr);
}

export fn hgb_zig_raster_rgb_to_halfblocks(rgb_pixels: [*]const u8, width: usize, height: usize, out_buf: [*]u8, out_cap: usize, out_len: *usize) callconv(.C) bool {
    var stream = std.io.fixedBufferStream(out_buf[0..out_cap]);
    var writer = stream.writer();
    
    const half_height = (height + 1) / 2;
    
    for (0..half_height) |y| {
        for (0..width) |x| {
            const top_idx = (y * 2 * width + x) * 3;
            const has_bottom = (y * 2 + 1) < height;
            const bottom_idx = if (has_bottom) ((y * 2 + 1) * width + x) * 3 else 0;
            
            const r_top = rgb_pixels[top_idx];
            const g_top = rgb_pixels[top_idx + 1];
            const b_top = rgb_pixels[top_idx + 2];
            
            if (has_bottom) {
                const r_bot = rgb_pixels[bottom_idx];
                const g_bot = rgb_pixels[bottom_idx + 1];
                const b_bot = rgb_pixels[bottom_idx + 2];
                writer.print("\x1b[38;2;{};{};{}m\x1b[48;2;{};{};{}m\u{2580}", .{r_top, g_top, b_top, r_bot, g_bot, b_bot}) catch return false;
            } else {
                writer.print("\x1b[38;2;{};{};{}m\x1b[49m\u{2580}", .{r_top, g_top, b_top}) catch return false;
            }
        }
        writer.print("\x1b[0m\n", .{}) catch return false;
    }
    
    out_len.* = stream.pos;
    return true;
}

// --- Phase 2: Lock-Free POSIX Shared Memory Ring Buffer ---

const ShmRingHeader = extern struct {
    head: usize align(64),
    tail: usize align(64),
    capacity: usize align(64),
};

export fn hgb_zig_shm_init_header(buf_ptr: *anyopaque, capacity: usize) callconv(.C) void {
    const header = @as(*ShmRingHeader, @ptrCast(@alignCast(buf_ptr)));
    @atomicStore(usize, &header.head, 0, .release);
    @atomicStore(usize, &header.tail, 0, .release);
    header.capacity = capacity;
}

export fn hgb_zig_shm_push(buf_ptr: *anyopaque, data: [*]const u8, len: usize) callconv(.C) bool {
    const header = @as(*ShmRingHeader, @ptrCast(@alignCast(buf_ptr)));
    const capacity = header.capacity;
    
    const current_tail = @atomicLoad(usize, &header.tail, .acquire);
    const current_head = @atomicLoad(usize, &header.head, .acquire);
    
    // Check if enough space
    // size available = capacity - (tail - head)
    if (capacity -% (current_tail -% current_head) < len) {
        return false;
    }
    
    const data_buf = @as([*]u8, @ptrCast(@as([*]u8, @ptrCast(header)) + @sizeOf(ShmRingHeader)));
    
    // Copy data
    for (0..len) |i| {
        data_buf[(current_tail + i) % capacity] = data[i];
    }
    
    @atomicStore(usize, &header.tail, current_tail + len, .release);
    return true;
}

export fn hgb_zig_shm_pop(buf_ptr: *anyopaque, out_buf: [*]u8, max_len: usize, out_len: *usize) callconv(.C) bool {
    const header = @as(*ShmRingHeader, @ptrCast(@alignCast(buf_ptr)));
    const capacity = header.capacity;
    
    const current_tail = @atomicLoad(usize, &header.tail, .acquire);
    const current_head = @atomicLoad(usize, &header.head, .acquire);
    
    const available = current_tail -% current_head;
    if (available == 0) {
        out_len.* = 0;
        return false;
    }
    
    const to_read = if (available < max_len) available else max_len;
    const data_buf = @as([*]u8, @ptrCast(@as([*]u8, @ptrCast(header)) + @sizeOf(ShmRingHeader)));
    
    for (0..to_read) |i| {
        out_buf[i] = data_buf[(current_head + i) % capacity];
    }
    
    @atomicStore(usize, &header.head, current_head + to_read, .release);
    out_len.* = to_read;
    return true;
}

export fn hgb_zig_shm_available(buf_ptr: *anyopaque) callconv(.C) usize {
    const header = @as(*ShmRingHeader, @ptrCast(@alignCast(buf_ptr)));
    const current_tail = @atomicLoad(usize, &header.tail, .acquire);
    const current_head = @atomicLoad(usize, &header.head, .acquire);
    return current_tail -% current_head;
}


// --- Phase 2: Bit-Parallel Myers Diff & LCS Distance Kernel ---

fn bitparallel_lcs(hashes_a: [*]const u64, len_a: usize, hashes_b: [*]const u64, len_b: usize) usize {
    if (len_a == 0 or len_b == 0) return 0;
    
    // Ensure b is the shorter sequence (if one is <= 64, make b <= 64)
    if (len_a < len_b) {
        return bitparallel_lcs(hashes_b, len_b, hashes_a, len_a);
    }
    
    if (len_b <= 64) {
        var v: u64 = 0;
        const mask: u64 = if (len_b == 64) ~@as(u64, 0) else (@as(u64, 1) << @as(u6, @intCast(len_b))) - 1;
        
        for (0..len_a) |i| {
            var pm: u64 = 0;
            const a_val = hashes_a[i];
            for (0..len_b) |j| {
                if (hashes_b[j] == a_val) {
                    pm |= (@as(u64, 1) << @as(u6, @intCast(j)));
                }
            }
            const u = v | pm;
            const shifted_v = (v << 1) | 1;
            const w = u -% shifted_v;
            v = (u & ~w) & mask;
        }
        return @popCount(v);
    }
    
    // Fallback DP LCS for sequences longer than 64 on both sides:
    const row = std.heap.page_allocator.alloc(usize, len_b + 1) catch return 0;
    defer std.heap.page_allocator.free(row);
    @memset(row, 0);
    
    for (0..len_a) |i| {
        var prev_diag: usize = 0;
        for (0..len_b) |j| {
            const temp = row[j + 1];
            if (hashes_a[i] == hashes_b[j]) {
                row[j + 1] = prev_diag + 1;
            } else {
                if (row[j] > row[j + 1]) {
                    row[j + 1] = row[j];
                }
            }
            prev_diag = temp;
        }
    }
    return row[len_b];
}

export fn hgb_zig_myers_diff_distance(hashes_a: [*]const u64, len_a: usize, hashes_b: [*]const u64, len_b: usize) callconv(.C) usize {
    if (len_a == 0) return len_b;
    if (len_b == 0) return len_a;

    var dp = std.heap.page_allocator.alloc(usize, len_b + 1) catch return len_a + len_b;
    defer std.heap.page_allocator.free(dp);

    for (0..len_b + 1) |j| dp[j] = 0;

    for (0..len_a) |i| {
        var prev = dp[0];
        for (0..len_b) |j| {
            const temp = dp[j + 1];
            if (hashes_a[i] == hashes_b[j]) {
                dp[j + 1] = prev + 1;
            } else {
                dp[j + 1] = if (dp[j + 1] > dp[j]) dp[j + 1] else dp[j];
            }
            prev = temp;
        }
    }
    
    const lcs = dp[len_b];
    return len_a + len_b - 2 * lcs;
}
export fn hgb_zig_myers_lcs_similarity(hashes_a: [*]const u64, len_a: usize, hashes_b: [*]const u64, len_b: usize) callconv(.C) f32 {
    if (len_a == 0 and len_b == 0) return 1.0;
    if (len_a == 0 or len_b == 0) return 0.0;
    
    const lcs = bitparallel_lcs(hashes_a, len_a, hashes_b, len_b);
    const max_len = if (len_a > len_b) len_a else len_b;
    return @as(f32, @floatFromInt(lcs)) / @as(f32, @floatFromInt(max_len));
}


// --- Phase 2: Comptime Galois Field GF(2^8) Reed-Solomon Math ---

const GfTables = struct {
    exp: [512]u8,
    log: [256]u8,
};

fn generateGfTables() GfTables {
    @setEvalBranchQuota(100000);
    var exp: [512]u8 = undefined;
    var log: [256]u8 = undefined;
    
    var x: u16 = 1;
    for (0..255) |i| {
        exp[i] = @as(u8, @intCast(x));
        exp[i + 255] = @as(u8, @intCast(x));
        log[x] = @as(u8, @intCast(i));
        x <<= 1;
        if ((x & 0x100) != 0) {
            x ^= 0x11D;
        }
    }
    exp[510] = 0;
    exp[511] = 0;
    log[0] = 0;
    
    return .{ .exp = exp, .log = log };
}

const gf = generateGfTables();

fn gfMul(a: u8, b: u8) u8 {
    if (a == 0 or b == 0) return 0;
    return gf.exp[@as(usize, gf.log[a]) + @as(usize, gf.log[b])];
}

export fn hgb_zig_gf256_poly_mul(p1: [*]const u8, len1: usize, p2: [*]const u8, len2: usize, out: [*]u8) callconv(.C) usize {
    const out_len = len1 + len2 - 1;
    for (0..out_len) |i| out[i] = 0;
    
    for (0..len1) |i| {
        for (0..len2) |j| {
            out[i + j] ^= gfMul(p1[i], p2[j]);
        }
    }
    return out_len;
}

export fn hgb_zig_gf256_rs_encode(data: [*]const u8, data_len: usize, ec_len: usize, out_ec: [*]u8) callconv(.C) void {
    if (ec_len == 0 or data_len == 0) return;
    
    // Generate generator polynomial
    const gen = std.heap.page_allocator.alloc(u8, ec_len + 1) catch return;
    defer std.heap.page_allocator.free(gen);
    
    const next_gen = std.heap.page_allocator.alloc(u8, ec_len + 1) catch return;
    defer std.heap.page_allocator.free(next_gen);
    
    gen[0] = 1;
    var current_len: usize = 1;
    
    for (0..ec_len) |i| {
        const factor = [2]u8{ 1, gf.exp[i] };
        current_len = hgb_zig_gf256_poly_mul(gen.ptr, current_len, &factor, 2, next_gen.ptr);
        for (0..current_len) |j| gen[j] = next_gen[j];
    }
    
    // Compute remainder
    const msg_out = std.heap.page_allocator.alloc(u8, data_len + ec_len) catch return;
    defer std.heap.page_allocator.free(msg_out);
    
    for (0..data_len) |i| msg_out[i] = data[i];
    for (data_len..data_len + ec_len) |i| msg_out[i] = 0;
    
    for (0..data_len) |i| {
        const coef = msg_out[i];
        if (coef != 0) {
            for (0..current_len) |j| {
                msg_out[i + j] ^= gfMul(gen[j], coef);
            }
        }
    }
    
    for (0..ec_len) |i| {
        out_ec[i] = msg_out[data_len + i];
    }
}

// --- Phase 3 & 4 ---

export fn hgb_zig_pagerank_csr(num_nodes: usize, row_offsets: [*]const u32, col_indices: [*]const u32, iterations: usize, damping: f64, out_scores: [*]f64) callconv(.C) void {
    if (num_nodes == 0) return;
    
    var scores = std.heap.page_allocator.alloc(f64, num_nodes) catch return;
    defer std.heap.page_allocator.free(scores);
    var next_scores = std.heap.page_allocator.alloc(f64, num_nodes) catch return;
    defer std.heap.page_allocator.free(next_scores);
    
    const initial_score = 1.0 / @as(f64, @floatFromInt(num_nodes));
    for (0..num_nodes) |i| scores[i] = initial_score;
    
    const base_score = (1.0 - damping) / @as(f64, @floatFromInt(num_nodes));
    
    for (0..iterations) |iter| {
        _ = iter;
        for (0..num_nodes) |i| next_scores[i] = base_score;
        
        for (0..num_nodes) |u| {
            const start = row_offsets[u];
            const end = row_offsets[u + 1];
            const deg = end - start;
            if (deg == 0) continue;
            
            const contribution = damping * scores[u] / @as(f64, @floatFromInt(deg));
            for (start..end) |idx| {
                const v = col_indices[idx];
                next_scores[v] += contribution;
            }
        }
        for (0..num_nodes) |i| scores[i] = next_scores[i];
    }
    
    for (0..num_nodes) |i| out_scores[i] = scores[i];
}

export fn hgb_zig_strip_ansi(input: [*]const u8, in_len: usize, out_buf: [*]u8, out_cap: usize, out_len: *usize) callconv(.C) bool {
    var state: u8 = 0;
    var written: usize = 0;
    
    for (0..in_len) |i| {
        const c = input[i];
        switch (state) {
            0 => {
                if (c == 0x1B) {
                    state = 1;
                } else {
                    if (written < out_cap) {
                        out_buf[written] = c;
                        written += 1;
                    }
                }
            },
            1 => {
                if (c == '[') {
                    state = 2;
                } else {
                    state = 0;
                }
            },
            2 => {
                if (c >= 0x40 and c <= 0x7E) {
                    state = 0;
                }
            },
            else => unreachable,
        }
    }
    
    out_len.* = written;
    return true;
}

export fn hgb_zig_levenshtein_distance(s1: [*]const u8, len1: usize, s2: [*]const u8, len2: usize) callconv(.C) usize {
    if (len1 == 0) return len2;
    if (len2 == 0) return len1;
    
    if (len1 <= 64 and len2 <= 64) {
        var dp_arr: [65]usize = undefined;
        for (0..len2 + 1) |j| dp_arr[j] = j;
        for (0..len1) |i| {
            var prev = dp_arr[0];
            dp_arr[0] = i + 1;
            for (0..len2) |j| {
                const temp = dp_arr[j + 1];
                if (s1[i] == s2[j]) {
                    dp_arr[j + 1] = prev;
                } else {
                    var min_val = prev;
                    if (dp_arr[j] < min_val) min_val = dp_arr[j];
                    if (dp_arr[j + 1] < min_val) min_val = dp_arr[j + 1];
                    dp_arr[j + 1] = min_val + 1;
                }
                prev = temp;
            }
        }
        return dp_arr[len2];
    }
    
    var dp = std.heap.page_allocator.alloc(usize, len2 + 1) catch return len1 + len2;
    defer std.heap.page_allocator.free(dp);
    
    for (0..len2 + 1) |j| dp[j] = j;
    
    for (0..len1) |i| {
        var prev = dp[0];
        dp[0] = i + 1;
        for (0..len2) |j| {
            const temp = dp[j + 1];
            if (s1[i] == s2[j]) {
                dp[j + 1] = prev;
            } else {
                var min_val = prev;
                if (dp[j] < min_val) min_val = dp[j];
                if (dp[j + 1] < min_val) min_val = dp[j + 1];
                dp[j + 1] = min_val + 1;
            }
            prev = temp;
        }
    }
    
    return dp[len2];
}

fn bitReverse2(x: usize, log2n: u5) usize {
    var res: usize = 0;
    var n = x;
    for (0..log2n) |_| {
        res = (res << 1) | (n & 1);
        n >>= 1;
    }
    return res;
}

export fn hgb_zig_dsp_fft_magnitude(samples: [*]const f32, n: usize, out_magnitudes: [*]f32) callconv(.C) void {
    if (n == 0 or (n & (n - 1)) != 0) return;
    if (n > 4096) return;
    
    const log2n = std.math.log2_int(usize, n);
    
    var real: [4096]f32 = undefined;
    var imag: [4096]f32 = undefined;
    
    for (0..n) |i| {
        const rev = bitReverse2(i, @as(u5, @intCast(log2n)));
        real[rev] = samples[i];
        imag[rev] = 0.0;
    }
    
    var s: usize = 1;
    while (s <= log2n) : (s += 1) {
        const m: usize = @as(usize, 1) << @as(u6, @intCast(s));
        const m2 = m >> 1;
        const theta = -2.0 * std.math.pi / @as(f32, @floatFromInt(m));
        
        const wm_real = @cos(theta);
        const wm_imag = @sin(theta);
        
        var k: usize = 0;
        while (k < n) : (k += m) {
            var w_real: f32 = 1.0;
            var w_imag: f32 = 0.0;
            
            for (0..m2) |j| {
                const t_real = w_real * real[k + j + m2] - w_imag * imag[k + j + m2];
                const t_imag = w_real * imag[k + j + m2] + w_imag * real[k + j + m2];
                
                const u_real = real[k + j];
                const u_imag = imag[k + j];
                
                real[k + j] = u_real + t_real;
                imag[k + j] = u_imag + t_imag;
                
                real[k + j + m2] = u_real - t_real;
                imag[k + j + m2] = u_imag - t_imag;
                
                const next_w_real = w_real * wm_real - w_imag * wm_imag;
                const next_w_imag = w_real * wm_imag + w_imag * wm_real;
                w_real = next_w_real;
                w_imag = next_w_imag;
            }
        }
    }
    
    for (0..n) |i| {
        out_magnitudes[i] = @sqrt(real[i] * real[i] + imag[i] * imag[i]);
    }
}

const LANDLOCK_CREATE_RULESET = 444;
const LANDLOCK_ADD_RULE = 445;
const LANDLOCK_RESTRICT_SELF = 446;
const PR_SET_NO_NEW_PRIVS = 38;

const landlock_ruleset_attr = extern struct {
    handled_access_fs: u64,
};

const landlock_path_beneath_attr = extern struct {
    allowed_access: u64,
    parent_fd: i32,
};

export fn hgb_zig_sandbox_check_support() callconv(.C) bool {
    const version = std.os.linux.syscall3(@as(std.os.linux.syscalls.X64, @enumFromInt(444)), 0, 0, 1);
    return version > 0;
}

export fn hgb_zig_sandbox_apply_landlock(allowed_dir: [*]const u8, len: usize) callconv(.C) i32 {
    var path_buf: [4096]u8 = undefined;
    if (len >= 4096) return -1;
    for (0..len) |i| path_buf[i] = allowed_dir[i];
    path_buf[len] = 0;
    
    var attr = landlock_ruleset_attr{ .handled_access_fs = 0x1FFF };
    const fd = std.os.linux.syscall3(@as(std.os.linux.syscalls.X64, @enumFromInt(444)), @intFromPtr(&attr), @sizeOf(landlock_ruleset_attr), 0);
    if (fd < 0) return @as(i32, @intCast(fd));
    
    const dir_fd = std.os.linux.syscall3(std.os.linux.SYS.openat, @as(usize, @bitCast(@as(isize, -100))), @intFromPtr(&path_buf), 0);
    if (@as(isize, @bitCast(dir_fd)) >= 0) {
        var path_attr = landlock_path_beneath_attr{
            .allowed_access = 0x1FFF,
            .parent_fd = @as(i32, @intCast(dir_fd)),
        };
        _ = std.os.linux.syscall4(@as(std.os.linux.syscalls.X64, @enumFromInt(445)), fd, 1, @intFromPtr(&path_attr), 0);
        _ = std.os.linux.syscall1(std.os.linux.SYS.close, dir_fd);
    }
    
    _ = std.os.linux.syscall5(std.os.linux.SYS.prctl, PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0);
    
    const ret = std.os.linux.syscall2(@as(std.os.linux.syscalls.X64, @enumFromInt(446)), fd, 0);
    _ = std.os.linux.syscall1(std.os.linux.SYS.close, fd);
    
    return @as(i32, @intCast(ret));
}

export fn hgb_zig_merkle_root(leaf_hashes_ptr: [*]const u8, leaf_count: usize, out_root: [*]u8) callconv(.C) void {
    const Blake3 = std.crypto.hash.Blake3;
    if (leaf_count == 0) {
        var h = Blake3.init(.{});
        h.update("empty_ledger");
        var out: [32]u8 = undefined;
        h.final(&out);
        @memcpy(out_root[0..32], &out);
        return;
    }
    if (leaf_count == 1) {
        @memcpy(out_root[0..32], leaf_hashes_ptr[0..32]);
        return;
    }

    var stack_buf: [256][32]u8 = undefined;
    var heap_buf: ?[][32]u8 = null;
    defer {
        if (heap_buf) |hb| std.heap.page_allocator.free(hb);
    }

    var working_buf: [][32]u8 = undefined;
    if (leaf_count <= 256) {
        working_buf = stack_buf[0..leaf_count];
    } else {
        heap_buf = std.heap.page_allocator.alloc([32]u8, leaf_count) catch {
            @memset(out_root[0..32], 0);
            return;
        };
        working_buf = heap_buf.?;
    }

    for (0..leaf_count) |i| {
        @memcpy(&working_buf[i], leaf_hashes_ptr[i * 32 .. (i + 1) * 32]);
    }

    var n = leaf_count;
    while (n > 1) {
        const half = n / 2;
        var i: usize = 0;
        while (i < half) : (i += 1) {
            var h = Blake3.init(.{});
            h.update(&working_buf[2 * i]);
            h.update(&working_buf[2 * i + 1]);
            var out: [32]u8 = undefined;
            h.final(&out);
            working_buf[i] = out;
        }
        if (n % 2 == 1) {
            working_buf[half] = working_buf[n - 1];
        }
        n = (n + 1) / 2;
    }

    @memcpy(out_root[0..32], &working_buf[0]);
}

export fn hgb_zig_blake3_node_pair(left: [*]const u8, left_len: usize, right: [*]const u8, right_len: usize, out_hex: [*]u8) callconv(.C) void {
    const Blake3 = std.crypto.hash.Blake3;
    var h = Blake3.init(.{});
    h.update("HGB_MERKLE_NODE:");
    h.update(left[0..left_len]);
    h.update(right[0..right_len]);
    var out: [32]u8 = undefined;
    h.final(&out);
    const charset = "0123456789abcdef";
    for (0..32) |i| {
        out_hex[i * 2] = charset[out[i] >> 4];
        out_hex[i * 2 + 1] = charset[out[i] & 0x0F];
    }
}

export fn hgb_zig_dsp_resample(in_samples: [*]const i16, in_len: usize, in_rate: u32, out_samples: [*]i16, out_cap: usize, out_rate: u32) callconv(.C) usize {
    if (in_len == 0 or in_rate == 0 or out_rate == 0 or out_cap == 0) return 0;
    if (in_rate == out_rate) {
        const copy_len = @min(in_len, out_cap);
        for (0..copy_len) |i| out_samples[i] = in_samples[i];
        return copy_len;
    }
    const ratio: f64 = @as(f64, @floatFromInt(in_rate)) / @as(f64, @floatFromInt(out_rate));
    const target_len = @min(@as(usize, @intFromFloat(@as(f64, @floatFromInt(in_len)) / ratio)), out_cap);
    
    var j: usize = 0;
    while (j < target_len) : (j += 1) {
        const src_pos = @as(f64, @floatFromInt(j)) * ratio;
        const idx = @as(usize, @intFromFloat(src_pos));
        if (idx + 1 < in_len) {
            const frac = @as(f32, @floatCast(src_pos - @as(f64, @floatFromInt(idx))));
            const s0 = @as(f32, @floatFromInt(in_samples[idx]));
            const s1 = @as(f32, @floatFromInt(in_samples[idx + 1]));
            const interpolated = (1.0 - frac) * s0 + frac * s1;
            const clamped = std.math.clamp(interpolated, -32768.0, 32767.0);
            out_samples[j] = @as(i16, @intFromFloat(clamped));
        } else if (idx < in_len) {
            out_samples[j] = in_samples[idx];
        } else {
            out_samples[j] = 0;
        }
    }
    return target_len;
}


pub const mcp = @import("hgb_mcp.zig");
comptime {
    _ = mcp;
}
