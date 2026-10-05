/** Local geometry receipts only. Admission belongs to the authenticated caller.
 * Never executes an action, changes focus or publishes live presence. */
export function createSemanticPresentation({request,valid,surfaceSize,now=()=>performance.now()}) {
 const watches=new Map();let closed=false,generation=-1,frame=-1,geometry,presentedAt=-Infinity;
 const key=t=>JSON.stringify([t.app,t.instance,t.record,t.resource,t.action]);
 function target(value){
  if(!value||!Number.isSafeInteger(value.instance)||value.instance<0)throw Error('invalid semantic instance');
  const t={instance:value.instance};
  for(const field of ['app','record','resource','action']){
   if(typeof value[field]!=='string'||!value[field]||value[field].length>4096)throw Error('invalid semantic target');
   t[field]=value[field];
  }
  return Object.freeze(t);
 }
 function gap(){for(const watch of watches.values())watch.value=null;}
 return {
  observe(value){
   if(closed)throw Error('semantic presentation closed');const t=target(value),id=key(t);
   if(watches.size>=32&&!watches.has(id))throw Error('semantic target capacity exceeded');
   let entry=watches.get(id);
   if(!entry){request(t,true);entry={target:t,users:0,value:null};watches.set(id,entry);}entry.users++;
   let stopped=false;
   return {read({maxAgeMs=1000}={}){
    const at=now(),v=entry.value;
    if(v&&surfaceSize&&JSON.stringify(surfaceSize())!==JSON.stringify(v.pixels))return null;
    if(stopped||closed||!valid()||!v||!Number.isFinite(at)||!Number.isFinite(maxAgeMs)||maxAgeMs<0||at<v.presentedAtMs||at-v.presentedAtMs>maxAgeMs)return null;
    return structuredClone(v);
   },close(){if(stopped)return;stopped=true;if(--entry.users===0){watches.delete(id);entry.value=null;if(!closed)request(t,false);}}};
  },
  present(state){
   if(closed)return;
   if(state.phase!=='presented'){gap();return;}
   // Ordinary status publication cannot establish a new pixels/layout relationship.
   if(state.presentation!==true){
    for(const entry of watches.values()){
     const v=entry.value;if(!v)continue;
     if((state.record!==undefined&&state.record!==v.target.record)||
       (state.frames!==undefined&&state.frames!==v.frame)||
       (state.gpuGeneration!==undefined&&state.gpuGeneration!==v.gpuGeneration)||
       (state.layoutGeneration!==undefined&&state.layoutGeneration!==v.layoutGeneration)||
       (state.logicalSize!==undefined&&JSON.stringify(state.logicalSize)!==JSON.stringify(v.logicalSize))||
       (state.surfaceSize!==undefined&&JSON.stringify(state.surfaceSize)!==JSON.stringify(v.pixels)))entry.value=null;
    }
    return;
   }
   if(!Number.isSafeInteger(state.layoutGeneration)||state.layoutGeneration<0||!Number.isSafeInteger(state.gpuGeneration)||state.gpuGeneration<0||!Number.isSafeInteger(state.frames)||state.frames<1||state.gpuGeneration<generation||(state.gpuGeneration===generation&&state.frames<frame)){gap();return;}
   const currentGeometry=JSON.stringify([state.record,state.logicalSize,state.surfaceSize,state.layoutGeneration,state.semanticLoci]);
   if(state.gpuGeneration===generation&&state.frames===frame){if(geometry!==currentGeometry)gap();return;}
   geometry=currentGeometry;
   generation=state.gpuGeneration;frame=state.frames;gap();const at=state.observedPresentedAtMs??now(),size=state.logicalSize;
   if(!valid()||!Number.isFinite(at)||at<presentedAt||!Array.isArray(size)||size.length!==2||!size.every(n=>Number.isFinite(n)&&n>0)||!Array.isArray(state.surfaceSize)||state.surfaceSize.length!==2||!state.surfaceSize.every(n=>Number.isSafeInteger(n)&&n>0)||!Array.isArray(state.semanticLoci)||state.semanticLoci.length>32)return;
   presentedAt=at;
   for(const entry of watches.values()){
    if(entry.target.record!==state.record)continue;
    const rows=state.semanticLoci.filter(row=>row?.target&&key(row.target)===key(entry.target));
    if(rows.length!==1)continue;const rect=rows[0].rect;
    if(!Array.isArray(rect)||rect.length!==4||!rect.every(Number.isFinite)||rect[0]<0||rect[1]<0||rect[2]<=0||rect[3]<=0||rect[0]+rect[2]>size[0]||rect[1]+rect[3]>size[1])continue;
    entry.value={target:{...entry.target},rect:[...rect],frame,gpuGeneration:generation,layoutGeneration:state.layoutGeneration,presentedAtMs:at,logicalSize:[...size],pixels:[...state.surfaceSize]};
   }
  },
  close(){if(closed)return;closed=true;gap();for(const entry of watches.values())request(entry.target,false);watches.clear();},
 };
}
