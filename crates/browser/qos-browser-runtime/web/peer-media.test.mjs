import test from 'node:test';
import assert from 'node:assert/strict';
import {createPeerMedia} from './peer-media.mjs';
const call='a'.repeat(64),publisher='b'.repeat(64);
function fixture(extra={}){
 const calls=[];let request=0,stage='bootstrap',intent=0;
 const prepared=target=>JSON.stringify({kind:'request',id:target==='/peer/v3/bootstrap'?0:++request,origin:'https://media.test',target,body:[1],proof:target==='/peer/v3/bootstrap'?null:[2]});
 const runtime={
  peer_media_bootstrap(){calls.push('bootstrap');return prepared('/peer/v3/bootstrap');},
  peer_media_bootstrap_complete(){calls.push('bootstrap-complete');},
  peer_media_start_http(){calls.push('start-http');stage='ready';return prepared('/peer/v3/challenge');},
  peer_media_current(){return true;},peer_media_supported(){return true;},peer_media_forget(){},peer_media_reset(){calls.push('reset');},peer_media_can_publish(){return true;},
  peer_media_join(){calls.push('join');stage='grant';const value=prepared('/peer/v3/media/grant/challenge');intent=request;return value;},
  peer_media_heartbeat(){stage='presence';return prepared('/peer/v3/media/grant/challenge');},
  peer_media_relay(){stage='relay';return prepared('/peer/v3/media/relay/challenge');},
  peer_media_leave(_id,handle){calls.push(['leave',handle]);return JSON.stringify({kind:'left'});},
  peer_media_close_relay(_id,handle,carrier){calls.push(['relay-close',handle,carrier]);return JSON.stringify({kind:'relay-closed'});},
  peer_media_advance(){return JSON.stringify(stage==='ready'?{kind:'ready'}:stage==='grant'?{kind:'grant',handle:intent,expiresInMs:60000,presenceInMs:15000,directions:{publish:true,subscribe:true},publishNamespace:`${call}/${publisher}`,inventory:{revision:'1',namespaces:[]}}:stage==='presence'?{kind:'presence',expiresInMs:15000,inventory:{revision:'1',namespaces:[]}}:{kind:'relay',handle:intent,carrier:4,endpoint:'https://media.test/moq/v21/voice',authorization:[1],publishNamespace:`${call}/${publisher}`,inventory:{revision:'1',namespaces:[]}});},
 };
 const audio={capabilities:async()=>({supported:true,capture:true,playback:true}),async start(options){calls.push(['audio-start',options.muted]);return {setMuted:async value=>calls.push(['mute',value]),selectInput:async()=>{},play(){},stop(){calls.push('audio-stop');}};}};
 const transportFactory=options=>({async connect(context){const auth=await options.authorize(context);calls.push(['transport',auth.carrierHandle,auth.subscribeNamespaces]);let close;const closed=new Promise(resolve=>close=resolve);return {publish(){},subscribe(){return()=>{};},refreshAuthorization(){},closed,close(){calls.push('transport-close');close();}};}});
 const client=createPeerMedia({runtime,peerId:1,persist:async()=>calls.push('persist'),audio,transportFactory,fetch:async(url,options)=>{calls.push(['fetch',url,options.redirect,options.credentials]);return new Response(Uint8Array.of(1));},...extra});
 return {client,calls,runtime};
}
test('installed adapter persists verified bootstrap before HTTP proof and stays idle muted without capture',async()=>{
 const f=fixture();await f.client.initialize();
 assert.equal(f.client.supported(),true);assert.equal(f.client.mediaV1.getSnapshot().state,'idle');assert.equal(f.client.mediaV1.getSnapshot().muted,true);
 assert.ok(f.calls.indexOf('persist')>f.calls.indexOf('bootstrap-complete'));
 assert.ok(f.calls.indexOf('start-http')>f.calls.indexOf('persist'));
 assert.equal(f.calls.some(v=>Array.isArray(v)&&v[0]==='audio-start'),false);
 await f.client.mediaV1.join({callId:call});
 assert.deepEqual(f.calls.find(v=>Array.isArray(v)&&v[0]==='audio-start'),['audio-start',true]);
 assert.deepEqual(f.calls.find(v=>Array.isArray(v)&&v[0]==='transport'),['transport',4,[]]);
 await f.client.dispose();
 assert.ok(f.calls.indexOf('audio-stop')<f.calls.indexOf('reset'));
});

test('fresh join refreshes expired media custody once without replaying prior participation',async()=>{
 let refreshed=0;const f=fixture({refreshHint:async()=>{refreshed++;f.runtime.peer_media_current=()=>true;}});
 await f.client.initialize();f.runtime.peer_media_current=()=>false;
 // A second setup allocates a new opaque handle; this test checks setup only.
 await f.client.mediaV1.join({callId:call});
 assert.equal(refreshed,1);assert.equal(f.calls.filter(v=>v==='bootstrap').length,2);
 await f.client.dispose();
});
test('failed floor acknowledgement never starts an HTTP proof',async()=>{
 const f=fixture({persist:async()=>{throw Error('commit failed');}});
 await assert.rejects(f.client.initialize(),/unavailable/);assert.ok(!f.calls.includes('start-http'));assert.equal(f.client.supported(),false);
 await f.client.dispose();
});

test('leave cancels a pending expiry refresh before fresh bootstrap or capture',async()=>{
 let resume,entered;const started=new Promise(resolve=>entered=resolve),gate=new Promise(resolve=>resume=resolve);
 const f=fixture({refreshHint:async()=>{entered();await gate;}});
 await f.client.initialize();f.runtime.peer_media_current=()=>false;
 const joining=f.client.mediaV1.join({callId:call});await started;
 await f.client.mediaV1.leave();resume();await joining;
 assert.equal(f.calls.filter(value=>value==='bootstrap').length,1);
 assert.equal(f.calls.some(value=>Array.isArray(value)&&value[0]==='audio-start'),false);
 assert.equal(f.client.mediaV1.getSnapshot().state,'ended');await f.client.dispose();
});

test('control response size fails closed before Rust consumes bytes',async()=>{
 let advanced=0;const f=fixture({fetch:async()=>new Response(new Uint8Array(32769))});
 f.runtime.peer_media_bootstrap_complete=()=>advanced++;
 await assert.rejects(f.client.initialize());assert.equal(advanced,0);assert.equal(f.client.supported(),false);await f.client.dispose();
});

for(const mode of ['failure','leave'])test(`expired custody remains explicitly retryable after ${mode}`,async t=>{
 let refreshes=0,release,entered;const blocked=new Promise(resolve=>release=resolve),started=new Promise(resolve=>entered=resolve);
 const f=fixture({refreshHint:async()=>{refreshes++;if(refreshes===1){if(mode==='failure')throw Error('transient network');entered();await blocked;}else f.runtime.peer_media_current=()=>true;}});
 t.after(async()=>{release();await f.client.dispose();});await f.client.initialize();f.runtime.peer_media_current=()=>false;
 const first=f.client.mediaV1.join({callId:call});
 if(mode==='leave'){await started;await f.client.mediaV1.leave();}
 if(mode==='failure')await assert.rejects(first,/unavailable/);else await first;
 assert.equal(f.client.supported(),true,'installed API remains reachable for explicit retry');
 await f.client.mediaV1.join({callId:call});assert.equal(refreshes,2);assert.equal(f.client.mediaV1.getSnapshot().state,'live');
 release();await new Promise(resolve=>setImmediate(resolve));
 assert.equal(f.calls.filter(value=>value==='bootstrap').length,2,'cancelled old refresh cannot start another bootstrap');
 assert.equal(f.client.mediaV1.getSnapshot().state,'live');
});

test('media borrows already acknowledged HTTPS custody without a second handshake or persistence owner',async()=>{
 const f=fixture();f.runtime.peer_media_bootstrap=()=>JSON.stringify({kind:'ready'});
 await f.client.initialize();assert.equal(f.client.supported(),true);
 assert.equal(f.calls.includes('persist'),false);assert.equal(f.calls.includes('start-http'),false);
 assert.equal(f.calls.some(call=>Array.isArray(call)&&call[0]==='fetch'),false);
 await f.client.dispose();
});

test('shared authorization expiry fences capture before refresh and uses closed reauthorize handle',async t=>{
 let release,entered;const gate=new Promise(resolve=>release=resolve),started=new Promise(resolve=>entered=resolve);
 const f=fixture({refreshHint:async signal=>{entered();await gate;signal.throwIfAborted();f.runtime.peer_media_current=()=>true;}});
 t.after(async()=>{release();await f.client.dispose();});
 f.runtime.peer_media_expire=()=>f.calls.push('expire');
 f.runtime.peer_media_reauthorize=(_id,old)=>{f.calls.push(['reauthorize',old]);const request=JSON.parse(f.runtime.peer_media_join());request.id+=1000;return JSON.stringify(request);};
 await f.client.initialize();await f.client.mediaV1.join({callId:call});await f.client.mediaV1.setMuted(false);
 const expired=f.client.expireAuthorization();
 assert.ok(f.calls.includes('audio-stop'),'actual capture is stopped before the shared refresh awaits');
 await started;assert.equal(f.calls.filter(v=>Array.isArray(v)&&v[0]==='reauthorize').length,0);
 f.runtime.peer_media_bootstrap=()=>JSON.stringify({kind:'ready'});
 release();await new Promise(resolve=>setImmediate(resolve));
 assert.equal(f.client.mediaV1.getSnapshot().state,'live','request correlation ID may differ from retained participation handle');
 await expired;
 assert.equal(f.calls.filter(v=>Array.isArray(v)&&v[0]==='reauthorize').length,1);
 assert.equal(f.client.mediaV1.getSnapshot().state,'live');
 assert.equal(f.client.mediaV1.getSnapshot().muted,false);
 assert.equal(f.calls.filter(v=>Array.isArray(v)&&v[0]==='audio-start').length,2);
});

test('Leave during shared refresh cancels resumed intent before a fresh Rust proof',async t=>{
 let release,entered;const gate=new Promise(resolve=>release=resolve),started=new Promise(resolve=>entered=resolve);
 const f=fixture({refreshHint:async signal=>{entered();await gate;signal.throwIfAborted();}});
 t.after(async()=>{release();await f.client.dispose();});
 f.runtime.peer_media_expire=()=>{};
 f.runtime.peer_media_reauthorize=()=>{throw Error('must not sign after Leave');};
 await f.client.initialize();await f.client.mediaV1.join({callId:call});
 const expired=f.client.expireAuthorization();await started;
 const leaving=f.client.mediaV1.leave();release();await leaving;await expired;
 assert.equal(f.client.mediaV1.getSnapshot().state,'ended');
 assert.equal(f.calls.filter(v=>Array.isArray(v)&&v[0]==='audio-start').length,1);
 assert.ok(f.calls.some(v=>Array.isArray(v)&&v[0]==='leave'));
});
