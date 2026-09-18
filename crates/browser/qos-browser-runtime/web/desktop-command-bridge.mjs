// Shared-widget requests enter the existing certified, durable action client.
// A response updates lifecycle only; admitted signed room state owns rendering.
export function bindDesktopCommands({actions,peek,ack,report=()=>{}}){
  let closed=false,busy=false,current=actions.getSnapshot().entries.filter(row=>row.intent?.app_id==='dev.qos.desktop').at(-1)?.invocation;
  const waiters=new Map();
  const status=row=>({desktopCommand:{invocation:row.invocation,state:row.state,terminal:row.terminal,
    error:row.update?.error?.message??row.reason??null}});
  const terminalError=row=>Error(row.update?.error?.message??row.reason??`Desktop command ${row.state}`);
  const settle=row=>{const waiter=waiters.get(row?.invocation);if(!waiter||!row.terminal)return;waiters.delete(row.invocation);if(row.state==='succeeded')waiter.resolve();else waiter.reject(terminalError(row));};
  const unsubscribe=actions.subscribe(snapshot=>{
    if(closed)return;
    const row=snapshot.entries.find(row=>row.invocation===current);
    if(row)report(status(row));
    for(const invocation of waiters.keys())settle(snapshot.entries.find(row=>row.invocation===invocation));
  });
  const retained=actions.getSnapshot().entries.find(row=>row.invocation===current);
  if(retained)report(status(retained));
  return {
    async pump(){
      if(closed||busy)return;
      const pending=peek();if(!pending)return;
      busy=true;current=pending.invocation;
      report({desktopCommand:{invocation:current,state:'submitting',terminal:false,error:null}});
      try{
        const capability=await actions.catalogue();
        const matches=capability.catalogue.surfaces.filter(surface=>surface.live!==false&&surface.cluster_id===capability.binding.room&&surface.app_id==='dev.qos.desktop'&&surface.record_id==='')
          .flatMap(surface=>(surface.actions??[]).filter(action=>action.resource==='desktop:session'&&action.action==='command').map(action=>({surface,action})));
        if(matches.length!==1)throw Error('Native desktop command service is unavailable');
        const {surface,action}=matches[0];
        const row=await actions.invoke({invocation:pending.invocation,app_peer:String(surface.peer_id),app_id:surface.app_id,record_id:surface.record_id,
          resource:action.resource,action:action.action,revision:action.revision??null,
          argument:{command:pending.command},expected:{host:capability.host,binding:capability.binding,descriptor:action}});
        if(!closed){ack(pending.invocation);report(status(row));settle(row);}
      }catch(error){
        if(!closed){
          const retained=actions.getSnapshot().entries.find(row=>row.invocation===pending.invocation);
          // The client retains any potentially dispatched work under this UUID.
          // A pre-admission failure is a visible refusal and cannot mutate topology.
          ack(pending.invocation);
          if(retained){report(status(retained));settle(retained);}
          else {const message=String(error.message??error);report({desktopCommand:{invocation:pending.invocation,state:'rejected',terminal:true,error:message}});const waiter=waiters.get(pending.invocation);if(waiter){waiters.delete(pending.invocation);waiter.reject(Error(message));}}
        }
      }finally{busy=false;}
    },
    waitForTerminal(invocation){
      if(closed)return Promise.reject(Error('Desktop command bridge closed'));
      const row=actions.getSnapshot().entries.find(row=>row.invocation===invocation);
      if(row?.terminal)return row.state==='succeeded'?Promise.resolve():Promise.reject(terminalError(row));
      if(waiters.has(invocation))return waiters.get(invocation).promise;
      let resolve,reject;const promise=new Promise((yes,no)=>{resolve=yes;reject=no;});waiters.set(invocation,{promise,resolve,reject});return promise;
    },
    close(){closed=true;unsubscribe();for(const waiter of waiters.values())waiter.reject(Error('Desktop command bridge closed'));waiters.clear();},
  };
}
