import assert from 'node:assert/strict';
import {createHash} from 'node:crypto';
import {copyFileSync, mkdtempSync, mkdirSync, writeFileSync, rmSync} from 'node:fs';
import {tmpdir} from 'node:os';
import {dirname, join, resolve} from 'node:path';
import {fileURLToPath} from 'node:url';
import test from 'node:test';
import {fixtureBytes} from './fixture-assets.mjs';

const tree = resolve(dirname(fileURLToPath(import.meta.url)), '../..');

test('browser fixtures use the verified in-tree seed and reject missing, corrupt and unpinned inputs', () => {
  const scratch = mkdtempSync(join(tmpdir(), 'tilo-browser-fixtures-'));
  // Even an environment that asks for a download leaves the seed the only source.
  const previous = Object.fromEntries(['TILO_TEST_ASSET_CACHE', 'TILO_TEST_ASSETS_OFFLINE'].map(key => [key, process.env[key]]));
  try {
    const repository = join(scratch, 'repository');
    mkdirSync(join(repository, 'tools'), {recursive: true});
    copyFileSync(join(tree, 'tools/test-assets.py'), join(repository, 'tools/test-assets.py'));
    const cache = join(repository, 'tools/test-assets/seed');
    Object.assign(process.env, {TILO_TEST_ASSET_CACHE: join(scratch, 'elsewhere'), TILO_TEST_ASSETS_OFFLINE: '0'});
    const payload = Buffer.from('frozen fixture bytes\n');
    const sha256 = createHash('sha256').update(payload).digest('hex');
    const file = 'case.pdf';
    const row = {file, sha256, bytes: payload.length, url: `https://code.tilo.so/api/packages/tilo/generic/source-evidence/${sha256}/${file}`};
    const manifest = join(scratch, 'artifacts.json');
    writeFileSync(manifest, JSON.stringify({schema: 1, assets: [row]}));
    assert.throws(() => fixtureBytes(repository, manifest, file), /uncached test asset/);
    mkdirSync(join(cache, sha256), {recursive: true});
    const cached = join(cache, sha256, file);
    writeFileSync(cached, payload);
    assert.deepEqual(fixtureBytes(repository, manifest, file), payload);
    const corrupt = Buffer.from(payload);
    corrupt[0] ^= 1;
    writeFileSync(cached, corrupt);
    assert.throws(() => fixtureBytes(repository, manifest, file), /SHA-256 mismatch/);
    writeFileSync(cached, payload);
    writeFileSync(manifest, JSON.stringify({schema: 1, assets: [{...row, url: 'https://example.com/case.pdf'}]}));
    assert.throws(() => fixtureBytes(repository, manifest, file), /unpinned URL/);
  } finally {
    for (const [key, value] of Object.entries(previous)) {
      if (value === undefined) delete process.env[key];
      else process.env[key] = value;
    }
    rmSync(scratch, {recursive: true, force: true});
  }
});
