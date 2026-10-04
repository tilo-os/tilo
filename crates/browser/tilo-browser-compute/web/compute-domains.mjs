import {VERSION,CHUNK,fault,inputView,bufferView,copyChunk,fields,matches,taskYield} from './compute-protocol.mjs';
const MiB=1024*1024;
const text=value=>typeof value==='string' && value.length>0 && value.length<=256;
// One page-global instance, explicitly installed by the host. No work/timers at idle.
// Managed bytes bound owned staging, not caller buffers, WASM heaps or browser RSS.
export function createComputeDomains({workerFactory=()=>new Worker(new URL('./compute-worker.mjs',import.meta.url),{type:'module'}),yieldTask=taskYield,timeoutMs=30000}={}) {
  if(!Number.isSafeInteger(timeoutMs)||timeoutMs<1||timeoutMs>2147483647)throw fault('protocol');
  const jobs=new Set(), ready=[],bindings=new Set();
  let next=0,leases=0,background=0,encoded=0,managed=0;
  const snapshot=()=>({jobs:jobs.size,leases,encoded,managed});
  function authorized(job) {
    let permitted=false;
    try { permitted=job.owner.authorize()===true; } catch {}
    return permitted && !job.dead && !job.owner.closed;
  }
  function retire(job) {
    if(!job.leased)return;
    job.leased=false;leases--;if(job.owner.lane==='background')background--;
    job.owner.active=false;clearTimeout(job.timer);
    const queued=ready.indexOf(job.owner);if(queued>=0){ready.splice(queued,1);ready.push(job.owner);}
    if(job.worker){for(const [k,f] of job.listeners)job.worker.removeEventListener(k,f);job.worker.terminate();job.worker=null;}
  }
  function release(job) {
    if(!jobs.delete(job))return;
    encoded-=job.size;managed-=job.weight;job.input=null;job.output=null;job.consume=null;
  }
  function finish(job,error) {
    if(job.dead)return;
    job.dead=true;retire(job);
    // A consumer may retain pixels across awaits. Abort rejects promptly but
    // never frees its reservation until that actual consumer relinquishes it.
    if(!job.consuming&&!job.preparing)release(job);
    if(error)job.reject(error);else job.resolve();
    pump();
  }
  async function consume(job) {
    if(!authorized(job)){finish(job,fault('closed'));return;}
    // Retirement and the next owner's authorize callback can reenter/close us.
    // Keep custody across the entire handoff, then check at actual delivery.
    job.consuming=true;
    try {
      retire(job);pump();
      if(!authorized(job))throw fault('closed');
      await job.consume({rgba:job.output,width:job.width,height:job.height,identity:job.identity,timings:job.timings});finish(job);
    }
    catch(error){finish(job,error);}
    finally{job.consuming=false;release(job);}
  }
  function receive(job,event) {
    if(job.dead||!job.leased||!job.worker)return; // exact closure generation
    try {
      const callbackStart=performance.now();
      const m=fields(event.data);
      if(!matches(m,job.identity))throw fault('protocol');
      if(m.type==='failed')throw fault('decode');
      if(m.type==='result'){
        if(job.output||![m.width,m.height].every(n=>Number.isSafeInteger(n)&&n>0&&n<=job.maxDimension)||m.length!==m.width*m.height*4||m.length>job.maxOutput||m.worker!==true||!Number.isFinite(m.decodeMs)||m.decodeMs<0)throw fault('protocol');
        const allocationStart=performance.now();job.output=new Uint8Array(m.length);
        job.timings.allocationMs=performance.now()-allocationStart;job.width=m.width;job.height=m.height;job.received=0;
        job.timings.decodeMs=m.decodeMs;job.timings.resultStart=performance.now();
        job.worker.postMessage({type:'credit',offset:0});job.timings.headerMs=performance.now()-callbackStart;return;
      }
      if(m.type!=='chunk'||!job.output||m.offset!==job.received)throw fault('protocol');
      const bytes=bufferView(m.bytes);
      if(bytes.length===0||bytes.length>CHUNK||bytes.length!==Math.min(CHUNK,job.output.length-job.received))throw fault('protocol');
      job.output.set(bytes,job.received);job.received+=bytes.length;
      const complete=job.received===job.output.length;
      if(!complete)job.worker.postMessage({type:'credit',offset:job.received});
      job.timings.maxChunkMs=Math.max(job.timings.maxChunkMs??0,performance.now()-callbackStart);
      if(complete){job.timings.stagingMs=performance.now()-job.timings.resultStart;void consume(job);}
    } catch(error){finish(job,error);}
  }
  async function start(job) {
    job.preparing=true;
    try {
      if(!authorized(job))throw fault('closed');
      const began=performance.now(),owned=new Uint8Array(job.size);
      for(let offset=0;offset<job.size;offset+=CHUNK){
        if(job.dead)return;
        copyChunk(owned,job.input,offset,Math.min(CHUNK,job.size-offset));
        await yieldTask(); // actual event-loop turn; bounded synchronous work
      }
      if(!authorized(job))throw fault('closed');
      job.timings.inputMs=performance.now()-began;job.input=null;
      const worker=workerFactory();job.worker=worker;
      if(job.dead){worker.terminate();job.worker=null;return;}
      job.listeners=[['message',e=>receive(job,e)],['error',()=>finish(job,fault('worker'))],['messageerror',()=>finish(job,fault('worker'))]];
      for(const [kind,fn]of job.listeners)worker.addEventListener(kind,fn);
      if(!authorized(job))throw fault('closed');
      job.timings.dispatch=performance.now();
      worker.postMessage({type:'decode',identity:job.identity,maxDimension:job.maxDimension,bytes:owned.buffer},[owned.buffer]);
    }catch(error){finish(job,error);}
    finally{job.preparing=false;if(job.dead)release(job);}
  }
  function pump() {
    while(leases<2){
      const index=ready.findIndex(o=>!o.closed&&!o.active&&o.queue.length&&(o.lane==='foreground'||background<1));
      if(index<0)return;
      const owner=ready.splice(index,1)[0],job=owner.queue.shift();
      if(owner.queue.length)ready.push(owner);
      if(job.dead)continue;
      owner.active=true;job.leased=true;leases++;if(owner.lane==='background')background++;
      job.timer=setTimeout(()=>finish(job,fault('timeout')),timeoutMs);
      void start(job);
    }
  }
  function bindOwner(options) {
    const o=fields(options);
    if(![o.owner,o.lifetime,o.domain].every(text)||!['foreground','background'].includes(o.lane)||typeof o.authorize!=='function')throw fault('protocol');
    const binding=JSON.stringify([o.owner,o.lifetime]);
    if(bindings.has(binding))throw fault('binding');
    bindings.add(binding);
    const owner={...o,queue:[],active:false,closed:false};
    const close=()=>{
      if(owner.closed)return;owner.closed=true;bindings.delete(binding);owner.signal?.removeEventListener('abort',close);
      for(const job of jobs)if(job.owner===owner)finish(job,fault('closed'));
      owner.queue.length=0;const i=ready.indexOf(owner);if(i>=0)ready.splice(i,1);
    };
    owner.signal?.addEventListener('abort',close,{once:true});if(owner.signal?.aborted)close();
    return Object.freeze({close,decode(request,consumer){
      try {
        const r=fields(request),input=inputView(r.bytes),size=input.length;
        if(owner.closed)throw fault('closed');
        if(!text(r.source)||typeof consumer!=='function'||!Number.isSafeInteger(r.maxDimension)||r.maxDimension<1||r.maxDimension>4096)throw fault('protocol');
        const maxOutput=4*r.maxDimension*r.maxDimension,weight=2*size+2*maxOutput+2*Math.min(CHUNK,maxOutput);
        if(size===0||size>16*MiB||jobs.size>=8||encoded+size>32*MiB||managed+weight>192*MiB)throw fault('budget');
        if(next===Number.MAX_SAFE_INTEGER)throw fault('overflow');
        const identity=Object.freeze({version:VERSION,owner:o.owner,lifetime:o.lifetime,domain:o.domain,request:++next,source:r.source});
        return new Promise((resolve,reject)=>{
          const job={owner,identity,input,size,weight,maxOutput,maxDimension:r.maxDimension,consume:consumer,resolve,reject,timings:{},listeners:[],dead:false};
          jobs.add(job);encoded+=size;managed+=weight;owner.queue.push(job);
          if(!ready.includes(owner))ready.push(owner);pump();
        });
      }catch(error){return Promise.reject(error);}
    }});
  }
  return Object.freeze({bindOwner,snapshot});
}
