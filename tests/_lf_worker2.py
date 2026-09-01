"""Run one loadforge plan from a JSON file; print the result as JSON.
Used by the benchmark harness to measure the child process CPU / memory."""
import json
import sys

import loadforge


def main():
    plan_path = sys.argv[1]
    with open(plan_path, encoding="utf-8") as f:
        plan = json.load(f)
    result = loadforge.run(plan)
    # normalize dict keys (status_codes keys are ints already from Rust)
    print(json.dumps({
        "total": result["total"],
        "success": result["success"],
        "failed": result["failed"],
        "rps": result["rps"],
        "p50_ms": result["p50_ms"],
        "p95_ms": result["p95_ms"],
        "p99_ms": result["p99_ms"],
        "min_ms": result["min_ms"],
        "max_ms": result["max_ms"],
        "avg_ms": result["avg_ms"],
        "bytes": result["bytes"],
        "checks_passed": result["checks_passed"],
        "checks_failed": result["checks_failed"],
    }))


if __name__ == "__main__":
    main()