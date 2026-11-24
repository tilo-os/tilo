// The worker that writes a page's profile stores to IndexedDB (`peer-files.mjs`): the page hands
// it each commit packed in one buffer it moves rather than copies, so the database's work on the
// parts runs here, off the page thread.
import {applyPacked,openFilesDatabase,readPacked} from './peer-files-db.mjs';

/** Serve `peer-files.mjs` requests arriving on `port` (`{id, type, namespace, packed}`):
 * `open` answers the namespace's rows packed, `commit` resolves once its transaction completed,
 * `close` lets the database go. Each answer is `{id, ok, result}` or `{id, ok: false, error}`. */
export function servePeerFiles(port,{indexedDB=globalThis.indexedDB,IDBKeyRange=globalThis.IDBKeyRange}={}){
  const open=new Map();
  const database=namespace=>{
    const db=open.get(namespace);
    if(!db)throw Object.assign(Error('the profile\'s files are not open'),{name:'InvalidStateError'});
    return db;
  };
  port.addEventListener('message',async({data:{id,type,namespace,packed}})=>{
    try{
      if(type==='open'){
        const db=open.get(namespace)??await openFilesDatabase(namespace,indexedDB);
        open.set(namespace,db);
        const rows=await readPacked(db);
        port.postMessage({id,ok:true,result:rows},[rows.buffer]);
      }else if(type==='commit'){
        await applyPacked(database(namespace),packed,{IDBKeyRange});
        port.postMessage({id,ok:true});
      }else if(type==='close'){
        open.get(namespace)?.close();open.delete(namespace);
        port.postMessage({id,ok:true});
      }else throw Error(`no request ${type}`);
    }catch(error){
      port.postMessage({id,ok:false,error:{name:error?.name??'Error',message:String(error?.message??error)}});
    }
  });
  port.start?.();
}

if(typeof WorkerGlobalScope!=='undefined'&&globalThis instanceof WorkerGlobalScope)servePeerFiles(globalThis);
