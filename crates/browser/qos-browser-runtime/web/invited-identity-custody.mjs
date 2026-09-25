/** Invited identities use the existing private identity store and exclusive device lease. */
// Most recently used invited identities of this origin, across invitations.
// Reload resumes from it without the invitation text (`{resume:true}`).
const RECENT='qos-certified-invited-v3:recent';
// Typed reasons (`error.code`) with product copy; see peer.d.ts.
const coded=(code,message)=>Object.assign(Error(message),{code});
export function createInvitedIdentityCustodian({runtime,locks,store}){
 async function hold(name){
  let resolve,reject,unlock;const acquired=new Promise((yes,no)=>{resolve=yes;reject=no;}),held=new Promise(yes=>unlock=yes);
  const task=locks.request(name,{ifAvailable:true,mode:'exclusive'},async lock=>{resolve(Boolean(lock));if(lock)await held;});task.catch(reject);
  if(!await acquired){await task;throw coded('open-in-other-tab','This invitation is already open in another tab or window.');}
  let done=false;return async()=>{if(done)return;done=true;unlock();await task;};
 }
 const reader=scope=>value=>{const identity=JSON.parse(runtime.invited_identity_public(value));if(identity.scope!==scope||typeof identity.device_public_key!=='string'||identity.device_public_key.length===0||identity.device_public_key.length>128||typeof identity.binding!=='string'||!/^[a-f0-9]{64}$/.test(identity.binding))throw Error('Invited identity custody mismatch');return identity;};
 // One lease shape for both entry points: exact device key, fixed binding.
 async function lease({scope,key,material,release,check,session}){
  const read=reader(scope),identity=read(material);if(identity.device_public_key!==key)throw Error('Invited identity locator mismatch');
  try{await store.remember?.(`qos-certified-invited-v3:${scope}`,key);}catch{/* adoption index is a hint only */}
  try{await store.remember?.(RECENT,key);}catch{/* resume index is a hint only */}
  try{session.setItem(RECENT,key);}catch{}
  const value={publicKey:key,scope,material,release,async save(next){check();if(released())throw Error('Invited identity custody released');const current=read(next);if(current.device_public_key!==key||current.binding!==identity.binding)throw Error('Invited identity custody changed');await store.save(key,next);check();value.material=next;}};
  let done=false;const released=()=>done;const inner=release;value.release=async()=>{done=true;await inner();};
  return value;
 }
 // Reload without the invitation text: the most recently used stored invited
 // identity of this origin (optionally only the given invited scope). The
 // stored material itself carries the signed invitation and admission.
 async function resume(invitation,session,signal){
  const check=()=>{signal?.throwIfAborted();};
  if(invitation.scope!==undefined&&(typeof invitation.scope!=='string'||invitation.scope.length===0||invitation.scope.length>256))throw TypeError('Invalid invited identity scope');
  const keys=[];let current=null;try{current=session.getItem(RECENT);}catch{}
  if(typeof current==='string'&&current.length>0&&current.length<=128)keys.push(current);
  if(typeof store.candidates==='function')for(const key of (await store.candidates(RECENT)).slice(0,32))if(!keys.includes(key))keys.push(key);
  let occupied=false;
  for(const key of keys){
   check();
   let material,identity;
   try{material=await store.load(key);if(!material)continue;identity=JSON.parse(runtime.invited_identity_public(material));}catch{continue;}
   if(identity?.device_public_key!==key||typeof identity.scope!=='string'||identity.scope.length===0||identity.scope.length>256)continue;
   if(invitation.scope!==undefined&&identity.scope!==invitation.scope)continue;
   const scope=identity.scope;let releaseScope,releaseDevice;
   try{releaseScope=await hold(`qos-invited-scope:${scope}`);}catch{occupied=true;continue;}
   try{releaseDevice=await hold(`qos-certified-device:${key}`);}catch{occupied=true;await releaseScope();continue;}
   let released=false;const release=async()=>{if(released)return;released=true;try{await releaseDevice();}finally{await releaseScope();}};
   try{
    check();material=await store.load(key);check();if(!material)throw coded('identity-lost','The invited identity stored in this browser is missing; open the invitation link again.');
    try{session.setItem(`qos-certified-invited-v3:${scope}`,key);}catch{}
    return await lease({scope,key,material,release,check,session});
   }catch(error){await release();throw error;}
  }
  if(occupied)throw coded('open-in-other-tab','This invitation is already open in another tab or window.');
  throw coded('invitation-required','No invitation is stored in this browser. Open the invitation link again.');
 }
 return async function acquire(invitation,session,signal){
  if(!locks)throw Error('Invited identity custody requires Web Locks');
  if(invitation?.resume===true)return resume(invitation,session,signal);
  if(invitation?.identity!=='new-person'||typeof invitation.text!=='string'||invitation.text.length===0||invitation.text.length>65536||typeof invitation.label!=='string'||invitation.label.length===0||invitation.label.length>128)throw TypeError('Invalid invited identity request');
  const check=()=>{signal?.throwIfAborted();};
  const scope=runtime.invited_identity_scope(invitation.text);
  if(typeof scope!=='string'||scope.length===0||scope.length>256)throw Error('Invalid invited identity scope');
  const locator=`qos-certified-invited-v3:${scope}`;
  check();const releaseScope=await hold(`qos-invited-scope:${scope}`);let releaseDevice,released=false;
  const release=async()=>{if(released)return;released=true;try{await releaseDevice?.();}finally{await releaseScope();}};
  try{
   check();let key=session.getItem(locator),material;
   const read=reader(scope);
   // Restarted browser: adopt this scope's stored, unlocked invited identity.
   if(!key&&typeof store.candidates==='function'){
    for(const candidate of (await store.candidates(locator)).slice(0,32)){
     let held;try{held=await hold(`qos-certified-device:${candidate}`);}catch{continue;}
     let adopted;try{adopted=await store.load(candidate);if(adopted&&read(adopted).device_public_key!==candidate)adopted=undefined;}catch{adopted=undefined;}
     if(!adopted){await held();continue;}
     key=candidate;releaseDevice=held;material=adopted;session.setItem(locator,key);break;
    }
   }
   if(key&&!releaseDevice){releaseDevice=await hold(`qos-certified-device:${key}`);check();material=await store.load(key);check();if(!material)throw coded('identity-lost','The invited identity stored in this browser is missing; explicit recovery is required.');}
   else if(key){check();}
   else{material=runtime.invited_identity_create(invitation.text,invitation.label);key=read(material).device_public_key;releaseDevice=await hold(`qos-certified-device:${key}`);check();await store.save(key,material);check();session.setItem(locator,key);}
   return await lease({scope,key,material,release,check,session});
  }catch(error){await release();throw error;}
 };
}
