#!/usr/bin/env python3
"""
test_pid_adaptive_hysteresis_governor.py - Unit & Integration Test Suite for Dynamic Adaptive PID Hysteresis
==============================================================================================================
Project Hagibis: Roadmap Item 2 (Dynamic Adaptive Hysteresis for Circuit Breakers)

Validates:
  1. Baseline Nominal Error Rate:
     When error rate <= setpoint and system load is zero/nominal, returns baseline cooling window T_0.
  2. Error Frequency Surge (e(t) > 0.4):
     A sudden surge in error rate activates PID proportional, integral, and derivative terms,
     scaling the cooling period by > 3x.
  3. High System Load (L > 0.8) + Error Burst:
     Compounding of system load factor (1 + beta * L_sys) with PID error scaling and flapping penalty,
     dynamically extending cooldown and suppressing flapping during heavy chaotic load duress.
  4. Quiescent Settling:
     When error frequency subsides and system load normalizes, the cooling period decays smoothly
     and monotonically back to the quiescent baseline T_0.
  5. CLI & Governor Integration:
     CLI commands support --adaptive-cooldown and return structured JSON with PID terms
     (P, I, D, load_factor, flapping_factor, computed_cooling_seconds).
  6. Telemetry Governor Bridge Integration:
     Incorporating worker thread concurrency, queue depth, and P99 latency telemetry into cooling calculations.
"""

import os
import sys
import time
import json
import unittest
import subprocess
from pathlib import Path

# Ensure observability-otel module is in sys.path
SCRIPT_DIR = Path(__file__).resolve().parent
if str(SCRIPT_DIR) not in sys.path:
    sys.path.insert(0, str(SCRIPT_DIR))

from ch8_circuit_breaker_governor import (
    AdaptivePidCircuitBreaker,
    AdaptiveCoolingResult,
    PidController,
    AutonomicCircuitBreakerGovernor,
    CircuitState,
    get_circuit_breaker_governor,
)


class TestPidAdaptiveHysteresisGovernor(unittest.TestCase):
    """Test suite validating Roadmap Item 2: Dynamic Adaptive Hysteresis."""

    def setUp(self):
        self.t0 = 1.0
        self.t_min = 1.0
        self.t_max = 64.0
        self.kp = 4.0
        self.ki = 0.8
        self.kd = 2.0
        self.setpoint = 0.05
        self.beta = 1.0

        self.cb = AdaptivePidCircuitBreaker(
            skill_id="test_adaptive_skill",
            t0=self.t0,
            t_min=self.t_min,
            t_max=self.t_max,
            kp=self.kp,
            ki=self.ki,
            kd=self.kd,
            setpoint=self.setpoint,
            beta=self.beta
        )

    def test_01_baseline_nominal_error_rate_returns_baseline_cooling(self):
        """
        Test 1: Baseline nominal error rate -> returns baseline cooling window.
        When error rate <= setpoint (e.g. 0.02 or 0.05) and load is nominal (0.0),
        u_PID <= 0, PID factor = 1.0, Load factor = 1.0, Flapping factor = 1.0.
        Returns baseline cooling window T_0.
        """
        # Scenario 1A: Error rate zero
        cool_zero = self.cb.compute_dynamic_cooling_period(
            error_rate=0.0,
            system_load=0.0,
            flapping_factor=1.0,
            dt=1.0
        )
        self.assertIsInstance(cool_zero, float)
        self.assertAlmostEqual(float(cool_zero), self.t0, places=2)
        self.assertEqual(float(cool_zero), 1.0)
        self.assertAlmostEqual(cool_zero.load_factor, 1.0, places=2)
        self.assertAlmostEqual(cool_zero.flapping_factor, 1.0, places=2)
        self.assertLessEqual(cool_zero.u_pid, 0.0)

        # Scenario 1B: Nominal non-zero error rate exactly at setpoint (0.05)
        self.cb.reset()
        cool_setpoint = self.cb.compute_dynamic_cooling_period(
            error_rate=self.setpoint,
            system_load=0.0,
            flapping_factor=1.0,
            dt=1.0
        )
        self.assertAlmostEqual(float(cool_setpoint), self.t0, places=2)
        self.assertEqual(float(cool_setpoint), 1.0)
        self.assertAlmostEqual(cool_setpoint.error_deviation, 0.0, places=3)

        # Scenario 1C: Small nominal error rate below setpoint (0.02)
        self.cb.reset()
        cool_sub = self.cb.compute_dynamic_cooling_period(
            error_rate=0.02,
            system_load=0.0,
            flapping_factor=1.0,
            dt=1.0
        )
        self.assertAlmostEqual(float(cool_sub), self.t0, places=2)
        self.assertEqual(float(cool_sub), 1.0)

    def test_02_error_frequency_surge_scales_cooling_window_over_3x(self):
        """
        Test 2: Error frequency surge (e(t) > 0.4) -> PID terms scale cooling window by > 3x.
        A sudden jump in error rate (e.g., from 0.0 to 0.50, deviation e(t) = 0.45 > 0.4)
        triggers large proportional (P) and derivative (D) kicks, producing u_PID > 2.0.
        PID factor = (1 + max(0, u_PID)) > 3.0.
        Cooling window T_cool scales to > 3x baseline T_0.
        """
        # Baseline check before surge
        self.cb.reset()
        cool_base = self.cb.compute_dynamic_cooling_period(error_rate=0.0, system_load=0.0, dt=1.0)
        self.assertEqual(float(cool_base), self.t0)

        # Error surge: jump to 0.50 error rate (e(t) = 0.50 - 0.05 = 0.45 > 0.4)
        cool_surge = self.cb.compute_dynamic_cooling_period(
            error_rate=0.50,
            system_load=0.0,
            flapping_factor=1.0,
            dt=1.0
        )

        # Verify deviation e(t) > 0.4
        self.assertGreater(cool_surge.error_deviation, 0.4)
        self.assertAlmostEqual(cool_surge.error_deviation, 0.45, places=3)

        # Verify individual PID terms
        self.assertGreater(cool_surge.p_term, 1.5)  # Kp * 0.45 = 1.8
        self.assertGreater(cool_surge.d_term, 0.5)  # Kd * 0.45 / 1.0 = 0.9
        self.assertGreater(cool_surge.i_term, 0.0)  # Ki * 0.45 = 0.36
        self.assertGreater(cool_surge.u_pid, 2.0)   # Total u_pid = 3.06 > 2.0

        # Verify PID terms scale cooling window by > 3x
        pid_scaling_factor = 1.0 + max(0.0, cool_surge.u_pid)
        self.assertGreater(pid_scaling_factor, 3.0)
        self.assertGreater(float(cool_surge), 3.0 * self.t0)

    def test_03_high_system_load_and_error_burst_compounding_prevents_flapping(self):
        """
        Test 3: High system load (L > 0.8) + error burst -> dynamic compounding prevents flapping.
        When chaotic high load (L_sys = 0.85 > 0.8) coincides with error burst (error_rate = 0.50):
          T_cool = T_0 * (1 + u_PID) * (1 + beta * L_sys) * gamma_flap
        The load multiplier compounds with the PID error factor, dramatically lengthening cooldown
        (e.g. 7.5s vs 4.06s vs 1.0s) and suppressing flapping oscillations.
        """
        # Surge under zero load
        cool_surge_only = self.cb.compute_dynamic_cooling_period(
            error_rate=0.50,
            system_load=0.0,
            flapping_factor=1.0,
            dt=1.0
        )

        # Reset for clean comparison
        self.cb.reset()

        # Surge under high load (L = 0.85 > 0.8)
        cool_compounded = self.cb.compute_dynamic_cooling_period(
            error_rate=0.50,
            system_load=0.85,
            flapping_factor=1.0,
            dt=1.0
        )

        # Verify load factor
        self.assertGreater(cool_compounded.system_load, 0.8)
        self.assertAlmostEqual(cool_compounded.system_load, 0.85, places=3)
        self.assertAlmostEqual(cool_compounded.load_factor, 1.85, places=3)

        # Verify compounding: T_compounded > T_surge_only
        self.assertGreater(float(cool_compounded), float(cool_surge_only))
        self.assertGreaterEqual(float(cool_compounded), 5.0 * self.t0)
        self.assertAlmostEqual(float(cool_compounded), 7.511, places=2)

        # Verify compounding with flapping penalty (e.g. gamma_flap = 1.5)
        self.cb.reset()
        cool_with_flapping = self.cb.compute_dynamic_cooling_period(
            error_rate=0.50,
            system_load=0.85,
            flapping_factor=1.5,
            dt=1.0
        )
        self.assertGreater(float(cool_with_flapping), float(cool_compounded))
        self.assertAlmostEqual(float(cool_with_flapping), 11.266, places=2)

    def test_04_quiescent_settling_decays_smoothly_back_to_t0(self):
        """
        Test 4: Quiescent settling -> cooling period decays smoothly back to T_0.
        Following an error burst and high load spike, as error rate drops back to quiescent nominal
        (0.0) and system load clears (0.0), PID control output and integral error decay smoothly,
        and T_cool monotonically relaxes back to baseline T_0.
        """
        # Step 0: Prime breaker with an error burst
        self.cb.reset()
        cool_burst = self.cb.compute_dynamic_cooling_period(
            error_rate=0.50,
            system_load=0.85,
            dt=1.0
        )
        self.assertGreater(float(cool_burst), 7.0)

        # Settling progression: error and load gradually subside to quiescent nominal
        settling_trajectory = [
            (0.35, 0.50),
            (0.20, 0.25),
            (0.10, 0.10),
            (0.05, 0.00),
            (0.00, 0.00),
        ]
        cooling_history = [float(cool_burst)]
        for err, load in settling_trajectory:
            cool_t = self.cb.compute_dynamic_cooling_period(
                error_rate=err,
                system_load=load,
                flapping_factor=1.0,
                dt=1.0
            )
            cooling_history.append(float(cool_t))

        # Verify monotonic non-increasing decay
        for i in range(len(cooling_history) - 1):
            self.assertGreaterEqual(
                cooling_history[i],
                cooling_history[i + 1],
                f"Cooling did not decay monotonically: {cooling_history}"
            )

        # Verify that final settled cooldown equals baseline T_0 exactly
        final_cooling = cooling_history[-1]
        self.assertAlmostEqual(final_cooling, self.t0, places=2)
        self.assertEqual(final_cooling, 1.0)

    def test_05_cli_adaptive_cooldown_json_output(self):
        """
        Verify CLI command --adaptive-cooldown outputs valid JSON with all required PID diagnostic fields:
        P, I, D, p_term, i_term, d_term, load_factor, flapping_factor, computed_cooling_seconds.
        """
        script_path = str(SCRIPT_DIR / "ch8_circuit_breaker_governor.py")
        cmd = [
            sys.executable,
            script_path,
            "adaptive-cooldown",
            "cli_test_skill",
            "--error-rate", "0.50",
            "--load", "0.85",
            "--flapping-factor", "1.2",
            "--t0", "1.0",
            "--tmax", "64.0"
        ]
        proc = subprocess.run(cmd, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, check=True)
        data = json.loads(proc.stdout)

        # Validate presence and correctness of all required telemetry fields
        for field in ["P", "I", "D", "p_term", "i_term", "d_term", "load_factor", "flapping_factor", "computed_cooling_seconds"]:
            self.assertIn(field, data, f"Missing required JSON diagnostic field: {field}")

        self.assertEqual(data["skill_id"], "cli_test_skill")
        self.assertEqual(data["error_rate"], 0.5)
        self.assertEqual(data["setpoint"], 0.05)
        self.assertAlmostEqual(data["load_factor"], 1.85, places=2)
        self.assertAlmostEqual(data["flapping_factor"], 1.2, places=2)
        self.assertGreaterEqual(data["computed_cooling_seconds"], 8.0)

    def test_06_telemetry_bridge_metrics_integration(self):
        """
        Verify that active worker count, queue depth, and P99 latency telemetry
        are correctly converted into system load and scale the adaptive cooling window.
        """
        # Nominal telemetry (workers=4, queue=0, p99=20ms) -> load = 0.0 -> returns T0
        cool_nominal = self.cb.compute_dynamic_cooling_period(
            error_rate=0.0,
            active_workers=4,
            queue_depth=0,
            p99_latency_ms=20.0,
            dt=1.0
        )
        self.assertEqual(float(cool_nominal), 1.0)
        self.assertAlmostEqual(cool_nominal.system_load, 0.0, places=2)

        # Heavily loaded telemetry: boosted workers=8, queue=10, p99=200ms
        self.cb.reset()
        cool_heavy = self.cb.compute_dynamic_cooling_period(
            error_rate=0.50,
            active_workers=8,
            queue_depth=10,
            p99_latency_ms=200.0,
            dt=1.0
        )
        self.assertGreater(cool_heavy.system_load, 0.8)
        self.assertGreaterEqual(cool_heavy.load_factor, 1.8)
        self.assertGreaterEqual(float(cool_heavy), 5.0)

    def test_07_governor_adaptive_state_machine_transition(self):
        """
        Verify that AutonomicCircuitBreakerGovernor uses dynamic adaptive hysteresis
        when tripping circuit states and setting open_until_timestamp.
        """
        gov = get_circuit_breaker_governor()
        skill = "governor_adaptive_test_skill"

        # Record successive failures to trigger trip
        now = time.time()
        for i in range(3):
            res = gov.record_mutation_attempt(
                skill_id=skill,
                pre_hash=f"pre_{i}",
                post_hash=f"post_{i}",
                success=False,
                error_trace=f"Compilation fault attempt {i}"
            )

        rec = gov._registry.get(skill)
        self.assertIsNotNone(rec)
        self.assertIn(rec.state, (CircuitState.OPEN, CircuitState.LOCAL_OPEN, CircuitState.GLOBAL_OPEN, CircuitState.PERM_TRIPPED, CircuitState.PERM_TRIPPED_CRYPTOLOCKED))
        # Verify open_until_timestamp is in the future
        self.assertGreater(rec.open_until_timestamp, now)


if __name__ == "__main__":
    unittest.main()
