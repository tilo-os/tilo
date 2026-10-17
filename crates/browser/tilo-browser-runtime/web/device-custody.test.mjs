import test from 'node:test';
import assert from 'node:assert/strict';
import {openDeviceCustody, CustodyError, MAX_SLOTS} from './device-custody.mjs';
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

test('a second tab takes a key of its own while the first holds one', async () => {
  const storage = memoryStorage();
  const locks = fakeLocks();
  const first = await openDeviceCustody({profile: 'p', storage, locks: locks.manager('tab-1')});
  const second = await openDeviceCustody({profile: 'p', storage, locks: locks.manager('tab-2')});
  assert.deepEqual([first.slot, first.created, second.slot, second.created], [0, true, 1, true]);
  assert.notDeepEqual(second.publicKey, first.publicKey, 'each tab is a device of its own');
  assert.ok(storage.rows.has('device-key:p') && storage.rows.has('device-key:p#1'));
  // Closing resolves once the key is free: the next tab takes it at once.
  await first.close();
  const third = await openDeviceCustody({profile: 'p', storage, locks: locks.manager('tab-3')});
  assert.deepEqual([third.slot, third.created], [0, false]);
  assert.deepEqual(third.publicKey, first.publicKey);
  second.close(); third.close();
});

test('a tab takes the first free slot, reuses released ones, and adds one only when all are held', async () => {
  const storage = memoryStorage();
  const locks = fakeLocks();
  const open = tab => openDeviceCustody({profile: 'p', storage, locks: locks.manager(tab)});
  const tabs = [await open('a'), await open('b'), await open('c')];
  assert.deepEqual(tabs.map(tab => tab.slot), [0, 1, 2]);
  // The middle tab closes (or reloads): the next tab takes its slot back,
  // with the same key, and no new one is made.
  await tabs[1].close();
  const reloaded = await open('b');
  assert.deepEqual([reloaded.slot, reloaded.created], [1, false]);
  assert.deepEqual(reloaded.publicKey, tabs[1].publicKey);
  assert.equal(storage.rows.size, 3);
  // Every slot held: a fourth key.
  const fourth = await open('d');
  assert.deepEqual([fourth.slot, fourth.created], [3, true]);
  // All closed: the next tab takes the first slot again.
  for (const tab of [tabs[0], reloaded, tabs[2], fourth]) await tab.close();
  const again = await open('e');
  assert.deepEqual([again.slot, again.created], [0, false]);
  assert.equal(storage.rows.size, 4, 'keys are reused, never piled up');
  again.close();
});

test('only a full pool is busy, and an existing key is required only where one is', async () => {
  const storage = memoryStorage();
  const locks = fakeLocks();
  const held = [];
  for (let tab = 0; tab < MAX_SLOTS; tab++) held.push(await openDeviceCustody({profile: 'p', storage, locks: locks.manager(`tab-${tab}`)}));
  await assert.rejects(openDeviceCustody({profile: 'p', storage, locks: locks.manager('one-too-many')}),
    error => error instanceof CustodyError && error.code === 'custody-busy');
  await held[5].close();
  // A link recipient takes an existing key only: the free one.
  const linked = await openDeviceCustody({profile: 'p', storage, locks: locks.manager('link'), requireExisting: true});
  assert.deepEqual([linked.slot, linked.created], [5, false]);
  await assert.rejects(openDeviceCustody({profile: 'p', storage, locks: locks.manager('link-2'), requireExisting: true}),
    error => error instanceof CustodyError && error.code === 'custody-busy');
  for (const custody of [...held, linked]) custody.close();
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
