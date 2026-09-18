import {test} from 'node:test';
import assert from 'node:assert/strict';
import {createConnectPeer,pendingPeerRecoveryCount,retryPendingPeerRecovery} from './peer-session.mjs';
import {recoverableClose,settleComponentClose} from './close-recovery.mjs';
const deferred=()=>{let resolve,reject;const promise=new Promise((a,b)=>{resolve=a;reject=b;});return{promise,resolve,reject};};
function fixture(){
 let next=0,held=0,storageRefs=0,failConnect=false,failClose=false,failCommit=false,pendingWrite=false,attempts=0,gate;
 const peers=new Set(),mounts=new Set(),closed=[],rebound=[],suspended=new Set();let componentOwners=0,durableRecord;
 const locks={request:async(_name,_opts,fn)=>{held++;try{return await fn({});}finally{held--;}}};
 const runtime={peer_enrollment:()=>JSON.stringify({profile:'p',device:'d',room:'r'}),refresh_storage_lease(){},
  async connect_peer(_raw,_bytes,status){attempts++;if(gate)await gate.promise;if(failConnect)throw Error('dial failed');const id=++next;peers.add(id);status(JSON.stringify({phase:'ready',records:[{app_id:'dev.qos.word-processor',record_id:'tab:1',heads:['h']}]}));return id;},
  async close_peer(id){closed.push(id);peers.delete(id);},async disconnect_peer(id){assert(peers.has(id));},
  peer_pending_writes(id){assert(peers.has(id),'closed peer');return JSON.stringify(pendingWrite?[{revision:1}]:[]);},acknowledge_storage(){pendingWrite=false;},peer_committed(id){assert(peers.has(id));},
  suspend_peer(id,value){assert(peers.has(id));value?suspended.add(id):suspended.delete(id);},
  rebind_peer(old,id){rebound.push([old,id]);for(const mount of mounts)mount.id=id;suspended.delete(old);},
 };
 const storage={fresh:true,hydrate:async()=>[],commit:async()=>{if(failCommit)throw Error('durable write failed');},release:async()=>{storageRefs--;}};
 const connect=createConnectPeer({init:async()=>{},runtime,locks,fetchEnrollment:async()=>({ok:true,text:async()=>'{raw}'}),acquireStorage:async()=>{storageRefs++;return storage;},mountDesktop:async()=>{
  const mount={id:next,cleanupCalls:0,input(){if(!suspended.has(mount.id)){assert(peers.has(mount.id),'input addressed removed peer');}},setSuspended(){}};
  componentOwners++;
  const componentRuntime={prepare_close:()=>JSON.stringify({writes:[{revision:7,bytes:[1,4,9]}],errors:[]}),acknowledge_storage(){},finish_close(){componentOwners--;}};
  mount.cleanup=recoverableClose({begin:()=>{mount.cleanupCalls++;},settle:async()=>{
    await settleComponentClose(componentRuntime,1,{commit:async writes=>{if(failClose)throw Error('retained close write failed');durableRecord=writes[0].bytes;}},'p','d');mounts.delete(mount);
  }});
  mounts.add(mount);return mount;
 }});
 return{connect,peers,mounts,closed,rebound,suspended,recovery:()=>({componentOwners,durableRecord}),setFailCommit:v=>{failCommit=v;pendingWrite=true;},setFailConnect:v=>failConnect=v,setFailClose:v=>failClose=v,setGate:v=>gate=v,counts:()=>({held,storageRefs,attempts})};
}
test('failed reconnect retains durable owner; next retry rebinds same mount and disposes',async()=>{
 const f=fixture(),peer=await f.connect();const mount=await peer.mountDesktop({});
 f.setFailConnect(true);await assert.rejects(peer.reconnect(),/dial failed/);
 assert(f.suspended.has(mount.id),'input is suspended while transport is unavailable');assert.doesNotThrow(()=>mount.input());
 f.setFailConnect(false);await peer.reconnect();assert.equal(f.rebound.length,1);assert(!f.suspended.has(mount.id));assert.doesNotThrow(()=>mount.input());
 await peer.dispose();assert.deepEqual(f.counts(),{held:0,storageRefs:0,attempts:3});assert.equal(f.peers.size,0);
});
test('overlapping reconnects coalesce and disposal awaits attempted rebind',async()=>{
 const f=fixture(),peer=await f.connect();await peer.mountDesktop({});const gate=deferred();f.setGate(gate);
 const a=peer.reconnect(),b=peer.reconnect();await Promise.resolve();const disposal=peer.dispose();gate.resolve();
 await Promise.allSettled([a,b]);await disposal;assert.equal(f.counts().held,0);assert.equal(f.peers.size,0);assert(f.counts().attempts<=2);
});
test('failed component close attempts all mounts and retry releases custody exactly once',async()=>{
 const f=fixture(),peer=await f.connect(),a=await peer.mountDesktop({}),b=await peer.mountDesktop({});
 f.setFailClose(true);await assert.rejects(peer.dispose(),/retained close/);assert.equal(a.cleanupCalls,1);assert.equal(b.cleanupCalls,1);assert.equal(f.counts().held,1);
 f.setFailClose(false);await peer.retryPersistence();await peer.dispose();await peer.dispose();assert.equal(f.mounts.size,0);assert.equal(f.peers.size,0);assert.deepEqual(f.counts(),{held:0,storageRefs:0,attempts:1});
 assert.deepEqual(f.recovery(),{componentOwners:0,durableRecord:[1,4,9]});const replacement=await f.connect();await replacement.dispose();assert.equal(f.counts().held,0);
});

test('setup failure with retained bytes has reachable recovery and releases only after successful commit',async()=>{
 const f=fixture();f.setFailCommit(true);await assert.rejects(f.connect(),/retained recovery/);
 assert.equal(f.counts().held,1);assert.equal(pendingPeerRecoveryCount(),1);
 f.setFailCommit(false);await retryPendingPeerRecovery();assert.equal(f.counts().held,0);assert.equal(f.peers.size,0);assert.equal(pendingPeerRecoveryCount(),0);
});
