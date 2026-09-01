// Multi-process HTTP/2 (TLS) server for high-VU benchmarking.
// Usage: node http2_cluster.js <port> <keyFile> <certFile> [workers]
const cluster = require("cluster");
const http2 = require("http2");
const fs = require("fs");
const os = require("os");

const PORT = parseInt(process.argv[2] || "18091", 10);
const KEY = process.argv[3];
const CERT = process.argv[4];
const WORKERS = parseInt(process.argv[5] || String(os.cpus().length), 10);

if (cluster.isPrimary) {
  for (let i = 0; i < WORKERS; i++) cluster.fork();
  cluster.on("exit", () => cluster.fork());
  process.stdin.resume();
  process.stdin.on("end", () => {
    for (const id in cluster.workers) cluster.workers[id].kill();
    setTimeout(() => process.exit(0), 100);
  });
} else {
  const server = http2.createSecureServer({
    key: fs.readFileSync(KEY),
    cert: fs.readFileSync(CERT),
    allowHTTP1: false,
  });
  server.on("stream", (stream) => {
    stream.respond({ ":status": 200, "content-type": "application/json" });
    stream.end('{"ok":true}');
  });
  server.listen(PORT, "127.0.0.1");
}