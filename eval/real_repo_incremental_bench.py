#!/usr/bin/env python3
"""R12 Real-Repo Incremental Robustness Benchmark.

Uses a safe temporary copy of a real repo (default: current OpenLocus source)
to test R10 incremental index's real-scenario robustness. Workload file
mutations occur inside the temp repo only; --out writes the benchmark report to
the requested caller path. Does not change Rust core, default CLI/search/retrieve,
and does not introduce watcher/daemon/TDB changes.

Level0 real-repo sample only — one repo (OpenLocus temp copy), not general
performance. Do not overclaim tombstone precision or generalise latency results.
"""

from __future__ import annotations

import argparse
import json
import os
import secrets
import shutil
import subprocess
import sys
import tempfile
import time
from pathlib import Path
from typing import Any

# ── Ignore patterns for copying source repo ──────────────────────────────────
COPY_IGNORE = shutil.ignore_patterns(
    "target",
    ".git",
    ".openlocus",
    "runs",
    "node_modules",
    "dist",
    "__pycache__",
    "*.pyc",
    ".DS_Store",
    "Cargo.lock",
)


# ── Per-run marker suffix ────────────────────────────────────────────────────

def generate_run_suffix() -> str:
    """Generate a per-run unique alphanumeric suffix (no _ or -).
    Short enough (8 hex chars) to avoid BM25 query parser issues with very long tokens."""
    return secrets.token_hex(4)


# ── Helpers ──────────────────────────────────────────────────────────────────

def run_cmd(args: list[str], cwd: str) -> dict[str, Any]:
    """Run an openlocus command and return parsed JSON + latency."""
    t0 = time.perf_counter()
    proc = subprocess.run(args, check=False, text=True, capture_output=True, cwd=cwd)
    latency_ms = int((time.perf_counter() - t0) * 1000)

    try:
        parsed = json.loads(proc.stdout) if proc.stdout.strip() else {}
        result: dict[str, Any]
        if isinstance(parsed, list):
            result = {"evidence": parsed}
        elif isinstance(parsed, dict):
            result = parsed
        else:
            result = {"raw": proc.stdout[:500]}
    except json.JSONDecodeError:
        result = {"raw_stdout": proc.stdout[:500], "raw_stderr": proc.stderr[:500]}

    result["latency_ms"] = latency_ms
    result["returncode"] = proc.returncode
    result["stderr"] = proc.stderr[:500] if proc.stderr else ""
    return result


def copy_repo(source: Path, dest: Path) -> Path:
    """Copy source repo to dest, excluding build artifacts."""
    repo = dest / "repo"
    shutil.copytree(str(source), str(repo), ignore=COPY_IGNORE, symlinks=False)
    return repo


def ensure_repo_init(repo: Path) -> None:
    """Ensure temp repo has .git/ and .openlocus/policy.toml."""
    git_dir = repo / ".git"
    git_dir.mkdir(exist_ok=True)

    openlocus_dir = repo / ".openlocus"
    openlocus_dir.mkdir(exist_ok=True)
    policy_path = openlocus_dir / "policy.toml"
    if not policy_path.exists():
        policy_path.write_text("")


def index_size_bytes(repo: Path) -> int:
    """Total size of .openlocus/index/ directory."""
    index_dir = repo / ".openlocus" / "index"
    if not index_dir.exists():
        return 0
    total = 0
    for f in index_dir.rglob("*"):
        if f.is_file():
            total += f.stat().st_size
    return total


def search_marker(ol: str, cwd: str, marker: str) -> dict[str, Any]:
    """Search for a marker token in persistent index. Checks returncode."""
    result = run_cmd([ol, "search", "bm25", marker, "--index", "persistent", "--json"], cwd)
    result["search_success"] = result.get("returncode", -1) == 0
    return result


def evidence_has_path_and_marker(
    evidence_list: list[dict], path_fragment: str = "", marker: str = ""
) -> bool:
    """Check if any evidence has BOTH path_fragment in path AND marker in excerpt.

    This is path+marker conjunction, not disjunction. The marker must be present
    in the cited excerpt, not merely echoed in why/query metadata.
    """
    for ev in evidence_list:
        path_ok = True
        if path_fragment:
            path_ok = path_fragment in ev.get("path", "")
        marker_ok = True
        if marker:
            meta = ev.get("meta", {})
            excerpt = meta.get("excerpt", "")
            marker_ok = marker in excerpt
        if path_ok and marker_ok:
            return True
    return False


def validate_citations(ol: str, cwd: str, evidence_list: list[dict], tmpdir: str) -> tuple[int, bool]:
    """Run citations validate on collected evidence.
    Returns (invalid_count, validator_ok).
    validator_ok is True only if returncode==0.
    For empty evidence, returns (0, True) — nothing to validate is not a pass for
    positive gates; callers must check evidence non-empty separately.
    """
    if not evidence_list:
        return 0, True
    citation_file = os.path.join(tmpdir, "evidence_to_validate.json")
    with open(citation_file, "w") as f:
        json.dump(evidence_list, f)
    result = run_cmd([ol, "citations", "validate", citation_file, "--json"], cwd)
    invalid_count = result.get("invalid_count", -1)
    validator_ok = result.get("returncode", -1) == 0
    return invalid_count, validator_ok


def check_no_verified_current_for_paths(
    evidence_list: list[dict], forbidden_paths: list[str]
) -> list[str]:
    """Return list of forbidden paths found with freshness=verified_current."""
    violations: list[str] = []
    for ev in evidence_list:
        meta = ev.get("meta", {})
        if meta.get("freshness") == "verified_current":
            ev_path = ev.get("path", "")
            for fp in forbidden_paths:
                if fp in ev_path:
                    violations.append(ev_path)
                    break
    return violations


def percentile(sorted_data: list[float], p: float) -> float:
    if not sorted_data:
        return 0.0
    idx = int(p / 100.0 * (len(sorted_data) - 1))
    return sorted_data[min(idx, len(sorted_data) - 1)]


def collect_all_evidence(ol: str, cwd: str, markers: list[str]) -> tuple[list[dict], bool]:
    """Search multiple markers and collect all evidence plus returncode status."""
    all_evidence: list[dict] = []
    all_searches_ok = True
    for m in markers:
        result = search_marker(ol, cwd, m)
        all_searches_ok = all_searches_ok and result.get("search_success", False)
        all_evidence.extend(result.get("evidence", []))
    return all_evidence, all_searches_ok


def assert_markers_absent(repo: Path, markers: list[str]) -> None:
    """Assert that none of the markers appear in any copied file in the repo.

    Markers are ASCII, so byte scanning every copied file is more exhaustive than
    extension-based text decoding and catches self-contamination in any indexed
    or accidentally included text file.
    """
    marker_bytes = [(marker, marker.encode("utf-8")) for marker in markers]
    for f in repo.rglob("*"):
        if not f.is_file():
            continue
        try:
            data = f.read_bytes()
        except OSError:
            continue
        for marker, needle in marker_bytes:
            if needle in data:
                raise AssertionError(
                    f"Marker self-contamination: '{marker}' found in "
                    f"{f.relative_to(repo)} before baseline build"
                )


# ── Workloads ────────────────────────────────────────────────────────────────

def workload_modify_one(
    ol: str, repo: Path, cwd: str, tmpdir: str, safety_checks: dict[str, bool], sfx: str
) -> dict[str, Any]:
    """A. modify_one: Change an indexed existing file with marker replacement."""
    result: dict[str, Any] = {}

    old_marker = f"r12oldmodifyalpha{sfx}"
    new_marker = f"r12newmodifyalpha{sfx}"

    # Pick a real file to modify
    target = repo / "crates" / "openlocus-index" / "src" / "persistent.rs"
    if not target.exists():
        rs_files = list(repo.rglob("*.rs"))
        target = rs_files[0] if rs_files else repo / "r12_bench" / "r12_modify_target.rs"
        target.parent.mkdir(parents=True, exist_ok=True)

    original = target.read_text()
    # Append old marker
    target.write_text(original + f"\n// {old_marker}\n")

    # Before update: dirty should detect modified
    dirty = run_cmd([ol, "index", "dirty", "--json"], cwd)
    safety_checks["modify_dirty_modified_count"] = dirty.get("modified_count", 0) >= 1
    safety_checks["modify_dirty_requires_update"] = dirty.get("requires_update") is True
    result["dirty_before"] = dirty

    # Search old marker — should not return VerifiedCurrent evidence for modified path
    search_old = search_marker(ol, cwd, old_marker)
    old_evidence = search_old.get("evidence", [])
    target_rel = str(target.relative_to(repo))
    stale_verified = check_no_verified_current_for_paths(old_evidence, [target_rel])
    safety_checks["modify_search_old_no_verified_current"] = len(stale_verified) == 0
    safety_checks["modify_search_old_returncode_ok"] = search_old.get("search_success", False)
    result["search_old"] = search_old

    # Now change the content to new marker
    target.write_text(original + f"\n// {new_marker}\n")

    # Update dirty
    update = run_cmd([ol, "index", "update", "--dirty", "--json"], cwd)
    safety_checks["modify_update_succeeds"] = update.get("success") is True
    result["update"] = update

    # After update: dirty clean, validate valid
    dirty_after = run_cmd([ol, "index", "dirty", "--json"], cwd)
    safety_checks["modify_dirty_clean_after"] = dirty_after.get("clean") is True

    validate = run_cmd([ol, "index", "validate", "--json"], cwd)
    safety_checks["modify_validate_valid_after"] = validate.get("valid") is True

    # New marker found at path with marker in evidence (path AND marker)
    search_new = search_marker(ol, cwd, new_marker)
    new_evidence = search_new.get("evidence", [])
    safety_checks["modify_new_marker_found"] = (
        len(new_evidence) > 0
        and evidence_has_path_and_marker(new_evidence, path_fragment=target_rel, marker=new_marker)
    )
    safety_checks["modify_new_search_returncode_ok"] = search_new.get("search_success", False)
    result["search_new"] = search_new

    # Old marker gone (no VerifiedCurrent for modified path)
    search_old2 = search_marker(ol, cwd, old_marker)
    old_evidence2 = search_old2.get("evidence", [])
    old_verified = check_no_verified_current_for_paths(old_evidence2, [target_rel])
    safety_checks["modify_old_marker_gone"] = len(old_verified) == 0
    safety_checks["modify_old_search_returncode_ok"] = search_old2.get("search_success", False)

    # Citation validation
    all_ev = new_evidence + old_evidence2
    if all_ev:
        invalid, validator_ok = validate_citations(ol, cwd, all_ev, tmpdir)
        safety_checks["modify_citations_invalid_count_zero"] = invalid == 0 and validator_ok
        result["invalid_citations"] = invalid
    else:
        # No evidence to validate; positive gate should not pass from this alone
        safety_checks["modify_citations_invalid_count_zero"] = True
        result["invalid_citations"] = 0

    # Restore
    target.write_text(original)

    return result


def workload_add_one(
    ol: str, repo: Path, cwd: str, tmpdir: str, safety_checks: dict[str, bool], sfx: str
) -> dict[str, Any]:
    """B. add_one: Add a new policy-included file."""
    result: dict[str, Any] = {}

    add_marker = f"r12addalpha{sfx}"
    add_filename = f"r12_add_target_{sfx}.rs"

    bench_dir = repo / "r12_bench"
    bench_dir.mkdir(parents=True, exist_ok=True)

    add_file = bench_dir / add_filename
    add_file.write_text(f"// {add_marker}\npub fn r12_added_fn() -> bool {{ true }}\n")

    # Dirty should detect added
    dirty = run_cmd([ol, "index", "dirty", "--json"], cwd)
    safety_checks["add_dirty_added_count"] = dirty.get("added_count", 0) >= 1
    result["dirty"] = dirty

    # Update
    update = run_cmd([ol, "index", "update", "--dirty", "--json"], cwd)
    safety_checks["add_update_succeeds"] = update.get("success") is True

    # Search marker found at path with marker (path AND marker)
    search = search_marker(ol, cwd, add_marker)
    evidence = search.get("evidence", [])
    add_rel = str(add_file.relative_to(repo))
    safety_checks["add_marker_found"] = (
        len(evidence) > 0
        and evidence_has_path_and_marker(evidence, path_fragment=add_rel, marker=add_marker)
    )
    safety_checks["add_search_returncode_ok"] = search.get("search_success", False)
    result["search"] = search

    # Clean + validate
    dirty_after = run_cmd([ol, "index", "dirty", "--json"], cwd)
    safety_checks["add_dirty_clean_after"] = dirty_after.get("clean") is True

    validate = run_cmd([ol, "index", "validate", "--json"], cwd)
    safety_checks["add_validate_valid_after"] = validate.get("valid") is True

    # Citation validation
    if evidence:
        invalid, validator_ok = validate_citations(ol, cwd, evidence, tmpdir)
        safety_checks["add_citations_invalid_count_zero"] = invalid == 0 and validator_ok
        result["invalid_citations"] = invalid
    else:
        safety_checks["add_citations_invalid_count_zero"] = False
        result["invalid_citations"] = -1

    return result


def workload_delete_one(
    ol: str, repo: Path, cwd: str, tmpdir: str, safety_checks: dict[str, bool], sfx: str
) -> dict[str, Any]:
    """C. delete_one: Delete a pre-built file."""
    result: dict[str, Any] = {}

    del_marker = f"r12deletealpha{sfx}"
    del_filename = f"r12_delete_target_{sfx}.rs"

    bench_dir = repo / "r12_bench"
    bench_dir.mkdir(parents=True, exist_ok=True)

    del_file = bench_dir / del_filename
    del_file.write_text(f"// {del_marker}\npub fn r12_delete_fn() -> bool {{ false }}\n")
    # Rebuild to include it
    run_cmd([ol, "index", "build", "--json"], cwd)

    # Delete
    del_file.unlink()

    # Dirty should detect deleted
    dirty = run_cmd([ol, "index", "dirty", "--json"], cwd)
    safety_checks["delete_dirty_deleted_count"] = dirty.get("deleted_count", 0) >= 1
    result["dirty"] = dirty

    # Update
    update = run_cmd([ol, "index", "update", "--dirty", "--json"], cwd)
    safety_checks["delete_update_succeeds"] = update.get("success") is True

    # Search marker should not return deleted path VerifiedCurrent
    search = search_marker(ol, cwd, del_marker)
    evidence = search.get("evidence", [])
    del_rel = str(del_file.relative_to(repo))
    verified_for_deleted = check_no_verified_current_for_paths(evidence, [del_rel])
    safety_checks["delete_no_verified_current_for_deleted"] = len(verified_for_deleted) == 0
    safety_checks["delete_search_returncode_ok"] = search.get("search_success", False)
    result["search"] = search

    # Clean + validate
    dirty_after = run_cmd([ol, "index", "dirty", "--json"], cwd)
    safety_checks["delete_dirty_clean_after"] = dirty_after.get("clean") is True

    validate = run_cmd([ol, "index", "validate", "--json"], cwd)
    safety_checks["delete_validate_valid_after"] = validate.get("valid") is True

    # Citation validation
    if evidence:
        invalid, validator_ok = validate_citations(ol, cwd, evidence, tmpdir)
        safety_checks["delete_citations_invalid_count_zero"] = invalid == 0 and validator_ok
        result["invalid_citations"] = invalid
    else:
        safety_checks["delete_citations_invalid_count_zero"] = True
        result["invalid_citations"] = 0

    return result


def workload_rename_one(
    ol: str, repo: Path, cwd: str, tmpdir: str, safety_checks: dict[str, bool], sfx: str
) -> dict[str, Any]:
    """D. rename_one: Rename a file and change its marker."""
    result: dict[str, Any] = {}

    old_marker = f"r12renameoldalpha{sfx}"
    new_marker = f"r12renamenewalpha{sfx}"
    old_filename = f"r12_rename_old_{sfx}.rs"
    new_filename = f"r12_rename_new_{sfx}.rs"

    bench_dir = repo / "r12_bench"
    bench_dir.mkdir(parents=True, exist_ok=True)

    old_file = bench_dir / old_filename
    new_file = bench_dir / new_filename

    old_file.write_text(f"// {old_marker}\npub fn r12_rename_old_fn() -> bool {{ true }}\n")
    # Rebuild
    run_cmd([ol, "index", "build", "--json"], cwd)

    # Rename: delete old, create new with different marker
    old_file.unlink()
    new_file.write_text(f"// {new_marker}\npub fn r12_rename_new_fn() -> bool {{ true }}\n")

    # Dirty should detect both added and deleted
    dirty = run_cmd([ol, "index", "dirty", "--json"], cwd)
    safety_checks["rename_dirty_added_count"] = dirty.get("added_count", 0) >= 1
    safety_checks["rename_dirty_deleted_count"] = dirty.get("deleted_count", 0) >= 1
    result["dirty"] = dirty

    # Update
    update = run_cmd([ol, "index", "update", "--dirty", "--json"], cwd)
    safety_checks["rename_update_succeeds"] = update.get("success") is True

    # Old marker/path gone (no VerifiedCurrent for old path)
    search_old = search_marker(ol, cwd, old_marker)
    old_evidence = search_old.get("evidence", [])
    old_rel = str(old_file.relative_to(repo))
    old_verified = check_no_verified_current_for_paths(old_evidence, [old_rel])
    safety_checks["rename_old_gone"] = len(old_verified) == 0
    safety_checks["rename_old_search_returncode_ok"] = search_old.get("search_success", False)

    # New marker found at new path with marker (path AND marker)
    search_new = search_marker(ol, cwd, new_marker)
    new_evidence = search_new.get("evidence", [])
    new_rel = str(new_file.relative_to(repo))
    safety_checks["rename_new_found"] = (
        len(new_evidence) > 0
        and evidence_has_path_and_marker(new_evidence, path_fragment=new_rel, marker=new_marker)
    )
    safety_checks["rename_new_search_returncode_ok"] = search_new.get("search_success", False)
    result["search_old"] = search_old
    result["search_new"] = search_new

    # Clean + validate
    dirty_after = run_cmd([ol, "index", "dirty", "--json"], cwd)
    safety_checks["rename_dirty_clean_after"] = dirty_after.get("clean") is True

    validate = run_cmd([ol, "index", "validate", "--json"], cwd)
    safety_checks["rename_validate_valid_after"] = validate.get("valid") is True

    # Citation validation
    all_ev = old_evidence + new_evidence
    if all_ev:
        invalid, validator_ok = validate_citations(ol, cwd, all_ev, tmpdir)
        safety_checks["rename_citations_invalid_count_zero"] = invalid == 0 and validator_ok
        result["invalid_citations"] = invalid
    else:
        safety_checks["rename_citations_invalid_count_zero"] = True
        result["invalid_citations"] = 0

    return result


def workload_policy_exclude(
    ol: str, repo: Path, cwd: str, safety_checks: dict[str, bool], sfx: str
) -> dict[str, Any]:
    """E. policy_exclude: Add .env.r12bench — should not dirty or appear."""
    result: dict[str, Any] = {}

    excluded_marker = f"r12excludedalpha{sfx}"
    env_file = repo / f".env.r12bench{sfx}"
    env_file.write_text(f"{excluded_marker.upper()}=secret_value\n")

    # Dirty should stay clean (policy-excluded)
    dirty = run_cmd([ol, "index", "dirty", "--json"], cwd)
    safety_checks["policy_exclude_dirty_clean"] = dirty.get("clean") is True
    safety_checks["policy_exclude_no_requires_update"] = dirty.get("requires_update") is not True
    result["dirty"] = dirty

    # Search marker should not find persistent evidence FROM the excluded file.
    # Path-aware check: no evidence should have the excluded file's path.
    search = search_marker(ol, cwd, excluded_marker)
    evidence = search.get("evidence", [])
    env_rel = str(env_file.relative_to(repo))
    has_excluded_evidence = any(env_rel in ev.get("path", "") for ev in evidence)
    safety_checks["policy_exclude_no_persistent_evidence"] = not has_excluded_evidence
    safety_checks["policy_exclude_search_returncode_ok"] = search.get("search_success", False)
    result["search"] = search

    # Clean up
    env_file.unlink()

    return result


def workload_branch_like_batch(
    ol: str, repo: Path, cwd: str, tmpdir: str, safety_checks: dict[str, bool], sfx: str
) -> dict[str, Any]:
    """F. branch_like_batch: Batch modify/add/delete/rename in one update."""
    result: dict[str, Any] = {}

    add_marker_0 = f"r12branchaddalpha0{sfx}"
    rename_new_marker_0 = f"r12branchrenamenewalpha0{sfx}"
    delete_marker_0 = f"r12branchdeletealpha0{sfx}"
    rename_old_marker_0 = f"r12branchrenameoldalpha0{sfx}"

    bench_dir = repo / "r12_bench"
    bench_dir.mkdir(parents=True, exist_ok=True)

    # Ensure branch files exist before batch
    for i in range(3):
        bd = bench_dir / f"branch_delete_{i}_{sfx}.rs"
        if not bd.exists():
            delete_marker = f"r12branchdeletealpha{i}{sfx}"
            bd.write_text(
                f"// {delete_marker}\npub fn branch_del_{i}() -> bool {{ false }}\n"
            )

        br = bench_dir / f"branch_rename_old_{i}_{sfx}.rs"
        if not br.exists():
            rename_old_marker = f"r12branchrenameoldalpha{i}{sfx}"
            br.write_text(
                f"// {rename_old_marker}\npub fn branch_rename_old_{i}() -> bool {{ true }}\n"
            )

    # Rebuild to include branch files
    run_cmd([ol, "index", "build", "--json"], cwd)

    # Prove all old/delete markers are indexed before removing them, so later
    # no-VerifiedCurrent checks are not vacuous empty-query checks.
    for i in range(3):
        delete_rel = str((bench_dir / f"branch_delete_{i}_{sfx}.rs").relative_to(repo))
        delete_marker = f"r12branchdeletealpha{i}{sfx}"
        pre_delete_search = search_marker(ol, cwd, delete_marker)
        pre_delete_evidence = pre_delete_search.get("evidence", [])
        safety_checks[f"batch_pre_delete{i}_indexed"] = (
            pre_delete_search.get("search_success", False)
            and len(pre_delete_evidence) > 0
            and evidence_has_path_and_marker(
                pre_delete_evidence, path_fragment=delete_rel, marker=delete_marker
            )
        )

        rename_old_rel = str((bench_dir / f"branch_rename_old_{i}_{sfx}.rs").relative_to(repo))
        rename_old_marker = f"r12branchrenameoldalpha{i}{sfx}"
        pre_rename_old_search = search_marker(ol, cwd, rename_old_marker)
        pre_rename_old_evidence = pre_rename_old_search.get("evidence", [])
        safety_checks[f"batch_pre_rename_old{i}_indexed"] = (
            pre_rename_old_search.get("search_success", False)
            and len(pre_rename_old_evidence) > 0
            and evidence_has_path_and_marker(
                pre_rename_old_evidence,
                path_fragment=rename_old_rel,
                marker=rename_old_marker,
            )
        )

    # --- Batch operations ---

    # Add 5 new files
    added_files = []
    for i in range(5):
        af = bench_dir / f"branch_add_{i}_{sfx}.rs"
        m = f"r12branchaddalpha{i}{sfx}"
        af.write_text(f"// {m}\npub fn branch_add_{i}() -> bool {{ true }}\n")
        added_files.append(af)

    # Delete 3 prebuilt branch_delete files
    deleted_files = []
    for i in range(3):
        df = bench_dir / f"branch_delete_{i}_{sfx}.rs"
        if df.exists():
            df.unlink()
            deleted_files.append(df)

    # Rename 3 branch_rename_old files
    renamed_old = []
    renamed_new = []
    for i in range(3):
        old = bench_dir / f"branch_rename_old_{i}_{sfx}.rs"
        new = bench_dir / f"branch_rename_new_{i}_{sfx}.rs"
        if old.exists():
            old.unlink()
            renamed_old.append(old)
        m_new = f"r12branchrenamenewalpha{i}{sfx}"
        new.write_text(f"// {m_new}\npub fn branch_rename_new_{i}() -> bool {{ true }}\n")
        renamed_new.append(new)

    # Dirty should cover all categories
    dirty = run_cmd([ol, "index", "dirty", "--json"], cwd)
    safety_checks["batch_dirty_has_added"] = dirty.get("added_count", 0) >= 1
    safety_checks["batch_dirty_has_deleted"] = dirty.get("deleted_count", 0) >= 1
    result["dirty"] = dirty

    # Update
    update = run_cmd([ol, "index", "update", "--dirty", "--json"], cwd)
    safety_checks["batch_update_succeeds"] = update.get("success") is True
    result["update"] = update

    # Clean + validate
    dirty_after = run_cmd([ol, "index", "dirty", "--json"], cwd)
    safety_checks["batch_dirty_clean_after"] = dirty_after.get("clean") is True

    validate = run_cmd([ol, "index", "validate", "--json"], cwd)
    safety_checks["batch_validate_valid_after"] = validate.get("valid") is True

    # Check add target 0: path AND marker
    add_0_rel = str(added_files[0].relative_to(repo))
    search_add = search_marker(ol, cwd, add_marker_0)
    add_evidence = search_add.get("evidence", [])
    safety_checks["batch_add0_path_and_marker_found"] = (
        len(add_evidence) > 0
        and evidence_has_path_and_marker(add_evidence, path_fragment=add_0_rel, marker=add_marker_0)
    )
    safety_checks["batch_add_search_returncode_ok"] = search_add.get("search_success", False)

    # Check rename-new target 0: path AND marker
    rename_new_0_rel = str(renamed_new[0].relative_to(repo))
    search_rename_new = search_marker(ol, cwd, rename_new_marker_0)
    rename_new_evidence = search_rename_new.get("evidence", [])
    safety_checks["batch_rename_new0_path_and_marker_found"] = (
        len(rename_new_evidence) > 0
        and evidence_has_path_and_marker(
            rename_new_evidence, path_fragment=rename_new_0_rel, marker=rename_new_marker_0
        )
    )
    safety_checks["batch_rename_new_search_returncode_ok"] = (
        search_rename_new.get("search_success", False)
    )

    # Check deleted path 0: no VerifiedCurrent
    deleted_0_rel = str(deleted_files[0].relative_to(repo))
    search_del = search_marker(ol, cwd, delete_marker_0)
    del_evidence = search_del.get("evidence", [])
    verified_for_deleted = check_no_verified_current_for_paths(del_evidence, [deleted_0_rel])
    safety_checks["batch_deleted0_no_verified_current"] = len(verified_for_deleted) == 0
    safety_checks["batch_deleted0_search_returncode_ok"] = search_del.get(
        "search_success", False
    )

    # Check rename-old path 0: no VerifiedCurrent
    renamed_old_0_rel = str(renamed_old[0].relative_to(repo))
    search_rename_old = search_marker(ol, cwd, rename_old_marker_0)
    rename_old_evidence = search_rename_old.get("evidence", [])
    verified_for_renamed_old = check_no_verified_current_for_paths(
        rename_old_evidence, [renamed_old_0_rel]
    )
    safety_checks["batch_rename_old0_no_verified_current"] = len(verified_for_renamed_old) == 0
    safety_checks["batch_rename_old0_search_returncode_ok"] = search_rename_old.get(
        "search_success", False
    )

    # Citation validation
    all_ev = add_evidence + del_evidence + rename_new_evidence + rename_old_evidence
    if all_ev:
        invalid, validator_ok = validate_citations(ol, cwd, all_ev, tmpdir)
        safety_checks["batch_citations_invalid_count_zero"] = invalid == 0 and validator_ok
        result["invalid_citations"] = invalid
    else:
        safety_checks["batch_citations_invalid_count_zero"] = True
        result["invalid_citations"] = 0

    return result


def workload_latency_compare(
    ol: str, source: Path, tmpdir: str, iterations: int, safety_checks: dict[str, bool], sfx: str
) -> dict[str, Any]:
    """G. latency_compare: Compare update --dirty vs full rebuild using twin repos."""
    result: dict[str, Any] = {}

    modify_update_latencies: list[float] = []
    modify_rebuild_latencies: list[float] = []
    batch_update_latencies: list[float] = []
    batch_rebuild_latencies: list[float] = []

    for i in range(iterations):
        # === modify_one latency: twin repo copies ===
        mod_marker = f"r12latencymodify{sfx}i{i}"

        # Twin A: update path
        a_tmpdir = tempfile.mkdtemp(prefix="openlocus_r12_lat_a_")
        repo_a = copy_repo(source, Path(a_tmpdir))
        ensure_repo_init(repo_a)
        cwd_a = str(repo_a)
        run_cmd([ol, "index", "purge", "--json"], cwd_a)
        run_cmd([ol, "index", "build", "--json"], cwd_a)

        target_a = repo_a / "crates" / "openlocus-index" / "src" / "persistent.rs"
        if not target_a.exists():
            rs_files = list(repo_a.rglob("*.rs"))
            target_a = rs_files[0] if rs_files else repo_a / "r12_bench" / "latency_target.rs"
            target_a.parent.mkdir(parents=True, exist_ok=True)
            if not target_a.exists():
                target_a.write_text("// latency target\n")

        original_a = target_a.read_text()
        target_a.write_text(original_a + f"\n// {mod_marker}\n")

        t0 = time.perf_counter()
        run_cmd([ol, "index", "update", "--dirty", "--json"], cwd_a)
        modify_update_latencies.append((time.perf_counter() - t0) * 1000)

        # Twin B: rebuild path (same mutation, then full build)
        b_tmpdir = tempfile.mkdtemp(prefix="openlocus_r12_lat_b_")
        repo_b = copy_repo(source, Path(b_tmpdir))
        ensure_repo_init(repo_b)
        cwd_b = str(repo_b)
        run_cmd([ol, "index", "purge", "--json"], cwd_b)
        run_cmd([ol, "index", "build", "--json"], cwd_b)

        target_b = repo_b / "crates" / "openlocus-index" / "src" / "persistent.rs"
        if not target_b.exists():
            rs_files = list(repo_b.rglob("*.rs"))
            target_b = rs_files[0] if rs_files else repo_b / "r12_bench" / "latency_target.rs"
            target_b.parent.mkdir(parents=True, exist_ok=True)
            if not target_b.exists():
                target_b.write_text("// latency target\n")
        # Apply same mutation
        original_b = target_b.read_text()
        target_b.write_text(original_b + f"\n// {mod_marker}\n")

        t0 = time.perf_counter()
        run_cmd([ol, "index", "build", "--json"], cwd_b)
        modify_rebuild_latencies.append((time.perf_counter() - t0) * 1000)

        # === batch latency: twin repo copies ===
        batch_marker = f"r12batchlat{sfx}i{i}"

        # Twin A: update path
        ba_tmpdir = tempfile.mkdtemp(prefix="openlocus_r12_blat_a_")
        repo_ba = copy_repo(source, Path(ba_tmpdir))
        ensure_repo_init(repo_ba)
        cwd_ba = str(repo_ba)
        run_cmd([ol, "index", "purge", "--json"], cwd_ba)
        run_cmd([ol, "index", "build", "--json"], cwd_ba)

        bench_a = repo_ba / "r12_bench"
        bench_a.mkdir(parents=True, exist_ok=True)
        for j in range(3):
            (bench_a / f"lat_del_{j}_{sfx}.rs").write_text(
                f"// lat_del_{j}\npub fn lat_del_{j}() {{}}\n"
            )
            (bench_a / f"lat_ren_old_{j}_{sfx}.rs").write_text(
                f"// lat_ren_old_{j}\npub fn lat_ren_old_{j}() {{}}\n"
            )
        run_cmd([ol, "index", "build", "--json"], cwd_ba)

        # Apply batch mutations
        target_ba = repo_ba / "crates" / "openlocus-cli" / "src" / "lib.rs"
        if target_ba.exists():
            orig_ba = target_ba.read_text()
            target_ba.write_text(orig_ba + f"\n// {batch_marker}\n")

        for j in range(5):
            (bench_a / f"lat_add_{j}_{sfx}.rs").write_text(
                f"// {batch_marker}add{j}\npub fn lat_add_{j}() {{}}\n"
            )
        for j in range(3):
            df = bench_a / f"lat_del_{j}_{sfx}.rs"
            if df.exists():
                df.unlink()
        for j in range(3):
            old = bench_a / f"lat_ren_old_{j}_{sfx}.rs"
            new = bench_a / f"lat_ren_new_{j}_{sfx}.rs"
            if old.exists():
                old.unlink()
            new.write_text(f"// {batch_marker}ren{j}\npub fn lat_ren_new_{j}() {{}}\n")

        t0 = time.perf_counter()
        run_cmd([ol, "index", "update", "--dirty", "--json"], cwd_ba)
        batch_update_latencies.append((time.perf_counter() - t0) * 1000)

        # Twin B: rebuild path (same mutations, then full build)
        bb_tmpdir = tempfile.mkdtemp(prefix="openlocus_r12_blat_b_")
        repo_bb = copy_repo(source, Path(bb_tmpdir))
        ensure_repo_init(repo_bb)
        cwd_bb = str(repo_bb)

        bench_b = repo_bb / "r12_bench"
        bench_b.mkdir(parents=True, exist_ok=True)
        # Apply same pre-build + mutations
        for j in range(3):
            (bench_b / f"lat_del_{j}_{sfx}.rs").write_text(
                f"// lat_del_{j}\npub fn lat_del_{j}() {{}}\n"
            )
            (bench_b / f"lat_ren_old_{j}_{sfx}.rs").write_text(
                f"// lat_ren_old_{j}\npub fn lat_ren_old_{j}() {{}}\n"
            )
        run_cmd([ol, "index", "purge", "--json"], cwd_bb)
        run_cmd([ol, "index", "build", "--json"], cwd_bb)

        # Apply same batch mutations
        target_bb = repo_bb / "crates" / "openlocus-cli" / "src" / "lib.rs"
        if target_bb.exists():
            orig_bb = target_bb.read_text()
            target_bb.write_text(orig_bb + f"\n// {batch_marker}\n")

        for j in range(5):
            (bench_b / f"lat_add_{j}_{sfx}.rs").write_text(
                f"// {batch_marker}add{j}\npub fn lat_add_{j}() {{}}\n"
            )
        for j in range(3):
            df = bench_b / f"lat_del_{j}_{sfx}.rs"
            if df.exists():
                df.unlink()
        for j in range(3):
            old = bench_b / f"lat_ren_old_{j}_{sfx}.rs"
            new = bench_b / f"lat_ren_new_{j}_{sfx}.rs"
            if old.exists():
                old.unlink()
            new.write_text(f"// {batch_marker}ren{j}\npub fn lat_ren_new_{j}() {{}}\n")

        t0 = time.perf_counter()
        run_cmd([ol, "index", "build", "--json"], cwd_bb)
        batch_rebuild_latencies.append((time.perf_counter() - t0) * 1000)

        # Clean up iteration temp dirs
        shutil.rmtree(a_tmpdir, ignore_errors=True)
        shutil.rmtree(b_tmpdir, ignore_errors=True)
        shutil.rmtree(ba_tmpdir, ignore_errors=True)
        shutil.rmtree(bb_tmpdir, ignore_errors=True)

    # Compute percentiles
    result["modify_update_ms"] = {
        "p50": percentile(sorted(modify_update_latencies), 50),
        "p95": percentile(sorted(modify_update_latencies), 95),
        "max": max(modify_update_latencies) if modify_update_latencies else 0,
    }
    result["modify_rebuild_ms"] = {
        "p50": percentile(sorted(modify_rebuild_latencies), 50),
        "p95": percentile(sorted(modify_rebuild_latencies), 95),
        "max": max(modify_rebuild_latencies) if modify_rebuild_latencies else 0,
    }
    result["batch_update_ms"] = {
        "p50": percentile(sorted(batch_update_latencies), 50),
        "p95": percentile(sorted(batch_update_latencies), 95),
        "max": max(batch_update_latencies) if batch_update_latencies else 0,
    }
    result["batch_rebuild_ms"] = {
        "p50": percentile(sorted(batch_rebuild_latencies), 50),
        "p95": percentile(sorted(batch_rebuild_latencies), 95),
        "max": max(batch_rebuild_latencies) if batch_rebuild_latencies else 0,
    }

    # Latency gate: p50 update < p50 full rebuild if possible
    mod_update_p50 = result["modify_update_ms"]["p50"]
    mod_rebuild_p50 = result["modify_rebuild_ms"]["p50"]
    batch_update_p50 = result["batch_update_ms"]["p50"]
    batch_rebuild_p50 = result["batch_rebuild_ms"]["p50"]

    result["latency_gate_passed"] = (
        mod_update_p50 < mod_rebuild_p50
        and batch_update_p50 < batch_rebuild_p50
    )
    result["latency_gate_checks"] = {
        "modify_update_faster": mod_update_p50 < mod_rebuild_p50,
        "batch_update_faster": batch_update_p50 < batch_rebuild_p50,
    }

    return result


def workload_growth_cycles(
    ol: str, source: Path, tmpdir: str, growth_cycles: int, safety_checks: dict[str, bool], sfx: str
) -> dict[str, Any]:
    """H. growth_cycles: N cycles of modify + update on a temp repo copy."""
    result: dict[str, Any] = {}

    growth_tmpdir = tempfile.mkdtemp(prefix="openlocus_r12_growth_")
    repo = copy_repo(source, Path(growth_tmpdir))
    ensure_repo_init(repo)
    cwd = str(repo)

    # Build
    run_cmd([ol, "index", "purge", "--json"], cwd)
    run_cmd([ol, "index", "build", "--json"], cwd)

    size_before = index_size_bytes(repo)
    result["size_before_bytes"] = size_before

    target = repo / "crates" / "openlocus-index" / "src" / "persistent.rs"
    if not target.exists():
        rs_files = list(repo.rglob("*.rs"))
        target = rs_files[0] if rs_files else repo / "r12_bench" / "growth_target.rs"
        target.parent.mkdir(parents=True, exist_ok=True)
        if not target.exists():
            target.write_text("// growth target\n")

    original = target.read_text()

    for i in range(growth_cycles):
        cycle_marker = f"r12growthcycle{sfx}c{i}"
        # Modify
        target.write_text(original + f"\n// {cycle_marker}\n")

        # Dirty detects modified
        dirty = run_cmd([ol, "index", "dirty", "--json"], cwd)
        safety_checks[f"growth_cycle_{i}_dirty_modified"] = dirty.get("modified_count", 0) >= 1

        # Update
        update = run_cmd([ol, "index", "update", "--dirty", "--json"], cwd)
        safety_checks[f"growth_cycle_{i}_update_succeeds"] = update.get("success") is True

        # Dirty clean after update
        dirty_after = run_cmd([ol, "index", "dirty", "--json"], cwd)
        safety_checks[f"growth_cycle_{i}_dirty_clean"] = dirty_after.get("clean") is True

        # Validate valid
        validate = run_cmd([ol, "index", "validate", "--json"], cwd)
        safety_checks[f"growth_cycle_{i}_validate_valid"] = validate.get("valid") is True

    size_after_updates = index_size_bytes(repo)
    result["size_after_updates_bytes"] = size_after_updates

    # Full rebuild
    run_cmd([ol, "index", "build", "--json"], cwd)
    size_after_rebuild = index_size_bytes(repo)
    result["size_after_rebuild_bytes"] = size_after_rebuild

    # Catastrophic guard: final_after_updates_size <= max(3 * rebuild, rebuild + 64MiB)
    # This is a catastrophic bound, NOT a proof of long-term bounded growth.
    max_allowed = max(3 * size_after_rebuild, size_after_rebuild + 64 * 1024 * 1024)
    result["growth_catastrophic_guard_passed"] = size_after_updates <= max_allowed
    result["observed_growth_ratio"] = (
        round(size_after_updates / size_after_rebuild, 2) if size_after_rebuild > 0 else 0
    )

    # Clean up
    shutil.rmtree(growth_tmpdir, ignore_errors=True)

    return result


# ── Main ─────────────────────────────────────────────────────────────────────

def main() -> None:
    parser = argparse.ArgumentParser(
        description="R12 Real-Repo Incremental Robustness Benchmark"
    )
    parser.add_argument(
        "--openlocus", default="target/debug/openlocus",
        help="Path to openlocus binary",
    )
    parser.add_argument(
        "--source", default=".",
        help="Source repo to copy (default: current directory)",
    )
    parser.add_argument(
        "--out", default="runs/real-repo-incremental-bench.json",
        help="Output JSON file",
    )
    parser.add_argument(
        "--iterations", type=int, default=3,
        help="Number of iterations for latency compare",
    )
    parser.add_argument(
        "--growth-cycles", type=int, default=20,
        help="Number of growth cycles",
    )
    parser.add_argument(
        "--keep-temp", action="store_true",
        help="Keep temp repo directory after benchmark",
    )
    args = parser.parse_args()

    ol = os.path.abspath(args.openlocus)
    source = Path(args.source).resolve()

    # Generate per-run unique suffix
    sfx = generate_run_suffix()

    tmpdir = tempfile.mkdtemp(prefix="openlocus_r12_real_repo_")
    repo = copy_repo(source, Path(tmpdir))
    ensure_repo_init(repo)
    cwd = str(repo)

    report: dict[str, Any] = {
        "report_kind": "real_repo_incremental_bench",
        "source_repo": str(source),
        "timestamp": time.strftime("%Y-%m-%dT%H:%M:%S%z"),
        "iterations": args.iterations,
        "growth_cycles": args.growth_cycles,
        "run_suffix": sfx,
    }

    if args.keep_temp:
        report["temp_repo"] = str(repo)

    safety_checks: dict[str, bool] = {}

    # ── 1. Baseline prep ──────────────────────────────────────────────────────

    # Collect all marker tokens that this run may place in indexed files.
    all_markers = [
        f"r12oldmodifyalpha{sfx}", f"r12newmodifyalpha{sfx}",
        f"r12addalpha{sfx}", f"r12deletealpha{sfx}",
        f"r12renameoldalpha{sfx}", f"r12renamenewalpha{sfx}",
        f"r12excludedalpha{sfx}",
    ]
    all_markers.extend(f"r12branchaddalpha{i}{sfx}" for i in range(5))
    all_markers.extend(f"r12branchdeletealpha{i}{sfx}" for i in range(3))
    all_markers.extend(f"r12branchrenameoldalpha{i}{sfx}" for i in range(3))
    all_markers.extend(f"r12branchrenamenewalpha{i}{sfx}" for i in range(3))
    all_markers.extend(f"r12latencymodify{sfx}i{i}" for i in range(args.iterations))
    all_markers.extend(f"r12batchlat{sfx}i{i}" for i in range(args.iterations))
    all_markers.extend(
        f"r12batchlat{sfx}i{i}add{j}" for i in range(args.iterations) for j in range(5)
    )
    all_markers.extend(
        f"r12batchlat{sfx}i{i}ren{j}" for i in range(args.iterations) for j in range(3)
    )
    all_markers.extend(f"r12growthcycle{sfx}c{i}" for i in range(args.growth_cycles))

    # Assert markers absent from copied repo before baseline build
    try:
        assert_markers_absent(repo, all_markers)
        safety_checks["baseline_markers_absent_pre_build"] = True
    except AssertionError as e:
        safety_checks["baseline_markers_absent_pre_build"] = False
        report["marker_contamination_error"] = str(e)

    # Purge, build, dirty, validate
    purge = run_cmd([ol, "index", "purge", "--json"], cwd)
    safety_checks["baseline_purge_succeeds"] = purge.get("purged") is True

    build = run_cmd([ol, "index", "build", "--json"], cwd)
    safety_checks["baseline_build_succeeds"] = build.get("success") is True
    safety_checks["baseline_file_count_positive"] = build.get("file_count", 0) > 0
    safety_checks["baseline_chunk_count_positive"] = build.get("chunk_count", 0) > 0

    dirty = run_cmd([ol, "index", "dirty", "--json"], cwd)
    safety_checks["baseline_dirty_clean"] = dirty.get("clean") is True

    validate = run_cmd([ol, "index", "validate", "--json"], cwd)
    safety_checks["baseline_validate_valid"] = validate.get("valid") is True

    report["baseline"] = {
        "build": build,
        "dirty": dirty,
        "validate": validate,
        "index_size_bytes": index_size_bytes(repo),
    }

    # ── 2. Workloads ─────────────────────────────────────────────────────────

    report["workloads"] = {}

    print("[R12] Running workload A: modify_one ...")
    report["workloads"]["modify_one"] = workload_modify_one(
        ol, repo, cwd, tmpdir, safety_checks, sfx
    )

    # Rebuild between workloads for clean state
    run_cmd([ol, "index", "build", "--json"], cwd)

    print("[R12] Running workload B: add_one ...")
    report["workloads"]["add_one"] = workload_add_one(
        ol, repo, cwd, tmpdir, safety_checks, sfx
    )

    run_cmd([ol, "index", "build", "--json"], cwd)

    print("[R12] Running workload C: delete_one ...")
    report["workloads"]["delete_one"] = workload_delete_one(
        ol, repo, cwd, tmpdir, safety_checks, sfx
    )

    run_cmd([ol, "index", "build", "--json"], cwd)

    print("[R12] Running workload D: rename_one ...")
    report["workloads"]["rename_one"] = workload_rename_one(
        ol, repo, cwd, tmpdir, safety_checks, sfx
    )

    run_cmd([ol, "index", "build", "--json"], cwd)

    print("[R12] Running workload E: policy_exclude ...")
    report["workloads"]["policy_exclude"] = workload_policy_exclude(
        ol, repo, cwd, safety_checks, sfx
    )

    print("[R12] Running workload F: branch_like_batch ...")
    report["workloads"]["branch_like_batch"] = workload_branch_like_batch(
        ol, repo, cwd, tmpdir, safety_checks, sfx
    )

    print(f"[R12] Running workload G: latency_compare ({args.iterations} iterations) ...")
    report["workloads"]["latency_compare"] = workload_latency_compare(
        ol, source, tmpdir, args.iterations, safety_checks, sfx
    )

    print(f"[R12] Running workload H: growth_cycles ({args.growth_cycles} cycles) ...")
    report["workloads"]["growth_cycles"] = workload_growth_cycles(
        ol, source, tmpdir, args.growth_cycles, safety_checks, sfx
    )

    # ── 3. Collect stale_verified_current violations ─────────────────────────

    stale_markers = [
        f"r12oldmodifyalpha{sfx}", f"r12deletealpha{sfx}", f"r12renameoldalpha{sfx}",
    ]
    stale_forbidden_paths = [
        f"r12_delete_target_{sfx}.rs", f"r12_rename_old_{sfx}.rs",
    ]
    all_stale_evidence, stale_searches_ok = collect_all_evidence(ol, cwd, stale_markers)
    stale_violations = check_no_verified_current_for_paths(
        all_stale_evidence, stale_forbidden_paths
    )
    safety_checks["summary_stale_searches_returncode_ok"] = stale_searches_ok

    # ── 4. Total invalid citations ───────────────────────────────────────────

    new_markers = [
        f"r12newmodifyalpha{sfx}", f"r12addalpha{sfx}", f"r12renamenewalpha{sfx}",
    ]
    all_new_evidence, new_searches_ok = collect_all_evidence(ol, cwd, new_markers)
    safety_checks["summary_new_marker_searches_returncode_ok"] = new_searches_ok
    total_invalid_citations = 0
    citations_validator_ok = True
    if all_new_evidence:
        total_invalid_citations, citations_validator_ok = validate_citations(
            ol, cwd, all_new_evidence, tmpdir
        )

    # ── 5. Summary ───────────────────────────────────────────────────────────

    report["safety_checks"] = safety_checks
    all_safety_passed = (
        all(safety_checks.values())
        and total_invalid_citations == 0
        and citations_validator_ok
        and stale_violations == []
    )
    report["all_safety_checks_passed"] = all_safety_passed
    report["latency_gate_passed"] = report["workloads"].get("latency_compare", {}).get(
        "latency_gate_passed", False
    )
    report["growth_catastrophic_guard_passed"] = report["workloads"].get("growth_cycles", {}).get(
        "growth_catastrophic_guard_passed", False
    )
    report["total_invalid_citations"] = total_invalid_citations
    report["citations_validator_ok"] = citations_validator_ok
    report["stale_verified_current_violations"] = stale_violations
    report["notes"] = [
        "Level0 real-repo sample only: one repo (OpenLocus temp copy), not general performance.",
        "Per-run unique markers avoid self-contamination from docs/script text in temp repo.",
        "All workload mutations occur only in temp copy; --out writes report to caller workspace.",
        "Latency gate is report-only; does not cause exit failure unless safety fails.",
        "Growth catastrophic guard (max(3×rebuild, rebuild+64MiB)) is a backstop, not proof of",
        "long-term bounded growth. 20 cycles observed growth ratio reported separately.",
        "Tantivy deletes are tombstones until merge; index size may grow before compaction.",
    ]

    # Write output
    out = Path(args.out)
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text(json.dumps(report, indent=2) + "\n")
    print(json.dumps(report, indent=2))

    # Cleanup
    if not args.keep_temp:
        shutil.rmtree(tmpdir, ignore_errors=True)

    # Exit code: failure only on safety, not latency/growth gates
    if not all_safety_passed:
        print("\n[R12] SAFETY CHECKS FAILED — exiting with code 1", file=sys.stderr)
        sys.exit(1)


if __name__ == "__main__":
    main()
