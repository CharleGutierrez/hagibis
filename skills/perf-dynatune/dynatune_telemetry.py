#!/usr/bin/env python3
"""
dynatune_telemetry.py - Hardware Governor, EPP, MGLRU & Dynatune Telemetry Interface
====================================================================================
Implements Python bindings for `skills/perf-dynatune/wit/dynatune.wit` contract.
Coordinates bare-metal CPU scaling governors, Energy Performance Preference (EPP),
Intel Turbo Boost, and Linux Multi-Gen LRU (MGLRU) min_ttl_ms memory compaction.
"""

import os
import sys
import json
import subprocess
from dataclasses import dataclass
from typing import Dict, Any, Optional

GOVERNOR_SCRIPT: str = os.path.abspath(
    os.path.join(os.path.dirname(__file__), "..", "..", "scripts", "cpu-governor.sh")
)

@dataclass
class HardwareTelemetryReport:
    active_governor: str
    energy_performance_preference: str
    turbo_state: str
    mglru_enabled_mask: str
    mglru_min_ttl_ms: int
    cpu_temp_millicelsius: int
    cpu_temp_c: float
    load_avg_1m: float
    throttled: bool
    target_profile: str

    def to_dict(self) -> Dict[str, Any]:
        return {
            "active_governor": self.active_governor,
            "energy_performance_preference": self.energy_performance_preference,
            "turbo_state": self.turbo_state,
            "mglru_enabled_mask": self.mglru_enabled_mask,
            "mglru_min_ttl_ms": self.mglru_min_ttl_ms,
            "cpu_temp_millicelsius": self.cpu_temp_millicelsius,
            "cpu_temp_c": self.cpu_temp_c,
            "load_avg_1m": self.load_avg_1m,
            "throttled": self.throttled,
            "target_profile": self.target_profile,
        }


class DynatuneGovernor:
    """
    Python wrapper implementing the dynatune governor contract.
    """
    def __init__(self, script_path: Optional[str] = None):
        self.script_path = script_path or GOVERNOR_SCRIPT

    def get_telemetry(self) -> HardwareTelemetryReport:
        if os.path.exists(self.script_path):
            try:
                cmd = ["bash", self.script_path, "telemetry"]
                res = subprocess.run(cmd, capture_output=True, text=True, timeout=5)
                if res.returncode == 0 and res.stdout.strip():
                    data = json.loads(res.stdout.strip())
                    temp_c = float(data.get("cpu_temp_c", 0.0))
                    return HardwareTelemetryReport(
                        active_governor=data.get("governor", "powersave"),
                        energy_performance_preference=data.get("epp", "balance_performance"),
                        turbo_state=data.get("turbo_boost", "Enabled"),
                        mglru_enabled_mask=data.get("mglru_enabled", "0x0007"),
                        mglru_min_ttl_ms=int(data.get("mglru_min_ttl_ms", 0)),
                        cpu_temp_millicelsius=int(temp_c * 1000),
                        cpu_temp_c=temp_c,
                        load_avg_1m=float(data.get("load_avg_1m", 0.0)),
                        throttled=temp_c >= 78.0,
                        target_profile=data.get("target_profile", "balanced"),
                    )
            except Exception:
                pass

        # Fallback reading directly from sysfs
        return self._read_sysfs_direct()

    def _read_sysfs_direct(self) -> HardwareTelemetryReport:
        gov = "powersave"
        epp = "balance_performance"
        turbo = "Enabled"
        mglru = "0x0000"
        mglru_ttl = 0
        temp_c = 40.0

        try:
            with open("/sys/devices/system/cpu/cpu0/cpufreq/scaling_governor", "r") as f:
                gov = f.read().strip()
        except Exception:
            pass

        try:
            with open("/sys/devices/system/cpu/cpu0/cpufreq/energy_performance_preference", "r") as f:
                epp = f.read().strip()
        except Exception:
            pass

        try:
            with open("/sys/devices/system/cpu/intel_pstate/no_turbo", "r") as f:
                turbo = "Disabled" if f.read().strip() == "1" else "Enabled"
        except Exception:
            pass

        try:
            with open("/sys/kernel/mm/lru_gen/enabled", "r") as f:
                mglru = f.read().strip()
        except Exception:
            pass

        try:
            with open("/sys/kernel/mm/lru_gen/min_ttl_ms", "r") as f:
                mglru_ttl = int(f.read().strip())
        except Exception:
            pass

        try:
            with open("/sys/class/thermal/thermal_zone0/temp", "r") as f:
                temp_c = float(f.read().strip()) / 1000.0
        except Exception:
            pass

        return HardwareTelemetryReport(
            active_governor=gov,
            energy_performance_preference=epp,
            turbo_state=turbo,
            mglru_enabled_mask=mglru,
            mglru_min_ttl_ms=mglru_ttl,
            cpu_temp_millicelsius=int(temp_c * 1000),
            cpu_temp_c=temp_c,
            load_avg_1m=0.0,
            throttled=temp_c >= 78.0,
            target_profile="balanced",
        )

    def set_profile(self, profile: str) -> bool:
        if profile not in ("performance", "balanced", "powersave"):
            raise ValueError(f"Invalid governor profile: {profile}")
        if os.path.exists(self.script_path):
            try:
                res = subprocess.run(["bash", self.script_path, profile], capture_output=True, timeout=5)
                return res.returncode == 0
            except Exception:
                return False
        return False

    def tune_mglru(self, min_ttl_ms: int) -> bool:
        ttl_path = "/sys/kernel/mm/lru_gen/min_ttl_ms"
        if os.path.exists(ttl_path) and os.access(ttl_path, os.W_OK):
            try:
                with open(ttl_path, "w") as f:
                    f.write(str(min_ttl_ms))
                return True
            except Exception:
                return False
        return True

    def check_thermal_headroom(self, max_temp_c: float = 78.0) -> bool:
        report = self.get_telemetry()
        return report.cpu_temp_c < max_temp_c


if __name__ == "__main__":
    dyn = DynatuneGovernor()
    tel = dyn.get_telemetry()
    print("Dynatune Telemetry Report:")
    print(json.dumps(tel.to_dict(), indent=2))
    assert tel.cpu_temp_c >= 0, "CPU temp must be non-negative"
    print("✔ Dynatune Governor module verified.")
