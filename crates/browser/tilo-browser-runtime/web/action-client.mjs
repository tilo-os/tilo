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
const RESULT_BYTES=256*1024,ACK_BYTES=64*1024;
const size=value=>encoder.encode(JSON.stringify(value)).length;
const validConsumer=value=>value?.kind==='interactive'&&uuid(value.parent)&&typeof value.slot==='string'&&value.slot.length>0&&value.slot.length<=128&&equal(Object.keys(value).sort(),['kind','parent','slot']);
const validUpdate=(update,id)=>{
  if(!update||update.invocation!==id||size(update)>RESULT_BYTES)return false;
  // Native ActionOutput omits an empty `resources` list (serde skip_serializing_if).
  if(update.state==='succeeded')return !!update.output&&typeof update.output==='object'&&(update.output.resources===undefined||(Array.isArray(update.output.resources)&&update.output.resources.every(v=>typeof v==='string')));
  if(update.state==='failed')return typeof update.error?.code==='string'&&typeof update.error.message==='string'&&typeof update.error.retryable==='boolean';
  if(update.state==='cancelled')return update.message===null||typeof update.message==='string';
  if(update.state==='accepted')return true;
  if(!['progress','item'].includes(update.state)||typeof update.sequence!=='string'||!/^[1-9][0-9]{0,19}$/.test(update.sequence)||BigInt(update.sequence)>18446744073709551615n)return false;
  return update.state==='item'||typeof update.phase==='string';
};
const chargedBytes=value=>size(value)+value.entries.reduce((total,row)=>total+(row.ticket?RESULT_BYTES-size(row.update)+ACK_BYTES-size(row.consumerAcknowledgement):0),0);
export async function createActionClient({context,load,readLegacyImage,commit,reconcile,request,clock={},version=1}){
  const loaded=await load();
  let disk=loaded??{version:1,entries:[]},serial=Promise.resolve(),pending=0,closed=false,recoveryCursor=0,capability={state:'unavailable',reason:'not-discovered'},capabilityRevision=0;
  let recovery;const catalogueRefreshes=new Map();
  const listeners=new Set(),lifetime=new AbortController(),admissions=new Map();
  const lastAbsent=new Set(),liveCreated=new Set(),cancelled=new Set(),knownRecorded=new Set(),rawRequests=new Set(),drives=new Map(),queuedInvocations=new Map();
  let ambiguous;const consuming=new Set();
  // Tickets this live owner just admitted and has never sent: the exact discovery they were
  // resolved against, kept briefly so the first drive dispatches at once (no second discovery,
  // no status probe of an operation no one has seen). Any later drive takes the normal path.
  const UNSENT_FRESH_MS=5000;const unsent=new Map();
  // Background recovery observes a retained row again after 1 s, doubling while
  // its observation stays unchanged, up to 16 s; a change, a new row or a live
  // drive of that row resets it. A scoped discovery serves every row of its scope
  // for 30 s (the host identity check), so a status probe is not a catalogue refresh.
  const RECOVERY_BASE_MS=1000,RECOVERY_MAX_MS=16000,RECOVERY_DISCOVERY_MS=30000;const recoveryDue=new Map(),recoveryDiscovery=new Map();
  // A failed admission owns only its live row, not other accepted same-ID
  // queue references. Public abort does not release those raw references.
  const releaseUnownedCancellation=id=>{if(!queuedInvocations.has(id)&&!liveCreated.has(id)&&!disk.entries.some(row=>row.invocation===id))cancelled.delete(id);};
  const healthy=()=>{if(ambiguous)throw actionError('storage-uncertain','admission','Action storage requires reconciliation');};
  async function reconcileStorage(){
    if(!ambiguous)return;
    if(!reconcile)throw actionError('storage-uncertain','admission','Action storage requires reconciliation');
    const held=ambiguous,result=await reconcile();
    if(held!==ambiguous)throw Error('Action storage reconciliation changed');
    if(result==='committed'){disk=held.next;ambiguous=undefined;notify();}
    else if(result==='aborted'){ambiguous=undefined;for(const id of liveCreated)if(!disk.entries.some(row=>row.invocation===id)){liveCreated.delete(id);releaseUnownedCancellation(id);}}
    else throw Error('Action storage reconciliation unresolved');
  }
  function rpc(message,control){
    healthy();control.check();
    const task=Promise.resolve().then(()=>{healthy();control.check();if(message.type==='invoke_bound'&&cancelled.has(message.invocation))throw Error('Original action cancellation requested');return request(message,control);});
    rawRequests.add(task);void task.then(()=>rawRequests.delete(task),()=>rawRequests.delete(task));return task;
  }
  if(![1,2].includes(version)||(disk.version!==1&&(version!==2||disk.version!==2||disk.writerFloor!==2||!uuid(disk.namespace)||(disk.legacyImage!==null&&typeof disk.legacyImage!=='string')))||!Array.isArray(disk.entries)||disk.entries.length>MAX_ENTRIES||size(disk)>MAX_BYTES)throw Error('Unsupported private action outbox');
  const identities=new Set();
  for(const row of disk.entries){
    if(!uuid(row.invocation)||identities.has(row.invocation)||!uuid(row.host)||row.intent?.type!=='invoke_bound'||row.intent.invocation!==row.invocation||row.intent.host!==row.host||
      !['room','sender','senderRoot','target','targetRoot','certificate'].every(key=>typeof row.binding?.[key]==='string'&&row.binding[key].length>0)||
      !['app_peer','app_id','record_id','resource','action'].every(key=>typeof row.intent[key]==='string')||
      typeof row.terminal!=='boolean'||typeof row.cancellationRequested!=='boolean'||
      (row.update&&row.update.invocation!==row.invocation)||
      (row.terminal&&(!terminal(row.update)||row.state!==row.update.state)))throw Error('Corrupt private action outbox');
    if(row.ticket&&(!equal(row.ticket,{version:2,namespace:disk.namespace,operation:row.invocation,binding:row.binding,consumer:row.ticket.consumer})||!validConsumer(row.ticket.consumer)||typeof row.businessCancellationRequested!=='boolean'||typeof row.knownRecorded!=='boolean'||(row.update&&!validUpdate(row.update,row.invocation))))throw Error('Corrupt private action ticket');
    identities.add(row.invocation);
  }
  if(version===2&&disk.version===1){
    const next={version:2,writerFloor:2,namespace:crypto.randomUUID(),legacyImage:loaded!==undefined&&loaded!==null?(readLegacyImage?.()??JSON.stringify(disk)):null,entries:copy(disk.entries)};
    if(chargedBytes(next)>MAX_BYTES)throw Error('Action outbox promotion capacity exceeded');
    try{await commit(copy(next));}catch(error){if(error?.name!=='StorageWorkerError'||!reconcile||await reconcile()!=='committed')throw error;}
    disk=next;
  }
  if(disk.version===2&&chargedBytes(disk)>MAX_BYTES)throw Error('Action outbox reserved capacity exceeded');
  // Reload never assumes the crash happened before dispatch. Only status may recover it.
  disk=copy(disk);
  for(const row of disk.entries){if(row.businessCancellationRequested)cancelled.add(row.invocation);if(!row.terminal){row.state='unknown';row.reason='recovered';}}
  const snapshot=()=>copy({version:1,capability,entries:disk.entries});
  const notify=()=>{for(const listener of listeners){try{listener(snapshot());}catch{}}};
  function enqueue(operation,options={},stage='discovery',queuedId){
    if(closed)return Promise.reject(Error('Action client closed'));if(pending>=MAX_PENDING)return Promise.reject(Error('Action request overload'));
    const control=actionControl({...options,parentSignal:lifetime.signal},clock,stage);pending++;
    if(queuedId)queuedInvocations.set(queuedId,(queuedInvocations.get(queuedId)??0)+1);
    const task=serial.catch(()=>{}).then(async()=>{control.check();await reconcileStorage();control.check();return operation(control);});
    serial=task.finally(()=>{
      pending--;control.dispose();
      if(queuedId){const remaining=queuedInvocations.get(queuedId)-1;if(remaining)queuedInvocations.set(queuedId,remaining);else{queuedInvocations.delete(queuedId);releaseUnownedCancellation(queuedId);}}
    });serial.catch(()=>{});
    return control.wait(task);
  }
  async function save(next,control,options){healthy();if(equal(next,disk))return;if((next.version===2?chargedBytes(next):size(next))>MAX_BYTES)throw Error('Action outbox capacity exceeded');try{control?.check();await commit(copy(next),control,options);}catch(error){if(error?.name==='StorageWorkerError')ambiguous={next};capabilityRevision++;capability={state:'unavailable',reason:'storage-failed'};notify();throw error;}disk=next;notify();}
  async function discover(control,scope,revision=scope?undefined:++capabilityRevision){
    const revoke=()=>{if(!scope&&revision===capabilityRevision)capabilityRevision++;};
    if(!scope){control.signal.addEventListener('abort',revoke,{once:true});if(control.signal.aborted)revoke();}
    try{
    control.check();const binding=copy(context());const response=await rpc(scope?{type:'catalogue_refresh',scope:copy(scope)}:{type:'catalogue_refresh'},control);control.check();healthy();
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
    try{healthy();}catch(error){return Promise.reject(error);}
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
    try{control.check();if(!equal(row.binding,context()))throw Error('Action identity changed before request');response=await rpc(message,control);if(closed)throw Error('Action client closed');healthy();if(!equal(row.binding,context()))throw Error('Action identity changed during observation');}catch(error){if(closed)throw error;const next=copy(disk),item=next.entries.find(item=>item.invocation===row.invocation);if(item&&!item.terminal){item.state='unknown';item.reason=String(error.message??error);}await save(next);throw error;}
    return applyObservation(row,response);
  }
  // The same observation with its network wait OUTSIDE the serialized storage
  // lane: a slow or lost reply for one operation (an agent spawn whose host
  // never answers) must not hold every later command (a desktop open) behind
  // it. Only the durable apply of the answer, or of the failure, is serialized.
  async function observeOff(row,message,control){
    let response;
    try{control.check();if(!equal(row.binding,context()))throw Error('Action identity changed before request');response=await control.wait(rpc(message,control));if(closed)throw Error('Action client closed');healthy();if(!equal(row.binding,context()))throw Error('Action identity changed during observation');}
    catch(error){if(closed)throw error;await enqueue(async()=>{const next=copy(disk),item=next.entries.find(item=>item.invocation===row.invocation);if(item&&!item.terminal){item.state='unknown';item.reason=String(error.message??error);}await save(next);});throw error;}
    return enqueue(()=>applyObservation(row,response));
  }
  // Run `operation` with its own control, outside the storage lane, after the
  // lane settled what was queued before it (for example the ticket's admission).
  function offLane(operation,options={},stage='observation'){
    if(closed)return Promise.reject(Error('Action client closed'));
    const control=actionControl({...options,parentSignal:lifetime.signal},clock,stage);
    const task=(async()=>{await enqueue(()=>{},options,stage);control.check();await reconcileStorage();return operation(control);})();
    const waited=control.wait(task);void waited.then(()=>control.dispose(),()=>control.dispose());return waited;
  }
  async function applyObservation(row,response){
    const retained=disk.entries.find(item=>item.invocation===row.invocation);
    if(!retained||!equal(retained.ticket,row.ticket))throw Error('Original action was removed');
    if(row.ticket&&response.type==='observation'&&(typeof response.uncertain!=='boolean'||typeof response.cancellation_requested!=='boolean'||(response.update!==null&&!validUpdate(response.update,row.invocation))))throw Error('Invalid ticket action observation');
    if(response.type==='observation'&&(response.host!==row.host||response.invocation!==row.invocation||(response.update&&response.update.invocation!==row.invocation)))throw Error('Action response correlation mismatch');
    lastAbsent.delete(row.invocation);
    if(response.type==='observation'&&response.update===null&&response.uncertain===false&&response.cancellation_requested===false)lastAbsent.add(row.invocation);
    if(response.type==='observation'&&response.update)knownRecorded.add(row.invocation);
    const next=copy(disk),item=next.entries.find(item=>item.invocation===row.invocation);
    if(!item.terminal){
      if(response.type==='observation'){
        const update=response.update;
        if(item.ticket&&update)item.knownRecorded=true;
        // Duplicated/older lifecycle responses cannot overwrite newer stream positions.
        if(update&&(terminal(update)||(!item.update?.sequence&&update.state!=='accepted')||!item.update||(update.sequence&&(!item.update.sequence||BigInt(update.sequence)>BigInt(item.update.sequence)))))item.update=copy(update);
        item.terminal=terminal(item.update)&&!!update&&!response.uncertain&&(!item.ticket||terminal(update));
        item.state=response.uncertain||!update?'unknown':item.update?.state??'unknown';
        item.reason=response.uncertain?'broker-uncertain':!item.update?'unknown-outcome':null;
        if(item.ticket&&!item.terminal&&terminal(item.update)){item.state='unknown';if(!response.uncertain)item.reason='terminal-unconfirmed';}
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
  async function invoke(input,control,current,ticket){
      input.invocation??=crypto.randomUUID();if(!uuid(input.invocation))throw Error('Full invocation UUID required');
      const existing=disk.entries.find(row=>row.invocation===input.invocation);
      if(existing){control.setStage('observation');if(!equal(existing.intent,intent(input,existing.host)))throw Error('Invocation intent changed');await bound(existing,control);return observe(existing,{type:'status',invocation:existing.invocation},control);}
      if(cancelled.has(input.invocation))throw Error('Original action cancellation requested');
      if(disk.entries.length>=MAX_ENTRIES)throw Error('Action outbox capacity exceeded; retained work is never evicted');
      current??=await discover(control,exactScope(input));
      control.check();if(!equal(current.binding,context()))throw Error('Action identity changed during selection');
      const value=intent(input,current.host),descriptor=resolve(current,value);
      if(input.expected&&(!equal(input.expected.binding,current.binding)||input.expected.host!==current.host||!equal(input.expected.descriptor,descriptor)))throw Error('Action binding or catalogue changed');
      if(cancelled.has(input.invocation))throw Error('Original action cancellation requested');
      const row={invocation:input.invocation,host:current.host,binding:current.binding,intent:value,state:'unknown',reason:'may-have-dispatched',update:null,terminal:false,cancellationRequested:false,...(ticket?{ticket:copy(ticket),businessCancellationRequested:false,knownRecorded:false,consumerAcknowledgement:null}:{})};
      control.check();
      control.setStage('admission');let settle;const reservation={promise:new Promise(resolve=>settle=resolve)};admissions.set(input.invocation,reservation);
      liveCreated.add(input.invocation);
      try{await save({...disk,entries:[...disk.entries,row]},control);}catch(error){if(!ambiguous){liveCreated.delete(input.invocation);releaseUnownedCancellation(input.invocation);}reservation.error=error;throw error;}finally{admissions.delete(input.invocation);settle(reservation.error);}
      if(ticket){unsent.set(input.invocation,{current:copy(current),at:(clock.now??Date.now)()});while(unsent.size>MAX_PENDING)unsent.delete(unsent.keys().next().value);return copy(ticket);}
      control.check();if(!equal(row.binding,context()))throw Error('Action identity changed after commit');
      if(cancelled.has(row.invocation))throw Error('Original action cancellation requested');
      control.setStage('observation');return observe(row,value,control);
  }
  function driveRetained(id,options={},ticket){
    if(drives.has(id)){
      // Joining an authorized ticket attempt still requires this caller's exact
      // public contract; a raw legacy ID must not inherit its scoped admission.
      try{if(version===2)ticket?findTicket(ticket):findLegacy(id);}catch(error){return Promise.reject(error);}
      return drives.get(id);
    }
    const task=offLane(async control=>{
      const row=ticket?findTicket(ticket):findLegacy(id);
      // Opt-in (`dispatchFresh`): callers whose command has no user-visible cancel window between
      // admission and dispatch (the desktop command bridge). Others keep the status probe first.
      const fresh=row.ticket&&options.dispatchFresh===true&&unsent.get(id);unsent.delete(id);
      if(fresh&&(clock.now??Date.now)()-fresh.at<UNSENT_FRESH_MS&&liveCreated.has(id)&&!row.update&&!row.terminal&&!knownRecorded.has(id)&&!lastAbsent.has(id)&&row.intent.confirmation===null){
        // First dispatch of this owner's own unsent ticket, against the discovery it was admitted with.
        if(!equal(row.binding,context())||!equal(fresh.current.binding,row.binding)||fresh.current.host!==row.host)throw Error('Action identity changed');
        const descriptor=resolve(fresh.current,row.intent);
        if(descriptor.contract?.confirmation?.mode!=='none')throw Error('Original action requires fresh confirmation');
        control.check();healthy();if(cancelled.has(id))throw Error('Original action cancellation requested');
        return observeOff(find(id),copy(row.intent),control);
      }
      const current=await bound(row,control);
      const observed=await observeOff(row,{type:'status',invocation:id},control);
      // Only this live owner's new rows have a known cancellation history.
      // Reopened v1 rows remain observation-only; no retained row is deleted.
      if(observed.terminal||(!row.ticket&&!liveCreated.has(id))||cancelled.has(id)||knownRecorded.has(id)||observed.knownRecorded||observed.businessCancellationRequested||observed.update||observed.cancellationRequested)return observed;
      if(!lastAbsent.has(id))return observed;
      // The broker catalogue publishes no descriptor revisions. A ticket this
      // live owner admitted against the current exact descriptor may make its
      // first dispatch like a v1 invocation; after reopen, only a revisioned
      // descriptor can re-target the original operation.
      if((!row.intent.revision&&!(row.ticket&&liveCreated.has(id)))||row.intent.confirmation!==null)throw Error('Original action requires a current exact target and fresh confirmation');
      const descriptor=resolve(current,row.intent);
      if(descriptor.contract?.confirmation?.mode!=='none')throw Error('Original action requires fresh confirmation');
      control.check();healthy();if(cancelled.has(id))throw Error('Original action cancellation requested');
      return observeOff(find(id),copy(row.intent),control);
    },options,'observation');
    drives.set(id,task);const settled=()=>{drives.delete(id);recoveryDue.delete(id);};void task.then(settled,settled);return task;
  }
  const findLegacy=id=>{const row=find(id);if(row.ticket)throw Error('A retained scoped ticket is required');return row;};
  const findTicket=ticket=>{
    if(disk.version!==2||!ticket||!equal(ticket.binding,context()))throw Error('Action ticket owner changed');
    const row=disk.entries.find(row=>row.invocation===ticket.operation);
    if(!row?.ticket||!equal(row.ticket,ticket))throw Error('Missing or foreign action ticket');return row;
  };
  const certain=row=>row.ticket&&!cancelled.has(row.invocation)&&row.terminal&&row.reason===null&&!row.businessCancellationRequested&&!row.cancellationRequested&&['succeeded','failed'].includes(row.state)&&row.state===row.update?.state&&validUpdate(row.update,row.invocation);
  const ticketSnapshot=()=>copy({version:2,capability,entries:disk.entries.filter(row=>row.ticket)});
  const v2=version===2?{
    version:2,getBinding:()=>copy(context()),getSnapshot:ticketSnapshot,
    subscribe(listener){const adapted=()=>listener(ticketSnapshot());listeners.add(adapted);return()=>listeners.delete(adapted);},
    catalogue:options=>enqueue(discover,options),refreshCatalogue,
    beginNew(input,options={}){
      input=copy(input);const consumer=copy(options.consumer);
      if(Object.hasOwn(input,'invocation'))return Promise.reject(Error('Fresh ticket admission accepts no invocation ID'));
      if(!validConsumer(consumer))return Promise.reject(Error('An interactive intended consumer is required'));
      return offLane(control=>discover(control,exactScope(input)),options,'discovery').then(current=>enqueue(async control=>{
        if(disk.entries.some(row=>row.ticket&&equal(row.ticket.consumer,consumer)))throw Error('Consumer slot already retains its original operation');
        if(!equal(current.binding,context()))throw Error('Action identity changed during discovery');
        const operation=crypto.randomUUID(),ticket={version:2,namespace:disk.namespace,operation,binding:copy(current.binding),consumer};
        return invoke({...input,invocation:operation},control,current,ticket);
      },options));
    },
    // One scoped discovery selects and admits a fresh ticket (the caller's `select` sees that exact
    // catalogue synchronously), so a new command costs one discovery instead of two.
    beginSelected(select,options={}){
      const consumer=copy(options.consumer);
      // Discovery waits outside the storage lane; only the admission is serialized.
      return offLane(control=>discover(control,options.discoveryScope),options,'discovery').then(current=>enqueue(async control=>{
        if(!validConsumer(consumer))throw Error('An interactive intended consumer is required');
        if(disk.entries.some(row=>row.ticket&&equal(row.ticket.consumer,consumer)))throw Error('Consumer slot already retains its original operation');
        if(!equal(current.binding,context()))throw Error('Action identity changed during discovery');
        const input=copy(select(copy(current)));
        if(Object.hasOwn(input,'invocation'))throw Error('Fresh ticket admission accepts no invocation ID');
        if(options.discoveryScope&&!equal(exactScope(input),options.discoveryScope))throw Error('Selected action is outside its discovery scope');
        const operation=crypto.randomUUID(),ticket={version:2,namespace:disk.namespace,operation,binding:copy(current.binding),consumer};
        return invoke({...input,invocation:operation},control,current,ticket);
      },options));
    },
    drive(ticket,options){ticket=copy(ticket);try{findTicket(ticket);}catch(error){return Promise.reject(error);}return driveRetained(ticket.operation,options,ticket);},
    observe(ticket,options){ticket=copy(ticket);return offLane(async control=>{const row=findTicket(ticket);await bound(row,control);return observeOff(row,{type:'status',invocation:row.invocation},control);},options,'observation');},
    requestCancellation(ticket){
      ticket=copy(ticket);try{const row=findTicket(ticket);cancelled.add(row.invocation);}catch(error){return Promise.reject(error);}
      return enqueue(async()=>{const row=findTicket(ticket);const next=copy(disk),item=next.entries.find(item=>item.invocation===row.invocation);item.businessCancellationRequested=true;item.consumerAcknowledgement=null;await save(next);},undefined,'observation')
        .then(()=>offLane(async control=>{const row=findTicket(ticket);await bound(row,control);return observeOff(row,{type:'cancel',invocation:row.invocation},control);},undefined,'observation'));
    },
    acknowledgeTerminal(ticket,acknowledgement){
      ticket=copy(ticket);acknowledgement=copy(acknowledgement);
      return enqueue(async()=>{const row=findTicket(ticket);if(!certain(row))throw Error('A certain uncancelled terminal observation is required');if(!equal(acknowledgement?.consumer,row.ticket.consumer)||!equal(acknowledgement?.update,row.update)||!acknowledgement.continuation||size(acknowledgement)>ACK_BYTES)throw Error('Intended consumer acknowledgement mismatch');const next=copy(disk);next.entries.find(item=>item.invocation===row.invocation).consumerAcknowledgement=acknowledgement;await save(next);});
    },
    async consumeTerminal(ticket){
      ticket=copy(ticket);const row=findTicket(ticket);consuming.add(row.invocation);
      try{await recovery?.catch(()=>{});return await enqueue(async()=>{const current=findTicket(ticket);if(!certain(current)||!current.consumerAcknowledgement||!equal(current.consumerAcknowledgement.update,current.update)||!equal(current.consumerAcknowledgement.consumer,current.ticket.consumer))throw Error('Certain terminal consumer acknowledgement required');const next=copy(disk);next.entries=next.entries.filter(item=>item.invocation!==current.invocation);await save(next);unsent.delete(current.invocation);liveCreated.delete(current.invocation);knownRecorded.delete(current.invocation);lastAbsent.delete(current.invocation);cancelled.delete(current.invocation);});}
      finally{consuming.delete(row.invocation);}
    },
  }:undefined;
  return {version:1,actionsV2:v2,getBinding:()=>copy(context()),getSnapshot:snapshot,subscribe(listener){listeners.add(listener);return()=>listeners.delete(listener);},
    invalidate(reason){capabilityRevision++;capability={state:'unavailable',reason};notify();},
    getInvocationStage:id=>disk.entries.some(row=>row.invocation===id)?'retained':admissions.has(id)?'admitting':'none',
    settleAdmission:id=>admissions.get(id)?.promise??Promise.resolve(),
    catalogue:options=>enqueue(discover,options),
    refreshCatalogue,driveRetained:(id,options)=>driveRetained(id,options),
    invoke:(input,options)=>{input=copy(input);return enqueue(control=>{if(version===2){if(!disk.entries.some(row=>row.invocation===input.invocation&&!row.ticket))throw Error('Selected v2 requires fresh ticket admission; legacy invoke only observes existing rows');findLegacy(input.invocation);}return invoke(input,control);},options,'discovery',uuid(input.invocation)?input.invocation:undefined);},
    // Resolve a new command synchronously inside the fresh discovery turn. This
    // avoids a second round trip without caching authority across user gestures.
    invokeSelected:(select,options={})=>enqueue(async control=>{
      if(version===2)throw Error('Selected v2 requires fresh ticket admission');
      const current=await discover(control,options.discoveryScope);
      const input=copy(select(copy(current)));
      return invoke(input,control,current);
    },options),
    status:(id,options)=>enqueue(async control=>{const row=findLegacy(id);await bound(row,control);return observe(row,{type:'status',invocation:id},control);},options,'observation'),
    cancel:(id,options)=>{if(version===2){try{findLegacy(id);}catch(error){return Promise.reject(error);}}if(disk.entries.some(row=>row.invocation===id)||admissions.has(id)||liveCreated.has(id)||queuedInvocations.has(id))cancelled.add(id);return enqueue(async control=>{const row=findLegacy(id);await bound(row,control);return observe(row,{type:'cancel',invocation:id},control);},options,'observation');},
    // `background: true` (the periodic caller) honors each row's backoff and the
    // 30 s discovery reuse; an explicit recover (a person's Check again) probes now.
    recover:({background=false}={})=>{
      if(recovery)return recovery;
      recovery=(async()=>{
        const at=(clock.now??Date.now)();
        const open=await enqueue(()=>disk.entries.filter(row=>!row.terminal&&!consuming.has(row.invocation)).map(row=>row.invocation));
        for(const id of recoveryDue.keys())if(!open.includes(id))recoveryDue.delete(id);
        // A row a live drive is observing right now needs no background probe.
        const rows=background?open.filter(id=>!drives.has(id)&&(recoveryDue.get(id)?.at??0)<=at):open;
        const discovered=new Map();
        for(let count=0;count<Math.min(16,rows.length);count++){
          const id=rows[(recoveryCursor+count)%rows.length];
          // Background network waits must not occupy the mutation lane used
          // by a new user command. Only applying the result is serialized.
          const stored=disk.entries.find(row=>row.invocation===id);if(!stored||stored.terminal||consuming.has(id))continue;const row=copy(stored);
          const control=actionControl({parentSignal:lifetime.signal},clock,'observation');
          const before=canonical([stored.state,stored.update,stored.reason,stored.cancellationRequested]);
          try{
            const scopeKey=canonical([exactScope(row.intent),context()]);
            const cached=recoveryDiscovery.get(scopeKey);
            let current;
            if(background&&cached&&(clock.now??Date.now)()-cached.at<RECOVERY_DISCOVERY_MS)current=copy(cached.current);
            else{
              if(!discovered.has(scopeKey))discovered.set(scopeKey,control.wait(discover(control,exactScope(row.intent))));
              current=await discovered.get(scopeKey);
              recoveryDiscovery.set(scopeKey,{at:(clock.now??Date.now)(),current:copy(current)});
              while(recoveryDiscovery.size>16)recoveryDiscovery.delete(recoveryDiscovery.keys().next().value);
            }
            if(!equal(row.binding,current.binding)||row.host!==current.host){
              await enqueue(async()=>{const next=copy(disk),item=next.entries.find(item=>item.invocation===id);if(item&&!item.terminal){item.state='unknown';item.reason=!equal(row.binding,current.binding)?'identity-changed':'host-changed';await save(next);}});
              continue;
            }
            if(!disk.entries.some(row=>row.invocation===id&&!row.terminal))continue;
            let response;
            try{control.check();if(!equal(row.binding,context()))throw Error('Action identity changed before request');response=await control.wait(rpc({type:'status',invocation:id},control));}
            catch(error){
              if(!closed)await enqueue(async()=>{const next=copy(disk),item=next.entries.find(item=>item.invocation===id);if(item&&!item.terminal){item.state='unknown';item.reason=String(error.message??error);await save(next);}});
              continue;
            }
            await enqueue(()=>{control.check();if(!equal(row.binding,context()))throw Error('Action identity changed during observation');return applyObservation(row,response);});
          }finally{
            control.dispose();
            const after=disk.entries.find(item=>item.invocation===id);
            const previous=recoveryDue.get(id)?.delay;
            const delay=!after||after.terminal||canonical([after.state,after.update,after.reason,after.cancellationRequested])!==before||previous===undefined?RECOVERY_BASE_MS:Math.min(RECOVERY_MAX_MS,previous*2);
            if(after&&!after.terminal)recoveryDue.set(id,{at:(clock.now??Date.now)()+delay,delay});else recoveryDue.delete(id);
          }
        }
        recoveryCursor=rows.length?(recoveryCursor+16)%rows.length:0;return snapshot();
      })().finally(()=>{recovery=undefined;});
      return recovery;
    },
    async close(){closed=true;lifetime.abort();await serial.catch(()=>{});await Promise.allSettled([...rawRequests]);await recovery?.catch(()=>{});listeners.clear();},
  };
}
