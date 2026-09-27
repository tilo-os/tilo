/**
 * Cloud owner source: carries only opaque Rust attempt handles, persistence
 * checkpoints and opaque native enrollment JSON.
 *
 * `cloud.enrollment` is injected by the caller (portal API or a local stand-in):
 *   inspect({signal}) -> Promise<string>            Store challenge JSON
 *   apply(payload,{operationKey,signal}) -> Promise<string>  Store.apply reply JSON
 * Both strings stay opaque here: signed u64 values never become JS numbers.
 * `operationKey` is deterministic per durable attempt and suits an
 * Idempotency-Key. Membership refresh goes to the page origin from Rust.
 *
 * Adapter error contract (reject with an Error carrying `code`):
 *   code 'unavailable' | 'busy' (or `status` 429/502/503/504, or a network
 *     TypeError): transient; inspect/apply are retried in place with jittered
 *     backoff (apply with the same operationKey, so it stays idempotent).
 *   code 'apply-refused' (+ optional `reason`: 'lost-race' | 'expired' |
 *     'replaced' | 'clock-skew' | string): Store.apply refused this signed
 *     request. Custody returns to Inspect with the same keys and a fresh
 *     operation key; a fresh inspect/apply follows (bounded), except that a
 *     'clock-skew' reason surfaces as a `clock-skew` error after the reset.
 *   code 'claimed-elsewhere': the Store is bound to another owner root.
 *     Custody returns to Inspect and the error surfaces as-is.
 * Errors thrown to the caller carry a typed `code` (see CLOUD_ERROR_CODES)
 * and a human `message`; the original is kept as `cause`.
 */
const MAX_ENROLLMENT_JSON=20*1024;
const ADAPTER_RETRIES=5,MAX_APPLY_RESETS=2;
const TRANSIENT_STATUS=new Set([429,502,503,504]);
export const CLOUD_ERROR_CODES=Object.freeze(['clock-skew','claimed-elsewhere','busy','open-in-other-tab','identity-lost','apply-refused','unavailable','membership-refused','enrollment-refused']);
const MESSAGES={
  'clock-skew':"This device's clock differs too much from the Cloud machine. Check the date and time settings, then try again.",
  'claimed-elsewhere':'This Cloud machine is already set up with another owner identity.',
  'busy':'The Cloud machine is busy. Try again in a moment.',
  'unavailable':'The Cloud machine cannot be reached right now. Try again in a moment.',
  'apply-refused':'The Cloud machine did not accept this sign-in attempt. Try again.',
  'membership-refused':'This browser is no longer an owner of this Cloud workspace.',
  'enrollment-refused':'The Cloud machine refused this sign-in.',
};
export function cloudError(code,message=MESSAGES[code]??'Cloud owner sign-in failed.',cause){
  const error=Error(message,cause===undefined?undefined:{cause});error.code=code;
  if(cause!==undefined)error.detail=String(cause?.message??cause).slice(0,512);
  return error;
}
/** Adds a typed `code` to runtime/adapter failures; unknown errors pass unchanged. */
export function classifyCloudError(error){
  if(error&&typeof error==='object'&&CLOUD_ERROR_CODES.includes(error.code))return error;
  const text=String(error?.message??error);
  let code;
  if(TRANSIENT_STATUS.has(error?.status))code='busy';
  else if(/Cloud owner clock skew|NotYetValid|not yet valid|issued in future/i.test(text))code='clock-skew';
  else if(/Cloud owner claimed elsewhere/.test(text))code='claimed-elsewhere';
  else if(/\((429|502|503|504)\)$|Cloud owner attempt busy|Too many Cloud owner attempts/.test(text))code='busy';
  else if(/onboarding browser operation failed|onboarding fetch failed|Descriptor discovery HTTP 5\d\d|Failed to fetch|NetworkError/.test(text)||error?.name==='TypeError'&&/fetch|network/i.test(text))code='unavailable';
  else if(/Cloud owner membership rejected \(40[13]\)/.test(text))code='membership-refused';
  else if(/Cloud owner enrollment refused/.test(text))code='enrollment-refused';
  return code?cloudError(code,undefined,error):error;
}
const transientAdapter=error=>error?.code==='unavailable'||error?.code==='busy'||TRANSIENT_STATUS.has(error?.status)||error?.name==='TypeError';
const pause=(ms,signal)=>new Promise((resolve,reject)=>{
  if(signal?.aborted){reject(signal.reason);return;}
  const abort=()=>{clearTimeout(timer);reject(signal.reason);};
  const timer=setTimeout(()=>{signal?.removeEventListener('abort',abort);resolve();},ms);
  signal?.addEventListener('abort',abort,{once:true});
});
export function createCloudPeerSource({runtime,lease,cloud,fetchImpl=globalThis.fetch,guard=()=>{},delay=pause}){
  const {accountId,machineId,enrollment}=cloud??{};
  if(typeof accountId!=='string'||typeof machineId!=='string'||typeof enrollment?.inspect!=='function'||typeof enrollment?.apply!=='function')throw TypeError('Invalid Cloud owner source options');
  let closed=false,generation=0,active=null,closePromise;
  const cleanups=new Set();
  const operations=new Set();
  const track=operation=>{
    let resolve,reject;const pending=new Promise((yes,no)=>{resolve=yes;reject=no;});
    operations.add(pending);
    pending.then(()=>operations.delete(pending),()=>operations.delete(pending));
    try{Promise.resolve(operation()).then(resolve,reject);}catch(error){reject(error);}
    return pending;
  };
  const retainCleanup=operation=>{
    let pending;try{pending=Promise.resolve(operation());}catch(error){pending=Promise.reject(error);}
    cleanups.add(pending);
    pending.then(()=>cleanups.delete(pending),()=>{});
    pending.catch(()=>{});
    return pending;
  };
  const check=attempt=>{
    guard();
    attempt?.signal?.throwIfAborted();
    if(closed||active!==attempt||attempt.generation!==generation)throw Error('Cloud owner source closed or replaced');
  };
  const cancel=attempt=>{
    if(!attempt)return Promise.resolve();
    if(attempt.cancelled)return attempt.cleanup;
    attempt.cancelled=true;
    attempt.externalSignal?.removeEventListener('abort',attempt.abort);
    attempt.controller.abort(Error('Cloud owner source closed or replaced'));
    attempt.cleanup=retainCleanup(()=>runtime.cloud_cancel(attempt.id));
    if(active===attempt)active=null;
    return attempt.cleanup;
  };
  const decode=(raw,kind)=>{
    if(typeof raw!=='string'||raw.length>4_194_304)throw Error('Invalid Cloud owner checkpoint');
    const value=JSON.parse(raw);
    if(value?.kind!==kind||!Array.isArray(value.material)||value.material.length===0||
        value.material.length>1_048_576||value.material.some(byte=>!Number.isInteger(byte)||byte<0||byte>255)){
      throw Error('Invalid Cloud owner checkpoint');
    }
    return {...value,material:Uint8Array.from(value.material)};
  };
  const opaque=value=>{
    if(typeof value!=='string'||value.length===0||new TextEncoder().encode(value).length>MAX_ENROLLMENT_JSON)throw Error('Invalid native enrollment JSON');
    return value;
  };
  // Durable ACK precedes every custody advance: the exact saved bytes are
  // handed back to Rust, which refuses anything but its staged checkpoint.
  const commit=async(attempt,raw)=>{
    const checkpoint=decode(raw,'checkpoint');check(attempt);
    await lease.save(checkpoint.material);check(attempt);
    runtime.cloud_acknowledge(attempt.id,lease.material);check(attempt);
  };
  // Transient adapter failures retry in place; everything else surfaces.
  async function adapter(attempt,operation){
    for(let failures=0;;failures++){
      try{return await operation();}
      catch(error){
        check(attempt);
        if(!transientAdapter(error)||failures>=ADAPTER_RETRIES)throw error;
        await delay(Math.round(Math.min(2000,200*2**failures)*(0.5+Math.random()*0.5)),attempt.signal);check(attempt);
      }
    }
  }
  async function enroll(attempt,existing){
    // inspect -> sign -> ACK -> apply -> verify -> ACK. A reload resumes at the
    // durable step and never repeats a completed inspect/apply. A refused
    // apply returns to Inspect (same keys) through its own durable checkpoint.
    let resets=0;
    for(let steps=0;steps<3+2*MAX_APPLY_RESETS;steps++){
      const next=JSON.parse(runtime.cloud_step(attempt.id));check(attempt);
      if(next?.step==='enrolled')return;
      if(existing!=null)throw Error('Cloud owner refresh requires completed enrollment');
      if(next?.step==='inspect'){
        const challenge=opaque(await adapter(attempt,()=>enrollment.inspect({signal:attempt.signal})));check(attempt);
        await commit(attempt,runtime.cloud_enroll(attempt.id,challenge));
      }else if(next?.step==='apply'&&typeof next.payload==='string'&&typeof next.operationKey==='string'){
        let reply;
        try{reply=opaque(await adapter(attempt,()=>enrollment.apply(next.payload,{operationKey:next.operationKey,signal:attempt.signal})));}
        catch(error){
          check(attempt);
          const refused=error?.code==='apply-refused',claimed=error?.code==='claimed-elsewhere';
          if((!refused&&!claimed)||typeof runtime.cloud_reset_apply!=='function')throw error;
          if(resets>=MAX_APPLY_RESETS)throw cloudError('apply-refused',undefined,error);
          resets++;await commit(attempt,runtime.cloud_reset_apply(attempt.id));
          if(claimed)throw cloudError('claimed-elsewhere',undefined,error);
          if(error.reason==='clock-skew')throw cloudError('clock-skew',undefined,error);
          continue;
        }
        check(attempt);
        await commit(attempt,runtime.cloud_accept(attempt.id,reply));
      }else throw Error('Invalid Cloud owner enrollment step');
    }
    throw Error('Cloud owner enrollment did not complete');
  }
  return {
    prepare({existing,signal}={}){return track(async()=>{
      if(closed)throw Error('Cloud owner source closed');
      // A prepared attempt that never reached connect/reconnect (the caller
      // failed in between) is released here instead of leaking a Rust slot.
      if(active?.prepared&&!active.consumed)await cancel(active);
      if(active)throw Error('Cloud owner preparation is already pending');
      signal?.throwIfAborted();
      const controller=new AbortController();
      const attempt={id:runtime.cloud_begin(lease.material,accountId,machineId,existing),generation:++generation,cancelled:false,signal:controller.signal,externalSignal:signal,controller};
      active=attempt;
      attempt.abort=()=>cancel(attempt);
      signal?.addEventListener('abort',attempt.abort,{once:true});
      try{
        await enroll(attempt,existing);
        const admitted=decode(await runtime.cloud_complete(attempt.id,lease.material),'admitted');
        check(attempt);
        await lease.save(admitted.material);
        check(attempt);
        if(!admitted.namespace||typeof admitted.namespace.profile!=='string'||
            typeof admitted.namespace.device!=='string'||typeof admitted.namespace.room!=='string'){
          throw Error('Invalid Cloud owner persistence namespace');
        }
        if(admitted.descriptorUrl!=null){
          const url=new URL(admitted.descriptorUrl);
          if(url.protocol!=='https:'||url.username||url.password||url.pathname!=='/peer/v3/descriptor'||url.search||url.hash)throw Error('Invalid Cloud owner descriptor URL');
          check(attempt);
          const response=await fetchImpl(url.href,{method:'GET',cache:'no-store',redirect:'error',credentials:'omit',signal:attempt.signal});
          check(attempt);if(!response.ok)throw Error(`Descriptor discovery HTTP ${response.status}`);
          const reader=response.body?.getReader();if(!reader)throw Error('Descriptor response body missing');
          const bytes=new Uint8Array(24576);let length=0;
          try{for(;;){const part=await reader.read();check(attempt);if(part.done)break;if(length+part.value.byteLength>bytes.length)throw Error('Descriptor response exceeds bound');bytes.set(part.value,length);length+=part.value.byteLength;}}
          finally{try{await reader.cancel();}catch{}}
          check(attempt);runtime.cloud_descriptor(attempt.id,bytes.subarray(0,length));
        }
        attempt.prepared=true;
        return {attempt,namespace:admitted.namespace};
      }catch(error){await cancel(attempt);throw classifyCloudError(error);}
    });},
    /** Releases a prepared attempt the caller will not connect (idempotent). */
    discard(prepared){const attempt=prepared?.attempt;return attempt&&!attempt.consumed?cancel(attempt):Promise.resolve();},
    connect(prepared,hydrated,callback){return track(async()=>{
      const attempt=prepared?.attempt;
      try{
        check(attempt);if(attempt)attempt.consumed=true;
        const peer=await runtime.cloud_connect(attempt.id,lease.material,hydrated,callback);
        if(closed||active!==attempt||generation!==attempt.generation){
          await retainCleanup(()=>runtime.close_peer(peer));
          throw Error('Cloud owner source closed or replaced');
        }
        return peer;
      }catch(error){throw classifyCloudError(error);}
      finally{await cancel(attempt);}
    });},
    reconnect(prepared,peer){return track(async()=>{
      const attempt=prepared?.attempt;
      try{check(attempt);attempt.consumed=true;await runtime.cloud_reconnect(attempt.id,lease.material,peer);check(attempt);}
      catch(error){throw classifyCloudError(error);}
      finally{await cancel(attempt);}
    });},
    // Transport-only reconnect of a still valid admitted owner: no membership
    // refresh. Rejects (code-less) when Rust requires a full refresh; the
    // session then falls back to prepare + reconnect.
    ...(typeof runtime.cloud_resume_transport==='function'?{resume(peer){return track(async()=>{
      guard();if(closed)throw Error('Cloud owner source closed');
      await runtime.cloud_resume_transport(peer,lease.material,accountId,machineId);
      guard();if(closed)throw Error('Cloud owner source closed');
    });}}:{}),
    refresh(peer,signal){return track(async()=>{
      const prepared=await this.prepare({existing:peer,signal});
      try{check(prepared.attempt);runtime.cloud_refresh_media(prepared.attempt.id,lease.material,peer);}
      finally{await cancel(prepared.attempt);}
    });},
    close(){
      if(closePromise)return closePromise;
      closed=true;
      generation++;
      cancel(active);
      closePromise=(async()=>{
        await Promise.allSettled([...operations]);
        await Promise.all([...cleanups]);
      })();
      closePromise.catch(()=>{});
      return closePromise;
    },
  };
}
