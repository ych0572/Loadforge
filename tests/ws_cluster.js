// Multi-process WebSocket echo server for high-VU benchmarking.
// Usage: node ws_cluster.js <port> [workers]
const cluster = require("cluster");
const http = require("http");
const crypto = require("crypto");
const os = require("os");

const PORT = parseInt(process.argv[2] || "18093", 10);
const WORKERS = parseInt(process.argv[3] || String(os.cpus().length), 10);
const GUID = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";

function sendFrame(socket, opcode, payload) {
  const len = payload.length;
  let header;
  if (len < 126) {
    header = Buffer.from([0x80 | opcode, len]);
  } else if (len <= 0xffff) {
    header = Buffer.alloc(4);
    header[0] = 0x80 | opcode;
    header[1] = 126;
    header.writeUInt16BE(len, 2);
  } else {
    header = Buffer.alloc(10);
    header[0] = 0x80 | opcode;
    header[1] = 127;
    header.writeBigUInt64BE(BigInt(len), 2);
  }
  socket.write(Buffer.concat([header, payload]));
}

if (cluster.isPrimary) {
  for (let i = 0; i < WORKERS; i++) cluster.fork();
  cluster.on("exit", () => cluster.fork());
  process.stdin.resume();
  process.stdin.on("end", () => {
    for (const id in cluster.workers) cluster.workers[id].kill();
    setTimeout(() => process.exit(0), 100);
  });
} else {
  const server = http.createServer();
  server.on("upgrade", (req, socket) => {
    const key = req.headers["sec-websocket-key"];
    const accept = crypto.createHash("sha1").update(key + GUID).digest("base64");
    socket.write(
      "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: " +
        accept + "\r\n\r\n"
    );

    let buffer = Buffer.alloc(0);
    socket.on("data", (chunk) => {
      buffer = Buffer.concat([buffer, chunk]);
      while (true) {
        if (buffer.length < 2) break;
        const opcode = buffer[0] & 0x0f;
        const masked = (buffer[1] & 0x80) !== 0;
        let len = buffer[1] & 0x7f;
        let off = 2;
        if (len === 126) {
          if (buffer.length < 4) break;
          len = buffer.readUInt16BE(2);
          off = 4;
        } else if (len === 127) {
          if (buffer.length < 10) break;
          len = Number(buffer.readBigUInt64BE(2));
          off = 10;
        }
        let maskKey = null;
        if (masked) {
          if (buffer.length < off + 4) break;
          maskKey = buffer.slice(off, off + 4);
          off += 4;
        }
        if (buffer.length < off + len) break;
        let payload = buffer.slice(off, off + len);
        if (masked) {
          const un = Buffer.alloc(len);
          for (let i = 0; i < len; i++) un[i] = payload[i] ^ maskKey[i % 4];
          payload = un;
        }
        buffer = buffer.slice(off + len);

        if (opcode === 0x8) {
          socket.end();
          return;
        } else if (opcode === 0x9) {
          sendFrame(socket, 0xa, payload);
        } else if (opcode === 0x1 || opcode === 0x2) {
          sendFrame(socket, opcode, payload);
        }
      }
    });
  });
  server.listen(PORT, "127.0.0.1");
}