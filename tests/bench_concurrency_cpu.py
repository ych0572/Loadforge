"""
Concurrency + resource comparison: Loadforge vs k6.

Target: Node.js *cluster* server (multi-process) so the server is NOT the
bottleneck - differences reflect the client engines.

Measured per run (each tool in its own child process):
- RPS / p95 / total
- CPU time (process kernel+user, via GetProcessTimes)
- peak working set (RAM, via GetProcessMemoryInfo)
- req per CPU-second (engine efficiency)

Note: the venv python.exe is a launcher that spawns the base interpreter,
so Loadforge is measured via the base interpreter + PYTHONPATH=venv.
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

K6 = r"D:\tools\k6\k6.exe"
NODE = r"D:\InterPreter\Nodejs\node.exe"
HERE = os.path.dirname(os.path.abspath(__file__))
CLUSTER_JS = os.path.join(HERE, "bench_server_cluster.js")
LF_WORKER = os.path.join(HERE, "_lf_worker.py")

BASE_PY = getattr(sys, "_base_executable", None) or os.path.join(sys.base_prefix, "python.exe")
VENV_SP = [p for p in site.getsitepackages() if p.endswith("site-packages")][0]

VU_LEVELS = [100, 200, 500, 1000, 2000, 5000]
DURATION = 10
WORKERS = 22

# ----------------------------------------------------------------------
# Windows process metrics (CPU time + peak working set)
# ----------------------------------------------------------------------
PROCESS_QUERY_INFORMATION = 0x0400
PROCESS_VM_READ = 0x0010


class FILETIME(ctypes.Structure):
    _fields_ = [("dwLowDateTime", wintypes.DWORD),
                ("dwHighDateTime", wintypes.DWORD)]


class PROCESS_MEMORY_COUNTERS(ctypes.Structure):
    _fields_ = [
        ("cb", wintypes.DWORD),
        ("PageFaultCount", wintypes.DWORD),
        ("PeakWorkingSetSize", ctypes.c_size_t),
        ("WorkingSetSize", ctypes.c_size_t),
        ("QuotaPeakPagedPoolUsage", ctypes.c_size_t),
        ("QuotaPagedPoolUsage", ctypes.c_size_t),
        ("QuotaPeakNonPagedPoolUsage", ctypes.c_size_t),
        ("QuotaNonPagedPoolUsage", ctypes.c_size_t),
        ("PagefileUsage", ctypes.c_size_t),
        ("PeakPagefileUsage", ctypes.c_size_t),
    ]


kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
psapi = ctypes.WinDLL("psapi", use_last_error=True)


def _cpu_secs(handle):
    c = FILETIME(); e = FILETIME(); k = FILETIME(); u = FILETIME()
    if kernel32.GetProcessTimes(handle, ctypes.byref(c), ctypes.byref(e),
                                ctypes.byref(k), ctypes.byref(u)):
        kt = (k.dwHighDateTime << 32) | k.dwLowDateTime
        ut = (u.dwHighDateTime << 32) | u.dwLowDateTime
        return (kt + ut) / 1e7
    return 0.0


def _peak_ws(handle):
    pmc = PROCESS_MEMORY_COUNTERS()
    pmc.cb = ctypes.sizeof(pmc)
    if psapi.GetProcessMemoryInfo(handle, ctypes.byref(pmc), pmc.cb):
        return pmc.PeakWorkingSetSize
    return 0


def spawn_and_measure(argv, env=None):
    p = subprocess.Popen(argv, stdout=subprocess.PIPE, stderr=subprocess.PIPE, env=env)
    handle = kernel32.OpenProcess(PROCESS_QUERY_INFORMATION | PROCESS_VM_READ,
                                  False, p.pid)
    t0 = time.perf_counter()
    out, err = p.communicate()
    wall = time.perf_counter() - t0
    cpu = 0.0
    mem = 0
    if handle:
        cpu = _cpu_secs(handle)
        mem = _peak_ws(handle)
        kernel32.CloseHandle(handle)
    return p.returncode, out.decode(errors="replace"), err.decode(errors="replace"), wall, cpu, mem


def _lf_env():
    env = dict(os.environ)
    env["PYTHONPATH"] = VENV_SP + os.pathsep + env.get("PYTHONPATH", "")
    return env


def run_loadforge(port, vu, duration):
    rc, out, err, wall, cpu, mem = spawn_and_measure(
        [BASE_PY, LF_WORKER, str(port), str(vu), str(duration)], env=_lf_env())
    try:
        d = json.loads(out.strip().splitlines()[-1])
    except Exception:
        d = {"total": 0, "rps": 0, "p95": 0, "p50": 0, "p99": 0}
    d["wall"] = wall
    d["cpu_secs"] = cpu
    d["peak_mb"] = mem / 1048576.0
    return d


def run_k6(port, vu, duration):
    script = os.path.join(tempfile.gettempdir(), f"_k6_cpu_{vu}.js")
    summary = os.path.join(tempfile.gettempdir(), f"_k6_cpu_{vu}.json")
    with open(script, "w", encoding="utf-8") as f:
        f.write('import http from "k6/http";\n')
        f.write(f'export const options = {{ vus: {vu}, duration: "{duration}s" }};\n')
        f.write(f'export default function () {{ http.get("http://127.0.0.1:{port}/api/test"); }}\n')
    try:
        rc, out, err, wall, cpu, mem = spawn_and_measure(
            [K6, "run", "--quiet", f"--summary-export={summary}", script])
        with open(summary, encoding="utf-8") as f:
            data = json.load(f)
        reqs = data["metrics"].get("http_reqs", {})
        lat = data["metrics"].get("http_req_duration", {})
        return {
            "total": int(reqs.get("count", 0)),
            "rps": float(reqs.get("rate", 0)),
            "p50": float(lat.get("med", 0)),
            "p95": float(lat.get("p(95)", 0)),
            "p99": float(lat.get("p(99)", 0)),
            "wall": wall,
            "cpu_secs": cpu,
            "peak_mb": mem / 1048576.0,
        }
    except Exception as e:
        return {"total": 0, "rps": 0, "p50": 0, "p95": 0, "p99": 0,
                "wall": 0, "cpu_secs": 0, "peak_mb": 0, "error": str(e)}
    finally:
        for f in (script, summary):
            try:
                os.remove(f)
            except Exception:
                pass


def kill_tree(proc):
    try:
        subprocess.run(["taskkill", "/PID", str(proc.pid), "/T", "/F"],
                       capture_output=True, timeout=10)
    except Exception:
        try:
            proc.kill()
        except Exception:
            pass


def eff(d):
    return d["total"] / max(d["cpu_secs"], 1e-6)


def main():
    port = 18110
    srv = subprocess.Popen([NODE, CLUSTER_JS, str(port), str(WORKERS)],
                           stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    print(f"Started cluster server on :{port} ({WORKERS} workers), warming up...")
    time.sleep(2.0)

    run_loadforge(port, 100, 2)
    run_k6(port, 100, 2)
    time.sleep(1.0)

    rows = []
    for vu in VU_LEVELS:
        lf = run_loadforge(port, vu, DURATION)
        time.sleep(1.0)
        k6 = run_k6(port, vu, DURATION)
        time.sleep(1.0)
        rows.append((vu, lf, k6))
        print(f"VU={vu:>5} | LF RPS={lf['rps']:>7.0f} p95={lf['p95']:>5.1f}ms "
              f"cpu={lf['cpu_secs']:>5.1f}s mem={lf['peak_mb']:>6.0f}MB "
              f"| k6 RPS={k6['rps']:>7.0f} p95={k6['p95']:>5.1f}ms "
              f"cpu={k6['cpu_secs']:>5.1f}s mem={k6['peak_mb']:>6.0f}MB "
              f"| RPS {lf['rps']/max(k6['rps'],1e-9):.2f}x  eff {eff(lf)/max(eff(k6),1e-9):.2f}x")

    kill_tree(srv)

    print("\n" + "=" * 112)
    print(f"{'VU':>5} {'LF RPS':>8} {'k6 RPS':>8} {'RPSx':>6} "
          f"{'LF p95':>8} {'k6 p95':>8} {'LF CPU':>7} {'k6 CPU':>7} "
          f"{'LF mem':>7} {'k6 mem':>7} {'eff x':>7}")
    print("-" * 112)
    for vu, lf, k6 in rows:
        print(f"{vu:>5} {lf['rps']:>8.0f} {k6['rps']:>8.0f} {lf['rps']/max(k6['rps'],1e-9):>5.2f}x "
              f"{lf['p95']:>7.1f}ms {k6['p95']:>7.1f}ms {lf['cpu_secs']:>6.1f}s {k6['cpu_secs']:>6.1f}s "
              f"{lf['peak_mb']:>6.0f}MB {k6['peak_mb']:>6.0f}MB {eff(lf)/max(eff(k6),1e-9):>6.2f}x")
    print("=" * 112)

    out = {"duration": DURATION, "workers": WORKERS,
           "target": "node bench_server_cluster.js", "levels": []}
    for vu, lf, k6 in rows:
        out["levels"].append({"vu": vu, "loadforge": lf, "k6": k6})
    with open(os.path.join(HERE, "bench_concurrency_cpu_results.json"),
              "w", encoding="utf-8") as f:
        json.dump(out, f, indent=2, ensure_ascii=False)
    print("\nSaved tests/bench_concurrency_cpu_results.json")


if __name__ == "__main__":
    main()