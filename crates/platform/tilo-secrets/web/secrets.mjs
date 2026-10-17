// The browser backend of tilo-secrets (src/web.rs). Each app has one AES-GCM
// key the browser generated as non-extractable; the key and every ciphertext
// live in IndexedDB. The additional data binds a ciphertext to its app and
// key, so a record copied to another name does not decrypt. Writers of one
// app take a Web Lock, so two tabs never mint two keys for it.
//
// This encrypts stored records, but does not defend against script in the
// page's origin or an attacker controlling the browser/profile holding its
// keys. The last parameter of each export is the
// environment, replaced by tests.

const DB='tilo-secrets',KEYS='keys',ITEMS='items';

/** A promise for an IDBRequest. */
const done=request=>new Promise((resolve,reject)=>{request.onsuccess=()=>resolve(request.result);request.onerror=()=>reject(request.error);});

/** Key-value storage over IndexedDB: `get`, `put` and `delete` per store. */
export function indexedDbStorage(idb=globalThis.indexedDB){
  let opened;
  const db=()=>opened??=new Promise((resolve,reject)=>{
    if(!idb){reject(Error('IndexedDB is unavailable in this browser'));return;}
    const request=idb.open(DB,1);
    request.onupgradeneeded=()=>{for(const name of [KEYS,ITEMS])if(!request.result.objectStoreNames.contains(name))request.result.createObjectStore(name);};
    request.onsuccess=()=>resolve(request.result);request.onerror=()=>reject(request.error);
  });
  // A successful request is not a committed write: quota/IO errors can still
  // abort the transaction afterwards. Never acknowledge custody before commit.
  const run=async(store,mode,op)=>{
    const tx=(await db()).transaction(store,mode,{durability:'strict'});
    const committed=new Promise((resolve,reject)=>{
      tx.oncomplete=resolve;
      tx.onabort=()=>reject(tx.error??Error('secret storage transaction aborted'));
      tx.onerror=()=>reject(tx.error??Error('secret storage transaction failed'));
    });
    const [value]=await Promise.all([done(op(tx.objectStore(store))),committed]);
    return value;
  };
  return {
    get:(store,key)=>run(store,'readonly',s=>s.get(key)),
    put:(store,key,value)=>run(store,'readwrite',s=>s.put(value,key)),
    delete:(store,key)=>run(store,'readwrite',s=>s.delete(key)),
    entries:async store=>{
      const tx=(await db()).transaction(store,'readonly'),source=tx.objectStore(store);
      const [keys,values]=await Promise.all([done(source.getAllKeys()),done(source.getAll())]);
      return keys.map((key,i)=>[key,values[i]]);
    },
  };
}

function environment(env){
  return {
    storage:env.storage??indexedDbStorage(env.indexedDB),
    subtle:'subtle' in env?env.subtle:globalThis.crypto?.subtle,
    random:env.random??(bytes=>globalThis.crypto.getRandomValues(bytes)),
    locks:'locks' in env?env.locks:globalThis.navigator?.locks,
  };
}

const exclusive=(locks,app,fn)=>{
  if(!locks?.request)throw Error('Web Locks are required for durable secret storage');
  return locks.request(`tilo-secrets:${app}`,fn);
};
const bound=(app,key)=>new TextEncoder().encode(`${app}\u0000${key}`);

async function appKey({storage,subtle},app,create){
  const existing=await storage.get(KEYS,app);
  if(existing||!create)return existing??null;
  // Losing custody is not an empty store. A replacement key would strand all
  // existing records, including ones this write does not touch.
  if(!storage.entries)throw Error('secret storage cannot verify existing custody');
  if((await storage.entries(ITEMS)).some(([[owner]])=>owner===app)){
    throw Error('the key for this app\'s secrets is gone; recovery is required');
  }
  const key=await subtle.generateKey({name:'AES-GCM',length:256},false,['encrypt','decrypt']);
  await storage.put(KEYS,app,key);
  return key;
}

/** The secret `key` of `app` as bytes, or `null`. */
export async function getSecret(app,key,env={}){
  const e=environment(env);
  if(!e.subtle)throw Error('WebCrypto is unavailable in this browser');
  const record=await e.storage.get(ITEMS,[app,key]);
  if(!record)return null;
  const cryptoKey=await appKey(e,app,false);
  if(!cryptoKey)throw Error('the key for this app\'s secrets is gone');
  const plain=await e.subtle.decrypt({name:'AES-GCM',iv:record.iv,additionalData:bound(app,key)},cryptoKey,record.data);
  return new Uint8Array(plain);
}

/** Store `bytes` as the secret `key` of `app`, replacing any earlier one. */
export async function putSecret(app,key,bytes,env={}){
  const e=environment(env);
  if(!e.subtle)throw Error('WebCrypto is unavailable in this browser');
  await exclusive(e.locks,app,async()=>{
    const cryptoKey=await appKey(e,app,true);
    const iv=e.random(new Uint8Array(12));
    const data=await e.subtle.encrypt({name:'AES-GCM',iv,additionalData:bound(app,key)},cryptoKey,bytes);
    await e.storage.put(ITEMS,[app,key],{iv,data:new Uint8Array(data)});
  });
}

/** Read or durably create a key under the app lock. Never replace custody.
 * `bytes` null forbids creation when ciphertext already exists.
 */
export async function getOrCreateSecret(app,key,bytes,env={}){
  const e=environment(env);
  if(!e.subtle)throw Error('WebCrypto is unavailable in this browser');
  return exclusive(e.locks,app,async()=>{
    const existing=await getSecret(app,key,e);
    if(existing)return existing;
    if(bytes===null)throw Error('existing vault key is missing; recovery is required');
    const cryptoKey=await appKey(e,app,true);
    const iv=e.random(new Uint8Array(12));
    const data=await e.subtle.encrypt({name:'AES-GCM',iv,additionalData:bound(app,key)},cryptoKey,bytes);
    await e.storage.put(ITEMS,[app,key],{iv,data:new Uint8Array(data)});
    return bytes.slice();
  });
}

/** Delete the secret `key` of `app`; a missing one is fine. */
export async function deleteSecret(app,key,env={}){
  const e=environment(env);
  await exclusive(e.locks,app,()=>e.storage.delete(ITEMS,[app,key]));
}

/**
 * Every stored secret this origin can decrypt, as `[app, key, bytes]`: the
 * page's synchronous mirror (`src/web.rs`) loads them before apps mount.
 * Missing keys and damaged records reject the entire load. Treating them as
 * absent would let a synchronous caller silently overwrite an existing vault.
 */
export async function loadSecrets(env={}){
  const e=environment(env);
  if(!e.subtle)throw Error('WebCrypto is unavailable in this browser');
  if(!e.storage.entries)throw Error('secret storage cannot enumerate records');
  const out=[],keys=new Map();
  for(const [[app,key],record] of await e.storage.entries(ITEMS)){
    if(!keys.has(app))keys.set(app,await appKey(e,app,false));
    const cryptoKey=keys.get(app);
    if(!cryptoKey)throw Error('the key for this app\'s secrets is gone; recovery is required');
    const plain=await e.subtle.decrypt({name:'AES-GCM',iv:record.iv,additionalData:bound(app,key)},cryptoKey,record.data);
    out.push([app,key,new Uint8Array(plain)]);
  }
  return out;
}

let tail=Promise.resolve(),failure=null;

/** Store or delete (`bytes` null) in order behind earlier queued writes. */
export function queueSecret(app,key,bytes,env={}){
  const owned=bytes?.slice();
  tail=tail.then(async()=>{
    try{
      if(failure)throw failure;
      await (owned?putSecret(app,key,owned,env):deleteSecret(app,key,env));
    }finally{owned?.fill(0);}
  }).catch(error=>{failure??=error;});
}

/** Resolves after commit; a failed queue stays failed until the page reloads. */
export async function flushSecrets(){
  await tail;
  if(failure)throw failure;
  await flushAcknowledgements();
}

// A synchronous caller must not publish a reference until a durable operation
// acknowledges completion. The first call starts work and returns null; callers
// retry on a later frame. A terminal result (including failure) is consumed
// exactly once. Retire its argument key and result then: the same arguments
// on a subsequent call describe new work, not a replay of an old commit.
const operations=new Map();
function acknowledged(id,work){
  let operation=operations.get(id);
  if(!operation){
    operation={state:'pending'};
    operations.set(id,operation);
    // Invoke synchronously so work can own borrowed WASM bytes before return.
    operation.completion=work().then(value=>{
      operation.state='complete';operation.value=value;
    },error=>{operation.state='failed';operation.error=error;});
  }
  if(operation.state==='pending')return null;
  operations.delete(id);
  const {value,error}=operation;
  delete operation.value;
  if(operation.state==='failed')throw error;
  return value;
}
export function pendingAcknowledgements(){
  return operations.size!==0;
}

/** Start an immutable credential write; consume its id once, only after commit. */
export function acknowledgedSecret(app,bytes,env={}){
  return acknowledgedSeal(app,`request-auth/${globalThis.crypto.randomUUID()}`,bytes,env);
}

/** Retry an immutable write by namespace and bytes; the first proposed key wins. */
export function acknowledgedSeal(app,key,bytes,env={}){
  const namespace=key.slice(0,key.lastIndexOf('/')+1);
  const operation=JSON.stringify(['credential',app,namespace,...bytes]);
  return acknowledged(operation,async()=>{
    // Only the initiating call makes a copy; retries must not leave extra
    // plaintext buffers behind. The owned copy is wiped on success or failure.
    const owned=bytes.slice();
    try{
      await putSecret(app,key,owned,env);
      return key;
    }finally{owned.fill(0);}
  });
}

const canonical=value=>JSON.stringify(value,(_key,v)=>v&&typeof v==='object'&&!Array.isArray(v)
  ?Object.fromEntries(Object.entries(v).sort(([a],[b])=>a.localeCompare(b))):v);

/** Reload, compare and merge an inventory under the same lock as secret writes.
 * baseline is used only to migrate an existing page vault. Changes map ids to
 * [original, replacement]; null means absent/deleted. Nothing writes on conflict.
 * A committed JSON result or failure is consumed by the next matching call.
 */
export function acknowledgedMerge(app,key,baseline,changes,env={}){
  const operation=JSON.stringify(['merge',app,key,baseline,changes]);
  return acknowledged(operation,async()=>{
    const e=environment(env);
    return exclusive(e.locks,app,async()=>{
      const stored=await getSecret(app,key,e);
      const entries=Object.assign(Object.create(null),stored?JSON.parse(new TextDecoder().decode(stored)):JSON.parse(baseline));
      for(const [id,[original,replacement]] of Object.entries(JSON.parse(changes))){
        const current=Object.hasOwn(entries,id)?entries[id]:null;
        if(canonical(current)!==canonical(original)&&canonical(current)!==canonical(replacement)){
          throw Error('credential changed concurrently; reopen and retry');
        }
        if(replacement===null)delete entries[id];else entries[id]=replacement;
      }
      const json=JSON.stringify(entries);
      const cryptoKey=await appKey(e,app,true);
      const iv=e.random(new Uint8Array(12));
      const data=await e.subtle.encrypt({name:'AES-GCM',iv,additionalData:bound(app,key)},cryptoKey,new TextEncoder().encode(json));
      await e.storage.put(ITEMS,[app,key],{iv,data:new Uint8Array(data)});
      return json;
    });
  });
}

/** Await explicit operations too, before any document/file flush. */
export async function flushAcknowledgements(){
  const snapshot=[...operations.values()];
  await Promise.all(snapshot.map(op=>op.completion));
  const failed=snapshot.find(op=>op.state==='failed');
  if(failed)throw failed.error;
}
