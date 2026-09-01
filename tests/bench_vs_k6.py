"""Loadforge vs k6: resource & performance comparison across workloads.

Workloads: HTTP/1.1 (plain), HTTP/2 (TLS), WebSocket (echo).
Measures: throughput, p95 latency, CPU time, peak memory.
"""
import ctypes
import json
import os
import site
import subprocess
import sys
import tempfile
import time
from ctypes import wintypes

HERE = os.path.dirname(os.path.abspath(__file__))
NODE = r"D:\InterPreter\Nodejs\node.exe"
K6 = r"D:\tools\k6\k6.exe"
PROTO = os.path.join(HERE, "proto_servers.js")
CLUSTER = os.path.join(HERE, "bench_server_cluster.js")
WORKER = os.path.join(HERE, "_lf_worker2.py")
CERT = os.path.join(HERE, "certs", "server.crt")
KEY = os.path.join(HERE, "certs", "server.key")

BASE_PY = getattr(sys, "_base_executable", None) or os.path.join(sys.base_prefix, "python.exe")
VENV_SP = [p for p in site.getsitepackages() if p.endswith("site-packages")][0]

VU_LEVELS = [100, 500, 1000]
WS_VU_LEVELS = [100, 200, 300]
DURATION = 5

PROCESS_QUERY_INFORMATION = 0x0400
PROCESS_VM_READ = 0x0010


class FILETIME(ctypes.Structure):
    _fields_ = [("dwLowDateTime", wintypes.DWORD), ("dwHighDateTime", wintypes.DWORD)]


class PROCESS_MEMORY_COUNTERS(ctypes.Structure):
    _fields_ = [
        ("cb", wintypes.DWORD), ("PageFaultCount", wintypes.DWORD),
        ("PeakWorkingSetSize", ctypes.c_size_t), ("WorkingSetSize", ctypes.c_size_t),
        ("QuotaPeakPagedPoolUsage", ctypes.c_size_t), ("QuotaPagedPoolUsage", ctypes.c_size_t),
        ("QuotaPeakNonPagedPoolUsage", ctypes.c_size_t), ("QuotaNonPagedPoolUsage", ctypes.c_size_t),
        ("PagefileUsage", ctypes.c_size_t), ("PeakPagefileUsage", ctypes.c_size_t),
    ]


kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
psapi = ctypes.WinDLL("psapi", use_last_error=True)


def spawn_and_measure(argv):
    env = dict(os.environ)
    env["PYTHONPATH"] = VENV_SP + os.pathsep + env.get("PYTHONPATH", "")
    p = subprocess.Popen(argv, stdout=subprocess.PIPE, stderr=subprocess.PIPE, env=env)
    h = kernel32.OpenProcess(PROCESS_QUERY_INFORMATION | PROCESS_VM_READ, False, p.pid)
    out, err = p.communicate()
    cpu = 0.0
    mem = 0
    if h:
        c = FILETIME(); e = FILETIME(); k = FILETIME(); u = FILETIME()
        if kernel32.GetProcessTimes(h, ctypes.byref(c), ctypes.byref(e), ctypes.byref(k), ctypes.byref(u)):
            kt = (k.dwHighDateTime << 32) | k.dwLowDateTime
            ut = (u.dwHighDateTime << 32) | u.dwLowDateTime
            cpu = (kt + ut) / 1e7
        pmc = PROCESS_MEMORY_COUNTERS()
        pmc.cb = ctypes.sizeof(pmc)
        if psapi.GetProcessMemoryInfo(h, ctypes.byref(pmc), pmc.cb):
            mem = pmc.PeakWorkingSetSize
        kernel32.CloseHandle(h)
    return p.returncode, out.decode(errors="replace"), err.decode(errors="replace"), cpu, mem


def run_lf(plan):
    fd, path = tempfile.mkstemp(suffix=".json", prefix="lfplan_")
    os.close(fd)
    with open(path, "w", encoding="utf-8") as f:
        json.dump(plan, f)
    try:
        rc, out, err, cpu, mem = spawn_and_measure([BASE_PY, WORKER, path])
        d = json.loads(out.strip().splitlines()[-1])
    finally:
        try:
            os.remove(path)
        except Exception:
            pass
    d["cpu_secs"] = cpu
    d["peak_mb"] = mem / 1048576.0
    return d


def run_k6(script, vu, duration, port):
    fd, path = tempfile.mkstemp(suffix=".js", prefix="k6bench_")
    os.close(fd)
    summary = path + ".json"
    with open(path, "w", encoding="utf-8") as f:
        f.write(script.format(vu=vu, duration=duration, port=port))
    try:
        rc, out, err, cpu, mem = spawn_and_measure(
            [K6, "run", "--quiet", f"--summary-export={summary}", path])
        with open(summary, encoding="utf-8") as f:
            data = json.load(f)
        return {"data": data, "cpu_secs": cpu, "peak_mb": mem / 1048576.0}
    finally:
        for f in (path, summary):
            try:
                os.remove(f)
            except Exception:
                pass


def stop_single(p):
    try:
        p.terminate()
        p.wait(timeout=5)
    except Exception:
        try:
            p.kill()
        except Exception:
            pass


def stop_cluster(p):
    try:
        p.stdin.close()
        p.wait(timeout=8)
    except Exception:
        try:
            p.terminate()
        except Exception:
            pass


# ----------------------------------------------------------------------
# k6 scripts (templates)
# ----------------------------------------------------------------------
K6_HTTP1 = '''
import http from "k6/http";
export const options = {{ vus: {vu}, duration: "{duration}s" }};
export default function () {{ http.get("http://127.0.0.1:{port}/api/test"); }}
'''

K6_HTTP2 = '''
import http from "k6/http";
export const options = {{ vus: {vu}, duration: "{duration}s", insecureSkipTLSVerify: true }};
export default function () {{ http.get("https://127.0.0.1:{port}/api/test"); }}
'''

K6_WS = '''
import ws from "k6/ws";
import {{ Counter }} from "k6/metrics";
let messages = new Counter("ws_messages");
export const options = {{ vus: {vu}, duration: "{duration}s" }};
export default function () {{
  ws.connect("ws://127.0.0.1:{port}/ws", null, function (socket) {{
    socket.on("open", () => socket.send("ping"));
    socket.on("message", () => {{ messages.add(1); socket.send("ping"); }});
    socket.on("close", () => {{}});
    socket.on("error", () => {{}});
    socket.setTimeout(() => socket.close(), {duration} * 1000);
  }});
}}
'''


def main():
    rows = []

    # ---------- HTTP/1.1 (plain, cluster server) ----------
    port = 18230
    srv = subprocess.Popen([NODE, CLUSTER, str(port), "22"],
                           stdin=subprocess.PIPE, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    time.sleep(2.0)
    print("== HTTP/1.1 ==")
    for vu in VU_LEVELS:
        lf = run_lf({"base_url": f"http://127.0.0.1:{port}", "vu": vu, "duration": DURATION,
                     "endpoints": [{"method": "GET", "path": "/api/test", "weight": 1}]})
        k6c = run_k6(K6_HTTP1, vu, DURATION, port)
        rows.append(("HTTP/1.1", vu, lf, k6c))
    stop_cluster(srv)

    # ---------- HTTP/2 (TLS) ----------
    port = 18231
    srv = subprocess.Popen([NODE, PROTO, "http2s", str(port), KEY, CERT],
                           stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    time.sleep(1.0)
    print("== HTTP/2 ==")
    for vu in VU_LEVELS:
        lf = run_lf({"base_url": f"https://127.0.0.1:{port}", "vu": vu, "duration": DURATION,
                     "http2": True, "insecure": True,
                     "endpoints": [{"method": "GET", "path": "/api/test", "weight": 1}]})
        k6c = run_k6(K6_HTTP2, vu, DURATION, port)
        rows.append(("HTTP/2", vu, lf, k6c))
    stop_single(srv)

    # ---------- WebSocket (echo) ----------
    port = 18232
    print("== WebSocket ==")
    for vu in WS_VU_LEVELS:
        # fresh server per run: k6's ws sockets can leave a single-process
        # server degraded, contaminating the next measurement
        srv = subprocess.Popen([NODE, PROTO, "ws", str(port)],
                               stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        time.sleep(0.8)
        lf = run_lf({"vu": vu, "duration": DURATION,
                     "ws": {"url": f"ws://127.0.0.1:{port}/ws", "message": "ping"}})
        stop_single(srv)

        srv = subprocess.Popen([NODE, PROTO, "ws", str(port)],
                               stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        time.sleep(0.8)
        k6c = run_k6(K6_WS, vu, DURATION, port)
        stop_single(srv)

        rows.append(("WebSocket", vu, lf, k6c))

    # ---------- report ----------
    print("\n" + "=" * 104)
    print(f"{'workload':<10} {'VU':>5} | {'LF thr':>8} {'k6 thr':>8} {'thr比':>6} | "
          f"{'LF p95':>7} {'k6 p95':>7} | {'LF CPU':>6} {'k6 CPU':>6} | {'LF mem':>6} {'k6 mem':>6} | {'eff比':>6}")
    print("-" * 104)
    for name, vu, lf, k6c in rows:
        data = k6c["data"]
        if name in ("HTTP/1.1", "HTTP/2"):
            k6_thr = data["metrics"].get("http_reqs", {}).get("rate", 0)
            k6_p95 = data["metrics"].get("http_req_duration", {}).get("p(95)", 0)
            k6_total = int(data["metrics"].get("http_reqs", {}).get("count", 0))
            lf_thr = lf["rps"]
            lf_p95 = lf["p95_ms"]
            lf_total = lf["total"]
        else:  # ws
            k6_thr = data["metrics"].get("ws_messages", {}).get("rate", 0)
            k6_p95 = 0
            k6_total = int(data["metrics"].get("ws_messages", {}).get("count", 0))
            lf_thr = lf["rps"]
            lf_p95 = lf["p95_ms"]
            lf_total = lf["total"]

        lf_eff = lf_total / max(lf["cpu_secs"], 1e-6)
        k6_eff = k6_total / max(k6c["cpu_secs"], 1e-6)
        print(f"{name:<10} {vu:>5} | {lf_thr:>8.0f} {k6_thr:>8.0f} {lf_thr/max(k6_thr,1e-9):>5.2f}x | "
              f"{lf_p95:>6.1f}ms {k6_p95:>6.1f}ms | {lf['cpu_secs']:>5.1f}s {k6c['cpu_secs']:>5.1f}s | "
              f"{lf['peak_mb']:>5.0f}MB {k6c['peak_mb']:>5.0f}MB | {lf_eff/max(k6_eff,1e-9):>5.2f}x")

    print("=" * 104)


if __name__ == "__main__":
    main()