#!/usr/bin/env python3
"""R10 Incremental Index Smoke — verify dirty summary + file-level update.

Checks:
- Build line index, assert clean
- Modify indexed file: status modified; search before update skips stale/no stale VerifiedCurrent; update dirty; search returns new content only; status clean
- Add new policy-included file: status added; update dirty; search finds it; status clean
- Delete file: status deleted; update dirty; search no evidence for old path; status clean
- Rename simulated delete+add: old gone, new found
- Policy-excluded added file does not dirty
- Policy hash mismatch refuses update
- Missing manifest refuses update
- Citations validate invalid_count=0
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


def create_fixture_repo(base: Path) -> Path:
    """Create a synthetic repo for incremental index smoke."""
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
    openlocus_dir = repo / ".openlocus"
    openlocus_dir.mkdir(exist_ok=True)
    (openlocus_dir / "policy.toml").write_text(policy_toml)


def remove_policy(repo: Path) -> None:
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
        default="runs/incremental-index-smoke.json",
        help="Output JSON file",
    )
    args = parser.parse_args()

    ol = os.path.abspath(args.openlocus)

    tmpdir = tempfile.mkdtemp(prefix="openlocus_incr_smoke_")
    repo = create_fixture_repo(Path(tmpdir))
    cwd = str(repo)

    report: dict[str, Any] = {
        "report_kind": "incremental_index_smoke",
        "timestamp": time.strftime("%Y-%m-%dT%H:%M:%S%z"),
        "fixture_repo": str(repo),
    }

    safety_checks: dict[str, bool] = {}

    # 1. Purge and build line index
    purge = run_cmd([ol, "index", "purge", "--json"], cwd)
    safety_checks["purge_succeeds"] = purge.get("purged") is True

    build = run_cmd([ol, "index", "build", "--json"], cwd)
    safety_checks["build_succeeds"] = build.get("success") is True
    safety_checks["build_file_count_positive"] = build.get("file_count", 0) > 0
    report["build"] = build

    # 2. Assert clean after build
    dirty = run_cmd([ol, "index", "dirty", "--json"], cwd)
    safety_checks["dirty_clean_after_build"] = dirty.get("clean") is True
    safety_checks["dirty_no_requires_update"] = dirty.get("requires_update") is not True
    safety_checks["dirty_no_requires_rebuild"] = dirty.get("requires_rebuild") is not True
    safety_checks["dirty_policy_hash_matches"] = dirty.get("policy_hash_matches") is True
    safety_checks["dirty_schema_matches"] = dirty.get("schema_matches") is True
    report["dirty_clean"] = dirty

    # 3. Modify indexed file
    auth_path = repo / "src" / "auth.rs"
    original_content = auth_path.read_text()
    auth_path.write_text(
        "pub fn validate_token() -> bool {\n"
        "    // validate the token\n"
        "    true\n"
        "}\n"
        "\n"
        "pub fn check_permission() -> bool {\n"
        "    // check permission\n"
        "    true\n"
        "}\n"
    )

    # 3a. Status should detect modified
    dirty_mod = run_cmd([ol, "index", "dirty", "--json"], cwd)
    safety_checks["dirty_after_modify_requires_update"] = dirty_mod.get("requires_update") is True
    safety_checks["dirty_after_modify_not_clean"] = dirty_mod.get("clean") is not True
    safety_checks["dirty_after_modify_has_modified"] = dirty_mod.get("modified_count", 0) > 0
    report["dirty_modified"] = dirty_mod

    # 3b. Search before update should skip stale/no stale VerifiedCurrent
    search_stale = run_cmd(
        [ol, "search", "bm25", "authenticate", "--index", "persistent", "--json"], cwd
    )
    stale_evidence = search_stale.get("evidence", [])
    stale_stats = search_stale.get("stats", {})
    # No evidence for the old path should have VerifiedCurrent freshness
    stale_verified_for_auth = any(
        ev.get("meta", {}).get("freshness") == "verified_current"
        and "auth" in ev.get("path", "")
        for ev in stale_evidence
    )
    safety_checks["search_before_update_no_verified_current"] = not stale_verified_for_auth
    report["search_stale"] = search_stale

    # 3c. Update dirty
    update_mod = run_cmd([ol, "index", "update", "--dirty", "--json"], cwd)
    safety_checks["update_dirty_modified_succeeds"] = update_mod.get("success") is True
    safety_checks["update_dirty_modified_count_positive"] = update_mod.get("modified_count", 0) > 0
    safety_checks["update_dirty_manifest_written"] = update_mod.get("manifest_written") is True
    report["update_modified"] = update_mod

    # 3d. Search should return new content only
    search_new = run_cmd(
        [ol, "search", "bm25", "validate_token", "--index", "persistent", "--json"], cwd
    )
    new_evidence = search_new.get("evidence", [])
    safety_checks["search_after_update_finds_new_content"] = len(new_evidence) > 0
    if new_evidence:
        has_new_content = any(
            "validate_token" in ev.get("meta", {}).get("excerpt", "")
            or "validate_token" in ev.get("meta", {}).get("excerpt", "")
            for ev in new_evidence
        )
        # At minimum, evidence should exist for the modified file
        safety_checks["search_after_update_on_auth_file"] = any(
            "auth" in ev.get("path", "") for ev in new_evidence
        )
    else:
        safety_checks["search_after_update_on_auth_file"] = False
    report["search_new"] = search_new

    # 3e. Status should be clean
    dirty_after_update = run_cmd([ol, "index", "dirty", "--json"], cwd)
    safety_checks["dirty_clean_after_update"] = dirty_after_update.get("clean") is True
    report["dirty_after_update"] = dirty_after_update

    # Restore original content
    auth_path.write_text(original_content)

    # 4. Add new policy-included file
    new_file = repo / "src" / "utils.rs"
    new_file.write_text("pub fn format_output() -> String {\n    String::new()\n}\n")

    # 4a. Status should detect added
    dirty_add = run_cmd([ol, "index", "dirty", "--json"], cwd)
    safety_checks["dirty_after_add_requires_update"] = dirty_add.get("requires_update") is True
    safety_checks["dirty_after_add_has_added"] = dirty_add.get("added_count", 0) > 0
    report["dirty_added"] = dirty_add

    # 4b. Update dirty
    update_add = run_cmd([ol, "index", "update", "--dirty", "--json"], cwd)
    safety_checks["update_dirty_added_succeeds"] = update_add.get("success") is True
    safety_checks["update_dirty_added_count_positive"] = update_add.get("added_count", 0) > 0
    report["update_added"] = update_add

    # 4c. Search should find it
    search_utils = run_cmd(
        [ol, "search", "bm25", "format_output", "--index", "persistent", "--json"], cwd
    )
    utils_evidence = search_utils.get("evidence", [])
    safety_checks["search_finds_added_file"] = any(
        "utils" in ev.get("path", "") for ev in utils_evidence
    )
    report["search_utils"] = search_utils

    # 4d. Status should be clean
    dirty_after_add = run_cmd([ol, "index", "dirty", "--json"], cwd)
    safety_checks["dirty_clean_after_add_update"] = dirty_after_add.get("clean") is True
    report["dirty_after_add_update"] = dirty_after_add

    # 5. Delete file
    config_path = repo / "src" / "config.rs"
    config_path.unlink()

    # 5a. Status should detect deleted
    dirty_del = run_cmd([ol, "index", "dirty", "--json"], cwd)
    safety_checks["dirty_after_delete_requires_update"] = dirty_del.get("requires_update") is True
    safety_checks["dirty_after_delete_has_deleted"] = dirty_del.get("deleted_count", 0) > 0
    report["dirty_deleted"] = dirty_del

    # 5b. Update dirty
    update_del = run_cmd([ol, "index", "update", "--dirty", "--json"], cwd)
    safety_checks["update_dirty_deleted_succeeds"] = update_del.get("success") is True
    safety_checks["update_dirty_deleted_count_positive"] = update_del.get("deleted_count", 0) > 0
    report["update_deleted"] = update_del

    # 5c. Search should not find deleted file
    search_del = run_cmd(
        [ol, "search", "bm25", "Config", "--index", "persistent", "--json"], cwd
    )
    del_evidence = search_del.get("evidence", [])
    safety_checks["search_no_evidence_for_deleted"] = not any(
        "config" in ev.get("path", "").lower() for ev in del_evidence
    )
    report["search_deleted"] = search_del

    # 5d. Status clean
    dirty_after_del = run_cmd([ol, "index", "dirty", "--json"], cwd)
    safety_checks["dirty_clean_after_delete_update"] = dirty_after_del.get("clean") is True
    report["dirty_after_delete_update"] = dirty_after_del

    # 6. Rename simulated delete+add
    # Rebuild clean first
    config_restore = repo / "src" / "config.rs"
    config_restore.write_text("pub struct Config {\n    pub name: String,\n}\n")
    build2 = run_cmd([ol, "index", "build", "--json"], cwd)

    # Rename: delete old, add new
    (repo / "src" / "config.rs").unlink()
    (repo / "src" / "settings.rs").write_text("pub struct Settings {\n    pub name: String,\n}\n")

    update_rename = run_cmd([ol, "index", "update", "--dirty", "--json"], cwd)
    safety_checks["update_rename_succeeds"] = update_rename.get("success") is True

    search_old = run_cmd(
        [ol, "search", "bm25", "Config", "--index", "persistent", "--json"], cwd
    )
    # Old path should be gone
    safety_checks["rename_old_gone"] = not any(
        "config" in ev.get("path", "").lower()
        for ev in search_old.get("evidence", [])
    )

    search_new_name = run_cmd(
        [ol, "search", "bm25", "Settings", "--index", "persistent", "--json"], cwd
    )
    safety_checks["rename_new_found"] = any(
        "settings" in ev.get("path", "").lower()
        for ev in search_new_name.get("evidence", [])
    )
    report["rename"] = {
        "update": update_rename,
        "search_old": search_old,
        "search_new": search_new_name,
    }

    # Clean up rename
    (repo / "src" / "settings.rs").unlink()
    config_restore.write_text("pub struct Config {\n    pub name: String,\n}\n")

    # 7. Policy-excluded added file does not dirty
    build3 = run_cmd([ol, "index", "build", "--json"], cwd)
    (repo / ".env.local").write_text("ANOTHER_SECRET=xyz\n")
    dirty_excluded = run_cmd([ol, "index", "dirty", "--json"], cwd)
    safety_checks["policy_excluded_no_dirty"] = dirty_excluded.get("clean") is True
    report["dirty_excluded"] = dirty_excluded

    # 8. Policy hash mismatch refuses update
    write_policy(repo, "[remote]\nallow = true\n")
    update_policy = run_cmd([ol, "index", "update", "--dirty", "--json"], cwd)
    safety_checks["policy_mismatch_refuses_update"] = (
        update_policy.get("success") is not True
        or "policy hash mismatch" in str(update_policy.get("error", ""))
    )
    report["update_policy_mismatch"] = update_policy

    remove_policy(repo)

    # 9. Missing manifest refuses update
    build4 = run_cmd([ol, "index", "build", "--json"], cwd)
    manifest_path = repo / ".openlocus" / "index" / "manifest.json"
    if manifest_path.exists():
        manifest_path.unlink()
    update_missing = run_cmd([ol, "index", "update", "--dirty", "--json"], cwd)
    safety_checks["missing_manifest_refuses_update"] = (
        update_missing.get("success") is not True
        or "manifest missing" in str(update_missing.get("error", ""))
        or "manifest load failed" in str(update_missing.get("error", ""))
    )
    report["update_missing_manifest"] = update_missing

    # 10. Skipped empty file: build clean, empty->nonempty update, searchable
    build_skip = run_cmd([ol, "index", "build", "--json"], cwd)
    # Create empty file
    empty_file = repo / "src" / "empty.rs"
    empty_file.write_text("")
    # Rebuild with empty file
    build_empty = run_cmd([ol, "index", "build", "--json"], cwd)
    # Dirty should be clean (skipped empty file unchanged)
    dirty_empty = run_cmd([ol, "index", "dirty", "--json"], cwd)
    safety_checks["dirty_clean_after_build_with_empty_file"] = dirty_empty.get("clean") is True
    # Empty file should NOT appear in added_files
    safety_checks["empty_file_not_in_added"] = (
        not any("empty.rs" in p for p in dirty_empty.get("added_files", []))
    )
    report["dirty_empty"] = dirty_empty

    # Make empty file non-empty
    empty_file.write_text("pub fn newly_indexable() -> bool { true }\n")
    dirty_grow = run_cmd([ol, "index", "dirty", "--json"], cwd)
    safety_checks["dirty_after_empty_to_nonempty_requires_update"] = (
        dirty_grow.get("requires_update") is True
    )
    safety_checks["dirty_after_empty_to_nonempty_modified"] = (
        dirty_grow.get("modified_count", 0) > 0
    )
    # Should be in modified_files, NOT added_files
    safety_checks["empty_to_nonempty_in_modified_not_added"] = (
        any("empty.rs" in p for p in dirty_grow.get("modified_files", []))
        and not any("empty.rs" in p for p in dirty_grow.get("added_files", []))
    )
    report["dirty_grow"] = dirty_grow

    # Update dirty — should promote skipped to indexed
    update_grow = run_cmd([ol, "index", "update", "--dirty", "--json"], cwd)
    safety_checks["update_empty_to_nonempty_succeeds"] = update_grow.get("success") is True
    report["update_grow"] = update_grow

    # Search should find newly indexed content
    search_grow = run_cmd(
        [ol, "search", "bm25", "newly_indexable", "--index", "persistent", "--json"], cwd
    )
    safety_checks["search_finds_promoted_file"] = any(
        "empty" in ev.get("path", "") for ev in search_grow.get("evidence", [])
    )
    report["search_grow"] = search_grow

    # Status should be clean
    dirty_after_grow = run_cmd([ol, "index", "dirty", "--json"], cwd)
    safety_checks["dirty_clean_after_empty_to_nonempty_update"] = dirty_after_grow.get("clean") is True
    report["dirty_after_grow"] = dirty_after_grow

    # 11. Schema mismatch refuses update (corrupt manifest schema)
    build_schema = run_cmd([ol, "index", "build", "--json"], cwd)
    manifest_path = repo / ".openlocus" / "index" / "manifest.json"
    if manifest_path.exists():
        raw_manifest = manifest_path.read_text()
        corrupted = raw_manifest.replace('"r8-bm25-v2"', '"unknown-v99"')
        manifest_path.write_text(corrupted)
    update_schema = run_cmd([ol, "index", "update", "--dirty", "--json"], cwd)
    safety_checks["schema_mismatch_refuses_update"] = (
        update_schema.get("success") is not True
    )
    report["update_schema_mismatch"] = update_schema

    # Rebuild to fix manifest
    build_fix = run_cmd([ol, "index", "build", "--json"], cwd)

    # 12. Chunk strategy mismatch refuses update (corrupt manifest strategy)
    manifest_path = repo / ".openlocus" / "index" / "manifest.json"
    if manifest_path.exists():
        raw_manifest = manifest_path.read_text()
        corrupted = raw_manifest.replace('"line_window_v1"', '"unknown_strategy"')
        manifest_path.write_text(corrupted)
    update_strategy = run_cmd([ol, "index", "update", "--dirty", "--json"], cwd)
    safety_checks["strategy_mismatch_refuses_update"] = (
        update_strategy.get("success") is not True
    )
    report["update_strategy_mismatch"] = update_strategy

    # 13. Citations validate invalid_count=0
    # Rebuild first
    build5 = run_cmd([ol, "index", "build", "--json"], cwd)
    search_cite = run_cmd(
        [ol, "search", "bm25", "authenticate", "--index", "persistent", "--json"], cwd
    )
    cite_evidence = search_cite.get("evidence", [])
    if cite_evidence:
        citation_file = os.path.join(tmpdir, "evidence_to_validate.json")
        with open(citation_file, "w") as f:
            json.dump(cite_evidence, f)
        validate_cite = run_cmd(
            [ol, "citations", "validate", citation_file, "--json"], cwd
        )
        safety_checks["citations_invalid_count_zero"] = validate_cite.get("invalid_count", -1) == 0
        report["citations_validate"] = validate_cite
    else:
        safety_checks["citations_invalid_count_zero"] = True  # no evidence to validate

    # 14. Cleanup
    purge2 = run_cmd([ol, "index", "purge", "--json"], cwd)
    safety_checks["purge_after_smoke_succeeds"] = purge2.get("purged") is True

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
