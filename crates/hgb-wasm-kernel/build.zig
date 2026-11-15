const std = @import("std");

pub fn build(b: *std.Build) void {
    // 1. WebAssembly Component Model Target (wasm32-wasip2 / WASI Preview2)
    // with SIMD128, Relaxed SIMD, Bulk Memory, Mutable Globals & Sign Extension
    var cpu_features_add = std.Target.Cpu.Feature.Set.empty;
    cpu_features_add.addFeature(@intFromEnum(std.Target.wasm.Feature.simd128));
    cpu_features_add.addFeature(@intFromEnum(std.Target.wasm.Feature.relaxed_simd));
    cpu_features_add.addFeature(@intFromEnum(std.Target.wasm.Feature.bulk_memory));
    cpu_features_add.addFeature(@intFromEnum(std.Target.wasm.Feature.mutable_globals));
    cpu_features_add.addFeature(@intFromEnum(std.Target.wasm.Feature.nontrapping_fptoint));
    cpu_features_add.addFeature(@intFromEnum(std.Target.wasm.Feature.sign_ext));

    // Standardize to wasm32-wasi (WASI Preview2 / Component Model foundation)
    const wasm_target = b.resolveTargetQuery(.{
        .cpu_arch = .wasm32,
        .os_tag = .wasi,
        .abi = .musl,
        .cpu_features_add = cpu_features_add,
    });

    const optimize = b.standardOptimizeOption(.{
        .preferred_optimize_mode = .ReleaseSmall,
    });

    // 2. Build Wasm Library / Executable without entry point, exporting WIT interfaces
    const wasm_lib = b.addExecutable(.{
        .name = "hgb_kernel",
        .root_source_file = b.path("src/main.zig"),
        .target = wasm_target,
        .optimize = optimize,
    });

    wasm_lib.entry = .disabled;
    wasm_lib.rdynamic = true;
    wasm_lib.import_symbols = true; // Retain undefined extern symbols for host WASI-NN and syscall imports
    wasm_lib.export_memory = true; // Expose WebAssembly linear memory for host tensor zero-copy access
    wasm_lib.max_memory = 512 * 65536; // 32 MiB maximum memory limit

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
    const test_step = b.step("test", "Run unit tests across memory, Myers diff, fuzzy search, WASI-NN, WIT interfaces, and SIMD");
    test_step.dependOn(&run_unit_tests.step);
}
