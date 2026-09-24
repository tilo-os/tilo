/** Invited identities use the existing private identity store and exclusive device lease. */
export function createInvitedIdentityCustodian({runtime,locks,store}){
 return async function acquire(invitation,session,signal){
  if(!locks)throw Error('Invited identity custody requires Web Locks');
  if(invitation?.identity!=='new-person'||typeof invitation.text!=='string'||invitation.text.length===0||invitation.text.length>65536||typeof invitation.label!=='string'||invitation.label.length===0||invitation.label.length>128)throw TypeError('Invalid invited identity request');
  const check=()=>{signal?.throwIfAborted();};
  const scope=runtime.invited_identity_scope(invitation.text);
  if(typeof scope!=='string'||scope.length===0||scope.length>256)throw Error('Invalid invited identity scope');
  const locator=`qos-certified-invited-v3:${scope}`;
  async function hold(name){
   let resolve,reject,unlock;const acquired=new Promise((yes,no)=>{resolve=yes;reject=no;}),held=new Promise(yes=>unlock=yes);
   const task=locks.request(name,{ifAvailable:true,mode:'exclusive'},async lock=>{resolve(Boolean(lock));if(lock)await held;});task.catch(reject);
   if(!await acquired){await task;throw Error('Invited identity custody is occupied');}
   let done=false;return async()=>{if(done)return;done=true;unlock();await task;};
  }
  check();const releaseScope=await hold(`qos-invited-scope:${scope}`);let releaseDevice,released=false;
  const release=async()=>{if(released)return;released=true;try{await releaseDevice?.();}finally{await releaseScope();}};
  try{
   check();let key=session.getItem(locator),material;
   const read=value=>{const identity=JSON.parse(runtime.invited_identity_public(value));if(identity.scope!==scope||typeof identity.device_public_key!=='string'||identity.device_public_key.length===0||identity.device_public_key.length>128||typeof identity.binding!=='string'||!/^[a-f0-9]{64}$/.test(identity.binding))throw Error('Invited identity custody mismatch');return identity;};
   // Restarted browser: adopt this scope's stored, unlocked invited identity.
   if(!key&&typeof store.candidates==='function'){
    for(const candidate of (await store.candidates(locator)).slice(0,32)){
     let held;try{held=await hold(`qos-certified-device:${candidate}`);}catch{continue;}
     let adopted;try{adopted=await store.load(candidate);if(adopted&&read(adopted).device_public_key!==candidate)adopted=undefined;}catch{adopted=undefined;}
     if(!adopted){await held();continue;}
     key=candidate;releaseDevice=held;material=adopted;session.setItem(locator,key);break;
    }
   }
   if(key&&!releaseDevice){releaseDevice=await hold(`qos-certified-device:${key}`);check();material=await store.load(key);check();if(!material)throw Error('Stored invited identity is missing; explicit recovery required');}
   else if(key){check();}
   else{material=runtime.invited_identity_create(invitation.text,invitation.label);key=read(material).device_public_key;releaseDevice=await hold(`qos-certified-device:${key}`);check();await store.save(key,material);check();session.setItem(locator,key);}
   const identity=read(material);if(identity.device_public_key!==key)throw Error('Invited identity locator mismatch');
   try{await store.remember?.(locator,key);}catch{/* adoption index is a hint only */}
   const lease={publicKey:key,material,release,async save(next){check();if(released)throw Error('Invited identity custody released');const current=read(next);if(current.device_public_key!==key||current.binding!==identity.binding)throw Error('Invited identity custody changed');await store.save(key,next);check();lease.material=next;}};
   return lease;
  }catch(error){await release();throw error;}
 };
}
