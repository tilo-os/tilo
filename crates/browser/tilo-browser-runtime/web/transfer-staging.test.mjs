import {test} from 'node:test';
import assert from 'node:assert/strict';
import {createInlineStorage,requestResult} from './storage-inline.mjs';
import {createStorageAccess} from './storage.mjs';
import {transactionComplete} from './host-core.mjs';
import {fakeIndexedDB,fakeLocks,fakeWorkers} from './testing/fake-storage.mjs';
const CHUNK=240*1024,MAX=256*1024*1024;
const descriptor=(n=1,totalBytes=4)=>({id:n.toString(16).padStart(32,'0'),scope:'verified scope',cutToken:'opaque cut',totalBytes,maxItemBytes:Math.min(totalBytes,64*1024*1024)});
const row=(revision='1',key='journal',bytes=[8])=>({key,revision,bytes:Uint8Array.from(bytes),actor:Uint8Array.of(7)});
const settle=async()=>{for(let i=0;i<10;i++)await new Promise(r=>setImmediate(r));};
async function fixture(t){const idb=fakeIndexedDB(),locks=fakeLocks(),storage=createInlineStorage({locks:locks.manager('page'),indexedDB:idb});const lease=await storage.acquireStorage('p','d');t.after(()=>lease.release());return {idb,locks,storage,lease};}
test('private stage has bounded ranges, exact duplicate bytes and is absent from both reader generations',async t=>{
 const {idb,lease}=await fixture(t);assert.ok(lease.transfers,'lease must expose private staging');
 const h=await lease.transfers.open(descriptor(1,CHUNK+4));const first=new Uint8Array(CHUNK).fill(3);
 await lease.transfers.write(h,0,first);await lease.transfers.write(h,0,first);
 await assert.rejects(lease.transfers.write(h,0,new Uint8Array(CHUNK).fill(4)),/conflict/i);
 await assert.rejects(lease.transfers.write(h,CHUNK+1,Uint8Array.of(1)),/offset|chunk/i);
 await lease.transfers.write(h,CHUNK,Uint8Array.of(4,5,6,7));
 assert.deepEqual(await lease.transfers.read(h,CHUNK-2,6),Uint8Array.of(3,3,4,5,6,7));
 await assert.rejects(lease.transfers.read(h,0,CHUNK+1),/range|bound/i);
 await lease.commit([row()]);
 assert.equal((await lease.hydrate()).length,1);
 const tx=lease.db.transaction('records','readonly'),done=transactionComplete(tx);
 const old=await requestResult(tx.objectStore('records').getAll());await done;
 assert.equal(old.filter(r=>r.namespace===lease.namespace).length,1);
 assert.equal(old.filter(r=>r.blob instanceof Blob).length,2);
 assert.equal((await lease.transfers.status(h)).nextOffset,CHUNK+4);
 assert.equal(idb.transactions().filter(tx=>tx.mode==='readwrite').every(tx=>tx.durability==='strict'),true);
});
test('final strict transaction atomically stores records and removes complete stage; abort retains both',async t=>{
 const {idb,lease}=await fixture(t);assert.ok(lease.transfers);
 await lease.commit([row('5')]);const h=await lease.transfers.open(descriptor());
 await assert.rejects(lease.commit([row('6')],{transfer:h}),/incomplete/i);
 await lease.transfers.write(h,0,Uint8Array.of(1,2,3,4));
 await assert.rejects(lease.commit([row('4')],{transfer:h}),/stale/i);
 assert.equal((await lease.hydrate())[0].revision,'5');assert.equal((await lease.transfers.status(h)).nextOffset,4);
 lease.abortNext=true;await assert.rejects(lease.commit([row('6')],{transfer:h}),/abort/i);
 assert.equal((await lease.hydrate())[0].revision,'5');assert.equal((await lease.transfers.status(h)).nextOffset,4);
 const gate=idb.gateNextCommit();let finished=false;const commit=lease.commit([row('6')],{transfer:h}).then(()=>{finished=true;});await gate.held;
 assert.equal(finished,false);assert.equal(idb.rows().find(r=>r.namespace===lease.namespace).revision,'5');assert.equal(idb.rows().filter(r=>r.blob).length,1);
 gate.release();await commit;assert.equal((await lease.hydrate())[0].revision,'6');
 await assert.rejects(lease.transfers.status(h),/missing|unknown/i);assert.equal(idb.rows().filter(r=>r.blob).length,0);
});
test('complete reservation is global across namespaces and crash reopen rebinds only exact descriptors',async t=>{
 const {idb,locks,storage,lease}=await fixture(t);assert.ok(lease.transfers);
 const h=await lease.transfers.open(descriptor(1,MAX));
 const other=await storage.acquireStorage('other','d');t.after(()=>other.release());
 await assert.rejects(other.transfers.open(descriptor(2)),e=>e.name==='QuotaExceededError');
 await lease.transfers.write(h,0,new Uint8Array(CHUNK).fill(9));await lease.release();
 const reopened=await createInlineStorage({locks:locks.manager('new-page'),indexedDB:idb}).acquireStorage('p','d');t.after(()=>reopened.release());
 await assert.rejects(reopened.transfers.status(h),/lease|epoch/i);
 await assert.rejects(reopened.transfers.open({...descriptor(1,MAX),cutToken:'changed'}),/conflict/i);
 const next=await reopened.transfers.open(descriptor(1,MAX));assert.notEqual(next.epoch,h.epoch);assert.equal(next.nextOffset,CHUNK);
 assert.deepEqual(await reopened.transfers.read(next,0,1),Uint8Array.of(9));await reopened.transfers.drop(next);
 await other.transfers.open(descriptor(2));
});
test('descriptor limits, two namespace reservations and immutable handles fail closed',async t=>{
 const {lease}=await fixture(t);assert.ok(lease.transfers);
 for(const change of [{totalBytes:MAX+1},{maxItemBytes:64*1024*1024+1},{scope:'a'.repeat(4097)},{id:'0'.repeat(32)}])await assert.rejects(lease.transfers.open({...descriptor(),...change}));
 const h=await lease.transfers.open(descriptor());await lease.transfers.open(descriptor(2));
 await assert.rejects(lease.transfers.open(descriptor(3)),e=>e.name==='QuotaExceededError');
 await assert.rejects(lease.transfers.write({...h,cutToken:'wrong'},0,Uint8Array.of(1,2,3,4)),/handle|scope|lease/i);
 await assert.rejects(lease.commit([],{transfer:h}),/empty/i);
});
test('inline FIFO joins raw writes before drop/release and bounds pending chunk copies',async t=>{
 const {idb,lease,locks}=await fixture(t);assert.ok(lease.transfers);
 const h=await lease.transfers.open(descriptor());const gate=idb.gateNextCommit();
 const bytes=Uint8Array.of(1,2,3,4);const writing=lease.transfers.write(h,0,bytes);bytes.fill(0);await gate.held;
 const reading=lease.transfers.read(h,0,4);await assert.rejects(lease.transfers.status(h),e=>e.name==='QuotaExceededError');
 let released=false;const release=lease.release().then(()=>{released=true;});await settle();assert.equal(released,false);assert.equal(locks.held().length,1);
 await assert.rejects(lease.transfers.drop(h),/closing|closed/i);gate.release();await writing;assert.deepEqual(await reading,Uint8Array.of(1,2,3,4));await release;
});
test('worker stage survives crash fallback in same epoch, final commit/drop and close remain ordered',async t=>{
 const idb=fakeIndexedDB(),locks=fakeLocks(),workers=fakeWorkers({locks,indexedDB:idb});
 const access=createStorageAccess({createWorker:workers.createWorker,inline:createInlineStorage({locks:locks.manager('page'),indexedDB:idb}),delay:()=>settle(),onFallback:()=>{}});
 const lease=await access.acquireStorage('p','d');t.after(async()=>{await lease.release();workers.dispose();});assert.ok(lease.transfers);
 const h=await lease.transfers.open(descriptor());await lease.transfers.write(h,0,Uint8Array.of(1,2,3,4));workers.workers[0].crash();
 assert.equal((await lease.transfers.status(h)).epoch,h.epoch);assert.deepEqual(await lease.transfers.read(h,0,4),Uint8Array.of(1,2,3,4));
 await lease.commit([row()],{transfer:h});assert.equal((await lease.hydrate()).length,1);
 const second=await lease.transfers.open(descriptor(2));const writing=lease.transfers.write(second,0,Uint8Array.of(5,6,7,8));const dropping=lease.transfers.drop(second);const closing=lease.release();await Promise.all([writing,dropping,closing]);assert.equal(idb.rows().filter(r=>r.blob).length,0);
});
test('reload discovers only bounded inert metadata, rejects old lease and rebinds exact descriptor',async t=>{
 const {lease,storage,idb}=await fixture(t);assert.equal(typeof lease.transfers.list,'function');
 const h=await lease.transfers.open(descriptor());await lease.transfers.write(h,0,Uint8Array.of(1,2,3,4));
 const other=await storage.acquireStorage('elsewhere','d');t.after(()=>other.release());await other.transfers.open(descriptor(2));
 await lease.release();const next=await storage.acquireStorage('p','d');t.after(()=>next.release());
 const before=idb.transactions().length,list=await next.transfers.list();assert.deepEqual(list,[{...h,nextOffset:4}]);
 assert.equal(idb.transactions().slice(before).flatMap(tx=>tx.reads).some(key=>key==='*'||(Array.isArray(key)&&key.length===4)),false,'discovery must never read chunks');
 await assert.rejects(next.transfers.read(h,0,1),/epoch|lease/i);
 const rebound=await next.transfers.open(list[0]);assert.notEqual(rebound.epoch,h.epoch);assert.deepEqual(await next.transfers.read(rebound,0,4),Uint8Array.of(1,2,3,4));
});
test('flush includes page-queued operations before worker dispatch',async t=>{
 const idb=fakeIndexedDB(),locks=fakeLocks(),workers=fakeWorkers({locks,indexedDB:idb});
 const access=createStorageAccess({createWorker:workers.createWorker,onFallback:()=>{}}),lease=await access.acquireStorage('p','d');t.after(async()=>{await lease.release();workers.dispose();});
 const gate=idb.gateNextCommit(),writing=lease.commit([row()]);let flushed=false;const flush=access.flush().then(()=>{flushed=true;});await gate.held;await settle();try{assert.equal(flushed,false,'flush must not overtake a page-queued write');}finally{gate.release();await Promise.all([writing,flush]);}
});
test('global count is 32 without eviction and namespace quota is independent of storage epochs',async t=>{
 const {storage,lease}=await fixture(t);const leases=[lease];
 for(let i=1;i<32;i++)leases.push(await storage.acquireStorage(`profile-${i}`,'d'));
 t.after(async()=>{for(const entry of leases)await entry.release();});
 const handles=[];for(const entry of leases)handles.push(await entry.transfers.open(descriptor()));
 const extra=await storage.acquireStorage('extra','d');t.after(()=>extra.release());
 await assert.rejects(extra.transfers.open(descriptor()),e=>e.name==='QuotaExceededError');
 assert.equal((await lease.transfers.status(handles[0])).nextOffset,0);
 await leases[10].transfers.drop(handles[10]);await extra.transfers.open(descriptor());
});
test('quota failure cannot advance a chunk prefix or evict an existing immutable chunk',async t=>{
 const {idb,lease}=await fixture(t),h=await lease.transfers.open(descriptor(1,CHUNK+2));
 await lease.transfers.write(h,0,new Uint8Array(CHUNK).fill(6));idb.failNextPut('QuotaExceededError');
 await assert.rejects(lease.transfers.write(h,CHUNK,Uint8Array.of(7,8)),e=>e.name==='QuotaExceededError');
 assert.equal((await lease.transfers.status(h)).nextOffset,CHUNK);assert.deepEqual(await lease.transfers.read(h,0,1),Uint8Array.of(6));
 await lease.transfers.write(h,CHUNK,Uint8Array.of(7,8));assert.equal((await lease.transfers.status(h)).nextOffset,CHUNK+2);
});
test('worker crash aborts in-flight chunk transaction and reopen exposes only durable prefix',async t=>{
 const idb=fakeIndexedDB(),locks=fakeLocks(),workers=fakeWorkers({locks,indexedDB:idb});
 const access=createStorageAccess({createWorker:workers.createWorker,inline:createInlineStorage({locks:locks.manager('page'),indexedDB:idb}),delay:()=>settle(),onFallback:()=>{}}),lease=await access.acquireStorage('p','d');
 t.after(async()=>{await lease.release();workers.dispose();});const h=await lease.transfers.open(descriptor());
 const gate=idb.gateNextCommit(),write=lease.transfers.write(h,0,Uint8Array.of(1,2,3,4));const failure=assert.rejects(write,e=>e.name==='StorageWorkerError');await gate.held;workers.workers[0].crash();gate.release();await failure;
 const list=await lease.transfers.list();assert.equal(list[0].nextOffset,0);assert.equal(list[0].epoch,h.epoch);
 await assert.rejects(lease.transfers.read(h,0,1),/incomplete/i);
 await lease.transfers.write(h,0,Uint8Array.of(1,2,3,4));assert.deepEqual(await lease.transfers.read(h,0,4),Uint8Array.of(1,2,3,4));
});
test('both reader generations leave private Blob contents unread and canonical hydration skips foreign rows',async t=>{
 const {lease,storage,idb}=await fixture(t),h=await lease.transfers.open(descriptor());await lease.transfers.write(h,0,Uint8Array.of(1,2,3,4));
 const other=await storage.acquireStorage('a foreign prefix','d');t.after(()=>other.release());await other.commit([row()]);await lease.commit([row()]);
 const original=Blob.prototype.arrayBuffer;let reads=0;Blob.prototype.arrayBuffer=function(){reads++;return original.call(this);};
 try{
  const before=idb.transactions().length;assert.equal((await lease.hydrate()).length,1);
  assert.deepEqual(idb.transactions().slice(before).flatMap(tx=>tx.reads),[JSON.stringify([lease.namespace,'journal'])]);
  const tx=lease.db.transaction('records','readonly'),done=transactionComplete(tx),rows=await requestResult(tx.objectStore('records').getAll());await done;
  assert.equal(rows.filter(r=>r.namespace===lease.namespace).length,1);assert.equal(reads,0,'old getAll clones immutable Blob handles, never requests their bytes');
 }finally{Blob.prototype.arrayBuffer=original;}
});
test('page facade validates closed bounded metadata before clone and preserves inputs rejected by queue pressure',async t=>{
 const idb=fakeIndexedDB(),locks=fakeLocks(),access=createStorageAccess({createWorker:()=>null,inline:createInlineStorage({locks:locks.manager('page'),indexedDB:idb})}),lease=await access.acquireStorage('p','d');t.after(()=>lease.release());
 await assert.rejects(lease.transfers.open({...descriptor(),scope:'x'.repeat(4097),uncloneable:()=>{}}),e=>e.name==='InvalidStateError');
 const h=await lease.transfers.open(descriptor()),gate=idb.gateNextCommit();const a=lease.transfers.write(h,0,Uint8Array.of(1,2,3,4));await gate.held;
 const b=lease.transfers.status(h),bytes=Uint8Array.of(1,2,3,4);try{await assert.rejects(lease.transfers.write(h,0,bytes),e=>e.name==='QuotaExceededError');assert.equal(bytes.byteLength,4);}finally{gate.release();await Promise.all([a,b]);}
});
test('wire-legal stream whose headers exceed actual 256MiB reservation reports storage pressure',async t=>{
 const {lease}=await fixture(t);await assert.rejects(lease.transfers.open({...descriptor(1,MAX),totalBytes:MAX+45}),e=>e.name==='QuotaExceededError');
 assert.deepEqual(await lease.transfers.list(),[]);
});
test('concurrent namespaces reserve complete streams atomically and never exceed shared byte budget',async t=>{
 const {lease,storage}=await fixture(t),other=await storage.acquireStorage('other','d');t.after(()=>other.release());
 const size=140*1024*1024,results=await Promise.allSettled([lease.transfers.open(descriptor(1,size)),other.transfers.open(descriptor(2,size))]);
 assert.equal(results.filter(r=>r.status==='fulfilled').length,1);assert.equal(results.find(r=>r.status==='rejected').reason.name,'QuotaExceededError');
 const entries=[...await lease.transfers.list(),...await other.transfers.list()];assert.equal(entries.length,1);assert.equal(entries[0].totalBytes,size);
});
test('a stale dirty record aborts the entire final cursor/journal commit and preserves imported actor bytes',async t=>{
 const {lease}=await fixture(t);await lease.commit([row('5','dirty',[9]),row('2','journal',[1])]);
 const h=await lease.transfers.open(descriptor());await lease.transfers.write(h,0,Uint8Array.of(1,2,3,4));
 await assert.rejects(lease.commit([row('3','journal',[2]),row('4','dirty',[0])],{transfer:h}),/stale/i);
 const records=await lease.hydrate();assert.deepEqual(records.map(r=>[r.key,r.revision,[...r.bytes],[...r.actor]]),[['dirty','5',[9],[7]],['journal','2',[1],[7]]]);assert.equal((await lease.transfers.status(h)).nextOffset,4);
});
test('active worker drains write then drop before releasing the exclusive storage lease',async t=>{
 const idb=fakeIndexedDB(),locks=fakeLocks(),workers=fakeWorkers({locks,indexedDB:idb}),access=createStorageAccess({createWorker:workers.createWorker,onFallback:()=>{}}),lease=await access.acquireStorage('p','d');
 t.after(async()=>{await lease.release();workers.dispose();});const h=await lease.transfers.open(descriptor()),gate=idb.gateNextCommit();
 const writing=lease.transfers.write(h,0,Uint8Array.of(1,2,3,4));await gate.held;const dropping=lease.transfers.drop(h);let closed=false;const close=lease.release().then(()=>{closed=true;});await settle();
 try{assert.equal(closed,false);assert.equal(locks.held().length,1);await assert.rejects(lease.transfers.status(h),/closing/i);}finally{gate.release();await Promise.all([writing,dropping,close]);}
 assert.equal(idb.rows().filter(r=>r.blob).length,0);assert.equal(locks.held().length,0);
});
test('legacy empty commit remains a no-op and does not consume the next transaction fault',async t=>{
 const {idb,lease}=await fixture(t);lease.abortNext=true;await lease.commit([]);assert.equal(lease.abortNext,true);assert.equal(idb.transactions().length,0);
 await assert.rejects(lease.commit([row()]),/abort/i);assert.deepEqual(await lease.hydrate(),[]);
});

test('resume CAS is atomic and preserves immutable handles, chunks and final cleanup',async t=>{
 const {lease,idb}=await fixture(t),h=await lease.transfers.open(descriptor());
 await lease.transfers.write(h,0,Uint8Array.of(1,2,3,4));
 assert.equal(h.resumeDigest,h.cutToken);
 assert.equal(typeof lease.transfers.compareAndSwapResumeDigest,'function');
 const before=idb.transactions().length;
 const outcomes=await Promise.allSettled([
  lease.transfers.compareAndSwapResumeDigest(h,h.cutToken,'renewed-A'),
  lease.transfers.compareAndSwapResumeDigest(h,h.cutToken,'renewed-B'),
 ]);
 assert.equal(outcomes.filter(r=>r.status==='fulfilled').length,1);
 assert.match(outcomes.find(r=>r.status==='rejected').reason.message,/digest|compare/i);
 assert.equal((await lease.transfers.status(h)).resumeDigest,'renewed-A');
 assert.equal(idb.transactions().slice(before).flatMap(tx=>tx.reads).some(k=>Array.isArray(k)&&k.length===4),false,'CAS must not load chunk bytes');
 assert.deepEqual(await lease.transfers.read(h,0,4),Uint8Array.of(1,2,3,4));
 assert.equal((await lease.transfers.status(h)).cutToken,h.cutToken);
 await lease.commit([row()],{transfer:h});assert.deepEqual(await lease.transfers.list(),[]);assert.equal(idb.rows().filter(r=>r.blob).length,0);
});
test('resume metadata reloads, old entries default to original token and stale leases cannot CAS',async t=>{
 const {lease,storage}=await fixture(t),h=await lease.transfers.open(descriptor());
 await lease.transfers.write(h,0,Uint8Array.of(1,2,3,4));
 // Persist the literal previous registry shape, without any migration rewrite.
 const key=['tilo-transfer-stage-v1','registry'];
 const tx=lease.db.transaction('records','readwrite'),done=transactionComplete(tx),store=tx.objectStore('records'),get=store.get(key);
 get.onsuccess=()=>{const old=get.result;delete old.entries[0].resumeDigest;store.put(old);};await done;
 assert.equal((await lease.transfers.list())[0].resumeDigest,h.cutToken);
 await lease.transfers.compareAndSwapResumeDigest(h,h.cutToken,'renewed-A');await lease.release();
 const next=await storage.acquireStorage('p','d');t.after(()=>next.release());
 const discovered=(await next.transfers.list())[0];assert.equal(discovered.resumeDigest,'renewed-A');
 await assert.rejects(next.transfers.compareAndSwapResumeDigest(h,'renewed-A','stale'),/epoch|lease/i);
 const rebound=await next.transfers.open(discovered);assert.equal(rebound.resumeDigest,'renewed-A');assert.notEqual(rebound.epoch,h.epoch);
 await assert.rejects(next.transfers.compareAndSwapResumeDigest(rebound,h.cutToken,'stale'),/digest|compare/i);
 await next.transfers.compareAndSwapResumeDigest(rebound,'renewed-A','renewed-B');
 await next.transfers.compareAndSwapResumeDigest(rebound,'renewed-B','renewed-C');
 assert.equal((await next.transfers.list())[0].resumeDigest,'renewed-C');assert.equal(rebound.cutToken,h.cutToken);
 assert.deepEqual(await next.transfers.read(rebound,0,4),Uint8Array.of(1,2,3,4));
});
for(const worker of [false,true])test(`resume CAS ${worker?'worker':'direct'} uses bounded FIFO metadata and preserves failed transaction`,{timeout:3000},async t=>{
 const idb=fakeIndexedDB(),locks=fakeLocks(),workers=fakeWorkers({locks,indexedDB:idb});
 const access=createStorageAccess({createWorker:worker?workers.createWorker:()=>null,inline:createInlineStorage({locks:locks.manager('page'),indexedDB:idb}),onFallback:()=>{}}),lease=await access.acquireStorage('p','d');
 t.after(async()=>{await lease.release();workers.dispose();});const h=await lease.transfers.open(descriptor());
 assert.equal(typeof lease.transfers.compareAndSwapResumeDigest,'function');
 await assert.rejects(lease.transfers.compareAndSwapResumeDigest(h,h.cutToken,'x'.repeat(4097)),e=>e.name==='InvalidStateError');
 idb.failNextPut('QuotaExceededError');await assert.rejects(lease.transfers.compareAndSwapResumeDigest(h,h.cutToken,'failed'),e=>e.name==='QuotaExceededError');
 assert.equal((await lease.transfers.status(h)).resumeDigest,h.cutToken);
 const gate=idb.gateNextCommit(),changing=lease.transfers.compareAndSwapResumeDigest(h,h.cutToken,'renewed-A');await gate.held;
 let released=false;const closing=lease.release().then(()=>{released=true;});await settle();try{assert.equal(released,false);assert.equal(locks.held().length,1);}finally{gate.release();await changing;await closing;}
 const next=await access.acquireStorage('p','d');t.after(()=>next.release());const current=(await next.transfers.list())[0];assert.equal(current.resumeDigest,'renewed-A');await next.release();
});
test('resume CAS validates opaque byte bounds and open cannot replace mutable resume metadata',async t=>{
 const {lease,idb}=await fixture(t),h=await lease.transfers.open({...descriptor(),resumeDigest:'untrusted override'});
 assert.equal(h.resumeDigest,h.cutToken);const before=idb.transactions().length;
 for(const invalid of ['', 'x'.repeat(4097), '\ud800', 'é'.repeat(2049)]){
  await assert.rejects(lease.transfers.compareAndSwapResumeDigest(h,invalid,'next'),e=>e.name==='InvalidStateError');
  await assert.rejects(lease.transfers.compareAndSwapResumeDigest(h,h.cutToken,invalid),e=>e.name==='InvalidStateError');
 }
 assert.equal(idb.transactions().length,before,'invalid metadata must be rejected before a transaction');
 const maximum='é'.repeat(2048);await lease.transfers.compareAndSwapResumeDigest(h,h.cutToken,maximum);
 assert.equal((await lease.transfers.open({...descriptor(),resumeDigest:'replace'})).resumeDigest,maximum);
});
test('worker crash aborts resume CAS and original handle still commits after fallback renewal', {timeout:3000},async t=>{
 const idb=fakeIndexedDB(),locks=fakeLocks(),workers=fakeWorkers({locks,indexedDB:idb});
 const access=createStorageAccess({createWorker:workers.createWorker,inline:createInlineStorage({locks:locks.manager('page'),indexedDB:idb}),delay:()=>settle(),onFallback:()=>{}}),lease=await access.acquireStorage('p','d');
 t.after(async()=>{await lease.release();workers.dispose();});const h=await lease.transfers.open(descriptor());await lease.transfers.write(h,0,Uint8Array.of(1,2,3,4));
 const gate=idb.gateNextCommit(),change=lease.transfers.compareAndSwapResumeDigest(h,h.cutToken,'uncommitted');
 const failure=assert.rejects(change,e=>e.name==='StorageWorkerError');await gate.held;workers.workers[0].crash();gate.release();await failure;
 const current=(await lease.transfers.list())[0];assert.equal(current.resumeDigest,h.cutToken);assert.equal(current.nextOffset,4);assert.equal(current.epoch,h.epoch);
 await lease.transfers.compareAndSwapResumeDigest(h,h.cutToken,'durable renewal');
 await lease.commit([row()],{transfer:h});assert.deepEqual(await lease.transfers.list(),[]);assert.equal(idb.rows().filter(r=>r.blob).length,0);assert.equal((await lease.hydrate()).length,1);
});
