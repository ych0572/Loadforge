// Multi-protocol test servers for Loadforge.
// Usage: node proto_servers.js <mode> <port> [keyFile certFile]
//   mode: http2 (h2c plaintext) | http2s (h2 over TLS) | sse | ws
const mode = process.argv[2];
const port = parseInt(process.argv[3] || "18080", 10);
const http = require("http");
const http2 = require("http2");
const crypto = require("crypto");
const fs = require("fs");

const GUID = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";

if (mode === "http2") {
  const s = http2.createServer();
  s.on("stream", (stream) => {
    stream.respond({ ":status": 200, "content-type": "application/json" });
    stream.end('{"ok":true}');
  });
  s.listen(port, "127.0.0.1");
} else if (mode === "http2s") {
  const s = http2.createSecureServer({
    key: fs.readFileSync(process.argv[4]),
    cert: fs.readFileSync(process.argv[5]),
    allowHTTP1: false,
  });
  s.on("stream", (stream) => {
    stream.respond({ ":status": 200, "content-type": "application/json" });
    stream.end('{"ok":true}');
  });
  s.listen(port, "127.0.0.1");
} else if (mode === "h2both") {
  const s = http2.createSecureServer({
    key: fs.readFileSync(process.argv[4]),
    cert: fs.readFileSync(process.argv[5]),
    allowHTTP1: true,
  });
  // With allowHTTP1:true, Node routes BOTH h2 streams and h1 requests
  // through the 'request' event (compat layer). Registering a 'stream'
  // handler here would double-respond and crash the server.
  s.on("request", (req, res) => {
    res.writeHead(200, { "content-type": "application/json" });
    res.end('{"ok":true}');
  });
  s.listen(port, "127.0.0.1");
} else if (mode === "sse") {
  const s = http.createServer((req, res) => {
    res.writeHead(200, {
      "Content-Type": "text/event-stream",
      "Cache-Control": "no-cache",
    });
    const timer = setInterval(() => res.write("data: hello\n\n"), 10);
    req.on("close", () => clearInterval(timer));
  });
  s.listen(port, "127.0.0.1");
} else if (mode === "ws") {
  const s = http.createServer();
  s.on("upgrade", (req, socket) => {
    const key = req.headers["sec-websocket-key"];
    const accept = crypto.createHash("sha1").update(key + GUID).digest("base64");
    socket.write(
      "HTTP/1.1 101 Switching Protocols\r\n" +
        "Upgrade: websocket\r\n" +
        "Connection: Upgrade\r\n" +
        "Sec-WebSocket-Accept: " + accept + "\r\n\r\n"
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
  s.listen(port, "127.0.0.1");
} else if (mode === "wssink") {
  // accept and discard frames (no echo) - for ws send mode testing
  const s = http.createServer();
  s.on("upgrade", (req, socket) => {
    const key = req.headers["sec-websocket-key"];
    const accept = crypto.createHash("sha1").update(key + GUID).digest("base64");
    socket.write(
      "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: " +
        accept + "\r\n\r\n"
    );
    socket.on("data", () => {});
  });
  s.listen(port, "127.0.0.1");
} else if (mode === "wspush") {
  // accept and push a text frame every 10ms - for ws recv mode testing
  const s = http.createServer();
  s.on("upgrade", (req, socket) => {
    const key = req.headers["sec-websocket-key"];
    const accept = crypto.createHash("sha1").update(key + GUID).digest("base64");
    socket.write(
      "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: " +
        accept + "\r\n\r\n"
    );
    const timer = setInterval(() => sendFrame(socket, 0x1, Buffer.from("push")), 10);
    socket.on("close", () => clearInterval(timer));
    socket.on("data", () => {});
  });
  s.listen(port, "127.0.0.1");
}

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