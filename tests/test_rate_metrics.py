"""Verify new features: min/max/avg + percentiles, ramp_up, rps arrival rate.

ramp_up is verified via a Node probe server that timestamps every accepted
connection: with a linear ramp, connections arrive gradually over `ramp_up`.
"""
import os
import subprocess
import time

import loadforge

HERE = os.path.dirname(os.path.abspath(__file__))
NODE = r"D:\InterPreter\Nodejs\node.exe"
PROBE = os.path.join(HERE, "ramp_probe.js")
OUTFILE = os.path.join(HERE, "_ramp_times.txt")
PORT = 18080


def start_server():
    p = subprocess.Popen([NODE, PROBE, str(PORT), OUTFILE],
                         stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    time.sleep(0.6)
    return p


def kill_server(p):
    try:
        p.terminate()
        p.wait(timeout=5)
    except Exception:
        try:
            p.kill()
        except Exception:
            pass


def clear_times():
    if os.path.exists(OUTFILE):
        os.remove(OUTFILE)


def read_times():
    if not os.path.exists(OUTFILE):
        return []
    with open(OUTFILE) as f:
        return [float(x) for x in f.read().split() if x.strip()]


def plan(vu, duration, **kw):
    p = {
        "base_url": f"http://127.0.0.1:{PORT}",
        "vu": vu, "duration": duration,
        "endpoints": [{"method": "GET", "path": "/api/test", "weight": 1}],
    }
    p.update(kw)
    return p


def main():
    srv = start_server()

    # 1. new result fields
    print("== 1. min/max/avg + percentiles ==")
    r = loadforge.run(plan(50, 2))
    print(f"  min={r['min_ms']:.3f} avg={r['avg_ms']:.3f} max={r['max_ms']:.3f}")
    print(f"  percentiles={ {k: round(v, 3) for k, v in sorted(r['percentiles'].items())} }")
    assert set(r["percentiles"]) == {50, 75, 90, 95, 99}
    ps = [r["percentiles"][p] for p in (50, 75, 90, 95, 99)]
    assert all(ps[i] <= ps[i + 1] for i in range(len(ps) - 1))
    assert r["min_ms"] <= r["avg_ms"] <= r["max_ms"]

    # 2. ramp_up: connection accept spread
    print("== 2. ramp_up (connection accept spread) ==")
    clear_times()
    loadforge.run(plan(100, 4, ramp_up=3))
    time.sleep(0.3)
    ramp = sorted(read_times())
    ramp_span = (ramp[-1] - ramp[0]) / 1000.0 if len(ramp) > 1 else 0.0
    print(f"  ramp_up=3s: {len(ramp)} connections, span={ramp_span:.2f}s")

    clear_times()
    loadforge.run(plan(100, 4))
    time.sleep(0.3)
    no = sorted(read_times())
    no_span = (no[-1] - no[0]) / 1000.0 if len(no) > 1 else 0.0
    print(f"  no ramp   : {len(no)} connections, span={no_span:.2f}s")

    assert ramp_span >= 2.0, f"ramp should spread ~3s, got {ramp_span:.2f}s"
    assert no_span < 1.0, f"no ramp should be near-instant, got {no_span:.2f}s"

    # 3. rps arrival-rate cap
    print("== 3. rps arrival-rate control ==")
    r_lim = loadforge.run(plan(50, 2, rps=100))
    print(f"  rps=100 (2s) total={r_lim['total']} measured_rps={r_lim['rps']:.1f}")
    assert 140 <= r_lim["total"] <= 260, f"expected ~200, got {r_lim['total']}"

    kill_server(srv)
    clear_times()
    print("\nALL CHECKS PASSED")


if __name__ == "__main__":
    main()