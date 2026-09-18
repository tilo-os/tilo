import test from 'node:test';
import assert from 'node:assert/strict';
import {settleComponentClose} from './close-recovery.mjs';
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
