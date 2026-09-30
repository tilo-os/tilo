/** Windows whose application crashed the runtime, kept per room.
 * The browser build aborts on a panic: the module cannot run again and the
 * page must start over. The runtime's panic hook names the window whose
 * application was running; the next load shows that window's error instead
 * of running the application, so one crashing app does not take every other
 * window down with it on each load. Entries expire, and a closed window's
 * id is never reused. */
const KEY='tilo.crashed-windows.v1';
const LIMIT=32;
const TTL_MS=7*24*60*60*1000;
const MESSAGE_LIMIT=500;

export function createCrashQuarantine({storage=globalThis.localStorage,now=Date.now}={}) {
  const read=()=>{
    try{const list=JSON.parse(storage?.getItem(KEY)??'[]');return Array.isArray(list)?list:[];}
    catch{return [];}
  };
  const current=()=>read().filter(entry=>typeof entry?.room==='string'&&typeof entry.tab==='string'&&now()-entry.at<TTL_MS);
  const write=list=>{try{storage?.setItem(KEY,JSON.stringify(list.slice(-LIMIT)));}catch{}};
  return {
    /** `[{tab, message}]` for `room`, as `quarantine_windows` takes them. */
    entries(room){return current().filter(entry=>entry.room===room).map(({tab,message})=>({tab,message}));},
    /** Keep `tab` out of `room`'s next load. False when it already was: its
     * application ran anyway, and starting over would crash again. */
    record(room,{tab,message}){
      const list=current(),id=String(tab);
      if(list.some(entry=>entry.room===room&&entry.tab===id))return false;
      list.push({room,tab:id,message:String(message??'').slice(0,MESSAGE_LIMIT),at:now()});
      write(list);
      return true;
    },
    forget(room,tab){write(current().filter(entry=>!(entry.room===room&&entry.tab===String(tab))));},
  };
}

/** What the page does about a runtime panic `report` (`{tab, message}`, as
 * the runtime's hook sends it): start over without the window that crashed
 * once it is quarantined, else only report it (a panic outside any window
 * would crash again on every load). */
export function crashAction(report,recorded) {
  return report?.tab!=null&&recorded?'restart':'report';
}
