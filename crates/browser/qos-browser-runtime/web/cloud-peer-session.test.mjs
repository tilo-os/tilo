import test from 'node:test';
import assert from 'node:assert/strict';
import {createConnectPeer} from './peer-session.mjs';
function fixture({phase='inspect'}={}){
 const calls=[];let attempt=0,refreshHint,staged=null;const state={phase};
 const namespace={profile:'certified:r',device:'d',room:'r'};
 const lease={material:Uint8Array.of(0),async save(bytes){calls.push('identity-ack');this.material=bytes;},async release(){calls.push('release-identity');}};
 const runtime={peer_delivery_begin:()=>JSON.stringify({kind:"ready"}),peer_delivery_next:()=>JSON.stringify({kind:"idle"}),peer_delivery_stop(){},
  cloud_begin(_material,account,machine){assert.equal(account,'acct');assert.equal(machine,'m-1');calls.push('begin');return ++attempt;},
  cloud_step(){return JSON.stringify(state.phase==='apply'?{step:'apply',operationKey:'op',payload:'{"p":1}'}:{step:state.phase});},
  cloud_enroll(){calls.push('enroll');staged='apply';return JSON.stringify({kind:'checkpoint',material:[1]});},
  cloud_accept(){calls.push('accept');staged='enrolled';return JSON.stringify({kind:'checkpoint',material:[2]});},
  cloud_acknowledge(){calls.push('ack');state.phase=staged;},
  async cloud_complete(){calls.push('complete');return JSON.stringify({kind:'admitted',material:[2],namespace});},
  async cloud_connect(_attempt,_material,_hydrated,status){calls.push('connect');status(JSON.stringify({phase:'ready',records:[],health:{transport:{phase:'connected',authenticated:true,generation:'1'},admission:{phase:'ready',requiredRecords:[],pendingPrerequisites:0}}}));return 7;},
  async cloud_reconnect(_attempt,_material,peer){assert.equal(peer,7);calls.push('reconnect');},
  cloud_cancel(){calls.push('cancel');},peer_bind_storage_epoch(){calls.push('bind-storage');},
  peer_media_bootstrap(){},cloud_refresh_media(_attempt,_material,peer){assert.equal(peer,7);calls.push('refresh-media');},
  refresh_storage_lease(){},peer_pending_writes:()=> '[]',peer_committed(){},
  async close_peer(){calls.push('close-peer');},async disconnect_peer(){calls.push('disconnect-peer');},
 };
 const enrollment={inspect:async()=>{calls.push('inspect');return '{"c":1}';},apply:async(payload,{operationKey})=>{calls.push(['apply',payload,operationKey]);return '{"r":1}';}};
 const connect=createConnectPeer({init:async()=>{},runtime,acquireIdentity:async()=>{throw Error('local enrollment forbidden');},acquireInvitedIdentity:async()=>{throw Error('invitation forbidden');},
  acquireCloudIdentity:async(cloud)=>{assert.equal(cloud.accountId,'acct');calls.push('acquire-cloud');return lease;},
  acquireStorage:async(profile,device)=>{assert.equal(profile,namespace.profile);assert.equal(device,namespace.device);calls.push('acquire-storage');return{epoch:'held',hydrate:async()=>[],commit:async()=>{},release:async()=>calls.push('release-storage')};},
  fetchEnrollment:async()=>{throw Error('no unauthenticated fetch expected');},createMedia:options=>{refreshHint=options.refreshHint;return{initialize:async()=>{},dispose:async()=>{},available:false};},events:{}});
 return{calls,runtime,connect,state,refresh:signal=>refreshHint(signal),options:{cloud:{accountId:'acct',machineId:'m-1',enrollment}}};
}
test('Cloud owner enrolls through the injected adapter, then runs the ordinary certified Peer session',async()=>{
 const f=fixture(),peer=await f.connect(f.options);
 const names=f.calls.map(v=>Array.isArray(v)?v[0]:v);
 assert.deepEqual(names.slice(0,14),['acquire-cloud','begin','inspect','enroll','identity-ack','ack','apply','accept','identity-ack','ack','complete','identity-ack','acquire-storage','connect']);
 assert.deepEqual(f.calls[6],['apply','{"p":1}','op']);
 await peer.reconnect();
 // Reconnect refreshes membership with the retained identity; no second inspect/apply.
 assert.equal(f.calls.filter(v=>v==='inspect').length,1);assert.equal(f.calls.filter(v=>Array.isArray(v)&&v[0]==='apply').length,1);
 assert.equal(f.calls.filter(v=>v==='complete').length,2);assert.equal(f.calls.filter(v=>v==='reconnect').length,1);
 await peer.dispose();assert(f.calls.indexOf('close-peer')<f.calls.indexOf('release-identity'));
});
test('Cloud owner media refresh uses membership refresh without replacing the data carrier',async()=>{
 const f=fixture({phase:'enrolled'}),peer=await f.connect(f.options);
 assert(!f.calls.includes('inspect'));
 await f.refresh(new AbortController().signal);
 assert.equal(f.calls.filter(v=>v==='connect').length,1);assert(!f.calls.includes('reconnect'));assert(f.calls.includes('refresh-media'));await peer.dispose();
});
test('Cloud owner option requires an adapter and excludes invitations',async()=>{
 const f=fixture();
 await assert.rejects(f.connect({cloud:{accountId:'acct',machineId:'m-1'}}),/inspect and apply/);
 await assert.rejects(f.connect({...f.options,invitation:{text:'x',identity:'new-person',label:'g'}}),/either an invitation or a Cloud owner/);
 assert(!f.calls.includes('acquire-cloud'));
});
