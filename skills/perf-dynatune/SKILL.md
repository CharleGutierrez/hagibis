---
name: perf-dynatune
version: 0.1.0
tier: System / Hardware Optimization
author: CaldoAI Untangling Architect
description: Bare-metal hardware governor controller, thermal telemetry sentry, and Multi-Gen LRU (MGLRU) dynamic memory tuner for Hagibis.
triggers:
  - "perf-dynatune"
  - "cpu-governor"
  - "thermal-check"
  - "mglru-tune"
  - "pgo-hardware-ramp"
---

# 🚀 Skill: Hardware Telemetry, Governor Profiles & MGLRU Dynamic Tuning

## 1. Overview
The `perf-dynatune` skill bridges high-level AI workloads (PGO training, LLVM BOLT optimizations, multi-agent swarms) with bare-metal Linux kernel performance governors. It guarantees maximum sustained CPU frequency during compute bursts while enforcing strict thermal ceilings (≤ 78°C) and tuning Linux Multi-Gen LRU (MGLRU) to protect active agent working sets against eviction.

## 2. Kernel Governance & Telemetry Matrix

| Profile | CPU Scaling Governor | Energy Perf Pref (EPP) | Intel Turbo Boost | MGLRU `min_ttl_ms` | Use Case |
|---------|----------------------|------------------------|-------------------|--------------------|----------|
| `performance` | `performance` | `performance` | Enabled (`0`) | `1000` | PGO compilation, BOLT layout, benchmark passes |
| `balanced` | `powersave` | `balance_performance` | Adaptive | `0` | Default interactive coding and background daemons |
| `powersave` | `powersave` | `balance_power` | Disabled (`1`) | `0` | Battery preservation, idle monitoring, thermal cooldown |

### Key Tuning Subsystems:
1. **MGLRU Memory Protection**:
   - Written to `/sys/kernel/mm/lru_gen/min_ttl_ms`.
   - During heavy compilation or embedding calculation, setting `min_ttl_ms=1000` instructs the kernel memory reclaimer to protect pages accessed within the last second, eliminating thrashing between rustc and LLM runtimes.
2. **Thermal Headroom Sentinel**:
   - Monitored at `/sys/class/thermal/thermal_zone*/temp`.
   - If die temperature reaches ≥ 78°C (78,000 m°C), dynatune automatically issues an exponential backoff or down-throttles the EPP to prevent hardware degradation.
3. **Bridge to Host Control**:
   - Integrates directly with `scripts/cpu-governor.sh` and `/home/toshiba/My AI Project/kernel-optimizer/dynabook-kernel-ctl`.

## 3. WebAssembly Interface Types (WIT)
The capability contract in `wit/dynatune.wit` exposes safe, typed governor manipulation to sandboxed orchestration tools:
- `get-telemetry`: Harvest real-time temperatures, frequencies, and governor states.
- `get-ebpf-kernel-telemetry`: Access microsecond scheduler latency, PSI full memory stalls, slab reclaimable memory, and socket exhaustion metrics.
- `set-profile`: Safely switch between `performance`, `balanced`, and `powersave`.
- `tune-mglru`: Adjust working set retention TTL.
- `check-thermal-headroom`: Guard pre-flight conditions before launching expensive benchmarks.

## 4. Usage Example
```bash
# Direct shell hook invocation
bash scripts/cpu-governor.sh performance
bash scripts/cpu-governor.sh status
bash scripts/cpu-governor.sh balanced
```

## 5. Universal eBPF Kernel Observability & Autonomic Self-Healing
Hagibis integrates a real-time Linux eBPF probe loader (`EbpfKernelProbeLoader` in `hgb-core` and `EbpfAutonomicHealer` in `hgb-daemon`):
- **Tracepoints Attached**: `raw_syscalls:sys_enter`, `raw_syscalls:sys_exit`, `exceptions:page_fault_user`, `thermal:thermal_temperature`, `sched:sched_switch`, `sched:sched_stat_wait`, `sock:inet_sock_set_state`, `tcp:tcp_drop`.
- **Kernel-Level Telemetry Monitoring**:
  - **Scheduler Latency**: Captures microsecond p50/p95/p99 dispatch latency and runqueue depth via `sched_switch` and yield probes.
  - **Memory Pressure**: Continuously monitors Linux PSI full stall (`psi_memory_full_avg10`), cgroup memory pressure, and reclaimable slab caches.
  - **Socket Exhaustion**: Tracks total open sockets, `TCP_TIME_WAIT` pileups, buffer pressure percentage, listen backlog drops, and file descriptor utilization (`file-nr`).
- **Autonomic Action Pipeline**:
  - Automatically shifts CPU governors (`performance` / `powersave`).
  - Dispatches incident triggers to `self-healing-loop`.
  - Preemptively isolates crashing or memory-leaking WASM components into isolated Firecracker microVM sandboxes before requests fail.
  - Throttles misbehaving agent loops under high scheduler runqueue pressure.
  - Drains lingering TCP socket backlogs to preserve edge gateway responsiveness.
