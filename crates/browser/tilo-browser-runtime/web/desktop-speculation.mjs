// Optimistic desktop commands (runtime `desktop_host/speculation.rs`): the
// runtime renders a queued reorder, open or close at once; this glue feeds it
// the command bridge's terminal outcomes and the committed window ids, and
// wakes a frame when a hold or deadline passes. Absent runtime exports leave
// the desktop durable-only.
export const CONFIRMED_HOLD_MS=3000,UNCONFIRMED_DEADLINE_MS=45000;
const RECEIPT_KIND='desktop.command';
export function createDesktopSpeculation({runtime,id,active,clock={}}){
  const set=clock.setTimeout??globalThis.setTimeout,clear=clock.clearTimeout??globalThis.clearTimeout;
  const available=typeof runtime.desktop_speculation_enable==='function'&&typeof runtime.desktop_command_settled==='function';
  // v2 tickets: the durable operation id (the command id's UUID) -> the Rust parent.
  const timers=new Set(),parents=new Map();let closed=false;
  const call=(invocation,outcome,receipt)=>{if(closed||!active())return;try{receipt===undefined?runtime.desktop_command_settled(id,invocation,outcome):runtime.desktop_command_settled(id,invocation,outcome,JSON.stringify(receipt));}catch{}};
  const wake=(invocation,delay)=>{const timer=set(()=>{timers.delete(timer);call(invocation,undefined);},delay);timers.add(timer);};
  const remember=(operation,parent)=>{if(typeof operation!=='string'||operation===parent)return;parents.set(operation,parent);while(parents.size>64)parents.delete(parents.keys().next().value);};
  if(available&&active()){try{runtime.desktop_speculation_enable(id,true);}catch{}}
  return {
    available,
    /** The bridge started submitting: re-check after the unconfirmed deadline. */
    pending(pending){if(available&&typeof pending?.invocation==='string')wake(pending.invocation,UNCONFIRMED_DEADLINE_MS+100);},
    /** Every bridge status report; terminal ones settle the speculation. */
    report(data){
      const row=data?.desktopCommand;if(!available||typeof row?.invocation!=='string')return;
      remember(row.operation,row.invocation);if(!row.terminal)return;
      const succeeded=row.state==='succeeded';call(row.invocation,succeeded);
      if(succeeded)wake(row.invocation,CONFIRMED_HOLD_MS+100);
    },
    /** A bridge lifecycle row (`invocation` is the Rust parent): a succeeded
     * receipt's `opened` binds an optimistic open to its committed window. */
    update(row){
      if(!available||typeof row?.invocation!=='string'||!row.terminal||row.state!=='succeeded')return;
      const value=row.update?.output?.value;if(value?.kind!==RECEIPT_KIND||!Array.isArray(value.opened)||!value.opened.length)return;
      call(row.invocation,true,{opened:value.opened});
    },
    /** A committed-desktop notice (its `invocation` is the durable operation). */
    committed(event){
      if(!available||typeof event?.invocation!=='string'||!event.opened||typeof event.opened!=='object')return;
      call(parents.get(event.invocation)??event.invocation,undefined,{opened:event.opened});
    },
    close(){closed=true;for(const timer of timers)clear(timer);timers.clear();parents.clear();},
  };
}
