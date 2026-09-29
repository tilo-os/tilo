// The browser backend of tilo-secrets (src/web.rs). Each app has one AES-GCM
// key the browser generated as non-extractable; the key and every ciphertext
// live in IndexedDB. The additional data binds a ciphertext to its app and
// key, so a record copied to another name does not decrypt. Writers of one
// app take a Web Lock, so two tabs never mint two keys for it.
//
// This protects secrets at rest (a copied profile), not against script
// running in the page's origin. The last parameter of each export is the
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
  const run=async(store,mode,op)=>{const tx=(await db()).transaction(store,mode);return done(op(tx.objectStore(store)));};
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
    subtle:env.subtle??globalThis.crypto?.subtle,
    random:env.random??(bytes=>globalThis.crypto.getRandomValues(bytes)),
    locks:'locks' in env?env.locks:globalThis.navigator?.locks,
  };
}

const exclusive=(locks,app,fn)=>locks?.request?locks.request(`tilo-secrets:${app}`,fn):fn();
const bound=(app,key)=>new TextEncoder().encode(`${app}\u0000${key}`);

async function appKey({storage,subtle},app,create){
  const existing=await storage.get(KEYS,app);
  if(existing||!create)return existing??null;
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

/** Delete the secret `key` of `app`; a missing one is fine. */
export async function deleteSecret(app,key,env={}){
  const e=environment(env);
  await exclusive(e.locks,app,()=>e.storage.delete(ITEMS,[app,key]));
}

/**
 * Every stored secret this origin can decrypt, as `[app, key, bytes]`: the
 * page's synchronous mirror (`src/web.rs`) loads them before apps mount.
 * A record that no longer decrypts (its app's key is gone) is left out.
 */
export async function loadSecrets(env={}){
  const e=environment(env);
  if(!e.subtle||!e.storage.entries)return [];
  const out=[],keys=new Map();
  for(const [[app,key],record] of await e.storage.entries(ITEMS)){
    if(!keys.has(app))keys.set(app,await appKey(e,app,false));
    const cryptoKey=keys.get(app);
    if(!cryptoKey)continue;
    try{
      const plain=await e.subtle.decrypt({name:'AES-GCM',iv:record.iv,additionalData:bound(app,key)},cryptoKey,record.data);
      out.push([app,key,new Uint8Array(plain)]);
    }catch{}
  }
  return out;
}

let tail=Promise.resolve(),failure=null;

/** Store or delete (`bytes` null) in order behind earlier queued writes. */
export function queueSecret(app,key,bytes,env={}){
  tail=tail.then(()=>bytes?putSecret(app,key,bytes,env):deleteSecret(app,key,env)).catch(error=>{failure=error;});
}

/** Resolves once every queued write is stored; rejects with the latest failure. */
export async function flushSecrets(){
  await tail;
  if(failure){const error=failure;failure=null;throw error;}
}
