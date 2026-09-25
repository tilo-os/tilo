/** Carries only opaque Rust attempt handles and persistence checkpoints. */
export function createInvitedPeerSource({runtime, lease, fetchImpl=globalThis.fetch,guard=()=>{}}) {
  let closed = false, generation = 0, active = null, closePromise;
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
  const check = (attempt) => {
    guard();
    attempt?.signal?.throwIfAborted();
    if (closed || active !== attempt || attempt.generation !== generation) {
      throw Error('Invited peer source closed or replaced');
    }
  };
  const cancel = (attempt) => {
    if (!attempt) return Promise.resolve();
    if (attempt.cancelled) return attempt.cleanup;
    attempt.cancelled = true;
    attempt.externalSignal?.removeEventListener('abort', attempt.abort);
    attempt.controller.abort(Error('Invited peer source closed or replaced'));
    attempt.cleanup=retainCleanup(()=>runtime.invited_cancel(attempt.id));
    if (active === attempt) active = null;
    return attempt.cleanup;
  };
  const decode = (raw, kind) => {
    if (typeof raw !== 'string' || raw.length > 4_194_304) throw Error('Invalid invitation checkpoint');
    const value = JSON.parse(raw);
    if (value?.kind !== kind || !Array.isArray(value.material) || value.material.length === 0 ||
        value.material.length > 1_048_576 || value.material.some(byte => !Number.isInteger(byte) || byte < 0 || byte > 255)) {
      throw Error('Invalid invitation checkpoint');
    }
    return {...value, material: Uint8Array.from(value.material)};
  };
  return {
    prepare({existing,signal}={}) {return track(async()=>{
      if (closed) throw Error('Invited peer source closed');
      // A prepared attempt the caller never connected is released, not leaked.
      if (active?.prepared && !active.consumed) await cancel(active);
      if (active) throw Error('Invitation preparation is already pending');
      signal?.throwIfAborted();
      const controller=new AbortController();
      const attempt = {id: runtime.invited_begin(lease.material,existing), generation: ++generation, cancelled: false,signal:controller.signal,externalSignal:signal,controller};
      active = attempt;
      attempt.abort=()=>cancel(attempt);
      signal?.addEventListener('abort',attempt.abort,{once:true});
      try {
        const checkpoint = decode(await runtime.invited_prepare(attempt.id), 'checkpoint');
        check(attempt);
        await lease.save(checkpoint.material);
        check(attempt);
        const admitted = decode(await runtime.invited_complete(attempt.id, lease.material), 'admitted');
        check(attempt);
        await lease.save(admitted.material);
        check(attempt);
        if (!admitted.namespace || typeof admitted.namespace.profile !== 'string' ||
            typeof admitted.namespace.device !== 'string' || typeof admitted.namespace.room !== 'string') {
          throw Error('Invalid invited persistence namespace');
        }
        if(admitted.descriptorUrl!=null){
          const url=new URL(admitted.descriptorUrl);
          if(url.protocol!=='https:'||url.username||url.password||url.pathname!=='/peer/v3/descriptor'||url.search||url.hash)throw Error('Invalid invited descriptor URL');
          check(attempt);
          const response=await fetchImpl(url.href,{method:'GET',cache:'no-store',redirect:'error',credentials:'omit',signal:attempt.signal});
          check(attempt);if(!response.ok)throw Error(`Descriptor discovery HTTP ${response.status}`);
          const reader=response.body?.getReader();if(!reader)throw Error('Descriptor response body missing');
          const bytes=new Uint8Array(24576);let length=0;
          try{for(;;){const part=await reader.read();check(attempt);if(part.done)break;if(length+part.value.byteLength>bytes.length)throw Error('Descriptor response exceeds bound');bytes.set(part.value,length);length+=part.value.byteLength;}}
          finally{try{await reader.cancel();}catch{}}
          check(attempt);runtime.invited_descriptor(attempt.id,bytes.subarray(0,length));
        }
        attempt.prepared = true;
        return {attempt, namespace: admitted.namespace};
      } catch (error) { await cancel(attempt); throw error; }
    });},
    /** Releases a prepared attempt the caller will not connect (idempotent). */
    discard(prepared) { const attempt = prepared?.attempt; return attempt && !attempt.consumed ? cancel(attempt) : Promise.resolve(); },
    connect(prepared, hydrated, callback) {return track(async()=>{
      const attempt = prepared?.attempt;
      try {
        check(attempt); attempt.consumed = true;
        const peer = await runtime.invited_connect(attempt.id, lease.material, hydrated, callback);
        if (closed || active !== attempt || generation !== attempt.generation) {
          await retainCleanup(()=>runtime.close_peer(peer));
          throw Error('Invited peer source closed or replaced');
        }
        return peer;
      } finally { await cancel(attempt); }
    });},
    reconnect(prepared,peer) {return track(async()=>{
      const attempt=prepared?.attempt;
      try{check(attempt);attempt.consumed=true;await runtime.invited_reconnect(attempt.id,lease.material,peer);check(attempt);}
      finally{await cancel(attempt);}
    });},
    // Transport-only reconnect of a still admitted guest: no membership
    // refresh. Rejects when Rust requires a refresh; the session then falls
    // back to prepare + reconnect.
    ...(typeof runtime.invited_resume_transport === 'function' ? {resume(peer) {return track(async()=>{
      guard(); if (closed) throw Error('Invited peer source closed');
      await runtime.invited_resume_transport(peer, lease.material);
      guard(); if (closed) throw Error('Invited peer source closed');
    });}} : {}),
    refresh(peer,signal) {return track(async()=>{
      const prepared=await this.prepare({existing:peer,signal});
      try{check(prepared.attempt);runtime.invited_refresh_media(prepared.attempt.id,lease.material,peer);}
      finally{await cancel(prepared.attempt);}
    });},
    close() {
      if (closePromise) return closePromise;
      closed = true;
      generation++;
      cancel(active);
      closePromise=(async()=>{
        // Cancellation may precede Endpoint.bind returning a resource. The raw
        // operation closes that late endpoint before settling; join it too.
        await Promise.allSettled([...operations]);
        await Promise.all([...cleanups]);
      })();
      closePromise.catch(()=>{});
      return closePromise;
    },
  };
}
