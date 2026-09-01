// High-performance HTTP server for benchmarking
// Node.js native http module, minimal response

const http = require("http");

const PORT = process.argv[2] || 18090;

const server = http.createServer((req, res) => {
  res.writeHead(200, { "Content-Type": "application/json" });
  res.end('{"status":"ok"}');
});

server.listen(PORT, "127.0.0.1", () => {
  console.log(`Server listening on :${PORT}`);
});
