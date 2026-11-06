// Real CONNECT peers for pause/ignore/pass. Each side must satisfy the byte
// contract independently; agreement between two broken proxies is insufficient.
'use strict';
const assert = require('node:assert/strict');
const fs = require('node:fs');
const http = require('node:http');
const net = require('node:net');
const { judge } = require('./declared');
const WHISTLE = require('./whistle-pkg');
const base = Number(process.env.PORT_BASE || 18700);
const [W, RS, ORIGIN] = [base, base + 1, base + 2];
const target = `127.0.0.1:${ORIGIN}`;
const jsonAt = process.argv.indexOf('--json');
const jsonPath = jsonAt < 0 ? null : process.argv[jsonAt + 1];
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const sockets = new Set();
let accepted;
const origin = net.createServer({ allowHalfOpen: true }, (socket) => {
  sockets.add(socket);
  socket.on('close', () => sockets.delete(socket));
  socket.on('error', () => {});
  accepted(socket);
});

function api(port, path, body, contentType = 'application/json') {
  const data = body === undefined ? undefined : typeof body === 'string' ? body : JSON.stringify(body);
  return new Promise((resolve, reject) => {
    const request = http.request({ host: '127.0.0.1', port, path, method: data === undefined ? 'GET' : 'POST',
      headers: data === undefined ? {} : { 'Content-Type': contentType, 'Content-Length': Buffer.byteLength(data) } }, (res) => {
      const parts = [];
      res.on('data', (chunk) => parts.push(chunk));
      res.on('end', () => {
        try {
          assert.equal(res.statusCode, 200);
          const result = JSON.parse(Buffer.concat(parts).toString());
          resolve(result);
        } catch (error) { reject(error); }
      });
    });
    request.on('error', reject);
    request.setTimeout(3000, () => request.destroy(new Error('API timeout')));
    request.end(data);
  });
}
async function until(read, label) {
  const deadline = performance.now() + 3000;
  do {
    const value = await read();
    if (value) return value;
    await sleep(10);
  } while (performance.now() < deadline);
  throw new Error('timeout: ' + label);
}
async function sessions(port) {
  const result = port === W ? await api(port, '/cgi-bin/sessions', { latest: true }) : await api(port, '/sessions.json');
  return (Array.isArray(result) ? result : result.data).filter((s) => s.url.includes(target));
}
async function setRules(port, flags, script) {
  const rule = (script ? '``` tcp.js\n' + script + '\n```\n' : '')
    + `${target} enable://${flags}` + (script ? ' frameScript://{tcp.js}' : '');
  if (port === W) await api(port, '/cgi-bin/rules/add', 'name=Default&selected=1&value=' + encodeURIComponent(rule), 'application/x-www-form-urlencoded');
  else await api(port, '/api/rules', rule, 'text/plain');
}
async function mode(port, id, direction, value) {
  if (port === W) await api(port, '/cgi-bin/socket/change-status', {
    reqId: id, [direction === 'send' ? 'sendStatus' : 'receiveStatus']: { pass: 0, pause: 1, ignore: 2 }[value],
  });
  else await api(port, '/api/ws/control', { id, dir: direction, mode: value });
}
async function open(port) {
  const previous = new Set((await sessions(port)).map((s) => s.id));
  const server = new Promise((resolve) => { accepted = resolve; });
  const client = await new Promise((resolve, reject) => {
    const request = http.request({ host: '127.0.0.1', port, method: 'CONNECT', path: target,
      headers: { Host: target, 'x-whistle-policy': 'tunnel' } });
    request.setTimeout(3000, () => request.destroy(new Error('CONNECT timeout')));
    request.on('error', reject);
    request.on('connect', (response, socket, head) => {
      if (response.statusCode !== 200 || head.length) { socket.destroy(); reject(new Error('CONNECT failed')); return; }
      sockets.add(socket);
      socket.on('close', () => sockets.delete(socket));
      socket.on('error', () => {});
      resolve(socket);
    });
    request.end();
  });
  const peer = await server;
  const received = { send: [], receive: [] };
  peer.on('data', (bytes) => received.send.push(bytes));
  client.on('data', (bytes) => received.receive.push(bytes));
  const session = await until(async () => (await sessions(port)).find((s) => !previous.has(s.id)), 'new CONNECT session');
  // Whistle lists CONNECT before its asynchronous pipe setup installs controls.
  // change-status still returns ec:0 when that context is absent.
  if (port === W) await until(async () => {
    const result = await api(W, '/cgi-bin/get-data?curReqId=' + encodeURIComponent(session.id));
    return result.data?.socketStatus && typeof result.data.socketStatus === 'object';
  }, 'Whistle control context');
  else await until(async () => (await api(RS, '/api/ws/status?id=' + session.id)).live, 'whix control context');
  return { client, peer, id: session.id, data: (dir) => Buffer.concat(received[dir]).toString('hex') };
}

const payload = Buffer.from([0, 255, 128, 65]);
const next = Buffer.from('next');
const cases = [];
for (const dir of ['send', 'receive']) {
  for (const action of ['pause', 'ignore', 'discard-held']) {
    cases.push({ name: `${dir}: ${action}`, dir, action,
      flags: action === 'discard-held' ? 'inspect' : action + (dir === 'send' ? 'Send' : 'Receive') });
  }
}
// Both pinned Whistle versions discard the paused chunk when resuming. A
// post-release marker proves forward progress; whix must also deliver the hold.
cases.push({ name: 'paused script output', dir: 'send', action: 'pause', flags: 'pauseSend',
  script: "ctx.handleSendToServerFrame = function (buf) { return 'S:' + buf; };", data: Buffer.from('text') });

function expectedAfter(port, c) {
  const data = c.data || payload;
  const marker = c.script ? Buffer.concat([Buffer.from('S:'), next]) : next;
  const held = c.action === 'pause' && port === RS
    ? (c.script ? Buffer.concat([Buffer.from('S:'), data]) : data) : Buffer.alloc(0);
  return Buffer.concat([held, marker]).toString('hex');
}
async function ask(port, c) {
  await setRules(port, c.flags, c.script);
  const wire = await open(port);
  try {
    if (c.action === 'discard-held') await mode(port, wire.id, c.dir, 'pause');
    const sender = c.dir === 'send' ? wire.client : wire.peer;
    const data = c.data || payload;
    await new Promise((resolve, reject) => sender.write(data, (e) => e ? reject(e) : resolve()));
    await sleep(120);
    const before = wire.data(c.dir);
    if (c.action === 'discard-held') await mode(port, wire.id, c.dir, 'ignore');
    await mode(port, wire.id, c.dir, 'pass');
    sender.write(next);
    const marker = (c.script ? Buffer.concat([Buffer.from('S:'), next]) : next).toString('hex');
    // Wait for an observed boundary, not the expected byte count: a dropped
    // held chunk should be reported as missing bytes, not as a fake timeout.
    await until(() => wire.data(c.dir).endsWith(marker), 'post-resume marker');
    return { before, after: wire.data(c.dir) };
  } finally {
    wire.client.destroy(); wire.peer.destroy();
  }
}

async function main() {
  await new Promise((resolve, reject) => { origin.once('error', reject); origin.listen(ORIGIN, '127.0.0.1', resolve); });
  const raw = [], report = [], expectationFailures = [];
  try {
    for (const c of cases) {
      const answers = {};
      for (const [name, port] of [['whistle', W], ['rs', RS]]) {
        try {
          const observed = await ask(port, c);
          answers[name] = JSON.stringify(observed);
          const expected = { before: '', after: expectedAfter(port, c) };
          try { assert.deepEqual(observed, expected); }
          catch { expectationFailures.push({ case: c.name, proxy: name, expected, observed }); }
        } catch (error) {
          answers[name] = JSON.stringify({ error: error.message });
          expectationFailures.push({ case: c.name, proxy: name, error: error.message });
        }
      }
      const same = answers.whistle === answers.rs;
      raw.push({ case: c.name, ...answers, same });
      if (!same) report.push({ name: c.name, problems: [`answer: whistle=${answers.whistle} rs=${answers.rs}`] });
    }
  } finally {
    for (const socket of sockets) socket.destroy();
    await new Promise((resolve) => origin.close(resolve));
  }
  const verdict = judge('tcp-controls-bench.js', report, raw.map((r) => r.case));
  const output = { ran: raw.length, expectationChecks: raw.length * 2, differing: verdict.news.length, declared: verdict.declared, stale: verdict.stale,
    report: verdict.news, raw, expectationFailures };
  if (jsonPath) fs.writeFileSync(jsonPath, JSON.stringify(output, null, 2));
  console.log(JSON.stringify(output, null, 2));
  process.exitCode = verdict.news.length || verdict.stale.length || expectationFailures.length ? 1 : 0;
}
main().catch((error) => { console.error(error); for (const socket of sockets) socket.destroy(); origin.close(); process.exitCode = 2; });
