#!/usr/bin/env python3
"""
Test Suite for Hagibis Reverse Engineering Pipeline, Continuous PGO/BOLT,
and OpenTelemetry Circuit Breaker Governance.
"""

import os
import sys
import json
import shutil
import tempfile
import unittest
import subprocess

WORKSPACE_ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", ".."))
WORKFLOW_PATH = os.path.join(WORKSPACE_ROOT, ".github", "workflows", "reverse-engineer-anything.yml")
PIPELINE_SCRIPT = os.path.join(WORKSPACE_ROOT, "scripts", "reverse-engineer-pipeline.sh")
CB_GOVERNOR_SCRIPT = os.path.join(WORKSPACE_ROOT, "scripts", "circuit-breaker-governor.sh")


class TestReverseEngineeringPipeline(unittest.TestCase):
    def setUp(self):
        self.test_dir = tempfile.mkdtemp(prefix="hgb_re_test_")
        self.otel_sink = os.path.join(self.test_dir, "otel_events.jsonl")
        self.cb_registry = os.path.join(self.test_dir, "cb_reg.json")
        self.webhook_sink = os.path.join(self.test_dir, "webhook_sink.jsonl")
        self.env = os.environ.copy()
        self.env["HGB_OTEL_SINK"] = self.otel_sink
        self.env["HGB_CB_REGISTRY"] = self.cb_registry
        self.env["HGB_WEBHOOK_SINK"] = self.webhook_sink
        self.env["HGB_STATE_DIR"] = os.path.join(self.test_dir, "state")
        self.env["LIBRARY_PATH"] = f"/home/toshiba/lib:{self.env.get('LIBRARY_PATH', '')}"
        self.env["CARGO_TARGET_DIR"] = "/home/toshiba/.cargo-target-hagibis"

    def tearDown(self):
        shutil.rmtree(self.test_dir, ignore_errors=True)

    def test_workflow_yaml_syntax_and_specification(self):
        """Validates the YAML structure and all 9 required pipeline stages in the GitHub Actions workflow."""
        wf_path = WORKFLOW_PATH if os.path.exists(WORKFLOW_PATH) else os.path.join(WORKSPACE_ROOT, "templates", "reverse-engineer-anything.yml")
        self.assertTrue(os.path.exists(wf_path), f"Workflow file not found at {wf_path}")

        try:
            import yaml
            with open(wf_path, "r") as f:
                wf_data = yaml.safe_load(f)
        except ImportError:
            wf_data = None

        with open(wf_path, "r") as f:
            content = f.read()

        self.assertIn("name:", content)
        self.assertIn("workflow_dispatch:", content)
        self.assertIn("target_binary:", content)
        self.assertIn("output_crate:", content)
        self.assertIn("enable_wasm_sandbox:", content)
        self.assertIn("enable_firecracker:", content)
        self.assertIn("enable_pgo_bolt:", content)
        self.assertIn("max_healing_cycles:", content)

        # Validate all 9 real stages
        self.assertIn("1. Ingest & Checksum Verification", content)
        self.assertIn("2. Decompile & Extract CFG / Emit WIT", content)
        self.assertIn("3. Lift into safe WASM component", content)
        self.assertIn("4. Autonomous Compiler Healing", content)
        self.assertIn("5. Sandboxed Dynamic Tracing", content)
        self.assertIn("6. Semantic Vector Indexing", content)
        self.assertIn("7. Continuous PGO + BOLT", content)
        self.assertIn("8. OpenTelemetry Telemetry Bridge", content)
        self.assertIn("9. Artifact packaging and consensus receipt", content)

    def test_pipeline_script_exists_and_executable(self):
        """Validates that scripts/reverse-engineer-pipeline.sh exists and is syntax-checked."""
        self.assertTrue(os.path.exists(PIPELINE_SCRIPT), f"Pipeline script not found: {PIPELINE_SCRIPT}")
        res = subprocess.run(["bash", "-n", PIPELINE_SCRIPT], capture_output=True, text=True)
        self.assertEqual(res.returncode, 0, f"Bash syntax check failed: {res.stderr}")

    def test_cli_help(self):
        """Validates that CLI responds to help commands."""
        res = subprocess.run(["bash", PIPELINE_SCRIPT, "help"], capture_output=True, text=True, env=self.env)
        self.assertEqual(res.returncode, 0)
        self.assertIn("Hagibis Autonomous Reverse Engineering", res.stdout)
        self.assertIn("ingest", res.stdout)
        self.assertIn("lift", res.stdout)
        self.assertIn("heal", res.stdout)
        self.assertIn("pgo-tune", res.stdout)
        self.assertIn("run-all", res.stdout)

    def test_ingest_and_checksum_verification(self):
        """Tests ingestion of a binary payload: extracts hashes, arch, symbols, and manifest."""
        # Create a mock ELF header binary
        sample_bin = os.path.join(self.test_dir, "sample.bin")
        # ELF 64-bit little endian x86_64 header magic
        elf_header = bytearray([0x7f, 0x45, 0x4c, 0x46, 2, 1, 1, 0] + [0]*10 + [0x3e, 0x00] + [0]*44)
        with open(sample_bin, "wb") as f:
            f.write(elf_header)

        res = subprocess.run(["bash", PIPELINE_SCRIPT, "ingest", sample_bin], capture_output=True, text=True, env=self.env)
        self.assertEqual(res.returncode, 0, f"Ingest failed: {res.stderr}\n{res.stdout}")

        manifest_path = os.path.join(self.test_dir, "ingest_manifest.json")
        self.assertTrue(os.path.exists(manifest_path), "Manifest was not generated")

        with open(manifest_path, "r") as f:
            data = json.load(f)

        self.assertEqual(data["binary_path"], sample_bin)
        self.assertEqual(data["metadata"]["format"], "ELF")
        self.assertEqual(data["metadata"]["arch"], "x86_64")
        self.assertTrue(len(data["sha256"]) == 64)
        self.assertTrue("blake3" in data)

    def test_lift_synthesizes_wit_and_rust_crate(self):
        """Tests lifting binary to safe Rust crate and WIT interface specification."""
        sample_bin = os.path.join(self.test_dir, "target.bin")
        with open(sample_bin, "wb") as f:
            f.write(b"\x7fELF" + b"\x00" * 60)

        output_crate_dir = os.path.join(self.test_dir, "reconstructed_crate")
        res = subprocess.run(["bash", PIPELINE_SCRIPT, "lift", sample_bin, output_crate_dir],
                             capture_output=True, text=True, env=self.env)
        self.assertEqual(res.returncode, 0, f"Lifting failed: {res.stderr}\n{res.stdout}")

        # Check WIT interface
        wit_file = os.path.join(output_crate_dir, "wit", "world.wit")
        self.assertTrue(os.path.exists(wit_file), "WIT interface file not created")
        with open(wit_file) as f:
            wit_content = f.read()
        self.assertIn("interface target-api", wit_content)
        self.assertIn("world reconstructed-target", wit_content)

        # Check Rust crate
        cargo_file = os.path.join(output_crate_dir, "Cargo.toml")
        lib_file = os.path.join(output_crate_dir, "src", "lib.rs")
        cfg_file = os.path.join(output_crate_dir, "cfg_graph.json")

        self.assertTrue(os.path.exists(cargo_file), "Cargo.toml not created")
        self.assertTrue(os.path.exists(lib_file), "src/lib.rs not created")
        self.assertTrue(os.path.exists(cfg_file), "cfg_graph.json not created")

        with open(lib_file) as f:
            lib_content = f.read()
        self.assertIn("pub fn execute_kernel", lib_content)
        self.assertIn("pub fn verify_invariants", lib_content)

        with open(cfg_file) as f:
            cfg_data = json.load(f)
        self.assertTrue(cfg_data["total_basic_blocks"] > 0)
        self.assertTrue(cfg_data["cyclomatic_complexity"] >= 1)

    def test_opentelemetry_formatting_and_state_persistence(self):
        """Validates OpenTelemetry event formatting, trace persistence, and schema."""
        sample_bin = os.path.join(self.test_dir, "otel_probe.bin")
        with open(sample_bin, "wb") as f:
            f.write(b"\x00asm\x01\x00\x00\x00") # WASM binary magic

        subprocess.run(["bash", PIPELINE_SCRIPT, "ingest", sample_bin],
                       capture_output=True, text=True, env=self.env)

        self.assertTrue(os.path.exists(self.otel_sink), "OTel sink file was not created")

        events = []
        with open(self.otel_sink, "r") as f:
            for line in f:
                if line.strip():
                    events.append(json.loads(line))

        self.assertTrue(len(events) > 0, "No OTel events logged")
        last_event = events[-1]

        # Verify OTel fields
        self.assertIn("timestamp", last_event)
        self.assertIn("timestamp_nano", last_event)
        self.assertIn("trace_id", last_event)
        self.assertIn("span_id", last_event)
        self.assertIn("service", last_event)
        self.assertEqual(last_event["service"], "hgb-reverse-engineer-pipeline")
        self.assertEqual(last_event["name"], "reverse_engineer.ingest")
        self.assertEqual(last_event["status"], "OK")
        self.assertEqual(len(last_event["trace_id"]), 32)
        self.assertEqual(len(last_event["span_id"]), 16)
        self.assertIn("binary", last_event["attributes"])

    def test_circuit_breaker_trips_on_three_consecutive_failures(self):
        """Tests that 3 consecutive failures trigger the circuit breaker, quarantine payload, and lock permissions."""
        broken_crate_dir = os.path.join(self.test_dir, "broken_target")
        os.makedirs(os.path.join(broken_crate_dir, "src"), exist_ok=True)
        with open(os.path.join(broken_crate_dir, "Cargo.toml"), "w") as f:
            f.write('[package]\nname = "broken_target"\nversion = "0.1.0"\nedition = "2021"\n')

        # Intentionally un-healable invalid syntax
        with open(os.path.join(broken_crate_dir, "src", "lib.rs"), "w") as f:
            f.write("SYNTAX_ERROR_CANNOT_PARSE !@#$%^&*()_+\n")

        # Cycle 1 failure
        res1 = subprocess.run(["bash", PIPELINE_SCRIPT, "heal", broken_crate_dir],
                              capture_output=True, text=True, env=self.env)
        self.assertNotEqual(res1.returncode, 0)

        # Cycle 2 failure
        res2 = subprocess.run(["bash", PIPELINE_SCRIPT, "heal", broken_crate_dir],
                              capture_output=True, text=True, env=self.env)
        self.assertNotEqual(res2.returncode, 0)

        # Cycle 3 failure -> Must Trip Circuit Breaker!
        res3 = subprocess.run(["bash", PIPELINE_SCRIPT, "heal", broken_crate_dir],
                              capture_output=True, text=True, env=self.env)
        self.assertNotEqual(res3.returncode, 0)

        # Check circuit breaker registry
        with open(self.cb_registry, "r") as f:
            cb_data = json.load(f)

        target_name = "broken_target"
        skill_entry = cb_data.get("skills", {}).get(target_name, {})
        self.assertIn(skill_entry.get("status"), ["Tripped", "Quarantined"],
                      f"Circuit breaker should be Tripped/Quarantined: {skill_entry}")
        self.assertGreaterEqual(skill_entry.get("consecutive_failures", 0), 3)

        # Check that write permissions were locked (read-only)
        self.assertFalse(os.access(os.path.join(broken_crate_dir, "src", "lib.rs"), os.W_OK),
                         "Write permissions were not revoked on failing target")

        # Subsequent attempts must immediately be blocked
        res_blocked = subprocess.run(["bash", PIPELINE_SCRIPT, "heal", broken_crate_dir],
                                     capture_output=True, text=True, env=self.env)
        self.assertNotEqual(res_blocked.returncode, 0)
        self.assertIn("Circuit Breaker", res_blocked.stderr)


if __name__ == "__main__":
    unittest.main()
