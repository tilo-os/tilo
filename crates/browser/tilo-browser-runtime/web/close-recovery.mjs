/** Begin teardown once, but retain a retryable owner until settlement succeeds. */
export function recoverableClose({begin,settle,onError=()=>{}}) {
  let began=false,complete=false,running;
  return function close(){
    if(complete)return Promise.resolve();
    if(running)return running;
    running=(async()=>{
      try{if(!began){await begin();began=true;}await settle();complete=true;}
      catch(error){onError(error);throw error;}
    })().finally(()=>{running=undefined;});
    return running;
  };
}
