#!/usr/bin/env python3
"""R9 AST vs Line Persistent BM25 Quality Bakeoff.

Compares persistent BM25 retrieval quality between --chunk-strategy line and
--chunk-strategy ast. Runs each strategy: purge, build, search per query,
score, then produces a combined report with delta, gate, and safety checks.

Does NOT change EvidenceCore, default behaviour, or implement incremental
indexing. Purely a reproducible eval script.
"""

from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
import time
from pathlib import Path
from typing import Any

# Re-use score.py functions directly
_SCRIPT_DIR = Path(__file__).resolve().parent
sys.path.insert(0, str(_SCRIPT_DIR))
import score as scorer  # noqa: E402


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


def compute_metrics(
    predictions: list[dict], gold: dict[str, dict], repo_root: str
) -> dict[str, Any]:
    """Compute full metric suite from score.py on predictions."""
    blake3_mod = scorer.load_blake3()

    total = len(predictions)
    ok = sum(1 for p in predictions if p.get("returncode") == 0)
    latencies = [p.get("latency_ms", 0) for p in predictions]

    metrics: dict[str, Any] = {
        "total_tasks": total,
        "successful": ok,
        "success_rate": ok / total if total else 0.0,
        "avg_latency_ms": sum(latencies) / total if total else 0,
        "max_latency_ms": max(latencies) if latencies else 0,
    }

    metrics["structural_validity"] = scorer.structural_validity(predictions)
    metrics["citation_validity"] = scorer.citation_validity(
        predictions, repo_root, blake3_mod
    )
    metrics["citation_hash_checked"] = blake3_mod is not None
    metrics["citation_validation_mode"] = (
        "path_range_hash" if blake3_mod is not None else "path_range_only"
    )

    # Retrieval metrics (need gold)
    if gold:
        for k in [1, 5, 10]:
            metrics[f"file_recall@{k}"] = scorer.file_recall_at_k(predictions, gold, k)
            metrics[f"file_precision@{k}"] = scorer.file_precision_at_k(
                predictions, gold, k
            )
        metrics["mrr"] = scorer.mrr(predictions, gold)
        for k in [10]:
            metrics[f"line_precision@{k}"] = scorer.line_precision_at_k(
                predictions, gold, k
            )
            metrics[f"line_recall@{k}"] = scorer.line_recall_at_k(predictions, gold, k)
            metrics[f"span_f0.5@{k}"] = scorer.span_f_beta_at_k(
                predictions, gold, k, 0.5
            )
            metrics[f"token_waste_ratio@{k}"] = scorer.token_waste_ratio_at_k(
                predictions, gold, k
            )
            metrics[f"wrong_span_rate@{k}"] = scorer.wrong_span_rate_at_k(
                predictions, gold, k
            )
            metrics[f"zero_overlap_evidence_rate@{k}"] = (
                scorer.zero_overlap_evidence_rate_at_k(predictions, gold, k)
            )

    return metrics


def run_strategy(
    ol: str,
    strategy: str,
    dataset: list[dict],
    gold: dict[str, dict],
    repo_root: str,
    pred_dir: str,
    label: str,
) -> dict[str, Any]:
    """Run purge/build/search for one strategy and return results + predictions."""
    # Purge
    purge = run_cmd([ol, "index", "purge", "--json"], repo_root)
    if purge.get("purged") is not True:
        # May be no index to purge; that's fine if returncode == 0
        pass

    # Build
    build = run_cmd(
        [ol, "index", "build", "--chunk-strategy", strategy, "--json"], repo_root
    )

    # Status
    status = run_cmd([ol, "index", "status", "--json"], repo_root)

    # Validate
    validate = run_cmd([ol, "index", "validate", "--json"], repo_root)

    # Search each query
    predictions: list[dict] = []
    latencies: list[int] = []
    stale_hits_total = 0
    invalid_hits_total = 0
    evidence_total = 0
    stats_keys_present_all = True

    for item in dataset:
        task_id = item.get("task_id", "")
        query = item.get("query", "")

        search = run_cmd(
            [ol, "search", "bm25", query, "--index", "persistent", "--json"],
            repo_root,
        )

        evidence_list = search.get("evidence", [])
        if not isinstance(evidence_list, list):
            evidence_list = []
        stats = search.get("stats", {})
        if not isinstance(stats, dict):
            stats = {}
            stats_keys_present_all = False

        if "stale_hits_skipped" not in stats or "invalid_hits_skipped" not in stats:
            stats_keys_present_all = False

        stale_hits_total += stats.get("stale_hits_skipped", 0)
        invalid_hits_total += stats.get("invalid_hits_skipped", 0)
        evidence_total += len(evidence_list)

        pred = {
            "task_id": task_id,
            "query": query,
            "method": f"bm25-persistent-{strategy}",
            "evidence": evidence_list,
            "latency_ms": search.get("latency_ms", 0),
            "returncode": search.get("returncode", -1),
        }
        predictions.append(pred)
        latencies.append(search.get("latency_ms", 0))

    # Write predictions JSONL
    pred_path = os.path.join(pred_dir, f"r9-{label}-persistent.jsonl")
    Path(pred_path).parent.mkdir(parents=True, exist_ok=True)
    with open(pred_path, "w", encoding="utf-8") as f:
        for pred in predictions:
            f.write(json.dumps(pred) + "\n")

    # Compute metrics
    metrics = compute_metrics(predictions, gold, repo_root)

    # Citation validator (Rust CLI) for both
    citation_file = os.path.join(pred_dir, f"r9-{label}-evidence-all.json")
    all_evidence = []
    for pred in predictions:
        all_evidence.extend(pred.get("evidence", []))
    with open(citation_file, "w", encoding="utf-8") as f:
        json.dump(all_evidence, f)

    citation_validate = run_cmd(
        [ol, "citations", "validate", citation_file, "--json"], repo_root
    )

    return {
        "strategy": strategy,
        "build": build,
        "status": status,
        "validate": validate,
        "predictions_path": pred_path,
        "metrics": metrics,
        "citation_validate": citation_validate,
        "stale_hits_total": stale_hits_total,
        "invalid_hits_total": invalid_hits_total,
        "evidence_count": evidence_total,
        "stats_keys_present_all": stats_keys_present_all,
        "prediction_count": len(predictions),
    }


def compute_delta(line_metrics: dict, ast_metrics: dict) -> dict[str, Any]:
    """Compute delta (ast - line) for key metrics."""
    keys = [
        "file_recall@1", "file_recall@5", "file_recall@10",
        "mrr",
        "span_f0.5@10",
        "token_waste_ratio@10",
        "citation_validity",
        "structural_validity",
        "success_rate",
        "avg_latency_ms",
        "max_latency_ms",
    ]
    delta: dict[str, Any] = {}
    for k in keys:
        lv = line_metrics.get(k)
        av = ast_metrics.get(k)
        if lv is not None and av is not None:
            delta[k] = round(av - lv, 6)
        else:
            delta[k] = None
    # Latency ratio
    if line_metrics.get("avg_latency_ms", 0) > 0:
        delta["latency_ratio"] = round(
            ast_metrics.get("avg_latency_ms", 0)
            / line_metrics.get("avg_latency_ms", 1),
            4,
        )
    else:
        delta["latency_ratio"] = None
    return delta


def evaluate_gate(
    line_result: dict, ast_result: dict, delta: dict
) -> dict[str, Any]:
    """Evaluate quality gate conditions."""
    lm = line_result["metrics"]
    am = ast_result["metrics"]

    gate: dict[str, Any] = {}

    # Both citation_validity == 1.0
    gate["citation_validity_both_1"] = (
        lm.get("citation_validity", 0) == 1.0
        and am.get("citation_validity", 0) == 1.0
    )

    # Both success_rate == 1.0
    gate["success_rate_both_1"] = (
        lm.get("success_rate", 0) == 1.0
        and am.get("success_rate", 0) == 1.0
    )

    # AST FileRecall@5 >= line
    gate["ast_file_recall_at_5_ge_line"] = (
        am.get("file_recall@5", 0) >= lm.get("file_recall@5", 0)
    )

    # AST SpanF0.5@10 >= line
    gate["ast_span_f0_5_at_10_ge_line"] = (
        am.get("span_f0.5@10", 0) >= lm.get("span_f0.5@10", 0)
    )

    # Token waste not worsened (ast <= line, lower is better)
    gate["ast_token_waste_not_worse"] = (
        am.get("token_waste_ratio@10", 1.0) <= lm.get("token_waste_ratio@10", 1.0)
    )

    # Latency ratio <= 1.25
    latency_ratio = delta.get("latency_ratio")
    gate["latency_ratio_le_1_25"] = latency_ratio is not None and latency_ratio <= 1.25

    gate["all_gate_conditions"] = all(
        v is True
        for k, v in gate.items()
        if k != "all_gate_conditions"
    )

    return gate


def evaluate_safety(
    line_result: dict, ast_result: dict
) -> dict[str, Any]:
    """Evaluate safety checks (independent of quality gate)."""
    checks: dict[str, bool] = {}

    # Build succeeds
    checks["line_build_succeeds"] = line_result["build"].get("success") is True
    checks["ast_build_succeeds"] = ast_result["build"].get("success") is True

    # Validate valid
    checks["line_validate_valid"] = line_result["validate"].get("valid") is True
    checks["ast_validate_valid"] = ast_result["validate"].get("valid") is True

    # Status strategy matches
    checks["line_status_strategy_line"] = (
        line_result["status"].get("chunk_strategy") == "line"
    )
    checks["ast_status_strategy_ast"] = (
        ast_result["status"].get("chunk_strategy") == "ast"
    )

    # Predictions should contain at least one materialized evidence item, not
    # merely one task row. The quality scorer handles empty-evidence rows, but
    # the bakeoff safety gate should ensure the retrieval path actually emits
    # evidence for this fixture.
    checks["line_evidence_nonempty"] = line_result["evidence_count"] > 0
    checks["ast_evidence_nonempty"] = ast_result["evidence_count"] > 0

    # Citation validator invalid_count == 0 for both
    checks["line_citation_invalid_zero"] = (
        line_result["citation_validate"].get("invalid_count", -1) == 0
    )
    checks["ast_citation_invalid_zero"] = (
        ast_result["citation_validate"].get("invalid_count", -1) == 0
    )

    # Bakeoff runs should expose skip counters. Nonzero invalid/stale candidate
    # counts are acceptable here because persistent search may inspect and drop
    # invalid candidates before output; the authoritative safety check is the
    # Rust citation validator's invalid_count == 0 on emitted evidence.
    checks["line_search_stats_present"] = line_result["stats_keys_present_all"]
    checks["ast_search_stats_present"] = ast_result["stats_keys_present_all"]
    checks["line_skip_counters_nonnegative"] = (
        line_result["stale_hits_total"] >= 0 and line_result["invalid_hits_total"] >= 0
    )
    checks["ast_skip_counters_nonnegative"] = (
        ast_result["stale_hits_total"] >= 0 and ast_result["invalid_hits_total"] >= 0
    )

    # Strategy defaults remain explicit (line is default, ast requires opt-in)
    checks["line_build_strategy_explicit"] = (
        line_result["build"].get("chunk_strategy") == "line"
    )
    checks["ast_build_strategy_explicit"] = (
        ast_result["build"].get("chunk_strategy") == "ast"
    )

    checks["all_safety_checks_passed"] = all(checks.values())

    return checks


def main() -> None:
    parser = argparse.ArgumentParser(
        description="R9 AST vs Line Persistent BM25 Quality Bakeoff"
    )
    parser.add_argument(
        "--openlocus",
        default="target/debug/openlocus",
        help="Path to openlocus binary",
    )
    parser.add_argument(
        "--dataset",
        default="fixtures/r2.jsonl",
        help="Dataset JSONL with task_id, query, method, gold_paths, gold_lines",
    )
    parser.add_argument(
        "--out",
        default="runs/ast-quality-bakeoff.json",
        help="Output JSON report path",
    )
    parser.add_argument(
        "--pred-dir",
        default="runs",
        help="Directory for prediction JSONL files",
    )
    args = parser.parse_args()

    ol = os.path.abspath(args.openlocus)
    repo_root = os.getcwd()

    # Load dataset
    dataset = scorer.load_predictions(args.dataset)  # same format: JSONL
    gold = scorer.load_dataset(args.dataset)

    if not dataset:
        print("ERROR: empty dataset", file=sys.stderr)
        sys.exit(1)

    report: dict[str, Any] = {
        "report_kind": "ast_quality_bakeoff",
        "timestamp": time.strftime("%Y-%m-%dT%H:%M:%S%z"),
        "openlocus": ol,
        "dataset": args.dataset,
        "dataset_size": len(dataset),
    }

    # ── Line strategy ────────────────────────────────────────────────────
    print("=== R9 Bakeoff: line strategy ===")
    line_result = run_strategy(
        ol, "line", dataset, gold, repo_root, args.pred_dir, "line"
    )
    print(f"  Line metrics: {json.dumps(line_result['metrics'], indent=2)}")

    # ── AST strategy ─────────────────────────────────────────────────────
    print("=== R9 Bakeoff: ast strategy ===")
    ast_result = run_strategy(
        ol, "ast", dataset, gold, repo_root, args.pred_dir, "ast"
    )
    print(f"  AST metrics: {json.dumps(ast_result['metrics'], indent=2)}")

    # ── Delta ────────────────────────────────────────────────────────────
    delta = compute_delta(line_result["metrics"], ast_result["metrics"])
    print(f"  Delta: {json.dumps(delta, indent=2)}")

    # ── Quality gate ─────────────────────────────────────────────────────
    gate = evaluate_gate(line_result, ast_result, delta)
    print(f"  Quality gate: {json.dumps(gate, indent=2)}")

    # ── Safety checks ────────────────────────────────────────────────────
    safety = evaluate_safety(line_result, ast_result)
    print(f"  Safety checks: {json.dumps(safety, indent=2)}")

    # ── Final report ─────────────────────────────────────────────────────
    report["line"] = line_result
    report["ast"] = ast_result
    report["delta"] = delta
    report["quality_gate"] = gate
    report["safety_checks"] = safety
    report["quality_gate_passed"] = gate.get("all_gate_conditions", False)
    report["safety_checks_passed"] = safety.get("all_safety_checks_passed", False)

    # Notes
    notes: list[str] = [
        "R9 bakeoff compares persistent BM25 quality between line and ast chunk strategies.",
        "Fixture is R2 small self-referential; results are not generalisable beyond this fixture.",
        "Negative results are valid: if AST does not improve quality, line remains default.",
        f"Line predictions: {line_result['predictions_path']}",
        f"AST predictions: {ast_result['predictions_path']}",
    ]

    # Add gate-specific notes
    if not gate.get("ast_file_recall_at_5_ge_line", True):
        notes.append(
            "AST FileRecall@5 < line: AST does not improve file-level recall at k=5."
        )
    if not gate.get("ast_span_f0_5_at_10_ge_line", True):
        notes.append(
            "AST SpanF0.5@10 < line: AST does not improve span-level precision/recall."
        )
    if not gate.get("ast_token_waste_not_worse", True):
        notes.append(
            "AST token_waste_ratio@10 > line: AST worsens token waste."
        )
    latency_ratio = delta.get("latency_ratio")
    if latency_ratio is not None and latency_ratio > 1.25:
        notes.append(
            f"AST latency ratio {latency_ratio:.2f} > 1.25: AST is significantly slower."
        )

    report["notes"] = notes

    # Write report
    out = Path(args.out)
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text(json.dumps(report, indent=2) + "\n")
    print(f"\n=== R9 Report written to {args.out} ===")

    # Cleanup: purge index so we don't leave stale state
    run_cmd([ol, "index", "purge", "--json"], repo_root)

    # Exit based on safety checks, not quality gate
    if not report["safety_checks_passed"]:
        print("ERROR: safety checks failed!", file=sys.stderr)
        sys.exit(1)


if __name__ == "__main__":
    main()
