import test from 'node:test';
import assert from 'node:assert/strict';
import {locate} from './location.mjs';

const browser=answer=>({geolocation:{getCurrentPosition(ok,fail,options){assert.equal(options.timeout,20000);answer(ok,fail);}}});

test('a fix resolves with its accuracy',async()=>{
  const navigator=browser(ok=>ok({coords:{latitude:52.5,longitude:13.4,accuracy:12}}));
  assert.deepEqual(await locate(20000,{navigator}),{lat:52.5,lon:13.4,accuracy:12});
});

test('refusals and missing services say why',async()=>{
  await assert.rejects(locate(20000,{navigator:browser((_,fail)=>fail({code:1}))}),/permission denied/);
  await assert.rejects(locate(20000,{navigator:browser((_,fail)=>fail({code:3}))}),/timed out/);
  await assert.rejects(locate(20000,{navigator:{}}),/no location service/);
});
