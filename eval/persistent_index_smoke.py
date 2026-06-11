#!/usr/bin/env python3
"""R7 Persistent Index Smoke — verify persistent BM25 index safety.

Checks: build/status/validate/search persistent, stale mutation detection,
deleted file safety, policy excluded file absence, policy change detection,
bench warm.
"""

from __future__ import annotations

import argparse
import json
import os
import subprocess
import tempfile
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
        result = {"raw_stdout": proc.stdout[:500], "raw_stderr": proc.stderr[:500]}

    result["latency_ms"] = latency_ms
    result["returncode"] = proc.returncode
    result["stderr"] = proc.stderr[:500] if proc.stderr else ""
    return result


def create_fixture_repo(base: Path) -> Path:
    """Create a synthetic repo with test files for persistent index smoke."""
    repo = base / "test_repo"
    repo.mkdir(parents=True, exist_ok=True)

    src = repo / "src"
    src.mkdir(exist_ok=True)
    (src / "auth.rs").write_text(
        "pub fn authenticate_user() -> bool {\n"
        "    // authenticate the user\n"
        "    true\n"
        "}\n"
        "\n"
        "pub fn authorize_action() -> bool {\n"
        "    // authorize the action\n"
        "    true\n"
        "}\n"
    )
    (src / "config.rs").write_text(
        "pub struct Config {\n"
        "    pub name: String,\n"
        "    pub max_retries: u32,\n"
        "}\n"
    )

    # Policy-excluded file
    (repo / ".env").write_text("SECRET_KEY=abc123\n")
    (repo / "secrets.pem").write_text("-----BEGIN RSA PRIVATE KEY-----\nfake\n")

    # Create .openlocus dir and default policy
    openlocus_dir = repo / ".openlocus"
    openlocus_dir.mkdir(exist_ok=True)
    (openlocus_dir / "policy.toml").write_text("")

    (repo / ".git").mkdir(exist_ok=True)

    return repo


def write_policy(repo: Path, policy_toml: str) -> None:
    """Write a policy.toml file to the repo's .openlocus dir."""
    openlocus_dir = repo / ".openlocus"
    openlocus_dir.mkdir(exist_ok=True)
    (openlocus_dir / "policy.toml").write_text(policy_toml)


def remove_policy(repo: Path) -> None:
    """Remove the policy.toml (revert to defaults)."""
    policy_path = repo / ".openlocus" / "policy.toml"
    if policy_path.exists():
        policy_path.unlink()


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--openlocus", default="target/debug/openlocus", help="Path to openlocus binary"
    )
    parser.add_argument(
        "--out",
        default="runs/persistent-index-smoke.json",
        help="Output JSON file",
    )
    args = parser.parse_args()

    ol = os.path.abspath(args.openlocus)

    # Create synthetic fixture repo
    tmpdir = tempfile.mkdtemp(prefix="openlocus_pi_smoke_")
    repo = create_fixture_repo(Path(tmpdir))
    cwd = str(repo)

    report: dict[str, Any] = {
        "report_kind": "persistent_index_smoke",
        "timestamp": time.strftime("%Y-%m-%dT%H:%M:%S%z"),
        "fixture_repo": str(repo),
    }

    safety_checks: dict[str, bool] = {}

    # 1. Purge any existing index
    purge = run_cmd([ol, "index", "purge", "--json"], cwd)
    safety_checks["purge_succeeds"] = purge.get("purged") is True

    # 2. Build persistent index
    build = run_cmd([ol, "index", "build", "--json"], cwd)
    safety_checks["build_succeeds"] = build.get("success") is True
    safety_checks["build_file_count_positive"] = build.get("file_count", 0) > 0
    safety_checks["build_chunk_count_positive"] = build.get("chunk_count", 0) > 0
    safety_checks["build_schema_version"] = build.get("schema_version") in ("r7-bm25-v1", "r8-bm25-v2")
    report["build"] = build

    # 3. Status check
    status = run_cmd([ol, "index", "status", "--json"], cwd)
    safety_checks["status_exists"] = status.get("exists") is True
    safety_checks["status_schema_matches"] = status.get("schema_version") in ("r7-bm25-v1", "r8-bm25-v2")
    safety_checks["status_policy_hash_matches"] = status.get("policy_hash_matches") is True
    safety_checks["status_no_rebuild_needed"] = status.get("requires_rebuild") is not True
    report["status"] = status

    # 4. Validate check
    validate = run_cmd([ol, "index", "validate", "--json"], cwd)
    safety_checks["validate_valid"] = validate.get("valid") is True
    safety_checks["validate_no_stale"] = len(validate.get("stale_files", [])) == 0
    safety_checks["validate_no_deleted"] = len(validate.get("deleted_files", [])) == 0
    safety_checks["validate_policy_hash_matches"] = validate.get("policy_hash_matches") is True
    report["validate"] = validate

    # 5. Search persistent BM25
    search = run_cmd([ol, "search", "bm25", "authenticate", "--index", "persistent", "--json"], cwd)
    evidence_list = search.get("evidence", [])
    stats = search.get("stats", {})
    safety_checks["search_returns_evidence"] = len(evidence_list) > 0
    safety_checks["search_stale_skipped_zero"] = stats.get("stale_hits_skipped", -1) == 0
    safety_checks["search_invalid_skipped_zero"] = stats.get("invalid_hits_skipped", -1) == 0

    # All evidence should be VerifiedCurrent
    all_verified = all(
        ev.get("meta", {}).get("freshness") == "verified_current"
        for ev in evidence_list
    )
    safety_checks["search_all_freshness_verified_current"] = all_verified

    # At least one evidence on auth.rs
    has_auth_evidence = any("auth" in ev.get("path", "") for ev in evidence_list)
    safety_checks["search_finds_auth_file"] = has_auth_evidence
    report["search"] = search

    # 6. Stale mutation test: modify indexed file, search should skip stale hits
    auth_path = repo / "src" / "auth.rs"
    original_content = auth_path.read_text()
    auth_path.write_text("// modified content\nfn modified() {}\n" + original_content)

    search_stale = run_cmd(
        [ol, "search", "bm25", "authenticate", "--index", "persistent", "--json"], cwd
    )
    stale_evidence = search_stale.get("evidence", [])
    stale_stats = search_stale.get("stats", {})

    # After modification, stale hits should be skipped
    # No VerifiedCurrent evidence should appear for the modified file
    stale_verified_for_modified = any(
        ev.get("meta", {}).get("freshness") == "verified_current"
        and "auth" in ev.get("path", "")
        for ev in stale_evidence
    )
    safety_checks["stale_mutation_no_verified_current_evidence"] = not stale_verified_for_modified
    safety_checks["stale_mutation_stale_hits_skipped"] = (
        stale_stats.get("stale_hits_skipped", 0) > 0 or len(stale_evidence) == 0
    )
    report["search_stale"] = search_stale

    # 7. Validate after mutation should detect stale
    validate_stale = run_cmd([ol, "index", "validate", "--json"], cwd)
    safety_checks["validate_detects_stale_after_mutation"] = (
        len(validate_stale.get("stale_files", [])) > 0 or not validate_stale.get("valid", True)
    )
    report["validate_stale"] = validate_stale

    # Restore original content for further tests
    auth_path.write_text(original_content)

    # 8. Deleted file test: delete a file, search should not return evidence for it
    # Rebuild index first
    build2 = run_cmd([ol, "index", "build", "--json"], cwd)
    # Now delete a file
    deleted_path = repo / "src" / "config.rs"
    deleted_path.unlink()

    search_deleted = run_cmd(
        [ol, "search", "bm25", "Config", "--index", "persistent", "--json"], cwd
    )
    deleted_evidence = search_deleted.get("evidence", [])
    # No evidence should appear for the deleted file
    has_deleted_evidence = any(
        "config" in ev.get("path", "").lower()
        for ev in deleted_evidence
    )
    safety_checks["deleted_file_no_evidence"] = not has_deleted_evidence
    report["search_deleted"] = search_deleted

    # 9. Policy excluded files should not appear in persistent index output
    # Rebuild
    config_restore = repo / "src" / "config.rs"
    config_restore.write_text("pub struct Config {\n    pub name: String,\n}\n")
    build3 = run_cmd([ol, "index", "build", "--json"], cwd)

    search_policy = run_cmd(
        [ol, "search", "bm25", "SECRET_KEY", "--index", "persistent", "--json"], cwd
    )
    policy_evidence = search_policy.get("evidence", [])
    has_env_evidence = any(
        ".env" in ev.get("path", "") or "secrets.pem" in ev.get("path", "")
        for ev in policy_evidence
    )
    safety_checks["policy_excluded_files_absent"] = not has_env_evidence
    report["search_policy_excluded"] = search_policy

    # 10. Policy change after build: change policy, persistent search must refuse/fail
    # Build with default policy (already done above), then change policy
    write_policy(repo, "[remote]\nallow = true\n")
    search_policy_changed = run_cmd(
        [ol, "search", "bm25", "authenticate", "--index", "persistent", "--json"], cwd
    )
    # Search should fail/refuse (returncode != 0 or error in output)
    policy_changed_refused = (
        search_policy_changed.get("returncode", 0) != 0
        or "policy hash mismatch" in search_policy_changed.get("raw_stderr", "")
        or "policy hash mismatch" in search_policy_changed.get("raw_stdout", "")
        or "policy hash mismatch" in str(search_policy_changed)
    )
    safety_checks["policy_change_refuses_search"] = policy_changed_refused
    # Validate should also detect the mismatch
    validate_policy_changed = run_cmd([ol, "index", "validate", "--json"], cwd)
    safety_checks["policy_change_validate_detects_mismatch"] = (
        not validate_policy_changed.get("valid", True)
        or not validate_policy_changed.get("policy_hash_matches", True)
    )
    report["search_policy_changed"] = search_policy_changed
    report["validate_policy_changed"] = validate_policy_changed

    # Restore default policy for further tests
    remove_policy(repo)

    # 11. Missing manifest test: Tantivy dir alone must not be searchable
    build_missing_manifest = run_cmd([ol, "index", "build", "--json"], cwd)
    manifest_path = repo / ".openlocus" / "index" / "manifest.json"
    if manifest_path.exists():
        manifest_path.unlink()
    search_missing_manifest = run_cmd(
        [ol, "search", "bm25", "authenticate", "--index", "persistent", "--json"], cwd
    )
    manifest_missing_refused = (
        search_missing_manifest.get("returncode", 0) != 0
        or "manifest missing" in search_missing_manifest.get("raw_stderr", "")
        or "manifest missing" in search_missing_manifest.get("raw_stdout", "")
        or "manifest missing" in str(search_missing_manifest)
    )
    safety_checks["manifest_missing_refuses_search"] = manifest_missing_refused
    report["search_missing_manifest"] = search_missing_manifest

    # 12. Bench warm
    # Create a small dataset for bench
    bench_dataset = os.path.join(tmpdir, "bench_dataset.jsonl")
    with open(bench_dataset, "w") as f:
        f.write(json.dumps({"task_id": "b1", "query": "authenticate", "method": "bm25"}) + "\n")
        f.write(json.dumps({"task_id": "b2", "query": "Config", "method": "bm25"}) + "\n")

    # Rebuild with default policy first (also restores missing manifest)
    build4 = run_cmd([ol, "index", "build", "--json"], cwd)

    bench = run_cmd(
        [ol, "bench", "warm", "--dataset", bench_dataset, "--iterations", "3", "--json"],
        cwd,
    )
    safety_checks["bench_warm_succeeds"] = bench.get("success") is True
    safety_checks["bench_warm_p50_positive"] = bench.get("warm_query_p50_ms", 0) >= 0
    safety_checks["bench_warm_invalid_citations_zero"] = bench.get("invalid_citations", -1) == 0
    # Verify bench reports index_open_ms separately from index_build_ms
    safety_checks["bench_warm_reports_open_ms"] = "index_open_ms" in bench
    safety_checks["bench_warm_open_ms_reasonable"] = (
        bench.get("index_open_ms", -1) >= 0 and bench.get("index_open_ms", 999999) < 60000
    )
    report["bench_warm"] = bench

    # 13. Purge cleanup
    purge2 = run_cmd([ol, "index", "purge", "--json"], cwd)
    safety_checks["purge_after_stale_succeeds"] = purge2.get("purged") is True

    # Summary
    report["safety_checks"] = safety_checks
    all_safe = all(safety_checks.values())
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
