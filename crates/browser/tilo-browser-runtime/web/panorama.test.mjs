import test from 'node:test';
import assert from 'node:assert/strict';
import {createPresentedSource, createPanorama} from './panorama.mjs';

// Synthetic surfaces only. Calls are recorded to assert the production renderer's
// transform/clip ordering; the browser pixel fixture exercises Canvas2D itself.
function canvas(width=100,height=50) {
 const calls=[];
 const context=new Proxy({calls}, {get:(o,k)=>k in o?o[k]:(...args)=>calls.push([k,...args])});
 return {width,height,calls,getContext:()=>context,toBlob:cb=>cb(new Blob(['fixture'],{type:'image/png'}))};
}
function fixture({room='room',app='notes',record='one',width=100,height=50}={}) {
 let valid=true, clock=0;
 const listeners=new Set(), members=new Set(['agent']);
 const layer={members:()=>[...members], onMember:fn=>{listeners.add(fn);return()=>listeners.delete(fn);},remoteState:()=>[{publisher:'agent',seq:1,receivedAtMs:clock,focus:{app,record},pointer:[20,10],participant:{kind:'agent',label:'A01',color:'#123456'}}]};
 const copies=[];
 const source=createPresentedSource({audience:room,app,canvas:canvas(width,height),layer:()=>layer,valid:()=>valid,now:()=>clock,makeCanvas:(w,h)=>{const copy=canvas(w,h);copies.push(copy);return copy;}});
 const output=canvas();
 const view=createPanorama({canvas:output,sources:[{source:source.handle,rect:[200,100,100,50]}],width:400,height:200,now:()=>clock});
 return {source,view,output,layer,copies,present:(frames=1)=>source.present({phase:'presented',captureProtected:false,frames,gpuGeneration:1,record,logicalSize:[width,height],surfaceSize:[width,height]}),set time(t){clock=t;},invalidate(){valid=false;},member(active){if(active)members.add('agent');else members.delete('agent');for(const fn of listeners)fn({publisher:'agent',active});}};
}

test('panorama refuses a source until an actual presented frame arrives',()=>{
 const f=fixture();assert.throws(()=>f.view.render({cx:200,cy:100,scale:1}),/presented/);f.view.close();
});
test('one transform maps real pixels and pointers with content clipping',()=>{
 const f=fixture();f.present();
 const receipt=f.view.render({cx:200,cy:100,scale:2});
 assert.equal(receipt.provenance,'shared-host composition of retained mounts');
 assert.deepEqual(receipt.panes[0].transform,[2,0,0,2,200,100]);
 assert.deepEqual(receipt.panes[0].pointers[0].screen,[240,120]);
 assert.equal(receipt.panes[0].frame,1);
 assert.equal(f.output.calls.filter(c=>c[0]==='drawImage').length,1);
 const clip=f.output.calls.findIndex(c=>c[0]==='clip'),image=f.output.calls.findIndex(c=>c[0]==='drawImage');
 assert.ok(clip>=0&&clip<image);f.view.close();
});
test('missing physical pointers and other app/record events never become cursors',()=>{
 const f=fixture();f.layer.remoteState=()=>[
 {publisher:'agent',focus:{app:'other',record:'one'},pointer:[1,2]},
 {publisher:'agent',focus:{app:'notes',record:'other'},pointer:[1,2]},
 {publisher:'agent',focus:{app:'notes',record:'one'},pointer:[100,2]},
 {publisher:'agent',focus:{app:'notes',record:'one'},pointer:null},
 {publisher:'unadmitted',focus:{app:'notes',record:'one'},pointer:[1,2]}];
 f.present();assert.deepEqual(f.view.render({cx:200,cy:100,scale:1}).panes[0].pointers,[]);f.view.close();
});
test('departed/rejoined publishers cannot reuse cached coordinates; stale pointers expire',()=>{
 const f=fixture();f.present();f.member(false);f.member(true);
 assert.deepEqual(f.view.render({cx:200,cy:100,scale:1}).panes[0].pointers,[]);
 f.present(2);assert.equal(f.view.render({cx:200,cy:100,scale:1}).panes[0].pointers.length,1);
 f.time=10001;assert.deepEqual(f.view.render({cx:200,cy:100,scale:1},{maxAgeMs:20000}).panes[0].pointers,[]);f.view.close();
});
test('hidden observers release copies and neither capture nor sample; disposal fails closed',()=>{
 const f=fixture();f.view.setVisible(false);f.layer.remoteState=()=>{throw Error('hidden source sampled');};f.present();
 assert.equal(f.view.render({cx:0,cy:0,scale:1}),null);
 f.view.setVisible(true);assert.throws(()=>f.view.render({cx:200,cy:100,scale:1}),/presented/);
 f.invalidate();assert.throws(()=>f.view.render({cx:200,cy:100,scale:1}),/unavailable/);f.view.close();
 assert.throws(()=>f.view.render({cx:0,cy:0,scale:1}),/closed/);
});
test('camera culls offscreen sources before reading pixels',()=>{
 const f=fixture();f.present();const r=f.view.render({cx:5000,cy:100,scale:1});assert.deepEqual(r.panes,[]);f.view.close();
});
test('capture dimensions, audience mixing, overlapping panes and stale frames are refused',()=>{
 const f=fixture(), g=fixture({room:'other'});
 assert.throws(()=>createPanorama({canvas:canvas(),sources:[{source:f.source.handle,rect:[0,0,100,50]},{source:g.source.handle,rect:[100,0,100,50]}],width:400,height:200}),/audience/);
 assert.throws(()=>createPanorama({canvas:canvas(),sources:[{source:f.source.handle,rect:[0,0,100,50]}],width:100000,height:200}),/dimensions/);
 f.present();f.time=1001;assert.throws(()=>f.view.render({cx:200,cy:100,scale:1}),/stale/);f.view.close();g.view.close();
});
test('PNG encoding admits one in flight and returns the same frame receipt',async()=>{
 const f=fixture();f.present();let finish;f.output.toBlob=fn=>{finish=fn;};
 const p=f.view.png({cx:200,cy:100,scale:1});
 await assert.rejects(f.view.png({cx:200,cy:100,scale:1}),/in flight/);
 finish(new Blob(['fixture']));const capture=await p;assert.equal(capture.receipt.sequence,1);assert.ok(capture.blob.size);f.view.close();
});

test('synthetic-only 80 presences in 20 panes preserve identities through zoom',()=>{
 const fixtures=Array.from({length:20},(_,i)=>fixture({record:`test-only-${i}`}));
 for(const [i,f]of fixtures.entries()){
  f.layer.members=()=>Array.from({length:4},(_,j)=>`test-only-agent-${4*i+j}`);
  f.layer.remoteState=()=>f.layer.members().map((publisher,j)=>({publisher,seq:1,receivedAtMs:0,focus:{app:'notes',record:`test-only-${i}`},pointer:[10+j*20,20]}));
 }
 const view=createPanorama({canvas:canvas(),sources:fixtures.map((f,i)=>({source:f.source.handle,rect:[(i%5)*110,Math.floor(i/5)*60,100,50]})),width:600,height:300,now:()=>0});
 fixtures.forEach(f=>f.present());
 const receipt=view.render({cx:270,cy:115,scale:1});
 assert.equal(receipt.panes.length,20);
 assert.equal(new Set(receipt.panes.flatMap(p=>p.pointers.map(p=>p.publisher))).size,80);
 const wider=view.render({cx:270,cy:115,scale:.5});
 assert.equal(new Set(wider.panes.flatMap(p=>p.pointers.map(p=>p.publisher))).size,80);
 view.close();fixtures.forEach(f=>f.view.close());
});

test('a relayout, old frame or conflicting generation cannot mix new geometry with old pixels',()=>{
 const f=fixture();f.present(3);
 f.source.present({phase:'presented',captureProtected:false,frames:2,gpuGeneration:1,record:'one',logicalSize:[100,50],surfaceSize:[100,50]});
 assert.throws(()=>f.view.render({cx:200,cy:100,scale:1}),/order/);
 assert.equal(f.output.calls.at(-1)[0],'clearRect','refusal blanks stale output');
 f.view.close();
});

test('a failed visible frame still releases offscreen sources across hide/show',()=>{
 const a=fixture(),b=fixture({record:'second'});let samples=0;b.layer.remoteState=()=>{samples++;return [];};
 // Remove the fixtures' own observers, leaving this one as their sole owner.
 a.view.close();b.view.close();
 const view=createPanorama({canvas:canvas(),sources:[{source:a.source.handle,rect:[0,0,100,50]},{source:b.source.handle,rect:[10000,0,100,50]}],width:100,height:50,now:()=>0});
 assert.throws(()=>view.render({cx:50,cy:25,scale:1}),/presented/);
 b.present();assert.equal(samples,0);
 view.setVisible(false);view.setVisible(true);b.present(2);assert.equal(samples,0);
 view.close();
});

test('mutating a receipt cannot mutate subsequent source geometry',()=>{
 const f=fixture();f.present();const first=f.view.render({cx:200,cy:100,scale:1});
 first.panes[0].logicalSize[0]=1;first.panes[0].pixels[0]=1;
 const second=f.view.render({cx:200,cy:100,scale:1});assert.deepEqual(second.panes[0].logicalSize,[100,50]);assert.deepEqual(second.panes[0].pixels,[100,50]);f.view.close();
});

test('a disposed source being revealed cannot prevent culling other panes',()=>{
 const a=fixture(),b=fixture({record:'second'});let samples=0;b.layer.remoteState=()=>{samples++;return [];};
 a.view.close();b.view.close();
 const view=createPanorama({canvas:canvas(),sources:[{source:a.source.handle,rect:[10000,0,100,50]},{source:b.source.handle,rect:[0,0,100,50]}],width:100,height:50,now:()=>0});
 b.present();view.render({cx:50,cy:25,scale:1});a.source.close();const before=samples;
 assert.throws(()=>view.render({cx:10050,cy:25,scale:1}),/unavailable/);
 b.present(2);assert.equal(samples,before);view.close();
});

test('an app backing canvas cannot be used as the observer output',()=>{
 const input=canvas();const source=createPresentedSource({audience:'room',app:'notes',canvas:input,layer:()=>undefined,valid:()=>true});
 assert.throws(()=>createPanorama({canvas:input,sources:[{source:source.handle,rect:[0,0,100,50]}],width:200,height:100}),/source canvas/);
 assert.equal(input.width,100);assert.equal(input.height,50);source.close();
});

test('external output resizing cannot bypass capture bounds or falsify the receipt',()=>{
 const f=fixture();f.present();f.output.width=100000;
 assert.throws(()=>f.view.render({cx:200,cy:100,scale:1}),/output dimensions/);
 assert.equal(f.output.width,400);assert.equal(f.output.height,200);f.view.close();
});


test('protected or unknown presentations retire capture before copying any pixels',()=>{
 for(const protection of [true,undefined]) {
  const f=fixture();f.present();f.view.render({cx:200,cy:100,scale:1});
  const copied=f.copies[0].calls.filter(c=>c[0]==='drawImage').length;
  f.source.present({phase:'presented',captureProtected:protection,frames:2,gpuGeneration:1,record:'one',logicalSize:[100,50],surfaceSize:[100,50]});
  assert.equal(f.copies[0].calls.filter(c=>c[0]==='drawImage').length,copied);
  const draws=f.output.calls.filter(c=>c[0]==='drawImage').length;
  assert.throws(()=>f.view.render({cx:200,cy:100,scale:1}),/capture protection/);
  assert.equal(f.output.calls.filter(c=>c[0]==='drawImage').length,draws);
  f.present(3);assert.doesNotThrow(()=>f.view.render({cx:200,cy:100,scale:1}));f.view.close();
 }
});
