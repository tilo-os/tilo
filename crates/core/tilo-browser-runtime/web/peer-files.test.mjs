import {test} from 'node:test';
import assert from 'node:assert/strict';
import {applyPacked,openFilesDatabase,readPacked} from './peer-files-db.mjs';
import {servePeerFiles} from './peer-files-worker.mjs';
import {fakeIndexedDB,fakeKeyRange as IDBKeyRange} from './testing/fake-storage.mjs';

/** A commit as the runtime packs it (`src/page_files.rs`): per file its path, where its parts
 * end, and the parts it carries. */
function commit(files){
  const out=[],u32=n=>{const b=new Uint8Array(4);new DataView(b.buffer).setUint32(0,n,true);return b;};
  const u64=n=>{const b=new Uint8Array(8);new DataView(b.buffer).setBigUint64(0,BigInt(n),true);return b;};
  for(const [path,end,parts] of files){
    const name=new TextEncoder().encode(path);
    out.push(u32(name.length),name,u64(end),u32(parts.length));
    for(const [part,bytes] of parts)out.push(u64(part),u32(bytes.length),Uint8Array.from(bytes));
  }
  const packed=new Uint8Array(out.reduce((n,b)=>n+b.length,0));let at=0;for(const b of out){packed.set(b,at);at+=b.length;}
  return packed;
}

/** Packed rows read back as `{path: {part: [bytes]}}`. */
function rows(packed){
  const view=new DataView(packed.buffer,packed.byteOffset,packed.byteLength),files={};let at=0;
  while(at<packed.byteLength){
    const len=view.getUint32(at,true),path=new TextDecoder().decode(packed.subarray(at+4,at+4+len));at+=4+len;
    const part=Number(view.getBigUint64(at,true)),size=view.getUint32(at+8,true);
    (files[path]??={})[part]=[...packed.subarray(at+12,at+12+size)];at+=12+size;
  }
  return files;
}

test('a commit writes its parts in one strict transaction, and what is past a file\'s end goes', async () => {
  const indexedDB=fakeIndexedDB();
  const db=await openFilesDatabase('["p","d"]',indexedDB);
  assert.deepEqual(rows(await readPacked(db)),{},'a new namespace holds nothing');
  await applyPacked(db,commit([['workspaces/a/0.pack',3,[[0,[1]],[1,[2]],[2,[3]]]],['state/page/x',1,[[0,[9]]]]]),{IDBKeyRange});
  // A commit resolves only once its transaction completed.
  const held=indexedDB.holdNextCommit();
  let committed=false;
  const second=applyPacked(db,commit([['workspaces/a/0.pack',1,[[0,[7]]]],['state/page/x',0,[]]]),{IDBKeyRange}).then(()=>{committed=true;});
  await held.held;
  assert.equal(committed,false,'nothing is saved before IndexedDB committed it');
  held.release();
  await second;
  assert.ok(indexedDB.transactions().filter(tx=>tx.mode==='readwrite').every(tx=>tx.durability==='strict'));
  db.close();
  const again=await openFilesDatabase('["p","d"]',indexedDB);
  assert.deepEqual(rows(await readPacked(again)),{'workspaces/a/0.pack':{0:[7]}});
  const other=await openFilesDatabase('["p","e"]',indexedDB);
  assert.deepEqual(rows(await readPacked(other)),{},'another namespace holds none of them');
});

test('a commit that fails leaves what was committed before it', async () => {
  const indexedDB=fakeIndexedDB();
  const db=await openFilesDatabase('n',indexedDB);
  await applyPacked(db,commit([['state/a',1,[[0,[1]]]]]),{IDBKeyRange});
  indexedDB.failNextPut('QuotaExceededError');
  await assert.rejects(applyPacked(db,commit([['state/a',1,[[0,[2]]]],['state/b',1,[[0,[3]]]]]),{IDBKeyRange}),{name:'QuotaExceededError'});
  assert.deepEqual(rows(await readPacked(db)),{'state/a':{0:[1]}});
});

test('the worker opens a namespace, writes its commits and hands back what it keeps', async () => {
  const indexedDB=fakeIndexedDB(),channel=new MessageChannel();
  servePeerFiles(channel.port2,{indexedDB,IDBKeyRange});
  let next=0;
  const ask=(message,transfer=[])=>new Promise((resolve,reject)=>{
    const id=++next;
    const listener=({data})=>{if(data.id!==id)return;channel.port1.removeEventListener('message',listener);data.ok?resolve(data.result):reject(Object.assign(Error(data.error.message),{name:data.error.name}));};
    channel.port1.addEventListener('message',listener);channel.port1.start();
    channel.port1.postMessage({...message,id},transfer);
  });
  try{
    assert.deepEqual(rows(await ask({type:'open',namespace:'n'})),{});
    const packed=commit([['state/peer/ab/record',1,[[0,[5,6]]]]]);
    await ask({type:'commit',namespace:'n',packed},[packed.buffer]);
    assert.equal(packed.byteLength,0,'the commit was moved to the worker, not copied');
    await assert.rejects(ask({type:'commit',namespace:'other',packed:commit([])}),{name:'InvalidStateError'});
    await ask({type:'close',namespace:'n'});
    assert.deepEqual(rows(await ask({type:'open',namespace:'n'})),{'state/peer/ab/record':{0:[5,6]}});
  }finally{channel.port1.close();channel.port2.close();}
});
