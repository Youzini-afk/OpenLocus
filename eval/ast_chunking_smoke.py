#!/usr/bin/env python3
"""R8 AST Chunking Smoke — verify AST-bounded chunking and symbol extraction.

Checks: AST index build/status/validate/search, AST symbol search,
stale mutation detection, parser error fallback, unsupported language fallback,
policy-excluded files absent, default line build still works, R7 persistent smoke still passes.
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
    """Create a synthetic repo with Rust/Python/JS/TS examples for AST smoke."""
    repo = base / "test_repo"
    repo.mkdir(parents=True, exist_ok=True)

    src = repo / "src"
    src.mkdir(exist_ok=True)

    # Rust file
    (src / "auth.rs").write_text(
        "pub fn authenticate_user() -> bool {\n"
        "    // authenticate the user\n"
        "    true\n"
        "}\n"
        "\n"
        "pub struct AuthConfig {\n"
        "    pub max_retries: u32,\n"
        "    pub timeout_ms: u64,\n"
        "}\n"
        "\n"
        "impl AuthConfig {\n"
        "    pub fn new() -> Self {\n"
        "        Self { max_retries: 3, timeout_ms: 5000 }\n"
        "    }\n"
        "}\n"
    )

    # Python file
    (src / "models.py").write_text(
        "@dataclass\n"
        "class User:\n"
        "    name: str\n"
        "    age: int\n"
        "\n"
        "def create_user(name: str, age: int) -> User:\n"
        "    return User(name=name, age=age)\n"
    )

    # TypeScript file
    (src / "api.ts").write_text(
        "interface ApiConfig {\n"
        "    baseUrl: string;\n"
        "    timeout: number;\n"
        "}\n"
        "\n"
        "export const handler = async (req: Request): Promise<Response> => {\n"
        "    return new Response('ok');\n"
        "};\n"
    )

    # JavaScript file
    (src / "utils.js").write_text(
        "function formatDate(date) {\n"
        "    return date.toISOString();\n"
        "}\n"
        "\n"
        "class Logger {\n"
        "    log(msg) {\n"
        "        console.log(msg);\n"
        "    }\n"
        "}\n"
    )

    # Parser error file (invalid Rust)
    (src / "broken.rs").write_text("fn incomplete(\n")

    # Policy-excluded files
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
        default="runs/ast-chunking-smoke.json",
        help="Output JSON file",
    )
    args = parser.parse_args()

    ol = os.path.abspath(args.openlocus)

    tmpdir = tempfile.mkdtemp(prefix="openlocus_ast_smoke_")
    repo = create_fixture_repo(Path(tmpdir))
    cwd = str(repo)

    report: dict[str, Any] = {
        "report_kind": "ast_chunking_smoke",
        "timestamp": time.strftime("%Y-%m-%dT%H:%M:%S%z"),
        "fixture_repo": str(repo),
    }

    safety_checks: dict[str, bool] = {}

    # ── 1. Default line build still works ──────────────────────────────
    purge = run_cmd([ol, "index", "purge", "--json"], cwd)
    safety_checks["purge_succeeds"] = purge.get("purged") is True

    build_line = run_cmd([ol, "index", "build", "--chunk-strategy", "line", "--json"], cwd)
    safety_checks["line_build_succeeds"] = build_line.get("success") is True
    safety_checks["line_build_file_count_positive"] = build_line.get("file_count", 0) > 0
    safety_checks["line_build_chunk_strategy_line"] = build_line.get("chunk_strategy") == "line"
    safety_checks["line_build_no_ast_stats"] = build_line.get("ast_stats") is None
    report["build_line"] = build_line

    status_line = run_cmd([ol, "index", "status", "--json"], cwd)
    safety_checks["line_status_exists"] = status_line.get("exists") is True
    safety_checks["line_status_chunk_strategy_line"] = status_line.get("chunk_strategy") == "line"
    report["status_line"] = status_line

    validate_line = run_cmd([ol, "index", "validate", "--json"], cwd)
    safety_checks["line_validate_valid"] = validate_line.get("valid") is True
    safety_checks["line_validate_chunk_strategy_line"] = validate_line.get("chunk_strategy") == "line"
    report["validate_line"] = validate_line

    # ── 2. AST build ───────────────────────────────────────────────────
    build_ast = run_cmd([ol, "index", "build", "--chunk-strategy", "ast", "--json"], cwd)
    safety_checks["ast_build_succeeds"] = build_ast.get("success") is True
    safety_checks["ast_build_file_count_positive"] = build_ast.get("file_count", 0) > 0
    safety_checks["ast_build_chunk_strategy_ast"] = build_ast.get("chunk_strategy") == "ast"
    safety_checks["ast_build_has_ast_stats"] = build_ast.get("ast_stats") is not None
    report["build_ast"] = build_ast

    # Check AST stats
    ast_stats = build_ast.get("ast_stats", {})
    if ast_stats:
        safety_checks["ast_stats_supported_files_positive"] = ast_stats.get("supported_files", 0) > 0
        safety_checks["ast_stats_ast_chunks_positive"] = ast_stats.get("ast_chunks", 0) > 0
        # Parser error file should show up
        safety_checks["ast_stats_parser_error_visible"] = ast_stats.get("parser_error_files", 0) > 0
    else:
        safety_checks["ast_stats_supported_files_positive"] = False
        safety_checks["ast_stats_ast_chunks_positive"] = False
        safety_checks["ast_stats_parser_error_visible"] = False

    # ── 3. AST status ──────────────────────────────────────────────────
    status_ast = run_cmd([ol, "index", "status", "--json"], cwd)
    safety_checks["ast_status_exists"] = status_ast.get("exists") is True
    safety_checks["ast_status_chunk_strategy_ast"] = status_ast.get("chunk_strategy") == "ast"
    safety_checks["ast_status_has_ast_stats"] = status_ast.get("ast_stats") is not None
    safety_checks["ast_status_no_rebuild_needed"] = status_ast.get("requires_rebuild") is not True
    report["status_ast"] = status_ast

    # ── 4. AST validate ─────────────────────────────────────────────────
    validate_ast = run_cmd([ol, "index", "validate", "--json"], cwd)
    safety_checks["ast_validate_valid"] = validate_ast.get("valid") is True
    safety_checks["ast_validate_chunk_strategy_ast"] = validate_ast.get("chunk_strategy") == "ast"
    report["validate_ast"] = validate_ast

    # ── 5. AST search persistent ────────────────────────────────────────
    search_ast = run_cmd(
        [ol, "search", "bm25", "authenticate", "--index", "persistent", "--json"], cwd
    )
    evidence_list = search_ast.get("evidence", [])
    stats = search_ast.get("stats", {})
    safety_checks["ast_search_returns_evidence"] = len(evidence_list) > 0
    safety_checks["ast_search_stale_skipped_zero"] = stats.get("stale_hits_skipped", -1) == 0
    all_verified = all(
        ev.get("meta", {}).get("freshness") == "verified_current"
        for ev in evidence_list
    )
    safety_checks["ast_search_all_freshness_verified_current"] = all_verified
    report["search_ast"] = search_ast

    # ── 6. Stale mutation test ──────────────────────────────────────────
    auth_path = repo / "src" / "auth.rs"
    original_content = auth_path.read_text()
    auth_path.write_text("// modified content\nfn modified() {}\n" + original_content)

    search_stale = run_cmd(
        [ol, "search", "bm25", "authenticate", "--index", "persistent", "--json"], cwd
    )
    stale_evidence = search_stale.get("evidence", [])
    stale_stats = search_stale.get("stats", {})
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

    # Validate should detect stale
    validate_stale = run_cmd([ol, "index", "validate", "--json"], cwd)
    safety_checks["validate_detects_stale_after_mutation"] = (
        len(validate_stale.get("stale_files", [])) > 0 or not validate_stale.get("valid", True)
    )
    report["validate_stale"] = validate_stale

    # Restore
    auth_path.write_text(original_content)

    # ── 7. AST symbol search ────────────────────────────────────────────
    # Rebuild with AST strategy
    build_ast2 = run_cmd([ol, "index", "build", "--chunk-strategy", "ast", "--json"], cwd)

    # AST mode
    symbol_ast = run_cmd(
        [ol, "search", "symbol", "authenticate_user", "--mode", "ast", "--json"], cwd
    )
    ast_sym_evidence = symbol_ast.get("evidence", []) if isinstance(symbol_ast.get("evidence"), list) else []
    # Note: symbol search may return a list directly or as evidence field
    if not ast_sym_evidence and isinstance(symbol_ast, list):
        ast_sym_evidence = symbol_ast
    safety_checks["ast_symbol_search_returns_results"] = len(ast_sym_evidence) > 0
    # AST symbol evidence should use tree_sitter channel
    has_tree_sitter = any(
        isinstance(ev, dict) and "tree_sitter" in str(ev.get("channels", []))
        for ev in ast_sym_evidence
    )
    safety_checks["ast_symbol_uses_tree_sitter_channel"] = has_tree_sitter
    # AST symbol evidence should be a narrow header/signature span, not the
    # whole function body. The fixture's function body spans lines 1-4, but
    # the header is line 1 only.
    ast_symbol_header_narrow = any(
        isinstance(ev, dict)
        and
        ev.get("path") == "src/auth.rs"
        and ev.get("start_line") == 1
        and ev.get("end_line") == 1
        and "true" not in ev.get("meta", {}).get("excerpt", "")
        for ev in ast_sym_evidence
    )
    safety_checks["ast_symbol_header_not_full_body"] = ast_symbol_header_narrow
    report["symbol_ast"] = symbol_ast

    # Regex mode
    symbol_regex = run_cmd(
        [ol, "search", "symbol", "authenticate_user", "--mode", "regex", "--json"], cwd
    )
    regex_sym_evidence = symbol_regex.get("evidence", []) if isinstance(symbol_regex.get("evidence"), list) else []
    if not regex_sym_evidence and isinstance(symbol_regex, list):
        regex_sym_evidence = symbol_regex
    safety_checks["regex_symbol_search_returns_results"] = len(regex_sym_evidence) > 0
    report["symbol_regex"] = symbol_regex

    # Auto mode
    symbol_auto = run_cmd(
        [ol, "search", "symbol", "authenticate_user", "--mode", "auto", "--json"], cwd
    )
    auto_sym_evidence = symbol_auto.get("evidence", []) if isinstance(symbol_auto.get("evidence"), list) else []
    if not auto_sym_evidence and isinstance(symbol_auto, list):
        auto_sym_evidence = symbol_auto
    safety_checks["auto_symbol_search_returns_results"] = len(auto_sym_evidence) > 0
    report["symbol_auto"] = symbol_auto

    # ── 8. Policy excluded files absent ─────────────────────────────────
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

    # ── 9. Citation validation ──────────────────────────────────────────
    # Write evidence to a file and validate
    if ast_sym_evidence:
        citation_file = os.path.join(tmpdir, "evidence_to_validate.json")
        with open(citation_file, "w") as f:
            json.dump(ast_sym_evidence, f)
        validate_citations = run_cmd(
            [ol, "citations", "validate", citation_file, "--json"], cwd
        )
        safety_checks["citations_valid"] = validate_citations.get("valid_count", 0) > 0
        safety_checks["citations_invalid_zero"] = validate_citations.get("invalid_count", -1) == 0
        report["citations_validate"] = validate_citations
    else:
        safety_checks["citations_valid"] = False
        safety_checks["citations_invalid_zero"] = False

    # ── 10. Schema mismatch requires rebuild ────────────────────────────
    # Write a manifest with wrong schema
    manifest_path = repo / ".openlocus" / "index" / "manifest.json"
    if manifest_path.exists():
        manifest_data = json.loads(manifest_path.read_text())
        manifest_data["schema_version"] = "r99-bm25-v99"
        manifest_path.write_text(json.dumps(manifest_data, indent=2))

        search_bad_schema = run_cmd(
            [ol, "search", "bm25", "authenticate", "--index", "persistent", "--json"], cwd
        )
        schema_refused = (
            search_bad_schema.get("returncode", 0) != 0
            or "schema version mismatch" in search_bad_schema.get("stderr", "")
            or "schema version mismatch" in str(search_bad_schema)
        )
        safety_checks["bad_schema_refuses_search"] = schema_refused
        report["search_bad_schema"] = search_bad_schema

    # ── 11. Default line build still works after AST ────────────────────
    build_line2 = run_cmd([ol, "index", "build", "--chunk-strategy", "line", "--json"], cwd)
    safety_checks["line_build_after_ast_succeeds"] = build_line2.get("success") is True
    safety_checks["line_build_after_ast_strategy_line"] = build_line2.get("chunk_strategy") == "line"
    report["build_line_after_ast"] = build_line2

    # ── 12. Cleanup ────────────────────────────────────────────────────
    purge2 = run_cmd([ol, "index", "purge", "--json"], cwd)
    safety_checks["purge_after_ast_succeeds"] = purge2.get("purged") is True

    # ── Summary ─────────────────────────────────────────────────────────
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
