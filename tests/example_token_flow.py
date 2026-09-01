"""
token-in-body 业务流程示例（可运行）

场景：每个 VU 每轮迭代
  1. POST /api/login  -> 响应 JSON body 里带 token + uid
  2. GET  /api/me     -> 用 Authorization: Bearer ${token}
  3. POST /api/order  -> 用 ${token} + ${uid}
登录本身也在 VU 范围内，会被计入 total / rps。
"""
import json
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

import loadforge

# 期望的 token / uid，服务端用来校验
TOKEN = "tok-123456"
UID = 42


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def _send(self, code, obj):
        body = json.dumps(obj).encode()
        self.send_response(code)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def _auth_ok(self):
        return self.headers.get("Authorization") == f"Bearer {TOKEN}"

    def do_POST(self):
        n = int(self.headers.get("Content-Length", 0))
        self.rfile.read(n)
        if self.path == "/api/login":
            # token 放在 JSON body 里返回
            self._send(200, {"code": 0, "data": {"token": TOKEN, "uid": UID}})
        elif self.path == "/api/order":
            if self._auth_ok():
                self._send(200, {"ok": True, "order_id": 1001})
            else:
                self._send(401, {"error": "unauthorized"})
        else:
            self._send(404, {"error": "not found"})

    def do_GET(self):
        if self.path == "/api/me":
            if self._auth_ok():
                self._send(200, {"id": UID, "name": "alice"})
            else:
                self._send(401, {"error": "unauthorized"})
        else:
            self._send(404, {"error": "not found"})

    def log_message(self, *a):
        pass


def main():
    port = 18086
    srv = ThreadingHTTPServer(("127.0.0.1", port), Handler)
    srv.daemon_threads = True
    threading.Thread(target=srv.serve_forever, daemon=True).start()

    # ---- 核心：业务流程定义 ----
    flow = [
        {
            "method": "POST",
            "path": "/api/login",
            "headers": {"Content-Type": "application/json"},
            "body": '{"username": "alice", "password": "secret"}',
            # 从响应 JSON body 提取 token 和 uid
            "extract": {"token": "data.token", "uid": "data.uid"},
        },
        {
            "method": "GET",
            "path": "/api/me",
            "headers": {"Authorization": "Bearer ${token}"},
        },
        {
            "method": "POST",
            "path": "/api/order",
            "headers": {"Authorization": "Bearer ${token}"},
            "body": '{"user_id": ${uid}, "item": "book"}',
        },
    ]

    result = loadforge.run({
        "base_url": f"http://127.0.0.1:{port}",
        "vu": 20,
        "duration": 5,
        "flow": flow,
    })

    print("=" * 52)
    print(f"  total     : {result['total']}")
    print(f"  success   : {result['success']}")
    print(f"  failed    : {result['failed']}")
    print(f"  rps       : {result['rps']:.0f}")
    print(f"  p95       : {result['p95_ms']:.1f}ms")
    print(f"  status    : {dict(result['status_codes'])}")
    print("=" * 52)

    # 关键断言：没有任何 401 -> 说明 token 提取+替换真正生效了
    assert 401 not in result["status_codes"], "token 没有正确传递！"
    assert result["failed"] == 0
    print("\n[OK] token 在 flow 内正确传递，登录也计入了压测")

    srv.shutdown()


if __name__ == "__main__":
    main()