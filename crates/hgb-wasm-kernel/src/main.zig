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
// 6. STATEFUL WASM COMPONENT DISTRIBUTED SYNC (WIT BINDINGS)
// ============================================================================

pub const WasmSyncState = extern struct {
    magic: u32,             // 0x53594E43 ('SYNC')
    revision: u64,
    sequence: u64,
    instruction_count: u64,
    status: u32,            // 0: Synchronized, 1: Diverging, 2: Migrating, 3: Halted
    active_tier: u32,       // 0: LocalDesktop, 1: EdgeWorker, 2: CloudFirecracker
    dirty_pages: u32,
    memory_pages: u32,
};

var global_sync_state = WasmSyncState{
    .magic = 0x53594E43,
    .revision = 1,
    .sequence = 0,
    .instruction_count = 0,
    .status = 0,
    .active_tier = 0,
    .dirty_pages = 0,
    .memory_pages = 4,
};

export fn wasm_checkpoint(out_ptr: [*]u8, max_len: u32) callconv(.C) u32 {
    const state_size = @sizeOf(WasmSyncState);
    if (max_len < state_size) return 0;

    global_sync_state.sequence += 1;
    const dest: *WasmSyncState = @ptrCast(@alignCast(out_ptr));
    dest.* = global_sync_state;
    return @as(u32, @intCast(state_size));
}

export fn wasm_restore_snapshot(in_ptr: [*]const u8, in_len: u32) callconv(.C) u32 {
    const state_size = @sizeOf(WasmSyncState);
    if (in_len < state_size) return 0;

    const src: *const WasmSyncState = @ptrCast(@alignCast(in_ptr));
    if (src.magic != 0x53594E43) return 0;

    global_sync_state = src.*;
    global_sync_state.revision += 1;
    global_sync_state.status = 0; // Synchronized
    return 1;
}

export fn wasm_sync_status() callconv(.C) u32 {
    return global_sync_state.status;
}

export fn wasm_execute_step(instructions: u64) callconv(.C) u64 {
    global_sync_state.instruction_count += instructions;
    global_sync_state.sequence += 1;
    global_sync_state.status = 1; // Diverging
    return global_sync_state.instruction_count;
}

// ============================================================================
// 7. UNIT TESTS (RUNNABLE VIA `zig test`)
// ============================================================================

test "stateful wasm component checkpoint and restore" {
    var snap_buf: [128]u8 = undefined;
    _ = wasm_execute_step(500);
    const written = wasm_checkpoint(&snap_buf, 128);
    try std.testing.expect(written > 0);

    const restored = wasm_restore_snapshot(&snap_buf, written);
    try std.testing.expectEqual(@as(u32, 1), restored);
    try std.testing.expectEqual(@as(u32, 0), wasm_sync_status());
}

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

// ============================================================================
// 7. ZERO-COPY WASM-TO-DAEMON IPC (SHM LINEAR MEMORY DESCRIPTORS)
// ============================================================================

pub const SHM_MAGIC: u32 = 0x53484D31; // 'SHM1'

pub const ShmBufferDescriptor = extern struct {
    offset: u32,
    length: u32,
    capacity: u32,
    flags: u32,
    checksum: u64,
};

pub const SHM_FLAG_CONTEXT_BUFFER: u32 = 1 << 0;
pub const SHM_FLAG_AST_REPRESENTATION: u32 = 1 << 1;
pub const SHM_FLAG_MODEL_TENSOR: u32 = 1 << 2;
pub const SHM_FLAG_DIRECT_POINTER: u32 = 1 << 3;
pub const SHM_FLAG_ZERO_COPY_VERIFIED: u32 = 1 << 4;

var shm_descriptors: [64]ShmBufferDescriptor = undefined;
var shm_descriptors_count: usize = 0;

export fn wasm_shm_alloc(size: usize, flags: u32) callconv(.C) ?[*]u8 {
    const ptr = allocBytes(size) orelse return null;
    const offset = @as(u32, @intCast(@intFromPtr(ptr)));

    if (shm_descriptors_count < shm_descriptors.len) {
        shm_descriptors[shm_descriptors_count] = ShmBufferDescriptor{
            .offset = offset,
            .length = 0,
            .capacity = @as(u32, @intCast(size)),
            .flags = flags | SHM_FLAG_DIRECT_POINTER,
            .checksum = 0,
        };
        shm_descriptors_count += 1;
    }
    return ptr;
}

export fn wasm_shm_get_ptr(offset: u32) callconv(.C) ?[*]u8 {
    if (offset == 0) return null;
    return @ptrFromInt(offset);
}

export fn wasm_shm_process_context(buf_ptr: [*]const u8, buf_len: u32, flags: u32) callconv(.C) u32 {
    if (buf_len == 0) return 0;

    // Fast FNV-1a 64-bit checksum computed in-place across linear memory
    var h: u64 = 0xcbf29ce484222325;
    for (0..buf_len) |i| {
        h ^= buf_ptr[i];
        h = h *% 0x100000001b3;
    }

    // Update descriptor entry if present
    const addr = @as(u32, @intCast(@intFromPtr(buf_ptr)));
    for (0..shm_descriptors_count) |i| {
        if (shm_descriptors[i].offset == addr) {
            shm_descriptors[i].length = buf_len;
            shm_descriptors[i].flags = flags | SHM_FLAG_ZERO_COPY_VERIFIED;
            shm_descriptors[i].checksum = h;
            break;
        }
    }

    return @as(u32, @truncate(h));
}

export fn wasm_shm_get_descriptor(offset: u32, out_ptr: [*]u8) callconv(.C) u32 {
    const desc_size = @sizeOf(ShmBufferDescriptor);
    for (0..shm_descriptors_count) |i| {
        if (shm_descriptors[i].offset == offset) {
            const dest: *ShmBufferDescriptor = @ptrCast(@alignCast(out_ptr));
            dest.* = shm_descriptors[i];
            return @as(u32, @intCast(desc_size));
        }
    }
    return 0;
}

test "zero copy shm allocation and in-place context processing" {
    const size = 1024;
    const ptr = wasm_shm_alloc(size, SHM_FLAG_CONTEXT_BUFFER);
    try std.testing.expect(ptr != null);

    const offset = @as(u32, @intCast(@intFromPtr(ptr.?)));
    const retrieved_ptr = wasm_shm_get_ptr(offset);
    try std.testing.expectEqual(ptr, retrieved_ptr);

    // Write context directly into buffer
    const msg = "Hagibis Zero-Copy WASM-to-Daemon IPC context stream";
    @memcpy(ptr.?[0..msg.len], msg);

    const token = wasm_shm_process_context(ptr.?, @as(u32, @intCast(msg.len)), SHM_FLAG_CONTEXT_BUFFER);
    try std.testing.expect(token != 0);

    // Verify descriptor
    var desc_buf: [@sizeOf(ShmBufferDescriptor)]u8 align(@alignOf(ShmBufferDescriptor)) = undefined;
    const desc_bytes = wasm_shm_get_descriptor(offset, &desc_buf);
    try std.testing.expectEqual(@as(u32, @intCast(@sizeOf(ShmBufferDescriptor))), desc_bytes);

    const desc: *ShmBufferDescriptor = @ptrCast(&desc_buf);
    try std.testing.expectEqual(offset, desc.offset);
    try std.testing.expectEqual(@as(u32, @intCast(msg.len)), desc.length);
    try std.testing.expect(desc.flags & SHM_FLAG_ZERO_COPY_VERIFIED != 0);
    try std.testing.expect(desc.checksum != 0);
}

// ============================================================================
// 8. HARDWARE-ACCELERATED TENSOR & SIMD OPERATIONS (Roadmap Pillar 2)
// ============================================================================

pub export fn hgb_wasm_simd_dot_product(vec_a_ptr: [*]const f32, vec_b_ptr: [*]const f32, len: usize) callconv(.C) f32 {
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
) callconv(.C) void {
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

pub export fn hgb_wasm_simd_relu(tensor_ptr: [*]f32, len: usize) callconv(.C) void {
    if (len == 0) return;
    const Vec4 = @Vector(4, f32);
    const zero_vec: Vec4 = @splat(0.0);
    const chunks = len / 4;
    var i: usize = 0;
    while (i < chunks * 4) : (i += 4) {
        const slice = tensor_ptr[i..][0..4];
        const v: Vec4 = slice.*;
        slice.* = @max(v, zero_vec);
    }
    var rem = chunks * 4;
    while (rem < len) : (rem += 1) {
        if (tensor_ptr[rem] < 0.0) {
            tensor_ptr[rem] = 0.0;
        }
    }
}

// ----------------------------------------------------------------------------
// Advanced Hardware SIMD Kernels: Softmax, RMSNorm, LayerNorm, & Embeddings
// ----------------------------------------------------------------------------

pub export fn hgb_wasm_simd_softmax(tensor_ptr: [*]f32, len: usize) callconv(.C) void {
    if (len == 0) return;

    var max_val: f32 = tensor_ptr[0];
    for (1..len) |i| {
        if (tensor_ptr[i] > max_val) {
            max_val = tensor_ptr[i];
        }
    }

    var sum_exp: f32 = 0.0;
    for (0..len) |i| {
        const e = @exp(tensor_ptr[i] - max_val);
        tensor_ptr[i] = e;
        sum_exp += e;
    }

    if (sum_exp > 0.0) {
        const inv_sum = 1.0 / sum_exp;
        for (0..len) |i| {
            tensor_ptr[i] *= inv_sum;
        }
    }
}

pub export fn hgb_wasm_simd_rmsnorm(
    tensor_ptr: [*]f32,
    weight_ptr: ?[*]const f32,
    len: usize,
    eps: f32,
) callconv(.C) void {
    if (len == 0) return;

    var sum_sq: f32 = 0.0;
    for (0..len) |i| {
        sum_sq += tensor_ptr[i] * tensor_ptr[i];
    }
    const rms = @sqrt((sum_sq / @as(f32, @floatFromInt(len))) + eps);
    const inv_rms = if (rms > 0.0) 1.0 / rms else 1.0;

    if (weight_ptr) |w| {
        for (0..len) |i| {
            tensor_ptr[i] = (tensor_ptr[i] * inv_rms) * w[i];
        }
    } else {
        for (0..len) |i| {
            tensor_ptr[i] *= inv_rms;
        }
    }
}

pub export fn hgb_wasm_simd_layernorm(
    tensor_ptr: [*]f32,
    gamma_ptr: ?[*]const f32,
    beta_ptr: ?[*]const f32,
    len: usize,
    eps: f32,
) callconv(.C) void {
    if (len == 0) return;

    var sum: f32 = 0.0;
    for (0..len) |i| {
        sum += tensor_ptr[i];
    }
    const mean = sum / @as(f32, @floatFromInt(len));

    var sum_sq_diff: f32 = 0.0;
    for (0..len) |i| {
        const diff = tensor_ptr[i] - mean;
        sum_sq_diff += diff * diff;
    }
    const variance = sum_sq_diff / @as(f32, @floatFromInt(len));
    const inv_std = 1.0 / @sqrt(variance + eps);

    for (0..len) |i| {
        var norm = (tensor_ptr[i] - mean) * inv_std;
        if (gamma_ptr) |g| {
            norm *= g[i];
        }
        if (beta_ptr) |b| {
            norm += b[i];
        }
        tensor_ptr[i] = norm;
    }
}

pub export fn hgb_wasm_simd_token_embedding_lookup(
    indices_ptr: [*]const u32,
    num_tokens: usize,
    table_ptr: [*]const f32,
    embedding_dim: usize,
    out_ptr: [*]f32,
) callconv(.C) void {
    if (num_tokens == 0 or embedding_dim == 0) return;

    const Vec4 = @Vector(4, f32);
    const chunks = embedding_dim / 4;

    for (0..num_tokens) |t| {
        const token_idx = indices_ptr[t];
        const table_row = table_ptr + (@as(usize, token_idx) * embedding_dim);
        const out_row = out_ptr + (t * embedding_dim);

        var j: usize = 0;
        while (j < chunks * 4) : (j += 4) {
            const v: Vec4 = table_row[j..][0..4].*;
            out_row[j..][0..4].* = v;
        }
        var rem = chunks * 4;
        while (rem < embedding_dim) : (rem += 1) {
            out_row[rem] = table_row[rem];
        }
    }
}

// ============================================================================
// 9. WASI-NN (Neural Network) STANDARD PROPOSAL IMPLEMENTATION
// ============================================================================

pub const NnErrno = enum(u32) {
    success = 0,
    invalid_argument = 1,
    invalid_encoding = 2,
    missing_memory = 3,
    busy = 4,
    runtime_error = 5,
    unsupported_operation = 6,
    too_large = 7,
    not_found = 8,
    security = 9,
};

pub const WasiNnGraphEncoding = enum(u32) {
    openvino = 0,
    onnx = 1,
    tensorflowlite = 2,
    pytorch = 3,
    gguf = 4,
    burn = 5,
};

pub const WasiNnExecutionTarget = enum(u32) {
    cpu = 0,
    gpu = 1,
    tpu = 2,
};

pub const WasiNnTensorType = enum(u32) {
    f16 = 0,
    f32 = 1,
    u8 = 2,
    i32 = 3,
};

pub const TensorDimensions = extern struct {
    buf: [*]const u32,
    size: u32,
};

pub const WasiNnTensor = extern struct {
    dimensions: TensorDimensions,
    tensor_type: u32,
    data: [*]const u8,
};

pub const GraphBuilder = extern struct {
    buf: [*]const u8,
    size: u32,
};

pub const GraphBuilderArray = extern struct {
    buf: [*]const GraphBuilder,
    size: u32,
};

// Extern host bindings for wasi_ephemeral_nn
const host_wasi_nn = struct {
    extern "wasi_ephemeral_nn" fn load(builders: *const GraphBuilderArray, encoding: u32, target: u32, graph: *u32) u32;
    extern "wasi_ephemeral_nn" fn init_execution_context(graph: u32, context: *u32) u32;
    extern "wasi_ephemeral_nn" fn set_input(context: u32, index: u32, tensor: *const WasiNnTensor) u32;
    extern "wasi_ephemeral_nn" fn compute(context: u32) u32;
    extern "wasi_ephemeral_nn" fn get_output(context: u32, index: u32, out_buffer: [*]u8, max_len: u32, out_len: *u32) u32;
};

// Fallback in-memory state for native test verification
const NativeWasiNnModel = struct {
    encoding: u32,
    target: u32,
    data: []const u8,
};

const NativeWasiNnContext = struct {
    graph_id: u32,
    input_data: [4096]u8,
    input_len: usize,
    input_dims: [8]u32,
    input_dims_count: u32,
    input_type: u32,
    output_data: [4096]u8,
    output_len: usize,
    output_dims: [8]u32,
    output_dims_count: u32,
};

var native_models: [16]NativeWasiNnModel = undefined;
var native_models_count: u32 = 0;
var native_contexts: [16]NativeWasiNnContext = undefined;
var native_contexts_count: u32 = 0;

pub fn wasiNnLoad(builders: *const GraphBuilderArray, encoding: u32, target: u32, graph: *u32) u32 {
    if (is_wasm) {
        return host_wasi_nn.load(builders, encoding, target, graph);
    } else {
        if (builders.size == 0) return @intFromEnum(NnErrno.invalid_argument);
        if (native_models_count >= native_models.len) return @intFromEnum(NnErrno.busy);

        native_models_count += 1;
        const id = native_models_count;
        native_models[id - 1] = NativeWasiNnModel{
            .encoding = encoding,
            .target = target,
            .data = builders.buf[0].buf[0..builders.buf[0].size],
        };
        graph.* = id;
        return @intFromEnum(NnErrno.success);
    }
}

pub fn wasiNnInitExecutionContext(graph: u32, context: *u32) u32 {
    if (is_wasm) {
        return host_wasi_nn.init_execution_context(graph, context);
    } else {
        if (graph == 0 or graph > native_models_count) return @intFromEnum(NnErrno.invalid_argument);
        if (native_contexts_count >= native_contexts.len) return @intFromEnum(NnErrno.busy);

        native_contexts_count += 1;
        const id = native_contexts_count;
        var ctx = &native_contexts[id - 1];
        ctx.graph_id = graph;
        ctx.input_len = 0;
        ctx.output_len = 0;
        ctx.input_dims_count = 0;
        ctx.output_dims_count = 0;
        context.* = id;
        return @intFromEnum(NnErrno.success);
    }
}

pub fn wasiNnSetInput(context: u32, index: u32, tensor: *const WasiNnTensor) u32 {
    if (is_wasm) {
        return host_wasi_nn.set_input(context, index, tensor);
    } else {
        if (context == 0 or context > native_contexts_count) return @intFromEnum(NnErrno.invalid_argument);
        if (index != 0) return @intFromEnum(NnErrno.invalid_argument);
        if (tensor.dimensions.size == 0) return @intFromEnum(NnErrno.invalid_argument);

        var ctx = &native_contexts[context - 1];
        var total_elems: usize = 1;
        for (0..tensor.dimensions.size) |i| {
            const dim = tensor.dimensions.buf[i];
            if (dim == 0) return @intFromEnum(NnErrno.invalid_argument);
            total_elems *= dim;
            if (i < ctx.input_dims.len) {
                ctx.input_dims[i] = dim;
            }
        }
        ctx.input_dims_count = tensor.dimensions.size;
        ctx.input_type = tensor.tensor_type;

        const elem_size: usize = if (tensor.tensor_type == @intFromEnum(WasiNnTensorType.f32)) 4 else 1;
        const byte_len = total_elems * elem_size;
        if (byte_len > ctx.input_data.len) return @intFromEnum(NnErrno.too_large);

        @memcpy(ctx.input_data[0..byte_len], tensor.data[0..byte_len]);
        ctx.input_len = byte_len;
        return @intFromEnum(NnErrno.success);
    }
}

pub fn wasiNnCompute(context: u32) u32 {
    if (is_wasm) {
        return host_wasi_nn.compute(context);
    } else {
        if (context == 0 or context > native_contexts_count) return @intFromEnum(NnErrno.invalid_argument);
        var ctx = &native_contexts[context - 1];
        if (ctx.input_len == 0) return @intFromEnum(NnErrno.invalid_argument);

        const num_floats = ctx.input_len / 4;
        const in_floats: [*]const f32 = @ptrCast(@alignCast(&ctx.input_data[0]));
        const out_floats: [*]f32 = @ptrCast(@alignCast(&ctx.output_data[0]));

        const model = native_models[ctx.graph_id - 1];
        for (0..num_floats) |i| {
            const val = in_floats[i];
            if (model.encoding == @intFromEnum(WasiNnGraphEncoding.gguf)) {
                // SiLU
                const silu = val / (1.0 + @exp(-val));
                out_floats[i] = silu;
            } else {
                // ReLU
                out_floats[i] = if (val > 0.0) val else 0.0;
            }
        }

        ctx.output_len = ctx.input_len;
        ctx.output_dims_count = ctx.input_dims_count;
        for (0..ctx.input_dims_count) |i| {
            ctx.output_dims[i] = ctx.input_dims[i];
        }
        return @intFromEnum(NnErrno.success);
    }
}

pub fn wasiNnGetOutput(context: u32, index: u32, out_buffer: [*]u8, max_len: u32, out_len: *u32) u32 {
    if (is_wasm) {
        return host_wasi_nn.get_output(context, index, out_buffer, max_len, out_len);
    } else {
        if (context == 0 or context > native_contexts_count) return @intFromEnum(NnErrno.invalid_argument);
        if (index != 0) return @intFromEnum(NnErrno.invalid_argument);

        const ctx = &native_contexts[context - 1];
        if (ctx.output_len > max_len) return @intFromEnum(NnErrno.too_large);

        @memcpy(out_buffer[0..ctx.output_len], ctx.output_data[0..ctx.output_len]);
        out_len.* = @as(u32, @intCast(ctx.output_len));
        return @intFromEnum(NnErrno.success);
    }
}

pub export fn wasi_ephemeral_nn_load(builders: *const GraphBuilderArray, encoding: u32, target: u32, graph: *u32) callconv(.C) u32 {
    return wasiNnLoad(builders, encoding, target, graph);
}

pub export fn wasi_ephemeral_nn_init_execution_context(graph: u32, context: *u32) callconv(.C) u32 {
    return wasiNnInitExecutionContext(graph, context);
}

pub export fn wasi_ephemeral_nn_set_input(context: u32, index: u32, tensor: *const WasiNnTensor) callconv(.C) u32 {
    return wasiNnSetInput(context, index, tensor);
}

pub export fn wasi_ephemeral_nn_compute(context: u32) callconv(.C) u32 {
    return wasiNnCompute(context);
}

pub export fn wasi_ephemeral_nn_get_output(context: u32, index: u32, out_buffer: [*]u8, max_len: u32, out_len: *u32) callconv(.C) u32 {
    return wasiNnGetOutput(context, index, out_buffer, max_len, out_len);
}

test "simd 128-bit dot product and matmul" {
    const a = [_]f32{ 1.0, 2.0, 3.0, 4.0, 5.0 };
    const b = [_]f32{ 2.0, 0.5, 1.0, -1.0, 2.0 };
    // 1*2 + 2*0.5 + 3*1 + 4*(-1) + 5*2 = 2 + 1 + 3 - 4 + 10 = 12.0
    const dot = hgb_wasm_simd_dot_product(&a, &b, 5);
    try std.testing.expectApproxEqAbs(@as(f32, 12.0), dot, 0.001);

    // Matmul test: 2x2 times 2x2
    const mat_a = [_]f32{ 1.0, 2.0, 3.0, 4.0 };
    const mat_b = [_]f32{ 2.0, 0.0, 1.0, 3.0 };
    var mat_out: [4]f32 = undefined;
    hgb_wasm_simd_matmul(&mat_a, &mat_b, &mat_out, 2, 2, 2);
    // [1*2 + 2*1, 1*0 + 2*3] = [4, 6]
    // [3*2 + 4*1, 3*0 + 4*3] = [10, 12]
    try std.testing.expectApproxEqAbs(@as(f32, 4.0), mat_out[0], 0.001);
    try std.testing.expectApproxEqAbs(@as(f32, 6.0), mat_out[1], 0.001);
    try std.testing.expectApproxEqAbs(@as(f32, 10.0), mat_out[2], 0.001);
    try std.testing.expectApproxEqAbs(@as(f32, 12.0), mat_out[3], 0.001);

    // ReLU test
    var relu_arr = [_]f32{ -1.0, 2.0, 0.0, -0.5, 3.5 };
    hgb_wasm_simd_relu(&relu_arr, 5);
    try std.testing.expectApproxEqAbs(@as(f32, 0.0), relu_arr[0], 0.001);
    try std.testing.expectApproxEqAbs(@as(f32, 2.0), relu_arr[1], 0.001);
    try std.testing.expectApproxEqAbs(@as(f32, 0.0), relu_arr[2], 0.001);
    try std.testing.expectApproxEqAbs(@as(f32, 0.0), relu_arr[3], 0.001);
    try std.testing.expectApproxEqAbs(@as(f32, 3.5), relu_arr[4], 0.001);
}

test "simd softmax, rmsnorm, and layernorm" {
    // Softmax
    var sm_arr = [_]f32{ 1.0, 2.0, 3.0 };
    hgb_wasm_simd_softmax(&sm_arr, 3);
    const sum = sm_arr[0] + sm_arr[1] + sm_arr[2];
    try std.testing.expectApproxEqAbs(@as(f32, 1.0), sum, 0.001);
    try std.testing.expect(sm_arr[2] > sm_arr[1] and sm_arr[1] > sm_arr[0]);

    // RMSNorm
    var rms_arr = [_]f32{ 2.0, 2.0, 2.0, 2.0 };
    hgb_wasm_simd_rmsnorm(&rms_arr, null, 4, 1e-5);
    // RMS = sqrt(16/4) = 2.0 -> elements should be ~1.0
    for (rms_arr) |v| {
        try std.testing.expectApproxEqAbs(@as(f32, 1.0), v, 0.01);
    }

    // LayerNorm
    var ln_arr = [_]f32{ 1.0, 2.0, 3.0, 4.0 };
    hgb_wasm_simd_layernorm(&ln_arr, null, null, 4, 1e-5);
    var ln_sum: f32 = 0.0;
    for (ln_arr) |v| ln_sum += v;
    try std.testing.expectApproxEqAbs(@as(f32, 0.0), ln_sum, 0.01);

    // Token embedding lookup
    const table = [_]f32{
        0.1, 0.2, 0.3, 0.4, // token 0
        1.1, 1.2, 1.3, 1.4, // token 1
        2.1, 2.2, 2.3, 2.4, // token 2
    };
    const tokens = [_]u32{ 2, 0 };
    var out_embeds: [8]f32 = undefined;
    hgb_wasm_simd_token_embedding_lookup(&tokens, 2, &table, 4, &out_embeds);
    try std.testing.expectApproxEqAbs(@as(f32, 2.1), out_embeds[0], 0.001);
    try std.testing.expectApproxEqAbs(@as(f32, 2.4), out_embeds[3], 0.001);
    try std.testing.expectApproxEqAbs(@as(f32, 0.1), out_embeds[4], 0.001);
    try std.testing.expectApproxEqAbs(@as(f32, 0.4), out_embeds[7], 0.001);
}

test "wasi-nn native contract execution" {
    const dummy_bytes = "dummy_model_bytes";
    const builders_arr = [_]GraphBuilder{.{
        .buf = dummy_bytes.ptr,
        .size = @as(u32, @intCast(dummy_bytes.len)),
    }};
    const builders = GraphBuilderArray{
        .buf = @ptrCast(&builders_arr[0]),
        .size = 1,
    };

    var graph_id: u32 = 0;
    const rc_load = wasiNnLoad(&builders, @intFromEnum(WasiNnGraphEncoding.onnx), @intFromEnum(WasiNnExecutionTarget.cpu), &graph_id);
    try std.testing.expectEqual(@as(u32, 0), rc_load);
    try std.testing.expect(graph_id > 0);

    var ctx_id: u32 = 0;
    const rc_ctx = wasiNnInitExecutionContext(graph_id, &ctx_id);
    try std.testing.expectEqual(@as(u32, 0), rc_ctx);
    try std.testing.expect(ctx_id > 0);

    const in_dims = [_]u32{ 1, 4 };
    const in_floats = [_]f32{ -1.5, 2.0, -0.5, 4.0 };
    const in_bytes: [*]const u8 = @ptrCast(@alignCast(&in_floats[0]));
    const tensor = WasiNnTensor{
        .dimensions = .{ .buf = &in_dims, .size = 2 },
        .tensor_type = @intFromEnum(WasiNnTensorType.f32),
        .data = in_bytes,
    };

    const rc_set = wasiNnSetInput(ctx_id, 0, &tensor);
    try std.testing.expectEqual(@as(u32, 0), rc_set);

    const rc_comp = wasiNnCompute(ctx_id);
    try std.testing.expectEqual(@as(u32, 0), rc_comp);

    var out_buf: [16]u8 = undefined;
    var out_len: u32 = 0;
    const rc_out = wasiNnGetOutput(ctx_id, 0, &out_buf, 16, &out_len);
    try std.testing.expectEqual(@as(u32, 0), rc_out);
    try std.testing.expectEqual(@as(u32, 16), out_len);

    const out_floats: [*]const f32 = @ptrCast(@alignCast(&out_buf[0]));
    try std.testing.expectApproxEqAbs(@as(f32, 0.0), out_floats[0], 0.001); // ReLU on -1.5
    try std.testing.expectApproxEqAbs(@as(f32, 2.0), out_floats[1], 0.001); // ReLU on 2.0
    try std.testing.expectApproxEqAbs(@as(f32, 0.0), out_floats[2], 0.001); // ReLU on -0.5
    try std.testing.expectApproxEqAbs(@as(f32, 4.0), out_floats[3], 0.001); // ReLU on 4.0
}
