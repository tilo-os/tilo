// DesktopNavigationV1 for one mount. A command is a verb of the desktop's
// published vocabularies: its effect id and arguments, the same ones a
// chord, an agent or a script names. The runtime admits every verb
// synchronously, refusing one that names something the desktop no longer
// holds; a close settles once its tab is gone from a published snapshot.
/** Mount-owned projection: no DOM lookup, transport mutation or polling timer. */
export function createDesktopNavigation({snapshot,revision,dispatch,active}) {
 let closed=false,serial=revision(),value=JSON.parse(snapshot());
 const listeners=new Set(),pendingCloses=new Map();
 function check(){if(closed)throw Error('Desktop navigation disposed');if(!active())throw Error('Desktop navigation unavailable');}
 function settleCloses(){
  for(const [key,pending] of pendingCloses){
   if(value.entries?.some(entry=>entry.route?.tab===pending.tab))continue;
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
  // The surface also moves on its own (a key chord, a click), between the
  // host's status reports: read the current state, not the last report's.
  getSnapshot(){check();refresh();return value;},
  subscribe(listener){check();listeners.add(listener);return()=>listeners.delete(listener);},
  dispatch(verb){
   try{
    check();
    const args=verb?.args??{};
    // JSON carries a non-finite number as null; refuse it with the runtime's own reason.
    if(verb.effect==='pan'&&!Number.isFinite(args.offset))throw Error('Invalid pan offset');
    if(verb.effect==='close-tab'){
     const key=args.tab,existing=pendingCloses.get(key);if(existing)return existing.promise;
     let resolve,reject;const promise=new Promise((yes,no)=>{resolve=yes;reject=no;});
     dispatch(JSON.stringify(verb));
     pendingCloses.set(key,{tab:key,promise,resolve,reject});
     refresh();return promise;
    }
    dispatch(JSON.stringify(verb));refresh();return Promise.resolve();
   }catch(error){return Promise.reject(error);}
  },
 };
 return {controller,refresh,close(){
  closed=true;listeners.clear();
  for(const pending of pendingCloses.values())pending.reject(Error('Desktop navigation disposed'));
  pendingCloses.clear();
 }};
}

/** The verbs a page, an agent or a script names, built where their arguments
 * are known: each is its resource's effect id and the arguments it takes, the
 * same encoding [`createDesktopNavigation`] carries. */
export const verb={
 /** Open an app in a new window of the shown strip. */
 openApp:app=>({effect:'open-app',args:{app}}),
 /** Bring the window holding the tab at `route` to the front. */
 focus:route=>({effect:'focus',args:{tab:route.tab}}),
 /** Close the tab at `route`, and its window when it is the window's last. */
 closeTab:route=>({effect:'close-tab',args:{tab:route.tab}}),
 /** Show a workspace's activity, `view` being `catch-up`, `explore` or `replay`. */
 activity:(workspace,view)=>({effect:'activity',args:{workspace,view}}),
 /** Close the workspace's activity surface. */
 closeActivity:workspace=>({effect:'close-activity',args:{workspace}}),
};
