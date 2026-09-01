// High-performance multi-process HTTP server for benchmarking.
// Graceful shutdown: when the parent closes stdin, kill all workers and exit.
const cluster = require("cluster");
const http = require("http");
const os = require("os");

const PORT = parseInt(process.argv[2] || "18090", 10);
const WORKERS = parseInt(process.argv[3] || String(os.cpus().length), 10);

if (cluster.isPrimary) {
  for (let i = 0; i < WORKERS; i++) cluster.fork();
  cluster.on("exit", () => cluster.fork());
  process.stdin.resume();
  process.stdin.on("end", () => {
    for (const id in cluster.workers) cluster.workers[id].kill();
    setTimeout(() => process.exit(0), 100);
  });
} else {
  const server = http.createServer((req, res) => {
    res.writeHead(200, { "Content-Type": "application/json" });
    res.end('{"status":"ok"}');
  });
  server.listen(PORT, "127.0.0.1");
}