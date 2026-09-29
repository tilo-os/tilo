import assert from 'node:assert/strict';
import test from 'node:test';
import {abiPackages, lockDigest} from './abi.mjs';

const entry = (name, version, extra = '') => `[[package]]\nname = "${name}"\nversion = "${version}"\n${extra}`;
const lock = (...entries) => `# generated\nversion = 4\n\n${entries.join('\n')}`;

test('the id covers the core packages the app collection shares', () => {
  const graph = {
    'tilo-browser-runtime': ['serde@1.0.0', 'tilo-ui@0.1.0', 'tilo-browser-peer@0.1.0', 'web-sys@0.3.85'],
    ebook: ['serde@1.0.0', 'tilo-ui@0.1.0', 'roxmltree@0.21.0'],
    'word-processor': ['tilo-ui@0.1.0', 'serde@1.0.0'],
  };
  assert.deepEqual([...abiPackages(graph)].sort(), ['serde@1.0.0', 'tilo-ui@0.1.0'],
    'a crate only the core uses (an optional peer) is no part of it, nor an app\'s own crate');
  assert.deepEqual([...abiPackages({'tilo-browser-runtime': ['serde@1.0.0']})], ['serde@1.0.0'],
    'without a collection the core\'s graph decides');
});

test('the lock digest covers exactly the core packages', () => {
  const core = new Set(['serde@1.0.0', 'tilo-ui@0.1.0']);
  const base = lockDigest(lock(entry('serde', '1.0.0', 'checksum = "aa"\n'), entry('tilo-ui', '0.1.0')), core);
  const withApp = lockDigest(lock(entry('rt-demo', '0.1.0'), entry('serde', '1.0.0', 'checksum = "aa"\n'), entry('tilo-ui', '0.1.0'), entry('left-pad', '1.3.0')), core);
  assert.equal(withApp, base, 'another workspace\'s own packages leave the core\'s id alone');
  const changed = lockDigest(lock(entry('serde', '1.0.0', 'checksum = "bb"\n'), entry('tilo-ui', '0.1.0')), core);
  assert.notEqual(changed, base, 'a core package\'s locked source is part of the id');
});
