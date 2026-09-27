/** Cloud owner identities (root + device keys) use the existing private identity store and exclusive device lease. */
const MAX_SELECTION=256;
// Typed reasons (`error.code`) with product copy; see peer.d.ts CloudErrorCode.
const coded=(code,message)=>Object.assign(Error(message),{code});
/** Holds one exclusive Web Lock without waiting; `busy()` builds the refusal when it is taken. Resolves to an idempotent release. */
async function holdExclusive(locks,name,busy){
 let resolve,reject,unlock;const acquired=new Promise((yes,no)=>{resolve=yes;reject=no;}),held=new Promise(yes=>unlock=yes);
 const task=locks.request(name,{ifAvailable:true,mode:'exclusive'},async lock=>{resolve(Boolean(lock));if(lock)await held;});task.catch(reject);
 if(!await acquired){await task;throw busy();}
 let done=false;return async()=>{if(done)return;done=true;unlock();await task;};
}
export function createCloudIdentityCustodian({runtime,locks,store}){
 return async function acquire(cloud,session,signal){
  if(!locks)throw Error('Cloud owner identity custody requires Web Locks');
  const {accountId,machineId,label='Tilo browser'}=cloud??{};
  for(const value of [accountId,machineId])if(typeof value!=='string'||value.length===0||value.length>MAX_SELECTION)throw TypeError('Invalid Cloud owner account or machine');
  if(typeof label!=='string'||label.trim().length===0||label.length>128)throw TypeError('Invalid Cloud owner device label');
  const check=()=>{signal?.throwIfAborted();};
  const scope=runtime.cloud_identity_scope(accountId,machineId);
  if(typeof scope!=='string'||!/^[a-f0-9]{64}$/.test(scope))throw Error('Invalid Cloud owner identity scope');
  const locator=`tilo-certified-cloud-v4:${scope}`;
  const hold=name=>holdExclusive(locks,name,()=>coded('open-in-other-tab','Tilo is already open for this Cloud machine in another tab or window.'));
  check();const releaseScope=await hold(`tilo-cloud-scope:${scope}`);let releaseDevice,released=false;
  const release=async()=>{if(released)return;released=true;try{await releaseDevice?.();}finally{await releaseScope();}};
  try{
   check();let key=session.getItem(locator),material;
   const read=value=>{const identity=JSON.parse(runtime.cloud_identity_public(value,accountId,machineId));if(identity.scope!==scope||typeof identity.device_public_key!=='string'||!/^[a-f0-9]{64}$/.test(identity.device_public_key)||typeof identity.binding!=='string'||!/^[a-f0-9]{64}$/.test(identity.binding))throw Error('Cloud owner identity custody mismatch');return identity;};
   // Restarted browser: adopt this scope's stored, unlocked owner identity.
   // A second first-claim identity could never enroll a Store already bound
   // to the first one, so adoption precedes creation.
   if(!key&&typeof store.candidates==='function'){
    for(const candidate of (await store.candidates(locator)).slice(0,32)){
     let held;try{held=await hold(`tilo-certified-device:${candidate}`);}catch{continue;}
     let adopted;try{adopted=await store.load(candidate);if(adopted&&read(adopted).device_public_key!==candidate)adopted=undefined;}catch{adopted=undefined;}
     if(!adopted){await held();continue;}
     key=candidate;releaseDevice=held;material=adopted;session.setItem(locator,key);break;
    }
   }
   if(key&&!releaseDevice){releaseDevice=await hold(`tilo-certified-device:${key}`);check();material=await store.load(key);check();if(!material)throw coded('identity-lost','The owner identity stored in this browser for this Cloud machine is missing; explicit recovery is required.');}
   else if(key){check();}
   else{material=runtime.cloud_identity_create(accountId,machineId,label);key=read(material).device_public_key;releaseDevice=await hold(`tilo-certified-device:${key}`);check();await store.save(key,material);check();session.setItem(locator,key);}
   const identity=read(material);if(identity.device_public_key!==key)throw Error('Cloud owner identity locator mismatch');
   try{await store.remember?.(locator,key);}catch{/* adoption index is a hint only */}
   const lease={publicKey:key,material,release,async save(next){check();if(released)throw Error('Cloud owner identity custody released');const current=read(next);if(current.device_public_key!==key||current.binding!==identity.binding)throw Error('Cloud owner identity custody changed');await store.save(key,next);check();lease.material=next;}};
   return lease;
  }catch(error){await release();throw error;}
 };
}
/**
 * Bilateral machine-mesh approvals from this browser's established Cloud owner
 * identities. Both machine scopes and their device identities are locked
 * without waiting, in sorted order (deadlock free), for the whole signing.
 * Linking never creates, adopts into the session, or saves an identity: it
 * only reads custody material and hands it to the runtime's native signer,
 * wipes its copies, and returns the public signed request JSON.
 */
export const CLOUD_MACHINE_LINK_ERROR_CODES=Object.freeze(['identity-missing','workspace-open','link-refused']);
const MAX_MESH_JSON=20*1024,MAX_CANDIDATES=32;
const LINK_MESSAGES={
 'identity-missing':"This browser has no Tilo owner identity for one of these machines. Open each machine's workspace in this browser first, then link again.",
 'workspace-open':"One of these machines has its workspace open in this browser. Close that workspace's tab or window, then link again.",
 'link-refused':'Tilo could not approve this machine link. Check both machines and try again.',
};
const linkError=(code,machineId,cause)=>Object.assign(Error(LINK_MESSAGES[code],cause===undefined?undefined:{cause}),{code,...(machineId?{machineId}:{})});
const wipe=value=>{if(ArrayBuffer.isView(value))new Uint8Array(value.buffer,value.byteOffset,value.byteLength).fill(0);};
export function createCloudMachineLinker({runtime,locks,store,ready=async()=>{},session=globalThis.sessionStorage}){
 return async function signCloudMachineMesh(request){
  const {accountId,kind,sourceMachineId,targetMachineId,linkId,sourceChallenge,targetChallenge,grant,signal}=request??{};
  const check=()=>signal?.throwIfAborted();
  for(const value of [accountId,sourceMachineId,targetMachineId])if(typeof value!=='string'||value.length===0||value.length>MAX_SELECTION)throw TypeError('Invalid Cloud account or machine');
  if(sourceMachineId===targetMachineId)throw TypeError('Select two different machines');
  const opaque=value=>typeof value==='string'&&value.length>0&&new TextEncoder().encode(value).length<=MAX_MESH_JSON;
  let source=sourceMachineId,target=targetMachineId;
  if(kind==='link'){
   if(typeof linkId!=='string'||!/^[A-Za-z0-9_-]{1,128}$/.test(linkId))throw TypeError('Invalid machine link id');
   if(!opaque(sourceChallenge)||!opaque(targetChallenge))throw TypeError('Invalid native mesh challenge');
  }else if(kind==='unlink'){
   if(!opaque(grant))throw TypeError('Invalid native mesh link grant');
   // Only machine IDs are read here; signed integers stay opaque to JS.
   let parsed;try{parsed=JSON.parse(grant);}catch{throw TypeError('Invalid native mesh link grant');}
   const first=parsed?.source?.machine_id,second=parsed?.target?.machine_id;
   if(first===sourceMachineId&&second===targetMachineId){}else if(first===targetMachineId&&second===sourceMachineId){source=targetMachineId;target=sourceMachineId;}
   else throw TypeError('Native link grant belongs to different machines');
  }else throw TypeError('Invalid machine mesh kind');
  if(!locks)throw Error('Machine linking requires Web Locks for identity custody');
  check();await ready();check();
  const sides=[source,target].map(machineId=>{
   const scope=runtime.cloud_identity_scope(accountId,machineId);
   if(typeof scope!=='string'||!/^[a-f0-9]{64}$/.test(scope))throw Error('Invalid Cloud owner identity scope');
   return {machineId,scope,locator:`tilo-certified-cloud-v4:${scope}`};
  });
  const releases=[],materials=[];
  const hold=async(name,machineId)=>{releases.push(await holdExclusive(locks,name,()=>linkError('workspace-open',machineId)));check();};
  try{
   // A held scope lock means a workspace (connectPeer custody) owns it now.
   for(const side of [...sides].sort((a,b)=>a.scope<b.scope?-1:1))await hold(`tilo-cloud-scope:${side.scope}`,side.machineId);
   for(const side of sides){
    let stored=null;try{stored=session?.getItem(side.locator)??null;}catch{/* no session storage */}
    const keys=[...new Set([stored,...(typeof store.candidates==='function'?await store.candidates(side.locator):[])])].filter(key=>typeof key==='string'&&/^[a-f0-9]{64}$/.test(key)).slice(0,MAX_CANDIDATES);check();
    side.keys=keys.sort();
   }
   // Device locks after every scope lock, again in one sorted order.
   const devices=sides.flatMap(side=>side.keys.map(key=>({side,key}))).sort((a,b)=>a.key<b.key?-1:a.key>b.key?1:0);
   for(const device of devices){
    try{await hold(`tilo-certified-device:${device.key}`,device.side.machineId);}catch(error){if(error?.code!=='workspace-open')throw error;device.busy=true;continue;}
    const {side,key}=device;if(side.material)continue;
    let material;try{material=await store.load(key);}catch{material=undefined;}check();
    if(!material)continue;
    materials.push(material);
    let identity;try{identity=JSON.parse(runtime.cloud_identity_public(material,accountId,side.machineId));}catch{continue;}
    // Only an enrolled identity of exactly this scope and device may approve.
    if(identity?.scope===side.scope&&identity.device_public_key===key&&identity.step==='enrolled')side.material=material;
   }
   for(const side of sides)if(!side.material)throw linkError(devices.some(device=>device.side===side&&device.busy)?'workspace-open':'identity-missing',side.machineId);
   check();
   const [a,b]=sides;let signed;
   try{
    signed=kind==='link'
     ?runtime.cloud_mesh_link(accountId,a.machineId,a.material,sourceChallenge,b.machineId,b.material,targetChallenge,linkId)
     :runtime.cloud_mesh_unlink(accountId,a.machineId,a.material,b.machineId,b.material,grant);
   }catch(error){throw /not enrolled/.test(String(error?.message??error))?linkError('identity-missing',undefined,error):linkError('link-refused',undefined,error);}
   check();
   if(!opaque(signed))throw linkError('link-refused');
   return signed;
  }finally{
   for(const material of materials)wipe(material);
   for(const release of releases.reverse())await release();
  }
 };
}
