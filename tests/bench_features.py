"""Feature performance experiment under the core principles.

Compares HTTP/1.1 vs HTTP/2, SSE, and WebSocket: throughput, latency,
CPU time and peak memory. Each run executes in a child process so CPU/memory
can be measured via Windows process APIs.
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

import loadforge  # noqa: F401 (only to confirm import works in-process)

HERE = os.path.dirname(os.path.abspath(__file__))
NODE = r"D:\InterPreter\Nodejs\node.exe"
PROTO = os.path.join(HERE, "proto_servers.js")
WORKER = os.path.join(HERE, "_lf_worker2.py")
CERT = os.path.join(HERE, "certs", "server.crt")
KEY = os.path.join(HERE, "certs", "server.key")

BASE_PY = getattr(sys, "_base_executable", None) or os.path.join(sys.base_prefix, "python.exe")
VENV_SP = [p for p in site.getsitepackages() if p.endswith("site-packages")][0]

VU_LEVELS = [100, 500, 1000, 2000]
DURATION = 5

# ----------------------------------------------------------------------
# Windows process metrics
# ----------------------------------------------------------------------
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


def run_plan(plan):
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


def start(mode, port, *extra):
    p = subprocess.Popen([NODE, PROTO, mode, str(port), *extra],
                         stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    time.sleep(1.0)
    return p


def stop(p):
    try:
        p.terminate()
        p.wait(timeout=5)
    except Exception:
        try:
            p.kill()
        except Exception:
            pass


def scenario_http(port, http2):
    return {
        "base_url": f"https://127.0.0.1:{port}",
        "http2": http2,
        "insecure": True,
        "endpoints": [{"method": "GET", "path": "/api/test", "weight": 1}],
    }


def scenario_sse(port):
    return {"base_url": f"http://127.0.0.1:{port}", "sse": {"path": "/events"}}


def scenario_ws(port):
    return {"ws": {"url": f"ws://127.0.0.1:{port}/ws", "message": "ping"}}


def sweep(name, server_mode, make_plan, port, extra=()):
    srv = start(server_mode, port, *extra)
    print(f"\n== {name} ==")
    rows = []
    for vu in VU_LEVELS:
        plan = make_plan(port)
        plan["vu"] = vu
        plan["duration"] = DURATION
        d = run_plan(plan)
        eff = d["total"] / max(d["cpu_secs"], 1e-6)
        rows.append((vu, d, eff))
        print(f"  VU={vu:>5}  thr={d['rps']:>9.0f}/s  total={d['total']:>9}  "
              f"p95={d['p95_ms']:>7.2f}ms  cpu={d['cpu_secs']:>6.1f}s  mem={d['peak_mb']:>6.0f}MB  "
              f"eff={eff:>9.0f}/cpu-s")
    stop(srv)
    return rows


def main():
    all_rows = []

    # h1 (TLS) on a secure server that also accepts http/1.1
    port = 18200
    srv = start("h2both", port, KEY, CERT)
    print("== HTTP/1.1 (TLS) ==")
    for vu in VU_LEVELS:
        plan = scenario_http(port, False); plan["vu"] = vu; plan["duration"] = DURATION
        d = run_plan(plan)
        all_rows.append(("HTTP/1.1", vu, d, d["total"]/max(d["cpu_secs"],1e-6)))
        print(f"  VU={vu:>5}  rps={d['rps']:>8.0f}  p95={d['p95_ms']:>6.2f}ms  cpu={d['cpu_secs']:>5.1f}s  mem={d['peak_mb']:>5.0f}MB")
    stop(srv)

    # h2 (TLS) on an h2-only secure server
    port2 = 18203
    srv2 = start("http2s", port2, KEY, CERT)
    print("== HTTP/2 (TLS) ==")
    for vu in VU_LEVELS:
        plan = scenario_http(port2, True); plan["vu"] = vu; plan["duration"] = DURATION
        d = run_plan(plan)
        all_rows.append(("HTTP/2", vu, d, d["total"]/max(d["cpu_secs"],1e-6)))
        print(f"  VU={vu:>5}  rps={d['rps']:>8.0f}  p95={d['p95_ms']:>6.2f}ms  cpu={d['cpu_secs']:>5.1f}s  mem={d['peak_mb']:>5.0f}MB")
    stop(srv2)

    for name, mode, mk, p in [("SSE", "sse", lambda p: scenario_sse(p), 18201),
                              ("WebSocket", "ws", lambda p: scenario_ws(p), 18202)]:
        print(f"\n== {name} ==")
        srv = start(mode, p)
        for vu in VU_LEVELS:
            plan = mk(p); plan["vu"] = vu; plan["duration"] = DURATION
            d = run_plan(plan)
            all_rows.append((name, vu, d, d["total"]/max(d["cpu_secs"],1e-6)))
            print(f"  VU={vu:>5}  thr={d['rps']:>9.0f}/s  total={d['total']:>9}  "
                  f"p95={d['p95_ms']:>7.2f}ms  cpu={d['cpu_secs']:>6.1f}s  mem={d['peak_mb']:>6.0f}MB")
        stop(srv)

    print("\n" + "=" * 92)
    print(f"{'workload':<12} {'VU':>5} {'thr':>9} {'p95':>7} {'cpu':>6} {'mem':>7} {'eff':>9}")
    print("-" * 92)
    for name, vu, d, eff in all_rows:
        print(f"{name:<12} {vu:>5} {d['rps']:>9.0f} {d['p95_ms']:>6.2f}ms {d['cpu_secs']:>5.1f}s {d['peak_mb']:>6.0f}MB {eff:>9.0f}")
    print("=" * 92)

    out = {"duration": DURATION, "levels": []}
    for name, vu, d, eff in all_rows:
        out["levels"].append({"workload": name, "vu": vu, "result": d, "eff": eff})
    with open(os.path.join(HERE, "bench_features_results.json"), "w", encoding="utf-8") as f:
        json.dump(out, f, indent=2, ensure_ascii=False)
    print("\nSaved tests/bench_features_results.json")


if __name__ == "__main__":
    main()