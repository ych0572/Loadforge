import subprocess, time
import loadforge

NODE = r"D:\InterPreter\Nodejs\node.exe"
PROTO = r"tests\proto_servers.js"

def start(mode, port):
    p = subprocess.Popen([NODE, PROTO, mode, str(port)], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    time.sleep(0.8)
    return p

def stop(p):
    try:
        p.terminate(); p.wait(timeout=5)
    except Exception:
        try: p.kill()
        except Exception: pass

# 1. WS send 模式（fire-and-forget，服务器丢弃不回）
print("== WS send (fire-and-forget) ==")
p = start("wssink", 18260)
r = loadforge.run({"vu": 20, "duration": 2,
                   "ws": {"url": "ws://127.0.0.1:18260/ws", "message": "ping", "mode": "send"}})
print(f"  sent={r['total']} success={r['success']} failed={r['failed']} rps={r['rps']:.0f} p95={r['p95_ms']:.2f}ms")
assert r["total"] > 0 and r["failed"] == 0 and r["p95_ms"] == 0.0
stop(p)

# 2. WS recv 模式（服务器每 10ms 推一条）
print("== WS recv (receive-only) ==")
p = start("wspush", 18261)
r = loadforge.run({"vu": 10, "duration": 2,
                   "ws": {"url": "ws://127.0.0.1:18261/ws", "mode": "recv"}})
print(f"  recv={r['total']} success={r['success']} failed={r['failed']} rps={r['rps']:.0f}")
assert r["total"] > 0 and r["failed"] == 0
stop(p)

# 3. WS echo 模式仍然工作（回归）
print("== WS echo (round-trip) ==")
p = start("ws", 18262)
r = loadforge.run({"vu": 10, "duration": 2,
                   "ws": {"url": "ws://127.0.0.1:18262/ws", "message": "ping"}})
print(f"  total={r['total']} failed={r['failed']} p95={r['p95_ms']:.2f}ms")
assert r["total"] > 0 and r["failed"] == 0  # 本机回环 RTT 亚毫秒，p95 可能为 0.0
stop(p)

# 4. SSE expect（逐事件内容断言）
print("== SSE with expect ==")
p = start("sse", 18263)
r = loadforge.run({"base_url": "http://127.0.0.1:18263", "vu": 10, "duration": 2,
                   "sse": {"path": "/events", "expect": {"body_contains": "hello"}}})
print(f"  events={r['total']} checks_passed={r['checks_passed']} checks_failed={r['checks_failed']}")
assert r["total"] > 0 and r["checks_failed"] == 0 and r["checks_passed"] == r["total"]
stop(p)

# 5. SSE expect 失败（内容不匹配）
print("== SSE expect failure ==")
r_fail = None
# 用一个 body_contains 不匹配的断言
p = start("sse", 18264)
r_fail = loadforge.run({"base_url": "http://127.0.0.1:18264", "vu": 5, "duration": 1,
                        "sse": {"path": "/events", "expect": {"body_contains": "NOPE"}}})
print(f"  events={r_fail['total']} checks_failed={r_fail['checks_failed']} sample={r_fail['check_failures'][:1]}")
assert r_fail["checks_failed"] == r_fail["total"]
stop(p)

print("\nALL CHECKS PASSED")