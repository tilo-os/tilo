import test from 'node:test';
import assert from 'node:assert/strict';
import {deleteSecret,flushSecrets,getSecret,loadSecrets,putSecret,queueSecret} from './secrets.mjs';

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

test('secrets round-trip, replace and delete under a non-extractable key',async()=>{
  const storage=memory(),env={storage,locks:null};
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
  const storage=memory(),env={storage,locks:null};
  await putSecret('so.tilo.a','token',new TextEncoder().encode('secret'),env);
  await putSecret('so.tilo.a','other',new TextEncoder().encode('x'),env);
  const record=storage.stores.items.get(JSON.stringify(['so.tilo.a','token']));
  storage.stores.items.set(JSON.stringify(['so.tilo.a','other']),record);
  await assert.rejects(getSecret('so.tilo.a','other',env));
});

test('writers of one app hold its lock',async()=>{
  const held=[];
  const locks={request:async(name,fn)=>{held.push(name);return fn();}};
  const env={storage:memory(),locks};
  await putSecret('so.tilo.a','token',new Uint8Array([1]),env);
  await deleteSecret('so.tilo.a','token',env);
  assert.deepEqual(held,['tilo-secrets:so.tilo.a','tilo-secrets:so.tilo.a']);
});

test('the mirror loads what decrypts and queued writes land in order',async()=>{
  const storage=memory(),env={storage,locks:null};
  await putSecret('so.tilo.totp','vault',new Uint8Array([7]),env);
  await putSecret('so.tilo.passwords','vault',new Uint8Array([8]),env);
  storage.stores.keys.delete(JSON.stringify('so.tilo.passwords'));
  assert.deepEqual((await loadSecrets(env)).map(([app,key,bytes])=>[app,key,[...bytes]]),[['so.tilo.totp','vault',[7]]]);
  queueSecret('so.tilo.totp','vault',new Uint8Array([1]),env);
  queueSecret('so.tilo.totp','vault',new Uint8Array([2]),env);
  queueSecret('so.tilo.totp','other',null,env);
  await flushSecrets();
  assert.deepEqual([...await getSecret('so.tilo.totp','vault',env)],[2]);
});
