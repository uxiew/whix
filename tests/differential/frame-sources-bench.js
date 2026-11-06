// Remote frame scripts over real WS and CONNECT peers. Each proxy must meet
// the byte and fetch contracts independently, even when both fail identically.
'use strict';
const assert = require('node:assert/strict');
const crypto = require('node:crypto');
const fs = require('node:fs');
const http = require('node:http');
const net = require('node:net');
const WebSocket = require('ws');
const base = Number(process.env.PORT_BASE || 18700);
const [W, RS, ORIGIN, SOURCE, TCP] = [base, base + 1, base + 2, base + 3, base + 4];
const host = '127.0.0.1';
const jsonAt = process.argv.indexOf('--json');
const jsonPath = jsonAt < 0 ? null : process.argv[jsonAt + 1];
if (jsonAt >= 0 && !jsonPath) throw new Error('--json requires an output path');
// Identical handlers avoid conflating 2.10.8's known WS direction bug with
// source loading. The shared counter still proves per-connection VM lifetime.
const script = 'var n=0; var handle=function(buf){return "F"+(++n)+":"+buf;}; '
  + 'ctx.handleSendToServerFrame=handle; ctx.handleSendToClientFrame=handle;';
// A TCP read is not a message: this transform is invariant under segmentation.
const tcpScript = 'var handle=function(buf){return buf.toString().toUpperCase();}; '
  + 'ctx.handleSendToServerFrame=handle; ctx.handleSendToClientFrame=handle;';
const cases = [
  { name: 'WS source first connection', kind: 'ws', active: true },
  { name: 'WS same source fresh connection state', kind: 'ws', active: true },
  { name: 'inspect CONNECT source first connection', kind: 'tcp', inspect: true, active: true },
  { name: 'new inspect CONNECT fetches the same source again', kind: 'tcp', inspect: true, active: true },
  { name: 'non-inspect CONNECT does not fetch', kind: 'tcp' },
  { name: 'ordinary HTTP does not fetch', kind: 'http' },
  { name: 'WS ignores valid script in 404 body', kind: 'ws', missing: true },
  { name: 'inspect CONNECT ignores valid script in 404 body', kind: 'tcp', inspect: true, missing: true },
];
function deferred() {
  let resolve;
  const promise = new Promise((done) => { resolve = done; });
  return { promise, resolve };
}
const sockets = new Set();
function track(socket) {
  sockets.add(socket);
  socket.on('close', () => sockets.delete(socket));
  socket.on('error', () => {});
  return socket;
}
let current;
const event = (state, type, data) => state.events.push({ type, ...(data === undefined ? {} : { data }) });
const source = http.createServer((req, res) => {
  const state = current;
  if (!state) { res.writeHead(500); res.end(); return; }
  event(state, 'source-start', req.url);
  state.fetches.push(req.url);
  // A pre-handshake download deadlocks against this gate and fails the bounded
  // exchange. No timing sleep is needed to prove that the handshake comes first.
  Promise.all([state.handshake.promise, state.firstWrite.promise]).then(() => {
    if (res.destroyed) return;
    event(state, 'source-release');
    res.writeHead(req.url === '/404.js' ? 404 : 200, { 'content-type': 'text/plain' });
    res.once('finish', () => event(state, 'source-finish'));
    res.end(state.kind === 'tcp' ? tcpScript : script);
  });
});
const origin = http.createServer((req, res) => {
  event(current, 'origin-http', req.url);
  current.httpCount++;
  res.end('HTTP_BODY');
});
const wss = new WebSocket.Server({ server: origin });
wss.on('connection', (ws) => {
  const state = current;
  event(state, 'origin-connected');
  state.originCount++;
  ws.on('error', () => {});
  ws.on('message', (data) => {
    event(state, 'origin-data', String(data));
    state.origin.push(String(data));
    ws.send('echo:' + data);
  });
});
const tcp = net.createServer((socket) => {
  track(socket);
  const state = current;
  event(state, 'origin-connected');
  state.originCount++;
  socket.on('data', (data) => {
    event(state, 'origin-data', String(data));
    state.origin.push(String(data));
    socket.write('echo:' + data);
  });
});
for (const server of [source, origin]) server.on('connection', track);

function request(port, path, body, contentType) {
  return new Promise((resolve, reject) => {
    const req = http.request({ host, port, path, method: body === undefined ? 'GET' : 'POST',
      headers: body === undefined ? {} : { 'content-type': contentType, 'content-length': Buffer.byteLength(body) } }, (res) => {
      const parts = [];
      res.on('error', reject);
      res.on('data', (data) => parts.push(data));
      res.on('end', () => resolve({ status: res.statusCode, body: Buffer.concat(parts).toString() }));
    });
    req.on('error', reject);
    req.setTimeout(5000, () => req.destroy(new Error('HTTP request timeout')));
    req.end(body);
  });
}
async function setRules(port, text) {
  const result = port === W
    ? await request(port, '/cgi-bin/rules/add', 'name=Default&selected=1&value=' + encodeURIComponent(text), 'application/x-www-form-urlencoded')
    : await request(port, '/api/rules', text, 'text/plain');
  assert.equal(result.status, 200, 'rules configuration');
  if (port === W) assert.equal(JSON.parse(result.body).ec, 0, 'Whistle accepted rules');
}
function frame(text) {
  const bytes = Buffer.from(text), mask = crypto.randomBytes(4);
  assert.ok(bytes.length < 126, 'fixture uses short frames');
  return Buffer.concat([Buffer.from([0x81, 0x80 | bytes.length]), mask,
    Buffer.from(bytes.map((b, i) => b ^ mask[i % 4]))]);
}
function exchange(port, c, state) {
  return new Promise((resolve, reject) => {
    const socket = track(net.connect(port, host));
    let buffer = Buffer.alloc(0), upgraded = false, sent = 0, settled = false, tcpData = '';
    const received = [];
    const timer = setTimeout(() => finish(new Error('frame exchange timeout')), 5000);
    function finish(error) {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      socket.destroy();
      if (error) reject(error); else resolve(received);
    }
    function send() {
      const text = ['a', 'b'][sent++];
      event(state, 'client-send', text);
      socket.write(c.kind === 'ws' ? frame(text) : text, (error) => {
        if (error) return finish(error);
        if (sent === 1) state.firstWrite.resolve();
      });
    }
    function accept(text) {
      event(state, 'client-data', text);
      received.push(text);
      if (received.length === 2) finish(); else send();
    }
    socket.on('connect', () => {
      socket.write(c.kind === 'ws'
        ? `GET ws://${host}:${ORIGIN}/echo HTTP/1.1\r\nHost: ${host}:${ORIGIN}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: ${crypto.randomBytes(16).toString('base64')}\r\nSec-WebSocket-Version: 13\r\n\r\n`
        : `CONNECT ${host}:${TCP} HTTP/1.1\r\nHost: ${host}:${TCP}\r\n\r\n`);
    });
    socket.on('error', finish);
    socket.on('close', () => { if (!settled) finish(new Error('connection closed before two replies')); });
    socket.on('data', (data) => {
      try {
        buffer = Buffer.concat([buffer, data]);
        if (!upgraded) {
          const end = buffer.indexOf('\r\n\r\n');
          if (end < 0) return;
          const line = buffer.subarray(0, end).toString().split('\r\n')[0];
          assert.match(line, c.kind === 'ws' ? /^HTTP\/1\.[01] 101 / : /^HTTP\/1\.[01] 200 /);
          event(state, 'handshake');
          state.handshake.resolve();
          buffer = buffer.subarray(end + 4);
          upgraded = true;
          send();
        }
        if (c.kind === 'tcp') {
          tcpData += buffer.toString();
          buffer = Buffer.alloc(0);
          // TCP chunks need not match writes: wait for the complete current
          // echo marker, then assert all accumulated bytes in the final oracle.
          const last = received.length ? 'b' : 'a';
          const marker = c.active ? last.toUpperCase() : last;
          if (tcpData.endsWith(marker)) { const text = tcpData; tcpData = ''; accept(text); }
          return;
        }
        while (buffer.length >= 2 && !settled) {
          assert.equal(buffer[0], 0x81, 'one complete text frame');
          assert.equal(buffer[1] & 0x80, 0, 'server frame is not masked');
          const length = buffer[1] & 0x7f;
          assert.ok(length < 126, 'fixture response fits short frame');
          if (buffer.length < 2 + length) return;
          const text = buffer.subarray(2, 2 + length).toString();
          buffer = buffer.subarray(2 + length);
          accept(text);
        }
      } catch (error) { finish(error); }
    });
  });
}
async function ask(port, c) {
  const state = { kind: c.kind, events: [], fetches: [], origin: [], originCount: 0, httpCount: 0,
    handshake: deferred(), firstWrite: deferred() };
  current = state;
  let effect, error;
  try {
    const rule = `${host}:${c.kind === 'tcp' ? TCP : ORIGIN} ${c.inspect ? 'enable://inspect ' : ''}`
      + `frameScript://http://${host}:${SOURCE}/${c.missing ? '404' : 'frame'}.js`;
    await setRules(port, rule);
    if (c.kind === 'http') {
      // If a broken implementation attempts a source read for ordinary HTTP,
      // let it complete so the fetch-count assertion explains the failure.
      state.handshake.resolve(); state.firstWrite.resolve();
      const response = await request(port, `http://${host}:${ORIGIN}/plain`);
      effect = { status: response.status, body: response.body, originCount: state.httpCount };
      assert.deepEqual(effect, { status: 200, body: 'HTTP_BODY', originCount: 1 });
    } else {
      const received = await exchange(port, c, state);
      effect = { received, origin: c.kind === 'tcp' ? state.origin.join('') : state.origin, originCount: state.originCount };
      const expectedOrigin = !c.active ? ['a', 'b'] : c.kind === 'tcp' ? ['A', 'B'] : ['F1:a', 'F3:b'];
      const expectedReceived = !c.active ? ['echo:a', 'echo:b'] : c.kind === 'tcp' ? ['ECHO:A', 'ECHO:B'] : ['F2:echo:F1:a', 'F4:echo:F3:b'];
      assert.deepEqual(effect, { received: expectedReceived,
        origin: c.kind === 'tcp' ? expectedOrigin.join('') : expectedOrigin, originCount: 1 });
    }
    const fetch = c.kind === 'ws' || c.inspect;
    assert.deepEqual(state.fetches, fetch ? [c.missing ? '/404.js' : '/frame.js'] : [], 'source reads per connection');
    if (fetch) {
      const index = (type) => state.events.findIndex((e) => e.type === type);
      // Socket callbacks need not follow wire order. The source response gate
      // proves handshake completion without requiring cross-socket callback order.
      assert.ok(index('client-send') < index('source-release'), 'first data sent while source gated');
      assert.ok(index('source-finish') > index('source-release'), 'source response finishes after gate release');
      assert.ok(index('source-finish') < index('origin-data'), 'source completes before first origin data');
    }
  } catch (cause) { error = cause.message; }
  finally {
    state.handshake.resolve(); state.firstWrite.resolve();
    // Close only fixture connections, never either externally owned proxy.
    for (const socket of sockets) socket.destroy();
    for (const ws of wss.clients) ws.terminate();
  }
  return { effect, ...(error ? { error } : {}), fetches: state.fetches, events: state.events };
}
const listen = (server, port) => new Promise((resolve, reject) => {
  server.once('error', reject); server.listen(port, host, resolve);
});
async function main() {
  const raw = [], report = [], expectationFailures = [];
  try {
    await Promise.all([listen(origin, ORIGIN), listen(source, SOURCE), listen(tcp, TCP)]);
    for (const c of cases) {
      const answers = {};
      for (const [name, port] of [['whistle', W], ['rs', RS]]) {
        answers[name] = await ask(port, c);
        if (answers[name].error) expectationFailures.push({ case: c.name, proxy: name, ...answers[name] });
      }
      const whistle = JSON.stringify(answers.whistle.effect ?? { error: answers.whistle.error });
      const rs = JSON.stringify(answers.rs.effect ?? { error: answers.rs.error });
      const same = whistle === rs;
      raw.push({ case: c.name, whistle, rs, same,
        sourceFetches: { whistle: answers.whistle.fetches, rs: answers.rs.fetches },
        events: { whistle: answers.whistle.events, rs: answers.rs.events } });
      if (!same) report.push({ name: c.name, problems: [`answer: whistle=${whistle} rs=${rs}`] });
    }
  } finally {
    for (const socket of sockets) socket.destroy();
    for (const ws of wss.clients) ws.terminate();
    await Promise.all([origin, source, tcp].map((server) => new Promise((resolve) => server.close(resolve))));
  }
  const output = { ran: raw.length, expectationChecks: raw.length * 2, differing: report.length,
    report, expectationFailures, raw };
  if (jsonPath) fs.writeFileSync(jsonPath, JSON.stringify(output, null, 2));
  console.log(JSON.stringify(output, null, 2));
  process.exitCode = report.length || expectationFailures.length ? 1 : 0;
}
main().catch((error) => { console.error(error); process.exitCode = 2; });
