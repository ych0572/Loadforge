"""Worker that runs one Loadforge test and prints the result as JSON.

Kept in its own process so the benchmark harness can measure the child's
CPU time and peak memory (same isolation as the k6 child process).
"""
import json
import sys

import loadforge


def main():
    port = int(sys.argv[1])
    vu = int(sys.argv[2])
    duration = int(sys.argv[3])

    result = loadforge.run({
        "base_url": f"http://127.0.0.1:{port}",
        "vu": vu,
        "duration": duration,
        "endpoints": [{"method": "GET", "path": "/api/test", "weight": 1}],
    })

    out = {
        "total": result["total"],
        "success": result["success"],
        "failed": result["failed"],
        "rps": result["rps"],
        "p50": result["p50_ms"],
        "p95": result["p95_ms"],
        "p99": result["p99_ms"],
    }
    print(json.dumps(out))


if __name__ == "__main__":
    main()