import {test} from 'node:test';
import assert from 'node:assert/strict';
import {mkdtempSync, mkdirSync, writeFileSync, readFileSync, rmSync, existsSync} from 'node:fs';
import {tmpdir} from 'node:os';
import {join} from 'node:path';
import {createHash} from 'node:crypto';
import {stageWorkerAssets} from './stage.mjs';
const assets=['compute-worker.mjs','compute-domains.mjs','compute-protocol.mjs','pkg/compute/tilo_browser_compute.js','pkg/compute/tilo_browser_compute_bg.wasm'];
function fixture(fn) {const root=mkdtempSync(join(tmpdir(),'compute-stage-'));try{const product=join(root,'product'),stage=join(root,'stage');mkdirSync(join(product,'pkg/compute'),{recursive:true});for(const name of [...assets,'index.html'])writeFileSync(join(product,name),name);fn(product,stage);}finally{rmSync(root,{recursive:true,force:true});}}
test('stages exactly hash-bound worker assets while preserving product index',()=>fixture((product,stage)=>{
  const hashes=stageWorkerAssets(product,stage);
  for(const name of assets){assert.ok(existsSync(join(stage,name)), `missing ${name}`);assert.deepEqual(readFileSync(join(stage,name)),readFileSync(join(product,name)));assert.equal(hashes[name],createHash('sha256').update(name).digest('hex'));}
  assert.equal(readFileSync(join(product,'index.html'),'utf8'),'index.html');assert.equal(existsSync(join(stage,'index.html')),false);
}));
test('refuses incomplete product, reused staging and staging inside product',()=>fixture((product,stage)=>{
  assert.throws(()=>stageWorkerAssets(product,join(product,'qualification')));
  mkdirSync(stage);writeFileSync(join(stage,'index.html'),'stale');assert.throws(()=>stageWorkerAssets(product,stage));
  rmSync(stage,{recursive:true});rmSync(join(product,assets[0]));assert.throws(()=>stageWorkerAssets(product,stage));assert.equal(existsSync(stage),false);
}));
