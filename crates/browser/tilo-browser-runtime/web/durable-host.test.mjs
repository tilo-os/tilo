import {test} from 'node:test';
import assert from 'node:assert/strict';
import {createDurableHost} from './durable-host.mjs';
import {createStorageAccess} from './storage.mjs';
import {createInlineStorage} from './storage-inline.mjs';
import {fakeIndexedDB,fakeLocks} from './testing/fake-storage.mjs';

const settle=async(rounds=40)=>{for(let i=0;i<rounds;i++)await new Promise(resolve=>setImmediate(resolve));};
const WRITER='tilo-writer:["p","d"]';

/** The wasm storage exports over a model of durable_store::MemoryStore::queued:
 * drains carry each key's whole current value, acknowledgements settle
 * revisions, retry re-queues failed keys. */
function fakeRuntime(){
  const stores=new Map(),log={acks:[],refreshed:0};
  const store=(profile,device)=>{const s=stores.get(JSON.stringify([profile,device]));if(!s)throw Error('unknown storage namespace');return s;};
  return {log,
    store,
    write(profile,device,key,text){const s=store(profile,device);s.revision++;s.current.set(key,text);s.queued.set(key,s.revision);s.hook?.();},
    refresh_storage_lease(profile,device){log.refreshed++;stores.delete(JSON.stringify([profile,device]));},
    load_storage(profile,device,rows){
      const key=JSON.stringify([profile,device]);if(stores.has(key))return;
      stores.set(key,{revision:0,current:new Map(rows.map(row=>[row.key,new TextDecoder().decode(row.bytes)])),queued:new Map(),inFlight:new Map(),failed:new Set(),hook:null,settings:null});
    },
    drain_storage(profile,device){
      const s=store(profile,device);
      const rows=[...s.queued].map(([key,revision])=>({key,bytes:new TextEncoder().encode(s.current.get(key)),actor:null,revision:String(revision)}));
      s.queued.clear();for(const row of rows)s.inFlight.set(row.revision,row.key);return rows;
    },
    acknowledge_storage(profile,device,revision,failure){
      const s=store(profile,device),key=s.inFlight.get(String(revision));
      if(key===undefined)throw Error(`revision ${revision} is not awaiting acknowledgement`);
      s.inFlight.delete(String(revision));failure===undefined?s.failed.delete(key):s.failed.add(key);log.acks.push([String(revision),failure]);
    },
    retry_storage(profile,device){const s=store(profile,device);let queued=0;for(const key of s.failed)if(!s.queued.has(key)){s.revision++;s.queued.set(key,s.revision);queued++;}return queued;},
    storage_write_hook(profile,device,hook){store(profile,device).hook=hook??null;},
    provide_settings(profile,device,json){const s=store(profile,device);if(s.current.has('settings'))return false;s.current.set('settings',json);return true;},
  };
}

function harness(){
  const idb=fakeIndexedDB(),locks=fakeLocks(),runtime=fakeRuntime();
  const access=createStorageAccess({createWorker:()=>null,inline:createInlineStorage({locks:locks.manager('page'),indexedDB:idb})});
  const host=createDurableHost({runtime,acquireStorage:access.acquireStorage,events:null});
  const stored=()=>Object.fromEntries(idb.rows().map(row=>[row.key,new TextDecoder().decode(row.bytes)]));
  return {idb,locks,runtime,host,stored};
}

test('accepted writes commit on their own in strict transactions and are acknowledged',async()=>{
  const h=harness();
  const storage=await h.host.open('p','d');
  assert.deepEqual(h.locks.held(),[WRITER],'one Web Lock per profile/device');
  h.runtime.write('p','d','a','one');h.runtime.write('p','d','b','two');h.runtime.write('p','d','a','three');
  await settle();
  assert.deepEqual(h.stored(),{a:'three',b:'two'});
  assert.ok(h.idb.transactions().filter(tx=>tx.mode==='readwrite').every(tx=>tx.durability==='strict'));
  assert.deepEqual(h.runtime.log.acks.map(([,failure])=>failure),[undefined,undefined],'one commit of both keys, each acknowledged');
  assert.deepEqual(storage.status,{persistence:'committed',pendingWrites:0,error:null});
  await storage.release();
});

test('flush resolves only once the commit is durable',async()=>{
  const h=harness();
  const storage=await h.host.open('p','d');
  const gate=h.idb.gateNextCommit();
  h.runtime.write('p','d','doc','draft');
  let flushed=false;const flush=storage.flush().then(()=>{flushed=true;});
  await gate.held;await settle();
  assert.equal(flushed,false,'not before IndexedDB commits');
  assert.equal(storage.status.persistence,'pending');
  gate.release();await flush;
  assert.deepEqual(h.stored(),{doc:'draft'});
  await storage.release();
});

test('a failed commit is reported, then retried by the next flush',async()=>{
  const h=harness();
  const storage=await h.host.open('p','d');const statuses=[];storage.subscribe(status=>statuses.push(status.persistence));
  h.idb.failNextPut('QuotaExceededError');
  h.runtime.write('p','d','doc','v1');
  await assert.rejects(storage.flush(),/QuotaExceededError/);
  assert.equal(storage.status.persistence,'failed');
  assert.match(h.runtime.log.acks.at(-1)[1],/QuotaExceededError/,'Rust learns the revision failed');
  assert.deepEqual(h.stored(),{});
  await storage.flush();
  assert.deepEqual(h.stored(),{doc:'v1'},'the failed key is committed with its current value');
  assert.deepEqual(statuses,['pending','failed','pending','committed']);
  await storage.release();
});

test('users share one lease; the last release flushes and frees the lock',async()=>{
  const h=harness();
  const [desktop,peer]=await Promise.all([h.host.open('p','d'),h.host.open('p','d')]);
  assert.equal(desktop,peer);
  h.runtime.write('p','d','peer-state','cursor 7');
  await desktop.release();
  assert.deepEqual(h.locks.held(),[WRITER],'the peer still holds the profile');
  h.runtime.write('p','d','peer-state','cursor 8');
  await peer.release();
  assert.deepEqual(h.stored(),{'peer-state':'cursor 8'});
  assert.deepEqual(h.locks.held(),[]);
  const reopened=await h.host.open('p','d');
  assert.equal(h.runtime.log.refreshed,2,'every fresh lease hydrates afresh');
  await reopened.release();
});

test('a release whose flush fails keeps custody until retried',async()=>{
  const h=harness();
  const storage=await h.host.open('p','d');
  h.idb.failNextPut('AbortError');
  h.runtime.write('p','d','doc','unsaved');
  await assert.rejects(storage.release(),/AbortError/);
  assert.deepEqual(h.locks.held(),[WRITER],'the writer lock is kept with the unsaved write');
  await storage.release();
  assert.deepEqual(h.stored(),{doc:'unsaved'});
  assert.deepEqual(h.locks.held(),[]);
});

test('host settings seed only a profile without stored settings',async()=>{
  const h=harness();
  const storage=await h.host.open('p','d',{settings:{appearance:{theme:'light'}}});
  assert.equal(h.runtime.store('p','d').current.get('settings'),'{"appearance":{"theme":"light"}}');
  const again=await h.host.open('p','d',{settings:{appearance:{theme:'dark'}}});
  assert.equal(h.runtime.store('p','d').current.get('settings'),'{"appearance":{"theme":"light"}}');
  await again.release();await storage.release();
  assert.deepEqual(h.stored(),{},'a provided document is not written back until it changes');
});
