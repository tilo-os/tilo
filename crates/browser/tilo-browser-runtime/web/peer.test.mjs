import test from 'node:test';
import assert from 'node:assert/strict';
import {createLiveLayer, connectPeer, httpsPost, inspectInvitation, joinWorkspace, liveEndpoints, rendererFailureCode, statusAsset, storageFailureCode} from './peer.mjs';

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
/** Resolves once `check()` holds; a loaded runner takes longer, not forever. */
async function until(check, what, timeoutMs = 10_000) {
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
    async certifyDevice(post, user, label, custody, onPairingCode) {
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
      return JSON.stringify({kind: 'invitation', room: ROOM, workspaceId: '7', name: 'Acme', role: 'Editor', expiresAtNs: '9'});
    },
    async askToJoin(post, invitation, root, custody, requestId, name, label) {
      calls.asked = {invitation: [...invitation], root, requestId, name, label};
      return JSON.stringify({reply: {kind: 'waiting'}, cert: {guest: true, device_public_key: Array(32).fill(0xdd)}});
    },
    async joinStatus(post, requestId, device) {
      calls.status = [...(calls.status ?? []), {requestId, device}];
      return JSON.stringify(calls.status.length < 2 ? {kind: 'waiting'} : {kind: 'admitted', room: ROOM, workspaceId: '7', name: 'Acme', role: 'Editor'});
    },
    encodeLivePresence: json => new TextEncoder().encode(json),
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
          config: JSON.parse(config), custody, namespace, record, save, post, channelVault, disposed: false, reconnects: 0,
          phase,
          records: [],
          admission: 'ready',
          failures: [],
          refreshes: 0,
          snapshot: () => JSON.stringify({phase: peer.phase, deviceId: 'dd', outgoingPending: 0, health: {version: 1, admission: {phase: peer.admission, pendingPrerequisites: peer.admission === 'ready' ? 0 : 1}}}),
          subscribe: listener => listeners.push(listener),
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
  const frame = {seq: 1, sentAtMs: Date.now(), focus: null, pointer: [1, 2], selection: null, drag: null};
  // An unsealed frame is never opened.
  session.presenceSubscribers.get(OTHER)(new TextEncoder().encode(JSON.stringify(frame)), performance.now());
  await settle();
  assert.deepEqual(presence, []);
  session.presenceSubscribers.get(OTHER)(sealedFrame(frame), performance.now());
  await until(() => presence.length, 'the sealed presence');
  assert.deepEqual(presence, [OTHER]);
  const [remote] = peer.live.remoteState(performance.now() + 50);
  assert.deepEqual(remote.pointer, [1, 2]);

  const record = {app: 'so.tilo.notes', record: 'note-1'};
  peer.live.watchRecord(record);
  const track = runtime.liveRecordTrack(JSON.stringify(record));
  const preview = {kind: 'preview', record, seq: 1, ops: [{kind: 'insert', at: '01', text: 'x'}]};
  session.opsSubscribers.get(`${OTHER}/${track}`)(sealedFrame(preview));
  await until(() => Object.keys(peer.live.remoteState()[0].previews).length, 'the sealed preview');
  assert.equal(Object.keys(peer.live.remoteState()[0].previews).length, 1);
  runtime.peer.emit('ready', [{kind: 'durable', record}]);
  assert.equal(Object.keys(peer.live.remoteState()[0].previews).length, 0, 'the durable change supersedes previews');

  peer.live.publishPresence({pointer: [5, 6]});
  // The publisher sends at most 120 frames/s: the pointer may wait out the
  // interval after the participant frame.
  const lastSent = () => openedFrame(session.presence.at(-1));
  await until(() => lastSent().pointer !== null, 'the pointer frame');
  assert.deepEqual(lastSent().pointer, [5, 6]);
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
  await new Promise(resolve => setTimeout(resolve, 400));
  assert.equal(liveCarriers.connects.length, 2);
  assert.equal(runtime.calls.grants, 2);
  await peer.dispose();
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

test('a presence subscription that ends while the member stays is retried', async () => {
  const runtime = fakeRuntime();
  const liveCarriers = fakeCarriers();
  const peer = await connect(options(runtime, memoryStore(), {liveCarriers}));
  await until(() => peer.live.state === 'live', 'the live layer');
  const [session] = liveCarriers.connects;
  session.memberWatcher(OTHER, true);
  assert.equal(session.presenceEnds.length, 1);
  session.presenceEnds[0]();
  await new Promise(resolve => setTimeout(resolve, 150));
  assert.equal(session.presenceEnds.length, 2, 'subscribed again');
  session.memberWatcher(OTHER, false);
  session.presenceEnds[1]();
  await new Promise(resolve => setTimeout(resolve, 150));
  assert.equal(session.presenceEnds.length, 2, 'not after the member left');
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
    endpoint: 'https://localhost:9700/moq/live', fallback: 'http://localhost:9764/moq/live',
  });
  assert.deepEqual(liveEndpoints('https://host.example:8443', {webtransportPort: 9700}), {
    endpoint: 'https://host.example:8443/moq/live', fallback: 'https://host.example:8443/moq/live',
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
  // A hidden helper's caret never overwrites the person's.
  helper.options.live().publishPresence({selection: {anchor: 'h1', head: 'h1'}});
  await new Promise(resolve => setTimeout(resolve, 50));
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
  chat.onInvoke = () => chat.options.live().publishPresence({selection: {anchor: 'c1', head: 'c1'}});
  assert.deepEqual(chat.invokeAction('chat.channel:general', 'post', {value: 'hi'}), {state: 'completed'});
  await new Promise(resolve => setTimeout(resolve, 50));
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
  const arrived = peer.whenRecord('so.tilo.word-processor', 'derived:default@tab:0000000000000000', {timeoutMs: 1000});
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
  assert.deepEqual(offer, {workspaceName: 'Acme', role: 'editor', room: ROOM, workspaceId: '7', expiresAtNs: '9', inviter: undefined});

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
