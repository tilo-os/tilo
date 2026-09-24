/** Bounded HTTPS executor. The existing Rust Peer supplies all exact proofs and owns state. */
const CONTROL=new Set(['/peer/v3/bootstrap','/peer/v3/challenge','/peer/v3/sessions']);
const POST=new Set(['/peer/v3/state','/peer/v3/acks','/peer/v3/commands/catalogue','/peer/v3/commands/invoke','/peer/v3/commands/status','/peer/v3/commands/cancel','/peer/v3/reads','/peer/v3/snapshots']);
// Plain held poll, or a PIPELINED_POLL_V1 chained poll (cursor required).
const poll=/^\/peer\/v3\/events\?(?:(?:cursor=(?:[0-9a-f]{2}){1,4096}&)?mode=poll|chain=[0-9a-f]{16}&cursor=(?:[0-9a-f]{2}){1,4096}&index=(?:0|[1-9][0-9]{0,9})&mode=chain)$/;
const snapshotChunk=/^\/peer\/v3\/snapshots\/[0-9a-f]{32}\/chunks\/(?:0|[1-9][0-9]{0,9})$/;
const detach=/^\/peer\/v3\/sessions\/[0-9a-f]{32}$/;
const fail=message=>Object.assign(Error(message),{code:'delivery-unavailable'});
const FAST_RENEWAL_WINDOW_MS=10000;
const TRANSIENT_RETRIES=3;
// Held polls (a chain of up to 3) + private poll + ACK + a bounded window of
// state uploads (4). The Rust scheduler bounds retained handles itself (10).
const MAX_ACTIVE=10;
// Poll, ACK and state uploads are safe to re-issue: the host answers a resent
// state item with its original durable receipt (exact delivery id + digest).
// A chained poll the host no longer continues (the chain was reset or
// replaced) is answered 409: not a transport failure, the next poll simply
// starts a new chain.
const chainedPoll=value=>value.method==='GET'&&/[?]chain=/.test(value.target)&&poll.test(value.target);
const idempotentTarget=value=>value.method==='GET'&&poll.test(value.target)||value.method==='POST'&&(value.target==='/peer/v3/acks'||value.target==='/peer/v3/state');
function decode(value){const parsed=JSON.parse(value);if(!parsed||typeof parsed.kind!=='string')throw fail('Invalid Rust delivery step');return parsed;}
function bytes(value,max,{empty=false}={}){if(!Array.isArray(value)||(!empty&&value.length===0)||value.length>max||value.some(v=>!Number.isInteger(v)||v<0||v>255))throw fail('Prepared delivery bytes exceed bounds');return Uint8Array.from(value);}
function limits(value){
 const origin=new URL(value.origin);if(origin.protocol!=='https:'||origin.origin!==value.origin||origin.username||origin.password)throw fail('Invalid delivery origin');
 const control=value.method==='POST'&&CONTROL.has(value.target);
 const get=value.method==='GET'&&(poll.test(value.target)||snapshotChunk.test(value.target)||value.target==='/peer/v3/private/events?mode=poll');
 const remove=value.method==='DELETE'&&detach.test(value.target);
 if(!control&&!(value.method==='POST'&&POST.has(value.target))&&!get&&!remove)throw fail('Invalid exact delivery target');
 if(!Number.isInteger(value.id)||value.id<=0||value.id>0xffffffff)throw fail('Invalid delivery request handle');
 const cap=control?(value.target==='/peer/v3/bootstrap'?4096:8192):value.target==='/peer/v3/acks'?8192:value.target==='/peer/v3/state'||get?1048576:262156;
 const body=bytes(value.body,cap,{empty:get||remove});
 if((get||remove)&&body.byteLength!==0)throw fail('Unexpected delivery request body');
 const headers={'Content-Type':'application/octet-stream'};
 if(value.proof!==null&&value.proof!==undefined)headers['x-tilo-delivery-proof']=[...bytes(value.proof,256)].map(v=>v.toString(16).padStart(2,'0')).join('');
 else if(!control)throw fail('Authenticated delivery proof missing');
 const maximum=control?(value.target==='/peer/v3/bootstrap'?32768:8192):value.target==='/peer/v3/acks'||remove?8192:value.target.startsWith('/peer/v3/commands/')||value.target==='/peer/v3/reads'?262156:1048576;
 return {body,headers,maximum,empty:get||remove||value.target==='/peer/v3/acks'};
}
export function createPeerDelivery({runtime,peerId,persist,transfers,commit,clock={},fetch:fetcher=globalThis.fetch,onError=()=>{}}){
 let transientFailures=0;
 const pending=new Set(),stages=new Map();let lifetime=new AbortController(),closed=false,started,closing,detaching,refreshing,renewing=false,pumping=false,active=0,failed=false;
 const check=()=>{lifetime.signal.throwIfAborted();if(closed)throw fail('Delivery closed');};
 function track(operation){const promise=Promise.resolve().then(operation);pending.add(promise);promise.then(()=>pending.delete(promise),()=>pending.delete(promise));return promise;}
 async function send(value,{detachOnly=false}={}){
  if(!detachOnly)check();else if(!closed||value.method!=='DELETE'||!detach.test(value.target))throw fail('Invalid final detach');
  const {body,headers,maximum,empty}=limits(value);
  const controller=new AbortController(),parent=detachOnly?new AbortController().signal:lifetime.signal;
  const abort=()=>controller.abort(parent.reason);parent.addEventListener('abort',abort,{once:true});if(parent.aborted)abort();
  const valid=()=>{controller.signal.throwIfAborted();if(!detachOnly)check();};
  const retryable=!detachOnly&&idempotentTarget(value);
  const timer=(clock.setTimeout??globalThis.setTimeout)(()=>{const error=fail('Delivery request timed out');controller.abort(error);if(!detachOnly&&!retryable)report(error);},detachOnly||CONTROL.has(value.target)?10000:30000);
  try{
   valid();const response=await fetcher(`${value.origin}${value.target}`,{method:value.method,headers,...(body.byteLength?{body}:{}),signal:controller.signal,credentials:'omit',redirect:'error',cache:'no-store'});
   valid();if(response.status!==200){
    const terminal=response.status===401||response.status===403;
    // A refused data stream means the host no longer serves this HTTP session
    // (for example its consumer registration predates an authorization change).
    // Recovery must bootstrap a fresh session, which re-registers the consumer.
    const sessionRefused=terminal&&!detachOnly&&(poll.test(value.target)||value.target==='/peer/v3/acks');
    if(sessionRefused)try{runtime.peer_delivery_discard_session?.(peerId);}catch{}
    const staleChain=response.status===409&&!detachOnly&&chainedPoll(value);
    throw Object.assign(fail(`Delivery HTTP ${response.status}`),{terminal,sessionRefused,staleChain});
   }
   const reader=response.body?.getReader();if(!reader){if(empty)return new Uint8Array();throw fail('Delivery response body missing');}
   const result=new Uint8Array(maximum);let size=0;
   try{for(;;){const item=await reader.read();valid();if(item.done)break;if(!(item.value instanceof Uint8Array)||size+item.value.byteLength>maximum)throw fail('Delivery response exceeds bounds');result.set(item.value,size);size+=item.value.byteLength;}}
   finally{try{await reader.cancel();}catch{}}
   if(size===0&&!empty)throw fail('Delivery response empty');return result.slice(0,size);
  }finally{(clock.clearTimeout??globalThis.clearTimeout)(timer);parent.removeEventListener('abort',abort);}
 }
 async function drive(initial){
  let value=decode(initial);
  for(let turn=0;turn<8192;turn++){
   if(turn&&turn%8===0)await new Promise(resolve=>setTimeout(resolve,0));
   check();
   if(value.kind==='request'){
    const id=value.id,retryable=idempotentTarget(value);
    try{const response=await send(value);check();if(retryable)transientFailures=0;value=decode(runtime.peer_delivery_advance(peerId,id,response));}
    catch(error){
     // A stale chained poll: forgotten below (Rust abandons that chain), then
     // the scheduler starts a new chain at once. Not a transient failure.
     if(error?.staleChain&&established&&!closed&&!failed&&!renewing){queueMicrotask(wake);return {kind:'idle'};}
     // A lost/timed-out poll or ACK on a lossy link is retried in place: the
     // Rust scheduler re-issues it (fresh proof) once this handle is forgotten.
     // Only repeated failures escalate to full transport recovery.
     if(retryable&&established&&!error?.terminal&&!closed&&!failed&&!renewing&&transientFailures<TRANSIENT_RETRIES){
      transientFailures++;const delay=Math.min(2000,200*2**(transientFailures-1));
      (clock.setTimeout??globalThis.setTimeout)(()=>{if(!closed&&!failed&&!renewing)wake();},delay);
      return {kind:'idle'};
     }
     throw error;
    }
    finally{try{runtime.peer_delivery_forget(peerId,id);}catch{}}
   }else if(value.kind==='stage-list'){
    const operation=value.id;try{
     if(!transfers)throw fail('Transfer storage unavailable');
     const entries=await transfers.list();check();
     if(!Array.isArray(entries)||entries.length>2)throw fail('Transfer metadata count exceeded');
     const raw=JSON.stringify(entries);if(raw.length>8192)throw fail('Transfer metadata exceeds bound');
     value=decode(runtime.peer_delivery_stage_list(peerId,operation,raw));
    }finally{try{runtime.peer_delivery_forget(peerId,operation);}catch{}}
   }else if(value.kind==='stage-discard'){
    if(!transfers)throw fail('Transfer storage unavailable');
    const handle=stages.get(value.transfer.id)??await transfers.open(value.transfer);check();
    if(handle.id!==value.transfer.id)throw fail('Discarded transfer differs');
    await transfers.drop(handle);check();stages.delete(handle.id);
    value=decode(runtime.peer_delivery_stage_discarded(peerId,handle.id));
   }else if(value.kind==='stage-open'){
    if(!transfers||stages.size>=2)throw fail('Transfer storage capacity unavailable');
    const handle=await transfers.open(value.transfer);check();
    if(handle.id!==value.transfer.id||!Number.isSafeInteger(handle.nextOffset))throw fail('Transfer storage handle mismatch');
    stages.set(handle.id,handle);value=decode(runtime.peer_delivery_stage_opened(peerId,handle.id,handle.nextOffset,handle.resumeDigest??handle.cutToken));
   }else if(value.kind==='stage-renewed'){
    const handle=stage(value.transferId);
    const status=await transfers.compareAndSwapResumeDigest(handle,value.expectedDigest,value.resumeDigest);check();
    if(status.id!==handle.id||status.resumeDigest!==value.resumeDigest)throw fail('Transfer renewal receipt mismatch');
    // The original immutable handle remains the only final cleanup identity.
    value=decode(runtime.peer_delivery_stage_renewed(peerId,handle.id,status.resumeDigest));
   }else if(value.kind==='stage-write'){
    const operation=value.id;try{
    const handle=stage(value.transferId);const offset=range(value.offset,0,handle.totalBytes);
    const payload=bytes(value.bytes,240*1024);await transfers.write(handle,offset,payload);check();
    const status=await transfers.status(handle);check();value=decode(runtime.peer_delivery_stage_written(peerId,handle.id,status.nextOffset));
    }finally{if(operation!==undefined)try{runtime.peer_delivery_forget(peerId,operation);}catch{}}
   }else if(value.kind==='stage-read'){
    const operation=value.id;try{
    const handle=stage(value.transferId),offset=range(value.offset,0,handle.totalBytes),length=range(value.length,1,240*1024);
    if(offset+length>handle.totalBytes)throw fail('Transfer read exceeds stream');
    const payload=await transfers.read(handle,offset,length);check();
    if(!(payload instanceof Uint8Array)||payload.byteLength!==length)throw fail('Transfer read length mismatch');
    value=decode(runtime.peer_delivery_stage_read(peerId,handle.id,offset,payload));
    }finally{if(operation!==undefined)try{runtime.peer_delivery_forget(peerId,operation);}catch{}}
   }else if(value.kind==='persist'){
    // Let the scheduler keep a poll outstanding (at the pending cut's cursor)
    // while this storage transaction commits; its page waits for the commit.
    // (Only once established: setup persists before delivery custody exists.)
    if(established)queueMicrotask(wake);
    await persist();check();value=decode(runtime.peer_delivery_resume(peerId));
   }else if(value.kind==='ready'||value.kind==='idle')return value;
   else throw fail('Unknown delivery transition');
  }
  throw fail('Delivery transition count exceeded');
 }
 // A refused data stream after a host-side policy/authority change only needs a
 // fresh HTTP session on the same Peer, key and storage. Renew it in place at
 // most once per window before escalating to full transport recovery
 // (re-enrollment, backoff and a retained replay).
 let lastFastRenewal=-Infinity,established=false;
 function report(error){
  if(closed||failed||renewing)return;
  const at=(clock.now??Date.now)();
  if(error?.sessionRefused&&established&&at-lastFastRenewal>=FAST_RENEWAL_WINDOW_MS){lastFastRenewal=at;void refreshAuthorization().catch(()=>{});return;}
  failed=true;try{runtime.peer_delivery_stop(peerId);}catch{}lifetime.abort(error);try{onError(error);}catch{}
 }
 function wake(){
  if(closed||failed||renewing||!started||pumping)return;pumping=true;
  try{while(active<MAX_ACTIVE){const initial=runtime.peer_delivery_next(peerId),value=decode(initial);if(value.kind==='idle')break;if(!['request','stage-list','stage-discard','stage-open','stage-renewed','stage-write','stage-read','persist'].includes(value.kind))throw fail('Invalid delivery scheduler output');active++;track(()=>drive(initial)).catch(report).finally(()=>{active--;if(!closed&&!failed)queueMicrotask(wake);});}}
  catch(error){report(error);}finally{pumping=false;}
 }
 function start(){
  if(started)return started;
  if(closed)return Promise.reject(fail('Delivery closed'));
  started=track(async()=>{const value=await drive(runtime.peer_delivery_begin(peerId));if(value.kind!=='ready')throw fail('Delivery initialization incomplete');check();established=true;wake();return true;});
  started.catch(report);return started;
 }
 function close(){
  if(closing)return closing;closed=true;lifetime.abort(fail('Delivery closed'));try{runtime.peer_delivery_stop(peerId);}catch{}
  // Do not release the parent's storage/key lease while a raw fetch or write is settling.
  closing=(async()=>{while(pending.size)await Promise.allSettled([...pending]);if(refreshing)await Promise.allSettled([refreshing]);})();return closing;
 }
 function detachSession(){
  if(detaching)return detaching;
  detaching=(async()=>{
   await close();if(typeof runtime.peer_delivery_detach!=='function')return;
   let value;
   try{
    value=decode(runtime.peer_delivery_detach(peerId));if(value.kind==='idle')return;
    const response=await send(value,{detachOnly:true});
    runtime.peer_delivery_advance(peerId,value.id,response);
   }catch{/* Detach has no state or participation effect; expiry/lost reply is best effort. */}
   finally{if(value?.id!==undefined)try{runtime.peer_delivery_forget(peerId,value.id);}catch{}}
  })();return detaching;
 }
 function refreshAuthorization(){
  if(closed)return Promise.reject(fail('Delivery closed'));
  if(refreshing)return refreshing;
  renewing=true;lifetime.abort(fail('Shared authorization refreshing'));try{runtime.peer_delivery_stop(peerId);}catch{}
  refreshing=(async()=>{
   while(pending.size)await Promise.allSettled([...pending]);
   if(closed)throw fail('Delivery closed');
   lifetime=new AbortController();failed=false;started=undefined;
   await start();check();return true;
  })().finally(()=>{renewing=false;refreshing=undefined;if(!closed)queueMicrotask(wake);});
  refreshing.catch(report);return refreshing;
 }
 function range(value,min,max){if(!Number.isSafeInteger(value)||value<min||value>max)throw fail('Transfer range exceeds bound');return value;}
 function stage(id){const handle=stages.get(id);if(!handle)throw fail('Unknown transfer handle');return handle;}
 async function commitBatch(writes,batch){
  if(typeof commit!=='function')throw fail('Storage commit unavailable');
  const handle=batch.transferId===undefined?undefined:stage(batch.transferId);
  await commit(writes,handle?{transfer:handle}:undefined);
  // A successful transaction consumes staging even when close began while it ran.
  // The parent's immutable Rust batch receives that same transaction result.
  if(handle)stages.delete(handle.id);
 }
 return Object.freeze({start,wake,close,detach:detachSession,commitBatch,refreshAuthorization});
}
