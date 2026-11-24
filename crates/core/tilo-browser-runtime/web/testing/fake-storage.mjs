// Node-only doubles for the persistence tests. Not packaged: build.sh copies
// web/*.mjs only, and node --test runs web/*.test.mjs only.
//
// - fakeIndexedDB: databases shared between "agents" (pages), one
//   transaction at a time per database, request callbacks on later ticks, an
//   auto-commit that tests can check, and a put failure hook.
// - fakeLocks: a Web Locks manager shared between agents; a terminated agent
//   releases its locks asynchronously like a real user agent.

const later=()=>new Promise(resolve=>setImmediate(resolve));
const keyId=key=>JSON.stringify(key);
const compareKeys=(a,b)=>{
 if(Array.isArray(a)!==Array.isArray(b))return Array.isArray(a)?1:-1;
 if(Array.isArray(a)){for(let i=0;i<Math.min(a.length,b.length);i++){const order=compareKeys(a[i],b[i]);if(order)return order;}return a.length-b.length;}
 return a<b?-1:a>b?1:0;
};
const domError=(name,message=`${name} simulated`)=>Object.assign(Error(message),{name});
/** The key ranges the fake reads: `IDBKeyRange.bound` only. */
export const fakeKeyRange={bound:(lower,upper)=>({range:true,lower,upper})};
const inRange=(range,key)=>!range||(compareKeys(key,range.lower)>=0&&compareKeys(key,range.upper)<=0);

export function fakeIndexedDB(){
  const databases=new Map(),log=[],hooks={commitHold:null,failPut:null};
  function createTransaction(db,owner,mode,options){
    const tx=new EventTarget();
    const entry={owner,mode,durability:options.durability??'default',outcome:'pending',keys:[],reads:[]};log.push(entry);
    tx.mode=mode;tx.durability=entry.durability;tx.error=null;
    const staged=[],queue=[];let started=false,finished=false,pumping=false,finishRun;
    const ran=new Promise(resolve=>{finishRun=resolve;});
    const previous=db.queue;db.queue=ran;
    const finish=(kind,error)=>{
      if(finished)return;finished=true;
      if(kind==='complete'){for(const [store,key,value] of staged){if(value===undefined)store.rows.delete(keyId(key));else store.rows.set(keyId(key),value);}}
      else tx.error=error??null;
      entry.outcome=kind;tx.dispatchEvent(new Event(kind));finishRun();
    };
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
      if(hooks.commitHold){const check=hooks.commitHold;hooks.commitHold=null;check.reached();await check.released;if(finished)return;}
      finish('complete');
    };
    const schedule=(work,request={onsuccess:null,onerror:null,result:undefined,error:null})=>{queue.push({request,work});void pump();return request;};
    tx.objectStore=name=>{
      const store=db.stores.get(name);if(!store)throw domError('NotFoundError',`no store ${name}`);
      const keyOf=value=>Array.isArray(store.keyPath)?store.keyPath.map(path=>value[path]):value[store.keyPath];
      const current=()=>{const rows=new Map(store.rows);for(const [target,key,value] of staged)if(target===store){if(value===undefined)rows.delete(keyId(key));else rows.set(keyId(key),value);}return rows;};
      return {
        get:key=>schedule(()=>{entry.reads.push(structuredClone(key));return structuredClone(current().get(keyId(key)));}),
        getAll:range=>schedule(()=>{entry.reads.push(range??'*');return [...current().values()].filter(value=>inRange(range,keyOf(value))).map(value=>structuredClone(value));}),
        openKeyCursor(){
          let previous,first=true;const request={onsuccess:null,onerror:null,result:undefined,error:null};
          const advance=target=>schedule(()=>{
            const keys=[...current().values()].map(keyOf).sort(compareKeys);
            const key=keys.find(key=>(first||compareKeys(key,previous)>0)&&(target===undefined||compareKeys(key,target)>=0));
            first=false;if(key===undefined)return null;previous=key;
            return {key:structuredClone(key),continue:next=>{advance(next);}};
          },request);
          advance();return request;
        },
        delete(key){
          if(mode!=='readwrite')throw domError('ReadOnlyError');
          return schedule(()=>{
            const keys=key?.range?[...current().values()].map(keyOf).filter(found=>inRange(key,found)):[key];
            for(const found of keys){staged.push([store,found,undefined]);entry.keys.push(found);}
          });
        },
        put(value){
          if(mode!=='readwrite')throw domError('ReadOnlyError');
          return schedule(()=>{
            if(hooks.failPut){const error=hooks.failPut;hooks.failPut=null;throw error;}
            const key=keyOf(value);staged.push([store,key,structuredClone(value)]);entry.keys.push(key);return key;
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
      objectStoreNames:{contains:name=>db.stores.has(name)},
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
        let aborted=false;
        request.transaction={abort(){aborted=true;}};
        if(fresh)request.onupgradeneeded?.({target:request});
        if(aborted){databases.delete(name);request.error=domError('AbortError');request.onerror?.({target:request});return;}
        request.onsuccess?.({target:request});
      });
      return request;
    },
  });
  return {
    ...agent('page'),
    databases:async()=>[...databases.keys()].map(name=>({name,version:1})),
    transactions:()=>log,
    holdNextCommit(){let reached,release;const held=new Promise(resolve=>{reached=resolve;}),released=new Promise(resolve=>{release=resolve;});hooks.commitHold={reached,released};return {held,release};},
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
        options.signal?.throwIfAborted();
        await new Promise((resolve,reject)=>{
          waiters.set(name,[...(waiters.get(name)??[]),resolve]);
          options.signal?.addEventListener('abort',()=>reject(options.signal.reason),{once:true});
        });
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
