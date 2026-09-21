import {createActionClient,actionError} from './action-client.mjs';
import {commitPeerWrites,commitPeerBatches,readPeerBatch} from './peer-persistence.mjs';
import {createRecoveryCoordinator} from './peer-recovery.mjs';
import {retryableMemo} from './asset-recovery.mjs';
const recoveryOwners=new Set();
function samePublicValue(left,right){
 if(Object.is(left,right))return true;
 if(!left||!right||typeof left!=='object'||typeof right!=='object'||Array.isArray(left)!==Array.isArray(right))return false;
 const leftKeys=Object.keys(left),rightKeys=Object.keys(right);if(leftKeys.length!==rightKeys.length)return false;
 return leftKeys.every(key=>Object.hasOwn(right,key)&&samePublicValue(left[key],right[key]));
}
export const pendingPeerRecoveryCount=()=>recoveryOwners.size;
export async function retryPendingPeerRecovery(){const results=await Promise.allSettled([...recoveryOwners].map(close=>close()));const failure=results.find(result=>result.status==='rejected');if(failure)throw failure.reason;}
export function createConnectPeer({init,runtime,mountDesktop,acquireStorage,acquireIdentity,fetchEnrollment=fetch,session=globalThis.sessionStorage,recoveryClock={},events=globalThis}){
const initialize=retryableMemo(init);
return async function connectPeer(options={}){
  const {enrollmentBaseUrl='/qos-peer/enrollment',signal}=options;
  let lease,actionsV1,actionRevision=0n,actionRecovery=false,actionTimer=null,actionPolling=false;
  let id,closed=false,closing=false,snapshot={phase:'authenticating',records:[]},connection,storage,namespace,raw,flush=Promise.resolve(),queuedPersist,failed=false,generation=0;
  const desktopCommitListeners=new Set(),actionResponseListeners=new Set();
  const subscribeDesktopCommits=listener=>{desktopCommitListeners.add(listener);return()=>desktopCommitListeners.delete(listener);};
  const subscribers=new Set(),mounts=new Set(),failedMounts=new Set(),pendingMounts=new Set(),mountControllers=new Set();
  let reconnecting,closePromise,setup,operationId=0,surfaceId=0,healthRevision=0n,saving,admissionTimer=null,admissionDeadline=null,admissionRevision=0,outgoingTimer=null,outgoingExpiryTimer=null,outgoingExpiryAtMs=null,outgoingExpiryChecked=null,outgoingBlocked=0,outgoingRevision=0;
  const admissionWaiters=new Set(),admissionFailures=new Map();let admissionEpisode,admissionEpoch=0;
  const outgoingWaiters=new Set();let outgoingEpisode,outgoingEpoch=0;
  const operations=new Map(),surfaces=new Map(),surfaceWaiters=new Map(),surfaceAttempts=new Map(),surfaceTargets=new WeakMap();
  let health={version:1,generation:'0',transport:{phase:'connecting',authenticated:false,lastRelayContactAtMs:null,lastSessionPeerContactAtMs:null,retry:{attempt:0,nextAttemptAtMs:null,inFlight:false}},admission:{phase:'waiting',requiredRecords:[],pendingPrerequisites:0,deadlineAtMs:null},records:[],surfaces:[],capabilities:{automaticRecovery:true,retrySaving:true,recoverRenderer:true,durablePeerAcknowledgement:false},failures:[],recoveryOperations:[]};
  const timerSet=recoveryClock.setTimeout??globalThis.setTimeout,timerClear=recoveryClock.clearTimeout??globalThis.clearTimeout,now=recoveryClock.now??Date.now;
  // Health is compared structurally only when something touched it since the
  // last published snapshot (any reassignment, or a moved admission deadline);
  // a light report between commit boundaries then costs no deep comparison.
  let publishedHealth;
  const report=(next,force=false)=>{
    if(next.health){if(next.health.admission||next.health.records)admissionRevision++;health={...health,...next.health,transport:{...health.transport,...next.health.transport,retry:health.transport.retry},failures:[...health.failures.filter(f=>!f.id.startsWith('runtime:')),...(next.health.failures??[])]};}
    for(const waiter of admissionWaiters)waiter.check();reconcileAdmission();
    const base=health,projected={...base,generation:snapshot.health?.generation??base.generation,admission:{...base.admission,deadlineAtMs:admissionDeadline}};
    const healthChanged=(base!==publishedHealth||snapshot.health?.admission?.deadlineAtMs!==admissionDeadline)&&!samePublicValue(snapshot.health,projected);
    const changed=force||healthChanged||Object.keys(next).some(key=>key!=='health'&&!samePublicValue(snapshot[key],next[key]));
    health=changed?{...projected,generation:String(++healthRevision)}:projected;publishedHealth=health;
    if(changed){snapshot={...snapshot,...next,health};options.onStatus?.(snapshot);for(const listener of subscribers)listener(snapshot);}
    for(const waiter of admissionWaiters)waiter.check();
  };
  // `force` publishes (peer_committed: outbox send plus a fresh health report)
  // even when this run committed nothing. Presented frames pass false: without a
  // commit there is nothing new to send, and the health report would be unchanged.
  async function persist(publish=true,force=true){
    if(id===undefined||!storage||closed||failed)return;
    // Coalesce only work that has not started and is still the queue tail.
    // A running drain or an intervening private-action write needs its own
    // following barrier, so new writes can never join an already-settling run.
    if(queuedPersist&&flush===queuedPersist.promise){queuedPersist.publish||=publish;queuedPersist.force||=force;return queuedPersist.promise;}
    const request={publish,force};
    request.promise=flush.catch(()=>{}).then(async()=>{
      if(queuedPersist===request)queuedPersist=undefined;
      if(closed||failed)return;
      let committed=0;
      const publication=()=>{if(request.publish&&(request.force||committed>0))runtime.peer_committed(id);};
      try{
        // The journal (including replay floors/outbox) and all canonical records
        // share this one strict IDB transaction. Network publication follows ack.
        const result=runtime.peer_pending_batch_bytes||runtime.peer_pending_batch
          ?await commitPeerBatches({pending:()=>readPeerBatch(runtime,id),commit:writes=>storage.commit(writes),acknowledge:value=>{if(value.result.ok)committed++;runtime.acknowledge_peer_storage_batch(id,JSON.stringify(value));},epoch:storage.epoch,publish:publication})
          :await commitPeerWrites({pending:()=>JSON.parse(runtime.peer_pending_writes(id)),commit:writes=>storage.commit(writes),acknowledge:write=>{committed++;runtime.acknowledge_storage(namespace.profile,namespace.device,BigInt(write.revision),undefined);},publish:publication});
        clearFailure('storage');
        if(result.publicationError){connectionFailure(result.publicationError);coordinator.schedule();}

      }catch(error){failed=true;setFailure('storage',error);report({error:String(error),persistence:'failed'});throw error;}
    });
    queuedPersist=request;flush=request.promise;
    return flush;
  }
  function actionRuntime(operation,stage){
    try{return operation();}catch(error){
      // This exact error originates in Rust action_context / peer_action_request.
      // Certificate, membership, protocol and arbitrary transport text stay fatal.
      if(String(error?.message??error)==='action transport disconnected')throw actionError('transport-disconnected',stage,'Action transport disconnected',stage==='discovery');
      throw error;
    }
  }
  function pollActions(){
    if(!actionsV1||closed||closing||actionPolling)return;
    if(actionTimer!==null)timerClear(actionTimer);actionTimer=null;actionPolling=true;
    void actionsV1.recover().catch(()=>{}).finally(()=>{actionPolling=false;if(!closed&&!closing)actionTimer=timerSet(pollActions,1000);});
  }
  // `persistenceNeeded` asks this host to commit pending writes now. It is a
  // trigger, not snapshot state: the runtime's light report between commit
  // boundaries (phase plus trigger) publishes nothing to subscribers.
  const update=value=>{if(closed||closing)return;if(actionsV1){try{runtime.peer_action_context(id);}catch{actionsV1.invalidate('identity-or-transport-unavailable');actionRecovery=false;}}const {persistenceNeeded,committedDesktop,actionResponseReady,outgoingExpiryAtMs:expiry,...parsed}=JSON.parse(value);if(actionResponseReady){for(const listener of [...actionResponseListeners])listener();if(!Object.keys(parsed).length&&!persistenceNeeded&&!committedDesktop)return;}if(committedDesktop){for(const listener of desktopCommitListeners){try{listener(committedDesktop);}catch{}}if(!Object.keys(parsed).length&&!persistenceNeeded)return;}const outgoingReport=Number.isSafeInteger(parsed.outgoingBlocked)&&parsed.outgoingBlocked>=0;if(outgoingReport){outgoingBlocked=parsed.outgoingBlocked;outgoingRevision++;outgoingExpiryAtMs=expiry===null||(Number.isSafeInteger(expiry)&&expiry>=0)?expiry:null;if(outgoingExpiryAtMs===null)outgoingExpiryChecked=null;}let next=parsed;if(failed)next={...next,phase:'incomplete',error:snapshot.error,persistence:'failed'};report(next);if(outgoingReport){scheduleOutgoingExpiry();reconcileOutgoing();}for(const waiter of outgoingWaiters)waiter.check();for(const listener of [...actionResponseListeners])listener();if(next.health?.transport?.phase==='offline'||next.phase==='failed'){coordinator.schedule();}if(next.phase==='ready'&&next.health?.transport?.authenticated){coordinator.reset();if(actionsV1&&!actionRecovery){actionRecovery=true;pollActions();}}if(persistenceNeeded)void persist().catch(()=>{});};
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
  const readyRecord=record=>{const row=health.records.find(row=>sameRecord(row.record,record));return row?.admission==='admitted'&&row.current&&row.editing.mode!=='blocked';};
  const admitted=()=>health.admission.phase==='ready'&&health.admission.pendingPrerequisites===0&&health.admission.requiredRecords.every(readyRecord);
  const terminalAdmission=()=>health.failures.find(f=>f.severity==='error'&&(f.scope.kind==='storage'||['identity-revoked','certificate-rejected','certificate-expired','signature-rejected','permission-required','crypto-unavailable','record-install-failed'].includes(f.code)));
  function stopAdmission(){if(admissionTimer!==null)timerClear(admissionTimer);admissionTimer=null;admissionDeadline=null;}
  function admissionTimeout(record,id=`session:admission:${admissionEpisode.epoch}`){
    const failure={id,code:'admission-timeout',scope:{kind:'admission',...(record?{record}:{})},severity:'error',message:'Shared state is still catching up; automatic recovery paused.',recovery:[{kind:'refresh-checkpoint',...(record?{record}:{})}],occurredAtMs:now()};
    admissionFailures.set(id,record);health={...health,failures:[...health.failures.filter(f=>f.id!==id),failure]};return Object.assign(Error('Checkpoint admission timed out'),{failureId:id});
  }
  function startAdmission(explicit=false,record,deadline=now()+30000){
    if(closed||closing||terminalAdmission()||!health.transport.authenticated)return;
    const transport=health.transport.generation;
    // A record retry owns only its own failures; joining never resets the budget.
    if(explicit){for(const [id,failedRecord] of admissionFailures){if(!record||!failedRecord||sameRecord(record,failedRecord)){health={...health,failures:health.failures.filter(f=>f.id!==id)};admissionFailures.delete(id);}}}
    if(admissionEpisode?.state==='active'&&admissionEpisode.transport===transport)return;
    if(!explicit&&admissionEpisode&&admissionEpisode.transport===transport&&admissionEpisode.state!=='complete')return;
    stopAdmission();admissionDeadline=Math.min(now()+30000,deadline);
    admissionEpisode={epoch:++admissionEpoch,transport,owner:id,start:now(),deadline:admissionDeadline,lastProgress:now(),progress:health.admission.progressRevision??'0',sends:0,lastSent:-Infinity,state:'active'};
    scheduleAdmission();
  }
  function scheduleAdmission(){
    if(admissionTimer!==null)timerClear(admissionTimer);admissionTimer=null;
    const episode=admissionEpisode;if(!episode||episode.state!=='active'||closed||closing)return;
    const due=episode.sends<2?Math.min(episode.deadline,Math.max(episode.lastProgress+10000,episode.lastSent+10000)):episode.deadline;
    admissionTimer=timerSet(()=>{
      admissionTimer=null;if(admissionEpisode!==episode||closed||closing)return;
      if(terminalAdmission()){episode.state='terminal';stopAdmission();return;}
      if(now()>=episode.deadline){episode.state='exhausted';stopAdmission();if(!admitted())admissionTimeout();for(const waiter of [...admissionWaiters])waiter.timeout();report({});return;}
      if(id!==undefined&&health.transport.authenticated&&health.transport.generation===episode.transport){
        episode.owner??=id;episode.sends++;episode.lastSent=now();
        try{runtime.peer_request_sync(id,episode.transport);}catch(error){connectionFailure(error);coordinator.schedule();}
      }else{episode.lastSent=now();if(!health.transport.authenticated)coordinator.schedule();}
      scheduleAdmission();
    },Math.max(0,due-now()));
  }
  function reconcileAdmission(){
    if(closed||closing)return;
    for(const [id,record] of admissionFailures){if(record?readyRecord(record):admitted()){health={...health,failures:health.failures.filter(f=>f.id!==id)};admissionFailures.delete(id);}}
    if(terminalAdmission()){if(admissionEpisode)admissionEpisode.state='terminal';stopAdmission();return;}
    if(admitted()&&!admissionWaiters.size){if(admissionEpisode)admissionEpisode.state='complete';stopAdmission();return;}
    startAdmission();
    const episode=admissionEpisode,progress=health.admission.progressRevision;if(episode?.state==='active'&&typeof progress==='string'&&/^(0|[1-9][0-9]*)$/.test(progress)&&BigInt(progress)>BigInt(episode.progress)){episode.progress=progress;episode.lastProgress=now();scheduleAdmission();}
  }
  function stopOutgoingExpiry(){if(outgoingExpiryTimer!==null)timerClear(outgoingExpiryTimer);outgoingExpiryTimer=null;}
  function scheduleOutgoingExpiry(){
    stopOutgoingExpiry();
    if(closed||closing||outgoingExpiryAtMs===null||outgoingExpiryAtMs===outgoingExpiryChecked)return;
    const expiry=outgoingExpiryAtMs;
    outgoingExpiryTimer=timerSet(()=>{
      outgoingExpiryTimer=null;if(closed||closing||outgoingExpiryAtMs!==expiry)return;
      const future=expiry>now();if(!future)outgoingExpiryChecked=expiry;
      try{runtime.peer_refresh_status(id);}catch{}
      if(future&&outgoingExpiryAtMs===expiry&&outgoingExpiryChecked!==expiry)scheduleOutgoingExpiry();
    },Math.max(0,expiry-now()));
  }
  function stopOutgoing(){if(outgoingTimer!==null)timerClear(outgoingTimer);outgoingTimer=null;}
  function startOutgoing(explicit=false,deadline=now()+30000){
    if(closed||closing||outgoingBlocked===0||!admitted()||terminalAdmission()||!health.transport.authenticated)return;
    const transport=health.transport.generation;
    if(outgoingEpisode?.state==='active'&&outgoingEpisode.transport===transport){scheduleOutgoing();return;}
    if(!explicit&&outgoingEpisode&&outgoingEpisode.transport===transport&&outgoingEpisode.state!=='complete')return;
    stopOutgoing();outgoingEpisode={epoch:++outgoingEpoch,transport,start:now(),deadline:Math.min(now()+30000,deadline),sends:0,state:'active'};scheduleOutgoing();
  }
  function scheduleOutgoing(){
    if(outgoingTimer!==null)timerClear(outgoingTimer);outgoingTimer=null;
    const episode=outgoingEpisode;if(!episode||episode.state!=='active'||closed||closing)return;
    const due=episode.sends<2?Math.min(episode.deadline,episode.start+(episode.sends+1)*10000):episode.deadline;
    outgoingTimer=timerSet(()=>{
      outgoingTimer=null;if(outgoingEpisode!==episode||closed||closing)return;
      if(outgoingBlocked===0){episode.state='complete';return;}
      if(now()>=episode.deadline){episode.state='exhausted';for(const waiter of [...outgoingWaiters])waiter.timeout();return;}
      if(admitted()&&id!==undefined&&health.transport.authenticated&&health.transport.generation===episode.transport){
        episode.sends++;try{runtime.peer_request_sync(id,episode.transport);}catch(error){connectionFailure(error);coordinator.schedule();}
        scheduleOutgoing();
      }
    },Math.max(0,due-now()));
  }
  function reconcileOutgoing(){
    if(closed||closing)return;
    if(outgoingBlocked===0){if(outgoingEpisode)outgoingEpisode.state='complete';stopOutgoing();return;}
    startOutgoing();
  }
  async function joinAdmission(explicit=false,record,deadline){
    if(!health.transport.authenticated)await reconnect();
    startAdmission(explicit,record,deadline);
    // Force one fresh projection without changing the published tail-coalescing
    // or private-write barriers. A stale ready callback cannot finish a waiter.
    await persist();
  }
  const connect=async(old)=>{
    actionRecovery=false;actionsV1?.invalidate("reconnecting");await refreshEnrollment();
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
    if(typeof runtime.peer_action_request==='function'){
      if(!actionsV1){
        const key=JSON.stringify({type:'privateActions',workspace:namespace.room});
        actionsV1=await createActionClient({
          clock:recoveryClock,
          context:()=>{if(closed||closing||!health.transport.authenticated)throw actionError('transport-disconnected','discovery','Action transport disconnected',true);return JSON.parse(actionRuntime(()=>runtime.peer_action_context(id),'discovery'));},
          load:async()=>{const rows=await storage.hydrate();const saved=rows.find(row=>{const storedKey=JSON.parse(row.key);return storedKey.type==='privateActions'&&storedKey.workspace===namespace.room;});if(saved)actionRevision=BigInt(saved.revision);return saved?JSON.parse(new TextDecoder().decode(saved.bytes)):undefined;},
          commit:(value,control,options)=>{
            const write={key,revision:String(++actionRevision),bytes:new TextEncoder().encode(JSON.stringify(value)),actor:null};
            // A terminal broker observation belongs to an already durable intent.
            // Post it to the same worker FIFO so it follows the transaction already
            // in flight without waiting for every later peer batch in that drain.
            if(options?.kind==='terminal-observation'){control?.check();return storage.commit([write]);}
            const task=flush.catch(()=>{}).then(()=>{control?.check();return storage.commit([write]);});flush=task;return task;
          },
          request:(message,control)=>{
            const discovery=message.type==='catalogue_refresh',stage=discovery?'discovery':'observation';
            const disconnected=()=>actionError('transport-disconnected',stage,'Action transport disconnected',discovery);
            control.check();if(closed||closing||id===undefined||!health.transport.authenticated)return Promise.reject(disconnected());
            const owner=id,transport=health.transport.generation;
            const request=actionRuntime(()=>runtime.peer_action_request(owner,JSON.stringify(message)),stage);
            const deadline=Math.min(now()+10000,control.deadlineAtMs??Infinity);
            return new Promise((resolve,reject)=>{
              let timer,settled=false;
              const finish=(error,response)=>{if(settled)return;settled=true;timerClear(timer);actionResponseListeners.delete(poll);control.signal.removeEventListener('abort',abort);runtime.peer_action_forget(owner,request);error?reject(error):resolve(response);};
              const abort=()=>finish(control.signal.reason??actionError('cancelled',stage,'Action attempt cancelled'));
              const poll=()=>{if(settled)return;timerClear(timer);try{
                control.check();if(closed||closing||owner!==id||transport!==health.transport.generation||!health.transport.authenticated)throw disconnected();
                const response=actionRuntime(()=>runtime.peer_action_response(owner,request),stage);if(response){finish(null,JSON.parse(response));return;}
                if(now()>=deadline)throw actionError('transport-timeout',stage,discovery?'Desktop service discovery delayed':'Action response expired; outcome unknown',discovery);
                timer=timerSet(poll,runtime.peer_action_notifications?.()===true?deadline-now():Math.min(25,deadline-now()));
              }catch(error){finish(error);}};
              control.signal.addEventListener('abort',abort,{once:true});actionResponseListeners.add(poll);poll();
            });
          },
        });
      }
      // Recovery observes retained IDs only. It never submits an old effect again.
      pollActions();
    }
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
    let healthChanged=false;
    if(status.error&&(status.phase==='failed'||status.asset==='unavailable')){
      const code=status.asset==='unavailable'?'asset-unavailable':/device.*lost/i.test(status.error)?'gpu-device-lost':'renderer-initialization-failed';
      const asset=status.asset==='unavailable';const failureId=asset?`asset:${surface}:${status.assetId}`:`surface:${surface}`;health={...health,failures:[...health.failures.filter(f=>f.id!==failureId),{id:failureId,code,scope:asset?{kind:'asset',surfaceId:surface,assetId:status.assetId}:{kind:'renderer',surfaceId:surface},severity:'error',message:asset?'An image could not be loaded.':'The desktop could not be displayed. Your work is retained.',recovery:[asset?{kind:'retry-asset',surfaceId:surface,assetId:status.assetId}:{kind:'retry-desktop',surfaceId:surface}],occurredAtMs:Date.now()}]};healthChanged=true;
    }
    const waiter=surfaceWaiters.get(surface);
    if(status.phase==='failed'&&waiter&&BigInt(generation)>=waiter.generation){surfaceWaiters.delete(surface);waiter.reject(Error('Desktop recovery failed'));}
    if(phase==='presented'&&waiter&&BigInt(generation)>waiter.generation){surfaceWaiters.delete(surface);health={...health,failures:health.failures.filter(f=>f.id!==`surface:${surface}`)};waiter.resolve();}
    if(status.persistence==='failed'){setFailure('storage',Error('Local storage transaction failed'));healthChanged=true;}
    const failureIds=health.failures.filter(f=>f.scope.surfaceId===surface).map(f=>f.id);
    const surfaceChanged=phase!==prior.phase||generation!==prior.generation||failureIds.length!==prior.failureIds.length||failureIds.some((id,index)=>id!==prior.failureIds[index]);
    if(surfaceChanged)health={...health,surfaces:health.surfaces.map(row=>row.id===surface?{...row,phase,generation,failureIds}:row)};
    if(healthChanged||surfaceChanged)report({},healthChanged);
  }
  async function settleMounts(items){
    const results=await Promise.allSettled([...items].map(mount=>mount.cleanup()));
    const failure=results.find(result=>result.status==='rejected');
    if(failure)throw failure.reason;
  }
  function close(){
    if(closed)return Promise.resolve();
    if(closePromise)return closePromise;
    closing=true;desktopCommitListeners.clear();const actionClose=actionsV1?.close();if(actionTimer!==null)timerClear(actionTimer);actionTimer=null;if(admissionTimer!==null)timerClear(admissionTimer);admissionTimer=null;admissionDeadline=null;stopOutgoing();stopOutgoingExpiry();coordinator.dispose();events?.removeEventListener?.('online',online);
    closePromise=(async()=>{
      for(const waiter of admissionWaiters)waiter.reject(Error('Session closed'));
      for(const waiter of outgoingWaiters)waiter.reject(Error('Session closed'));
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
      await actionClose;
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
  function refreshCheckpoint(request,operation,deadline=now()+30000){
    // A transport join may precede the authoritative checkpoint. Observe a fresh
    // admission projection, then settle only the resource named by this request.
    const revision=admissionRevision;
    return new Promise((resolve,reject)=>{
      let joined=false,settled=false;
      const timeout=timerSet(()=>waiter.timeout(),Math.max(0,deadline-now()));
      const finish=error=>{if(settled)return;settled=true;timerClear(timeout);admissionWaiters.delete(waiter);error?reject(error):resolve();};
      const waiter={reject:finish,timeout:()=>finish(admissionTimeout(request.record,`recovery:admission:${operation.id}`)),check:()=>{
        const row=request.record?health.records.find(row=>sameRecord(row.record,request.record)):null;
        const relevant=health.failures.filter(f=>f.severity==='error'&&(
          ['connection','storage'].includes(f.scope.kind)||(['admission','permission'].includes(f.scope.kind)&&(!request.record||!f.scope.record||sameRecord(f.scope.record,request.record)))||row?.failureIds.includes(f.id)));
        const terminal=relevant.find(f=>f.scope.kind==='storage'||['identity-revoked','certificate-rejected','certificate-expired','signature-rejected','permission-required','crypto-unavailable','record-install-failed'].includes(f.code)||(admissionRevision>revision&&f.code==='admission-timeout'));
        if(terminal){finish(Object.assign(Error('Checkpoint admission failed'),{failureId:terminal.id}));return;}
        if(admissionRevision<=revision)return;
        const ready=row=>row?.admission==='admitted'&&row.current&&row.editing.mode!=='blocked';
        const admitted=request.record?ready(row):health.admission.phase==='ready'&&health.admission.pendingPrerequisites===0&&health.admission.requiredRecords.every(record=>ready(health.records.find(row=>sameRecord(row.record,record))));
        if(joined&&admitted&&!relevant.some(f=>f.scope.kind!=='connection'))finish();
      }};
      admissionWaiters.add(waiter);waiter.check();if(settled)return;
      joinAdmission(true,request.record,deadline).then(()=>{joined=true;waiter.check();reconcileAdmission();},finish);
    });
  }
  function refreshOutgoing(operation,deadline=now()+30000){
    const revision=outgoingRevision;
    return new Promise((resolve,reject)=>{
      let settled=false;
      const timeout=timerSet(()=>waiter.timeout(),Math.max(0,deadline-now()));
      const finish=error=>{if(settled)return;settled=true;timerClear(timeout);outgoingWaiters.delete(waiter);error?reject(error):resolve();};
      const waiter={reject:finish,timeout:()=>finish(Object.assign(Error('Checkpoint reconciliation timed out'),{failureId:'runtime:outgoing'})),check:()=>{
        if(closed||closing){finish(Error('Session closed'));return;}
        const terminal=terminalAdmission();if(terminal){finish(Object.assign(Error('Checkpoint reconciliation failed'),{failureId:terminal.id}));return;}
        if(outgoingRevision>revision&&outgoingBlocked===0)finish();
      }};
      outgoingWaiters.add(waiter);waiter.check();if(settled)return;startOutgoing(true,deadline);void persist().catch(finish);
    });
  }
  function withinRecoveryDeadline(task,deadline,failureId){
    return new Promise((resolve,reject)=>{
      const timeout=timerSet(()=>reject(Object.assign(Error('Checkpoint recovery timed out'),{failureId})),Math.max(0,deadline-now()));
      Promise.resolve(task).then(resolve,reject).finally(()=>timerClear(timeout));
    });
  }
  async function recoverCheckpoint(request,operation){
    const deadline=now()+30000;
    const terminal=terminalAdmission();if(terminal)throw Object.assign(Error('Checkpoint recovery failed'),{failureId:terminal.id});
    if(!health.transport.authenticated)await withinRecoveryDeadline(reconnect(),deadline,'runtime:outgoing');
    await refreshCheckpoint(request,operation,deadline);
    if(outgoingBlocked>0)await refreshOutgoing(operation,deadline);
  }
  function waitForDesktopAdmission(signal){
    // The renderer checks again after asynchronous asset loading and Winit
    // dispatch. A ready snapshot from before its rejection cannot authorize retry.
    const revision=admissionRevision;
    return new Promise((resolve,reject)=>{
      let settled=false;
      const finish=error=>{if(settled)return;settled=true;admissionWaiters.delete(waiter);signal.removeEventListener('abort',abort);error?reject(error):resolve();};
      const abort=()=>finish(Error('Desktop attempt cancelled'));
      const waiter={reject:finish,timeout:()=>finish(Error('Desktop admission timed out')),check:()=>{
        if(closed||closing||signal.aborted){abort();return;}
        const failure=health.failures.find(f=>f.severity==='error'&&(['admission','permission','storage'].includes(f.scope.kind)||['identity-revoked','certificate-rejected','certificate-expired','signature-rejected','crypto-unavailable'].includes(f.code)));
        if(health.admission.phase==='failed'||failure){finish(Error(failure?.message??'Desktop admission failed'));return;}
        if(admissionRevision<=revision)return;
        const admitted=health.admission.phase==='ready'&&health.admission.pendingPrerequisites===0&&health.admission.requiredRecords.every(record=>{
          const row=health.records.find(row=>sameRecord(row.record,record));
          return row?.admission==='admitted'&&row.current&&row.editing.mode!=='blocked';
        });
        if(snapshot.phase==='ready'&&admitted)finish();
      }};
      admissionWaiters.add(waiter);signal.addEventListener('abort',abort,{once:true});waiter.check();
      // Ask the existing durable owner for a fresh projection even when the last
      // checkpoint arrived while the rejected host was releasing its resources.
      if(!settled)void joinAdmission().catch(finish);
    });
  }
  function recover(request){
    if(closed||closing)return Promise.reject(Error('Session closed'));
    if(!['retry-connection','refresh-checkpoint','reauthenticate','retry-saving','retry-desktop','retry-asset','review-permissions'].includes(request?.kind))return Promise.reject(Error('Unsupported recovery request'));
    if(request.kind==='retry-desktop'&&!surfaceAttempts.has(request.surfaceId))return Promise.reject(Error('Unknown desktop surface'));
    const key=JSON.stringify(request);if(operations.has(key))return operations.get(key);
    let operation={id:String(++operationId),request,state:'pending'};
    health={...health,recoveryOperations:[...health.recoveryOperations,operation]};report({});
    const task=Promise.resolve().then(async()=>{
      switch(request.kind){
        case 'retry-connection':case 'reauthenticate':await reconnect();break;
        case 'refresh-checkpoint':await recoverCheckpoint(request,operation);break;
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
        case 'review-permissions':operation={...operation,state:'requires-user'};break;
        default:throw Error('Unsupported recovery request');
      }
      if(operation.state==='pending')operation={...operation,state:'succeeded'};
    }).catch(error=>{operation={...operation,state:'failed',failureId:error?.failureId??health.failures.at(-1)?.id};}).then(()=>{health={...health,recoveryOperations:health.recoveryOperations.map(current=>current.id===operation.id?operation:current)};if(!closed&&!closing)report({});return{operationId:operation.id};}).finally(()=>operations.delete(key));
    operations.set(key,task);return task;
  }
  try{
    await initialize();
    lease=await acquireIdentity(enrollmentBaseUrl,session,signal);
    signal?.addEventListener('abort',()=>{void close().catch(error=>report({phase:'incomplete',error:String(error)}));},{once:true});
    if(signal?.aborted)throw signal.reason??Error('Session aborted');
    setup=connect();await setup;
    if(closed||closing)throw signal?.reason??Error('Session disposed during connection');
    return {get actionsV1(){return actionsV1;},getSnapshot:()=>snapshot,subscribe(listener){subscribers.add(listener);return()=>subscribers.delete(listener);},
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
          const admissionPending='canonical desktop records are not admitted';
          const allocate=async retryAdmission=>{
            if(entry.cancelled||closed||closing||surfaceTargets.get(target)!==surface)throw Error('Desktop attempt cancelled');
            const current=++allocation;let admissionRejected=false;
            try{
              return await mountDesktop(target,{...mountOptions,...namespace,actionsV1,subscribeDesktopCommits,signal:controller.signal,app,peer:id,record:record?.record_id||undefined,author:snapshot.deviceId,onStatus:status=>{
                if(entry.cancelled||current!==allocation||closed||closing||surfaceTargets.get(target)!==surface)return;
                // Only this exact initial guard failure is temporary. Allocation
                // cleanup must finish before retry; all security/GPU errors pass through.
                if(sessionDesktop&&retryAdmission&&status.phase==='failed'&&status.error===admissionPending){admissionRejected=true;status={phase:'admitting'};}
                else if(admissionRejected&&status.phase==='closed'&&status.persistence!=='failed'&&!status.error)status={...status,phase:'admitting'};
                surfaceStatus(surface,status);mountOptions.onStatus?.({...status,surfaceId:surface});if(status.phase==='presented'||status.persistence==='committed')void persist(true,false).catch(()=>{});
              }});
            }catch(error){
              if(current===allocation)allocation++;
              if(!sessionDesktop||!retryAdmission||error?.message!==admissionPending)throw error;
              await waitForDesktopAdmission(controller.signal);
              // One automatic retry only: a repeatedly changing admission must
              // remain an explicit recoverable failure, never a busy allocation loop.
              return allocate(false);
            }
          };
          const pending=allocate(true).then(async mounted=>{
            if(entry.cancelled||closed||closing||surfaceTargets.get(target)!==surface){await mounted.cleanup();throw Error('Session disposed while mounting');}
            const cleanup=mounted.cleanup.bind(mounted);
            mounted.cleanup=async()=>{try{await cleanup();mounts.delete(mounted);failedMounts.delete(mounted);surfaces.delete(surface);surfaceAttempts.delete(surface);mountControllers.delete(controller);mountOptions.signal?.removeEventListener('abort',abort);}catch(error){failedMounts.add(mounted);throw error;}};
            entry.mounted=mounted;mounts.add(mounted);surfaces.set(surface,mounted);
            health={...health,failures:health.failures.filter(f=>f.id!==`surface:${surface}`)};
            surfaceStatus(surface,mounted.state?.()??{phase:'presented'});mountOptions.onMount?.(mounted);return mounted;
          }).catch(error=>{
            if(!entry.cancelled&&!closed&&!closing&&surfaceTargets.get(target)===surface){surfaceStatus(surface,{phase:'failed',error:String(error)});mountOptions.onStatus?.({phase:'failed',surfaceId:surface,error:String(error)});}throw error;
          }).finally(()=>{pendingMounts.delete(pending);entry.pending=null;});
          entry.pending=pending;pendingMounts.add(pending);
          return pending;
        };
        return entry.run();
      },
      reconnect,recover,
      retryPersistence,failNextWrite:()=>{storage.abortNext=true;},durableState:()=>storage.hydrate(),dispose:close};
  }catch(error){try{await close();}catch(cleanupError){throw new AggregateError([error,cleanupError],'Peer setup failed; retained recovery is available');}throw error;}
}

}
