import test from 'node:test';
import assert from 'node:assert/strict';
import {commitPeerWrites} from './peer-persistence.mjs';
import * as persistence from './peer-persistence.mjs';
test('shared batch reader uses owned binary payloads without invoking the JSON export',()=>{
 const batch={id:'peer:1',writes:[{bytes:new Uint8Array([0,128,255]),actor:new Uint8Array([7]),revision:'9007199254740993'}]};
 const runtime={peer_pending_batch_bytes(id){assert.equal(id,17);return batch;},peer_pending_batch(){throw Error('binary data must not round trip through JSON');}};
 assert.equal(persistence.readPeerBatch(runtime,17),batch);
});
test('shared batch reader supports older JSON runtimes with exact revision strings',()=>{
 const batch={id:'peer:1',writes:[{bytes:[0,128,255],revision:'9007199254740993'}]};
 assert.deepEqual(persistence.readPeerBatch({peer_pending_batch:id=>{assert.equal(id,17);return JSON.stringify(batch);}},17),batch);
});
test('a newer checkpoint arriving during IDB commit is committed before publication',async()=>{
 let revision=1,durable=0;const batches=[];let publishes=0;
 await commitPeerWrites({pending:()=>revision>durable?[{revision}]:[],commit:async writes=>{batches.push(writes.map(w=>w.revision));if(revision===1){await Promise.resolve();revision=2;}},acknowledge:write=>{durable=Math.max(durable,write.revision);},publish:()=>{assert.equal(durable,revision,'cannot publish over a newer uncommitted admission');publishes++;}});
 assert.deepEqual(batches,[[1],[2]]);assert.equal(publishes,1);
});
test('an aborted transaction retains admission and never publishes',async()=>{
 let acknowledged=false,published=false;
 await assert.rejects(commitPeerWrites({pending:()=>[{revision:1}],commit:async()=>{throw Error('aborted');},acknowledge:()=>{acknowledged=true;},publish:()=>{published=true;}}),/aborted/);
 assert.equal(acknowledged,false);assert.equal(published,false);
});
test('publication failure is separate from completed local durability',async()=>{
 let durable=false;const result=await commitPeerWrites({pending:()=>durable?[]:[{revision:'9007199254740993'}],commit:async()=>{},acknowledge:()=>{durable=true;},publish:()=>{throw Error('relay unavailable');}});
 assert.equal(durable,true);assert.equal(result.localDurable,true);assert.match(String(result.publicationError),/relay unavailable/);
});
test('immutable batch settlement identifies transaction epoch and preserves newer batches',async()=>{
 const {commitPeerBatches}=await import('./peer-persistence.mjs');let index=0;const committed=[],settled=[];
 const result=await commitPeerBatches({pending:()=>index<2?{id:`batch-${index}`,writes:[{revision:String(9007199254740993n+BigInt(index))}]}:{id:'0',writes:[]},commit:async writes=>committed.push(writes[0].revision),acknowledge:value=>{settled.push(value);index++;},epoch:'lease-2',now:()=>123,publish:()=>{throw Error('offline');}});
 assert.deepEqual(committed,['9007199254740993','9007199254740994']);assert.deepEqual(settled.map(s=>[s.batchId,s.epoch,s.result.ok]),[['batch-0','lease-2',true],['batch-1','lease-2',true]]);assert.equal(result.localDurable,true);assert.match(String(result.publicationError),/offline/);
});
test('aborted batch is reported as failed without publication or advancing identities',async()=>{
 const {commitPeerBatches}=await import('./peer-persistence.mjs');let settlement,published=false;
 await assert.rejects(commitPeerBatches({pending:()=>({id:'retained-1',writes:[{revision:'5'}]}),commit:async()=>{throw Object.assign(Error('quota'),{name:'QuotaExceededError'});},acknowledge:value=>settlement=value,epoch:'lease-1',now:()=>234,publish:()=>published=true}),/quota/);
 assert.equal(settlement.batchId,'retained-1');assert.equal(settlement.result.code,'storage-quota');assert.equal(published,false);
});

test('a running drain forwards the exact later cut batch to the atomic storage transaction',async()=>{
 let index=0;const first={id:'old',writes:[{revision:'1'}]},cut={id:'cut',transferId:'0123456789abcdef0123456789abcdef',writes:[{revision:'2'}]};const seen=[];
 await persistence.commitPeerBatches({pending:()=>index===0?first:index===1?cut:{id:'0',writes:[]},commit:async(writes,batch)=>{assert.equal(writes,batch.writes);seen.push(batch);await Promise.resolve();},acknowledge:()=>index++,epoch:'epoch',publish:()=>{}});
 assert.deepEqual(seen,[first,cut]);
});
test('capture backpressure retries instead of failing the storage transaction',async()=>{
 const {commitPeerBatches,storageBusy}=await import('./peer-persistence.mjs');const acknowledged=[];let published=false;
 const result=await commitPeerBatches({pending:()=>{throw Error('storage-pressure: settle the captured transfer cut before another capture');},commit:async()=>{throw Error('must not commit');},acknowledge:value=>acknowledged.push(value),epoch:'e',publish:async()=>{published=true;}});
 assert.deepEqual(result,{localDurable:false,busy:true,publicationError:null});
 assert.equal(acknowledged.length,0,'no failed settlement is reported for a refused capture');
 assert.equal(published,false);
 assert.equal(storageBusy(Error('storage-pressure: x')),true);assert.equal(storageBusy(Error('QuotaExceededError')),false);
 await assert.rejects(commitPeerBatches({pending:()=>{throw Error('storage lease must be bound before batch capture');},commit:async()=>{},acknowledge:()=>{},epoch:'e',publish:async()=>{}}),/lease must be bound/,'other capture errors still fail');
});
