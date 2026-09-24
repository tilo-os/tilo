import test from 'node:test';
import assert from 'node:assert/strict';
import {createConnectPeer} from './peer-session.mjs';
import {createPeerMedia} from './peer-media.mjs';
const deferred=()=>{let resolve;const promise=new Promise(yes=>resolve=yes);return{promise,resolve};};
function fixture({realMedia=false}={}){
 const calls=[];let attempt=0,refreshHint,media;
 const namespace={profile:'certified:r',device:'d',room:'r'};
 const lease={material:Uint8Array.of(0),async save(bytes){calls.push('identity-ack');this.material=bytes;},async release(){calls.push('release-identity');}};
 const runtime={peer_delivery_begin:()=>JSON.stringify({kind:"ready"}),peer_delivery_next:()=>JSON.stringify({kind:"idle"}),peer_delivery_stop(){},
  invited_begin(){calls.push('begin');return ++attempt;},
  async invited_prepare(){calls.push('prepare');return JSON.stringify({kind:'checkpoint',material:[1]});},
  async invited_complete(){calls.push('complete');return JSON.stringify({kind:'admitted',material:[2],namespace});},
  async invited_connect(_attempt,_material,_hydrated,status){calls.push('connect');status(JSON.stringify({phase:'ready',records:[],health:{transport:{phase:'connected',authenticated:true,generation:'1'},admission:{phase:'ready',requiredRecords:[],pendingPrerequisites:0}}}));return 7;},
  async invited_reconnect(_attempt,_material,peer){assert.equal(peer,7);calls.push('reconnect');},
  invited_cancel(){calls.push('cancel');},peer_bind_storage_epoch(){calls.push('bind-storage');},
  peer_media_bootstrap(){},invited_refresh_media(_attempt,_material,peer){assert.equal(peer,7);calls.push('refresh-media');},
  refresh_storage_lease(){},peer_pending_writes:()=> '[]',peer_committed(){},
  async close_peer(){calls.push('close-peer');},async disconnect_peer(){calls.push('disconnect-peer');},
 };
 const connect=createConnectPeer({init:async()=>{},runtime,acquireIdentity:async()=>{throw Error('owner enrollment forbidden');},acquireInvitedIdentity:async()=>{calls.push('acquire-invited');return lease;},
  acquireStorage:async(profile,device)=>{assert.equal(profile,namespace.profile);assert.equal(device,namespace.device);calls.push('acquire-storage');return{epoch:'held',hydrate:async()=>[],commit:async()=>{},release:async()=>calls.push('release-storage')};},
  fetchEnrollment:async()=>{throw Error('owner enrollment forbidden');},createMedia:options=>{refreshHint=options.refreshHint;if(realMedia){media=createPeerMedia({...options,fetch:async()=>new Response(Uint8Array.of(1)),audio:{capabilities:async()=>({supported:true,capture:true,playback:true}),start:async()=>{throw Error('capture must not start');}}});return media;}return{initialize:async()=>{},dispose:async()=>{},available:false};},events:{}});
 return{calls,runtime,connect,media:()=>media,refresh:signal=>refreshHint(signal),options:{invitation:{text:'signed-invitation',identity:'new-person',label:'Guest'}}};
}
test('invitation source uses the existing Peer owner, persistence and reconnect without owner enrollment',async()=>{
 const f=fixture(),peer=await f.connect(f.options);
 assert.deepEqual(f.calls.slice(0,9),['acquire-invited','begin','prepare','identity-ack','complete','identity-ack','acquire-storage','connect','cancel']);
 await peer.reconnect();assert.equal(f.calls.filter(value=>value==='reconnect').length,1);await peer.dispose();
 assert(f.calls.indexOf('close-peer')<f.calls.indexOf('release-identity'));
});
test('guest media refresh uses current membership without replacing the data carrier',async()=>{
 const f=fixture(),peer=await f.connect(f.options);
 await f.refresh(new AbortController().signal);
 assert.equal(f.calls.filter(value=>value==='connect').length,1);assert(!f.calls.includes('reconnect'));assert(f.calls.includes('refresh-media'));await peer.dispose();
});
test('late guest membership refresh cannot install after the Peer closes',async()=>{
 const f=fixture(),peer=await f.connect(f.options),gate=deferred();f.runtime.invited_complete=()=>gate.promise;
 const pending=f.refresh(new AbortController().signal);for(let i=0;i<8;i++)await Promise.resolve();const closing=peer.dispose();
 gate.resolve(JSON.stringify({kind:'admitted',material:[2],namespace:{profile:'certified:r',device:'d',room:'r'}}));
 await assert.rejects(pending,/custody changed/);await closing;assert(!f.calls.includes('refresh-media'));
});
test('aborted invitation preparation cancels its Rust owner before waiting for completion and releases custody',async()=>{
 const f=fixture(),gate=deferred(),entered=deferred(),signal=new AbortController();f.runtime.invited_prepare=()=>{entered.resolve();return gate.promise;};
 const pending=f.connect({...f.options,signal:signal.signal});
 await entered.promise;signal.abort(Error('cancelled'));
 assert(f.calls.includes('cancel'));gate.resolve(JSON.stringify({kind:'checkpoint',material:[1]}));
 await assert.rejects(pending);assert(!f.calls.includes('complete'));assert(f.calls.includes('release-identity'));
});
test('real media disposal retains Peer custody through a late refresh bind and endpoint teardown',async()=>{
 const f=fixture({realMedia:true}),entered=deferred(),bound=deferred(),endpointClosed=deferred();let disposed=false;
 Object.assign(f.runtime,{
  peer_media_bootstrap:()=>JSON.stringify({kind:'request',id:1,origin:'https://native.example',target:'/peer/v3/bootstrap',body:[1]}),
  peer_media_bootstrap_complete(){},peer_media_start_http:()=>JSON.stringify({kind:'ready'}),peer_media_supported:()=>true,
  peer_media_current:()=>false,peer_media_reset(){},peer_media_can_publish:()=>false,
 });
 const peer=await f.connect(f.options);await f.media().initialize();
 f.runtime.invited_prepare=async()=>{entered.resolve();await bound.promise;f.calls.push('late-endpoint-created');await endpointClosed.promise;f.calls.push('late-endpoint-closed');throw Error('cancelled bind');};
 const joining=peer.mediaV1.join({callId:'ab'.repeat(32)}).catch(error=>error);
 await entered.promise;const closing=peer.dispose().then(()=>{disposed=true;});
 for(let i=0;i<30;i++)await Promise.resolve();
 assert.equal(disposed,false);assert(!f.calls.includes('release-storage'));assert(!f.calls.includes('release-identity'));
 bound.resolve();for(let i=0;i<10;i++)await Promise.resolve();
 assert(f.calls.includes('late-endpoint-created'));assert(!f.calls.includes('release-identity'));
 endpointClosed.resolve();await joining;await closing;assert(!f.calls.includes('refresh-media'));
 assert(f.calls.indexOf('late-endpoint-closed')<f.calls.indexOf('release-storage'));
 assert(f.calls.indexOf('late-endpoint-closed')<f.calls.indexOf('release-identity'));
});
