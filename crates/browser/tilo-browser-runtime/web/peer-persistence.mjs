/** Preserve JS-owned byte arrays; legacy runtimes expose JSON number arrays. */
export function readPeerBatch(runtime,id){
 return runtime.peer_pending_batch_bytes?runtime.peer_pending_batch_bytes(id):JSON.parse(runtime.peer_pending_batch(id));
}
/** Local transaction failure rejects; publication failure retains its committed proof. */
export async function commitPeerWrites({pending,commit,acknowledge,publish}){
  let writes=pending();
  while(writes.length){await commit(writes);for(const write of writes)acknowledge(write);writes=pending();}
  try{await publish();return{localDurable:true,publicationError:null};}
  catch(publicationError){return{localDurable:true,publicationError};}
}
/** A capture refused because another batch for this peer is still settling. */
export function storageBusy(error){return /^storage-pressure:/.test(String(error?.message??error));}
/** Immutable Rust-owned batch ties admission, outbox and record heads together.
 * Capture backpressure is not a transaction failure: nothing was captured, so
 * the caller retries after the in-flight batch settles (`busy: true`). */
export async function commitPeerBatches({pending,commit,acknowledge,epoch,now=Date.now,publish}){
 const next=()=>{try{return pending();}catch(error){if(storageBusy(error))return null;throw error;}};
 let batch=next();
 while(batch?.writes.length){
  try{await commit(batch.writes,batch);}
  catch(error){acknowledge({batchId:batch.id,epoch,observedAtMs:now(),result:{ok:false,code:error?.name==='QuotaExceededError'?'storage-quota':error?.name==='SecurityError'?'storage-denied':'storage-transaction-failed'}});throw error;}
  acknowledge({batchId:batch.id,epoch,observedAtMs:now(),result:{ok:true}});batch=next();
 }
 if(batch===null)return{localDurable:false,busy:true,publicationError:null};
 try{await publish();return{localDurable:true,publicationError:null};}catch(publicationError){return{localDurable:true,publicationError};}
}
