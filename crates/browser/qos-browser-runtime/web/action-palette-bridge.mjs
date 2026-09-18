// A mount only retains display controls. The session-owned actionsV1 outbox
// remains the sole owner of invocation UUIDs, persistence and recovery.
export function bindActionPalette({actions,publish,report=()=>{}}){
  let closed=false, controls=new Map(), previous;
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
  return {
    async submit(request){
      if(closed)throw Error('Action surface disposed');
      const control=controls.get(request.control);
      if(!control)throw Error('Action control is stale or unavailable');
      // No invocation ID is derived from a widget/correlation number. The
      // durable client allocates the full UUID inside its serialized admission.
      const row=await actions.invoke({...control.route,expected:control.expected,argument:request.argument,confirmation:request.confirmation});
      if(!closed)report({action:{invocation:row.invocation,state:row.state,terminal:row.terminal,cancellationRequested:row.cancellationRequested}});
      return row;
    },
    close(){closed=true;controls.clear();unsubscribe();},
  };
}
