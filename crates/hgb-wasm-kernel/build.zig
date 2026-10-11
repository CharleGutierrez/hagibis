const std = @import("std");

pub fn build(b: *std.Build) void {
    // 1. WebAssembly Freestanding Target
    const wasm_target = b.resolveTargetQuery(.{
        .cpu_arch = .wasm32,
        .os_tag = .freestanding,
        .abi = .none,
    });

    const optimize = b.standardOptimizeOption(.{
        .preferred_optimize_mode = .ReleaseSmall,
    });

    // 2. Build Wasm Library / Executable without entry point
    const wasm_lib = b.addExecutable(.{
        .name = "hgb_kernel",
        .root_source_file = b.path("src/main.zig"),
        .target = wasm_target,
        .optimize = optimize,
    });

    wasm_lib.entry = .disabled;
    wasm_lib.rdynamic = true;

    // Export memory and functions
    b.installArtifact(wasm_lib);

    // 3. Unit Tests (targeting host for `zig test`)
    const host_target = b.standardTargetOptions(.{});
    const unit_tests = b.addTest(.{
        .root_source_file = b.path("src/main.zig"),
        .target = host_target,
        .optimize = .Debug,
    });

    const run_unit_tests = b.addRunArtifact(unit_tests);
    const test_step = b.step("test", "Run unit tests across memory, Myers diff, fuzzy search, and Git DAG");
    test_step.dependOn(&run_unit_tests.step);
}
