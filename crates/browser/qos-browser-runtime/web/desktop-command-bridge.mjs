import {actionError} from './action-client.mjs';
const DESKTOP_SCOPE={app_id:'dev.qos.desktop',record_id:'',resource:'desktop:session',action:'command'};
// Shared-widget requests enter the existing certified, durable action client.
// A response updates lifecycle only; admitted signed room state owns rendering.
export function bindDesktopCommands({actions,peek,ack,report=()=>{},onPending=()=>{},onUpdate=()=>{},clock={}}){
  if(actions.version===2)return bindTicketDesktopCommands({actions,peek,ack,report,onPending,onUpdate,clock});
  let closed=false,attempt,current=actions.getSnapshot().entries.filter(row=>row.intent?.app_id==='dev.qos.desktop').at(-1)?.invocation;
  const waiters=new Map(),now=clock.now??Date.now,set=clock.setTimeout??globalThis.setTimeout,clear=clock.clearTimeout??globalThis.clearTimeout;
  const samePending=value=>{const head=peek();return head?.invocation===value.invocation&&head.command===value.command;};
  const retire=value=>{clear(value.timer);clear(value.deadlineTimer);if(attempt===value)attempt=undefined;};
  const rejectWaiter=(id,error)=>{const waiter=waiters.get(id);if(waiter){waiters.delete(id);waiter.reject(error);}};
  const refuse=(value,error)=>{if(closed)return;if(samePending(value.pending))ack(value.pending.invocation);const row={invocation:value.pending.invocation,state:error.code==='cancelled'?'cancelled':'rejected',terminal:true,error:String(error.message??error)};onUpdate(row);report({desktopCommand:row});rejectWaiter(value.pending.invocation,error);retire(value);};
  const control=value=>({signal:value.controller.signal,deadlineAtMs:value.deadline,isCurrent:()=>!closed&&attempt===value&&samePending(value.pending)});
  const check=value=>{if(closed||attempt!==value||value.controller.signal.aborted||!samePending(value.pending))throw actionError('cancelled','discovery','Desktop command attempt cancelled');if(now()>=value.deadline)throw actionError('deadline-exceeded','discovery','Desktop service unavailable; command was not sent');};

  const status=row=>({desktopCommand:{invocation:row.invocation,state:row.state,terminal:row.terminal,
    error:row.update?.error?.message??row.reason??null}});
  const terminalError=row=>Error(row.update?.error?.message??row.reason??`Desktop command ${row.state}`);
  const settle=row=>{const waiter=waiters.get(row?.invocation);if(!waiter||!row.terminal)return;waiters.delete(row.invocation);if(row.state==='succeeded')waiter.resolve();else waiter.reject(terminalError(row));};
  const unsubscribe=actions.subscribe(snapshot=>{
    if(closed)return;
    for(const entry of snapshot.entries)if(entry.intent?.app_id==='dev.qos.desktop')onUpdate(entry);
    const row=snapshot.entries.find(row=>row.invocation===current);
    if(row)report(status(row));
    for(const invocation of waiters.keys())settle(snapshot.entries.find(row=>row.invocation===invocation));
  });
  const retained=actions.getSnapshot().entries.find(row=>row.invocation===current);
  if(retained)report(status(retained));
  async function pump(){
    if(closed)return;
    if(attempt&&!samePending(attempt.pending)){attempt.controller.abort();if(!attempt.busy)refuse(attempt,actionError('cancelled','discovery','Desktop command superseded'));}
    if(attempt?.busy||attempt?.timer!==undefined)return;
    const pending=peek();if(!pending)return;
    const value=attempt??(attempt={pending:{...pending},controller:new AbortController(),deadline:now()+30000,retries:0});
    value.busy=true;current=value.pending.invocation;onPending(value.pending);
    if(value.deadlineTimer===undefined)value.deadlineTimer=set(()=>{value.controller.abort();if(!value.busy)refuse(value,actionError('deadline-exceeded','discovery','Desktop service unavailable; command was not sent'));},Math.max(0,value.deadline-now()));
    report({desktopCommand:{invocation:current,state:'submitting',terminal:false,error:null}});
    try{
      check(value);let row=actions.getSnapshot().entries.find(row=>row.invocation===current);
      if(row){if(row.intent?.app_id!=='dev.qos.desktop'||row.intent?.argument?.command!==value.pending.command)throw Error('Invocation intent changed');row=await actions.status(current,control(value));}
      else {
        const binding=actions.getBinding?.();if(value.binding&&JSON.stringify(value.binding)!==JSON.stringify(binding))throw Error('Action identity changed during discovery');value.binding??=binding;
        row=await actions.invokeSelected(capability=>{
        check(value);
        const matches=capability.catalogue.surfaces.filter(surface=>surface.live!==false&&surface.cluster_id===capability.binding.room&&surface.app_id==='dev.qos.desktop'&&surface.record_id==='')
          .flatMap(surface=>(surface.actions??[]).filter(action=>action.resource==='desktop:session'&&action.action==='command').map(action=>({surface,action})));
        if(matches.length!==1)throw Error('Native desktop command service is unavailable');
        const {surface,action}=matches[0];
        const input={invocation:value.pending.invocation,app_peer:String(surface.peer_id),app_id:surface.app_id,record_id:surface.record_id,
          resource:action.resource,action:action.action,revision:action.revision??null,
          argument:{command:value.pending.command,...(action.params?.some(param=>param.name==='committed_desktop_v2'&&param.ty==='boolean')?{committed_desktop_v2:true}:{})},expected:{host:capability.host,binding:capability.binding,descriptor:action}};
        if(value.input&&JSON.stringify(input)!==JSON.stringify(value.input))throw Error('Action binding or catalogue changed');
        value.input??=input;check(value);return value.input;
        },{...control(value),discoveryScope:DESKTOP_SCOPE});
      }
      if(!closed){if(samePending(value.pending))ack(value.pending.invocation);report(status(row));settle(row);retire(value);}
    }catch(error){
      // A commit reservation is possible dispatch even before the row becomes
      // visible. Wait for it to settle; absence alone must never authorize retry.
      if(actions.getInvocationStage?.(value.pending.invocation)==='admitting'){
        if(!closed)report({desktopCommand:{invocation:value.pending.invocation,state:'unknown',terminal:false,error:'Command admission is still being saved; original invocation retained'}});
        error=await actions.settleAdmission(value.pending.invocation)??error;
      }
      if(!closed){
        const retained=actions.getSnapshot().entries.find(row=>row.invocation===value.pending.invocation);
        if(retained){if(samePending(value.pending))ack(value.pending.invocation);report(status(retained));settle(retained);retire(value);}
        else if(!samePending(value.pending))refuse(value,actionError('cancelled','discovery','Desktop command superseded'));
        else if(!value.controller.signal.aborted&&error.stage==='discovery'&&error.retryable===true&&['transport-timeout','transport-disconnected','Offline','Backpressure','AdmissionUnavailable'].includes(error.code)&&value.retries<2&&now()<value.deadline){
          const delay=[250,1000][value.retries++];value.timer=set(()=>{value.timer=undefined;void pump();},Math.min(delay,value.deadline-now()));
          report({desktopCommand:{invocation:value.pending.invocation,state:'submitting',terminal:false,error:'Desktop service discovery delayed; retrying'}});
        }else {if(now()>=value.deadline||error.retryable===true)error=actionError('discovery-exhausted','discovery','Desktop service unavailable; command was not sent');refuse(value,error);}
      }
    }finally{value.busy=false;}
  }
  return {
    pump,
    cancel(invocation){
      if(closed)return false;
      if(!attempt&&peek()?.invocation===invocation){refuse({pending:{...peek()}},actionError('cancelled','discovery','Desktop command attempt cancelled'));return true;}
      if(attempt?.pending.invocation!==invocation)return false;
      const value=attempt;value.controller.abort();clear(value.timer);value.timer=undefined;
      if(!value.busy)refuse(value,actionError('cancelled','discovery','Desktop command attempt cancelled'));
      return true;
    },
    waitForTerminal(invocation){
      if(closed)return Promise.reject(Error('Desktop command bridge closed'));
      const row=actions.getSnapshot().entries.find(row=>row.invocation===invocation);
      if(row?.terminal)return row.state==='succeeded'?Promise.resolve():Promise.reject(terminalError(row));
      if(waiters.has(invocation))return waiters.get(invocation).promise;
      let resolve,reject;const promise=new Promise((yes,no)=>{resolve=yes;reject=no;});waiters.set(invocation,{promise,resolve,reject});return promise;
    },
    close(){closed=true;if(attempt){attempt.controller.abort();retire(attempt);}unsubscribe();for(const waiter of waiters.values())waiter.reject(Error('Desktop command bridge closed'));waiters.clear();},
  };
}

// The Rust queue ID is a presentation parent, never a caller-supplied operation ID.
function bindTicketDesktopCommands({actions,peek,ack,report,onPending,onUpdate,clock}){
  let closed=false,attempt,previous;const now=clock.now??Date.now;
  const waiters=new Map(),completed=new Map(),finishing=new Map(),ownedParents=new Set();
  const consumer=parent=>({kind:'interactive',parent,slot:'desktop-command'});
  const retained=parent=>actions.getSnapshot().entries.find(row=>row.ticket.consumer.parent===parent&&row.ticket.consumer.slot==='desktop-command');
  const terminalError=row=>Error(row.update?.error?.message??row.reason??`Desktop command ${row.state}`);
  const present=row=>{
    if(closed)return;
    const parent=row.ticket.consumer.parent;if(!ownedParents.has(parent))return;const presentation={...row,invocation:parent};
    onUpdate(presentation);report({desktopCommand:{invocation:parent,operation:row.invocation,state:row.state,terminal:row.terminal,error:row.update?.error?.message??row.reason??null}});
    if(!row.terminal)return;
    completed.set(parent,row);while(completed.size>32)completed.delete(completed.keys().next().value);
    const waiter=waiters.get(parent);if(waiter){waiters.delete(parent);row.state==='succeeded'?waiter.resolve():waiter.reject(terminalError(row));}
    if(finishing.has(parent)||!['succeeded','failed'].includes(row.state)||row.businessCancellationRequested||row.cancellationRequested)return;
    // Synchronous live presentation has accepted this interactive result. Its
    // explicit receipt is durable before removal. No automated parent uses this.
    const task=(async()=>{await actions.acknowledgeTerminal(row.ticket,{consumer:row.ticket.consumer,update:row.update,continuation:{kind:'desktop-result-presented',parent}});if(!closed)await actions.consumeTerminal(row.ticket);})().catch(()=>{});
    finishing.set(parent,task);void task.finally(()=>{finishing.delete(parent);if(!retained(parent))ownedParents.delete(parent);});
  };
  const unsubscribe=actions.subscribe(snapshot=>{if(!closed)for(const row of snapshot.entries)if(row.ticket.consumer.slot==='desktop-command')present(row);});
  return {
    async pump(){
      if(closed)return;
      const pending=peek();
      // Supersession only cancels an attempt still owned by the Rust queue. Once
      // acknowledged, the persisted ticket owns the command and its drive must
      // reach the original dispatch; the queue head is then legitimately gone.
      if(attempt){if(!attempt.acknowledged&&(pending?.invocation!==attempt.pending.invocation||pending?.command!==attempt.pending.command))attempt.controller.abort();return;}
      if(!pending)return;
      const value=previous?.pending.invocation===pending.invocation?previous:{pending:{...pending},controller:new AbortController(),deadline:now()+30000,retries:0,retryAfter:0,admissionStarted:false,binding:actions.getBinding?.()};
      previous=value;if(now()<value.retryAfter)return;
      if(now()>=value.deadline&&!retained(pending.invocation)){report({desktopCommand:{invocation:pending.invocation,state:value.admissionStarted?'unknown':'rejected',terminal:!value.admissionStarted,error:value.admissionStarted?'Original admission is unconfirmed':'Desktop service unavailable; command was not sent'}});if(!value.admissionStarted&&peek()?.invocation===pending.invocation)ack(pending.invocation);return;}
      const options={signal:value.controller.signal,deadlineAtMs:value.deadline,isCurrent:()=>!closed&&peek()?.invocation===pending.invocation&&peek()?.command===pending.command};
      attempt=value;ownedParents.add(pending.invocation);onPending(value.pending);
      try{
        let row=retained(pending.invocation),ticket=row?.ticket;
        if(row&&row.intent.argument?.command!==pending.command)throw Error('Original desktop parent intent changed');
        if(!ticket){
          const capability=await actions.catalogue(options);
          if(value.binding&&JSON.stringify(value.binding)!==JSON.stringify(capability.binding))throw Error('Action identity changed during discovery');
          if(value.admissionStarted){row=retained(pending.invocation);if(!row)throw Error('Original admission is unconfirmed; no replacement was created');ticket=row.ticket;}
          if(!ticket){
          if(closed||value.controller.signal.aborted||peek()?.invocation!==pending.invocation)throw Error('Desktop command attempt cancelled');
          const matches=capability.catalogue.surfaces.filter(s=>s.live!==false&&s.cluster_id===capability.binding.room&&s.app_id==='dev.qos.desktop'&&s.record_id==='').flatMap(surface=>(surface.actions??[]).filter(a=>a.resource==='desktop:session'&&a.action==='command').map(action=>({surface,action})));
          if(matches.length!==1)throw Error('Native desktop command service is unavailable');
          const {surface,action}=matches[0];
          value.admissionStarted=true;ticket=await actions.beginNew({app_peer:String(surface.peer_id),app_id:surface.app_id,record_id:surface.record_id,resource:action.resource,action:action.action,revision:action.revision??null,argument:{command:pending.command,...(action.params?.some(p=>p.name==='committed_desktop_v2'&&p.ty==='boolean')?{committed_desktop_v2:true}:{})},expected:{host:capability.host,binding:capability.binding,descriptor:action}},{...options,consumer:consumer(pending.invocation)});
          }
        }
        if(closed||value.controller.signal.aborted||peek()?.invocation!==pending.invocation||peek()?.command!==pending.command)return;
        // The persisted mapping owns recovery before presentation leaves Rust.
        value.acknowledged=true;if(peek()?.invocation===pending.invocation)ack(pending.invocation);
        row=await actions.drive(ticket,{signal:value.controller.signal,deadlineAtMs:value.deadline});present(row);
      }catch(error){
        if(!closed){const row=retained(pending.invocation);if(row){if(peek()?.invocation===pending.invocation)ack(pending.invocation);present(row);}else {value.retryAfter=now()+[250,1000,30000][Math.min(value.retries++,2)];report({desktopCommand:{invocation:pending.invocation,state:'unknown',terminal:false,error:String(error.message??error)}});}}
      }finally{if(attempt===value)attempt=undefined;}
    },
    cancel(parent){if(closed)return false;const row=retained(parent);if(row){void actions.requestCancellation(row.ticket).catch(()=>{});return true;}if(attempt?.pending.invocation!==parent)return false;attempt.controller.abort();return true;},
    waitForTerminal(parent){
      if(closed)return Promise.reject(Error('Desktop command bridge closed'));
      ownedParents.add(parent);const row=retained(parent)??completed.get(parent);if(row?.ticket)present(row);if(row?.terminal)return row.state==='succeeded'?Promise.resolve():Promise.reject(terminalError(row));
      if(waiters.has(parent))return waiters.get(parent).promise;
      let resolve,reject;const promise=new Promise((yes,no)=>{resolve=yes;reject=no;});waiters.set(parent,{promise,resolve,reject});return promise;
    },
    close(){closed=true;attempt?.controller.abort();unsubscribe();for(const waiter of waiters.values())waiter.reject(Error('Desktop command bridge closed'));waiters.clear();completed.clear();ownedParents.clear();},
  };
}
