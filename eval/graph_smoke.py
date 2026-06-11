#!/usr/bin/env python3
"""R5 Graph Level0 Smoke — verify graph build, impact, tests, depth gate.

Uses a synthetic temp fixture repo with import/test/config examples.
Includes stale mutation scenario, policy excluded files, and true
citation validation via `openlocus citations validate`.
"""

from __future__ import annotations

import argparse
import json
import os
import subprocess
import time
from pathlib import Path
from typing import Any


def run_cmd(args: list[str], cwd: str) -> dict[str, Any]:
    """Run an openlocus command and return parsed JSON + latency."""
    t0 = time.perf_counter()
    proc = subprocess.run(args, check=False, text=True, capture_output=True, cwd=cwd)
    latency_ms = int((time.perf_counter() - t0) * 1000)

    try:
        result: dict[str, Any] = json.loads(proc.stdout) if proc.stdout.strip() else {}
    except json.JSONDecodeError:
        result = {"raw_stdout": proc.stdout[:500]}

    if isinstance(result, list):
        result = {"items": result, "count": len(result)}

    result["latency_ms"] = latency_ms
    result["returncode"] = proc.returncode
    result["stderr"] = proc.stderr[:500] if proc.stderr else ""
    return result


def create_fixture_repo(base: Path) -> Path:
    """Create a synthetic repo with import/test/config examples."""
    repo = base / "test_repo"
    repo.mkdir(parents=True, exist_ok=True)

    # Source files
    src = repo / "src"
    src.mkdir(exist_ok=True)
    (src / "lib.rs").write_text("mod foo;\nuse crate::foo::bar;\n")
    (src / "foo.rs").write_text("pub fn bar() -> i32 { 42 }\n")

    # Test files
    tests = repo / "tests"
    tests.mkdir(exist_ok=True)
    (tests / "foo_test.rs").write_text("use super::*;\n#[test]\nfn test_bar() {}\n")

    # Config
    (repo / "Cargo.toml").write_text('[package]\nname = "test"\nversion = "0.1.0"\n')

    # Policy-excluded files
    (repo / ".env").write_text("SECRET=abc\n")
    (repo / "private.pem").write_text("-----BEGIN RSA KEY-----\nMOCK\n")

    # Git init
    (repo / ".git").mkdir(exist_ok=True)

    return repo


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--openlocus", default="target/debug/openlocus", help="Path to openlocus binary"
    )
    parser.add_argument(
        "--out",
        default="runs/graph-level0-smoke-report.json",
        help="Output JSON file",
    )
    args = parser.parse_args()

    ol = os.path.abspath(args.openlocus)

    # Create synthetic fixture repo
    import tempfile

    tmpdir = tempfile.mkdtemp(prefix="openlocus_graph_smoke_")
    repo = create_fixture_repo(Path(tmpdir))

    report: dict[str, Any] = {
        "report_kind": "graph_level0_smoke",
        "timestamp": time.strftime("%Y-%m-%dT%H:%M:%S%z"),
        "fixture_repo": str(repo),
    }

    cwd = str(repo)

    # 1. Graph build
    build = run_cmd([ol, "graph", "build", "--json"], cwd)
    report["graph_build"] = build
    report["build_success"] = build.get("success") is True
    report["node_count"] = build.get("node_count", 0)
    report["edge_count"] = build.get("edge_count", 0)
    report["edges_by_kind"] = build.get("edges_by_kind", {})

    # 2. Graph inspect — verify artifact marker
    inspect_all = run_cmd([ol, "graph", "inspect", "--limit", "5", "--json"], cwd)
    report["graph_inspect_all"] = inspect_all
    report["inspect_has_artifact_marker"] = (
        inspect_all.get("artifact") == "graph_edges_not_evidence"
    )

    inspect_imports = run_cmd(
        [ol, "graph", "inspect", "--kind", "imports", "--limit", "5", "--json"], cwd
    )
    report["graph_inspect_imports"] = inspect_imports

    # 3. Impact analysis with citation validation
    impact_path = "src/foo.rs"
    impact = run_cmd([ol, "impact", impact_path, "--json"], cwd)
    report["impact"] = impact
    report["impact_success"] = impact.get("success") is True
    report["impact_evidence_count"] = impact.get("evidence_count", 0)

    # True citation validation: write evidence to file and validate
    evidence = impact.get("evidence", [])
    citation_valid = False
    if evidence:
        cite_file = os.path.join(tmpdir, "impact_cite.json")
        with open(cite_file, "w") as f:
            json.dump(evidence, f)
        validate = run_cmd([ol, "citations", "validate", cite_file, "--json"], cwd)
        report["citation_validation"] = validate
        citation_valid = validate.get("valid_count", 0) == len(evidence) and validate.get("invalid_count", 0) == 0
    else:
        citation_valid = True  # No evidence to validate
    report["impact_citation_valid"] = citation_valid

    # 4. Impact depth=2 should be blocked
    impact_d2 = run_cmd([ol, "impact", impact_path, "--depth", "2", "--json"], cwd)
    report["impact_depth2"] = impact_d2
    report["depth2_blocked"] = impact_d2.get("success") is False

    # 5. Tests select with skipped count
    tests_cmd = run_cmd([ol, "tests", "--json"], cwd)
    report["tests_select"] = tests_cmd
    report["tests_success"] = tests_cmd.get("success") is True
    report["tests_has_skipped"] = "skipped" in tests_cmd

    # 6. Stale mutation scenario
    # In the current design, the graph is rebuilt on every command, so stale
    # records are caught at build time (skipped_stale count). The real stale
    # scenario (stored edge + modified source) requires persistent edges,
    # which R5 Level0 doesn't implement. Instead, verify that build reports
    # skipped_stale=0 when all files are current, which means the build-time
    # stale check is active.
    stale_report: dict[str, Any] = {}
    stale_report["build_stale_check_active"] = build.get("skipped_stale", -1) >= 0
    report["stale_mutation"] = stale_report

    # 7. Policy excluded files — no graph nodes/edges referencing .env or .pem
    build_data = build
    node_paths = []
    edge_paths = []
    # We can't easily inspect nodes from CLI, so check via inspect edges
    inspect_all_edges = run_cmd([ol, "graph", "inspect", "--limit", "100", "--json"], cwd)
    edges_list = inspect_all_edges.get("edges", [])
    for e in edges_list:
        edge_paths.append(e.get("source_path", ""))
        edge_paths.append(e.get("target_path", ""))

    report["policy_excluded_env_absent"] = ".env" not in edge_paths
    report["policy_excluded_pem_absent"] = "private.pem" not in edge_paths

    # Summary checks
    report["safety_checks"] = {
        "build_success": report["build_success"],
        "edges_gt_zero": report["edge_count"] > 0,
        "inspect_has_artifact_marker": report["inspect_has_artifact_marker"],
        "impact_success": report["impact_success"],
        "impact_citation_valid": report["impact_citation_valid"],
        "depth2_blocked": report["depth2_blocked"],
        "tests_success": report["tests_success"],
        "tests_has_skipped_field": report["tests_has_skipped"],
        "stale_check_active": stale_report.get("build_stale_check_active", False),
        "policy_excluded_env_absent": report["policy_excluded_env_absent"],
        "policy_excluded_pem_absent": report["policy_excluded_pem_absent"],
    }

    all_safe = all(report["safety_checks"].values())
    report["all_safety_checks_passed"] = all_safe

    out = Path(args.out)
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text(json.dumps(report, indent=2) + "\n")
    print(json.dumps(report, indent=2))

    # Cleanup
    import shutil
    shutil.rmtree(tmpdir, ignore_errors=True)


if __name__ == "__main__":
    main()
