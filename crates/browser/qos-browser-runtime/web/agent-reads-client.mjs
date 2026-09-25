// Disposable reads over the certified Rust carrier. No durable outbox or storage callbacks.
const encoder=new TextEncoder(),MAX_PENDING=32,MAX_BYTES=240*1024;
const copy=value=>structuredClone(value);
const canonical=value=>JSON.stringify(value,(_,v)=>v&&typeof v==='object'&&!Array.isArray(v)?Object.fromEntries(Object.keys(v).sort().map(k=>[k,v[k]])):v);
const equal=(a,b)=>canonical(a)===canonical(b);
const uuid=value=>typeof value==='string'&&/^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/.test(value)&&value!=='00000000-0000-0000-0000-000000000000';
const opaque=value=>typeof value==='string'&&value.length>0&&encoder.encode(value).length<=128;
const fail=message=>Error(`Agent reads unavailable: ${message}`);
function validateRequest(value){
  if(!value||value.version!==1)throw fail('unsupported request');
  const keys={agent_capabilities:['version','kind'],agent_list:['version','kind','cursor'],agent_conversation:['version','kind','agent_id','cursor']}[value.kind];
  if(!keys||!equal(Object.keys(value).sort(),keys.sort()))throw fail('unsupported read');
  if(value.kind!=='agent_capabilities'&&value.cursor!==null&&(!opaque(value.cursor)||!/^[\x00-\x7f]+$/.test(value.cursor)))throw fail('invalid cursor');
  if(value.kind==='agent_conversation'&&(!opaque(value.agent_id)||value.agent_id.trim()!==value.agent_id))throw fail('invalid agent');
}

export function createAgentReadsClient({actions,context,supported,request,clock={}}){
  const now=clock.now??Date.now,set=clock.setTimeout??globalThis.setTimeout,clear=clock.clearTimeout??globalThis.clearTimeout;
  const pending=new Map();let closed=false,invalidated=false,generation=0;
  function getBinding(){
    if(closed||invalidated||!supported())throw fail('carrier disconnected or unsupported');
    const current=actions.getSnapshot().capability,action=copy(context());
    if(current?.state!=='ready'||!uuid(current.host)||!equal(current.binding,action))throw fail('current certified catalogue missing');
    const binding=current.catalogue?.agentReadsV1;
    if(!binding||binding.actionHost!==current.host||binding.agent?.host!==current.host||binding.agent.room!==action.room||binding.agent.device!==action.sender||!opaque(binding.agent.machine))throw fail('provider binding mismatch');
    for(const name of ['spawn','message']){
      const value=binding[name];if(value===null)continue;
      if(!value?.route||!value.descriptor)throw fail('provider route missing');
      const route=value.route;
      const found=(current.catalogue.surfaces??[]).filter(surface=>surface.live!==false&&(surface.cluster_id===undefined||surface.cluster_id===action.room)&&String(surface.peer_id)===route.app_peer&&surface.app_id===route.app_id&&surface.record_id===route.record_id)
        .flatMap(surface=>surface.actions??[]).filter(descriptor=>descriptor.resource===route.resource&&descriptor.action===route.action&&descriptor.revision===route.revision);
      if(found.length!==1||!equal(found[0],value.descriptor))throw fail('ambiguous or stale provider route');
    }
    return copy({...binding,action});
  }
  function invalidate(reason='binding changed'){
    generation++;invalidated=true;
    for(const entry of [...pending.values()])entry.abort(fail(reason));
  }
  const unsubscribe=actions.subscribe(()=>{
    const before=[...pending.values()];
    invalidated=false;
    let binding;try{binding=getBinding();}catch{invalidate();return;}
    for(const entry of before)if(!equal(entry.binding,binding))entry.abort(fail('provider binding changed'));
  });
  function read({invocationId,request:body,signal}={}){
    let binding;
    try{
      binding=getBinding();validateRequest(body);
      if(!uuid(invocationId)||pending.has(invocationId)||pending.size>=MAX_PENDING)throw fail('invalid, duplicate or excessive pending read');
      if(signal?.aborted)throw signal.reason??fail('cancelled');
    }catch(error){return Promise.reject(error);}
    body=copy(body);const epoch=generation,deadline=now()+10000,controller=new AbortController();
    return new Promise((resolve,reject)=>{
      let timer,settled=false;
      const finish=(error,response)=>{
        if(settled)return;settled=true;clear(timer);pending.delete(invocationId);signal?.removeEventListener('abort',abort);
        if(error){controller.abort(error);reject(error);}else resolve(response);
      };
      const abort=()=>finish(signal?.reason??fail('cancelled'));
      pending.set(invocationId,{binding,abort:error=>finish(error)});
      signal?.addEventListener('abort',abort,{once:true});
      timer=set(()=>finish(fail('read deadline exceeded')),10000);
      if(signal?.aborted){abort();return;}
      let task;try{task=request({type:'agent_read_v1',host:binding.actionHost,invocation:invocationId,request:body},{signal:controller.signal,deadlineAtMs:deadline});}catch(error){finish(error);return;}
      Promise.resolve(task).then(result=>{
        if(settled)return;
        try{
          if(epoch!==generation||now()>=deadline||!equal(binding,getBinding()))throw fail('read identity changed');
          const {action,...expected}=binding;
          const response=result?.response;
          if(result?.type!=='agent_read_result_v1'||result.host!==binding.actionHost||result.invocation!==invocationId||!equal(result.binding,expected)||encoder.encode(JSON.stringify(result)).length>MAX_BYTES||response?.version!==1||response.invocationId!==invocationId||!equal(response.binding,binding.agent))throw fail('invalid read response');
          if(response.kind!=='agent_error'&&response.kind!==body.kind)throw fail('wrong read response kind');
          if(response.kind==='agent_conversation'&&response.agentId!==body.agent_id)throw fail('wrong conversation');
          finish(null,copy(response));
        }catch(error){finish(error);}
      },error=>finish(error));
    });
  }
  return {version:1,getBinding,request:read,invalidate,close(){if(closed)return;closed=true;invalidate('closed');unsubscribe();}};
}
