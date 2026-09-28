import {transactionComplete} from './host-core.mjs';

// Private custody, not authority: only Rust validates manifests, digests, native
// admission and the cut. These rows must never be document hydration inputs.
// No DB version bump: array keys are disjoint from the v1 canonical string keys.
export const TRANSFER_CHUNK_BYTES=240*1024;
export const TRANSFER_STORAGE_BYTES=256*1024*1024;
const ITEM_BYTES=64*1024*1024,MAX_STAGES=32,MAX_NAMESPACE_STAGES=2;
const PREFIX='tilo-transfer-stage-v1',REGISTRY=[PREFIX,'registry'];
const FIELDS=['id','scope','cutToken','totalBytes','maxItemBytes'];
const invalid=message=>Object.assign(Error(message),{name:'InvalidStateError'});
const pressure=message=>Object.assign(Error(message),{name:'QuotaExceededError'});
const integer=(n,min,max)=>Number.isSafeInteger(n)&&n>=min&&n<=max;
const opaque=s=>typeof s==='string'&&s.length>0&&s.length<=4096&&s.isWellFormed()&&new TextEncoder().encode(s).length<=4096;
function descriptor(value){
 if(!value||typeof value.id!=='string'||!/^[0-9a-f]{32}$/.test(value.id)||/^0+$/.test(value.id)||!opaque(value.scope)||!opaque(value.cutToken)||!integer(value.totalBytes,1,Number.MAX_SAFE_INTEGER)||!integer(value.maxItemBytes,1,Math.min(ITEM_BYTES,value.totalBytes)))throw invalid('Invalid transfer descriptor or storage bound');
 if(value.totalBytes>TRANSFER_STORAGE_BYTES)throw pressure('Transfer stream including headers exceeds the 256MiB staging reservation');
 return Object.fromEntries(FIELDS.map(k=>[k,value[k]]));
}
export function captureTransferHandle(value){const result=descriptor(value);if(!opaque(value.epoch))throw invalid('Invalid transfer lease epoch');return {...result,epoch:value.epoch};}
const same=(a,b)=>FIELDS.every(k=>a[k]===b[k]);
const chunkKey=(namespace,id,index)=>[PREFIX,namespace,id,index];
function registry(value){
 if(value===undefined)return {id:REGISTRY,version:1,entries:[]};
 if(value?.version!==1||!Array.isArray(value.entries)||value.entries.length>MAX_STAGES)throw invalid('Corrupt transfer registry');
 let total=0;const keys=new Set(),counts=new Map();
 for(const entry of value.entries){
  descriptor(entry);
  if(entry.resumeDigest!==undefined&&!opaque(entry.resumeDigest))throw invalid('Corrupt transfer resume digest');
  if(typeof entry.namespace!=='string'||!opaque(entry.epoch)||!integer(entry.nextOffset,0,entry.totalBytes)||(entry.nextOffset!==entry.totalBytes&&entry.nextOffset%TRANSFER_CHUNK_BYTES!==0))throw invalid('Corrupt transfer registry entry');
  const key=JSON.stringify([entry.namespace,entry.id]);if(keys.has(key))throw invalid('Duplicate transfer registry entry');keys.add(key);
  total+=entry.totalBytes;counts.set(entry.namespace,(counts.get(entry.namespace)??0)+1);
 }
 if(total>TRANSFER_STORAGE_BYTES||[...counts.values()].some(n=>n>MAX_NAMESPACE_STAGES))throw invalid('Corrupt transfer reservation bounds');
 return value;
}
function lookup(entries,namespace,epoch,handle){
 if(handle.epoch!==epoch)throw invalid('Transfer lease epoch mismatch');
 const entry=entries.find(e=>e.namespace===namespace&&e.id===handle.id);
 if(!entry)throw invalid('Unknown or missing transfer');
 if(entry.epoch!==epoch||!same(entry,handle))throw invalid('Transfer handle scope or lease mismatch');
 return entry;
}
const publicHandle=entry=>({...descriptor(entry),epoch:entry.epoch,nextOffset:entry.nextOffset,resumeDigest:entry.resumeDigest??entry.cutToken});
// All callbacks here enqueue IDB requests synchronously. No Blob read/await may
// keep a transaction alive; Blob bytes are read only after readonly completion.
function withRegistry(store,tx,body,fail){
 const request=store.get(REGISTRY);
 request.onsuccess=()=>{try{body(registry(request.result));}catch(error){fail(error);tx.abort();}};
}
async function transaction(db,mode,body){
 const tx=db.transaction('records',mode,mode==='readwrite'?{durability:'strict'}:undefined);
 const done=transactionComplete(tx);let failure,result;
 withRegistry(tx.objectStore('records'),tx,value=>{result=body(value,tx.objectStore('records'),tx,error=>{failure=error;});},error=>{failure=error;});
 try{await done;}catch(error){throw failure??error;}return result;
}
function checkedBlob(row,size){if(!(row?.blob instanceof Blob)||row.blob.size!==size)throw invalid('Missing or corrupt transfer chunk');return row.blob;}
export function validateTransferChunk(offset,bytes){
 if(!integer(offset,0,TRANSFER_STORAGE_BYTES)||offset%TRANSFER_CHUNK_BYTES!==0||!(bytes instanceof Uint8Array)||bytes.byteLength<1||bytes.byteLength>TRANSFER_CHUNK_BYTES)throw invalid('Invalid transfer chunk offset or bound');
}
export function createTransferStaging({db,namespace,epoch,enqueue}){
 // Preparation runs synchronously after the queue pressure/closing precheck.
 const submit=(prepare,task)=>enqueue(task,true,prepare);
 return {
  list(){return submit(()=>undefined,()=>transaction(db,'readonly',r=>r.entries.filter(e=>e.namespace===namespace).map(publicHandle)));},
  open(value){return submit(()=>descriptor(value),d=>transaction(db,'readwrite',(r,store)=>{
   let entry=r.entries.find(e=>e.namespace===namespace&&e.id===d.id);
   if(entry){if(!same(entry,d))throw invalid('Transfer descriptor conflict');entry.epoch=epoch;}
   else{
    if(r.entries.length>=MAX_STAGES||r.entries.filter(e=>e.namespace===namespace).length>=MAX_NAMESPACE_STAGES||r.entries.reduce((n,e)=>n+e.totalBytes,0)+d.totalBytes>TRANSFER_STORAGE_BYTES)throw pressure('Transfer staging reservation exhausted; finish or drop an existing transfer');
    entry={...d,namespace,epoch,nextOffset:0,resumeDigest:d.cutToken};r.entries.push(entry);
   }
   store.put(r);return publicHandle(entry);
  }));},
  // Inert discovery metadata only. Rust authenticates Renew and same-content
  // continuity before requesting this CAS; immutable cleanup identity is unchanged.
  compareAndSwapResumeDigest(value,expectedDigest,nextDigest){return submit(()=>captureTransferRequest('compareAndSwapResumeDigest',[value,expectedDigest,nextDigest]),([h,expected,next])=>transaction(db,'readwrite',(r,store)=>{
   const entry=lookup(r.entries,namespace,epoch,h);
   if((entry.resumeDigest??entry.cutToken)!==expected)throw invalid('Transfer resume digest compare-and-swap conflict');
   entry.resumeDigest=next;store.put(r);return publicHandle(entry);
  }));},
  status(value){return submit(()=>captureTransferHandle(value),h=>transaction(db,'readonly',r=>publicHandle(lookup(r.entries,namespace,epoch,h))));},
  write(value,offset,bytes){return submit(()=>{const h=captureTransferHandle(value);validateTransferChunk(offset,bytes);return {h,offset,blob:new Blob([bytes])};},async({h,offset,blob})=>{
   const previous=await transaction(db,'readwrite',(r,store,tx,fail)=>{
    const e=lookup(r.entries,namespace,epoch,h),size=Math.min(TRANSFER_CHUNK_BYTES,e.totalBytes-offset);
    if(size<=0||blob.size!==size||offset>e.nextOffset)throw invalid('Transfer chunk offset, length or gap');
    if(offset<e.nextOffset){
     const result={blob:null},request=store.get(chunkKey(namespace,e.id,offset/TRANSFER_CHUNK_BYTES));
     request.onsuccess=()=>{try{result.blob=checkedBlob(request.result,size);}catch(error){fail(error);tx.abort();}};return result;
    }
    store.put({id:chunkKey(namespace,e.id,offset/TRANSFER_CHUNK_BYTES),blob});e.nextOffset+=size;store.put(r);return null;
   });
   if(previous){const [a,b]=await Promise.all([previous.blob.arrayBuffer(),blob.arrayBuffer()]);const x=new Uint8Array(a),y=new Uint8Array(b);if(x.some((byte,i)=>byte!==y[i]))throw invalid('Conflicting immutable transfer chunk');}
  });},
  read(value,offset,length){return submit(()=>{const h=captureTransferHandle(value);if(!integer(offset,0,TRANSFER_STORAGE_BYTES)||!integer(length,1,TRANSFER_CHUNK_BYTES)||offset+length>h.totalBytes)throw invalid('Transfer read range exceeds bound');return {h,offset,length};},async({h,offset,length})=>{
   const parts=await transaction(db,'readonly',(r,store,tx,fail)=>{
    const e=lookup(r.entries,namespace,epoch,h);if(offset+length>e.nextOffset)throw invalid('Transfer read range is incomplete');
    const parts=[];for(let i=Math.floor(offset/TRANSFER_CHUNK_BYTES);i<=Math.floor((offset+length-1)/TRANSFER_CHUNK_BYTES);i++){
     const start=i*TRANSFER_CHUNK_BYTES,part={blob:null,start};parts.push(part);const request=store.get(chunkKey(namespace,e.id,i));
     request.onsuccess=()=>{try{part.blob=checkedBlob(request.result,Math.min(TRANSFER_CHUNK_BYTES,e.totalBytes-start));}catch(error){fail(error);tx.abort();}};
    }return parts;
   });
   const result=new Uint8Array(length);for(const {blob,start} of parts){const from=Math.max(offset,start),to=Math.min(offset+length,start+blob.size);result.set(new Uint8Array(await blob.slice(from-start,to-start).arrayBuffer()),from-offset);}return result;
  });},
  drop(value){return submit(()=>captureTransferHandle(value),h=>transaction(db,'readwrite',(r,store)=>{
   const e=lookup(r.entries,namespace,epoch,h);remove(store,r,e);
  }));},
 };
}
function remove(store,r,entry){for(let i=0;i<Math.ceil(entry.nextOffset/TRANSFER_CHUNK_BYTES);i++)store.delete(chunkKey(entry.namespace,entry.id,i));r.entries=r.entries.filter(e=>e!==entry);store.put(r);}
/** Called inside the SAME canonical record transaction, never after it. */
export function commitTransfer(store,tx,{namespace,epoch,handle},applyWrites,fail){
 withRegistry(store,tx,r=>{const e=lookup(r.entries,namespace,epoch,handle);if(e.nextOffset!==e.totalBytes)throw invalid('Incomplete transfer cannot commit');remove(store,r,e);applyWrites();},fail);
}

/** Bound and capture the closed page/worker request before queuing or copying. */
export function captureTransferRequest(method,args){
 const arities={list:0,open:1,status:1,write:3,read:3,drop:1,compareAndSwapResumeDigest:3};
 if(!Object.hasOwn(arities,method)||!Array.isArray(args)||args.length!==arities[method])throw invalid('Invalid transfer storage request');
 if(method==='list')return [];
 if(method==='open')return [descriptor(args[0])];
 const h=captureTransferHandle(args[0]);
 if(method==='compareAndSwapResumeDigest'){if(!opaque(args[1])||!opaque(args[2]))throw invalid('Invalid transfer resume digest bound');return [h,args[1],args[2]];}
 if(method==='write'){validateTransferChunk(args[1],args[2]);return [h,args[1],new Uint8Array(args[2])];}
 if(method==='read'){if(!integer(args[1],0,TRANSFER_STORAGE_BYTES)||!integer(args[2],1,TRANSFER_CHUNK_BYTES)||args[1]+args[2]>h.totalBytes)throw invalid('Transfer read range exceeds bound');return [h,args[1],args[2]];}
 return [h];
}
