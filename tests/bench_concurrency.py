"""
Concurrency comparison: Loadforge vs k6.

Methodology (fair, same machine / same target / same scenario):
- Same Node.js HTTP target (bench_server.js), fresh instance per run.
- Same endpoint: GET /api/test.
- Same VU count and duration for both tools.
- k6 runs in open-loop (vus + duration, no sleep) just like Loadforge VUs.
"""
import json
import os
import subprocess
import tempfile
import time

import loadforge

K6 = r"D:\tools\k6\k6.exe"
NODE = r"D:\InterPreter\Nodejs\node.exe"
SERVER_JS = os.path.join(os.path.dirname(__file__), "bench_server.js")

VU_LEVELS = [100, 200, 500, 1000, 2000]
DURATION = 10

_port = 18100


def next_port():
    global _port
    _port += 1
    return _port


def start_server(port):
    p = subprocess.Popen(
        [NODE, SERVER_JS, str(port)],
        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
    )
    time.sleep(0.6)
    return p


def stop_server(p):
    try:
        p.terminate()
        p.wait(timeout=5)
    except Exception:
        try:
            p.kill()
        except Exception:
            pass


def run_loadforge(port, vu, duration):
    result = loadforge.run({
        "base_url": f"http://127.0.0.1:{port}",
        "vu": vu,
        "duration": duration,
        "endpoints": [{"method": "GET", "path": "/api/test", "weight": 1}],
    })
    return {
        "total": result["total"],
        "success": result["success"],
        "failed": result["failed"],
        "rps": result["rps"],
        "p50": result["p50_ms"],
        "p95": result["p95_ms"],
        "p99": result["p99_ms"],
    }


def run_k6(port, vu, duration):
    script = os.path.join(tempfile.gettempdir(), f"_k6_conc_{vu}.js")
    summary = os.path.join(tempfile.gettempdir(), f"_k6_conc_{vu}.json")
    with open(script, "w", encoding="utf-8") as f:
        f.write('import http from "k6/http";\n')
        f.write(f'export const options = {{ vus: {vu}, duration: "{duration}s" }};\n')
        f.write(f'export default function () {{ http.get("http://127.0.0.1:{port}/api/test"); }}\n')
    try:
        subprocess.run(
            [K6, "run", "--quiet", f"--summary-export={summary}", script],
            capture_output=True, timeout=duration + 90,
        )
        with open(summary, encoding="utf-8") as f:
            data = json.load(f)
        reqs = data["metrics"].get("http_reqs", {})
        lat = data["metrics"].get("http_req_duration", {})
        return {
            "total": int(reqs.get("count", 0)),
            "success": int(reqs.get("count", 0)),  # k6 counts all http_reqs
            "failed": 0,
            "rps": float(reqs.get("rate", 0)),
            "p50": float(lat.get("med", 0)),
            "p95": float(lat.get("p(95)", 0)),
            "p99": float(lat.get("p(99)", 0)),
        }
    except Exception as e:
        return {"total": 0, "success": 0, "failed": 0, "rps": 0,
                "p50": 0, "p95": 0, "p99": 0, "error": str(e)}
    finally:
        for f in (script, summary):
            try:
                os.remove(f)
            except Exception:
                pass


def run_one(vu, duration):
    port = next_port()
    srv = start_server(port)
    lf = run_loadforge(port, vu, duration)
    stop_server(srv)

    port = next_port()
    srv = start_server(port)
    k6 = run_k6(port, vu, duration)
    stop_server(srv)
    return lf, k6


def main():
    print(f"Concurrency comparison  |  Node.js target  |  duration={DURATION}s\n")

    # warm-up (discarded): JIT / connection warm-up for both tools
    run_one(100, 2)

    rows = []
    for vu in VU_LEVELS:
        lf, k6 = run_one(vu, DURATION)
        rows.append((vu, lf, k6))
        rps_ratio = lf["rps"] / max(k6["rps"], 1e-9)
        print(f"VU={vu:>5}  "
              f"LF  RPS={lf['rps']:>8.0f}  p95={lf['p95']:>6.1f}ms  total={lf['total']:>8}  "
              f"|  k6  RPS={k6['rps']:>8.0f}  p95={k6['p95']:>6.1f}ms  total={k6['total']:>8}  "
              f"|  LF/k6 RPS={rps_ratio:>5.2f}x")
        if "error" in k6:
            print(f"    [k6 error] {k6['error']}")

    print("\n" + "=" * 78)
    print(f"{'VU':>6} {'LF RPS':>10} {'k6 RPS':>10} {'LF/k6':>8} "
          f"{'LF p95':>9} {'k6 p95':>9} {'LF total':>10} {'k6 total':>10}")
    print("-" * 78)
    for vu, lf, k6 in rows:
        print(f"{vu:>6} {lf['rps']:>10.0f} {k6['rps']:>10.0f} {lf['rps']/max(k6['rps'],1e-9):>8.2f} "
              f"{lf['p95']:>8.1f}ms {k6['p95']:>8.1f}ms {lf['total']:>10} {k6['total']:>10}")
    print("=" * 78)

    out = {"duration": DURATION, "target": "node bench_server.js", "levels": []}
    for vu, lf, k6 in rows:
        out["levels"].append({"vu": vu, "loadforge": lf, "k6": k6})
    with open(os.path.join(os.path.dirname(__file__), "bench_concurrency_results.json"),
              "w", encoding="utf-8") as f:
        json.dump(out, f, indent=2, ensure_ascii=False)
    print("\nSaved tests/bench_concurrency_results.json")


if __name__ == "__main__":
    main()