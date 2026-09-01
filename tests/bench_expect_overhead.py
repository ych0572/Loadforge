"""Measure the overhead of `expect` assertions during load testing."""
import ctypes, json, os, site, subprocess, sys, tempfile, time
from ctypes import wintypes

NODE = r"D:\InterPreter\Nodejs\node.exe"
CLUSTER = r"tests\bench_server_cluster.js"
WORKER = r"tests\_lf_worker2.py"
BASE_PY = getattr(sys, "_base_executable", None) or os.path.join(sys.base_prefix, "python.exe")
VENV_SP = [p for p in site.getsitepackages() if p.endswith("site-packages")][0]

QUERY = 0x0400; VM_READ = 0x0010
class FT(ctypes.Structure):
    _fields_ = [("lo", wintypes.DWORD), ("hi", wintypes.DWORD)]
class PMC(ctypes.Structure):
    _fields_ = [("cb", wintypes.DWORD), ("pf", wintypes.DWORD), ("peak", ctypes.c_size_t),
                ("ws", ctypes.c_size_t), ("qpp", ctypes.c_size_t), ("qpu", ctypes.c_size_t),
                ("qnp", ctypes.c_size_t), ("qnu", ctypes.c_size_t), ("pf_u", ctypes.c_size_t),
                ("peakpf", ctypes.c_size_t)]
k32 = ctypes.WinDLL("kernel32", use_last_error=True); psapi = ctypes.WinDLL("psapi", use_last_error=True)

def measure(argv):
    env = dict(os.environ); env["PYTHONPATH"] = VENV_SP + os.pathsep + env.get("PYTHONPATH", "")
    p = subprocess.Popen(argv, stdout=subprocess.PIPE, stderr=subprocess.PIPE, env=env)
    h = k32.OpenProcess(QUERY | VM_READ, False, p.pid)
    out, err = p.communicate()
    cpu = 0.0; mem = 0
    if h:
        c = FT(); e = FT(); k = FT(); u = FT()
        if k32.GetProcessTimes(h, ctypes.byref(c), ctypes.byref(e), ctypes.byref(k), ctypes.byref(u)):
            cpu = ((((k.hi << 32) | k.lo) + ((u.hi << 32) | u.lo)) / 1e7)
        pmc = PMC(); pmc.cb = ctypes.sizeof(pmc)
        if psapi.GetProcessMemoryInfo(h, ctypes.byref(pmc), pmc.cb):
            mem = pmc.peak
        k32.CloseHandle(h)
    return p.returncode, out.decode(), cpu, mem / 1048576.0

def run(plan):
    fd, path = tempfile.mkstemp(suffix=".json"); os.close(fd)
    json.dump(plan, open(path, "w", encoding="utf-8"))
    try:
        rc, out, cpu, mem = measure([BASE_PY, WORKER, path])
        d = json.loads(out.strip().splitlines()[-1])
    finally:
        os.remove(path)
    return d, cpu, mem

def main():
    port = 18244
    srv = subprocess.Popen([NODE, CLUSTER, str(port), "22"], stdin=subprocess.PIPE,
                           stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    time.sleep(2.0)

    cases = [
        ("no expect", None),
        ("status only", {"status": 200}),
        ("status+body+json", {"status": 200, "body_contains": "ok",
                              "json": {"status": "ok"}}),
    ]
    print(f"{'case':<18} {'rps':>8} {'p95':>7} {'cpu':>6} {'mem':>6} {'check_fail':>10}")
    for name, expect in cases:
        ep = {"method": "GET", "path": "/api/test", "weight": 1}
        if expect is not None:
            ep["expect"] = expect
        d, cpu, mem = run({"base_url": f"http://127.0.0.1:{port}", "vu": 500, "duration": 4,
                           "endpoints": [ep]})
        print(f"{name:<18} {d['rps']:>8.0f} {d['p95_ms']:>6.1f}ms {cpu:>5.1f}s {mem:>5.0f}MB {d['checks_failed']:>10}")

    try:
        srv.stdin.close(); srv.wait(timeout=8)
    except Exception:
        srv.terminate()

main()