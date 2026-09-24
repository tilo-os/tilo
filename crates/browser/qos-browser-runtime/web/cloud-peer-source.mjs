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
 */
const MAX_ENROLLMENT_JSON=20*1024;
export function createCloudPeerSource({runtime,lease,cloud,fetchImpl=globalThis.fetch,guard=()=>{}}){
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
  async function enroll(attempt,existing){
    // inspect -> sign -> ACK -> apply -> verify -> ACK. A reload resumes at the
    // durable step and never repeats a completed inspect/apply.
    for(let steps=0;steps<3;steps++){
      const next=JSON.parse(runtime.cloud_step(attempt.id));check(attempt);
      if(next?.step==='enrolled')return;
      if(existing!=null)throw Error('Cloud owner refresh requires completed enrollment');
      if(next?.step==='inspect'){
        const challenge=opaque(await enrollment.inspect({signal:attempt.signal}));check(attempt);
        await commit(attempt,runtime.cloud_enroll(attempt.id,challenge));
      }else if(next?.step==='apply'&&typeof next.payload==='string'&&typeof next.operationKey==='string'){
        const reply=opaque(await enrollment.apply(next.payload,{operationKey:next.operationKey,signal:attempt.signal}));check(attempt);
        await commit(attempt,runtime.cloud_accept(attempt.id,reply));
      }else throw Error('Invalid Cloud owner enrollment step');
    }
    throw Error('Cloud owner enrollment did not complete');
  }
  return {
    prepare({existing,signal}={}){return track(async()=>{
      if(closed)throw Error('Cloud owner source closed');
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
        return {attempt,namespace:admitted.namespace};
      }catch(error){await cancel(attempt);throw error;}
    });},
    connect(prepared,hydrated,callback){return track(async()=>{
      const attempt=prepared?.attempt;
      check(attempt);
      try{
        const peer=await runtime.cloud_connect(attempt.id,lease.material,hydrated,callback);
        if(closed||active!==attempt||generation!==attempt.generation){
          await retainCleanup(()=>runtime.close_peer(peer));
          throw Error('Cloud owner source closed or replaced');
        }
        return peer;
      }finally{await cancel(attempt);}
    });},
    reconnect(prepared,peer){return track(async()=>{
      const attempt=prepared?.attempt;check(attempt);
      try{await runtime.cloud_reconnect(attempt.id,lease.material,peer);check(attempt);}
      finally{await cancel(attempt);}
    });},
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
