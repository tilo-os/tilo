import assert from 'node:assert/strict';
import {readFileSync, writeFileSync, mkdirSync, existsSync, readdirSync, realpathSync} from 'node:fs';
import {dirname, join, relative, resolve, sep} from 'node:path';
import {createHash} from 'node:crypto';
export const workerAssets = ['compute-worker.mjs','compute-domains.mjs','compute-protocol.mjs',
  'pkg/compute/tilo_browser_compute.js','pkg/compute/tilo_browser_compute_bg.wasm'];
export const sha = bytes => createHash('sha256').update(bytes).digest('hex');
export function stageWorkerAssets(product, staging) {
  product = realpathSync(product);
  staging = resolve(staging);
  // Resolve existing parents too, so aliases cannot place fixtures in the product.
  let parent = staging, suffix = [];
  while (!existsSync(parent)) { suffix.unshift(parent.slice(dirname(parent).length + 1)); parent = dirname(parent); }
  staging = join(realpathSync(parent), ...suffix);
  const path = relative(product, staging);
  assert.ok(path === '..' || path.startsWith(`..${sep}`) || resolve(path) === path, 'qualification staging must be outside product');
  assert.ok(!existsSync(staging) || readdirSync(staging).length === 0, 'fresh empty staging required');
  const bytes = Object.fromEntries(workerAssets.map(name => [name, readFileSync(join(product, name))]));
  const hashes = {};
  for (const name of workerAssets) {
    mkdirSync(dirname(join(staging, name)), {recursive:true});
    writeFileSync(join(staging, name), bytes[name]); // Copies, never links to product assets.
    hashes[name] = sha(bytes[name]);
    assert.equal(sha(readFileSync(join(staging, name))), hashes[name]);
  }
  return hashes;
}
export function bundleHashes(directory) {
  const hashes = {};
  function collect(path) {
    for (const entry of readdirSync(path, {withFileTypes:true})) {
      const file = join(path, entry.name);
      if (entry.isDirectory()) collect(file);
      else hashes[relative(directory, file)] = sha(readFileSync(file));
    }
  }
  collect(directory);
  return hashes;
}
