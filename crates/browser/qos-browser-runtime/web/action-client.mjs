// Private, device-owned outbox. No document/presence path imports this module.
const MAX_ENTRIES=256,MAX_BYTES=4*1024*1024,MAX_PENDING=32;
const copy=value=>structuredClone(value);
const canonical=value=>JSON.stringify(value,(_,v)=>v&&typeof v==='object'&&!Array.isArray(v)?Object.fromEntries(Object.keys(v).sort().map(k=>[k,v[k]])):v);
const equal=(a,b)=>canonical(a)===canonical(b);
const uuid=value=>typeof value==='string'&&/^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/.test(value)&&value!=='00000000-0000-0000-0000-000000000000';
const terminal=update=>['succeeded','failed','cancelled'].includes(update?.state);
const encoder=new TextEncoder();
export async function createActionClient({context,load,commit,request}){
  let disk=await load()??{version:1,entries:[]},serial=Promise.resolve(),pending=0,closed=false,recoveryCursor=0,capability={state:'unavailable',reason:'not-discovered'};
  const listeners=new Set();
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
  function enqueue(operation){if(closed)return Promise.reject(Error('Action client closed'));if(pending>=MAX_PENDING)return Promise.reject(Error('Action request overload'));pending++;const task=serial.catch(()=>{}).then(operation);serial=task;return task.finally(()=>{pending--;});}
  async function save(next){if(equal(next,disk))return;if(encoder.encode(JSON.stringify(next)).length>MAX_BYTES)throw Error('Action outbox capacity exceeded');try{await commit(copy(next));}catch(error){capability={state:'unavailable',reason:'storage-failed'};notify();throw error;}disk=next;notify();}
  async function discover(){try{const binding=copy(context());const response=await request({type:'catalogue_refresh'});if(response.type!=='broker_catalogue'||!uuid(response.host)||response.catalogue?.version!==1||!Array.isArray(response.catalogue.surfaces))throw Error(response.code??response.reason??'Broker unavailable');if(!equal(binding,context()))throw Error('Action identity changed during discovery');capability={state:'ready',host:response.host,binding,catalogue:copy(response.catalogue)};notify();return copy(capability);}catch(error){capability={state:'unavailable',reason:String(error.message??error)};notify();throw error;}}
  const find=id=>{const row=disk.entries.find(row=>row.invocation===id);if(!row)throw Error('Unknown local invocation');return row;};
  async function bound(row){if(!equal(row.binding,context()))throw Error('Action identity changed');const current=await discover();if(current.host!==row.host)throw Error('Action host changed; original invocation retained');return current;}
  async function observe(row,message){
    let response;
    try{response=await request(message);}catch(error){const next=copy(disk),item=next.entries.find(item=>item.invocation===row.invocation);if(!item.terminal){item.state='unknown';item.reason=String(error.message??error);}await save(next);throw error;}
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
    await save(next);return copy(item);
  }
  function intent(input,host){return {type:'invoke_bound',host,invocation:input.invocation,app_peer:String(input.app_peer),app_id:input.app_id,record_id:input.record_id,resource:input.resource,action:input.action,argument:input.argument??null,confirmation:input.confirmation??null,revision:input.revision??null};}
  function resolve(current,value){
    const surfaces=current.catalogue.surfaces.filter(s=>String(s.peer_id)===value.app_peer&&s.app_id===value.app_id&&s.record_id===value.record_id);
    const offered=surfaces.flatMap(s=>s.actions??[]).filter(a=>a.resource===value.resource&&a.action===value.action&&(a.revision??null)===value.revision);
    if(offered.length!==1)throw Error('Action catalogue route or revision is stale');
    const declared=offered[0].contract?.confirmation;
    if(declared?.mode==='none'&&value.confirmation!==null)throw Error('Undeclared confirmation');
    if(declared?.mode==='confirm'&&value.confirmation?.kind!=='confirm')throw Error('Action confirmation required');
    if(declared?.mode==='phrase'&&(value.confirmation?.kind!=='phrase'||value.confirmation.phrase!==declared.phrase))throw Error('Action confirmation phrase required');
    if(!['none','confirm','phrase'].includes(declared?.mode))throw Error('Unsupported confirmation mechanism');
  }
  return {version:1,getSnapshot:snapshot,subscribe(listener){listeners.add(listener);return()=>listeners.delete(listener);},
    invalidate(reason){capability={state:'unavailable',reason};notify();},
    catalogue:()=>enqueue(discover),
    invoke:input=>{input=copy(input);return enqueue(async()=>{
      input.invocation??=crypto.randomUUID();if(!uuid(input.invocation))throw Error('Full invocation UUID required');
      const existing=disk.entries.find(row=>row.invocation===input.invocation);
      if(existing){if(!equal(existing.intent,intent(input,existing.host)))throw Error('Invocation intent changed');await bound(existing);return observe(existing,{type:'status',invocation:existing.invocation});}
      if(disk.entries.length>=MAX_ENTRIES)throw Error('Action outbox capacity exceeded; retained work is never evicted');
      const current=await discover(),value=intent(input,current.host);resolve(current,value);
      const row={invocation:input.invocation,host:current.host,binding:current.binding,intent:value,state:'unknown',reason:'may-have-dispatched',update:null,terminal:false,cancellationRequested:false};
      await save({...disk,entries:[...disk.entries,row]});
      if(!equal(row.binding,context()))throw Error('Action identity changed after commit');
      return observe(row,value);
    });},
    status:id=>enqueue(async()=>{const row=find(id);await bound(row);return observe(row,{type:'status',invocation:id});}),
    cancel:id=>enqueue(async()=>{const row=find(id);await bound(row);return observe(row,{type:'cancel',invocation:id});}),
    recover:()=>enqueue(async()=>{
      const current=await discover(),rows=disk.entries.filter(row=>!row.terminal);
      // One recovery turn is bounded; rotate fairly over retained uncertain work.
      for(let count=0;count<Math.min(16,rows.length);count++){
        const row=rows[(recoveryCursor+count)%rows.length];
        if(!equal(row.binding,current.binding)||row.host!==current.host){const next=copy(disk),item=next.entries.find(item=>item.invocation===row.invocation);item.state='unknown';item.reason=!equal(row.binding,current.binding)?'identity-changed':'host-changed';await save(next);continue;}
        try{await observe(row,{type:'status',invocation:row.invocation});}catch{ /* Keep original identity and durable uncertainty. */ }
      }
      recoveryCursor=rows.length?(recoveryCursor+16)%rows.length:0;return snapshot();
    }),
    async close(){closed=true;await serial.catch(()=>{});listeners.clear();},
  };
}
