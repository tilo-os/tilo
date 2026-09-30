import test from 'node:test';
import assert from 'node:assert/strict';
import {webcrypto} from 'node:crypto';
import {createFontDemandPump,createFontLoader,createIdleFontInstaller,createIdlePrewarm} from './font-loader.mjs';

const bytes={
  core:new Uint8Array([1,2,3]),
  arabic:new Uint8Array([4,5]),
  cjk:new Uint8Array([6,7,8,9]),
  emoji:new Uint8Array([10]),
};
const hashes={
  core:'039058c6f2c0cb492c533b0a4d14ef77cc0f78abccced5287d84a1a2011cfb81',
  arabic:'2fa1b377bf67309f65e5e5b57c12d1d2c6f6f3aa0f2ef231744b406a142440f4',
  cjk:'d4e73f1f5b1e1f8e7a90ef3d303e79ba6e1a138e7e6a74a7a4ad9a4b43f5a7b6',
  emoji:'01ba4719c80b6fe911b091a7c05124b64eeece964e09c058ef8f9805daca546b',
};

// Derive fixture hashes with the platform primitive. The expected behavior
// still independently checks the loader against deliberately corrupt bytes.
async function descriptor(){
  for(const [name,value] of Object.entries(bytes))hashes[name]=Buffer.from(await webcrypto.subtle.digest('SHA-256',value)).toString('hex');
  return {schema:1,catalog:'release-a',assets:Object.entries(bytes).map(([name,value])=>({name:`${name}.ttf`,bytes:value.byteLength,sha256:hashes[name],startup:name==='core'}))};
}
function response(body,{status=200}={}){
  const value=body instanceof Uint8Array?body:new TextEncoder().encode(JSON.stringify(body));
  return {ok:status>=200&&status<300,status,clone(){return response(value.slice(),{status});},async arrayBuffer(){return value.slice().buffer;},async json(){return JSON.parse(new TextDecoder().decode(value));}};
}
test('startup fetches only declared startup fonts and verifies immutable bytes',async()=>{
  const manifest=await descriptor(),requested=[];
  const loader=createFontLoader({baseURL:'https://example.test/app/',crypto:webcrypto,fetch:async (url,options)=>{
    requested.push([String(url),options]);
    if(String(url).endsWith('fonts.json'))return response(manifest);
    const name=new URL(url).pathname.split('/').at(-1).replace('.ttf','');
    return response(bytes[name]);
  }});
  const startup=await loader.startup();
  assert.deepEqual(Object.keys(startup),['core.ttf']);
  assert.deepEqual([...startup['core.ttf']],[1,2,3]);
  assert.deepEqual(requested.map(([url])=>new URL(url).pathname),['/app/fonts.json','/app/fonts/core.ttf']);
  assert(requested.every(([,options])=>options.mode==='same-origin'&&options.redirect==='error'&&options.credentials==='same-origin'));
});

test('optional demands are deduplicated and bounded to two concurrent fetches',async()=>{
  const manifest=await descriptor(),gates=new Map(),starts=[];let concurrent=0,peak=0;
  const loader=createFontLoader({baseURL:'https://example.test/app/',crypto:webcrypto,fetch:async url=>{
    if(String(url).endsWith('fonts.json'))return response(manifest);
    const name=new URL(url).pathname.split('/').at(-1).replace('.ttf','');starts.push(name);concurrent++;peak=Math.max(peak,concurrent);
    await new Promise(resolve=>gates.set(name,resolve));concurrent--;return response(bytes[name]);
  }});
  await loader.manifest();
  const arabicA=loader.request('arabic.ttf'),arabicB=loader.request('arabic.ttf');
  const cjk=loader.request('cjk.ttf'),emoji=loader.request('emoji.ttf');
  await new Promise(resolve=>setTimeout(resolve,0));
  assert.strictEqual(arabicA,arabicB);assert.equal(peak,2);assert.deepEqual(starts,['arabic','cjk']);
  gates.get('arabic')();await arabicA;await new Promise(resolve=>setTimeout(resolve,0));assert.deepEqual(starts,['arabic','cjk','emoji']);
  gates.get('cjk')();gates.get('emoji')();await Promise.all([cjk,emoji]);assert.equal(peak,2);
});

test('CacheStorage cannot block verified startup font readiness',async()=>{
  const manifest=await descriptor(),requested=[];let cacheOpens=0,timeout;
  const loader=createFontLoader({baseURL:'https://example.test/releases/digest/',crypto:webcrypto,
    caches:{open:()=>{cacheOpens++;return new Promise(()=>{});}},
    fetch:async url=>{requested.push(String(url));return String(url).endsWith('fonts.json')?response(manifest):response(bytes.core);},
  });
  const outcome=await Promise.race([
    loader.startup(),
    new Promise(resolve=>{timeout=setTimeout(()=>resolve('blocked'),100);}),
  ]);
  clearTimeout(timeout);
  assert.notEqual(outcome,'blocked');
  assert.deepEqual([...outcome['core.ttf']],[1,2,3]);
  assert.equal(cacheOpens,0);
  assert.deepEqual(requested,[
    'https://example.test/releases/digest/fonts.json',
    'https://example.test/releases/digest/fonts/core.ttf',
  ]);
});

test('an integrity mismatch reloads once outside the browser HTTP cache',async()=>{
  const manifest=await descriptor(),fontRequests=[];
  const loader=createFontLoader({baseURL:'https://example.test/releases/digest/',crypto:webcrypto,fetch:async (url,options)=>{
    if(String(url).endsWith('fonts.json'))return response(manifest);
    fontRequests.push(options);return response(fontRequests.length===1?new Uint8Array([9,9,9]):bytes.core);
  }});
  assert.deepEqual([...(await loader.startup())['core.ttf']],[1,2,3]);
  assert.equal(fontRequests.length,2);
  assert.equal(fontRequests[0].cache,undefined);
  assert.equal(fontRequests[1].cache,'reload');
  assert(fontRequests.every(options=>options.credentials==='same-origin'&&options.mode==='same-origin'&&options.redirect==='error'));
});

test('a permanently corrupt HTTP response is rejected after one reload',async()=>{
  const manifest=await descriptor();let fontRequests=0;
  const loader=createFontLoader({baseURL:'https://example.test/releases/digest/',crypto:webcrypto,fetch:async url=>{
    if(String(url).endsWith('fonts.json'))return response(manifest);
    fontRequests++;return response(new Uint8Array([9,9,9]));
  }});
  await assert.rejects(loader.startup(),/font asset (size|SHA-256) mismatch/);
  assert.equal(fontRequests,2);
});

test('an offline font request fails without an arbitrary retry',async()=>{
  const manifest=await descriptor();let fontRequests=0;
  const loader=createFontLoader({baseURL:'https://example.test/releases/digest/',crypto:webcrypto,fetch:async url=>{
    if(String(url).endsWith('fonts.json'))return response(manifest);
    fontRequests++;throw Error('offline');
  }});
  await assert.rejects(loader.startup(),/offline/);
  assert.equal(fontRequests,1);
});

test('failed optional demand backs off, then a later relevant demand retries',async()=>{
  const manifest=await descriptor();let now=1000,attempts=0,available=false;
  const loader=createFontLoader({baseURL:'https://example.test/app/',crypto:webcrypto,caches:undefined,now:()=>now,retryDelayMs:500,fetch:async url=>{
    if(String(url).endsWith('fonts.json'))return response(manifest);attempts++;return available?response(bytes.arabic):response(new Uint8Array(),{status:503});
  }});
  await assert.rejects(loader.request('arabic.ttf'),/HTTP 503/);assert.equal(attempts,1);
  now=1200;await assert.rejects(loader.request('arabic.ttf'),/backoff/);assert.equal(attempts,1);
  now=1500;available=true;assert.deepEqual([...(await loader.request('arabic.ttf'))],[4,5]);assert.equal(attempts,2);
});

test('asset names come only from the fixed manifest and cannot escape the font origin',async()=>{
  const manifest=await descriptor();
  const loader=createFontLoader({baseURL:'https://example.test/app/',crypto:webcrypto,caches:undefined,fetch:async url=>String(url).endsWith('fonts.json')?response(manifest):response(bytes.core)});
  await assert.rejects(loader.request('../secret'),/unknown font asset/);
  await assert.rejects(loader.request('https://evil.test/core.ttf'),/unknown font asset/);
});

test('a demand completing after mount disposal cannot install or redraw',async()=>{
  let resolve,valid=true;const installed=[],reports=[];
  const loader={request:()=>new Promise(done=>{resolve=done;})};
  const pump=createFontDemandPump({loader,valid:()=>valid,install:(name,value)=>installed.push([name,value]),report:value=>reports.push(value)});
  const completion=pump(['arabic.ttf']);valid=false;resolve(new Uint8Array([4,5]));await completion;
  assert.deepEqual(installed,[]);assert.deepEqual(reports,[]);
});

test('a completion owned by a suspended mount still installs for its later resume',async()=>{
  let resolve,renderActive=true;const installed=[];
  const pump=createFontDemandPump({loader:{request:()=>new Promise(done=>{resolve=done;})},valid:()=>true,install:name=>installed.push([name,renderActive]),report:()=>{}});
  const completion=pump(['arabic.ttf']);renderActive=false;resolve(bytes.arabic);await completion;
  assert.deepEqual(installed,[['arabic.ttf',false]]);
});

test('a live demand installs once and reports optional failure without throwing into status delivery',async()=>{
  const installed=[],reports=[],failed=[];
  const loader={request:name=>name==='bad.ttf'?Promise.reject(Error('offline')):Promise.resolve(new Uint8Array([1]))};
  const pump=createFontDemandPump({loader,valid:()=>true,install:(name,value)=>installed.push([name,[...value]]),failed:name=>failed.push(name),report:value=>reports.push(value)});
  await pump(['good.ttf','good.ttf','bad.ttf']);
  assert.deepEqual(installed,[['good.ttf',[1]]]);
  assert.deepEqual(failed,['bad.ttf']);
  assert.equal(reports.length,1);assert.equal(reports[0].font,'unavailable');assert.match(reports[0].fontError,/offline/);assert.equal('error' in reports[0],false);
});

test('a relevant demand during backoff retries once at the fixed deadline without rearming Rust',async()=>{
  let now=1200,requests=0;const releases=[],installed=[],failed=[],reports=[];
  const backoff=Error('font asset retry backoff: arabic.ttf');backoff.fontBackoff=true;backoff.retryAt=1500;
  const extended=Error('font asset retry backoff: arabic.ttf');extended.fontBackoff=true;extended.retryAt=1800;
  const loader={request:()=>requests++===0?Promise.reject(backoff):requests===2?Promise.reject(extended):Promise.resolve(bytes.arabic)};
  const completion=createFontDemandPump({
    loader,valid:()=>true,now:()=>now,sleep:ms=>new Promise(resolve=>{releases.push([ms,resolve]);}),
    install:(name,value)=>installed.push([name,[...value]]),failed:name=>failed.push(name),report:value=>reports.push(value),
  })(['arabic.ttf']);
  await new Promise(resolve=>setTimeout(resolve,0));
  assert.equal(requests,1);assert.deepEqual(failed,[]);assert.deepEqual(reports,[]);
  assert.equal(releases[0][0],300);now=1500;releases.shift()[1]();await new Promise(resolve=>setTimeout(resolve,0));
  assert.equal(requests,2);assert.equal(releases[0][0],300);now=1800;releases.shift()[1]();await completion;
  assert.equal(requests,3);assert.deepEqual(installed,[['arabic.ttf',[4,5]]]);assert.deepEqual(failed,[]);
});

test('a disposed mount cancels its scheduled backoff retry before network or install',async()=>{
  let now=1200,release,valid=true,requests=0;const installed=[];
  const backoff=Error('font asset retry backoff: arabic.ttf');backoff.fontBackoff=true;backoff.retryAt=1500;
  const completion=createFontDemandPump({
    loader:{request:()=>{requests++;return Promise.reject(backoff);}},valid:()=>valid,now:()=>now,
    sleep:()=>new Promise(resolve=>{release=resolve;}),install:name=>installed.push(name),report:()=>{},
  })(['arabic.ttf']);
  await new Promise(resolve=>setTimeout(resolve,0));
  valid=false;now=1500;release();await completion;
  assert.equal(requests,1);assert.deepEqual(installed,[]);
});

test('a successful retry clears only its prior font-specific availability issue',async()=>{
  let now=1200,release,request=0;const reports=[];
  const backoff=Error('font asset retry backoff: arabic.ttf');backoff.fontBackoff=true;backoff.retryAt=1500;
  const loader={request:()=>{
    request++;
    if(request===1)return Promise.reject(Error('offline'));
    if(request===2)return Promise.reject(backoff);
    return Promise.resolve(bytes.arabic);
  }};
  const pump=createFontDemandPump({loader,valid:()=>true,now:()=>now,sleep:()=>new Promise(resolve=>{release=resolve;}),install:()=>{},failed:()=>{},report:value=>reports.push(value)});
  await pump(['arabic.ttf']);
  const completion=pump(['arabic.ttf']);await new Promise(resolve=>setTimeout(resolve,0));
  now=1500;release();await completion;
  assert.deepEqual(reports.map(({font,fontAsset})=>({font,fontAsset})),[
    {font:'unavailable',fontAsset:'arabic.ttf'},
    {font:'available',fontAsset:'arabic.ttf'},
  ]);
  assert.equal('error' in reports[0],false);assert.match(reports[0].fontError,/offline/);
});

function prewarmFixture({fail=new Set(),corrupt=new Set(),gate=false}={}){
  const requested=[],gates=new Map(),arrivals=new Map();let manifest;
  const arrival=name=>{if(!arrivals.has(name)){let resolve;arrivals.set(name,{seen:new Promise(r=>{resolve=r;}),resolve});}return arrivals.get(name);};
  const fetch=async (url,options)=>{
    if(String(url).endsWith('fonts.json'))return response(manifest);
    const name=new URL(url).pathname.split('/').at(-1).replace('.ttf','');requested.push([name,options?.cache??'default']);arrival(name).resolve();
    if(gate)await new Promise(resolve=>gates.set(name,resolve));
    if(fail.has(name))throw TypeError('offline');
    return response(corrupt.has(name)?new Uint8Array(bytes[name].byteLength):bytes[name]);
  };
  // fetched(name) settles once a fetch for name starts, however long the
  // verification before it takes.
  return {requested,gates,fetch,fetched:name=>arrival(name).seen,async ready(){manifest=await descriptor();return manifest;}};
}

test('pre-warm verifies optional fonts within budget and a later demand is a cache hit',async()=>{
  const f=prewarmFixture();await f.ready();
  const loader=createFontLoader({baseURL:'https://example.test/app/',crypto:webcrypto,fetch:f.fetch});
  // cjk (4 bytes) exceeds the per-asset bound; the total budget admits arabic + emoji.
  const warmed=await loader.prewarm({maxAssetBytes:3,budgetBytes:3,prefer:['emoji.ttf']});
  assert.deepEqual(warmed,['emoji.ttf','arabic.ttf']);
  assert.deepEqual(f.requested.map(([name])=>name),['emoji','arabic']);
  assert.deepEqual([...await loader.request('emoji.ttf')],[10]);
  assert.deepEqual([...await loader.request('arabic.ttf')],[4,5]);
  assert.equal(f.requested.length,2,'a warmed font is served from memory');
  await loader.request('cjk.ttf');
  assert.deepEqual(f.requested.map(([name])=>name),['emoji','arabic','cjk'],'an unwarmed font is a miss that fetches');
});

test('pre-warm keeps integrity checks: bytes that differ from the manifest digest are never retained',async()=>{
  const f=prewarmFixture({corrupt:new Set(['arabic'])});await f.ready();
  const loader=createFontLoader({baseURL:'https://example.test/app/',crypto:webcrypto,fetch:f.fetch});
  assert.deepEqual(await loader.prewarm(),['cjk.ttf','emoji.ttf']);
  // The mismatch was reloaded once outside the HTTP cache and then dropped.
  assert.deepEqual(f.requested.filter(([name])=>name==='arabic').map(([,cache])=>cache),['default','reload']);
  await assert.rejects(loader.request('arabic.ttf'),/SHA-256 mismatch/);
  assert.equal(f.requested.filter(([name])=>name==='arabic').length,4,'a demand re-verifies from the network, not from the pre-warm');
});

test('a new release manifest invalidates warmed bytes: loaders are per release root',async()=>{
  const f=prewarmFixture();await f.ready();
  const a=createFontLoader({baseURL:'https://example.test/releases/a/',crypto:webcrypto,fetch:f.fetch});
  await a.prewarm({prefer:['emoji.ttf'],budgetBytes:1});
  const changed=await descriptor();changed.catalog='release-b';changed.assets.find(asset=>asset.name==='emoji.ttf').sha256='0'.repeat(64);
  const b=createFontLoader({baseURL:'https://example.test/releases/b/',crypto:webcrypto,fetch:async (url,options)=>String(url).endsWith('fonts.json')?response(changed):f.fetch(url,options)});
  await assert.rejects(b.request('emoji.ttf'),/SHA-256 mismatch/,'bytes warmed for the old digest do not satisfy the new one');
  assert.deepEqual([...await a.request('emoji.ttf')],[10]);
});

test('a failed pre-warm never arms the demand backoff',async()=>{
  const f=prewarmFixture({fail:new Set(['emoji'])});await f.ready();
  const loader=createFontLoader({baseURL:'https://example.test/app/',crypto:webcrypto,fetch:f.fetch,now:()=>0});
  assert.deepEqual(await loader.prewarm({prefer:['emoji.ttf'],budgetBytes:1}),[]);
  await assert.rejects(loader.request('emoji.ttf'),/offline/);
  assert.equal(f.requested.filter(([name])=>name==='emoji').length,2,'the demand fetched at once instead of waiting out a backoff');
});

test('a demand is never queued behind idle pre-warm work',async()=>{
  const f=prewarmFixture({gate:true});await f.ready();
  const loader=createFontLoader({baseURL:'https://example.test/app/',crypto:webcrypto,fetch:f.fetch});
  const warming=loader.prewarm({prefer:['emoji.ttf','arabic.ttf'],maxAssetBytes:2});
  await new Promise(resolve=>setTimeout(resolve,0));
  assert.deepEqual(f.requested.map(([name])=>name),['emoji'],'pre-warm uses one fetch at a time');
  const cjk=loader.request('cjk.ttf');await new Promise(resolve=>setTimeout(resolve,0));
  assert.deepEqual(f.requested.map(([name])=>name),['emoji','cjk'],'the demand starts beside the pre-warm fetch');
  f.gates.get('cjk')();await cjk;
  // Once emoji is verified, arabic's pre-warm fetch starts; a demand for it shares that fetch.
  f.gates.get('emoji')();await f.fetched('arabic');
  const arabic=loader.request('arabic.ttf');await new Promise(resolve=>setTimeout(resolve,0));
  assert.equal(f.requested.filter(([name])=>name==='arabic').length,1);
  f.gates.get('arabic')();
  assert.deepEqual([...await arabic],[4,5]);
  assert.deepEqual(await warming,['emoji.ttf','arabic.ttf']);
});

test('idle pre-warm runs once, on idle time with a timeout fallback',async()=>{
  const calls=[];let ran=0;
  const schedule=createIdlePrewarm(()=>{ran++;return 'warm';},{requestIdle:(fn,options)=>calls.push(['idle',options,fn])});
  const first=schedule();assert.strictEqual(schedule(),first);
  assert.equal(calls.length,1);assert.equal(calls[0][1].timeout,2000);assert.equal(ran,0);
  calls[0][2]();assert.equal(await first,'warm');assert.equal(ran,1);
  const timers=[];const failing=createIdlePrewarm(()=>{throw Error('ignored');},{requestIdle:undefined,setTimer:(fn,ms)=>timers.push([fn,ms])})();
  assert.equal(timers[0][1],2000);timers[0][0]();assert.equal(await failing,undefined,'a failed pre-warm never rejects into the mount');
});

test('a demand for a queued pre-warm font promotes it to a demand fetch',async()=>{
  const f=prewarmFixture({gate:true});await f.ready();
  const loader=createFontLoader({baseURL:'https://example.test/app/',crypto:webcrypto,fetch:f.fetch,optionalConcurrency:1});
  const cjk=loader.request('cjk.ttf');await new Promise(resolve=>setTimeout(resolve,0));
  const warming=loader.prewarm({prefer:['emoji.ttf'],budgetBytes:1});await new Promise(resolve=>setTimeout(resolve,0));
  assert.deepEqual(f.requested.map(([name])=>name),['cjk'],'idle work waits while the demand holds the only slot');
  const emoji=loader.request('emoji.ttf');
  f.gates.get('cjk')();await cjk;await new Promise(resolve=>setTimeout(resolve,0));
  assert.deepEqual(f.requested.map(([name])=>name),['cjk','emoji']);
  f.gates.get('emoji')();
  assert.deepEqual([...await emoji],[10]);assert.deepEqual(await warming,['emoji.ttf']);
  assert.equal(f.requested.length,2);
});

test('idle installer installs only verified in-memory fonts, one per idle slice, and never fetches',async()=>{
  const f=prewarmFixture();await f.ready();
  const loader=createFontLoader({baseURL:'https://example.test/app/',crypto:webcrypto,fetch:f.fetch});
  await loader.prewarm({prefer:['emoji.ttf'],budgetBytes:1});
  const idle=[],installs=[];let live=true;
  const installer=createIdleFontInstaller({loader,names:['emoji.ttf','arabic.ttf','cjk.ttf'],valid:()=>live,install:(name,value)=>installs.push([name,[...value]]),requestIdle:fn=>idle.push(fn)});
  assert.equal(installer.start(),true);assert.equal(installer.start(),false);
  const fetched=f.requested.length;
  idle.shift()({didTimeout:false,timeRemaining:()=>2});
  assert.deepEqual(installs,[],'a short idle slice defers the install');
  idle.shift()({didTimeout:false,timeRemaining:()=>40});
  assert.deepEqual(installs,[['emoji.ttf',[10]]]);
  idle.shift()({didTimeout:false,timeRemaining:()=>40});
  assert.deepEqual(installs.map(([name])=>name),['emoji.ttf'],'a font not pre-warmed is left to its demand');
  live=false;idle.shift()({didTimeout:true,timeRemaining:()=>0});
  assert.equal(installs.length,1,'a disposed mount installs nothing');
  assert.equal(idle.length,0);assert.equal(f.requested.length,fetched);
});

test('an unavailable font is fetched, verified and installed again on retry, skipping its backoff',async()=>{
  const manifest=await descriptor();let corrupt=true,now=1000;
  const loader=createFontLoader({baseURL:'https://example.test/app/',crypto:webcrypto,now:()=>now,fetch:async url=>{
    if(String(url).endsWith('fonts.json'))return response(manifest);
    return response(corrupt?new Uint8Array([0,0]):bytes.arabic);
  }});
  const installed=[],reports=[],failed=[];
  const pump=createFontDemandPump({loader,valid:()=>true,install:(name,value)=>installed.push([name,[...value]]),failed:name=>failed.push(name),report:value=>reports.push(value)});
  await pump(['arabic.ttf']);
  assert.deepEqual(failed,['arabic.ttf']);assert.equal(reports.at(-1).font,'unavailable');
  await assert.rejects(pump.retry('arabic.ttf'),/SHA-256 mismatch/);
  corrupt=false;
  await pump.retry('arabic.ttf');
  assert.deepEqual(installed,[['arabic.ttf',[4,5]]]);
  assert.deepEqual(reports.at(-1),{font:'available',fontAsset:'arabic.ttf'});
  await assert.rejects(pump.retry('arabic.ttf'),/not unavailable/);
});
