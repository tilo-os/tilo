import {test} from 'node:test';
import assert from 'node:assert/strict';
import {createConnectPeer,pendingPeerRecoveryCount,retryPendingPeerRecovery} from './peer-session.mjs';
import {recoverableClose,settleComponentClose} from './close-recovery.mjs';
import {createStorageAccess} from './storage.mjs';
import {createInlineStorage} from './storage-inline.mjs';
import {fakeIndexedDB,fakeLocks,fakeWorkers} from './testing/fake-storage.mjs';
const deferred=()=>{let resolve,reject;const promise=new Promise((a,b)=>{resolve=a;reject=b;});return{promise,resolve,reject};};
test('presentation bursts coalesce queued persistence behind an in-flight commit',async t=>{
 const f=fixture();let publications=0,commits=0;
 f.runtime.peer_committed=()=>{publications++;};
 const peer=await f.connect(),mount=await peer.mountDesktop({});
 t.after(()=>peer.dispose());await drain();publications=0;
 const entered=deferred(),gate=deferred();
 f.storage.commit=async()=>{commits++;entered.resolve();await gate.promise;};
 f.setFailCommit(false);
 f.status({phase:'ready',persistenceNeeded:true});await entered.promise;
 for(let i=0;i<100;i++)mount.present(1);
 gate.resolve();await peer.retryPersistence();
 assert.equal(commits,1);
 assert(publications<=2,`one active and one queued run should cover the burst; saw ${publications}`);
});
test('presented frames without pending writes neither publish nor recompute peer health',async t=>{
 const f=fixture();let publications=0;
 const peer=await f.connect(),mount=await peer.mountDesktop({});
 t.after(()=>peer.dispose());
 // Settle every queued persistence run without forcing a publication.
 const settle=async()=>{for(let i=0;i<5;i++){await new Promise(resolve=>setTimeout(resolve,0));await drain();}};
 await settle();
 f.runtime.peer_committed=()=>{publications++;};
 for(let frame=0;frame<100;frame++)mount.present(1);
 await settle();
 assert.equal(publications,0,`idle presented frames must not call peer_committed; saw ${publications}`);
 // The runtime reporting pending writes still commits and then publishes.
 f.setFailCommit(false);f.status({phase:'ready',persistenceNeeded:true});
 await settle();
 assert(publications>=1,'a committed batch publishes');
 const afterCommit=publications;
 for(let frame=0;frame<100;frame++)mount.present(1);
 await settle();
 assert.equal(publications,afterCommit,'later idle frames stay silent');
});
test('a write arriving during publication gets a following persistence barrier',{timeout:1000},async t=>{
 const f=fixture();let revision=0,durable=0;const committed=[],second=deferred();
 f.runtime.peer_pending_writes=()=>JSON.stringify(revision>durable?[{revision:String(revision)}]:[]);
 f.storage.commit=async writes=>{committed.push(Number(writes[0].revision));};
 f.runtime.acknowledge_storage=(_profile,_device,value)=>{durable=Number(value);if(durable===2)second.resolve();};
 f.runtime.peer_committed=()=>{if(durable===1&&revision===1){revision=2;f.status({phase:'ready',persistenceNeeded:true});}};
 const peer=await f.connect();t.after(()=>peer.dispose());
 revision=1;const first=peer.retryPersistence();await second.promise;await first;
 assert.deepEqual(committed,[1,2]);assert.equal(durable,revision);
});
test('typed persistence batches keep binary bytes and exact revisions through commit before acknowledgement',async()=>{
 const f=fixture();let pending=true,committed=false,acknowledged=false;
 const write={key:'{"type":"admission","workspace":"room"}',bytes:new Uint8Array([0,128,255]),actor:new Uint8Array([2,4]),revision:'9007199254740993'};
 f.storage.epoch='test-epoch';
 f.runtime.peer_pending_batch_bytes=()=>pending?{id:'1:1',writes:[write],records:[]}:{id:'0',writes:[],records:[]};
 f.runtime.peer_pending_batch=()=>{throw Error('numeric JSON batch must not be used when binary API is available');};
 f.runtime.peer_pending_writes=()=>{throw Error('legacy writes must not be used');};
 f.storage.commit=async writes=>{
  assert.equal(acknowledged,false);assert.equal(writes.length,1);
  assert(writes[0].bytes instanceof Uint8Array);assert.deepEqual([...writes[0].bytes],[0,128,255]);
  assert(writes[0].actor instanceof Uint8Array);assert.equal(writes[0].revision,'9007199254740993');committed=true;
 };
 f.runtime.acknowledge_peer_storage_batch=(_id,raw)=>{assert(committed);const value=JSON.parse(raw);assert.equal(value.batchId,'1:1');assert.equal(value.epoch,'test-epoch');assert.equal(value.result.ok,true);pending=false;acknowledged=true;};
 f.runtime.peer_committed=()=>{assert(acknowledged);};
 const peer=await f.connect();assert(acknowledged);await peer.dispose();
});
function fixture({enroll,recoveryClock,initialStatus,acquireStorage}={}){
 let next=0,held=0,storageRefs=0,failConnect=false,failClose=false,failCommit=false,pendingWrite=false,attempts=0,gate;
 const mountGates=[],mountSignals=[],mountOptionsSeen=[];
 const syncs=[];const peers=new Set(),mounts=new Set(),closed=[],rebound=[],suspended=new Set();let componentOwners=0,durableRecord,statusCallback,commitStatus,refreshStatus,reconnectStatus,publishFailure=false,failMount=false;
 const locks={request:async(_name,_opts,fn)=>{held++;try{return await fn({});}finally{held--;}}};
 const runtime={identity_request:()=>'{public}',identity_complete:()=>'{proof}',bind_identity:()=> 'opaque-key',peer_enrollment:()=>JSON.stringify({profile:'p',device:'d',room:'r'}),refresh_storage_lease(){},
  async connect_peer(_raw,_identity,_bytes,status){statusCallback=status;attempts++;if(gate)await gate.promise;if(failConnect)throw Error('dial failed');const id=++next;peers.add(id);status(JSON.stringify(initialStatus??{phase:'ready',records:[{app_id:'dev.qos.word-processor',record_id:'tab:1',heads:['h']}],health:{transport:{phase:'connected',authenticated:true,generation:'1'},admission:{phase:'ready',requiredRecords:[],pendingPrerequisites:0,progressRevision:'0'}}}));return id;},
  async reconnect_peer(id){attempts++;assert(peers.has(id));if(gate)await gate.promise;if(failConnect)throw Error('dial failed');if(reconnectStatus)statusCallback(JSON.stringify(reconnectStatus()));return id;},
  peer_request_sync(id,generation){assert(peers.has(id));syncs.push([id,generation]);},
  peer_refresh_status(id){assert(peers.has(id));if(refreshStatus)statusCallback(JSON.stringify(refreshStatus()));},
  async close_peer(id){closed.push(id);peers.delete(id);},async disconnect_peer(id){assert(peers.has(id));},
  peer_pending_writes(id){assert(peers.has(id),'closed peer');return JSON.stringify(pendingWrite?[{revision:1}]:[]);},acknowledge_storage(){pendingWrite=false;},peer_committed(id){assert(peers.has(id));if(publishFailure)throw Error('relay unavailable');if(commitStatus)statusCallback(JSON.stringify(commitStatus()));},
  suspend_peer(id,value){assert(peers.has(id));value?suspended.add(id):suspended.delete(id);},
  rebind_peer(old,id){rebound.push([old,id]);for(const mount of mounts)mount.id=id;suspended.delete(old);},
 };
 const storage={fresh:true,hydrate:async()=>[],commit:async()=>{if(failCommit)throw Error('durable write failed');},release:async()=>{storageRefs--;}};
 const connect=createConnectPeer({init:async()=>{},runtime,recoveryClock,acquireIdentity:async()=>{held++;return{material:'opaque-key',save:async()=>{},release:async()=>{held--;}}},fetchEnrollment:enroll??(async()=>({ok:true,text:async()=>'{raw}'})),acquireStorage:acquireStorage??(async()=>{storageRefs++;return storage;}),mountDesktop:async(_target,options)=>{
  mountOptionsSeen.push(options);mountSignals.push(options.signal);const mountGate=mountGates.shift();if(mountGate){try{await mountGate.promise;}catch(error){options.onStatus({phase:'failed',error:error.message});if(mountGate.cleanup)await mountGate.cleanup.promise;options.onStatus({phase:'closed',persistence:'committed',pendingWrites:0,error:null});throw error;}}
  if(failMount)throw Error('initial fonts unavailable');
  const mount={id:next,cleanupCalls:0,flushCalls:0,async flush(){mount.flushCalls++;},state:()=>({phase:'presented',gpuGeneration:1}),recover(){options.onStatus({phase:'gpu-pending',gpuGeneration:2});},status(value){options.onStatus(value);},present(gpuGeneration){options.onStatus({phase:'presented',gpuGeneration});},failRenderer(){options.onStatus({phase:'failed',gpuGeneration:1,error:'GPU device lost'});},input(){if(!suspended.has(mount.id)){assert(peers.has(mount.id),'input addressed removed peer');}},setSuspended(){}};
  componentOwners++;
  const componentRuntime={prepare_close:()=>JSON.stringify({writes:[{revision:7,bytes:[1,4,9]}],errors:[]}),acknowledge_storage(){},finish_close(){componentOwners--;}};
  mount.cleanup=recoverableClose({begin:()=>{mount.cleanupCalls++;},settle:async()=>{
    await settleComponentClose(componentRuntime,1,{commit:async writes=>{if(failClose)throw Error('retained close write failed');durableRecord=writes[0].bytes;}},'p','d');mounts.delete(mount);
  }});
  mounts.add(mount);return mount;
 }});
 return{runtime,storage,connect,syncs,peers,mounts,closed,mountSignals,mountOptionsSeen,queueMount:gate=>mountGates.push(gate),rebound,suspended,setFailMount:v=>failMount=v,setPublishFailure:v=>{publishFailure=v;pendingWrite=true;},setCommitStatus:v=>commitStatus=v,setRefreshStatus:v=>refreshStatus=v,setReconnectStatus:v=>reconnectStatus=v,status:value=>statusCallback(JSON.stringify(value)),recovery:()=>({componentOwners,durableRecord}),setFailCommit:v=>{failCommit=v;pendingWrite=true;},setFailConnect:v=>failConnect=v,setFailClose:v=>failClose=v,setGate:v=>gate=v,counts:()=>({held,storageRefs,attempts})};
}
test('committed desktop notices reach live mount subscriptions without entering snapshots or replaying',async t=>{
 const f=fixture(),peer=await f.connect();t.after(()=>peer.dispose());
 await peer.mountDesktop({});await drain();
 const subscribe=f.mountOptionsSeen.at(-1).subscribeDesktopCommits,received=[],late=[],snapshots=[];
 assert.equal(typeof subscribe,'function');
 const unsubscribe=subscribe(event=>received.push(event));
 const unsubscribeSnapshots=peer.subscribe(snapshot=>snapshots.push(snapshot));t.after(unsubscribeSnapshots);
 const notice={invocation:'local-launch',room:'r',opened:{strip:'9007199254740993',group:'9007199254740994',tab:'9007199254740995'}};
 const before=peer.getSnapshot();f.status({committedDesktop:notice});await drain();
 assert.deepEqual(received,[notice]);assert.equal(peer.getSnapshot(),before);assert.deepEqual(snapshots,[],'transient notification must not publish a retained session snapshot');
 const stopLate=subscribe(event=>late.push(event));assert.deepEqual(late,[],'new mount subscribers do not replay earlier launch focus');
 f.status({phase:'ready',committedDesktop:{...notice,invocation:'second'}});await drain();
 assert.equal(received.length,2);assert.equal(late.length,1);
 assert.equal(Object.hasOwn(peer.getSnapshot(),'committedDesktop'),false);
 assert(snapshots.every(snapshot=>!Object.hasOwn(snapshot,'committedDesktop')));
 unsubscribe();f.status({committedDesktop:{...notice,invocation:'third'}});await drain();
 assert.equal(received.length,2,'unsubscribed mounts receive no late notices');assert.equal(late.length,2);stopLate();
});
test('committed desktop subscriptions are isolated from listener errors and stop when session disposal starts',async()=>{
 const f=fixture(),peer=await f.connect();await peer.mountDesktop({});
 const subscribe=f.mountOptionsSeen.at(-1).subscribeDesktopCommits,received=[];
 subscribe(()=>{throw Error('disposed mount callback');});subscribe(event=>received.push(event));
 const notice={invocation:'local-launch',room:'r',opened:{strip:'2',group:'3',tab:'4'}};
 assert.doesNotThrow(()=>f.status({committedDesktop:notice}));await drain();assert.deepEqual(received,[notice]);
 const disposing=peer.dispose();f.status({committedDesktop:{...notice,invocation:'during-disposal'}});await disposing;
 f.status({committedDesktop:{...notice,invocation:'after-disposal'}});await drain();assert.deepEqual(received,[notice]);
});
test('failed reconnect retains durable owner; next retry rebinds same mount and disposes',async()=>{
 const f=fixture(),peer=await f.connect();const mount=await peer.mountDesktop({});
 f.setFailConnect(true);await assert.rejects(peer.reconnect(),/dial failed/);
 assert(!f.suspended.has(mount.id),'admitted local input stays available while transport is unavailable');assert.doesNotThrow(()=>mount.input());
 f.setFailConnect(false);await peer.reconnect();assert.equal(f.rebound.length,0);assert.equal(mount.id,1);assert.equal(f.closed.length,0);assert(!f.suspended.has(mount.id));assert.doesNotThrow(()=>mount.input());
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

test('each retry refreshes bootstrap with same client key via POST proof; setup errors release ownership',async()=>{
 let held=0,calls=[],attempt=0;
 const runtime={identity_request:()=>'{"device_public_key":"client","request_id":"stable"}',identity_complete:(_key,response)=>{assert.equal(response,'challenge');return 'proof';},bind_identity:()=> 'same-key',peer_enrollment:()=>'{"profile":"p","device":"client","room":"r"}',refresh_storage_lease(){},async connect_peer(raw,key,_rows,status){assert.equal(key,'same-key');assert.equal(raw,`bootstrap-${++attempt}`);status('{"phase":"ready","records":[]}');return attempt;},async reconnect_peer(id,raw,key){assert.equal(id,1);assert.equal(key,'same-key');assert.equal(raw,`bootstrap-${++attempt}`);return id;},peer_pending_writes:()=> '[]',peer_committed(){},disconnect_peer(){},close_peer(){},suspend_peer(){},rebind_peer(){}};
 const connect=createConnectPeer({init:async()=>{},runtime,mountDesktop(){},acquireStorage:async()=>({hydrate:async()=>[],release:async()=>{}}),acquireIdentity:async()=>{held++;return{material:'same-key',save:async()=>{},release:async()=>{held--;}}},fetchEnrollment:async(url,options)=>{calls.push([url,options.method,options.body]);return{ok:true,text:async()=>url.endsWith('/challenge')?'challenge':`bootstrap-${attempt+1}`};}});
 const peer=await connect();await peer.reconnect();assert.equal(calls.length,4);assert(calls.every(call=>call[1]==='POST'));assert.deepEqual(calls[0],calls[2]);assert.deepEqual(calls[1],calls[3]);await peer.dispose();assert.equal(held,0);
});

test('enrollment policy failure is explicit and releases key without trying another identity',async()=>{
 let calls=0;const f=fixture({enroll:async()=>{calls++;return{ok:false,status:403,text:async()=>'{"error":"device revoked"}'}}});
 await assert.rejects(f.connect(),/Enrollment HTTP 403: device revoked/);assert.equal(calls,1);assert.equal(f.counts().held,0);assert.equal(f.counts().attempts,0);
});
test('abort during enrollment settles constructor and releases custody before a new owner can load',async()=>{
 const gate=deferred(),entered=deferred();const f=fixture({enroll:async(_url,{signal})=>{entered.resolve();await gate.promise;if(signal.aborted)throw signal.reason;return{ok:true,text:async()=>'{raw}'};}});
 const abort=new AbortController();const pending=f.connect({signal:abort.signal});await entered.promise;abort.abort();assert.equal(f.counts().held,1);gate.resolve();await assert.rejects(pending);assert.equal(f.counts().held,0);assert.equal(f.counts().attempts,0);
});

test('committed local work survives publication failure with connection-only recovery',async()=>{
 const f=fixture(),peer=await f.connect();f.setPublishFailure(true);await peer.retryPersistence();
 const health=peer.getSnapshot().health;assert(health.failures.some(f=>f.scope.kind==='connection'));assert(!health.failures.some(f=>f.scope.kind==='storage'));assert.notEqual(peer.getSnapshot().persistence,'failed');
 f.setPublishFailure(false);await peer.reconnect();assert(!peer.getSnapshot().health.failures.some(f=>f.scope.kind==='connection'));await peer.dispose();
});
test('health generations increase as decimal integers and storage recovery is coalesced',async()=>{
 const f=fixture(),peer=await f.connect();const seen=[];peer.subscribe(snapshot=>seen.push(snapshot.health.generation));
 f.setFailCommit(true);await assert.rejects(peer.retryPersistence());assert(peer.getSnapshot().health.failures.some(f=>f.scope.kind==='storage'));
 f.setFailCommit(false);const a=peer.recover({kind:'retry-saving'}),b=peer.recover({kind:'retry-saving'});assert.equal(a,b);assert.deepEqual(await a,await b);assert(!peer.getSnapshot().health.failures.some(f=>f.scope.kind==='storage'));assert(seen.every((value,index)=>index===0||BigInt(value)>BigInt(seen[index-1])));await peer.dispose();
});

test('generation-only runtime reports do not publish a new peer snapshot',async()=>{
 const f=fixture(),peer=await f.connect(),notifications=[];peer.subscribe(snapshot=>notifications.push(snapshot));
 const ready=structuredClone(peer.getSnapshot());ready.health.transport.authenticated=true;f.status(ready);await drain();notifications.length=0;
 const mount=await peer.mountDesktop({});await drain();notifications.length=0;const before=peer.getSnapshot(),beforeValue=structuredClone(before);
 f.setCommitStatus(()=>{const current=structuredClone(peer.getSnapshot());current.health.generation=String(BigInt(current.health.generation)+1n);return current;});
 mount.present(1);await drain();
 assert.equal(notifications.length,0);assert.strictEqual(peer.getSnapshot(),before);assert.deepEqual(peer.getSnapshot(),beforeValue);
 await peer.dispose();
});

test('semantic report dedup still publishes recovery operation settlement',async()=>{
 const f=fixture(),peer=await f.connect(),states=[];peer.subscribe(snapshot=>states.push(snapshot.health.recoveryOperations.map(operation=>operation.state)));
 await peer.recover({kind:'review-permissions'});
 assert.deepEqual(states,[['pending'],['requires-user']]);
 await peer.dispose();
});

test('unchanged presented frames keep surface health stable while failure recovery still publishes',async()=>{
 const f=fixture(),peer=await f.connect(),notifications=[];peer.subscribe(snapshot=>notifications.push(snapshot));const mount=await peer.mountDesktop({});
 const presentedGeneration=peer.getSnapshot().health.generation,presentedNotifications=notifications.length;
 for(let frame=0;frame<60;frame++)mount.status({phase:'presented',gpuGeneration:1,frame});
 assert.equal(peer.getSnapshot().health.generation,presentedGeneration);assert.equal(notifications.length,presentedNotifications);

 mount.failRenderer();const failed=peer.getSnapshot();assert(BigInt(failed.health.generation)>BigInt(presentedGeneration));assert.equal(failed.health.surfaces[0].phase,'failed');assert(failed.health.failures.some(f=>f.code==='gpu-device-lost'));
 const surfaceId=failed.health.surfaces[0].id,recovery=peer.recover({kind:'retry-desktop',surfaceId});await drain();
 const recovering=peer.getSnapshot(),recoveringNotifications=notifications.length;assert.equal(recovering.health.surfaces[0].phase,'initializing');
 mount.status({phase:'gpu-pending',gpuGeneration:2,frame:61});assert.equal(peer.getSnapshot().health.generation,recovering.health.generation);assert.equal(notifications.length,recoveringNotifications);
 mount.present(2);const result=await recovery,restored=peer.getSnapshot();assert.equal(operation(peer,result).state,'succeeded');assert.equal(restored.health.surfaces[0].phase,'presented');assert(!restored.health.failures.some(f=>f.scope.surfaceId===surfaceId));
 await peer.dispose();
});

test('same-phase asset and storage failures still publish their health side effects',async()=>{
 const f=fixture(),peer=await f.connect(),notifications=[];peer.subscribe(snapshot=>notifications.push(snapshot));const mount=await peer.mountDesktop({});
 const beforeAsset=notifications.length;mount.status({phase:'presented',gpuGeneration:1,asset:'unavailable',assetId:'image-1',error:'asset load failed'});
 assert.equal(notifications.length,beforeAsset+1);assert(peer.getSnapshot().health.failures.some(f=>f.id.endsWith(':image-1')&&f.code==='asset-unavailable'));
 const afterAsset=notifications.length;mount.status({phase:'presented',gpuGeneration:1,asset:'unavailable',assetId:'image-1',error:'asset load failed'});
 assert.equal(notifications.length,afterAsset+1,'a repeated error must retain its reporting side effect');
 const beforeStorage=notifications.length;mount.status({phase:'presented',gpuGeneration:1,persistence:'failed'});
 assert.equal(notifications.length,beforeStorage+1);assert(peer.getSnapshot().health.failures.some(f=>f.scope.kind==='storage'));
 await peer.dispose();
});

test('desktop retry settles only after newer presentation and preserves unrelated saving failure',async()=>{
 const f=fixture(),peer=await f.connect(),mount=await peer.mountDesktop({});mount.failRenderer();f.setFailCommit(true);await assert.rejects(peer.retryPersistence());
 const surface=peer.getSnapshot().health.surfaces[0].id;let done=false;const recovery=peer.recover({kind:'retry-desktop',surfaceId:surface}).then(value=>{done=true;return value;});
 for(let i=0;i<4;i++)await Promise.resolve();mount.present(1);await Promise.resolve();assert.equal(done,false);mount.present(2);await recovery;
 assert(peer.getSnapshot().health.failures.some(f=>f.scope.kind==='storage'));assert(!peer.getSnapshot().health.failures.some(f=>f.scope.kind==='renderer'));
 f.setFailCommit(false);await peer.retryPersistence();await peer.dispose();
});
test('disposing during desktop recovery settles the waiter and emits no late completion',async()=>{
 const f=fixture(),peer=await f.connect();await peer.mountDesktop({});const surface=peer.getSnapshot().health.surfaces[0].id;
 const recovery=peer.recover({kind:'retry-desktop',surfaceId:surface});await Promise.resolve();await peer.dispose();await recovery;assert.equal(peer.getSnapshot().health.transport.phase,'closed');
});

test('saving retry clears each live host persistence latch before settling shared journal',async()=>{
 const f=fixture(),peer=await f.connect(),mount=await peer.mountDesktop({});await peer.retryPersistence();assert.equal(mount.flushCalls,1);await peer.dispose();
});

test('initial allocation failure retains exact attempt and successful retry delivers its handle',async()=>{
 const f=fixture(),peer=await f.connect();f.setFailMount(true);const abort=new AbortController();let surface,handle;const statuses=[];
 await assert.rejects(peer.mountDesktop({}, {signal:abort.signal,onSurface:id=>surface=id,onMount:mount=>handle=mount,onStatus:status=>statuses.push(status)}),/fonts unavailable/);
 assert(surface);assert(statuses.every(status=>status.surfaceId===surface));f.setFailMount(false);
 const result=await peer.recover({kind:'retry-desktop',surfaceId:surface});assert.equal(peer.getSnapshot().health.recoveryOperations.find(op=>op.id===result.operationId).state,'succeeded');assert(handle);assert.equal(peer.getSnapshot().health.surfaces.find(row=>row.id===surface).phase,'presented');
 abort.abort();for(let i=0;i<8;i++)await Promise.resolve();assert.equal(f.mounts.size,0);await assert.rejects(peer.recover({kind:'retry-desktop',surfaceId:surface}),/Unknown/);await peer.dispose();
});
test('aborting a failed initial mount prevents recovery from resurrecting its target',async()=>{
 const f=fixture(),peer=await f.connect();f.setFailMount(true);const abort=new AbortController();let surface;
 await assert.rejects(peer.mountDesktop({}, {signal:abort.signal,onSurface:id=>surface=id}));abort.abort();f.setFailMount(false);
 await assert.rejects(peer.recover({kind:'retry-desktop',surfaceId:surface}),/Unknown/);assert.equal(f.mounts.size,0);await peer.dispose();
});

const drain=async()=>{for(let i=0;i<50;i++)await Promise.resolve();};
function clock(){
 let time=0,wallOffset=0,id=0;const timers=new Map();
 return{now:()=>time+wallOffset,shiftWall:ms=>{wallOffset+=ms;},setTimeout:(fn,delay)=>{timers.set(++id,{fn,at:time+delay});return id;},clearTimeout:id=>timers.delete(id),async tick(ms){const end=time+ms;await drain();for(;;){const due=[...timers].filter(([,t])=>t.at<=end).sort((a,b)=>a[1].at-b[1].at)[0];if(!due)break;time=due[1].at;timers.delete(due[0]);due[1].fn();await drain();}time=end;await drain();}};
}
const record={roomId:'r',appId:'dev.qos.word-processor',recordId:'tab:1'};
const otherRecord={...record,recordId:'tab:2'};
function admissionStatus(admitted=false,records=[record]){
 return{phase:admitted?'ready':'incomplete',health:{transport:{phase:'connected',authenticated:true,generation:'1'},admission:{phase:admitted?'ready':'syncing',requiredRecords:records,pendingPrerequisites:admitted?0:1},records:records.map(record=>({record,admission:admitted?'admitted':'unavailable',current:admitted?{heads:['h']}:null,editing:{mode:admitted?'online':'blocked'},failureIds:[]})),failures:admitted?[]:[{id:'runtime:admission',code:'prerequisite-missing',scope:{kind:'admission'},severity:'error'}]}};
}
const operation=(peer,result)=>peer.getSnapshot().health.recoveryOperations.find(op=>op.id===result.operationId);
function outgoingStatus(blocked,generation='1',expiryAtMs=null){
 const ready=admissionStatus(true,[]);
 return{...ready,outgoingBlocked:blocked,outgoingPending:blocked,outgoingExpiryAtMs:expiryAtMs,synchronization:blocked?'blocked':'acknowledged',health:{...ready.health,transport:{...ready.health.transport,generation},failures:blocked?[{id:'runtime:outgoing',code:'checkpoint-expired',scope:{kind:'admission'},severity:'notice',message:'Saved changes are waiting for the session peer to confirm them.',recovery:[{kind:'refresh-checkpoint'}],occurredAtMs:0}]:[]}};
}
test('fresh outgoing expiry wakes an idle peer once and starts bounded reconciliation',async()=>{
 const time=clock(),f=fixture({recoveryClock:time,initialStatus:outgoingStatus(0,'1',300001)});f.setRefreshStatus(()=>outgoingStatus(1));const peer=await f.connect();
 await time.tick(300000);assert.equal(f.syncs.length,0);await time.tick(1);assert.equal(f.syncs.length,0,'expiry first requests an authoritative Rust report');
 await time.tick(10000);assert.equal(f.syncs.length,1);assert.equal(peer.getSnapshot().outgoingBlocked,1);await peer.dispose();
});
test('unchanged expiry projection cannot create a zero-delay status loop',async()=>{
 const time=clock();let refreshes=0;const status=outgoingStatus(0,'1',300001),f=fixture({recoveryClock:time,initialStatus:status});
 f.setRefreshStatus(()=>{refreshes++;return status;});const peer=await f.connect();await time.tick(300001);await drain();await time.tick(1000);
 assert.equal(refreshes,1);assert.equal(f.syncs.length,0);await peer.dispose();
});
test('clock rollback reschedules a still-future outgoing expiry without polling',async()=>{
 const time=clock();let refreshes=0;const status=outgoingStatus(0,'1',300001),f=fixture({recoveryClock:time,initialStatus:status});
 f.runtime.peer_refresh_status=id=>{assert(f.peers.has(id));refreshes++;};const peer=await f.connect();await time.tick(300000);time.shiftWall(-1000);await time.tick(1);
 assert.equal(refreshes,1);await time.tick(999);assert.equal(refreshes,1);await time.tick(1);assert.equal(refreshes,2);await time.tick(1000);assert.equal(refreshes,2);await peer.dispose();
});
test('expired outgoing recovery is bounded per condition and rearms only after clear or reconnect',async()=>{
 const time=clock(),f=fixture({recoveryClock:time}),peer=await f.connect();
 f.status(outgoingStatus(53));await drain();await time.tick(10000);assert.equal(f.syncs.length,1);
 for(let second=0;second<10;second++){f.status(outgoingStatus(53));await time.tick(1000);}
 assert.equal(f.syncs.length,2,'repeated reports share the same two-send episode');
 for(let second=0;second<60;second++){f.status(outgoingStatus(53));await time.tick(1000);}
 assert.equal(f.syncs.length,2,'an exhausted condition cannot rearm itself');
 assert.equal(peer.getSnapshot().health.admission.phase,'ready');
 assert(peer.getSnapshot().health.failures.some(failure=>failure.code==='checkpoint-expired'&&failure.severity==='notice'));
 f.status(outgoingStatus(0));f.status(outgoingStatus(1));await time.tick(10000);
 assert.equal(f.syncs.length,3,'a cleared then newly blocked condition gets a new bounded episode');
 await time.tick(20000);assert.equal(f.syncs.length,4);
 f.status(outgoingStatus(1,'2'));await time.tick(10000);
 assert.equal(f.syncs.length,5,'a new authenticated transport gets one new bounded episode');
 await peer.dispose();
});
test('explicit outgoing refresh waits for authoritative clear and can retry an exhausted episode',async()=>{
 const time=clock(),f=fixture({recoveryClock:time}),peer=await f.connect();
 f.status(outgoingStatus(53));await time.tick(30000);assert.equal(f.syncs.length,2);
 let done=false;const retry=peer.recover({kind:'refresh-checkpoint'}).then(result=>{done=true;return result;});await drain();
 f.status(outgoingStatus(53));await drain();assert.equal(done,false,'an unrelated ready report cannot complete outgoing repair');
 await time.tick(10000);assert.equal(f.syncs.length,3,'manual retry gets one new bounded episode');
 f.status(outgoingStatus(0));const result=await retry;
 assert.equal(operation(peer,result).state,'succeeded');await peer.dispose();
});
test('explicit outgoing refresh timeout retains the blocked condition',async()=>{
 const time=clock(),f=fixture({recoveryClock:time}),peer=await f.connect();f.status(outgoingStatus(53));
 const retry=peer.recover({kind:'refresh-checkpoint'});await drain();f.status(outgoingStatus(53));await time.tick(30000);
 const result=await retry;assert.equal(operation(peer,result).state,'failed');
 assert.equal(peer.getSnapshot().outgoingBlocked,53);assert(peer.getSnapshot().health.failures.some(failure=>failure.code==='checkpoint-expired'));
 await peer.dispose();
});
test('explicit outgoing refresh reconnects before its bounded episode',async()=>{
 const time=clock(),offline=outgoingStatus(53);offline.health.transport={...offline.health.transport,phase:'offline',authenticated:false};
 const f=fixture({recoveryClock:time,initialStatus:offline});f.setReconnectStatus(()=>outgoingStatus(53,'2'));const peer=await f.connect();const retry=peer.recover({kind:'refresh-checkpoint'});await drain();
 assert.equal(f.counts().attempts,2,'manual reconciliation must rejoin transport');
 await time.tick(10000);assert.equal(f.syncs.length>=1,true);f.status(outgoingStatus(0,'2'));
 assert.equal(operation(peer,await retry).state,'succeeded');await peer.dispose();
});
test('terminal admission while outgoing is blocked refuses manual reconciliation promptly',async()=>{
 for(const code of ['identity-revoked','record-install-failed']){
  const value=outgoingStatus(53);value.health.failures.push({id:`runtime:${code}`,code,scope:{kind:'admission'},severity:'error',message:'terminal',recovery:[],occurredAtMs:0});
  const f=fixture({recoveryClock:clock(),initialStatus:value}),peer=await f.connect();const result=await peer.recover({kind:'refresh-checkpoint'});
  assert.equal(operation(peer,result).state,'failed',code);assert.equal(f.syncs.length,0);await peer.dispose();
 }
});
test('terminal health without an outgoing field settles an active outgoing waiter',async()=>{
 const time=clock(),f=fixture({recoveryClock:time}),peer=await f.connect();f.status(outgoingStatus(53));
 const retry=peer.recover({kind:'refresh-checkpoint'});await drain();f.status(outgoingStatus(53));await drain();
 const terminal=admissionStatus(true,[]);terminal.health.failures.push({id:'runtime:revoked',code:'identity-revoked',scope:{kind:'admission'},severity:'error',message:'terminal',recovery:[],occurredAtMs:0});f.status(terminal);
 assert.equal(operation(peer,await retry).state,'failed');assert.equal(f.syncs.length,0);await peer.dispose();
});
test('record checkpoint recovery requires both outgoing clear and requested admission',async()=>{
 const blocked=admissionStatus(false,[record]);Object.assign(blocked,{outgoingBlocked:53,outgoingPending:53,synchronization:'blocked'});blocked.health.failures.push({id:'runtime:outgoing',code:'checkpoint-expired',scope:{kind:'admission'},severity:'notice',message:'waiting',recovery:[{kind:'refresh-checkpoint'}],occurredAtMs:0});
 const time=clock(),f=fixture({recoveryClock:time,initialStatus:blocked}),peer=await f.connect();let done=false;const retry=peer.recover({kind:'refresh-checkpoint',record}).then(result=>{done=true;return result;});await drain();
 const cleared=admissionStatus(false,[record]);Object.assign(cleared,{outgoingBlocked:0,outgoingPending:0,synchronization:'acknowledged'});f.status(cleared);await drain();assert.equal(done,false,'outgoing clear cannot admit the requested record');
 f.status({...admissionStatus(true,[record]),outgoingBlocked:0,outgoingPending:0,synchronization:'acknowledged'});assert.equal(operation(peer,await retry).state,'succeeded');await peer.dispose();
});
test('admission and outgoing reconciliation share one manual recovery deadline',async()=>{
 const blocked=admissionStatus(false,[record]);Object.assign(blocked,{outgoingBlocked:53,outgoingPending:53,synchronization:'blocked'});
 const time=clock(),f=fixture({recoveryClock:time,initialStatus:blocked}),peer=await f.connect();const retry=peer.recover({kind:'refresh-checkpoint',record});
 await time.tick(20000);f.status({...admissionStatus(true,[record]),outgoingBlocked:53,outgoingPending:53,synchronization:'blocked'});await drain();
 await time.tick(9999);assert.equal(operation(peer,{operationId:'1'})?.state,'pending');await time.tick(1);
 const result=await retry;assert.equal(operation(peer,result).state,'failed');assert.equal(f.syncs.length,3,'one boundary request cannot extend the overall deadline');await peer.dispose();
});
test('checkpoint retry waits beyond transport join for actual admission; connection retry stays transport-only',async()=>{
 const f=fixture({recoveryClock:clock()}),peer=await f.connect();f.status(admissionStatus());await drain();
 const transport=await peer.recover({kind:'retry-connection'});assert.equal(operation(peer,transport).state,'succeeded');
 let done=false;const recovery=peer.recover({kind:'refresh-checkpoint'}).then(result=>{done=true;return result;});await drain();
 assert.equal(done,false);assert.equal(peer.getSnapshot().health.recoveryOperations.at(-1).state,'pending');
 f.status(admissionStatus(true));const result=await recovery;assert.equal(operation(peer,result).state,'succeeded');await peer.dispose();
});
test('checkpoint retry binds completion to the requested record even when global admission is ready',async()=>{
 const f=fixture({recoveryClock:clock()}),peer=await f.connect();f.status(admissionStatus(true,[otherRecord]));await drain();
 let done=false;const recovery=peer.recover({kind:'refresh-checkpoint',record}).then(result=>{done=true;return result;});await drain();assert.equal(done,false);
 f.status(admissionStatus(true,[otherRecord]));await drain();assert.equal(done,false);
 f.status(admissionStatus(true));assert.equal(operation(peer,await recovery).state,'succeeded');await peer.dispose();
});
test('checkpoint timeout fails the still-missing requested record after unrelated admission succeeds',async()=>{
 const time=clock(),f=fixture({recoveryClock:time}),peer=await f.connect();f.status(admissionStatus());await drain();
 const recovery=peer.recover({kind:'refresh-checkpoint',record});await drain();f.status(admissionStatus(true,[otherRecord]));await drain();await time.tick(30000);
 const op=operation(peer,await recovery);assert.equal(op.state,'failed');assert(peer.getSnapshot().health.failures.some(f=>f.id===op.failureId&&f.code==='admission-timeout'));await peer.dispose();
});
test('checkpoint retry settles failed on terminal admission error or disposal and ignores later success',async()=>{
 for(const terminal of [true,false]){
  const f=fixture({recoveryClock:clock()}),peer=await f.connect();f.status(admissionStatus());await drain();const recovery=peer.recover({kind:'refresh-checkpoint'});await drain();
  if(terminal){const status=admissionStatus();status.health.failures=[{id:'runtime:revoked',code:'identity-revoked',scope:{kind:'connection'},severity:'error'}];f.status(status);}else await peer.dispose();
  const op=operation(peer,await recovery);assert.equal(op.state,'failed');if(terminal){assert.equal(op.failureId,'runtime:revoked');f.status(admissionStatus(true));await drain();assert.equal(op.state,'failed');await peer.dispose();}
 }
});
test('superseded mount is cancelled and its late rejection cannot publish stale failure or retry',async()=>{
 const f=fixture(),peer=await f.connect(),target={},gate=deferred(),oldStatuses=[];let oldSurface;
 f.queueMount(gate);const old=peer.mountDesktop(target,{onSurface:id=>oldSurface=id,onStatus:status=>oldStatuses.push(status)});const rejected=assert.rejects(old,/old allocation/);
 const replacement=await peer.mountDesktop(target);assert.equal(f.mountSignals[0].aborted,true);const count=oldStatuses.length;
 gate.reject(Error('old allocation failed'));await rejected;assert.equal(oldStatuses.length,count);assert(!peer.getSnapshot().health.failures.some(f=>f.scope.surfaceId===oldSurface));
 assert.equal(peer.getSnapshot().health.surfaces.find(s=>s.id===oldSurface)?.phase,'closed');await assert.rejects(peer.recover({kind:'retry-desktop',surfaceId:oldSurface}),/Unknown/);assert.equal(replacement.cleanupCalls,0);await peer.dispose();
});
test('superseding a presented mount retires its running renderer recovery',async()=>{
 const f=fixture(),peer=await f.connect(),target={},old=await peer.mountDesktop(target);old.failRenderer();const surface=peer.getSnapshot().health.surfaces[0].id;
 const recovery=peer.recover({kind:'retry-desktop',surfaceId:surface});await drain();await peer.mountDesktop(target);assert.equal(operation(peer,await recovery).state,'failed');old.present(2);assert(!peer.getSnapshot().health.failures.some(f=>f.scope.surfaceId===surface));await peer.dispose();
});

test('partial renderer status after device loss does not fail recovery before a new presentation',async()=>{
 const f=fixture(),peer=await f.connect(),mount=await peer.mountDesktop({});mount.failRenderer();mount.recover=()=>mount.status({initialSurfaceSize:{width:800,height:600}});
 const surface=peer.getSnapshot().health.surfaces[0].id;let done=false;const recovery=peer.recover({kind:'retry-desktop',surfaceId:surface}).then(result=>{done=true;return result;});await drain();assert.equal(done,false);
 mount.present(2);assert.equal(operation(peer,await recovery).state,'succeeded');await peer.dispose();
});

test('checkpoint retry can recover after a prior timeout and rejects a failed transport attempt',async()=>{
 const time=clock(),f=fixture({recoveryClock:time}),peer=await f.connect();f.status(admissionStatus());await drain();
 const first=peer.recover({kind:'refresh-checkpoint'});await drain();await time.tick(30000);assert.equal(operation(peer,await first).state,'failed');
 let done=false;const retry=peer.recover({kind:'refresh-checkpoint'}).then(result=>{done=true;return result;});await drain();f.status(admissionStatus());await drain();assert.equal(done,false);
 f.status(admissionStatus(true));assert.equal(operation(peer,await retry).state,'succeeded');f.setFailConnect(true);const offline=admissionStatus(true);offline.health.transport={phase:'offline',authenticated:false,generation:'1'};f.status(offline);await drain();assert.equal(operation(peer,await peer.recover({kind:'refresh-checkpoint'})).state,'failed');await peer.dispose();
});

test('canonical session capability mounts the entire empty desktop without selecting a record',async()=>{
 const f=fixture(),peer=await f.connect();
 f.status({phase:'ready',desktop:{version:1,mode:'session'},records:[],health:{admission:{phase:'ready',requiredRecords:[],pendingPrerequisites:0}}});
 await Promise.resolve();
 await peer.mountDesktop({});
 assert.equal(f.mountOptionsSeen[0].app,'desktop');
 assert.equal(f.mountOptionsSeen[0].record,undefined);
 assert.equal(peer.getSnapshot().health.surfaces[0].record,undefined);
 await peer.dispose();
});

test('versioned private actions share committed device storage and never expose legacy sendAction',async()=>{
 const f=fixture(),responses=new Map(),rows=new Map();let sequence=0;
 const host='aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa',invocation='bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb';
 const route={app_peer:'7',app_id:'writer',record_id:'tab:1',resource:'document',action:'insert',revision:null};
 const catalogue={version:1,surfaces:[{peer_id:'7',app_id:'writer',record_id:'tab:1',actions:[{resource:'document',action:'insert',contract:{confirmation:{mode:'none'}}}]}]};
 f.storage.commit=async writes=>{for(const write of writes)rows.set(write.key,structuredClone(write));};
 f.storage.hydrate=async()=>[...rows.values()];
 f.runtime.peer_action_context=()=>JSON.stringify({room:'r',sender:'d',senderRoot:'root',target:'native',targetRoot:'root',certificate:'exact'});
 f.runtime.peer_action_request=(_id,raw)=>{const message=JSON.parse(raw),id=String(++sequence);if(message.type==='invoke_bound'){const stored=[...rows.values()].find(row=>JSON.parse(row.key).type==='privateActions');assert(stored);assert.equal(JSON.parse(new TextDecoder().decode(stored.bytes)).entries[0].invocation,invocation);}
 responses.set(id,message.type==='catalogue_refresh'?{type:'broker_catalogue',host,catalogue}:{type:'observation',host,invocation,update:{state:'succeeded',invocation,output:{resources:[]}},uncertain:false,cancellation_requested:false});return id;};
 f.runtime.peer_action_response=(_id,id)=>JSON.stringify(responses.get(id));f.runtime.peer_action_forget=(_id,id)=>responses.delete(id);
 const peer=await f.connect();try{assert.equal(peer.sendAction,undefined);assert.equal(peer.actionsV1.version,1);
 const row=await peer.actionsV1.invoke({...route,invocation});assert.equal(row.terminal,true);
 const stored=[...rows.values()].find(row=>JSON.parse(row.key).type==='privateActions');assert.equal(JSON.parse(stored.key).workspace,'r');assert.equal(stored.actor,null);
 }finally{await peer.dispose();}assert.equal(f.counts().held,0);
});

// The real host awaits assets and queues Winit allocation after the ready callback.
// A signed checkpoint can invalidate that earlier admission before allocation.
const pendingDesktopAdmission='canonical desktop records are not admitted';
function desktopAdmission(ready){
 const value=admissionStatus(true);
 value.desktop={version:1,mode:'session'};
 if(!ready){value.phase='admitting';value.health.admission.phase='syncing';value.health.admission.pendingPrerequisites=1;}
 return value;
}
test('desktop allocation waits for a fresh admitted checkpoint after readiness changes in the host gap',async()=>{
 const f=fixture({recoveryClock:clock()}),peer=await f.connect();
 f.status(desktopAdmission(true));await drain();
 const gap=deferred(),statuses=[];f.queueMount(gap);
 let outcome='pending';const mounting=peer.mountDesktop({}, {onStatus:status=>statuses.push(status)}).then(value=>{outcome='presented';return value;},error=>{outcome='failed';return error;});
 f.status(desktopAdmission(false));await drain();gap.reject(Error(pendingDesktopAdmission));await drain();
 assert.equal(outcome,'pending','a temporary admission gap must retain the surface attempt');
 assert.equal(f.mountOptionsSeen.length,1);assert(!statuses.some(status=>status.phase==='failed'));
 const surface=peer.getSnapshot().health.surfaces[0];assert.equal(surface.phase,'initializing');assert.deepEqual(surface.failureIds,[]);
 // Surface/publication reports and incomplete signed state are not fresh readiness.
 await peer.retryPersistence();f.status(desktopAdmission(false));await drain();assert.equal(f.mountOptionsSeen.length,1);
 f.status(desktopAdmission(true));await drain();const mounted=await mounting;
 assert.equal(outcome,'presented');assert.equal(peer.getSnapshot().health.surfaces.length,1);assert.equal(f.mountOptionsSeen.length,2);
 assert.equal(f.mountOptionsSeen[0].signal,f.mountOptionsSeen[1].signal);assert.equal(f.mounts.size,1);
 await mounted.cleanup();await peer.dispose();assert.equal(f.counts().held,0);
});
test('desktop admission retry refuses stale readiness and times out without allocating again',async()=>{
 const time=clock(),f=fixture({recoveryClock:time}),peer=await f.connect();f.status(desktopAdmission(true));await drain();
 const gap=deferred();f.queueMount(gap);let outcome='pending';const mounting=peer.mountDesktop({}).then(()=>{outcome='presented';},error=>{outcome=error.message;});
 gap.reject(Error(pendingDesktopAdmission));await drain();assert.equal(outcome,'pending');
 await time.tick(30000);await mounting;assert.match(outcome,/admission.*timed out/i);assert.equal(f.mountOptionsSeen.length,1);
 assert.equal(peer.getSnapshot().health.surfaces[0].phase,'failed');await peer.dispose();
});
test('desktop admission wait terminates for permission denial, cancellation and close',async()=>{
 for(const stop of ['permission','abort','close']){
  const f=fixture({recoveryClock:clock()}),peer=await f.connect();f.status(desktopAdmission(true));await drain();
  const gap=deferred(),controller=new AbortController();f.queueMount(gap);
  let outcome='pending';const mounting=peer.mountDesktop({}, {signal:controller.signal}).then(()=>{outcome='presented';},error=>{outcome=error.message;});
  gap.reject(Error(pendingDesktopAdmission));await drain();assert.equal(outcome,'pending');
  if(stop==='permission'){const status=desktopAdmission(false);status.health.admission.phase='failed';status.health.failures=[{id:'runtime:admission',code:'identity-revoked',scope:{kind:'admission'},severity:'error',message:'This device no longer has access.'}];f.status(status);}
  else if(stop==='abort')controller.abort();else await peer.dispose();
  await drain();await mounting;assert.notEqual(outcome,'pending');assert.notEqual(outcome,'presented');
  f.status(desktopAdmission(true));await drain();assert.equal(f.mountOptionsSeen.length,1);await peer.dispose();assert.equal(f.counts().held,0);
 }
});
test('desktop admission retry is bounded if fresh ready observations repeatedly lose the allocation race',async()=>{
 const f=fixture({recoveryClock:clock()}),peer=await f.connect();f.status(desktopAdmission(true));await drain();
 const first=deferred(),second=deferred();f.queueMount(first);f.queueMount(second);
 let outcome='pending';const mounting=peer.mountDesktop({}).then(()=>{outcome='presented';},error=>{outcome=error.message;});
 first.reject(Error(pendingDesktopAdmission));await drain();assert.equal(outcome,'pending');f.status(desktopAdmission(true));await drain();
 assert.equal(f.mountOptionsSeen.length,2);second.reject(Error(pendingDesktopAdmission));await drain();await mounting;
 assert.equal(outcome,pendingDesktopAdmission);f.status(desktopAdmission(true));await drain();assert.equal(f.mountOptionsSeen.length,2);await peer.dispose();
});
test('ordinary mount failures and security errors never enter desktop admission retry',async()=>{
 for(const reason of ['document read forbidden','device revoked','canonical desktop records are not admitted: forbidden']){
  const f=fixture({recoveryClock:clock()}),peer=await f.connect();f.status(desktopAdmission(true));await drain();const gap=deferred();f.queueMount(gap);
  const mounting=peer.mountDesktop({});const rejected=assert.rejects(mounting,error=>error.message===reason);gap.reject(Error(reason));await rejected;
  f.status(desktopAdmission(true));await drain();assert.equal(f.mountOptionsSeen.length,1);await peer.dispose();
 }
});

test('desktop retry waits for rejected host cleanup and ignores its late callbacks',async()=>{
 const f=fixture({recoveryClock:clock()}),peer=await f.connect();f.status(desktopAdmission(true));await drain();
 const gap=deferred();gap.cleanup=deferred();f.queueMount(gap);let outcome='pending';const statuses=[];
 const mounting=peer.mountDesktop({}, {onStatus:status=>statuses.push(status)}).then(value=>{outcome='presented';return value;},error=>{outcome=error.message;return error;});
 gap.reject(Error(pendingDesktopAdmission));await drain();f.status(desktopAdmission(true));await drain();
 assert.equal(f.mountOptionsSeen.length,1,'the failed host still owns its storage/canvas until cleanup settles');
 gap.cleanup.resolve();await drain();assert.equal(outcome,'pending','readiness observed during cleanup is stale for the rejected attempt');
 f.mountOptionsSeen[0].onStatus({phase:'presented',gpuGeneration:99});
 assert.equal(peer.getSnapshot().health.surfaces[0].phase,'initializing','retired allocation cannot publish false presentation');
 f.status(desktopAdmission(true));await drain();const mounted=await mounting;assert.equal(outcome,'presented');
 f.mountOptionsSeen[0].onStatus({phase:'failed',gpuGeneration:100,error:'stale allocation failure'});
 assert(!statuses.some(status=>status.phase==='failed'));assert.equal(peer.getSnapshot().health.surfaces[0].phase,'presented');assert.deepEqual(peer.getSnapshot().health.surfaces[0].failureIds,[]);
 await mounted.cleanup();await peer.dispose();
});
test('connection-scoped revoked identity terminates admission wait without allocation',async()=>{
 const f=fixture({recoveryClock:clock()}),peer=await f.connect();f.status(desktopAdmission(true));await drain();const gap=deferred();f.queueMount(gap);
 let outcome='pending';const mounting=peer.mountDesktop({}).then(()=>{outcome='presented';},error=>{outcome=error.message;});gap.reject(Error(pendingDesktopAdmission));await drain();
 const status=desktopAdmission(true);status.health.failures=[{id:'session:connection',code:'identity-revoked',scope:{kind:'connection'},severity:'error',message:'Device revoked'}];f.status(status);await drain();
 assert.equal(outcome,'Device revoked');await mounting;assert.equal(f.mountOptionsSeen.length,1);await peer.dispose();
});

function progressing(progress='0',ready=false){return{phase:ready?'ready':'admitting',desktop:{version:1,mode:'session'},health:{transport:{phase:'connected',authenticated:true,generation:'1'},admission:{phase:ready?'ready':'syncing',progressRevision:progress,requiredRecords:[],pendingPrerequisites:ready?0:1},records:[],failures:[]}};}
test('active initial admission beyond ten seconds stays syncing and becomes ready',async()=>{
 const time=clock(),f=fixture({recoveryClock:time,initialStatus:progressing()}),peer=await f.connect();
 await time.tick(9000);f.status(progressing('1'));await time.tick(1001);assert.equal(peer.getSnapshot().health.admission.phase,'syncing');assert.equal(peer.getSnapshot().health.failures.length,0);
 await time.tick(7999);f.status(progressing('2'));await time.tick(600);f.status(progressing('3',true));await drain();assert.equal(peer.getSnapshot().health.admission.phase,'ready');assert.equal(f.syncs.length,0);assert.equal(f.counts().attempts,1);assert.equal(f.mounts.size,0);await peer.dispose();
});
test('enrollment delay does not spend authenticated admission recovery budget',async()=>{
 const time=clock(),gate=deferred(),entered=deferred(),states=[];const f=fixture({recoveryClock:time,initialStatus:progressing(),enroll:async()=>{entered.resolve();await gate.promise;return{ok:true,text:async()=>'{raw}'};}});
 const connecting=f.connect({onStatus:s=>states.push(s)});await entered.promise;await time.tick(12000);assert(!states.some(s=>s.health?.failures.some(f=>f.code==='admission-timeout')));gate.resolve();const peer=await connecting;await time.tick(9999);assert.equal(f.syncs.length,0);await time.tick(1);assert.equal(f.syncs.length,1);await peer.dispose();
});
test('missing checkpoint coalesces same-session recovery and fresh record predicates',async()=>{
 const time=clock(),f=fixture({recoveryClock:time,initialStatus:progressing()}),peer=await f.connect();const a=peer.recover({kind:'refresh-checkpoint',record}),b=peer.recover({kind:'refresh-checkpoint',record:otherRecord});await drain();
 await time.tick(10000);assert.equal(f.syncs.length,1);assert.equal(f.counts().attempts,1);f.status({...admissionStatus(true),health:{...admissionStatus(true).health,transport:{phase:'connected',authenticated:true,generation:'1'}}});await drain();assert.equal(operation(peer,await a).state,'succeeded');let done=false;b.then(()=>done=true);await drain();assert.equal(done,false);
 f.status({...admissionStatus(true,[record,otherRecord]),health:{...admissionStatus(true,[record,otherRecord]).health,transport:{phase:'connected',authenticated:true,generation:'1'}}});assert.equal(operation(peer,await b).state,'succeeded');await peer.dispose();
});
test('permanent missing state exhausts exactly two sends and retains mounted admitted state',async()=>{
 const time=clock(),f=fixture({recoveryClock:time,initialStatus:progressing('0',true)}),peer=await f.connect(),mount=await peer.mountDesktop({});f.status(progressing());await drain();
 await time.tick(10000);assert.equal(f.syncs.length,1);f.status(progressing());await time.tick(10000);assert.equal(f.syncs.length,2);await time.tick(10000);
 assert.equal(peer.getSnapshot().health.admission.phase,'syncing');assert(peer.getSnapshot().health.failures.some(f=>f.code==='admission-timeout'));f.status(progressing());await time.tick(60000);assert.equal(f.syncs.length,2);assert.equal(f.mounts.size,1);mount.input();f.status(progressing('1',true));await drain();assert(!peer.getSnapshot().health.failures.some(f=>f.code==='admission-timeout'));await peer.dispose();
});
test('useful progress changes idle spacing but never the absolute episode lifetime',async()=>{
 const time=clock(),f=fixture({recoveryClock:time,initialStatus:progressing()}),peer=await f.connect();await time.tick(9000);f.status(progressing('1'));await time.tick(9999);assert.equal(f.syncs.length,0);await time.tick(1);assert.equal(f.syncs.length,1);f.status(progressing('2'));await time.tick(11000);assert(peer.getSnapshot().health.failures.some(f=>f.code==='admission-timeout'));assert.equal(f.syncs.length,2);await peer.dispose();
});
test('authorization denial or close terminates catch-up without later sends',async()=>{
 for(const close of [true,false]){const time=clock(),f=fixture({recoveryClock:time,initialStatus:progressing()}),peer=await f.connect();await time.tick(5000);if(close)await peer.dispose();else{const value=progressing();value.health.failures=[{id:'runtime:revoked',code:'identity-revoked',scope:{kind:'admission'},severity:'error'}];f.status(value);}await time.tick(60000);assert.equal(f.syncs.length,0);if(!close)await peer.dispose();}
});
test('desktop admission wait shares accepted progress beyond ten seconds',async()=>{
 const time=clock(),f=fixture({recoveryClock:time,initialStatus:progressing('0',true)}),peer=await f.connect();const gap=deferred();f.queueMount(gap);let outcome='pending';const mounting=peer.mountDesktop({}).then(m=>{outcome='ready';return m;},e=>{outcome=e.message;});await drain();f.status(progressing());gap.reject(Error(pendingDesktopAdmission)); await drain();await time.tick(9000);f.status(progressing('1'));await time.tick(9000);assert.equal(outcome,'pending');f.status(progressing('2'));await time.tick(600);f.status(progressing('3',true));await mounting;assert.equal(outcome,'ready');assert.equal(f.mountOptionsSeen.length,2);await peer.dispose();
});
function privateTransport(f){
 const sent=[],forgot=[];let next=0;Object.assign(f.runtime,{peer_action_context:()=>JSON.stringify({room:'r',sender:'s',senderRoot:'root',target:'t',targetRoot:'root',certificate:'cert'}),peer_action_request:(_id,raw)=>{sent.push(JSON.parse(raw));return String(++next);},peer_action_response:()=>null,peer_action_forget:(id,request)=>forgot.push([id,request])});return{sent,forgot};
}
test('private discovery timeout is typed and forgets correlation once',async()=>{
 const time=clock(),f=fixture({recoveryClock:time,initialStatus:progressing('0',true)}),transport=privateTransport(f),peer=await f.connect();const pending=peer.actionsV1.catalogue(),rejected=assert.rejects(pending,e=>e.code==='transport-timeout'&&e.stage==='discovery'&&e.retryable===true);await drain();await time.tick(10000);await rejected;assert.equal(transport.forgot.length,1);await peer.dispose();assert.equal(transport.forgot.length,1);
});
test('private discovery abort and intent deadline promptly forget correlation',async()=>{
 for(const cancel of ['abort','deadline']){const time=clock(),f=fixture({recoveryClock:time,initialStatus:progressing('0',true)}),transport=privateTransport(f),peer=await f.connect(),abort=new AbortController();const pending=peer.actionsV1.catalogue({signal:abort.signal,deadlineAtMs:100}),rejected=assert.rejects(pending);await drain();if(cancel==='abort')abort.abort();else await time.tick(100);await rejected;await drain();assert.equal(transport.forgot.length,1);await peer.dispose();assert.equal(transport.forgot.length,1);}
});
test('manual refresh joins the active budget and only explicit exhausted retry starts another',async()=>{
 const time=clock(),f=fixture({recoveryClock:time,initialStatus:progressing()}),peer=await f.connect();await time.tick(9000);const a=peer.recover({kind:'refresh-checkpoint',record});await drain();await time.tick(11000);assert.equal(f.syncs.length,2);const b=peer.recover({kind:'refresh-checkpoint',record:otherRecord});await drain();await time.tick(10000);assert.equal(operation(peer,await a).state,'failed');assert.equal(operation(peer,await b).state,'failed');assert.equal(f.syncs.length,2);await time.tick(60000);assert.equal(f.syncs.length,2);
 const retry=peer.recover({kind:'refresh-checkpoint',record});await drain();await time.tick(10000);assert.equal(f.syncs.length,3);f.status(admissionStatus(true));assert.equal(operation(peer,await retry).state,'succeeded');await peer.dispose();
});
test('repeated rejected reports and public generations cannot restart an exhausted episode',async()=>{
 const time=clock(),f=fixture({recoveryClock:time,initialStatus:progressing()}),peer=await f.connect();for(let n=0;n<30;n++){const value=progressing();value.health.generation=String(n+1);value.health.failures=[{id:'runtime:packet',code:'prerequisite-missing',scope:{kind:'admission'},severity:'notice'}];f.status(value);await time.tick(1000);}assert.equal(f.syncs.length,2);assert(peer.getSnapshot().health.failures.some(f=>f.code==='admission-timeout'));
 f.status(progressing());await time.tick(30000);assert.equal(f.syncs.length,2);const restored=progressing();restored.health.transport.generation='2';f.status(restored);await time.tick(10000);assert.equal(f.syncs.length,3);assert.equal(f.syncs.at(-1)[1],'2');await peer.dispose();
});
test('top-level ready does not clear a record timeout or real runtime denial',async()=>{
 const time=clock(),f=fixture({recoveryClock:time,initialStatus:progressing()}),peer=await f.connect();const task=peer.recover({kind:'refresh-checkpoint',record});await drain();await time.tick(30000);const op=operation(peer,await task);f.status(admissionStatus(true,[otherRecord]));await drain();assert(peer.getSnapshot().health.failures.some(f=>f.id===op.failureId));const denial=admissionStatus(true,[otherRecord]);denial.health.failures=[{id:'runtime:denied',code:'permission-required',scope:{kind:'admission',record},severity:'error'}];f.status(denial);await drain();assert(peer.getSnapshot().health.failures.some(f=>f.id==='runtime:denied'));assert(peer.getSnapshot().health.failures.some(f=>f.id===op.failureId));await peer.dispose();
});
test('explicit record retry preserves another record timeout until that record is admitted',async()=>{
 const time=clock(),f=fixture({recoveryClock:time,initialStatus:progressing()}),peer=await f.connect();const a=peer.recover({kind:'refresh-checkpoint',record}),b=peer.recover({kind:'refresh-checkpoint',record:otherRecord});await drain();await time.tick(30000);const failure=operation(peer,await b).failureId;await a;const retry=peer.recover({kind:'refresh-checkpoint',record});await drain();assert(peer.getSnapshot().health.failures.some(f=>f.id===failure));f.status(admissionStatus(true,[record]));await retry;assert(peer.getSnapshot().health.failures.some(f=>f.id===failure));await peer.dispose();
});
test('cancelled private admission behind peer persistence cannot begin its storage commit',async()=>{
 const time=clock(),f=fixture({recoveryClock:time,initialStatus:progressing('0',true)});const transport=privateTransport(f);
 const host='aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa',invocation='bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb';
 f.runtime.peer_action_response=()=>JSON.stringify({type:'broker_catalogue',host,catalogue:{version:1,surfaces:[{peer_id:'7',app_id:'writer',record_id:'tab:1',actions:[{resource:'document',action:'insert',contract:{confirmation:{mode:'none'}}}]}]}});
 const peer=await f.connect(),hold=deferred(),entered=deferred();let writes=0;
 f.storage.commit=async()=>{writes++;entered.resolve();await hold.promise;};f.setFailCommit(false);const persistence=peer.retryPersistence();await entered.promise;
 const abort=new AbortController(),pending=peer.actionsV1.invoke({invocation,app_peer:'7',app_id:'writer',record_id:'tab:1',resource:'document',action:'insert'},{signal:abort.signal});await drain();assert.equal(peer.actionsV1.getInvocationStage(invocation),'admitting');abort.abort();await assert.rejects(pending);hold.resolve();await persistence;await drain();assert.equal(writes,1);assert.equal(peer.actionsV1.getSnapshot().entries.length,0);assert.equal(transport.sent.filter(m=>m.type==='invoke_bound').length,0);await peer.dispose();
});
test('session close aborts private polling immediately and ignores its late transport generation',async()=>{
 const time=clock(),f=fixture({recoveryClock:time,initialStatus:progressing('0',true)}),transport=privateTransport(f),peer=await f.connect();const task=peer.actionsV1.catalogue(),rejected=assert.rejects(task);await drain();await peer.dispose();await rejected;assert.equal(transport.forgot.length,1);await time.tick(60000);assert.equal(transport.forgot.length,1);
});
test('regressing or absent progress revisions cannot postpone the idle sync',async()=>{
 const time=clock(),f=fixture({recoveryClock:time,initialStatus:progressing()}),peer=await f.connect();await time.tick(9000);f.status(progressing('1'));await time.tick(9000);f.status(progressing('0'));await drain();const absent=progressing();delete absent.health.admission.progressRevision;f.status(absent);await time.tick(1000);assert.equal(f.syncs.length,1);await peer.dispose();
});
test('already terminal admission or storage failure settles checkpoint recovery without another projection',async()=>{
 for(const code of ['permission-required','record-install-failed','storage-transaction-failed']){const time=clock(),f=fixture({recoveryClock:time,initialStatus:progressing()}),peer=await f.connect();const value=progressing();value.health.failures=[{id:'runtime:denied',code,scope:{kind:code.startsWith('storage')?'storage':'admission'},severity:'error'}];f.status(value);await drain();let done=false;const task=peer.recover({kind:'refresh-checkpoint'}).then(result=>{done=true;return result;});await drain();assert.equal(done,true,code);assert.equal(operation(peer,await task).state,'failed');await time.tick(60000);assert.equal(f.syncs.length,0);await peer.dispose();}
});
test('concrete Rust transport rejection is typed at the private API boundary without retrying authorization',async()=>{
 for(const message of ['action transport disconnected','device cert has expired','device is absent from governed roster']){const time=clock(),f=fixture({recoveryClock:time,initialStatus:progressing('0',true)});privateTransport(f);const peer=await f.connect();f.runtime.peer_action_context=()=>{throw message;};await assert.rejects(peer.actionsV1.catalogue(),error=>message==='action transport disconnected'?error.code==='transport-disconnected'&&error.stage==='discovery'&&error.retryable===true:error===message);await peer.dispose();}
});
test('storage failure after renderer joins admission recovery rejects immediately and preserves saving failure',async()=>{
 const time=clock(),f=fixture({recoveryClock:time}),peer=await f.connect();f.status(desktopAdmission(true));await drain();
 const gap=deferred();f.queueMount(gap);let outcome='pending';
 const mounting=peer.mountDesktop({}).then(()=>{outcome='presented';},error=>{outcome=error.message;});
 f.status(desktopAdmission(false));await drain();gap.reject(Error(pendingDesktopAdmission));await drain();
 assert.equal(outcome,'pending');assert.equal(f.mountOptionsSeen.length,1);assert.equal(peer.getSnapshot().health.surfaces[0].phase,'initializing');
 f.setFailCommit(true);await assert.rejects(peer.retryPersistence(),/durable write failed/);await drain();
 const storageFailure=peer.getSnapshot().health.failures.find(f=>f.scope.kind==='storage');
 assert.equal(storageFailure.code,'storage-transaction-failed');assert.equal(outcome,storageFailure.message,'renderer waiter must reject as soon as saving fails');
 await mounting;await time.tick(60000);assert.equal(f.syncs.length,0);assert.equal(f.mountOptionsSeen.length,1);
 assert.deepEqual(peer.getSnapshot().health.failures.find(f=>f.id===storageFailure.id),storageFailure);
 f.setFailCommit(false);await peer.retryPersistence();await peer.dispose();
});
test('light reports trigger persistence without a new snapshot; the commit report advances the health generation; identical reports do not notify',async()=>{
 const f=fixture(),peer=await f.connect(),notifications=[];peer.subscribe(snapshot=>notifications.push(snapshot));
 const settle=async()=>{for(let i=0;i<5;i++){await new Promise(resolve=>setTimeout(resolve,0));await drain();}};
 await settle();
 const before=peer.getSnapshot();notifications.length=0;
 let commits=0;const commit=f.storage.commit;f.storage.commit=async writes=>{commits++;return commit(writes);};
 // Publication path: between commit boundaries the runtime reports only the retained phase and the persistence trigger.
 f.setFailCommit(false);f.status({phase:'ready',persistenceNeeded:true});await settle();
 assert.equal(commits,1,'a light report with persistenceNeeded triggers persistence');
 assert.strictEqual(peer.getSnapshot(),before,'a light report publishes no new snapshot');
 assert.equal(peer.getSnapshot().health.generation,before.health.generation,'a light report does not advance the health generation');
 assert.equal(notifications.length,0,'listeners are not notified for a light report');
 // Acknowledgement path: the trigger is cleared; still nothing new for listeners.
 f.status({phase:'ready',persistenceNeeded:false});await settle();
 assert.equal(notifications.length,0);assert.strictEqual(peer.getSnapshot(),before);
 // The full report after a commit carries fresh durability and advances the generation once.
 f.setCommitStatus(()=>({phase:'ready',records:before.records,health:{transport:{phase:'connected',authenticated:true},admission:{phase:'ready',requiredRecords:[],pendingPrerequisites:0},records:[{record:{roomId:'r',appId:'dev.qos.word-processor',recordId:'tab:1'},admission:'admitted',current:{heads:['h']},editing:{mode:'online'},local:{state:'durable',committedAtMs:1},acknowledgement:{state:'covered',coversCurrentBasis:true},failureIds:[]}],failures:[]}}));
 f.setFailCommit(false);f.status({phase:'ready',persistenceNeeded:true});await settle();
 assert.equal(commits,2);
 const committed=peer.getSnapshot();
 assert(BigInt(committed.health.generation)>BigInt(before.health.generation),'the report after a commit advances the health generation');
 assert.equal(notifications.length,1,'one notification for the commit report');
 assert.equal(committed.health.records[0].local.state,'durable');
 // An identical repeated report produces no listener notification.
 const repeated=structuredClone(committed);
 f.status(repeated);f.status(repeated);await settle();
 assert.equal(notifications.length,1);assert.strictEqual(peer.getSnapshot(),committed);
 await peer.dispose();
});

test('a private-action write between two peer batches keeps its place in the persistence worker queue',async t=>{
 const idb=fakeIndexedDB(),locks=fakeLocks(),workers=fakeWorkers({locks,indexedDB:idb});t.after(()=>workers.dispose());
 const access=createStorageAccess({createWorker:workers.createWorker,inline:createInlineStorage({locks:locks.manager('page'),indexedDB:idb})});
 const settle=async(rounds=40)=>{for(let i=0;i<rounds;i++)await new Promise(resolve=>setImmediate(resolve));};
 const f=fixture({acquireStorage:access.acquireStorage}),responses=new Map();let sequence=0,revision=0,durable=0,storage;
 const host='aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa',invocation='bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb';
 const route={app_peer:'7',app_id:'writer',record_id:'tab:1',resource:'document',action:'insert',revision:null};
 const catalogue={version:1,surfaces:[{peer_id:'7',app_id:'writer',record_id:'tab:1',actions:[{resource:'document',action:'insert',contract:{confirmation:{mode:'none'}}}]}]};
 f.runtime.peer_pending_batch_bytes=()=>revision>durable?{id:`1:${revision}`,writes:[{key:'{"type":"record"}',revision:String(revision),bytes:new Uint8Array([revision]),actor:null}],records:[]}:{id:'0',writes:[],records:[]};
 f.runtime.acknowledge_peer_storage_batch=(_id,raw)=>{const value=JSON.parse(raw);assert.equal(value.epoch,storage.epoch);if(value.result.ok)durable=Number(value.batchId.split(':')[1]);};
 // Publication of the first batch is when the runtime reports the next pending write.
 f.runtime.peer_committed=()=>{if(durable===1&&revision===1){revision=2;f.status({phase:'ready',persistenceNeeded:true});}};
 f.runtime.peer_action_context=()=>JSON.stringify({room:'r',sender:'d',senderRoot:'root',target:'native',targetRoot:'root',certificate:'exact'});
 f.runtime.peer_action_request=(_id,raw)=>{const message=JSON.parse(raw),id=String(++sequence);responses.set(id,message.type==='catalogue_refresh'?{type:'broker_catalogue',host,catalogue}:{type:'observation',host,invocation,update:{state:'succeeded',invocation,output:{resources:[]}},uncertain:false,cancellation_requested:false});return id;};
 f.runtime.peer_action_response=(_id,id)=>JSON.stringify(responses.get(id));f.runtime.peer_action_forget=(_id,id)=>responses.delete(id);
 const peer=await f.connect();storage=await access.acquireStorage('p','d');
 try{
  const gate=idb.gateNextCommit();revision=1;
  const first=peer.retryPersistence();await gate.held;
  const invoked=peer.actionsV1.invoke({...route,invocation});await settle();
  assert.equal(idb.transactions().filter(tx=>tx.mode==='readwrite').length,1,'the action write waits behind the in-flight batch');
  gate.release();await first;await invoked;await peer.retryPersistence();
  const committed=idb.transactions().filter(tx=>tx.mode==='readwrite').map(tx=>tx.keys.map(id=>JSON.parse(JSON.parse(id)[1]).type));
  // intent write, then the second batch reported at publication, then the observed outcome
  assert.deepEqual(committed,[['record'],['privateActions'],['record'],['privateActions']]);
  assert.equal(durable,2);
  assert.deepEqual(idb.rows().filter(row=>JSON.parse(row.key).type==='record').map(row=>[row.revision,[...row.bytes]]),[['2',[2]]]);
  assert.equal(access.mode(),'worker');
 }finally{await storage.release();await peer.dispose();}
 assert.deepEqual(locks.held(),[]);});

test('a terminal observation reaches the next transaction boundary during continuous peer persistence',async t=>{
 const idb=fakeIndexedDB(),locks=fakeLocks(),workers=fakeWorkers({locks,indexedDB:idb});t.after(()=>workers.dispose());
 const access=createStorageAccess({createWorker:workers.createWorker,inline:createInlineStorage({locks:locks.manager('page'),indexedDB:idb})});
 const settle=async(rounds=40)=>{for(let i=0;i<rounds;i++)await new Promise(resolve=>setImmediate(resolve));};
 const f=fixture({acquireStorage:access.acquireStorage}),responses=new Map();let sequence=0,revision=0,durable=0,storage,publications=0,terminal=false;
 const host='aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa',invocation='bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb';
 const route={app_peer:'7',app_id:'writer',record_id:'tab:1',resource:'document',action:'insert',revision:null};
 const catalogue={version:1,surfaces:[{peer_id:'7',app_id:'writer',record_id:'tab:1',actions:[{resource:'document',action:'insert',contract:{confirmation:{mode:'none'}}}]}]};
 f.runtime.peer_pending_batch_bytes=()=>revision>durable?{id:`1:${revision}`,writes:[{key:'{"type":"record"}',revision:String(revision),bytes:new Uint8Array([revision]),actor:null}],records:[]}:{id:'0',writes:[],records:[]};
 f.runtime.acknowledge_peer_storage_batch=(_id,raw)=>{const value=JSON.parse(raw);assert.equal(value.epoch,storage.epoch);if(value.result.ok){durable=Number(value.batchId.split(':')[1]);if(durable<8)revision=durable+1;}};
 f.runtime.peer_committed=()=>{if(!revision)return;publications++;assert.equal(durable,8,'publication follows the complete peer drain');};
 f.runtime.peer_action_context=()=>JSON.stringify({room:'r',sender:'d',senderRoot:'root',target:'native',targetRoot:'root',certificate:'exact'});
 f.runtime.peer_action_request=(_id,raw)=>{const message=JSON.parse(raw),request=String(++sequence);responses.set(request,message.type==='catalogue_refresh'?{type:'broker_catalogue',host,catalogue}:{type:'observation',host,invocation,update:{state:message.type==='invoke_bound'?'accepted':'succeeded',invocation,output:{resources:[]}},uncertain:false,cancellation_requested:false});return request;};
 f.runtime.peer_action_response=(_id,request)=>JSON.stringify(responses.get(request));f.runtime.peer_action_forget=(_id,request)=>responses.delete(request);
 const peer=await f.connect();storage=await access.acquireStorage('p','d');await settle();
 try{
  await peer.actionsV1.invoke({...route,invocation});
  peer.actionsV1.subscribe(snapshot=>{terminal=snapshot.entries.some(row=>row.invocation===invocation&&row.terminal);});
  const before=idb.transactions().filter(tx=>tx.mode==='readwrite').length;
  const gate=idb.gateNextCommit();revision=1;const draining=peer.retryPersistence();await gate.held;
  const outcome=peer.actionsV1.status(invocation);await settle();assert.equal(terminal,false,'terminal is not exposed before its strict commit');
  gate.release();await Promise.all([draining,outcome]);
  const committed=idb.transactions().filter(tx=>tx.mode==='readwrite').slice(before).map(tx=>tx.keys.map(id=>JSON.parse(JSON.parse(id)[1]).type));
  assert.deepEqual(committed,[['record'],['privateActions'],...Array.from({length:7},()=>['record'])]);
  assert.equal(terminal,true);assert.equal(durable,8);assert.equal(publications,1);
 }finally{await storage.release();await peer.dispose();}
 assert.deepEqual(locks.held(),[]);
});

test('failed terminal storage keeps the durable nonterminal row and retries status without resubmitting the effect',async t=>{
 const idb=fakeIndexedDB(),locks=fakeLocks(),workers=fakeWorkers({locks,indexedDB:idb});t.after(()=>workers.dispose());
 const access=createStorageAccess({createWorker:workers.createWorker,inline:createInlineStorage({locks:locks.manager('page'),indexedDB:idb})});
 const f=fixture({acquireStorage:access.acquireStorage}),responses=new Map(),requests=[];let sequence=0,terminalResponse=false;
 const host='aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa',invocation='bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb';
 const route={app_peer:'7',app_id:'writer',record_id:'tab:1',resource:'document',action:'insert',revision:null};
 const catalogue={version:1,surfaces:[{peer_id:'7',app_id:'writer',record_id:'tab:1',actions:[{resource:'document',action:'insert',contract:{confirmation:{mode:'none'}}}]}]};
 f.runtime.peer_action_context=()=>JSON.stringify({room:'r',sender:'d',senderRoot:'root',target:'native',targetRoot:'root',certificate:'exact'});
 f.runtime.peer_action_request=(_id,raw)=>{const message=JSON.parse(raw),request=String(++sequence);requests.push(message.type);responses.set(request,message.type==='catalogue_refresh'?{type:'broker_catalogue',host,catalogue}:{type:'observation',host,invocation,update:{state:terminalResponse?'succeeded':'accepted',invocation,output:{resources:[]}},uncertain:false,cancellation_requested:false});return request;};
 f.runtime.peer_action_response=(_id,request)=>JSON.stringify(responses.get(request));f.runtime.peer_action_forget=(_id,request)=>responses.delete(request);
 const peer=await f.connect();
 try{
  await peer.actionsV1.invoke({...route,invocation});terminalResponse=true;idb.failNextPut('QuotaExceededError');
  await assert.rejects(peer.actionsV1.status(invocation),error=>error.name==='QuotaExceededError');
  assert.equal(peer.actionsV1.getSnapshot().entries[0].terminal,false);
  assert.equal((await peer.actionsV1.status(invocation)).terminal,true);
  assert.equal(requests.filter(type=>type==='invoke_bound').length,1);
  assert.equal(requests.filter(type=>type==='status').length,2);
 }finally{await peer.dispose();}
 assert.deepEqual(locks.held(),[]);
});

test('session close waits for a terminal transaction already started at the worker boundary',async t=>{
 const idb=fakeIndexedDB(),locks=fakeLocks(),workers=fakeWorkers({locks,indexedDB:idb});t.after(()=>workers.dispose());
 const access=createStorageAccess({createWorker:workers.createWorker,inline:createInlineStorage({locks:locks.manager('page'),indexedDB:idb})});
 const settle=async(rounds=40)=>{for(let i=0;i<rounds;i++)await new Promise(resolve=>setImmediate(resolve));};
 const f=fixture({acquireStorage:access.acquireStorage}),responses=new Map();let sequence=0,terminalResponse=false;
 const host='aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa',invocation='bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb';
 const route={app_peer:'7',app_id:'writer',record_id:'tab:1',resource:'document',action:'insert',revision:null};
 const catalogue={version:1,surfaces:[{peer_id:'7',app_id:'writer',record_id:'tab:1',actions:[{resource:'document',action:'insert',contract:{confirmation:{mode:'none'}}}]}]};
 f.runtime.peer_action_context=()=>JSON.stringify({room:'r',sender:'d',senderRoot:'root',target:'native',targetRoot:'root',certificate:'exact'});
 f.runtime.peer_action_request=(_id,raw)=>{const message=JSON.parse(raw),request=String(++sequence);responses.set(request,message.type==='catalogue_refresh'?{type:'broker_catalogue',host,catalogue}:{type:'observation',host,invocation,update:{state:terminalResponse?'succeeded':'accepted',invocation,output:{resources:[]}},uncertain:false,cancellation_requested:false});return request;};
 f.runtime.peer_action_response=(_id,request)=>JSON.stringify(responses.get(request));f.runtime.peer_action_forget=(_id,request)=>responses.delete(request);
 const peer=await f.connect();await peer.actionsV1.invoke({...route,invocation});terminalResponse=true;
 const gate=idb.gateNextCommit(),outcome=peer.actionsV1.status(invocation),settlement=Promise.allSettled([outcome]);await gate.held;
 let closed=false;const disposal=peer.dispose().then(()=>{closed=true;});await settle();assert.equal(closed,false,'lease release waits for the started terminal transaction');
 gate.release();await settlement;await disposal;
 const stored=idb.rows().find(row=>JSON.parse(row.key).type==='privateActions');
 const disk=JSON.parse(new TextDecoder().decode(stored.bytes));assert.equal(disk.entries[0].terminal,true);
 assert.equal(sequence,4,'close does not resend the admitted effect');assert.deepEqual(locks.held(),[]);
});

test('private replies wake their waiter immediately without polling or entering public snapshots',async t=>{
 const time=clock(),f=fixture({recoveryClock:time,initialStatus:progressing('0',true)}),transport=privateTransport(f);
 let response=null,reads=0;
 f.runtime.peer_action_notifications=()=>true;
 f.runtime.peer_action_response=()=>{reads++;return response&&JSON.stringify(response);};
 const peer=await f.connect();t.after(()=>peer.dispose());
 let outcome='pending';const request=peer.actionsV1.catalogue().then(()=>{outcome='ready';},()=>{outcome='rejected';});
 await drain();const initialReads=reads;await time.tick(24);assert.equal(reads,initialReads);
 response={type:'broker_catalogue',host:'aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa',catalogue:{version:1,surfaces:[]}};
 f.status({actionResponseReady:true});await drain();
 assert.equal(outcome,'ready','a delivered reply must not wait for the next 25ms timer');
 assert.equal(peer.getSnapshot().actionResponseReady,undefined);assert.equal(transport.forgot.length,1);
 await request;const completedReads=reads;await time.tick(10000);assert.equal(reads,completedReads);
});
