import {test} from 'node:test';
import assert from 'node:assert/strict';
import {createStorageAccess} from './storage.mjs';
import {createInlineStorage} from './storage-inline.mjs';
import {fakeIndexedDB,fakeLocks,fakeWorkers,postedTypes} from './testing/fake-storage.mjs';

const settle=async(rounds=20)=>{for(let i=0;i<rounds;i++)await new Promise(resolve=>setImmediate(resolve));};
const write=(key,revision,bytes=[1,2,3],actor=null)=>({key,revision,bytes:new Uint8Array(bytes),actor:actor&&new Uint8Array(actor)});
const WRITER='tilo-writer:["p","d"]';
/** One page with its persistence worker over a shared origin (locks + IndexedDB). */
function harness(t,{idb=fakeIndexedDB(),locks=fakeLocks(),prefix='worker'}={}){
  const workers=fakeWorkers({locks,indexedDB:idb,prefix}),events=new EventTarget(),fallbacks=[];
  const access=createStorageAccess({createWorker:workers.createWorker,inline:createInlineStorage({locks:locks.manager('page'),indexedDB:idb}),events,delay:()=>settle(1),lockRetryMs:0,onFallback:error=>fallbacks.push(error)});
  t.after(()=>workers.dispose());
  return {idb,locks,workers,access,events,fallbacks,worker:()=>workers.workers[0]};
}

test('in-page lease: strict transactions, same-epoch revision guard, and a fresh epoch after release',async()=>{
  const idb=fakeIndexedDB(),locks=fakeLocks();
  const storage=createInlineStorage({locks:locks.manager('page'),indexedDB:idb});
  const lease=await storage.acquireStorage('p','d');
  assert.deepEqual(locks.held(),[WRITER]);assert.equal(lease.fresh,true);
  await lease.commit([write('k','5',[1])]);
  assert.deepEqual(idb.transactions().map(tx=>[tx.mode,tx.durability]),[['readwrite','strict']]);
  await lease.commit([write('k','4',[2])]);
  assert.deepEqual(idb.rows().map(row=>[row.revision,[...row.bytes],row.epoch]),[['5',[1],lease.epoch]],'an older same-epoch revision never overwrites');
  await assert.rejects(createInlineStorage({locks:locks.manager('other'),indexedDB:idb}).acquireStorage('p','d'),/already open in another browser tab/);
  await lease.release();assert.deepEqual(locks.held(),[]);
  const next=await storage.acquireStorage('p','d');
  assert.notEqual(next.epoch,lease.epoch);
  await next.commit([write('k','1',[3])]);
  assert.deepEqual(idb.rows().map(row=>[row.revision,[...row.bytes],row.epoch]),[['1',[3],next.epoch]],'a foreign epoch is replaced regardless of revision');
  await next.release();
});

test('in-page lease: the abortNext hook aborts one strict transaction and the commit rejects',async()=>{
  const idb=fakeIndexedDB(),locks=fakeLocks();
  const lease=await createInlineStorage({locks:locks.manager('page'),indexedDB:idb}).acquireStorage('p','d');
  lease.abortNext=true;
  await assert.rejects(lease.commit([write('k','1')]),/aborted/);
  assert.equal(idb.rows().length,0);assert.equal(lease.abortNext,false);
  await lease.commit([write('k','2')]);assert.equal(idb.rows().length,1);
  await lease.release();
});

test('worker protocol: open, commit with transferred bytes, hydrate, release',async t=>{
  const h=harness(t);
  const lease=await h.access.acquireStorage('p','d');const worker=h.worker();
  assert.equal(h.access.mode(),'worker');assert.equal(lease.namespace,'["p","d"]');assert.match(lease.epoch,/^[0-9a-f-]{36}$/);
  assert.equal(h.locks.holder(WRITER),worker.owner,'the worker holds the writer lock');
  const row=write('{"type":"record"}','9007199254740993',[0,128,255],[7]);
  await lease.commit([row]);
  assert.equal(row.bytes.byteLength,0,'page bytes moved to the worker without a copy');assert.equal(row.actor.byteLength,0);
  assert.deepEqual(worker.posted,[
    {id:1,type:'open',profile:'p',device:'d'},
    {id:2,type:'commit',namespace:'["p","d"]',abortNext:false,writes:[{key:'{"type":"record"}',revision:'9007199254740993',bytes:new Uint8Array([0,128,255]),actor:new Uint8Array([7])}]},
  ]);
  assert.deepEqual(worker.replies.map(reply=>[reply.id,reply.ok,reply.result]),[[1,true,{namespace:'["p","d"]',epoch:lease.epoch}],[2,true,undefined]]);
  const [stored]=h.idb.rows();
  assert.equal(stored.epoch,lease.epoch);assert.deepEqual([...stored.bytes],[0,128,255]);assert.deepEqual([...stored.actor],[7]);
  assert.equal(stored.id,JSON.stringify(['["p","d"]','{"type":"record"}']));
  assert.deepEqual(h.idb.transactions().map(tx=>[tx.owner,tx.mode,tx.durability]),[[worker.owner,'readwrite','strict']]);
  const rows=await lease.hydrate();
  assert.equal(rows.length,1);assert(rows[0].bytes instanceof Uint8Array);assert.deepEqual([...rows[0].bytes],[0,128,255]);
  await lease.release();
  assert.deepEqual(postedTypes(worker),['open','commit','hydrate','release']);
  assert.deepEqual(h.locks.held(),[]);
});

test('a commit resolves only once the worker confirms the strict transaction completed',async t=>{
  const h=harness(t);const lease=await h.access.acquireStorage('p','d');
  const gate=h.idb.gateNextCommit();let resolved=false;
  const commit=lease.commit([write('k','1')]).then(()=>{resolved=true;});
  await gate.held;await settle();
  assert.equal(resolved,false);assert.equal(h.idb.rows().length,0);
  gate.release();await commit;
  assert.equal(h.idb.rows().length,1);
  await lease.release();
});

test('the same-epoch revision guard and revision validation run in the worker',async t=>{
  const h=harness(t);const lease=await h.access.acquireStorage('p','d');
  await lease.commit([write('k','5',[5])]);await lease.commit([write('k','4',[4])]);
  assert.deepEqual(h.idb.rows().map(row=>[row.revision,[...row.bytes]]),[['5',[5]]]);
  await assert.rejects(lease.commit([write('k','01')]),/Invalid storage revision/);
  assert.equal(h.access.mode(),'worker');
  await lease.release();
});

test('failNextWrite aborts the next strict transaction inside the worker',async t=>{
  const h=harness(t);const lease=await h.access.acquireStorage('p','d');
  lease.abortNext=true;
  await assert.rejects(lease.commit([write('k','1')]),/aborted/);
  assert.equal(h.idb.rows().length,0);assert.equal(lease.abortNext,false);
  await lease.commit([write('k','2')]);assert.equal(h.idb.rows().length,1);
  await lease.release();
});

test('quota and security errors keep their codes through the worker',async t=>{
  const h=harness(t);const lease=await h.access.acquireStorage('p','d');
  for(const [name,code] of [['QuotaExceededError','storage-quota'],['SecurityError','storage-denied']]){
    h.idb.failNextPut(name);
    await assert.rejects(lease.commit([write('k','1')]),error=>error.name===name&&/simulated/.test(error.message)&&(error.code===undefined||error.code===code));
  }
  assert.equal(h.access.mode(),'worker','transaction errors are not worker failures');
  await lease.release();
});

test('a worker crash fails the in-flight commit as storage-transaction-failed and the session continues in-page with the same epoch',async t=>{
  const h=harness(t);const lease=await h.access.acquireStorage('p','d');const worker=h.worker();
  const gate=h.idb.gateNextCommit();
  const run=lease.commit([write('k','1')]);
  await gate.held;worker.crash();
  await assert.rejects(run,error=>error.name==='StorageWorkerError');
  assert.equal(worker.terminated,true);assert.equal(h.fallbacks.length,1);assert.equal(h.access.mode(),'inline');
  assert.equal(h.idb.rows().length,0,'the interrupted transaction did not commit');
  await lease.commit([write('k','2',[9])]);
  assert.equal(h.locks.holder(WRITER),'page','the page re-acquired the writer lock');
  assert.equal(h.idb.transactions().at(-1).owner,'page');
  assert.deepEqual(h.idb.rows().map(row=>[row.revision,row.epoch]),[['2',lease.epoch]]);
  const other=await h.access.acquireStorage('p','d2');
  assert.equal(h.workers.workers.length,1,'no second worker for the rest of the session');
  await other.commit([write('k','1')]);
  assert.deepEqual((await lease.hydrate()).map(row=>row.revision),['2']);
  await other.release();await lease.release();
  assert.deepEqual(h.locks.held(),[]);
});

test('without Worker, without Web Locks in the worker, or with a worker that fails to load, leases run in-page',async t=>{
  {
    const idb=fakeIndexedDB(),locks=fakeLocks();
    const access=createStorageAccess({createWorker:()=>null,inline:createInlineStorage({locks:locks.manager('page'),indexedDB:idb})});
    const lease=await access.acquireStorage('p','d');
    assert.equal(access.mode(),'inline');await lease.commit([write('k','1')]);
    assert.equal(idb.transactions()[0].owner,'page');await lease.release();
  }
  {
    const idb=fakeIndexedDB(),locks=fakeLocks(),workers=fakeWorkers({locks,indexedDB:idb,workerLocks:false}),fallbacks=[];
    t.after(()=>workers.dispose());
    const access=createStorageAccess({createWorker:workers.createWorker,inline:createInlineStorage({locks:locks.manager('page'),indexedDB:idb}),onFallback:error=>fallbacks.push(error)});
    const lease=await access.acquireStorage('p','d');
    assert.equal(fallbacks[0]?.name,'StorageUnsupportedError');assert.equal(workers.workers[0].terminated,true);
    assert.equal(locks.holder(WRITER),'page');await lease.release();
  }
  {
    const idb=fakeIndexedDB(),locks=fakeLocks(),fallbacks=[];
    const createWorker=()=>{
      const worker=new EventTarget();
      worker.postMessage=()=>{queueMicrotask(()=>worker.dispatchEvent(Object.assign(new Event('error'),{message:'SyntaxError: import declarations may only appear at top level of a module'})));};
      worker.terminate=()=>{worker.terminated=true;};return worker;
    };
    const access=createStorageAccess({createWorker,inline:createInlineStorage({locks:locks.manager('page'),indexedDB:idb}),onFallback:error=>fallbacks.push(error)});
    const lease=await access.acquireStorage('p','d');
    assert.match(fallbacks[0]?.message,/import declarations/);assert.equal(access.mode(),'inline');
    await lease.commit([write('k','1')]);assert.equal(idb.rows().length,1);await lease.release();
  }
});

test('two tabs contend for the writer lock through their workers',async t=>{
  const idb=fakeIndexedDB(),locks=fakeLocks();
  const a=harness(t,{idb,locks,prefix:'tab-a'}),b=harness(t,{idb,locks,prefix:'tab-b'});
  const first=await a.access.acquireStorage('p','d');
  await assert.rejects(b.access.acquireStorage('p','d'),/already open in another browser tab/);
  assert.equal(b.access.mode(),'worker','contention is not a worker failure');assert.equal(b.fallbacks.length,0);
  await first.commit([write('k','1',[1])]);await first.release();
  const second=await b.access.acquireStorage('p','d');
  assert.notEqual(second.epoch,first.epoch);assert.equal(locks.holder(WRITER),b.worker().owner);
  assert.deepEqual((await second.hydrate()).map(row=>[...row.bytes]),[[1]]);
  await second.release();
});

test('hydrate through the worker returns only this namespace as byte arrays',async t=>{
  const h=harness(t);
  const mine=await h.access.acquireStorage('p','d'),other=await h.access.acquireStorage('p','e');
  await mine.commit([write('a','1',[1]),write('b','2',[2],[3])]);await other.commit([write('a','1',[9])]);
  const rows=await mine.hydrate();
  assert.deepEqual(rows.map(row=>[row.key,row.revision,[...row.bytes],row.actor&&[...row.actor],row.namespace]).sort(),[['a','1',[1],null,'["p","d"]'],['b','2',[2],[3],'["p","d"]']]);
  assert(rows.every(row=>row.bytes instanceof Uint8Array&&row.epoch===mine.epoch));
  await mine.release();await other.release();
});

test('one worker queue per namespace orders peer batches, private-action writes and close writes',async t=>{
  const h=harness(t);const lease=await h.access.acquireStorage('p','d');
  const gate=h.idb.gateNextCommit(),order=[];
  const peer=lease.commit([write('{"type":"record"}','1',[1])]).then(()=>order.push('peer'));
  await gate.held;
  const action=lease.commit([write('{"type":"privateActions"}','1',[2])]).then(()=>order.push('action'));
  const close=lease.commit([{key:'{"type":"record"}',revision:2,bytes:[3],actor:null}]).then(()=>order.push('close'));
  await settle();
  assert.deepEqual(order,[]);assert.equal(h.idb.transactions().length,1,'later writes wait behind the in-flight strict transaction');
  gate.release();await Promise.all([peer,action,close]);
  assert.deepEqual(order,['peer','action','close']);
  assert.deepEqual(h.idb.transactions().map(tx=>tx.outcome),['complete','complete','complete']);
  assert.deepEqual(h.idb.rows().map(row=>[row.key,String(row.revision),[...row.bytes]]).sort(),[['{"type":"privateActions"}','1',[2]],['{"type":"record"}','2',[3]]]);
  await lease.release();
});

test('pagehide posts a final flush that settles after the queued commits',async t=>{
  const h=harness(t);const lease=await h.access.acquireStorage('p','d');const worker=h.worker();
  const gate=h.idb.gateNextCommit();const commit=lease.commit([write('k','1')]);await gate.held;
  h.events.dispatchEvent(new Event('pagehide'));await settle();
  assert.deepEqual(postedTypes(worker),['open','commit','flush']);
  assert.equal(worker.replies.length,1,'the flush answers only after the queued commit');
  gate.release();await commit;await settle();
  assert.deepEqual(worker.replies.map(reply=>reply.id),[1,2,3]);
  await lease.release();
});

test('shared references open one worker lease and re-acquisition waits for its release',async t=>{
  const h=harness(t);
  const [a,b]=await Promise.all([h.access.acquireStorage('p','d'),h.access.acquireStorage('p','d')]);
  assert.equal(a,b);assert.equal(a.references,2);assert.deepEqual(postedTypes(h.worker()),['open']);
  await a.release();assert.deepEqual(postedTypes(h.worker()),['open']);
  const closing=b.release(),again=h.access.acquireStorage('p','d');
  const next=await again;await closing;
  assert.notEqual(next,a);assert.notEqual(next.epoch,a.epoch);
  assert.deepEqual(postedTypes(h.worker()),['open','release','open']);
  await next.release();assert.deepEqual(h.locks.held(),[]);
});

test('a lease already releasing when the worker dies is not re-acquired in-page',async t=>{
  const h=harness(t);
  const lease=await h.access.acquireStorage('p','d'),kept=await h.access.acquireStorage('p','e');
  const gate=h.idb.gateNextCommit();
  const commit=lease.commit([write('k','1')]);await gate.held;
  const releasing=lease.release();await settle();
  h.worker().crash();
  await assert.rejects(commit,error=>error.name==='StorageWorkerError');
  await releasing;await settle();
  assert.equal(h.locks.holder(WRITER),undefined,'the released namespace stays unlocked');
  assert.equal(h.locks.holder('tilo-writer:["p","e"]'),'page','the live namespace moved in-page');
  const again=await h.access.acquireStorage('p','d');assert.notEqual(again.epoch,lease.epoch);
  await again.release();await kept.release();assert.deepEqual(h.locks.held(),[]);
});
