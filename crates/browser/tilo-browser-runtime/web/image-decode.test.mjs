import test from 'node:test';
import assert from 'node:assert/strict';
import {createImageDecoder} from './image-decode.mjs';

/** A worker that answers each request as `answer` says, or fails to load. */
function fakeWorker({answer,loadFails=false}={}){
  const made=[];
  class Worker{
    constructor(url,options){this.url=url;this.options=options;this.sent=[];made.push(this);}
    postMessage(message){
      this.sent.push(message);
      queueMicrotask(()=>loadFails?this.onerror?.({preventDefault(){}}):this.onmessage?.({data:answer(message)}));
    }
    terminate(){this.terminated=true;}
  }
  return {Worker,made};
}

test('images decode in one worker, each answer reaching its own request', async () => {
  const {Worker,made}=fakeWorker({answer:({id,maxDimension})=>({id,image:{width:maxDimension,height:1,rgba:new Uint8Array(4)}})});
  const decode=createImageDecoder({Worker,url:'worker.mjs',decode:()=>assert.fail('decoded on the page')});
  const [a,b]=await Promise.all([decode(new Uint8Array([1]),64),decode(new Uint8Array([2]),128)]);
  assert.equal(a.width,64);
  assert.equal(b.width,128);
  assert.equal(made.length,1);
  assert.deepEqual(made[0].options,{type:'module',name:'tilo-image-decode'});
});

test('an image the worker cannot decode fails on its own', async () => {
  const {Worker}=fakeWorker({answer:({id})=>({id,error:'broken image'})});
  const decode=createImageDecoder({Worker,url:'worker.mjs',decode:()=>assert.fail('decoded on the page')});
  await assert.rejects(decode(new Uint8Array([1]),64),/broken image/);
});

test('a worker that cannot start leaves decoding to the page', async () => {
  const page=[];
  const onPage=async (bytes,maxDimension)=>{page.push(maxDimension);return {width:maxDimension,height:1,rgba:new Uint8Array(4)};};
  const failing=fakeWorker({loadFails:true});
  const decode=createImageDecoder({Worker:failing.Worker,url:'worker.mjs',decode:onPage});
  assert.equal((await decode(new Uint8Array([1]),32)).width,32);
  assert.equal((await decode(new Uint8Array([1]),48)).width,48);
  assert.deepEqual(page,[32,48]);
  assert.equal(failing.made.length,1);
  assert.ok(failing.made[0].terminated);

  const missing=createImageDecoder({Worker:class{constructor(){throw Error('no workers');}},url:'worker.mjs',decode:onPage});
  assert.equal((await missing(new Uint8Array([1]),16)).width,16);
});
