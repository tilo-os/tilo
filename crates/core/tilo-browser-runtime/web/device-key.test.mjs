import test from 'node:test';
import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';
import {
  openDeviceKey, idbStorage, certifiedDeviceKeys, rememberBrowserDevice, BROWSER_DEVICES,
  DeviceKeyError, MAX_SLOTS, DEFAULT_AVATAR_COLOR,
  personRecord, rootOfPerson, readPerson, writePerson, deletePerson, PERSON_KEY,
} from './device-key.mjs';
import {fakeLocks} from './testing/fake-storage.mjs';

/** A tab's `sessionStorage`: its own, kept across its reloads. */
function memorySession(rows = new Map()) {
  return {
    rows,
    getItem: key => (rows.has(key) ? rows.get(key) : null),
    setItem: (key, value) => { rows.set(key, String(value)); },
    removeItem: key => { rows.delete(key); },
  };
}

/** What a tab opens its device key with: its locks and its own session. Tabs
 * here share one module, so none asks the others over a channel. */
const tab = (locks, name, session = memorySession()) => ({locks: locks.manager(name), session, broadcast: null});

/** The tabs' cross-tab messages over an in-process bus whose deliveries reach
 * the other tabs `delay` ms after they are sent: a busy machine delivering an
 * answer after a shorter window has passed. Messages are carried by name, as
 * the browser carries them. */
function delayedDeliveries(delay) {
  const byName = new Map();
  return name => {
    const opened = {onmessage: null, closed: false};
    opened.postMessage = data => {
      for (const other of byName.get(name) ?? []) {
        if (other === opened || other.closed) continue;
        setTimeout(() => { if (!other.closed) other.onmessage?.({data}); }, delay);
      }
    };
    opened.close = () => { opened.closed = true; };
    byName.set(name, [...(byName.get(name) ?? []), opened]);
    return opened;
  };
}

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
  const deviceKey = await openDeviceKey({profile: 'p', storage, ...tab(locks, 'tab-1')});
  assert.equal(deviceKey.kind, 'webcrypto-ed25519');
  assert.equal(deviceKey.publicKey.length, 32);
  const stored = storage.rows.get('device-key:p');
  assert.equal(stored.privateKey.extractable, false);
  await assert.rejects(crypto.subtle.exportKey('pkcs8', stored.privateKey));

  const message = new TextEncoder().encode('challenge');
  const signature = await deviceKey.sign(message);
  const publicKey = await crypto.subtle.importKey('raw', deviceKey.publicKey, {name: 'Ed25519'}, false, ['verify']);
  assert.ok(await crypto.subtle.verify({name: 'Ed25519'}, publicKey, signature, message));
  deviceKey.close();
  await new Promise(resolve => setImmediate(resolve));

  const again = await openDeviceKey({profile: 'p', storage, ...tab(locks, 'tab-1')});
  assert.deepEqual(again.publicKey, deviceKey.publicKey);
  again.close();
});

test('a second tab takes a key of its own while the first holds one', async () => {
  const storage = memoryStorage();
  const locks = fakeLocks();
  const first = await openDeviceKey({profile: 'p', storage, ...tab(locks, 'tab-1')});
  const second = await openDeviceKey({profile: 'p', storage, ...tab(locks, 'tab-2')});
  assert.deepEqual([first.slot, first.created, second.slot, second.created], [0, true, 1, true]);
  assert.notDeepEqual(second.publicKey, first.publicKey, 'each tab is a device of its own');
  assert.ok(storage.rows.has('device-key:p') && storage.rows.has('device-key:p#1'));
  // Closing resolves once the key is free: the next tab takes it at once.
  await first.close();
  const third = await openDeviceKey({profile: 'p', storage, ...tab(locks, 'tab-3')});
  assert.deepEqual([third.slot, third.created], [0, false]);
  assert.deepEqual(third.publicKey, first.publicKey);
  second.close(); third.close();
});

test('a tab takes the first free slot, reuses released ones, and adds one only when all are held', async () => {
  const storage = memoryStorage();
  const locks = fakeLocks();
  const open = name => openDeviceKey({profile: 'p', storage, ...tab(locks, name)});
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
  for (let n = 0; n < MAX_SLOTS; n++) held.push(await openDeviceKey({profile: 'p', storage, ...tab(locks, `tab-${n}`)}));
  await assert.rejects(openDeviceKey({profile: 'p', storage, ...tab(locks, 'one-too-many')}),
    error => error instanceof DeviceKeyError && error.code === 'device-key-busy');
  await held[5].close();
  // A link recipient takes an existing key only: the free one.
  const linked = await openDeviceKey({profile: 'p', storage, ...tab(locks, 'link'), requireExisting: true});
  assert.deepEqual([linked.slot, linked.created], [5, false]);
  await assert.rejects(openDeviceKey({profile: 'p', storage, ...tab(locks, 'link-2'), requireExisting: true}),
    error => error instanceof DeviceKeyError && error.code === 'device-key-busy');
  for (const deviceKey of [...held, linked]) deviceKey.close();
});

test('without WebCrypto Ed25519 the seed is stored only sealed', async () => {
  const storage = memoryStorage();
  const locks = fakeLocks();
  const deviceKey = await openDeviceKey({profile: 'p', storage, ...tab(locks, 'tab'), preferWebCrypto: false});
  assert.equal(deviceKey.kind, 'sealed-seed');
  const stored = storage.rows.get('device-key:p');
  assert.equal(stored.wrapKey.extractable, false);
  assert.equal(stored.sealed.length, 32 + 16, 'ciphertext and tag, no plaintext seed');
  const {seed} = await deviceKey.unseal();
  assert.equal(seed.length, 32);
  const {seed: again} = await deviceKey.unseal();
  assert.deepEqual(again, seed);
  await deviceKey.destroy();
  assert.equal(storage.rows.has('device-key:p'), false);
  await assert.rejects(deviceKey.unseal(), error => error.code === 'device-key-closed');
});

test('two profiles on one origin hold two device keys', async () => {
  const storage = memoryStorage();
  const locks = fakeLocks();
  const a = await openDeviceKey({profile: 'a', storage, ...tab(locks, 'tab-1')});
  const b = await openDeviceKey({profile: 'b', storage, ...tab(locks, 'tab-1')});
  assert.notDeepEqual(a.publicKey, b.publicKey);
  a.close(); b.close();
});

test('a link recipient requires existing device key without creating a key', async () => {
  const storage = memoryStorage();
  const locks = fakeLocks();
  let generations = 0;
  const unavailableCrypto = {subtle: {generateKey: async () => { generations++; throw new Error('unexpected generation'); }}};
  await assert.rejects(openDeviceKey({profile: 'existing', storage,
    ...tab(locks, 'tab-1'), crypto: unavailableCrypto, requireExisting: true}),
    error => error instanceof DeviceKeyError && error.code === 'device-key-missing');
  assert.equal(generations, 0);
  assert.equal(storage.rows.size, 0);
  // The refusal releases device key, so the ordinary enrollment flow can continue.
  const enrolled = await openDeviceKey({profile: 'existing', storage, ...tab(locks, 'tab-2')});
  await enrolled.close();
  const reopened = await openDeviceKey({profile: 'existing', storage,
    ...tab(locks, 'tab-1'), requireExisting: true});
  assert.deepEqual(reopened.publicKey, enrolled.publicKey);
  await reopened.close();
});

test('a reload takes its own slot back, waiting for the page before it, while other tabs are open', async () => {
  const storage = memoryStorage();
  const locks = fakeLocks();
  const session = memorySession();
  const first = await openDeviceKey({profile: 'p', storage, ...tab(locks, 'tab-a-page-1', session)});
  const other = await openDeviceKey({profile: 'p', storage, ...tab(locks, 'tab-b')});
  assert.deepEqual([first.slot, other.slot], [0, 1]);
  // Twenty reloads of the first tab, each starting before the page it
  // replaces let go of its key: every one is the same device.
  let page = first;
  for (let reload = 2; reload <= 21; reload++) {
    const next = openDeviceKey({profile: 'p', storage, ...tab(locks, `tab-a-page-${reload}`, session)});
    await new Promise(resolve => setTimeout(resolve, 5));
    page.close();
    page = await next;
    assert.deepEqual([page.slot, page.created], [0, false], `reload ${reload} is the same device`);
  }
  assert.deepEqual(page.publicKey, first.publicKey);
  assert.equal(storage.rows.size, 2, 'two tabs, two devices, through every reload');
  page.close(); other.close();
});

test('a reload comes back as the tab’s first device, not as a second one its page opened on the same host', async () => {
  const storage = memoryStorage();
  const locks = fakeLocks();
  const pageA = await import('./device-key.mjs?page=two-peers');
  const pageB = await import('./device-key.mjs?page=two-peers-reloaded');
  const broadcast = name => new BroadcastChannel(name);
  const session = memorySession();
  const events = new EventTarget();
  // The workspace's peer, then a second peer of the same person on the same
  // host (the one carrying reports to the home workspace).
  const workspace = await pageA.openDeviceKey({profile: 'p', storage, locks: locks.manager('page-1'), session, broadcast, events});
  const home = await pageA.openDeviceKey({profile: 'p', storage, locks: locks.manager('page-1'), session, broadcast, events});
  assert.deepEqual([workspace.slot, home.slot], [0, 1]);
  events.dispatchEvent(new Event('pagehide'));
  await workspace.close(); await home.close();
  const reloaded = await pageB.openDeviceKey({profile: 'p', storage, locks: locks.manager('page-2'), session, broadcast, events: new EventTarget()});
  assert.deepEqual([reloaded.slot, reloaded.created], [0, false], 'the device the host let into the workspace');
  assert.deepEqual(reloaded.publicKey, workspace.publicKey);
  await reloaded.close();
});

test('a duplicated tab, whose remembered slot a live tab holds, takes a slot of its own at once', async () => {
  const storage = memoryStorage();
  const locks = fakeLocks();
  // Two pages: each its own module, as each tab loads it.
  const pageA = await import('./device-key.mjs?page=a');
  const pageB = await import('./device-key.mjs?page=b');
  const broadcast = name => new BroadcastChannel(name);
  const session = memorySession();
  const original = await pageA.openDeviceKey({profile: 'p', storage, locks: locks.manager('a'), session, broadcast, events: new EventTarget()});
  // A duplicate copies the original's sessionStorage, so it remembers slot 0 too.
  const started = Date.now();
  const duplicate = await pageB.openDeviceKey({profile: 'p', storage, locks: locks.manager('b'),
    session: memorySession(new Map(session.rows)), broadcast, events: new EventTarget()});
  assert.deepEqual([original.slot, duplicate.slot, duplicate.created], [0, 1, true]);
  assert.ok(Date.now() - started < pageB.OWN_SLOT_WAIT_MS, 'it did not wait for the original to close');
  await original.close(); await duplicate.close();
});

test('a duplicated tab whose live answer comes late still takes a key of its own, without waiting out the tab', async () => {
  const storage = memoryStorage();
  const locks = fakeLocks();
  const pageA = await import('./device-key.mjs?page=late-answer');
  const pageB = await import('./device-key.mjs?page=late-answer-reloaded');
  // Each message reaches the other tab after the window a page at first waits
  // for an answer: the holder is live, its answer is just slow.
  const broadcast = delayedDeliveries(pageB.LIVE_ASK_MS + 100);
  const session = memorySession();
  const original = await pageA.openDeviceKey({profile: 'p', storage, locks: locks.manager('original'), session, broadcast, events: new EventTarget()});
  assert.equal(original.created, true);
  const started = Date.now();
  const duplicate = await pageB.openDeviceKey({profile: 'p', storage, locks: locks.manager('duplicate'),
    session: memorySession(new Map(session.rows)), broadcast, events: new EventTarget()});
  const took = Date.now() - started;
  assert.equal(duplicate.created, true, 'a key of its own, never the live tab’s');
  assert.notDeepEqual(duplicate.publicKey, original.publicKey);
  // Well before the wait a page gives the page that held the key before it:
  // the late answer ended that wait.
  assert.ok(took < 10_000, `it took ${took} ms`);
  await original.close(); await duplicate.close();
});

test('a page on its way out answers for no slot, so its reload waits for it', async () => {
  const storage = memoryStorage();
  const locks = fakeLocks();
  const pageA = await import('./device-key.mjs?page=leaving');
  const pageB = await import('./device-key.mjs?page=reloaded');
  const broadcast = name => new BroadcastChannel(name);
  const session = memorySession();
  const events = new EventTarget();
  const leaving = await pageA.openDeviceKey({profile: 'p', storage, locks: locks.manager('old'), session, broadcast, events});
  events.dispatchEvent(new Event('pagehide'));
  const reloaded = pageB.openDeviceKey({profile: 'p', storage, locks: locks.manager('new'), session, broadcast, events: new EventTarget()});
  await new Promise(resolve => setTimeout(resolve, pageB.LIVE_ASK_MS + 100));
  leaving.close();
  const back = await reloaded;
  assert.deepEqual([back.slot, back.created], [0, false]);
  assert.deepEqual(back.publicKey, leaving.publicKey);
  await back.close();
});

test('a new tab reuses a free slot that has a key before it makes another', async () => {
  const storage = memoryStorage();
  const locks = fakeLocks();
  const open = name => openDeviceKey({profile: 'p', storage, ...tab(locks, name)});
  const [a, b, c] = [await open('a'), await open('b'), await open('c')];
  await a.close();
  await c.close();
  // Slot 0 and slot 2 are free with keys: the lowest of them, no new key.
  const next = await open('d');
  assert.deepEqual([next.slot, next.created], [0, false]);
  const after = await open('e');
  assert.deepEqual([after.slot, after.created], [2, false]);
  assert.equal(storage.rows.size, 3);
  b.close(); next.close(); after.close();
});

test('a browser keeps each tab device certificate once, under one id', async () => {
  const namespaces = memorySession();
  const cert = key => JSON.stringify({cert: {device_public_key: [key, ...new Array(31).fill(0)], device_label: 'Browser'}, hostCert: {}});
  rememberBrowserDevice(cert(1), namespaces);
  rememberBrowserDevice(cert(2), namespaces);
  rememberBrowserDevice(cert(1), namespaces);
  const kept = JSON.parse(namespaces.getItem(BROWSER_DEVICES));
  assert.match(kept.id, /^[0-9a-f]{32}$/);
  assert.deepEqual(kept.certs.map(one => one.device_public_key[0]), [2, 1]);
  // Each one's text as issued, which a number would round, by its key.
  assert.equal(kept.exact[`01${'00'.repeat(31)}`], cert(1));
  assert.deepEqual(Object.keys(kept.exact).length, 2);
  rememberBrowserDevice('not json', namespaces);
  assert.equal(JSON.parse(namespaces.getItem(BROWSER_DEVICES)).id, kept.id);
});

/** As much of IndexedDB as `idbStorage` uses: databases by name, each at a version with its
 * object stores. A store is made only while its database is opened at a newer version, and a
 * transaction over a store the database does not have throws, as a browser's does. An upgrade
 * that is aborted leaves no database, as a browser's does for one that did not exist. */
function fakeDatabases(databases = new Map()) {
  const failure = (name, message) => Object.assign(new Error(message), {name});
  const answered = (tx, work) => {
    const request = {};
    queueMicrotask(() => { request.result = work(); request.onsuccess?.(); tx.oncomplete?.(); });
    return request;
  };
  return {
    databases,
    open(name, version) {
      const request = {};
      queueMicrotask(() => {
        const database = databases.get(name) ?? {version: 0, stores: new Map()};
        databases.set(name, database);
        if (version < database.version) {
          request.error = failure('VersionError', 'the database is at a newer version');
          request.onerror?.();
          return;
        }
        let upgrading = false;
        request.result = {
          objectStoreNames: {contains: store => database.stores.has(store)},
          createObjectStore(store) {
            if (!upgrading) throw failure('InvalidStateError', 'a store is made in an upgrade');
            database.stores.set(store, new Map());
          },
          transaction(store) {
            const rows = database.stores.get(store);
            if (!rows) throw failure('NotFoundError', `no store ${store}`);
            const tx = {objectStore: () => ({
              get: key => answered(tx, () => rows.get(key)),
              put: (value, key) => answered(tx, () => { rows.set(key, value); }),
              delete: key => answered(tx, () => { rows.delete(key); }),
              getAll: () => answered(tx, () => [...rows.values()]),
            })};
            return tx;
          },
        };
        if (version > database.version) {
          upgrading = true;
          database.version = version;
          request.transaction = {abort() {
            databases.delete(name);
            request.error = failure('AbortError', 'The version change transaction was aborted');
            request.onerror?.();
          }};
          request.onupgradeneeded?.();
          upgrading = false;
          if (request.error) return;
        }
        request.onsuccess?.();
      });
      return request;
    },
  };
}

/** A browser's database of its device as a page left it: version 1, with `stores`. */
const browserWith = stores => fakeDatabases(new Map([['tilo-device', {
  version: 1,
  stores: new Map(Object.entries(stores).map(([store, rows]) => [store, new Map(rows)])),
}]]));

/** A device key and a person as a page keeps them, for a browser that has them already. */
async function keptAlready() {
  const storage = memoryStorage();
  const made = await openDeviceKey({profile: 'p', storage, ...tab(fakeLocks(), 'then')});
  const publicKey = made.publicKey;
  await made.close();
  const person = {rootPublicKey: 'ab'.repeat(32), held: 'here'};
  return {rows: [...storage.rows, ['identity', person]], publicKey, person};
}

for (const [entered, store] of [['before the store had the name it has', 'custody'], ['after', 'device-key']]) {
  test(`a browser that entered ${entered} finds its device key and its person where they are kept`, async () => {
    const {rows, publicKey, person} = await keptAlready();
    const indexedDB = browserWith({[store]: rows});
    const storage = idbStorage({indexedDB});
    const deviceKey = await openDeviceKey({profile: 'p', storage, ...tab(fakeLocks(), 'now'), requireExisting: true});
    assert.equal(deviceKey.created, false, 'the device it was, not a new one');
    assert.ok(publicKey);
    assert.deepEqual(deviceKey.publicKey, publicKey);
    assert.deepEqual(await storage.get('identity'), person, 'the person this browser keeps');
    // What a page writes now is where a page loaded before reads it, in a database that page
    // can still open: nothing was moved and the version is the one it had.
    await storage.put('identity', {...person, held: 'elsewhere'});
    const database = indexedDB.databases.get('tilo-device');
    assert.deepEqual([database.version, [...database.stores.keys()]], [1, [store]]);
    assert.equal(database.stores.get(store).get('identity').held, 'elsewhere');
    // The page reads the certificate it kept for that key, with the key that signs for it.
    const namespaces = memorySession();
    const certificate = JSON.stringify({cert: {device_public_key: [...publicKey]}, hostCert: null});
    rememberBrowserDevice(certificate, namespaces);
    const keys = await certifiedDeviceKeys({indexedDB, namespaces});
    assert.deepEqual(keys.map(key => key.certificate), [certificate], 'the text as it was issued');
    assert.equal(keys[0].privateKey, database.stores.get(store).get('device-key:p').privateKey);
    await deviceKey.close();
  });
}

test('a browser that never had a device gets no database from the certified keys', async () => {
  const namespaces = memorySession();
  rememberBrowserDevice(JSON.stringify({cert: {device_public_key: new Array(32).fill(0)}}), namespaces);
  const indexedDB = fakeDatabases();
  assert.deepEqual([await certifiedDeviceKeys({indexedDB, namespaces}), indexedDB.databases.size], [[], 0]);
});

test('a browser that never entered keeps its device key in the store of today’s name', async () => {
  const indexedDB = fakeDatabases();
  const storage = idbStorage({indexedDB});
  const deviceKey = await openDeviceKey({profile: 'p', storage, ...tab(fakeLocks(), 'first')});
  assert.equal(deviceKey.created, true);
  const database = indexedDB.databases.get('tilo-device');
  assert.deepEqual([database.version, [...database.stores.keys()]], [1, ['device-key']]);
  assert.ok(database.stores.get('device-key').has('device-key:p'));
  await deviceKey.close();
});

test('a page loaded before a site’s update and a page loaded after keep each other from the same device key', async () => {
  const storage = memoryStorage();
  const locks = fakeLocks();
  const free = (owner, name) => locks.manager(owner).request(name, {mode: 'exclusive', ifAvailable: true}, lock => Boolean(lock));
  // A page loaded before holds the first key as pages took one then: under the older prefix alone.
  let leave;
  const before = locks.manager('before').request('tilo-device-custody:p', {mode: 'exclusive', ifAvailable: true},
    lock => lock && new Promise(resolve => { leave = resolve; }));
  const now = await openDeviceKey({profile: 'p', storage, ...tab(locks, 'now')});
  assert.deepEqual([locks.holder('tilo-device-key:p'), locks.holder('tilo-device-key:p#1')], [undefined, 'now'],
    'the key a page loaded before holds is not taken: the next one is');
  // A page loaded before finds the key a page of now holds taken, and so does a page of a
  // build in between, which took a key under today's prefix alone.
  assert.equal(await free('before-2', 'tilo-device-custody:p#1'), false);
  assert.equal(await free('between', 'tilo-device-key:p#1'), false);
  leave();
  await before;
  await now.close();
  assert.deepEqual(locks.held(), [], 'a closed device key is held under neither prefix');
});

test('a tab asks a page loaded before a site’s update whether it holds the key the tab remembers, and answers one', async () => {
  const storage = memoryStorage();
  const locks = fakeLocks();
  const page = await import('./device-key.mjs?page=after-an-update');
  const asking = name => new BroadcastChannel(name);
  // The tab's session remembers the first key, as after a tab was duplicated.
  const session = memorySession();
  const first = await page.openDeviceKey({profile: 'p', storage, locks: locks.manager('now'), session, broadcast: asking, events: new EventTarget()});
  await first.close();
  // A page loaded before holds that key under the older prefix and answers where pages asked then.
  let leave;
  const held = locks.manager('before').request('tilo-device-custody:p', {mode: 'exclusive', ifAvailable: true},
    lock => lock && new Promise(resolve => { leave = resolve; }));
  const before = asking('tilo-device-custody');
  before.onmessage = ({data}) => { if (data?.ask === 'tilo-device-custody:p') before.postMessage({live: data.ask}); };
  const started = Date.now();
  const duplicate = await page.openDeviceKey({profile: 'p', storage, locks: locks.manager('now'),
    session: memorySession(new Map(session.rows)), broadcast: asking, events: new EventTarget()});
  assert.equal(locks.holder('tilo-device-key:p#1'), 'now', 'a key of its own');
  assert.ok(Date.now() - started < page.OWN_SLOT_WAIT_MS, 'it did not wait for the page loaded before to close');
  // The page of now answers a page loaded before that asks for the key it holds.
  const answer = new Promise(resolve => { before.onmessage = ({data}) => { if (data?.live) resolve(data.live); }; });
  before.postMessage({ask: 'tilo-device-custody:p#1'});
  assert.equal(await answer, 'tilo-device-custody:p#1');
  before.close();
  leave();
  await held;
  await duplicate.close();
});

test("a person root is kept in the same store as the tab's device key, as the identity store's record", async () => {
  const storage = memoryStorage();
  const locks = fakeLocks();
  const deviceKey = await openDeviceKey({profile: 'p', storage, ...tab(locks, 'tab-1')});
  const seed = new Uint8Array(32).fill(7);
  const envelope = {version: 1, root_public_key: 'ab', wraps: []};
  const revocations = [{device_public_key: 'cd'}];
  await writePerson(storage, {
    record: personRecord({seed, publicKey: deviceKey.publicKey, displayName: ' Ada ', createdAt: '2026-01-02T03:04:05Z'}),
    envelope,
    revocations,
  });
  // One store holds both: the tab's device key and the person's root.
  assert.ok(storage.rows.has('device-key:p'), "the tab's key");
  assert.ok(storage.rows.has(PERSON_KEY), 'the person, in the one store');
  const kept = await readPerson(storage);
  assert.equal(kept.record.display_name, 'Ada', 'a name is kept as the record fits it');
  assert.deepEqual(kept.record.avatar_color, DEFAULT_AVATAR_COLOR);
  assert.deepEqual(kept.envelope, envelope);
  assert.deepEqual(kept.revocations, revocations);
  assert.deepEqual(rootOfPerson(kept.record), {seed, publicKey: deviceKey.publicKey});
  await deletePerson(storage);
  assert.equal(await readPerson(storage), null);
  await deviceKey.close();
});

test('a root this page writes and a root a computer keeps are the same record', () => {
  const fixture = readFileSync(new URL('../../tilo-identity/tests/fixtures/identity-record.json', import.meta.url), 'utf8');
  const record = JSON.parse(fixture);
  // The page reads the record a computer keeps: its root is the seed and public key inside.
  const {seed, publicKey} = rootOfPerson(record);
  assert.deepEqual(seed, Uint8Array.from(record.threshold_identity.secret_bytes));
  assert.deepEqual(publicKey, Uint8Array.from(record.threshold_identity.group_public_key));
  // And writes it back byte for byte, so a tab's root and a computer's are one record.
  const written = JSON.stringify(personRecord({
    seed, publicKey, displayName: record.display_name, avatarColor: record.avatar_color, createdAt: record.created_at,
  }), null, 2);
  assert.equal(written.trimEnd(), fixture.trimEnd());
});
