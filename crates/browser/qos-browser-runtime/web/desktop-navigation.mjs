/** Mount-owned projection: no DOM lookup, transport mutation or polling timer. */
export function createDesktopNavigation({snapshot,revision,dispatch,search,active}) {
 let closed=false, serial=revision(), value=JSON.parse(snapshot()), listeners=new Set();
 function check(){if(closed)throw Error('Desktop navigation disposed');if(!active())throw Error('Desktop navigation unavailable');}
 function refresh(){if(closed||!active())return;const next=revision();if(next===serial)return;serial=next;value=JSON.parse(snapshot());for(const listener of [...listeners])listener(value);}
 function validateWorkspace(command){
  if(!['workspace','workspace-activity','close-workspace-activity'].includes(command.kind))return;
  const id=command.workspace;
  if(typeof id!=='string'||!/^(0|[1-9][0-9]*)$/.test(id)||BigInt(id)>18446744073709551615n)throw Error('Invalid navigation identity');
  if(!value.workspaces?.some(workspace=>workspace.id===id))throw Error('Workspace unavailable');
  const caps=value.capabilities??{};
  if(command.kind==='workspace'){
   if(!caps.workspaceSelect)throw Error('Workspace selection unavailable');
  }else{
   if(id!==value.activeWorkspace)throw Error('Workspace is no longer active');
   if(!caps.workspaceHistory)throw Error('Workspace history unavailable');
   if(command.kind==='workspace-activity'){
    if(!['catch-up','explore','replay','tasks'].includes(command.view))throw Error('Workspace activity view unavailable');
    if(command.view==='replay'&&!caps.workspaceReplay)throw Error('Workspace replay unavailable');
   }
  }
 }
 const controller={version:1,
  getSnapshot(){check();return value;},
  subscribe(listener){check();listeners.add(listener);return()=>listeners.delete(listener);},
  async dispatch(command){check();if(command.generation!==value.generation)throw Error('Stale desktop surface');validateWorkspace(command);if(command.kind==='commands'){if(!search)throw Error('Command search unavailable');search(command.generation,command.query);}else dispatch(JSON.stringify(command));refresh();},
 };
 return {controller,refresh,close(){closed=true;listeners.clear();}};
}
