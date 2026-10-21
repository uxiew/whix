#!/usr/bin/env node
// Bounded, synthetic loopback traffic only. Not a throughput SLA or an oracle:
// validate frame identity/order, live controls, and isolation from a slow peer.
'use strict';
const assert = require('node:assert/strict');
const crypto = require('node:crypto');
const fs = require('node:fs');
const http = require('node:http');
const os = require('node:os');
const path = require('node:path');
const { spawn, spawnSync } = require('node:child_process');
const WebSocket = require('ws');

const REPO = path.resolve(__dirname, '../..');
const args = process.argv.slice(2);
const options = {};
const allowed = new Set(['seconds', 'connections', 'frames', 'bytes', 'out', 'binary']);
for (let i = 0; i < args.length; i += 2) {
  const key = args[i].replace(/^--/, '');
  if (!args[i].startsWith('--') || !allowed.has(key) || !args[i + 1] || args[i + 1].startsWith('--')) {
    throw new Error('usage: traffic-stress.js [--seconds 300] [--connections 32] [--frames 32] [--bytes 1024] [--out FILE] [--binary FILE]');
  }
  options[key] = args[i + 1];
}
function integer(key, fallback, min, max) {
  const n = options[key] === undefined ? fallback : Number(options[key]);
  if (!Number.isInteger(n) || n < min || n > max) throw new Error(key + ' must be ' + min + '..' + max);
  return n;
}
const config = {
  seconds: integer('seconds', 300, 1, 3600),
  connections: integer('connections', 32, 1, 64),
  frames: integer('frames', 32, 1, 128),
  bytes: integer('bytes', 1024, 16, 16384),
};
const binary = path.resolve(options.binary || process.env.RS_BIN || path.join(REPO, 'target/debug/whix'));
assert(fs.existsSync(binary) && fs.statSync(binary).isFile(), 'build whix first: cargo build --locked');
fs.accessSync(binary, fs.constants.R_OK | fs.constants.X_OK);
const output = path.resolve(options.out || path.join(REPO, 'target/traffic-stress', new Date().toISOString().replace(/[:.]/g, '-') + '.json'));
assert(!fs.existsSync(output), 'refusing to overwrite an earlier result');
fs.mkdirSync(path.dirname(output), { recursive: true });
const scratch = fs.mkdtempSync(path.join(os.tmpdir(), 'whix-traffic-stress-'));
const run = crypto.randomBytes(8).toString('hex');
const states = new Map(), clients = new Set(), originSockets = new Set();
const agent = new http.Agent({ keepAlive: true, maxSockets: 16 });
const healthAgent = new http.Agent({ keepAlive: true, maxSockets: 2 });
const stats = { rssSamples: 0, firstProxyRssMiB: null, lastProxyRssMiB: null, waves: 0, sessions: 0, originFrames: 0, clientFrames: 0, intentionallyDropped: 0, maxApiMs: 0, maxSampledProxyRssMiB: null };
let proxy, proxyExit, port, origin, wss, nextId = 1, failure, finishing = false, lastSample = 0, proxyStopped = false;
let interrupt;
const interrupted = new Promise((_, reject) => { interrupt = reject; });
interrupted.catch(() => {});
for (const signal of ['SIGINT', 'SIGTERM']) process.on(signal, () => interrupt(new Error('Interrupted by ' + signal)));
const pause = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
function check() { if (failure) throw failure; if (finishing) throw new Error('workload stopped'); }
function recordFailure(error) { if (!failure && !finishing) failure = error; }
async function deadline(promise, ms, label) {
  let timer;
  try { return await Promise.race([promise, interrupted, new Promise((_, reject) => { timer = setTimeout(() => reject(new Error(label + ' timed out')), ms); })]); }
  finally { clearTimeout(timer); }
}
async function until(predicate, label, ms = 4000) {
  const end = Date.now() + ms;
  while (Date.now() < end) { check(); const value = await predicate(); if (value) return value; await pause(10); }
  throw new Error(label + ' timed out');
}
function api(route, data, { measure = true, health = false, timeout = 4000, allowError = false } = {}) {
  check();
  return new Promise((resolve, reject) => {
    const began = Date.now(), raw = typeof data === 'string';
    const body = data === undefined ? null : raw ? data : JSON.stringify(data);
    const req = http.request({ host: '127.0.0.1', port, path: route, method: body === null ? 'GET' : 'POST',
      agent: health ? healthAgent : agent,
      headers: body === null ? {} : { 'content-type': raw ? 'text/plain' : 'application/json', 'content-length': Buffer.byteLength(body) } }, (res) => {
      let text = '';
      res.on('data', (chunk) => text += chunk);
      res.on('error', (error) => { clearTimeout(timer); reject(error); });
      res.on('end', () => {
        clearTimeout(timer);
        if (measure) stats.maxApiMs = Math.max(stats.maxApiMs, Date.now() - began);
        try {
          const result = JSON.parse(text);
          if (!allowError) assert(res.statusCode === 200 && result.ok !== false, route + ': ' + text.slice(0, 200));
          resolve(result);
        } catch (error) { reject(error); }
      });
    });
    const timer = setTimeout(() => req.destroy(new Error(route + ' timed out')), timeout);
    req.on('error', (error) => { clearTimeout(timer); reject(error); });
    req.end(body);
  });
}
function payload(id, role, seq) {
  const data = Buffer.allocUnsafe(config.bytes);
  data.writeUInt32BE(id, 0); data[4] = role; data.writeUInt32BE(seq, 5);
  for (let i = 9; i < data.length; i++) data[i] = (id + role * 31 + seq * 7 + i * 13) & 255;
  return data;
}
function receive(s, side, data, flags) {
  if (finishing || s.slow) return;
  try {
    assert(flags && flags.binary && Buffer.isBuffer(data), 'binary opcode lost');
    assert.equal(data.length, config.bytes, 'frame boundary/length changed');
    const id = data.readUInt32BE(0), role = data[4], seq = data.readUInt32BE(5);
    assert.equal(id, s.id, 'frame crossed sessions');
    assert(role <= (side === 'origin' ? 1 : 2), 'frame reached the wrong endpoint');
    assert.equal(seq, s[side + 'Next'][role], 'frame reordered, lost or duplicated');
    assert(data.equals(payload(id, role, seq)), 'payload bytes changed');
    s[side + 'Next'][role]++;
    stats[side + 'Frames']++;
  } catch (error) { recordFailure(error); }
}
function sendPeer(s, role, seq) {
  check();
  return deadline(new Promise((resolve, reject) => s.clientSocket.send(payload(s.id, role, seq), { binary: true }, (error) => error ? reject(error) : resolve())), 4000, 'peer send');
}
function inject(s, dir, role, seq) {
  return api('/api/ws/send', { id: s.session, dir, type: 'binary', encoding: 'base64', data: payload(s.id, role, seq).toString('base64') });
}
async function connect(slow = false) {
  const s = { id: nextId++, slow, originNext: [0, 0, 0], clientNext: [0, 0, 0], originClosed: null, clientClosed: null };
  s.tag = run + '-' + s.id;
  states.set(s.tag, s);
  s.firstBytes = new Promise((resolve) => { s.sawBytes = resolve; });
  const ws = new WebSocket('ws://127.0.0.1:' + port + '/stream?tag=' + s.tag, { host: 'stress.test', perMessageDeflate: false });
  s.clientSocket = ws; clients.add(ws);
  ws.on('message', (data, flags) => receive(s, 'client', data, flags));
  ws.on('error', (error) => { if (!s.slow) recordFailure(error); });
  ws.on('close', (code) => {
    s.clientClosed = code; clients.delete(ws);
    if (!finishing && !s.slow && code !== 1000) recordFailure(new Error('client closed abnormally: ' + code));
  });
  await deadline(new Promise((resolve, reject) => { ws.once('open', resolve); ws.once('error', reject); }), 4000, 'upgrade');
  return s;
}
function status(s) { return api('/api/ws/status?id=' + s.session); }
async function register(list) {
  await until(async () => {
    const rows = await api('/sessions.json');
    const byTag = new Map(rows.filter((r) => r.status === 101).map((r) => [new URL(r.url).searchParams.get('tag'), r]));
    if (!list.every((s) => byTag.has(s.tag))) return false;
    for (const s of list) s.session = byTag.get(s.tag).id;
    return true;
  }, 'sessions captured');
  await until(async () => (await Promise.all(list.map(status))).every((s) => s.live), 'live registrations');
}
async function controls(s) {
  let seq = config.frames;
  await api('/api/ws/control', { id: s.session, dir: 'send', mode: 'pause' });
  await sendPeer(s, 0, seq);
  await until(async () => (await status(s)).send.held === 1, 'send held');
  assert.equal(s.originNext[0], seq);
  await inject(s, 'send', 1, seq);
  await until(() => s.clientNext[1] === seq + 1, 'injection bypassed pause');
  s.originNext[0]++; s.clientNext[0]++; stats.intentionallyDropped++;
  await api('/api/ws/control', { id: s.session, dir: 'send', mode: 'ignore' });
  await api('/api/ws/control', { id: s.session, dir: 'send', mode: 'pass' });
  await sendPeer(s, 0, ++seq);
  await until(() => s.clientNext[0] === seq + 1, 'ignore then pass');
  await api('/api/ws/control', { id: s.session, dir: 'receive', mode: 'pause' });
  await sendPeer(s, 0, ++seq);
  await until(async () => (await status(s)).receive.held === 1, 'receive held');
  await inject(s, 'receive', 2, config.frames);
  await until(() => s.clientNext[2] === config.frames + 1, 'injection bypassed receive pause');
  await api('/api/ws/release', { id: s.session, dir: 'receive' });
  await until(() => s.clientNext[0] === seq + 1, 'receive released');
}
async function wave() {
  const list = await Promise.all(Array.from({ length: config.connections }, () => connect()));
  await register(list);
  await deadline(Promise.all(list.flatMap((s) => [
    (async () => { for (let i = 0; i < config.frames; i++) await sendPeer(s, 0, i); })(),
    (async () => { for (let i = 0; i < config.frames; i++) await inject(s, 'send', 1, i); })(),
    (async () => { for (let i = 0; i < config.frames; i++) await inject(s, 'receive', 2, i); })(),
  ])), 15000, 'concurrent traffic');
  await until(() => list.every((s) => s.originNext.slice(0, 2).every((n) => n === config.frames) && s.clientNext.every((n) => n === config.frames)), 'all payloads arrived');
  await controls(list[0]);
  for (const [i, s] of list.entries()) (i % 2 ? s.originSocket : s.clientSocket).close(1000);
  await until(() => list.every((s) => s.originClosed === 1000 && s.clientClosed === 1000), 'bilateral normal close');
  await until(async () => (await Promise.all(list.map(status))).every((s) => !s.live), 'closed registrations removed');
  for (const s of list) states.delete(s.tag);
  stats.sessions += list.length; stats.waves++;
}
async function backpressure() {
  const s = await connect(true); await register([s]);
  const data = Buffer.alloc(8 * 1024 * 1024, 90).toString('base64');
  let pending, attempts = 0, settled;
  // Kernels have different socket buffer sizes; stay bounded rather than
  // assuming one write always fills the queue.
  for (; attempts < 4; attempts++) {
    settled = false;
    pending = api('/api/ws/send', { id: s.session, dir: 'send', type: 'binary', encoding: 'base64', data }, { measure: false, timeout: 7000, allowError: true });
    pending.then(() => { settled = true; }, recordFailure);
    await deadline(s.firstBytes, 3000, 'slow peer received first bytes');
    await pause(150);
    if (!settled) break;
    assert.equal((await pending).ok, true);
  }
  assert(!settled, 'fixture could not establish backpressure within 32 MiB');
  const healthyStart = Date.now();
  await api('/api/status', undefined, { health: true });
  const healthMs = Date.now() - healthyStart;
  assert(healthMs < 2000, 'slow session blocked control requests');
  const began = Date.now();
  await api('/api/ws/abort', { id: s.session }, { health: true });
  const result = await deadline(pending, 1500, 'abort cancels blocked injection');
  assert.equal(result.ok, false, 'blocked injection was not rejected');
  await until(async () => !(await status(s)).live, 'aborted registration removed', 1500);
  s.originSocket._socket.resume();
  await until(() => s.clientClosed !== null && s.originClosed !== null, 'both peers disconnected');
  states.delete(s.tag);
  stats.backpressure = { attempts: attempts + 1, healthMs, abortMs: Date.now() - began, injectionRejected: true };
}
async function sampleRss() {
  if (process.platform === 'win32' || Date.now() - lastSample < 5000) return;
  lastSample = Date.now();
  const result = spawnSync('ps', ['-o', 'rss=', '-p', String(proxy.pid)], { encoding: 'utf8', timeout: 1000 });
  const kib = Number(result.stdout?.trim());
  if (result.status === 0 && kib > 0) {
    const mib = kib / 1024;
    stats.rssSamples++;
    stats.firstProxyRssMiB ??= mib;
    stats.lastProxyRssMiB = mib;
    stats.maxSampledProxyRssMiB = Math.max(stats.maxSampledProxyRssMiB || 0, mib);
    assert(mib < 512, 'proxy exceeded the 512 MiB workload safety guard');
  }
}
async function runWorkload() {
  origin = http.createServer((_, res) => res.end('stress fixture'));
  wss = new WebSocket.Server({ server: origin, perMessageDeflate: false });
  wss.on('connection', (ws) => {
    const tag = new URL(ws.upgradeReq.url, 'http://stress.test').searchParams.get('tag');
    const s = states.get(tag);
    if (!s) { ws.terminate(); recordFailure(new Error('unknown connection tag')); return; }
    s.originSocket = ws; originSockets.add(ws);
    if (s.slow) ws._socket.prependOnceListener('data', (chunk) => { ws._socket.pause(); s.sawBytes(chunk.length); });
    ws.on('message', (data, flags) => {
      receive(s, 'origin', data, flags);
      if (!s.slow && !finishing) ws.send(data, { binary: true }, (error) => { if (error) recordFailure(error); });
    });
    ws.on('error', (error) => { if (!s.slow) recordFailure(error); });
    ws.on('close', (code) => {
      s.originClosed = code; originSockets.delete(ws);
      if (!finishing && !s.slow && code !== 1000) recordFailure(new Error('origin closed abnormally: ' + code));
    });
  });
  await new Promise((resolve, reject) => { origin.once('error', reject); origin.listen(0, '127.0.0.1', resolve); });
  const env = { ...process.env }; delete env.WHIX_PASSWORD; delete env.WHIX_GUEST_PASSWORD;
  proxy = spawn(binary, ['-p', '0', '--dir', scratch, '--no-persist'], { cwd: scratch, env, stdio: ['ignore', 'pipe', 'pipe'] });
  proxyExit = new Promise((resolve) => proxy.once('close', () => { proxyStopped = true; resolve(); }));
  let log = '';
  proxy.stderr.on('data', (b) => { log = (log + b).slice(-4000); });
  proxy.on('error', recordFailure);
  port = await deadline(new Promise((resolve, reject) => {
    proxy.stdout.on('data', (b) => { log = (log + b).slice(-4000); const m = log.match(/whix listening on http:\/\/127\.0\.0\.1:(\d+)/); if (m) resolve(Number(m[1])); });
    proxy.once('error', reject);
    proxy.once('exit', () => reject(new Error('proxy exited before startup: ' + log)));
  }), 10000, 'proxy startup');
  await api('/api/rules', 'stress.test host://127.0.0.1:' + origin.address().port);
  const began = Date.now();
  await deadline(Promise.all([wave(), backpressure()]), 20000, 'slow peer alongside healthy traffic');
  do {
    check(); await sampleRss(); await wave();
    if (stats.waves % 25 === 0) console.error('waves=' + stats.waves + ' sessions=' + stats.sessions + ' frames=' + (stats.originFrames + stats.clientFrames));
  } while (Date.now() - began < config.seconds * 1000);
  stats.elapsedMs = Date.now() - began;
  await sampleRss();
}
async function sha256(file) {
  const hash = crypto.createHash('sha256');
  for await (const chunk of fs.createReadStream(file)) hash.update(chunk);
  return hash.digest('hex');
}
(async () => {
  const report = { started: new Date().toISOString(), node: process.version, platform: process.platform + '/' + process.arch, config, passed: false, stats };
  try { report.binarySha256 = await sha256(binary); report.scriptSha256 = await sha256(__filename); await deadline(runWorkload(), (config.seconds + 60) * 1000, 'whole workload'); check(); report.passed = true; }
  catch (error) { report.error = String(error.stack || error); process.exitCode = 1; }
  finally {
    finishing = true;
    agent.destroy(); healthAgent.destroy();
    for (const ws of clients) ws.terminate();
    for (const ws of originSockets) ws.terminate();
    if (origin) await deadline(new Promise((resolve) => origin.close(resolve)), 3000, 'fixture cleanup').catch(() => {});
    if (proxy && !proxyStopped) {
      proxy.kill();
      await Promise.race([proxyExit, pause(2000)]);
      if (!proxyStopped) { proxy.kill('SIGKILL'); await proxyExit; }
    }
    fs.rmSync(scratch, { recursive: true, force: true });
    report.finished = new Date().toISOString();
    fs.writeFileSync(output, JSON.stringify(report, null, 2) + '\n', { flag: 'wx' });
    console.log(JSON.stringify(report, null, 2));
  }
})();
