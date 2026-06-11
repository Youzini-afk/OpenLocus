#!/usr/bin/env python3
"""R2 retrieval scorer.

Computes:
- structural_validity: fraction of evidence with valid path, content_sha, line range.
- citation_validity: fraction of evidence where path exists, range is in-bounds,
  and, when the optional Python `blake3` package is installed, content_sha
  matches the current file hash.
- FileRecall@1/5/10, FilePrecision@5/10, MRR
- LinePrecision@10, LineRecall@10, SpanF0.5@10
- token_waste_ratio@10, wrong_span_rate@10, zero_overlap_evidence_rate@10

Metric semantics:
- file_recall@k: fraction of tasks where at least 1 gold file appears in top-k.
- wrong_span_rate@k: fraction of evidence on a gold file with zero line overlap.
- zero_overlap_evidence_rate@k: fraction of all top-k evidence with zero line
  overlap with any gold span (broader than wrong_span_rate which only counts
  evidence on gold files).
"""

from __future__ import annotations

import argparse
import importlib
import json
from pathlib import Path


def load_predictions(path: str) -> list[dict]:
    results = []
    for line in Path(path).read_text(encoding="utf-8").splitlines():
        if not line.strip():
            continue
        results.append(json.loads(line))
    return results


def load_dataset(path: str) -> dict[str, dict]:
    """Load dataset indexed by task_id."""
    tasks = {}
    for line in Path(path).read_text(encoding="utf-8").splitlines():
        if not line.strip():
            continue
        item = json.loads(line)
        tasks[item["task_id"]] = item
    return tasks


# ── File-level metrics ────────────────────────────────────────────────


def file_recall_at_k(
    predictions: list[dict], gold: dict[str, dict], k: int
) -> float:
    """Fraction of tasks where at least 1 gold file appears in top-k."""
    hits = 0
    total = 0
    for pred in predictions:
        task_id = pred["task_id"]
        if task_id not in gold:
            continue
        total += 1
        gold_paths = set(gold[task_id].get("gold_paths", []))
        pred_paths = set(e.get("path", "") for e in pred["evidence"][:k])
        if gold_paths & pred_paths:
            hits += 1
    return hits / total if total else 0.0


def file_precision_at_k(
    predictions: list[dict], gold: dict[str, dict], k: int
) -> float:
    """Precision of predicted files at k against gold files."""
    total_precision = 0.0
    total = 0
    for pred in predictions:
        task_id = pred["task_id"]
        if task_id not in gold:
            continue
        total += 1
        gold_paths = set(gold[task_id].get("gold_paths", []))
        pred_paths = [e.get("path", "") for e in pred["evidence"][:k]]
        if not pred_paths:
            continue
        correct = sum(1 for p in pred_paths if p in gold_paths)
        total_precision += correct / len(pred_paths)
    return total_precision / total if total else 0.0


def mrr(predictions: list[dict], gold: dict[str, dict]) -> float:
    """Mean Reciprocal Rank based on file match."""
    total_rr = 0.0
    total = 0
    for pred in predictions:
        task_id = pred["task_id"]
        if task_id not in gold:
            continue
        total += 1
        gold_paths = set(gold[task_id].get("gold_paths", []))
        for rank, e in enumerate(pred["evidence"], 1):
            if e.get("path", "") in gold_paths:
                total_rr += 1.0 / rank
                break
    return total_rr / total if total else 0.0


# ── Line-level metrics ────────────────────────────────────────────────


def line_precision_at_k(
    predictions: list[dict], gold: dict[str, dict], k: int
) -> float:
    """Of top-k evidence lines, what fraction are in gold?"""
    total_prec = 0.0
    total = 0
    for pred in predictions:
        task_id = pred["task_id"]
        if task_id not in gold:
            continue
        total += 1
        gold_lines = build_gold_line_set(gold[task_id])
        pred_lines = set()
        all_pred_lines = set()
        for e in pred["evidence"][:k]:
            start = e.get("start_line", 0)
            end = e.get("end_line", 0)
            path = e.get("path", "")
            for ln in range(start, end + 1):
                all_pred_lines.add((path, ln))
                if (path, ln) in gold_lines:
                    pred_lines.add((path, ln))
        if all_pred_lines:
            total_prec += len(pred_lines) / len(all_pred_lines)
    return total_prec / total if total else 0.0


def line_recall_at_k(
    predictions: list[dict], gold: dict[str, dict], k: int
) -> float:
    """Of gold lines, what fraction appear in top-k evidence spans?"""
    total_recall = 0.0
    total = 0
    for pred in predictions:
        task_id = pred["task_id"]
        if task_id not in gold:
            continue
        total += 1
        gold_lines = build_gold_line_set(gold[task_id])
        if not gold_lines:
            total_recall += 1.0
            continue
        pred_lines = set()
        for e in pred["evidence"][:k]:
            start = e.get("start_line", 0)
            end = e.get("end_line", 0)
            path = e.get("path", "")
            for ln in range(start, end + 1):
                pred_lines.add((path, ln))
        recalled = gold_lines & pred_lines
        total_recall += len(recalled) / len(gold_lines)
    return total_recall / total if total else 0.0


def span_f_beta_at_k(
    predictions: list[dict], gold: dict[str, dict], k: int, beta: float = 0.5
) -> float:
    """Span F-beta combining line precision and recall."""
    prec = line_precision_at_k(predictions, gold, k)
    rec = line_recall_at_k(predictions, gold, k)
    if prec + rec == 0:
        return 0.0
    beta2 = beta * beta
    return (1 + beta2) * prec * rec / (beta2 * prec + rec)


def token_waste_ratio_at_k(
    predictions: list[dict], gold: dict[str, dict], k: int
) -> float:
    """Ratio of non-gold lines to total lines in top-k (lower is better)."""
    total_waste = 0.0
    total = 0
    for pred in predictions:
        task_id = pred["task_id"]
        if task_id not in gold:
            continue
        total += 1
        gold_lines = build_gold_line_set(gold[task_id])
        all_pred_lines = 0
        non_gold_lines = 0
        for e in pred["evidence"][:k]:
            start = e.get("start_line", 0)
            end = e.get("end_line", 0)
            path = e.get("path", "")
            for ln in range(start, end + 1):
                all_pred_lines += 1
                if (path, ln) not in gold_lines:
                    non_gold_lines += 1
        if all_pred_lines > 0:
            total_waste += non_gold_lines / all_pred_lines
    return total_waste / total if total else 0.0


def wrong_span_rate_at_k(
    predictions: list[dict], gold: dict[str, dict], k: int
) -> float:
    """Fraction of evidence on a gold file with zero line overlap with gold spans."""
    total_wrong = 0
    total = 0
    for pred in predictions:
        task_id = pred["task_id"]
        if task_id not in gold:
            continue
        gold_paths = set(gold[task_id].get("gold_paths", []))
        gold_lines = build_gold_line_set(gold[task_id])
        for e in pred["evidence"][:k]:
            path = e.get("path", "")
            if path not in gold_paths:
                continue  # only count evidence on gold files
            total += 1
            start = e.get("start_line", 0)
            end = e.get("end_line", 0)
            has_overlap = any(
                (path, ln) in gold_lines for ln in range(start, end + 1)
            )
            if not has_overlap:
                total_wrong += 1
    return total_wrong / total if total else 0.0


def zero_overlap_evidence_rate_at_k(
    predictions: list[dict], gold: dict[str, dict], k: int
) -> float:
    """Fraction of all top-k evidence with zero line overlap with any gold span."""
    total_wrong = 0
    total = 0
    for pred in predictions:
        task_id = pred["task_id"]
        if task_id not in gold:
            continue
        gold_lines = build_gold_line_set(gold[task_id])
        for e in pred["evidence"][:k]:
            total += 1
            start = e.get("start_line", 0)
            end = e.get("end_line", 0)
            path = e.get("path", "")
            has_overlap = any(
                (path, ln) in gold_lines for ln in range(start, end + 1)
            )
            if not has_overlap:
                total_wrong += 1
    return total_wrong / total if total else 0.0


# ── Validity metrics ──────────────────────────────────────────────────


def structural_validity(predictions: list[dict]) -> float:
    """Fraction of evidence with valid path, content_sha, and line range."""
    total = 0
    valid = 0
    for pred in predictions:
        for e in pred["evidence"]:
            total += 1
            if (
                e.get("path")
                and e.get("content_sha")
                and e.get("start_line", 0) >= 1
                and e.get("start_line", 0) <= e.get("end_line", 0)
            ):
                valid += 1
    return valid / total if total else 0.0


def load_blake3():
    """Load optional Python blake3 module for hash-backed citation checks."""
    try:
        return importlib.import_module("blake3")
    except ImportError:
        return None


def citation_validity(predictions: list[dict], repo_root: str, blake3_mod=None) -> float:
    """Citation validity for Python eval.

    Always verifies path existence and line range bounds. If the optional Python
    `blake3` package is installed, also verifies content_sha against current
    file bytes. For unconditional hash/excerpt validation, use the Rust CLI:
    `openlocus citations validate`.
    """
    total = 0
    valid = 0
    root = Path(repo_root)
    for pred in predictions:
        for e in pred["evidence"]:
            total += 1
            path = e.get("path", "")
            sha = e.get("content_sha", "")
            start = e.get("start_line", 0)
            end = e.get("end_line", 0)

            # Structural check
            if not path or not sha or start < 1 or start > end:
                continue

            # File must exist
            full_path = root / path
            if not full_path.exists():
                continue

            # Read file and compute hash
            try:
                content = full_path.read_bytes()
            except OSError:
                continue

            try:
                text = content.decode("utf-8")
            except UnicodeDecodeError:
                continue

            lines = text.splitlines()
            total_lines = len(lines)

            # Range in bounds
            if end > total_lines:
                continue

            # Verify content_sha when optional Python blake3 is available.
            if blake3_mod is not None:
                computed_sha = blake3_mod.blake3(content).hexdigest()
                if computed_sha != sha:
                    continue

            valid += 1

    return valid / total if total else 0.0


# ── Gold line set builder ─────────────────────────────────────────────


def build_gold_line_set(task: dict) -> set[tuple[str, int]]:
    """Build set of (path, line_number) from gold_lines field."""
    result = set()
    gold_paths = task.get("gold_paths", [])
    gold_lines_list = task.get("gold_lines", [])
    for i, path in enumerate(gold_paths):
        if i < len(gold_lines_list):
            entry = gold_lines_list[i]
            # entry can be [start, end] or [[start, end], ...]
            if isinstance(entry, list):
                if len(entry) >= 2 and isinstance(entry[0], list):
                    # Nested: [[start, end], [start, end], ...]
                    for span in entry:
                        if isinstance(span, list) and len(span) >= 2:
                            for ln in range(span[0], span[1] + 1):
                                result.add((path, ln))
                else:
                    # Flat: [start, end]
                    for ln in range(entry[0], entry[1] + 1):
                        result.add((path, ln))
    return result


# ── Main ──────────────────────────────────────────────────────────────


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--pred", required=True)
    parser.add_argument("--dataset", default=None, help="Dataset for gold spans")
    parser.add_argument(
        "--repo-root", default=".", help="Repo root for citation validation"
    )
    args = parser.parse_args()

    predictions = load_predictions(args.pred)
    blake3_mod = load_blake3()

    if not predictions:
        print(json.dumps({"error": "no predictions"}, indent=2))
        return

    # Basic counts
    total = len(predictions)
    ok = sum(1 for p in predictions if p.get("returncode") == 0)
    avg_latency = (
        sum(p.get("latency_ms", 0) for p in predictions) / total if total else 0
    )

    metrics = {
        "total_tasks": total,
        "successful": ok,
        "success_rate": ok / total if total else 0.0,
        "avg_latency_ms": avg_latency,
    }

    # Structural validity (no file I/O needed)
    metrics["structural_validity"] = structural_validity(predictions)

    # Citation validity (hash-backed when optional Python blake3 is available).
    metrics["citation_validity"] = citation_validity(
        predictions, args.repo_root, blake3_mod
    )
    metrics["citation_hash_checked"] = blake3_mod is not None
    metrics["citation_validation_mode"] = (
        "path_range_hash" if blake3_mod is not None else "path_range_only"
    )

    # If dataset provided, compute retrieval metrics
    if args.dataset:
        gold = load_dataset(args.dataset)
        for k in [1, 5, 10]:
            metrics[f"file_recall@{k}"] = file_recall_at_k(predictions, gold, k)
            metrics[f"file_precision@{k}"] = file_precision_at_k(predictions, gold, k)
        metrics["mrr"] = mrr(predictions, gold)
        for k in [10]:
            metrics[f"line_precision@{k}"] = line_precision_at_k(predictions, gold, k)
            metrics[f"line_recall@{k}"] = line_recall_at_k(predictions, gold, k)
            metrics[f"span_f0.5@{k}"] = span_f_beta_at_k(predictions, gold, k, 0.5)
            metrics[f"token_waste_ratio@{k}"] = token_waste_ratio_at_k(
                predictions, gold, k
            )
            metrics[f"wrong_span_rate@{k}"] = wrong_span_rate_at_k(
                predictions, gold, k
            )
            metrics[f"zero_overlap_evidence_rate@{k}"] = (
                zero_overlap_evidence_rate_at_k(predictions, gold, k)
            )

    print(json.dumps(metrics, indent=2))


if __name__ == "__main__":
    main()
