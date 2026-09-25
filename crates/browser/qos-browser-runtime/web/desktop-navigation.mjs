import {createDesktopLaunchFocus} from './desktop-launch-focus.mjs';
/** Mount-owned projection: no DOM lookup, transport mutation or polling timer. */
export function createDesktopNavigation({snapshot,revision,dispatch,search,globalSearch,focusSearch=()=>{},captureSelect=()=>undefined,completeSelect=()=>{},awaitTerminal,active,captureLaunchFocus=()=>undefined}) {
 const parse=()=>{const next=JSON.parse(snapshot());next.capabilities={...next.capabilities,close:Boolean(awaitTerminal&&next.capabilities?.close),globalSearch:Boolean(globalSearch&&next.capabilities?.globalSearch)};return next;};
 const sameRoute=(a,b)=>a?.workspace===b?.workspace&&a?.strip===b?.strip&&a?.group===b?.group&&a?.tab===b?.tab;
 const routeKey=route=>JSON.stringify([route?.workspace,route?.strip,route?.group,route?.tab]);
 let closed=false, serial=revision(), value=parse(), listeners=new Set(), pendingCloses=new Map(), launches=new Map(), claimedLaunchRoutes=new Set();
 // Launch placeholders are presentation only. Native u64 identities remain in
 // the untouched command string; only the app's string ID is read for a label.
 const launchFocus=createDesktopLaunchFocus({snapshot:()=>value,capture:captureLaunchFocus,select:route=>controller.dispatch({kind:'select',generation:value.generation,route})});
 const publish=()=>{value={...value,pendingLaunches:[...launches].map(([invocation,item])=>({invocation,label:item.label}))};for(const listener of [...listeners])listener(value);};
 function settleLaunches(){
  const claimed=claimedLaunchRoutes;
  for(const [invocation,item] of launches){
   if(!item.succeeded&&!item.committed)continue;
   const entry=value.entries?.find(entry=>entry.label===item.label&&(!item.workspace||entry.route.workspace===item.workspace)&&!item.before.has(routeKey(entry.route))&&!claimed.has(routeKey(entry.route)));
   if(entry){claimed.add(routeKey(entry.route));launches.delete(invocation);}
  }
 }
 function commandPending(pending){
  if(closed||!active()||launches.has(pending.invocation))return;
  launchFocus.pending(pending);
  let app;try{app=JSON.parse(pending.command).command?.OpenTab?.app;}catch{return;}
  const application=value.applications?.find(application=>application.id===app);if(!application)return;
  if(!launches.size)claimedLaunchRoutes.clear();
  launches.set(pending.invocation,{label:application.label,workspace:value.activeWorkspace,before:new Set((value.entries??[]).map(entry=>routeKey(entry.route))),succeeded:false});publish();
 }
 function commandUpdated(row){
  if(closed)return;launchFocus.updated(row);const item=launches.get(row.invocation);if(!item||!row.terminal)return;
  if(row.state==='succeeded'){item.succeeded=true;settleLaunches();}else launches.delete(row.invocation);
  if(!launches.has(row.invocation))publish();
 }
 function check(){if(closed)throw Error('Desktop navigation disposed');if(!active())throw Error('Desktop navigation unavailable');}
 function settleCloses(){for(const [key,pending] of pendingCloses){if(!pending.terminal)continue;const present=Array.isArray(value.entries)?value.entries.some(entry=>sameRoute(entry.route,pending.route)):sameRoute(value.active,pending.route);if(!present){pendingCloses.delete(key);pending.resolve();}}}
 function refresh(){if(closed||!active())return;const next=revision();if(next===serial)return;serial=next;const previous=value.generation;value=parse();if(previous!==value.generation){launches.clear();claimedLaunchRoutes.clear();}settleCloses();settleLaunches();publish();launchFocus.refresh();}
 function openGlobalSearch(generation,query=""){launchFocus.cancel();check();refresh();generation??=value.generation;if(generation!==value.generation)throw Error("Stale desktop surface");if(!value.capabilities?.globalSearch||!globalSearch)throw Error("Global search unavailable");globalSearch(generation,query);focusSearch();refresh();}
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
  dispatch(command){
   launchFocus.cancel();
   try{check();if(command.generation!==value.generation)throw Error('Stale desktop surface');validateWorkspace(command);
    if(command.kind==='close'){
     if(!value.capabilities.close||!awaitTerminal)throw Error('Close unavailable');
     const key=routeKey(command.route),existing=pendingCloses.get(key);if(existing)return existing.promise;
     let resolve,reject;const promise=new Promise((yes,no)=>{resolve=yes;reject=no;});pendingCloses.set(key,{route:{...command.route},promise,resolve,reject,terminal:false});
     try{const invocation=dispatch(JSON.stringify(command));if(typeof invocation!=='string'||!invocation)throw Error('Close request identity unavailable');
      void Promise.resolve(awaitTerminal(invocation)).then(()=>{const pending=pendingCloses.get(key);if(pending?.promise===promise){pending.terminal=true;settleCloses();}},error=>{if(pendingCloses.get(key)?.promise===promise)pendingCloses.delete(key);reject(error);});
     }catch(error){pendingCloses.delete(key);reject(error);}
     refresh();return promise;
    }
    const beforeSelect=command.kind==='select'?captureSelect():undefined;
    if(command.kind==='global-search'){openGlobalSearch(command.generation,command.query??'');}else if(command.kind==='commands'){if(!search)throw Error('Command search unavailable');search(command.generation,command.query);}else dispatch(JSON.stringify(command));refresh();return command.kind==='select'?Promise.resolve(completeSelect(beforeSelect)).then(()=>{}):Promise.resolve();
   }catch(error){return Promise.reject(error);}
  },
 };
 return {controller,refresh,openGlobalSearch,commandPending,commandUpdated,commandCommitted(event){if(closed)return;const item=launches.get(event.invocation);if(item)item.committed=true;refresh();launchFocus.committed(event);settleLaunches();publish();},close(){closed=true;launchFocus.close();launches.clear();claimedLaunchRoutes.clear();listeners.clear();for(const pending of pendingCloses.values())pending.reject(Error('Desktop navigation disposed'));pendingCloses.clear();}};
}
