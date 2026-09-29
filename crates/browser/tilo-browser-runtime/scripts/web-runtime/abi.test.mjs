import assert from 'node:assert/strict';
import test from 'node:test';
import {lockDigest} from './abi.mjs';

const entry = (name, version, extra = '') => `[[package]]\nname = "${name}"\nversion = "${version}"\n${extra}`;
const lock = (...entries) => `# generated\nversion = 4\n\n${entries.join('\n')}`;

test('the lock digest covers exactly the core packages', () => {
  const core = new Set(['serde@1.0.0', 'tilo-ui@0.1.0']);
  const base = lockDigest(lock(entry('serde', '1.0.0', 'checksum = "aa"\n'), entry('tilo-ui', '0.1.0')), core);
  const withApp = lockDigest(lock(entry('rt-demo', '0.1.0'), entry('serde', '1.0.0', 'checksum = "aa"\n'), entry('tilo-ui', '0.1.0'), entry('left-pad', '1.3.0')), core);
  assert.equal(withApp, base, 'another workspace\'s own packages leave the core\'s id alone');
  const changed = lockDigest(lock(entry('serde', '1.0.0', 'checksum = "bb"\n'), entry('tilo-ui', '0.1.0')), core);
  assert.notEqual(changed, base, 'a core package\'s locked source is part of the id');
});
