import {test} from 'node:test';
import assert from 'node:assert/strict';
import {createComputeDomains} from './compute-domains.mjs';
const tick = () => new Promise(resolve => setImmediate(resolve));
class Worker {
  constructor() { this.listeners = new Map(); this.messages = []; this.dead = false; }
  addEventListener(k, f) { this.listeners.set(k, f); }
  removeEventListener(k) { this.listeners.delete(k); }
  terminate() { this.dead = true; }
  postMessage(m) { this.messages.push(m); }
  emit(m) { this.listeners.get('message')?.({data: m}); }
  fail(k = 'error') { this.listeners.get(k)?.({}); }
  reply() {
    const r = this.messages[0];
    this.emit({...r.identity, type:'result', width:1, height:1, length:4, decodeMs:1, worker:true});
    this.emit({...r.identity, type:'chunk', offset:0, bytes:new Uint8Array([1,2,3,4]).buffer});
  }
}
function setup(options={}) {
  const workers=[];
  const pool=createComputeDomains({workerFactory:()=>{const w=new Worker();workers.push(w);return w;}, yieldTask:tick, ...options});
  const owner=(n,lane='background',signal)=>pool.bindOwner({owner:n,lifetime:`life-${n}`,domain:'images',lane,signal,authorize:()=>true});
  const request=(o, extra={}, consume=async()=>{})=>o.decode({bytes:new Uint8Array([1,2]),source:'ticket',maxDimension:1,...extra},consume);
  return {pool,workers,owner,request};
}
test('admission refuses oversized input before worker creation', async()=>{
  const {pool,workers,owner,request}=setup();
  await assert.rejects(request(owner('a'),{bytes:new Uint8Array(16*1024*1024+1)}),/budget/);
  assert.equal(workers.length,0); assert.equal(pool.snapshot().jobs,0);
});
test('real output custody lasts through blocked consumer and releases exactly once',async()=>{
  const {pool,workers,owner,request}=setup();
  let release; const blocked=new Promise(r=>release=r); let pixels;
  const p=request(owner('a'),{},async result=>{pixels=[...result.rgba];await blocked;});
  await tick(); workers[0].reply(); await tick();
  assert.deepEqual(pixels,[1,2,3,4]); assert.equal(pool.snapshot().jobs,1); assert.equal(pool.snapshot().leases,0);
  assert.ok(pool.snapshot().managed>0); assert.equal(workers[0].dead,true);
  release(); await p; assert.deepEqual(pool.snapshot(),{jobs:0,leases:0,encoded:0,managed:0});
});
test('two leases, only one background, fair owners and queued cancellation',async()=>{
  const {pool,workers,owner,request}=setup(); const a=owner('a'), b=owner('b'), f=owner('f','foreground');
  const stop=new AbortController(); const c=owner('c','foreground',stop.signal);
  const p1=request(a),p2=request(a),p3=request(b),p4=request(f),p5=request(c); const canceled=assert.rejects(p5,/closed/);
  await tick();assert.equal(workers.length,2);assert.equal(pool.snapshot().leases,2);stop.abort();await canceled;
  workers[0].reply();await p1;await tick();assert.equal(workers[2].messages[0].identity.owner,'b');
  workers[1].reply();workers[2].reply();await Promise.all([p3,p4]);await tick();workers[3].reply();await p2;
  assert.equal(pool.snapshot().jobs,0);
});
test('active abort, stale generation, worker failures and timeout leave no pending job',async(t)=>{
  // Cancellation is ordered before timeout; an event-loop stall must not race
  // that order. Advance the actual production timer explicitly below.
  t.mock.timers.enable({apis:['setTimeout']});
  const {pool,workers,owner,request}=setup({timeoutMs:20});const abort=new AbortController();
  const p=request(owner('a','foreground',abort.signal));const rejected=assert.rejects(p,/closed/);await tick();
  const stale=workers[0].listeners.get('message');abort.abort();await rejected;
  const next=request(owner('b'));await tick();stale({data:{bad:true}});workers[1].reply();await next;
  for(const kind of ['error','messageerror']){const q=request(owner(kind));const failed=assert.rejects(q,/worker/);await tick();workers.at(-1).fail(kind);await failed;}
  const timedOut=assert.rejects(request(owner('timeout')),/timeout/);await tick();
  t.mock.timers.tick(19);await tick();assert.equal(pool.snapshot().jobs,1);
  assert.equal(workers.at(-1).dead,false);
  t.mock.timers.tick(1);await timedOut;assert.equal(pool.snapshot().jobs,0);
  assert.equal(workers.at(-1).dead,true);
});
test('cached subviews use intrinsic fields and bounded yielded exact copies',async()=>{
  let yields=0;const {workers,owner,request}=setup({yieldTask:async()=>{yields++;await tick();}});
  const backing=new Uint8Array(600000);backing.fill(23);const bytes=backing.subarray(7,550007);
  Object.defineProperties(bytes,{buffer:{get(){throw Error('own buffer');}},byteLength:{value:1},byteOffset:{value:0},subarray:{value(){throw Error('subarray');}}});
  const p=request(owner('a'),{bytes});await tick();await tick();await tick();await tick();
  assert.ok(yields>=3);assert.equal(workers[0].messages[0].bytes.byteLength,550000);assert.equal(backing.byteLength,600000);
  workers[0].reply();await p;
});
test('malformed identities and duplicate/out of order chunks fail closed',async()=>{
  for(const bad of ['identity','duplicate','offset']){
    const {pool,workers,owner,request}=setup();const p=request(owner('a'));const rejected=assert.rejects(p,/protocol/);await tick();const w=workers[0],id=w.messages[0].identity;
    w.emit({...id,type:'result',width:1,height:1,length:4,decodeMs:1,worker:true,...(bad==='identity'?{source:'wrong'}:{})});
    if(bad==='duplicate')w.emit({...id,type:'result',width:1,height:1,length:4});
    if(bad==='offset')w.emit({...id,type:'chunk',offset:1,bytes:new ArrayBuffer(4)});
    await rejected;assert.equal(pool.snapshot().managed,0);
  }
});
test('aborted preparation keeps budget until its yielded copy releases custody',async()=>{
  let resume;const gate=new Promise(r=>resume=r);const {pool,owner,request}=setup({yieldTask:()=>gate});
  const abort=new AbortController(),p=request(owner('a','foreground',abort.signal));const rejected=assert.rejects(p,/closed/);
  abort.abort();await rejected;assert.equal(pool.snapshot().jobs,1);assert.ok(pool.snapshot().managed>0);
  resume();await tick();assert.equal(pool.snapshot().jobs,0);
});
test('eight accepted jobs and 32 MiB encoded admission cover queued jobs',async()=>{
  const {pool,owner,request}=setup();const a=owner('a');const pending=[];
  for(let i=0;i<8;i++)pending.push(assert.rejects(request(a),/closed/));
  await assert.rejects(request(a),/budget/);assert.equal(pool.snapshot().jobs,8);a.close();await Promise.all(pending);await tick();
  const b=owner('b');const bytes=new Uint8Array(16*1024*1024);const p=assert.rejects(request(b,{bytes}),/closed/),q=assert.rejects(request(b,{bytes}),/closed/);
  await assert.rejects(request(b),/budget/);b.close();await Promise.all([p,q]);await tick();assert.equal(pool.snapshot().encoded,0);
});
test('consumer abort or throw releases only after actual consumer completion',async()=>{
  const {pool,workers,owner,request}=setup();let release;const gate=new Promise(r=>release=r);const a=owner('a');
  const p=request(a,{},async()=>{await gate;throw Error('consumer');});const rejected=assert.rejects(p,/closed/);
  await tick();workers[0].reply();await tick();a.close();await rejected;assert.equal(pool.snapshot().jobs,1);
  release();await tick();assert.equal(pool.snapshot().jobs,0);
});
test('managed cap includes simultaneous full worker and page RGBA staging',async()=>{
  const {pool,workers,owner,request}=setup();let release;const gate=new Promise(r=>release=r);
  const a=owner('a'),b=owner('b');const p=request(a,{maxDimension:4096},async()=>gate);await tick();workers[0].reply();await tick();
  const q=request(b,{maxDimension:4096});b.close();
  try {await assert.rejects(q,/budget/);} finally {release();await p;await tick();}
  assert.equal(pool.snapshot().managed,0);
});
test('host authorization checked again before dispatch and delivery; reentrant close wins',async()=>{
  const {pool,workers}=setup();let permitted=true,calls=0;
  const a=pool.bindOwner({owner:'a',lifetime:'l',domain:'d',lane:'foreground',authorize:()=>{calls++;return permitted;}});
  const p=a.decode({bytes:new Uint8Array([1]),source:'s',maxDimension:1},async()=>assert.fail('unauthorized consumer'));
  const rejected=assert.rejects(p,/closed/);await tick();permitted=false;workers[0].reply();await rejected;assert.ok(calls>=3);
  let b;b=pool.bindOwner({owner:'b',lifetime:'l',domain:'d',lane:'foreground',authorize:()=>{b.close();return true;}});
  await assert.rejects(b.decode({bytes:new Uint8Array([1]),source:'s',maxDimension:1},async()=>{}),/closed/);assert.equal(pool.snapshot().jobs,0);
});
test('shared, detached and accessor-bearing requests are refused without spawn',async()=>{
  const {workers,owner,request}=setup();const a=owner('a');
  await assert.rejects(request(a,{bytes:new Uint8Array(new SharedArrayBuffer(4))}),/protocol/);
  const bytes=new Uint8Array(4);structuredClone(bytes,{transfer:[bytes.buffer]});await assert.rejects(request(a,{bytes}),/protocol/);
  const input={source:'ticket',maxDimension:1,get bytes(){throw Error('accessor invoked');}};
  await assert.rejects(a.decode(input,async()=>{}),/protocol/);assert.equal(workers.length,0);
});
test('abort during partial output discards staging and duplicate terminal messages do not resettle',async()=>{
  const {pool,workers,owner,request}=setup();const a=owner('a');const p=request(a,{maxDimension:512});const rejected=assert.rejects(p,/closed/);
  await tick();const w=workers[0],id=w.messages[0].identity,callback=w.listeners.get('message');
  w.emit({...id,type:'result',width:512,height:512,length:1048576,decodeMs:1,worker:true});
  w.emit({...id,type:'chunk',offset:0,bytes:new ArrayBuffer(256*1024)});assert.ok(pool.snapshot().managed>=1048576);
  a.close();await rejected;callback({data:{...id,type:'chunk',offset:256*1024,bytes:new ArrayBuffer(256*1024)}});
  assert.equal(pool.snapshot().managed,0);assert.equal(w.dead,true);
});
test('duplicate live canonical owner/lifetime binding cannot bypass one active lease',()=>{
  const {pool,owner}=setup();const a=owner('a');
  assert.throws(()=>owner('a','foreground'),/binding/);
  assert.throws(()=>pool.bindOwner({owner:'a',lifetime:'life-a',domain:'another',lane:'foreground',authorize:()=>true}),/binding/);
  a.close();assert.doesNotThrow(()=>owner('a'));
});
for(const change of ['close','revoke']) {
  test(`queued owner authorization cannot deliver after ${change}`,async()=>{
    const {pool,workers,request}=setup();let live=true,delivered=0;
    const a=pool.bindOwner({owner:'a',lifetime:'a1',domain:'images',lane:'background',authorize:()=>live});
    const b=pool.bindOwner({owner:'b',lifetime:'b1',domain:'images',lane:'background',authorize:()=>{if(change==='close')a.close();else live=false;return true;}});
    const p=request(a,{},async()=>{delivered++;});const refused=assert.rejects(p,/closed/);refused.catch(()=>{});
    const q=request(b);await tick();assert.equal(workers.length,1);workers[0].reply();
    await tick();workers[1].reply();await q;await refused;
    assert.equal(delivered,0,`${change} during handoff must precede delivery`);
    assert.deepEqual(pool.snapshot(),{jobs:0,leases:0,encoded:0,managed:0});a.close();b.close();
  });
}
test('retirement reentry holds custody until the delivery handoff unwinds',async()=>{
  const {pool,workers,owner,request}=setup();const a=owner('a');let held;
  const p=request(a,{},async()=>assert.fail('closed result delivered'));const refused=assert.rejects(p,/closed/);
  await tick();const w=workers[0],terminate=w.terminate.bind(w);
  w.terminate=()=>{terminate();a.close();held=pool.snapshot();};w.reply();await refused;
  assert.equal(held.jobs,1);assert.ok(held.managed>0);assert.equal(pool.snapshot().jobs,0);
});
