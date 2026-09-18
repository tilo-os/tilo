/** Mount-owned projection: no DOM lookup, transport mutation or polling timer. */
export function createDesktopNavigation({snapshot,revision,dispatch,search,active}) {
 let closed=false, serial=revision(), value=JSON.parse(snapshot()), listeners=new Set();
 function check(){if(closed)throw Error('Desktop navigation disposed');if(!active())throw Error('Desktop navigation unavailable');}
 function refresh(){if(closed||!active())return;const next=revision();if(next===serial)return;serial=next;value=JSON.parse(snapshot());for(const listener of [...listeners])listener(value);}
 const controller={version:1,
  getSnapshot(){check();return value;},
  subscribe(listener){check();listeners.add(listener);return()=>listeners.delete(listener);},
  async dispatch(command){check();if(command.generation!==value.generation)throw Error('Stale desktop surface');if(command.kind==='commands'){if(!search)throw Error('Command search unavailable');search(command.generation,command.query);}else dispatch(JSON.stringify(command));refresh();},
 };
 return {controller,refresh,close(){closed=true;listeners.clear();}};
}
