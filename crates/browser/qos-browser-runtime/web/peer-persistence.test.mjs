import test from 'node:test';
import assert from 'node:assert/strict';
import {commitPeerWrites} from './peer-persistence.mjs';
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
