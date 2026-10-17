import test from 'node:test';
import assert from 'node:assert/strict';
import {acknowledgedMerge,acknowledgedSeal,acknowledgedSecret,deleteSecret,flushAcknowledgements,flushSecrets,getOrCreateSecret,getSecret,indexedDbStorage,loadSecrets,pendingAcknowledgements,putSecret,queueSecret} from './secrets.mjs';

// These tests are single-writer unless they supply a competing lock manager.
const locks={request:async(_name,fn)=>fn()};

// Storage that keeps structured values in maps, as IndexedDB would.
function memory(){
  const stores={keys:new Map(),items:new Map()};
  const id=key=>JSON.stringify(key);
  return {
    stores,
    get:async(store,key)=>stores[store].get(id(key)),
    put:async(store,key,value)=>{stores[store].set(id(key),value);},
    delete:async(store,key)=>{stores[store].delete(id(key));},
    entries:async store=>[...stores[store]].map(([key,value])=>[JSON.parse(key),value]),
  };
}

test('concurrent vault key initialization returns one durable winner',async()=>{
  let tail=Promise.resolve();
  const serialized={request:(_name,fn)=>{
    const result=tail.then(fn);tail=result.catch(()=>{});return result;
  }};
  const storage=memory(),env={storage,locks:serialized};
  const [a,b]=await Promise.all([
    getOrCreateSecret('Tilo','account-vault',new Uint8Array([1,2,3]),env),
    getOrCreateSecret('Tilo','account-vault',new Uint8Array([4,5,6]),env),
  ]);
  assert.deepEqual(a,b);
  assert.deepEqual(await getSecret('Tilo','account-vault',env),a);
  assert.deepEqual(await getOrCreateSecret('Tilo','account-vault',null,env),a);
});

test('missing existing custody and failed key writes never acknowledge creation',async()=>{
  const storage=memory(),env={storage,locks};
  await assert.rejects(getOrCreateSecret('Tilo','account-vault',null,env),/recovery is required/);
  const original=storage.put;
  storage.put=async(store,...args)=>{
    if(store==='items')throw Error('key commit refused');
    return original(store,...args);
  };
  await assert.rejects(getOrCreateSecret('Tilo','account-vault',new Uint8Array([7]),env),/key commit refused/);
  assert.equal(await getSecret('Tilo','account-vault',env),null);
  storage.put=original;
  assert.deepEqual(await getOrCreateSecret('Tilo','account-vault',new Uint8Array([8]),env),new Uint8Array([8]));
});

test('secrets round-trip, replace and delete under a non-extractable key',async()=>{
  const storage=memory(),env={storage,locks};
  assert.equal(await getSecret('so.tilo.totp','vault',env),null);
  await putSecret('so.tilo.totp','vault',new Uint8Array([0,1,255]),env);
  assert.deepEqual([...await getSecret('so.tilo.totp','vault',env)],[0,1,255]);
  const key=storage.stores.keys.get(JSON.stringify('so.tilo.totp'));
  assert.equal(key.extractable,false,'the browser must refuse to export the key');
  const record=storage.stores.items.get(JSON.stringify(['so.tilo.totp','vault']));
  assert.ok(!Buffer.from(record.data).includes(Buffer.from([0,1,255])),'stored bytes are ciphertext');
  await putSecret('so.tilo.totp','vault',new TextEncoder().encode('second'),env);
  assert.equal(new TextDecoder().decode(await getSecret('so.tilo.totp','vault',env)),'second');
  await deleteSecret('so.tilo.totp','vault',env);
  assert.equal(await getSecret('so.tilo.totp','vault',env),null);
  await deleteSecret('so.tilo.totp','vault',env);
});

test('a ciphertext moved to another app or key does not decrypt',async()=>{
  const storage=memory(),env={storage,locks};
  await putSecret('so.tilo.a','token',new TextEncoder().encode('secret'),env);
  await putSecret('so.tilo.a','other',new TextEncoder().encode('x'),env);
  const record=storage.stores.items.get(JSON.stringify(['so.tilo.a','token']));
  storage.stores.items.set(JSON.stringify(['so.tilo.a','other']),record);
  await assert.rejects(getSecret('so.tilo.a','other',env));
  await assert.rejects(loadSecrets(env),'damaged records must not disappear from the mirror');
});

test('request success followed by transaction abort is not acknowledged as custody',async()=>{
  const database={transaction:()=>{
    const tx={objectStore:()=>({put:()=>{
      const request={result:undefined};
      queueMicrotask(()=>{
        request.onsuccess();
        queueMicrotask(()=>{tx.error=Error('commit refused');tx.onabort();});
      });
      return request;
    }})};
    return tx;
  }};
  const idb={open:()=>{
    const request={result:database};
    queueMicrotask(()=>request.onsuccess());
    return request;
  }};
  await assert.rejects(indexedDbStorage(idb).put('items','key',new Uint8Array([1])),/commit refused/);
});

test('writers of one app hold its lock',async()=>{
  const held=[];
  const locks={request:async(name,fn)=>{held.push(name);return fn();}};
  const env={storage:memory(),locks};
  await putSecret('so.tilo.a','token',new Uint8Array([1]),env);
  await deleteSecret('so.tilo.a','token',env);
  assert.deepEqual(held,['tilo-secrets:so.tilo.a','tilo-secrets:so.tilo.a']);
});

test('the mirror rejects missing custody and queued writes land in order',async()=>{
  const storage=memory(),env={storage,locks};
  await putSecret('so.tilo.totp','vault',new Uint8Array([7]),env);
  await putSecret('so.tilo.passwords','vault',new Uint8Array([8]),env);
  assert.equal((await loadSecrets(env)).length,2);
  const passwordKey=storage.stores.keys.get(JSON.stringify('so.tilo.passwords'));
  storage.stores.keys.delete(JSON.stringify('so.tilo.passwords'));
  await assert.rejects(loadSecrets(env),/recovery is required/);
  const before=[...storage.stores.items];
  await assert.rejects(putSecret('so.tilo.passwords','other',new Uint8Array([9]),env),/recovery is required/);
  assert.deepEqual([...storage.stores.items],before,'failed custody must not replace ciphertext');
  assert.equal(storage.stores.keys.has(JSON.stringify('so.tilo.passwords')),false);
  storage.stores.keys.set(JSON.stringify('so.tilo.passwords'),passwordKey);
  queueSecret('so.tilo.totp','vault',new Uint8Array([1]),env);
  queueSecret('so.tilo.totp','vault',new Uint8Array([2]),env);
  queueSecret('so.tilo.totp','other',null,env);
  await flushSecrets();
  assert.deepEqual([...await getSecret('so.tilo.totp','vault',env)],[2]);
  const borrowed=new Uint8Array([3]);
  queueSecret('so.tilo.totp','vault',borrowed,env);
  borrowed[0]=99;
  await flushSecrets();
  assert.deepEqual([...await getSecret('so.tilo.totp','vault',env)],[3]);

  const failed={...env,storage:{...storage,put:async()=>{throw Error('quota exceeded');}}};
  queueSecret('so.tilo.totp','vault',new Uint8Array([4]),failed);
  await assert.rejects(flushSecrets(),/quota exceeded/);
  queueSecret('so.tilo.totp','vault',new Uint8Array([5]),env);
  await assert.rejects(flushSecrets(),/quota exceeded/);
  assert.deepEqual([...await getSecret('so.tilo.totp','vault',env)],[3]);
});

test('missing crypto, enumeration or cross-tab locking fails closed',async()=>{
  const storage=memory();
  await assert.rejects(loadSecrets({storage,subtle:null}),/WebCrypto/);
  await assert.rejects(loadSecrets({storage:{}}),/enumerate/);
  await assert.rejects(putSecret('so.tilo.a','token',new Uint8Array([1]),{storage,locks:null}),/Web Locks/);
  assert.equal(storage.stores.keys.size,0);
  assert.equal(storage.stores.items.size,0);
});


// A real mutual-exclusion queue, shared by independent simulated pages.
function serializedLocks(){
  let tail=Promise.resolve();
  return {request:(_name,fn)=>{
    const next=tail.then(fn);tail=next.catch(()=>{});return next;
  }};
}

test('credential references are withheld until commit and survive a fresh load',async()=>{
  const storage=memory(),env={storage,locks:serializedLocks()};
  const bytes=new TextEncoder().encode('device-local credential');
  assert.equal(acknowledgedSecret('ack-roundtrip',bytes,env),null);
  assert.equal((await loadSecrets(env)).length,0,'no synchronous custody acknowledgement');
  await flushAcknowledgements();
  const id=acknowledgedSecret('ack-roundtrip',bytes,env);
  assert.match(id,/^request-auth\//);
  // New storage wrapper/page, without any synchronous mirror.
  const fresh={storage:{...storage},locks:env.locks};
  const restored=await loadSecrets(fresh);
  assert.deepEqual(restored,[[ 'ack-roundtrip',id,bytes ]]);
});

test('credential publication stays pending while the storage commit is outstanding',async()=>{
  const storage=memory();
  let release,started;
  const committing=new Promise(resolve=>{started=resolve;});
  const gate=new Promise(resolve=>{release=resolve;});
  const env={locks:serializedLocks(),storage:{...storage,put:async(store,key,value)=>{
    if(store==='items'){started();await gate;}
    return storage.put(store,key,value);
  }}};
  const bytes=new TextEncoder().encode('unpublished auth');
  assert.equal(acknowledgedSecret('ack-delayed',bytes,env),null);
  await committing;
  assert.equal(acknowledgedSecret('ack-delayed',bytes,env),null,
    'encryption and a started write are not durable custody');
  assert.equal((await loadSecrets({storage,locks:env.locks})).length,0);
  release();
  await flushAcknowledgements();
  const id=acknowledgedSecret('ack-delayed',bytes,env);
  assert.match(id,/^request-auth\//);
  assert.deepEqual(await loadSecrets({storage,locks:env.locks}),[['ack-delayed',id,bytes]]);
});

test('immutable seals retry by namespace and bytes and retain the first key',async()=>{
  const storage=memory(),env={storage,locks:serializedLocks()};
  const bytes=new TextEncoder().encode('local credential');
  assert.equal(acknowledgedSeal('generic-seal','tokens/first',bytes,env),null);
  assert.equal(acknowledgedSeal('generic-seal','tokens/retry',bytes,env),null);
  assert.equal(acknowledgedSeal('generic-seal','other/first',bytes,env),null);
  await flushAcknowledgements();
  assert.equal(acknowledgedSeal('generic-seal','tokens/retry',bytes,env),'tokens/first');
  assert.equal(acknowledgedSeal('generic-seal','other/retry',bytes,env),'other/first');
  assert.equal(await getSecret('generic-seal','tokens/retry',env),null);
  assert.equal((await loadSecrets(env)).length,2,'retries must not duplicate writes');
  assert.equal(acknowledgedSeal('generic-seal','tokens/new',bytes,env),null,
    'consumed acknowledgements must not replay into a new transaction');
  await flushAcknowledgements();
  assert.equal(acknowledgedSeal('generic-seal','tokens/new',bytes,env),'tokens/new');
});

test('page inventories reload and merge under a cross-page lock',async()=>{
  const storage=memory(),env={storage,locks:serializedLocks()};
  const first=JSON.stringify({alice:[null,{password:'one'}]});
  const second=JSON.stringify({bob:[null,{password:'two'}]});
  assert.equal(acknowledgedMerge('ack-inventory','vault.entries','{}',first,env),null);
  assert.equal(acknowledgedMerge('ack-inventory','vault.entries','{}',second,env),null);
  await flushAcknowledgements();
  assert.ok(acknowledgedMerge('ack-inventory','vault.entries','{}',first,env));
  assert.ok(acknowledgedMerge('ack-inventory','vault.entries','{}',second,env));
  const restored=JSON.parse(new TextDecoder().decode(await getSecret('ack-inventory','vault.entries',{...env})));
  assert.deepEqual(restored,{alice:{password:'one'},bob:{password:'two'}});
  const edit=JSON.stringify({alice:[{password:'one'},{password:'updated'}]});
  assert.equal(acknowledgedMerge('ack-inventory','vault.entries','{}',edit,env),null);
  await flushAcknowledgements();
  assert.ok(acknowledgedMerge('ack-inventory','vault.entries','{}',edit,env));
  const remove=JSON.stringify({bob:[{password:'two'},null]});
  assert.equal(acknowledgedMerge('ack-inventory','vault.entries','{}',remove,env),null);
  await flushAcknowledgements();
  assert.ok(acknowledgedMerge('ack-inventory','vault.entries','{}',remove,env));
  const latest=JSON.parse(new TextDecoder().decode(await getSecret('ack-inventory','vault.entries',env)));
  assert.deepEqual(latest,{alice:{password:'updated'}});
});

test('legacy page inventory migrates without dropping unedited entries',async()=>{
  const env={storage:memory(),locks:serializedLocks()};
  const baseline=JSON.stringify({old:{password:'preserved'},edit:{password:'before'}});
  const patch=JSON.stringify({edit:[{password:'before'},{password:'after'}]});
  assert.equal(acknowledgedMerge('ack-migrate','vault.entries',baseline,patch,env),null);
  await flushAcknowledgements();
  assert.deepEqual(JSON.parse(acknowledgedMerge('ack-migrate','vault.entries',baseline,patch,env)),{
    old:{password:'preserved'},edit:{password:'after'},
  });
});

test('quota failures and stale writers never acknowledge or replace durable custody',async()=>{
  const storage=memory(),env={storage,locks:serializedLocks()};
  const initial=JSON.stringify({alice:[null,{password:'original'}]});
  acknowledgedMerge('ack-failure','vault.entries','{}',initial,env);
  await flushAcknowledgements();
  acknowledgedMerge('ack-failure','vault.entries','{}',initial,env);
  const ciphertext=storage.stores.items.get(JSON.stringify(['ack-failure','vault.entries']));
  const conflict=JSON.stringify({alice:[null,{password:'loser'}]});
  assert.equal(acknowledgedMerge('ack-failure','vault.entries','{}',conflict,env),null);
  await assert.rejects(flushAcknowledgements(),/concurrently/);
  assert.throws(()=>acknowledgedMerge('ack-failure','vault.entries','{}',conflict,env),/concurrently/);
  assert.deepEqual(storage.stores.items.get(JSON.stringify(['ack-failure','vault.entries'])),ciphertext);

  const full={...env,storage:{...storage,put:async()=>{throw Error('quota exceeded');}}};
  const bytes=new Uint8Array([91,92,93]);
  assert.equal(acknowledgedSecret('ack-quota',bytes,full),null);
  // Drain all operations, then consume the per-operation failure. The earlier
  // conflict was already consumed, so it must not poison unrelated operations.
  await assert.rejects(flushAcknowledgements());
  assert.throws(()=>acknowledgedSecret('ack-quota',bytes,full),/quota exceeded/);
  assert.equal(await getSecret('ack-quota','request-auth/not-published',env),null);
  assert.equal((await loadSecrets(env)).some(([app])=>app==='ack-quota'),false);
});


test('every repeated A/B inventory transition commits anew before acknowledgement',async()=>{
  const storage=memory(),env={storage,locks:serializedLocks()};
  const app='ack-repeated-transitions',key='vault.entries';
  const A={password:'A'},B={password:'B'};
  await putSecret(app,key,new TextEncoder().encode(JSON.stringify({alice:A})),env);
  for(const [original,replacement] of [[A,B],[B,A],[A,B],[B,A],[A,B]]){
    const changes=JSON.stringify({alice:[original,replacement]});
    assert.equal(acknowledgedMerge(app,key,'{}',changes,env),null,
      'a prior identical transition must never acknowledge this write');
    assert.equal(pendingAcknowledgements(),true);
    // Repeated polls while work is pending must not start duplicate writes.
    assert.equal(acknowledgedMerge(app,key,'{}',changes,env),null);
    await flushAcknowledgements();
    assert.equal(pendingAcknowledgements(),true,'commit is not consumption');
    assert.deepEqual(JSON.parse(acknowledgedMerge(app,key,'{}',changes,env)),{alice:replacement});
    assert.equal(pendingAcknowledgements(),false,'consumed results must be retired');
    const fresh={storage:{...storage},locks:env.locks};
    assert.deepEqual(JSON.parse(new TextDecoder().decode(await getSecret(app,key,fresh))),{alice:replacement},
      'read durable ciphertext, never a cached result, after every transition');
  }
});

test('consumed seals retire plaintext and identical bytes start new durable writes',async()=>{
  const storage=memory(),env={storage,locks:serializedLocks()};
  const bytes=new Uint8Array([11,22,33]);
  const copies=[],slice=bytes.slice.bind(bytes);
  bytes.slice=()=>{const copy=slice();copies.push(copy);return copy;};
  const ids=[];
  for(let i=0;i<3;i++){
    assert.equal(acknowledgedSecret('ack-repeat-seal',bytes,env),null);
    assert.equal(acknowledgedSecret('ack-repeat-seal',bytes,env),null);
    assert.equal(copies.length,i+1,'pending polls do not copy plaintext');
    await flushAcknowledgements();
    const id=acknowledgedSecret('ack-repeat-seal',bytes,env);
    assert.match(id,/^request-auth\//);
    assert.ok(!ids.includes(id),'a consumed credential id cannot be replayed');
    ids.push(id);
    assert.equal(pendingAcknowledgements(),false);
    assert.deepEqual([...copies[i]],[0,0,0],'owned plaintext is wiped after commit');
    assert.deepEqual([...await getSecret('ack-repeat-seal',id,{...env,storage:{...storage}})],[11,22,33]);
  }
});

test('failed repeated merges and seals never replay success and can retry after consumption',async()=>{
  const storage=memory(),locks=serializedLocks();
  let refused=false,writes=0;
  const env={locks,storage:{...storage,put:async(...args)=>{
    if(args[0]==='items'){
      writes++;
      if(refused)throw Error('commit refused');
    }
    return storage.put(...args);
  }}};
  const app='ack-retry',key='vault.entries',A={password:'A'},B={password:'B'};
  await putSecret(app,key,new TextEncoder().encode(JSON.stringify({alice:A})),env);
  const forward=JSON.stringify({alice:[A,B]}),back=JSON.stringify({alice:[B,A]});
  for(const patch of [forward,back]){
    assert.equal(acknowledgedMerge(app,key,'{}',patch,env),null);
    await flushAcknowledgements();
    assert.ok(acknowledgedMerge(app,key,'{}',patch,env));
  }
  refused=true;
  assert.equal(acknowledgedMerge(app,key,'{}',forward,env),null);
  await assert.rejects(flushAcknowledgements(),/commit refused/);
  await assert.rejects(flushAcknowledgements(),/commit refused/,'flush must not consume a failure');
  assert.throws(()=>acknowledgedMerge(app,key,'{}',forward,env),/commit refused/);
  assert.equal(pendingAcknowledgements(),false);
  assert.deepEqual(JSON.parse(new TextDecoder().decode(await getSecret(app,key,{storage,locks}))),{alice:A});
  refused=false;
  assert.equal(acknowledgedMerge(app,key,'{}',forward,env),null);
  await flushAcknowledgements();
  assert.deepEqual(JSON.parse(acknowledgedMerge(app,key,'{}',forward,env)),{alice:B});
  assert.deepEqual(JSON.parse(new TextDecoder().decode(await getSecret(app,key,{storage,locks}))),{alice:B});
  assert.equal(writes,5,'initial write, both transitions, refused repeat, successful retry');

  const bytes=new Uint8Array([44,55]),copies=[],slice=bytes.slice.bind(bytes);
  bytes.slice=()=>{const copy=slice();copies.push(copy);return copy;};
  refused=true;
  assert.equal(acknowledgedSecret(app,bytes,env),null);
  await assert.rejects(flushAcknowledgements(),/commit refused/);
  assert.throws(()=>acknowledgedSecret(app,bytes,env),/commit refused/);
  assert.deepEqual([...copies[0]],[0,0]);
  assert.equal((await loadSecrets({storage,locks})).length,1,'failed seal published no record');
  refused=false;
  assert.equal(acknowledgedSecret(app,bytes,env),null);
  await flushAcknowledgements();
  const id=acknowledgedSecret(app,bytes,env);
  assert.deepEqual([...await getSecret(app,id,{storage,locks})],[44,55]);
  assert.equal(pendingAcknowledgements(),false);
  await flushAcknowledgements();
});
