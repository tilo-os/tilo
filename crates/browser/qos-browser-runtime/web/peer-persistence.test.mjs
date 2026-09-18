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
