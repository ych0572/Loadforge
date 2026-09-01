"""Verify HTTP/2 (h2c + h2-over-TLS), SSE, and WebSocket workloads."""
import os
import subprocess
import time

import loadforge

HERE = os.path.dirname(os.path.abspath(__file__))
NODE = r"D:\InterPreter\Nodejs\node.exe"
PROTO = os.path.join(HERE, "proto_servers.js")
CERT = os.path.join(HERE, "certs", "server.crt")
KEY = os.path.join(HERE, "certs", "server.key")


def start(mode, port, *extra):
    p = subprocess.Popen([NODE, PROTO, mode, str(port), *extra],
                         stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    time.sleep(0.8)
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


def main():
    procs = []
    try:
        _run(procs)
    finally:
        for p in procs:
            stop(p)


def _run(procs):
    # 1. HTTP/2 plaintext (h2c prior knowledge)
    print("== 1. HTTP/2 (h2c) ==")
    p = start("http2", 18090)
    procs.append(p)
    r = loadforge.run({
        "base_url": "http://127.0.0.1:18090", "vu": 20, "duration": 2,
        "http2": True,
        "endpoints": [{"method": "GET", "path": "/api/test", "weight": 1}],
    })
    print(f"  total={r['total']} success={r['success']} failed={r['failed']} rps={r['rps']:.0f} status={dict(r['status_codes'])}")
    assert r["total"] > 0 and r["success"] == r["total"] and 200 in r["status_codes"]
    procs.append(p)

    # 2. HTTP/2 over TLS
    print("== 2. HTTP/2 over TLS ==")
    p = start("http2s", 18091, KEY, CERT)
    r = loadforge.run({
        "base_url": "https://127.0.0.1:18091", "vu": 20, "duration": 2,
        "http2": True, "insecure": True,
        "endpoints": [{"method": "GET", "path": "/api/test", "weight": 1}],
    })
    print(f"  total={r['total']} success={r['success']} failed={r['failed']} rps={r['rps']:.0f} status={dict(r['status_codes'])}")
    assert r["total"] > 0 and r["success"] == r["total"] and 200 in r["status_codes"]
    procs.append(p)

    # 3. SSE
    print("== 3. SSE ==")
    p = start("sse", 18092)
    r = loadforge.run({
        "base_url": "http://127.0.0.1:18092", "vu": 20, "duration": 2,
        "sse": {"path": "/events"},
    })
    print(f"  events={r['total']} success={r['success']} failed={r['failed']} rps={r['rps']:.0f}")
    assert r["total"] > 0 and r["success"] == r["total"]
    procs.append(p)

    # 4. WebSocket echo
    print("== 4. WebSocket ==")
    p = start("ws", 18093)
    r = loadforge.run({
        "vu": 20, "duration": 2,
        "ws": {"url": "ws://127.0.0.1:18093/ws", "message": "hello"},
    })
    print(f"  msgs={r['total']} success={r['success']} failed={r['failed']} "
          f"rps={r['rps']:.0f} p50={r['p50_ms']:.2f}ms p95={r['p95_ms']:.2f}ms status={dict(r['status_codes'])}")
    assert r["total"] > 0 and r["success"] == r["total"] and 101 in r["status_codes"]
    assert r["p95_ms"] >= 0
    procs.append(p)

    print("\nALL CHECKS PASSED")


if __name__ == "__main__":
    main()