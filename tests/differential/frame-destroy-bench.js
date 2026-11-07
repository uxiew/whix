// Whistle 2.10.13 has two measured destroy gaps: top-level execution precedes
// connection registration, and CONNECT omits reqId. whix deliberately closes
// both peers in those cases too. Agreement alone is never a passing oracle.
'use strict';
const assert = require('node:assert/strict');
const fs = require('node:fs');
const http = require('node:http');
const net = require('node:net');
const WebSocket = require('ws');
const crypto = require('node:crypto');
const WHISTLE = require('./whistle-pkg');
const base = Number(process.env.PORT_BASE || 18700);
const [W, RS, WS_ORIGIN, TCP_ORIGIN] = [base, base + 1, base + 2, base + 3];
const host = '127.0.0.1';
const jsonAt = process.argv.indexOf('--json');
const jsonPath = jsonAt < 0 ? null : process.argv[jsonAt + 1];
if (WHISTLE.version !== '2.10.13') throw new Error(`frame-destroy-bench requires Whistle 2.10.13; got ${WHISTLE.version}`);
if (jsonAt >= 0 && !jsonPath) throw new Error('--json requires an output path');
const sockets = new Set();
function track(socket) {
  sockets.add(socket); socket.on('close', () => sockets.delete(socket)); socket.on('error', () => {});
  return socket;
}
let current;
const record = (s, type, data) => s.events.push({ type, ...(data === undefined ? {} : { data }) });
const origin = http.createServer();
origin.on('connection', track);
const wss = new WebSocket.Server({ server: origin });
wss.on('connection', (ws) => {
  const s = current; s.originCount++;
  ws.on('error', () => {});
  ws.on('message', (bytes) => { record(s, 'origin-data', String(bytes)); echo(String(bytes), (text) => ws.send(text)); });
  ws.on('close', (code) => { record(s, 'origin-close', code); s.check(); });
});
function echo(data, send) {
  // Exception replacements have no newline. Recognize our suffix markers
  // even when TCP coalesces that text with the subsequent LATE write.
  for (const match of data.matchAll(/(?:TRIGGER|LATE|RETURN)\n/g)) send(match[0]);
}
const tcp = net.createServer((socket) => {
  track(socket); const s = current; s.originCount++;
  let pending = '';
  socket.on('data', (bytes) => {
    record(s, 'origin-data', String(bytes)); pending += bytes;
    const end = pending.lastIndexOf('\n');
    if (end >= 0) { echo(pending.slice(0, end + 1), (text) => socket.write(text)); pending = pending.slice(end + 1); }
  });
  socket.on('end', () => record(s, 'origin-eof'));
  socket.on('close', () => { record(s, 'origin-close'); s.check(); });
});
const before = "c.sendToServer('BEFORE_S\\n');c.sendToClient('BEFORE_C\\n');";
const after = "c.sendToServer('AFTER_S\\n');c.sendToClient('AFTER_C\\n');";
const cases = [{ name: 'control', phase: 'control', script: 'var c=ctx;' }];
for (const phase of ['top', 'send', 'receive']) for (const action of ['return', 'repeat', 'throw']) {
  const body = before + 'c.destroy();' + (action === 'repeat' ? 'c.destroy();' : '') + after
    + (action === 'throw' ? "throw new Error('DESTROY_THROW');" : phase === 'top' ? '' : "return 'RETURN\\n';");
  const script = phase === 'top' ? 'var c=ctx;' + body : 'var c=ctx;c.'
    + (phase === 'send' ? 'handleSendToServerFrame' : 'handleSendToClientFrame')
    + "=function(b,o){if(o.frameScript||String(b)!=='TRIGGER\\n')return b;" + body + '};';
  cases.push({ name: `${phase} ${action}`, phase, action, script });
}
function expected(kind, c, who) {
  const closes = c.phase !== 'control' && (who === 'rs' || (kind === 'ws' && c.phase !== 'top'));
  let origin, client;
  if (c.phase === 'control') origin = client = 'TRIGGER\nLATE\n';
  else if (closes && c.phase === 'top') origin = client = '';
  else if (closes) { origin = (c.phase === 'receive' ? 'TRIGGER\n' : '') + 'BEFORE_S\n'; client = 'BEFORE_C\n'; }
  else if (c.phase === 'top') { origin = 'BEFORE_S\nAFTER_S\nTRIGGER\nLATE\n'; client = (kind === 'ws' ? 'BEFORE_C\nAFTER_C\n' : '') + 'TRIGGER\nLATE\n'; }
  else {
    const send = c.phase === 'send', throws = c.action === 'throw';
    origin = (send ? '' : 'TRIGGER\n') + 'BEFORE_S\nAFTER_S\n'
      + (send ? throws ? 'DESTROY_THROW (handleSendToServerFrame)' : 'RETURN\n' : '') + 'LATE\n';
    client = 'BEFORE_C\nAFTER_C\n' + (throws ? send ? '' : 'DESTROY_THROW (handleSendToClientFrame)' : 'RETURN\n') + 'LATE\n';
  }
  return { origin, client, originCount: 1, clientEnded: closes, originClosed: closes,
    closeFrames: [], ...(kind === 'ws' ? { originCloseCodes: closes ? [1006] : [] } : {}), lateRoundTrip: !closes };
}
async function setRules(port, text) {
  const body = port === W ? 'name=Default&selected=1&value=' + encodeURIComponent(text) : text;
  await new Promise((resolve, reject) => {
    const req = http.request({ host, port, path: port === W ? '/cgi-bin/rules/add' : '/api/rules', method: 'POST',
      headers: { 'content-type': port === W ? 'application/x-www-form-urlencoded' : 'text/plain', 'content-length': Buffer.byteLength(body) } }, (res) => {
      let data = ''; res.on('data', (b) => { data += b; }); res.on('error', reject);
      res.on('end', () => { try { assert.equal(res.statusCode, 200); if (port === W) assert.equal(JSON.parse(data).ec, 0); resolve(); } catch (e) { reject(e); } });
    });
    req.on('error', reject); req.setTimeout(4000, () => req.destroy(new Error('rules API timeout'))); req.end(body);
  });
}
function frame(text) {
  const bytes = Buffer.from(text), mask = crypto.randomBytes(4);
  return Buffer.concat([Buffer.from([0x81, 0x80 | bytes.length]), mask, Buffer.from(bytes.map((b, i) => b ^ mask[i % 4]))]);
}
function exchange(port, kind, s) {
  return new Promise((resolve, reject) => {
    const socket = track(net.connect(port, host));
    let buffer = Buffer.alloc(0), upgraded = false, settled = false, lateTimer;
    const timer = setTimeout(() => finish(new Error('no bilateral close or subsequent LATE roundtrip within 3s')), 3000);
    function finish(error) { if (settled) return; settled = true; clearTimeout(timer); clearTimeout(lateTimer); error ? reject(error) : resolve(); }
    const has = (type) => s.events.some((e) => e.type === type);
    s.check = () => {
      if (((has('client-eof') || has('client-reset')) && has('origin-close')) || s.events.filter((e) => e.type === 'client-data').map((e) => e.data).join('').endsWith('LATE\n')) finish();
    };
    function send(text) { if (!socket.destroyed) { record(s, 'client-send', text); socket.write(kind === 'ws' ? frame(text) : text); } }
    socket.on('connect', () => socket.write(kind === 'ws'
      ? `GET ws://${host}:${WS_ORIGIN}/ HTTP/1.1\r\nHost: ${host}:${WS_ORIGIN}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: ${crypto.randomBytes(16).toString('base64')}\r\nSec-WebSocket-Version: 13\r\n\r\n`
      : `CONNECT ${host}:${TCP_ORIGIN} HTTP/1.1\r\nHost: ${host}:${TCP_ORIGIN}\r\n\r\n`));
    socket.on('end', () => { record(s, 'client-eof'); s.check(); });
    socket.on('error', (e) => {
      record(s, 'client-error', e.code);
      // Aborting with unread client bytes may produce RST instead of FIN.
      // Still require a completed handshake and independently closed origin.
      if (upgraded && e.code === 'ECONNRESET') { record(s, 'client-reset'); s.check(); }
      else finish(e);
    });
    socket.on('data', (bytes) => {
      try {
        buffer = Buffer.concat([buffer, bytes]);
        if (!upgraded) {
          const end = buffer.indexOf('\r\n\r\n'); if (end < 0) return;
          const status = buffer.subarray(0, end).toString().split('\r\n')[0];
          assert.match(status, kind === 'ws' ? /^HTTP\/1\.[01] 101 / : /^HTTP\/1\.[01] 200 /);
          record(s, 'handshake', status); buffer = buffer.subarray(end + 4); upgraded = true;
          send('TRIGGER\n');
          // Successful later traffic proves a no-op; merely not seeing EOF
          // during a short timeout would not establish that the tunnel works.
          lateTimer = setTimeout(() => send('LATE\n'), 100);
        }
        if (kind === 'tcp') { if (buffer.length) record(s, 'client-data', buffer.toString()); buffer = Buffer.alloc(0); }
        else while (buffer.length >= 2) {
          const opcode = buffer[0] & 15, size = buffer[1] & 127;
          assert.ok(size < 126 && !(buffer[1] & 128), 'short unmasked server frame');
          if (buffer.length < size + 2) break;
          const body = buffer.subarray(2, size + 2); buffer = buffer.subarray(size + 2);
          if (opcode === 8) record(s, 'close-frame', size >= 2 ? body.readUInt16BE(0) : 1005);
          else if (opcode === 1 || opcode === 2) record(s, 'client-data', body.toString());
        }
        s.check();
      } catch (e) { finish(e); }
    });
  });
}
async function ask(port, kind, c) {
  const s = { events: [], originCount: 0, check() {} }; current = s;
  let error;
  try {
    // Do not mix in upstream's known pre-200 TCP injection bug. Keep the
    // handshake parser strict; top-level TCP tests inject only toward origin.
    const script = kind === 'tcp' && c.phase === 'top'
      ? c.script.replace("c.sendToClient('BEFORE_C\\n');", '').replace("c.sendToClient('AFTER_C\\n');", '')
      : c.script;
    await setRules(port, '``` f.js\n' + script + '\n```\n' + host + ':' + (kind === 'ws' ? WS_ORIGIN : TCP_ORIGIN)
      + (kind === 'tcp' ? ' enable://inspect' : '') + ' frameScript://{f.js}');
    await exchange(port, kind, s);
  } catch (e) { error = e.message; }
  const events = JSON.parse(JSON.stringify(s.events)); // Exclude teardown-induced closes.
  s.check = () => {};
  for (const socket of sockets) socket.destroy(); for (const ws of wss.clients) ws.terminate();
  const data = (type) => events.filter((e) => e.type === type).map((e) => e.data);
  const client = data('client-data').join('');
  return { effect: { origin: data('origin-data').join(''), client, originCount: s.originCount,
    clientEnded: events.some((e) => e.type === 'client-eof' || e.type === 'client-reset'), originClosed: events.some((e) => e.type === 'origin-close'),
    closeFrames: data('close-frame'), ...(kind === 'ws' ? { originCloseCodes: data('origin-close') } : {}), lateRoundTrip: client.endsWith('LATE\n') }, events, ...(error ? { error } : {}) };
}
const listen = (server, port) => new Promise((resolve, reject) => { server.once('error', reject); server.listen(port, host, resolve); });
async function main() {
  const raw = [], report = [], expectationFailures = [];
  try {
    await Promise.all([listen(origin, WS_ORIGIN), listen(tcp, TCP_ORIGIN)]);
    for (const kind of ['ws', 'tcp']) for (const c of cases) {
      const name = `${kind}: ${c.name}`, answers = {};
      for (const [who, port] of [['whistle', W], ['rs', RS]]) {
        const result = answers[who] = await ask(port, kind, c);
        try { if (result.error) throw new Error(result.error); assert.deepEqual(result.effect, expected(kind, c, who)); }
        catch (e) { expectationFailures.push({ case: name, proxy: who, error: e.message, expected: expected(kind, c, who), ...result }); }
      }
      const whistle = JSON.stringify(answers.whistle.effect), rs = JSON.stringify(answers.rs.effect), same = whistle === rs;
      const gap = c.phase !== 'control' && (kind === 'tcp' || c.phase === 'top');
      raw.push({ case: name, whistle, rs, same, events: { whistle: answers.whistle.events, rs: answers.rs.events } });
      if (gap || !same) report.push({ name, expectedUpstreamGap: gap,
        reason: gap ? kind === 'tcp' ? 'Whistle 2.10.13 CONNECT omits reqId, so destroy is a no-op' : 'Whistle 2.10.13 top-level destroy runs before connection registration' : 'unexpected effect difference',
        problems: [`answer: whistle=${whistle} rs=${rs}`] });
    }
  } finally {
    for (const socket of sockets) socket.destroy(); for (const ws of wss.clients) ws.terminate();
    await Promise.all([origin, tcp].map((server) => new Promise((resolve) => server.close(resolve))));
  }
  const output = { whistleVersion: WHISTLE.version, ran: raw.length, expectationChecks: raw.length * 2,
    differing: report.filter((r) => !r.expectedUpstreamGap).length, expectedUpstreamGaps: report.filter((r) => r.expectedUpstreamGap).length,
    report, raw, expectationFailures };
  if (jsonPath) fs.writeFileSync(jsonPath, JSON.stringify(output, null, 2));
  console.log(JSON.stringify(output, null, 2));
  process.exitCode = output.differing || expectationFailures.length ? 1 : 0;
}
main().catch((e) => { console.error(e); process.exitCode = 2; });
