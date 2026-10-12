import assert from 'node:assert/strict';
import {test} from 'node:test';
import {inspectArtifact} from './artifact.mjs';
const module = (flags=1, maximum=[0x80,0x20]) => new Uint8Array([0,97,115,109,1,0,0,0,5,3+maximum.length,1,flags,18,...maximum]);
test('final worker artifact carries a private 4096-page ceiling',()=>assert.deepEqual(inspectArtifact(module()),{flags:1,initial:18,maximum:4096}));
test('refuses missing, shared, wrong-limit, malformed and over-budget workers',()=>{
  for(const bytes of [new Uint8Array([0,97,115,109,1,0,0,0]),module(3),module(1,[0x81,0x20]),module(1,[]),new Uint8Array(1024*1024+1)])assert.throws(()=>inspectArtifact(bytes));
});

test('dev worker retains memory checks without release-only byte budget',()=>{
  const padded = new Uint8Array([...module(),0,0x80,0x80,0x40,...new Uint8Array(1024*1024)]);
  assert.throws(()=>inspectArtifact(padded));
  assert.deepEqual(inspectArtifact(padded,{release:false}),{flags:1,initial:18,maximum:4096});
  assert.throws(()=>inspectArtifact(module(3),{release:false}));
});
function importing(name) {
  const text = new TextEncoder(), encode = value => [text.encode(value).length,...text.encode(value)];
  const imports = [1,...encode('./tilo_browser_compute_bg.js'),...encode(name),0,0];
  return new Uint8Array([...module().slice(0,8),1,4,1,96,0,0,2,imports.length,...imports,...module().slice(8)]);
}
test('dev permits only the observed bindgen externref initializer beyond release glue',()=>{
  assert.deepEqual(inspectArtifact(importing('__wbindgen_init_externref_table'),{release:false}),{flags:1,initial:18,maximum:4096});
  assert.throws(()=>inspectArtifact(importing('__wbindgen_init_externref_table')));
  assert.throws(()=>inspectArtifact(importing('fd_write'),{release:false}));
});
