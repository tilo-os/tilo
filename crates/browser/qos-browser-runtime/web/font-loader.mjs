const hex=bytes=>[...new Uint8Array(bytes)].map(value=>value.toString(16).padStart(2,'0')).join('');

/**
 * Loads the canonical browser font catalog. Startup fonts form the mount
 * barrier; optional fonts enter this loader only after a Rust demand.
 */
export function createFontLoader({
  baseURL,
  fetch:fetchAsset=globalThis.fetch,
  crypto=globalThis.crypto,
  now=Date.now,
  retryDelayMs=2_000,
  optionalConcurrency=2,
}={}){
  const root=new URL(baseURL??import.meta.url);
  let manifestPending,parsed;
  const loaded=new Map(),pending=new Map(),failures=new Map(),queue=[];
  let active=0;

  async function fetchRequired(url,options={}){
    const result=await fetchAsset(url,{...options,credentials:'same-origin',mode:'same-origin',redirect:'error'});
    if(!result.ok)throw Error(`font asset HTTP ${result.status}`);
    return result;
  }
  function parseManifest(value){
    if(value?.schema!==1||typeof value.catalog!=='string'||!Array.isArray(value.assets))throw Error('invalid canonical font manifest');
    const assets=new Map();
    for(const asset of value.assets){
      if(!asset||typeof asset.name!=='string'||!/^[A-Za-z0-9][A-Za-z0-9._\[\]-]*$/.test(asset.name)||
        !Number.isSafeInteger(asset.bytes)||asset.bytes<1||!/^[0-9a-f]{64}$/.test(asset.sha256)||typeof asset.startup!=='boolean'||assets.has(asset.name)){
        throw Error('invalid canonical font asset descriptor');
      }
      assets.set(asset.name,Object.freeze({...asset}));
    }
    return Object.freeze({schema:1,catalog:value.catalog,assets});
  }
  async function loadManifest(){
    const url=new URL('./fonts.json',root);
    return parseManifest(await (await fetchRequired(url)).json());
  }
  function manifest(){
    if(parsed)return Promise.resolve(parsed);
    if(!manifestPending){
      const attempt=loadManifest().then(value=>(parsed=value));
      manifestPending=attempt;attempt.catch(()=>{if(manifestPending===attempt)manifestPending=undefined;});
    }
    return manifestPending;
  }
  async function verify(asset,value){
    const bytes=value instanceof Uint8Array?value:new Uint8Array(value);
    if(bytes.byteLength!==asset.bytes)throw Object.assign(Error(`font asset size mismatch: ${asset.name}`),{fontIntegrity:true});
    const digest=hex(await crypto.subtle.digest('SHA-256',bytes));
    if(digest!==asset.sha256)throw Object.assign(Error(`font asset SHA-256 mismatch: ${asset.name}`),{fontIntegrity:true});
    return bytes;
  }
  async function load(asset){
    const url=new URL(`./fonts/${asset.name}`,root);
    if(url.origin!==root.origin)throw Error('font asset crossed its release origin');
    try{return await verify(asset,await (await fetchRequired(url)).arrayBuffer());}
    catch(error){
      if(!error?.fontIntegrity)throw error;
      return verify(asset,await (await fetchRequired(url,{cache:'reload'})).arrayBuffer());
    }
  }
  function schedule(operation){
    return new Promise((resolve,reject)=>{queue.push({operation,resolve,reject});pump();});
  }
  function pump(){
    while(active<optionalConcurrency&&queue.length){
      const job=queue.shift();active++;
      Promise.resolve().then(job.operation).then(job.resolve,job.reject).finally(()=>{active--;pump();});
    }
  }
  function request(name){
    if(loaded.has(name))return Promise.resolve(loaded.get(name));
    if(pending.has(name))return pending.get(name);
    const attempt=manifest().then(value=>{
      const asset=value.assets.get(name);
      if(!asset)throw Error(`unknown font asset: ${name}`);
      const failure=failures.get(name);
      if(failure&&now()<failure.until){const error=Error(`font asset retry backoff: ${name}`);error.fontBackoff=true;error.retryAt=failure.until;throw error;}
      return schedule(()=>load(asset));
    }).then(bytes=>{loaded.set(name,bytes);failures.delete(name);return bytes;},error=>{if(!error.fontBackoff)failures.set(name,{until:now()+retryDelayMs,error});throw error;});
    pending.set(name,attempt);attempt.finally(()=>{if(pending.get(name)===attempt)pending.delete(name);}).catch(()=>{});
    return attempt;
  }
  async function startup(){
    const value=await manifest();
    const assets=[...value.assets.values()].filter(asset=>asset.startup);
    const pairs=await Promise.all(assets.map(async asset=>[asset.name,await load(asset)]));
    for(const [name,bytes] of pairs)loaded.set(name,bytes);
    return Object.fromEntries(pairs);
  }
  return {manifest,startup,request};
}

/** Couples optional completion to one live mount generation. */
export function createFontDemandPump({loader,valid,install,failed=()=>{},report,now=Date.now,sleep=ms=>new Promise(resolve=>setTimeout(resolve,ms))}){
  const applying=new Map(),unavailable=new Set();
  return names=>Promise.allSettled([...new Set(names)].map(name=>{
    if(applying.has(name))return applying.get(name);
    const apply=async()=>{
      let bytes;
      while(valid()){
        try{bytes=await loader.request(name);break;}
        catch(error){
          if(!error?.fontBackoff)throw error;
          const retryAt=Number.isFinite(error.retryAt)?error.retryAt:now();
          await sleep(Math.max(1,retryAt-now()));
        }
      }
      if(bytes===undefined||!valid())return;
      install(name,bytes);
      if(unavailable.delete(name))report({font:'available',fontAsset:name});
    };
    const job=apply().catch(error=>{
      if(valid()){
        failed(name);
        unavailable.add(name);
        report({font:'unavailable',fontAsset:name,fontError:String(error)});
      }
    }).finally(()=>{if(applying.get(name)===job)applying.delete(name);});
    applying.set(name,job);return job;
  }));
}
