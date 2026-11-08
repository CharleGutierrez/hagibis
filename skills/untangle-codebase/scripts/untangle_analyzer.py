#!/usr/bin/env python3
"""
untangle_analyzer.py - Comprehensive Static Architecture & Layer Governance Analyzer
===================================================================================
Part of CaldoAI Skill: untangle-codebase

Checks:
  1. Workspace crate dependencies and 5-tier layer ordering:
     Core (Tier 1) -> Storage/Kernel (Tier 2) -> Nextgen (Tier 3) -> Daemon (Tier 4) -> CLI (Tier 5)
  2. Circular dependencies across workspace crates (DAG verification).
  3. Leaky abstractions (e.g. raw SQL/DB direct access outside hgb-storage).
  4. Dead or unabsorbed patch files (*.patch, *.diff) lingering in the workspace.
  5. WIT (WebAssembly Interface Types) syntax and MCP (Model Context Protocol) schema integrity.
"""

import os
import sys
import json
import re
from pathlib import Path
from typing import Dict, List, Set, Tuple, Optional, Any

# ANSI Colors
COLOR_RESET = "\033[0m"
COLOR_RED = "\033[31m"
COLOR_GREEN = "\033[32m"
COLOR_YELLOW = "\033[33m"
COLOR_BLUE = "\033[34m"
COLOR_MAGENTA = "\033[35m"
COLOR_CYAN = "\033[36m"
COLOR_BOLD = "\033[1m"

LAYER_TIERS = {
    "wat": 1,
    "hgb-wasm-kernel": 1,
    "hgb-core": 1,
    "hgb-storage": 2,
    "hgb-nextgen": 3,
    "hgb-daemon": 4,
    "hgb-cli": 5,
}

LAYER_NAMES = {
    1: "Foundation Core & Wasm Kernel SDK",
    2: "Storage & Persistence",
    3: "Nextgen Swarm Orchestration",
    4: "Daemon Runtime Service",
    5: "CLI User Interface",
}


class UntangleAnalyzer:
    def __init__(self, workspace_root: Path, strict: bool = False):
        self.root = workspace_root.resolve()
        self.strict = strict
        self.violations: List[Dict[str, Any]] = []
        self.warnings: List[Dict[str, Any]] = []
        self.passed_checks: List[str] = []
        self.crate_deps: Dict[str, Set[str]] = {}
        self.crate_dev_deps: Dict[str, Set[str]] = {}

    def log_violation(self, code: str, title: str, details: str, file_path: Optional[Path] = None, line: int = 0):
        self.violations.append({
            "code": code,
            "title": title,
            "details": details,
            "file": str(file_path) if file_path else None,
            "line": line,
            "severity": "ERROR"
        })

    def log_warning(self, code: str, title: str, details: str, file_path: Optional[Path] = None, line: int = 0):
        self.warnings.append({
            "code": code,
            "title": title,
            "details": details,
            "file": str(file_path) if file_path else None,
            "line": line,
            "severity": "WARNING"
        })

    def parse_cargo_manifest(self, toml_path: Path) -> Tuple[Optional[str], Set[str], Set[str]]:
        """Parses a Cargo.toml file for package name, direct dependencies, and dev dependencies."""
        if not toml_path.exists():
            return None, set(), set()

        content = toml_path.read_text(encoding="utf-8", errors="replace")
        package_name = None
        deps = set()
        dev_deps = set()

        current_section = None
        in_package = False

        for line in content.splitlines():
            line = line.strip()
            if not line or line.startswith("#"):
                continue

            sec_match = re.match(r"^\[([a-zA-Z0-9_.-]+)\]$", line)
            if sec_match:
                current_section = sec_match.group(1)
                in_package = (current_section == "package")
                continue

            if in_package and line.startswith("name"):
                name_match = re.match(r'name\s*=\s*["\']([^"\']+)["\']', line)
                if name_match:
                    package_name = name_match.group(1)

            if current_section == "dependencies":
                dep_match = re.match(r"^([a-zA-Z0-9_-]+)\s*=", line)
                if dep_match:
                    dep = dep_match.group(1)
                    if dep in LAYER_TIERS:
                        deps.add(dep)

            elif current_section == "dev-dependencies":
                dep_match = re.match(r"^([a-zA-Z0-9_-]+)\s*=", line)
                if dep_match:
                    dep = dep_match.group(1)
                    if dep in LAYER_TIERS:
                        dev_deps.add(dep)

        return package_name, deps, dev_deps

    def check_layer_ordering(self):
        """ARCH-001: Verifies downward-only dependency flow across crates."""
        crates_dir = self.root / "crates"
        if not crates_dir.exists():
            # Workspace might be organized without crates/ dir
            crates_dir = self.root

        found_crates = set()
        for toml_path in crates_dir.glob("**/Cargo.toml"):
            pkg_name, deps, dev_deps = self.parse_cargo_manifest(toml_path)
            if pkg_name and pkg_name in LAYER_TIERS:
                found_crates.add(pkg_name)
                self.crate_deps[pkg_name] = deps
                self.crate_dev_deps[pkg_name] = dev_deps

        if not found_crates:
            self.log_warning("ARCH-001", "No Crate Manifests Found", f"No workspace crates found under {crates_dir}")
            return

        for crate, deps in self.crate_deps.items():
            crate_tier = LAYER_TIERS.get(crate, 99)
            for dep in deps:
                dep_tier = LAYER_TIERS.get(dep, 99)
                # Upward dependency: a lower tier depends on a strictly higher tier
                if crate_tier < dep_tier:
                    self.log_violation(
                        "ARCH-001",
                        "Layer Hierarchy Inversion",
                        f"Crate '{crate}' (Tier {crate_tier}: {LAYER_NAMES.get(crate_tier, 'Unknown')}) depends on higher-tier crate '{dep}' (Tier {dep_tier}: {LAYER_NAMES.get(dep_tier, 'Unknown')}). Production dependencies must flow strictly downward.",
                        crates_dir / crate / "Cargo.toml"
                    )

        self.passed_checks.append("ARCH-001: Layer Ordering Invariant Verification")

    def check_circular_dependencies(self):
        """ARCH-002: Detects circular dependency cycles across workspace crates."""
        visited: Dict[str, int] = {}  # 0 = unvisited, 1 = visiting, 2 = visited
        cycle_detected = False

        def dfs(node: str, path: List[str]):
            nonlocal cycle_detected
            visited[node] = 1
            path.append(node)

            for neighbor in self.crate_deps.get(node, []):
                state = visited.get(neighbor, 0)
                if state == 1:
                    cycle_detected = True
                    cycle_path = " -> ".join(path[path.index(neighbor):] + [neighbor])
                    self.log_violation(
                        "ARCH-002",
                        "Circular Crate Dependency Cycle",
                        f"Detected circular dependency cycle: {cycle_path}",
                        self.root / "Cargo.toml"
                    )
                elif state == 0:
                    dfs(neighbor, path)

            path.pop()
            visited[node] = 2

        for crate in self.crate_deps:
            if visited.get(crate, 0) == 0:
                dfs(crate, [])

        if not cycle_detected:
            self.passed_checks.append("ARCH-002: Strict Acyclic Graph (DAG) Invariant")

    def check_dead_patches(self):
        """ARCH-003: Audits workspace root for unabsorbed/dead patch files."""
        patch_files = list(self.root.glob("*.patch")) + list(self.root.glob("*.diff"))
        if patch_files:
            for p in patch_files:
                self.log_violation(
                    "ARCH-003",
                    "Dead / Unabsorbed Patch File Detected",
                    f"Found lingering patch file '{p.name}'. Patches must be absorbed directly into the crate source tree and deleted.",
                    p
                )
        else:
            self.passed_checks.append("ARCH-003: Zero Dead Patch Files Invariant")

    def check_leaky_abstractions(self):
        """ARCH-004: Checks higher layers for raw database queries or direct storage leakage."""
        cli_src = self.root / "crates" / "hgb-cli" / "src"
        leaky_patterns = [
            (re.compile(r'rusqlite::Connection::open\('), "Direct rusqlite connection opened in CLI (must route via hgb-storage)"),
            (re.compile(r'SELECT\s+[\w\s,*]+\s+FROM\s+\w+', re.IGNORECASE), "Embedded raw SQL query string inside CLI layer"),
        ]

        if cli_src.exists():
            for rs_file in cli_src.rglob("*.rs"):
                # Exclude tests if needed, or scan all
                try:
                    lines = rs_file.read_text(encoding="utf-8", errors="replace").splitlines()
                    for idx, line in enumerate(lines, start=1):
                        for pattern, msg in leaky_patterns:
                            if pattern.search(line) and not line.strip().startswith("//"):
                                self.log_warning("ARCH-004", "Leaky Storage Abstraction", msg, rs_file, idx)
                except Exception as e:
                    self.log_warning("ARCH-004", "Scan Failure", f"Could not read {rs_file}: {e}", rs_file)

        self.passed_checks.append("ARCH-004: Leaky Abstraction & Boundary Cleanliness")

    def check_wit_and_mcp_integrity(self):
        """ARCH-005: Validates WebAssembly Interface Types (.wit) and MCP manifests."""
        # 1. MCP Manifests
        mcp_candidates = [
            self.root / "hagibis.mcp.json",
            self.root / "hagibis.mcp.json.example"
        ]
        mcp_checked = False
        for mcp_path in mcp_candidates:
            if mcp_path.exists():
                mcp_checked = True
                try:
                    data = json.loads(mcp_path.read_text(encoding="utf-8"))
                    if "mcpServers" not in data or not isinstance(data["mcpServers"], dict):
                        self.log_violation(
                            "ARCH-005",
                            "Invalid MCP Manifest Schema",
                            f"'{mcp_path.name}' must contain an 'mcpServers' object dictionary.",
                            mcp_path
                        )
                    else:
                        for srv_name, srv_conf in data["mcpServers"].items():
                            if "command" not in srv_conf and "url" not in srv_conf:
                                self.log_violation(
                                    "ARCH-005",
                                    "Missing Server Command or URL in MCP Manifest",
                                    f"Server '{srv_name}' in '{mcp_path.name}' lacks a required 'command' or 'url' attribute.",
                                    mcp_path
                                )
                except json.JSONDecodeError as e:
                    self.log_violation(
                        "ARCH-005",
                        "Corrupted MCP Manifest JSON",
                        f"Failed parsing '{mcp_path.name}': {e}",
                        mcp_path
                    )

        # 2. WIT Interfaces
        wit_files = list(self.root.rglob("*.wit"))
        for wit_path in wit_files:
            content = wit_path.read_text(encoding="utf-8", errors="replace")
            # Basic WIT grammar checks: package statement, balanced braces
            if not re.search(r"package\s+[\w:-]+(@[\w.-]+)?;", content):
                self.log_violation(
                    "ARCH-005",
                    "Missing WIT Package Declaration",
                    f"'{wit_path.name}' lacks a valid 'package <namespace>:<pkg>@<ver>;' header.",
                    wit_path
                )
            open_braces = content.count("{")
            close_braces = content.count("}")
            if open_braces != close_braces:
                self.log_violation(
                    "ARCH-005",
                    "Unbalanced Braces in WIT File",
                    f"'{wit_path.name}' has {open_braces} open braces and {close_braces} close braces.",
                    wit_path
                )

        self.passed_checks.append("ARCH-005: WIT & MCP Schema Integrity")

    def run_all(self) -> int:
        """Executes full diagnostic suite and returns exit code."""
        self.check_layer_ordering()
        self.check_circular_dependencies()
        self.check_dead_patches()
        self.check_leaky_abstractions()
        self.check_wit_and_mcp_integrity()

        is_failed = len(self.violations) > 0 or (self.strict and len(self.warnings) > 0)
        return 1 if is_failed else 0

    def print_report(self):
        print(f"\n{COLOR_BOLD}{COLOR_CYAN}========================================================================{COLOR_RESET}")
        print(f"{COLOR_BOLD}{COLOR_CYAN} Hagibis CaldoAI Untangle & Architectural Governance Report{COLOR_RESET}")
        print(f"{COLOR_BOLD}{COLOR_CYAN} Workspace: {self.root}{COLOR_RESET}")
        print(f"{COLOR_BOLD}{COLOR_CYAN}========================================================================{COLOR_RESET}\n")

        print(f"{COLOR_BOLD}Passed Invariant Checks:{COLOR_RESET}")
        for passed in self.passed_checks:
            print(f"  {COLOR_GREEN}✔{COLOR_RESET} {passed}")

        if self.warnings:
            print(f"\n{COLOR_BOLD}{COLOR_YELLOW}Warnings ({len(self.warnings)}):{COLOR_RESET}")
            for w in self.warnings:
                loc = f" ({w['file']}:{w['line']})" if w['file'] else ""
                print(f"  {COLOR_YELLOW}⚠ [{w['code']}]{COLOR_RESET} {COLOR_BOLD}{w['title']}{COLOR_RESET}{loc}")
                print(f"    {w['details']}")

        if self.violations:
            print(f"\n{COLOR_BOLD}{COLOR_RED}Violations / Blockers ({len(self.violations)}):{COLOR_RESET}")
            for v in self.violations:
                loc = f" ({v['file']}:{v['line']})" if v['file'] else ""
                print(f"  {COLOR_RED}✖ [{v['code']}]{COLOR_RESET} {COLOR_BOLD}{v['title']}{COLOR_RESET}{loc}")
                print(f"    {v['details']}")

        print(f"\n{COLOR_BOLD}Summary Scorecard:{COLOR_RESET}")
        print(f"  • Invariants Checked: {len(self.passed_checks)}")
        print(f"  • Warnings:           {len(self.warnings)}")
        print(f"  • Violations:         {len(self.violations)}")
        
        if not self.violations and not (self.strict and self.warnings):
            print(f"\n{COLOR_BOLD}{COLOR_GREEN}✔ Architecture Score: 100/100 (Clean Decoupled Architecture){COLOR_RESET}\n")
        else:
            print(f"\n{COLOR_BOLD}{COLOR_RED}✖ Architecture Score: FAILED (Governance violations detected){COLOR_RESET}\n")

    def to_json(self) -> str:
        return json.dumps({
            "workspace": str(self.root),
            "passed_checks": self.passed_checks,
            "violations": self.violations,
            "warnings": self.warnings,
            "status": "PASS" if not self.violations and not (self.strict and self.warnings) else "FAIL"
        }, indent=2)


def main():
    import argparse
    parser = argparse.ArgumentParser(description="CaldoAI Untangle & Architectural Governance Analyzer for Hagibis")
    parser.add_argument("--workspace", "-w", type=Path, default=Path.cwd(), help="Path to workspace root")
    parser.add_argument("--strict", "-s", action="store_true", help="Fail with non-zero exit code on any warning")
    parser.add_argument("--json", "-j", action="store_true", help="Output JSON report")

    args = parser.parse_args()
    analyzer = UntangleAnalyzer(args.workspace, strict=args.strict)
    code = analyzer.run_all()

    if args.json:
        print(analyzer.to_json())
    else:
        analyzer.print_report()

    return code


if __name__ == "__main__":
    sys.exit(main())
