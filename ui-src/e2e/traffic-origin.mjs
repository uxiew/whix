// Synthetic loopback peers. The browser controls the console; these sockets
// prove what actually crosses the proxy, independently of its capture APIs.
import http from 'node:http';
import net from 'node:net';
import { createHash, randomBytes } from 'node:crypto';

let uploads = [], fromClient = [], fromServer = [];
let client, serverPeer;
const sockets = new Set();
function track(socket) {
  sockets.add(socket);
  socket.on('close', () => sockets.delete(socket));
  // Node HTTP upgrade sockets allow half-open TCP; finish our own half only
  // after observing the proxy's EOF, as a WebSocket peer would.
  socket.on('end', () => socket.end());
  socket.on('error', () => {}); // Disconnect is an expected assertion.
  return socket;
}
function frame(opcode, bytes, masked) {
  if (bytes.length >= 126) throw new Error('Fixture only writes short frames');
  const head = Buffer.from([0x80 | opcode, (masked ? 0x80 : 0) | bytes.length]);
  if (!masked) return Buffer.concat([head, bytes]);
  const key = randomBytes(4);
  return Buffer.concat([head, key, Buffer.from(bytes.map((b, i) => b ^ key[i % 4]))]);
}
function frames(socket, record, initial = Buffer.alloc(0)) {
  let pending = initial;
  function consume(chunk = Buffer.alloc(0)) {
    pending = Buffer.concat([pending, chunk]);
    while (pending.length >= 2) {
      const opcode = pending[0] & 15, masked = !!(pending[1] & 128);
      let length = pending[1] & 127, offset = 2;
      if (length === 126) {
        if (pending.length < 4) return;
        length = pending.readUInt16BE(2); offset = 4;
      } else if (length === 127) {
        if (pending.length < 10) return;
        length = Number(pending.readBigUInt64BE(2)); offset = 10;
      }
      if (length > 8 * 1024 * 1024) return socket.destroy();
      const keyLength = masked ? 4 : 0;
      if (pending.length < offset + keyLength + length) return;
      const key = pending.subarray(offset, offset + keyLength);
      const bytes = Buffer.from(pending.subarray(offset + keyLength, offset + keyLength + length));
      if (masked) for (let i = 0; i < bytes.length; i++) bytes[i] ^= key[i % 4];
      pending = pending.subarray(offset + keyLength + length);
      if (opcode === 9) socket.write(frame(10, bytes, !masked));
      else if (opcode === 8) socket.end(frame(8, bytes, !masked));
      else if (opcode === 1 || opcode === 2) record.push({ opcode, masked, base64: bytes.toString('base64') });
    }
  }
  socket.on('data', consume);
  consume();
}
function rawBytes(socket, record, initial = Buffer.alloc(0)) {
  const capture = (bytes) => { if (bytes.length) record.push({ base64: bytes.toString('base64') }); };
  capture(initial);
  socket.on('data', capture);
}
async function openClient(kind = 'websocket') {
  client = track(net.connect(25198, '127.0.0.1'));
  const socket = client;
  await new Promise((resolve, reject) => {
    const timeout = setTimeout(() => { socket.destroy(); reject(new Error('Upgrade timed out')); }, 3000);
    let pending = Buffer.alloc(0);
    const fail = () => { clearTimeout(timeout); reject(new Error('Upgrade closed')); };
    socket.once('close', fail);
    const wsHeaders = kind === 'websocket' ? 'Sec-WebSocket-Version: 13\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n' : '';
    socket.once('connect', () => socket.write(`GET http://traffic.test/socket HTTP/1.1\r\nHost: traffic.test\r\nConnection: Upgrade\r\nUpgrade: ${kind}\r\n${wsHeaders}\r\n`));
    function header(chunk) {
      pending = Buffer.concat([pending, chunk]);
      const end = pending.indexOf('\r\n\r\n');
      if (end < 0) return;
      clearTimeout(timeout); socket.off('close', fail); socket.off('data', header);
      if (!pending.toString().startsWith('HTTP/1.1 101')) { socket.destroy(); reject(new Error('Not upgraded')); return; }
      (kind === 'raw' ? rawBytes : frames)(socket, fromServer, pending.subarray(end + 4));
      resolve();
    }
    socket.on('data', header);
  });
}
const origin = http.createServer(async (req, res) => {
  const chunks = [];
  for await (const chunk of req) chunks.push(chunk);
  const body = Buffer.concat(chunks);
  try {
    if (req.url === '/health') return res.end('ok');
    if (req.url === '/reset') {
      client?.destroy(); serverPeer?.destroy();
      uploads = []; fromClient = []; fromServer = [];
      client = undefined; serverPeer = undefined;
    } else if (req.url === '/ws/open' || req.url === '/tcp/open') {
      await openClient(req.url === '/tcp/open' ? 'raw' : 'websocket');
    } else if (['/ws/client-send', '/ws/server-send', '/tcp/client-send', '/tcp/server-send'].includes(req.url)) {
      const { base64, opcode = 2 } = JSON.parse(body);
      const peer = req.url.endsWith('client-send') ? client : serverPeer;
      if (!peer || peer.destroyed) throw new Error('Peer not live');
      const bytes = Buffer.from(base64, 'base64');
      peer.write(req.url.startsWith('/tcp/') ? bytes : frame(opcode, bytes, peer === client));
    } else if (req.url?.startsWith('/upload')) {
      uploads.push({ base64: body.toString('base64'), headers: req.headers });
    } else if (req.url !== '/state') {
      res.writeHead(404); return res.end('not found');
    }
    res.setHeader('Content-Type', 'application/json');
    res.end(JSON.stringify({ uploads, fromClient, fromServer, clientLive: !!client && !client.destroyed, serverLive: !!serverPeer && !serverPeer.destroyed, serverReadEnded: serverPeer?.readableEnded, serverWriteEnded: serverPeer?.writableEnded }));
  } catch (error) { res.writeHead(500); res.end(String(error)); }
});
origin.on('upgrade', (req, socket, head) => {
  serverPeer = track(socket);
  if (req.headers.upgrade === 'raw') {
    socket.write('HTTP/1.1 101 Switching Protocols\r\nUpgrade: raw\r\nConnection: Upgrade\r\n\r\n');
    rawBytes(socket, fromClient, head);
    return;
  }
  const accept = createHash('sha1').update(req.headers['sec-websocket-key'] + '258EAFA5-E914-47DA-95CA-C5AB0DC85B11').digest('base64');
  socket.write('HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: ' + accept + '\r\n\r\n');
  frames(socket, fromClient, head);
});
origin.listen(25197, '127.0.0.1');
for (const signal of ['SIGINT', 'SIGTERM']) process.on(signal, () => {
  for (const socket of sockets) socket.destroy();
  origin.close(() => process.exit(0));
});
