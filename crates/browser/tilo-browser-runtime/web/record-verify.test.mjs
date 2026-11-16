import test from 'node:test';
import assert from 'node:assert/strict';
import {createRecordVerifier} from './record-verify.mjs';

/** A worker that answers each record as `answer` says. */
function fakeWorker({answer,fails=false}={}){
  const made=[];
  class Worker{
    constructor(url,options){this.url=url;this.options=options;this.sent=[];made.push(this);}
    postMessage(message){
      this.sent.push(message);
      if(!('id' in message))return;
      queueMicrotask(()=>fails?this.onerror?.({preventDefault(){}}):this.onmessage?.({data:answer(message)}));
    }
    terminate(){this.terminated=true;}
  }
  return {Worker,made};
}

/** Timers the test runs by hand. */
function timers(){
  const due=new Map();let next=0;
  return {setTimer:(run)=>{due.set(++next,run);return next;},clearTimer:id=>due.delete(id),run:()=>{for(const [id,run] of [...due]){due.delete(id);run();}},count:()=>due.size};
}

test('records verify in one worker, started with the page module', async () => {
  const {Worker,made}=fakeWorker({answer:({id,bytes})=>({id,hashes:new Uint8Array(32).fill(bytes[0])})});
  const module={compiled:true};
  const verify=createRecordVerifier({Worker,url:'worker.mjs',module:()=>module,...timers()});
  const [a,b]=await Promise.all([verify(new Uint8Array([1])),verify(new Uint8Array([2]))]);
  assert.equal(a[0],1);
  assert.equal(b[0],2);
  assert.equal(made.length,1);
  assert.deepEqual(made[0].options,{type:'module',name:'tilo-record-verify'});
  assert.equal(made[0].sent[0].module,module,'the page module goes first');
});

test('a record the worker refuses rejects on its own', async () => {
  const {Worker}=fakeWorker({answer:({id})=>({id,error:'mismatching heads'})});
  const verify=createRecordVerifier({Worker,url:'worker.mjs',...timers()});
  await assert.rejects(verify(new Uint8Array([1])),/mismatching heads/);
});

test('a worker that fails rejects what it held, and the next record starts another', async () => {
  const failing=fakeWorker({fails:true});
  const verify=createRecordVerifier({Worker:failing.Worker,url:'worker.mjs',...timers()});
  await assert.rejects(verify(new Uint8Array([1])),/failed/);
  assert.ok(failing.made[0].terminated);
  await assert.rejects(verify(new Uint8Array([1])));
  assert.equal(failing.made.length,2);
});

test('an idle worker is let go, and a later record starts it again', async () => {
  const {Worker,made}=fakeWorker({answer:({id})=>({id,hashes:new Uint8Array(32)})});
  const clock=timers();
  const verify=createRecordVerifier({Worker,url:'worker.mjs',...clock});
  await verify(new Uint8Array([1]));
  assert.equal(clock.count(),1);
  clock.run();
  assert.ok(made[0].terminated,'its memory goes with it');
  await verify(new Uint8Array([2]));
  assert.equal(made.length,2);
});

test('the smallest record waiting is verified first, one at a time', async () => {
  const made=[];
  class Worker{
    constructor(){this.held=[];made.push(this);}
    postMessage(message){if('id' in message)this.held.push(message);}
    terminate(){}
  }
  const verify=createRecordVerifier({Worker,url:'worker.mjs',...timers()});
  const sizes=[3,1,2];
  const answers=sizes.map(size=>verify(new Uint8Array(size)));
  const [worker]=made;
  const sent=()=>worker.held.map(message=>message.bytes.length);
  assert.deepEqual(sent(),[3],'one record at a time');
  const answer=()=>{
    const {id,bytes}=worker.held.at(-1);
    worker.onmessage({data:{id,hashes:new Uint8Array(32).fill(bytes.length)}});
  };
  answer();
  assert.deepEqual(sent(),[3,1],'then the smallest waiting');
  answer();
  assert.deepEqual(sent(),[3,1,2]);
  answer();
  const hashes=await Promise.all(answers);
  assert.deepEqual(hashes.map(hash=>hash[0]),sizes,'each answer reaches its own request');
});
