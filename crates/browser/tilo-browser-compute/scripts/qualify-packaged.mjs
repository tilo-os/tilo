// Qualify exact canonical output in dedicated disposable staging, never in-place.
import assert from 'node:assert/strict';
import {readFileSync, writeFileSync, realpathSync} from 'node:fs';
import {join, resolve} from 'node:path';
import {fileURLToPath} from 'node:url';
import {execFileSync} from 'node:child_process';
import {stageWorkerAssets, bundleHashes} from './stage.mjs';
const root = fileURLToPath(new URL('../../../..', import.meta.url));
assert.ok(process.argv[2] && process.argv[3], 'usage: qualify-packaged.mjs PRODUCT_BUNDLE EMPTY_TEST_STAGING');
const product = realpathSync(process.argv[2]), staging = resolve(process.argv[3]);
const chromeArgs = process.argv.slice(4);
assert.ok(chromeArgs.every(arg => arg === '--no-sandbox'), 'unsupported qualification argument');
const before = bundleHashes(product);
const assets = stageWorkerAssets(product, staging);
try {
  execFileSync(process.env.CARGO || 'cargo', ['run','--locked','--manifest-path',join(root,'crates/browser/tilo-browser-compute/Cargo.toml'),
    '--example','fixtures','--',join(staging,'fixtures')], {cwd:root, stdio:'inherit'});
  execFileSync(process.execPath,[join(root,'crates/browser/tilo-browser-compute/scripts/qualify.mjs'),staging,...chromeArgs],{cwd:root,stdio:'inherit'});
  const path = join(staging,'qualification.json');
  const receipt = JSON.parse(readFileSync(path));
  assert.deepEqual(receipt.assets, assets, 'qualified assets must match emitted product bytes');
  const after = bundleHashes(product);
  assert.deepEqual(after, before, 'every canonical product asset unchanged after qualification');
  receipt.canonicalProduct = {bundle:product, beforeFiles:before, afterFiles:after, assets, unchanged:true};
  writeFileSync(path,JSON.stringify(receipt,null,2)+'\n');
} finally {
  assert.deepEqual(bundleHashes(product),before,'qualification must preserve every product file');
}
