"""Loadforge experiment harness: functional + high-concurrency verification.

Uses a thread-free asyncio HTTP responder so the target server does not
explode OS threads under 100K VU. The engine under test is loadforge.
"""
import asyncio
import threading
import time
import json
import sys

import loadforge


# ----------------------------------------------------------------------
# Minimal asyncio HTTP responder (one event loop, no thread-per-connection)
# ----------------------------------------------------------------------
async def handle(reader, writer):
    try:
        data = await reader.readuntil(b"\r\n\r\n")
    except (asyncio.IncompleteReadError, asyncio.LimitOverrunError):
        writer.close()
        try:
            await writer.wait_closed()
        except Exception:
            pass
        return

    # parse request line only; body is irrelevant for this responder
    try:
        request_line = data.split(b"\r\n", 1)[0].decode("latin1")
    except Exception:
        request_line = "GET / HTTP/1.1"

    body = b'{"status":"ok"}'
    resp = (
        b"HTTP/1.1 200 OK\r\n"
        b"Content-Type: application/json\r\n"
        b"Content-Length: " + str(len(body)).encode() + b"\r\n"
        b"Connection: close\r\n"
        b"\r\n" + body
    )
    try:
        writer.write(resp)
        await writer.drain()
    except Exception:
        pass
    finally:
        writer.close()
        try:
            await writer.wait_closed()
        except Exception:
            pass


class ServerThread:
    """Run the asyncio server in a dedicated background thread."""
    def __init__(self, port):
        self.port = port
        self.loop = None
        self.server = None
        self.thread = None

    def start(self):
        ready = threading.Event()
        self.thread = threading.Thread(target=self._run, args=(ready,), daemon=True)
        self.thread.start()
        ready.wait(5)

    def _run(self, ready):
        self.loop = asyncio.new_event_loop()
        asyncio.set_event_loop(self.loop)
        self.server = self.loop.run_until_complete(
            asyncio.start_server(handle, "127.0.0.1", self.port)
        )
        ready.set()
        self.loop.run_forever()

    def stop(self):
        if self.server is not None and self.loop is not None:
            self.server.close()
            self.loop.call_soon_threadsafe(self.loop.stop)


def run_plan(plan):
    t0 = time.perf_counter()
    result = loadforge.run(plan)
    wall = time.perf_counter() - t0
    return result, wall


def show(title, result, wall):
    total = result["total"]
    ok = result["success"] / total * 100 if total else 0.0
    print(f"--- {title} ---")
    print(f"  total   : {total}")
    print(f"  success : {result['success']}  ({ok:.1f}%)")
    print(f"  failed  : {result['failed']}")
    print(f"  rps     : {result['rps']:.0f}")
    print(f"  p50/p95/p99 : {result['p50_ms']:.2f}/{result['p95_ms']:.2f}/{result['p99_ms']:.2f} ms")
    print(f"  elapsed : {result['elapsed_secs']:.2f}s (engine) / {wall:.2f}s (wall)")
    print(f"  status  : {dict(result['status_codes'])}")


def main():
    out = {}

    # ==================================================================
    # 1. Functional verification (full chain)
    # ==================================================================
    print("=" * 60)
    print("1. Functional verification (full chain)")
    print("=" * 60)
    srv = ServerThread(18080)
    srv.start()
    try:
        plan = {
            "base_url": "http://127.0.0.1:18080",
            "vu": 100,
            "duration": 3,
            "endpoints": [
                {"method": "GET", "path": "/api/users", "weight": 5},
                {"method": "GET", "path": "/api/products", "weight": 3},
                {"method": "POST", "path": "/api/orders", "weight": 2,
                 "body": '{"product_id": "test-001", "amount": 100}'},
            ],
        }
        result, wall = run_plan(plan)
        show("Functional (100 VU / 3s)", result, wall)
        out["functional"] = {
            "vu": 100, "duration": 3,
            "total": result["total"], "success": result["success"],
            "failed": result["failed"], "rps": result["rps"],
            "p50_ms": result["p50_ms"], "p95_ms": result["p95_ms"],
            "p99_ms": result["p99_ms"], "status_codes": dict(result["status_codes"]),
        }
    finally:
        srv.stop()
    print()

    # ==================================================================
    # 2. High-concurrency verification
    # ==================================================================
    print("=" * 60)
    print("2. High-concurrency verification (1K / 10K / 100K VU)")
    print("=" * 60)
    for vu, dur in [(1000, 2), (10000, 2), (100000, 2)]:
        port = {1000: 18081, 10000: 18082, 100000: 18083}[vu]
        srv = ServerThread(port)
        srv.start()
        try:
            plan = {
                "base_url": f"http://127.0.0.1:{port}",
                "vu": vu,
                "duration": dur,
                "endpoints": [{"method": "GET", "path": "/api/test", "weight": 1}],
            }
            print(f"\n>>> {vu} VU / {dur}s")
            result, wall = run_plan(plan)
            show(f"{vu} VU", result, wall)
            out[f"concurrency_{vu}"] = {
                "vu": vu, "duration": dur,
                "total": result["total"], "success": result["success"],
                "failed": result["failed"], "rps": result["rps"],
                "p50_ms": result["p50_ms"], "p95_ms": result["p95_ms"],
                "p99_ms": result["p99_ms"], "status_codes": dict(result["status_codes"]),
            }
        finally:
            srv.stop()
    print()

    print("=" * 60)
    print(json.dumps(out, indent=2))
    print("=" * 60)

    with open("evaluation/results.json", "w", encoding="utf-8") as f:
        json.dump(out, f, indent=2)
    print("Wrote evaluation/results.json")


if __name__ == "__main__":
    main()
