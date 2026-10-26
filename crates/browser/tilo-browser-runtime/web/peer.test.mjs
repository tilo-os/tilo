import test from 'node:test';
import assert from 'node:assert/strict';
import {createLiveLayer, connectPeer, httpsPost, inspectInvitation, joinWorkspace, liveEndpoints, HOST_CHANGED, PROFILE_DEVICE, profileDevice, REFUSED_JOIN, renewDevice, UNUSABLE_INVITATION, rendererFailureCode, statusAsset, storageFailureCode, withCustody, withStartFields} from './peer.mjs';

const ROOM = '6f1d3a52-1c7e-4b8e-9d2a-0f4a8c3e5b71';
const SEALED = 0x53;
const sealedFrame = value => Uint8Array.from([SEALED, ...new TextEncoder().encode(JSON.stringify(value))]);
const openedFrame = bytes => JSON.parse(new TextDecoder().decode(bytes.subarray(1)));
const settle = () => new Promise(resolve => setTimeout(resolve, 0));
const ROOM_HEX = 'aa'.repeat(32);
const SELF = 'dd'.repeat(32);
const OTHER = 'cc'.repeat(32);
const AGENT = 'ee'.repeat(32);
const tick = () => new Promise(resolve => setTimeout(resolve, 5));
/** The shared hang bound: a wait this long means the code under test is stuck. */
const HANG_BOUND_MS = Number(process.env.TILO_TEST_HANG_BOUND_SECS || 120) * 1000;
/** Resolves once `check()` holds; a loaded runner takes longer, not forever. */
async function until(check, what, timeoutMs = HANG_BOUND_MS) {
  const deadline = Date.now() + timeoutMs;
  while (!check()) {
    if (Date.now() > deadline) throw new Error(`timed out waiting for ${what}`);
    await tick();
  }
}
// A test that fails before disposing its peer must not leave the live
// layer's retry timers holding the process open until the suite times out.
const opened = new Set();
test.afterEach(async () => {
  for (const peer of opened) await peer.dispose();
  opened.clear();
});
async function connect(options) {
  const peer = await connectPeer(options);
  opened.add(peer);
  return peer;
}

function memoryStore() {
  const rows = new Map();
  return {rows, get: async key => rows.get(key), put: async (key, value) => { rows.set(key, value); }, delete: async key => { rows.delete(key); }};
}

/** A live session with the interface of live-transport / live-fallback sessions. */
function fakeCarriers() {
  const connects = [];
  const carriers = {
    connects,
    async connect(options) {
      let closeSession;
      const session = {
        options,
        presence: [], ops: [], closedOps: [],
        memberWatcher: undefined,
        presenceSubscribers: new Map(),
        opsSubscribers: new Map(),
        closed: new Promise(resolve => { closeSession = resolve; }),
        publishPresence(bytes) { session.presence.push(bytes); return true; },
        publishOps(name, bytes) { session.ops.push({name, bytes}); return true; },
        closeOps(name) { session.closedOps.push(name); },
        watchMembers({onMember}) { session.memberWatcher = onMember; return () => {}; },
        subscribePresence(publisher, onFrame, onEnd) { session.presenceSubscribers.set(publisher, onFrame); session.presenceEnds = [...(session.presenceEnds ?? []), onEnd]; return () => {}; },
        subscribeOps(publisher, name, onFrame) { session.opsSubscribers.set(`${publisher}/${name}`, onFrame); return () => {}; },
        close() { closeSession(); },
      };
      connects.push(session);
      return session;
    },
  };
  return carriers;
}

function fakeRuntime({phase = 'ready'} = {}) {
  const calls = {certify: [], started: [], grants: 0, grantAgents: []};
  let peer;
  const runtime = {
    calls,
    get peer() { return peer; },
    async hostInfo(post) { calls.hostPost = post; return JSON.stringify({hostCert: {}, webtransportPort: 443, certificateSha256: 'ab'.repeat(32)}); },
    async certifyDevice(post, user, label, invite, custody, onPairingCode) {
      calls.certify.push({post, user, label, custody});
      onPairingCode('123456');
      return JSON.stringify({cert: {device: calls.certify.length}, hostCert: {host: true}});
    },
    async certifyWithRoot(post, root, custody, label) {
      calls.certify.push({post, root, custody, label});
      return JSON.stringify({cert: {device: calls.certify.length, expires_at_ns: calls.expiresAtNs ?? (Date.now() + 30 * 86_400_000) * 1e6}, hostCert: {host: true}});
    },
    async inspectInvitation(post, invitation) {
      calls.inspected = [...invitation];
      return JSON.stringify({kind: 'invitation', room: ROOM, workspaceId: '7', name: 'Acme', role: 'Editor', expiresAtNs: '9',
        inviter: {name: 'Ada', userPublicKey: 'aa'.repeat(32)}});
    },
    async askToJoin(post, invitation, root, custody, requestId, name, label, token) {
      calls.asked = {invitation: [...invitation], root, requestId, name, label, token: token && new TextDecoder().decode(token)};
      if (token) return JSON.stringify({reply: {kind: 'admitted', room: ROOM, workspaceId: '7', name: 'Acme', role: 'Editor'}, cert: {guest: true, device_public_key: Array(32).fill(0xdd)}});
      return JSON.stringify({reply: {kind: 'waiting'}, cert: {guest: true, device_public_key: Array(32).fill(0xdd)}});
    },
    async joinStatus(post, requestId, device) {
      calls.status = [...(calls.status ?? []), {requestId, device}];
      return JSON.stringify(calls.status.length < 2 ? {kind: 'waiting'} : {kind: 'admitted', room: ROOM, workspaceId: '7', name: 'Acme', role: 'Editor'});
    },
    encodeLivePresence: json => new TextEncoder().encode(json),
    presenceColor: id => `#${String(id).padEnd(6, '0').slice(0, 6).replace(/[^0-9a-f]/g, '0')}`,
    decodeLiveFrame: bytes => {
      const value = JSON.parse(new TextDecoder().decode(bytes));
      return JSON.stringify(value.kind ? value : {kind: 'presence', frame: value});
    },
    liveRecordTrack: record => `ops/${'0'.repeat(64 - JSON.parse(record).record.length)}${JSON.parse(record).record}`,
    encodeLivePreview: (record, seq, ops) => new TextEncoder().encode(JSON.stringify({kind: 'preview', record: JSON.parse(record), seq: Number(seq), ops: JSON.parse(ops)})),
    encodeLivePreviewEnd: record => new TextEncoder().encode(JSON.stringify({kind: 'previewEnd', record: JSON.parse(record)})),
    WasmPeer: {
      start(config, custody, namespace, record, save, post, channelVault) {
        const listeners = [];
        const durable = [];
        peer = {
          config: JSON.parse(config), configText: config, custody, namespace, record, save, post, channelVault, disposed: false, reconnects: 0, retries: 0,
          phase,
          records: [],
          admission: 'ready',
          failures: [],
          refreshes: 0,
          snapshot: () => JSON.stringify({phase: peer.phase, deviceId: 'dd', outgoingPending: 0, health: {version: 1, admission: {phase: peer.admission, pendingPrerequisites: peer.admission === 'ready' ? 0 : 1}}}),
          subscribe: listener => listeners.push(listener),
          sessionToken: () => 'ce'.repeat(32),
          joinRoom: room => { peer.room = room; },
          liveGrant: async (room, ...agent) => {
            calls.grants++;
            calls.grantAgents.push(agent);
            return JSON.stringify({token: '0102', room: ROOM_HEX, publisher: agent.length ? AGENT : SELF, expiresAtNs: (Date.now() + 600_000) * 1e6});
          },
          // Stand-in channel keys: a sealed frame carries the SEALED marker,
          // and only a sealed frame opens.
          sealLive: async (room, bytes) => (peer.sealing === false ? null : Uint8Array.from([SEALED, ...bytes])),
          openLive: async (room, bytes) => (bytes[0] === SEALED ? bytes.slice(1) : null),
          roomRecords: () => JSON.stringify(peer.records),
          hasRecord: (app, record) => peer.records.some(row => row.app === app && row.record === record),
          takeDurableEvents: () => JSON.stringify(durable.splice(0)),
          takeRemoteActionEvents: () => '[]',
          sendRemoteAction: () => {},
          reconnect: () => { peer.reconnects++; },
          retryNow: () => { peer.retries++; },
          refresh: () => { peer.refreshes++; peer.onRefresh?.(); },
          reportFailure: (surface, asset, code, message) => {
            if (!['storage-transaction-failed', 'storage-quota', 'storage-denied', 'asset-unavailable', 'gpu-unavailable', 'gpu-device-lost', 'renderer-initialization-failed'].includes(code)) throw new Error(`unknown failure code ${code}`);
            peer.failures = [...peer.failures.filter(f => f.surface !== surface || f.asset !== asset), {surface, asset, code, message}];
          },
          clearFailure: (surface, asset) => { peer.failures = peer.failures.filter(f => f.surface !== surface || f.asset !== asset); },
          clearSurface: surface => { peer.failures = peer.failures.filter(f => f.surface !== surface); },
          dispose: () => { peer.disposed = true; },
          flush: async () => {},
          setLocalState: () => {},
          emit(phase = peer.phase, events = []) { peer.phase = phase; durable.push(...events); for (const listener of listeners) listener(); },
        };
        calls.started.push(peer);
        return peer;
      },
    },
  };
  return runtime;
}

const custody = {kind: 'webcrypto-ed25519', publicKey: new Uint8Array(32), sign: async () => new Uint8Array(64)};
function vaultMemory() {
  const rows = new Map();
  return {rows, get: async key => rows.get(key), put: async (key, value) => { rows.set(key, value); }};
}

const options = (runtime, store, extra = {}) => ({
  origin: 'https://host.example:8443', userPublicKey: 'bb'.repeat(32), label: 'Browser on Cloud', room: ROOM,
  custody, store, runtime, liveCarriers: fakeCarriers(), fetch: async () => { throw new Error('no network in tests'); }, ...extra,
});

test('httpsPost posts bodies with the session header to the host origin', async () => {
  const seen = [];
  const post = httpsPost('https://host.example:8443', async (url, init) => {
    seen.push({url, init});
    return {status: 204, arrayBuffer: async () => new ArrayBuffer(0)};
  });
  const answer = await post('/peer/v1/send', 'ab', new Uint8Array([1]));
  assert.equal(answer.status, 204);
  assert.equal(seen[0].url, 'https://host.example:8443/peer/v1/send');
  assert.equal(seen[0].init.headers['x-tilo-session'], 'ab');
  assert.equal(seen[0].init.credentials, 'omit');
  assert.throws(() => httpsPost('http://host.example'), TypeError);
});

test('first connect certifies over HTTPS once, shows the pairing code and keeps the certificate', async () => {
  const runtime = fakeRuntime();
  const store = memoryStore();
  const codes = [];
  const peer = await connect(options(runtime, store, {onPairingCode: code => codes.push(code)}));
  assert.deepEqual(codes, ['123456']);
  assert.equal(runtime.calls.certify.length, 1);
  assert.equal(runtime.peer.room, ROOM);
  assert.deepEqual(runtime.peer.config.cert, {device: 1});
  assert.equal(typeof runtime.peer.post, 'function', 'the session posts to the host origin');
  assert.equal(peer.host.certificateSha256, 'ab'.repeat(32));
  await peer.dispose();

  const second = await connect(options(runtime, store));
  assert.equal(runtime.calls.certify.length, 1, 'the stored certificate is reused');
  await second.dispose();
  assert.equal(runtime.peer.disposed, true);
});

// What a host on a clock with nanosecond resolution issues: 19 significant
// digits, which a JS number does not hold (json-text.mjs).
const NANOS = '1792023733547123457';
const LINUX_CERT = `{"device_public_key":[221],"issued_at_ns":${NANOS},"expires_at_ns":${NANOS}9,"signature":[1]}`;

test('a certificate reaches the session byte for byte, its nanosecond timestamps intact', async () => {
  const runtime = fakeRuntime();
  runtime.certifyDevice = async () => `{"cert":${LINUX_CERT},"hostCert":{"issued_at_ns":${NANOS}}}`;
  const peer = await connect(options(runtime, memoryStore(), {displayName: 'Ada "A"'}));
  assert.ok(runtime.peer.configText.includes(`"cert":${LINUX_CERT}`), runtime.peer.configText);
  assert.ok(runtime.peer.configText.includes(`"hostCert":{"issued_at_ns":${NANOS}}`));
  assert.equal(runtime.peer.config.displayName, 'Ada "A"');
  await peer.dispose();
});

test('a guest certificate from an invitation reaches the session byte for byte', async () => {
  const runtime = fakeRuntime();
  runtime.askToJoin = async () => `{"reply":{"kind":"admitted","room":"${ROOM}","workspaceId":"7","name":"Acme","role":"Editor"},"cert":${LINUX_CERT}}`;
  runtime.hostInfo = async () => `{"hostCert":{"issued_at_ns":${NANOS}},"webtransportPort":443,"certificateSha256":"${'ab'.repeat(32)}"}`;
  const identity = {kind: 'webcrypto-ed25519', publicKey: new Uint8Array(32).fill(0xee), userPublicKey: 'ee'.repeat(32), sign: async () => new Uint8Array(64)};
  const peer = await joinWorkspace({
    origin: 'https://host.example:8443', invitation: Uint8Array.from([1, 2, 3]), identity, admissionToken: 'token',
    runtime, custody, store: memoryStore(), storage: null,
    liveCarriers: fakeCarriers(), fetch: async () => { throw new Error('no network in tests'); },
  });
  assert.ok(runtime.peer.configText.includes(`"cert":${LINUX_CERT}`), runtime.peer.configText);
  assert.ok(runtime.peer.configText.includes(`"hostCert":{"issued_at_ns":${NANOS}}`));
  await peer.dispose();
});

test('a page holding the root certifies itself, with no pairing code, and renews a certificate about to expire', async () => {
  const runtime = fakeRuntime();
  const store = memoryStore();
  const codes = [];
  const identity = {kind: 'webcrypto-ed25519', publicKey: new Uint8Array(32).fill(0xbb), userPublicKey: 'bb'.repeat(32), sign: async () => new Uint8Array(64)};
  const peer = await connect(options(runtime, store, {userPublicKey: undefined, identity, onPairingCode: code => codes.push(code)}));
  assert.deepEqual(codes, []);
  assert.equal(runtime.calls.certify.length, 1);
  assert.equal(runtime.calls.certify[0].root, identity);
  assert.equal(runtime.peer.config.cert.device, 1);
  await peer.dispose();

  const again = await connect(options(runtime, store, {identity}));
  assert.equal(runtime.calls.certify.length, 1, 'a current certificate is reused');
  await again.dispose();
  await store.put('certificate', JSON.stringify({cert: {device: 9, expires_at_ns: (Date.now() + 3_600_000) * 1e6}, hostCert: {}}));
  const renewed = await connect(options(runtime, store, {identity}));
  assert.equal(runtime.calls.certify.length, 2, 'one about to expire is issued anew');
  await renewed.dispose();
  await assert.rejects(connect(options(runtime, store, {userPublicKey: 'cc'.repeat(32), identity})), TypeError);
});

test('a watching page sees the members live and sends nothing of its own', async () => {
  const runtime = fakeRuntime();
  const liveCarriers = fakeCarriers();
  const peer = await connect(options(runtime, memoryStore(), {liveCarriers, watching: true}));
  assert.equal(runtime.peer.config.watching, true, 'the session asks no channel for access');
  assert.equal(peer.voice, undefined, 'a watching page joins no call');
  await until(() => peer.live.state === 'live', 'the live layer');
  const [session] = liveCarriers.connects;
  assert.deepEqual(session.options.directions, {publish: false, subscribe: true});

  const presence = [];
  peer.live.onPresence(value => presence.push(value.publisher));
  session.memberWatcher(OTHER, true);
  session.presenceSubscribers.get(OTHER)(sealedFrame({seq: 1, sentAtMs: Date.now(), focus: null, pointer: {strip: '1', at: [1, 2]}, selection: null, drag: null}), performance.now());
  await until(() => presence.length, 'a member\'s cursor');

  peer.live.publishPresence({pointer: {strip: '1', at: [5, 6]}});
  const record = {app: 'so.tilo.notes', record: 'note-1'};
  assert.equal(peer.live.publishPreview(record, [{kind: 'insert', at: '01', text: 'x'}]), false);
  peer.live.endPreview(record);
  peer.setDisplayName('Visitor');
  runtime.peer.here = async () => { throw new Error('a watching page posted /here'); };
  assert.deepEqual(await peer.here(null, [{id: '01', action: 'Edited', resource: 'Note', app: 'Notes', at_ms: 1}]), []);
  // A presence change leaves at the end of the task it was made in (nothing
  // was sent before, so no rate limit defers it): one turn of the event loop
  // has run every send these calls queued.
  await settle();
  assert.deepEqual(session.presence, []);
  assert.deepEqual(session.ops, []);
  assert.deepEqual(session.closedOps, []);
  await peer.dispose();
});

test('a member page publishes on the live plane and keeps its call', async () => {
  const runtime = fakeRuntime();
  const liveCarriers = fakeCarriers();
  const peer = await connect(options(runtime, memoryStore(), {liveCarriers}));
  assert.equal(runtime.peer.config.watching, false);
  await until(() => peer.live.state === 'live', 'the live layer');
  assert.deepEqual(liveCarriers.connects[0].options.directions, {publish: true, subscribe: true});
  await peer.dispose();
});

test('the live layer joins over the carrier with the session grant and interpolates members', async () => {
  const runtime = fakeRuntime();
  const liveCarriers = fakeCarriers();
  const peer = await connect(options(runtime, memoryStore(), {liveCarriers}));
  await until(() => peer.live.state === 'live', 'the live layer');
  const [session] = liveCarriers.connects;
  assert.equal(session.options.endpoint, 'https://host.example:8443/moq/live');
  assert.deepEqual([...session.options.authorization], [1, 2]);
  assert.deepEqual(session.options.namespace, ['tilo-live', ROOM_HEX, SELF]);
  assert.equal(peer.live.state, 'live');

  const presence = [];
  peer.live.onPresence(value => presence.push(value.publisher));
  session.memberWatcher(OTHER, true);
  assert.deepEqual(peer.live.members(), [OTHER]);
  const frame = {seq: 1, sentAtMs: Date.now(), focus: null, pointer: {strip: '1', at: [1, 2]}, selection: null, drag: null};
  // An unsealed frame is never opened.
  session.presenceSubscribers.get(OTHER)(new TextEncoder().encode(JSON.stringify(frame)), performance.now());
  await settle();
  assert.deepEqual(presence, []);
  session.presenceSubscribers.get(OTHER)(sealedFrame(frame), performance.now());
  await until(() => presence.length, 'the sealed presence');
  assert.deepEqual(presence, [OTHER]);
  const [remote] = peer.live.remoteState(performance.now() + 50);
  assert.deepEqual(remote.pointer, {strip: '1', at: [1, 2]});

  const record = {app: 'so.tilo.notes', record: 'note-1'};
  peer.live.watchRecord(record);
  const track = runtime.liveRecordTrack(JSON.stringify(record));
  const preview = {kind: 'preview', record, seq: 1, ops: [{kind: 'insert', at: '01', text: 'x'}]};
  session.opsSubscribers.get(`${OTHER}/${track}`)(sealedFrame(preview));
  await until(() => Object.keys(peer.live.remoteState()[0].previews).length, 'the sealed preview');
  assert.equal(Object.keys(peer.live.remoteState()[0].previews).length, 1);
  runtime.peer.emit('ready', [{kind: 'durable', record}]);
  assert.equal(Object.keys(peer.live.remoteState()[0].previews).length, 0, 'the durable change supersedes previews');

  peer.live.publishPresence({pointer: {strip: '1', at: [5, 6]}});
  // The publisher sends at most 120 frames/s: the pointer may wait out the
  // interval after the participant frame.
  const lastSent = () => openedFrame(session.presence.at(-1));
  await until(() => lastSent().pointer !== null, 'the pointer frame');
  assert.deepEqual(lastSent().pointer, {strip: '1', at: [5, 6]});
  assert.ok(peer.live.publishPreview(record, [{kind: 'insert', at: '01', text: 'y'}]));
  await until(() => session.ops.length, 'the sealed preview frame');
  assert.equal(session.ops.at(-1).name, track);
  assert.equal(session.ops.at(-1).bytes[0], SEALED, 'only a sealed frame leaves');
  assert.ok(session.presence.every(bytes => bytes[0] === SEALED), 'only sealed presence leaves');

  // Without the record's keys nothing leaves at all.
  runtime.peer.sealing = false;
  const sentBefore = session.ops.length;
  peer.live.publishPreview(record, [{kind: 'insert', at: '01', text: 'z'}]);
  await settle();
  assert.equal(session.ops.length, sentBefore);
  await peer.dispose();
  assert.equal(peer.live.state, 'closed');
});

test('a lost live session reconnects with a fresh grant', async () => {
  const runtime = fakeRuntime();
  const liveCarriers = fakeCarriers();
  const peer = await connect(options(runtime, memoryStore(), {liveCarriers}));
  await until(() => peer.live.state === 'live', 'the live layer');
  liveCarriers.connects[0].close();
  await until(() => liveCarriers.connects.length === 2 && peer.live.state === 'live', 'the reconnect');
  assert.equal(liveCarriers.connects.length, 2);
  assert.equal(runtime.calls.grants, 2);
  await peer.dispose();
});

test('late decryption cannot revive a departed or rejoined publisher', async () => {
  const runtime = fakeRuntime();
  const liveCarriers = fakeCarriers();
  const peer = await connect(options(runtime, memoryStore(), {liveCarriers}));
  await until(() => peer.live.state === 'live', 'the live layer');
  const [session] = liveCarriers.connects;
  session.memberWatcher(OTHER, true);
  const original = runtime.peer.openLive.bind(runtime.peer);
  let release;
  runtime.peer.openLive = (...args) => new Promise(resolve => { release = () => resolve(original(...args)); });
  session.presenceSubscribers.get(OTHER)(sealedFrame({seq:1,sentAtMs:1,pointer: {strip: '1', at: [1, 2]}}),1);
  session.memberWatcher(OTHER, false);
  session.memberWatcher(OTHER, true);
  release();
  await settle();
  assert.deepEqual(peer.live.remoteState(), [], 'the previous membership epoch is gone');
});

test('reauthentication forgets the certificate and certifies again', async () => {
  const runtime = fakeRuntime();
  const store = memoryStore();
  const peer = await connect(options(runtime, store));
  runtime.peer.emit('failed');
  assert.equal(peer.getSnapshot().phase, 'failed');
  runtime.hostInfo = async () => JSON.stringify({hostCert: {host: 'new'}, webtransportPort: 443, certificateSha256: 'cd'.repeat(32)});
  await peer.recover({kind: 'reauthenticate'});
  assert.equal(runtime.calls.certify.length, 2);
  assert.equal(runtime.calls.started[0].disposed, true);
  assert.equal(peer.host.certificateSha256, 'cd'.repeat(32), 'a changed host is learnt again');
  await peer.recover({kind: 'retry-connection'});
  assert.equal(runtime.peer.reconnects, 1);
  await peer.dispose();
});

test('a kept certificate of another host than the one answering now is refused before the session starts', async () => {
  // The host at this origin was set up again (a new device key or root):
  // its predecessor's certificate and pinned state are never handed to it.
  const runtime = fakeRuntime();
  const store = memoryStore();
  const host = (user, device) => ({user_public_key: Array(32).fill(user), device_public_key: Array(32).fill(device)});
  await store.put('certificate', JSON.stringify({cert: {device: 9}, hostCert: host(1, 2)}));
  runtime.hostInfo = async () => JSON.stringify({hostCert: host(1, 3), webtransportPort: 443});
  await assert.rejects(connect(options(runtime, store)), {name: 'AdmissionError', code: 'host-changed', message: HOST_CHANGED});
  assert.equal(runtime.calls.started.length, 0, 'no session was started');
  runtime.hostInfo = async () => JSON.stringify({hostCert: host(1, 2), webtransportPort: 443});
  const same = await connect(options(runtime, store));
  assert.equal(runtime.calls.certify.length, 0, 'the same host takes the kept certificate');
  await same.dispose();
});

/** A profile's storage and page values, as `host.mjs` and the runtime keep them. */
function profileFakes(runtime) {
  const values = new Map();
  runtime.profile_value = (profile, device, name) => values.get(`${profile}/${device}/${name}`);
  runtime.set_profile_value = (profile, device, name, bytes) => {
    if (bytes === undefined) values.delete(`${profile}/${device}/${name}`);
    else values.set(`${profile}/${device}/${name}`, bytes);
  };
  const handles = [];
  const desktop = {
    async openProfileStorage(profile, device) {
      // One namespace at a time: the old one is let go before the next opens.
      assert.ok(handles.every(handle => handle.released), 'no other namespace is still open');
      const handle = {profile, device, released: 0, async flush() {}, async release() { handle.released++; }};
      handles.push(handle);
      return handle;
    },
    async mountDesktop(target, mountOptions) { return {target, ...mountOptions}; },
  };
  const rows = new Map();
  const namespaces = {getItem: key => rows.get(key) ?? null, setItem: (key, value) => rows.set(key, value)};
  return {values, handles, desktop, namespaces, vaultStorage: vaultMemory()};
}
const hostCertOf = (user, device) => ({user_public_key: Array(32).fill(user), device_public_key: Array(32).fill(device)});
const textOf = bytes => (bytes === undefined ? undefined : new TextDecoder().decode(bytes));
const person = () => ({kind: 'webcrypto-ed25519', publicKey: new Uint8Array(32).fill(0xee), userPublicKey: 'ee'.repeat(32), sign: async () => new Uint8Array(64)});

test('a renewed device is a new key in a namespace of its own; what the old key kept is never opened for it', async () => {
  const rows = new Map();
  const namespaces = {getItem: key => rows.get(key) ?? null, setItem: (key, value) => rows.set(key, value)};
  const profile = 'tilo-identity:ee@host.example:8443';
  let destroyed = false;
  const old = {slot: 1, created: false, destroy: async () => { destroyed = true; }};
  const opened = [];
  const open = async name => { opened.push(name); return {slot: 1, created: true}; };
  const renewed = await renewDevice(old, profile, {open, namespaces, random: bytes => bytes.fill(0xab)});
  assert.ok(destroyed, 'the old key is forgotten');
  assert.deepEqual(opened, [profile]);
  assert.equal(renewed.created, true);
  assert.equal(profileDevice(profile, namespaces, 1), `${PROFILE_DEVICE}:${'ab'.repeat(16)}#1`);
  assert.equal(profileDevice(profile, namespaces, 0), PROFILE_DEVICE, 'other slots stay where they were');
  // Another tab took the freed slot first: the key opened instead is not
  // new, and keeps its namespace.
  const taken = await renewDevice({slot: 0, destroy: async () => {}}, profile, {open: async () => ({slot: 2, created: false}), namespaces});
  assert.equal(taken.created, false);
  assert.equal(profileDevice(profile, namespaces, 2), `${PROFILE_DEVICE}#2`);
});

test('a device the host set up again admitted keeps its state apart; what was kept for the previous host is never opened for it', async () => {
  const runtime = fakeRuntime();
  const {values, handles, desktop, namespaces, vaultStorage} = profileFakes(runtime);
  const profile = 'tilo-identity:ee@host.example:8443';
  const identity = person();
  // What this browser kept for the host before it was set up again: its
  // certificate, and (in the runtime's record beside it) the outbox.
  const kept = JSON.stringify({cert: {device: 9}, hostCert: hostCertOf(1, 2)});
  values.set(`${profile}/${PROFILE_DEVICE}/certificate`, new TextEncoder().encode(kept));
  runtime.hostInfo = async () => JSON.stringify({hostCert: hostCertOf(1, 3), webtransportPort: 443});
  const common = {origin: 'https://host.example:8443', identity, runtime, custody, desktop, namespaces, vaultStorage, profile,
    liveCarriers: fakeCarriers(), fetch: async () => { throw new Error('no network in tests'); }};

  // Reconnecting with the kept state is refused, and nothing moves.
  await assert.rejects(connect({...common, room: ROOM}), {name: 'AdmissionError', code: 'host-changed', message: HOST_CHANGED});
  assert.equal(runtime.calls.started.length, 0, 'no session started');
  assert.equal(runtime.calls.certify.length, 0, 'nothing was certified by the new host');
  assert.equal(profileDevice(profile, namespaces), PROFILE_DEVICE, 'the profile stays where it was');

  // The new host admits the person (its door's invitation and token).
  const peer = await joinWorkspace({...common, invitation: Uint8Array.from([1, 2, 3]), admissionToken: 'one-time-token', storage: null});
  const fresh = `${PROFILE_DEVICE}:${'03'.repeat(32)}`;
  assert.equal(runtime.calls.started.length, 1);
  assert.equal(runtime.peer.namespace, JSON.stringify({profile, device: fresh}), 'the session runs in a namespace of the new host');
  assert.equal(runtime.peer.record, undefined, 'and is handed no record of its own');
  assert.equal(textOf(values.get(`${profile}/${PROFILE_DEVICE}/certificate`)), kept, 'what was kept for the previous host is untouched');
  assert.deepEqual(JSON.parse(textOf(values.get(`${profile}/${fresh}/certificate`))).hostCert, hostCertOf(1, 3));
  assert.equal(profileDevice(profile, namespaces), fresh, 'the profile keeps its peer there from now on');
  assert.equal(runtime.calls.asked.root, identity, 'the kept certificate is the previous host\'s: the device is certified anew');
  // The refused reconnect, the join reading the device's certificate there,
  // the admitted session leaving it, and the new host's namespace.
  assert.deepEqual(handles.map(handle => [handle.device, handle.released]),
    [[PROFILE_DEVICE, 1], [PROFILE_DEVICE, 1], [PROFILE_DEVICE, 1], [fresh, 0]]);
  assert.equal(vaultStorage.rows.size, 2, 'the new namespace seals its channel state with a vault of its own');
  assert.equal((await peer.mountDesktop('target', {app: 'scratch'})).device, fresh, 'its desktops mount there too');
  await peer.dispose();

  // The next visit reconnects there, with no new admission.
  const again = await connect({...common, room: ROOM});
  assert.equal(runtime.peer.namespace, JSON.stringify({profile, device: fresh}));
  await again.dispose();
  // No session ever ran in the previous host's namespace, so its record
  // and outbox never reached the new host.
  assert.ok(runtime.calls.started.every(started => JSON.parse(started.namespace).device === fresh));
  assert.equal(runtime.calls.started.length, 2);
});

test('a device the room let go asks to join again under the certificate it already holds from that host', async () => {
  // The room keeps the certificate of every device it admitted, also once
  // its person was removed or pruned after an absence, and refuses the same
  // device under another one: a certificate made anew would never get back in.
  const runtime = fakeRuntime();
  const {values, handles, desktop, namespaces, vaultStorage} = profileFakes(runtime);
  const profile = 'tilo-identity:ee@host.example:8443';
  const identity = person();
  const host = hostCertOf(1, 2);
  const certificate = {user_public_key: Array(32).fill(0xee), device_public_key: Array.from(custody.publicKey), issued_at_ns: 5};
  // Nanoseconds past what a JavaScript number holds exactly: only the kept
  // text itself still matches the certificate's signature.
  const kept = JSON.stringify({cert: certificate, hostCert: host}).replace('"issued_at_ns":5', '"issued_at_ns":1792040183583123457');
  values.set(`${profile}/${PROFILE_DEVICE}/certificate`, new TextEncoder().encode(kept));
  runtime.hostInfo = async () => JSON.stringify({hostCert: host, webtransportPort: 443});
  runtime.askToJoinAs = async (post, invitation, held, custodyKey, requestId, name, token) => {
    runtime.calls.askedAs = {held, token: token && new TextDecoder().decode(token)};
    return JSON.stringify({reply: {kind: 'admitted', room: ROOM, workspaceId: '7', name: 'Acme', role: 'Editor'}, cert: JSON.parse(held).cert});
  };
  const common = {origin: 'https://host.example:8443', identity, runtime, custody, desktop, namespaces, vaultStorage, profile,
    liveCarriers: fakeCarriers(), fetch: async () => { throw new Error('no network in tests'); }};
  const peer = await joinWorkspace({...common, invitation: Uint8Array.from([1, 2, 3]), admissionToken: 'one-time-token', storage: null});
  assert.equal(runtime.calls.askedAs.held, kept, 'asked under the certificate the device holds');
  assert.equal(runtime.calls.askedAs.token, 'one-time-token');
  assert.equal(runtime.calls.asked, undefined, 'no certificate made anew');
  assert.match(runtime.peer.configText, /"issued_at_ns":1792040183583123457[,}]/, 'and the session runs under it, exactly');
  assert.equal(textOf(values.get(`${profile}/${PROFILE_DEVICE}/certificate`)), kept, 'kept as the very text it was');
  assert.equal(runtime.calls.certify.length, 0);
  assert.deepEqual(handles.map(handle => [handle.device, handle.released]), [[PROFILE_DEVICE, 1], [PROFILE_DEVICE, 0]]);
  await peer.dispose();

  // Not the device's, or not the person's: certified anew, as before.
  for (const other of [{...certificate, device_public_key: Array(32).fill(0xdd)}, {...certificate, user_public_key: Array(32).fill(0xaa)}]) {
    const fresh = fakeRuntime();
    const fakes = profileFakes(fresh);
    fakes.values.set(`${profile}/${PROFILE_DEVICE}/certificate`, new TextEncoder().encode(JSON.stringify({cert: other, hostCert: host})));
    fresh.hostInfo = runtime.hostInfo;
    fresh.askToJoinAs = async () => assert.fail('another device\'s or person\'s certificate is never presented');
    const joined = await joinWorkspace({...common, runtime: fresh, desktop: fakes.desktop, namespaces: fakes.namespaces,
      vaultStorage: fakes.vaultStorage, invitation: Uint8Array.from([1, 2, 3]), admissionToken: 'one-time-token', storage: null});
    assert.equal(fresh.calls.asked.root, identity);
    await joined.dispose();
  }
});

test('a peer with a store of its own never moves it: a host set up again is refused even once it admitted the device', async () => {
  const runtime = fakeRuntime();
  const store = memoryStore();
  await store.put('certificate', JSON.stringify({cert: {device: 9}, hostCert: hostCertOf(1, 2)}));
  await store.put('peer-record', 'outbox for the previous host');
  runtime.hostInfo = async () => JSON.stringify({hostCert: hostCertOf(1, 3), webtransportPort: 443});
  await assert.rejects(joinWorkspace({origin: 'https://host.example:8443', invitation: Uint8Array.from([1, 2, 3]), identity: person(),
    admissionToken: 'one-time-token', runtime, custody, store, storage: null, liveCarriers: fakeCarriers(),
    fetch: async () => { throw new Error('no network in tests'); }}), {code: 'host-changed'});
  assert.equal(runtime.calls.started.length, 0, 'the kept record was never handed to a session');
  assert.deepEqual(JSON.parse(store.rows.get('certificate')).hostCert, hostCertOf(1, 2), 'nor its certificate replaced');
});

test('reauthenticating against a host set up again reloads the page to join it, keeping the certificate', async () => {
  const runtime = fakeRuntime();
  const store = memoryStore();
  runtime.hostInfo = async () => JSON.stringify({hostCert: hostCertOf(1, 2), webtransportPort: 443});
  await store.put('certificate', JSON.stringify({cert: {device: 9}, hostCert: hostCertOf(1, 2)}));
  const peer = await connect(options(runtime, store));
  runtime.hostInfo = async () => JSON.stringify({hostCert: hostCertOf(1, 3), webtransportPort: 443});
  let reloads = 0;
  const location = globalThis.location;
  globalThis.location = {reload: () => { reloads += 1; }};
  try {
    await peer.recover({kind: 'reauthenticate'});
  } finally {
    globalThis.location = location;
  }
  assert.equal(reloads, 1, 'the page loads again, where the entry joins the new host');
  assert.equal(runtime.calls.started.length, 1, 'no session started for the new host');
  assert.equal(runtime.calls.started[0].disposed, false, 'the running session was left alone');
  assert.deepEqual(JSON.parse(store.rows.get('certificate')).hostCert, hostCertOf(1, 2));
  assert.equal(runtime.calls.certify.length, 0);
});

test('a sealed seed is unsealed straight into the runtime', async () => {
  const runtime = fakeRuntime();
  const seed = new Uint8Array(32).fill(7);
  const sealed = {kind: 'sealed-seed', unseal: async () => ({seed})};
  const peer = await connect(options(runtime, memoryStore(), {custody: sealed}));
  assert.equal(runtime.calls.certify[0].custody.seed, seed);
  assert.equal(runtime.peer.custody.seed, seed);
  await peer.dispose();
});

test('with a profile the peer keeps its state in the profile storage and mounts there', async () => {
  const runtime = fakeRuntime();
  const values = new Map();
  runtime.profile_value = (profile, device, name) => values.get(`${profile}/${device}/${name}`);
  runtime.set_profile_value = (profile, device, name, bytes) => {
    if (bytes === undefined) values.delete(`${profile}/${device}/${name}`);
    else values.set(`${profile}/${device}/${name}`, bytes);
  };
  const opened = [];
  const mounts = [];
  const desktop = {
    async openProfileStorage(profile, device) {
      const handle = {profile, device, released: 0, flushed: 0,
        async flush() { handle.flushed++; }, async release() { handle.released++; }};
      opened.push(handle);
      return handle;
    },
    async mountDesktop(target, mountOptions) { mounts.push(mountOptions); return {target}; },
  };
  const vaultStorage = vaultMemory();
  const peer = await connect(options(runtime, undefined, {profile: 'work', device: 'laptop', displayName: 'Ada', desktop, vaultStorage}));
  assert.equal(opened.length, 1);
  assert.equal(runtime.peer.namespace, JSON.stringify({profile: 'work', device: 'laptop'}));
  const vault = runtime.peer.channelVault;
  assert.equal(vault.length, 32, 'the runtime gets the unsealed channel vault');
  assert.equal(vaultStorage.rows.size, 1);
  for (const [, stored] of vaultStorage.rows) assert.equal(stored.wrapKey.extractable, false);
  for (const [, bytes] of values) {
    assert.ok(!Buffer.from(bytes).toString('hex').includes(Buffer.from(vault).toString('hex')),
      'the profile storage never holds the vault');
  }
  assert.equal(runtime.peer.save, undefined, 'the outbox is saved by the runtime into the profile');
  assert.equal(runtime.peer.config.displayName, 'Ada');
  assert.ok(values.has('work/laptop/certificate'), 'the certificate is kept in the profile');

  await peer.mountDesktop('target', {app: 'scratch'});
  assert.equal(mounts[0].profile, 'work');
  assert.equal(mounts[0].device, 'laptop');
  assert.equal(mounts[0].room, ROOM);
  assert.equal(mounts[0].peer, peer);

  await peer.flush();
  assert.equal(opened[0].flushed, 1);
  await peer.dispose();
  assert.equal(opened[0].released, 1);

  const again = await connect(options(runtime, undefined, {profile: 'work', device: 'laptop', desktop, vaultStorage}));
  assert.equal(runtime.calls.certify.length, 1, 'the profile certificate is reused');
  assert.deepEqual(runtime.peer.channelVault, vault, 'the same vault opens the stored channel state');
  await again.dispose();
});

test('a presence subscription that ends while the member stays is retried', async t => {
  const runtime = fakeRuntime();
  const liveCarriers = fakeCarriers();
  const peer = await connect(options(runtime, memoryStore(), {liveCarriers}));
  await until(() => peer.live.state === 'live', 'the live layer');
  const [session] = liveCarriers.connects;
  session.memberWatcher(OTHER, true);
  assert.equal(session.presenceEnds.length, 1);
  // The retry backoff runs on the test's clock: past every backoff step, a
  // retry that was scheduled has run, and one that was not never will.
  t.mock.timers.enable({apis: ['setTimeout']});
  session.presenceEnds[0]();
  t.mock.timers.tick(60_000);
  assert.equal(session.presenceEnds.length, 2, 'subscribed again');
  session.memberWatcher(OTHER, false);
  session.presenceEnds[1]();
  t.mock.timers.tick(60_000);
  assert.equal(session.presenceEnds.length, 2, 'not after the member left');
  t.mock.timers.reset();
  await peer.dispose();
});

/** A `host.mjs` stand-in whose mounts are driven by the test through `status`. */
function fakeDesktop() {
  const mounts = [];
  const desktop = {
    mounts,
    async mountDesktop(target, mountOptions) {
      const mount = {
        target, options: mountOptions, recovered: 0, flushed: 0, retried: [], closed: false,
        status: status => mountOptions.onStatus?.(status),
        recover() { mount.recovered++; mount.onRecover?.(); },
        async retryAsset(assetId) { mount.retried.push(assetId); if (mount.assetFails) throw new Error('still unavailable'); },
        async flush() { mount.flushed++; if (mount.flushFails) throw new Error('flush failed'); },
        async cleanup() { mount.closed = true; },
        invokeAction(resource, action) { mount.onInvoke?.(resource, action); return {state: 'completed'}; },
      };
      mounts.push(mount);
      mount.status({phase: 'gpu-pending', id: 1});
      mount.status({phase: 'presented', gpuGeneration: 1, frames: 1});
      return mount;
    },
  };
  return desktop;
}

test('failure codes follow the mount and store errors', () => {
  assert.equal(rendererFailureCode('WebGPU device lost: Destroyed: gone; call recover'), 'gpu-device-lost');
  assert.equal(rendererFailureCode('WebGPU unavailable: no adapter'), 'gpu-unavailable');
  assert.equal(rendererFailureCode('WebGPU error: Validation Error: buffer too large; call recover'), 'renderer-initialization-failed');
  assert.equal(storageFailureCode('QuotaExceededError: full'), 'storage-quota');
  assert.equal(storageFailureCode('SecurityError: denied'), 'storage-denied');
  assert.equal(storageFailureCode('AbortError'), 'storage-transaction-failed');
  assert.deepEqual(statusAsset({asset: 'unavailable', assetId: 3}), {assetId: '3', failed: true, message: 'An image could not be loaded.'});
  assert.equal(statusAsset({font: 'available', fontAsset: 'Noto.ttf'}).assetId, 'font:Noto.ttf');
  assert.equal(statusAsset({data: 'unavailable', dataAsset: 'typst/a.otf'}).assetId, 'data:typst/a.otf');
  assert.equal(statusAsset({phase: 'presented'}), undefined);
});

test('retry-desktop recreates a lost renderer and resolves on a frame of the new GPU generation', async () => {
  const runtime = fakeRuntime();
  const desktop = fakeDesktop();
  const peer = await connect(options(runtime, memoryStore(), {desktop}));
  const seen = [];
  const mount = await peer.mountDesktop('target', {surfaceId: 'tilo-desktop-1', onStatus: status => seen.push(status.phase)});
  assert.equal(mount.surfaceId, 'tilo-desktop-1');
  assert.equal(mount.options.surfaceId, undefined, 'the surface id is not mount configuration');
  assert.deepEqual(seen, ['gpu-pending', 'presented'], 'statuses still reach the caller');

  mount.status({phase: 'failed', error: 'WebGPU device lost: Destroyed: ; call recover', gpuGeneration: 1});
  assert.deepEqual(runtime.peer.failures.map(f => [f.surface, f.asset, f.code]), [['tilo-desktop-1', undefined, 'gpu-device-lost']]);

  mount.onRecover = () => queueMicrotask(() => {
    mount.status({phase: 'failed', error: 'WebGPU device lost: Destroyed: ; call recover', gpuGeneration: 1});
    mount.status({phase: 'presented', gpuGeneration: 2, frames: 9});
  });
  const result = await peer.recover({kind: 'retry-desktop', surfaceId: 'tilo-desktop-1'});
  assert.equal(result.operationId, 'retry-desktop:tilo-desktop-1');
  assert.equal(mount.recovered, 1);
  assert.deepEqual(runtime.peer.failures, [], 'a presented frame of the new generation clears the failure');

  mount.onRecover = () => queueMicrotask(() => mount.status({phase: 'failed', error: 'WebGPU unavailable: no adapter', gpuGeneration: 3}));
  await assert.rejects(peer.recover({kind: 'retry-desktop', surfaceId: 'tilo-desktop-1'}), /Desktop recovery failed/);
  assert.equal(runtime.peer.failures[0].code, 'gpu-unavailable');
  await assert.rejects(peer.recover({kind: 'retry-desktop', surfaceId: 'nope'}), /Unknown desktop surface/);

  await mount.cleanup();
  assert.equal(mount.closed, true);
  assert.deepEqual(runtime.peer.failures, [], 'closing a surface clears its failures');
  await assert.rejects(peer.recover({kind: 'retry-desktop', surfaceId: 'tilo-desktop-1'}), /Unknown desktop surface/);
  await peer.dispose();
});

test('retry-asset fetches an image, font or data file again and clears its failure', async () => {
  const runtime = fakeRuntime();
  const desktop = fakeDesktop();
  const peer = await connect(options(runtime, memoryStore(), {desktop}));
  const mount = await peer.mountDesktop('target');
  assert.equal(mount.surfaceId, 'surface-1');
  mount.status({asset: 'unavailable', assetId: '4', assetUrl: 'https://example.com/a.png', error: 'asset HTTP 404'});
  mount.status({font: 'unavailable', fontAsset: 'Noto-COLRv1.ttf', fontError: 'SHA-256 mismatch'});
  mount.status({data: 'unavailable', dataAsset: 'typst/fonts.bin'});
  assert.deepEqual(runtime.peer.failures.map(f => f.asset), ['4', 'font:Noto-COLRv1.ttf', 'data:typst/fonts.bin']);
  assert.ok(runtime.peer.failures.every(f => f.surface === 'surface-1' && f.code === 'asset-unavailable'));

  await peer.recover({kind: 'retry-asset', surfaceId: 'surface-1', assetId: 'font:Noto-COLRv1.ttf'});
  assert.deepEqual(mount.retried, ['font:Noto-COLRv1.ttf']);
  assert.deepEqual(runtime.peer.failures.map(f => f.asset), ['4', 'data:typst/fonts.bin']);
  mount.status({asset: 'ready', assetId: '4'});
  assert.deepEqual(runtime.peer.failures.map(f => f.asset), ['data:typst/fonts.bin'], 'a ready status clears it too');

  mount.assetFails = true;
  await assert.rejects(peer.recover({kind: 'retry-asset', surfaceId: 'surface-1', assetId: 'data:typst/fonts.bin'}), /still unavailable/);
  assert.equal(runtime.peer.failures.length, 1, 'a failed retry keeps the failure');
  await peer.dispose();
});

test('retry-saving commits the store again, then every mount, and clears the storage failure', async () => {
  const runtime = fakeRuntime();
  runtime.profile_value = () => undefined;
  runtime.set_profile_value = () => {};
  const desktop = fakeDesktop();
  let storageListener;
  let flushFails = true;
  const storage = {flushed: 0,
    subscribe(listener) { storageListener = listener; return () => { storageListener = undefined; }; },
    async flush() { storage.flushed++; if (flushFails) throw new Error('AbortError: commit aborted'); },
    async release() {}};
  desktop.openProfileStorage = async () => storage;
  const peer = await connect(options(runtime, undefined, {profile: 'work', device: 'laptop', desktop, vaultStorage: vaultMemory()}));
  const mount = await peer.mountDesktop('target');

  storageListener({persistence: 'failed', pendingWrites: 2, error: 'QuotaExceededError: the quota has been exceeded'});
  mount.status({persistence: 'failed', pendingWrites: 2, error: 'AbortError: commit aborted'});
  assert.deepEqual(runtime.peer.failures.map(f => [f.surface, f.code]), [[undefined, 'storage-transaction-failed']], 'one storage failure, the newest');

  await assert.rejects(peer.recover({kind: 'retry-saving'}), /commit aborted/);
  assert.equal(runtime.peer.failures.length, 1);
  flushFails = false;
  assert.equal((await peer.recover({kind: 'retry-saving'})).operationId, 'retry-saving');
  assert.equal(storage.flushed, 2);
  assert.equal(mount.flushed, 1);
  assert.deepEqual(runtime.peer.failures, []);
  await peer.dispose();
  assert.equal(storageListener, undefined, 'dispose stops listening to the store');
});

test('refresh-checkpoint subscribes the room again and resolves once the host admitted it', async () => {
  const runtime = fakeRuntime();
  const peer = await connect(options(runtime, memoryStore()));
  runtime.peer.emit('ready');
  runtime.peer.onRefresh = () => setTimeout(() => {
    runtime.peer.admission = 'syncing';
    runtime.peer.emit();
    setTimeout(() => { runtime.peer.admission = 'ready'; runtime.peer.emit(); }, 5);
  }, 5);
  let settled = false;
  const refreshing = peer.recover({kind: 'refresh-checkpoint'}).then(result => { settled = true; return result; });
  assert.equal(runtime.peer.refreshes, 1);
  await tick();
  assert.equal(settled, false, 'the earlier admission is not evidence of the refresh');
  assert.equal((await refreshing).operationId, 'refresh-checkpoint');
  await peer.dispose();
});

test('httpsPost and the live plane accept a loopback http room for local review', () => {
  assert.doesNotThrow(() => httpsPost('http://localhost:9764'));
  assert.doesNotThrow(() => httpsPost('http://127.0.0.1:9764'));
  assert.throws(() => httpsPost('http://10.0.0.2:9764'), TypeError);
  assert.deepEqual(liveEndpoints('http://localhost:9764', {webtransportPort: 9700}), {
    endpoint: 'https://127.0.0.1:9700/moq/live', fallback: 'http://localhost:9764/moq/live',
  });
  assert.deepEqual(liveEndpoints('https://host.example:8443', {webtransportPort: 9700}), {
    endpoint: 'https://host.example:8443/moq/live', fallback: 'https://host.example:8443/moq/live',
  });
  // Behind an HTTP-only proxy: QUIC goes to the direct origin, the stream stays proxied.
  assert.deepEqual(liveEndpoints('https://rooms.example', {webtransportPort: 9781, webtransportOrigin: 'https://live.rooms.example'}), {
    endpoint: 'https://live.rooms.example/moq/live', fallback: 'https://rooms.example/moq/live',
  });
});

test('the live grant waits until the room admitted the device', async () => {
  const runtime = fakeRuntime({phase: 'admitting'});
  const liveCarriers = fakeCarriers();
  const peer = await connect(options(runtime, memoryStore(), {liveCarriers}));
  await tick();
  assert.equal(runtime.calls.grants, 0, 'the relay refuses a grant before admission');
  assert.equal(peer.live.available, false);
  const ready = peer.whenReady();
  runtime.peer.emit('ready');
  assert.equal((await ready).phase, 'ready');
  await until(() => peer.live.state === 'live', 'the live layer');
  assert.equal(runtime.calls.grants, 1);
  assert.equal(peer.live.state, 'live');
  assert.equal(peer.live.publisherId, SELF);
  const last = liveCarriers.connects[0].presence.at(-1);
  const sent = last ? openedFrame(last) : null;
  assert.equal(sent?.participant?.kind ?? 'person', 'person');
  await peer.dispose();
});

test("a person's presence colour is the runtime's for who they are, whatever their device key", async () => {
  const runtime = fakeRuntime();
  const peer = await connect(options(runtime, memoryStore()));
  // The page computes no colour of its own: the runtime's for the user
  // root, which a device key kept sealed (no public key here) cannot blank.
  assert.equal(peer.participant.color, runtime.presenceColor('bb'.repeat(32)));
  await peer.dispose();
});

test('leaving the page leaves the live plane and its host session at once, so others take it down; back online, the session retries now', async () => {
  const page = new EventTarget();
  globalThis.addEventListener = page.addEventListener.bind(page);
  globalThis.removeEventListener = page.removeEventListener.bind(page);
  const navigator = Object.getOwnPropertyDescriptor(globalThis, 'navigator');
  const beacons = [];
  Object.defineProperty(globalThis, 'navigator', {configurable: true, value: {sendBeacon: (url, body) => { beacons.push([url, body]); return true; }}});
  try {
    const runtime = fakeRuntime();
    const liveCarriers = fakeCarriers();
    const peer = await connect(options(runtime, memoryStore(), {liveCarriers}));
    await until(() => peer.live.state === 'live', 'the live layer');
    let closed = false;
    liveCarriers.connects[0].closed.then(() => { closed = true; });
    page.dispatchEvent(Object.assign(new Event('pagehide'), {persisted: true}));
    await tick();
    assert.equal(peer.live.state, 'live', 'a page kept to come back to stays');
    assert.deepEqual(beacons, [], 'and keeps its host session');
    page.dispatchEvent(Object.assign(new Event('pagehide'), {persisted: false}));
    await tick();
    assert.equal(peer.live.state, 'closed');
    assert.ok(closed, 'its session closed, withdrawing its announcement');
    assert.deepEqual(beacons, [['https://host.example:8443/peer/v1/session/end', 'ce'.repeat(32)]],
      'the host hears the page went, its session token the body');
    // Back online, the session tries again now instead of after its backoff.
    page.dispatchEvent(new Event('online'));
    assert.equal(runtime.peer.retries, 1);
    await peer.dispose();
    page.dispatchEvent(new Event('online'));
    assert.equal(runtime.peer.retries, 1, 'not once the page let the session go');
  } finally {
    delete globalThis.addEventListener;
    delete globalThis.removeEventListener;
    if (navigator) Object.defineProperty(globalThis, 'navigator', navigator);
    else delete globalThis.navigator;
  }
});

test('whenReady rejects when the host refuses the device', async () => {
  const runtime = fakeRuntime({phase: 'admitting'});
  const peer = await connect(options(runtime, memoryStore()));
  const ready = peer.whenReady();
  runtime.peer.emit('failed');
  await assert.rejects(ready, /refused/);
  await peer.dispose();
});

test('an agent layer publishes under the agent grant, and agents are told apart from people', async () => {
  const runtime = fakeRuntime();
  const liveCarriers = fakeCarriers();
  const peer = await connect(options(runtime, memoryStore(), {liveCarriers}));
  const agent = peer.agentLive('Reading agent');
  assert.equal(peer.agentLive('Reading agent'), agent, 'one layer per agent');
  await until(() => peer.live.state === 'live' && agent.state === 'live', 'both live layers');
  assert.deepEqual(runtime.calls.grantAgents, [[], ['Reading agent']]);
  const agentSession = liveCarriers.connects.find(session => session.options.namespace[2] === AGENT);
  assert.ok(agentSession, 'the agent layer joins under its own namespace');
  assert.equal(agent.agent, 'Reading agent');
  agent.publishPresence({selection: {anchor: '03', head: '03'}});
  const lastSent = () => (agentSession.presence.length ? openedFrame(agentSession.presence.at(-1)) : {});
  await until(() => lastSent().selection, 'the agent caret');
  const sent = lastSent();
  assert.deepEqual(sent.participant, {kind: 'agent', activity: null, label: 'Reading agent', color: sent.participant.color});

  // The person's layer names the agent as an agent, and people by their label.
  const [session] = liveCarriers.connects;
  const activity = [];
  const members = [];
  peer.live.onAgentActivity(value => activity.push(value));
  peer.live.onMember(value => members.push(value));
  const presence = (from, participant) => session.presenceSubscribers.get(from)(sealedFrame({
    seq: 1, sentAtMs: Date.now(), focus: null, pointer: null, selection: null, drag: null, participant,
  }), performance.now());
  session.memberWatcher(AGENT, true);
  session.memberWatcher(OTHER, true);
  presence(AGENT, {kind: 'agent', label: 'Reading agent', color: '#8e4ec6', activity: null});
  presence(OTHER, {kind: 'person', label: 'Visitor', color: '#e5484d', activity: null});
  await until(() => peer.live.people().length, 'the sealed presence of both');
  assert.deepEqual(peer.live.agents(), [{publisher: AGENT, agentId: 'Reading agent', name: 'Reading agent'}]);
  assert.deepEqual(peer.live.people(), [{device: OTHER, name: 'Visitor', color: '#e5484d'}]);
  assert.equal(activity[0].name, 'Reading agent');
  assert.deepEqual(members.at(-1), {publisher: OTHER, active: true}, 'a name that arrives is announced');
  await peer.dispose();
  assert.equal(agent.state, 'closed');
});

test('a member focusing a record subscribes its typing previews there', async () => {
  const runtime = fakeRuntime();
  const liveCarriers = fakeCarriers();
  const peer = await connect(options(runtime, memoryStore(), {liveCarriers}));
  await until(() => peer.live.state === 'live', 'the live layer');
  const [session] = liveCarriers.connects;
  session.memberWatcher(OTHER, true);
  const record = {app: 'so.tilo.word-processor', record: 'main'};
  session.presenceSubscribers.get(OTHER)(sealedFrame({
    seq: 1, sentAtMs: Date.now(), focus: record, pointer: null, selection: null, drag: null,
  }), performance.now());
  await until(() => session.opsSubscribers.size, 'the focused record subscription');
  assert.ok(session.opsSubscribers.has(`${OTHER}/${runtime.liveRecordTrack(JSON.stringify(record))}`));
  await peer.dispose();
});

test('mounts publish through their own view: the one acted in speaks, and an agent mount as its agent', async () => {
  const runtime = fakeRuntime();
  const liveCarriers = fakeCarriers();
  const desktop = fakeDesktop();
  const peer = await connect(options(runtime, memoryStore(), {liveCarriers, desktop}));
  await until(() => peer.live.state === 'live', 'the live layer');
  const target = () => {
    const handlers = new Map();
    return {handlers, addEventListener: (type, handler) => handlers.set(type, handler), fire: type => handlers.get(type)()};
  };
  const writerTarget = target();
  const helperTarget = target();
  const writer = await peer.mountDesktop(writerTarget, {app: 'so.tilo.word-processor'});
  const helper = await peer.mountDesktop(helperTarget, {app: 'so.tilo.chat'});
  const agentMount = await peer.mountDesktop(target(), {app: 'so.tilo.word-processor', agent: 'Reading agent'});
  await tick();
  const [session] = liveCarriers.connects;
  const selections = () => session.presence.map(bytes => openedFrame(bytes).selection?.anchor ?? null);
  const sent = anchor => until(() => selections().at(-1) === anchor, `the caret ${anchor}`);
  writer.options.live().publishPresence({selection: {anchor: 'w1', head: 'w1'}});
  await sent('w1');
  // A hidden helper's caret never overwrites the person's: it never
  // reaches the layer.
  assert.equal(helper.options.live().publishPresence({selection: {anchor: 'h1', head: 'h1'}}), false);
  assert.equal(selections().at(-1), 'w1');
  assert.ok(!selections().includes('h1'));
  helperTarget.fire('pointerdown');
  helper.options.live().publishPresence({selection: {anchor: 'h2', head: 'h2'}});
  await sent('h2');
  // Back in the writer: its unchanged caret is said again.
  writerTarget.fire('keydown');
  await sent('w1');
  // The agent's mount publishes on the agent's own layer.
  assert.equal(agentMount.options.live().agent, 'Reading agent');
  assert.equal(writer.options.live().agent, undefined);
  await helper.cleanup();
  await peer.dispose();
});

test('an action through a mount speaks for the person only if it moved that mount\'s caret', async () => {
  const runtime = fakeRuntime();
  const liveCarriers = fakeCarriers();
  const desktop = fakeDesktop();
  const peer = await connect(options(runtime, memoryStore(), {liveCarriers, desktop}));
  await until(() => peer.live.state === 'live', 'the live layer');
  const writer = await peer.mountDesktop(undefined, {app: 'so.tilo.word-processor'});
  const chat = await peer.mountDesktop(undefined, {app: 'so.tilo.chat'});
  await tick();
  const [session] = liveCarriers.connects;
  const selections = () => session.presence.map(bytes => openedFrame(bytes).selection?.anchor ?? null);
  const sent = anchor => until(() => selections().at(-1) === anchor, `the caret ${anchor}`);
  writer.options.live().publishPresence({selection: {anchor: 'w1', head: 'w1'}});
  await sent('w1');
  chat.options.live().publishPresence({selection: {anchor: 'c1', head: 'c1'}});
  // Posting leaves Chat's caret where it was: the person stays in the writer.
  let posted;
  chat.onInvoke = () => { posted = chat.options.live().publishPresence({selection: {anchor: 'c1', head: 'c1'}}); };
  assert.deepEqual(chat.invokeAction('chat.channel:general', 'post', {value: 'hi'}), {state: 'completed'});
  assert.equal(posted, false, 'Chat\'s unchanged caret never reaches the layer');
  assert.ok(!selections().includes('c1'));
  // Selecting a message moves it: Chat speaks now.
  chat.onInvoke = () => chat.options.live().publishPresence({selection: {anchor: 'c2', head: 'c2'}});
  chat.invokeAction('chat.message:1', 'select');
  await sent('c2');
  await peer.dispose();
});

test('the room\'s records this page holds, and waiting for one', async () => {
  const runtime = fakeRuntime();
  const peer = await connect(options(runtime, memoryStore()));
  assert.deepEqual(peer.records(), []);
  const arrived = peer.whenRecord('so.tilo.word-processor', 'derived:default@tab:0000000000000000', {timeoutMs: HANG_BOUND_MS});
  runtime.peer.records.push({app: 'so.tilo.word-processor', record: 'derived:default@tab:0000000000000000'});
  runtime.peer.emit();
  await arrived;
  assert.equal(peer.hasRecord('so.tilo.word-processor', 'derived:default@tab:0000000000000000'), true);
  assert.deepEqual(peer.records(), [{app: 'so.tilo.word-processor', record: 'derived:default@tab:0000000000000000'}]);
  await assert.rejects(peer.whenRecord('so.tilo.notes', 'missing', {timeoutMs: 20}), /no so.tilo.notes record missing/);
  await peer.dispose();
});

test('recertification retires voice before disposing the certified peer', async () => {
  const runtime = fakeRuntime();
  const retired = [];
  const peer = await connect(options(runtime, memoryStore(), {live: false, voice: {createCall: () => ({leave() {retired.push(runtime.peer.disposed);}, dispose() {}})}}));
  const previous = runtime.peer;
  const service = peer.voice;
  await peer.recover({kind: 'reauthenticate'});
  assert.deepEqual(retired, [false], 'the old call retires while its session can send leave');
  assert.equal(previous.disposed, true);
  assert.equal(peer.voice, service, 'mounted shared controls retain their media service');
  await peer.dispose();
});

test('raw record watches bind the member namespace and stop retrying after unwatch', async () => {
  let onMember;
  let endTrack;
  let deliver;
  let opened = 0;
  let canceled = 0;
  const timers = [];
  const record = {app: 'so.tilo.legacy-window', record: '@tilo:legacy-windows'};
  const runtime = fakeRuntime();
  const track = runtime.liveRecordTrack(JSON.stringify(record));
  const session = {
    closed: new Promise(() => {}), close() {}, publishPresence: () => true,
    watchMembers(options) { onMember = options.onMember; },
    subscribePresence() { return () => {}; },
    subscribeOps(publisher, name, onFrame, onEnd) {
      assert.equal(publisher, OTHER);
      assert.equal(name, track);
      opened++; deliver = onFrame; endTrack = onEnd;
      return () => { canceled++; };
    },
  };
  const layer = createLiveLayer({liveGrant: async () => JSON.stringify({token: '0102', room: ROOM_HEX,
    publisher: SELF, expiresAtNs: (Date.now() + 600000) * 1e6})}, runtime, ROOM, {
    endpoint: 'test', carriers: {connect: async () => session},
    setTimer: callback => {timers.push(callback); return timers.length;}, clearTimer() {},
  });
  await until(() => onMember, 'the live member watcher');
  const received = [];
  const stop = layer.watchRecordFrames(record, (publisher, bytes) => received.push([publisher, [...bytes]]));
  const only = [];
  const stopMember = layer.subscribeMemberRecord(OTHER, record, bytes => only.push([...bytes]));
  onMember(OTHER, true);
  assert.equal(opened, 1, 'one raw subscription serves both consumers');
  deliver(Uint8Array.from([0xf1, 4]));
  assert.deepEqual(received, [[OTHER, [0xf1, 4]]]);
  assert.deepEqual(only, [[0xf1, 4]]);
  stop();
  assert.equal(canceled, 0, 'the member consumer still owns the subscription');
  endTrack();
  stopMember();
  for (const callback of timers.splice(1)) callback(); // omit the grant-renewal timer
  assert.equal(opened, 1, 'an ended subscription has no consumers left to retry for');
  layer.stop();
});

test('joining with an invitation waits for the owner, then connects with the root-certified device', async () => {
  const runtime = fakeRuntime();
  const store = memoryStore();
  const rows = new Map();
  const storage = {getItem: key => rows.get(key) ?? null, setItem: (key, value) => rows.set(key, value)};
  const identity = {kind: 'webcrypto-ed25519', publicKey: new Uint8Array(32).fill(0xee), userPublicKey: 'ee'.repeat(32), sign: async () => new Uint8Array(64)};
  const offer = await inspectInvitation('https://host.example:8443', 'AQID', {runtime});
  assert.deepEqual(runtime.calls.inspected, [1, 2, 3]);
  assert.deepEqual(offer, {workspaceName: 'Acme', role: 'editor', room: ROOM, workspaceId: '7', expiresAtNs: '9',
    inviter: {name: 'Ada', userPublicKey: 'aa'.repeat(32)}});

  const progress = [];
  const peer = await joinWorkspace({
    origin: 'https://host.example:8443', invitation: Uint8Array.from([1, 2, 3]), identity, displayName: 'Bob',
    runtime, custody, store, storage, pollMs: 1, onProgress: event => progress.push(event.phase),
    liveCarriers: fakeCarriers(), fetch: async () => { throw new Error('no network in tests'); },
  });
  assert.deepEqual(progress, ['asking', 'waiting', 'waiting', 'admitted']);
  assert.equal(runtime.calls.asked.root, identity);
  assert.equal(runtime.calls.asked.name, 'Bob');
  assert.equal(runtime.calls.status[0].device, 'dd'.repeat(32));
  assert.equal(runtime.calls.status[0].requestId, runtime.calls.asked.requestId);
  assert.equal(runtime.calls.certify.length, 0, 'no certification at the host');
  assert.deepEqual(runtime.peer.config.cert, {guest: true, device_public_key: Array(32).fill(0xdd)});
  assert.equal(runtime.peer.room, ROOM);
  assert.equal(rows.get('tilo-join-requests'), '{}', 'the answered request is forgotten');
  await peer.dispose();
});

test('a device key opened for a peer is let go when it is disposed, aborted or fails to connect', async () => {
  const custody = () => ({closed: 0, close() { this.closed += 1; }});
  const disposed = custody();
  let inner = 0;
  const peer = await withCustody(disposed, async () => ({dispose: async () => { inner += 1; }}));
  assert.equal(disposed.closed, 0);
  await peer.dispose();
  assert.equal(inner, 1);
  assert.equal(disposed.closed, 1);

  const failed = custody();
  await assert.rejects(withCustody(failed, async () => { throw new Error('host unreachable'); }), /host unreachable/);
  assert.equal(failed.closed, 1);

  const aborted = custody();
  const controller = new AbortController();
  await withCustody(aborted, async () => ({dispose: async () => {}}), controller.signal);
  controller.abort();
  assert.equal(aborted.closed, 1);
});

test('a pre-admitted person joins without waiting for the owner', async () => {
  const runtime = fakeRuntime();
  const identity = {kind: 'webcrypto-ed25519', publicKey: new Uint8Array(32).fill(0xee), userPublicKey: 'ee'.repeat(32), sign: async () => new Uint8Array(64)};
  const progress = [];
  const peer = await joinWorkspace({
    origin: 'https://host.example:8443', invitation: Uint8Array.from([1, 2, 3]), identity, displayName: 'Cy',
    admissionToken: 'one-time-token', runtime, custody, store: memoryStore(), storage: null,
    onProgress: event => progress.push(event.phase),
    liveCarriers: fakeCarriers(), fetch: async () => { throw new Error('no network in tests'); },
  });
  assert.deepEqual(progress, ['asking', 'admitted']);
  assert.equal(runtime.calls.asked.token, 'one-time-token');
  assert.equal(runtime.calls.status, undefined, 'never polled');
  assert.equal(runtime.peer.room, ROOM);
  await peer.dispose();
});

test('an invitation the host will not honour is refused in plain words, and so is an admission it refuses', async () => {
  // Spent, expired, withdrawn or issued for someone else: the person is told
  // what to do, the host's reason goes to the console.
  const runtime = fakeRuntime();
  runtime.inspectInvitation = async () => JSON.stringify({kind: 'refused', reason: 'invitation was already redeemed'});
  await assert.rejects(inspectInvitation('https://host.example:8443', 'AQID', {runtime}),
    {name: 'AdmissionError', code: 'invitation-unusable', message: UNUSABLE_INVITATION});
  const identity = {kind: 'webcrypto-ed25519', publicKey: new Uint8Array(32).fill(0xee), userPublicKey: 'ee'.repeat(32), sign: async () => new Uint8Array(64)};
  const join = async reply => {
    const refusing = fakeRuntime();
    refusing.askToJoin = async () => JSON.stringify({reply, cert: {guest: true, device_public_key: Array(32).fill(0xdd)}});
    return joinWorkspace({origin: 'https://host.example:8443', invitation: Uint8Array.from([1, 2, 3]), identity,
      admissionToken: 'another-persons-token', runtime: refusing, custody, store: memoryStore(), storage: null,
      liveCarriers: fakeCarriers(), fetch: async () => { throw new Error('no network in tests'); }});
  };
  await assert.rejects(join({kind: 'refused', reason: 'this invitation is bound to another person'}),
    {name: 'AdmissionError', code: 'refused', message: REFUSED_JOIN});
  await assert.rejects(join({kind: 'declined'}), {name: 'AdmissionError', code: 'refused', message: /declined/});
});

test('an old membership retry cannot replace a rejoined publishers presence subscription', async () => {
  const runtime = fakeRuntime();
  const carriers = fakeCarriers();
  const timers = [];
  const layer = createLiveLayer({
    liveGrant: async () => JSON.stringify({token: '0102', room: ROOM_HEX, publisher: SELF, expiresAtNs: (Date.now() + 600000) * 1e6}),
    sealLive: async (_room, bytes) => bytes,
    openLive: async (_room, bytes) => bytes.slice(1),
  }, runtime, ROOM, {
    endpoint: 'test', carriers,
    setTimer: callback => { timers.push(callback); return timers.length; }, clearTimer() {},
  });
  try {
    await until(() => carriers.connects[0]?.memberWatcher, 'the live member watcher');
    const session = carriers.connects[0];
    session.memberWatcher(OTHER, true);
    session.presenceEnds[0]();
    const oldRetry = timers.at(-1);
    session.memberWatcher(OTHER, false);
    session.memberWatcher(OTHER, true);
    session.presenceEnds[1]();
    const currentRetry = timers.at(-1);
    oldRetry();
    currentRetry();
    session.presenceSubscribers.get(OTHER)(sealedFrame({seq: 1, sentAtMs: 1, pointer: {strip: '1', at: [10, 20]}}), 1);
    await settle();
    assert.deepEqual(layer.remoteState(1).map(remote => remote.pointer?.at), [[10, 20]], 'the current membership must retain a working subscription');
  } finally {
    layer.stop();
  }
});


test('each live grant tells the host what the page decodes; an agent\'s layer does not speak for the page', async () => {
  const advertised = [];
  const peerOf = () => ({
    liveGrant: async () => JSON.stringify({token: '0102', room: ROOM_HEX, publisher: SELF, expiresAtNs: (Date.now() + 600000) * 1e6}),
    legacyCodecs: async (room, codecs) => { advertised.push([room, JSON.parse(codecs)]); return true; },
  });
  const timers = [];
  const settings = (carriers, pending = []) => ({
    endpoint: 'test', carriers, decodable: async () => ['h264', 'vp9'],
    setTimer: callback => { pending.push(callback); return pending.length; }, clearTimer() {},
  });
  const carriers = fakeCarriers();
  const layer = createLiveLayer(peerOf(), fakeRuntime(), ROOM, settings(carriers, timers));
  const agentCarriers = fakeCarriers();
  const agent = createLiveLayer(peerOf(), fakeRuntime(), ROOM, {...settings(agentCarriers), agent: 'summarizer'});
  try {
    await until(() => advertised.length === 1, 'the codec advert');
    assert.deepEqual(advertised, [[ROOM, ['h264', 'vp9']]]);
    await until(() => agentCarriers.connects[0], 'the agent layer connected');
    // The grant is renewed: the advert goes with the new one.
    timers[0]();
    await until(() => advertised.length === 2, 'the advert with the renewed grant');
    await settle();
    assert.equal(advertised.length, 2, 'the agent layer never advertised');
  } finally {
    layer.stop();
    agent.stop();
  }
});

test('resource hydration can be cancelled and leaving the peer refuses pending opens', async () => {
  const runtime = fakeRuntime();
  const peer = await connect(options(runtime, memoryStore()));
  const cancelled = new AbortController();
  const pending = peer.whenRecord('so.tilo.chat', '', {signal: cancelled.signal});
  cancelled.abort();
  await assert.rejects(pending, error => error.name === 'AbortError');
  const leaving = peer.whenRecord('so.tilo.notes', 'missing');
  const refused = assert.rejects(leaving, /connection closed/);
  await peer.dispose();
  await refused;
  await assert.rejects(peer.whenRecord('so.tilo.notes', 'missing'), /connection closed/);
});

test('a member opening a link never certifies or renews a device implicitly', async () => {
  const runtime = fakeRuntime(), store = memoryStore();
  await assert.rejects(connect(options(runtime, store, {requireExistingCertificate: true})), /obtain admission/);
  assert.equal(runtime.calls.certify.length, 0);
  const prior = JSON.stringify({cert: {device: 'previous'}, hostCert: {host: true}});
  await store.put('certificate', prior);
  const peer = await connect(options(runtime, store, {requireExistingCertificate: true}));
  assert.equal(runtime.calls.certify.length, 0);
  assert.equal(runtime.peer.config.cert.device, 'previous');
  await assert.rejects(peer.recover({kind: 'reauthenticate'}), /identity flow/);
  assert.equal(await store.get('certificate'), prior, 'the saved certificate is preserved');
  assert.equal(runtime.calls.certify.length, 0);
  await peer.dispose();
});

test('leaving, cancelling and timing out pending room admission settle browser navigation', async () => {
  const runtime = fakeRuntime({phase: 'authenticating'});
  const peer = await connect(options(runtime, memoryStore()));
  const controller = new AbortController();
  const pending = peer.whenReady({signal: controller.signal});
  controller.abort();
  await assert.rejects(pending, error => error.name === 'AbortError');
  await assert.rejects(peer.whenReady({timeoutMs: 1}), /timed out/);
  const leaving = peer.whenReady();
  await peer.dispose();
  await assert.rejects(leaving, /closed/);
});


test('feedback startup waits for owner admission and never joins another room', async () => {
  const runtime = fakeRuntime();
  const peer = await connect(options(runtime, memoryStore(), {live: false, voice: false}));
  runtime.peer.hasRecord = () => true;
  let status = 'awaiting authenticated admission';
  let configured;
  runtime.peer.configureFeedback = value => { configured = JSON.parse(value); };
  runtime.peer.feedbackStatus = () => status;
  const destination = {room: ROOM, provider: 'record:so.tilo.chat:', channel: 'feedback'};
  const waiting = peer.configureFeedback(destination);
  await settle();
  assert.deepEqual(configured, destination);
  assert.equal(peer.feedbackStatus(), 'awaiting authenticated admission');
  status = 'ready';
  runtime.peer.emit();
  await waiting;
  assert.equal(runtime.peer.room, ROOM);
  await assert.rejects(peer.configureFeedback({...destination, room: 'another-room'}), /existing room/);
  status = 'owner refused current certified admission';
  await assert.rejects(peer.configureFeedback(destination), /owner refused/);
});

for (const attachSignal of [false, true]) test(`leaving a page joins peer storage release (signal=${attachSignal})`, async () => {
  const runtime = fakeRuntime();
  const values = new Map();
  runtime.profile_value = (profile, device, key) => values.get(`${profile}/${device}/${key}`);
  runtime.set_profile_value = (profile, device, key, bytes) => values.set(`${profile}/${device}/${key}`, bytes);
  let release, begun, reentered, releases = 0;
  const gate = new Promise(resolve => {release = resolve;});
  const started = new Promise(resolve => {begun = resolve;});
  const controller = new AbortController();
  const desktop = {
    async openProfileStorage() {
      return {
        async flush() {},
        async release() { releases++; begun(); reentered = peer.dispose(); await gate; },
      };
    },
  };
  const peer = await connect(options(runtime, undefined, {profile: 'overlap', device: 'owned', desktop,
    vaultStorage: vaultMemory(),...(attachSignal ? {signal: controller.signal} : {})}));
  let completed = false;
  controller.abort();
  const closing = peer.dispose().then(() => { completed = true; });
  try {
    await started;
    const repeated = peer.dispose();
    assert.equal(peer.dispose(), repeated, 'repeated disposal shares one completion');
    assert.equal(reentered, repeated, 'reentrant disposal joins the installed completion');
    await settle();
    assert.equal(completed, false, 'leaving must await storage release');
  } finally {
    release();
    await closing;
  }
  await peer.dispose();
  assert.equal(releases, 1, 'storage is released exactly once');
});


test('every disposal caller observes storage release failure', async () => {
  const runtime = fakeRuntime();
  runtime.profile_value = () => undefined;
  runtime.set_profile_value = () => {};
  const failure = new Error('storage release refused');
  let releases = 0;
  const desktop = {async openProfileStorage() { return {
    async flush() {}, async release() { releases++; throw failure; },
  }; }};
  const peer = await connectPeer(options(runtime, undefined, {
    profile: 'release-failure', device: 'owned', desktop, vaultStorage: vaultMemory(), live: false,
  }));
  const first = peer.dispose(), second = peer.dispose();
  assert.equal(first, second);
  const outcomes = await Promise.allSettled([first, second]);
  assert.deepEqual(outcomes, [{status: 'rejected', reason: failure}, {status: 'rejected', reason: failure}]);
  await assert.rejects(peer.dispose(), error => error === failure);
  assert.equal(releases, 1);
  assert.equal(runtime.peer.disposed, true);
});

test('the peer starts from the certificate text, keeping nanosecond timestamps exact', () => {
  const certified = '{"cert":{"issued_at_ns":1791962530023125779,"expires_at_ns":1792048930023125779},"hostCert":{"issued_at_ns":1}}';
  const started = withStartFields(certified, {displayName: 'Ada'});
  assert.ok(started.includes('1791962530023125779') && started.includes('1792048930023125779'));
  assert.equal(JSON.parse(started).displayName, 'Ada');
  assert.equal(withStartFields(certified, {displayName: undefined}), certified);
});
