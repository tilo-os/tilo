const RENEW_MS=20000;
const GRANT_LIFETIME_MS=60000;
const HEARTBEAT_MS=5000;
const PRESENCE_LIFETIME_MS=15000;
const ATTEMPT_TIMEOUT_MS=10000;
const HEARTBEAT_TIMEOUT_MS=5000;
const RECOVERY_FAST_WINDOW_MS=10000;
const RECOVERY_STOP_MS=60000;

const permissionError=error=>error?.name==='NotAllowedError'||error?.name==='SecurityError'||error?.code==='permission-denied'||error?.code==='permission';
const expiredError=error=>error?.code==='expired'||error?.code==='revoked'||error?.terminal===true;
// The host superseded this participation (e.g. moderation advanced the call
// epoch) while the person remains permitted: rejoin the same call in place.
const staleError=error=>error?.code==='stale'&&error?.terminal!==true;
const abortError=(message,code='aborted')=>Object.assign(Error(message),{name:'AbortError',code});

function participationId(){
 if(globalThis.crypto?.randomUUID)return globalThis.crypto.randomUUID();
 return `media-${Date.now().toString(36)}-${Math.random().toString(36).slice(2)}`;
}

function availableMedia(capabilities){
 if(capabilities===true)return true;
 const media=capabilities?.mediaV1??capabilities;
 return media?.version===1&&media?.available!==false;
}

function cleanupOf(value){
 if(typeof value==='function')return value;
 if(typeof value?.close==='function')return ()=>value.close();
 if(typeof value?.unsubscribe==='function')return ()=>value.unsubscribe();
 return null;
}

export function createMediaSession({authority,transport,audio,clock={},onError=()=>{},bindAuthorizationControl}={}){
 if(!authority||!transport||!audio)throw new TypeError('media authority, transport, and audio are required');
 const now=clock.now??(()=>globalThis.performance.now());
 const setTimer=clock.setTimeout??globalThis.setTimeout;
 const clearTimer=clock.clearTimeout??globalThis.clearTimeout;
 let snapshot={state:'idle',muted:true,callId:null,reason:null};
 let inputId=null,active=null,generation=0,disposed=false,lastCleanup=Promise.resolve();
 let authorizationRecovery=null,custodyPending=0,custodyFailed=false;
 const failedStops=new Set();
 const listeners=new Set();
 const timerKeys=['renewTimer','grantDeadlineTimer','heartbeatTimer','presenceDeadlineTimer','recoveryTimer','recoveryDeadlineTimer'];

 const report=error=>{try{onError(error);}catch{}};
 const notify=()=>{
  const value={...snapshot};
  for(const listener of [...listeners])try{listener(value);}catch(error){report(error);}
 };
 const update=change=>{snapshot={...snapshot,...change};notify();};
 const isCurrent=part=>!disposed&&active===part&&!part.closed&&part.generation===generation;
 const isCarrierCurrent=(part,carrier)=>Boolean(carrier)&&isCurrent(part)&&part.carrier===carrier&&!carrier.closed&&carrier.generation===part.carrierGeneration;
 const settle=operation=>{
  try{return Promise.resolve(operation()).catch(error=>{report(error);});}
  catch(error){report(error);return Promise.resolve();}
 };
 const stopLocal=(part,operation)=>settle(async()=>{
  try{await operation();}catch(error){
   part.cleanupFailed=true;custodyFailed=true;
   // Keep the actual owner reachable when its stop outcome is failed.
   failedStops.add(operation);throw error;
  }
 });
 const cancelTimer=(part,key)=>{
  if(part[key]!==null)clearTimer(part[key]);
  part[key]=null;
 };
 const schedule=(part,key,fn,delay)=>{
  cancelTimer(part,key);
  part[key]=setTimer(()=>{part[key]=null;fn();},Math.max(0,delay));
 };
 const cancelTimers=part=>{for(const key of timerKeys)cancelTimer(part,key);};
 const trackCleanup=(part,promise)=>{
  const task=Promise.resolve(promise);
  part.pendingCleanup.add(task);
  task.then(()=>part.pendingCleanup.delete(task),()=>part.pendingCleanup.delete(task));
  return task;
 };
 const trackRaw=(part,promise)=>{
  const result=Promise.resolve(promise),settled=result.then(()=>{},()=>{});
  part.rawOperations.add(settled);void settled.then(()=>part.rawOperations.delete(settled));
  return result;
 };
 const boundedCleanup=(tasks,timeout=ATTEMPT_TIMEOUT_MS)=>{
  if(tasks.length===0)return Promise.resolve();
  let timeoutId;
  const deadline=new Promise((_,reject)=>{timeoutId=setTimer(()=>reject(abortError('Media cleanup incomplete','cleanup-incomplete')),timeout);});
  return Promise.race([Promise.allSettled(tasks),deadline]).finally(()=>clearTimer(timeoutId));
 };
 const request=(part,kind,signal=part.controller.signal)=>authority[kind]({callId:part.callId,participationId:part.participationId,generation:part.generation,signal,...(part.grant===null?{}:{grant:part.grant})});
 const leaveRequest=part=>({callId:part.callId,participationId:part.participationId,generation:part.generation,...(part.grant===null&&part.resumeGrant===null?{}:{grant:part.grant??part.resumeGrant})});
 const release=part=>{
  if(part.leaveSent||!part.joined)return Promise.resolve();
  part.leaveSent=true;
  try{return Promise.resolve(authority.leave(leaveRequest(part))).catch(report);}
  catch(error){report(error);return Promise.resolve();}
 };

 function runAttempt(part,operation,{timeout=ATTEMPT_TIMEOUT_MS,onLate,awaitLate=false}={}){
  const controller=new AbortController();
  const parentAbort=()=>controller.abort(part.controller.signal.reason??abortError('Participation ended'));
  part.controller.signal.addEventListener('abort',parentAbort,{once:true});
  const timeoutId=setTimer(()=>controller.abort(abortError('Media operation timed out','attempt-timeout')),timeout);
  let abandoned=false,completed=false;
  const scope={cancel(){abandoned=true;clearTimer(timeoutId);controller.abort(abortError('Participation ended'));}};
  part.attempts.add(scope);
  const raw=Promise.resolve().then(()=>operation(controller.signal));
  const lateTask=raw.then(value=>abandoned&&onLate?settle(()=>onLate(value)):undefined,()=>{});
  trackRaw(part,lateTask);
  if(awaitLate)trackCleanup(part,lateTask);
  const aborted=new Promise((_,reject)=>{
   if(controller.signal.aborted)reject(controller.signal.reason??abortError('Media operation aborted'));
   else controller.signal.addEventListener('abort',()=>reject(controller.signal.reason??abortError('Media operation aborted')),{once:true});
  });
  return Promise.race([raw,aborted]).then(value=>{completed=true;return value;}).finally(()=>{
   abandoned=!completed;clearTimer(timeoutId);part.attempts.delete(scope);
   part.controller.signal.removeEventListener('abort',parentAbort);
  });
 }

 function closeCarrier(part,carrier){
  if(!carrier)return Promise.resolve();
  if(carrier.cleanupPromise)return carrier.cleanupPromise;
  carrier.closed=true;
  if(part.carrier===carrier)part.carrier=null;
  if(part.openingCarrier===carrier)part.openingCarrier=null;
  if(carrier.timeout!==null)clearTimer(carrier.timeout);
  carrier.timeout=null;
  part.controller.signal.removeEventListener('abort',carrier.parentAbort);
  carrier.controller.abort(abortError('Media carrier closed'));
  const cleanup=carrier.subscription;carrier.subscription=null;
  const session=carrier.session;carrier.session=null;
  const tasks=[];
  if(cleanup)tasks.push(stopLocal(part,cleanup));
  if(session)tasks.push(stopLocal(part,()=>session.close()));
  carrier.cleanupPromise=Promise.allSettled(tasks).then(()=>{});
  return carrier.cleanupPromise;
 }

 async function openCarrier(part,grant){
  const carrier={generation:++part.carrierGeneration,controller:new AbortController(),parentAbort:null,session:null,subscription:null,closed:false,timeout:null,cleanupPromise:null};
  carrier.parentAbort=()=>carrier.controller.abort(part.controller.signal.reason??abortError('Participation ended'));
  part.controller.signal.addEventListener('abort',carrier.parentAbort,{once:true});
  part.openingCarrier=carrier;
  carrier.timeout=setTimer(()=>carrier.controller.abort(abortError('Media carrier timed out','attempt-timeout')),ATTEMPT_TIMEOUT_MS);
  const aborted=new Promise((_,reject)=>carrier.controller.signal.addEventListener('abort',()=>reject(carrier.controller.signal.reason??abortError('Media carrier aborted')),{once:true}));
  try{
   const rawSession=Promise.resolve().then(()=>transport.connect({grant,signal:carrier.controller.signal}));
   trackCleanup(part,rawSession.then(session=>carrier.closed?stopLocal(part,()=>session.close()):undefined,()=>{}));
   const session=await Promise.race([rawSession,aborted]);
   if(!isCurrent(part)||part.openingCarrier!==carrier)throw abortError('Stale media carrier');
   carrier.session=session;
   if(typeof session?.publish!=='function'||typeof session?.subscribe!=='function'||typeof session?.close!=='function'||typeof session?.refreshAuthorization!=='function'||typeof session?.closed?.then!=='function')throw Error('Incomplete media transport session');
   if(grant.directions?.subscribe===true){
   const rawSubscription=Promise.resolve().then(()=>session.subscribe({
    signal:carrier.controller.signal,
    onFrame:frame=>{
     if(!isCarrierCurrent(part,carrier))return;
     try{
      const result=part.audio?.play(frame);
      if(result?.then)void result.catch(error=>{if(isCarrierCurrent(part,carrier))report(error);});
     }catch(error){report(error);}
    },
    onPublisherEnded:publisherId=>{
     if(!isCarrierCurrent(part,carrier)||typeof part.audio?.removePublisher!=='function')return;
     try{
      const result=part.audio.removePublisher(publisherId);
      if(result?.then)void result.catch(error=>{if(isCarrierCurrent(part,carrier))report(error);});
     }catch(error){report(error);}
    },
   }));
   trackCleanup(part,rawSubscription.then(value=>{if(carrier.closed){const cleanup=cleanupOf(value);if(cleanup)return stopLocal(part,cleanup);}},()=>{}));
   carrier.subscription=cleanupOf(await Promise.race([rawSubscription,aborted]));
   }
   if(!isCurrent(part)||part.openingCarrier!==carrier)throw abortError('Stale media carrier');
   clearTimer(carrier.timeout);carrier.timeout=null;part.openingCarrier=null;part.carrier=carrier;
   Promise.resolve(session.closed).then(()=>beginRecovery(part,Error('Media transport closed'),carrier),error=>beginRecovery(part,error,carrier));
   return carrier;
  }catch(error){await closeCarrier(part,carrier);throw error;}
 }

 function stopResources(part){
  const tasks=[];
  const resource=part.audio;part.audio=null;
  if(resource)tasks.push(stopLocal(part,()=>resource.stop()));
  cancelTimers(part);
  for(const attempt of [...part.attempts])attempt.cancel();
  part.controller.abort(abortError('Participation ended'));
  if(part.openingCarrier)tasks.push(closeCarrier(part,part.openingCarrier));
  if(part.carrier)tasks.push(closeCarrier(part,part.carrier));
  tasks.push(...part.pendingCleanup);
  part.resourceCleanup=Promise.allSettled(tasks);
  // Every retiring generation retains custody, including fresh recovery
  // capture permission/input work. Aborting its observer is not settlement.
  part.custody=Promise.allSettled([part.resourceCleanup,...part.rawOperations]);
  custodyPending++;void part.custody.then(()=>{custodyPending--;});
  return boundedCleanup(tasks).then(()=>{
   if(part.cleanupFailed)throw abortError('Media cleanup failed','cleanup-incomplete');
  });
 }
 const completeAuthorization=(part,result=Promise.resolve())=>{
  if(authorizationRecovery?.part!==part)return;
  const completion=authorizationRecovery;authorizationRecovery=null;
  Promise.resolve(result).then(completion.resolve,completion.reject);
 };
 const finish=(part,state,reason,{notifyAuthority=true}={})=>{
  if(part.finishPromise){if(notifyAuthority)void release(part);return part.finishPromise;}
  part.closed=true;if(active===part)active=null;
  if(part.generation===generation)generation++;
  const cleanup=stopResources(part);
  // A null state retires the participation without publishing an end (used
  // by an in-place rejoin, which keeps the session visibly reconnecting).
  if(state!==null)update({state,callId:null,reason:part.cleanupFailed?'cleanup-incomplete':reason});
  if(notifyAuthority)void release(part);
  part.finishPromise=cleanup;lastCleanup=cleanup;completeAuthorization(part,cleanup);
  void cleanup.catch(error=>{
   if(active===null&&generation===part.generation+1)update({reason:'cleanup-incomplete'});
   report(error);
  });
  return cleanup;
 };

 function checkedDeadline(value,maximum){
  const remaining=value-now();
  if(!Number.isFinite(value)||!Number.isFinite(remaining)||remaining<=0||remaining>maximum)throw Object.assign(Error('Invalid or expired verified media deadline'),{code:'expired'});
  return value;
 }
 function armGrantDeadline(part){
  part.grantExpiresAt=checkedDeadline(part.grant?.expiresAt,GRANT_LIFETIME_MS);
  schedule(part,'grantDeadlineTimer',()=>{if(isCurrent(part))void finish(part,'ended','renewal-expired');},part.grantExpiresAt-now());
 }
 function armPresenceDeadline(part,expiresAt){
  part.presenceExpiresAt=checkedDeadline(expiresAt,PRESENCE_LIFETIME_MS);
  schedule(part,'presenceDeadlineTimer',()=>{if(isCurrent(part))void finish(part,'ended','presence-expired');},part.presenceExpiresAt-now());
 }
 function scheduleHeartbeat(part){
  if(!isCurrent(part)||!part.joined)return;
  schedule(part,'heartbeatTimer',()=>void heartbeat(part).catch(report),HEARTBEAT_MS);
 }
 async function heartbeat(part){
  if(!isCurrent(part))return;
  try{
   const presence=await runAttempt(part,signal=>request(part,'heartbeat',signal),{timeout:HEARTBEAT_TIMEOUT_MS});
   if(!isCurrent(part))return;
   armPresenceDeadline(part,presence?.expiresAt);
  }catch(error){
   if(!isCurrent(part))return;
   if(staleError(error)){report(error);return rejoinCall(part);}
   if(permissionError(error)||expiredError(error)){report(error);await finish(part,'ended','permission');return;}
   report(error);
  }
  scheduleHeartbeat(part);
 }

 function scheduleRenew(part){
  if(!isCurrent(part)||snapshot.state!=='live')return;
  schedule(part,'renewTimer',()=>void renew(part).catch(report),Math.min(RENEW_MS,Math.max(0,part.grantExpiresAt-now()-1000)));
 }
 async function renew(part){
  if(!isCurrent(part))return;
  const carrier=part.carrier;
  try{
   const grant=await runAttempt(part,async signal=>{
    const fresh=await request(part,'renew',signal);
    if(!isCarrierCurrent(part,carrier))throw abortError('Stale media carrier');
    await carrier.session.refreshAuthorization({grant:fresh,signal});
    return fresh;
   },{onLate:fresh=>releaseClosedGrant(part,fresh)});
   if(!isCarrierCurrent(part,carrier))return;
   part.grant=grant;armGrantDeadline(part);scheduleRenew(part);
  }catch(error){
   if(!isCurrent(part))return;
   if(staleError(error)){report(error);return rejoinCall(part);}
   if(permissionError(error)||expiredError(error)){report(error);await finish(part,'ended',permissionError(error)?'permission':'renewal-expired');return;}
   beginRecovery(part,error,carrier,'renewal-expired');
  }
 }
 function releaseClosedGrant(part,grant){
  if(isCurrent(part)||part.authorizationRetired)return;
  part.grant=grant;part.joined=true;part.leaveSent=false;return release(part);
 }
 function captureLateJoinGrant(part,grant){
  if(part.authorizationRetired)return;
  part.grant=grant;part.joined=true;
  if(!isCurrent(part)){part.leaveSent=false;return release(part);}
 }

 function recoveryDelay(part){
  const elapsed=now()-part.recoveryStartedAt;
  if(now()>=part.recoveryStopAt)return null;
  if(elapsed>=RECOVERY_FAST_WINDOW_MS)return Math.min(5000,part.recoveryStopAt-now());
  return Math.min(2000,250*2**part.recoveryAttempt,RECOVERY_FAST_WINDOW_MS-elapsed,part.recoveryStopAt-now());
 }
 function beginRecovery(part,error,carrier=part.carrier,endReason='recovery-timeout'){
  if(!isCurrent(part)||!isCarrierCurrent(part,carrier)||snapshot.state==='reconnecting')return;
  report(error);cancelTimer(part,'renewTimer');void closeCarrier(part,carrier);
  part.recoveryStartedAt=now();part.recoveryStopAt=Math.min(now()+RECOVERY_STOP_MS,part.grantExpiresAt??Number.POSITIVE_INFINITY);part.recoveryAttempt=0;part.recoveryEndReason=endReason;
  cancelTimer(part,'grantDeadlineTimer');
  schedule(part,'recoveryDeadlineTimer',()=>{if(isCurrent(part))void finish(part,'ended',part.recoveryEndReason);},part.recoveryStopAt-now());
  update({state:'reconnecting',reason:'connection'});scheduleRecovery(part);
 }
 function scheduleRecovery(part){
  if(!isCurrent(part))return;
  const delay=recoveryDelay(part);
  if(delay===null){void finish(part,'ended',part.recoveryEndReason);return;}
  schedule(part,'recoveryTimer',()=>void recover(part).catch(report),delay);
 }
 async function recover(part){
  if(!isCurrent(part))return;
  try{
   const grant=await runAttempt(part,signal=>request(part,'renew',signal),{onLate:fresh=>releaseClosedGrant(part,fresh)});
   if(!isCurrent(part))return;
   checkedDeadline(grant?.expiresAt,GRANT_LIFETIME_MS);
   await openCarrier(part,grant);
   if(!isCurrent(part))return;
   part.grant=grant;part.recoveryStartedAt=null;part.recoveryAttempt=0;
   cancelTimer(part,'recoveryDeadlineTimer');armGrantDeadline(part);
   update({state:'live',reason:null});scheduleRenew(part);
  }catch(error){
   if(!isCurrent(part))return;
   if(staleError(error)){report(error);return rejoinCall(part);}
   if(permissionError(error)||expiredError(error)){report(error);await finish(part,'ended','permission');return;}
   report(error);if(part.openingCarrier)void closeCarrier(part,part.openingCarrier);
   part.recoveryAttempt++;scheduleRecovery(part);
  }
 }

 const publish=(part,frame)=>{
  const carrier=part.carrier;
  if(!isCarrierCurrent(part,carrier)||snapshot.state!=='live'||snapshot.muted)return;
  try{
   const result=carrier.session.publish(frame);
   if(result?.then)void result.catch(error=>{if(isCarrierCurrent(part,carrier))beginRecovery(part,error,carrier);});
  }catch(error){beginRecovery(part,error,carrier);}
 };

 function newParticipation(callId){
  const part={callId,participationId:participationId(),generation:++generation,controller:new AbortController(),closed:false,joined:false,leaveSent:false,grant:null,resumeGrant:null,audio:null,carrier:null,openingCarrier:null,carrierGeneration:0,attempts:new Set(),pendingCleanup:new Set(),rawOperations:new Set(),finishPromise:null,muteSequence:0,inputSequence:0,grantExpiresAt:null,presenceExpiresAt:null,recoveryStartedAt:null,recoveryStopAt:null,recoveryAttempt:0,recoveryEndReason:'recovery-timeout'};
  for(const key of timerKeys)part[key]=null;
  return part;
 }
 function expireAuthorization(){
  const recovering=authorizationRecovery;
  if(recovering&&active?.grant===null)return recovering.promise;
  const old=active;
  if(!old||disposed)return lastCleanup;
  if(!old.joined||old.grant===null)return finish(old,'ended','authorization-expired');
  const prior=old.grant;
  old.authorizationRetired=true;old.closed=true;old.grant=null;old.joined=false;
  const part=newParticipation(old.callId);part.resumeGrant=prior;part.joined=true;
  active=part;
  if(recovering){recovering.part=part;authorizationRecovery=recovering;}
  else{
   let resolve,reject;
   const promise=new Promise((yes,no)=>{resolve=yes;reject=no;});
   authorizationRecovery={part,promise,resolve,reject,startedAt:now(),stopAt:now()+RECOVERY_STOP_MS};void promise.catch(report);
  }
  const {promise,startedAt,stopAt}=authorizationRecovery;
  old.finishPromise=stopResources(old);void old.finishPromise.catch(report);
  // Abort is only an observation fence. No fresh resource is allocated until
  // the abandoned raw operations and actual local stop work have settled.
  const custody=old.custody;
  trackCleanup(part,custody);
  part.recoveryStartedAt=startedAt;part.recoveryStopAt=stopAt;
  schedule(part,'recoveryDeadlineTimer',()=>{if(isCurrent(part))void finish(part,'ended','authorization-expired');},stopAt-now());
  update({state:'reconnecting',reason:'authorization'});
  void boundedCleanup([custody]).then(()=>{
   if(old.cleanupFailed)throw abortError('Media cleanup failed','cleanup-incomplete');
   if(isCurrent(part))void reauthorize(part).catch(report);
  }).catch(error=>{
   if(isCurrent(part)){report(error);completeAuthorization(part,Promise.reject(error));void finish(part,'ended','cleanup-incomplete');}
  });
  return promise;
 }
 function releaseLateAuthorization(part,grant){
  part.grant=grant;part.resumeGrant=null;part.joined=true;part.leaveSent=false;
  if(isCurrent(part)){
   // Never await this attempt's own late-cleanup task through finish().
   void finish(part,'ended','authorization-expired');return;
  }
  return release(part);
 }
 async function reauthorize(part){
  if(!isCurrent(part))return;
  try{
   if(typeof authority.reauthorize!=='function')throw Object.assign(Error('Media reauthorization unavailable'),{terminal:true});
   const grant=await runAttempt(part,signal=>authority.reauthorize({...leaveRequest(part),grant:part.resumeGrant,signal}),{onLate:fresh=>releaseLateAuthorization(part,fresh),awaitLate:true});
   if(!isCurrent(part))return;
   const prior=part.resumeGrant;part.grant=grant;part.resumeGrant=null;part.leaveSent=false;
   if(grant===prior||(prior.handle!==undefined&&grant?.handle===prior.handle)||grant?.directions?.publish!==prior.directions?.publish||grant?.directions?.subscribe!==prior.directions?.subscribe)throw Object.assign(Error('Media authorization scope changed'),{terminal:true});
   armGrantDeadline(part);armPresenceDeadline(part,grant.presenceExpiresAt);scheduleHeartbeat(part);
   const startInput=inputId;
   const resource=await runAttempt(part,signal=>audio.start({inputId:startInput,muted:true,signal,onFrame:frame=>publish(part,frame)}),{onLate:value=>stopLocal(part,()=>value.stop()),awaitLate:true});
   if(!isCurrent(part)){await settle(()=>resource.stop());return;}
   part.audio=resource;
   if(inputId!==startInput&&inputId!==null)await trackRaw(part,resource.selectInput(inputId));
   if(!isCurrent(part))return;
   await openCarrier(part,grant);
   if(!isCurrent(part))return;
   await setMuted(snapshot.muted);
   if(!isCurrent(part))return;
   cancelTimer(part,'recoveryDeadlineTimer');update({state:'live',reason:null});scheduleRenew(part);completeAuthorization(part);
  }catch(error){
   if(!isCurrent(part))return;
   report(error);
   // Once a fresh participation exists, failure tears it down. Only failed
   // authority refresh itself may retry the same retained intent.
   if(part.resumeGrant===null||permissionError(error)||expiredError(error)){
    await finish(part,'ended',permissionError(error)?'permission':'authorization-expired');return;
   }
   if(part.rawOperations.size!==0){
    await finish(part,'ended','cleanup-incomplete');return;
   }
   const delay=recoveryDelay(part);
   if(delay===null){await finish(part,'ended','authorization-expired');return;}
   part.recoveryAttempt++;schedule(part,'recoveryTimer',()=>void reauthorize(part).catch(report),delay);
  }
 }

 let rejoining=null;
 const sleepFor=(ms,ticket)=>new Promise(resolve=>{const id=setTimer(resolve,ms);ticket.wake=()=>{clearTimer(id);resolve();};});
 async function rejoinCall(part){
  if(!isCurrent(part)||rejoining)return;
  const ticket={callId:part.callId,muted:snapshot.muted,cancelled:false,wake:null};rejoining=ticket;
  update({state:'reconnecting',reason:'authorization'});
  const stopAt=now()+RECOVERY_STOP_MS;
  let attempt=0,ended='rejoin-failed';
  try{
   await finish(part,null,'superseded').catch(()=>{});
   await boundedCleanup([part.custody]).catch(()=>{});
   // The durable session can be re-authenticating at the same moment (a
   // moderation commit may reset it); retry the rejoin with backoff inside
   // the normal recovery window instead of ending the call on first failure.
   for(;;){
    if(ticket.cancelled||disposed||active)return;
    try{
     await join({callId:ticket.callId},{rejoin:ticket});
     // Preserve the person's microphone intent across the in-place rejoin.
     if(!ticket.cancelled&&!ticket.muted&&snapshot.state==='live'&&active?.callId===ticket.callId)await setMuted(false).catch(report);
     return;
    }catch(error){
     report(error);
     if(permissionError(error)||expiredError(error)){ended=permissionError(error)?'permission':'rejoin-failed';break;}
     if(error?.code==='cleanup-incomplete')await boundedCleanup([lastCleanup]).catch(()=>{});
    }
    const delay=Math.min(attempt<4?250*2**attempt:5000,stopAt-now());attempt++;
    if(delay<=0)break;
    await sleepFor(delay,ticket);
   }
   if(!ticket.cancelled&&!disposed&&!active)update({state:'ended',callId:null,reason:ended});
  }catch(error){report(error);if(!ticket.cancelled&&!disposed&&!active)update({state:'ended',callId:null,reason:ended});}
  finally{if(rejoining===ticket)rejoining=null;}
 }
 async function join({callId}={},{rejoin=null}={}){
  if(rejoin===null&&rejoining){rejoining.cancelled=true;rejoining.wake?.();rejoining=null;}
  if(disposed)throw Error('Media session disposed');
  if(typeof callId!=='string'||callId.length===0)throw new TypeError('callId is required');
  if(active)throw Error('Media session is already participating');
  if(custodyPending||custodyFailed)throw abortError('Media cleanup incomplete','cleanup-incomplete');
  const part=newParticipation(callId);
  active=part;update(rejoin?{callId}:{state:'preparing',muted:true,callId,reason:null});
  let stage='capability';
  try{
   const capabilities=await authority.capabilities({signal:part.controller.signal});
   if(!availableMedia(capabilities))throw Object.assign(Error('Media unavailable'),{code:'unsupported-media'});
   if(!isCurrent(part))return;
   stage='audio';
   const startMuted=snapshot.muted,startInput=inputId;
   const resource=await runAttempt(part,signal=>audio.start({inputId:startInput,muted:startMuted,signal,onFrame:frame=>publish(part,frame)}),{onLate:value=>stopLocal(part,()=>value.stop()),awaitLate:true});
   if(!isCurrent(part)){await settle(()=>resource.stop());return;}
   part.audio=resource;
   if(snapshot.muted!==startMuted)await trackRaw(part,resource.setMuted(snapshot.muted));
   if(inputId!==startInput&&inputId!==null)await trackRaw(part,resource.selectInput(inputId));
   if(!isCurrent(part))return;
   stage='join';
   const grant=await runAttempt(part,signal=>request(part,'join',signal),{onLate:fresh=>captureLateJoinGrant(part,fresh)});
   part.grant=grant;part.joined=true;
   if(!isCurrent(part)){await release(part);return;}
   armGrantDeadline(part);armPresenceDeadline(part,grant.presenceExpiresAt);scheduleHeartbeat(part);
   if(!rejoin)update({state:'joining',reason:null});
   await openCarrier(part,grant);
   if(!isCurrent(part))return;
   update({state:'live',reason:null});scheduleRenew(part);
  }catch(error){
   if(!isCurrent(part))return;
   let state='ended',reason='join-failed';
   if(stage==='capability'){state='unavailable';reason=error?.code==='unsupported-media'?'unsupported':'capability';}
   else if(permissionError(error)){state='unavailable';reason='permission';}
   // An in-place rejoin attempt retires silently; its caller retries/ends.
   report(error);await finish(part,rejoin?null:state,reason);throw error;
  }
 }

 function leave(){
  if(rejoining){rejoining.cancelled=true;rejoining.wake?.();rejoining=null;if(!active){update({state:'ended',callId:null,reason:'left'});return lastCleanup;}}
  if(!active)return lastCleanup;
  const part=active;update({state:'leaving',reason:null});return finish(part,'ended','left');
 }
 async function setMuted(value){
  if(disposed)throw Error('Media session disposed');
  if(typeof value!=='boolean')throw new TypeError('muted must be boolean');
  update({muted:value});
  const part=active,resource=part?.audio;
  if(!part||!resource)return;
  const sequence=++part.muteSequence;
  try{await trackRaw(part,resource.setMuted(value));}
  catch(error){if(isCurrent(part)&&part.audio===resource&&part.muteSequence===sequence){if(!value)update({muted:true,reason:'permission'});report(error);}throw error;}
 }
 async function selectInput(value){
  if(disposed)throw Error('Media session disposed');
  if(typeof value!=='string'||value.length===0)throw new TypeError('input id is required');
  inputId=value;
  const part=active,resource=part?.audio;
  if(!part||!resource)return;
  const sequence=++part.inputSequence;
  try{await trackRaw(part,resource.selectInput(value));}
  catch(error){if(isCurrent(part)&&part.audio===resource&&part.inputSequence===sequence){update({reason:permissionError(error)?'permission':'input'});report(error);}throw error;}
 }
 function subscribe(listener){
  if(typeof listener!=='function')throw new TypeError('listener is required');
  if(disposed)throw Error('Media session disposed');
  listeners.add(listener);return ()=>listeners.delete(listener);
 }
 function dispose(){
  if(disposed)return lastCleanup;
  disposed=true;if(rejoining){rejoining.cancelled=true;rejoining.wake?.();rejoining=null;}
  const part=active;
  let cleanup;
  if(part){update({state:'leaving',reason:null});cleanup=finish(part,'ended','disposed');}
  else{update({state:'ended',callId:null,reason:'disposed'});cleanup=lastCleanup;}
  lastCleanup=cleanup;listeners.clear();return cleanup;
 }
 bindAuthorizationControl?.(Object.freeze({expire:expireAuthorization}));
 return {version:1,getSnapshot:()=>({...snapshot}),subscribe,join:options=>join(options),leave,setMuted,selectInput,dispose};
}
