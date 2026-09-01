"""
Benchmark: Loadforge vs k6
Uses Node.js HTTP server for maximum throughput.
"""

import subprocess, os, time, json, tempfile, threading

VU = 100
DURATION = 10
K6_BIN = r"D:\tools\k6\k6.exe"
NODE_BIN = r"D:\InterPreter\Nodejs\node.exe"
SERVER_JS = os.path.join(os.path.dirname(__file__), "bench_server.js")


def start_node_server(port):
    proc = subprocess.Popen([NODE_BIN, SERVER_JS, str(port)], stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    time.sleep(0.5)
    return proc


def run_k6(port):
    summary = os.path.join(os.path.dirname(__file__), "k6_summary.json")
    script = os.path.join(tempfile.gettempdir(), f"_k6_bench_{port}.js")
    with open(script, "w") as f:
        f.write(f'import http from "k6/http";\n')
        f.write(f'export const options = {{ vus: {VU}, duration: "{DURATION}s" }};\n')
        f.write(f'export default function () {{ http.get("http://127.0.0.1:{port}/api/test"); }}\n')
    subprocess.run([K6_BIN, "run", "--quiet", f"--summary-export={summary}", script], capture_output=True)
    try: os.remove(script)
    except: pass
    try:
        with open(summary) as f:
            data = json.load(f)
        reqs = data["metrics"].get("http_reqs", {})
        dur = data["metrics"].get("http_req_duration", {})
        return {"total": int(reqs.get("count", 0)), "rps": reqs.get("rate", 0),
                "p50": dur.get("med", 0), "p95": dur.get("p(95)", 0), "p99": dur.get("p(99)", 0)}
    except:
        return None


def run_loadforge(port):
    import loadforge
    result = loadforge.run({
        "base_url": f"http://127.0.0.1:{port}",
        "vu": VU, "duration": DURATION,
        "endpoints": [{"method": "GET", "path": "/api/test", "weight": 1}],
    })
    return {"total": result["total"], "success": result["success"], "rps": result["rps"],
            "p50": result["p50_ms"], "p95": result["p95_ms"], "p99": result["p99_ms"]}


def main():
    PORT_LF, PORT_K6 = 18090, 18091

    srv1 = start_node_server(PORT_LF)
    srv2 = start_node_server(PORT_K6)

    print(f"Config: VU={VU}, Duration={DURATION}s (Node.js server)\n")

    # Loadforge
    print(f"{'='*50}\n  Running Loadforge...\n{'='*50}")
    lf = run_loadforge(PORT_LF)
    ok = (lf["success"] / max(lf["total"], 1)) * 100
    print(f"  Total: {lf['total']:>10}  Success: {lf['success']:>10} ({ok:.0f}%)")
    print(f"  RPS  : {lf['rps']:>10.0f}  P50: {lf['p50']:.1f}ms  P95: {lf['p95']:.1f}ms  P99: {lf['p99']:.1f}ms\n")

    # k6
    print(f"{'='*50}\n  Running k6...\n{'='*50}")
    k6 = run_k6(PORT_K6)
    if k6:
        print(f"  Total: {k6['total']:>10}")
        print(f"  RPS  : {k6['rps']:>10.0f}  P50: {k6['p50']:.1f}ms  P95: {k6['p95']:.1f}ms  P99: {k6['p99']:.1f}ms\n")
    else:
        print("  [!] k6 summary not available\n")

    # Compare
    if k6 and k6["total"] > 0:
        print(f"{'='*50}\n  Comparison ({VU} VU, {DURATION}s)\n{'='*50}")
        r = lf["rps"] / max(k6["rps"], 1)
        print(f"  {'Metric':<10} {'Loadforge':>12} {'k6':>12} {'Ratio':>8}")
        print(f"  {'-'*44}")
        print(f"  {'Total':<10} {lf['total']:>12} {k6['total']:>12} {lf['total']/max(k6['total'],1):>8.2f}x")
        print(f"  {'RPS':<10} {lf['rps']:>12.0f} {k6['rps']:>12.0f} {r:>8.2f}x")
        print(f"  {'P50':<10} {lf['p50']:>11.1f}ms {k6['p50']:>11.1f}ms")
        print(f"  {'P95':<10} {lf['p95']:>11.1f}ms {k6['p95']:>11.1f}ms")
        print(f"  {'P99':<10} {lf['p99']:>11.1f}ms {k6['p99']:>11.1f}ms")

    srv1.terminate()
    srv2.terminate()
    try: os.remove(os.path.join(os.path.dirname(__file__), "k6_summary.json"))
    except: pass


if __name__ == "__main__":
    main()
