import {actionError} from './action-client.mjs';
const DESKTOP_SCOPE={app_id:'dev.qos.desktop',record_id:'',resource:'desktop:session',action:'command'};
// Shared-widget requests enter the existing certified, durable action client.
// A response updates lifecycle only; admitted signed room state owns rendering.
export function bindDesktopCommands({actions,peek,ack,report=()=>{},onPending=()=>{},onUpdate=()=>{},clock={}}){
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
