import test from 'node:test';import assert from 'node:assert/strict';
import {createAssetRecovery,retryableMemo} from './asset-recovery.mjs';
test('asset recovery retries exactly the failed admitted request after its cause is removed',async()=>{
 let available=false;const loads=[],completed=[],statuses=[];const request={loader:2,url:'tilo-blob://content'};
 const assets=createAssetRecovery({active:()=>true,load:async request=>{loads.push(request);if(!available)throw Error('unavailable');return new Uint8Array([1,2]);},complete:(request,bytes)=>{completed.push([request,bytes]);return !!bytes;},retry:()=>true,report:status=>statuses.push(status)});
 await assert.rejects(assets.request(request),/unavailable/);const id=statuses.at(-1).assetId;available=true;
 const a=assets.recover(id),b=assets.recover(id);assert.equal(a,b);await a;assert.equal(loads.length,2);assert(loads.every(row=>row===request));assert.equal(statuses.at(-1).asset,'ready');await assert.rejects(assets.recover('unknown'),/Unknown/);
});
test('initial asset initialization retries a rejected promise without poisoning later mounts',async()=>{
 let available=false,calls=0;const ready=retryableMemo(async()=>{calls++;if(!available)throw Error('missing');return 'ready';});
 await assert.rejects(ready(),/missing/);available=true;assert.equal(await ready(),'ready');assert.equal(await ready(),'ready');assert.equal(calls,2);
});
