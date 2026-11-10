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

## 5. Native eBPF Kernel Observability & Autonomic Self-Healing
Hagibis integrates a real-time Linux eBPF probe loader (`EbpfKernelProbeLoader` in `hgb-core` and `EbpfAutonomicHealer` in `hgb-daemon`):
- **Tracepoints Attached**: `raw_syscalls:sys_enter`, `raw_syscalls:sys_exit`, `exceptions:page_fault_user`, `thermal:thermal_temperature`.
- **Microsecond Anomaly Detection**: Intercepts memory thrashing (PSI pressure > 40% / 50k page faults/sec), thermal throttles (> 85°C), and deadlocks (blocked syscalls > 30ms).
- **Autonomic Action**: Automatically actuates `/kernel-optimizer/plugins/cpu-governor.sh` or `dynabook-kernel-ctl`, dispatches alerts to `self-healing-loop`, and evicts runaway or thrashing tasks into isolated Firecracker microVM sandboxes with zero human intervention.
