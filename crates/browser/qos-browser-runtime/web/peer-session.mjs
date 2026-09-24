import {createActionStorage} from './action-storage.mjs';
import {createPeerDelivery} from './peer-delivery.mjs';
import {createPeerMedia} from './peer-media.mjs';
import {createLiveClient} from './live-client.mjs';
import {createInvitedPeerSource} from './invited-peer-source.mjs';
import {createCloudPeerSource} from './cloud-peer-source.mjs';
import {createActionClient,actionError} from './action-client.mjs';
import {createAgentReadsClient} from './agent-reads-client.mjs';
import {commitPeerWrites,commitPeerBatches,readPeerBatch} from './peer-persistence.mjs';
import {createRecoveryCoordinator} from './peer-recovery.mjs';
import {retryableMemo} from './asset-recovery.mjs';
const recoveryOwners=new Set();
// Host 503 during native (re)subscription: ~8 attempts span roughly 10 s.
const ENROLLMENT_503_RETRIES=8;
function samePublicValue(left,right){
 if(Object.is(left,right))return true;
 if(!left||!right||typeof left!=='object'||typeof right!=='object'||Array.isArray(left)!==Array.isArray(right))return false;
 const leftKeys=Object.keys(left),rightKeys=Object.keys(right);if(leftKeys.length!==rightKeys.length)return false;
 return leftKeys.every(key=>Object.hasOwn(right,key)&&samePublicValue(left[key],right[key]));
}
export const pendingPeerRecoveryCount=()=>recoveryOwners.size;
export async function retryPendingPeerRecovery(){const results=await Promise.allSettled([...recoveryOwners].map(close=>close()));const failure=results.find(result=>result.status==='rejected');if(failure)throw failure.reason;}
export function createConnectPeer({init,runtime,createMedia=createPeerMedia,createLive=createLiveClient,createDelivery=createPeerDelivery,mountDesktop,acquireStorage,acquireIdentity,acquireInvitedIdentity,acquireCloudIdentity,fetchEnrollment=fetch,session=globalThis.sessionStorage,recoveryClock={},events=globalThis}){
const initialize=retryableMemo(init);
return async function connectPeer(options={}){
  const {enrollmentBaseUrl='/qos-peer/enrollment',signal}=options;
  let liveClient;// LIVE_V1 (optional, ephemeral; durable delivery is unaffected)
  let lease,invitedSource,invitedPrepared,actionsV1,agentReadsV1,mediaClient,deliveryClient,actionRecovery=false,actionTimer=null,actionPolling=false;
  const invitedRefreshes=new Set();
  // Invited guest or Cloud owner: both are closed Rust sources with the same
  // prepare/connect/reconnect/refresh contract.
  let createSource;
  const localContexts=new Set();let localContext;
  let authorizationRefresh,authorizationController,authorizationNeedsRefresh=false;
  let id,closed=false,closing=false,snapshot={phase:'authenticating',records:[]},connection,storage,namespace,raw,flush=Promise.resolve(),queuedPersist,failed=false,generation=0;
  const desktopCommitListeners=new Set(),actionResponseListeners=new Set();
  const subscribeDesktopCommits=listener=>{desktopCommitListeners.add(listener);return()=>desktopCommitListeners.delete(listener);};
  const subscribers=new Set(),mounts=new Set(),failedMounts=new Set(),pendingMounts=new Set(),mountControllers=new Set();
  let initialConnectSettled=false,enrollmentGeneration=0,reconnecting,closePromise,setup,operationId=0,surfaceId=0,healthRevision=0n,saving,admissionTimer=null,admissionDeadline=null,admissionRevision=0,outgoingTimer=null,outgoingExpiryTimer=null,outgoingExpiryAtMs=null,outgoingExpiryChecked=null,outgoingBlocked=0,outgoingRevision=0;
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
  let busyTimer=null;
  // Local storage failure recovery. A failed transaction is retried on a bounded
  // backoff; the first success clears the failure. While the device cannot save,
  // delivery reconnects are paused: every delivery cut needs an exact atomic
  // storage acknowledgement, so reconnecting could only loop.
  const STORAGE_RETRY_BASE_MS=1000,STORAGE_RETRY_MAX_MS=30000;
  let storageRetryTimer=null,storageRetryAttempt=0,storageFaultShown=false,reconnectDeferred=false;
  const storagePaused=()=>failed&&!closed&&!closing;
  // Only errors raised by the storage transaction itself are storage failures.
  const storageCommit=(operation)=>{try{return Promise.resolve(operation()).catch(error=>{throw markStorageError(error);});}catch(error){return Promise.reject(markStorageError(error));}};
  function markStorageError(error){if(error&&typeof error==='object')try{error.storageFailure=true;}catch{}return error;}
  const storageBlocked=()=>Object.assign(Error(`Local storage is unavailable; saving retries automatically${snapshot.error?` (${snapshot.error})`:''}`),{name:'StorageUnavailableError',storageBlocked:true,terminal:true});
  function stopStorageRetry(){if(storageRetryTimer!==null)timerClear(storageRetryTimer);storageRetryTimer=null;}
  function scheduleStorageRetry(){
    if(closed||closing||storageRetryTimer!==null)return;
    const delay=Math.min(STORAGE_RETRY_MAX_MS,STORAGE_RETRY_BASE_MS*2**Math.min(storageRetryAttempt,5));storageRetryAttempt++;
    storageRetryTimer=timerSet(()=>{storageRetryTimer=null;if(closed||closing)return;failed=false;void persist().catch(()=>{});},delay);
  }
  function storageRecovered(){
    stopStorageRetry();storageRetryAttempt=0;
    if(storageFaultShown){storageFaultShown=false;report({error:null,persistence:'committed'});try{if(id!==undefined)runtime.peer_refresh_status?.(id);}catch{}}
    if(reconnectDeferred&&!closed&&!closing){reconnectDeferred=false;queueMicrotask(()=>{if(!closed&&!closing)void reconnect().catch(()=>{});});}
  }
  async function persist(publish=true,force=true){
    if(id===undefined||!storage||closed)return;
    if(failed)throw storageBlocked();
    // Coalesce only work that has not started and is still the queue tail.
    // A running drain or an intervening private-action write needs its own
    // following barrier, so new writes can never join an already-settling run.
    if(queuedPersist&&flush===queuedPersist.promise){queuedPersist.publish||=publish;queuedPersist.force||=force;return queuedPersist.promise;}
    const request={publish,force};
    request.promise=flush.catch(()=>{}).then(async()=>{
      if(queuedPersist===request)queuedPersist=undefined;
      if(closed)return;
      if(failed)throw storageBlocked();
      let committed=0;
      const publication=()=>{if(request.publish&&(request.force||committed>0))runtime.peer_committed(id);};
      try{
        // The journal (including replay floors/outbox) and all canonical records
        // share this one strict IDB transaction. Network publication follows ack.
        const result=runtime.peer_pending_batch_bytes||runtime.peer_pending_batch
          ?await commitPeerBatches({pending:()=>readPeerBatch(runtime,id),commit:(writes,batch)=>deliveryClient?deliveryClient.commitBatch(writes,batch):batch.transferId===undefined?storageCommit(()=>storage.commit(writes)):Promise.reject(Error('Transfer custody unavailable')),acknowledge:value=>{if(value.result.ok)committed++;runtime.acknowledge_peer_storage_batch(id,JSON.stringify(value));},epoch:storage.epoch,publish:publication})
          :await commitPeerWrites({pending:()=>JSON.parse(runtime.peer_pending_writes(id)),commit:writes=>storageCommit(()=>storage.commit(writes)),acknowledge:write=>{committed++;runtime.acknowledge_storage(namespace.profile,namespace.device,BigInt(write.revision),undefined);},publish:publication});
        // Another persister's capture is still settling: nothing was captured, retry shortly.
        if(result.busy){if(busyTimer===null&&!closed)busyTimer=timerSet(()=>{busyTimer=null;void persist(request.publish,request.force).catch(()=>{});},25);return;}
        clearFailure('storage');storageRecovered();
        if(result.publicationError){connectionFailure(result.publicationError);coordinator.schedule();}

      }catch(error){
        // A real storage transaction failure (quota, denied, aborted transaction)
        // pauses saving and shows the banner; custody/runtime errors do not. Both
        // retry on the bounded backoff so pending work is never stranded.
        if(error?.storageFailure){failed=true;storageFaultShown=true;setFailure('storage',error);report({error:String(error),persistence:'failed'});}
        scheduleStorageRetry();throw error;
      }
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
  const update=value=>{if(closed||closing)return;if(actionsV1){try{runtime.peer_action_context(id);}catch{actionsV1.invalidate('identity-or-transport-unavailable');agentReadsV1?.invalidate('identity-or-transport-unavailable');actionRecovery=false;}}const {mediaAuthorizationExpired,deliveryReady,persistenceNeeded,committedDesktop,actionResponseReady,outgoingExpiryAtMs:expiry,...parsed}=JSON.parse(value);if(mediaAuthorizationExpired){authorizationNeedsRefresh=true;coordinator.schedule();if(mediaClient)void mediaClient.expireAuthorization().catch(error=>options.onMediaError?.(error));if(!Object.keys(parsed).length&&!deliveryReady&&!persistenceNeeded&&!committedDesktop&&!actionResponseReady)return;}if(deliveryReady){deliveryClient?.wake();if(!Object.keys(parsed).length&&!persistenceNeeded&&!committedDesktop&&!actionResponseReady)return;}if(actionResponseReady){for(const listener of [...actionResponseListeners])listener();if(!Object.keys(parsed).length&&!persistenceNeeded&&!committedDesktop)return;}if(committedDesktop){for(const listener of desktopCommitListeners){try{listener(committedDesktop);}catch{}}if(!Object.keys(parsed).length&&!persistenceNeeded)return;}const outgoingReport=Number.isSafeInteger(parsed.outgoingBlocked)&&parsed.outgoingBlocked>=0;if(outgoingReport){outgoingBlocked=parsed.outgoingBlocked;outgoingRevision++;outgoingExpiryAtMs=expiry===null||(Number.isSafeInteger(expiry)&&expiry>=0)?expiry:null;if(outgoingExpiryAtMs===null)outgoingExpiryChecked=null;}let next=parsed;if(failed)next={...next,phase:'incomplete',error:snapshot.error,persistence:'failed'};report(next);if(outgoingReport){scheduleOutgoingExpiry();reconcileOutgoing();}for(const waiter of outgoingWaiters)waiter.check();for(const listener of [...actionResponseListeners])listener();if(next.health?.transport?.phase==='offline'||next.phase==='failed'){coordinator.schedule();}if(next.phase==='ready'&&next.health?.transport?.authenticated){coordinator.reset();if(actionsV1&&!actionRecovery){actionRecovery=true;pollActions();}}if(persistenceNeeded)void persist().catch(()=>{});};
  async function post(route,body){
    const response=await fetchEnrollment(`${enrollmentBaseUrl}/${route}`,{method:'POST',headers:{'Content-Type':'application/json'},body,signal,cache:'no-store',credentials:'same-origin',redirect:'error'});
    signal?.throwIfAborted();if(closed||closing)throw Error('Enrollment custody closed');
    const reader=response.body?.getReader();if(!reader)throw Error('Enrollment response body missing');
    const buffer=new Uint8Array(1048576);let length=0;
    try{for(;;){const chunk=await reader.read();signal?.throwIfAborted();if(closed||closing)throw Error('Enrollment custody closed');if(chunk.done)break;if(!(chunk.value instanceof Uint8Array)||length+chunk.value.byteLength>buffer.byteLength)throw Error('Enrollment response exceeds bounds');buffer.set(chunk.value,length);length+=chunk.value.byteLength;}}finally{try{await reader.cancel();}catch{}}
    const text=new TextDecoder('utf-8',{fatal:true}).decode(buffer.subarray(0,length));
    if(!response.ok){let reason;try{const value=JSON.parse(text);reason=typeof value.error==='string'?value.error:undefined;}catch{}throw Error(`Enrollment HTTP ${response.status}${reason?`: ${reason}`:''}`);}
    return text;
  }
  async function refreshEnrollment(){
    enrollmentGeneration++;
    if(invitedSource){
      invitedPrepared=await invitedSource.prepare({signal});
      const nextNamespace=invitedPrepared.namespace;
      if(namespace&&!samePublicValue(namespace,nextNamespace))throw Error('Invited workspace changed; retained owner requires explicit migration');
      namespace=nextNamespace;
      if(!storage){storage=await acquireStorage(namespace.profile,namespace.device);if(storage.fresh){runtime.refresh_storage_lease(namespace.profile,namespace.device);storage.fresh=false;}}
      return;
    }
    // A host that is (re)establishing its native subscription answers 503
    // ("retry enrollment with the same identity"). Enrollment is idempotent for
    // one identity, so retry the whole challenge/complete pair promptly with a
    // short jittered backoff instead of failing the session open or waiting for
    // the slower transport recovery schedule.
    let challenge,next;
    for(let attempt=0;;attempt++){
      try{challenge=await post('challenge',runtime.identity_request(lease.material));next=await post('complete',runtime.identity_complete(lease.material,challenge));break;}
      catch(error){
        if(!/^Enrollment HTTP 503\b/.test(String(error?.message))||attempt>=ENROLLMENT_503_RETRIES||closed||closing)throw error;
        await new Promise(resolve=>timerSet(resolve,Math.round(Math.min(2000,200*2**attempt)*(0.5+Math.random()*0.5))));
        signal?.throwIfAborted();if(closed||closing)throw Error('Enrollment custody closed');
      }
    }
    // Opaque signed timestamps go directly to Rust. Pin authority before dial.
    let bound;
    if(typeof runtime.bind_identity_for_peer==='function'){
      const binding=JSON.parse(runtime.bind_identity_for_peer(lease.material,challenge,next,id));
      if(!Number.isSafeInteger(binding.context)||binding.context<=0||binding.context>0xffffffff||!Array.isArray(binding.material)||binding.material.length>4096||binding.material.some(value=>!Number.isInteger(value)||value<0||value>255))throw Error('Invalid local onboarding custody');
      localContext=binding.context;localContexts.add(localContext);bound=Uint8Array.from(binding.material);
    }else bound=runtime.bind_identity(lease.material,challenge,next);
    const nextNamespace=JSON.parse(runtime.peer_enrollment(next));
    if(namespace&&JSON.stringify(namespace)!==JSON.stringify(nextNamespace))throw Error('Enrolled workspace changed; retained owner requires explicit migration');
    await lease.save(bound);raw=next;namespace=nextNamespace;
    if(!storage){storage=await acquireStorage(namespace.profile,namespace.device);if(storage.fresh){runtime.refresh_storage_lease(namespace.profile,namespace.device);storage.fresh=false;}}
  }
  async function refreshSharedAuthorization(check){
    check();actionRecovery=false;actionsV1?.invalidate('shared-authorization-refresh');agentReadsV1?.invalidate('shared-authorization-refresh');
    await persist(false);check();
    if(!deliveryClient)throw Error('Shared delivery authorization unavailable');
    await deliveryClient.refreshAuthorization();check();
  }
  // Shared session expiry renews authorization on the same Peer/key/storage owner.
  function refreshMediaHint(mediaSignal){
    mediaSignal?.throwIfAborted();
    if(closed||closing)return Promise.reject(Error('Shared authorization custody closed'));
    if(!authorizationRefresh){
      authorizationController=new AbortController();
      const controller=authorizationController;
      // A completed refresh re-established delivery on the renewed authorization:
      // clear the connection failure its expiry reported (as a reconnect does).
      const raw=performSharedRefresh(controller.signal).then(value=>{authorizationNeedsRefresh=false;if(!closed&&!closing){clearFailure('connection');report({});}return value;});
      authorizationRefresh=raw.finally(()=>{if(authorizationController===controller){authorizationController=undefined;authorizationRefresh=undefined;}});
      authorizationRefresh.catch(()=>{});
    }
    const shared=authorizationRefresh;
    if(!mediaSignal)return shared;
    return new Promise((resolve,reject)=>{
      const abort=()=>{mediaSignal.removeEventListener('abort',abort);reject(mediaSignal.reason??Error('Media refresh cancelled'));};
      mediaSignal.addEventListener('abort',abort,{once:true});
      shared.then(value=>{mediaSignal.removeEventListener('abort',abort);if(mediaSignal.aborted)abort();else resolve(value);},error=>{mediaSignal.removeEventListener('abort',abort);reject(error);});
    });
  }
  async function performSharedRefresh(mediaSignal){
    const owner=id,epoch=enrollmentGeneration,material=lease.material;
    const check=()=>{mediaSignal.throwIfAborted();if(closed||closing||id!==owner||enrollmentGeneration!==epoch)throw Error('Media enrollment custody changed');};
    if(invitedSource){
      check();const refresh=createSource(check);invitedRefreshes.add(refresh);
      try{await refresh.refresh(owner,mediaSignal);check();await persist();check();await refreshSharedAuthorization(check);}
      finally{await refresh.close();invitedRefreshes.delete(refresh);}
      return;
    }
    async function boundedPost(route,body,extraHeaders={}){
      check();const response=await fetchEnrollment(`${enrollmentBaseUrl}/${route}`,{method:'POST',headers:{'Content-Type':'application/json',...extraHeaders},body,signal:mediaSignal,cache:'no-store',credentials:'same-origin',redirect:'error'});
      check();if(!response.ok)throw Error(`Enrollment HTTP ${response.status}`);
      const reader=response.body?.getReader();if(!reader)throw Error('Enrollment response body missing');
      const bytes=new Uint8Array(1048576);let length=0;
      try{for(;;){const chunk=await reader.read();check();if(chunk.done)break;if(length+chunk.value.byteLength>bytes.length)throw Error('Enrollment response exceeds bounds');bytes.set(chunk.value,length);length+=chunk.value.byteLength;}}
      finally{try{await reader.cancel();}catch{}}
      return new TextDecoder('utf-8',{fatal:true}).decode(bytes.subarray(0,length));
    }
    check();const challenge=await boundedPost('challenge',runtime.identity_request(material));check();
    // Renewal keeps this Peer's admitted history and needs only a fresh
    // certificate binding and delivery hint: ask the host to omit governance.
    // Hosts that predate the header ignore it and send the full reply.
    const next=await boundedPost('complete',runtime.identity_complete(material,challenge),{'X-Tilo-Enrollment-Governance':'omit'});check();
    const bound=runtime.bind_identity(material,challenge,next);
    if(bound.length!==material.length||bound.some((value,index)=>value!==material[index]))throw Error('Media enrollment identity changed');
    const nextNamespace=JSON.parse(runtime.peer_enrollment(next));
    if(!samePublicValue(namespace,nextNamespace))throw Error('Media enrollment namespace changed');
    check();runtime.peer_media_update_hint(owner,next);await refreshSharedAuthorization(check);
  }
  function clearFailure(scope){health={...health,failures:health.failures.filter(f=>f.scope.kind!==scope)};}
  function setFailure(scope,error){
    const text=String(error);let code=scope==='storage'?'storage-transaction-failed':'transport-unavailable';
    // A signed in-band onboarding refusal (removed member, expired grant) is the
    // host's authoritative answer: stop retrying instead of looping every few seconds.
    if(/revoked/i.test(text)||/Workspace join rejected \(40[13]\)/.test(text))code='identity-revoked';else if(/certificate|signature|root mismatch|binding mismatch|HTTP 40[13]/i.test(text))code='certificate-rejected';else if(error?.name==='QuotaExceededError')code='storage-quota';else if(error?.name==='SecurityError')code='storage-denied';
    const terminal=['identity-revoked','certificate-rejected'].includes(code);if(terminal&&error&&typeof error==='object')error.terminal=true;
    clearFailure(scope);health={...health,failures:[...health.failures,{id:`session:${scope}`,code,scope:{kind:scope},severity:'error',message:scope==='storage'?'Changes could not be saved on this device. Saving retries automatically.':terminal?'This device needs authorization to reconnect.':'Connection is unavailable. Your saved work is retained.',recovery:[{kind:scope==='storage'?'retry-saving':terminal?'reauthenticate':'retry-connection'}],occurredAtMs:Date.now()}]};return terminal;
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
    try {
    if(typeof runtime.peer_delivery_begin!=='function')throw Object.assign(Error('Runtime HTTPS delivery is unavailable'),{terminal:true});
    await deliveryClient?.close();await persist(false);deliveryClient=undefined;
    actionRecovery=false;agentReadsV1?.invalidate("reconnecting");actionsV1?.invalidate("reconnecting");await refreshEnrollment();
    if(closed||closing)throw Error('Session disposed during enrollment');
    if(old===undefined){
      const current=generation;
      const hydrated=await storage.hydrate(),status=value=>queueMicrotask(()=>{if(current===generation)update(value);});
      connection=invitedSource?invitedSource.connect(invitedPrepared,hydrated,status):runtime.connect_peer(raw,lease.material,hydrated,status,localContext);
      const next=await connection;
      if(closed||closing){await runtime.close_peer(next);return;}
      id=next;
      // Bind the held lease before a queued persistence drain can capture work.
      // Older v1/v2 runtimes keep their original acknowledgement contract.
      runtime.peer_bind_storage_epoch?.(id,storage.epoch);
    }else{
      if(!invitedSource&&typeof runtime.reconnect_peer!=='function')throw Object.assign(Error('Runtime cannot retain the state owner during reconnect'),{terminal:true});
      connection=invitedSource?invitedSource.reconnect(invitedPrepared,old):runtime.reconnect_peer(old,raw,lease.material,localContext);await connection;
      if(closed||closing)return;
    }
    clearFailure('connection');await persist();
    if(closed||closing)return;
    deliveryClient=createDelivery({runtime,peerId:id,clock:recoveryClock,persist:()=>persist(),fetch:fetchEnrollment,transfers:storage.transfers,commit:(writes,options)=>storageCommit(()=>storage.commit(writes,options)),onError:error=>{
      if(closed||closing)return;
      // Delivery stopped because this device cannot save: resume once storage recovers.
      if(storagePaused()){reconnectDeferred=true;health={...health,transport:{...health.transport,phase:'offline',authenticated:false}};report({});return;}
      connectionFailure(error);coordinator.schedule();}});
    await deliveryClient.start();if(closed||closing)return;
    if(typeof runtime.peer_action_request==='function'){
      if(!actionsV1){
        const key=JSON.stringify({type:'privateActions',workspace:namespace.room});
        actionsV1=await createActionClient({version:options.actionsVersion??2,
          clock:recoveryClock,
          context:()=>{if(closed||closing||!health.transport.authenticated)throw actionError('transport-disconnected','discovery','Action transport disconnected',true);return JSON.parse(actionRuntime(()=>runtime.peer_action_context(id),'discovery'));},
          ...createActionStorage({storage,key,beforeCommit:(control,options)=>{
            // Retain the same worker FIFO. Terminal observations may join at its
            // next transaction boundary; ordinary admissions follow peer flush.
            if(options?.kind==='terminal-observation'){control?.check();return;}
            const task=flush.catch(()=>{}).then(()=>control?.check());flush=task;return task;
          }}),
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
      if(!agentReadsV1&&typeof runtime.peer_agent_read_request==='function'){
        agentReadsV1=createAgentReadsClient({actions:actionsV1,clock:recoveryClock,
          context:()=>JSON.parse(runtime.peer_action_context(id)),
          supported:()=>!closed&&!closing&&id!==undefined&&health.transport.authenticated&&runtime.peer_agent_reads_supported?.(id)===true,
          request:(message,control)=>{
            if(closed||closing||id===undefined||!health.transport.authenticated)throw Error('Agent read transport disconnected');
            const owner=id,transport=health.transport.generation;
            const request=runtime.peer_agent_read_request(owner,JSON.stringify(message));
            return new Promise((resolve,reject)=>{
              let timer,settled=false;
              const finish=(error,response)=>{if(settled)return;settled=true;timerClear(timer);actionResponseListeners.delete(poll);control.signal.removeEventListener('abort',abort);runtime.peer_agent_read_forget(owner,request);error?reject(error):resolve(response);};
              const abort=()=>finish(control.signal.reason??Error('Agent read cancelled'));
              const poll=()=>{if(settled)return;timerClear(timer);try{
                if(control.signal.aborted)throw control.signal.reason;
                if(closed||closing||owner!==id||transport!==health.transport.generation||!health.transport.authenticated)throw Error('Agent read transport disconnected');
                const response=runtime.peer_agent_read_response(owner,request);if(response){finish(null,JSON.parse(response));return;}
                if(now()>=control.deadlineAtMs)throw Error('Agent read deadline exceeded');
                timer=timerSet(poll,runtime.peer_action_notifications?.()===true?control.deadlineAtMs-now():Math.min(25,control.deadlineAtMs-now()));
              }catch(error){finish(error);}};
              control.signal.addEventListener('abort',abort,{once:true});actionResponseListeners.add(poll);poll();
            });
          },
        });
      }
      // Recovery observes retained IDs only. It never submits an old effect again.
      pollActions();
    }
    if(!mediaClient&&typeof runtime.peer_media_bootstrap==='function'){
      mediaClient=createMedia({runtime,peerId:id,persist:()=>persist(),refreshHint:refreshMediaHint,onError:error=>options.onMediaError?.(error)});
      void mediaClient.initialize().then(()=>{if(!closed&&!closing)report({},true);}).catch(error=>options.onMediaError?.(error));
    }
    if(!liveClient&&typeof runtime.peer_live_grant==='function'){liveClient=createLive({runtime,peerId:id,fetch:fetchEnrollment,onError:error=>options.onLiveError?.(error)});liveClient.start();}
    } finally {
      if(localContext!==undefined){const context=localContext;localContext=undefined;if(localContexts.delete(context))runtime.cancel_local_onboarding(context);}
    }
  };
  const recoveryCoordinator=createRecoveryCoordinator({...recoveryClock,attempt:async()=>{
    // No reconnect while local storage cannot save: it would loop on the same
    // unacknowledged cut. The storage retry resumes delivery on success.
    if(storagePaused()){reconnectDeferred=true;throw storageBlocked();}
    // The runtime reports its transport offline until the first handshake
    // completes. On a high-latency link that handshake outlasts the first
    // backoff delay; a recovery attempt must not close the initial connection
    // mid-handshake. It waits for it and becomes a no-op if it succeeded.
    if(!initialConnectSettled&&setup){await setup.catch(()=>{});if(closed||closing)throw Error('Session closed');if(health.transport.authenticated)return;}
    try{health={...health,transport:{...health.transport,phase:'reconnecting',authenticated:false}};report({});if(authorizationNeedsRefresh||authorizationRefresh)await refreshMediaHint();else await connect(id);}catch(error){if(storagePaused()){reconnectDeferred=true;throw storageBlocked();}connectionFailure(error);throw error;}
  },onState:retry=>{health={...health,transport:{...health.transport,phase:!health.transport.authenticated&&(retry.inFlight||retry.nextAttemptAtMs!==null)?'reconnecting':health.transport.phase,retry}};report({});}});
  const coordinator={schedule(){if(storagePaused()){reconnectDeferred=true;return;}recoveryCoordinator.schedule();},retry:()=>recoveryCoordinator.retry(),reset:()=>recoveryCoordinator.reset(),dispose:()=>recoveryCoordinator.dispose()};
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
    closing=true;for(const context of localContexts)runtime.cancel_local_onboarding(context);localContexts.clear();authorizationController?.abort(Error('Media enrollment custody changed: Session closed'));const authorizationClose=authorizationRefresh;const invitedClose=invitedSource?.close(),invitedRefreshClose=[...invitedRefreshes].map(refresh=>refresh.close());liveClient?.close();const mediaClose=mediaClient?.dispose(),deliveryClose=deliveryClient?.close();agentReadsV1?.close();desktopCommitListeners.clear();const actionClose=actionsV1?.close();if(actionTimer!==null)timerClear(actionTimer);actionTimer=null;if(admissionTimer!==null)timerClear(admissionTimer);admissionTimer=null;admissionDeadline=null;stopOutgoing();stopOutgoingExpiry();stopStorageRetry();coordinator.dispose();events?.removeEventListener?.('online',online);
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
      await invitedClose;
      await Promise.all(invitedRefreshClose);
      await authorizationClose?.catch(()=>{});
      // Stop the replaceable endpoint even when component settlement retains
      // custody. Keep its durable owner addressable for the next retry.
      if(id!==undefined)await runtime.disconnect_peer(id);
      await mediaClose;
      await deliveryClose;
      await deliveryClient?.detach?.();
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
    if(options.invitation&&options.cloud)throw TypeError('Choose either an invitation or a Cloud owner');
    if(options.cloud){
      if(typeof acquireCloudIdentity!=='function')throw Error('Cloud owner identity runtime unavailable');
      const cloud=options.cloud;
      if(typeof cloud.enrollment?.inspect!=='function'||typeof cloud.enrollment?.apply!=='function')throw TypeError('Cloud owner enrollment adapter requires inspect and apply');
      lease=await acquireCloudIdentity(cloud,session,signal);
      createSource=guard=>createCloudPeerSource({runtime,lease,cloud,fetchImpl:fetchEnrollment,guard});
      invitedSource=createSource();
    }else if(options.invitation){
      if(typeof acquireInvitedIdentity!=='function')throw Error('Invited identity runtime unavailable');
      lease=await acquireInvitedIdentity(options.invitation,session,signal);
      createSource=guard=>createInvitedPeerSource({runtime,lease,fetchImpl:fetchEnrollment,guard});
      invitedSource=createSource();
    }else{lease=await acquireIdentity(enrollmentBaseUrl,session,signal);}
    signal?.addEventListener('abort',()=>{void close().catch(error=>report({phase:'incomplete',error:String(error)}));},{once:true});
    if(signal?.aborted)throw signal.reason??Error('Session aborted');
    setup=connect();try{await setup;}finally{initialConnectSettled=true;}
    if(closed||closing)throw signal?.reason??Error('Session disposed during connection');
    return {get mediaV1(){return mediaClient?.supported()?mediaClient.mediaV1:undefined;},get liveV1(){return liveClient?.api;},get actionsV1(){return actionsV1;},get actionsV2(){return actionsV1?.actionsV2;},get agentReadsV1(){return runtime.peer_agent_reads_supported?.(id)===true?agentReadsV1:undefined;},getSnapshot:()=>snapshot,subscribe(listener){subscribers.add(listener);return()=>subscribers.delete(listener);},
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
              return await mountDesktop(target,{...mountOptions,...namespace,actionsV1,actionsV2:actionsV1?.actionsV2,subscribeDesktopCommits,live:()=>liveClient?.api,liveMembers:()=>typeof runtime.peer_live_members==='function'?runtime.peer_live_members(id):'[]',signal:controller.signal,app,peer:id,record:record?.record_id||undefined,author:snapshot.deviceId,onStatus:status=>{
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
