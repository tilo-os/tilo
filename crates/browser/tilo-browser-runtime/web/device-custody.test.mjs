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

// Whether a promise has settled yet, after the turns a lock hand-off takes.
const settled = async promise => {
  let done = false;
  promise.then(() => { done = true; }, () => { done = true; });
  for (let turn = 0; turn < 5; turn++) await new Promise(resolve => setImmediate(resolve));
  return done;
};

test('a second tab of the browser waits for the device key and is the same device', async () => {
  const storage = memoryStorage();
  const locks = fakeLocks();
  const first = await openDeviceCustody({profile: 'p', storage, locks: locks.manager('tab-1'), onWait: () => {}});
  const said = [];
  const second = openDeviceCustody({profile: 'p', storage, locks: locks.manager('tab-2'), onWait: waiting => said.push(waiting)});
  assert.equal(await settled(second), false, 'it waits while the first tab holds the key');
  assert.deepEqual(said, [true], 'and says that it waits');
  assert.deepEqual([...storage.rows.keys()], ['device-key:p'], 'no second key is made meanwhile');
  await first.close();
  const opened = await second;
  assert.deepEqual([opened.created, said], [false, [true, false]]);
  assert.deepEqual(opened.publicKey, first.publicKey, 'one device of the person, not one per tab');
  await opened.close();
});

test('two tabs and a reload under contention: one device, however the key changes hands', async () => {
  const storage = memoryStorage();
  const locks = fakeLocks();
  const open = tab => openDeviceCustody({profile: 'p', storage, locks: locks.manager(tab), onWait: () => {}});
  const a = await open('a');
  // A second tab waits; the first reloads: its new page asks for the key
  // while the old one still holds it, and the old page then goes away.
  const b = open('b');
  const reloaded = open('a-reloaded');
  assert.equal(await settled(b), false);
  assert.equal(await settled(reloaded), false);
  locks.terminate('a');
  // One of them has the key, the other still waits for it.
  const firstIn = await Promise.race([b.then(custody => ['b', custody]), reloaded.then(custody => ['a-reloaded', custody])]);
  const other = firstIn[0] === 'b' ? reloaded : b;
  assert.equal(await settled(other), false, 'the other waits, it does not become a device of its own');
  await firstIn[1].close();
  const secondIn = await other;
  for (const custody of [firstIn[1], secondIn]) {
    assert.equal(custody.created, false);
    assert.deepEqual(custody.publicKey, a.publicKey);
  }
  assert.deepEqual([...storage.rows.keys()], ['device-key:p'], 'one device key for the browser profile');
  await secondIn.close();
});

test('a tab that stops waiting makes no key, and an existing key is required only where one is', async () => {
  const storage = memoryStorage();
  const locks = fakeLocks();
  const held = await openDeviceCustody({profile: 'p', storage, locks: locks.manager('tab-1'), onWait: () => {}});
  const leaving = new AbortController();
  const waiting = openDeviceCustody({profile: 'p', storage, locks: locks.manager('tab-2'), onWait: () => {}, signal: leaving.signal});
  leaving.abort(new Error('the tab closed'));
  await assert.rejects(waiting, /the tab closed/);
  assert.equal(storage.rows.size, 1);
  // A link recipient with a key waits for it like any tab.
  const linked = openDeviceCustody({profile: 'p', storage, locks: locks.manager('link'), requireExisting: true, onWait: () => {}});
  await held.close();
  assert.equal((await linked).created, false);
  await (await linked).close();
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
