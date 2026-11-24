// The database a profile's peer store keeps its files in (`src/page_files.rs`): one per profile
// namespace, one row per part of a file, `{path, part, bytes}` keyed by path and part. What moves
// between the database and the runtime is packed in one buffer, so the page thread copies it once
// and hands it over rather than handing the database each part.
import {requestResult,transactionComplete} from './idb-transaction.mjs';

export const DATABASE_PREFIX='tilo-peer-files:';
export const databaseName=namespace=>`${DATABASE_PREFIX}${namespace}`;

/** Open `namespace`'s database, made when it is not there. */
export async function openFilesDatabase(namespace,indexedDB=globalThis.indexedDB){
  const request=indexedDB.open(databaseName(namespace),1);
  request.onupgradeneeded=()=>request.result.createObjectStore('parts',{keyPath:['path','part']});
  return requestResult(request);
}

const encoder=new TextEncoder(),decoder=new TextDecoder('utf-8',{fatal:true});

/** `rows` packed as the runtime reads them: per row its path (u32 length, UTF-8), part (u64),
 * length (u32) and bytes, little-endian. */
export function packRows(rows){
  const named=rows.map(row=>[encoder.encode(row.path),row.part,new Uint8Array(row.bytes)]);
  const packed=new Uint8Array(named.reduce((size,[path,,bytes])=>size+16+path.length+bytes.length,0));
  const view=new DataView(packed.buffer);
  let at=0;
  for(const [path,part,bytes] of named){
    view.setUint32(at,path.length,true);packed.set(path,at+4);at+=4+path.length;
    view.setBigUint64(at,BigInt(part),true);view.setUint32(at+8,bytes.length,true);
    packed.set(bytes,at+12);at+=12+bytes.length;
  }
  return packed;
}

/** Every row of the database, packed ([`packRows`]). */
export async function readPacked(db){
  const tx=db.transaction('parts','readonly'),done=transactionComplete(tx);
  const rows=await requestResult(tx.objectStore('parts').getAll());
  await done;
  return packRows(rows);
}

/** The rows of `path` alone, its bytes put back together; `null` when it has none. */
export async function readFile(db,path,{IDBKeyRange=globalThis.IDBKeyRange}={}){
  const tx=db.transaction('parts','readonly'),done=transactionComplete(tx);
  const rows=await requestResult(tx.objectStore('parts').getAll(IDBKeyRange.bound([path,0],[path,Infinity])));
  await done;
  if(!rows.length)return null;
  rows.sort((a,b)=>a.part-b.part);
  const bytes=new Uint8Array(rows.reduce((size,row)=>size+row.bytes.byteLength,0));
  let at=0;for(const row of rows){bytes.set(new Uint8Array(row.bytes),at);at+=row.bytes.byteLength;}
  return bytes;
}

/** Write a packed commit (`src/page_files.rs` `pack_file`) in one strict transaction: per file its
 * parts from where it ends on go, then the parts it carries are put. Resolves once the transaction
 * completed. */
export async function applyPacked(db,packed,{IDBKeyRange=globalThis.IDBKeyRange}={}){
  const tx=db.transaction('parts','readwrite',{durability:'strict'}),done=transactionComplete(tx);
  const parts=tx.objectStore('parts'),view=new DataView(packed.buffer,packed.byteOffset,packed.byteLength);
  let at=0;
  try{
    while(at<packed.byteLength){
      const length=view.getUint32(at,true),path=decoder.decode(packed.subarray(at+4,at+4+length));at+=4+length;
      const end=Number(view.getBigUint64(at,true)),count=view.getUint32(at+8,true);at+=12;
      parts.delete(IDBKeyRange.bound([path,end],[path,Infinity]));
      for(let i=0;i<count;i++){
        const part=Number(view.getBigUint64(at,true)),size=view.getUint32(at+8,true);
        // A copy of its own: a view would store the whole commit's buffer with each part.
        parts.put({path,part,bytes:packed.slice(at+12,at+12+size)});at+=12+size;
      }
    }
  }catch(error){tx.abort();await done.catch(()=>{});throw error;}
  await done;
}
