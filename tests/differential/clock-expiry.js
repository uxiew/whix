'use strict';
const { performance } = require('node:perf_hooks');
const MAX_HEADER_WINDOW_MS = 1000;
const MAX_CLOCK_DRIFT_MS = 20;
const MAX_PAIR_GAP_MS = 3000;

// Opt-in fixture policy, not an inferred tolerance between the two proxies.
function validatePolicy(policy) {
  if (!policy || typeof policy !== 'object' || Array.isArray(policy)
      || !Number.isSafeInteger(policy.offsetMs)
      || !Number.isInteger(policy.gapMs === undefined ? 0 : policy.gapMs)
      || (policy.gapMs ?? 0) < 0 || (policy.gapMs ?? 0) > MAX_PAIR_GAP_MS) {
    throw new Error('clockExpires requires an integer offsetMs and gapMs in 0..3000');
  }
}

function startClock() { return { wall: Date.now(), mono: performance.now() }; }
function atHeaders(start) {
  return { startMs: start.wall, headersMs: Date.now(), elapsedMs: performance.now() - start.mono };
}

function inspect(answer, offsetMs) {
  const expires = answer.headers?.expires;
  const { startMs, headersMs, elapsedMs } = answer.clock || {};
  const evidence = { expires, expiresHeaderCount: answer.expiresHeaderCount, startMs, headersMs, elapsedMs };
  const fail = (reason) => ({ ...evidence, ok: false, reason });
  // Do not turn a slow/failed observation or a wall-clock adjustment into a
  // wide interval that could conceal a genuinely different expiry policy.
  if (!Number.isSafeInteger(startMs) || !Number.isSafeInteger(headersMs)
      || !Number.isFinite(elapsedMs) || elapsedMs < 0 || elapsedMs > MAX_HEADER_WINDOW_MS
      || headersMs < startMs || headersMs - startMs > MAX_HEADER_WINDOW_MS
      || Math.abs(headersMs - startMs - elapsedMs) > MAX_CLOCK_DRIFT_MS) {
    return fail('missing, wide or discontinuous clock window');
  }
  if (answer.expiresHeaderCount !== 1) return fail('expected exactly one Expires header');
  if (typeof expires !== 'string'
      || !/^(Mon|Tue|Wed|Thu|Fri|Sat|Sun), \d{2} (Jan|Feb|Mar|Apr|May|Jun|Jul|Aug|Sep|Oct|Nov|Dec) \d{4} \d{2}:\d{2}:\d{2} GMT$/.test(expires)) {
    return fail('missing or noncanonical HTTP date');
  }
  const timestamp = Date.parse(expires);
  if (!Number.isFinite(timestamp) || new Date(timestamp).toUTCString() !== expires) {
    return fail('invalid HTTP date');
  }
  const minMs = Math.floor((startMs + offsetMs) / 1000) * 1000;
  const maxMs = Math.floor((headersMs + offsetMs) / 1000) * 1000;
  if (!Number.isSafeInteger(minMs) || !Number.isSafeInteger(maxMs)
      || timestamp < minMs || timestamp > maxMs) {
    return { ...fail('outside the expected generation window'), minMs, maxMs };
  }
  return { ...evidence, minMs, maxMs, ok: true };
}

function assessExpiry(policy, whistle, rs, problems) {
  if (policy === undefined) return { problems, evidence: null };
  validatePolicy(policy);
  const evidence = {
    offsetMs: policy.offsetMs,
    whistle: inspect(whistle, policy.offsetMs),
    rs: inspect(rs, policy.offsetMs),
  };
  if (evidence.whistle.ok && evidence.rs.ok) {
    return { problems: problems.filter((p) => !p.startsWith('res.header.expires:')), evidence };
  }
  // Even equal-but-wrong dates fail; the legacy one-second exception cannot
  // excuse this independent expected-value check.
  return { problems: [...problems, 'res.clock.expires: ' + JSON.stringify(evidence)], evidence };
}

function withClockFailures(news, failures) {
  if (!failures.length) return news;
  const result = news.map((r) => ({ ...r, problems: [...r.problems] }));
  for (const failure of failures) {
    let entry = result.find((r) => r.name === failure.name);
    if (!entry) { entry = { name: failure.name, problems: [] }; result.push(entry); }
    entry.problems = [...new Set([...entry.problems, ...failure.problems])];
  }
  return result;
}

module.exports = { validatePolicy, startClock, atHeaders, assessExpiry, withClockFailures };
