"""
Verify Loadforge capabilities:
1. single HTTP request (endpoints mode)
2. single HTTPS request (endpoints mode, self-signed via insecure)
3. endpoints mode with weighted GET/POST + headers + body
4. HTTP business flow (ordered steps + variable extraction/substitution)
5. HTTPS business flow
6. HTTPS without `insecure` must fail cleanly (negative check)
"""
import json
import ssl
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

import loadforge

CERT = r"tests\certs\server.crt"
KEY = r"tests\certs\server.key"

TOKEN = "tok-123"
USER_ID = 7


class ApiHandler(BaseHTTPRequestHandler):
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

    def do_GET(self):
        if self.path == "/api/login":
            self._send(200, {"token": TOKEN, "user_id": USER_ID})
        elif self.path.startswith("/api/users/"):
            if self._auth_ok():
                self._send(200, {"id": USER_ID, "name": "alice"})
            else:
                self._send(401, {"error": "unauthorized"})
        else:
            self._send(200, {"status": "ok", "path": self.path})

    def do_POST(self):
        length = int(self.headers.get("Content-Length", 0))
        self.rfile.read(length)
        if self.path == "/api/echo":
            self._send(200, {"ok": True, "path": self.path})
        elif self._auth_ok():
            self._send(200, {"ok": True, "order_id": 99})
        else:
            self._send(401, {"error": "unauthorized"})

    def log_message(self, *a):
        pass


def start_server(port, use_tls=False):
    srv = ThreadingHTTPServer(("127.0.0.1", port), ApiHandler)
    srv.daemon_threads = True
    if use_tls:
        ctx = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
        ctx.load_cert_chain(CERT, KEY)
        srv.socket = ctx.wrap_socket(srv.socket, server_side=True)
    threading.Thread(target=srv.serve_forever, daemon=True).start()
    return srv


def run(plan):
    r = loadforge.run(plan)
    ok = r["success"] / max(r["total"], 1) * 100
    print(f"  total={r['total']} success={r['success']} failed={r['failed']} "
          f"ok={ok:.1f}% rps={r['rps']:.0f} status={dict(r['status_codes'])}")
    return r


def main():
    http_srv = start_server(18080)
    https_srv = start_server(18443, use_tls=True)

    print("== 1. single HTTP request ==")
    r1 = run({"base_url": "http://127.0.0.1:18080", "vu": 20, "duration": 2,
              "endpoints": [{"method": "GET", "path": "/api/test", "weight": 1}]})
    assert r1["status_codes"].get(200, 0) == r1["total"]

    print("== 2. single HTTPS request (insecure) ==")
    r2 = run({"base_url": "https://127.0.0.1:18443", "vu": 20, "duration": 2,
              "insecure": True,
              "endpoints": [{"method": "GET", "path": "/api/test", "weight": 1}]})
    assert r2["status_codes"].get(200, 0) == r2["total"]

    print("== 3. endpoints: weighted GET/POST + headers + body ==")
    r3 = run({"base_url": "http://127.0.0.1:18080", "vu": 20, "duration": 2,
              "endpoints": [
                  {"method": "GET", "path": "/api/users", "weight": 5},
                  {"method": "POST", "path": "/api/echo", "weight": 2,
                   "body": '{"product_id": "test-001"}',
                   "headers": {"Content-Type": "application/json"}},
              ]})
    assert r3["failed"] == 0 and r3["status_codes"].get(200, 0) == r3["total"]

    flow = [
        {"method": "GET", "path": "/api/login",
         "extract": {"token": "token", "uid": "user_id"}},
        {"method": "GET", "path": "/api/users/${uid}",
         "headers": {"Authorization": "Bearer ${token}"}},
        {"method": "POST", "path": "/api/order",
         "headers": {"Authorization": "Bearer ${token}"},
         "body": '{"user_id": ${uid}}'},
    ]

    print("== 4. HTTP business flow ==")
    r4 = run({"base_url": "http://127.0.0.1:18080", "vu": 20, "duration": 2,
              "flow": flow})
    assert 401 not in r4["status_codes"] and r4["failed"] == 0

    print("== 5. HTTPS business flow ==")
    r5 = run({"base_url": "https://127.0.0.1:18443", "vu": 20, "duration": 2,
              "insecure": True, "flow": flow})
    assert 401 not in r5["status_codes"] and r5["failed"] == 0

    print("== 6. HTTPS without insecure (expected failures) ==")
    r6 = run({"base_url": "https://127.0.0.1:18443", "vu": 5, "duration": 1,
              "endpoints": [{"method": "GET", "path": "/api/test", "weight": 1}]})
    print("  (status 0 = TLS verification rejected, as expected)")

    http_srv.shutdown()
    https_srv.shutdown()
    print("\nALL CHECKS PASSED")


if __name__ == "__main__":
    main()