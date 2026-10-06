const std = @import("std");

/// Find substring in memory
fn mem_find(haystack: []const u8, needle: []const u8) ?usize {
    if (needle.len == 0) return 0;
    if (haystack.len < needle.len) return null;
    var i: usize = 0;
    while (i <= haystack.len - needle.len) : (i += 1) {
        if (std.mem.eql(u8, haystack[i .. i + needle.len], needle)) {
            return i;
        }
    }
    return null;
}

/// A very primitive zero-copy JSON scanner for MCP
export fn hgb_zig_mcp_parse_request(
    json_ptr: [*]const u8,
    json_len: usize,
    out_id: *i64,
    out_has_id: *bool,
    out_method_ptr: *[*]const u8,
    out_method_len: *usize,
    out_params_ptr: *[*]const u8,
    out_params_len: *usize,
) bool {
    const json = json_ptr[0..json_len];
    if (mem_find(json, "\"jsonrpc\"") == null) return false;
    
    // Default values
    out_has_id.* = false;
    out_id.* = 0;
    out_method_len.* = 0;
    out_params_len.* = 0;
    
    // Extract ID (primitive scan: looking for "id": [numeric/null])
    if (mem_find(json, "\"id\"")) |id_idx| {
        var start = id_idx + 4;
        while (start < json.len and (json[start] == ':' or json[start] == ' ' or json[start] == '\t' or json[start] == '\n' or json[start] == '\r')) {
            start += 1;
        }
        var end = start;
        while (end < json.len and json[end] != ',' and json[end] != '}') {
            end += 1;
        }
        // Extract numeric ID
        const id_str = std.mem.trim(u8, json[start..end], " \t\r\n\"");
        if (std.fmt.parseInt(i64, id_str, 10)) |id| {
            out_id.* = id;
            out_has_id.* = true;
        } else |_| {}
    }

    // Extract Method
    if (mem_find(json, "\"method\"")) |m_idx| {
        var start = m_idx + 8;
        while (start < json.len and (json[start] == ':' or json[start] == ' ' or json[start] == '\t' or json[start] == '\n' or json[start] == '\r' or json[start] == '"')) {
            start += 1;
        }
        var end = start;
        while (end < json.len and json[end] != '"') {
            end += 1;
        }
        if (end > start) {
            out_method_ptr.* = json[start..].ptr;
            out_method_len.* = end - start;
        }
    } else {
        return false; // method is required
    }

    // Extract Params (can be object or array, primitive scan)
    if (mem_find(json, "\"params\"")) |p_idx| {
        var start = p_idx + 8;
        while (start < json.len and (json[start] == ':' or json[start] == ' ' or json[start] == '\t' or json[start] == '\n' or json[start] == '\r')) {
            start += 1;
        }
        // We find matching brace/bracket
        if (start < json.len and (json[start] == '{' or json[start] == '[')) {
            const open_char = json[start];
            const close_char: u8 = if (open_char == '{') '}' else ']';
            var depth: usize = 0;
            var end = start;
            var in_string = false;
            var escaped = false;
            while (end < json.len) : (end += 1) {
                const c = json[end];
                if (in_string) {
                    if (escaped) {
                        escaped = false;
                    } else if (c == '\\') {
                        escaped = true;
                    } else if (c == '"') {
                        in_string = false;
                    }
                } else {
                    if (c == '"') {
                        in_string = true;
                    } else if (c == open_char) {
                        depth += 1;
                    } else if (c == close_char) {
                        depth -= 1;
                        if (depth == 0) {
                            end += 1;
                            break;
                        }
                    }
                }
            }
            if (end > start) {
                out_params_ptr.* = json[start..].ptr;
                out_params_len.* = end - start;
            }
        }
    }
    return true;
}

export fn hgb_zig_mcp_extract_tool_call(
    params_ptr: [*]const u8,
    params_len: usize,
    out_name_ptr: *[*]const u8,
    out_name_len: *usize,
    out_args_ptr: *[*]const u8,
    out_args_len: *usize,
) bool {
    const params = params_ptr[0..params_len];
    out_name_len.* = 0;
    out_args_len.* = 0;

    // Extract Name
    if (mem_find(params, "\"name\"")) |n_idx| {
        var start = n_idx + 6;
        while (start < params.len and (params[start] == ':' or params[start] == ' ' or params[start] == '\t' or params[start] == '\n' or params[start] == '\r' or params[start] == '"')) {
            start += 1;
        }
        var end = start;
        while (end < params.len and params[end] != '"') {
            end += 1;
        }
        if (end > start) {
            out_name_ptr.* = params[start..].ptr;
            out_name_len.* = end - start;
        }
    } else {
        return false;
    }

    // Extract Arguments
    if (mem_find(params, "\"arguments\"")) |a_idx| {
        var start = a_idx + 11;
        while (start < params.len and (params[start] == ':' or params[start] == ' ' or params[start] == '\t' or params[start] == '\n' or params[start] == '\r')) {
            start += 1;
        }
        if (start < params.len and params[start] == '{') {
            var depth: usize = 0;
            var end = start;
            var in_string = false;
            var escaped = false;
            while (end < params.len) : (end += 1) {
                const c = params[end];
                if (in_string) {
                    if (escaped) {
                        escaped = false;
                    } else if (c == '\\') {
                        escaped = true;
                    } else if (c == '"') {
                        in_string = false;
                    }
                } else {
                    if (c == '"') {
                        in_string = true;
                    } else if (c == '{') {
                        depth += 1;
                    } else if (c == '}') {
                        depth -= 1;
                        if (depth == 0) {
                            end += 1;
                            break;
                        }
                    }
                }
            }
            if (end > start) {
                out_args_ptr.* = params[start..].ptr;
                out_args_len.* = end - start;
            }
        }
    }
    return true;
}

export fn hgb_zig_mcp_format_initialize_response(
    id: i64,
    has_id: bool,
    server_name_ptr: [*]const u8,
    server_name_len: usize,
    version_ptr: [*]const u8,
    version_len: usize,
    out_buf_ptr: [*]u8,
    out_cap: usize,
    out_len: *usize,
) bool {
    const server_name = server_name_ptr[0..server_name_len];
    const version = version_ptr[0..version_len];
    const out_buf = out_buf_ptr[0..out_cap];
    
    var stream = std.io.fixedBufferStream(out_buf);
    const writer = stream.writer();
    
    if (has_id) {
        writer.print("{{\"jsonrpc\":\"2.0\",\"id\":{},\"result\":{{\"protocolVersion\":\"2024-11-05\",\"capabilities\":{{\"tools\":{{\"listChanged\":false}}}},\"serverInfo\":{{\"name\":\"{s}\",\"version\":\"{s}\"}}}}}}", .{ id, server_name, version }) catch return false;
    } else {
        writer.print("{{\"jsonrpc\":\"2.0\",\"id\":null,\"result\":{{\"protocolVersion\":\"2024-11-05\",\"capabilities\":{{\"tools\":{{\"listChanged\":false}}}},\"serverInfo\":{{\"name\":\"{s}\",\"version\":\"{s}\"}}}}}}", .{ server_name, version }) catch return false;
    }
    
    out_len.* = stream.getPos() catch return false;
    return true;
}

fn json_escape(writer: anytype, str: []const u8) !void {
    for (str) |c| {
        switch (c) {
            '"' => try writer.writeAll("\\\""),
            '\\' => try writer.writeAll("\\\\"),
            '\n' => try writer.writeAll("\\n"),
            '\r' => try writer.writeAll("\\r"),
            '\t' => try writer.writeAll("\\t"),
            0x08 => try writer.writeAll("\\b"),
            0x0C => try writer.writeAll("\\f"),
            else => {
                if (c < 0x20) {
                    try writer.print("\\u00{x:0>2}", .{c});
                } else {
                    try writer.writeByte(c);
                }
            }
        }
    }
}

export fn hgb_zig_mcp_format_tool_result(
    id: i64,
    has_id: bool,
    content_ptr: [*]const u8,
    content_len: usize,
    is_error: bool,
    out_buf_ptr: [*]u8,
    out_cap: usize,
    out_len: *usize,
) bool {
    const content = content_ptr[0..content_len];
    const out_buf = out_buf_ptr[0..out_cap];
    var stream = std.io.fixedBufferStream(out_buf);
    const writer = stream.writer();
    
    if (has_id) {
        writer.print("{{\"jsonrpc\":\"2.0\",\"id\":{},\"result\":{{\"isError\":{},\"content\":[{{\"type\":\"text\",\"text\":\"", .{ id, is_error }) catch return false;
    } else {
        writer.print("{{\"jsonrpc\":\"2.0\",\"id\":null,\"result\":{{\"isError\":{},\"content\":[{{\"type\":\"text\",\"text\":\"", .{ is_error }) catch return false;
    }
    
    json_escape(writer, content) catch return false;
    
    writer.print("\"}}]}}}}", .{}) catch return false;
    
    out_len.* = stream.getPos() catch return false;
    return true;
}

export fn hgb_zig_mcp_arena_create(capacity: usize) ?*anyopaque {
    const allocator = std.heap.page_allocator;
    const buf = allocator.alloc(u8, capacity) catch return null;
    const fba = allocator.create(std.heap.FixedBufferAllocator) catch {
        allocator.free(buf);
        return null;
    };
    fba.* = std.heap.FixedBufferAllocator.init(buf);
    return @ptrCast(fba);
}

export fn hgb_zig_mcp_arena_alloc(arena_opaque: *anyopaque, size: usize) ?[*]u8 {
    const fba: *std.heap.FixedBufferAllocator = @ptrCast(@alignCast(arena_opaque));
    const allocator = fba.allocator();
    const slice = allocator.alloc(u8, size) catch return null;
    return slice.ptr;
}

export fn hgb_zig_mcp_arena_reset(arena_opaque: *anyopaque) void {
    const fba: *std.heap.FixedBufferAllocator = @ptrCast(@alignCast(arena_opaque));
    fba.reset();
}

export fn hgb_zig_mcp_arena_destroy(arena_opaque: *anyopaque) void {
    const fba: *std.heap.FixedBufferAllocator = @ptrCast(@alignCast(arena_opaque));
    const buf = fba.buffer;
    const allocator = std.heap.page_allocator;
    allocator.destroy(fba);
    allocator.free(buf);
}
