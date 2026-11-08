"""
ch8_autonomous_loops.py - Chapter 8: Autonomous Hyper-Executor Superpowers
"""

from reportlab.platypus import Paragraph, Spacer, Table, TableStyle, KeepTogether
from reportlab.lib import colors
try:
    from styles import get_masterclass_styles, CodeSnippet
except ImportError:
    from styles import get_masterclass_styles
    def CodeSnippet(code, lang="Rust"):
        from styles import make_code_block
        return make_code_block(code, caption=lang)

def build_autonomous_loops_chapter():
    styles = get_masterclass_styles()
    if "ChapterTitle" not in styles:
        styles["ChapterTitle"] = styles.get("ChapterHeading", styles.get("Heading1"))
    if "ChapterSubtitle" not in styles:
        styles["ChapterSubtitle"] = styles.get("CoverSubtitle", styles.get("Normal"))
    if "BodyTextCustom" not in styles:
        styles["BodyTextCustom"] = styles.get("Body", styles.get("Normal"))

    story = []

    story.append(Paragraph("Chapter 8: The Four Autonomous Superpowers", styles["ChapterTitle"]))
    story.append(Paragraph("Neural Coprocessor, Zero-Copy POSIX SHM, Self-Healing Daemon Loops & P2P Swarm Mesh", styles["ChapterSubtitle"]))
    story.append(Spacer(1, 15))

    p1_desc = (
        "<b>1. Neural Coprocessor & Local Model Hot-Swapping (hgb-core):</b><br/>"
        "Dynamically classifies developer prompt complexity and routes compute across local WASM kernels "
        "(&lt;2ms deterministic compute), local lightweight Ollama models (Qwen2.5-Coder:1.5b, Phi3:3.8b), "
        "and frontier models via MCP (Gemini-2.5-Flash, Claude-3.5-Sonnet). Employs Dynatune thermal synergy "
        "to downscale quantization (Q8_0 -> Q4_K_M -> Q2_K) and engage thermal fallback chains."
    )
    story.append(Paragraph(p1_desc, styles["BodyTextCustom"]))
    story.append(Spacer(1, 10))

    story.append(CodeSnippet(
        "// Neural Coprocessor Routing in Action\n"
        "let coproc = NeuralCoprocessor::new();\n"
        "let decision = coproc.route(\"format json string with 2-space indentation\");\n"
        "assert!(decision.is_wasm_offload); // Dispatched to hgb-wasm-kernel (<2ms)",
        "Rust"
    ))
    story.append(Spacer(1, 15))

    p2_desc = (
        "<b>2. Zero-Copy POSIX Shared Memory Ring Buffer (hgb-storage):</b><br/>"
        "Eliminates disk I/O bottlenecks through a circular lock-free ring buffer backed by <code>/dev/shm</code> "
        "and <code>memmap2::MmapMut</code>. Employs 64-byte aligned atomic write/read heads and hardware memory fences "
        "to deliver sub-microsecond IPC latency and multi-gigabyte/sec throughput."
    )
    story.append(Paragraph(p2_desc, styles["BodyTextCustom"]))
    story.append(Spacer(1, 10))

    story.append(CodeSnippet(
        "// High-Throughput POSIX Shared Memory Ring Buffer\n"
        "let mut ring = ShmRingBuffer::new(\"llm_pipe\", 16 * 1024 * 1024)?;\n"
        "ring.push(b\"Live token payload streamed at nanosecond latency\")?;\n"
        "let mut buf = [0u8; 1024];\n"
        "let bytes_read = ring.pop(&mut buf)?;",
        "Rust"
    ))
    story.append(Spacer(1, 15))

    p3_desc = (
        "<b>3. Self-Healing Autonomous Daemon Loops (hgb-daemon):</b><br/>"
        "A background supervisor actively monitoring the Tokio event bus for failed WASM, Firecracker, or bare-metal "
        "build executions. Automatically synthesizes diagnostic packages, prompts local LLMs for surgical patches, "
        "verifies First-Green safety inside an isolated RAM CoW OverlaySandbox, and logs DPO pairs."
    )
    story.append(Paragraph(p3_desc, styles["BodyTextCustom"]))
    story.append(Spacer(1, 10))

    p4_desc = (
        "<b>4. Distributed P2P Swarm Mesh (hgb-nextgen):</b><br/>"
        "Facilitates local peer discovery, BLAKE3 authentication tokens, load-balanced task delegation to idle nodes, "
        "and an encrypted distributed hash table (DHT) caching vector embeddings and MCP tool responses."
    )
    story.append(Paragraph(p4_desc, styles["BodyTextCustom"]))
    story.append(Spacer(1, 15))

    p5_desc = (
        "<b>5. Zero-Overhead IPC via Shared Memory & Unix Domain Sockets:</b><br/>"
        "Upgrades daemon-to-CLI communication to framed Unix Domain Socket streaming paired with zero-copy shared memory handles. "
        "Payloads exceeding 4 KiB (such as large LLM context windows or sibling Ollama tensor embeddings) are written directly to "
        "<code>/dev/shm</code>, transmitting only 64-byte aligned descriptors over UDS to eliminate kernel buffer copying."
    )
    story.append(Paragraph(p5_desc, styles["BodyTextCustom"]))
    story.append(Spacer(1, 10))

    story.append(CodeSnippet(
        "// Zero-Overhead UDS + SHM IPC Channel\n"
        "let channel = ShmIpcChannel::connect(\"/run/user/1000/hgbd_shm.sock\")?;\n"
        "let message = ShmIpcMessage::new_shm(ShmMessageType::TensorPayload, tensor_bytes)?;\n"
        "channel.send(&message)?;\n"
        "let response = channel.recv()?;",
        "Rust"
    ))
    story.append(Spacer(1, 15))

    p6_desc = (
        "<b>6. Autonomous Multi-Agent Swarm Orchestration (Actor Model):</b><br/>"
        "Implements a native Tokio Actor Model with four specialized roles: Planner, Coder, Reviewer, and Sandbox Executor. "
        "Agents debate code modifications across iterative consensus rounds inside isolated RAM CoW OverlaySandboxes. "
        "Upon achieving First-Green consensus, a cryptographic BLAKE3 SwarmConsensusCertificate is generated along with DPO distillation pairs."
    )
    story.append(Paragraph(p6_desc, styles["BodyTextCustom"]))
    story.append(Spacer(1, 10))

    story.append(CodeSnippet(
        "// Multi-Agent Actor Swarm Consensus\n"
        "let coordinator = ActorSwarmCoordinator::new(workspace_root)?;\n"
        "let certificate = coordinator.execute_debate(\"optimize buffer pool allocation\", 3).await?;\n"
        "assert!(certificate.consensus_reached);\n"
        "println!(\"Winning patch blake3: {}\", certificate.certificate_hash);",
        "Rust"
    ))
    story.append(Spacer(1, 15))

    p7_desc = (
        "<b>7. Distributed Edge-to-Cloud State Synchronization (CRDTs):</b><br/>"
        "Features Conflict-free Replicated Data Types (CRDTs) including Last-Write-Wins (LWW) Registers, Observed-Remove (OR) Sets, "
        "and Vector Clocks. Enables bidirectional synchronization of architectural decision records (ADRs), skill vaults, and vector embeddings "
        "between local workstation runtimes and Cloudflare Workers edge nodes (wrangler.toml) with zero data loss."
    )
    story.append(Paragraph(p7_desc, styles["BodyTextCustom"]))
    story.append(Spacer(1, 10))

    story.append(CodeSnippet(
        "// Distributed Edge-to-Cloud CRDT Synchronization\n"
        "let mut local_doc = CrdtSyncDocument::new(\"dynabook-node\");\n"
        "let edge_payload = local_doc.export_edge_sync_payload();\n"
        "// Reconcile with remote Cloudflare Worker payload\n"
        "let report = local_doc.import_edge_sync_payload(&remote_edge_payload)?;\n"
        "println!(\"Reconciled {} ADRs and {} skills\", report.adrs_reconciled, report.skills_reconciled);",
        "Rust"
    ))
    story.append(Spacer(1, 15))

    p8_desc = (
        "<b>8. Hardware-Aware Dynamic CPU Governor Integration:</b><br/>"
        "Deeply bridges hgb-daemon with sibling modules dynatune and kernel-optimizer. Inspects Linux sysfs thermal zones, AC power state, "
        "and CPU energy performance preference (EPP). Automatically bursts CPU frequency governors to 'performance' during heavy builds or "
        "Ollama inferences, and utilizes RAII GovernorGuards to drop frequencies back to 'powersave' when idle, while throttling gracefully if temps exceed 80°C."
    )
    story.append(Paragraph(p8_desc, styles["BodyTextCustom"]))
    story.append(Spacer(1, 10))

    story.append(CodeSnippet(
        "// Hardware-Aware Dynamic Governor Guard\n"
        "let watchdog = DynamicGovernorWatchdog::new();\n"
        "{\n"
        "    let _guard = watchdog.boost_for_workload(WorkloadKind::HeavyCompilation)?;\n"
        "    // CPU frequency scaled to peak, EPP shifted to performance\n"
        "    run_heavy_compilation();\n"
        "} // RAII drop: governor seamlessly restored to baseline powersave",
        "Rust"
    ))
    story.append(Spacer(1, 15))

    p9_desc = (
        "<b>9. Hybrid Kernel-Level Acceleration (dynatune, CPU Governors & MGLRU Memory Pressure):</b><br/>"
        "Bridges hgb-daemon deeply with sibling projects <code>dynatune</code> and <code>kernel-optimizer</code> "
        "(<code>dynabook-kernel-ctl</code>, <code>plugins/mglru-monitor.sh</code>, <code>plugins/cpu-governor.sh</code>). "
        "Dynamic CPU frequency governor shifting to <code>performance</code>, Energy Performance Preference (EPP) "
        "tuning to <code>performance</code>, Multi-Gen LRU (MGLRU) low-latency tuning (<code>min_ttl_ms 1000</code>), "
        "and reclaim efficiency analysis in <code>/proc/vmstat</code> (pgscan vs pgsteal). Features thermal and battery "
        "watchdogs backing off to balanced mode when CPU temp approaches 80°C or battery power is low, managed by RAII <code>KernelAccelerationGuard</code>."
    )
    story.append(Paragraph(p9_desc, styles["BodyTextCustom"]))
    story.append(Spacer(1, 10))

    story.append(CodeSnippet(
        "// Hybrid Kernel-Level Acceleration with RAII Guard\n"
        "let accelerator = HybridKernelAccelerator::new();\n"
        "{\n"
        "    let _guard = accelerator.boost_workload(\"heavy_ai_inference\")?;\n"
        "    // CPU governor -> performance, EPP -> performance, MGLRU min_ttl_ms -> 1000\n"
        "    execute_heavy_model_pipeline();\n"
        "} // RAII drop: automatically resets to powersave governor, balance_performance EPP, min_ttl_ms 0",
        "Rust"
    ))
    story.append(Spacer(1, 15))

    p10_desc = (
        "<b>10. Distributed P2P Wasm Compute Mesh & Ephemeral MicroVM Swarms:</b><br/>"
        "Evolves <code>hgb-wasm-kernel</code> into a sovereign Distributed Compute Mesh. Discovers peer <code>hgbd</code> nodes "
        "across LAN and edge environments, tracks CPU load from <code>/proc/loadavg</code> and memory from <code>/proc/meminfo</code>, "
        "and packages mini-agents or parsers into <code>.wasm</code> modules with cryptographic BLAKE3 hashes and capability declarations. "
        "Streams modules across peers and delegates workloads to idle nodes, seamlessly integrating ephemeral Firecracker microVMs "
        "or rootless Landlock LSM + RAM CoW <code>OverlaySandbox</code> in <code>/dev/shm</code> for untrusted LLM code isolation."
    )
    story.append(Paragraph(p10_desc, styles["BodyTextCustom"]))
    story.append(Spacer(1, 10))

    story.append(CodeSnippet(
        "// Distributed P2P WASM Compute Mesh Execution\n"
        "let mut mesh = WasmComputeMesh::new(workspace_root)?;\n"
        "let pkg = mesh.publish_package(\"data_cleaner\", &wasm_bytes, vec![\"crypto\".into()])?;\n"
        "let result = mesh.execute_workload(\"task_01\", \"data_cleaner\", payload, false)?;\n"
        "println!(\"Executed on node {} in {}ms\", result.executing_node_id, result.duration_ms);",
        "Rust"
    ))
    story.append(Spacer(1, 15))

    p11_desc = (
        "<b>11. Proactive Autonomous Feedback Loops (Vibe-Coding Watcher & Closed-Loop Self-Healing):</b><br/>"
        "Real-time autonomous feedback loop embedded in <code>hgb-daemon</code>. Automatically intercepts code changes or MCP tool file writes, "
        "triggers language checks (<code>cargo check</code>, <code>zig test</code>, <code>python3 -m py_compile</code>), and catches failures. "
        "Synthesizes structured diagnostic prompts, queries local Ollama models (<code>qwen2.5-coder:1.5b</code>, <code>phi3:3.8b</code>) "
        "for surgical diffs, verifies candidate fixes inside an isolated RAM CoW <code>OverlaySandbox</code>, and commits only when "
        "First-Green verified! Simultaneously logs DPO distillation pairs in <code>.hgb/distillation/</code> for recursive self-evolution."
    )
    story.append(Paragraph(p11_desc, styles["BodyTextCustom"]))
    story.append(Spacer(1, 10))

    story.append(CodeSnippet(
        "// Proactive Autonomous Vibe-Coding Watcher\n"
        "let watcher = VibeCodingWatcher::new(workspace_root);\n"
        "let record = watcher.trigger_watch_cycle(Path::new(\"src/main.rs\"), None)?;\n"
        "if record.compiler_failed && record.sandbox_verified {\n"
        "    println!(\"Auto-healed file and committed First-Green patch in {}ms!\", record.duration_ms);\n"
        "}",
        "Rust"
    ))
    story.append(Spacer(1, 15))

    p12_desc = (
        "<b>12. Advanced Episodic Vector Memory & Long-Horizon Self-Correction (hgb-storage):</b><br/>"
        "Features an embedded zero-dependency SIMD-accelerated vector indexing engine backed by SQLite WAL "
        "(<code>.hgb/episodic_vector_memory.db</code>). When a task or compilation fails, captures compiler diagnostics "
        "and error stacks, searches past semantically similar incidents via 8-wide unrolled SIMD cosine similarity, "
        "recalls winning patches, and synthesizes a high-confidence Self-Correction Reflection that guides the next iteration."
    )
    story.append(Paragraph(p12_desc, styles["BodyTextCustom"]))
    story.append(Spacer(1, 10))

    story.append(CodeSnippet(
        "// Long-Horizon Self-Correction Reflection Loop\n"
        "let memory = EpisodicVectorMemoryEngine::new(workspace_root)?;\n"
        "let reflection = memory.generate_self_correction_reflection(&compiler_diagnostic)?;\n"
        "if reflection.should_apply_known_fix {\n"
        "    println!(\"Applying verified winning patch from episode: {}\", reflection.failure_analysis);\n"
        "    sandbox.apply_patch(&reflection.best_matching_fix.unwrap())?;\n"
        "}",
        "Rust"
    ))
    story.append(Spacer(1, 15))

    p13_desc = (
        "<b>13. Test-Driven Genetic Mutator & Autonomous Self-Healing (hgb-nextgen):</b><br/>"
        "Integrates directly with CI workflows and local CLI test executions. When compile or test errors occur, "
        "the Genetic Mutator captures stderr diagnostics, error codes, and line numbers. It queries sibling local Ollama "
        "(<code>qwen2.5-coder:1.5b</code> / <code>dynabook-coder:latest</code>) and deploys 4 genetic mutation operators "
        "(<code>TypeCast</code>, <code>BoundaryCheck</code>, <code>ImportFix</code>, <code>SyntaxCorrection</code>). "
        "Mutant candidates are evaluated inside an ephemeral RAM CoW <code>OverlaySandbox</code> with a multi-objective fitness "
        "function (exit code == 0, blast radius minimization, test pass rate). Winning patches are cryptographically sealed "
        "with a BLAKE3 certificate and automatically committed."
    )
    story.append(Paragraph(p13_desc, styles["BodyTextCustom"]))
    story.append(Spacer(1, 10))

    story.append(CodeSnippet(
        "// Test-Driven Genetic Mutator Self-Healing\n"
        "let mutator = GeneticPatchMutator::new(workspace_root);\n"
        "let report = mutator.evaluate_and_heal(\"src/lib.rs\", failure_log);\n"
        "if let Some(winner) = report.winning_candidate {\n"
        "    println!(\"Sealed winning mutant patch {} (fitness: {:.1}) with BLAKE3: {}\", winner.id, winner.fitness_score, report.certificate_hash);\n"
        "}",
        "Rust"
    ))
    story.append(Spacer(1, 15))

    # =========================================================================
    # PILLAR 5: FULLY AUTONOMOUS SELF-IMPROVING CI/CD & WORKBENCH LOOP
    # =========================================================================
    p14_desc = (
        "<b>5. Fully Autonomous Self-Improving CI/CD & Workbench Loop (Pillar 5):</b><br/>"
        "Unifies the complete developer feedback cycle into a closed-loop autonomous workbench. "
        "Integrates real-time compiler interception across workspace crates (<code>hgb-core</code>, <code>hgb-daemon</code>), "
        "automated test execution, AST diagnostic extraction (Rust E0308/E0425, Zig, Python), speculative branch synthesis, "
        "and multi-candidate verification within an isolated ephemeral RAM Copy-on-Write (CoW) <code>OverlaySandbox</code> "
        "in <code>/dev/shm</code>. Only First-Green verified patches are committed back to physical workspace disk with atomic "
        "timeline checkpoints. Following successful verification, triggers dynamic zero-downtime hot-reloads of <code>WasmHotPool</code> "
        "modules, model weights, and daemon state via the <code>hgb_hot_reload_component</code> MCP protocol.<br/><br/>"
        "<b>Autonomous CI/CD & Workbench Loop Architecture:</b><br/>"
        "&nbsp;&nbsp;[Code Mutation / Build Trigger]<br/>"
        "&nbsp;&nbsp;&nbsp;&nbsp;&nbsp;&nbsp;│<br/>"
        "&nbsp;&nbsp;&nbsp;&nbsp;&nbsp;&nbsp;▼<br/>"
        "&nbsp;&nbsp;[CompilerInterceptor: Parse stderr / panics / AST locations]<br/>"
        "&nbsp;&nbsp;&nbsp;&nbsp;&nbsp;&nbsp;│<br/>"
        "&nbsp;&nbsp;&nbsp;&nbsp;&nbsp;&nbsp;▼<br/>"
        "&nbsp;&nbsp;[Branch Synthesis: Ollama Speculative Generation & Rule Mutators]<br/>"
        "&nbsp;&nbsp;&nbsp;&nbsp;&nbsp;&nbsp;│<br/>"
        "&nbsp;&nbsp;&nbsp;&nbsp;&nbsp;&nbsp;▼<br/>"
        "&nbsp;&nbsp;[RAM CoW OverlaySandbox (/dev/shm): Isolated &lt;2ms Virtualization]<br/>"
        "&nbsp;&nbsp;&nbsp;&nbsp;&nbsp;&nbsp;│<br/>"
        "&nbsp;&nbsp;&nbsp;&nbsp;&nbsp;&nbsp;▼<br/>"
        "&nbsp;&nbsp;[First-Green Verification: cargo check &amp; cargo test (exit 0)]<br/>"
        "&nbsp;&nbsp;&nbsp;&nbsp;&nbsp;&nbsp;├── Pass ──► [Atomic Disk Commit + DPO Distillation Pair]<br/>"
        "&nbsp;&nbsp;&nbsp;&nbsp;&nbsp;&nbsp;│                  │<br/>"
        "&nbsp;&nbsp;&nbsp;&nbsp;&nbsp;&nbsp;│                  ▼<br/>"
        "&nbsp;&nbsp;&nbsp;&nbsp;&nbsp;&nbsp;│            [Zero-Downtime Hot-Reload (WasmHotPool / Config)]<br/>"
        "&nbsp;&nbsp;&nbsp;&nbsp;&nbsp;&nbsp;└── Fail ──► [Genetic Repair / Clean Rollback (Zero Pollution)]"
    )
    story.append(Paragraph(p14_desc, styles["BodyTextCustom"]))
    story.append(Spacer(1, 10))

    story.append(CodeSnippet(
        "// Autonomous CI/CD Self-Healing & Hot-Reload Loop (Rust)\n"
        "let daemon_loop = SelfHealingDaemonLoop::new(workspace_root);\n"
        "// 1. Run crate-level cargo checks or test suite\n"
        "let report = daemon_loop.run_crate_ci_check(Some(\"hgb-core\"), true)?;\n"
        "if report.sandbox_verification_passed && report.applied_to_disk {\n"
        "    println!(\"Healed crate failure in {}ms; First-Green verified!\", report.duration_ms);\n"
        "    // 2. Trigger zero-downtime hot-reload of WASM pool and daemon state\n"
        "    let reload_status = daemon_loop.trigger_hot_reload_hook(\"wasm_plugin\")?;\n"
        "    assert_eq!(reload_status[\"status\"], \"reloaded\");\n"
        "}",
        "Rust"
    ))
    story.append(Spacer(1, 10))

    story.append(CodeSnippet(
        "# Python Autonomous Workbench Runner\n"
        "import urllib.request, json\n"
        "\n"
        "def run_autonomous_ci_heal(target_crate=\"hgb-core\"):\n"
        "    payload = json.dumps({\n"
        "        \"jsonrpc\": \"2.0\", \"id\": 1, \"method\": \"tools/call\",\n"
        "        \"params\": {\n"
        "            \"name\": \"hgb_self_healing_ci\",\n"
        "            \"arguments\": {\"target_crate\": target_crate, \"run_tests\": True, \"auto_apply\": True}\n"
        "        }\n"
        "    }).encode('utf-8')\n"
        "    req = urllib.request.Request('http://127.0.0.1:8083/mcp', data=payload, headers={'Content-Type': 'application/json'})\n"
        "    with urllib.request.urlopen(req) as resp:\n"
        "        res = json.loads(resp.read().decode('utf-8'))\n"
        "    print(\"Autonomous CI Status:\", res[\"result\"][\"content\"][0][\"text\"])\n"
        "\n"
        "if __name__ == '__main__':\n"
        "    run_autonomous_ci_heal()",
        "Python"
    ))
    story.append(Spacer(1, 15))

    # =========================================================================
    # POWER-UP 3: AUTONOMOUS MCP SELF-HEALING & PROFILING LOOP
    # =========================================================================
    p15_desc = (
        "<b>14. Autonomous MCP Self-Healing & PGO Profiling Loop (AutonomousMcpSelfHealingLoop):</b><br/>"
        "Elevates the MCP JSON configuration into a resident self-healing engine bridging <code>hagibis.mcp.json</code>, "
        "<code>NativeSelfHealingDaemon</code>, <code>PgoFeedbackLoop</code>, and <code>scripts/pgo-optimize.sh</code>. "
        "Exposes three production-grade MCP tools:<br/>"
        "• <b><code>hgb_self_healing_diagnose</code></b>: Intercepts compiler diagnostics across Rust AST (E0308 type mismatch, "
        "E0425 unresolved symbol, E0596 mutable borrow), Zig, and Python. Isolates speculative repairs inside an ephemeral "
        "RAM CoW sandbox (<code>/dev/shm</code>) and verifies First-Green exit code 0 before atomic workspace commit.<br/>"
        "• <b><code>hgb_pgo_tune_pass</code></b>: Continuously inspects execution latency SLAs (p99 latency) and tool invocation "
        "frequency. Upon detecting regressions or heavy build slowdowns, collects binary execution profiles into "
        "<code>.hgb/pgo_hotspots.json</code> and launches <code>scripts/pgo-optimize.sh</code> in background mode with strict cooldown guards.<br/>"
        "• <b><code>hgb_auto_patch_bottleneck</code></b>: Automatically discovers hot execution bottlenecks and synthesizes zero-overhead "
        "optimizations including capacity preallocation (<code>Vec::with_capacity</code>), inlining attributes (<code>#[inline(always)]</code>), "
        "buffered I/O wrappers, and 8-wide SIMD vectorization chunks, certified with BLAKE3 consensus certificates."
    )
    story.append(Paragraph(p15_desc, styles["BodyTextCustom"]))
    story.append(Spacer(1, 10))

    story.append(CodeSnippet(
        "// Autonomous MCP Self-Healing & Profiling Loop (Rust)\n"
        "let autonomous_loop = AutonomousMcpSelfHealingLoop::new(workspace_root)?;\n"
        "\n"
        "// 1. Autonomous Compilation Diagnosis & Speculative Repair\n"
        "let diag_report = autonomous_loop.handle_self_healing_diagnose(SelfHealingDiagnoseRequest {\n"
        "    target_file: \"src/lib.rs\".to_string(),\n"
        "    error_context: Some(compiler_stderr),\n"
        "    code: None,\n"
        "    compiler: Some(\"rust\".to_string()),\n"
        "    check_command: Some(\"cargo check\".to_string()),\n"
        "    max_retries: Some(3),\n"
        "    auto_apply: true,\n"
        "}).await?;\n"
        "assert!(diag_report.healed && diag_report.verification_passed);\n"
        "\n"
        "// 2. Profile-Guided Optimization (PGO) Background Pass\n"
        "let pgo_report = autonomous_loop.handle_pgo_tune_pass(PgoTunePassRequest {\n"
        "    trigger_reason: Some(\"latency_p99_regression\".to_string()),\n"
        "    force: false,\n"
        "    pgo_only: true,\n"
        "    run_benchmark: false,\n"
        "    cooldown_seconds: Some(300),\n"
        "    target_binary: Some(\"all\".to_string()),\n"
        "}).await?;\n"
        "if pgo_report.triggered {\n"
        "    println!(\"Launched PGO pipeline PID: {:?}\", pgo_report.pid);\n"
        "}\n"
        "\n"
        "// 3. Zero-Overhead Bottleneck Optimization\n"
        "let opt_report = autonomous_loop.handle_auto_patch_bottleneck(AutoPatchBottleneckRequest {\n"
        "    target_file: \"src/compute.rs\".to_string(),\n"
        "    function_name: Some(\"calculate_similarity\".to_string()),\n"
        "    optimization_strategy: \"preallocate_capacity\".to_string(),\n"
        "    verify_first_green: true,\n"
        "    auto_apply: true,\n"
        "}).await?;\n"
        "println!(\"Sealed optimization with BLAKE3 cert: {:?}\", opt_report.consensus_certificate_hash);",
        "Rust"
    ))
    story.append(Spacer(1, 15))

    # =========================================================================
    # ARCHITECTURAL POWER-UP III: VECTOR-NATIVE STORAGE & MCP MEMORY MESH
    # =========================================================================
    p16_desc = (
        "<b>15. Vector-Native Storage & MCP Memory Mesh (VectorMcpMemoryMesh):</b><br/>"
        "Supercharges Hagibis's state fabric with a persistent vector memory mesh combining embedded vector "
        "storage (IEEE-754 little-endian binary float vectors compatible with <code>sqlite-vec</code> and native HNSW), "
        "SQLite metadata, and FTS5 full-text indexing. Stores long-term developer context, past fix patterns, "
        "command outcomes, and user preferences into an in-memory cached WAL database (<code>.hgb/vector_memory.db</code>).<br/>"
        "Exposes four production MCP memory mesh tools:<br/>"
        "• <b><code>index_codebase_memory</code></b>: Recursively parses workspace codebases, extracts AST symbols (functions, "
        "structs, traits, classes), partitions content into overlapping chunks, and registers IEEE-754 semantic embeddings.<br/>"
        "• <b><code>semantic_memory_search</code></b>: Queries vector memory using hybrid ANN skip-list retrieval (HNSW), "
        "AVX2 SIMD cosine similarity, and FTS5 BM25 lexical ranking with reciprocal score fusion.<br/>"
        "• <b><code>recall_developer_context</code></b>: Recalls developer habits, architectural decisions, and previous debugging "
        "sessions with access-count reinforcement.<br/>"
        "• <b><code>update_long_term_memory</code></b>: Dynamically records new architectural insights, rules, and fix patterns "
        "with zero restart downtime."
    )
    story.append(Paragraph(p16_desc, styles["BodyTextCustom"]))
    story.append(Spacer(1, 10))

    story.append(CodeSnippet(
        "// Vector-Native Storage & MCP Memory Mesh (VectorMcpMemoryMesh)\n"
        "let mesh = VectorMcpMemoryMesh::open_workspace(&workspace_root)?;\n"
        "\n"
        "// 1. Recursive Codebase AST Indexing & Chunk Embedding\n"
        "let report = mesh.index_codebase(&workspace_root, &IndexOptions::default())?;\n"
        "println!(\"Indexed {} chunks across {} files (duration: {}ms)\",\n"
        "    report.total_chunks, report.indexed_files, report.duration_ms);\n"
        "\n"
        "// 2. Hybrid Semantic Vector + FTS5 Retrieval\n"
        "let results = mesh.search_by_text(\"HNSW cosine similarity graph search\", 5, None)?;\n"
        "for hit in results {\n"
        "    println!(\"Hit [score: {:.3}]: {}\", hit.combined_score, hit.item.title);\n"
        "}\n"
        "\n"
        "// 3. Persistent Long-Term Memory Insight Recording\n"
        "let item = mesh.record_past_fix_pattern(\n"
        "    \"E0308 type mismatch\",\n"
        "    \"Expected &str found String\",\n"
        "    \"Convert with .as_str()\",\n"
        "    true,\n"
        ")?;\n"
        "println!(\"Stored long-term fix memory: {}\", item.id);",
        "Rust"
    ))
    story.append(Spacer(1, 15))

    # =========================================================================
    # SECTION 16: CALDOAI-INSPIRED SKILL UNTANGLING & AUTONOMIC HARDWARE-TUNED PGO FEEDBACK
    # =========================================================================
    p17_desc = (
        "<b>16. CaldoAI-Inspired Skill Untangling & Autonomic Hardware-Tuned PGO Feedback:</b><br/>"
        "Pioneers CaldoAI-inspired modularization by untangling monolithic agent loops into sovereign, "
        "reusable, atomic skills (diagnostic interceptors, speculative sandboxes, PGO profilers, and hardware monitors). "
        "Each skill exposes declarative precondition schemas and BLAKE3 attestation receipts, eliminating tool hallucination "
        "and prompt congestion. Connects the autonomous event loop (<code>AutonomousDaemonLoopService</code> and "
        "<code>AutonomousMcpSelfHealingLoop</code>) directly with real-time Linux kernel telemetry from <code>dynatune</code> "
        "and <code>kernel-optimizer</code> via <code>KernelDynatuneLoop</code> and <code>HardwareDynamicFfiBridge</code>.<br/><br/>"
        "<b>Autonomic Tri-Modal Feedback & Optimization Vectors:</b><br/>"
        "• <b>Thermal Spike Mitigation</b>: Real-time sysfs thermal zone monitoring with millidegree precision. When package temperatures "
        "reach or exceed 78.0°C (or critical thermal clamp limits), the daemon automatically dials back CPU frequency governors to "
        "<code>powersave</code>, sets Energy Performance Preference (EPP) to <code>power</code>, throttles concurrency to 1 worker, "
        "and initiates an autonomous background PGO tuning pass (<code>scripts/pgo-optimize.sh</code>) to restructure instruction caches and minimize thermal dissipation.<br/>"
        "• <b>Swap Pressure & MGLRU Compaction</b>: Actively tracks <code>/proc/pressure/memory</code> (PSI avg10) and host swap exhaustion "
        "(<code>swap_used_mb &gt; 512MB</code>) alongside Multi-Gen LRU (MGLRU) reclaim efficiency in <code>/proc/vmstat</code>. Automatically "
        "escalates MGLRU low-latency aging (<code>min_ttl_ms 1000</code>) and launches background PGO passes (<code>--pgo-only</code>) to compress "
        "resident set sizes (RSS) and eliminate disk paging bottlenecks.<br/>"
        "• <b>p99 Latency SLA Self-Tuning</b>: Continuously computes rolling latency percentiles across WASM microkernel and LLM stream executions. "
        "When p99 latency regresses beyond SLA thresholds (e.g. &gt;500ms), the daemon dynamically bursts CPU governors to <code>performance</code>, "
        "engages Intel Turbo Boost, and launches autonomous PGO + LLVM BOLT profile passes with strict cooldown guards to reclaim sub-millisecond responsiveness."
    )
    story.append(Paragraph(p17_desc, styles["BodyTextCustom"]))
    story.append(Spacer(1, 10))

    story.append(CodeSnippet(
        "// Autonomic Hardware-Tuned Telemetry & PGO Feedback Loop (Rust)\n"
        "let autonomous_loop = AutonomousMcpSelfHealingLoop::new(workspace_root)?;\n"
        "let kernel_loop = KernelDynatuneLoop::new();\n"
        "let ffi_bridge = HardwareDynamicFfiBridge::new();\n"
        "\n"
        "// 1. Inspect real-time hardware telemetry from dynatune & kernel-optimizer\n"
        "let thermal = ffi_bridge.read_thermal_telemetry();\n"
        "let memory = ffi_bridge.read_memory_telemetry();\n"
        "let percentiles = autonomous_loop.pgo_feedback.compute_latency_percentiles().await;\n"
        "\n"
        "// 2. Autonomic Tri-Modal Telemetry Evaluation & Governor Adjustment\n"
        "if thermal.cpu_pkg_temp_c >= 78.0 || thermal.is_thermal_clamped {\n"
        "    // Thermal Spike: clamp governor to powersave & launch PGO cooling pass\n"
        "    kernel_loop.apply_profile(KernelProfile::ThermalThrottled)?;\n"
        "    ffi_bridge.apply_profile(HardwareGovernorProfile::Powersave)?;\n"
        "    autonomous_loop.handle_pgo_tune_pass(PgoTunePassRequest {\n"
        "        trigger_reason: Some(\"thermal_spike_throttle\".into()),\n"
        "        pgo_only: true,\n"
        "        ..Default::default()\n"
        "    }).await?;\n"
        "} else if memory.swap_used_mb > 512 || memory.psi_some.avg10 > 15.0 {\n"
        "    // Swap Pressure: escalate MGLRU min_ttl_ms to 1000ms & compress RSS footprint\n"
        "    kernel_loop.set_mglru_min_ttl(1000)?;\n"
        "    autonomous_loop.handle_pgo_tune_pass(PgoTunePassRequest {\n"
        "        trigger_reason: Some(\"swap_pressure_mglru\".into()),\n"
        "        pgo_only: true,\n"
        "        ..Default::default()\n"
        "    }).await?;\n"
        "} else if percentiles.sample_count >= 5 && percentiles.p99_ms > 500.0 {\n"
        "    // p99 Latency SLA Regression: burst to performance & launch PGO+BOLT pass\n"
        "    kernel_loop.apply_profile(KernelProfile::Performance)?;\n"
        "    autonomous_loop.handle_pgo_tune_pass(PgoTunePassRequest {\n"
        "        trigger_reason: Some(\"latency_p99_regression\".into()),\n"
        "        pgo_only: false,\n"
        "        ..Default::default()\n"
        "    }).await?;\n"
        "}",
        "Rust"
    ))
    story.append(Spacer(1, 15))

    story.append(CodeSnippet(
        "# CaldoAI-Inspired Skill Client & Daemon Event Loop Connector (Python)\n"
        "from docs.masterclass.ch8_autonomous_loops import AutonomousDaemonLoopClient\n"
        "\n"
        "client = AutonomousDaemonLoopClient(mcp_url=\"http://127.0.0.1:8083\")\n"
        "\n"
        "# 1. Evaluate autonomic hardware telemetry and adjust governor profiles\n"
        "report = client.evaluate_autonomic_telemetry(force_pgo=False)\n"
        "print(f\"Telemetry Status: CPU={report.get('cpu_temp_c')}°C | Swap={report.get('swap_used_mb')}MB\")\n"
        "\n"
        "# 2. Trigger autonomous PGO tuning pass on SLA regression\n"
        "if report.get('latency_regression_detected'):\n"
        "    pgo_res = client.trigger_pgo_pass(reason=\"latency_p99_sla_violation\")\n"
        "    print(\"Launched background PGO pipeline PID:\", pgo_res.get(\"pid\"))",
        "Python"
    ))
    story.append(Spacer(1, 15))

    # =========================================================================
    # SECTION 17: LANCEDB VECTOR RETRIEVAL, FIRECRACKER MICROVM & OPENTELEMETRY TRACING
    # =========================================================================
    p18_desc = (
        "<b>17. LanceDB Vector Retrieval, Firecracker MicroVM Provisioning & OpenTelemetry Distributed Tracing:</b><br/>"
        "Scales Hagibis's cognitive execution runtime with zero-copy LanceDB columnar vector indexing, hardware-isolated "
        "Firecracker microVM sandboxing, and OpenTelemetry (OTel) distributed tracing.<br/>"
        "• <b>LanceDB Vector Store</b>: Stores dense embedding representations in Apache Arrow disk-backed tables, "
        "delivering sub-millisecond similarity queries across millions of code symbols without RAM bloat.<br/>"
        "• <b>Firecracker MicroVM Provisioning (firecracker_microvm_pipeline)</b>: Spawns ephemeral Linux microVMs "
        "in &lt;15ms with dedicated vCPUs, memory ceilings, and read-only root filesystems, providing kernel-level "
        "isolation for untrusted LLM-generated code execution.<br/>"
        "• <b>OpenTelemetry Distributed Tracing</b>: Injects OTel trace context headers across agent turns, capturing "
        "tool latency percentiles, LLM token usages, and reasoning branch histories into unified trace spans."
    )
    story.append(Paragraph(p18_desc, styles["BodyTextCustom"]))
    story.append(Spacer(1, 10))

    story.append(CodeSnippet(
        "// Section 17: LanceDB Vector Retrieval & Firecracker MicroVM Execution\n"
        "let vm_pipeline = FirecrackerMicrovmPipeline::new(FirecrackerMicrovmConfig::default())?;\n"
        "let execution_report = vm_pipeline.execute_code(CodeExecutionSpec {\n"
        "    language: \"rust\".into(),\n"
        "    code: code_snippet,\n"
        "    timeout_secs: 10,\n"
        "    memory_limit_mb: 256,\n"
        "}).await?;\n"
        "assert_eq!(execution_report.exit_code, 0);",
        "Rust"
    ))
    story.append(Spacer(1, 15))

    # =========================================================================
    # SECTION 18: RIG NATIVE RUST AGENT FRAMEWORK & AUTONOMIC HARDWARE GOVERNOR LOOP
    # =========================================================================
    p19_desc = (
        "<b>18. Rig Native Rust Agent Loop & Autonomic Hardware Governor Integration (hgb-core):</b><br/>"
        "Introduces a high-performance native Rust agent framework modeled after <code>rig</code> and <code>langchain</code>. "
        "Features declarative <code>ToolDefinition</code> (JSON schema), async <code>ToolCallable</code> dispatch, and multi-turn "
        "ReAct cognitive reasoning loops with dynamic Model Context Protocol (MCP) tool routing.<br/>"
        "Directly interfaces with bare-metal Linux hardware telemetry from sibling projects <code>kernel-optimizer</code> "
        "(<code>dynabook-kernel-ctl</code>) and <code>dynatune</code> to enforce autonomic thermal and power governance:<br/>"
        "• <b>Thermal Spike Throttling (&gt;= 78.0°C)</b>: Automatically dials reasoning token budget down to 512 tokens, "
        "inserts 250ms cooling sleeps between cognitive iterations, clamps CPU scaling governors to <code>powersave</code>, "
        "and flags <code>hardware_throttled = true</code>.<br/>"
        "• <b>Battery Conservation Mode</b>: When AC mains is unplugged, constrains maximum reasoning loop iterations to 5 "
        "and sets Energy Performance Preference (EPP) to <code>balance_power</code>."
    )
    story.append(Paragraph(p19_desc, styles["BodyTextCustom"]))
    story.append(Spacer(1, 10))

    story.append(CodeSnippet(
        "// Section 18: Rig Native Rust Agent Loop with Autonomic Hardware Governor\n"
        "let agent = RigAgent::builder()\n"
        "    .system_prompt(\"You are Hagibis's autonomous hardware-aware coding architect.\")\n"
        "    .provider(RigProviderConfig {\n"
        "        kind: RigProviderKind::DeterministicFallback,\n"
        "        model: \"deterministic-rules-v1\".into(),\n"
        "        endpoint: None,\n"
        "        api_key: None,\n"
        "        temperature: Some(0.2),\n"
        "        max_tokens: Some(2048),\n"
        "    })\n"
        "    .max_iterations(10)\n"
        "    .enable_autonomic_hardware_throttle(true)\n"
        "    .tool(FnTool::new(\n"
        "        \"calculator\",\n"
        "        \"Evaluate math expressions\",\n"
        "        serde_json::json!({\"type\": \"object\", \"properties\": {\"expression\": {\"type\": \"string\"}}}),\n"
        "        |args| Box::pin(async move { Ok(serde_json::json!({\"result\": 42})) }),\n"
        "    ))\n"
        "    .build()?;\n"
        "\n"
        "let response = agent.prompt(\"Use calculator to evaluate 6 * 7\").await?;\n"
        "println!(\"Final Answer: {}\", response.final_answer);\n"
        "println!(\"Hardware Throttled: {}\", response.hardware_throttled);\n"
        "println!(\"CPU Temp: {:.1}°C\", response.hardware_state.cpu_pkg_temp_c);",
        "Rust"
    ))
    story.append(Spacer(1, 15))

    return story

# =============================================================================
# Direct Connection with hgb-daemon's Event Loop (AutonomousDaemonLoopClient)
# =============================================================================

import os
import json
import socket
import urllib.request
import urllib.error

class AutonomousDaemonLoopClient:
    """
    Direct Python client interfacing with hgb-daemon's resident autonomous event loop
    (AutonomousDaemonLoopService / AutonomousMcpSelfHealingLoop) via HTTP/SSE MCP and Unix Domain Sockets.
    """
    def __init__(self, mcp_url="http://127.0.0.1:8083", socket_path=None):
        self.mcp_url = mcp_url
        self.socket_path = socket_path or os.environ.get("HGB_DAEMON_SOCKET") or "/run/user/1000/hgb.sock"

    def call_mcp_tool(self, tool_name, arguments=None):
        payload = json.dumps({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": tool_name,
                "arguments": arguments or {}
            }
        }).encode("utf-8")
        endpoints = [
            f"{self.mcp_url}/message",
            f"{self.mcp_url}/mcp",
            self.mcp_url,
        ]
        last_error = None
        for ep in endpoints:
            try:
                req = urllib.request.Request(ep, data=payload, headers={"Content-Type": "application/json"})
                with urllib.request.urlopen(req, timeout=5) as resp:
                    res = json.loads(resp.read().decode("utf-8"))
                    if "result" in res:
                        content = res["result"].get("content", [])
                        if content and "text" in content[0]:
                            try:
                                return json.loads(content[0]["text"])
                            except Exception:
                                return content[0]["text"]
                        return res["result"]
            except Exception as e:
                last_error = e
        return {
            "status": "offline_fallback",
            "tool": tool_name,
            "error": str(last_error),
            "message": f"Daemon MCP endpoint not reachable at {self.mcp_url}. Ensure hgbd is running."
        }

    def evaluate_autonomic_telemetry(self, force_pgo=False):
        """Evaluate real-time hardware telemetry and trigger autonomous optimizations."""
        return self.call_mcp_tool("hgb_autonomic_hardware_tune", {"force_pgo": force_pgo})

    def trigger_pgo_pass(self, reason="latency_p99_regression", force=False, pgo_only=True):
        """Dispatch autonomous PGO tuning pass with cooldown guards."""
        return self.call_mcp_tool("hgb_pgo_tune_pass", {
            "trigger_reason": reason,
            "force": force,
            "pgo_only": pgo_only,
            "run_benchmark": False
        })

    def diagnose_and_heal(self, target_file, error_context="", compiler="auto", auto_apply=True):
        """Invoke autonomous self-healing compiler diagnostic repair in /dev/shm sandbox."""
        return self.call_mcp_tool("hgb_self_healing_diagnose", {
            "target_file": target_file,
            "error_context": error_context,
            "compiler": compiler,
            "auto_apply": auto_apply
        })

    def patch_bottleneck(self, target_file, function_name=None, strategy="auto", auto_apply=True):
        """Auto-patch performance bottlenecks in target file."""
        return self.call_mcp_tool("hgb_auto_patch_bottleneck", {
            "target_file": target_file,
            "function_name": function_name,
            "optimization_strategy": strategy,
            "auto_apply": auto_apply
        })

    def query_lancedb_vector_index(self, query_vector, table_name="codebase_embeddings", limit=5):
        """Query LanceDB vector dataset using Reciprocal Rank Fusion (RRF)."""
        return self.call_mcp_tool("hgb_vector_lancedb_query", {
            "vector": query_vector,
            "table": table_name,
            "limit": limit
        })

    def execute_rig_agent_loop(self, prompt, system_prompt="You are Hagibis's autonomous architect.", provider="deterministic_fallback"):
        """Execute multi-turn Rig agent reasoning loop with autonomic hardware throttling."""
        return self.call_mcp_tool("hgb_rig_agent_execute", {
            "prompt": prompt,
            "system_prompt": system_prompt,
            "provider": provider,
            "enable_autonomic_hardware_throttle": True
        })

def connect_daemon_autonomous_loop(mcp_url="http://127.0.0.1:8083", socket_path=None):
    return AutonomousDaemonLoopClient(mcp_url=mcp_url, socket_path=socket_path)

def query_autonomous_daemon_loop(workflow_id=None, mcp_url="http://127.0.0.1:8083"):
    client = AutonomousDaemonLoopClient(mcp_url=mcp_url)
    return client.call_mcp_tool("hgb_autonomic_hardware_tune", {})

def trigger_autonomous_pgo_pass(reason="latency_p99_regression", force=False, mcp_url="http://127.0.0.1:8083"):
    client = AutonomousDaemonLoopClient(mcp_url=mcp_url)
    return client.trigger_pgo_pass(reason=reason, force=force)

def evaluate_autonomic_hardware_telemetry(force_pgo=False, mcp_url="http://127.0.0.1:8083"):
    client = AutonomousDaemonLoopClient(mcp_url=mcp_url)
    return client.evaluate_autonomic_telemetry(force_pgo=force_pgo)

def diagnose_compiler_error(target_file, error_context="", mcp_url="http://127.0.0.1:8083"):
    client = AutonomousDaemonLoopClient(mcp_url=mcp_url)
    return client.diagnose_and_heal(target_file, error_context=error_context)

if __name__ == "__main__":
    from reportlab.platypus import SimpleDocTemplate
    doc = SimpleDocTemplate("/tmp/test_ch8.pdf")
    doc.build(build_autonomous_loops_chapter())
    print("Chapter 8 built successfully!")
