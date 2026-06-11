#!/usr/bin/env python3
"""R3 Level0 Storage Smoke/Conformance — verify conservative and tdb backends.

Outputs runs/storage-report.json with status/build/purge results for
each backend. This is a Level0 smoke test, not a full storage bakeoff
or TDB comparison.

report_kind = "storage_level0_smoke"
"""

from __future__ import annotations

import argparse
import json
import subprocess
import time
from pathlib import Path
from typing import Any


def run_store_cmd(openlocus: str, subcmd: str, backend: str, cwd: str) -> dict[str, Any]:
    """Run a store subcommand and return parsed JSON + latency."""
    cmd = [openlocus, "store", subcmd, backend, "--json"]
    t0 = time.perf_counter()
    proc = subprocess.run(cmd, check=False, text=True, capture_output=True, cwd=cwd)
    latency_ms = int((time.perf_counter() - t0) * 1000)

    try:
        result: dict[str, Any] = json.loads(proc.stdout) if proc.stdout.strip() else {}
    except json.JSONDecodeError:
        result = {"raw_stdout": proc.stdout[:500]}

    result["latency_ms"] = latency_ms
    result["returncode"] = proc.returncode
    result["stderr"] = proc.stderr[:500] if proc.stderr else ""
    return result


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--openlocus", default="target/debug/openlocus", help="Path to openlocus binary"
    )
    parser.add_argument("--cwd", default=".", help="Working directory")
    parser.add_argument(
        "--out",
        default="runs/storage-report.json",
        help="Output JSON file",
    )
    args = parser.parse_args()

    backends = ["conservative", "tdb"]
    subcommands = ["status", "build", "purge"]

    report: dict[str, Any] = {
        "report_kind": "storage_level0_smoke",
        "timestamp": time.strftime("%Y-%m-%dT%H:%M:%S%z"),
        "backends": {},
    }

    for backend in backends:
        report["backends"][backend] = {}
        for subcmd in subcommands:
            result = run_store_cmd(args.openlocus, subcmd, backend, args.cwd)
            report["backends"][backend][subcmd] = result

    out = Path(args.out)
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text(json.dumps(report, indent=2) + "\n")
    print(json.dumps(report, indent=2))


if __name__ == "__main__":
    main()
