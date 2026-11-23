// Dedicated persistence worker: owns the writer lock, the IndexedDB connection
// and every strict transaction for the namespaces the page opens, so the page
// pays one postMessage per commit instead of the transaction and its callbacks.
import {createInlineStorage} from './storage-inline.mjs';
import {captureTransferRequest} from './transfer-staging.mjs';
import {packBinary,serializeError,unsupported} from './persistence-protocol.mjs';

/** Serve the storage protocol on a port-like object (`self` in a worker, a
 * MessagePort in tests). One FIFO queue per namespace orders peer batches,
 * private-action writes and close writes exactly as they were posted. */
export function servePersistence(port,{locks,indexedDB,crypto}={}){
  const storage=createInlineStorage({locks,indexedDB,crypto});
  const leases=new Map();
  const reply=(id,result,transfer=[])=>port.postMessage({id,ok:true,result},transfer);
  const fail=(id,error)=>port.postMessage({id,ok:false,error:serializeError(error)});
  const lookup=namespace=>{const entry=leases.get(namespace);if(!entry||entry.closing)throw Object.assign(Error('storage lease is not open in the persistence worker'),{name:'InvalidStateError'});return entry;};
  const enqueue=(entry,task)=>{const run=entry.queue.catch(()=>{}).then(task);entry.queue=run;return run;};
  async function handle(message){
    const {id,type,namespace}=message??{};
    try{
      switch(type){
        case 'open':{
          if(!locks)throw unsupported('Web Locks are unavailable in the persistence worker');
          if(!indexedDB)throw unsupported('IndexedDB is unavailable in the persistence worker');
          const lease=await storage.acquireStorage(message.profile,message.device);
          if(!leases.has(lease.namespace))leases.set(lease.namespace,{lease,queue:Promise.resolve(),staging:0,closing:false});
          reply(id,{namespace:lease.namespace,epoch:lease.epoch});return;
        }
        case 'hydrate':{
          const entry=lookup(namespace);
          const {rows,transfer}=packBinary(await enqueue(entry,()=>entry.lease.hydrate()));
          reply(id,rows,transfer);return;
        }
        case 'commit':{
          const entry=lookup(namespace);
          await enqueue(entry,()=>{if(message.abortNext)entry.lease.abortNext=true;return entry.lease.commit(message.writes,message.options);});
          reply(id);return;
        }
        case 'transfer':{
          const entry=lookup(namespace),{method,args}=message;
          if(entry.staging>=2)throw Object.assign(Error('Transfer staging has two pending operations'),{name:'QuotaExceededError'});
          const captured=captureTransferRequest(method,args);entry.staging++;
          try{const result=await enqueue(entry,()=>entry.lease.transfers[method](...captured));reply(id,result,result instanceof Uint8Array?[result.buffer]:[]);}
          finally{entry.staging--;}
          return;
        }
        case 'release':{
          const entry=leases.get(namespace);
          if(entry){if(entry.lease.references===1)entry.closing=true;await enqueue(entry,()=>entry.lease.release());if(entry.lease.closing)leases.delete(namespace);}
          reply(id);return;
        }
        case 'flush':{
          const entries=namespace?[lookup(namespace)]:[...leases.values()];
          await Promise.allSettled(entries.map(entry=>entry.queue));
          reply(id);return;
        }
        default:throw Error(`unknown storage request: ${String(type)}`);
      }
    }catch(error){fail(id,error);}
  }
  port.onmessage=event=>{void handle(event.data);};
}

if(typeof WorkerGlobalScope!=='undefined'&&globalThis instanceof WorkerGlobalScope){
  servePersistence(globalThis,{locks:globalThis.navigator?.locks,indexedDB:globalThis.indexedDB,crypto:globalThis.crypto});
}
