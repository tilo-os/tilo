import test from 'node:test';
import assert from 'node:assert/strict';
import {settleComponentClose,recoverableClose} from './close-recovery.mjs';
import {createStorageAccess} from './storage.mjs';
import {createInlineStorage} from './storage-inline.mjs';
import {fakeIndexedDB,fakeLocks,fakeWorkers} from './testing/fake-storage.mjs';
const settle=async(rounds=20)=>{for(let i=0;i<rounds;i++)await new Promise(resolve=>setImmediate(resolve));};
function workerStorage(t){
  const idb=fakeIndexedDB(),locks=fakeLocks(),workers=fakeWorkers({locks,indexedDB:idb});t.after(()=>workers.dispose());
  const access=createStorageAccess({createWorker:workers.createWorker,inline:createInlineStorage({locks:locks.manager('page'),indexedDB:idb}),delay:()=>settle(1),lockRetryMs:0});
  return {idb,locks,workers,access};
}
const closeWrite={key:'{"type":"record"}',revision:2,bytes:[1,4,9],actor:null};
test('close drain: acknowledgement and finish_close wait for the worker-confirmed commit of the close writes',async t=>{
  const {idb,workers,access}=workerStorage(t);const storage=await access.acquireStorage('p','d');
  const events=[];
  const runtime={prepare_close:()=>JSON.stringify({writes:[closeWrite],errors:[]}),acknowledge_storage(_profile,_device,revision){events.push(['ack',revision]);},finish_close(){events.push(['finish']);}};
  const gate=idb.gateNextCommit();
  const close=recoverableClose({begin(){events.push(['begin']);},settle:()=>settleComponentClose(runtime,1,storage,'p','d')});
  const closing=close();
  await gate.held;await settle();
  assert.deepEqual(events,[['begin']],'nothing settles before the strict transaction completed in the worker');
  assert.equal(idb.rows().length,0);
  gate.release();await closing;
  assert.deepEqual(events,[['begin'],['ack',2n],['finish']]);
  assert.deepEqual(idb.rows().map(row=>[row.revision,[...row.bytes],row.epoch]),[[2,[1,4,9],storage.epoch]]);
  assert.deepEqual(workers.workers[0].posted.map(message=>message.type),['open','commit']);
  await storage.release();
});
test('close drain: a worker crash during the close write keeps the close retryable and the retry commits in-page',async t=>{
  const {idb,locks,workers,access}=workerStorage(t);const storage=await access.acquireStorage('p','d');
  let began=0,finished=0;const errors=[];
  const runtime={prepare_close:()=>JSON.stringify({writes:[closeWrite],errors:[]}),acknowledge_storage(){},finish_close(){finished++;}};
  const gate=idb.gateNextCommit();
  const close=recoverableClose({begin(){began++;},settle:()=>settleComponentClose(runtime,1,storage,'p','d'),onError:error=>errors.push(error)});
  const first=close();await gate.held;workers.workers[0].crash();
  await assert.rejects(first,error=>error.name==='StorageWorkerError');
  assert.equal(finished,0);assert.equal(errors.length,1);assert.equal(idb.rows().length,0);
  await close();
  assert.equal(began,1);assert.equal(finished,1);
  assert.equal(idb.transactions().at(-1).owner,'page');assert.equal(locks.holder('tilo-writer:["p","d"]'),'page');
  assert.deepEqual(idb.rows().map(row=>[row.revision,row.epoch]),[[2,storage.epoch]]);
  await storage.release();assert.deepEqual(locks.held(),[]);
});
test('component retry enqueues current recovery bytes before commit and final report settlement',async()=>{
  const events=[];let durable=false;
  const runtime={
    prepare_close(){events.push('retry component');return JSON.stringify({writes:[{revision:2}],errors:['view pending']});},
    acknowledge_storage(profile,device,revision,error){assert.equal(revision,2n);assert.equal(error,undefined);events.push('ack');durable=true;},
    finish_close(){assert.equal(durable,true);events.push('finish component');}
  };
  await settleComponentClose(runtime,1,{async commit(writes){assert.deepEqual(writes,[{revision:2}]);events.push('commit');}},'p','d');
  assert.deepEqual(events,['retry component','commit','ack','finish component']);
});
test('component-level failure after successful IDB commit rejects and retries the retained report again',async()=>{
  let componentFailed=true;let retries=0;let finalized=0;
  const runtime={
    prepare_close(){retries++;return JSON.stringify({writes:[],errors:componentFailed?['component close failed']:[]});},
    acknowledge_storage(){throw Error('nothing to acknowledge');},
    finish_close(){if(componentFailed)throw Error('component close failed');finalized++;}
  };
  const storage={async commit(){throw Error('no pending bytes');}};
  await assert.rejects(settleComponentClose(runtime,1,storage,'p','d'),/component close failed/);
  assert.equal(finalized,0);componentFailed=false;
  await settleComponentClose(runtime,1,storage,'p','d');
  assert.equal(retries,2);assert.equal(finalized,1);
});
test('aborted IDB commit never settles component reports',async()=>{
  let finalized=false;
  const runtime={prepare_close:()=>JSON.stringify({writes:[{revision:1}],errors:[]}),acknowledge_storage(){throw Error('must not ack');},finish_close(){finalized=true;}};
  await assert.rejects(settleComponentClose(runtime,1,{async commit(){throw Error('abort');}},'p','d'),/abort/);
  assert.equal(finalized,false);
});

test('host close retries retained actual component settlement after aborted write, then becomes idempotent',async()=>{
  const {recoverableClose}=await import('./close-recovery.mjs');let began=0,releases=0,failed=true,retained=1;
  const runtime={prepare_close:()=>JSON.stringify({writes:[{revision:7}],errors:[]}),acknowledge_storage(){},finish_close(){retained--;}};
  const storage={commit:async()=>{if(failed)throw Error('aborted close write');}};
  const close=recoverableClose({begin:()=>{began++;},settle:async()=>{await settleComponentClose(runtime,1,storage,'p','d');releases++;}});
  await assert.rejects(close(),/aborted/);assert.equal(retained,1);assert.equal(releases,0);
  failed=false;await Promise.all([close(),close()]);await close();assert.equal(began,1);assert.equal(retained,0);assert.equal(releases,1);
});
