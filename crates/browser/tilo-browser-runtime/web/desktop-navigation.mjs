// DesktopNavigationV1 for one mount. Ported from PR 541's
// desktop-navigation.mjs without its launch placeholders and command-ticket
// correlation: the runtime admits every command synchronously, and a close
// settles once its route is gone from a published snapshot.
/** Mount-owned projection: no DOM lookup, transport mutation or polling timer. */
export function createDesktopNavigation({snapshot,revision,dispatch,active}) {
 const sameRoute=(a,b)=>a?.workspace===b?.workspace&&a?.strip===b?.strip&&a?.group===b?.group&&a?.tab===b?.tab;
 const routeKey=route=>JSON.stringify([route?.workspace,route?.strip,route?.group,route?.tab]);
 let closed=false,serial=revision(),value=JSON.parse(snapshot());
 const listeners=new Set(),pendingCloses=new Map();
 function check(){if(closed)throw Error('Desktop navigation disposed');if(!active())throw Error('Desktop navigation unavailable');}
 function settleCloses(){
  for(const [key,pending] of pendingCloses){
   if(value.entries?.some(entry=>sameRoute(entry.route,pending.route)))continue;
   pendingCloses.delete(key);pending.resolve();
  }
 }
 function refresh(){
  if(closed||!active())return;
  const next=revision();if(next===serial)return;
  serial=next;value=JSON.parse(snapshot());settleCloses();
  for(const listener of [...listeners])listener(value);
 }
 const controller={version:1,
  getSnapshot(){check();return value;},
  subscribe(listener){check();listeners.add(listener);return()=>listeners.delete(listener);},
  dispatch(command){
   try{
    check();
    if(command?.generation!==value.generation)throw Error('Stale desktop surface');
    // JSON carries a non-finite number as null; refuse it with the runtime's own reason.
    if(command.kind==='pan'&&!Number.isFinite(command.offset))throw Error('Invalid pan offset');
    if(command.kind==='close'){
     const key=routeKey(command.route),existing=pendingCloses.get(key);if(existing)return existing.promise;
     let resolve,reject;const promise=new Promise((yes,no)=>{resolve=yes;reject=no;});
     dispatch(JSON.stringify(command));
     pendingCloses.set(key,{route:{...command.route},promise,resolve,reject});
     refresh();return promise;
    }
    dispatch(JSON.stringify(command));refresh();return Promise.resolve();
   }catch(error){return Promise.reject(error);}
  },
 };
 return {controller,refresh,close(){
  closed=true;listeners.clear();
  for(const pending of pendingCloses.values())pending.reject(Error('Desktop navigation disposed'));
  pendingCloses.clear();
 }};
}
