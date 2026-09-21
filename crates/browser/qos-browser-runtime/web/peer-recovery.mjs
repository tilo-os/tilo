/** One transport attempt and timer per stable session owner. */
// Reaching `ready` is not the same as staying there. The coordinator used to
// clear its attempt counter the moment a session became ready, so a session
// that dropped again seconds later retried on the 1 s base delay forever, and
// each retry cost the relay a full retained replay. `reset` now only records
// that the session settled; the counter is cleared at the next `schedule` and
// only if it held for at least STABLE_MS.
const STABLE_MS=30000;
const MAX_DELAY_MS=15000;
export function createRecoveryCoordinator({attempt,onState=()=>{},now=Date.now,random=Math.random,setTimeout=globalThis.setTimeout,clearTimeout=globalThis.clearTimeout}){
 let counter=0,timer=null,active=null,disposed=false,generation=0,nextAttemptAtMs=null,requested=false,settledAt=null;
 const emit=()=>{if(!disposed)onState({attempt:counter,nextAttemptAtMs,inFlight:active!==null});};
 const cancel=()=>{if(timer!==null)clearTimeout(timer);timer=null;nextAttemptAtMs=null;};
 const grade=()=>{
  if(settledAt===null)return;
  // The session that just ended had reached ready. Credit it only if it lasted.
  counter=now()-settledAt>=STABLE_MS?0:counter+1;
  settledAt=null;
 };
 function schedule(){
  if(disposed||timer!==null)return;
  if(active){requested=true;return;}requested=false;
  grade();
  const delay=Math.round((0.5+Math.max(0,Math.min(1,random()))*0.5)*Math.min(MAX_DELAY_MS,1000*2**Math.min(counter,4)));
  nextAttemptAtMs=Math.round(now())+delay;timer=setTimeout(()=>{timer=null;nextAttemptAtMs=null;void retry().catch(()=>{});},delay);emit();
 }
 function retry(){
  if(disposed)return Promise.reject(Error('Session closed'));
  cancel();if(active)return active;
  const current=++generation;
  active=Promise.resolve().then(attempt).then(value=>{
   if(!disposed&&current===generation){active=null;if(requested)schedule();else emit();}return value;
  },error=>{
   if(!disposed&&current===generation){active=null;requested=false;settledAt=null;counter++;if(!error?.terminal)schedule();else emit();}throw error;
  });emit();return active;
 }
 return{schedule,retry,reset(){requested=false;cancel();if(settledAt===null)settledAt=now();emit();},dispose(){disposed=true;generation++;cancel();},get inFlight(){return active;}};
}
