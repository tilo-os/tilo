/** A session locator is a hint. Only an acquired per-device lock owns its secret. */
export function createIdentityCustodian({runtime,locks,store}){
 return async function acquire(base,session,signal){
  if(!locks)throw Error('Certified actor custody requires Web Locks');
  const checkAbort=()=>{if(signal?.aborted)throw signal.reason??new DOMException('Aborted','AbortError');};
  const locator=`qos-certified-identity-v2:${base}`;
  async function hold(key){
   let resolve,reject,unlock;const acquired=new Promise((yes,no)=>{resolve=yes;reject=no;});
   const held=new Promise(yes=>{unlock=yes;});
   const task=locks.request(`qos-certified-device:${key}`,{ifAvailable:true,mode:'exclusive'},async lock=>{resolve(Boolean(lock));if(lock)await held;});task.catch(reject);
   if(!await acquired){await task;return null;}
   let released=false;return async()=>{if(released)return;released=true;unlock();await task;};
  }
  checkAbort();let key=session.getItem(locator),release,material;
  if(key){release=await hold(key);if(release){try{checkAbort();material=await store.load(key);if(!material)throw Error('Stored certified identity is missing; preserved journal requires explicit recovery');}catch(error){await release();throw error;}}}
  // A restarted browser has no session hint. Adopt the most recently used
  // stored identity for this base whose exclusive device lock is free, so the
  // same device (journal, storage, retained commands) continues after restart.
  if(!key&&!release&&typeof store.candidates==='function'){
   for(const candidate of (await store.candidates(locator)).slice(0,32)){
    checkAbort();const held=await hold(candidate);if(!held)continue;
    let adopted;try{adopted=await store.load(candidate);if(adopted&&JSON.parse(runtime.identity_request(adopted)).device_public_key!==candidate)adopted=undefined;}catch{adopted=undefined;}
    if(!adopted){await held();continue;}
    key=candidate;release=held;material=adopted;session.setItem(locator,key);break;
   }
  }
  if(!release){
   material=runtime.create_identity();key=JSON.parse(runtime.identity_request(material)).device_public_key;
   release=await hold(key);if(!release)throw Error('New certified identity custody collision');
   try{checkAbort();await store.save(key,material);session.setItem(locator,key);}catch(error){await release();throw error;}
  }
  try{checkAbort();if(JSON.parse(runtime.identity_request(material)).device_public_key!==key)throw Error('Stored identity locator mismatch');}catch(error){await release();throw error;}
  try{await store.remember?.(locator,key);}catch{/* the adoption index is a hint only */}
  const lease={publicKey:key,material,release,async save(next){if(JSON.parse(runtime.identity_request(next)).device_public_key!==key)throw Error('Identity mutation changes certified key');await store.save(key,next);lease.material=next;}};
  return lease;
 };
}
