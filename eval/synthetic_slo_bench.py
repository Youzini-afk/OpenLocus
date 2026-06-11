#!/usr/bin/env python3
"""R10 Synthetic SLO Benchmark — deterministic generated repo ≥1000 files.

Level0 synthetic only. Do not make broad performance claims.
Measures: build_ms, dirty status latency, persistent CLI search latency,
open-once bench-warm query latency, and one-file update latency. Validates
no invalid citations.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import random
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


def generate_repo(base: Path, seed: int = 42, num_files: int = 1000) -> Path:
    """Generate a deterministic synthetic repo with ≥num_files files."""
    rng = random.Random(seed)
    repo = base / "slo_bench_repo"
    repo.mkdir(parents=True, exist_ok=True)

    # Create .openlocus with default policy
    openlocus_dir = repo / ".openlocus"
    openlocus_dir.mkdir(exist_ok=True)
    (openlocus_dir / "policy.toml").write_text("")
    (repo / ".git").mkdir(exist_ok=True)

    languages = {
        ".rs": [
            "fn {name}() -> {ret} {{\n    // {comment}\n    {val}\n}}\n",
            "pub struct {Name} {{\n    field: {type},\n}}\n",
            "impl {Name} {{\n    pub fn new() -> Self {{ Self {{ field: {val} }} }}\n}}\n",
        ],
        ".py": [
            "def {name}():\n    \"\"\"{comment}\"\"\"\n    return {val}\n\n",
            "class {Name}:\n    def __init__(self):\n        self.value = {val}\n\n",
        ],
        ".ts": [
            "export function {name}(): {ret} {{\n    // {comment}\n    return {val};\n}}\n\n",
            "interface {Name} {{\n    value: {type};\n}}\n\n",
        ],
        ".md": [
            "# {Name}\n\n{comment}\n",
        ],
        ".txt": [
            "{comment}\n{name} = {val}\n",
        ],
    }

    names = [
        "authenticate", "authorize", "validate", "process", "transform",
        "calculate", "compute", "analyze", "generate", "render",
        "parse", "format", "encode", "decode", "compress",
        "fetch", "store", "cache", "index", "search",
        "filter", "sort", "merge", "split", "join",
        "create", "delete", "update", "read", "write",
    ]
    types = ["String", "i32", "u64", "bool", "f64", "Vec<String>"]
    rets = ["String", "i32", "bool", "Result<(), Error>"]
    comments = [
        "handles the request", "processes the data", "validates input",
        "transforms the output", "computes the result", "analyzes the payload",
        "generates the response", "renders the view", "parses the document",
        "formats the message",
    ]

    exts = list(languages.keys())
    file_count = 0

    # Create directories
    for i in range(20):
        (repo / f"src/pkg{i}").mkdir(parents=True, exist_ok=True)

    while file_count < num_files:
        ext = rng.choice(exts)
        pkg_idx = file_count % 20
        name = rng.choice(names)
        dir_path = repo / f"src/pkg{pkg_idx}"

        filename = f"{name}_{file_count}{ext}"
        file_path = dir_path / filename

        templates = languages[ext]
        template = rng.choice(templates)

        content = ""
        # Generate 3-8 functions/classes per file
        for j in range(rng.randint(3, 8)):
            fn_name = f"{name}_{j}"
            comment = rng.choice(comments)
            val = rng.choice(['true', 'false', '0', '1', '"ok"', 'None', 'Some(0)'])
            typ = rng.choice(types)
            ret = rng.choice(rets)

            filled = template.format(
                name=fn_name,
                Name=fn_name[0].upper() + fn_name[1:],
                comment=comment,
                val=val,
                type=typ,
                ret=ret,
            )
            content += filled

        file_path.write_text(content)
        file_count += 1

    return repo


def percentile(sorted_data: list[float], p: float) -> float:
    if not sorted_data:
        return 0.0
    idx = int(p / 100.0 * (len(sorted_data) - 1))
    return sorted_data[min(idx, len(sorted_data) - 1)]


def write_bench_dataset(repo: Path, queries: list[str]) -> Path:
    """Write a fixtures/r2-compatible JSONL query dataset for bench warm."""
    dataset_path = repo / "synthetic-bench-dataset.jsonl"
    with dataset_path.open("w", encoding="utf-8") as f:
        for idx, query in enumerate(queries):
            f.write(
                json.dumps(
                    {
                        "task_id": f"synthetic-{idx}",
                        "query": query,
                        "method": "bm25",
                        "gold_paths": [],
                        "gold_lines": {},
                    }
                )
                + "\n"
            )
    return dataset_path


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--openlocus", default="target/debug/openlocus", help="Path to openlocus binary"
    )
    parser.add_argument(
        "--out",
        default="runs/synthetic-slo-bench.json",
        help="Output JSON file",
    )
    parser.add_argument(
        "--num-files", type=int, default=1000, help="Number of files to generate"
    )
    parser.add_argument(
        "--seed", type=int, default=42, help="Random seed for deterministic generation"
    )
    args = parser.parse_args()

    ol = os.path.abspath(args.openlocus)

    tmpdir = tempfile.mkdtemp(prefix="openlocus_slo_bench_")
    repo = generate_repo(Path(tmpdir), seed=args.seed, num_files=args.num_files)
    cwd = str(repo)

    report: dict[str, Any] = {
        "report_kind": "synthetic_slo_bench",
        "timestamp": time.strftime("%Y-%m-%dT%H:%M:%S%z"),
        "num_files": args.num_files,
        "seed": args.seed,
    }

    # 1. Build index (full)
    purge = run_cmd([ol, "index", "purge", "--json"], cwd)
    t0 = time.perf_counter()
    build = run_cmd([ol, "index", "build", "--json"], cwd)
    build_ms = int((time.perf_counter() - t0) * 1000)
    report["build_ms"] = build_ms
    report["build_file_count"] = build.get("file_count", 0)
    report["build_chunk_count"] = build.get("chunk_count", 0)
    report["build_success"] = build.get("success") is True

    # 2. Dirty status latency (clean state)
    dirty_latencies = []
    dirty: dict[str, Any] = {}
    for _ in range(5):
        t = time.perf_counter()
        dirty = run_cmd([ol, "index", "dirty", "--json"], cwd)
        dirty_latencies.append((time.perf_counter() - t) * 1000)
    report["dirty_status_latency_ms"] = {
        "p50": percentile(sorted(dirty_latencies), 50),
        "p95": percentile(sorted(dirty_latencies), 95),
        "max": max(dirty_latencies),
    }
    report["dirty_clean"] = dirty.get("clean") is True

    # 3. Persistent CLI search latency p95 (each call opens index fresh)
    search_latencies = []
    queries = ["authenticate", "process", "validate", "transform", "compute",
               "parse", "format", "filter", "search", "create"]
    for query in queries:
        for _ in range(3):
            t = time.perf_counter()
            search = run_cmd(
                [ol, "search", "bm25", query, "--index", "persistent", "--json"], cwd
            )
            search_latencies.append((time.perf_counter() - t) * 1000)

    search_latencies_sorted = sorted(search_latencies)
    report["persistent_cli_search_latency_ms"] = {
        "p50": percentile(search_latencies_sorted, 50),
        "p95": percentile(search_latencies_sorted, 95),
        "max": percentile(search_latencies_sorted, 100),
    }

    # 3b. Open-once warm query latency from the Rust bench harness. This is not
    # outer CLI wall-clock latency; it is the CLI's internal PersistentBm25Index
    # open-once query p50/p95 over the synthetic dataset.
    bench_dataset = write_bench_dataset(repo, queries)
    bench_warm = run_cmd(
        [
            ol,
            "bench",
            "warm",
            "--dataset",
            str(bench_dataset),
            "--iterations",
            "3",
            "--json",
        ],
        cwd,
    )
    report["bench_warm"] = {
        "success": bench_warm.get("success") is True,
        "index_open_ms": bench_warm.get("index_open_ms", 0),
        "queries": bench_warm.get("queries", 0),
        "iterations": bench_warm.get("iterations", 0),
        "warm_query_p50_ms": bench_warm.get("warm_query_p50_ms", 0),
        "warm_query_p95_ms": bench_warm.get("warm_query_p95_ms", 0),
        "warm_query_max_ms": bench_warm.get("warm_query_max_ms", 0),
        "invalid_citations": bench_warm.get("invalid_citations", 0),
        "stale_hits_skipped": bench_warm.get("stale_hits_skipped", 0),
        "notes": bench_warm.get("notes", []),
        "returncode": bench_warm.get("returncode", -1),
    }

    # Validate no invalid citations from search
    total_invalid = 0
    for query in queries:
        search = run_cmd(
            [ol, "search", "bm25", query, "--index", "persistent", "--json"], cwd
        )
        evidence_list = search.get("evidence", [])
        if evidence_list:
            citation_file = os.path.join(tmpdir, f"cite_{query}.json")
            with open(citation_file, "w") as f:
                json.dump(evidence_list, f)
            validate = run_cmd(
                [ol, "citations", "validate", citation_file, "--json"], cwd
            )
            total_invalid += validate.get("invalid_count", 0)

    report["total_invalid_citations"] = total_invalid

    # 4. One-file update latency (true update: write different content each iteration)
    target_file = repo / "src" / "pkg0"
    rs_files = sorted(target_file.glob("*.rs"))
    update_success = True
    update_modified_count_ok = True
    if rs_files:
        modify_file = rs_files[0]
        original = modify_file.read_text()

        update_latencies = []
        for i in range(5):
            # Rebuild first to ensure clean state
            run_cmd([ol, "index", "build", "--json"], cwd)

            # Write genuinely different content each iteration (iteration counter token)
            modify_file.write_text(f"// iteration_{i} marker\n{original}")

            # Verify dirty detects the modification
            dirty_check = run_cmd([ol, "index", "dirty", "--json"], cwd)
            if dirty_check.get("modified_count", 0) != 1:
                update_modified_count_ok = False

            t = time.perf_counter()
            update = run_cmd([ol, "index", "update", "--dirty", "--json"], cwd)
            update_latencies.append((time.perf_counter() - t) * 1000)

            if not update.get("success"):
                update_success = False

        update_latencies_sorted = sorted(update_latencies)
        report["one_file_update_latency_ms"] = {
            "p50": percentile(update_latencies_sorted, 50),
            "p95": percentile(update_latencies_sorted, 95),
            "max": percentile(update_latencies_sorted, 100),
        }
        report["update_success"] = update_success
        report["update_modified_count_ok"] = update_modified_count_ok

        # Restore original content
        modify_file.write_text(original)

    # 5. Summary
    report["all_safety_checks_passed"] = (
        report["build_success"]
        and report["dirty_clean"]
        and report["total_invalid_citations"] == 0
        and report.get("update_success", True)
        and report.get("update_modified_count_ok", True)
        and report.get("bench_warm", {}).get("success") is True
        and report.get("bench_warm", {}).get("invalid_citations", 0) == 0
    )

    out = Path(args.out)
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text(json.dumps(report, indent=2) + "\n")
    print(json.dumps(report, indent=2))

    # Cleanup
    import shutil
    shutil.rmtree(tmpdir, ignore_errors=True)


if __name__ == "__main__":
    main()
