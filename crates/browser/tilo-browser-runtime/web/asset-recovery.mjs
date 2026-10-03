/** A rejected asset/runtime initialization never poisons later mount attempts. */
export function retryableMemo(load){
 let pending;
 return()=>{if(!pending){const attempt=Promise.resolve().then(load);pending=attempt;attempt.catch(()=>{if(pending===attempt)pending=undefined;});}return pending;};
}
/** IDs can only name requests drained from this live canonical image loader. */
export function createAssetRecovery({load,complete,retry,active,report}){
 let next=0;const entries=new Map(),byRequest=new Map();
 function attempt(entry,retrying){
  if(entry.pending)return entry.pending;
  entry.pending=Promise.resolve().then(async()=>{
   if(!active())throw Error('Asset surface disposed');
   if(retrying&&!retry(entry.request))throw Error('Asset request is stale');
   try{
    const bytes=await load(entry.request);if(!active())throw Error('Asset surface disposed');
    if(!complete(entry.request,bytes))throw Error('Asset decode or content validation failed');
    entry.failed=false;report({asset:'ready',assetId:entry.id,assetUrl:entry.request.url});
   }catch(error){
    if(active()){complete(entry.request,undefined);entry.failed=true;report({asset:'unavailable',assetId:entry.id,assetUrl:entry.request.url,error:String(error)});}throw error;
   }
  }).finally(()=>entry.pending=null);return entry.pending;
 }
 return{
  request(request){const key=JSON.stringify([request.loader,request.url]);let entry=byRequest.get(key);if(!entry){entry={id:String(++next),request,failed:false,pending:null};entries.set(entry.id,entry);byRequest.set(key,entry);}return attempt(entry,false);},
  recover(id){const entry=entries.get(id);if(!entry||(!entry.failed&&!entry.pending))return Promise.reject(Error('Unknown or settled asset request'));return attempt(entry,true);}
 };
}
