'use strict';
const test = require('node:test');
const assert = require('node:assert/strict');
const { assessExpiry, validatePolicy, startClock, atHeaders } = require('./clock-expiry');
const epoch = Date.UTC(2026, 9, 23, 12);
const policy = { offsetMs: -60000000, gapMs: 2300 };
const expiryProblem = 'res.header.expires: whistle="earlier" rs="later"';
function answer(start = epoch + 100, end = start + 50, offset = policy.offsetMs) {
  return { headers: { expires: new Date(start + offset).toUTCString() }, expiresHeaderCount: 1,
    clock: { startMs: start, headersMs: end, elapsedMs: end - start } };
}
function check(a, b, problems = [expiryProblem]) { return assessExpiry(policy, a, b, problems); }

test('sequential requests several seconds apart retain their own expiry policy', () => {
  const a = answer(), b = answer(epoch + 2650);
  assert.notEqual(a.headers.expires, b.headers.expires);
  const result = check(a, b);
  assert.deepEqual(result.problems, []);
  assert.equal(result.evidence.whistle.ok, true);
  assert.equal(result.evidence.rs.ok, true);
  assert.deepEqual([a.clock.startMs, b.clock.startMs], [epoch + 100, epoch + 2650]);
});

test('HTTP-date rounding is bounded by request start and receipt of headers', () => {
  const a = answer(epoch + 990, epoch + 1010);
  a.headers.expires = new Date(epoch + 1000 + policy.offsetMs).toUTCString();
  assert.deepEqual(check(a, answer()).problems, []);
  a.headers.expires = new Date(epoch + 2000 + policy.offsetMs).toUTCString();
  assert(check(a, answer()).problems.some((p) => p.startsWith('res.clock.expires:')));
});

test('a changed TTL is not accepted just because dates are close', () => {
  for (const delta of [-1000, 1000, 86400000]) {
    const result = check(answer(), answer(epoch + 100, epoch + 150, policy.offsetMs + delta));
    assert(result.problems.includes(expiryProblem));
    assert.equal(result.evidence.rs.ok, false);
  }
});

test('both proxies returning the same wrong date still fails', () => {
  const a = answer(epoch + 100, epoch + 150, policy.offsetMs + 60000);
  assert(check(a, a, []).problems.some((p) => p.startsWith('res.clock.expires:')));
});

test('missing, duplicate, malformed and impossible dates fail closed', () => {
  for (const expires of [undefined, '', ['a', 'b'], 'tomorrow', 'Fri, 30 Feb 2026 12:00:00 GMT',
    'Thu, 22 Oct 2026 19:20:00 GMT; injected', 'Sat, 22 Oct 2026 19:20:00 GMT']) {
    const b = answer(); b.headers.expires = expires;
    assert.equal(check(answer(), b).evidence.rs.ok, false, String(expires));
  }
});

test('uncertain timing cannot broaden the accepted expiry window', () => {
  for (const clock of [undefined, {}, { startMs: epoch, headersMs: epoch + 2000, elapsedMs: 2000 },
    { startMs: epoch, headersMs: epoch - 1, elapsedMs: 1 },
    { startMs: epoch, headersMs: epoch + 200, elapsedMs: 10 },
    { startMs: epoch, headersMs: epoch + 1, elapsedMs: NaN }]) {
    const b = answer(); b.clock = clock;
    assert.equal(check(answer(), b).evidence.rs.ok, false);
  }
});

test('normalization preserves all non-Expires differences and does not mutate input', () => {
  const problems = [expiryProblem, 'res.header.expires-extra: changed', 'res.header.set-cookie: changed',
    'res.header.cache-control: changed', 'res.body: changed', 'req.header.expires: changed'];
  const before = [...problems];
  assert.deepEqual(check(answer(), answer(epoch + 2600), problems).problems, problems.slice(1));
  assert.deepEqual(problems, before);
});

test('unannotated and fixed-date cases are untouched', () => {
  const problems = [expiryProblem];
  const result = assessExpiry(undefined, answer(), answer(epoch + 2600), problems);
  assert.equal(result.problems, problems);
  assert.equal(result.evidence, null);
});

test('fixture configuration is validated before starting services', () => {
  for (const value of [null, [], {}, { offsetMs: NaN }, { offsetMs: Infinity }, { offsetMs: 1.5 },
    { offsetMs: 0, gapMs: -1 }, { offsetMs: 0, gapMs: 3001 }, { offsetMs: 0, gapMs: '2300' }, { offsetMs: 0, gapMs: null }]) {
    assert.throws(() => validatePolicy(value));
  }
  for (const value of [{ offsetMs: 0 }, { offsetMs: 600000, gapMs: 0 }, policy]) validatePolicy(value);
});

test('the clock sampler supplies wall and monotonic observations', () => {
  const start = startClock(), clock = atHeaders(start);
  assert(Number.isSafeInteger(clock.startMs));
  assert(Number.isSafeInteger(clock.headersMs));
  assert(clock.elapsedMs >= 0);
});

test('independent clock failures cannot disappear through declared differences', () => {
  const { withClockFailures } = require('./clock-expiry');
  const failure = { name: 'expiry', problems: ['res.clock.expires: wrong'] };
  assert.deepEqual(withClockFailures([], [failure]), [failure]);
  const news = [{ name: 'expiry', rules: 'rule', problems: ['res.body: wrong'] }];
  const result = withClockFailures(news, [failure, failure]);
  assert.equal(result.length, 1);
  assert.deepEqual(result[0].problems, ['res.body: wrong', 'res.clock.expires: wrong']);
  assert.deepEqual(news[0].problems, ['res.body: wrong']);
});

test('the real HTTP observer freezes the clock at headers, not at the end of a slow body', { timeout: 4000 }, async (t) => {
  const http = require('node:http');
  const { through } = require('./harness');
  let timer;
  const server = http.createServer((req, res) => {
    if (req.url.endsWith('/duplicate')) {
      const expiry = new Date(Date.now() + policy.offsetMs).toUTCString();
      res.writeHead(200, { expires: [expiry, expiry] });
      return res.end('duplicate');
    }
    res.writeHead(200, { expires: new Date(Date.now() + policy.offsetMs).toUTCString() });
    res.flushHeaders();
    timer = setTimeout(() => res.end('body'), 1100);
  });
  t.after(async () => {
    clearTimeout(timer);
    server.closeAllConnections();
    await new Promise((resolve) => server.close(resolve));
  });
  await new Promise((resolve, reject) => { server.on('error', reject); server.listen(0, '127.0.0.1', resolve); });
  const response = await through(server.address().port, {});
  assert.equal(response.body, 'body');
  assert(Date.now() - response.clock.headersMs >= 1000);
  assert.deepEqual(assessExpiry(policy, response, response, []).problems, []);
  const duplicate = await through(server.address().port, { path: '/duplicate' });
  assert.equal(duplicate.expiresHeaderCount, 2);
  assert(assessExpiry(policy, response, duplicate, []).problems.length > 0);
});

test('positive and zero offsets use the same independent oracle', () => {
  for (const offsetMs of [0, 600000]) {
    const a = answer(epoch + 100, epoch + 150, offsetMs);
    const b = answer(epoch + 2600, epoch + 2650, offsetMs);
    assert.deepEqual(assessExpiry({ offsetMs }, a, b, [expiryProblem]).problems, []);
  }
});

test('server Date cannot legitimize an incorrect Expires value', () => {
  const b = answer(epoch + 100, epoch + 150, policy.offsetMs + 86400000);
  b.headers.date = new Date(epoch + 86400000).toUTCString();
  assert.equal(check(answer(), b).evidence.rs.ok, false);
});

test('clock checks do not collapse unrelated existing reports with duplicate names', () => {
  const { withClockFailures } = require('./clock-expiry');
  const news = [{ name: 'same label', problems: ['one'] }, { name: 'same label', problems: ['two'] }];
  assert.equal(withClockFailures(news, []), news);
  const result = withClockFailures(news, [{ name: 'expiry', problems: ['res.clock.expires: wrong'] }]);
  assert.equal(result.length, 3);
  assert.deepEqual(result.slice(0, 2), news);
});
