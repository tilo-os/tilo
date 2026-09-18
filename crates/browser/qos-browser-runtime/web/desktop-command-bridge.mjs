// Shared-widget requests enter the existing certified, durable action client.
// A response updates lifecycle only; admitted signed room state owns rendering.
export function bindDesktopCommands({actions,peek,ack,report=()=>{}}){
  let closed=false,busy=false,current=actions.getSnapshot().entries.filter(row=>row.intent?.app_id==='dev.qos.desktop').at(-1)?.invocation;
  const status=row=>({desktopCommand:{invocation:row.invocation,state:row.state,terminal:row.terminal,
    error:row.update?.error?.message??row.reason??null}});
  const unsubscribe=actions.subscribe(snapshot=>{
    if(closed||!current)return;
    const row=snapshot.entries.find(row=>row.invocation===current);
    if(row)report(status(row));
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
        if(!closed){ack(pending.invocation);report(status(row));}
      }catch(error){
        if(!closed){
          const retained=actions.getSnapshot().entries.find(row=>row.invocation===pending.invocation);
          // The client retains any potentially dispatched work under this UUID.
          // A pre-admission failure is a visible refusal and cannot mutate topology.
          ack(pending.invocation);
          report(retained?status(retained):{desktopCommand:{invocation:pending.invocation,state:'rejected',terminal:true,error:String(error.message??error)}});
        }
      }finally{busy=false;}
    },
    close(){closed=true;unsubscribe();},
  };
}
