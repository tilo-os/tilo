import test from 'node:test';
import assert from 'node:assert/strict';
import {connectPeer, httpsPost, inspectInvitation, joinWorkspace, HOST_CHANGED, PROFILE_DEVICE, profileDevice, REFUSED_JOIN, renewDevice, UNUSABLE_INVITATION, rendererFailureCode, statusAsset, storageFailureCode, withCustody, withStartFields} from './peer.mjs';
import {runtimeFake} from './runtime-surface.mjs';
import {memberText} from './json-text.mjs';

const WORKSPACE = '6f1d3a52-1c7e-4b8e-9d2a-0f4a8c3e5b71';
const settle = () => new Promise(resolve => setTimeout(resolve, 0));
const SELF = 'dd'.repeat(32);
const OTHER = 'cc'.repeat(32);
const AGENT = 'ee'.repeat(32);
const tick = () => new Promise(resolve => setTimeout(resolve, 5));
/** The iroh endpoint a host publishes as its address. */
const HOST_ADDRESS = {endpointAddr: {}};
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

function fakeRuntime({phase = 'ready'} = {}) {
  const calls = {certify: [], started: []};
  let peer;
  const runtime = runtimeFake({
    async hostInfo(post) { calls.hostPost = post; return JSON.stringify({address: HOST_ADDRESS, hostCert: {}}); },
    async certifyDevice(user, label, invite, custody, onPairingCode) {
      calls.certify.push({user, label, invite, custody});
      onPairingCode('123456');
      return JSON.stringify({cert: {device: calls.certify.length}, hostCert: {host: true}});
    },
    async certifyWithRoot(post, root, custody, label) {
      calls.certify.push({post, root, custody, label});
      return JSON.stringify({cert: {device: calls.certify.length, expires_at_ns: calls.expiresAtNs ?? (Date.now() + 30 * 86_400_000) * 1e6}, hostCert: {host: true}});
    },
    async inspectInvitation(post, invitation) {
      calls.inspected = [...invitation];
      return JSON.stringify({kind: 'invitation', room: WORKSPACE, workspaceId: '7', name: 'Acme', role: 'Editor', expiresAtNs: '9',
        members: 4, inviter: {name: 'Ada', userPublicKey: 'aa'.repeat(32)}});
    },
    /** The shared rule of how a device comes back (tilo-session-host's
     * `reconnecting`, tested there), as far as these tests lean on it. */
    reconnectCertificate(kept, admitted, hostCert, withRoot) {
      const same = (a, b) => !a || !b || (String(a.user_public_key) === String(b.user_public_key)
        && String(a.device_public_key) === String(b.device_public_key));
      const host = JSON.parse(hostCert);
      const keptCert = kept ? JSON.parse(kept) : null;
      const elsewhere = Boolean(keptCert) && !same(keptCert.hostCert, host);
      if (admitted) return elsewhere ? 'adopt-host' : 'admitted';
      if (elsewhere) return 'host-changed';
      const ends = keptCert?.cert?.expires_at_ns;
      const ending = withRoot && typeof ends === 'number' && ends / 1e6 < Date.now() + 86_400_000;
      return keptCert && !ending ? 'kept' : 'certify';
    },
    /** The shared entry: admits at once, under the certificate the page
     * held when it held one (as text, so its nanoseconds stay exact), else
     * under a guest certificate of the root's. */
    async acceptInvitation(post, invitation, custody, root, options) {
      const asked = JSON.parse(options);
      calls.accepted = {invitation: [...invitation], root, ...asked};
      if (calls.refuse) throw Object.assign(new Error(calls.refuse), {name: 'HostRefusal', code: 'not_admitted'});
      const cert = asked.held ? memberText(asked.held, 'cert') : JSON.stringify({guest: true, device_public_key: Array(32).fill(0xdd)});
      const info = await runtime.hostInfo(post);
      const host = memberText(info, 'hostCert');
      return `{"admission":{"workspace":"${WORKSPACE}","workspace_id":7,"name":"Acme","role":"Editor"},"certificate":{"cert":${cert},"hostCert":${host}},"host":${info}}`;
    },
    /** The platform secret store's channel vault key of a namespace. */
    async channelVault(namespace) {
      calls.vaults = [...(calls.vaults ?? []), namespace];
      return new Uint8Array(32).fill(0x5a);
    },
    presenceColor: id => `#${String(id).padEnd(6, '0').slice(0, 6).replace(/[^0-9a-f]/g, '0')}`,
    WasmPeer: {
      start(config, custody, namespace, record, save, channelVault) {
        const listeners = [];
        const durable = [];
        const liveEvents = [];
        peer = {
          config: JSON.parse(config), configText: config, custody, namespace, record, save, channelVault, disposed: false, reconnects: 0, retries: 0,
          phase,
          records: [],
          admission: 'ready',
          failures: [],
          refreshes: 0,
          snapshot: () => JSON.stringify({phase: peer.phase, deviceId: 'dd', outgoingPending: 0, health: {version: 1, admission: {phase: peer.admission, pendingPrerequisites: peer.admission === 'ready' ? 0 : 1}}}),
          subscribe: listener => listeners.push(listener),
          sessionToken: () => 'ce'.repeat(32),
          joinWorkspace: workspace => { peer.workspace = workspace; },
          workspaceRecords: () => JSON.stringify(peer.records),
          hasRecord: (app, record) => peer.records.some(row => row.app === app && row.record === record),
          takeDurableEvents: () => JSON.stringify(durable.splice(0)),
          /** This page's device key: the publisher the workspace knows it by. */
          deviceKey: () => SELF,
          said: [], published: [], left: [], previews: [], endedPreviews: [],
          /** The page's word of itself: the board keeps it and says what changed. */
          sayPresence(workspace, value) { const said = JSON.parse(value); peer.said.push({workspace, value: said}); peer.published.push(said); return 0; },
          presenceDue: () => 0,
          presenceMembers: () => JSON.stringify(peer.memberList ?? []),
          leavePresence(workspace) { peer.left.push(workspace); },
          async publishPreview(workspace, record, seq, ops) {
            if (peer.sealing === false) return false;
            peer.previews.push({record: JSON.parse(record), seq: Number(seq), ops: JSON.parse(ops)});
            return true;
          },
          async endPreview(workspace, record) { peer.endedPreviews.push(JSON.parse(record)); },
          takeLiveEvents: () => JSON.stringify(liveEvents.splice(0)),
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
          /** What the session told this page: durable changes to read, and
           * what the members said. */
          emit(phase = peer.phase, events = [], said = []) { peer.phase = phase; durable.push(...events); liveEvents.push(...said); for (const listener of listeners) listener(); },
        };
        calls.started.push(peer);
        return peer;
      },
    },
  }, {calls, get peer() { return peer; }});
  return runtime;
}

const custody = {kind: 'webcrypto-ed25519', publicKey: new Uint8Array(32), sign: async () => new Uint8Array(64)};
const options = (runtime, store, extra = {}) => ({
  origin: 'https://host.example:8443', userPublicKey: 'bb'.repeat(32), label: 'Browser on Cloud', workspace: WORKSPACE,
  custody, store, runtime, fetch: async () => { throw new Error('no network in tests'); }, ...extra,
});

/** What the workspace's members said, as the session hands it to the page. */
const say = (runtime, said, durable = []) => runtime.peer.emit(runtime.peer.phase, durable, said);

test('httpsPost posts bodies to the host origin', async () => {
  const seen = [];
  const post = httpsPost('https://host.example:8443', async (url, init) => {
    seen.push({url, init});
    return {status: 204, arrayBuffer: async () => new ArrayBuffer(0)};
  });
  const answer = await post('/peer/v1/host', new Uint8Array([1]));
  assert.equal(answer.status, 204);
  assert.equal(seen[0].url, 'https://host.example:8443/peer/v1/host');
  assert.deepEqual(seen[0].init.body, new Uint8Array([1]));
  assert.equal(seen[0].init.credentials, 'omit');
  // Only the browser's own headers: a cross-origin host is asked without a
  // preflight round trip first.
  assert.equal(seen[0].init.headers, undefined);
  assert.throws(() => httpsPost('http://host.example'), TypeError);
});

test('first connect certifies with the host it names once, shows the pairing code and keeps the certificate', async () => {
  const runtime = fakeRuntime();
  const store = memoryStore();
  const codes = [];
  const peer = await connect(options(runtime, store, {onPairingCode: code => codes.push(code)}));
  assert.deepEqual(codes, ['123456']);
  assert.equal(runtime.calls.certify.length, 1);
  assert.equal(runtime.peer.workspace, WORKSPACE);
  assert.deepEqual(runtime.peer.config.cert, {device: 1});
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
  runtime.acceptInvitation = async () => `{"admission":{"workspace":"${WORKSPACE}","workspace_id":7,"name":"Acme","role":"Editor"},"certificate":{"cert":${LINUX_CERT},"hostCert":{"issued_at_ns":${NANOS}}}}`;
  runtime.hostInfo = async () => `{"address":${JSON.stringify(HOST_ADDRESS)},"hostCert":{"issued_at_ns":${NANOS}}}`;
  const identity = {kind: 'webcrypto-ed25519', publicKey: new Uint8Array(32).fill(0xee), userPublicKey: 'ee'.repeat(32), sign: async () => new Uint8Array(64)};
  const peer = await joinWorkspace({
    origin: 'https://host.example:8443', invitation: Uint8Array.from([1, 2, 3]), identity, admissionToken: 'token',
    runtime, custody, store: memoryStore(), storage: null,
    fetch: async () => { throw new Error('no network in tests'); },
  });
  assert.ok(runtime.peer.configText.includes(`"cert":${LINUX_CERT}`), runtime.peer.configText);
  assert.ok(runtime.peer.configText.includes(`"hostCert":{"issued_at_ns":${NANOS}}`));
  await peer.dispose();
});

test('a session after an entry dials the host the entry asked, without asking it again', async () => {
  const runtime = fakeRuntime();
  let asked = 0;
  const hostInfo = runtime.hostInfo;
  runtime.hostInfo = async post => { asked += 1; return hostInfo(post); };
  const identity = {kind: 'webcrypto-ed25519', publicKey: new Uint8Array(32).fill(0xee), userPublicKey: 'ee'.repeat(32), sign: async () => new Uint8Array(64)};
  const peer = await joinWorkspace({
    origin: 'https://host.example:8443', invitation: Uint8Array.from([1, 2, 3]), identity,
    runtime, custody, store: memoryStore(), storage: null,
    fetch: async () => { throw new Error('no network in tests'); },
  });
  assert.equal(asked, 1, 'the host is asked who it is once, by the entry');
  assert.deepEqual(runtime.peer.config.address, HOST_ADDRESS);
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

test('a member page joins its workspace as one device of it, and leaves it', async () => {
  const runtime = fakeRuntime();
  const peer = await connect(options(runtime, memoryStore()));
  await until(() => peer.live.state === 'live', 'the live layer');
  assert.deepEqual(runtime.peer.said.map(word => word.workspace), [WORKSPACE], 'its word of itself once live');
  assert.equal(runtime.peer.said[0].value.participant.kind, 'person', 'with who it is');
  assert.equal(peer.live.publisherId, SELF, 'the publisher the workspace knows this device by');

  peer.live.publishPresence({pointer: {tile: {tab: '7'}, at: [5, 6]}});
  await until(() => runtime.peer.said.at(-1).value.pointer, 'the pointer word');
  assert.deepEqual(runtime.peer.said.at(-1).value.pointer, {tile: {tab: '7'}, at: [5, 6]});

  peer.live.publishPresence({pointer: null});
  assert.ok(!('pointer' in runtime.peer.said.at(-1).value), 'the pointer taken off');

  const record = {app: 'so.tilo.notes', record: 'note-1'};
  assert.equal(peer.live.publishPreview(record, [{kind: 'insert', at: '01', text: 'y'}]), true);
  await until(() => runtime.peer.previews.length, 'the preview frame');
  assert.deepEqual(runtime.peer.previews.at(-1), {record, seq: 1, ops: [{kind: 'insert', at: '01', text: 'y'}]});
  peer.live.endPreview(record);
  await until(() => runtime.peer.endedPreviews.length, 'the preview end');
  assert.deepEqual(runtime.peer.endedPreviews, [record]);
  // Without the record's keys nothing leaves at all.
  runtime.peer.sealing = false;
  const before = runtime.peer.previews.length;
  await tick();
  assert.equal(peer.live.publishPreview(record, [{kind: 'insert', at: '01', text: 'z'}]), true);
  await settle();
  assert.equal(runtime.peer.previews.length, before, 'a preview that could not be sealed never left');

  await peer.dispose();
  assert.deepEqual(runtime.peer.left, [WORKSPACE], 'leaving withdraws it');
  assert.equal(peer.live.state, 'closed');
});

test('a layer says its word only once its session admitted it', async () => {
  const runtime = fakeRuntime({phase: 'admitting'});
  const peer = await connect(options(runtime, memoryStore()));
  await tick();
  assert.deepEqual(runtime.peer.said, [], 'not before the workspace knows the device');
  assert.equal(peer.live.available, false);
  const ready = peer.whenReady();
  runtime.peer.emit('ready');
  assert.equal((await ready).phase, 'ready');
  await until(() => peer.live.state === 'live', 'the live layer');
  assert.deepEqual(runtime.peer.said.map(word => word.workspace), [WORKSPACE]);
  assert.equal(peer.live.published ?? undefined, undefined);
  await peer.dispose();
});

test('reauthentication forgets the certificate and certifies again', async () => {
  const runtime = fakeRuntime();
  const store = memoryStore();
  const peer = await connect(options(runtime, store));
  runtime.peer.emit('failed');
  assert.equal(peer.getSnapshot().phase, 'failed');
  runtime.hostInfo = async () => JSON.stringify({address: HOST_ADDRESS, hostCert: {host: 'new'}});
  await peer.recover({kind: 'reauthenticate'});
  assert.equal(runtime.calls.certify.length, 2);
  assert.equal(runtime.calls.started[0].disposed, true);
  assert.deepEqual(peer.host.hostCert, {host: 'new'}, 'a changed host is learnt again');
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
  runtime.hostInfo = async () => JSON.stringify({address: HOST_ADDRESS, hostCert: host(1, 3)});
  await assert.rejects(connect(options(runtime, store)), {name: 'AdmissionError', code: 'host-changed', message: HOST_CHANGED});
  assert.equal(runtime.calls.started.length, 0, 'no session was started');
  runtime.hostInfo = async () => JSON.stringify({address: HOST_ADDRESS, hostCert: host(1, 2)});
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
  return {values, handles, desktop, namespaces};
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
  const {values, handles, desktop, namespaces} = profileFakes(runtime);
  const profile = 'tilo-identity:ee@host.example:8443';
  const identity = person();
  // What this browser kept for the host before it was set up again: its
  // certificate, and (in the runtime's record beside it) the outbox.
  const kept = JSON.stringify({cert: {device: 9}, hostCert: hostCertOf(1, 2)});
  values.set(`${profile}/${PROFILE_DEVICE}/certificate`, new TextEncoder().encode(kept));
  runtime.hostInfo = async () => JSON.stringify({address: HOST_ADDRESS, hostCert: hostCertOf(1, 3)});
  const common = {origin: 'https://host.example:8443', identity, runtime, custody, desktop, namespaces, profile,
    fetch: async () => { throw new Error('no network in tests'); }};

  // Reconnecting with the kept state is refused, and nothing moves.
  await assert.rejects(connect({...common, workspace: WORKSPACE}), {name: 'AdmissionError', code: 'host-changed', message: HOST_CHANGED});
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
  assert.equal(runtime.calls.accepted.held, kept, 'the entry is handed what the device kept, and decides it no longer serves');
  assert.equal(runtime.calls.accepted.root, identity, 'with the root that certifies it anew');
  // The refused reconnect, the join reading the device's certificate there,
  // the admitted session leaving it, and the new host's namespace.
  assert.deepEqual(handles.map(handle => [handle.device, handle.released]),
    [[PROFILE_DEVICE, 1], [PROFILE_DEVICE, 1], [PROFILE_DEVICE, 1], [fresh, 0]]);
  assert.equal(new Set(runtime.calls.vaults).size, 2, 'the new namespace seals its channel state with a vault of its own');
  assert.equal((await peer.mountDesktop('target', {app: 'desktop'})).device, fresh, 'its desktops mount there too');
  await peer.dispose();

  // The next visit reconnects there, with no new admission.
  const again = await connect({...common, workspace: WORKSPACE});
  assert.equal(runtime.peer.namespace, JSON.stringify({profile, device: fresh}));
  await again.dispose();
  // No session ever ran in the previous host's namespace, so its record
  // and outbox never reached the new host.
  assert.ok(runtime.calls.started.every(started => JSON.parse(started.namespace).device === fresh));
  assert.equal(runtime.calls.started.length, 2);
});

test('a device the workspace let go asks to join again under the certificate it already holds from that host', async () => {
  // The workspace keeps the certificate of every device it admitted, also once
  // its person was removed or pruned after an absence, and refuses the same
  // device under another one. Whether the kept one still serves is the shared
  // entry's to say (tilo-session-host's `entering_certificate`); the page
  // hands it over exactly as kept.
  const runtime = fakeRuntime();
  const {values, handles, desktop, namespaces} = profileFakes(runtime);
  const profile = 'tilo-identity:ee@host.example:8443';
  const identity = person();
  const host = hostCertOf(1, 2);
  const certificate = {user_public_key: Array(32).fill(0xee), device_public_key: Array.from(custody.publicKey), issued_at_ns: 5};
  // Nanoseconds past what a JavaScript number holds exactly: only the kept
  // text itself still matches the certificate's signature.
  const kept = JSON.stringify({cert: certificate, hostCert: host}).replace('"issued_at_ns":5', '"issued_at_ns":1792040183583123457');
  values.set(`${profile}/${PROFILE_DEVICE}/certificate`, new TextEncoder().encode(kept));
  runtime.hostInfo = async () => JSON.stringify({address: HOST_ADDRESS, hostCert: host});
  const common = {origin: 'https://host.example:8443', identity, runtime, custody, desktop, namespaces, profile,
    fetch: async () => { throw new Error('no network in tests'); }};
  const peer = await joinWorkspace({...common, invitation: Uint8Array.from([1, 2, 3]), admissionToken: 'one-time-token', storage: null});
  assert.equal(runtime.calls.accepted.held, kept, 'asked with the certificate the device holds');
  assert.equal(runtime.calls.accepted.admissionToken, 'one-time-token');
  assert.match(runtime.peer.configText, /"issued_at_ns":1792040183583123457[,}]/, 'and the session runs under it, exactly');
  assert.equal(textOf(values.get(`${profile}/${PROFILE_DEVICE}/certificate`)), kept, 'kept as the very text it was');
  assert.equal(runtime.calls.certify.length, 0);
  assert.deepEqual(handles.map(handle => [handle.device, handle.released]), [[PROFILE_DEVICE, 1], [PROFILE_DEVICE, 0]]);
  await peer.dispose();
});

test('a peer with a store of its own never moves it: a host set up again is refused even once it admitted the device', async () => {
  const runtime = fakeRuntime();
  const store = memoryStore();
  await store.put('certificate', JSON.stringify({cert: {device: 9}, hostCert: hostCertOf(1, 2)}));
  await store.put('peer-record', 'outbox for the previous host');
  runtime.hostInfo = async () => JSON.stringify({address: HOST_ADDRESS, hostCert: hostCertOf(1, 3)});
  await assert.rejects(joinWorkspace({origin: 'https://host.example:8443', invitation: Uint8Array.from([1, 2, 3]), identity: person(),
    admissionToken: 'one-time-token', runtime, custody, store, storage: null,
    fetch: async () => { throw new Error('no network in tests'); }}), {code: 'host-changed'});
  assert.equal(runtime.calls.started.length, 0, 'the kept record was never handed to a session');
  assert.deepEqual(JSON.parse(store.rows.get('certificate')).hostCert, hostCertOf(1, 2), 'nor its certificate replaced');
});

test('reauthenticating against a host set up again reloads the page to join it, keeping the certificate', async () => {
  const runtime = fakeRuntime();
  const store = memoryStore();
  runtime.hostInfo = async () => JSON.stringify({address: HOST_ADDRESS, hostCert: hostCertOf(1, 2)});
  await store.put('certificate', JSON.stringify({cert: {device: 9}, hostCert: hostCertOf(1, 2)}));
  const peer = await connect(options(runtime, store));
  runtime.hostInfo = async () => JSON.stringify({address: HOST_ADDRESS, hostCert: hostCertOf(1, 3)});
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
  const peer = await connect(options(runtime, undefined, {profile: 'work', device: 'laptop', displayName: 'Ada', desktop}));
  assert.equal(opened.length, 1);
  assert.equal(runtime.peer.namespace, JSON.stringify({profile: 'work', device: 'laptop'}));
  const vault = runtime.peer.channelVault;
  assert.equal(vault.length, 32, 'the runtime gets the unsealed channel vault');
  assert.deepEqual(runtime.calls.vaults, [`page:work/laptop:${'bb'.repeat(32)}`], 'the vault is the profile-and-device one');
  for (const [, bytes] of values) {
    assert.ok(!Buffer.from(bytes).toString('hex').includes(Buffer.from(vault).toString('hex')),
      'the profile storage never holds the vault');
  }
  assert.equal(runtime.peer.save, undefined, 'the outbox is saved by the runtime into the profile');
  assert.equal(runtime.peer.config.displayName, 'Ada');
  assert.ok(values.has('work/laptop/certificate'), 'the certificate is kept in the profile');

  await peer.mountDesktop('target', {app: 'desktop'});
  assert.equal(mounts[0].profile, 'work');
  assert.equal(mounts[0].device, 'laptop');
  assert.equal(mounts[0].workspace, WORKSPACE);
  assert.equal(mounts[0].peer, peer);

  await peer.flush();
  assert.equal(opened[0].flushed, 1);
  await peer.dispose();
  assert.equal(opened[0].released, 1);

  const again = await connect(options(runtime, undefined, {profile: 'work', device: 'laptop', desktop}));
  assert.equal(runtime.calls.certify.length, 1, 'the profile certificate is reused');
  assert.deepEqual(runtime.peer.channelVault, vault, 'the same vault opens the stored channel state');
  await again.dispose();
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
  const peer = await connect(options(runtime, undefined, {profile: 'work', device: 'laptop', desktop}));
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

test('refresh-checkpoint subscribes the workspace again and resolves once the host admitted it', async () => {
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

test('httpsPost accepts a loopback http workspace for local review', () => {
  assert.doesNotThrow(() => httpsPost('http://localhost:9764'));
  assert.doesNotThrow(() => httpsPost('http://127.0.0.1:9764'));
  assert.throws(() => httpsPost('http://10.0.0.2:9764'), TypeError);
});

test("a person's presence colour is the runtime's for who they are, whatever their device key", async () => {
  const runtime = fakeRuntime();
  const peer = await connect(options(runtime, memoryStore()));
  // The page computes no colour of its own: the runtime's for the user
  // root, which a device key kept sealed (no public key here) cannot blank.
  assert.equal(peer.participant.color, runtime.presenceColor('bb'.repeat(32)));
  await peer.dispose();
});

test('leaving the page withdraws its presence from the workspace at once; back online, the session retries now', async () => {
  const page = new EventTarget();
  globalThis.addEventListener = page.addEventListener.bind(page);
  globalThis.removeEventListener = page.removeEventListener.bind(page);
  try {
    const runtime = fakeRuntime();
    const peer = await connect(options(runtime, memoryStore()));
    await until(() => peer.live.state === 'live', 'the live layer');
    assert.deepEqual(runtime.peer.said.at(-1).workspace, WORKSPACE);
    page.dispatchEvent(Object.assign(new Event('pagehide'), {persisted: true}));
    await tick();
    assert.equal(peer.live.state, 'live', 'a page kept to come back to stays');
    assert.deepEqual(runtime.peer.left, [], 'and keeps its place');
    page.dispatchEvent(Object.assign(new Event('pagehide'), {persisted: false}));
    await tick();
    assert.equal(peer.live.state, 'closed');
    assert.deepEqual(runtime.peer.left, [WORKSPACE],
      'the members take it down at once, not when its keepalive stops');
    // Back online, the session tries again now instead of after its backoff.
    page.dispatchEvent(new Event('online'));
    assert.equal(runtime.peer.retries, 1);
    await peer.dispose();
    page.dispatchEvent(new Event('online'));
    assert.equal(runtime.peer.retries, 1, 'not once the page let the session go');
  } finally {
    delete globalThis.addEventListener;
    delete globalThis.removeEventListener;
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

test('a page waiting to show its desktop goes on once it holds the shared desktop, before the rest arrives', async () => {
  const runtime = fakeRuntime({phase: 'admitting'});
  const peer = await connect(options(runtime, memoryStore()));
  let shown = false;
  let ready = false;
  const showing = peer.whenReady({shown: true}).then(() => { shown = true; });
  const whole = peer.whenReady().then(() => { ready = true; });
  // Another record first: the desktop is not shown from it.
  runtime.peer.records.push({app: 'so.tilo.notes', record: 'note-1'});
  runtime.peer.emit('admitting');
  await settle();
  assert.equal(shown, false);
  runtime.peer.records.push({app: 'so.tilo.browser-desktop', record: 'desktop'});
  runtime.peer.emit('admitting');
  await showing;
  assert.equal(ready, false, 'the rest of the workspace is still on its way');
  runtime.peer.emit('ready');
  await whole;
  await peer.dispose();
});

test('a page whose workspace sent no desktop yet shows once the host has replayed it all', async () => {
  const runtime = fakeRuntime({phase: 'admitting'});
  const peer = await connect(options(runtime, memoryStore()));
  let shown = false;
  const showing = peer.whenReady({shown: true}).then(() => { shown = true; });
  runtime.peer.records.push({app: 'so.tilo.notes', record: 'note-1'});
  runtime.peer.emit('admitting');
  await settle();
  assert.equal(shown, false, 'a desktop mounted now would start a record of its own');
  runtime.peer.emit('ready');
  await showing;
  await peer.dispose();
});

test('mounts publish through their own view: the one acted in speaks', async () => {
  const runtime = fakeRuntime();
  const desktop = fakeDesktop();
  const peer = await connect(options(runtime, memoryStore(), {desktop}));
  await until(() => peer.live.state === 'live', 'the live layer');
  const target = () => {
    const handlers = new Map();
    return {handlers, addEventListener: (type, handler) => handlers.set(type, handler), fire: type => handlers.get(type)()};
  };
  const writerTarget = target();
  const helperTarget = target();
  const writer = await peer.mountDesktop(writerTarget, {app: 'so.tilo.word-processor'});
  const helper = await peer.mountDesktop(helperTarget, {app: 'so.tilo.chat'});
  await tick();
  const selections = () => runtime.peer.published.map(value => value.selection?.anchor ?? null);
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
  await helper.cleanup();
  await peer.dispose();
});

test('an action through a mount speaks for the person only if it moved that mount\'s caret', async () => {
  const runtime = fakeRuntime();
  const desktop = fakeDesktop();
  const peer = await connect(options(runtime, memoryStore(), {desktop}));
  await until(() => peer.live.state === 'live', 'the live layer');
  const writer = await peer.mountDesktop(undefined, {app: 'so.tilo.word-processor'});
  const chat = await peer.mountDesktop(undefined, {app: 'so.tilo.chat'});
  await tick();
  const selections = () => runtime.peer.published.map(value => value.selection?.anchor ?? null);
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

test('the workspace\'s records this page holds, and waiting for one', async () => {
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

test('an invitation is accepted as the root-certified device, through the shared entry', async () => {
  const runtime = fakeRuntime();
  const store = memoryStore();
  const identity = {kind: 'webcrypto-ed25519', publicKey: new Uint8Array(32).fill(0xee), userPublicKey: 'ee'.repeat(32), sign: async () => new Uint8Array(64)};
  const offer = await inspectInvitation('https://host.example:8443', 'AQID', {runtime});
  assert.deepEqual(runtime.calls.inspected, [1, 2, 3]);
  // What the card says comes from here: the workspace, how many people are in
  // it, the part offered and who offers it.
  assert.deepEqual(offer, {workspaceName: 'Acme', role: 'editor', room: WORKSPACE, workspaceId: '7', expiresAtNs: '9',
    members: 4, inviter: {name: 'Ada', userPublicKey: 'aa'.repeat(32)}});

  const progress = [];
  const peer = await joinWorkspace({
    origin: 'https://host.example:8443', invitation: Uint8Array.from([1, 2, 3]), identity, displayName: 'Bob',
    runtime, custody, store, storage: null, onProgress: event => progress.push(event.phase),
    fetch: async () => { throw new Error('no network in tests'); },
  });
  assert.deepEqual(progress, ['asking', 'admitted']);
  assert.deepEqual(runtime.calls.accepted.invitation, [1, 2, 3]);
  assert.equal(runtime.calls.accepted.root, identity);
  assert.equal(runtime.calls.accepted.name, 'Bob');
  assert.equal(runtime.calls.accepted.held, null, 'a store with no certificate holds none to enter with');
  assert.equal(runtime.calls.certify.length, 0, 'no certification at the host');
  assert.deepEqual(runtime.peer.config.cert, {guest: true, device_public_key: Array(32).fill(0xdd)});
  assert.equal(runtime.peer.workspace, WORKSPACE);
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
    fetch: async () => { throw new Error('no network in tests'); },
  });
  assert.deepEqual(progress, ['asking', 'admitted']);
  assert.equal(runtime.calls.accepted.admissionToken, 'one-time-token');
  assert.equal(runtime.peer.workspace, WORKSPACE);
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
  const refusing = fakeRuntime();
  refusing.calls.refuse = 'The workspace did not let this device in: this invitation is bound to another person';
  await assert.rejects(joinWorkspace({origin: 'https://host.example:8443', invitation: Uint8Array.from([1, 2, 3]), identity,
    admissionToken: 'another-persons-token', runtime: refusing, custody, store: memoryStore(), storage: null,
    fetch: async () => { throw new Error('no network in tests'); }}),
  {name: 'AdmissionError', code: 'refused', message: REFUSED_JOIN});
});

test('a person renamed since the page started says the new name with the next frame', async () => {
  const runtime = fakeRuntime();
  const peer = await connect(options(runtime, memoryStore()));
  await until(() => peer.live.state === 'live', 'the live layer');
  const labels = () => runtime.peer.published.map(value => value.participant?.label).filter(Boolean);
  await until(() => labels().length, 'the first announcement');
  assert.deepEqual(labels().at(-1), 'Browser on Cloud');
  peer.setDisplayName('Bea');
  peer.live.publishPresence({selection: {anchor: '03', head: '03'}});
  await until(() => labels().at(-1) === 'Bea', 'the name the person chose since');
  await peer.dispose();
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

test('leaving, cancelling and timing out pending workspace admission settle browser navigation', async () => {
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


test('a workspace’s reports go through the page’s peer in the destination’s workspace, by its runtime id', async () => {
  const runtime = fakeRuntime();
  const peer = await connect(options(runtime, memoryStore(), {live: false, voice: false}));
  const named = [];
  runtime.peer.id = 7;
  runtime.peer.sendFeedbackThrough = id => named.push(id);
  peer.sendFeedbackThrough({runtimeId: 9});
  assert.deepEqual(named, [9]);
  assert.throws(() => peer.sendFeedbackThrough({}), /connected peer/);
  await peer.dispose();
  assert.throws(() => peer.sendFeedbackThrough({runtimeId: 9}), /connected peer/);
});

test('feedback startup waits for owner admission and never joins another workspace', async () => {
  const runtime = fakeRuntime();
  const peer = await connect(options(runtime, memoryStore(), {live: false, voice: false}));
  runtime.peer.hasRecord = () => true;
  let status = 'awaiting authenticated admission';
  let configured;
  runtime.peer.configureFeedback = value => { configured = JSON.parse(value); };
  runtime.peer.feedbackStatus = () => status;
  const destination = {room: WORKSPACE, provider: 'record:so.tilo.chat:', channel: 'feedback'};
  const waiting = peer.configureFeedback(destination);
  await settle();
  assert.deepEqual(configured, destination);
  assert.equal(peer.feedbackStatus(), 'awaiting authenticated admission');
  status = 'ready';
  runtime.peer.emit();
  await waiting;
  assert.equal(runtime.peer.workspace, WORKSPACE);
  await assert.rejects(peer.configureFeedback({...destination, room: 'another-room'}), /existing workspace/);
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
    ...(attachSignal ? {signal: controller.signal} : {})}));
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
    profile: 'release-failure', device: 'owned', desktop, live: false,
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
