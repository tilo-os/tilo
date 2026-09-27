import test from 'node:test';
import assert from 'node:assert/strict';
import {createActionStorage} from './action-storage.mjs';
import {createStorageAccess} from './storage.mjs';
import {createInlineStorage} from './storage-inline.mjs';
import {fakeIndexedDB,fakeLocks,fakeWorkers} from './testing/fake-storage.mjs';
const settle=async()=>{for(let i=0;i<10;i++)await new Promise(r=>setImmediate(r));};
const value=n=>({version:1,entries:[{number:n}]});
function fixture(t){const idb=fakeIndexedDB(),locks=fakeLocks(),workers=fakeWorkers({locks,indexedDB:idb});t.after(()=>workers.dispose());const access=createStorageAccess({createWorker:workers.createWorker,inline:createInlineStorage({locks:locks.manager('page'),indexedDB:idb}),delay:settle,onFallback:()=>{}});return{idb,workers,access};}
test('explicit worker transaction abort keeps confirmed bytes and permits a later write',async t=>{
 const f=fixture(t),lease=await f.access.acquireStorage('p','d'),owner=createActionStorage({storage:lease,key:'private'});await owner.load();await owner.commit(value(1));lease.abortNext=true;
 await assert.rejects(owner.commit(value(2)),error=>error.name!=='StorageWorkerError');await assert.rejects(owner.reconcile(),/No uncertain/);
 await owner.commit(value(3));const row=(await lease.hydrate())[0];assert.equal(row.revision,'2');assert.deepEqual(JSON.parse(new TextDecoder().decode(row.bytes)),value(3));await lease.release();
});
test('worker death before commit reconciles exact prior bytes and reuses the uncommitted revision',async t=>{
 const f=fixture(t),lease=await f.access.acquireStorage('p','d'),owner=createActionStorage({storage:lease,key:'private'});await owner.load();await owner.commit(value(1));
 const gate=f.idb.gateNextCommit();const write=owner.commit(value(2));const rejected=assert.rejects(write,error=>error.name==='StorageWorkerError');await gate.held;f.workers.workers[0].crash();await rejected;
 await assert.rejects(owner.commit(value(3)),/reconciliation/);assert.equal(await owner.reconcile(),'aborted');await owner.commit(value(3));assert.equal((await lease.hydrate())[0].revision,'2');await lease.release();
});
test('unexpected same-lease bytes keep reconciliation latched without overwriting',async t=>{
 const f=fixture(t),lease=await f.access.acquireStorage('p','d'),owner=createActionStorage({storage:lease,key:'private'});await owner.load();await owner.commit(value(1));
 const gate=f.idb.gateNextCommit(),write=owner.commit(value(2)),rejected=assert.rejects(write,error=>error.name==='StorageWorkerError');await gate.held;f.workers.workers[0].crash();await rejected;
 await lease.commit([{key:'private',revision:'2',bytes:new TextEncoder().encode(JSON.stringify(value(99))),actor:null}]);
 await assert.rejects(owner.reconcile(),/differs/);await assert.rejects(owner.commit(value(3)),/reconciliation/);assert.deepEqual(JSON.parse(new TextDecoder().decode((await lease.hydrate())[0].bytes)),value(99));await lease.release();
});

test('an initially absent row is a prior image, but absence after confirmed bytes is a conflict',async t=>{
 for(const prior of [false,true]){
  const f=fixture(t),lease=await f.access.acquireStorage('p',String(prior));let hide=false;
  const owner=createActionStorage({storage:{epoch:lease.epoch,hydrate:()=>hide?Promise.resolve([]):lease.hydrate(),commit:writes=>lease.commit(writes)},key:'private'});
  assert.equal(await owner.load(),undefined);if(prior)await owner.commit(value(1));
  const gate=f.idb.gateNextCommit(),write=owner.commit(value(2)),rejected=assert.rejects(write,error=>error.name==='StorageWorkerError');await gate.held;f.workers.workers[0].crash();await rejected;
  if(prior){hide=true;await assert.rejects(owner.reconcile(),/differs/);await assert.rejects(owner.commit(value(3)),/reconciliation/);}
  else assert.equal(await owner.reconcile(),'aborted');
  await lease.release();
 }
});

for(const failure of ['definitive abort','worker loss reconciled aborted'])test(`same-ID cancellation survives ${failure} with another raw admission queued`,async t=>{
 const {createActionClient}=await import('./action-client.mjs');
 let client,lease,release;
 t.after(async()=>{release?.();await client?.close();await lease?.release();});
 const f=fixture(t);lease=await f.access.acquireStorage('p',failure);
 const host='aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa',invocation='bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb';
 const binding={room:'room',sender:'device',senderRoot:'person',target:'native',targetRoot:'root',certificate:'exact'};
 const route={app_peer:'7',app_id:'writer',record_id:'r',resource:'document',action:'insert',revision:'v1'};
 const catalogue={version:1,surfaces:[{peer_id:'7',app_id:'writer',record_id:'r',actions:[{resource:'document',action:'insert',revision:'v1',contract:{confirmation:{mode:'none'}}}]}]};
 let entered;const held=new Promise(r=>release=r),waiting=new Promise(r=>entered=r);
 const persistence=createActionStorage({storage:lease,key:'private',beforeCommit:async()=>{entered();await held;}}),sent=[];
 client=await createActionClient({...persistence,context:()=>binding,request:async m=>{sent.push(m);return m.type==='catalogue_refresh'?{type:'broker_catalogue',host,catalogue}:{type:'observation',host,invocation:m.invocation,update:null,uncertain:false,cancellation_requested:false};}});
 if(failure==='definitive abort')lease.abortNext=true;
 const gate=failure==='worker loss reconciled aborted'?f.idb.gateNextCommit():null;
 const first=client.invoke({...route,invocation});const firstRejected=assert.rejects(first,failure==='definitive abort'?/aborted/:/worker failed/);await waiting;
 const abort=new AbortController(),second=client.invoke({...route,invocation},{signal:abort.signal}),secondRejected=assert.rejects(second,/cancel|abort/i);
 const third=client.invoke({...route,invocation}),thirdResult=third.then(value=>({value}),error=>({error}));
 const cancelling=client.cancel(invocation).catch(error=>error);abort.abort();await secondRejected;
 // Public B already rejected, but A is still held and B/C raw references remain.
 assert.equal(sent.filter(m=>m.type==='invoke_bound').length,0);release();
 if(gate){await gate.held;f.workers.workers[0].crash();}
 await firstRejected;const outcome=await thirdResult;await cancelling;
 assert.equal(sent.filter(m=>m.type==='invoke_bound').length,0,'C must not send after A clears its failed admission');
 assert.match(outcome.error?.message??'',/cancel/i);assert.equal((await lease.hydrate()).length,0,'neither queued successor acquired a durable row');
 // All same-ID raw operations have now settled with no live or durable owner.
 await client.invoke({...route,invocation});assert.equal(sent.filter(m=>m.type==='invoke_bound').length,1);
});
