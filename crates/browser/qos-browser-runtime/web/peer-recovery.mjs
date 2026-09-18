/** One transport attempt and timer per stable session owner. */
export function createRecoveryCoordinator({attempt,onState=()=>{},now=Date.now,random=Math.random,setTimeout=globalThis.setTimeout,clearTimeout=globalThis.clearTimeout}){
 let counter=0,timer=null,active=null,disposed=false,generation=0,nextAttemptAtMs=null,requested=false;
 const emit=()=>{if(!disposed)onState({attempt:counter,nextAttemptAtMs,inFlight:active!==null});};
 const cancel=()=>{if(timer!==null)clearTimeout(timer);timer=null;nextAttemptAtMs=null;};
 function schedule(){
  if(disposed||timer!==null)return;
  if(active){requested=true;return;}requested=false;
  const delay=(0.5+Math.max(0,Math.min(1,random()))*0.5)*Math.min(4000,1000*2**Math.min(counter,3));
  nextAttemptAtMs=now()+delay;timer=setTimeout(()=>{timer=null;nextAttemptAtMs=null;void retry().catch(()=>{});},delay);emit();
 }
 function retry(){
  if(disposed)return Promise.reject(Error('Session closed'));
  cancel();if(active)return active;
  const current=++generation;
  active=Promise.resolve().then(attempt).then(value=>{
   if(!disposed&&current===generation){active=null;if(requested)schedule();else emit();}return value;
  },error=>{
   if(!disposed&&current===generation){active=null;requested=false;counter++;if(!error?.terminal)schedule();else emit();}throw error;
  });emit();return active;
 }
 return{schedule,retry,reset(){counter=0;requested=false;cancel();emit();},dispose(){disposed=true;generation++;cancel();},get inFlight(){return active;}};
}
