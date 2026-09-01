"""Verify expect assertions (with failure details) and iterations mode."""
import json
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

import loadforge


class H(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def _send(self, code, obj):
        body = json.dumps(obj).encode()
        self.send_response(code)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):
        if self.path == "/api/ok":
            self._send(200, {"code": 0, "data": {"name": "alice", "id": 42}})
        elif self.path == "/api/wrong_status":
            self._send(201, {"code": 0})
        elif self.path == "/api/wrong_body":
            self._send(200, {"name": "bob"})
        else:
            self._send(404, {"error": "not found"})

    def log_message(self, *a):
        pass


def main():
    srv = ThreadingHTTPServer(("127.0.0.1", 18087), H)
    srv.daemon_threads = True
    threading.Thread(target=srv.serve_forever, daemon=True).start()

    base = "http://127.0.0.1:18087"

    # 1. 断言全部通过
    print("== 1. expect all pass ==")
    r = loadforge.run({
        "base_url": base, "vu": 5, "duration": 1,
        "endpoints": [{
            "method": "GET", "path": "/api/ok", "weight": 1,
            "expect": {
                "status": 200,
                "body_contains": "alice",
                "json": {"data": {"name": "alice", "id": 42}},
            },
        }],
    })
    print(f"  total={r['total']} checks_passed={r['checks_passed']} checks_failed={r['checks_failed']}")
    assert r["checks_failed"] == 0
    assert r["checks_passed"] == r["total"] * 3
    assert r["check_failures"] == []

    # 2. status 断言失败 + 详情
    print("== 2. status assertion failure ==")
    r = loadforge.run({
        "base_url": base, "vu": 5, "duration": 1,
        "endpoints": [{"method": "GET", "path": "/api/wrong_status", "weight": 1,
                       "expect": {"status": 200}}],
    })
    print(f"  checks_failed={r['checks_failed']}  sample={r['check_failures'][:2]}")
    assert r["checks_failed"] == r["total"]
    f = r["check_failures"][0]
    assert f["check"] == "status" and f["expected"] == "200" and f["actual"] == "201"
    assert f["method"] == "GET" and f["path"] == "/api/wrong_status"

    # 3. json 断言失败 + 详情
    print("== 3. json assertion failure ==")
    r = loadforge.run({
        "base_url": base, "vu": 5, "duration": 1,
        "endpoints": [{"method": "GET", "path": "/api/wrong_body", "weight": 1,
                       "expect": {"json": {"name": "alice"}}}],
    })
    print(f"  checks_failed={r['checks_failed']}  sample={r['check_failures'][:2]}")
    f = r["check_failures"][0]
    assert f["check"] == "json.name"
    assert f["expected"] == '"alice"' and f["actual"] == '"bob"'

    # 4. iterations 固定次数（endpoints）
    print("== 4. iterations (endpoints) ==")
    r = loadforge.run({
        "base_url": base, "vu": 10, "iterations": 100,
        "endpoints": [{"method": "GET", "path": "/api/ok", "weight": 1,
                       "expect": {"status": 200}}],
    })
    print(f"  total={r['total']} (expect 100)")
    assert r["total"] == 100, f"expected exactly 100, got {r['total']}"
    assert r["checks_failed"] == 0

    # 5. iterations 固定次数（flow，每轮 3 步）
    print("== 5. iterations (flow, 3 steps) ==")
    r = loadforge.run({
        "base_url": base, "vu": 3, "iterations": 10,
        "flow": [
            {"method": "GET", "path": "/api/ok"},
            {"method": "GET", "path": "/api/ok"},
            {"method": "GET", "path": "/api/ok"},
        ],
    })
    print(f"  total={r['total']} (expect 30)")
    assert r["total"] == 30, f"expected exactly 30, got {r['total']}"

    srv.shutdown()
    print("\nALL CHECKS PASSED")


if __name__ == "__main__":
    main()