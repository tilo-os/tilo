/** Mount-owned projection: no DOM lookup, transport mutation or polling timer. */
export function createDesktopNavigation({snapshot,revision,dispatch,search,globalSearch,focusSearch=()=>{},awaitTerminal,active}) {
 const parse=()=>{const next=JSON.parse(snapshot());next.capabilities={...next.capabilities,close:Boolean(awaitTerminal&&next.capabilities?.close),globalSearch:Boolean(globalSearch&&next.capabilities?.globalSearch)};return next;};
 const sameRoute=(a,b)=>a?.workspace===b?.workspace&&a?.strip===b?.strip&&a?.group===b?.group&&a?.tab===b?.tab;
 const routeKey=route=>JSON.stringify([route?.workspace,route?.strip,route?.group,route?.tab]);
 let closed=false, serial=revision(), value=parse(), listeners=new Set(), pendingCloses=new Map();
 function check(){if(closed)throw Error('Desktop navigation disposed');if(!active())throw Error('Desktop navigation unavailable');}
 function settleCloses(){for(const [key,pending] of pendingCloses){if(!pending.terminal)continue;const present=Array.isArray(value.entries)?value.entries.some(entry=>sameRoute(entry.route,pending.route)):sameRoute(value.active,pending.route);if(!present){pendingCloses.delete(key);pending.resolve();}}}
 function refresh(){if(closed||!active())return;const next=revision();if(next===serial)return;serial=next;value=parse();settleCloses();for(const listener of [...listeners])listener(value);}
 function openGlobalSearch(generation,query=""){check();refresh();generation??=value.generation;if(generation!==value.generation)throw Error("Stale desktop surface");if(!value.capabilities?.globalSearch||!globalSearch)throw Error("Global search unavailable");globalSearch(generation,query);focusSearch();refresh();}
 const controller={version:1,
  getSnapshot(){check();return value;},
  subscribe(listener){check();listeners.add(listener);return()=>listeners.delete(listener);},
  dispatch(command){
   try{check();if(command.generation!==value.generation)throw Error('Stale desktop surface');
    if(command.kind==='close'){
     if(!value.capabilities.close||!awaitTerminal)throw Error('Close unavailable');
     const key=routeKey(command.route),existing=pendingCloses.get(key);if(existing)return existing.promise;
     let resolve,reject;const promise=new Promise((yes,no)=>{resolve=yes;reject=no;});pendingCloses.set(key,{route:{...command.route},promise,resolve,reject,terminal:false});
     try{const invocation=dispatch(JSON.stringify(command));if(typeof invocation!=='string'||!invocation)throw Error('Close request identity unavailable');
      void Promise.resolve(awaitTerminal(invocation)).then(()=>{const pending=pendingCloses.get(key);if(pending?.promise===promise){pending.terminal=true;settleCloses();}},error=>{if(pendingCloses.get(key)?.promise===promise)pendingCloses.delete(key);reject(error);});
     }catch(error){pendingCloses.delete(key);reject(error);}
     refresh();return promise;
    }
    if(command.kind==='global-search'){openGlobalSearch(command.generation,command.query??'');}else if(command.kind==='commands'){if(!search)throw Error('Command search unavailable');search(command.generation,command.query);}else dispatch(JSON.stringify(command));refresh();return Promise.resolve();
   }catch(error){return Promise.reject(error);}
  },
 };
 return {controller,refresh,openGlobalSearch,close(){closed=true;listeners.clear();for(const pending of pendingCloses.values())pending.reject(Error('Desktop navigation disposed'));pendingCloses.clear();}};
}
