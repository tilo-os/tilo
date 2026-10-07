import {createComputeDomains} from './compute-domains.mjs';
const check=(condition,message)=>{if(!condition)throw Error(message);};
const sleep=ms=>new Promise(r=>setTimeout(r,ms));
const bytes=async name=>new Uint8Array(await(await fetch(`fixtures/${name}`)).arrayBuffer());
const hash=async value=>Array.from(new Uint8Array(await crypto.subtle.digest('SHA-256',value)),b=>b.toString(16).padStart(2,'0')).join('');
const owner=(pool,id,signal)=>pool.bindOwner({owner:id,lifetime:'qualification-1',domain:'image',lane:'foreground',authorize:()=>true,signal});
async function refused(p,pattern){try{await p;}catch(e){check(pattern.test(e.message),e.message);return;}throw Error('expected refusal');}
export async function qualify(){
  const qualificationStarted=performance.now();
  let workers=0,workerResults=0,ticks=0;const cpuIntervals=[],heartbeatTimes=[];
  const pool=createComputeDomains({workerFactory:()=>{
    workers++;const worker=new Worker('./compute-worker.mjs',{type:'module'});
    worker.addEventListener('message',e=>{if(e.data.type==='result'&&e.data.worker===true){workerResults++;cpuIntervals.push([e.data.decodeStartedAt,e.data.decodeFinishedAt]);}});return worker;
  }});
  const a=owner(pool,'qualification');const formats=[];
  for(const name of ['small.png','small.jpg','small.webp']){
    const input=await bytes(name),expected=await bytes(`${name}.rgba`);
    const dimensions=(await(await fetch(`fixtures/${name}.dimensions`)).text()).split(',').map(Number);
    await a.decode({bytes:input,source:`fixture:${name}`,maxDimension:5},async result=>{
      check(result.width===dimensions[0]&&result.height===dimensions[1],`${name} dimensions`);
      const actual=await hash(result.rgba);check(actual===await hash(expected),`${name} native pixels`);
      formats.push({name,width:result.width,height:result.height,sha256:actual,timings:result.timings});
    });check(input.byteLength>0,'caller input detached');
  }
  await refused(a.decode({bytes:new Uint8Array([1,2,3]),source:'bad',maxDimension:1},async()=>{}),/decode/);
  await refused(a.decode({bytes:await bytes('oversized.png'),source:'large-source',maxDimension:1},async()=>{}),/decode/);
  const work=await bytes('work.png'),expectedWork=await hash(await bytes('work.png.rgba'));
  let cpuMs=0,iterations=0,maxPageGapMs=0,last=performance.now();
  const heartbeat=setInterval(()=>{const now=performance.now();maxPageGapMs=Math.max(maxPageGapMs,now-last);last=now;ticks++;heartbeatTimes.push(performance.timeOrigin+now);},5);
  const started=performance.now();let ticksDuringWorker=0;const startupMs=[],inputMs=[],stagingMs=[],allocationMs=[],headerMs=[],maxChunkMs=[];
  try{
    while(cpuMs<1000){
      const before=ticks;
      await a.decode({bytes:work,source:`work:${iterations}`,maxDimension:2048},async result=>{
        check(await hash(result.rgba)===expectedWork,'real workload pixels');
        cpuMs+=result.timings.decodeMs;startupMs.push(result.timings.resultStart-result.timings.dispatch-result.timings.decodeMs);
        inputMs.push(result.timings.inputMs);stagingMs.push(result.timings.stagingMs);allocationMs.push(result.timings.allocationMs);headerMs.push(result.timings.headerMs);maxChunkMs.push(result.timings.maxChunkMs);
      });
      ticksDuringWorker+=ticks-before;iterations++;check(iterations<200,'insufficient real decode CPU');
    }
  }finally{clearInterval(heartbeat);}
  const workEnded=performance.now();
  const ticksDuringDecode=heartbeatTimes.filter(t=>cpuIntervals.some(([start,end])=>t>start&&t<end)).length;
  check(cpuMs>=1000&&ticksDuringDecode>20,'page tasks did not continue during real decode');
  const abort=new AbortController(),b=owner(pool,'cancel',abort.signal);const count=workers;
  const canceled=b.decode({bytes:work,source:'cancel-real',maxDimension:2048},async()=>{throw Error('canceled result delivered');});
  while(workers===count)await sleep(1);abort.abort();await refused(canceled,/closed/);
  await a.decode({bytes:await bytes('small.png'),source:'after-cancel',maxDimension:5},async()=>{});
  for(const fixture of ['trap','hang']){
    const failPool=createComputeDomains({workerFactory:()=>new Worker(`./${fixture}-fixture.mjs`,{type:'module'}),timeoutMs:fixture==='hang'?100:10000});
    await refused(owner(failPool,fixture).decode({bytes:new Uint8Array([1]),source:fixture,maxDimension:1},async()=>{}),fixture==='hang'?/timeout/:/worker/);
    check(failPool.snapshot().jobs===0,`${fixture} pending leak`);
  }
  const memory=await new Promise((resolve,reject)=>{
    const worker=new Worker('./memory-fixture.mjs',{type:'module'});const timer=setTimeout(()=>{worker.terminate();reject(Error('memory probe timeout'));},10000);
    worker.onmessage=e=>{clearTimeout(timer);worker.terminate();resolve(e.data);};worker.onerror=()=>{clearTimeout(timer);worker.terminate();reject(Error('memory probe error'));};
  });
  check(memory.nonshared&&memory.growthRefused,'actual memory maximum not enforced');
  check(pool.snapshot().jobs===0&&pool.snapshot().leases===0,'pending worker leakage');
  const idleWorkers=workers;await sleep(100);check(workers===idleWorkers,'idle worker spawn');a.close();
  return {formats,workers,workerResults,qualificationMs:performance.now()-qualificationStarted,work:{encodedBytes:work.length,iterations,cpuMs,wallMs:workEnded-started,ticksDuringWorker,ticksDuringDecode,maxPageGapMs,startupMs,inputMs,stagingMs,allocationMs,headerMs,maxChunkMs},memory,final:pool.snapshot(),scope:'standalone page; not 100 apps, OS RSS, 120 Hz or Notes integration'};
}
