"""
Loadforge MVP - Python Test Plan

Python only defines the test plan (declarative config).
Rust engine handles all execution.
"""

import loadforge

# ============================================================
# Define Test Plan
# ============================================================
plan = {
    "base_url": "http://127.0.0.1:18080",
    "vu": 100,             # Virtual Users
    "duration": 10,         # seconds
    "endpoints": [
        {"method": "GET",  "path": "/api/users",    "weight": 5},
        {"method": "GET",  "path": "/api/products",  "weight": 3},
        {"method": "POST", "path": "/api/orders",    "weight": 2,
         "body": '{"product_id": "test-001", "amount": 100}'},
    ],
}

# ============================================================
# Run (all execution happens in Rust)
# ============================================================
result = loadforge.run(plan)

# ============================================================
# Print Results
# ============================================================
print()
print("=" * 50)
print("  Loadforge Test Results")
print("=" * 50)
print(f"  Total Requests : {result['total']}")
print(f"  Success        : {result['success']}")
print(f"  Failed         : {result['failed']}")
print(f"  Duration       : {result['elapsed_secs']:.2f}s")
print(f"  RPS            : {result['rps']:.0f}")
print(f"  P50 Latency    : {result['p50_ms']:.1f}ms")
print(f"  P95 Latency    : {result['p95_ms']:.1f}ms")
print(f"  P99 Latency    : {result['p99_ms']:.1f}ms")
print(f"  Status Codes   : {result['status_codes']}")
print("=" * 50)
