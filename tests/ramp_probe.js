// Probe server: records the wall-clock time of every new TCP connection.
// Used to verify ramp-up (connections should arrive gradually).
// Usage: node ramp_probe.js [port] [outfile]
const http = require("http");
const fs = require("fs");

const PORT = parseInt(process.argv[2] || "18080", 10);
const OUTFILE = process.argv[3] || "_ramp_times.txt";

const server = http.createServer((req, res) => {
  res.writeHead(200, { "Content-Type": "application/json" });
  res.end('{"status":"ok"}');
});

server.on("connection", () => {
  fs.appendFileSync(OUTFILE, Date.now() + "\n");
});

server.listen(PORT, "127.0.0.1", () => {
  console.log("listening " + PORT);
});