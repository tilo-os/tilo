import {createMediaSession} from './media-session.mjs';
import {createBrowserAudio} from './media-audio.mjs';
import {createMoqTransport} from './media-moq.mjs';

const TARGETS=new Set(['/peer/v3/bootstrap','/peer/v3/challenge','/peer/v3/sessions',...['grant/challenge','grant/complete','relay/challenge','relay/refresh/challenge','relay/refresh/complete','relay/close','presence'].map(path=>`/peer/v3/media/${path}`)]);
const namespacePattern=/^[0-9a-f]{64}\/[0-9a-f]{64}$/;
const failure=(message,code='unavailable')=>Object.assign(Error(message),{code});
function bytes(value,max){if(!Array.isArray(value)||value.length===0||value.length>max||value.some(v=>!Number.isInteger(v)||v<0||v>255))throw failure('Invalid bounded Rust media bytes');return Uint8Array.from(value);}
function handle(value){if(!Number.isInteger(value)||value<=0||value>0xffffffff)throw failure('Invalid Rust media handle');return value;}
function decoded(value){const parsed=JSON.parse(value);if(!parsed||typeof parsed.kind!=='string')throw failure('Invalid Rust media result');return parsed;}
function inventory(value){
 if(!value||typeof value.revision!=='string'||!/^[1-9][0-9]{0,19}$/.test(value.revision)||BigInt(value.revision)>0xffffffffffffffffn||!Array.isArray(value.namespaces)||value.namespaces.length>7)throw failure('Invalid signed media inventory');
 let previous='';for(const name of value.namespaces){if(typeof name!=='string'||!namespacePattern.test(name)||name<=previous)throw failure('Invalid exact media namespace');previous=name;}
 return {revision:value.revision,namespaces:[...value.namespaces]};
}
function mapped(error){
 if(error?.code)return error;
 const text=String(error?.message??error);
 if(/unauthorized|permission|denied/i.test(text))return failure('Media permission denied','permission');
 if(/expired|expiry|revoked/i.test(text))return failure('Media authorization expired','expired');
 if(/stale|mismatch|wrong .*purpose|continuity|signature/i.test(text))return Object.assign(failure('Media authorization context changed','authorization-changed'),{terminal:true});
 return failure('Media control unavailable');
}

/** The existing Peer owns all identity/proof state. This module only ferries bounded bytes. */
// Content-free transport counters (delivery mode, redundancy, repaired and
// missing objects) are announced as a page event for diagnostics; nothing in
// the product listens, and no media, identity or namespace is included.
function reportTransportStats(value){
 if(value?.type!=='media-transport-stats'||typeof globalThis.dispatchEvent!=='function'||typeof globalThis.CustomEvent!=='function')return;
 const {type,...detail}=value;
 try{globalThis.dispatchEvent(new CustomEvent('qos-media-transport-stats',{detail}));}catch{}
}

export function createPeerMedia({runtime,peerId,persist,refreshHint,fetch:fetcher=globalThis.fetch,audio=createBrowserAudio(),transportFactory=createMoqTransport,clock={},onError=()=>{}}){
 const now=clock.now??(()=>performance.now()),setTimer=clock.setTimeout??globalThis.setTimeout,clearTimer=clock.clearTimeout??globalThis.clearTimeout;
 const life=new AbortController(),requests=new Set(),contexts=new Map(),carriers=new Map();
 let authorizationControl,authorizationExpiry;
 let initialized,initializing=false,initializationGeneration=0,needsRefresh=false,installed=false,available=false,disposed=false,disposal;
 const report=error=>{try{onError(error);}catch{}};
 function scope(operation,parent,{lifetime=true}={}){
  const controller=new AbortController();const abort=()=>controller.abort(parent?.reason??failure('Media operation aborted','aborted'));
  const stop=()=>controller.abort(life.signal.reason??failure('Media disposed','aborted'));
  if(parent?.aborted)abort();else parent?.addEventListener('abort',abort,{once:true});
  if(lifetime){if(life.signal.aborted)stop();else life.signal.addEventListener('abort',stop,{once:true});}
  const timer=setTimer(()=>controller.abort(failure('Media operation timed out','attempt-timeout')),10000);requests.add(controller);
  const aborted=new Promise((_,reject)=>{if(controller.signal.aborted)reject(controller.signal.reason);else controller.signal.addEventListener('abort',()=>reject(controller.signal.reason),{once:true});});
  return Promise.race([Promise.resolve().then(()=>operation(controller.signal)),aborted]).finally(()=>{clearTimer(timer);requests.delete(controller);parent?.removeEventListener('abort',abort);life.signal.removeEventListener('abort',stop);});
 }
 async function send(value,signal){
  signal.throwIfAborted();
  const origin=new URL(value.origin);if(origin.protocol!=='https:'||origin.origin!==value.origin||origin.username||origin.password||!TARGETS.has(value.target))throw failure('Invalid prepared media target');
  const body=bytes(value.body,value.target==='/peer/v3/bootstrap'?4096:8192);
  const headers={'Content-Type':'application/octet-stream'};
  if(value.proof!==null&&value.proof!==undefined)headers['x-tilo-delivery-proof']=[...bytes(value.proof,256)].map(v=>v.toString(16).padStart(2,'0')).join('');
  const response=await fetcher(`${value.origin}${value.target}`,{method:'POST',headers,body,signal,redirect:'error',credentials:'omit',cache:'no-store'});
  signal.throwIfAborted();
  // 409: the host superseded this participation (call epoch advanced by
  // moderation) while the person stays permitted; the controller rejoins.
  if(!response.ok)throw failure(response.status===409?'Media participation superseded':'Native media request failed',response.status===401||response.status===403?'permission':response.status===409?'stale':'unavailable');
  const maximum=value.target==='/peer/v3/bootstrap'?32768:8192;
  const reader=response.body?.getReader();if(!reader)throw failure('Native media response body missing');
  const output=new Uint8Array(maximum);let size=0;
  try{for(;;){const item=await reader.read();signal.throwIfAborted();if(item.done)break;if(size+item.value.byteLength>maximum)throw failure('Native media response exceeds bounds');output.set(item.value,size);size+=item.value.byteLength;}}
  finally{try{await reader.cancel();}catch{}}
  if(size===0)throw failure('Native media response empty');
  return output.slice(0,size);
 }
 async function run(initial,signal){
  let value=decoded(initial);
  for(let steps=0;value.kind==='request';steps++){
   if(steps>=4)throw failure('Media control exceeded bounded stage count');
   const id=value.id;handle(id);
   try{const response=await send(value,signal);signal.throwIfAborted();value=decoded(runtime.peer_media_advance(peerId,id,response));}
   finally{try{runtime.peer_media_forget(peerId,id);}catch{}}
  }
  signal.throwIfAborted();return value;
 }
 async function initialize({fresh=false,signal:parent}={}){
  if(disposed)return false;
  if(fresh&&!initializing&&installed&&runtime.peer_media_current(peerId)!==true)needsRefresh=true;
  const refresh=fresh&&!initializing&&needsRefresh;
  if(refresh){initialized=undefined;available=false;runtime.peer_media_reset(peerId);}
  if(initialized)return initialized;
  const ticket=++initializationGeneration;initializing=true;
  initialized=scope(async signal=>{
   if(refresh){if(typeof refreshHint!=='function')throw failure('Media enrollment refresh unavailable','expired');await refreshHint(signal);signal.throwIfAborted();}
   const support=await audio.capabilities();signal.throwIfAborted();
   if(!support.supported||!support.playback||!support.capture)return false;
   const first=decoded(runtime.peer_media_bootstrap(peerId));
   if(first.kind!=='ready'){
    if(first.kind!=='request'||first.target!=='/peer/v3/bootstrap')throw failure('Invalid bootstrap stage');
    const response=await send(first,signal);signal.throwIfAborted();
    runtime.peer_media_bootstrap_complete(peerId,response);
    await persist();signal.throwIfAborted();
    const ready=await run(runtime.peer_media_start_http(peerId),signal);
    if(ready.kind!=='ready')throw failure('Native media handshake incomplete');
   }
   available=runtime.peer_media_supported(peerId)===true;
   if(available){installed=true;needsRefresh=false;}
   return available;
  },parent).catch(error=>{if(ticket===initializationGeneration){initialized=undefined;available=false;try{runtime.peer_media_reset(peerId);}catch{}}throw mapped(error);}).finally(()=>{if(ticket===initializationGeneration)initializing=false;});
  return initialized;
 }
 function budget(value,max,started){if(!Number.isFinite(value)||value<=0||value>max)throw failure('Invalid verified media lifetime','expired');const deadline=started+value;if(deadline<=now())throw failure('Media authorization expired in transit','expired');return deadline;}
 function grant(value,started){
  if(value.kind!=='grant'||!namespacePattern.test(value.publishNamespace)||typeof value.directions?.publish!=='boolean'||typeof value.directions?.subscribe!=='boolean')throw failure('Invalid typed media grant');
  return Object.freeze({handle:handle(value.handle),expiresAt:budget(value.expiresInMs,60000,started),presenceExpiresAt:budget(value.presenceInMs,15000,started),directions:{...value.directions},publishNamespace:value.publishNamespace,inventory:inventory(value.inventory)});
 }
 const key=context=>`${context.generation}:${context.participationId}`;
 function leaveHandle(value){
  let initial;try{initial=runtime.peer_media_leave(peerId,value);}catch(error){return Promise.reject(mapped(error));}
  return scope(signal=>run(initial,signal),undefined,{lifetime:false}).catch(error=>{report(mapped(error));});
 }
 const authority={
  async capabilities({signal}={}){return {version:1,available:await initialize({fresh:true,signal})};},
  async join(context){
   if(disposed||!available)throw failure('Media unavailable','unsupported-media');
   const started=now();let retained;
   try{
    // Join reserves native publication permission; local capture remains muted.
    const initial=runtime.peer_media_join(peerId,context.callId,true,true);
    retained=handle(decoded(initial).id);if(contexts.size>=2)throw failure('Media intent capacity');contexts.set(key(context),retained);
    const result=grant(await run(initial,context.signal),started);if(result.handle!==retained)throw failure('Media intent changed');return result;
   }catch(error){if(retained){contexts.delete(key(context));void leaveHandle(retained);}throw mapped(error);}
  },
  async reauthorize(context){
   if(disposed)throw failure('Media disposed','aborted');
   const previous=handle(context.grant?.handle);
   try{
    if(typeof refreshHint!=='function')throw failure('Media enrollment refresh unavailable','expired');
    await refreshHint(context.signal);context.signal.throwIfAborted();
    const ready=decoded(runtime.peer_media_bootstrap(peerId));
    if(ready.kind!=='ready'||runtime.peer_media_supported(peerId)!==true)throw failure('Fresh shared media custody unavailable','expired');
    available=true;needsRefresh=false;initialized=Promise.resolve(true);
    const started=now(),initial=runtime.peer_media_reauthorize(peerId,previous);
    const result=grant(await run(initial,context.signal),started);
    // Rust correlates each request to its retained participation. A retry has
    // a fresh request ID while keeping the same native reservation and handle.
    if(result.handle===previous)throw failure('Media recovery intent changed','authorization-changed');
    for(const [id,value] of contexts)if(value===previous)contexts.delete(id);
    contexts.set(key(context),result.handle);return result;
   }catch(error){if(context.signal.aborted)void leaveHandle(previous);throw mapped(error);}
  },
  async renew(context){const started=now();try{return grant(await run(runtime.peer_media_renew(peerId,handle(context.grant.handle)),context.signal),started);}catch(error){throw mapped(error);}},
  async heartbeat(context){const started=now();try{const value=await run(runtime.peer_media_heartbeat(peerId,handle(context.grant.handle)),context.signal);if(value.kind!=='presence')throw failure('Missing signed media presence');inventory(value.inventory);return {expiresAt:budget(value.expiresInMs,15000,started)};}catch(error){throw mapped(error);}},
  leave(context){const retained=contexts.get(key(context))??context.grant?.handle;contexts.delete(key(context));for(const [id,value] of contexts)if(value===retained)contexts.delete(id);return retained?leaveHandle(handle(retained)):Promise.resolve();},
 };
 const transport={async connect({grant,signal}){
  const relay=await run(runtime.peer_media_relay(peerId,handle(grant.handle)),signal);
  if(relay.kind!=='relay'||relay.handle!==grant.handle||relay.publishNamespace!==grant.publishNamespace)throw failure('Relay intent mismatch');
  const carrier=handle(relay.carrier),authorizedInventory=inventory(relay.inventory);
  const implementation=transportFactory({url:relay.endpoint,authorize:async()=>({authorization:bytes(relay.authorization,129),carrierHandle:carrier,publishNamespace:relay.publishNamespace,subscribePrefix:relay.publishNamespace.split('/')[0],subscribeNamespaces:authorizedInventory.namespaces,directions:grant.directions}),refreshAuthorization:async({grant:next,carrierHandle,signal})=>{
   if(carrierHandle!==carrier||next.handle!==grant.handle)throw failure('Stale media carrier');
   // Roster changes are offered in place by the call-scoped relay session;
   // the signed inventory is validated but no longer pins the carrier.
   const result=await run(runtime.peer_media_refresh(peerId,next.handle,carrier),signal);if(result.kind!=='refreshed')throw failure('Media refresh not acknowledged');
   return {carrierHandle:carrier,expiresAtMs:Date.now()+Math.max(0,next.expiresAt-now())};
  },onReport:reportTransportStats});
  let session;
  try{session=await implementation.connect({grant,signal});}catch(error){void closeRemote();throw error;}
  let closed=false;
  function closeRemote(){let initial;try{initial=runtime.peer_media_close_relay(peerId,grant.handle,carrier);}catch{return Promise.resolve();}return scope(signal=>run(initial,signal),undefined,{lifetime:false}).catch(report);}
  const close=()=>{if(closed)return;closed=true;carriers.delete(carrier);try{session.close();}finally{void closeRemote();}};
  carriers.set(carrier,{close});void Promise.resolve(session.closed).then(close,close);
  return {...session,close};
 }};
 const guardedAudio={async start(options){
  const resource=await audio.start({...options,muted:true});
  return {...resource,setMuted(value){if(!value&&runtime.peer_media_can_publish(peerId)!==true)throw failure('Media publication permission unavailable','permission');return resource.setMuted(value);}};
 }};
 const controller=createMediaSession({authority,transport,audio:guardedAudio,clock:{now,setTimeout:setTimer,clearTimeout:clearTimer},onError:report,bindAuthorizationControl:control=>{authorizationControl=control;}});
 const mediaV1={...controller,setMuted(value){if(!value&&runtime.peer_media_can_publish(peerId)!==true)return Promise.reject(failure('Media publication permission unavailable','permission'));return controller.setMuted(value);}};
 function expireAuthorization(){
  if(disposed)return disposal??Promise.resolve();
  runtime.peer_media_expire(peerId);
  initializationGeneration++;initialized=undefined;initializing=false;available=false;needsRefresh=true;
  for(const request of requests)request.abort(failure('Shared authorization expired','expired'));
  const cleanup=authorizationControl.expire();
  // The controller must still see a second expiry after a fresh grant arrives;
  // it owns coalescing and the original absolute recovery deadline.
  if(authorizationExpiry)return authorizationExpiry;
  authorizationExpiry=Promise.resolve(cleanup).finally(()=>{authorizationExpiry=undefined;});
  authorizationExpiry.catch(report);return authorizationExpiry;
 }
 function dispose(){
  if(disposal)return disposal;disposed=true;available=false;
  const cleanup=mediaV1.dispose();life.abort(failure('Media disposed','aborted'));
  disposal=Promise.resolve(cleanup).then(()=>{for(const carrier of carriers.values())carrier.close();runtime.peer_media_reset(peerId);contexts.clear();});
  return disposal;
 }
 return {mediaV1,initialize,supported:()=>installed&&!disposed,expireAuthorization,dispose};
}
