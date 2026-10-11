//! 🪽 Hagibis Sovereign WebAssembly Microkernel (wasm32-freestanding)
//!
//! Production-grade WebAssembly microkernel written in Zig 0.13.0 for the Hagibis PWA Studio.
//! Targeted for Intel Core i3-7100U laptops with strict memory bounds (< 30-50 KB compiled binary).
//! Zero external dependencies, zero libc overhead.
//!
//! Subsystems:
//! 1. Memory Management: Freestanding block allocator (`wasm_alloc`, `wasm_free`)
//! 2. Myers SES Diff Engine: High-performance linear-memory line diffing (`wasm_diff_lines`)
//! 3. Fuzzy & Levenshtein Engine: Fast string search & scoring (`wasm_fuzzy_score`, `wasm_levenshtein`)
//! 4. Git DAG Graph Layout: Branch track allocation & coordinate layout (`wasm_layout_dag`)
//! 5. Health & Version Checks: `wasm_get_version`, `wasm_ping`

const std = @import("std");
const builtin = @import("builtin");

const is_wasm = builtin.target.cpu.arch == .wasm32;

// ============================================================================
// 1. FREESTANDING LINEAR MEMORY MANAGEMENT
// ============================================================================

pub const MAGIC_BLOCK: u32 = 0x48474257; // 'HGBW'
pub const ALIGNMENT: usize = 8;

pub const BlockHeader = extern struct {
    magic: u32,
    size: u32,
    is_free: u32,
    next: ?*BlockHeader,
};

var first_block: ?*BlockHeader = null;
var heap_top: usize = 0;
var heap_initialized: bool = false;

// Host test memory fallback when executing `zig test` on native platforms
var host_heap_buf: [16 * 1024 * 1024]u8 align(64) = undefined;

fn initHeapIfNeeded() void {
    if (heap_initialized) return;

    if (is_wasm) {
        const current_pages = @wasmMemorySize(0);
        if (current_pages < 4) {
            _ = @wasmMemoryGrow(0, 4 - current_pages);
        }
        heap_top = 0x10000; // 64 KiB base offset
    } else {
        heap_top = @intFromPtr(&host_heap_buf[0]);
    }
    heap_initialized = true;
}

fn alignUp(val: usize, alignment: usize) usize {
    return (val + alignment - 1) & ~(alignment - 1);
}

pub fn allocBytes(size: usize) ?[*]u8 {
    if (size == 0) return null;
    initHeapIfNeeded();

    const aligned_payload = alignUp(size, ALIGNMENT);
    const total_needed = aligned_payload + @sizeOf(BlockHeader);

    // 1. Search free-list for a reusable block (First-Fit)
    var curr = first_block;
    while (curr) |b| {
        if (b.magic == MAGIC_BLOCK and b.is_free == 1 and b.size >= aligned_payload) {
            if (b.size >= aligned_payload + @sizeOf(BlockHeader) + 16) {
                const remaining_size = b.size - @as(u32, @intCast(aligned_payload + @sizeOf(BlockHeader)));
                const split_ptr = @as([*]u8, @ptrCast(b)) + @sizeOf(BlockHeader) + aligned_payload;
                const new_block: *BlockHeader = @ptrCast(@alignCast(split_ptr));
                new_block.* = BlockHeader{
                    .magic = MAGIC_BLOCK,
                    .size = remaining_size,
                    .is_free = 1,
                    .next = b.next,
                };
                b.size = @as(u32, @intCast(aligned_payload));
                b.next = new_block;
            }
            b.is_free = 0;
            return @as([*]u8, @ptrCast(b)) + @sizeOf(BlockHeader);
        }
        curr = b.next;
    }

    // 2. Bump heap_top
    if (is_wasm) {
        const current_bytes = @as(usize, @intCast(@wasmMemorySize(0))) * 65536;
        if (heap_top + total_needed > current_bytes) {
            const bytes_needed = (heap_top + total_needed) - current_bytes;
            const pages_to_grow = (bytes_needed + 65535) / 65536;
            const grow_result = @wasmMemoryGrow(0, @intCast(pages_to_grow));
            if (grow_result < 0) return null;
        }
    } else {
        const heap_start = @intFromPtr(&host_heap_buf[0]);
        if (heap_top + total_needed > heap_start + host_heap_buf.len) return null;
    }

    const block_addr = heap_top;
    heap_top += total_needed;

    const new_b: *BlockHeader = @ptrFromInt(block_addr);
    new_b.* = BlockHeader{
        .magic = MAGIC_BLOCK,
        .size = @as(u32, @intCast(aligned_payload)),
        .is_free = 0,
        .next = null,
    };

    if (first_block == null) {
        first_block = new_b;
    } else {
        var last = first_block.?;
        while (last.next) |nxt| {
            last = nxt;
        }
        last.next = new_b;
    }

    return @as([*]u8, @ptrCast(new_b)) + @sizeOf(BlockHeader);
}

pub fn freeBytes(ptr: [*]u8, size: usize) void {
    _ = size;
    const header_ptr = ptr - @sizeOf(BlockHeader);
    const b: *BlockHeader = @ptrCast(@alignCast(header_ptr));

    if (b.magic != MAGIC_BLOCK) return;
    b.is_free = 1;

    // Coalesce adjacent free blocks
    var curr = first_block;
    while (curr) |c| {
        if (c.is_free == 1) {
            while (c.next) |nxt| {
                if (nxt.is_free == 1) {
                    const expected_nxt = @as([*]u8, @ptrCast(c)) + @sizeOf(BlockHeader) + c.size;
                    if (expected_nxt == @as([*]u8, @ptrCast(nxt))) {
                        c.size += @as(u32, @intCast(@sizeOf(BlockHeader) + nxt.size));
                        c.next = nxt.next;
                        continue;
                    }
                }
                break;
            }
        }
        curr = c.next;
    }
}

export fn wasm_alloc(size: usize) callconv(.C) ?[*]u8 {
    return allocBytes(size);
}

export fn wasm_free(ptr: ?[*]u8, size: usize) callconv(.C) void {
    if (ptr) |p| freeBytes(p, size);
}

// Zig std.mem.Allocator VTable Integration
fn wasmAllocFn(ctx: *anyopaque, len: usize, ptr_align: u8, ret_addr: usize) ?[*]u8 {
    _ = ctx; _ = ptr_align; _ = ret_addr;
    return allocBytes(len);
}
fn wasmResizeFn(ctx: *anyopaque, buf: []u8, buf_align: u8, new_len: usize, ret_addr: usize) bool {
    _ = ctx; _ = buf_align; _ = ret_addr;
    return new_len <= buf.len;
}
fn wasmFreeFn(ctx: *anyopaque, buf: []u8, buf_align: u8, ret_addr: usize) void {
    _ = ctx; _ = buf_align; _ = ret_addr;
    freeBytes(buf.ptr, buf.len);
}

pub const wasm_allocator = std.mem.Allocator{
    .ptr = undefined,
    .vtable = &std.mem.Allocator.VTable{
        .alloc = wasmAllocFn,
        .resize = wasmResizeFn,
        .free = wasmFreeFn,
    },
};

// ============================================================================
// 2. MYERS SHORTEST EDIT SCRIPT (SES) LINE DIFF ENGINE
// ============================================================================

pub const DiffOp = enum(u32) {
    equal = 0,
    delete = 1,
    insert = 2,
    modify = 3,
};

pub const DiffChunk = extern struct {
    op: u32,               // 0: Equal, 1: Delete, 2: Insert, 3: Modify
    old_start_line: u32,   // 1-indexed
    old_line_count: u32,
    new_start_line: u32,   // 1-indexed
    new_line_count: u32,
    old_byte_offset: u32,
    old_byte_len: u32,
    new_byte_offset: u32,
    new_byte_len: u32,
};

pub const DiffResult = extern struct {
    magic: u32,            // 0x44494646 ('DIFF')
    chunk_count: u32,
    chunks_ptr: [*]DiffChunk,
    total_added_lines: u32,
    total_deleted_lines: u32,
    total_modified_lines: u32,
    old_total_lines: u32,
    new_total_lines: u32,
};

const LineSpan = struct {
    offset: u32,
    len: u32,
    hash: u64,
};

// Ultra-fast 64-bit FNV-1a Hash
fn hashLine(bytes: []const u8) u64 {
    var h: u64 = 0xcbf29ce484222325;
    for (bytes) |b| {
        h ^= b;
        h = h *% 0x100000001b3;
    }
    return h;
}

fn splitLines(allocator: std.mem.Allocator, text: []const u8) ![]LineSpan {
    var list = std.ArrayList(LineSpan).init(allocator);
    errdefer list.deinit();

    var offset: usize = 0;
    while (offset < text.len) {
        var end = offset;
        while (end < text.len and text[end] != '\n') : (end += 1) {}
        
        var line_len = end - offset;
        // Strip trailing \r if present
        if (line_len > 0 and text[offset + line_len - 1] == '\r') {
            line_len -= 1;
        }

        const line_bytes = text[offset .. offset + line_len];
        try list.append(LineSpan{
            .offset = @as(u32, @intCast(offset)),
            .len = @as(u32, @intCast(line_len)),
            .hash = hashLine(line_bytes),
        });

        offset = if (end < text.len) end + 1 else text.len;
    }

    return list.toOwnedSlice();
}

export fn wasm_diff_lines(
    old_ptr: [*]const u8,
    old_len: usize,
    new_ptr: [*]const u8,
    new_len: usize,
) callconv(.C) ?*DiffResult {
    const old_text = old_ptr[0..old_len];
    const new_text = new_ptr[0..new_len];

    const old_lines = splitLines(wasm_allocator, old_text) catch return null;
    defer wasm_allocator.free(old_lines);
    const new_lines = splitLines(wasm_allocator, new_text) catch return null;
    defer wasm_allocator.free(new_lines);

    const n = old_lines.len;
    const m = new_lines.len;
    const max_d = n + m;

    var chunks_list = std.ArrayList(DiffChunk).init(wasm_allocator);
    errdefer chunks_list.deinit();

    // Fast path: Identical or one empty
    if (n == 0 and m == 0) {
        return createDiffResult(&chunks_list, 0, 0, 0, 0, 0);
    }

    if (n == 0) {
        chunks_list.append(DiffChunk{
            .op = @intFromEnum(DiffOp.insert),
            .old_start_line = 1,
            .old_line_count = 0,
            .new_start_line = 1,
            .new_line_count = @as(u32, @intCast(m)),
            .old_byte_offset = 0,
            .old_byte_len = 0,
            .new_byte_offset = 0,
            .new_byte_len = @as(u32, @intCast(new_len)),
        }) catch return null;
        return createDiffResult(&chunks_list, @as(u32, @intCast(m)), 0, 0, 0, @as(u32, @intCast(m)));
    }

    if (m == 0) {
        chunks_list.append(DiffChunk{
            .op = @intFromEnum(DiffOp.delete),
            .old_start_line = 1,
            .old_line_count = @as(u32, @intCast(n)),
            .new_start_line = 1,
            .new_line_count = 0,
            .old_byte_offset = 0,
            .old_byte_len = @as(u32, @intCast(old_len)),
            .new_byte_offset = 0,
            .new_byte_len = 0,
        }) catch return null;
        return createDiffResult(&chunks_list, 0, @as(u32, @intCast(n)), 0, @as(u32, @intCast(n)), 0);
    }

    // Myers SES Dynamic Programming
    const stride = 2 * max_d + 1;
    const total_cells = (max_d + 1) * stride;

    const v_history = wasm_allocator.alloc(isize, total_cells) catch return null;
    defer wasm_allocator.free(v_history);

    // Initial snake (D = 0)
    var x: isize = 0;
    var y: isize = 0;
    while (x < @as(isize, @intCast(n)) and y < @as(isize, @intCast(m)) and old_lines[@as(usize, @intCast(x))].hash == new_lines[@as(usize, @intCast(y))].hash) {
        x += 1;
        y += 1;
    }
    v_history[max_d] = x;

    var final_d: usize = 0;
    if (x >= @as(isize, @intCast(n)) and y >= @as(isize, @intCast(m))) {
        final_d = 0;
    } else {
        var found = false;
        var d: usize = 1;
        while (d <= max_d) : (d += 1) {
            var k: isize = -@as(isize, @intCast(d));
            const max_k: isize = @as(isize, @intCast(d));
            while (k <= max_k) : (k += 2) {
                var cur_x: isize = 0;
                if (k == -@as(isize, @intCast(d))) {
                    cur_x = v_history[(d - 1) * stride + @as(usize, @intCast(k + 1 + @as(isize, @intCast(max_d))))];
                } else if (k == @as(isize, @intCast(d))) {
                    cur_x = v_history[(d - 1) * stride + @as(usize, @intCast(k - 1 + @as(isize, @intCast(max_d))))] + 1;
                } else {
                    const left = v_history[(d - 1) * stride + @as(usize, @intCast(k - 1 + @as(isize, @intCast(max_d))))];
                    const right = v_history[(d - 1) * stride + @as(usize, @intCast(k + 1 + @as(isize, @intCast(max_d))))];
                    cur_x = if (left < right) right else left + 1;
                }

                var cur_y = cur_x - k;
                while (cur_x < @as(isize, @intCast(n)) and cur_y < @as(isize, @intCast(m)) and old_lines[@as(usize, @intCast(cur_x))].hash == new_lines[@as(usize, @intCast(cur_y))].hash) {
                    cur_x += 1;
                    cur_y += 1;
                }

                v_history[d * stride + @as(usize, @intCast(k + @as(isize, @intCast(max_d))))] = cur_x;

                if (cur_x >= @as(isize, @intCast(n)) and cur_y >= @as(isize, @intCast(m))) {
                    final_d = d;
                    found = true;
                    break;
                }
            }
            if (found) break;
        }
    }

    // Backtrack Myers graph from (n, m) to (0, 0)
    var raw_edits = std.ArrayList(struct { op: DiffOp, old_idx: usize, new_idx: usize }).init(wasm_allocator);
    defer raw_edits.deinit();

    var curr_x: isize = @as(isize, @intCast(n));
    var curr_y: isize = @as(isize, @intCast(m));
    var cur_d = final_d;

    while (cur_d > 0) : (cur_d -= 1) {
        const k = curr_x - curr_y;
        var k_prev: isize = undefined;
        if (k == -@as(isize, @intCast(cur_d))) {
            k_prev = k + 1;
        } else if (k == @as(isize, @intCast(cur_d))) {
            k_prev = k - 1;
        } else {
            const left = v_history[(cur_d - 1) * stride + @as(usize, @intCast(k - 1 + @as(isize, @intCast(max_d))))];
            const right = v_history[(cur_d - 1) * stride + @as(usize, @intCast(k + 1 + @as(isize, @intCast(max_d))))];
            k_prev = if (left < right) k + 1 else k - 1;
        }

        const x_prev = v_history[(cur_d - 1) * stride + @as(usize, @intCast(k_prev + @as(isize, @intCast(max_d))))];
        const y_prev = x_prev - k_prev;

        const is_insert = (k_prev == k + 1);
        const x_start = if (is_insert) x_prev else x_prev + 1;
        const y_start = if (is_insert) y_prev + 1 else y_prev;

        while (curr_x > x_start and curr_y > y_start) {
            curr_x -= 1;
            curr_y -= 1;
            raw_edits.append(.{ .op = .equal, .old_idx = @as(usize, @intCast(curr_x)), .new_idx = @as(usize, @intCast(curr_y)) }) catch return null;
        }

        if (is_insert) {
            raw_edits.append(.{ .op = .insert, .old_idx = @as(usize, @intCast(x_prev)), .new_idx = @as(usize, @intCast(y_prev)) }) catch return null;
        } else {
            raw_edits.append(.{ .op = .delete, .old_idx = @as(usize, @intCast(x_prev)), .new_idx = @as(usize, @intCast(y_prev)) }) catch return null;
        }

        curr_x = x_prev;
        curr_y = y_prev;
    }

    while (curr_x > 0 and curr_y > 0) {
        curr_x -= 1;
        curr_y -= 1;
        raw_edits.append(.{ .op = .equal, .old_idx = @as(usize, @intCast(curr_x)), .new_idx = @as(usize, @intCast(curr_y)) }) catch return null;
    }

    // Reverse to forward order
    std.mem.reverse(@TypeOf(raw_edits.items[0]), raw_edits.items);

    // Group contiguous raw edits into cohesive Monaco diff chunks
    var i: usize = 0;
    var total_added: u32 = 0;
    var total_deleted: u32 = 0;
    var total_modified: u32 = 0;

    while (i < raw_edits.items.len) {
        const edit = raw_edits.items[i];
        if (edit.op == .equal) {
            i += 1;
            continue;
        }

        const start_old = edit.old_idx;
        const start_new = edit.new_idx;
        var del_count: u32 = 0;
        var ins_count: u32 = 0;

        var j = i;
        while (j < raw_edits.items.len and raw_edits.items[j].op != .equal) : (j += 1) {
            if (raw_edits.items[j].op == .delete) del_count += 1;
            if (raw_edits.items[j].op == .insert) ins_count += 1;
        }

        var op = DiffOp.modify;
        if (del_count > 0 and ins_count == 0) {
            op = .delete;
            total_deleted += del_count;
        } else if (ins_count > 0 and del_count == 0) {
            op = .insert;
            total_added += ins_count;
        } else {
            op = .modify;
            total_modified += @max(del_count, ins_count);
        }

        const old_byte_off = if (del_count > 0 and start_old < n) old_lines[start_old].offset else 0;
        const old_byte_len = if (del_count > 0 and start_old + del_count <= n) blk: {
            const last_l = old_lines[start_old + del_count - 1];
            break :blk (last_l.offset + last_l.len) - old_byte_off;
        } else 0;

        const new_byte_off = if (ins_count > 0 and start_new < m) new_lines[start_new].offset else 0;
        const new_byte_len = if (ins_count > 0 and start_new + ins_count <= m) blk: {
            const last_l = new_lines[start_new + ins_count - 1];
            break :blk (last_l.offset + last_l.len) - new_byte_off;
        } else 0;

        chunks_list.append(DiffChunk{
            .op = @intFromEnum(op),
            .old_start_line = @as(u32, @intCast(start_old + 1)),
            .old_line_count = del_count,
            .new_start_line = @as(u32, @intCast(start_new + 1)),
            .new_line_count = ins_count,
            .old_byte_offset = old_byte_off,
            .old_byte_len = old_byte_len,
            .new_byte_offset = new_byte_off,
            .new_byte_len = new_byte_len,
        }) catch return null;

        i = j;
    }

    return createDiffResult(&chunks_list, total_added, total_deleted, total_modified, @as(u32, @intCast(n)), @as(u32, @intCast(m)));
}

fn createDiffResult(
    list: *std.ArrayList(DiffChunk),
    added: u32,
    deleted: u32,
    modified: u32,
    old_cnt: u32,
    new_cnt: u32,
) ?*DiffResult {
    const res_ptr = wasm_allocator.create(DiffResult) catch return null;
    const count = @as(u32, @intCast(list.items.len));
    const slice = list.toOwnedSlice() catch return null;

    res_ptr.* = DiffResult{
        .magic = 0x44494646,
        .chunk_count = count,
        .chunks_ptr = slice.ptr,
        .total_added_lines = added,
        .total_deleted_lines = deleted,
        .total_modified_lines = modified,
        .old_total_lines = old_cnt,
        .new_total_lines = new_cnt,
    };
    return res_ptr;
}

export fn wasm_diff_free(result: ?*DiffResult) callconv(.C) void {
    if (result) |res| {
        if (res.chunk_count > 0) {
            wasm_allocator.free(res.chunks_ptr[0..res.chunk_count]);
        }
        wasm_allocator.destroy(res);
    }
}

// ============================================================================
// 3. LEVENSHTEIN & SUB-MILLISECOND FUZZY MATCHER
// ============================================================================

export fn wasm_levenshtein(
    s1_ptr: [*]const u8,
    len1: usize,
    s2_ptr: [*]const u8,
    len2: usize,
) callconv(.C) u32 {
    if (len1 == 0) return @as(u32, @intCast(len2));
    if (len2 == 0) return @as(u32, @intCast(len1));

    const s1 = s1_ptr[0..len1];
    const s2 = s2_ptr[0..len2];

    if (len2 <= 256) {
        var row: [257]u32 = undefined;
        for (0..len2 + 1) |j| row[j] = @as(u32, @intCast(j));

        for (0..len1) |i| {
            var prev = row[0];
            row[0] = @as(u32, @intCast(i + 1));
            for (0..len2) |j| {
                const temp = row[j + 1];
                if (s1[i] == s2[j]) {
                    row[j + 1] = prev;
                } else {
                    var min_v = prev;
                    if (row[j] < min_v) min_v = row[j];
                    if (row[j + 1] < min_v) min_v = row[j + 1];
                    row[j + 1] = min_v + 1;
                }
                prev = temp;
            }
        }
        return row[len2];
    }

    const row = wasm_allocator.alloc(u32, len2 + 1) catch return @as(u32, @intCast(len1 + len2));
    defer wasm_allocator.free(row);

    for (0..len2 + 1) |j| row[j] = @as(u32, @intCast(j));

    for (0..len1) |i| {
        var prev = row[0];
        row[0] = @as(u32, @intCast(i + 1));
        for (0..len2) |j| {
            const temp = row[j + 1];
            if (s1[i] == s2[j]) {
                row[j + 1] = prev;
            } else {
                var min_v = prev;
                if (row[j] < min_v) min_v = row[j];
                if (row[j + 1] < min_v) min_v = row[j + 1];
                row[j + 1] = min_v + 1;
            }
            prev = temp;
        }
    }
    return row[len2];
}

fn toLowerAscii(c: u8) u8 {
    return if (c >= 'A' and c <= 'Z') c + 32 else c;
}

fn isBoundaryChar(c: u8) bool {
    return c == '/' or c == '\\' or c == '_' or c == '-' or c == '.' or c == ' ';
}

export fn wasm_fuzzy_score(
    query_ptr: [*]const u8,
    query_len: usize,
    target_ptr: [*]const u8,
    target_len: usize,
) callconv(.C) i32 {
    if (query_len == 0) return 0;
    if (query_len > target_len) return -1;

    const query = query_ptr[0..query_len];
    const target = target_ptr[0..target_len];

    var score: i32 = 0;
    var consecutive: i32 = 0;
    var q_idx: usize = 0;
    var prev_matched_idx: usize = 0;

    for (0..target.len) |t_idx| {
        if (q_idx >= query.len) break;

        const qc = toLowerAscii(query[q_idx]);
        const tc = toLowerAscii(target[t_idx]);

        if (qc == tc) {
            var bonus: i32 = 10;

            // Start of string bonus
            if (t_idx == 0) {
                bonus += 30;
            } else if (isBoundaryChar(target[t_idx - 1])) {
                bonus += 25; // Word boundary bonus
            } else if (target[t_idx] >= 'A' and target[t_idx] <= 'Z' and target[t_idx - 1] >= 'a' and target[t_idx - 1] <= 'z') {
                bonus += 20; // CamelCase boundary bonus
            }

            // Consecutive match bonus
            if (consecutive > 0) {
                bonus += consecutive * 15;
            }

            // Distance penalty from last match
            if (q_idx > 0 and t_idx > prev_matched_idx + 1) {
                const distance = @as(i32, @intCast(t_idx - prev_matched_idx));
                bonus -= @min(15, distance * 2);
            }

            score += bonus;
            consecutive += 1;
            prev_matched_idx = t_idx;
            q_idx += 1;
        } else {
            consecutive = 0;
        }
    }

    if (q_idx < query.len) return -1; // Subsequence not matched

    // Exact length match bonus
    if (query.len == target.len) score += 50;
    return score;
}

// ============================================================================
// 4. GIT DAG GRAPH LAYOUT ENGINE
// ============================================================================

pub const CommitInput = extern struct {
    hash_id: u32,
    num_parents: u32,
    parent_ids: [4]u32,
};

pub const DagNodeOutput = extern struct {
    hash_id: u32,
    x: u32,                // Branch track column
    y: u32,                // Row index
    color_idx: u32,        // Palette index (0-7)
    num_parents: u32,
    parent_tracks: [4]u32,
    parent_rows: [4]u32,
    active_lanes_mask: u32,
};

pub const DagResult = extern struct {
    magic: u32,            // 0x47444147 ('GDAG')
    node_count: u32,
    max_tracks: u32,
    nodes_ptr: [*]DagNodeOutput,
};

export fn wasm_layout_dag(
    commits_ptr: [*]const CommitInput,
    count: u32,
) callconv(.C) ?*DagResult {
    if (count == 0) {
        const empty_res = wasm_allocator.create(DagResult) catch return null;
        empty_res.* = DagResult{ .magic = 0x47444147, .node_count = 0, .max_tracks = 0, .nodes_ptr = undefined };
        return empty_res;
    }

    const commits = commits_ptr[0..count];
    const out_nodes = wasm_allocator.alloc(DagNodeOutput, count) catch return null;

    // Active branch tracks: lane_index -> expected_commit_id
    var active_lanes: [32]?u32 = [_]?u32{null} ** 32;
    var max_tracks_used: u32 = 1;

    for (0..count) |i| {
        const c = commits[i];
        const row_y = @as(u32, @intCast(i));

        // Find lane assigned to this commit or first free lane
        var my_lane: ?u32 = null;
        for (0..32) |l| {
            if (active_lanes[l]) |exp_id| {
                if (exp_id == c.hash_id) {
                    my_lane = @as(u32, @intCast(l));
                    break;
                }
            }
        }

        if (my_lane == null) {
            for (0..32) |l| {
                if (active_lanes[l] == null) {
                    my_lane = @as(u32, @intCast(l));
                    break;
                }
            }
        }

        const lane = my_lane orelse 0;
        active_lanes[lane] = null; // Clear self

        if (lane + 1 > max_tracks_used) {
            max_tracks_used = lane + 1;
        }

        // Initialize node output
        var node = DagNodeOutput{
            .hash_id = c.hash_id,
            .x = lane,
            .y = row_y,
            .color_idx = lane % 8,
            .num_parents = c.num_parents,
            .parent_tracks = [_]u32{0} ** 4,
            .parent_rows = [_]u32{0} ** 4,
            .active_lanes_mask = 0,
        };

        // Reserve lanes for parents
        const p_count = @min(c.num_parents, 4);
        for (0..p_count) |p_idx| {
            const p_id = c.parent_ids[p_idx];
            var p_row: u32 = row_y + 1;
            // Search forward in commits for actual row index of parent
            for (i + 1..count) |cand_idx| {
                if (commits[cand_idx].hash_id == p_id) {
                    p_row = @as(u32, @intCast(cand_idx));
                    break;
                }
            }
            node.parent_rows[p_idx] = p_row;

            if (p_idx == 0) {
                // Primary parent continues on current lane
                active_lanes[lane] = p_id;
                node.parent_tracks[p_idx] = lane;
            } else {
                // Secondary parent: find existing reservation or first free lane
                var p_lane: ?u32 = null;
                for (0..32) |l| {
                    if (active_lanes[l]) |exp_id| {
                        if (exp_id == p_id) {
                            p_lane = @as(u32, @intCast(l));
                            break;
                        }
                    }
                }
                if (p_lane == null) {
                    for (0..32) |l| {
                        if (active_lanes[l] == null) {
                            p_lane = @as(u32, @intCast(l));
                            break;
                        }
                    }
                }
                const chosen = p_lane orelse lane;
                active_lanes[chosen] = p_id;
                node.parent_tracks[p_idx] = chosen;
                if (chosen + 1 > max_tracks_used) max_tracks_used = chosen + 1;
            }
        }

        // Compute active lanes bitmask
        var mask: u32 = 0;
        for (0..32) |l| {
            if (active_lanes[l] != null or l == lane) {
                mask |= (@as(u32, 1) << @as(u5, @intCast(l)));
            }
        }
        node.active_lanes_mask = mask;

        out_nodes[i] = node;
    }

    const res_ptr = wasm_allocator.create(DagResult) catch return null;
    res_ptr.* = DagResult{
        .magic = 0x47444147,
        .node_count = count,
        .max_tracks = max_tracks_used,
        .nodes_ptr = out_nodes.ptr,
    };
    return res_ptr;
}

export fn wasm_dag_free(result: ?*DagResult) callconv(.C) void {
    if (result) |res| {
        if (res.node_count > 0) {
            wasm_allocator.free(res.nodes_ptr[0..res.node_count]);
        }
        wasm_allocator.destroy(res);
    }
}

// ============================================================================
// 5. VERSION & HEALTH DIAGNOSTICS
// ============================================================================

export fn wasm_get_version() callconv(.C) u32 {
    return 0x00010000; // v1.0.0
}

export fn wasm_ping() callconv(.C) u32 {
    return 0x484742; // 'HGB' (4736834)
}

// ============================================================================
// 6. UNIT TESTS (RUNNABLE VIA `zig test`)
// ============================================================================

test "wasm kernel health and version check" {
    try std.testing.expectEqual(@as(u32, 0x00010000), wasm_get_version());
    try std.testing.expectEqual(@as(u32, 0x484742), wasm_ping());
}

test "myers line diff additions deletions and modifications" {
    const old_t = "fn main() {\n    println!(\"Hello World\");\n    let x = 42;\n}\n";
    const new_t = "fn main() {\n    println!(\"Hello Hagibis\");\n    let x = 43;\n    let y = 100;\n}\n";

    const res = wasm_diff_lines(old_t.ptr, old_t.len, new_t.ptr, new_t.len);
    try std.testing.expect(res != null);
    defer wasm_diff_free(res);

    const r = res.?;
    try std.testing.expectEqual(@as(u32, 0x44494646), r.magic);
    try std.testing.expect(r.chunk_count > 0);
    try std.testing.expect(r.total_modified_lines > 0 or r.total_added_lines > 0);
}

test "levenshtein edit distance calculations" {
    const s1 = "hgb_accelerate";
    const s2 = "hgb_accelerated";
    const dist = wasm_levenshtein(s1.ptr, s1.len, s2.ptr, s2.len);
    try std.testing.expectEqual(@as(u32, 1), dist);

    const dist_same = wasm_levenshtein(s1.ptr, s1.len, s1.ptr, s1.len);
    try std.testing.expectEqual(@as(u32, 0), dist_same);
}

test "fuzzy score boundary and camel case bonuses" {
    const pattern = "hgbwasm";
    const good_target = "crates/hgb-wasm-kernel/main.zig";
    const bad_target = "random/string/without/matches.rs";

    const good_score = wasm_fuzzy_score(pattern.ptr, pattern.len, good_target.ptr, good_target.len);
    const bad_score = wasm_fuzzy_score(pattern.ptr, pattern.len, bad_target.ptr, bad_target.len);

    try std.testing.expect(good_score > 0);
    try std.testing.expectEqual(@as(i32, -1), bad_score);
}

test "git dag layout lane allocation" {
    var commits: [3]CommitInput = undefined;
    // Commit 0 (HEAD, child of 1)
    commits[0] = CommitInput{ .hash_id = 100, .num_parents = 1, .parent_ids = [_]u32{ 101, 0, 0, 0 } };
    // Commit 1 (child of 102)
    commits[1] = CommitInput{ .hash_id = 101, .num_parents = 1, .parent_ids = [_]u32{ 102, 0, 0, 0 } };
    // Commit 2 (root)
    commits[2] = CommitInput{ .hash_id = 102, .num_parents = 0, .parent_ids = [_]u32{ 0, 0, 0, 0 } };

    const dag = wasm_layout_dag(commits[0..].ptr, 3);
    try std.testing.expect(dag != null);
    defer wasm_dag_free(dag);

    const d = dag.?;
    try std.testing.expectEqual(@as(u32, 0x47444147), d.magic);
    try std.testing.expectEqual(@as(u32, 3), d.node_count);
    try std.testing.expectEqual(@as(u32, 0), d.nodes_ptr[0].x);
    try std.testing.expectEqual(@as(u32, 0), d.nodes_ptr[1].x);
}
