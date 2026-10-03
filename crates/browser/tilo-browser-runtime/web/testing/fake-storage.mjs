// Node-only doubles for the persistence tests. Not packaged: build.sh copies
// web/*.mjs only, and node --test runs web/*.test.mjs only.
//
// - fakeIndexedDB: databases shared between "agents" (page, workers), one
//   transaction at a time per database, request callbacks on later ticks, an
//   auto-commit that tests can gate, and a put failure hook.
// - fakeLocks: a Web Locks manager shared between agents; a terminated agent
//   releases its locks asynchronously like a real user agent.
// - fakeWorkers: Worker-shaped objects over MessageChannel that run the real
//   persistence worker (structured clone and transfer included).
import {servePersistence} from '../persistence-worker.mjs';

const later=()=>new Promise(resolve=>setImmediate(resolve));
const keyId=key=>JSON.stringify(key);
const compareKeys=(a,b)=>{
 if(Array.isArray(a)!==Array.isArray(b))return Array.isArray(a)?1:-1;
 if(Array.isArray(a)){for(let i=0;i<Math.min(a.length,b.length);i++){const order=compareKeys(a[i],b[i]);if(order)return order;}return a.length-b.length;}
 return a<b?-1:a>b?1:0;
};
const domError=(name,message=`${name} simulated`)=>Object.assign(Error(message),{name});

export function fakeIndexedDB(){
  const databases=new Map(),log=[],hooks={commitGate:null,failPut:null},pending=new Set();
  function createTransaction(db,owner,mode,options){
    const tx=new EventTarget();
    const entry={owner,mode,durability:options.durability??'default',outcome:'pending',keys:[],reads:[]};log.push(entry);
    tx.mode=mode;tx.durability=entry.durability;tx.error=null;
    const staged=[],queue=[];let started=false,finished=false,pumping=false,finishRun;
    const ran=new Promise(resolve=>{finishRun=resolve;});
    const previous=db.queue;db.queue=ran;
    const finish=(kind,error)=>{
      if(finished)return;finished=true;pending.delete(control);
      if(kind==='complete'){for(const [store,key,value] of staged){if(value===undefined)store.rows.delete(keyId(key));else store.rows.set(keyId(key),value);}}
      else tx.error=error??null;
      entry.outcome=kind;tx.dispatchEvent(new Event(kind));finishRun();
    };
    const control={owner,abort:error=>finish('abort',error)};pending.add(control);
    tx.abort=()=>finish('abort',tx.error);
    const pump=async()=>{
      if(pumping||!started||finished)return;pumping=true;
      while(queue.length&&!finished){
        const {request,work}=queue.shift();
        await later();
        if(finished)break;
        try{request.result=work();request.onsuccess?.({target:request});}
        catch(error){request.error=error;tx.error=error;request.onerror?.({target:request});if(!finished){tx.dispatchEvent(new Event('error'));finish('abort',error);}}
      }
      pumping=false;
      if(finished)return;
      if(queue.length){void pump();return;}
      await later();
      if(finished||pumping)return;
      if(queue.length){void pump();return;}
      if(hooks.commitGate){const gate=hooks.commitGate;hooks.commitGate=null;gate.reached();await gate.released;if(finished)return;}
      finish('complete');
    };
    const schedule=(work,request={onsuccess:null,onerror:null,result:undefined,error:null})=>{queue.push({request,work});void pump();return request;};
    tx.objectStore=name=>{
      const store=db.stores.get(name);if(!store)throw domError('NotFoundError',`no store ${name}`);
      const current=()=>{const rows=new Map(store.rows);for(const [target,key,value] of staged)if(target===store){if(value===undefined)rows.delete(keyId(key));else rows.set(keyId(key),value);}return rows;};
      return {
        get:key=>schedule(()=>{entry.reads.push(structuredClone(key));return structuredClone(current().get(keyId(key)));}),
        getAll:()=>schedule(()=>{entry.reads.push('*');return [...current().values()].map(value=>structuredClone(value));}),
        openKeyCursor(){
          let previous,first=true;const request={onsuccess:null,onerror:null,result:undefined,error:null};
          const advance=target=>schedule(()=>{
            const keys=[...current().values()].map(value=>value[store.keyPath]).sort(compareKeys);
            const key=keys.find(key=>(first||compareKeys(key,previous)>0)&&(target===undefined||compareKeys(key,target)>=0));
            first=false;if(key===undefined)return null;previous=key;
            return {key:structuredClone(key),continue:next=>{advance(next);}};
          },request);
          advance();return request;
        },
        delete(key){if(mode!=='readwrite')throw domError('ReadOnlyError');return schedule(()=>{staged.push([store,key,undefined]);entry.keys.push(key);});},
        put(value){
          if(mode!=='readwrite')throw domError('ReadOnlyError');
          return schedule(()=>{
            if(hooks.failPut){const error=hooks.failPut;hooks.failPut=null;throw error;}
            const key=value[store.keyPath];staged.push([store,key,structuredClone(value)]);entry.keys.push(key);return key;
          });
        },

      };
    };
    previous.then(()=>{started=true;void pump();});
    return tx;
  }
  function connection(db,owner){
    let closed=false;
    return {
      name:db.name,
      createObjectStore(name,{keyPath}){db.stores.set(name,{keyPath,rows:new Map()});},
      close(){closed=true;},
      transaction(_names,mode='readonly',options={}){
        if(closed)throw domError('InvalidStateError','connection closed');
        return createTransaction(db,owner,mode,options);
      },
    };
  }
  const agent=owner=>({
    open(name){
      const request={onsuccess:null,onerror:null,onupgradeneeded:null,result:undefined,error:null};
      queueMicrotask(()=>{
        let db=databases.get(name);const fresh=!db;
        if(fresh){db={name,stores:new Map(),queue:Promise.resolve()};databases.set(name,db);}
        request.result=connection(db,owner);
        if(fresh)request.onupgradeneeded?.({target:request});
        request.onsuccess?.({target:request});
      });
      return request;
    },
  });
  return {
    ...agent('page'),agent,
    /** A terminated agent's connections abort every transaction still pending. */
    terminate(owner){for(const control of [...pending])if(control.owner===owner)control.abort(domError('AbortError','agent terminated'));},
    rows:(name='tilo-browser-records-v1',store='records')=>[...(databases.get(name)?.stores.get(store)?.rows.values()??[])],
    transactions:()=>log,
    gateNextCommit(){let reached,release;const held=new Promise(resolve=>{reached=resolve;}),released=new Promise(resolve=>{release=resolve;});hooks.commitGate={reached,released};return {held,release};},
    failNextPut(name){hooks.failPut=domError(name);},
  };
}

export function fakeLocks(){
  const held=new Map(),waiters=new Map();
  const wake=name=>{const list=waiters.get(name)??[];waiters.delete(name);for(const resolve of list)resolve();};
  const releaseEntry=(name,entry)=>{if(held.get(name)!==entry)return;held.delete(name);wake(name);};
  const manager=owner=>({
    async request(name,options,callback){
      if(typeof options==='function'){callback=options;options={};}
      while(held.has(name)){
        if(options.ifAvailable)return callback(null);
        await new Promise(resolve=>{waiters.set(name,[...(waiters.get(name)??[]),resolve]);});
      }
      const entry={owner};held.set(name,entry);
      try{return await callback({name,mode:options.mode??'exclusive'});}
      finally{releaseEntry(name,entry);}
    },
  });
  return {
    manager,
    terminate(owner){setImmediate(()=>{for(const [name,entry] of [...held])if(entry.owner===owner)releaseEntry(name,entry);});},
    held:()=>[...held.keys()],
    holder:name=>held.get(name)?.owner,
  };
}

export function fakeWorkers({locks,indexedDB,crypto=globalThis.crypto,prefix='worker',workerLocks=true}){
  const workers=[];
  class FakeWorker extends EventTarget{
    constructor(index){
      super();this.owner=`${prefix}-${index}`;this.terminated=false;this.posted=[];this.replies=[];
      const channel=new MessageChannel();this.channel=channel;
      channel.port1.onmessage=event=>{this.replies.push(event.data);this.dispatchEvent(new MessageEvent('message',{data:event.data}));};
      servePersistence(channel.port2,{locks:workerLocks?locks.manager(this.owner):undefined,indexedDB:indexedDB.agent(this.owner),crypto});
      channel.port1.unref();channel.port2.unref();
    }
    postMessage(message,transfer=[]){this.posted.push(structuredClone(message));this.channel.port1.postMessage(message,transfer);}
    terminate(){
      if(this.terminated)return;this.terminated=true;
      this.channel.port1.close();this.channel.port2.close();
      locks.terminate(this.owner);indexedDB.terminate(this.owner);
    }
    /** The worker stops answering; the page observes only the error event. */
    crash(message='simulated worker crash'){
      this.channel.port2.onmessage=null;this.channel.port1.onmessage=null;
      this.dispatchEvent(Object.assign(new Event('error'),{message}));
    }
  }
  return {
    createWorker(){const worker=new FakeWorker(workers.length);workers.push(worker);return worker;},
    workers,
    dispose(){for(const worker of workers)worker.terminate();},
  };
}

/** Requests posted by the page, without ids, for protocol assertions. */
export const postedTypes=worker=>worker.posted.map(message=>message.type);
