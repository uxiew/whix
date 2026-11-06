// HTTP rule/script sources: real origin + source server, with independent
// expected effects for each proxy. run.js owns the isolated proxy pair.
'use strict';
const assert = require('node:assert/strict');
const fs = require('node:fs');
const http = require('node:http');
const { isDeepStrictEqual } = require('node:util');
const base = Number(process.env.PORT_BASE || 18700);
const [W, RS, ORIGIN, SOURCE] = [base, base + 1, base + 2, base + 3];
const host = '127.0.0.1';
const target = `http://${host}:${ORIGIN}`;
const location = `http://${host}:${SOURCE}`;
const jsonAt = process.argv.indexOf('--json');
const jsonPath = jsonAt < 0 ? null : process.argv[jsonAt + 1];
const sources = new Map();
const cases = [];

for (const protocol of ['reqScript', 'reqRules', 'resScript', 'resRules']) {
  const response = protocol.startsWith('res');
  const rule = `* ${response ? 'resHeaders' : 'reqHeaders'}://x-source=${protocol}`;
  const content = protocol.endsWith('Script') ? `rules.push('${rule}');` : rule;
  for (const queryOnly of [false, true]) {
    const path = queryOnly ? `/?family=${protocol}` : `/${protocol}`;
    sources.set(path, content);
    cases.push({ name: `${protocol}${queryOnly ? ' query-only URL' : ''}`,
      rules: `${target} ${protocol}://${location}${queryOnly ? path.slice(1) : path}`,
      [response ? 'res' : 'req']: { 'x-source': protocol }, fetches: [path], response });
  }
}

for (const protocol of ['resRules', 'resScript']) {
  const rule = '* resHeaders://x-source=filtered includeFilter://s:201';
  const path = `/filter-${protocol}`;
  sources.set(path, protocol === 'resScript' ? `rules.push('${rule}');` : rule);
  for (const matched of [true, false]) {
    cases.push({ name: `${protocol} inner response filter ${matched ? 'match' : 'miss'}`,
      rules: `${target} ${protocol}://${location}${path}`, path: matched ? '/probe' : '/miss',
      status: matched ? 201 : 202, res: matched ? { 'x-source': 'filtered' } : {},
      fetches: [path], response: true });
  }
  cases.push({ name: `${protocol} outer status filter stays request-phase`,
    rules: `${target} ${protocol}://${location}${path} includeFilter://s:201`,
    fetches: [], response: true });
}

sources.set('/duplicate', '* resHeaders://x-source=duplicate');
sources.set('/first', "rules.push('* resHeaders://x-source=first');");
sources.set('/second', "rules.push('* resHeaders://x-source=second');");
sources.set('/coexist', "rules.push('* resHeaders://x-source-script=also');");
sources.set('/status-', "rules.push('* resHeaders://x-source='+statusCode);");
cases.push(
  { name: 'duplicate resRules share a static source',
    rules: `${target} resRules://${location}/duplicate\n${target} resRules://${location}/duplicate`,
    res: { 'x-source': 'duplicate' }, fetches: ['/duplicate'], whistleFetches: ['/duplicate', '/duplicate'], response: true },
  { name: 'only first resScript source is fetched',
    rules: `${target} resScript://${location}/first\n${target} resScript://${location}/second`,
    res: { 'x-source': 'first' }, fetches: ['/first'], response: true },
  { name: 'resRules and resScript both execute',
    rules: `${target} resRules://${location}/resRules\n${target} resScript://${location}/coexist`,
    res: { 'x-source': 'resRules', 'x-source-script': 'also' }, fetches: ['/resRules', '/coexist'], response: true },
  { name: 'response script uses response context but request-phase URL template',
    rules: target + ' resScript://`' + location + '/status-${statusCode}`',
    res: { 'x-source': '201' }, fetches: ['/status-'], response: true },
);

let origins = [], fetches = [];
const origin = http.createServer((request, response) => {
  origins.push({ path: request.url, headers: request.headers });
  response.writeHead(request.url === '/miss' ? 202 : 201,
    { 'content-type': 'text/plain', 'x-origin': 'real-loopback' });
  response.end('origin-body');
});
const source = http.createServer((request, response) => {
  fetches.push({ path: request.url, headers: request.headers, origins: origins.length });
  response.writeHead(sources.has(request.url) ? 200 : 404, { 'content-type': 'text/plain' });
  response.end(sources.get(request.url) || 'missing source');
});

function request(port, path, body, headers = {}) {
  return new Promise((resolve, reject) => {
    if (body !== undefined) headers = { ...headers, 'content-length': Buffer.byteLength(body) };
    const req = http.request({ host, port, path, headers, method: body === undefined ? 'GET' : 'POST' }, (res) => {
      const parts = [];
      res.on('data', (data) => parts.push(data));
      res.on('error', reject);
      res.on('end', () => resolve({ status: res.statusCode, headers: res.headers, body: Buffer.concat(parts).toString() }));
    });
    req.on('error', reject);
    req.setTimeout(5000, () => req.destroy(new Error('request timeout')));
    req.end(body);
  });
}
async function setRules(port, text) {
  const result = port === W
    ? await request(port, '/cgi-bin/rules/add', 'name=Default&selected=1&value=' + encodeURIComponent(text),
      { 'content-type': 'application/x-www-form-urlencoded' })
    : await request(port, '/api/rules', text, { 'content-type': 'text/plain' });
  assert.equal(result.status, 200, 'rules configuration');
  if (port === W) assert.equal(JSON.parse(result.body).ec, 0, 'Whistle accepted the rules');
}
const markers = (headers) => Object.fromEntries(Object.entries(headers).filter(([name]) => name.startsWith('x-source')));
const listen = (server, port) => new Promise((resolve, reject) => {
  server.once('error', reject);
  server.listen(port, host, resolve);
});

async function main() {
  const raw = [], report = [], expectationFailures = [], fetchPolicyDifferences = [];
  try {
    await Promise.all([listen(origin, ORIGIN), listen(source, SOURCE)]);
    for (const c of cases) {
      const answers = {};
      for (const [name, port] of [['whistle', W], ['rs', RS]]) {
        origins = []; fetches = [];
        let effect;
        try {
          await setRules(port, c.rules);
          const result = await request(port, target + (c.path || '/probe'), undefined,
            { host: `${host}:${ORIGIN}`, cookie: 'fixture-only', authorization: 'Bearer fixture-only' });
          effect = { status: result.status, body: result.body, origin: result.headers['x-origin'],
            originCount: origins.length, req: markers(origins[0]?.headers || {}), res: markers(result.headers) };
          assert.deepEqual(effect, { status: c.status || 201, body: 'origin-body', origin: 'real-loopback',
            originCount: 1, req: c.req || {}, res: c.res || {} });
          const expectedFetches = port === W && c.whistleFetches ? c.whistleFetches : c.fetches;
          assert.deepEqual(fetches.map((f) => f.path).sort(), [...expectedFetches].sort(), 'selected source fetches');
          for (const f of fetches) {
            assert.equal(f.origins, c.response ? 1 : 0, 'source execution phase');
            assert.equal(f.headers.host, `${host}:${SOURCE}`, 'source Host includes its port');
            assert.equal(f.headers.cookie, undefined, 'do not forward client cookies');
            assert.equal(f.headers.authorization, undefined, 'do not forward client credentials');
          }
        } catch (error) {
          expectationFailures.push({ case: c.name, proxy: name, error: error.message, effect, origins, fetches });
        }
        answers[name] = { effect, fetches: fetches.map((f) => f.path) };
      }
      // Compare effects, not the explicitly different per-phase fetch policy.
      // Both policies are still asserted above; dynamic sources are not claimed equivalent.
      const same = isDeepStrictEqual(answers.whistle.effect, answers.rs.effect);
      const whistle = JSON.stringify(answers.whistle.effect), rs = JSON.stringify(answers.rs.effect);
      raw.push({ case: c.name, whistle, rs, same,
        sourceFetches: { whistle: answers.whistle.fetches, rs: answers.rs.fetches } });
      if (!same) report.push({ name: c.name, problems: [`answer: whistle=${whistle} rs=${rs}`] });
      if (c.whistleFetches) fetchPolicyDifferences.push({ case: c.name, whistle: answers.whistle.fetches, rs: answers.rs.fetches });
    }
  } finally {
    await Promise.all([origin, source].map((server) => {
      server.closeAllConnections();
      return new Promise((resolve) => server.close(resolve));
    }));
  }
  const output = { ran: raw.length, expectationChecks: raw.length * 2, differing: report.length,
    report, expectationFailures, fetchPolicyDifferences, raw };
  if (jsonPath) fs.writeFileSync(jsonPath, JSON.stringify(output, null, 2));
  console.log(JSON.stringify(output, null, 2));
  process.exitCode = report.length || expectationFailures.length ? 1 : 0;
}
main().catch((error) => { console.error(error); process.exitCode = 2; });
