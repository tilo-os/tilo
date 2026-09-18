import {commitPeerWrites,commitPeerBatches} from './peer-persistence.mjs';
import {createRecoveryCoordinator} from './peer-recovery.mjs';
import {retryableMemo} from './asset-recovery.mjs';
const recoveryOwners=new Set();
export const pendingPeerRecoveryCount=()=>recoveryOwners.size;
export async function retryPendingPeerRecovery(){const results=await Promise.allSettled([...recoveryOwners].map(close=>close()));const failure=results.find(result=>result.status==='rejected');if(failure)throw failure.reason;}
export function createConnectPeer({init,runtime,mountDesktop,acquireStorage,acquireIdentity,fetchEnrollment=fetch,session=globalThis.sessionStorage,recoveryClock={},events=globalThis}){
const initialize=retryableMemo(init);
return async function connectPeer(options={}){
  const {enrollmentBaseUrl='/qos-peer/enrollment',signal}=options;
  let lease;
  let id,closed=false,closing=false,snapshot={phase:'authenticating',records:[]},connection,storage,namespace,raw,flush=Promise.resolve(),failed=false,generation=0;
  const subscribers=new Set(),mounts=new Set(),failedMounts=new Set(),pendingMounts=new Set(),mountControllers=new Set();
  let reconnecting,closePromise,setup,operationId=0,surfaceId=0,healthRevision=0n,saving,admissionTimer=null,admissionDeadline=null,admissionRevision=0;
  const admissionWaiters=new Set();
  const operations=new Map(),surfaces=new Map(),surfaceWaiters=new Map(),surfaceAttempts=new Map(),surfaceTargets=new WeakMap();
  let health={version:1,generation:'0',transport:{phase:'connecting',authenticated:false,lastRelayContactAtMs:null,lastSessionPeerContactAtMs:null,retry:{attempt:0,nextAttemptAtMs:null,inFlight:false}},admission:{phase:'waiting',requiredRecords:[],pendingPrerequisites:0,deadlineAtMs:null},records:[],surfaces:[],capabilities:{automaticRecovery:true,retrySaving:true,recoverRenderer:true,durablePeerAcknowledgement:false},failures:[],recoveryOperations:[]};
  const timerSet=recoveryClock.setTimeout??globalThis.setTimeout,timerClear=recoveryClock.clearTimeout??globalThis.clearTimeout,now=recoveryClock.now??Date.now;
  const report=next=>{if(next.health){if(next.health.admission||next.health.records)admissionRevision++;health={...health,...next.health,transport:{...health.transport,...next.health.transport,retry:health.transport.retry},failures:[...health.failures.filter(f=>!f.id.startsWith('runtime:')),...(next.health.failures??[])]};}health={...health,generation:String(++healthRevision),admission:{...health.admission,deadlineAtMs:admissionDeadline}};snapshot={...snapshot,...next,health};options.onStatus?.(snapshot);for(const listener of subscribers)listener(snapshot);for(const waiter of admissionWaiters)waiter.check();};
  async function persist(publish=true){
    if(id===undefined||!storage||closed||failed)return;
    flush=flush.catch(()=>{}).then(async()=>{
      if(closed||failed)return;
      try{
        // The journal (including replay floors/outbox) and all canonical records
        // share this one strict IDB transaction. Network publication follows ack.
        const result=runtime.peer_pending_batch
          ?await commitPeerBatches({pending:()=>JSON.parse(runtime.peer_pending_batch(id)),commit:writes=>storage.commit(writes),acknowledge:value=>runtime.acknowledge_peer_storage_batch(id,JSON.stringify(value)),epoch:storage.epoch,publish:()=>{if(publish)runtime.peer_committed(id);}})
          :await commitPeerWrites({pending:()=>JSON.parse(runtime.peer_pending_writes(id)),commit:writes=>storage.commit(writes),acknowledge:write=>runtime.acknowledge_storage(namespace.profile,namespace.device,BigInt(write.revision),undefined),publish:()=>{if(publish)runtime.peer_committed(id);}});
        clearFailure('storage');
        if(result.publicationError){connectionFailure(result.publicationError);coordinator.schedule();}

      }catch(error){failed=true;setFailure('storage',error);report({error:String(error),persistence:'failed'});throw error;}
    });
    return flush;
  }
  const update=value=>{if(closed||closing)return;let next=JSON.parse(value);if(failed)next={...next,phase:'incomplete',error:snapshot.error,persistence:'failed'};if(next.phase==='ready'){if(admissionTimer!==null)timerClear(admissionTimer);admissionTimer=null;admissionDeadline=null;clearFailure('admission');}report(next);if(next.health?.transport?.phase==='offline'||next.phase==='failed'){coordinator.schedule();}if(next.phase==='ready'&&next.health?.transport?.authenticated){coordinator.reset();}if(next.persistenceNeeded)void persist().catch(()=>{});};
  async function post(route,body){
    const response=await fetchEnrollment(`${enrollmentBaseUrl}/${route}`,{method:'POST',headers:{'Content-Type':'application/json'},body,signal,cache:'no-store',credentials:'same-origin'});
    const text=await response.text();
    if(!response.ok){let reason;try{const value=JSON.parse(text);reason=typeof value.error==='string'?value.error:undefined;}catch{}throw Error(`Enrollment HTTP ${response.status}${reason?`: ${reason}`:''}`);}
    return text;
  }
  async function refreshEnrollment(){
    const challenge=await post('challenge',runtime.identity_request(lease.material));
    const next=await post('complete',runtime.identity_complete(lease.material,challenge));
    // Opaque signed timestamps go directly to Rust. Pin authority before dial.
    const bound=runtime.bind_identity(lease.material,challenge,next);
    const nextNamespace=JSON.parse(runtime.peer_enrollment(next));
    if(namespace&&JSON.stringify(namespace)!==JSON.stringify(nextNamespace))throw Error('Enrolled workspace changed; retained owner requires explicit migration');
    await lease.save(bound);raw=next;namespace=nextNamespace;
    if(!storage){storage=await acquireStorage(namespace.profile,namespace.device);if(storage.fresh){runtime.refresh_storage_lease(namespace.profile,namespace.device);storage.fresh=false;}}
  }
  function clearFailure(scope){health={...health,failures:health.failures.filter(f=>f.scope.kind!==scope)};}
  function setFailure(scope,error){
    const text=String(error);let code=scope==='storage'?'storage-transaction-failed':'transport-unavailable';
    if(/revoked/i.test(text))code='identity-revoked';else if(/certificate|signature|root mismatch|binding mismatch|HTTP 40[13]/i.test(text))code='certificate-rejected';else if(error?.name==='QuotaExceededError')code='storage-quota';else if(error?.name==='SecurityError')code='storage-denied';
    const terminal=['identity-revoked','certificate-rejected'].includes(code);if(terminal&&error&&typeof error==='object')error.terminal=true;
    clearFailure(scope);health={...health,failures:[...health.failures,{id:`session:${scope}`,code,scope:{kind:scope},severity:'error',message:scope==='storage'?'Changes could not be saved on this device.':terminal?'This device needs authorization to reconnect.':'Connection is unavailable. Your saved work is retained.',recovery:[{kind:scope==='storage'?'retry-saving':terminal?'reauthenticate':'retry-connection'}],occurredAtMs:Date.now()}]};return terminal;
  }
  function connectionFailure(error){const terminal=setFailure('connection',error);health={...health,transport:{...health.transport,phase:terminal?'failed':'offline',authenticated:false}};report({error:String(error)});}
  function admissionAttempt(){
    if(admissionTimer!==null)timerClear(admissionTimer);admissionDeadline=now()+10000;
    admissionTimer=timerSet(()=>{admissionTimer=null;if(closed||closing)return;clearFailure('admission');health={...health,admission:{...health.admission,phase:'failed'},failures:[...health.failures,{id:'session:admission',code:'admission-timeout',scope:{kind:'admission'},severity:'error',message:'Shared state is taking longer to load.',recovery:[{kind:'refresh-checkpoint'}],occurredAtMs:now()}]};report({});},10000);
  }
  const connect=async(old)=>{
    admissionAttempt();await refreshEnrollment();
    if(closed||closing)throw Error('Session disposed during enrollment');
    if(old===undefined){
      const current=generation;
      connection=runtime.connect_peer(raw,lease.material,await storage.hydrate(),value=>queueMicrotask(()=>{if(current===generation)update(value);}));
      const next=await connection;
      if(closed||closing){await runtime.close_peer(next);return;}
      id=next;
    }else{
      if(typeof runtime.reconnect_peer!=='function')throw Object.assign(Error('Runtime cannot retain the state owner during reconnect'),{terminal:true});
      connection=runtime.reconnect_peer(old,raw,lease.material);await connection;
      if(closed||closing)return;
    }
    clearFailure('connection');await persist();
  };
  const coordinator=createRecoveryCoordinator({...recoveryClock,attempt:async()=>{
    try{health={...health,transport:{...health.transport,phase:'reconnecting',authenticated:false}};report({});await connect(id);}catch(error){connectionFailure(error);throw error;}
  },onState:retry=>{health={...health,transport:{...health.transport,phase:!health.transport.authenticated&&(retry.inFlight||retry.nextAttemptAtMs!==null)?'reconnecting':health.transport.phase,retry}};report({});}});
  const online=()=>{if(!closed&&!closing)void reconnect().catch(()=>{});};events?.addEventListener?.('online',online);
  function surfaceStatus(surface,status){
    const prior=health.surfaces.find(row=>row.id===surface);if(!prior)return;
    const generation=typeof status.gpuGeneration==='string'?status.gpuGeneration:Number.isSafeInteger(status.gpuGeneration)?String(status.gpuGeneration):prior.generation;
    if(!/^(0|[1-9][0-9]*)$/.test(generation)||BigInt(generation)<BigInt(prior.generation))return;
    const phase=status.phase==='gpu-pending'?'initializing':status.phase==='presented'?'presented':status.phase==='failed'?'failed':status.phase==='closed'?'closed':prior.phase;
    if(status.error&&(status.phase==='failed'||status.asset==='unavailable')){
      const code=status.asset==='unavailable'?'asset-unavailable':/device.*lost/i.test(status.error)?'gpu-device-lost':'renderer-initialization-failed';
      const asset=status.asset==='unavailable';const failureId=asset?`asset:${surface}:${status.assetId}`:`surface:${surface}`;health={...health,failures:[...health.failures.filter(f=>f.id!==failureId),{id:failureId,code,scope:asset?{kind:'asset',surfaceId:surface,assetId:status.assetId}:{kind:'renderer',surfaceId:surface},severity:'error',message:asset?'An image could not be loaded.':'The desktop could not be displayed. Your work is retained.',recovery:[asset?{kind:'retry-asset',surfaceId:surface,assetId:status.assetId}:{kind:'retry-desktop',surfaceId:surface}],occurredAtMs:Date.now()}]};
    }
    const waiter=surfaceWaiters.get(surface);
    if(status.phase==='failed'&&waiter&&BigInt(generation)>=waiter.generation){surfaceWaiters.delete(surface);waiter.reject(Error('Desktop recovery failed'));}
    if(phase==='presented'&&waiter&&BigInt(generation)>waiter.generation){surfaceWaiters.delete(surface);health={...health,failures:health.failures.filter(f=>f.id!==`surface:${surface}`)};waiter.resolve();}
    if(status.persistence==='failed')setFailure('storage',Error('Local storage transaction failed'));
    health={...health,surfaces:health.surfaces.map(row=>row.id===surface?{...row,phase,generation,failureIds:health.failures.filter(f=>f.scope.surfaceId===surface).map(f=>f.id)}:row)};report({});
  }
  async function settleMounts(items){
    const results=await Promise.allSettled([...items].map(mount=>mount.cleanup()));
    const failure=results.find(result=>result.status==='rejected');
    if(failure)throw failure.reason;
  }
  function close(){
    if(closed)return Promise.resolve();
    if(closePromise)return closePromise;
    closing=true;if(admissionTimer!==null)timerClear(admissionTimer);admissionTimer=null;admissionDeadline=null;coordinator.dispose();events?.removeEventListener?.('online',online);
    closePromise=(async()=>{
      for(const waiter of admissionWaiters)waiter.reject(Error('Session closed'));
      for(const waiter of surfaceWaiters.values())waiter.reject(Error('Session closed'));surfaceWaiters.clear();
      for(const entry of surfaceAttempts.values())entry.cancelled=true;
      for(const controller of mountControllers)controller.abort();surfaceAttempts.clear();
      await Promise.allSettled([...pendingMounts]);
      await reconnecting?.catch(()=>{});
      await setup?.catch(()=>{});
      await connection?.catch(()=>{});
      // Stop the replaceable endpoint even when component settlement retains
      // custody. Keep its durable owner addressable for the next retry.
      if(id!==undefined)await runtime.disconnect_peer(id);
      await settleMounts(mounts);
      failed=false;await persist(false);
      generation++;if(id!==undefined){await runtime.close_peer(id);id=undefined;}
      await storage?.release();await lease?.release();closed=true;recoveryOwners.delete(close);health={...health,transport:{...health.transport,phase:'closed',authenticated:false,retry:{attempt:0,nextAttemptAtMs:null,inFlight:false}},surfaces:health.surfaces.map(surface=>({...surface,phase:'closed'}))};report({phase:'closed',error:null});
    })().catch(error=>{recoveryOwners.add(close);report({phase:'incomplete',error:String(error),recoveryAvailable:true});throw error;}).finally(()=>{closing=false;closePromise=undefined;});
    return closePromise;
  }
  function reconnect(){
    if(closed||closing)return Promise.reject(Error('Session closed'));
    if(reconnecting)return reconnecting;
    reconnecting=coordinator.retry().finally(()=>{reconnecting=undefined;});return reconnecting;
  }
  function retryPersistence(){if(saving)return saving;saving=(async()=>{await settleMounts(failedMounts);failed=false;for(const mount of mounts)await mount.flush?.();await persist(!reconnecting);})().finally(()=>{saving=undefined;});return saving;}
  const sameRecord=(a,b)=>a&&b&&a.roomId===b.roomId&&a.appId===b.appId&&a.recordId===b.recordId;
  function refreshCheckpoint(request,operation){
    // A transport join may precede the authoritative checkpoint. Observe a fresh
    // admission projection, then settle only the resource named by this request.
    const revision=admissionRevision;
    health={...health,failures:health.failures.filter(f=>!(f.code==='admission-timeout'&&!f.id.startsWith('runtime:')&&(!request.record||!f.scope.record||sameRecord(f.scope.record,request.record))))};
    return new Promise((resolve,reject)=>{
      let joined=false,settled=false,timeout;
      const finish=error=>{if(settled)return;settled=true;timerClear(timeout);admissionWaiters.delete(waiter);error?reject(error):resolve();};
      const waiter={reject:finish,check:()=>{
        if(admissionRevision<=revision)return;
        const row=request.record?health.records.find(row=>sameRecord(row.record,request.record)):null;
        const relevant=health.failures.filter(f=>f.severity==='error'&&(
          f.scope.kind==='connection'||(['admission','permission'].includes(f.scope.kind)&&(!request.record||!f.scope.record||sameRecord(f.scope.record,request.record)))||row?.failureIds.includes(f.id)));
        const terminal=relevant.find(f=>['identity-revoked','certificate-rejected','certificate-expired','signature-rejected','permission-required','crypto-unavailable','admission-timeout'].includes(f.code));
        if(terminal){finish(Object.assign(Error('Checkpoint admission failed'),{failureId:terminal.id}));return;}
        const ready=row=>row?.admission==='admitted'&&row.current&&row.editing.mode!=='blocked';
        const admitted=request.record?ready(row):health.admission.phase==='ready'&&health.admission.pendingPrerequisites===0&&health.admission.requiredRecords.every(record=>ready(health.records.find(row=>sameRecord(row.record,record))));
        if(joined&&admitted&&!relevant.some(f=>f.scope.kind!=='connection'))finish();
      }};
      timeout=timerSet(()=>{
        const failureId=`recovery:admission:${operation.id}`;
        const failure={id:failureId,code:'admission-timeout',scope:{kind:'admission',...(request.record?{record:request.record}:{})},severity:'error',message:'Shared state is taking longer to load.',recovery:[request],occurredAtMs:now()};
        finish(Object.assign(Error('Checkpoint recovery timed out'),{failureId}));
        health={...health,failures:[...health.failures,failure]};if(!closed&&!closing)report({});
      },10000);
      admissionWaiters.add(waiter);
      reconnect().then(()=>{joined=true;waiter.check();},finish);
    });
  }
  function recover(request){
    if(closed||closing)return Promise.reject(Error('Session closed'));
    if(!['retry-connection','refresh-checkpoint','reauthenticate','retry-saving','retry-desktop','retry-asset','review-permissions'].includes(request?.kind))return Promise.reject(Error('Unsupported recovery request'));
    if(request.kind==='retry-desktop'&&!surfaceAttempts.has(request.surfaceId))return Promise.reject(Error('Unknown desktop surface'));
    const key=JSON.stringify(request);if(operations.has(key))return operations.get(key);
    const operation={id:String(++operationId),request,state:'pending'};
    health={...health,recoveryOperations:[...health.recoveryOperations,operation]};report({});
    const task=Promise.resolve().then(async()=>{
      switch(request.kind){
        case 'retry-connection':case 'reauthenticate':await reconnect();break;
        case 'refresh-checkpoint':await refreshCheckpoint(request,operation);break;
        case 'retry-saving':await retryPersistence();break;
        case 'retry-desktop':{
          const mount=surfaces.get(request.surfaceId);if(!mount){await surfaceAttempts.get(request.surfaceId).run();break;}
          const prior=health.surfaces.find(surface=>surface.id===request.surfaceId);const generation=BigInt(prior?.generation??'0');
          await new Promise((resolve,reject)=>{
            const timeout=timerSet(()=>{surfaceWaiters.delete(request.surfaceId);reject(Error('Desktop recovery timed out'));},10000);
            surfaceWaiters.set(request.surfaceId,{generation,resolve:()=>{timerClear(timeout);resolve();},reject:error=>{timerClear(timeout);reject(error);}});
            try{Promise.resolve(mount.recover()).catch(error=>{surfaceWaiters.get(request.surfaceId)?.reject(error);surfaceWaiters.delete(request.surfaceId);});}catch(error){timerClear(timeout);surfaceWaiters.delete(request.surfaceId);reject(error);}
          });break;
        }
        case 'retry-asset':{const mount=surfaces.get(request.surfaceId);if(!mount?.retryAsset)throw Error('Asset recovery is unavailable');await mount.retryAsset(request.assetId);health={...health,failures:health.failures.filter(f=>f.id!==`asset:${request.surfaceId}:${request.assetId}`)};break;}
        case 'review-permissions':operation.state='requires-user';break;
        default:throw Error('Unsupported recovery request');
      }
      if(operation.state==='pending')operation.state='succeeded';
    }).catch(error=>{operation.state='failed';operation.failureId=error?.failureId??health.failures.at(-1)?.id;}).then(()=>{if(!closed&&!closing)report({});return{operationId:operation.id};}).finally(()=>operations.delete(key));
    operations.set(key,task);return task;
  }
  try{
    await initialize();
    lease=await acquireIdentity(enrollmentBaseUrl,session,signal);
    signal?.addEventListener('abort',()=>{void close().catch(error=>report({phase:'incomplete',error:String(error)}));},{once:true});
    if(signal?.aborted)throw signal.reason??Error('Session aborted');
    setup=connect();await setup;
    if(closed||closing)throw signal?.reason??Error('Session disposed during connection');
    return {getSnapshot:()=>snapshot,subscribe(listener){subscribers.add(listener);return()=>subscribers.delete(listener);},
      async mountDesktop(target,mountOptions={}){
        if(closed||closing)throw Error('Session closed');
        if(snapshot.phase!=='ready')throw Error('Certified record hydration is incomplete');
        const sessionDesktop=snapshot.desktop?.version===1&&snapshot.desktop.mode==='session'&&!mountOptions.app;
        const app=sessionDesktop?'desktop':(mountOptions.app??'writer');
        const record=sessionDesktop?undefined:snapshot.records.find(row=>row.app_id===(app==='writer'?'dev.qos.word-processor':'dev.qos.chat'));
        if(!sessionDesktop&&!record)throw Error('Canonical record missing');
        surfaceAttempts.get(surfaceTargets.get(target))?.cancel();
        const controller=new AbortController();mountControllers.add(controller);
        const surface=String(++surfaceId);let allocation=0;
        const entry={cancelled:false,pending:null,mounted:null,run:null,cancel:null};surfaceAttempts.set(surface,entry);surfaceTargets.set(target,surface);
        const abort=()=>{
          if(entry.cancelled)return;entry.cancelled=true;mountOptions.signal?.removeEventListener('abort',abort);controller.abort(mountOptions.signal?.reason);surfaceAttempts.delete(surface);mountControllers.delete(controller);
          surfaceWaiters.get(surface)?.reject(Error('Desktop attempt cancelled'));surfaceWaiters.delete(surface);health={...health,failures:health.failures.filter(f=>f.scope.surfaceId!==surface),surfaces:health.surfaces.map(row=>row.id===surface?{...row,phase:'closed',failureIds:[]}:row)};if(!closed&&!closing)report({});
          if(entry.mounted)void entry.mounted.cleanup().catch(()=>{});
        };
        entry.cancel=abort;
        mountOptions.signal?.addEventListener('abort',abort,{once:true});if(mountOptions.signal?.aborted)abort();
        health={...health,surfaces:[...health.surfaces,{id:surface,...(record?{record:{roomId:namespace.room,appId:record.app_id,recordId:record.record_id}}:{}),phase:'initializing',generation:'0',failureIds:[]}]};
        mountOptions.onSurface?.(surface);mountOptions.onStatus?.({phase:'gpu-pending',surfaceId:surface});report({});
        entry.run=()=>{
          if(entry.cancelled||closed||closing||surfaceTargets.get(target)!==surface)return Promise.reject(Error('Desktop attempt cancelled'));
          if(entry.pending)return entry.pending;
          const current=++allocation;
          const pending=mountDesktop(target,{...mountOptions,...namespace,signal:controller.signal,app,peer:id,record:record?.record_id||undefined,author:snapshot.deviceId,onStatus:status=>{
            if(entry.cancelled||current!==allocation||closed||closing||surfaceTargets.get(target)!==surface)return;
            surfaceStatus(surface,status);mountOptions.onStatus?.({...status,surfaceId:surface});if(status.phase==='presented'||status.persistence==='committed')void persist().catch(()=>{});
          }});
          pendingMounts.add(pending);
          entry.pending=Promise.resolve(pending).then(async mounted=>{
            if(entry.cancelled||closed||closing||current!==allocation||surfaceTargets.get(target)!==surface){await mounted.cleanup();throw Error('Session disposed while mounting');}
            const cleanup=mounted.cleanup.bind(mounted);
            mounted.cleanup=async()=>{try{await cleanup();mounts.delete(mounted);failedMounts.delete(mounted);surfaces.delete(surface);surfaceAttempts.delete(surface);mountControllers.delete(controller);mountOptions.signal?.removeEventListener('abort',abort);}catch(error){failedMounts.add(mounted);throw error;}};
            entry.mounted=mounted;mounts.add(mounted);surfaces.set(surface,mounted);
            health={...health,failures:health.failures.filter(f=>f.id!==`surface:${surface}`)};
            surfaceStatus(surface,mounted.state?.()??{phase:'presented'});mountOptions.onMount?.(mounted);return mounted;
          }).catch(error=>{
            if(!entry.cancelled&&current===allocation&&!closed&&!closing&&surfaceTargets.get(target)===surface){surfaceStatus(surface,{phase:'failed',error:String(error)});mountOptions.onStatus?.({phase:'failed',surfaceId:surface,error:String(error)});}throw error;
          }).finally(()=>{pendingMounts.delete(pending);entry.pending=null;});
          return entry.pending;
        };
        return entry.run();
      },
      reconnect,recover,
      retryPersistence,failNextWrite:()=>{storage.abortNext=true;},durableState:()=>storage.hydrate(),dispose:close};
  }catch(error){try{await close();}catch(cleanupError){throw new AggregateError([error,cleanupError],'Peer setup failed; retained recovery is available');}throw error;}
}

}
