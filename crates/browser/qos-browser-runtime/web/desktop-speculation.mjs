// Optimistic window reorders (runtime `desktop_host/speculation.rs`): the
// runtime renders a queued pure reorder at once; this glue feeds it the command
// bridge's terminal outcomes and wakes a frame when a hold or deadline passes.
// Absent runtime exports leave the desktop durable-only.
export const CONFIRMED_HOLD_MS=3000,UNCONFIRMED_DEADLINE_MS=45000;
export function createDesktopSpeculation({runtime,id,active,clock={}}){
  const set=clock.setTimeout??globalThis.setTimeout,clear=clock.clearTimeout??globalThis.clearTimeout;
  const available=typeof runtime.desktop_speculation_enable==='function'&&typeof runtime.desktop_command_settled==='function';
  const timers=new Set();let closed=false;
  const call=(invocation,outcome)=>{if(closed||!active())return;try{runtime.desktop_command_settled(id,invocation,outcome);}catch{}};
  const wake=(invocation,delay)=>{const timer=set(()=>{timers.delete(timer);call(invocation,undefined);},delay);timers.add(timer);};
  if(available&&active()){try{runtime.desktop_speculation_enable(id,true);}catch{}}
  return {
    available,
    /** The bridge started submitting: re-check after the unconfirmed deadline. */
    pending(pending){if(available&&typeof pending?.invocation==='string')wake(pending.invocation,UNCONFIRMED_DEADLINE_MS+100);},
    /** Every bridge status report; terminal ones settle the speculation. */
    report(data){
      const row=data?.desktopCommand;if(!available||!row?.terminal||typeof row.invocation!=='string')return;
      const succeeded=row.state==='succeeded';call(row.invocation,succeeded);
      if(succeeded)wake(row.invocation,CONFIRMED_HOLD_MS+100);
    },
    close(){closed=true;for(const timer of timers)clear(timer);timers.clear();},
  };
}
