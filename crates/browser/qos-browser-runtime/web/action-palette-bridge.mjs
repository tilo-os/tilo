// A mount only retains display controls. The session-owned actionsV1 outbox
// remains the sole owner of invocation UUIDs, persistence and recovery.
export function bindActionPalette({actions,publish,report=()=>{}}){
  let closed=false, controls=new Map(), previous;const pending=new Map();const refreshController=new AbortController();
  const canonical=value=>JSON.stringify(value,(_,v)=>v&&typeof v==='object'&&!Array.isArray(v)?Object.fromEntries(Object.keys(v).sort().map(k=>[k,v[k]])):v);
  function update(snapshot){
    if(closed)return;
    const capability=snapshot.capability, rows=[], next=new Map();
    if(capability.state==='ready')for(const surface of capability.catalogue.surfaces){
      if(surface.app_id==='dev.qos.desktop')continue; // Shared desktop widgets bind semantic arguments.
      if(surface.live===false||surface.cluster_id!==capability.binding.room)continue;
      for(const action of surface.actions??[]){
        const route={app_peer:String(surface.peer_id),app_id:surface.app_id,record_id:surface.record_id,resource:action.resource,action:action.action,revision:action.revision??null};
        const key=canonical([capability.host,capability.binding,route,action]);
        if(next.has(key))continue;
        next.set(key,{route,expected:{host:capability.host,binding:capability.binding,descriptor:action}});
        rows.push({key,...action});
      }
    }
    controls=next;
    const encoded=JSON.stringify(rows);
    if(encoded!==previous){
      try{publish(rows);previous=encoded;}
      catch(error){controls.clear();publish([]);report({actionError:String(error)});}
    }
  }
  const unsubscribe=actions.subscribe(update);update(actions.getSnapshot());
  void actions.refreshCatalogue?.({signal:refreshController.signal}).catch(error=>{if(!closed&&error?.code!=='cancelled')report({actionError:String(error.message??error),actionStage:'palette-discovery'});});
  return {
    async submit(request){
      if(closed)throw Error('Action surface disposed');
      const control=controls.get(request.control);
      if(!control)throw Error('Action control is stale or unavailable');
      // No invocation ID is derived from a widget/correlation number. The
      // durable client allocates the full UUID inside its serialized admission.
      const intent={...control.route,expected:control.expected,argument:request.argument,confirmation:request.confirmation};
      if(actions.version!==2){const row=await actions.invoke(intent);if(!closed)report({action:{invocation:row.invocation,state:row.state,terminal:row.terminal,cancellationRequested:row.cancellationRequested}});return row;}
      let retained=pending.get(request.control);
      if(retained&&canonical(retained.intent)!==canonical(intent))throw Error('The original palette intent is still retained');
      if(!retained){
        // Equal command text/route does not identify another mount's consumer.
        // Only this retained attempt may retry its exact persisted parent/ticket.
        retained={intent:structuredClone(intent),consumer:{kind:'interactive',parent:crypto.randomUUID(),slot:'palette-result'},ticket:undefined,started:false};pending.set(request.control,retained);
      }
      if(retained.task)return retained.task;
      const task=(async()=>{
        if(!retained.ticket){
          if(retained.started){await actions.catalogue({signal:refreshController.signal});retained.ticket=actions.getSnapshot().entries.find(row=>canonical(row.ticket.consumer)===canonical(retained.consumer))?.ticket;if(!retained.ticket)throw Error('Original palette admission is unconfirmed; no replacement was created');}
          else {retained.started=true;retained.ticket=await actions.beginNew(retained.intent,{consumer:retained.consumer,signal:refreshController.signal,isCurrent:()=>!closed});}
        }
        if(closed)throw Error('Action surface disposed; original operation retained');
        const ticket=retained.ticket,row=await actions.drive(ticket,{signal:refreshController.signal,isCurrent:()=>!closed});
        if(!closed)report({action:{invocation:row.invocation,state:row.state,terminal:row.terminal,cancellationRequested:row.cancellationRequested}});
        if(!closed&&row.terminal&&['succeeded','failed'].includes(row.state)&&!row.businessCancellationRequested&&!row.cancellationRequested){
          await actions.acknowledgeTerminal(ticket,{consumer:ticket.consumer,update:row.update,continuation:{kind:'palette-result-presented'}});
          if(!closed){await actions.consumeTerminal(ticket);pending.delete(request.control);}
        }
        return row;
      })();retained.task=task;try{return await task;}finally{if(retained.task===task)retained.task=undefined;}
    },
    close(){closed=true;refreshController.abort();controls.clear();unsubscribe();},
  };
}
