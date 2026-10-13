import test from 'node:test';
import assert from 'node:assert/strict';
import {openDeviceCustody, CustodyError} from './device-custody.mjs';
import {fakeLocks} from './testing/fake-storage.mjs';

function memoryStorage() {
  const rows = new Map();
  return {
    rows,
    get: async key => rows.get(key),
    put: async (key, value) => { rows.set(key, value); },
    delete: async key => { rows.delete(key); },
  };
}

test('a WebCrypto device key is non-extractable, signs, and survives reopening', async () => {
  const storage = memoryStorage();
  const locks = fakeLocks();
  const custody = await openDeviceCustody({profile: 'p', storage, locks: locks.manager('tab-1')});
  assert.equal(custody.kind, 'webcrypto-ed25519');
  assert.equal(custody.publicKey.length, 32);
  const stored = storage.rows.get('device-key:p');
  assert.equal(stored.privateKey.extractable, false);
  await assert.rejects(crypto.subtle.exportKey('pkcs8', stored.privateKey));

  const message = new TextEncoder().encode('challenge');
  const signature = await custody.sign(message);
  const publicKey = await crypto.subtle.importKey('raw', custody.publicKey, {name: 'Ed25519'}, false, ['verify']);
  assert.ok(await crypto.subtle.verify({name: 'Ed25519'}, publicKey, signature, message));
  custody.close();
  await new Promise(resolve => setImmediate(resolve));

  const again = await openDeviceCustody({profile: 'p', storage, locks: locks.manager('tab-1')});
  assert.deepEqual(again.publicKey, custody.publicKey);
  again.close();
});

test('a second tab cannot take custody while the first holds it', async () => {
  const storage = memoryStorage();
  const locks = fakeLocks();
  const first = await openDeviceCustody({profile: 'p', storage, locks: locks.manager('tab-1')});
  await assert.rejects(
    openDeviceCustody({profile: 'p', storage, locks: locks.manager('tab-2')}),
    error => error instanceof CustodyError && error.code === 'custody-busy',
  );
  // Closing resolves once the key is free: the next holder takes it at once.
  await first.close();
  const second = await openDeviceCustody({profile: 'p', storage, locks: locks.manager('tab-2')});
  assert.deepEqual(second.publicKey, first.publicKey);
  second.close();
});

test('without WebCrypto Ed25519 the seed is stored only sealed', async () => {
  const storage = memoryStorage();
  const locks = fakeLocks();
  const custody = await openDeviceCustody({profile: 'p', storage, locks: locks.manager('tab'), preferWebCrypto: false});
  assert.equal(custody.kind, 'sealed-seed');
  const stored = storage.rows.get('device-key:p');
  assert.equal(stored.wrapKey.extractable, false);
  assert.equal(stored.sealed.length, 32 + 16, 'ciphertext and tag, no plaintext seed');
  const {seed} = await custody.unseal();
  assert.equal(seed.length, 32);
  const {seed: again} = await custody.unseal();
  assert.deepEqual(again, seed);
  await custody.destroy();
  assert.equal(storage.rows.has('device-key:p'), false);
  await assert.rejects(custody.unseal(), error => error.code === 'custody-closed');
});

test('two profiles on one origin hold two device keys', async () => {
  const storage = memoryStorage();
  const locks = fakeLocks();
  const a = await openDeviceCustody({profile: 'a', storage, locks: locks.manager('tab-1')});
  const b = await openDeviceCustody({profile: 'b', storage, locks: locks.manager('tab-1')});
  assert.notDeepEqual(a.publicKey, b.publicKey);
  a.close(); b.close();
});

test('a link recipient requires existing custody without creating a key', async () => {
  const storage = memoryStorage();
  const locks = fakeLocks();
  let generations = 0;
  const unavailableCrypto = {subtle: {generateKey: async () => { generations++; throw new Error('unexpected generation'); }}};
  await assert.rejects(openDeviceCustody({profile: 'existing', storage,
    locks: locks.manager('tab-1'), crypto: unavailableCrypto, requireExisting: true}),
    error => error instanceof CustodyError && error.code === 'custody-missing');
  assert.equal(generations, 0);
  assert.equal(storage.rows.size, 0);
  // The refusal releases custody, so the ordinary enrollment flow can continue.
  const enrolled = await openDeviceCustody({profile: 'existing', storage, locks: locks.manager('tab-2')});
  await enrolled.close();
  const reopened = await openDeviceCustody({profile: 'existing', storage,
    locks: locks.manager('tab-1'), requireExisting: true});
  assert.deepEqual(reopened.publicKey, enrolled.publicKey);
  await reopened.close();
});
