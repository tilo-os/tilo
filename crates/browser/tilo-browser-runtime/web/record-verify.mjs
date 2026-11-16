/** Verify a large record's changes in a worker, so the page's main thread
 * neither rebuilds nor hashes every change of a company's tracker before it
 * can draw again (src/record_verify.rs). The worker runs its own instance of
 * the page's module: the compiled module the page started with when there is
 * one, else the worker compiles it from the same file. It is started on the
 * first record and let go once it has been idle for `idleMs`, so its memory
 * does not stay. It verifies one record at a time, the smallest waiting
 * first, so a note is not held up behind a tracker of thousands of issues.
 * A request answers the record's change hashes, 32 bytes each, or rejects;
 * the page then verifies the record itself. */
export function createRecordVerifier({Worker=globalThis.Worker,url=new URL('./record-verify-worker.mjs',import.meta.url),module=()=>undefined,idleMs=30000,setTimer=setTimeout,clearTimer=clearTimeout}={}){
  let worker=null;
  let next=0;
  let idle=null;
  /** The request the worker holds, and those waiting for it. */
  let running=null;
  const waiting=[];
  function stop(error){
    worker?.terminate?.();
    worker=null;
    const dropped=running?[running,...waiting]:[...waiting];
    running=null;waiting.length=0;
    for(const request of dropped)request.reject(error);
  }
  function send(){
    if(running||!waiting.length)return;
    let smallest=0;
    for(let at=1;at<waiting.length;at++)if(waiting[at].bytes.length<waiting[smallest].bytes.length)smallest=at;
    [running]=waiting.splice(smallest,1);
    const {id,bytes}=running;
    running.bytes=null;
    worker.postMessage({id,bytes},[bytes.buffer]);
  }
  function start(){
    if(worker)return worker;
    worker=new Worker(url,{type:'module',name:'tilo-record-verify'});
    worker.onmessage=({data})=>{
      const request=running;
      if(!request||request.id!==data.id)return;
      running=null;
      if(data.hashes)request.resolve(data.hashes);
      else request.reject(Error(data.error));
      send();
      rest();
    };
    worker.onerror=event=>{event?.preventDefault?.();stop(Error('the record worker failed'));};
    worker.postMessage({module:module()});
    return worker;
  }
  function rest(){
    if(running||waiting.length)return;
    clearTimer(idle);
    idle=setTimer(()=>{if(!running&&!waiting.length)stop(Error('idle'));},idleMs);
  }
  return bytes=>new Promise((resolve,reject)=>{
    clearTimer(idle);
    try{start();}catch(error){reject(error);return;}
    waiting.push({id:next++,bytes,resolve,reject});
    send();
  });
}
