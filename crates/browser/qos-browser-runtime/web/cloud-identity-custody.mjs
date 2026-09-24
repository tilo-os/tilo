/** Cloud owner identities (root + device keys) use the existing private identity store and exclusive device lease. */
const MAX_SELECTION=256;
export function createCloudIdentityCustodian({runtime,locks,store}){
 return async function acquire(cloud,session,signal){
  if(!locks)throw Error('Cloud owner identity custody requires Web Locks');
  const {accountId,machineId,label='Tilo browser'}=cloud??{};
  for(const value of [accountId,machineId])if(typeof value!=='string'||value.length===0||value.length>MAX_SELECTION)throw TypeError('Invalid Cloud owner account or machine');
  if(typeof label!=='string'||label.trim().length===0||label.length>128)throw TypeError('Invalid Cloud owner device label');
  const check=()=>{signal?.throwIfAborted();};
  const scope=runtime.cloud_identity_scope(accountId,machineId);
  if(typeof scope!=='string'||!/^[a-f0-9]{64}$/.test(scope))throw Error('Invalid Cloud owner identity scope');
  const locator=`qos-certified-cloud-v4:${scope}`;
  async function hold(name){
   let resolve,reject,unlock;const acquired=new Promise((yes,no)=>{resolve=yes;reject=no;}),held=new Promise(yes=>unlock=yes);
   const task=locks.request(name,{ifAvailable:true,mode:'exclusive'},async lock=>{resolve(Boolean(lock));if(lock)await held;});task.catch(reject);
   if(!await acquired){await task;throw Error('Cloud owner identity custody is occupied');}
   let done=false;return async()=>{if(done)return;done=true;unlock();await task;};
  }
  check();const releaseScope=await hold(`qos-cloud-scope:${scope}`);let releaseDevice,released=false;
  const release=async()=>{if(released)return;released=true;try{await releaseDevice?.();}finally{await releaseScope();}};
  try{
   check();let key=session.getItem(locator),material;
   const read=value=>{const identity=JSON.parse(runtime.cloud_identity_public(value,accountId,machineId));if(identity.scope!==scope||typeof identity.device_public_key!=='string'||!/^[a-f0-9]{64}$/.test(identity.device_public_key)||typeof identity.binding!=='string'||!/^[a-f0-9]{64}$/.test(identity.binding))throw Error('Cloud owner identity custody mismatch');return identity;};
   // Restarted browser: adopt this scope's stored, unlocked owner identity.
   // A second first-claim identity could never enroll a Store already bound
   // to the first one, so adoption precedes creation.
   if(!key&&typeof store.candidates==='function'){
    for(const candidate of (await store.candidates(locator)).slice(0,32)){
     let held;try{held=await hold(`qos-certified-device:${candidate}`);}catch{continue;}
     let adopted;try{adopted=await store.load(candidate);if(adopted&&read(adopted).device_public_key!==candidate)adopted=undefined;}catch{adopted=undefined;}
     if(!adopted){await held();continue;}
     key=candidate;releaseDevice=held;material=adopted;session.setItem(locator,key);break;
    }
   }
   if(key&&!releaseDevice){releaseDevice=await hold(`qos-certified-device:${key}`);check();material=await store.load(key);check();if(!material)throw Error('Stored Cloud owner identity is missing; explicit recovery required');}
   else if(key){check();}
   else{material=runtime.cloud_identity_create(accountId,machineId,label);key=read(material).device_public_key;releaseDevice=await hold(`qos-certified-device:${key}`);check();await store.save(key,material);check();session.setItem(locator,key);}
   const identity=read(material);if(identity.device_public_key!==key)throw Error('Cloud owner identity locator mismatch');
   try{await store.remember?.(locator,key);}catch{/* adoption index is a hint only */}
   const lease={publicKey:key,material,release,async save(next){check();if(released)throw Error('Cloud owner identity custody released');const current=read(next);if(current.device_public_key!==key||current.binding!==identity.binding)throw Error('Cloud owner identity custody changed');await store.save(key,next);check();lease.material=next;}};
   return lease;
  }catch(error){await release();throw error;}
 };
}
