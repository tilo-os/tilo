const hex=bytes=>[...new Uint8Array(bytes)].map(value=>value.toString(16).padStart(2,'0')).join('');

/**
 * Loads the canonical browser font catalog. Startup fonts form the mount
 * barrier; optional fonts are installed only after a Rust demand, though an
 * idle pre-warm may fetch and verify them earlier.
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
  let active=0,warming=0;
  // Idle pre-warm work: never ahead of a Rust demand, one fetch at a time.
  const idleQueue=[],warmups=new Map();

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
    while(active+warming<optionalConcurrency&&queue.length){
      const job=queue.shift();active++;
      Promise.resolve().then(job.operation).then(job.resolve,job.reject).finally(()=>{active--;pump();});
    }
    // A pre-warm fetch starts only when no demand waits and a slot is free.
    while(!queue.length&&!warming&&active<optionalConcurrency&&idleQueue.length){
      const job=idleQueue.shift();warming++;
      Promise.resolve().then(job.operation).then(job.resolve,job.reject).finally(()=>{warming--;pump();});
    }
  }
  function request(name){
    if(loaded.has(name))return Promise.resolve(loaded.get(name));
    if(pending.has(name))return pending.get(name);
    const warm=warmups.get(name);
    if(warm){
      // A demand never waits behind idle work: a queued pre-warm fetch is
      // promoted, and a failed one falls back to an ordinary demand.
      const index=idleQueue.indexOf(warm.job);
      if(index>=0){idleQueue.splice(index,1);warm.promoted=true;queue.unshift(warm.job);pump();}
      return warm.attempt.catch(()=>{if(warmups.get(name)===warm)warmups.delete(name);return request(name);});
    }
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
  /**
   * Moves verified optional-font work ahead of demand: the same fetch and
   * SHA-256/size check as a demand, one asset at a time behind any demand.
   * Bytes are retained (not installed) so a later demand needs no network.
   * Bounded by a per-asset and a total byte budget. A failure is dropped
   * silently: it never arms the demand backoff, and nothing unverified is
   * ever retained.
   */
  async function prewarm({maxAssetBytes=5*1024*1024,budgetBytes=8*1024*1024,prefer=[],signal}={}){
    const value=await manifest();
    let total=0;const chosen=[];
    const rank=asset=>{const index=prefer.indexOf(asset.name);return index<0?prefer.length:index;};
    for(const asset of [...value.assets.values()].sort((a,b)=>rank(a)-rank(b))){
      if(asset.startup||asset.bytes>maxAssetBytes||total+asset.bytes>budgetBytes)continue;
      total+=asset.bytes;chosen.push(asset);
    }
    const warmed=[];
    for(const asset of chosen){
      if(signal?.aborted)break;
      if(loaded.has(asset.name)||pending.has(asset.name)||warmups.has(asset.name)||failures.has(asset.name))continue;
      const warm={};
      warm.attempt=new Promise((resolve,reject)=>{
        warm.job={operation:()=>signal?.aborted&&!warm.promoted?Promise.reject(Error('font pre-warm cancelled')):load(asset),resolve,reject};
        idleQueue.push(warm.job);
      }).then(bytes=>{if(!loaded.has(asset.name))loaded.set(asset.name,bytes);return loaded.get(asset.name);});
      warmups.set(asset.name,warm);pump();
      try{await warm.attempt;warmed.push(asset.name);}catch{}
      finally{if(warmups.get(asset.name)===warm)warmups.delete(asset.name);}
    }
    return warmed;
  }
  async function startup(){
    const value=await manifest();
    const assets=[...value.assets.values()].filter(asset=>asset.startup);
    const pairs=await Promise.all(assets.map(async asset=>[asset.name,await load(asset)]));
    for(const [name,bytes] of pairs)loaded.set(name,bytes);
    return Object.fromEntries(pairs);
  }
  /** Verified bytes already in memory, or undefined; never fetches. */
  function cached(name){return loaded.get(name);}
  /** A demand for `name` now, without waiting out a failed fetch's backoff. */
  function retry(name){failures.delete(name);return request(name);}
  return {manifest,startup,request,prewarm,cached,retry};
}

/** Couples optional completion to one live mount generation. */
export function createFontDemandPump({loader,valid,install,failed=()=>{},report,now=Date.now,sleep=ms=>new Promise(resolve=>setTimeout(resolve,ms))}){
  const applying=new Map(),unavailable=new Set();
  const demand=names=>Promise.allSettled([...new Set(names)].map(name=>{
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
  /** Fetch, verify and install an unavailable font again, skipping its
   * backoff; rejects while it stays unavailable. */
  demand.retry=async name=>{
    if(!unavailable.has(name))throw Error(`font asset is not unavailable: ${name}`);
    const bytes=await loader.retry(name);
    if(!valid())throw Error('Font surface disposed');
    install(name,bytes);
    if(unavailable.delete(name))report({font:'available',fontAsset:name});
  };
  return demand;
}

/**
 * Runs `work` once, when the page is idle after the first presented frame.
 * Falls back to a timer where requestIdleCallback is unavailable. Every call
 * returns the same settled-never-rejecting promise of that one run.
 */
export function createIdlePrewarm(work,{requestIdle=globalThis.requestIdleCallback?.bind(globalThis),setTimer=globalThis.setTimeout.bind(globalThis),timeoutMs=2_000}={}){
  let scheduled;
  return()=>{
    if(!scheduled)scheduled=new Promise(resolve=>{
      const run=()=>{resolve(Promise.resolve().then(work).catch(()=>undefined));};
      if(requestIdle)requestIdle(run,{timeout:timeoutMs});else setTimer(run,timeoutMs);
    });
    return scheduled;
  };
}

/**
 * Installs already verified, pre-warmed fonts into one live mount during idle
 * periods, one font per idle slice, so a later app open does not pay the
 * install on its first frames. Only fonts the loader holds in memory are
 * installed; nothing is fetched. Stops when the mount is no longer valid.
 */
export function createIdleFontInstaller({loader,names,install,valid,requestIdle=globalThis.requestIdleCallback?.bind(globalThis),setTimer=globalThis.setTimeout.bind(globalThis),minimumSliceMs=12,maxAttempts=60}){
  const remaining=[...names],installed=new Set();let attempts=0,started=false;
  const next=()=>{if(requestIdle)requestIdle(step,{timeout:5_000});else setTimer(()=>step(),50);};
  function step(deadline){
    if(!valid()||!remaining.length||++attempts>maxAttempts)return;
    if(deadline&&!deadline.didTimeout&&deadline.timeRemaining()<minimumSliceMs){next();return;}
    const name=remaining.shift(),bytes=loader.cached(name);
    if(bytes){try{install(name,bytes);installed.add(name);}catch{}}
    if(remaining.length)next();
  }
  return {start(){if(started)return false;started=true;next();return true;},installed};
}
