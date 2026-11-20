import test from 'node:test';
import assert from 'node:assert/strict';
import {readFile} from 'node:fs/promises';
import {chooseWorkspace, chosenWorkspace, enteredWorkspaces, joinViaHost, keptMembership} from './workspaces.mjs';
import {AdmissionError, admit} from './admission.mjs';
import {assistantAddress, PAIRING_CLOCK, PAIRING_DECLINED, PAIRING_SAME_PERSON, parsePairSecret} from './pairing.mjs';
import {runtimeFake} from './runtime-surface.mjs';

const USER = 'aa'.repeat(32);
/** The short link the page's site keeps an invitation under. */
const SEALED = 'https://web.example/i/AbCdEfGhIjKlMnOp#s3cr3tKeyS3cr3tKeyS3cr3tKeyS3cr3tKeyS3cr';
const ORIGIN = 'https://node.example';
const tick = async () => { for (let i = 0; i < 30; i++) await Promise.resolve(); };

class Element {
  constructor(tag) { this.tag = tag; this.children = []; this.listeners = new Map(); this.dataset = {}; this.style = {}; this.classList = {add() {}, remove() {}}; }
  append(...nodes) { this.children.push(...nodes); }
  prepend(...nodes) { this.children.unshift(...nodes); }
  replaceChildren(...nodes) { this.children = nodes; }
  setAttribute() {}
  addEventListener(type, listener) { this.listeners.set(type, listener); }
  focus() {}
  select() {}
  // The one part a form or a section is asked for by a selector: its button, its foot.
  querySelector(selector) {
    this.parts ??= new Map();
    if (!this.parts.has(selector)) this.parts.set(selector, new Element(selector));
    return this.parts.get(selector);
  }
  remove() { document.body.children = document.body.children.filter(node => node !== this); }
}

/** The parts of the entry a page turned away fills in (index.html,
 * `#arrival-refusal`): its words, and the field a link is pasted into. */
const ENTRY_PARTS = ['arrival-refusal', 'arrival-refusal-heading', 'arrival-refusal-body', 'arrival-invite', 'arrival-invite-toggle',
  'arrival-invite-link', 'arrival-invite-error'];

// Run the real page wiring and workspace actions. Replace only the booted
// WASM/GPU peer, arrival animation, and root device key with fixtures. `pair`
// stands for the runtime certifying this tab's device key through the server
// (`pairDevice`, peer.mjs): what the server answers a new device. `identity` is
// the person this tab has open, and `kept` the one this browser keeps
// whether or not a tab opened them (`keptIdentity`). `join` stands for
// joining through a server's entry (`joinViaHost`) where a test says what the
// server answers; without one the page's own join runs. `entryParts` gives the page
// the entry's own parts (`ENTRY_PARTS`), as index.html has them. `profile` is what
// the server keeps of the person a pairing link names (`{display_name}`, or
// `null` for none), and `whose` what the person presses on the card that says
// whose device this becomes: `'go'`, `'stop'`, or a function told the card
// that answers which.
async function fixture({boot, remembered = [], own = false, search = '', session = {}, local = {}, member = null, entry = null, scope = null,
  pair = async () => { throw new Error('this fixture pairs no device'); }, identity = {userPublicKey: USER}, kept = null, join = null, entryParts = false,
  profile = null, whose = 'go', runtime: answers = {}} = {}) {
  const values = new Map(Object.entries(local));
  let assigned = null;
  const stored = new Map(Object.entries(session));
  const listeners = [];
  globalThis.localStorage = {getItem: key => values.get(key) ?? null, setItem: (key, value) => values.set(key, value), removeItem: key => values.delete(key)};
  globalThis.sessionStorage = {getItem: key => stored.get(key) ?? null, setItem: (key, value) => stored.set(key, value), removeItem: key => stored.delete(key)};
  globalThis.addEventListener = (type, listener) => listeners.push({type, listener});
  globalThis.removeEventListener = (type, listener) => {
    const index = listeners.findIndex(held => held.type === type && held.listener === listener);
    if (index >= 0) listeners.splice(index, 1);
  };
  globalThis.location = {href: 'https://web.example/#/workspace', search, pathname: '/', reload: () => calls.push('reload'),
    assign: url => { calls.push('assign'); assigned = url; }};
  globalThis.history = {pushState: (_state, _title, url) => { calls.push('history'); assigned = url ?? assigned; },
    replaceState: (_state, _title, url) => { calls.push('replace'); assigned = url ?? assigned; }};
  globalThis.document = {body: new Element('body'), documentElement: {dataset: {}, toggleAttribute() {}, removeAttribute() {}}, createElement: tag => new Element(tag),
    // A card is found by its kind, as the page looks for the one a new card replaces.
    querySelector: selector => document.body.children.find(card => selector === `[data-card="${card.dataset.card}"]`) ?? null,
    getElementById: id => id === 'status' ? status : id === 'surface' ? new Element('canvas')
      : id.startsWith('arrival-failure') || (entryParts && ENTRY_PARTS.includes(id)) ? (screen[id] ??= new Element(id)) : null};
  const appendToBody = document.body.append.bind(document.body);
  document.body.append = (...nodes) => {
    appendToBody(...nodes);
    for (const card of nodes.filter(node => node.dataset?.card === 'pair-whose')) {
      queueMicrotask(() => {
        const pressed = typeof whose === 'function' ? whose(card) : whose;
        const buttons = card.children.find(part => part.className === 'tilo-entry-actions').children;
        buttons.find(button => button.textContent === (pressed === 'stop' ? 'Stop' : 'Add this device')).listeners.get('click')();
      });
    }
  };
  // The failure screen's parts (index.html, `#arrival-failure`), by id.
  const screen = {};
  const status = {textContent: 'Loading…'};
  const calls = [];
  const shown = [];
  let diagnostics;
  const peer = {origin: ORIGIN, workspace: 'current', userPublicKey: USER,
    host: {hostCert: {user_public_key: Array(32).fill(170), device_public_key: Array(32).fill(0xcd)}},
    createInvitation: async () => { calls.push('invite'); return 'signed-secret'; },
    // A member, not the owner, of a server that keeps its wait list to whoever
    // it lets invite.
    membership: {list: async () => ({name: 'Current', members: []}), mayInvite: async () => false, waitList: async () => { throw new Error('only someone the workspace lets invite lists the wait list'); }},
    // A machine the person set up places the workspace under the id the page
    // chose, this device admitted to it in the same call.
    workspaces: {place: async ({name}) => ({workspace: 'created', name})},
    // The person's workspaces as the host lists them; a fixture says what it
    // answers, and nothing by default.
    personalWorkspaces: async () => [],
    dispose: async () => calls.push('dispose')};
  const page = {peer: own ? null : {...peer, ...member}, entry: entry ?? {base: 'https://web.example/'},
    mount: {setHostActions: actions => shown.push(actions), setOwnPerson: person => calls.push(['own', person]), flush: async () => calls.push('flush'), cleanup: async () => calls.push('cleanup')},
    router: {route: {target: {scope: own ? {kind: 'own'} : scope ?? {kind: 'workspace', host: ORIGIN, workspace: 'current'}}}, stop: () => calls.push('stop')},
    link: {format: route => `https://web.example/#/${route.target.scope.workspace}`}};
  const prepared = [];
  const runtime = runtimeFake({
    // What a test has the runtime answer beside the fixture's own, held to the module's surface as they are.
    ...answers,
    presenceColor: id => {
      calls.push(['colour', id]);
      return '#ffd166';
    },
    sealedInvitationLink: async (base, host, invitation) => {
      calls.push(['sealed', base, host, invitation]);
      return SEALED;
    },
    // The rules of the list are the runtime's (tilo-session-client's `entered`,
    // tested there); these keep only what the page's steps here rely on.
    forgetGoneWorkspaces: (list, origin, serverDevice) => {
      const all = JSON.parse(list);
      const gone = entry => entry.origin === origin && entry.hostDevice !== serverDevice;
      return JSON.stringify({entered: all.filter(entry => !gone(entry)), unchoose: all.filter(gone)});
    },
    rememberEntered: (list, entry) => {
      const {origin, workspace, hostDevice} = JSON.parse(entry);
      const {entered, unchoose} = JSON.parse(runtime.forgetGoneWorkspaces(list, origin, hostDevice));
      const same = kept => kept.origin === origin && kept.workspace === workspace;
      const name = JSON.parse(entry).name ?? entered.find(same)?.name;
      return JSON.stringify({entered: [{origin, workspace, hostDevice, ...(name ? {name} : {})}, ...entered.filter(kept => !same(kept))], unchoose});
    },
    nameEntered: (list, place, name) => {
      const {origin, workspace} = JSON.parse(place);
      return JSON.stringify({entered: JSON.parse(list).map(entry => entry.origin === origin && entry.workspace === workspace && !entry.name ? {...entry, name} : entry), unchoose: []});
    },
  });
  globalThis.__entryPageFixture = {page, runtime, mounted: [], prepared, prefetched: [], identity, kept, prepareGpu: () => prepared.push(document.documentElement.dataset.arrival),
    joinViaHost: join ?? joinViaHost, pair,
    findProfile: async person => (profile ? {version: 1, user_public_key: person, ...profile} : null),
    boot: async options => {
      diagnostics = options.diagnostics;
      // The page loads its runtime as boot starts; the fixture's is the stub above.
      await options.deps.loadRuntime();
      return boot ? boot(options, page) : page;
    }};
  // A workspace an earlier visit entered, and those it kept as they were
  // stored, whichever host held them; `null` leaves the store as `local` has
  // it, as a browser that lost what it remembered has none.
  if (remembered !== null) {
    localStorage.setItem('tilo-entered-workspaces', JSON.stringify([
      {origin: ORIGIN, workspace: 'other', hostDevice: 'cd'.repeat(32), name: 'Other'}, ...remembered]));
  }
  let source = await readFile(new URL('./entry-page.mjs', import.meta.url), 'utf8');
  source = source
    .replace("import {boot, grammar} from './entry.mjs';", 'const boot = globalThis.__entryPageFixture.boot, grammar = () => globalThis.__entryPageFixture.page.link;')
    .replace("import {applicationName, loadRuntime, mountDesktop, prefetchApplication, prefetchApplications, prefetchDesktop, prepareGpu, reportPageErrors} from './host.mjs';", 'const applicationName = async () => null, loadRuntime = async () => globalThis.__entryPageFixture.runtime, mountDesktop = (...args) => globalThis.__entryPageFixture.mounted.push(args), prefetchApplication = () => {}, prefetchApplications = apps => globalThis.__entryPageFixture.prefetched.push(apps), prefetchDesktop = () => {}, prepareGpu = () => globalThis.__entryPageFixture.prepareGpu(), reportPageErrors = () => {};')
    .replace(/import \{([^}]*)\bstartArrival, ([^}]*)\} from '.\/arrival.mjs';/, "import {$1$2} from './arrival.mjs';\nconst startArrival = () => {};")
    .replace("import {servedVersion, watchForUpdate} from './bundle-update.mjs';", "import {servedVersion} from './bundle-update.mjs';\nconst watchForUpdate = () => {};")
    .replace("import {rendererUnavailable} from './webgpu-safety.mjs';", 'const rendererUnavailable = async () => null;')
    .replace("import {browserIdentity, browserPerson, currentIdentity, fetchEnvelope, IdentityError, keptIdentity, MAX_DISPLAY_NAME_BYTES, signInWithPasskey} from './identity.mjs';", 'const currentIdentity = () => globalThis.__entryPageFixture.identity;\nconst browserPerson = async () => globalThis.__entryPageFixture.identity;\nconst keptIdentity = async () => globalThis.__entryPageFixture.kept;\nconst signInWithPasskey = (...args) => globalThis.__entryPageFixture.signIn(...args);\nconst fetchEnvelope = async () => null;')
    .replace('hostToReconnect, joinGranted, joinViaHost,', 'hostToReconnect, joinGranted,')
    .replace('identityProfile, inspectInvitation, invitedServer, joinWorkspace, openInvitation,', 'identityProfile, inspectInvitation, invitedServer, openInvitation,')
    .replace('return await pairViaHost({origin,', 'return await pairViaHost({pair: (...args) => globalThis.__entryPageFixture.pair(...args), findProfile: (...args) => globalThis.__entryPageFixture.findProfile(...args), origin,')
    .replace('const PAGE_BUILD', 'const joinViaHost = (...args) => globalThis.__entryPageFixture.joinViaHost(...args);\nconst joinWorkspace = (...args) => globalThis.__entryPageFixture.joinWorkspace(...args);\nconst PAGE_BUILD')
    .replace(/from '(\.\/[^']+)'/g, (_all, path) => `from '${new URL(path, import.meta.url).href}'`);
  assert.ok(source.includes('pairViaHost({pair: (...args) => globalThis.__entryPageFixture.pair(...args), findProfile: (...args) => globalThis.__entryPageFixture.findProfile(...args), origin,'), 'the page pairs through the fixture’s host');
  // A fresh module per fixture; the nonce rides in the source since Bun refuses a fragment on a base64 data URL.
  const module = await import(`data:text/javascript;base64,${Buffer.from(`${source}\n// ${Math.random()}`).toString('base64')}`);
  const {startEntryPage} = module;
  // A page that cannot get in throws its cause on, to the console.
  // The entry listens on the window for a link followed while it shows.
  if (entryParts) globalThis.window = globalThis;
  const failure = await startEntryPage({mode: 'desktop'}).then(() => null, error => error)
    .finally(() => { if (entryParts) delete globalThis.window; });
  await tick();
  // `signed()` stands for the pairing a sign-in makes (`signInAs`): the device
  // this page becomes, certified as the person, in `workspace` (the one it
  // pairs through), which asks its server for the person's own workspace when
  // no remembered one is opened (`own`: what the server answers).
  const signed = ({workspace = 'current', own = 'mine'} = {}) => async (userPublicKey, invite) => {
    calls.push(['pair', userPublicKey, invite]);
    return {...page.peer, workspace,
      enterOwnWorkspace: async () => { calls.push(['own']); return {workspace: own, name: 'Mine', origin: ORIGIN}; }};
  };
  return {calls, shown, diagnostics, page, prepared, signed, mounted: globalThis.__entryPageFixture.mounted, prefetched: globalThis.__entryPageFixture.prefetched, listeners, session: stored, failure, at: () => assigned, screen, module};
}

/** A workspace of the host other than its listed one (the Lounge), which a link names. */
const STUDIO = '7d7cf1fb-7214-4cce-9b34-1b5fdf9c0d3c';
/** The way-in rules for a link to such a workspace, by a browser with no account there
 * (tilo-session-client's `way_in`, tested there): come back as the person, take the
 * workspace's own entry where it lets anyone in, or sign in first. */
const studioRules = {wayIn: (_link, here) => {
  const known = JSON.parse(here);
  return known.person ? 'reconnect' : known.listed ? 'entry' : 'sign-in';
}, wayInAfter: () => 'no-access', admittedAsAsked: () => true};
/** A directory each server answers (`<origin>/peer/v1/directory`), by origin: the
 * public workspaces it lists, as `publicListings` reads them. */
const directories = listed => async (url, init) => {
  const at = new URL(url);
  if (at.pathname !== '/peer/v1/directory' || init?.method !== 'POST') return {ok: true};
  return {ok: true, json: async () => ({workspaces: listed[at.origin] ?? []})};
};
/** A page that opens `#/@node.example/w/<STUDIO>/so.tilo.chat`: the real admission, with the page's own deps. */
const studioLink = ({reconnected = []} = {}) => async ({deps, onStep}, booted) => {
  const scope = {kind: 'workspace', host: ORIGIN, workspace: STUDIO};
  deps.onScope({scope, entry: {base: 'https://web.example/', home: LOUNGE}});
  onStep('join');
  const {peer} = await admit({scope, runtime: studioRules, deps: {...deps, workspaceAt: () => LOUNGE,
    reconnect: async options => { reconnected.push(options); return {...booted.peer, workspace: STUDIO}; }}});
  return peer ? booted : null;
};

test('a signed-out browser at a link to a workspace is asked to sign in, and comes back to that link signed in', async () => {
  globalThis.location = undefined;
  const before = {credential: Object.getOwnPropertyDescriptor(globalThis, 'PublicKeyCredential'), navigator: Object.getOwnPropertyDescriptor(globalThis, 'navigator')};
  Object.defineProperty(globalThis, 'PublicKeyCredential', {value: function PublicKeyCredential() {}, configurable: true});
  Object.defineProperty(globalThis, 'navigator', {value: {credentials: {get() {}}, language: 'en'}, configurable: true});
  try {
    const joins = [];
    const {failure, screen, calls} = await fixture({identity: null, kept: null, entryParts: true,
      join: async () => { joins.push('joinViaHost'); throw new Error('nothing joins'); }, boot: studioLink()});
    // Nothing failed and nothing joined: the page asks for a sign-in.
    assert.equal(failure, null, 'no failure is thrown on to the console');
    assert.deepEqual(joins, []);
    const root = document.documentElement.dataset;
    assert.equal(root.entry, 'refused');
    assert.equal(root.failure, undefined, 'never the failure screen');
    assert.equal(screen['arrival-refusal-heading'].textContent, 'Sign in to open this workspace');
    assert.match(screen['arrival-refusal-body'].textContent, /Sign in as one of them/);
    assert.doesNotMatch(screen['arrival-refusal-heading'].textContent, /Lounge/, 'not under the site’s own workspace’s name');
    // Signing in with a passkey loads the same link again, as the person: not their own workspace.
    const parts = {};
    for (const id of ['arrival-sign-in', 'arrival-sign-in-ask', 'arrival-sign-in-agent', 'arrival-sign-in-button', 'arrival-sign-in-note']) {
      parts[id] = screen[id] ?? Object.assign(new Element(id), {hidden: false, textContent: '', classList: {add() {}, remove() {}}});
    }
    const {module, session: kept} = await fixture({identity: null, kept: null, boot: async () => null});
    document.getElementById = id => parts[id] ?? null;
    globalThis.location = {href: `https://web.example/#/@node.example/w/${STUDIO}/so.tilo.chat`, pathname: '/', reload: () => calls.push('reload')};
    globalThis.__entryPageFixture.signIn = async () => ({userPublicKey: USER});
    module.offerSignIn({workspace: {name: null, host: {origin: ORIGIN}, workspace: STUDIO}, named: null, messages: null, runtime: null,
      link: null, stay: true});
    await parts['arrival-sign-in-button'].listeners.get('click')();
    assert.ok(calls.includes('reload'), 'the link loads again');
    assert.equal(globalThis.location.href, `https://web.example/#/@node.example/w/${STUDIO}/so.tilo.chat`, 'at the place it named');
    assert.equal(kept.get('tilo-open-own-workspace'), undefined, 'not sent to a workspace of their own');
    // Loaded again, signed in: the page comes back as the person into the linked workspace, and opens it.
    const reconnected = [];
    const signedIn = await fixture({boot: studioLink({reconnected})});
    assert.equal(signedIn.failure, null);
    assert.deepEqual(reconnected.map(asked => [asked.origin, asked.workspace]), [[ORIGIN, STUDIO]]);
    assert.equal(document.documentElement.dataset.entry, 'open', 'the linked workspace opens');
    // Signed in with their agent instead, the browser keeps no person, only
    // that it became their device on the host: that counts as signed in too.
    const agentReconnected = [];
    const asDevice = await fixture({identity: null, kept: null, boot: studioLink({reconnected: agentReconnected}),
      local: {'tilo-host-memberships': JSON.stringify({[ORIGIN]: {origin: ORIGIN, workspace: STUDIO, userPublicKey: USER, paired: true}})}});
    assert.equal(asDevice.failure, null);
    assert.deepEqual(agentReconnected.map(asked => [asked.origin, asked.workspace]), [[ORIGIN, STUDIO]]);
    assert.equal(document.documentElement.dataset.entry, 'open', 'the linked workspace opens for the agent-signed-in device');
  } finally {
    for (const [name, descriptor] of [['PublicKeyCredential', before.credential], ['navigator', before.navigator]]) {
      if (descriptor) Object.defineProperty(globalThis, name, descriptor); else delete globalThis[name];
    }
  }
});

test('a signed-out browser at a link to a public workspace is let in through that workspace’s own entry, not asked to sign in', async () => {
  const before = globalThis.fetch;
  const joins = [];
  try {
    globalThis.fetch = directories({[ORIGIN]: [{workspace: STUDIO, name: 'Studio', entry: `directory/${STUDIO}/`, invitationOnly: false}]});
    const {failure} = await fixture({identity: null, kept: null, entryParts: true, boot: studioLink(),
      join: async options => { joins.push(options); return {workspace: STUDIO}; }});
    assert.equal(failure, null);
    assert.deepEqual(joins.map(({workspace}) => [workspace.workspace, workspace.admission, workspace.name]),
      [[STUDIO, `${ORIGIN}/peer/v1/directory/${STUDIO}/`, 'Studio']], 'its own entry, never the site’s listed one');
    assert.equal(document.documentElement.dataset.refusal, undefined, 'no sign-in screen');
    assert.equal(document.documentElement.dataset.entry, 'open', 'the linked workspace opens');
  } finally {
    globalThis.fetch = before;
  }
});

test('the sign-in screen at a closed workspace offers someone with no Tilo identity the site’s wait list, as its front page does', async () => {
  const before = globalThis.fetch;
  const asked = [];
  try {
    const read = directories({'https://home.example': [{workspace: 'lounge', name: 'Lounge', entry: 'directory/lounge/', invitationOnly: true,
      waitList: {contact: null, mail: true}}]});
    // The page also reads what sits beside it (its locale file, which under
    // node is a `file://` URL): only what it asks a server counts.
    globalThis.fetch = async (url, init) => {
      const at = new URL(url);
      if (at.protocol === 'http:' || at.protocol === 'https:') asked.push(at.origin);
      return read(url, init);
    };
    const {failure, screen} = await fixture({identity: null, kept: null, entryParts: true, boot: studioLink(),
      join: async () => { throw new Error('nothing joins'); }});
    assert.equal(failure, null);
    assert.deepEqual([...new Set(asked)].sort(), ['https://home.example', ORIGIN].sort(), 'the link’s server and the home’s directory');
    assert.equal(screen['arrival-refusal-heading'].textContent, 'Sign in to open this workspace');
    assert.match(screen['arrival-refusal-body'].textContent, /New to Tilo\? It’s invite-only right now: leave your email/);
    assert.equal(document.documentElement.dataset.refusal, 'wait-list');
    assert.equal(screen['arrival-invite'].hidden, true, 'pasting an invite waits behind its toggle, as on the front page');
    // A site that keeps no wait list asks only for a sign-in or an invite.
    globalThis.fetch = directories({});
    const plain = await fixture({identity: null, kept: null, entryParts: true, boot: studioLink(),
      join: async () => { throw new Error('nothing joins'); }});
    assert.match(plain.screen['arrival-refusal-body'].textContent, /Sign in as one of them, or paste an invite link/);
    assert.equal(document.documentElement.dataset.refusal, 'invite');
  } finally {
    globalThis.fetch = before;
  }
});

test('the entry says what works where passkeys cannot be used, and nothing when the person closes the sheet', async () => {
  const {module, calls} = await fixture({boot: async () => { throw new AdmissionError('no-access', 'You don’t have access to this workspace.'); }});
  // The entry's sign-in row (index.html, `#arrival-sign-in`), by id.
  const parts = {};
  for (const id of ['arrival-sign-in', 'arrival-sign-in-ask', 'arrival-sign-in-agent', 'arrival-sign-in-button', 'arrival-sign-in-note']) {
    const classes = new Set();
    parts[id] = Object.assign(new Element(id), {hidden: false, textContent: '', classes,
      classList: {add: name => classes.add(name), remove: name => classes.delete(name)}});
  }
  document.getElementById = id => parts[id] ?? null;
  // A web view inside an app has passkeys to offer, whether or not its app may use this site's.
  const before = {credential: Object.getOwnPropertyDescriptor(globalThis, 'PublicKeyCredential'), navigator: Object.getOwnPropertyDescriptor(globalThis, 'navigator')};
  Object.defineProperty(globalThis, 'PublicKeyCredential', {value: function PublicKeyCredential() {}, configurable: true});
  Object.defineProperty(globalThis, 'navigator', {value: {credentials: {get() {}}}, configurable: true});
  try {
    module.offerSignIn({workspace: null, named: 'Lounge', messages: null, runtime: null, link: null});
    const button = parts['arrival-sign-in-button'];
    const note = parts['arrival-sign-in-note'];
    assert.equal(button.textContent, 'Use a passkey');
    assert.equal(button.hidden, false);
    const press = answer => {
      globalThis.__entryPageFixture.signIn = answer;
      return button.listeners.get('click')();
    };
    // The system refused before it showed anything: the place cannot use passkeys, and the
    // entry says the way in that works there.
    await press(async () => { throw Object.assign(new Error('Passkeys cannot be used here'), {name: 'IdentityError', code: 'passkeys-refused'}); });
    assert.equal(note.textContent, 'Passkeys can’t be used here. On a device where you are signed in, choose “Pair a new device” and paste its link here.');
    assert.ok(note.classes.has('refusal-problem'));
    assert.equal(button.disabled, false, 'the other ways in stay');
    // The person closed the sheet: their own answer, said by nothing, and the sentence before goes.
    await press(async () => { throw Object.assign(new Error('closed'), {name: 'NotAllowedError'}); });
    assert.equal(note.textContent, '');
    assert.ok(!note.classes.has('refusal-problem'));
    // A passkey of another site's is still told apart from both.
    await press(async () => { throw Object.assign(new Error('none'), {name: 'IdentityError', code: 'no-envelope'}); });
    assert.match(note.textContent, /doesn’t open a Tilo identity this site knows/);
    assert.ok(!calls.includes('reload'), 'nobody was signed in');
  } finally {
    for (const [name, descriptor] of [['PublicKeyCredential', before.credential], ['navigator', before.navigator]]) {
      if (descriptor) Object.defineProperty(globalThis, name, descriptor); else delete globalThis[name];
    }
  }
});

test('the GPU device opens as the page starts, while the host is asked, once', async () => {
  const steps = [];
  const {prepared, failure} = await fixture({boot: async ({onStep}, page) => {
    steps.push(globalThis.__entryPageFixture.prepared.length);
    onStep('join');
    onStep('open');
    onStep('open');
    return page;
  }});
  assert.deepEqual(steps, [1], 'the device was asked for before the page asked the host');
  assert.equal(prepared.length, 1, 'and only once, however the page moves on');
  assert.equal(failure, null);
  assert.equal(document.documentElement.dataset.entry, 'open');
});

test('a page the host refuses mounts nothing, so it compiles no renderer for the next page to wait on', async () => {
  const {prepared, failure, mounted} = await fixture({boot: async ({onStep}) => {
    onStep('join');
    throw new AdmissionError('no-access', 'You don’t have access to this workspace.');
  }});
  assert.equal(failure?.code, 'no-access');
  assert.equal(document.documentElement.dataset.entry, 'refused');
  // The device alone: the renderer's pipelines compile as a mount draws.
  assert.equal(prepared.length, 1);
  assert.deepEqual(mounted, []);
});

test('a page that cannot open its workspace shows one failure screen: the cause, Try again, and the other workspaces in it', async () => {
  const {failure, screen, calls} = await fixture({boot: async ({deps, onStep}) => {
    deps.onScope({scope: {kind: 'workspace', host: 'https://127.0.0.1:24100', workspace: 'current'},
      entry: {base: 'https://web.example/', home: null}});
    onStep('join');
    throw Object.assign(new Error('join: HTTP 500'), {name: 'HostAdmissionError', code: 'refused', status: 500});
  }});
  assert.equal(failure?.message, 'join: HTTP 500', 'the cause is thrown on, to the console');
  const root = document.documentElement.dataset;
  assert.equal(root.failure, 'host-error');
  assert.equal(screen['arrival-failure-heading'].textContent, '127.0.0.1:24100 couldn’t open this workspace');
  assert.match(screen['arrival-failure-body'].textContent, /answered with an error/);
  assert.equal(screen['arrival-failure-retry'].hidden, false);
  assert.match(screen['arrival-failure-text'].textContent, /Host: 127\.0\.0\.1:24100/);
  assert.match(screen['arrival-failure-text'].textContent, /HTTP 500/);
  // The other workspaces are part of the screen, never a card over it.
  assert.equal(document.body.children.length, 0, 'no card over the screen');
  assert.equal(screen['arrival-failure-elsewhere'].hidden, false);
  const labels = screen['arrival-failure-others'].children.map(item => item.children[0].children[0].textContent);
  assert.ok(labels.includes('Other'), labels.join());
  const other = screen['arrival-failure-others'].children[labels.indexOf('Other')];
  other.children[0].listeners.get('click')();
  assert.ok(calls.includes('history') || calls.includes('assign'), 'it opens the other workspace');
});

test('a browser removed from its person’s devices reads that on arrival, on the one failure screen, with nothing to try again', async () => {
  const {failure, screen} = await fixture({boot: async ({onStep}) => {
    onStep('join');
    // What the session's stop reaches the page as: its server refused its hello and said why
    // (`whenReady` in peer.mjs keeps the code).
    throw Object.assign(new Error('This device was removed from your devices.'), {code: 'device-revoked'});
  }});
  assert.equal(failure?.name, 'AdmissionError');
  assert.equal(failure?.code, 'device-revoked');
  const root = document.documentElement.dataset;
  assert.equal(root.entry, 'refused');
  assert.equal(root.refusal, 'device-revoked');
  assert.equal(root.failure, 'refused');
  assert.equal(root.reconnect, undefined, 'never the screen of a server that is away');
  assert.equal(screen['arrival-failure-body'].textContent, 'This browser was removed from your devices, so it can’t open this workspace anymore.');
  assert.doesNotMatch(screen['arrival-failure-heading'].textContent + screen['arrival-failure-body'].textContent, /reach|restarting|certificate/i);
  assert.equal(screen['arrival-failure-retry'].hidden, true, 'trying again changes nothing');
  assert.equal(document.body.children.length, 0, 'no card over the screen');
});

test('a page whose host never answers names that host, not this site', async () => {
  globalThis.fetch = async () => ({ok: true});
  const {screen} = await fixture({boot: async ({deps}) => {
    deps.onScope({scope: {kind: 'workspace', host: 'https://127.0.0.1:24100', workspace: 'current'},
      entry: {base: 'https://web.example/', home: null}});
    // Let in, then the host's connection goes.
    deps.onAdmitted({workspaceName: 'Lounge'});
    throw new TypeError('Failed to fetch');
  }});
  assert.equal(document.documentElement.dataset.failure, 'unreachable');
  assert.equal(screen['arrival-failure-heading'].textContent, 'Can’t connect to 127.0.0.1:24100');
  assert.match(screen['arrival-failure-body'].textContent, /doesn’t trust its certificate/);
  assert.equal(screen['arrival-failure-host'].hidden, false);
  assert.equal(screen['arrival-failure-host'].textContent, 'Open 127.0.0.1:24100');
});

test('a page reports its failure to its entry where the browser keeps no beacons, and always within what the entry reads', async () => {
  const {module} = await fixture({boot: async () => { throw new AdmissionError('no-access', 'You don’t have access to this workspace.'); }});
  const {reportBody, sendReport, MAX_REPORT_BYTES} = module;
  // A Firefox stack is long: the cause still arrives, the stack is cut.
  const body = reportBody({kind: 'failed', message: 'loading: Error: Shared resource connection closed', source: 'https://lounge.example',
    line: 0, column: 0, stack: '@https://lounge.example/v/7/peer.mjs:1:1\n'.repeat(400), page: '/', build: '7'});
  assert.ok(new TextEncoder().encode(body).length <= MAX_REPORT_BYTES, `${body.length}`);
  const read = JSON.parse(body);
  assert.equal(read.message, 'loading: Error: Shared resource connection closed');
  assert.ok(read.stack.startsWith('@https://lounge.example/v/7/peer.mjs:1:1'));
  // LibreWolf keeps `sendBeacon` with beacons off: it says it sent and sends
  // nothing. The report goes as a request that outlives the page instead.
  const sent = [];
  const beacons = [];
  const fetch = async (url, init) => { sent.push({url: String(url), ...init}); return {ok: true}; };
  sendReport(new URL('https://host.example/directory/w/errors'), body, {navigator: {sendBeacon: (...args) => beacons.push(args) > 0}, fetch});
  assert.equal(sent.length, 1);
  assert.deepEqual(beacons, [], 'no beacon that may go nowhere');
  assert.equal(sent[0].url, 'https://host.example/directory/w/errors');
  assert.equal(sent[0].method, 'POST');
  assert.equal(sent[0].keepalive, true, 'it outlives the page');
  assert.equal(sent[0].mode, 'no-cors', 'any site reports to its host');
  assert.equal(sent[0].body, body);
  // A page without fetch sends a beacon.
  sendReport('https://host.example/directory/w/errors', body, {navigator: {sendBeacon: (...args) => beacons.push(args) > 0}, fetch: null});
  assert.equal(beacons.length, 1);
  // Nothing it meets throws into the page.
  sendReport('https://host.example/directory/w/errors', body, {navigator: {sendBeacon: () => { throw new TypeError('blocked'); }},
    fetch: () => { throw new TypeError('blocked'); }});
});

test('the account choice says it is the account choice, and resolves to the account taken', async () => {
  // Pages driving a link wait for this card rather than counting buttons
  // against a clock.
  const accounts = [{displayName: 'Fixture reader', userPublicKey: USER}, {displayName: '', userPublicKey: 'bb'.repeat(32)}];
  let chosen;
  const {failure} = await fixture({boot: async ({deps}, page) => {
    const choice = deps.chooseAccount(accounts);
    const shown = document.body.children.filter(form => form.dataset.card === 'accounts');
    assert.equal(shown.length, 1, 'one account choice is shown');
    const buttons = shown[0].children.find(child => child.className === 'tilo-entry-options' || child.className === 'tilo-entry-actions').children;
    assert.deepEqual(buttons.map(button => button.textContent), ['Fixture reader', 'bbbbbbbbbbbb']);
    buttons[1].listeners.get('click')();
    chosen = await choice;
    return page;
  }});
  assert.equal(failure, null);
  assert.equal(chosen, accounts[1]);
  assert.equal(document.body.children.filter(form => form.dataset.card === 'accounts').length, 0, 'taking an account closes the choice');
});

test('the invitation a link opens says who invited them, to what and as what, before they join', async () => {
  const cards = () => document.body.children.filter(form => form.dataset.card === 'invite');
  // What a node reads as: its own words, or its children's in order.
  const text = node => typeof node === 'string' ? node
    : node.textContent ?? node.children.map(text).join('');
  const shape = card => {
    const [head, ...rest] = card.children;
    const actions = rest.at(-1);
    const [from, workspace, people] = head.children;
    return {face: from.children.length > 1 ? from.children[0].style : null, from: text(from),
      workspace: text(workspace), heading: workspace.tag, people: people && text(people),
      role: rest.length > 1 ? rest[0].children.map(text) : null,
      actions: actions.children.map(text), primary: actions.children[0].className, join: actions.children[0],
      notNow: actions.children[1]};
  };
  let offered;
  let answered;
  let declined;
  let notNow;
  const {failure} = await fixture({boot: async ({deps}, page) => {
    const answer = deps.confirmInvitation({workspaceName: 'Lounge', role: 'commenter', members: 4,
      inviter: {name: 'Nimble Panda', userPublicKey: USER}});
    offered = shape(cards()[0]);
    offered.join.listeners.get('click')();
    answered = await answer;
    const other = deps.confirmInvitation({workspaceName: 'Commons', role: 'viewer', members: 1, inviter: null});
    declined = shape(cards()[0]);
    declined.notNow.listeners.get('click')();
    notNow = await other;
    return page;
  }});
  assert.equal(failure, null);
  // One sentence: who asked them, by their initial in the colour presence
  // gives them and their name as the host verified it, then the workspace
  // as the card's heading.
  assert.deepEqual(offered.face, {background: '#ffd166', color: '#101114'},
    'the monogram is drawn in the inviter’s presence colour, dark on a light one');
  assert.equal(offered.from, 'NNimble Panda invited you to');
  assert.equal(offered.workspace, 'Lounge');
  assert.equal(offered.heading, 'h2');
  assert.equal(offered.people, '4 people');
  // As what: the role's name, and beneath it what it lets them do, in the
  // desktop role chooser's words.
  assert.deepEqual(offered.role, ['Commenter', 'Can view shared work and add comments, but cannot edit it.']);
  assert.deepEqual(offered.actions, ['Join', 'Not now']);
  assert.equal(offered.primary, 'tilo-entry-primary', 'Join is the primary action');
  assert.equal(answered, true, 'Join joins');
  // A link that names no one: the card says the person is invited, with no
  // stand-in name or face.
  assert.equal(declined.face, null);
  assert.equal(declined.from, 'You’re invited to');
  assert.equal(declined.people, '1 person');
  assert.deepEqual(declined.role, ['Viewer', 'Can view shared work, but cannot comment or edit it.']);
  assert.equal(notNow, false, 'Not now does not join');
  assert.equal(cards().length, 0, 'either answer closes the card');
});

test('someone a wait-list invitation lets in comes back to their own workspace, kept the moment it exists', async () => {
  let kept;
  const {failure} = await fixture({boot: async ({deps}, page) => {
    const own = await deps.ownWorkspace({...page.peer, enterOwnWorkspace: async () => ({workspace: 'own-id', name: 'Ada'})});
    // Before anything opens it: a tab closed now still comes back to it.
    kept = chosenWorkspace();
    assert.deepEqual(own, {workspace: 'own-id', name: 'Ada'});
    return page;
  }});
  assert.equal(failure, null);
  assert.deepEqual(kept, {origin: ORIGIN, workspace: 'own-id'});
});

test('an invitation from someone the workspace has not named reads their profile, else plain words, never a key', async () => {
  const cards = () => document.body.children.filter(form => form.dataset.card === 'invite');
  // What a node reads as: its own words, or its children's in order.
  const text = node => typeof node === 'string' ? node
    : node.textContent ?? node.children.map(text).join('');
  const asked = [];
  globalThis.fetch = async url => {
    asked.push(String(url));
    return {ok: true, json: async () => ({profile: {user_public_key: USER, display_name: 'Asli'}, issued_at_ms: 1})};
  };
  const close = card => card.children.at(-1).children[1].listeners.get('click')();
  let profiled;
  let unnamed;
  const {failure} = await fixture({boot: async ({deps}, page) => {
    // The workspace signed no name for who asked: the card asks the machine
    // serving it for the name their profile keeps, as the desktop names
    // someone not here.
    const first = deps.confirmInvitation({workspaceName: 'Lounge', role: 'viewer', members: 2,
      inviter: {name: '', userPublicKey: USER}}, ORIGIN);
    await tick();
    profiled = text(cards()[0].children[0].children[0]);
    close(cards()[0]);
    await first;
    // A machine keeping no profile for them: the card says the person is
    // invited in plain words, with no face and no name made up for who
    // asked. Who asked is their key, which is never read as a name.
    globalThis.fetch = async () => ({ok: false});
    const second = deps.confirmInvitation({workspaceName: 'Lounge', role: 'viewer', members: 2,
      inviter: {name: '', userPublicKey: USER}}, ORIGIN);
    await tick();
    const from = cards()[0].children[0].children[0];
    unnamed = {from: text(from), face: from.children.length > 1 ? from.children[0].style : null};
    close(cards()[0]);
    await second;
    return page;
  }});
  assert.equal(failure, null);
  assert.equal(profiled, 'AAsli invited you to', 'their profile names them, their initial in their colour');
  assert.deepEqual(unnamed, {from: 'You’re invited to', face: null},
    'plain words where nobody named who asked: no stand-in name, no face');
  assert.equal(new URL(asked[0]).pathname, '/peer/v1/profile/fetch', asked.join());
});

test('a wait-list invitation is the short link its site keeps, not the invitation’s text', async () => {
  const ed = {requestId: '05'.repeat(32), contact: 'ed@example.org', addedAtMs: 1_000};
  const {calls, diagnostics} = await fixture({member: {
    createInvitation: async () => 'SIGNED-invitation',
    membership: {list: async () => ({name: 'Current', members: []}),
      mayInvite: async () => true, waitList: async () => [ed], unlist: async () => {}, mailInvite: async () => { throw new Error('this entry sends no mail'); }},
  }});
  diagnostics.onHostAction('waiting-to-join');
  await tick();
  const waiting = document.body.children.find(form => form.dataset.card === 'waiting');
  assert.ok(waiting, 'the wait list is shown');
  // The listed section’s rows: one per address, each Invite then Remove.
  const row = waiting.children[1].children[2].children[0];
  assert.equal(row.children[1].textContent, 'Invite');
  row.children[1].listeners.get('click')();
  await tick();
  // The link kept ready for the one-press copy is sealed the same way as the
  // row's: both are the site's short link, naming the host they admit into.
  assert.deepEqual(calls.filter(call => Array.isArray(call) && call[0] === 'sealed').at(-1),
    ['sealed', 'https://web.example/', ORIGIN, 'SIGNED-invitation'],
    'the invitation is sealed onto the page’s own site, naming the host it admits into');
  const share = document.body.children.find(form => form.dataset.card === 'share');
  assert.ok(share, 'an entry that sends no mail shares the link instead');
  assert.equal(share.children.find(child => child.name === 'link').value, SEALED,
    'what the inviter sends is the short link, not the invitation’s text');
  assert.match(share.children[1].textContent, /as an editor/);
});

test('workspaces another host held at the origin of the page host are no longer offered', async () => {
  // The host at ORIGIN was set up again on fresh state twice since this
  // browser entered workspaces there; both workspaces went with the hosts before.
  const {shown} = await fixture({remembered: [
    {origin: ORIGIN, workspace: 'gone-1', hostDevice: 'e1'.repeat(32), name: 'Other'},
    {origin: ORIGIN, workspace: 'gone-2', hostDevice: 'e2'.repeat(32), name: 'Other'},
    {origin: 'https://elsewhere.example', workspace: 'away', hostDevice: 'e3'.repeat(32), name: 'Away'},
  ]});
  assert.deepEqual(shown.at(-1).map(action => action.title), ['Set up a machine', 'Open your desktop', 'Open Other', 'Open Away', 'New workspace', 'Join a workspace']);
  assert.deepEqual(enteredWorkspaces().map(entry => entry.workspace), ['current', 'other', 'away']);
});

test('the person’s workspaces come from their host when this browser remembers none', async () => {
  // This browser's kept list is unreadable (an older spelling), so it offers
  // nothing of its own; the host still lists the person's workspaces, and the
  // switcher fills them in once it answers.
  const {shown} = await fixture({remembered: null, local: {'tilo-entered-workspaces': '[{"room":"old spelling"}]'},
    member: {personalWorkspaces: async () => [
      {workspace: 'current', name: 'Lounge', role: 'member', host: {name: 'node.example'}},
      {workspace: 'mine', name: 'My workspace', role: 'owner', host: {name: 'node.example'}},
    ]}});
  await tick();
  const titles = shown.at(-1).map(action => action.title);
  assert.ok(titles.includes('Open My workspace'), titles.join(', '));
  assert.ok(!titles.includes('Open Lounge'), 'the workspace in front is not offered back');
});

test('a bare home entry, which names no workspace until admitted, does not offer the one in front back to itself', async () => {
  const {shown} = await fixture({scope: {kind: 'workspace', host: ORIGIN, workspace: null},
    member: {personalWorkspaces: async () => [
      {workspace: 'current', name: 'Lounge', role: 'member', host: {name: 'node.example'}},
      {workspace: 'mine', name: 'My workspace', role: 'owner', host: {name: 'node.example'}},
    ]}});
  await tick();
  const titles = shown.map(actions => actions.map(action => action.title)).at(-1);
  assert.ok(titles.includes('Open My workspace'), titles.join(', '));
  assert.ok(!titles.includes('Open Lounge'), 'the workspace in front is not offered back');
});

test('the page keeps switch actions together and dispatches them', async () => {
  const {calls, shown, diagnostics} = await fixture();
  const actions = shown.at(-1);
  assert.deepEqual(actions.map(action => action.title), ['Set up a machine', 'Open your desktop', 'Open Other', 'New workspace', 'Join a workspace']);
  diagnostics.onHostAction(actions.find(action => action.title === 'Open Other').id);
  await tick();
  assert.deepEqual(calls, ['flush', 'stop', 'cleanup', 'dispose', 'history', 'reload']);
  assert.deepEqual(chosenWorkspace(), {origin: ORIGIN, workspace: 'other'});
});

test('leaving a workspace opens another of the person’s in its place, never the one left', async () => {
  const {calls, diagnostics} = await fixture();
  choose(diagnostics, 'left-workspace:current');
  await settled();
  assert.deepEqual(calls, ['flush', 'stop', 'cleanup', 'dispose', 'history', 'reload']);
  assert.deepEqual(chosenWorkspace(), {origin: ORIGIN, workspace: 'other'});
});

test('a workspace offers the way back to the person’s own desktop, and their own desktop does not offer itself', async () => {
  const {calls, shown, diagnostics, page} = await fixture();
  page.link.format = route => `https://web.example/#/${route.target.scope.kind === 'own' ? 'me' : route.target.scope.workspace}`;
  chooseWorkspace({origin: ORIGIN, workspace: 'other'});
  const back = shown.at(-1).find(action => action.title === 'Open your desktop');
  assert.ok(back, 'a workspace is no dead end');
  const urls = [];
  globalThis.history.pushState = (_state, _title, url) => { urls.push(url); calls.push('history'); };
  diagnostics.onHostAction(back.id);
  await tick();
  assert.deepEqual(calls.slice(-6), ['flush', 'stop', 'cleanup', 'dispose', 'history', 'reload']);
  assert.deepEqual(urls, ['https://web.example/#/me']);
  assert.deepEqual(chosenWorkspace(), {origin: ORIGIN, workspace: 'other'}, 'the workspace a bare entry reopens stays');
  const {shown: ownShown} = await fixture({own: true});
  assert.ok(!ownShown.flat().some(action => action.title === 'Open your desktop'));
});

test('creating a workspace stays in the current workspace when pending work cannot be saved', async () => {
  const {calls, diagnostics, page} = await fixture();
  page.mount.flush = async () => { calls.push('flush'); throw new Error('storage unavailable'); };
  diagnostics.onHostAction('new-workspace');
  await tick();
  const form = document.body.children.at(-1);
  form.children.find(child => child.tag === 'label').children[0].value = 'Created';
  await form.listeners.get('submit')({preventDefault() {}});
  await tick();
  assert.deepEqual(calls, ['flush']);
  assert.equal(chosenWorkspace(), null);
  const failure = document.body.children.at(-1);
  assert.match(failure.children[0].textContent, /not saved yet/);
  failure.children.flatMap(child => [child, ...(child.children ?? [])]).find(child => child.tag === 'button').listeners.get('click')();
  await tick();
});

test('creating a workspace remembers it and uses the existing save-and-close switch path', async () => {
  const {calls, diagnostics} = await fixture();
  diagnostics.onHostAction('new-workspace');
  await tick();
  const form = document.body.children.at(-1);
  form.children.find(child => child.tag === 'label').children[0].value = 'Created';
  await form.listeners.get('submit')({preventDefault() {}});
  await tick();
  assert.deepEqual(calls, ['flush', 'stop', 'cleanup', 'dispose', 'history', 'reload']);
  assert.deepEqual(chosenWorkspace(), {origin: ORIGIN, workspace: 'created'});
  assert.equal(enteredWorkspaces().find(entry => entry.workspace === 'created').name, 'Created');
});


test('the person’s own desktop writes as their root identity and shows them unnamed until they give a name', async () => {
  let mountOwn;
  const {calls} = await fixture({own: true, boot: async (options, page) => { mountOwn = options.deps.mountDesktop; return page; }});
  // No workspace's peer certifies who writes there: the browser's root identity does.
  await mountOwn('surface', {onStatus() {}});
  const [[, options]] = globalThis.__entryPageFixture.mounted;
  assert.equal(options.author, USER);
  const own = calls.find(call => call[0] === 'own');
  assert.ok(own, 'the desktop is told who its person is');
  assert.equal(own[1].user, USER);
  assert.equal(own[1].label, '', 'a person without a name is shown as one, never by a made-up name');
});

test('the home workspace’s peer that carries reports from another workspace joins it without calls or a live session', async () => {
  let homePeer;
  await fixture({boot: async (options, page) => { homePeer = options.deps.homePeer; return page; }});
  const home = {name: 'Lounge', host: {origin: 'https://lounge.example'}, admission: 'https://lounge.example/'};
  const joined = [];
  const carrier = {workspace: 'LOUNGE'};
  globalThis.__entryPageFixture.joinViaHost = async options => { joined.push(options); return carrier; };
  assert.equal(await homePeer(home), null, 'a person the home never let in gets no peer there');
  assert.deepEqual(joined, []);
  localStorage.setItem('tilo-host-memberships', JSON.stringify({'https://lounge.example': {origin: 'https://lounge.example', workspace: 'LOUNGE', userPublicKey: USER}}));
  assert.equal(await homePeer(home), carrier);
  assert.equal(joined.length, 1);
  // Nothing live or in calls, which only the workspace in front has.
  assert.equal(joined[0].background, undefined);
  assert.equal(joined[0].live, false);
  assert.equal(joined[0].voice, false);
  assert.equal(joined[0].workspace, home);
  await assert.rejects(joined[0].wait({attempt: 0}), /did not let this device in/, 'it never waits at the entry');
});

test('an embedded workspace offers no invite sheet it could not carry out', async () => {
  let deps;
  await fixture({search: '?embed=1', boot: async (options, page) => { deps = options.deps; return page; }});
  const embedded = deps;
  // The host's own key rides along: it is a member of the workspaces it
  // holds, and no person a link is made for.
  const serverKey = Array(32).fill(0x2a);
  const joining = {workspace: 'current', origin: ORIGIN, userPublicKey: USER, host: {hostCert: {user_public_key: serverKey}}};
  assert.equal(embedded.invite({peer: joining, entry: {base: 'https://web.example/'}}), null);
  await fixture({boot: async (options, page) => { deps = options.deps; return page; }});
  // Named no earlier than admission names the person; the sheet makes the
  // link either way, and says who made it once there is a name.
  assert.deepEqual(deps.invite({peer: joining, entry: {base: 'https://web.example/'}}),
    {base: 'https://web.example/', origin: ORIGIN, workspace: 'current', name: '', user: USER, host: '2a'.repeat(32)});
});

const LOUNGE = {name: 'Lounge', host: {origin: 'https://home.example'}, admission: 'https://home.example/peer/v1/directory/lounge/'};

test('an embed shows its site’s fallback until the visitor’s first gesture, which joins', async () => {
  const told = [];
  const requests = [];
  globalThis.window = globalThis;
  globalThis.parent = {postMessage: (message, origin) => told.push({message, origin})};
  globalThis.fetch = async url => { requests.push(String(url)); throw new Error('the page asked the network before anyone did anything'); };
  const {calls, failure, listeners, session} = await fixture({search: '?embed=1', boot: async (options, booted) => {
    const {deps} = options;
    deps.onScope({scope: booted.router.route.target.scope, entry: {base: 'https://home.example/', embedder: 'https://site.example'}});
    // Nothing mounts. The page takes no admission and makes no person of its own.
    assert.deepEqual(await deps.enterViaHost({workspace: LOUNGE}), {idle: true});
    assert.equal(document.documentElement.dataset.entry, 'idle');
    assert.equal(document.getElementById('status').textContent, 'Click or type to join');
    return null;
  }});
  assert.equal(failure, null);
  assert.deepEqual(told, [{message: {type: 'tilo-embed', state: 'idle'}, origin: 'https://site.example'}]);
  assert.deepEqual(requests, [], 'no spectate, no entry, no person');
  assert.deepEqual(calls, []);
  // The first pointer or key joins on the page that loads for it, and the
  // gesture goes no further than the page.
  const gesture = listeners.find(held => held.type === 'pointerdown');
  assert.ok(gesture);
  assert.ok(listeners.some(held => held.type === 'keydown'));
  let stopped = false;
  gesture.listener({preventDefault: () => { stopped = true; }, stopPropagation: () => { stopped = true; }});
  assert.ok(stopped, 'the site’s own click handlers never see it');
  assert.equal(document.getElementById('status').textContent, 'Joining…');
  assert.equal(session.get('tilo-embed-join'), '1');
  assert.deepEqual(calls, ['reload']);
  assert.deepEqual(listeners.filter(held => ['pointerdown', 'keydown'].includes(held.type)), [], 'the gesture is taken once');
});

test('the page that loads after the gesture joins as this browser’s own person', async () => {
  const requests = [];
  globalThis.window = globalThis;
  globalThis.parent = {postMessage: () => {}};
  globalThis.fetch = async url => { requests.push(String(url)); throw new Error('no request before the entry'); };
  const joined = [];
  const {failure, session} = await fixture({search: '?embed=1', session: {'tilo-embed-join': '1'}, boot: async (options, booted) => {
    const {deps} = options;
    deps.onScope({scope: booted.router.route.target.scope, entry: {base: 'https://home.example/', embedder: 'https://site.example'}});
    globalThis.__entryPageFixture.joinViaHost = async options => { joined.push(options); return {workspace: 'LOUNGE', origin: 'https://home.example'}; };
    assert.equal((await deps.enterViaHost({workspace: LOUNGE})).workspace, 'LOUNGE');
    return null;
  }});
  assert.equal(failure, null);
  assert.equal(joined.length, 1, 'joined once, with no pairing code');
  // The person the browser holds, not a visitor's key the page made up.
  assert.equal(joined[0].identity.userPublicKey, USER);
  assert.equal(joined[0].workspace, LOUNGE);
  assert.equal(joined[0].displayName, null, 'no name given, none made up');
  assert.deepEqual(requests, [], 'no request was made for a visitor');
  assert.equal(session.has('tilo-embed-join'), false, 'the gesture is spent');
});

test('a person who joins through an invitation joins under no name until they give one', async () => {
  globalThis.window = globalThis;
  globalThis.parent = {postMessage: () => {}};
  const joined = [];
  const {failure} = await fixture({boot: async (options, booted) => {
    const {deps} = options;
    deps.onScope({scope: booted.router.route.target.scope, entry: {base: 'https://home.example/'}});
    globalThis.__entryPageFixture.joinWorkspace = async options => { joined.push(options); return {workspace: 'WORKSPACE'}; };
    await deps.joinWorkspace({origin: 'https://home.example', invitation: 'INVITATION'});
    return null;
  }});
  assert.equal(failure, null);
  assert.equal(joined.length, 1);
  assert.equal(joined[0].identity.userPublicKey, USER);
  // A person who gave no name joins under none: never a made-up name.
  assert.equal(joined[0].displayName, null);
});

test('an uncaught page error reaches the entry the site names', async () => {
  // What the page sent to its entry's errors route: a request that outlives the page.
  const beacons = [];
  globalThis.window = globalThis;
  globalThis.parent = {postMessage: () => {}};
  const before = globalThis.fetch;
  globalThis.fetch = async (url, init) => {
    if (String(url).endsWith('/errors')) { beacons.push({url: String(url), body: init.body, keepalive: init.keepalive}); return {ok: true}; }
    return before(url, init);
  };
  const {failure, listeners} = await fixture({search: '?embed=1', boot: async (options, booted) => {
    options.deps.onScope({scope: booted.router.route.target.scope,
      entry: {base: 'https://home.example/', home: LOUNGE, embedder: 'https://site.example'}});
    return null;
  }});
  assert.equal(failure, null);
  const fire = (type, event) => listeners.filter(held => held.type === type).forEach(held => held.listener(event));
  fire('error', {message: 'x is not a function', filename: 'https://home.example/v/7/page.js', lineno: 12, colno: 4,
    error: {stack: 'at page.js:12'}});
  fire('unhandledrejection', {reason: Error('the host went away')});
  assert.equal(beacons.length, 2);
  assert.equal(beacons[0].url, 'https://home.example/peer/v1/directory/lounge/errors', 'the entry\'s own errors route');
  assert.equal(beacons[0].keepalive, true);
  assert.deepEqual(JSON.parse(beacons[0].body), {
    kind: 'error', message: 'x is not a function', source: 'https://home.example/v/7/page.js', line: 12, column: 4,
    stack: 'at page.js:12', page: '/', build: 'development'});
  const rejection = JSON.parse(beacons[1].body);
  assert.equal(rejection.kind, 'unhandledrejection');
  assert.equal(rejection.message, 'the host went away');
  assert.equal(rejection.page, '/');
  assert.equal(typeof rejection.stack, 'string');
  // A page that keeps failing says five things a load, never more.
  for (let i = 0; i < 10; i++) fire('error', {message: `again ${i}`});
  assert.equal(beacons.length, 5);
  globalThis.fetch = before;
});

test('a page that stops, on any screen, tells the host it could not open its workspace, and why', async () => {
  const reports = [];
  const before = globalThis.fetch;
  globalThis.fetch = async (url, init) => {
    if (String(url).endsWith('/errors')) { reports.push(JSON.parse(init.body)); return {ok: true}; }
    return before(url, init);
  };
  try {
    // A browser that cannot draw: its own screen, and the host hears it.
    await fixture({boot: async ({deps}, booted) => {
      deps.onScope({scope: booted.router.route.target.scope, entry: {base: 'https://home.example/', home: LOUNGE}});
      return {...booted, rendererUnavailable: {reason: 'Tilo draws with WebGPU, which is turned off in this browser.', steps: []}};
    }});
    assert.equal(document.documentElement.dataset.entry, 'unsupported');
    assert.deepEqual(reports.map(report => [report.kind, report.message]),
      [['failed', 'unsupported: Tilo draws with WebGPU, which is turned off in this browser.']]);
    // A join that failed: the failure screen's cause.
    reports.length = 0;
    await fixture({boot: async ({deps, onStep}, booted) => {
      deps.onScope({scope: booted.router.route.target.scope, entry: {base: 'https://home.example/', home: LOUNGE}});
      onStep('join');
      throw Object.assign(new Error('that workspace is not public'), {name: 'HostAdmissionError', code: 'refused'});
    }});
    assert.equal(document.documentElement.dataset.failure, 'no-access');
    assert.deepEqual(reports.map(report => report.kind), ['failed']);
    assert.match(reports[0].message, /^no-access: HostAdmissionError: that workspace is not public/);
  } finally {
    globalThis.fetch = before;
  }
});

// A session's own status, as the peer publishes it (PeerSnapshot).
function sessionOf({phase = 'ready', transport = 'connected', admission = 'ready'} = {}) {
  return {phase, outgoingPending: 0, health: {version: 1, transport: {phase: transport}, admission: {phase: admission}, records: [], failures: []}};
}

test("the page fetches the apps the workspace's windows show, and nothing else", async () => {
  const r = await fixture();
  const {diagnostics, prefetched} = r;
  // A desktop still drawing asks for nothing: there is no window yet.
  diagnostics.onStatus({phase: 'gpu-pending', frames: 0, openApps: []});
  assert.deepEqual(prefetched, [], 'no fetch before the first frame is up');
  diagnostics.onStatus({phase: 'presented', frames: 1, openApps: ['so.tilo.notes', 'so.tilo.tasks']});
  assert.deepEqual(
    prefetched,
    [['so.tilo.notes', 'so.tilo.tasks']],
    "one fetch, for the apps the windows show",
  );
});

test('the bar follows the session after the mount, not the mount alone', async () => {
  const statuses = [];
  let session = sessionOf();
  const r = await fixture({boot: async (options, booted) => {
    booted.peer = {...booted.peer, mountDesktop: async () => booted.mount,
      getSnapshot: () => session, subscribe: () => () => {}, onStatus: listener => statuses.push(listener)};
    return booted;
  }});
  const {diagnostics} = r;
  const root = document.documentElement.dataset;
  assert.equal(statuses.length, 1, 'the page follows the session, not only the mount it made');
  diagnostics.onStatus({phase: 'presented', frames: 1});
  assert.equal(root.connection, undefined, 'a session that is well has nothing to say');
  // Nothing unmounted: the desktop is still there, the session is not.
  session = sessionOf({phase: 'incomplete', transport: 'offline'});
  statuses[0]();
  assert.equal(root.connection, 'offline', 'a transport that dropped after the mount is not connected');
  // The mount never changed: the line comes from the session alone.
  session = sessionOf();
  statuses[0]();
  assert.notEqual(root.connection, 'offline', 'and it clears once the transport is back');
});

test('an invited newcomer on an invite-only site stays in the site’s home workspace their invitation let them into, loading the page once', async () => {
  // A fresh browser that opened an invitation keeps its setup for later
  // (index.html) and joins the shared home workspace. The bare site shows the
  // first screen only to a browser that has joined nothing, and an invite-only
  // site holds it behind its entry: sending the newcomer there only opened
  // the home workspace again, which sent them back, without end.
  const before = globalThis.fetch;
  try {
    globalThis.fetch = directories({[ORIGIN]: [{workspace: 'current', name: 'Lounge', entry: 'directory/current/', invitationOnly: true,
      waitList: {contact: null, mail: true}}]});
    const home = {name: 'Lounge', host: {origin: ORIGIN}, admission: `${ORIGIN}/peer/v1/directory/current/`};
    const {calls, failure, at} = await fixture({entry: {base: 'https://web.example/', home},
      local: {'tilo-setup': JSON.stringify({step: 'later', at: 1}),
        'tilo-host-memberships': JSON.stringify({[ORIGIN]: {origin: ORIGIN, workspace: 'current', userPublicKey: USER}})}});
    assert.equal(failure, null);
    assert.deepEqual(calls.filter(call => call === 'reload' || call === 'assign'), [], 'the page is not loaded again');
    assert.notEqual(at(), 'https://web.example/', 'not sent to the bare site');
    assert.equal(document.documentElement.dataset.entry, 'open', 'the home workspace opens');
    assert.equal(JSON.parse(localStorage.getItem('tilo-setup')).step, 'later', 'their own setup still waits for later');
  } finally {
    globalThis.fetch = before;
  }
});

/** A page opened at a site's short invitation link into the Lounge, through
 * the real admission with the page's own deps: the invitation sent off the
 * wait list (`waitList`) or not. `steps` hears what the page asked. */
const shortInvitation = ({waitList, steps}) => async ({deps, onStep}, booted) => {
  const scope = {kind: 'sealed', site: 'https://web.example', handle: 'abcdefghijklmnop'};
  const home = {name: 'Lounge', host: {origin: ORIGIN}, admission: `${ORIGIN}/peer/v1/directory/current/`};
  deps.onScope({scope, entry: {base: 'https://web.example/', home}});
  onStep('join');
  const lounge = {...booted.peer, workspace: 'current', workspaceName: 'Lounge',
    enterOwnWorkspace: async () => { steps.push('make own'); return {workspace: OWN, workspace_id: 'own-id', name: 'Ada’s workspace'}; },
    dispose: async () => steps.push('dispose lounge')};
  const {peer} = await admit({scope, secrets: {invite: 'K'.repeat(43)}, runtime: {wayIn: () => 'sealed-invitation', wayInAfter: () => 'no-access', admittedAsAsked: () => true},
    deps: {...deps,
      openInvitation: async () => ({invitation: 'the invitation', host: ORIGIN}),
      inspectInvitation: async () => ({workspaceName: 'Lounge', role: 'editor', workspace: 'current', members: 3, waitList}),
      confirmInvitation: async () => true,
      joinWorkspace: async () => { steps.push('join lounge'); return lounge; },
      reconnect: async ({workspace}) => { steps.push(['enter', workspace]); return {...booted.peer, workspace}; }}});
  deps.onAdmitted(peer);
  booted.peer = peer;
  booted.router.route.target.scope = {kind: 'workspace', host: ORIGIN, workspace: peer.workspace};
  return booted;
};
const OWN = '0b1d8f52-5d0e-4a8e-9c1b-6f3e2a7d4c19';

test('a person invited off the wait list opens their own workspace, made as they come in, loading the page once', async () => {
  // Someone the Lounge let in off its wait list is new here: their own
  // workspace is where they start, and the Lounge stays one switch away.
  // Where to land is decided while they are let in, so nothing loads twice.
  const steps = [];
  const {calls, failure, at} = await fixture({remembered: [], boot: shortInvitation({waitList: true, steps}),
    local: {'tilo-setup': JSON.stringify({step: 'later', at: 1})}});
  assert.equal(failure, null);
  assert.deepEqual(steps, ['join lounge', 'make own', 'dispose lounge', ['enter', OWN]],
    'joined the Lounge, then their own workspace made and entered in its place');
  assert.deepEqual(calls.filter(call => call === 'reload' || call === 'assign'), [], 'the page is not loaded again');
  assert.equal(at(), null, 'the page goes nowhere');
  assert.equal(document.documentElement.dataset.entry, 'open', 'their own workspace opens');
  assert.deepEqual(chosenWorkspace(), {origin: ORIGIN, workspace: OWN}, 'a bare entry reopens it');
  assert.equal(enteredWorkspaces().find(kept => kept.workspace === OWN)?.name, 'Ada’s workspace', 'remembered under its own name');
  assert.ok(!enteredWorkspaces().some(kept => kept.workspace === OWN && kept.name === 'Lounge'), 'never under the Lounge’s');
});

test('a person invited into the Lounge by a short link opens the Lounge, loading the page once', async () => {
  // An ordinary invitation names its place, and the person stays there.
  const steps = [];
  const {calls, failure} = await fixture({remembered: [], boot: shortInvitation({waitList: false, steps}),
    local: {'tilo-setup': JSON.stringify({step: 'later', at: 1})}});
  assert.equal(failure, null);
  assert.deepEqual(steps, ['join lounge'], 'no workspace of their own is made');
  assert.deepEqual(calls.filter(call => call === 'reload' || call === 'assign'), [], 'the page is not loaded again');
  assert.equal(document.documentElement.dataset.entry, 'open', 'the Lounge opens');
  assert.equal(chosenWorkspace(), null, 'a bare entry opens the site’s home');
});

test('a person already set up keeps the workspace a link into the site’s own home opened', async () => {
  const home = {name: 'Lounge', host: {origin: ORIGIN}, admission: `${ORIGIN}/peer/v1/directory/current/`};
  const {failure, at} = await fixture({entry: {base: 'https://web.example/', home},
    local: {'tilo-setup': JSON.stringify({step: 'done'})}});
  assert.equal(failure, null);
  assert.notEqual(at(), 'https://web.example/',
    'a person who has been set up is not sent to the first screen again');
});

/** A person whose root is on another device: whom a pairing link names. */
const ELSEWHERE = 'bb'.repeat(32);
/** A site whose home workspace the server at `ORIGIN` keeps: that server is the site's own. */
const HOME_AT_ORIGIN = {name: 'Lounge', host: {origin: ORIGIN}};
/** A page that opened a pairing link for `person` into the workspace `current`
 * at the server `ORIGIN`, on a site whose home workspace is `home` (`null`:
 * the site names no server but the one its page came from). */
const pairingBoot = (person, home = null) => async ({deps}, page) => {
  deps.onScope({scope: {kind: 'workspace', host: ORIGIN, workspace: 'current'}, entry: {base: 'https://web.example/', home}});
  await deps.pairDevice({origin: ORIGIN, workspace: 'current', userPublicKey: person, invite: '0f'.repeat(16), listed: null});
  return page;
};

test('a new device the host turns away for its clock is told so, not that its link expired', async () => {
  // What a host answered a device whose clock ran ahead of its own, by 20 ms
  // or by an hour: the page read "expired" in it and blamed the link, which
  // nothing had happened to.
  const {failure, screen} = await fixture({boot: pairingBoot(ELSEWHERE),
    pair: async () => { throw new Error('certification request is expired or not yet valid'); }});
  assert.equal(failure?.code, 'pairing');
  assert.equal(document.documentElement.dataset.entry, 'refused');
  assert.equal(screen['arrival-failure-body'].textContent, PAIRING_CLOCK);
  assert.doesNotMatch(screen['arrival-failure-body'].textContent, /expired|used already/);
});

test('a device that came in by pairing is set up: it stays in its workspace and is never sent to the first screen', async () => {
  // A pairing link shows no first screen, so nothing marked a paired browser
  // as set up, and the page it opened handed itself to the bare site: which
  // opened the same workspace as the same device, and handed itself on again,
  // for as long as the tab was open.
  const home = {name: 'Lounge', host: {origin: ORIGIN}, admission: `${ORIGIN}/peer/v1/directory/current/`};
  const paired = {[ORIGIN]: {origin: ORIGIN, workspace: 'current', userPublicKey: USER, paired: true}};
  const {calls, failure, at} = await fixture({entry: {base: 'https://web.example/', home},
    local: {'tilo-host-memberships': JSON.stringify(paired)}});
  assert.equal(failure, null);
  assert.deepEqual(calls.filter(call => call === 'reload' || call === 'assign'), [],
    'the page stays in the workspace its link opened');
  assert.notEqual(at(), 'https://web.example/');
  assert.equal(JSON.parse(localStorage.getItem('tilo-setup'))?.step, 'done',
    'and the bare site, opened later, shows it no first screen either');
});

test('a pairing link opened in the browser that made it says this browser is that person already, and asks no host', async () => {
  // The new tab asked the host, the tab that made the link approved, and the
  // browser was kept as a device paired to its own person: its search then
  // offered "Pair a new device" no more. A new tab has not opened the person
  // (`identity`), so the browser is asked whom it keeps (`kept`); a tab that
  // has opened them knows.
  const joined = {[ORIGIN]: {origin: ORIGIN, workspace: 'current', userPublicKey: USER}};
  for (const who of [{identity: null, kept: {rootPublicKey: USER}}, {identity: {userPublicKey: USER}, kept: null}]) {
    const asked = [];
    const {failure, screen} = await fixture({...who, boot: pairingBoot(USER), local: {'tilo-host-memberships': JSON.stringify(joined)},
      pair: async options => { asked.push(options); return {workspace: 'current'}; }});
    assert.deepEqual(asked, [], 'the host is asked nothing: the link is not spent, and no tab asks to approve');
    assert.deepEqual(JSON.parse(localStorage.getItem('tilo-host-memberships')), joined,
      'the browser is kept as its own person, not as a device paired to them');
    assert.equal(failure?.code, 'pairing');
    assert.equal(document.documentElement.dataset.entry, 'refused');
    assert.equal(screen['arrival-failure-body'].textContent, PAIRING_SAME_PERSON);
  }
});

/** What a card of `kind` says: its heading and paragraphs in order, or `null` when none is shown. */
const cardWords = kind => document.body.children.find(card => card.dataset.card === kind)?.children.map(part => part.textContent) ?? null;

test('a device waiting to be approved says where: the device that made its link, and where “Pair a new device” is found', async () => {
  // The card said "Approve there if it shows this code" and not where there
  // is. Only the page that made a link asks about the device that answers it,
  // so that is where the person goes; and when nothing asks there, a new link
  // is what gets them on, from an action they are told where to find.
  let shown = null;
  const {failure, screen} = await fixture({boot: pairingBoot(ELSEWHERE), pair: async options => {
    // The host queued the request and said the code; the person then declines.
    options.onPairingCode('123456');
    shown = cardWords('pair-wait');
    throw new Error('declined by the approving device');
  }});
  assert.equal(document.documentElement.dataset.pairingCode, '123456');
  const [heading, where, code, ...rest] = shown;
  assert.equal(heading, 'Waiting for approval on your other device…');
  assert.equal(code, '123456');
  assert.match(where, /Approve there if it shows this code:$/);
  assert.match(where, /The device that made this link/, 'where to go');
  const otherwise = rest.join(' ');
  assert.match(otherwise, /“Pair a new device”/);
  assert.match(otherwise, /your picture, at the top right/, 'where “Pair a new device” is found: on the person’s own card');
  assert.match(otherwise, /in search/, 'and in the desktop’s search');
  assert.match(otherwise, /On a device where you are signed in/);
  assert.match(otherwise, /goes on by itself once you approve/);
  assert.equal(cardWords('pair-wait'), null, 'the card goes with the wait');
  assert.equal(failure?.code, 'pairing');
  assert.equal(screen['arrival-failure-body'].textContent, PAIRING_DECLINED);
});

test('a tab that waits to be approved without a link says where to approve it, and how to get a link when nothing asks', async () => {
  // A second tab of a paired browser is a device of its own, and asks the
  // host with no link: its request names no invite, so no page asks about it
  // by itself. It said to choose “Pair a new device” and approve, which makes
  // a new link and asks about nothing; it now says to open that link here.
  const home = {name: 'Lounge', host: {origin: ORIGIN}, admission: `${ORIGIN}/peer/v1/directory/current/`};
  let said = null;
  const {failure} = await fixture({
    join: async options => {
      options.onPairingCode('123456');
      said = document.getElementById('status').textContent;
      return globalThis.__entryPageFixture.page.peer;
    },
    boot: async ({deps}, page) => { await deps.enterViaHost({workspace: home}); return page; }});
  assert.equal(failure, null);
  assert.match(said, /^Pairing code 123456\./);
  assert.match(said, /approve 123456/);
  assert.match(said, /“Pair a new device”/);
  assert.match(said, /your picture, at the top right/, 'where “Pair a new device” is found: on the person’s own card');
  assert.match(said, /in search/, 'and in the desktop’s search');
  assert.match(said, /On a device where you are signed in/);
  assert.match(said, /open its link in this tab/);
});

test('a browser an earlier page paired to its own person opens as that person again', async () => {
  // What a pairing link opened in the browser that made it left behind, before
  // such a link was refused: the host kept as paired. The page then came up as
  // a device whose root is elsewhere, left the root it keeps unused, and
  // offered "Pair a new device" no more.
  const set = JSON.stringify({step: 'done'});
  const marked = {[ORIGIN]: {origin: ORIGIN, workspace: 'current', userPublicKey: USER, paired: true}};
  const own = await fixture({kept: {rootPublicKey: USER}, local: {'tilo-host-memberships': JSON.stringify(marked), 'tilo-setup': set}});
  assert.equal(own.failure, null);
  assert.deepEqual(JSON.parse(localStorage.getItem('tilo-host-memberships')), {[ORIGIN]: {origin: ORIGIN, workspace: 'current', userPublicKey: USER}},
    'the host is kept as one this browser joined as that person, whose root it holds');

  // A device paired to a person whose root is elsewhere stays their device,
  // whether this browser keeps another person or none.
  for (const kept of [{rootPublicKey: ELSEWHERE}, null]) {
    const device = await fixture({kept, local: {'tilo-host-memberships': JSON.stringify(marked), 'tilo-setup': set}});
    assert.equal(device.failure, null);
    assert.deepEqual(JSON.parse(localStorage.getItem('tilo-host-memberships')), marked, JSON.stringify(kept));
  }
});

test('the entry’s link field takes a pairing link and an invitation’s short link, and says so of anything else', async () => {
  // A person with no identity on a new device stands at the entry with the
  // link “Pair a new device” made on a device of theirs. The field read only
  // `invite`, so it answered “That isn’t an invite link”, as it did to the
  // short link the invite sheet hands over.
  const home = {name: 'Lounge', host: {origin: ORIGIN}, admission: `${ORIGIN}/peer/v1/directory/current/`};
  const turnedAway = () => fixture({entryParts: true, boot: async ({deps}) => {
    deps.onScope({scope: {kind: 'workspace', host: ORIGIN, workspace: 'current'}, entry: {base: 'https://web.example/', home}});
    throw Object.assign(new Error('join: HTTP 403'), {name: 'HostAdmissionError', code: 'invitation-only'});
  }});
  const paste = (screen, text) => {
    screen['arrival-invite-link'].value = text;
    screen['arrival-invite'].listeners.get('submit')({preventDefault() {}});
    return screen['arrival-invite-error'].textContent;
  };
  const pairing = `https://web.example/#/@node.example/w/current?pair=${ELSEWHERE}${'0f'.repeat(16)}`;
  // A sealed invitation's short link: its handle, and the key that opens it.
  const short = `https://web.example/i/AbCdEfGhIjKlMnOp#${'k'.repeat(43)}`;
  for (const link of [pairing, short, 'https://web.example/#/@node.example/w/current?invite=abc']) {
    const {screen, at, failure} = await turnedAway();
    assert.equal(failure, null);
    assert.equal(document.documentElement.dataset.refusal, 'invite');
    assert.equal(paste(screen, `  ${link}\n`), '', `taken: ${link}`);
    assert.equal(at(), link, 'and opened');
  }
  const {screen, at} = await turnedAway();
  for (const text of ['https://web.example/', 'hello', '']) {
    assert.equal(paste(screen, text), 'That isn’t an invite or pairing link. Paste the whole link.', text);
  }
  assert.equal(at(), null, 'nothing else is opened');
  assert.equal(screen['arrival-invite-toggle'].textContent, 'Have an invite or pairing link?');
  assert.equal(screen['arrival-invite-link'].placeholder, 'Paste an invite or pairing link');
  assert.match(screen['arrival-refusal-body'].textContent, /“Pair a new device”/, 'the entry says a pairing link is a way in');
});

/** What a card says, part by part, with the buttons under it by their words. */
const cardAndButtons = card => card.children.flatMap(part => (part.className === 'tilo-entry-actions'
  ? part.children.map(button => button.textContent) : [part.textContent]));

test('a device that follows a pairing link says whose device it becomes before it asks, and Stop pairs nothing', async () => {
  // A pairing link makes the device that opens it a device of whoever made
  // the link, and the device said nothing of whose it would be. It asked the
  // host at once and showed a code. Now it names that person first, as the
  // host that keeps them names them, and asks nobody until its person goes on.
  // The site's own server is the one taken at its word, and the card says so.
  const words = await import('./pairing.mjs');
  for (const [profile, sentence] of [
    [{display_name: 'Ada Lovelace'}, 'You are adding this device to Ada Lovelace’s devices. If that is not you, stop.'],
    [null, 'You are adding this device to the devices of someone this host has no name for. If you did not make this link yourself, stop.'],
  ]) {
    const asked = [];
    let shown = null;
    const {failure, screen} = await fixture({boot: pairingBoot(ELSEWHERE, HOME_AT_ORIGIN), identity: null, profile,
      whose: card => { shown = cardAndButtons(card); return 'stop'; },
      pair: async options => { asked.push(options); return {workspace: 'current'}; }});
    assert.deepEqual(shown, ['Add this device?', sentence, 'The server that adds it is this site’s own, node.example.',
      words.PAIRING_WHOSE.consequence, 'Add this device', 'Stop']);
    assert.deepEqual(asked, [], 'the host is asked nothing: the link is not spent, and nobody is asked to approve');
    assert.equal(localStorage.getItem('tilo-host-memberships'), null, 'this browser is kept as nobody’s device');
    assert.equal(failure?.code, 'pairing');
    assert.equal(document.documentElement.dataset.entry, 'refused');
    assert.equal(screen['arrival-failure-body'].textContent, words.PAIRING_STOPPED);
    assert.equal(cardWords('pair-whose'), null, 'the card goes with its answer');
  }
});

test('a device that waits to be approved says again whose it becomes, and its Stop leaves with nothing paired', async () => {
  const words = await import('./pairing.mjs');
  const sentence = 'You are adding this device to Ada Lovelace’s devices. If that is not you, stop.';
  let said = null;
  let waiting = null;
  const letGo = [];
  // This browser holds a person of its own (`identity`), and the link names another.
  const server = 'The server that adds it is this site’s own, node.example.';
  const {failure, screen, calls, at} = await fixture({boot: pairingBoot(ELSEWHERE, HOME_AT_ORIGIN), profile: {display_name: 'Ada Lovelace'},
    whose: card => { said = cardAndButtons(card); return 'go'; },
    pair: async options => {
      options.onPairingCode('123456');
      const card = document.body.children.find(part => part.dataset.card === 'pair-wait');
      waiting = card.children.map(part => part.textContent);
      // The person stops. The device that made the link approves all the same.
      card.children.find(part => part.textContent === 'Stop').listeners.get('click')();
      return {workspace: 'current', dispose: async () => letGo.push('device')};
    }});
  assert.deepEqual(said, ['Add this device?', sentence, server, words.PAIRING_WHOSE.consequence, words.PAIRING_WHOSE.already, 'Add this device', 'Stop'],
    'a browser that is somebody already is told it would act as another here');
  assert.deepEqual(waiting.slice(0, 5), ['Waiting for approval on your other device…', words.PAIRING_WAIT.where, '123456', sentence, server],
    'while it waits it says again whose it becomes, and which server adds it');
  assert.deepEqual(calls.filter(call => call === 'assign' || call === 'reload'), ['assign', 'reload'],
    'the page leaves, so the host hears its request go and nobody can approve it any more');
  assert.equal(at(), 'https://web.example/', 'for the bare site: the link is gone from the address');
  assert.deepEqual(letGo, ['device'], 'the device an approval made meanwhile is let go');
  assert.equal(localStorage.getItem('tilo-host-memberships'), null, 'and this browser is kept as nobody’s device');
  assert.equal(failure?.code, 'pairing');
  assert.equal(screen['arrival-failure-body'].textContent, words.PAIRING_STOPPED);
});

test('only the server a site names is its own, and a device says which server adds it', async () => {
  // The words the cards above are made of, and the rule that says whose a
  // server is. (pairing.test.mjs is not among the suites the build server
  // takes, so what decides a card is held here, where it is.)
  const {ofThisSite, pairingWhere, pairingWhose} = await import('./pairing.mjs');
  const site = {page: 'https://web.example/app/#/w/current', entry: {base: 'https://web.example/', home: {host: {origin: 'https://node.example'}}}};
  assert.equal(ofThisSite('https://web.example', site), true, 'the server the page came from');
  assert.equal(ofThisSite('https://node.example/', site), true, 'the server that keeps the site’s home workspace');
  // A link names any server: one that looks alike, another port, plain
  // http, or nothing that reads as a server at all.
  for (const other of ['https://elsewhere.example', 'https://node.example.elsewhere.example', 'https://node.example:8443', 'http://node.example',
    'node.example', 'about:blank', '', null, undefined]) {
    assert.equal(ofThisSite(other, site), false, String(other));
  }
  assert.equal(ofThisSite('https://node.example', {page: 'https://web.example/', entry: {base: 'https://web.example/', home: null}}), false,
    'a site without a home names itself alone');
  assert.equal(ofThisSite('https://node.example', {page: 'https://web.example/'}), false);
  assert.equal(ofThisSite('https://node.example', {page: 'about:blank', entry: {base: 'about:blank'}}), false);

  assert.equal(pairingWhere('https://node.example', {ofSite: true}), 'The server that adds it is this site’s own, node.example.');
  const other = 'which is not this site’s own. What you type here afterwards goes to that server, not to this site. Go on only if you know that server is yours.';
  assert.equal(pairingWhere('https://node.example:8443', {ofSite: false}), `This device would join another server, node.example:8443, ${other}`);
  assert.equal(pairingWhere('https://node.example'), `This device would join another server, node.example, ${other}`,
    'a caller that did not say is told it is another’s');
  assert.equal(pairingWhere('https://node.example', {ofSite: 'yes'}), `This device would join another server, node.example, ${other}`);
  // A name spelled with a letter of another script reads as its address writes it.
  assert.match(pairingWhere('https://nod\u0435.example'), /^This device would join another server, xn--[a-z0-9-]+\.example, which is not/);
  assert.equal(pairingWhere('nowhere', {ofSite: true}), `This device would join another server, ${other}`);
  // A server the site does not name as its own names nobody, whatever it
  // answered, and so does a caller that did not say whose the server is.
  for (const whose of [undefined, {}, {ofSite: false}, {ofSite: 'yes'}, {ofSite: 1}]) {
    for (const name of ['Ada Lovelace', '', null]) {
      assert.equal(pairingWhose(name, whose),
        'You are adding this device to the devices of whoever made this link. If you did not make it yourself, stop.');
    }
  }
  assert.equal(pairingWhose('Ada Lovelace', {ofSite: true}), 'You are adding this device to Ada Lovelace’s devices. If that is not you, stop.');
});

test('a pairing link that names a server this site does not name as its own puts no name on the card, and the card says which server it is', async () => {
  // Whoever writes a link writes the server it names (`#/@<server>/w/…`),
  // and the card asked that server who the link's person is. A server of the
  // link's maker answers any name for the key, its reader's own too, and the
  // card said it under this site's address as if the site knew that person.
  const words = await import('./pairing.mjs');
  const asked = [];
  const paired = [];
  let shown = null;
  // The site is web.example and names no server but itself; the link names node.example.
  const boot = async ({deps}, page) => {
    globalThis.__entryPageFixture.findProfile = async (person, origins) => {
      asked.push(origins);
      return {version: 1, user_public_key: person, display_name: 'Bo Smith'};
    };
    deps.onScope({scope: {kind: 'workspace', host: ORIGIN, workspace: 'current'}, entry: {base: 'https://web.example/', home: null}});
    await deps.pairDevice({origin: ORIGIN, workspace: 'current', userPublicKey: ELSEWHERE, invite: '0f'.repeat(16), listed: null});
    return page;
  };
  const {failure, screen} = await fixture({boot, identity: null,
    whose: card => { shown = cardAndButtons(card); return 'stop'; },
    pair: async options => { paired.push(options); return {workspace: 'current'}; }});
  assert.ok(shown, 'the card is shown');
  assert.ok(!shown.join('\n').includes('Bo Smith'), `the card names a person on the word of the link’s own server alone: “${shown[1]}”`);
  assert.deepEqual(asked, [], 'a server this site does not name is asked nothing before the person goes on');
  assert.deepEqual(shown, ['Add this device?',
    'You are adding this device to the devices of whoever made this link. If you did not make it yourself, stop.',
    'This device would join another server, node.example, which is not this site’s own. What you type here afterwards goes to that server, not to this site. Go on only if you know that server is yours.',
    words.PAIRING_WHOSE.consequence, 'Add this device', 'Stop']);
  assert.deepEqual(paired, [], 'Stop asks that server nothing either');
  assert.equal(failure?.code, 'pairing');
  assert.equal(screen['arrival-failure-body'].textContent, words.PAIRING_STOPPED);
});

test('a tab its person signed in with their agent pairs without asking whose device it becomes; a link handed to it is asked about', async () => {
  // "Sign in with your agent": the tab showed a code, the person's computer
  // claimed it with `tilo login`, and the tab opened the pairing link the
  // claim named. Its card then warned of someone it had no name for, and of
  // a link the reader did not make, about a link this page made for
  // its own person a moment before.
  const words = await import('./pairing.mjs');
  const invite = '0f'.repeat(16);
  const asked = [];
  let shown = null;
  let waiting = null;
  const own = await fixture({boot: pairingBoot(ELSEWHERE, HOME_AT_ORIGIN), session: {'tilo-own-pairing': words.pairSecret(ELSEWHERE, invite)},
    whose: card => { shown = cardAndButtons(card); return 'stop'; },
    pair: async options => {
      asked.push(options.userPublicKey);
      options.onPairingCode('123456');
      waiting = {own: cardWords('pair-own'), other: cardWords('pair-wait')};
      return {workspace: 'current'};
    }});
  assert.equal(shown, null, 'no card asks whose device this becomes');
  assert.deepEqual(asked, [ELSEWHERE], 'the server is asked at once');
  assert.deepEqual(waiting, {own: ['Signing you in…'], other: null},
    'while it goes through the tab says it signs its person in: no code, and no other device that asks');
  assert.equal(cardWords('pair-own'), null, 'and the card goes with it');
  assert.equal(own.failure, null);
  assert.equal(own.session.get('tilo-own-pairing'), undefined, 'what the tab kept is read once');

  // The same tab handed a link by someone else, or one for another invite or
  // person: the card and its warning, as before.
  for (const session of [{}, {'tilo-own-pairing': words.pairSecret(ELSEWHERE, 'aa'.repeat(16))}, {'tilo-own-pairing': words.pairSecret(USER, invite)}]) {
    shown = null;
    const paired = [];
    const handed = await fixture({boot: pairingBoot(ELSEWHERE, HOME_AT_ORIGIN), session,
      whose: card => { shown = cardAndButtons(card); return 'stop'; },
      pair: async options => { paired.push(options); return {workspace: 'current'}; }});
    assert.match(shown?.[1] ?? '', /^You are adding this device to the devices of someone .* no name for\. If you did not make this link yourself, stop\.$/,
      JSON.stringify(session));
    assert.deepEqual(paired, [], 'nobody is asked before the person goes on');
    assert.equal(handed.screen['arrival-failure-body'].textContent, words.PAIRING_STOPPED);
  }
});

test('the pairing sheet hands its link to Tilo Assistant by an address of the app, on the devices that app is for', () => {
  const link = `https://web.example/#/@server.example:8443/w/1f0e?pair=${USER}${'0f'.repeat(16)}`;
  const mac = {userAgent: 'Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/26.0 Safari/605.1.15'};
  const address = assistantAddress(link, mac);
  assert.equal(address, `tilo-assistant://pair?link=${encodeURIComponent(link)}`);
  // The address is one word: nothing of the link ends it or reads as a part of its own.
  assert.doesNotMatch(address.slice('tilo-assistant://pair?link='.length), /[#?&=\/:@ ]/);
  const read = new URL(address);
  assert.equal(read.protocol, 'tilo-assistant:');
  assert.equal(read.searchParams.get('link'), link);
  assert.deepEqual([...read.searchParams.keys()], ['link']);
  const phone = {userAgent: 'Mozilla/5.0 (iPhone; CPU iPhone OS 26_0 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/26.0 Mobile/15E148 Safari/604.1'};
  assert.equal(assistantAddress(link, phone), address);
  assert.equal(assistantAddress(link, {userAgentData: {platform: 'macOS', brands: []}}), address);
  // There is no Tilo Assistant for these, so nothing is offered that would lead nowhere.
  for (const agent of ['Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 Chrome/140.0 Safari/537.36',
    'Mozilla/5.0 (X11; Linux x86_64; rv:140.0) Gecko/20100101 Firefox/140.0',
    'Mozilla/5.0 (Linux; Android 16; Pixel 9) AppleWebKit/537.36 Chrome/140.0 Mobile Safari/537.36', '']) {
    assert.equal(assistantAddress(link, {userAgent: agent}), null, agent);
  }
  // Only a web address is handed over: the app pairs with a link, and takes nothing else.
  for (const other of [undefined, null, '', 'javascript:alert(1)', 'tilo-assistant://pair?link=x', 'file:///tmp/a-link', ' https://web.example/']) {
    assert.equal(assistantAddress(other, mac), null, String(other));
  }
});

// ---- Adding a device from a person's own desktop ---------------------------

/** A person whose root this browser holds, as the page reads them
 * (`currentIdentity`): not kept yet, so nothing of theirs is published. */
const rootHolder = (userPublicKey = USER) => ({userPublicKey, kept: false, keepWithPasskey: async () => {},
  createRecoveryKey: async () => 'recovery', revokeDevice: async () => {}});

/** A link as the site's grammar writes it: a workspace at its server, or the person's own desktop. */
const siteLink = {format: ({target: {scope}}) => (scope.kind === 'own' ? 'https://web.example/#/me'
  : `https://web.example/#/@${new URL(scope.host).hostname}/w/${scope.workspace}`)};

/**
 * The site's own server as a page with no session there reaches its pairing
 * (the runtime's pairing reach, at the address the server publishes): the
 * devices `waiting()` says wait there, and every reach, question and answer,
 * with the root each came with. `session()` is that reach as a workspace's
 * peer hands it out.
 */
function homeServer(waiting = () => []) {
  const asked = [];
  class Pairing {
    constructor(address) { asked.push(['reach', address]); }
    async pending(root) { asked.push(['pending', root?.userPublicKey]); return JSON.stringify(waiting()); }
    async answer(root, digest, approved) {
      asked.push(['answer', root?.userPublicKey, digest, approved]);
      return approved ? '{"cert":{}}' : null;
    }
    async close() { asked.push(['close']); }
  }
  return {asked, count: what => asked.filter(([said]) => said === what).length, session: () => new Pairing('session'),
    runtime: {PairingHost: Pairing, hostInfo: async () => JSON.stringify({address: {id: 'home-server'}}), qrSvg: () => '<svg/>'}};
}

/**
 * The page's repeating and late timers, held instead of started, so a test
 * drives the pairing's poll itself (`poll(ms)`: every held timer of that
 * period, once) and nothing outlives it. `release()` gives the clock back.
 */
function heldTimers() {
  const real = {setInterval: globalThis.setInterval, clearInterval: globalThis.clearInterval, setTimeout: globalThis.setTimeout, now: Date.now};
  const every = new Map();
  let next = 1;
  let ahead = 0;
  globalThis.setInterval = (fire, ms) => { const id = `held-${next++}`; every.set(id, {fire, ms}); return id; };
  globalThis.clearInterval = id => { if (!every.delete(id)) real.clearInterval(id); };
  globalThis.setTimeout = (fire, ms, ...rest) => (ms >= 1000 ? `late-${next++}` : real.setTimeout(fire, ms, ...rest));
  Date.now = () => real.now() + ahead;
  return {
    poll: async ms => { for (const held of [...every.values()]) if (held.ms === ms) held.fire(); await settled(); },
    later: ms => { ahead += ms; },
    release() {
      Object.assign(globalThis, {setInterval: real.setInterval, clearInterval: real.clearInterval, setTimeout: real.setTimeout});
      Date.now = real.now;
    },
  };
}

/** The person chooses the desktop's action `id`, which its page carries out. */
const choose = (diagnostics, id, value) => diagnostics.onHostAction(id, value);
/** A page that opened the pairing link naming `named`: its own way in pairs it. */
const following = named => async ({deps}, page) => {
  deps.onScope({scope: {kind: 'workspace', host: named.origin, workspace: named.workspace}, entry: HOME_ENTRY});
  await deps.pairDevice({...named, listed: null});
  return page;
};
const settled = async () => { for (let turn = 0; turn < 4; turn++) await tick(); };
/** How often a page with an open pairing link asks its server who waits (`PAIR_POLL_MS`). */
const PAIR_POLL = 3000;
const card = kind => document.body.children.find(shown => shown.dataset.card === kind) ?? null;
const pairingLinkOf = sheet => sheet.children.find(part => part.name === 'pairing-link').value;
/** What a pairing link names: the server and workspace it opens, the person and the invite. */
const namedBy = link => {
  const [, server, workspace, secret] = /#\/@([^/]+)\/w\/([^?]+)\?pair=([0-9a-f]+)$/.exec(link);
  return {origin: `https://${server}`, workspace, ...parsePairSecret(secret)};
};
const waitingDevice = (digest, invite) => ({digest, pairingCode: '123456', label: 'Safari on iPhone', device: 'ee'.repeat(32),
  invite, requestedAtMs: Date.now()});
/** A site whose home workspace, `lounge`, the server at `ORIGIN` keeps. */
const HOME_ENTRY = {base: 'https://web.example/', home: {...HOME_AT_ORIGIN, workspace: 'lounge'}};
/** A person's own desktop on that site, in a browser that holds `identity`;
 * `chosen` is the workspace a bare entry opens for them, when they chose one:
 * one this browser entered, as a choice is kept only while it remembers that. */
const ownDesktop = ({identity = rootHolder(), server, chosen = null, entry = HOME_ENTRY, ...rest}) => fixture({own: true, identity, entry,
  ...(chosen ? {remembered: [{origin: ORIGIN, workspace: chosen, hostDevice: 'cd'.repeat(32), name: 'Mine'}]} : {}),
  runtime: server.runtime, ...rest, boot: async (_options, page) => {
    if (chosen) chooseWorkspace({id: `open-workspace:${chosen}`, name: 'Mine', origin: ORIGIN, workspace: chosen});
    page.link = siteLink;
    return page;
  }});

test('on a person’s own desktop Add a device opens the pairing sheet, with a link another device of theirs joins with', async () => {
  // Settings' Add a device asks the session for the sheet, and the desktop
  // hands the page the action `pair-device`. On a person's own desktop
  // no workspace's peer is in front, and the page offered no pairing there:
  // the press opened nothing and said nothing. A device joins the person, not
  // a workspace: their home server carries the request, and their root answers.
  const timers = heldTimers();
  try {
    let waiting = [];
    const server = homeServer(() => waiting);
    const {diagnostics, shown, failure} = await ownDesktop({server, chosen: 'mine'});
    assert.equal(failure, null);
    assert.ok(shown.flat().some(action => action.id === 'pair-device'), 'the own desktop offers pairing, in search and on the person’s card');
    choose(diagnostics, 'pair-device');
    await settled();
    assert.equal(card('pair-link')?.children[0].textContent, 'Pair a new device', 'the sheet opens');
    const named = namedBy(pairingLinkOf(card('pair-link')));
    assert.deepEqual({origin: named.origin, workspace: named.workspace, userPublicKey: named.userPublicKey},
      {origin: ORIGIN, workspace: 'mine', userPublicKey: USER}, 'the link opens the person’s workspace on their home server, as them');
    assert.match(named.invite, /^[0-9a-f]{32}$/, 'and names a fresh invite');
    assert.deepEqual(server.asked, [['reach', JSON.stringify({id: 'home-server'})], ['pending', USER]],
      'the root asks its home server who waits, at the address that server publishes');

    // The device that opened the link waits there under its invite: this page
    // asks its person, with the code the device shows, and answers nothing itself.
    waiting = [waitingDevice('d1', named.invite)];
    await timers.poll(PAIR_POLL);
    const ask = card('pair-ask');
    assert.ok(ask, 'the person is asked about the device that answered the link');
    assert.ok(cardAndButtons(ask).includes('123456'), 'with the six digits the device shows');
    assert.equal(server.count('answer'), 0, 'nothing is let in before the person approves');
    const approve = ask.children.find(part => part.className === 'tilo-entry-actions').children.find(button => button.textContent === 'Approve 123456');
    await approve.listeners.get('click')();
    assert.deepEqual(server.asked.filter(([what]) => what === 'answer'), [['answer', USER, 'd1', true]], 'the root this page holds answers, once');
    assert.ok(cardAndButtons(card('pair-ask')).includes('Safari on iPhone is now one of your devices'));

    // Another device of theirs follows that link: the page's own way in pairs
    // it through the server the link names, as the person and invite it names.
    const joined = [];
    const device = await fixture({identity: null, pair: async options => { joined.push(options); return {workspace: named.workspace}; },
      boot: following(named)});
    assert.equal(device.failure, null);
    assert.deepEqual(joined.map(options => [options.origin, options.workspace, options.userPublicKey, options.invite]),
      [[ORIGIN, 'mine', USER, named.invite]]);
  } finally {
    timers.release();
  }
});

test('the own desktop’s pairing link opens a workspace of the home server this browser knows, and where it knows none no sheet opens', async () => {
  const timers = heldTimers();
  try {
    for (const [remembered, entry, workspace] of [
      // The one this browser entered on that server.
      [[], HOME_ENTRY, 'other'],
      // Else the site's home workspace itself.
      [null, HOME_ENTRY, 'lounge'],
      // A site that names no workspace of its home, in a browser that kept none: nowhere for a device to land.
      [null, {base: 'https://web.example/', home: HOME_AT_ORIGIN}, null],
    ]) {
      const server = homeServer();
      const {diagnostics} = await ownDesktop({server, remembered, entry});
      choose(diagnostics, 'pair-device');
      await settled();
      assert.equal(card('pair-link') ? namedBy(pairingLinkOf(card('pair-link'))).workspace : null, workspace);
      if (!workspace) assert.deepEqual(server.asked, [], 'and the server is asked nothing');
    }
  } finally {
    timers.release();
  }
});

test('a page that is not one of the person’s root-holding devices opens no pairing sheet', async () => {
  // Only the root signs a new device's certificate. A browser that was itself
  // paired holds no root, on the person's own desktop as in a workspace, and
  // a browser that holds another person's root is not this person: neither
  // shows a link, and neither asks the server who waits.
  const timers = heldTimers();
  try {
    for (const page of [
      {own: true, identity: {userPublicKey: USER}},
      {own: false, identity: {userPublicKey: USER}},
      {own: false, identity: rootHolder(ELSEWHERE)},
    ]) {
      const server = homeServer(() => [waitingDevice('d1', '0f'.repeat(16))]);
      const {diagnostics, shown} = await fixture({...page, entry: HOME_ENTRY, runtime: server.runtime,
        member: {pairing: () => server.session()}});
      assert.ok(!shown.flat().some(action => action.id === 'pair-device'), 'pairing is not offered');
      choose(diagnostics, 'pair-device');
      await settled();
      assert.equal(card('pair-link'), null, 'no sheet, no link');
      assert.equal(card('pair-ask'), null, 'and nobody is asked about a device');
      assert.deepEqual(server.asked, [], 'the server is asked nothing');
    }
  } finally {
    timers.release();
  }
});

test('a spent invitation and an ended one let nobody in from the own desktop: no device naming either is asked about', async () => {
  const timers = heldTimers();
  try {
    let waiting = [];
    const server = homeServer(() => waiting);
    const {diagnostics} = await ownDesktop({server, chosen: 'mine'});
    choose(diagnostics, 'pair-device');
    await settled();
    const spent = namedBy(pairingLinkOf(card('pair-link'))).invite;
    waiting = [waitingDevice('d1', spent)];
    await timers.poll(PAIR_POLL);
    const decline = card('pair-ask').children.find(part => part.className === 'tilo-entry-actions').children.find(button => button.textContent === 'Decline');
    await decline.listeners.get('click')();
    assert.deepEqual(server.asked.filter(([what]) => what === 'answer'), [['answer', USER, 'd1', false]]);
    // Answered, the invite is spent, approved or not: a second device naming it is never put to the person.
    waiting = [waitingDevice('d2', spent)];
    const before = server.count('pending');
    await timers.poll(PAIR_POLL);
    await timers.poll(PAIR_POLL);
    assert.equal(server.count('pending'), before, 'the server is not even asked who waits: no invite of this page is open');
    assert.equal(server.count('answer'), 1, 'and nothing more is answered');
    assert.ok(cardAndButtons(card('pair-ask')).includes('Declined. Safari on iPhone was not let in.'), 'no new ask took the first one’s place');

    // A link nobody answered ends ten minutes after it was made: its sheet
    // closes, and a device that names it afterwards is not asked about either.
    choose(diagnostics, 'pair-device');
    await settled();
    const ended = namedBy(pairingLinkOf(card('pair-link'))).invite;
    assert.notEqual(ended, spent, 'each press makes a fresh invite');
    timers.later(10 * 60 * 1000);
    waiting = [waitingDevice('d3', ended)];
    const asks = server.count('pending');
    await timers.poll(PAIR_POLL);
    assert.equal(card('pair-link'), null, 'the ended link’s sheet closes');
    assert.equal(server.count('pending'), asks);
    assert.equal(server.count('answer'), 1);
  } finally {
    timers.release();
  }
});

test('a device approved through the own desktop’s link is the device of the person that link names, and of nobody else', async () => {
  const timers = heldTimers();
  try {
    let waiting = [];
    const server = homeServer(() => waiting);
    const {diagnostics} = await ownDesktop({server, chosen: 'mine'});
    choose(diagnostics, 'pair-device');
    await settled();
    const named = namedBy(pairingLinkOf(card('pair-link')));
    // A device waiting under an invite this page never made (another person's
    // link, or none) is not this page's to approve, whatever the server lists.
    waiting = [waitingDevice('d9', '0e'.repeat(16)), {...waitingDevice('d8', null)}];
    await timers.poll(PAIR_POLL);
    assert.equal(card('pair-ask'), null);
    assert.equal(server.count('answer'), 0);
    assert.ok(server.asked.every(([what, root]) => !['pending', 'answer'].includes(what) || root === USER),
      'every question to the server came with this person’s root and no other');
    // The browser that follows the link keeps itself as that person's device alone.
    await fixture({identity: null, pair: async () => ({workspace: named.workspace}), boot: following(named)});
    const kept = keptMembership(HOME_AT_ORIGIN);
    assert.deepEqual([kept?.userPublicKey, kept?.paired], [USER, true]);
  } finally {
    timers.release();
  }
});

/** The link a signed-in tab formats for the workspace it opens. */
const signedInLink = {format: ({target}) => `https://web.example/#/@node.example/w/${target.scope.workspace}`};
const INVITE = 'e'.repeat(32);

test('a tab an agent signs in pairs on its own page and loads, once, the workspace the person last used', async () => {
  globalThis.fetch = async () => ({ok: false});
  const {module, calls, at, session, signed} = await fixture();
  const before = calls.length;
  await module.signInAs({user: USER, invite: INVITE}, {origin: ORIGIN, home: 'lounge'}, signedInLink, signed());
  const made = calls.slice(before);
  assert.deepEqual(made.filter(call => Array.isArray(call) && call[0] === 'pair'), [['pair', USER, INVITE]],
    'the tab pairs itself as the person the claim names, with the invite it named');
  assert.ok(!made.some(call => Array.isArray(call) && call[0] === 'own'),
    'a workspace this browser remembers is not asked of the host again');
  assert.deepEqual(made.filter(call => call === 'dispose'), ['dispose'],
    'the pairing session is let go before the page loads');
  assert.equal(made.filter(call => call === 'reload').length, 1, 'the sign-in loads one page');
  assert.ok(!made.includes('assign'), 'and has nothing to navigate to a second time');
  assert.equal(at(), 'https://web.example/#/@node.example/w/current',
    'the workspace the person last used, not the home workspace it paired through');
  assert.equal(session.get('tilo-open-own-workspace'), undefined,
    'the loaded page has no workspace of its own to open after drawing this one');
});

test('a tab an agent signs in with only the home remembered loads the person’s own workspace, never the home', async () => {
  globalThis.fetch = async () => ({ok: false});
  const {module, calls, at, session, signed} = await fixture({remembered: null});
  const before = calls.length;
  // The one workspace this browser remembers on that server is the home it
  // was let into: a sign-in opens the person's own there instead.
  await module.signInAs({user: USER, invite: INVITE}, {origin: ORIGIN, home: 'current'}, signedInLink, signed());
  const made = calls.slice(before);
  assert.deepEqual(made.filter(call => Array.isArray(call) && call[0] === 'own'), [['own']],
    'the home workspace is no place to sign in to: the host is asked for the person’s own');
  assert.equal(made.filter(call => call === 'reload').length, 1, 'the sign-in loads one page');
  assert.equal(at(), 'https://web.example/#/@node.example/w/mine', 'and lands on the person’s own workspace');
  assert.equal(session.get('tilo-open-own-workspace'), undefined, 'nothing is left to open after it');
});
