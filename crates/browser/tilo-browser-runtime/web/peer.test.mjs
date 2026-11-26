import test from 'node:test';
import assert from 'node:assert/strict';
import {connectPeer, httpsPost, inspectInvitation, joinWorkspace, pairDevice, HOST_CHANGED, PROFILE_DEVICE, profileDevice, REFUSED_JOIN, renewDevice, UNUSABLE_INVITATION, storageFailureCode, withDeviceKey, withStartFields} from './peer.mjs';
import {runtimeFake} from './runtime-surface.mjs';
import {memberText} from './json-text.mjs';
import {forgetSiteServers, nameSiteServer, STALE_SITE_SERVER, staleSiteServer} from './site-host.mjs';
import {failureCause} from './failure-screen.mjs';
import {hostUnreachable} from './session-refusal.mjs';
import {deviceRemoved} from './session-refusal.mjs';

const WORKSPACE = '6f1d3a52-1c7e-4b8e-9d2a-0f4a8c3e5b71';
const settle = () => new Promise(resolve => setTimeout(resolve, 0));
const SELF = 'dd'.repeat(32);
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
  let nextPeerId = 1;
  const runtime = runtimeFake({
    async hostInfo(post) { calls.hostPost = post; return JSON.stringify({address: HOST_ADDRESS, hostCert: {}}); },
    async certifyDevice(user, label, invite, deviceKey, onPairingCode) {
      calls.certify.push({user, label, invite, deviceKey});
      onPairingCode('123456');
      return JSON.stringify({cert: {device: calls.certify.length}, hostCert: {host: true}});
    },
    async certifyWithRoot(root, deviceKey, label, hostCert) {
      calls.certify.push({hostCert, root, deviceKey, label});
      return JSON.stringify({cert: {device: calls.certify.length, expires_at_ns: calls.expiresAtNs ?? (Date.now() + 30 * 86_400_000) * 1e6}, hostCert: {host: true}});
    },
    async inspectInvitation(post, invitation) {
      calls.inspected = [...invitation];
      return JSON.stringify({kind: 'invitation', workspace: WORKSPACE, workspaceId: '7', name: 'Acme', role: 'Editor', expiresAtNs: '9',
        members: 4, inviter: {name: 'Ada', userPublicKey: 'aa'.repeat(32)}});
    },
    /** The shared rule of how a device comes back (tilo-session-client's
     * `reconnecting`, tested there), as far as these tests lean on it. */
    reconnectCertificate(kept, admitted, hostCert, withRoot) {
      const same = (a, b) => !a || !b || (String(a.user_public_key) === String(b.user_public_key)
        && String(a.device_public_key) === String(b.device_public_key));
      const host = JSON.parse(hostCert);
      const keptCert = kept ? JSON.parse(kept) : null;
      const admittedCert = admitted ? JSON.parse(admitted) : null;
      const elsewhere = Boolean(keptCert) && !same(keptCert.hostCert, host);
      const otherDevice = Boolean(keptCert && admittedCert) && !same(keptCert.cert, admittedCert.cert);
      if (admitted) return elsewhere || otherDevice ? 'adopt-host' : 'admitted';
      if (elsewhere) return 'host-changed';
      const ends = keptCert?.cert?.expires_at_ns;
      const ending = withRoot && typeof ends === 'number' && ends / 1e6 < Date.now() + 86_400_000;
      return keptCert && !ending ? 'kept' : 'certify';
    },
    /** The shared entry with an invitation: the certificate the page held
     * when it held one (as text, so its nanoseconds stay exact), else a guest
     * certificate of the root's, and the entry the session asks for as it
     * opens, carrying the invitation. */
    async invitationEntry(hostCert, invitation, deviceKey, root, options) {
      const asked = JSON.parse(options);
      calls.accepted = {hostCert, invitation: [...invitation], root, ...asked};
      const cert = asked.held ? memberText(asked.held, 'cert') : JSON.stringify({guest: true, device_public_key: Array(32).fill(0xdd)});
      const through = {Invitation: {invitation: [...invitation], token: asked.admissionToken ? [...new TextEncoder().encode(asked.admissionToken)] : null}};
      return `{"certificate":{"cert":${cert},"hostCert":${hostCert}},"entering":{"workspace":"${WORKSPACE}","name":${JSON.stringify(asked.name)},"request_id":[${Array(32).fill(7)}],"through":${JSON.stringify(through)}}}`;
    },
    /** The shared entry through an entry: the certificate the root makes now, as text, and the
     * entry the session asks for as it opens. */
    async entryRequest(hostCert, workspace, deviceKey, root, options) {
      calls.entryRequest = {hostCert, workspace, root, ...JSON.parse(options)};
      return `{"certificate":{"cert":${LINUX_CERT},"hostCert":${hostCert}},"entering":{"workspace":"${workspace}","name":"Ana","request_id":[${Array(32).fill(7)}],"through":"Entry"}}`;
    },
    /** The platform secret store's channel vault key of a namespace. */
    async channelVault(namespace) {
      calls.vaults = [...(calls.vaults ?? []), namespace];
      return new Uint8Array(32).fill(0x5a);
    },
    presenceColor: id => `#${String(id).padEnd(6, '0').slice(0, 6).replace(/[^0-9a-f]/g, '0')}`,
    WasmPeer: {
      start(config, signer, namespace, record, save, channelVault) {
        const listeners = [];
        const durable = [];
        const liveEvents = [];
        peer = {
          config: JSON.parse(config), configText: config, signer, namespace, record, save, channelVault, disposed: false, reconnects: 0, retries: 0,
          phase,
          admission: 'ready',
          failures: [],
          refreshes: 0,
          snapshot: () => JSON.stringify({phase: peer.phase, deviceId: 'dd', outgoingPending: 0, health: {version: 1, admission: {phase: peer.admission, pendingPrerequisites: peer.admission === 'ready' ? 0 : 1},
            ...(peer.transport ? {transport: {phase: peer.transport}} : {})},
            ...(peer.entry ? {entry: peer.entry} : {}),
            ...(peer.error ? {error: peer.error, errorCode: peer.errorCode} : {})}),
          subscribe: listener => listeners.push(listener),
          sessionToken: () => 'ce'.repeat(32),
          joinWorkspace: workspace => { peer.workspace = workspace; },
          takeDurableEvents: () => JSON.stringify(durable.splice(0)),
          /** This page's device key: the publisher the workspace knows it by. */
          deviceKey: () => SELF,
          /** The host's question about a computer a ticket brought, answered with the root the
           * page holds: what the page's peer passes its session's own answer back from. */
          approveAsked: async (root, asked) => JSON.stringify({root: root.userPublicKey, asked: JSON.parse(asked)}),
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
        // The page this session opens as, when it names the one it replaces.
        peer.id = arguments[6] ?? nextPeerId;
        nextPeerId = Math.max(nextPeerId, peer.id + 1);
        calls.started.push(peer);
        // A session that enters (through an entry, or with an invitation) hears the host's answer
        // soon after it starts.
        if (peer.config.entering) {
          const started = peer;
          setTimeout(() => { started.entry = calls.entry ?? {outcome: 'admitted', workspace: WORKSPACE, name: 'Lounge', role: 'editor'}; started.emit(); }, 1);
        }
        return peer;
      },
    },
  }, {calls, get peer() { return peer; }});
  return runtime;
}

const deviceKey = {kind: 'webcrypto-ed25519', publicKey: new Uint8Array(32), sign: async () => new Uint8Array(64)};
const options = (runtime, store, extra = {}) => ({
  origin: 'https://host.example:8443', userPublicKey: 'bb'.repeat(32), label: 'Browser on Cloud', workspace: WORKSPACE,
  deviceKey, store, runtime, fetch: async () => { throw new Error('no network in tests'); }, ...extra,
});

test('an action asked once is answered by the host, and nothing of it is kept in the page', async () => {
  // A host that says which device it is: only such a host has actions to ask.
  const runtime = certifiedBySite(fakeRuntime());
  runtime.hostInfo = async () => siteNamed();
  runtime.hostRefusal = text => JSON.stringify({code: JSON.parse(text).code ?? 'refused', message: JSON.parse(text).message ?? ''});
  const peer = await connect(options(runtime, memoryStore(), {voice: false, live: false}));
  const sent = [];
  const answers = [];
  runtime.peer.actionMessage = async (workspace, person, message) => {
    sent.push({workspace, person, asked: JSON.parse(message)});
    return JSON.stringify(answers.shift());
  };
  const target = {provider: 'tilo.vault', address: {resource: 'vault', action: 'add'}};
  const argument = {kind: 'key', belongsTo: ['https://api.example'], username: '', secret: 'typed into a protected field'};

  // The host ran it: its answer, under the id the caller named.
  answers.push({Receipt: {invocation: 'inv-1', status: {Completed: {output: {value: {reference: 'vault.item:00'}}}}}});
  assert.deepEqual(await peer.actions.once(target, argument, {invocation: 'inv-1'}),
    {invocation: 'inv-1', state: 'succeeded', output: {value: {reference: 'vault.item:00'}, resources: []}});
  // Asked as this page's person in its workspace, with the argument as typed.
  assert.deepEqual(sent, [{workspace: WORKSPACE, person: 'bb'.repeat(32),
    asked: {Invoke: {invocation: 'inv-1', target, argument: {Json: argument}}}}]);
  // Nothing of it is kept: the page knows no such invocation, so it holds no argument of it.
  await assert.rejects(peer.actions.status('inv-1'), {code: 'unknown_invocation'});

  // One the host accepted and still runs is followed by asking the host, not by what a page kept.
  answers.push({Receipt: {invocation: 'inv-2', status: 'Accepted'}});
  assert.deepEqual(await peer.actions.once(target, argument, {invocation: 'inv-2'}), {invocation: 'inv-2', state: 'accepted'});
  answers.push({Update: {state: {update: {Succeeded: {output: {value: {done: true}}}}}}});
  assert.deepEqual(await peer.actions.observe('inv-2'), {invocation: 'inv-2', state: 'succeeded', output: {value: {done: true}, resources: []}});
  assert.deepEqual(sent.at(-1).asked, {Observe: {invocation: 'inv-2'}});
  answers.push({});
  assert.deepEqual(await peer.actions.observe('inv-2'), {invocation: 'inv-2', state: 'unknown'});

  // A refusal is the host's own, and a host that says nothing is said so.
  answers.push({Receipt: {invocation: 'inv-3', status: {Rejected: {error: {code: 'unknown_action', message: 'No such action is offered'}}}}});
  await assert.rejects(peer.actions.once(target, argument, {invocation: 'inv-3'}), {code: 'unknown_action', message: 'No such action is offered'});
  answers.push({});
  await assert.rejects(peer.actions.once(target, argument), {code: 'unavailable'});
  // What is no target is refused before the host is asked.
  const asked = sent.length;
  await assert.rejects(peer.actions.once({provider: 'tilo.vault'}, argument), TypeError);
  assert.equal(sent.length, asked);
});

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
  runtime.invitationEntry = async hostCert => `{"certificate":{"cert":${LINUX_CERT},"hostCert":${hostCert}},"entering":{"workspace":"${WORKSPACE}","name":"Ana","request_id":[${Array(32).fill(7)}],"through":{"Invitation":{"invitation":[1,2,3],"token":null}}}}`;
  runtime.hostInfo = async () => `{"address":${JSON.stringify(HOST_ADDRESS)},"hostCert":{"issued_at_ns":${NANOS}}}`;
  const identity = {kind: 'webcrypto-ed25519', publicKey: new Uint8Array(32).fill(0xee), userPublicKey: 'ee'.repeat(32), sign: async () => new Uint8Array(64)};
  const peer = await joinWorkspace({
    origin: 'https://acme.example:8443', invitation: Uint8Array.from([1, 2, 3]), identity, admissionToken: 'token',
    runtime, deviceKey, store: memoryStore(), storage: null,
    fetch: async () => { throw new Error('no network in tests'); },
  });
  assert.ok(runtime.peer.configText.includes(`"cert":${LINUX_CERT}`), runtime.peer.configText);
  assert.ok(runtime.peer.configText.includes(`"hostCert":{"issued_at_ns":${NANOS}}`));
  await peer.dispose();
});

test('an entry is entered on the session itself: its first connect asks, and the host answers in its status', async () => {
  const runtime = fakeRuntime();
  let asked = 0;
  runtime.hostInfo = async () => { asked += 1; return `{"address":${JSON.stringify(HOST_ADDRESS)},"hostCert":{"issued_at_ns":${NANOS}}}`; };
  const identity = {kind: 'webcrypto-ed25519', publicKey: new Uint8Array(32).fill(0xee), userPublicKey: 'ee'.repeat(32), sign: async () => new Uint8Array(64)};
  const progress = [];
  const peer = await joinWorkspace({
    origin: 'https://host.example:8443', entry: `https://host.example:8443/peer/v1/directory/${WORKSPACE}/`, identity,
    displayName: 'Ana', runtime, deviceKey, store: memoryStore(), storage: null, onProgress: event => progress.push(event),
    fetch: async () => { throw new Error('no network in tests'); },
  });
  assert.equal(asked, 1, 'the host is asked who it is once, and nothing else over HTTPS');
  assert.equal(runtime.calls.entryRequest.workspace, WORKSPACE);
  assert.equal(runtime.calls.entryRequest.hostCert, `{"issued_at_ns":${NANOS}}`, 'the host certificate reaches the entry byte for byte');
  assert.equal(runtime.peer.config.entering.workspace, WORKSPACE, 'the session asks to enter as it opens');
  assert.ok(runtime.peer.configText.includes(`"cert":${LINUX_CERT}`), runtime.peer.configText);
  assert.equal(runtime.peer.workspace, WORKSPACE);
  assert.deepEqual(progress.at(-1), {phase: 'admitted', workspace: WORKSPACE, workspaceName: 'Lounge', role: 'editor'});
  await peer.dispose();
});

test('an entry that refuses on the session says so in its own code, and a workspace refusal is a refused join', async () => {
  for (const [entry, check] of [
    [{outcome: 'refused', code: 'full', message: 'come back later', retryAfterSecs: 30},
      error => error.name === 'DeviceRefusal' && error.code === 'full' && error.retryAfter === 30],
    [{outcome: 'refused', code: 'not-admitted', message: 'key known under another certificate'},
      error => error.name === 'AdmissionError' && error.code === 'refused' && error.message === REFUSED_JOIN],
  ]) {
    const runtime = fakeRuntime();
    runtime.calls.entry = entry;
    const identity = {kind: 'webcrypto-ed25519', publicKey: new Uint8Array(32).fill(0xee), userPublicKey: 'ee'.repeat(32), sign: async () => new Uint8Array(64)};
    await assert.rejects(joinWorkspace({
      origin: 'https://host.example:8443', entry: `https://host.example:8443/peer/v1/directory/${WORKSPACE}/`, identity,
      runtime, deviceKey, store: memoryStore(), storage: null,
      fetch: async () => { throw new Error('no network in tests'); },
    }), check);
    assert.ok(runtime.peer.disposed, 'a refused session is let go');
  }
});

test('a session after an entry dials the host the entry asked, without asking it again', async () => {
  const runtime = fakeRuntime();
  let asked = 0;
  const hostInfo = runtime.hostInfo;
  runtime.hostInfo = async post => { asked += 1; return hostInfo(post); };
  const identity = {kind: 'webcrypto-ed25519', publicKey: new Uint8Array(32).fill(0xee), userPublicKey: 'ee'.repeat(32), sign: async () => new Uint8Array(64)};
  const peer = await joinWorkspace({
    origin: 'https://host.example:8443', invitation: Uint8Array.from([1, 2, 3]), identity,
    runtime, deviceKey, store: memoryStore(), storage: null,
    fetch: async () => { throw new Error('no network in tests'); },
  });
  assert.equal(asked, 1, 'the host is asked who it is once, by the entry');
  assert.deepEqual(runtime.peer.config.address, HOST_ADDRESS);
  await peer.dispose();
});

/** What a site hands its page about the host at `https://host.example:8443` (`entry.json`
 * `home.host.info`), the host's device key `device`. */
const siteNamed = (device = 1) => `{"hostCert":{"user_public_key":[9],"device_public_key":[${device}],"issued_at_ns":${NANOS}},`
  + `"address":{"endpointAddr":{"id":"e${device}","addrs":[]},"relayUrl":"https://live.host.example/"}}`;
const home = (info = siteNamed()) => ({name: 'Lounge', host: {origin: 'https://host.example:8443', info}});
/** A runtime whose host certifies this page's device as the host the site names. */
const certifiedBySite = runtime => {
  runtime.certifyDevice = async () => `{"cert":${LINUX_CERT},"hostCert":${memberText(siteNamed(), 'hostCert')}}`;
  return runtime;
};
const refusalOptions = (runtime, extra = {}) => ({
  origin: 'https://host.example:8443', entry: `https://host.example:8443/peer/v1/directory/${WORKSPACE}/`,
  identity: {kind: 'webcrypto-ed25519', publicKey: new Uint8Array(32).fill(0xee), userPublicKey: 'ee'.repeat(32), sign: async () => new Uint8Array(64)},
  displayName: 'Ana', runtime, deviceKey, store: memoryStore(), storage: null,
  fetch: async () => { throw new Error('no network in tests'); }, ...extra,
});

test('an entry whose site names its host is entered with no request to the host', async t => {
  t.after(forgetSiteServers);
  nameSiteServer(home());
  const runtime = fakeRuntime();
  let asked = 0;
  runtime.hostInfo = async () => { asked += 1; throw new Error('the host is not asked'); };
  const progress = [];
  const peer = await joinWorkspace(refusalOptions(runtime, {onProgress: event => progress.push(event)}));
  assert.equal(asked, 0, 'the page asks the host nothing over HTTPS');
  assert.equal(runtime.calls.entryRequest.hostCert, memberText(siteNamed(), 'hostCert'), 'the site\'s certificate reaches the entry byte for byte');
  assert.deepEqual(runtime.peer.config.address, JSON.parse(siteNamed()).address, 'the session dials the host where the site says');
  assert.equal(progress.at(-1).phase, 'admitted');
  await peer.dispose();
});

test('a page its site told of the host connects to it again without asking it', async t => {
  t.after(forgetSiteServers);
  nameSiteServer(home());
  const runtime = certifiedBySite(fakeRuntime());
  let asked = 0;
  runtime.hostInfo = async () => { asked += 1; throw new Error('the host is not asked'); };
  await connect(options(runtime, memoryStore()));
  assert.equal(asked, 0);
  assert.deepEqual(runtime.peer.config.address, JSON.parse(siteNamed()).address);
});

test('a site that names its host as it was before it was set up again stops the entry, and says why', async t => {
  t.after(forgetSiteServers);
  nameSiteServer(home(siteNamed(1)));
  const runtime = fakeRuntime();
  // The host the site named answers no more: its entry is never answered.
  runtime.calls.entry = {outcome: 'waiting'};
  let asked = 0;
  runtime.hostInfo = async () => { asked += 1; return siteNamed(2); };
  const said = [];
  const error = console.error;
  console.error = message => said.push(String(message));
  // What the page did not catch reaches the site's entry through `reportError`, where a runtime
  // has one: it is caught here, as the entry would, not left to fail the run.
  const reported = [];
  const report = Object.getOwnPropertyDescriptor(globalThis, 'reportError');
  globalThis.reportError = failure => reported.push(failure);
  t.after(() => {
    console.error = error;
    if (report) Object.defineProperty(globalThis, 'reportError', report);
    else delete globalThis.reportError;
  });
  const entering = joinWorkspace(refusalOptions(runtime));
  await until(() => runtime.peer, 'the session');
  runtime.peer.transport = 'reconnecting';
  runtime.peer.emit();
  await assert.rejects(entering, failed => failed.name === 'AdmissionError' && failed.code === 'stale-entry'
    && failed.message === STALE_SITE_SERVER);
  assert.equal(asked, 1, 'the host is asked who it is once, only once the session could not reach it');
  assert.ok(said.some(line => line.includes('TILO_WEB_HOME_HOST_INFO') && line.includes('web-host.json')), said.join('\n'));
  assert.ok(runtime.peer.disposed, 'the session is let go');
  assert.ok(reported.some(failure => failure.message.includes('TILO_WEB_HOME_HOST_INFO')),
    'the entry hears what to set');
});

test('a host that listens for pages elsewhere, or under a renewed pinned certificate, is not the one the site named', () => {
  const at = (origin, hash) => {
    const info = JSON.parse(siteNamed());
    info.address.web = {origin, certificate_sha256: hash};
    return info;
  };
  const named = at('https://host.example:8443', [1, 2]);
  assert.equal(staleSiteServer(named, at('https://host.example:8443', [1, 2])), false);
  assert.equal(staleSiteServer(named, at('https://host.example:9443', [1, 2])), true);
  assert.equal(staleSiteServer(named, at('https://host.example:8443', [3, 4])), true);
});

test('a session that cannot reach the host its site names as it is goes on trying', async t => {
  t.after(forgetSiteServers);
  nameSiteServer(home());
  const runtime = certifiedBySite(fakeRuntime());
  let asked = 0;
  runtime.hostInfo = async () => { asked += 1; return siteNamed(); };
  const peer = await connect(options(runtime, memoryStore()));
  const states = [];
  peer.onStatus(snapshot => states.push(snapshot.phase));
  runtime.peer.transport = 'reconnecting';
  runtime.peer.emit('admitting');
  await until(() => asked === 1, 'the host asked');
  await settle();
  runtime.peer.transport = 'connected';
  runtime.peer.emit('ready');
  assert.ok(!states.includes('failed'), states.join());
  assert.equal(peer.getSnapshot().phase, 'ready');
});

test('a host that refuses the connection is said at once, not after a timeout, and a session that gets through after is in', async () => {
  const runtime = fakeRuntime({phase: 'authenticating'});
  const peer = await connect(options(runtime, memoryStore()));
  const began = Date.now();
  const ready = peer.whenReady({timeoutMs: HANG_BOUND_MS, capMs: HANG_BOUND_MS});
  runtime.peer.transport = 'offline';
  Object.assign(runtime.peer, {error: 'websocket: connection refused', errorCode: 'transport-ended'});
  runtime.peer.emit('incomplete');
  const error = await ready.then(() => assert.fail('let in'), failed => failed);
  assert.ok(Date.now() - began < 1000, 'known in well under the wait');
  assert.equal(failureCause(error), 'unreachable', error.message);
  assert.match(error.message, /https:\/\/host\.example:8443/);
  assert.match(error.message, /websocket: connection refused/);
  assert.ok(hostUnreachable(error), 'nothing the host refused: a kept membership stays');
  // The session goes on trying; once it gets through, it is a session like any other.
  runtime.peer.transport = 'connected';
  delete runtime.peer.error;
  runtime.peer.emit('ready');
  assert.equal(peer.getSnapshot().phase, 'ready');
  runtime.peer.transport = 'offline';
  runtime.peer.emit('incomplete');
  assert.equal(peer.getSnapshot().phase, 'incomplete', 'a dropped connection of a session that was in is no unreachable host');
  await peer.dispose();
});

test('a session its server stops because this browser was removed says so at once, by its code, and never as a server that is away', async () => {
  const runtime = fakeRuntime({phase: 'authenticating'});
  const peer = await connect(options(runtime, memoryStore()));
  const began = Date.now();
  const ready = peer.whenReady({timeoutMs: HANG_BOUND_MS, capMs: HANG_BOUND_MS});
  // The server refused the session's first hello and said why (the handshake's own refusal):
  // the session stops there, failed, and was at no moment offline.
  runtime.peer.transport = 'failed';
  Object.assign(runtime.peer, {error: 'This device was removed from your devices.', errorCode: 'device-revoked'});
  runtime.peer.emit('failed');
  const error = await ready.then(() => assert.fail('let in'), failed => failed);
  assert.ok(Date.now() - began < 1000, 'known in well under the wait');
  assert.equal(error.code, 'device-revoked');
  assert.ok(deviceRemoved(error), 'the page tells the person, and waits for nothing');
  assert.notEqual(failureCause(error), 'unreachable', error.message);
  assert.deepEqual([peer.getSnapshot().phase, peer.getSnapshot().errorCode], ['failed', 'device-revoked'], 'the session keeps its own code');
  await peer.dispose();
});

test('an entry the page cannot reach is said at once, after its site\'s host is asked who it is', async t => {
  t.after(forgetSiteServers);
  nameSiteServer(home());
  const runtime = fakeRuntime();
  runtime.calls.entry = {outcome: 'waiting'};
  let asked = 0;
  runtime.hostInfo = async () => { asked += 1; throw new TypeError('Failed to fetch'); };
  const began = Date.now();
  const entering = joinWorkspace(refusalOptions(runtime));
  await until(() => runtime.peer, 'the session');
  runtime.peer.transport = 'offline';
  runtime.peer.emit('incomplete');
  await assert.rejects(entering, failed => failed.code === 'unanswered' || failed.code === 'unavailable');
  assert.ok(Date.now() - began < 1000, 'known in well under the wait');
  assert.equal(asked, 1);
  assert.ok(runtime.peer.disposed, 'the session is let go');
});

test('a host that answers the page that it does not let it in is said at once', async () => {
  const runtime = fakeRuntime({phase: 'admitting'});
  const peer = await connect(options(runtime, memoryStore()));
  runtime.peer.transport = 'connected';
  const ready = peer.whenReady({timeoutMs: HANG_BOUND_MS, capMs: HANG_BOUND_MS});
  await settle();
  Object.assign(runtime.peer, {error: 'device not in workspace membership', errorCode: 'permission-required'});
  runtime.peer.emit('admitting');
  const error = await ready.then(() => assert.fail('let in'), failed => failed);
  assert.equal(error.code, 'not-let-in');
  assert.equal(failureCause(error), 'host-refused');
  assert.equal(error.sessionError, 'device not in workspace membership');
  await peer.dispose();
});

test('a site naming its host in a form the page cannot read stops the page', () => {
  for (const info of ['{', '{"hostCert":{}}', '{"address":{"endpointAddr":{}}}']) {
    assert.throws(() => nameSiteServer(home(info)), /TILO_WEB_HOME_HOST_INFO/);
  }
  forgetSiteServers();
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

test('a page holding the root approves the computer a ticket brought, through its peer', async () => {
  const runtime = fakeRuntime();
  const identity = {kind: 'webcrypto-ed25519', publicKey: new Uint8Array(32).fill(0xbb), userPublicKey: 'bb'.repeat(32), sign: async () => new Uint8Array(64)};
  const peer = await connect(options(runtime, memoryStore(), {userPublicKey: undefined, identity}));
  assert.equal(typeof peer.approveAsked, 'function', 'the page peer offers the host’s question to the root this tab holds');
  const approved = JSON.parse(await peer.approveAsked(identity, JSON.stringify({user_public_key: identity.userPublicKey})));
  assert.deepEqual(approved, {root: identity.userPublicKey, asked: {user_public_key: identity.userPublicKey}});
  await peer.dispose();
});

test('a member page joins its workspace as one device of it, and leaves it', async () => {
  const runtime = fakeRuntime();
  const peer = await connect(options(runtime, memoryStore()));
  await until(() => peer.live.state === 'live', 'the live layer');
  assert.deepEqual(runtime.peer.said.map(word => word.workspace), [WORKSPACE], 'its word of itself once live');
  assert.equal(runtime.peer.said[0].value.member.kind, 'person', 'with who it is');
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

test('a page that certifies its device again keeps its peer id, which its mounted desktops name it by', async () => {
  const runtime = fakeRuntime();
  const peer = await connect(options(runtime, memoryStore()));
  const id = peer.runtimeId;
  assert.equal(typeof id, 'number', 'the page has an id its mounts name it by');
  runtime.peer.emit('failed');
  await peer.recover({kind: 'reauthenticate'});
  assert.equal(runtime.calls.started.length, 2, 'a session replaced the one that was turned away');
  assert.equal(runtime.calls.started[0].disposed, true);
  assert.equal(runtime.calls.started[1].id, id, 'the replacement session opens as the same page');
  assert.equal(peer.runtimeId, id, 'so a desktop mounted before keeps reaching the workspace through it');
  await peer.dispose();
});

test('a page an invitation let in certifies its device again with its own root and has a working session', async () => {
  const runtime = fakeRuntime();
  const identity = person();
  const peer = await joinWorkspace({
    origin: 'https://acme.example:8443', invitation: Uint8Array.from([1, 2, 3]), identity, admissionToken: 'token',
    runtime, deviceKey, store: memoryStore(), storage: null,
    fetch: async () => { throw new Error('no network in tests'); },
  });
  await peer.whenReady({timeoutMs: HANG_BOUND_MS});
  const id = peer.runtimeId;
  const first = runtime.peer;
  first.phase = 'failed';
  first.error = 'the certificate it showed was refused';
  first.errorCode = 'certificate-rejected';
  first.emit();
  const seen = [];
  const stop = peer.onStatus(snapshot => seen.push(snapshot.phase));
  await peer.recover({kind: 'reauthenticate'});
  stop();
  // Certified by the person's own root, not through any approver, though the server is not theirs.
  assert.equal(runtime.calls.certify.length, 1);
  assert.equal(runtime.calls.certify[0].root, identity, 'the root the page holds certifies its device');
  assert.equal(first.disposed, true);
  const second = runtime.peer;
  assert.notEqual(second, first, 'a new session replaced the one turned away');
  assert.equal(second.id, id, 'under the page id its windows know');
  assert.equal(second.config.entering, undefined, 'a member already: no invitation is spent again');
  assert.deepEqual(second.config.cert, {device: 1, expires_at_ns: second.config.cert.expires_at_ns});
  assert.equal(seen[0], 'authenticating', 'between the two sessions the page holds none, and never says ready');
  second.emit('ready');
  assert.equal((await peer.whenReady({timeoutMs: HANG_BOUND_MS})).phase, 'ready');
  await peer.dispose();
});

test('a re-certification that fails leaves the session it was to replace, and a session that cannot start says the page is not connected', async () => {
  const runtime = fakeRuntime();
  const peer = await connect(options(runtime, memoryStore(), {identity: person(), userPublicKey: undefined}));
  const first = runtime.peer;
  const certify = runtime.certifyWithRoot;
  runtime.certifyWithRoot = async () => { throw new Error('the root did not sign'); };
  await assert.rejects(peer.recover({kind: 'reauthenticate'}), /did not sign/);
  assert.equal(first.disposed, false, 'the session stands while no new certificate is at hand');
  assert.equal(peer.getSnapshot().phase, 'ready');

  // Certified, but the session under the new certificate cannot start: the page held none
  // from the moment it let the old one go, and says it is not connected, never ready.
  runtime.certifyWithRoot = certify;
  const start = runtime.WasmPeer.start;
  runtime.WasmPeer.start = () => { throw new Error('no session for you'); };
  const seen = [];
  const stop = peer.onStatus(snapshot => seen.push(snapshot));
  await assert.rejects(peer.recover({kind: 'reauthenticate'}), /no session for you/);
  stop();
  assert.equal(first.disposed, true);
  assert.deepEqual(seen.map(snapshot => snapshot.phase), ['authenticating', 'failed']);
  const said = peer.getSnapshot();
  assert.equal(said.phase, 'failed');
  assert.equal(said.errorCode, 'not-connected');
  assert.equal(said.error, 'This browser isn’t connected to the workspace. Reload the page to connect again.');
  assert.match(said.cause, /no session for you/);

  // Trying again later opens the page under the id it had.
  runtime.WasmPeer.start = start;
  await peer.recover({kind: 'reauthenticate'});
  assert.equal(runtime.peer.id, first.id);
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
  const common = {origin: 'https://host.example:8443', identity, runtime, deviceKey, desktop, namespaces, profile,
    fetch: async () => { throw new Error('no network in tests'); }};

  // Reconnecting with the kept state is refused, and nothing moves.
  await assert.rejects(connect({...common, workspace: WORKSPACE}), {name: 'AdmissionError', code: 'host-changed', message: HOST_CHANGED});
  assert.equal(runtime.calls.started.length, 0, 'no session started');
  assert.equal(runtime.calls.certify.length, 0, 'nothing was certified by the new host');
  assert.equal(profileDevice(profile, namespaces), PROFILE_DEVICE, 'the profile stays where it was');

  // The new host admits the person (its entry's invitation and token).
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

test('a stale saved peer entry without a matching certificate moves aside when a reset server admits the device', async () => {
  const runtime = fakeRuntime();
  const {values, desktop, namespaces} = profileFakes(runtime);
  const profile = 'tilo-identity:ee@server.example:8443';
  const userPublicKey = 'ee'.repeat(32);
  const certOf = (user, dev) => ({user_public_key: Array(32).fill(user), device_public_key: Array(32).fill(dev)});
  const oldServer = certOf(1, 2);
  const newServer = certOf(1, 3);
  const oldDevice = certOf(0xee, 0x11);
  const newDevice = certOf(0xee, 0x22);
  const serverKey = 'host' + 'Cert';
  const wrap = (cert, server) => JSON.stringify({cert, [serverKey]: server});
  // Namespace holds a saved peer entry pinned to the old server, while
  // the certificate entry is absent (or already overwritten by an earlier try).
  const savedByDevice = new Map([
    [`${profile}/${PROFILE_DEVICE}/${userPublicKey}`, wrap(oldDevice, oldServer)],
  ]);
  runtime.savedPeerCertificate = (p, d, u) => savedByDevice.get(`${p}/${d}/${u}`);
  runtime['host' + 'Info'] = async () => JSON.stringify({address: HOST_ADDRESS, [serverKey]: newServer});
  const common = {origin: 'https://server.example:8443', userPublicKey, runtime, deviceKey, desktop, namespaces, profile,
    fetch: async () => { throw new Error('no network in tests'); }};

  // Reconnecting without admission refuses instead of starting against the stale entry.
  await assert.rejects(connect({...common, workspace: WORKSPACE}), {name: 'AdmissionError', message: HOST_CHANGED});
  assert.equal(runtime.calls.started.length, 0);

  // Pairing / admission under the new server adopts a fresh namespace.
  const admitted = wrap(newDevice, newServer);
  const peer = await connect({...common, workspace: WORKSPACE, deviceCertificate: admitted});
  const fresh = `${PROFILE_DEVICE}:${'03'.repeat(32)}`;
  assert.equal(runtime.peer.namespace, JSON.stringify({profile, device: fresh}));
  assert.equal(profileDevice(profile, namespaces), fresh);
  await peer.dispose();

  // Re-admitting on the same server under a different device key also moves to a fresh namespace.
  const thirdDevice = certOf(0xee, 0x33);
  savedByDevice.set(`${profile}/${fresh}/${userPublicKey}`, admitted);
  const peer2 = await connect({...common, workspace: WORKSPACE, deviceCertificate: wrap(thirdDevice, newServer)});
  const byDevice = `${PROFILE_DEVICE}:${'03'.repeat(32)}:${'33'.repeat(32)}`;
  assert.equal(runtime.peer.namespace, JSON.stringify({profile, device: byDevice}));
  assert.equal(profileDevice(profile, namespaces), byDevice);
  assert.equal(JSON.parse(textOf(values.get(`${profile}/${byDevice}/certificate`))).cert.device_public_key[0], 0x33);
  await peer2.dispose();
});

test('a device the workspace let go asks to join again under the certificate it already holds from that host', async () => {
  // The workspace keeps the certificate of every device it admitted, also once
  // its person was removed or pruned after an absence, and refuses the same
  // device under another one. Whether the kept one still serves is the shared
  // entry's to say (tilo-session-client's `entering_certificate`); the page
  // hands it over exactly as kept.
  const runtime = fakeRuntime();
  const {values, handles, desktop, namespaces} = profileFakes(runtime);
  const profile = 'tilo-identity:ee@host.example:8443';
  const identity = person();
  const host = hostCertOf(1, 2);
  const certificate = {user_public_key: Array(32).fill(0xee), device_public_key: Array.from(deviceKey.publicKey), issued_at_ns: 5};
  // Nanoseconds past what a JavaScript number holds exactly: only the kept
  // text itself still matches the certificate's signature.
  const kept = JSON.stringify({cert: certificate, hostCert: host}).replace('"issued_at_ns":5', '"issued_at_ns":1792040183583123457');
  values.set(`${profile}/${PROFILE_DEVICE}/certificate`, new TextEncoder().encode(kept));
  runtime.hostInfo = async () => JSON.stringify({address: HOST_ADDRESS, hostCert: host});
  const common = {origin: 'https://host.example:8443', identity, runtime, deviceKey, desktop, namespaces, profile,
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
    admissionToken: 'one-time-token', runtime, deviceKey, store, storage: null,
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
  const peer = await connect(options(runtime, memoryStore(), {deviceKey: sealed}));
  assert.equal(runtime.calls.certify[0].deviceKey.seed, seed);
  assert.equal(runtime.peer.signer.seed, seed);
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

test('storage failure codes follow the store errors', () => {
  assert.equal(storageFailureCode('QuotaExceededError: full'), 'storage-quota');
  assert.equal(storageFailureCode('SecurityError: denied'), 'storage-denied');
  assert.equal(storageFailureCode('AbortError'), 'storage-transaction-failed');
});

test('retry-saving commits the store again and clears the storage failure', async () => {
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

  storageListener({persistence: 'failed', pendingWrites: 2, error: 'QuotaExceededError: the quota has been exceeded'});
  storageListener({persistence: 'failed', pendingWrites: 2, error: 'AbortError: commit aborted'});
  assert.deepEqual(runtime.peer.failures.map(f => [f.surface, f.code]), [[undefined, 'storage-transaction-failed']], 'one storage failure, the newest');

  await assert.rejects(peer.recover({kind: 'retry-saving'}), /commit aborted/);
  assert.equal(runtime.peer.failures.length, 1);
  flushFails = false;
  assert.equal((await peer.recover({kind: 'retry-saving'})).operationId, 'retry-saving');
  assert.equal(storage.flushed, 2);
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
  assert.equal(peer.member.color, runtime.presenceColor('bb'.repeat(32)));
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

test('whenReady keeps the code the host stopped the session with, for the page to say why', async () => {
  const runtime = fakeRuntime({phase: 'admitting'});
  const peer = await connect(options(runtime, memoryStore()));
  const ready = peer.whenReady();
  Object.assign(runtime.peer, {error: 'This workspace was deleted by its owner.', errorCode: 'workspace-deleted'});
  runtime.peer.emit('failed');
  await assert.rejects(ready, {message: 'This workspace was deleted by its owner.', code: 'workspace-deleted',
    sessionError: 'This workspace was deleted by its owner.', sessionErrorCode: 'workspace-deleted'});
  await peer.dispose();
});

test('a host moving the page on is waited for; one that says nothing is not', async () => {
  const runtime = fakeRuntime({phase: 'authenticating'});
  const peer = await connect(options(runtime, memoryStore()));
  // Each step the session takes gives the host the wait again, within the cap.
  const admitting = peer.whenReady({timeoutMs: 80, capMs: 5000});
  await new Promise(resolve => setTimeout(resolve, 60));
  runtime.peer.emit('admitting');
  await new Promise(resolve => setTimeout(resolve, 60));
  runtime.peer.emit('ready');
  assert.equal((await admitting).phase, 'ready');
  await peer.dispose();

  const quiet = fakeRuntime({phase: 'admitting'});
  const silent = await connect(options(quiet, memoryStore()));
  // A host the session never got through to did not answer: the page asks it
  // again (`hostUnreachable`), as it does a refused connection.
  await assert.rejects(silent.whenReady({timeoutMs: 80, capMs: 5000}), {name: 'TimeoutError', code: 'unanswered', connected: false});
  // A host that answered and never let the page in is not one that is away.
  quiet.peer.transport = 'connected';
  quiet.peer.emit('admitting');
  await assert.rejects(silent.whenReady({timeoutMs: 80, capMs: 5000}), {name: 'TimeoutError', code: 'timed-out', connected: true});
  await silent.dispose();
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

test('an invitation the host made for itself names what it makes of whoever accepts it, and nobody who sent it', async () => {
  const offered = async ({administrator, owner}) => {
    const runtime = fakeRuntime();
    runtime.inspectInvitation = async () => JSON.stringify({kind: 'invitation', workspace: WORKSPACE, workspaceId: '7',
      name: 'Lounge', role: 'Editor', expiresAtNs: '9', members: 0, administrator, owner});
    return inspectInvitation('https://host.example:8443', 'AQID', {runtime});
  };
  const owner = await offered({administrator: true, owner: true});
  assert.equal(owner.role, 'owner');
  assert.equal(owner.inviter, undefined, 'the card says "You’re invited to", from no person');
  assert.equal(owner.members, 0, 'the host is none of the people it counts');
  assert.equal((await offered({administrator: true, owner: false})).role, 'administrator');
  assert.equal((await offered({administrator: false, owner: false})).role, 'editor');
});

test('an invitation is accepted as the root-certified device, through the shared entry', async () => {
  const runtime = fakeRuntime();
  const store = memoryStore();
  const identity = {kind: 'webcrypto-ed25519', publicKey: new Uint8Array(32).fill(0xee), userPublicKey: 'ee'.repeat(32), sign: async () => new Uint8Array(64)};
  const offer = await inspectInvitation('https://host.example:8443', 'AQID', {runtime});
  assert.deepEqual(runtime.calls.inspected, [1, 2, 3]);
  // What the card says comes from here: the workspace, how many people are in
  // it, the part offered and who offers it.
  assert.deepEqual(offer, {workspaceName: 'Acme', role: 'editor', workspace: WORKSPACE, workspaceId: '7', expiresAtNs: '9',
    members: 4, inviter: {name: 'Ada', userPublicKey: 'aa'.repeat(32)}, waitList: false});

  const progress = [];
  const peer = await joinWorkspace({
    origin: 'https://host.example:8443', invitation: Uint8Array.from([1, 2, 3]), identity, displayName: 'Bob',
    runtime, deviceKey, store, storage: null, onProgress: event => progress.push(event.phase),
    fetch: async () => { throw new Error('no network in tests'); },
  });
  assert.deepEqual(progress, ['asking', 'admitted']);
  assert.deepEqual(runtime.calls.accepted.invitation, [1, 2, 3]);
  assert.deepEqual(runtime.peer.config.entering.through.Invitation.invitation, [1, 2, 3],
    'the session carries the invitation in its first message');
  assert.equal(runtime.calls.accepted.root, identity);
  assert.equal(runtime.calls.accepted.name, 'Bob');
  assert.equal(runtime.calls.accepted.held, null, 'a store with no certificate holds none to enter with');
  assert.equal(runtime.calls.certify.length, 0, 'no certification at the host');
  assert.deepEqual(runtime.peer.config.cert, {guest: true, device_public_key: Array(32).fill(0xdd)});
  assert.equal(runtime.peer.workspace, WORKSPACE);
  await peer.dispose();
});
test('a device key opened for a peer is let go when it is disposed, aborted or fails to connect', async () => {
  const deviceKey = () => ({closed: 0, close() { this.closed += 1; }});
  const disposed = deviceKey();
  let inner = 0;
  const peer = await withDeviceKey(disposed, async () => ({dispose: async () => { inner += 1; }}));
  assert.equal(disposed.closed, 0);
  await peer.dispose();
  assert.equal(inner, 1);
  assert.equal(disposed.closed, 1);

  const failed = deviceKey();
  await assert.rejects(withDeviceKey(failed, async () => { throw new Error('host unreachable'); }), /host unreachable/);
  assert.equal(failed.closed, 1);

  const aborted = deviceKey();
  const controller = new AbortController();
  await withDeviceKey(aborted, async () => ({dispose: async () => {}}), controller.signal);
  controller.abort();
  assert.equal(aborted.closed, 1);
});

test('a pre-admitted person joins without waiting for the owner', async () => {
  const runtime = fakeRuntime();
  const identity = {kind: 'webcrypto-ed25519', publicKey: new Uint8Array(32).fill(0xee), userPublicKey: 'ee'.repeat(32), sign: async () => new Uint8Array(64)};
  const progress = [];
  const peer = await joinWorkspace({
    origin: 'https://host.example:8443', invitation: Uint8Array.from([1, 2, 3]), identity, displayName: 'Cy',
    admissionToken: 'one-time-token', runtime, deviceKey, store: memoryStore(), storage: null,
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
  refusing.calls.entry = {outcome: 'refused', code: 'not-admitted', message: 'this invitation is bound to another person'};
  await assert.rejects(joinWorkspace({origin: 'https://host.example:8443', invitation: Uint8Array.from([1, 2, 3]), identity,
    admissionToken: 'another-persons-token', runtime: refusing, deviceKey, store: memoryStore(), storage: null,
    fetch: async () => { throw new Error('no network in tests'); }}),
  {name: 'AdmissionError', code: 'refused', message: REFUSED_JOIN});
});

test('a person renamed since the page started says the new name with the next frame', async () => {
  const runtime = fakeRuntime();
  const peer = await connect(options(runtime, memoryStore()));
  await until(() => peer.live.state === 'live', 'the live layer');
  const labels = () => runtime.peer.published.map(value => value.member?.label).filter(label => typeof label === 'string');
  await until(() => labels().length, 'the first announcement');
  assert.deepEqual(labels().at(-1), '', 'a person who gave no name goes by none, never their device');
  peer.setDisplayName('Bea');
  peer.live.publishPresence({selection: {anchor: '03', head: '03'}});
  await until(() => labels().at(-1) === 'Bea', 'the name the person chose since');
  await peer.dispose();
});

test('a name shown here stays the page\'s; only a name the person gives reaches their server', async () => {
  const runtime = fakeRuntime();
  const named = [];
  const peer = await connect(options(runtime, memoryStore()));
  runtime.peer.setDisplayName = name => named.push(['shown', name]);
  runtime.peer.nameSelf = name => named.push(['given', name]);
  await until(() => peer.live.state === 'live', 'the live layer');
  peer.setDisplayName('Ada');
  assert.deepEqual(named, [['shown', 'Ada']], 'a name shown here is never given to the workspace');
  peer.nameSelf('  ');
  peer.nameSelf('Quinten');
  assert.deepEqual(named.at(-1), ['given', 'Quinten']);
  assert.equal(named.filter(([kind]) => kind === 'given').length, 1, 'a blank name is no name');
  await peer.dispose();
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


for (const attachSignal of [false, true]) test(`leaving a page joins peer storage release (signal=${attachSignal})`, async () => {
  const runtime = fakeRuntime();
  const values = new Map();
  runtime.profile_value = (profile, device, key) => values.get(`${profile}/${device}/${key}`);
  runtime.set_profile_value = (profile, device, key, bytes) => values.set(`${profile}/${device}/${key}`, bytes);
  let release, begun, reentered, releases = 0;
  const hold = new Promise(resolve => {release = resolve;});
  const started = new Promise(resolve => {begun = resolve;});
  const controller = new AbortController();
  const desktop = {
    async openProfileStorage() {
      return {
        async flush() {},
        async release() { releases++; begun(); reentered = peer.dispose(); await hold; },
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

test('the label the host gives a device is never its person\'s name', async () => {
  const runtime = fakeRuntime();
  runtime.certifyDevice = async () => `{"cert":${LINUX_CERT},"hostCert":{"issued_at_ns":${NANOS}},"label":"Browser on Cloud 2"}`;
  const peer = await connect(options(runtime, memoryStore()));
  assert.equal(peer.member.label, '', 'a person who gave no name goes by none');
  await peer.dispose();
});

test('the feed of changes leaves the page\'s ask as names with nothing a change says, and the wait for a write ends by itself', async () => {
  const runtime = certifiedBySite(fakeRuntime());
  runtime.hostInfo = async () => siteNamed();
  const peer = await connect(options(runtime, memoryStore(), {voice: false, live: false}));
  const sent = [];
  const answers = [];
  runtime.peer.actionMessage = async (workspace, person, message) => {
    sent.push({workspace, person, asked: JSON.parse(message)});
    return JSON.stringify(answers.shift());
  };
  // With no cursor nothing that happened yet is news: the answer is where to read on from.
  answers.push({ChangesResponse: {correlation: 1, result: {Ok: {changes: [], cursor: 'c-1'}}}});
  assert.deepEqual(await peer.actions.changes(), {changed: [], cursor: 'c-1'});
  assert.equal(sent[0].workspace, WORKSPACE);
  assert.equal(sent[0].person, 'bb'.repeat(32));
  assert.deepEqual(Object.keys(sent[0].asked), ['ChangesRequest']);
  assert.equal('after' in sent[0].asked.ChangesRequest, false);
  // The server tells this person each change with what it says. Only the names go on, a document once.
  const change = (app, of, sentence) => ({app, record: of,
    entry: {change: 'ab12', sentence, time_ms: 1, actor: 'person', by: 'Ada', title: 'The quiet plan', invocation: 'inv-9'}});
  answers.push({ChangesResponse: {correlation: 2, result: {Ok: {cursor: 'c-2', changes: [
    change('so.tilo.tasks', 'part-1', 'Ada renamed the task to "Buy the company"'),
    change('so.tilo.tasks', 'part-1', 'Ada closed the task'),
    change('so.tilo.notes', '', 'Bo wrote a paragraph'),
  ]}}}});
  const told = await peer.actions.changes('c-1');
  assert.deepEqual(told, {changed: [{app: 'so.tilo.tasks', document: 'part-1'}, {app: 'so.tilo.notes', document: ''}], cursor: 'c-2'});
  assert.equal(sent[1].asked.ChangesRequest.after, 'c-1');
  for (const kept of ['Buy the company', 'closed', 'paragraph', 'Ada', 'quiet', 'inv-9', 'ab12']) {
    assert.equal(JSON.stringify(told).includes(kept), false, `nothing of ${kept}`);
  }
  // A server that says nothing is said so.
  answers.push({});
  await assert.rejects(peer.actions.changes('c-2'), {code: 'unavailable'});

  // The wait for the server's word of a write ends by itself, never sooner, and is let go of when released.
  const asked = [];
  // A server that never answers: the wait ends at its own time.
  runtime.peer.writeLanded = workspace => { asked.push(workspace); return new Promise(() => {}); };
  let began = Date.now();
  assert.equal(await peer.actions.landed(60).woke, false);
  assert.ok(Date.now() - began >= 50, `held ${Date.now() - began} ms`);
  assert.deepEqual(asked, [WORKSPACE]);
  // A server that cannot tell, and a session that says early that nothing landed: the wait costs
  // its whole time all the same, so nobody asks in a circle.
  for (const answer of [async () => { throw new Error('this provider cannot tell'); }, async () => false]) {
    runtime.peer.writeLanded = answer;
    began = Date.now();
    assert.equal(await peer.actions.landed(60).woke, false);
    assert.ok(Date.now() - began >= 50, `held ${Date.now() - began} ms`);
  }
  // A write landed: said at once.
  runtime.peer.writeLanded = async () => true;
  const heard = peer.actions.landed(60_000);
  assert.equal(await heard.woke, true);
  heard.release();
  // Released: the wait is over now, and the session is told so, which lets go of its own.
  let over = false;
  runtime.peer.writeLanded = (workspace, until) => until.then(() => { over = true; return false; });
  const held = peer.actions.landed(60_000);
  await Promise.resolve();
  held.release();
  assert.equal(await held.woke, false);
  assert.equal(over, true);
});

test('a paired device entering a workspace derives the person key from its certificate bytes', async () => {
  const runtime = fakeRuntime();
  const keyBytes = [0x00, 0x0f, 0xab, ...Array(29).fill(0xcd)];
  const expectedKey = `000fab${'cd'.repeat(29)}`;
  runtime.certifyDevice = async () => JSON.stringify({
    cert: {user_public_key: keyBytes, device_public_key: Array(32).fill(0xdd)},
    hostCert: {device_public_key: Array(32).fill(0x11)},
  });
  const peer = await pairDevice({
    origin: 'https://host.example:8443',
    userPublicKey: expectedKey,
    entry: `https://host.example:8443/peer/v1/directory/${WORKSPACE}/`,
    runtime,
    deviceKey,
    store: memoryStore(),
    storage: null,
    voice: false,
    live: false,
    fetch: async () => { throw new Error('no network in tests'); },
  });
  opened.add(peer);
  assert.equal(peer.userPublicKey, expectedKey);
  assert.equal(runtime.peer.workspace, WORKSPACE);
});
