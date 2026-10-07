import test from 'node:test';
import assert from 'node:assert/strict';
import {createSemanticPresentation} from './semantic-presentation.mjs';
const target={app:'notes',instance:12,record:'record-a',resource:'bound-alias',action:'edit'};
const state=(target,rect=[2,3,20,30])=>({phase:'presented',presentation:true,frames:1,gpuGeneration:1,layoutGeneration:1,record:'record-a',logicalSize:[400,300],surfaceSize:[800,600],semanticLoci:[{target,rect}]});
test('exact target latches only at a successful presentation and is read-only',()=>{
 let clock=10,valid=true;const requests=[];
 const latch=createSemanticPresentation({request:(t,on)=>requests.push(on?t:null),valid:()=>valid,now:()=>clock});
 const watch=latch.observe(target);assert.equal(watch.read(),null);
 latch.present({...state(target),presentation:false});assert.equal(watch.read(),null);
 latch.present(state({...target,record:'foreign'}));assert.equal(watch.read(),null);
 latch.present({...state(target),frames:2});const value=watch.read();
 assert.deepEqual(value.rect,[2,3,20,30]);assert.equal(value.frame,2);assert.equal(value.presentedAtMs,10);
 value.rect[0]=90;assert.equal(watch.read().rect[0],2);assert.equal(requests.length,1);
 latch.present({...state(target),presentation:false,frames:2,semanticLoci:[]});assert.equal(watch.read().rect[0],2);
 clock=1011;assert.equal(watch.read(),null);clock=10;valid=false;assert.equal(watch.read(),null);
 watch.close();assert.equal(watch.read(),null);assert.equal(requests.at(-1),null);
});
test('record changes, missing geometry, resize and disposal refuse old geometry',()=>{
 const latch=createSemanticPresentation({request:()=>{},valid:()=>true,now:()=>10});const watch=latch.observe(target);
 latch.present(state(target));assert.ok(watch.read());
 latch.present({...state(target),frames:2,record:'record-b'});assert.equal(watch.read(),null);
 latch.present({...state(target),frames:3});assert.ok(watch.read());
 latch.present({...state(target),frames:4,semanticLoci:[{target,rect:null}]});assert.equal(watch.read(),null);
 latch.present({...state(target),frames:5});assert.ok(watch.read());
 latch.present({...state(target),frames:6,logicalSize:[5,5]});assert.equal(watch.read(),null);
 latch.close();assert.equal(watch.read(),null);assert.throws(()=>latch.observe(target),/closed/);
});
test('queue and target sizes are bounded; ambiguous and regressing frames are gaps',()=>{
 const latch=createSemanticPresentation({request:()=>{},valid:()=>true,now:()=>10});
 const watches=Array.from({length:32},(_,i)=>latch.observe({...target,action:String(i)}));
 assert.throws(()=>latch.observe(target),/capacity/);watches[0].close();const watch=latch.observe(target);
 assert.throws(()=>latch.observe({...target,resource:'a'.repeat(4097)}));
 latch.present({...state(target),frames:4,semanticLoci:[{target,rect:[1,1,2,2]},{target,rect:[2,2,3,3]}]});assert.equal(watch.read(),null);
 latch.present({...state(target),frames:5});assert.ok(watch.read());
 latch.present({...state(target),frames:3});assert.equal(watch.read(),null);
});

test('status identity changes invalidate while duplicate actual presentations cannot refresh or change geometry',()=>{
 let clock=10;const latch=createSemanticPresentation({request:()=>{},valid:()=>true,now:()=>clock});const watch=latch.observe(target);
 latch.present(state(target));clock=20;latch.present(state(target));assert.equal(watch.read().presentedAtMs,10);
 latch.present({...state(target),presentation:false,record:'changed'});assert.equal(watch.read(),null);
 latch.present({...state(target),frames:2});assert.ok(watch.read());
 latch.present({...state(target),frames:2,semanticLoci:[{target,rect:[3,3,20,30]}]});assert.equal(watch.read(),null);
});
test('duplicate watches share a bounded request and unsubscribe once on disposal',()=>{
 const requests=[];const latch=createSemanticPresentation({request:(t,on)=>requests.push(on),valid:()=>true,now:()=>10});
 const a=latch.observe(target),b=latch.observe(target);assert.deepEqual(requests,[true]);
 a.close();assert.deepEqual(requests,[true]);latch.close();assert.deepEqual(requests,[true,false]);b.close();assert.deepEqual(requests,[true,false]);
});
test('missing layout proof, invalid bounds and clocks remain explicit gaps',()=>{
 let clock=20;const latch=createSemanticPresentation({request:()=>{},valid:()=>true,now:()=>clock});const watch=latch.observe(target);
 latch.present({...state(target),layoutGeneration:undefined});assert.equal(watch.read(),null);
 latch.present({...state(target),frames:2});assert.ok(watch.read());
 clock=NaN;assert.equal(watch.read(),null);clock=10;
 latch.present({...state(target),frames:3});assert.equal(watch.read(),null);clock=30;
 latch.present({...state(target),frames:4,semanticLoci:[{target,rect:[-1,3,20,30]}]});assert.equal(watch.read(),null);
 latch.present({...state(target),frames:5,semanticLoci:[{target,rect:[2,3,Infinity,30]}]});assert.equal(watch.read(),null);
 latch.present({...state(target),frames:6});assert.ok(watch.read());
 latch.present({phase:'suspended'});assert.equal(watch.read(),null);
});
