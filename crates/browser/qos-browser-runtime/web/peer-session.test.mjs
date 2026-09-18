import {test} from 'node:test';
import assert from 'node:assert/strict';
import {createConnectPeer,pendingPeerRecoveryCount,retryPendingPeerRecovery} from './peer-session.mjs';
import {recoverableClose,settleComponentClose} from './close-recovery.mjs';
const deferred=()=>{let resolve,reject;const promise=new Promise((a,b)=>{resolve=a;reject=b;});return{promise,resolve,reject};};
function fixture({enroll,recoveryClock}={}){
 let next=0,held=0,storageRefs=0,failConnect=false,failClose=false,failCommit=false,pendingWrite=false,attempts=0,gate;
 const mountGates=[],mountSignals=[],mountOptionsSeen=[];
 const peers=new Set(),mounts=new Set(),closed=[],rebound=[],suspended=new Set();let componentOwners=0,durableRecord,statusCallback,publishFailure=false,failMount=false;
 const locks={request:async(_name,_opts,fn)=>{held++;try{return await fn({});}finally{held--;}}};
 const runtime={identity_request:()=>'{public}',identity_complete:()=>'{proof}',bind_identity:()=> 'opaque-key',peer_enrollment:()=>JSON.stringify({profile:'p',device:'d',room:'r'}),refresh_storage_lease(){},
  async connect_peer(_raw,_identity,_bytes,status){statusCallback=status;attempts++;if(gate)await gate.promise;if(failConnect)throw Error('dial failed');const id=++next;peers.add(id);status(JSON.stringify({phase:'ready',records:[{app_id:'dev.qos.word-processor',record_id:'tab:1',heads:['h']}]}));return id;},
  async reconnect_peer(id){attempts++;assert(peers.has(id));if(gate)await gate.promise;if(failConnect)throw Error('dial failed');return id;},
  async close_peer(id){closed.push(id);peers.delete(id);},async disconnect_peer(id){assert(peers.has(id));},
  peer_pending_writes(id){assert(peers.has(id),'closed peer');return JSON.stringify(pendingWrite?[{revision:1}]:[]);},acknowledge_storage(){pendingWrite=false;},peer_committed(id){assert(peers.has(id));if(publishFailure)throw Error('relay unavailable');},
  suspend_peer(id,value){assert(peers.has(id));value?suspended.add(id):suspended.delete(id);},
  rebind_peer(old,id){rebound.push([old,id]);for(const mount of mounts)mount.id=id;suspended.delete(old);},
 };
 const storage={fresh:true,hydrate:async()=>[],commit:async()=>{if(failCommit)throw Error('durable write failed');},release:async()=>{storageRefs--;}};
 const connect=createConnectPeer({init:async()=>{},runtime,recoveryClock,acquireIdentity:async()=>{held++;return{material:'opaque-key',save:async()=>{},release:async()=>{held--;}}},fetchEnrollment:enroll??(async()=>({ok:true,text:async()=>'{raw}'})),acquireStorage:async()=>{storageRefs++;return storage;},mountDesktop:async(_target,options)=>{
  mountOptionsSeen.push(options);mountSignals.push(options.signal);const mountGate=mountGates.shift();if(mountGate)await mountGate.promise;
  if(failMount)throw Error('initial fonts unavailable');
  const mount={id:next,cleanupCalls:0,flushCalls:0,async flush(){mount.flushCalls++;},state:()=>({phase:'presented',gpuGeneration:1}),recover(){options.onStatus({phase:'gpu-pending',gpuGeneration:2});},status(value){options.onStatus(value);},present(gpuGeneration){options.onStatus({phase:'presented',gpuGeneration});},failRenderer(){options.onStatus({phase:'failed',gpuGeneration:1,error:'GPU device lost'});},input(){if(!suspended.has(mount.id)){assert(peers.has(mount.id),'input addressed removed peer');}},setSuspended(){}};
  componentOwners++;
  const componentRuntime={prepare_close:()=>JSON.stringify({writes:[{revision:7,bytes:[1,4,9]}],errors:[]}),acknowledge_storage(){},finish_close(){componentOwners--;}};
  mount.cleanup=recoverableClose({begin:()=>{mount.cleanupCalls++;},settle:async()=>{
    await settleComponentClose(componentRuntime,1,{commit:async writes=>{if(failClose)throw Error('retained close write failed');durableRecord=writes[0].bytes;}},'p','d');mounts.delete(mount);
  }});
  mounts.add(mount);return mount;
 }});
 return{runtime,storage,connect,peers,mounts,closed,mountSignals,mountOptionsSeen,queueMount:gate=>mountGates.push(gate),rebound,suspended,setFailMount:v=>failMount=v,setPublishFailure:v=>{publishFailure=v;pendingWrite=true;},status:value=>statusCallback(JSON.stringify(value)),recovery:()=>({componentOwners,durableRecord}),setFailCommit:v=>{failCommit=v;pendingWrite=true;},setFailConnect:v=>failConnect=v,setFailClose:v=>failClose=v,setGate:v=>gate=v,counts:()=>({held,storageRefs,attempts})};
}
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
 let time=0,id=0;const timers=new Map();
 return{now:()=>time,setTimeout:(fn,delay)=>{timers.set(++id,{fn,at:time+delay});return id;},clearTimeout:id=>timers.delete(id),async tick(ms){time+=ms;for(const [id,timer] of [...timers])if(timer.at<=time&&timers.delete(id))timer.fn();await drain();}};
}
const record={roomId:'r',appId:'dev.qos.word-processor',recordId:'tab:1'};
const otherRecord={...record,recordId:'tab:2'};
function admissionStatus(admitted=false,records=[record]){
 return{phase:admitted?'ready':'incomplete',health:{admission:{phase:admitted?'ready':'failed',requiredRecords:records,pendingPrerequisites:admitted?0:1},records:records.map(record=>({record,admission:admitted?'admitted':'unavailable',current:admitted?{heads:['h']}:null,editing:{mode:admitted?'online':'blocked'},failureIds:[]})),failures:admitted?[]:[{id:'runtime:admission',code:'prerequisite-missing',scope:{kind:'admission'},severity:'error'}]}};
}
const operation=(peer,result)=>peer.getSnapshot().health.recoveryOperations.find(op=>op.id===result.operationId);
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
 const recovery=peer.recover({kind:'refresh-checkpoint',record});await drain();f.status(admissionStatus(true,[otherRecord]));await drain();await time.tick(10000);
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
 const first=peer.recover({kind:'refresh-checkpoint'});await drain();await time.tick(10000);assert.equal(operation(peer,await first).state,'failed');
 let done=false;const retry=peer.recover({kind:'refresh-checkpoint'}).then(result=>{done=true;return result;});await drain();f.status(admissionStatus());await drain();assert.equal(done,false);
 f.status(admissionStatus(true));assert.equal(operation(peer,await retry).state,'succeeded');f.setFailConnect(true);assert.equal(operation(peer,await peer.recover({kind:'refresh-checkpoint'})).state,'failed');await peer.dispose();
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
