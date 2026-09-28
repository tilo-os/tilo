import test from 'node:test';import assert from 'node:assert/strict';
import {createInvitedPeerSource} from './invited-peer-source.mjs';
const deferred=()=>{let resolve;const promise=new Promise(yes=>resolve=yes);return{promise,resolve};};
function fixture(){const calls=[];let next=0;const runtime={invited_begin(){calls.push('begin');return ++next;},async invited_prepare(){calls.push('prepare');return JSON.stringify({kind:'checkpoint',material:[1]});},async invited_complete(_id,material){calls.push(['complete',material]);return JSON.stringify({kind:'admitted',material:[2],namespace:{profile:'certified:r',device:'d',room:'r'}});},invited_cancel(id){calls.push(['cancel',id]);},async invited_connect(){calls.push('connect');return 4;}};const lease={material:Uint8Array.of(0),async save(material){calls.push(['save',[...material]]);this.material=material;}};const source=createInvitedPeerSource({runtime,lease});return{runtime,lease,source,calls};}
test('redemption completion follows exact checkpoint ACK and Peer construction follows admitted identity ACK',async()=>{const f=fixture();const prepared=await f.source.prepare();assert.deepEqual(f.calls.slice(0,5),['begin','prepare',['save',[1]],['complete',Uint8Array.of(1)],['save',[2]]]);assert.equal(await f.source.connect(prepared,[],()=>{}),4);assert.equal(f.calls.at(-1)[0],'cancel');});
test('failed checkpoint ACK cannot send Complete',async()=>{const f=fixture();f.lease.save=async()=>{throw Error('ACK failed');};await assert.rejects(f.source.prepare(),/ACK failed/);assert.ok(!f.calls.some(v=>Array.isArray(v)&&v[0]==='complete'));assert.deepEqual(f.calls.at(-1),['cancel',1]);});
test('close cancels before a late prepared proof can be committed or sent',async()=>{const f=fixture(),gate=deferred();f.runtime.invited_prepare=()=>gate.promise;const pending=f.source.prepare();await Promise.resolve();f.source.close();gate.resolve(JSON.stringify({kind:'checkpoint',material:[1]}));await assert.rejects(pending,/closed/);assert.ok(!f.calls.some(v=>Array.isArray(v)&&v[0]==='save'));});
test('close during checkpoint ACK cannot send Complete after ACK returns',async()=>{const f=fixture(),gate=deferred();f.lease.save=()=>gate.promise;const pending=f.source.prepare();await Promise.resolve();await Promise.resolve();f.source.close();gate.resolve();await assert.rejects(pending,/closed/);assert.ok(!f.calls.some(v=>Array.isArray(v)&&v[0]==='complete'));});
test('late connected owner is closed and never returned after source disposal',async()=>{const f=fixture(),gate=deferred(),prepared=await f.source.prepare();f.runtime.invited_connect=()=>gate.promise;f.runtime.close_peer=async id=>f.calls.push(['close-peer',id]);const pending=f.source.connect(prepared,[],()=>{});f.source.close();gate.resolve(8);await assert.rejects(pending,/closed/);assert.deepEqual(f.calls.at(-1),['close-peer',8]);});
test('uncertain Complete retries with the exact durable checkpoint and no replacement identity',async()=>{
 const f=fixture(),complete=f.runtime.invited_complete,seen=[];let uncertain=true;
 f.runtime.invited_complete=async(id,material)=>{seen.push([...material]);if(uncertain){uncertain=false;throw Error('reply lost');}return complete(id,material);};
 await assert.rejects(f.source.prepare(),/reply lost/);assert.deepEqual([...f.lease.material],[1]);
 await f.source.prepare();assert.deepEqual(seen,[[1],[1]]);f.source.close();
});
test('descriptor discovery is bounded, uses the exact origin and never supplies authentication',async()=>{
 const f=fixture();f.runtime.invited_complete=async()=>JSON.stringify({kind:'admitted',material:[2],namespace:{profile:'p',device:'d',room:'r'},descriptorUrl:'https://workspace.example/peer/v3/descriptor'});
 const discovered=[];f.runtime.invited_descriptor=(id,bytes)=>discovered.push([id,[...bytes]]);
 let url,options;const source=createInvitedPeerSource({runtime:f.runtime,lease:f.lease,fetchImpl:async(u,o)=>{url=u;options=o;return new Response(Uint8Array.of(3,4));}});
 await source.prepare();assert.equal(url,'https://workspace.example/peer/v3/descriptor');assert.equal(options.credentials,'omit');assert.equal(options.redirect,'error');assert.equal(options.headers,undefined);assert.deepEqual(discovered,[[1,[3,4]]]);source.close();
 const tooBig=createInvitedPeerSource({runtime:f.runtime,lease:f.lease,fetchImpl:async()=>new Response(new Uint8Array(24577))});
 await assert.rejects(tooBig.prepare(),/exceeds bound/);assert.equal(discovered.length,1);
});
test('failed preparation retains custody until Rust endpoint cleanup settles',async()=>{
 const f=fixture(),gate=deferred();let settled=false;
 f.runtime.invited_prepare=async()=>{throw Error('network failed');};f.runtime.invited_cancel=()=>gate.promise;
 const pending=f.source.prepare();pending.catch(()=>{settled=true;});for(let i=0;i<8;i++)await Promise.resolve();
 assert.equal(settled,false);gate.resolve();await assert.rejects(pending,/network failed/);
});
test('close joins a pending bind and the late endpoint close even when cancel initially has no endpoint',async()=>{
 const f=fixture(),bound=deferred(),endpointClosed=deferred();let closed=false,created=false;
 f.runtime.invited_prepare=async()=>{await bound.promise;created=true;await endpointClosed.promise;throw Error('cancelled after bind');};
 const pending=f.source.prepare(),rejected=assert.rejects(pending,/cancelled after bind/);
 const closing=f.source.close().then(()=>{closed=true;});
 for(let i=0;i<8;i++)await Promise.resolve();assert.equal(closed,false);
 bound.resolve();for(let i=0;i<8;i++)await Promise.resolve();assert.equal(created,true);assert.equal(closed,false);
 endpointClosed.resolve();await rejected;await closing;assert.equal(closed,true);
});
test('a late Peer teardown failure keeps the same rejected custody promise',async()=>{
 const f=fixture(),connected=deferred(),prepared=await f.source.prepare();
 f.runtime.invited_connect=()=>connected.promise;f.runtime.close_peer=async()=>{throw Error('endpoint cleanup failed');};
 const connecting=f.source.connect(prepared,[],()=>{}),failedConnect=assert.rejects(connecting,/cleanup failed/);
 const closing=f.source.close(),failedClose=assert.rejects(closing,/cleanup failed/);connected.resolve(8);
 await failedConnect;await failedClose;assert.equal(f.source.close(),closing);
});
test('transport-only resume of an admitted guest needs no attempt and no membership refresh',async()=>{
 const calls=[];const runtime={invited_resume_transport:async(peer,material)=>calls.push(['resume',peer,[...material]])};
 const source=createInvitedPeerSource({runtime,lease:{material:Uint8Array.of(5)}});
 await source.resume(3);assert.deepEqual(calls,[['resume',3,[5]]]);source.close();await assert.rejects(source.resume(3),/closed/);
 assert.equal(createInvitedPeerSource({runtime:{},lease:{material:Uint8Array.of(5)}}).resume,undefined);
});
