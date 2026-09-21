// Private, device-owned outbox. No document/presence path imports this module.
const MAX_ENTRIES=256,MAX_BYTES=4*1024*1024,MAX_PENDING=32;
const copy=value=>structuredClone(value);
const canonical=value=>JSON.stringify(value,(_,v)=>v&&typeof v==='object'&&!Array.isArray(v)?Object.fromEntries(Object.keys(v).sort().map(k=>[k,v[k]])):v);
const equal=(a,b)=>canonical(a)===canonical(b);
const uuid=value=>typeof value==='string'&&/^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/.test(value)&&value!=='00000000-0000-0000-0000-000000000000';
const terminal=update=>['succeeded','failed','cancelled'].includes(update?.state);
const encoder=new TextEncoder();
export const actionError=(code,stage,message,retryable=false)=>Object.assign(Error(message),{code,stage,retryable});
// Public cancellation may settle early; callers must keep the underlying serial
// operation ordered until any storage transaction already started has settled.
export function actionControl({signal,parentSignal,deadlineAtMs,isCurrent}={},clock={},stage='discovery'){
  const now=clock.now??Date.now,set=clock.setTimeout??globalThis.setTimeout,clear=clock.clearTimeout??globalThis.clearTimeout;
  const controller=new AbortController();let timer;
  const abort=reason=>{if(timer!==undefined)clear(timer);controller.abort(reason);};
  const cancelled=()=>abort(actionError('cancelled',stage,'Action attempt cancelled'));
  const sources=[signal,parentSignal].filter(Boolean);
  for(const source of sources){source.addEventListener('abort',cancelled,{once:true});if(source.aborted)cancelled();}
  const expire=()=>abort(actionError('deadline-exceeded',stage,stage==='discovery'?'Desktop service unavailable; command was not sent':'Action outcome unknown; original invocation retained'));
  if(deadlineAtMs!==undefined&&!controller.signal.aborted){if(now()>=deadlineAtMs)expire();else timer=set(expire,deadlineAtMs-now());}
  const check=()=>{if(controller.signal.aborted)throw controller.signal.reason;if(deadlineAtMs!==undefined&&now()>=deadlineAtMs){expire();throw controller.signal.reason;}if(isCurrent&&!isCurrent()){cancelled();throw controller.signal.reason;}};
  return {signal:controller.signal,deadlineAtMs,isCurrent,check,setStage:next=>{stage=next;},
    wait(task){return new Promise((resolve,reject)=>{const stop=()=>reject(controller.signal.reason);controller.signal.addEventListener('abort',stop,{once:true});Promise.resolve(task).then(resolve,reject).finally(()=>controller.signal.removeEventListener('abort',stop));if(controller.signal.aborted)stop();});},
    dispose(){if(timer!==undefined)clear(timer);for(const source of sources)source.removeEventListener('abort',cancelled);},
  };
}
export async function createActionClient({context,load,commit,request,clock={}}){
  let disk=await load()??{version:1,entries:[]},serial=Promise.resolve(),pending=0,closed=false,recoveryCursor=0,capability={state:'unavailable',reason:'not-discovered'},capabilityRevision=0;
  let recovery;const catalogueRefreshes=new Map();
  const listeners=new Set(),lifetime=new AbortController(),admissions=new Map();
  if(disk.version!==1||!Array.isArray(disk.entries)||disk.entries.length>MAX_ENTRIES||encoder.encode(JSON.stringify(disk)).length>MAX_BYTES)throw Error('Unsupported private action outbox');
  const identities=new Set();
  for(const row of disk.entries){
    if(!uuid(row.invocation)||identities.has(row.invocation)||!uuid(row.host)||row.intent?.type!=='invoke_bound'||row.intent.invocation!==row.invocation||row.intent.host!==row.host||
      !['room','sender','senderRoot','target','targetRoot','certificate'].every(key=>typeof row.binding?.[key]==='string'&&row.binding[key].length>0)||
      !['app_peer','app_id','record_id','resource','action'].every(key=>typeof row.intent[key]==='string')||
      typeof row.terminal!=='boolean'||typeof row.cancellationRequested!=='boolean'||
      (row.update&&row.update.invocation!==row.invocation)||
      (row.terminal&&(!terminal(row.update)||row.state!==row.update.state)))throw Error('Corrupt private action outbox');
    identities.add(row.invocation);
  }
  // Reload never assumes the crash happened before dispatch. Only status may recover it.
  disk=copy(disk);
  for(const row of disk.entries)if(!row.terminal){row.state='unknown';row.reason='recovered';}
  const snapshot=()=>copy({version:1,capability,entries:disk.entries});
  const notify=()=>{for(const listener of listeners){try{listener(snapshot());}catch{}}};
  function enqueue(operation,options={},stage='discovery'){
    if(closed)return Promise.reject(Error('Action client closed'));if(pending>=MAX_PENDING)return Promise.reject(Error('Action request overload'));
    const control=actionControl({...options,parentSignal:lifetime.signal},clock,stage);pending++;
    const task=serial.catch(()=>{}).then(()=>{control.check();return operation(control);});
    serial=task.finally(()=>{pending--;control.dispose();});serial.catch(()=>{});
    return control.wait(task);
  }
  async function save(next,control,options){if(equal(next,disk))return;if(encoder.encode(JSON.stringify(next)).length>MAX_BYTES)throw Error('Action outbox capacity exceeded');try{control?.check();await commit(copy(next),control,options);}catch(error){capabilityRevision++;capability={state:'unavailable',reason:'storage-failed'};notify();throw error;}disk=next;notify();}
  async function discover(control,scope,revision=scope?undefined:++capabilityRevision){
    const revoke=()=>{if(!scope&&revision===capabilityRevision)capabilityRevision++;};
    if(!scope){control.signal.addEventListener('abort',revoke,{once:true});if(control.signal.aborted)revoke();}
    try{
    control.check();const binding=copy(context());const response=await request(scope?{type:'catalogue_refresh',scope:copy(scope)}:{type:'catalogue_refresh'},control);control.check();
    if(response.type!=='broker_catalogue'||!uuid(response.host)||response.catalogue?.version!==1||!Array.isArray(response.catalogue.surfaces)){
      const code=response.type==='rejected'?response.code:'invalid-catalogue';
      const message=code==='action_response_exceeds_limits'?(scope?'Desktop service returned too much data; command was not sent':'Action palette returned too much data'):code??'Broker unavailable';
      throw actionError(code,'discovery',message,['Offline','Backpressure','AdmissionUnavailable'].includes(code));
    }
    if(!equal(binding,context()))throw Error('Action identity changed during discovery');
    const current={state:'ready',host:response.host,binding,catalogue:copy(response.catalogue)};
    if(!scope&&revision===capabilityRevision&&!closed){capability=current;notify();}return copy(current);
  }catch(error){if(!scope&&revision===capabilityRevision&&!closed){capability={state:'unavailable',reason:String(error.message??error)};notify();}throw error;}
  finally{if(!scope)control.signal.removeEventListener('abort',revoke);}}
  function refreshCatalogue(options={}){
    if(closed)return Promise.reject(Error('Action client closed'));
    let key;try{key=canonical(context());}catch(error){return Promise.reject(error);}
    let entry=catalogueRefreshes.get(key);
    if(!entry||entry.control.signal.aborted){
      const control=actionControl({parentSignal:lifetime.signal},clock,'discovery');
      const promise=control.wait(discover(control)).finally(()=>{control.dispose();if(catalogueRefreshes.get(key)===entry)catalogueRefreshes.delete(key);});
      entry={control,promise};catalogueRefreshes.set(key,entry);promise.catch(()=>{});
    }
    const waiter=actionControl({...options,parentSignal:lifetime.signal},clock,'discovery');
    return waiter.wait(entry.promise).finally(()=>waiter.dispose());
  }
  const find=id=>{const row=disk.entries.find(row=>row.invocation===id);if(!row)throw Error('Unknown local invocation');return row;};
  const exactScope=value=>({app_id:value.app_id,record_id:value.record_id,resource:value.resource,action:value.action});
  async function bound(row,control){if(!equal(row.binding,context()))throw Error('Action identity changed');const current=await discover(control,exactScope(row.intent));if(current.host!==row.host)throw Error('Action host changed; original invocation retained');return current;}
  async function observe(row,message,control){
    let response;
    try{control.check();if(!equal(row.binding,context()))throw Error('Action identity changed before request');response=await request(message,control);}catch(error){const next=copy(disk),item=next.entries.find(item=>item.invocation===row.invocation);if(!item.terminal){item.state='unknown';item.reason=String(error.message??error);}await save(next);throw error;}
    return applyObservation(row,response);
  }
  async function applyObservation(row,response){
    if(response.type==='observation'&&(response.host!==row.host||response.invocation!==row.invocation||(response.update&&response.update.invocation!==row.invocation)))throw Error('Action response correlation mismatch');
    const next=copy(disk),item=next.entries.find(item=>item.invocation===row.invocation);
    if(!item.terminal){
      if(response.type==='observation'){
        const update=response.update;
        // Duplicated/older lifecycle responses cannot overwrite newer stream positions.
        if(update&&(terminal(update)||(!item.update?.sequence&&update.state!=='accepted')||!item.update||(update.sequence&&(!item.update.sequence||BigInt(update.sequence)>BigInt(item.update.sequence)))))item.update=copy(update);
        item.terminal=terminal(item.update)&&!!update&&!response.uncertain;
        item.state=response.uncertain||!update?'unknown':item.update?.state??'unknown';
        item.reason=response.uncertain?'broker-uncertain':!item.update?'unknown-outcome':null;
        item.cancellationRequested=!!response.cancellation_requested||item.cancellationRequested;
      }else if(response.type==='cancellation_requested'&&response.invocation===row.invocation){item.cancellationRequested=true;}
      else {item.state='unknown';item.reason=response.code??response.reason??'unexpected-response';}
    }
    // Never expose a new terminal result until the strict durable commit completes.
    await save(next,undefined,item.terminal?{kind:'terminal-observation'}:undefined);return copy(item);
  }
  function intent(input,host){return {type:'invoke_bound',host,invocation:input.invocation,app_peer:String(input.app_peer),app_id:input.app_id,record_id:input.record_id,resource:input.resource,action:input.action,argument:input.argument??null,confirmation:input.confirmation??null,revision:input.revision??null};}
  function resolve(current,value){
    const surfaces=current.catalogue.surfaces.filter(s=>s.live!==false&&(s.cluster_id===undefined||s.cluster_id===current.binding.room)&&String(s.peer_id)===value.app_peer&&s.app_id===value.app_id&&s.record_id===value.record_id);
    const offered=surfaces.flatMap(s=>s.actions??[]).filter(a=>a.resource===value.resource&&a.action===value.action&&(a.revision??null)===value.revision);
    if(offered.length!==1)throw Error('Action catalogue route or revision is stale');
    const declared=offered[0].contract?.confirmation;
    if(declared?.mode==='none'&&value.confirmation!==null)throw Error('Undeclared confirmation');
    if(declared?.mode==='confirm'&&value.confirmation?.kind!=='confirm')throw Error('Action confirmation required');
    if(declared?.mode==='phrase'&&(value.confirmation?.kind!=='phrase'||value.confirmation.phrase!==declared.phrase))throw Error('Action confirmation phrase required');
    if(!['none','confirm','phrase'].includes(declared?.mode))throw Error('Unsupported confirmation mechanism');
    return offered[0];
  }
  async function invoke(input,control,current){
      input.invocation??=crypto.randomUUID();if(!uuid(input.invocation))throw Error('Full invocation UUID required');
      const existing=disk.entries.find(row=>row.invocation===input.invocation);
      if(existing){control.setStage('observation');if(!equal(existing.intent,intent(input,existing.host)))throw Error('Invocation intent changed');await bound(existing,control);return observe(existing,{type:'status',invocation:existing.invocation},control);}
      if(disk.entries.length>=MAX_ENTRIES)throw Error('Action outbox capacity exceeded; retained work is never evicted');
      current??=await discover(control,exactScope(input));
      control.check();if(!equal(current.binding,context()))throw Error('Action identity changed during selection');
      const value=intent(input,current.host),descriptor=resolve(current,value);
      if(input.expected&&(!equal(input.expected.binding,current.binding)||input.expected.host!==current.host||!equal(input.expected.descriptor,descriptor)))throw Error('Action binding or catalogue changed');
      const row={invocation:input.invocation,host:current.host,binding:current.binding,intent:value,state:'unknown',reason:'may-have-dispatched',update:null,terminal:false,cancellationRequested:false};
      control.check();
      control.setStage('admission');let settle;const reservation={promise:new Promise(resolve=>settle=resolve)};admissions.set(input.invocation,reservation);
      try{await save({...disk,entries:[...disk.entries,row]},control);}catch(error){reservation.error=error;throw error;}finally{admissions.delete(input.invocation);settle(reservation.error);}
      control.check();if(!equal(row.binding,context()))throw Error('Action identity changed after commit');
      control.setStage('observation');return observe(row,value,control);
  }
  return {version:1,getBinding:()=>copy(context()),getSnapshot:snapshot,subscribe(listener){listeners.add(listener);return()=>listeners.delete(listener);},
    invalidate(reason){capabilityRevision++;capability={state:'unavailable',reason};notify();},
    getInvocationStage:id=>disk.entries.some(row=>row.invocation===id)?'retained':admissions.has(id)?'admitting':'none',
    settleAdmission:id=>admissions.get(id)?.promise??Promise.resolve(),
    catalogue:options=>enqueue(discover,options),
    refreshCatalogue,
    invoke:(input,options)=>{input=copy(input);return enqueue(control=>invoke(input,control),options);},
    // Resolve a new command synchronously inside the fresh discovery turn. This
    // avoids a second round trip without caching authority across user gestures.
    invokeSelected:(select,options={})=>enqueue(async control=>{
      const current=await discover(control,options.discoveryScope);
      const input=copy(select(copy(current)));
      return invoke(input,control,current);
    },options),
    status:(id,options)=>enqueue(async control=>{const row=find(id);await bound(row,control);return observe(row,{type:'status',invocation:id},control);},options,'observation'),
    cancel:(id,options)=>enqueue(async control=>{const row=find(id);await bound(row,control);return observe(row,{type:'cancel',invocation:id},control);},options,'observation'),
    recover:()=>{
      if(recovery)return recovery;
      recovery=(async()=>{
        const rows=await enqueue(()=>disk.entries.filter(row=>!row.terminal).map(row=>row.invocation));
        for(let count=0;count<Math.min(16,rows.length);count++){
          const id=rows[(recoveryCursor+count)%rows.length];
          // Background network waits must not occupy the mutation lane used
          // by a new user command. Only applying the result is serialized.
          const row=copy(find(id));if(row.terminal)continue;
          const control=actionControl({parentSignal:lifetime.signal},clock,'observation');
          try{
            const current=await control.wait(discover(control,exactScope(row.intent)));
            if(!equal(row.binding,current.binding)||row.host!==current.host){
              await enqueue(async()=>{const next=copy(disk),item=next.entries.find(item=>item.invocation===id);if(!item.terminal){item.state='unknown';item.reason=!equal(row.binding,current.binding)?'identity-changed':'host-changed';await save(next);}});
              continue;
            }
            if(find(id).terminal)continue;
            let response;
            try{control.check();if(!equal(row.binding,context()))throw Error('Action identity changed before request');response=await control.wait(request({type:'status',invocation:id},control));}
            catch(error){
              if(!closed)await enqueue(async()=>{const next=copy(disk),item=next.entries.find(item=>item.invocation===id);if(!item.terminal){item.state='unknown';item.reason=String(error.message??error);await save(next);}});
              continue;
            }
            await enqueue(()=>{control.check();if(!equal(row.binding,context()))throw Error('Action identity changed during observation');return applyObservation(row,response);});
          }finally{control.dispose();}
        }
        recoveryCursor=rows.length?(recoveryCursor+16)%rows.length:0;return snapshot();
      })().finally(()=>{recovery=undefined;});
      return recovery;
    },
    async close(){closed=true;lifetime.abort();await serial.catch(()=>{});listeners.clear();},
  };
}
