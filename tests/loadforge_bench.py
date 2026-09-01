"""
Loadforge benchmark script
Target: local HTTP server on :18090
Comparable with k6_bench.js
"""

import loadforge

plan = {
    "base_url": "http://127.0.0.1:18090",
    "vu": 100,
    "duration": 10,
    "endpoints": [
        {"method": "GET", "path": "/api/test", "weight": 1},
    ],
}

result = loadforge.run(plan)

print(f"Total={result['total']}  "
      f"RPS={result['rps']:.0f}  "
      f"P50={result['p50_ms']:.1f}ms  "
      f"P95={result['p95_ms']:.1f}ms  "
      f"P99={result['p99_ms']:.1f}ms  "
      f"OK={result['success']}/{result['total']}")

# Output raw numbers for comparison
print(f"__RESULT__{result['total']}|{result['success']}|{result['rps']:.0f}|{result['p50_ms']:.1f}|{result['p95_ms']:.1f}|{result['p99_ms']:.1f}")
