'use strict';

// Execute the pinned package's real exporters with inert in-memory stores.
// Never load lib/rules/util: it initializes the user's Whistle data directory
// and starts an update timer. The same fixtures feed real-proxy Rust tests.
const fs = require('fs');
const path = require('path');
const vm = require('vm');
const assert = require('assert/strict');
const whistle = require('./whistle-pkg');
const fixture = (kind) => JSON.parse(fs.readFileSync(path.join(__dirname, '../data/whistle', `${kind}.json`), 'utf8'));

function exported(kind, selected) {
  const data = fixture(kind);
  const store = {
    getDefault: () => data.Default,
    list: () => data[''].filter((name) => name !== 'Default').map((name) => ({ name, data: data[name] })),
  };
  const module = { exports: {} };
  const source = path.join(whistle.dir, 'biz/webui/cgi-bin', kind, 'export.js');
  vm.runInNewContext(fs.readFileSync(source, 'utf8'), {
    module,
    require(name) {
      if (name === '../../../../lib/rules/util') return { [kind]: store };
      if (name === '../util') return { formatDate: () => 'fixture', sendGzipText: (_req, res, _err, text) => res.send(text) };
      throw new Error(`unexpected exporter dependency: ${name}`);
    },
  }, { filename: source, timeout: 1000 });
  let answer;
  module.exports({ query: { [kind]: selected && JSON.stringify(selected) } }, {
    attachment() { return this; },
    send(text) { answer = JSON.parse(text); },
  });
  return answer;
}

assert.deepEqual(exported('rules'), fixture('rules'));
assert.deepEqual(exported('rules', { '10': true }), { '10': fixture('rules')['10'], '': ['Default', '10'] });
assert.deepEqual(exported('values'), fixture('values'));
assert.deepEqual(exported('values', {}), { '': [] });
console.log(`migration-export: 4 checks passed against whistle ${whistle.version}`);
