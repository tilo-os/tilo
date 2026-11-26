import test from 'node:test';
import assert from 'node:assert/strict';
import {readFile} from 'node:fs/promises';
import {chosenWorkspace, enteredWorkspaces, joinViaHost} from './workspaces.mjs';
import {AdmissionError, admit} from './admission.mjs';
import {assistantAddress, PAIRING_CLOCK, PAIRING_DECLINED, PAIRING_SAME_PERSON} from './pairing.mjs';
import {runtimeFake} from './runtime-surface.mjs';
import {createFirstScreen} from './first-screen.mjs';

const USER = 'aa'.repeat(32);
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
  profile = null, whose = 'go', runtime: answers = {}, firstScreen = null} = {}) {
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
  globalThis.__entryPageFixture = {page, runtime, mounted: [], prepared, prefetched: [], identity, kept, recoveryShown: true, prepareGpu: () => prepared.push(document.documentElement.dataset.arrival),
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
  if (firstScreen) {
    globalThis.__entryPageFixture.firstScreen = firstScreen;
    source = source.replace("import {firstScreen, openSignIn, setupDone} from './first-screen.mjs';",
      "import {openSignIn, setupDone} from './first-screen.mjs';\nconst firstScreen = globalThis.__entryPageFixture.firstScreen;");
  }
  source = source
    .replace("import {boot, grammar} from './entry.mjs';", 'const boot = globalThis.__entryPageFixture.boot, grammar = () => globalThis.__entryPageFixture.page.link;')
    .replace("import {loadRuntime} from './host.mjs';", 'const loadRuntime = async () => globalThis.__entryPageFixture.runtime;')
    .replace(/import \{([^}]*)\bstartArrival, ([^}]*)\} from '.\/arrival.mjs';/, "import {$1$2} from './arrival.mjs';\nconst startArrival = () => {};")
    .replace("import {browserIdentity, browserPerson, currentIdentity, keepRecoveryWordsShown, keptIdentity, recoveryWords, recoveryWordsShown} from './identity.mjs';", 'const currentIdentity = () => globalThis.__entryPageFixture.identity;\nconst browserPerson = async () => globalThis.__entryPageFixture.identity;\nconst keptIdentity = async () => globalThis.__entryPageFixture.kept;\nconst recoveryWords = identity => identity.recoveryWords;\nconst recoveryWordsShown = async () => globalThis.__entryPageFixture.recoveryShown;\nconst keepRecoveryWordsShown = async () => {};')
    .replace('hostToReconnect, joinViaHost,', 'hostToReconnect,')
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
test('the personal workspace link admits this existing device to its own workspace', async () => {
  let asks = 0;
  const {failure, at} = await fixture({search: '?own-workspace=1', member: {
    host: {hostCert: {device_public_key: Array(32).fill(0xcd), capabilities: {can_own_workspaces: true}}},
    enterOwnWorkspace: async options => {
      assert.equal(options.name, undefined, 'the existing workspace is requested');
      asks++;
      return {workspace: STUDIO, name: 'Private'};
    },
  }, boot: async ({deps}, page) => {
    await deps.onAdmitted(page.peer);
    assert.equal(await deps.holdFirstScreen({...page, workspace: 'current'}), true);
  }});
  assert.ifError(failure);
  assert.equal(asks, 1);
  assert.equal(at(), 'https://web.example/w/' + STUDIO);
  assert.deepEqual(chosenWorkspace(), {origin: ORIGIN, workspace: STUDIO});
});

test('New workspace waits for a name and reuses the signed create after a lost answer', async () => {
  const asks = [];
  const hosted = {user_public_key: Array(32).fill(170), device_public_key: Array(32).fill(0xcd),
    capabilities: {can_own_workspaces: true}};
  const {failure, at, session} = await fixture({search: '?new-workspace=1', member: {
    host: {hostCert: hosted},
    enterOwnWorkspace: async args => {
      asks.push(args);
      if (asks.length === 1) throw Object.assign(new Error('No answer'), {code: 'unanswered'});
      return {workspace: STUDIO, name: args.name};
    },
  }, boot: async ({deps}, page) => {
    await deps.onAdmitted(page.peer);
    const here = {...page, url: 'https://web.example/w/current', workspace: 'current'};
    const cancelled = deps.holdFirstScreen(here);
    await tick();
    const first = document.querySelector('[data-card="new-workspace"]');
    assert.ok(first, 'the admitted device is asked before making a workspace');
    const cancel = first.children.at(-1).children.find(button => button.textContent === 'Cancel');
    cancel.listeners.get('click')();
    assert.equal(await cancelled, false);
    assert.equal(asks.length, 0);
    assert.equal(chosenWorkspace(), null);
    const pending = deps.holdFirstScreen(here);
    await tick();
    const form = document.querySelector('[data-card="new-workspace"]');
    assert.ok(form, 'the admitted device has a name form');
    assert.equal(asks.length, 0);
    const [label, error] = form.children;
    label.children[0].value = 'Studio';
    const submit = () => form.listeners.get('submit')({preventDefault() {}});
    await submit();
    assert.equal(error.hidden, false, 'a lost reply leaves the form open for retry');
    assert.equal(chosenWorkspace(), null);
    await submit();
    assert.equal(await pending, true);
    return null;
  }});
  assert.ifError(failure);
  assert.equal(asks.length, 2);
  assert.equal(asks[0].name, 'Studio');
  assert.match(asks[0].invocation, /^[a-f0-9-]{36}$/);
  assert.equal(asks[0].invocation, asks[1].invocation);
  assert.equal(at(), 'https://web.example/w/' + STUDIO);
  assert.deepEqual(chosenWorkspace(), {origin: ORIGIN, workspace: STUDIO});
  assert.equal(enteredWorkspaces().find(kept => kept.workspace === STUDIO)?.name, 'Studio');
  assert.deepEqual(JSON.parse(session.get('tilo-page-workspace')), {workspace: STUDIO, name: 'Studio'});
});

test('a browser on a host that offers no personal workspaces cannot start a workspace from an entry link', async () => {
  let calls = 0;
  const {failure} = await fixture({search: '?new-workspace=1', member: {enterOwnWorkspace: async () => ++calls},
    boot: async ({deps}, page) => {
      await deps.onAdmitted(page.peer);
      assert.equal(await deps.holdFirstScreen({...page, workspace: 'current'}), false);
      assert.equal(document.querySelector('[data-card="new-workspace"]'), null);
      return null;
    }});
  assert.ifError(failure);
  assert.equal(calls, 0);
});

test('the first screen mints, resumes and approves the agent ticket without changing either certificate', async () => {
  let make;
  let resume;
  const firstScreen = {reveal() {}, whenDone() {}, offerSetup(maker, resumer) { make = maker; resume = resumer; }};
  const identity = {userPublicKey: USER};
  const asked = '{"cert":{"issued_at_ns":1792023733547123457},"digest":[123]}';
  const approved = '{"cert":{"issued_at_ns":1792023733547123457},"approver":{"issued_at_ns":1792023733547123461},"signature":[19]}';
  const {failure} = await fixture({firstScreen, identity,
    member: {
      enterOwnWorkspace: async () => ({workspace: STUDIO, name: 'My studio', origin: ORIGIN}),
      approveAsked: async (root, question) => {
        assert.equal(root, identity);
        assert.equal(question, asked);
        return approved;
      },
    },
    boot: async ({deps}, page) => {
      await deps.onAdmitted(page.peer);
      assert.equal(await deps.holdFirstScreen(page), true);
      return null;
    }});
  assert.equal(failure, null);
  const before = globalThis.fetch;
  const requests = [];
  try {
    globalThis.location.origin = 'https://web.example';
    const replies = [
      Response.json({id: 'follow-id', ticket: 'login-ticket'}),
      new Response(`{"asked":${asked},"agent":"Reviewer","steps":[]}`),
      new Error('approval receipt lost'),
      new Response(`{"asked":${asked},"certified":"stored certificate","agent":"Reviewer","steps":["logged-in","tools-added","entered"]}`),
    ];
    globalThis.fetch = async (url, {method, body}) => {
      requests.push([url, method, body]);
      const reply = replies.shift();
      if (reply instanceof Error) throw reply;
      return reply;
    };
    const made = await make();
    assert.equal(made.code, 'login-ticket');
    const resumed = resume(made.saved);
    await assert.rejects(resumed.follow(() => true), /approval receipt lost/);
    assert.deepEqual((await resumed.follow(() => true)).steps, ['logged-in', 'tools-added', 'entered']);
    assert.deepEqual(requests, [
      [ORIGIN + '/peer/v1/ticket', 'POST', JSON.stringify({user: USER, workspace: STUDIO, name: 'My studio', site: 'https://web.example'})],
      [ORIGIN + '/peer/v1/ticket/asked', 'POST', '{"id":"follow-id"}'],
      [ORIGIN + '/peer/v1/ticket/answer', 'POST', `{"id":"follow-id","approved":${approved}}`],
      [ORIGIN + '/peer/v1/ticket/asked', 'POST', '{"id":"follow-id"}'],
    ]);
  } finally {
    globalThis.fetch = before;
  }
});

// Exercise the real first-screen controller and ticket requests, stopping at
// the two asynchronous boundaries that precede sending an approval to the host.
for (const stage of ['host read', 'signature', 'failed signature']) {
  test(`leaving agent setup during a ${stage} sends no answer to the old ticket`, async () => {
    const reached = Promise.withResolvers();
    const release = Promise.withResolvers();
    const minted = Promise.withResolvers();
    const answers = [];
    let signing = 0;
    let polls = 0;
    let tickets = 0;
    let advance;
    const firstScreen = createFirstScreen({
      view: new Proxy({}, {get: () => () => {}}),
      clipboard: {copy: async () => true},
      every: callback => { advance = callback; return () => {}; },
      later: () => 0, clear() {},
    });
    const {failure} = await fixture({firstScreen,
      member: {
        enterOwnWorkspace: async () => ({workspace: STUDIO, name: 'Studio', origin: ORIGIN}),
        approveAsked: async () => {
          signing++;
          reached.resolve();
          return release.promise;
        },
      },
      boot: async ({deps}, page) => {
        await deps.onAdmitted(page.peer);
        assert.equal(await deps.holdFirstScreen(page), true);
        return null;
      }});
    assert.ifError(failure);
    let offerQuestion = stage === 'host read';
    const before = globalThis.fetch;
    globalThis.fetch = async (url, {body}) => {
      const route = new URL(url).pathname;
      if (route === '/peer/v1/ticket') {
        minted.resolve();
        ++tickets;
        return Response.json({id: String(tickets), ticket: 'tilo1' + tickets});
      }
      if (route === '/peer/v1/ticket/answer') {
        answers.push(JSON.parse(body));
        return new Response(null, {status: 204});
      }
      assert.equal(route, '/peer/v1/ticket/asked');
      polls++;
      if (JSON.parse(body).id !== '1' || !offerQuestion) return Response.json({});
      if (stage === 'host read') {
        reached.resolve();
        await release.promise;
      }
      return Response.json({asked: {digest: [42]}});
    };
    try {
      await firstScreen.copy();
      await minted.promise;
      await tick();
      assert.ok(polls > 0, 'the new offer is followed while it is shown');
      if (stage !== 'host read') {
        await firstScreen.copy();
        await tick();
        offerQuestion = true;
        advance();
      }
      await reached.promise;
      if (stage === 'host read') firstScreen.skip();
      else firstScreen.back();
      if (stage === 'failed signature') release.reject(new Error('root locked'));
      else release.resolve('{"signature":[42]}');
      await tick();
      assert.equal(signing, stage === 'host read' ? 0 : 1);
      assert.deepEqual(answers, []);
    } finally {
      firstScreen.close();
      globalThis.fetch = before;
    }
  });
}

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

test('a workspace handed to the page is kept as one this browser entered', async () => {
  // The host's device key, which a remembered workspace names as the one that
  // held it when the browser entered.
  const hostDevice = 'cd'.repeat(32);
  const {failure} = await fixture({remembered: null, boot: async ({deps}, page) => {
    deps.onScope({scope: page.router.route.target.scope, entry: {base: 'https://web.example/', home: null}});
    // A workspace this browser joined as a member (an invitation it redeemed,
    // an account it holds): kept under its host's device key, so a bare entry
    // reopens it and admission comes back to it as this person.
    deps.onPageFirst({origin: ORIGIN, workspace: 'joined',
      host: {hostCert: {device_public_key: Array(32).fill(0xcd)}}});
    assert.deepEqual(enteredWorkspaces().map(kept => [kept.origin, kept.workspace, kept.hostDevice]),
      [[ORIGIN, 'joined', hostDevice]], 'the workspace it joined is remembered');
    return null;
  }});
  assert.equal(failure, null);
});

test('a guest the site’s entry let in is not kept as a workspace this browser entered', async () => {
  const {failure} = await fixture({remembered: null, boot: async ({deps}) => {
    // A visitor the site's entry let in is a guest of the workspace, not a
    // member of it: nothing is kept of where they were let in.
    globalThis.__entryPageFixture.joinViaHost = async () => ({origin: ORIGIN, workspace: 'guest',
      host: {hostCert: {device_public_key: Array(32).fill(0xcd)}}});
    await deps.enterViaHost({workspace: {name: 'Lounge', host: {origin: ORIGIN}}});
    deps.onPageFirst({origin: ORIGIN, workspace: 'guest', host: {hostCert: {device_public_key: Array(32).fill(0xcd)}}});
    assert.deepEqual(enteredWorkspaces(), [], 'a guest is not kept as entered');
    return null;
  }});
  assert.equal(failure, null);
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
  // A short link: the invitation its workspace keeps at its host, and the
  // secret that names it there.
  const short = `https://web.example/#/@node.example/w/current?invite=${'k'.repeat(43)}`;
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

/** The link a signed-in tab formats for the workspace it opens. */
const signedInLink = {format: ({target}) => `https://web.example/#/@node.example/w/${target.scope.workspace}`};
const INVITE = 'e'.repeat(32);

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
