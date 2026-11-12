import test from 'node:test';
import assert from 'node:assert/strict';
import {readFile} from 'node:fs/promises';
import {chooseWorkspace, chosenWorkspace, enteredWorkspaces, joinViaHost} from './workspaces.mjs';
import {AdmissionError} from './admission.mjs';
import {runtimeFake} from './runtime-surface.mjs';

const USER = 'aa'.repeat(32);
/** The short link the page's site keeps an invitation under. */
const SEALED = 'https://web.example/i/AbCdEfGhIjKlMnOp#s3cr3tKeyS3cr3tKeyS3cr3tKeyS3cr3tKeyS3cr';
const ORIGIN = 'https://node.example';
const tick = async () => { for (let i = 0; i < 30; i++) await Promise.resolve(); };

class Element {
  constructor(tag) { this.tag = tag; this.children = []; this.listeners = new Map(); this.dataset = {}; this.style = {}; }
  append(...nodes) { this.children.push(...nodes); }
  prepend(...nodes) { this.children.unshift(...nodes); }
  replaceChildren(...nodes) { this.children = nodes; }
  setAttribute() {}
  addEventListener(type, listener) { this.listeners.set(type, listener); }
  focus() {}
  select() {}
  remove() { document.body.children = document.body.children.filter(node => node !== this); }
}

// Run the real page wiring and workspace actions. Replace only the booted
// WASM/GPU peer, arrival animation, and root custody with fixtures.
async function fixture({boot, remembered = [], own = false, search = '', session = {}, member = null} = {}) {
  const values = new Map();
  const stored = new Map(Object.entries(session));
  const listeners = [];
  globalThis.localStorage = {getItem: key => values.get(key) ?? null, setItem: (key, value) => values.set(key, value), removeItem: key => values.delete(key)};
  globalThis.sessionStorage = {getItem: key => stored.get(key) ?? null, setItem: (key, value) => stored.set(key, value), removeItem: key => stored.delete(key)};
  globalThis.addEventListener = (type, listener) => listeners.push({type, listener});
  globalThis.removeEventListener = (type, listener) => {
    const index = listeners.findIndex(held => held.type === type && held.listener === listener);
    if (index >= 0) listeners.splice(index, 1);
  };
  globalThis.location = {href: 'https://web.example/#/room', search, pathname: '/', reload: () => calls.push('reload'), assign: () => calls.push('assign')};
  globalThis.history = {pushState: () => calls.push('history')};
  globalThis.document = {body: new Element('body'), documentElement: {dataset: {}, toggleAttribute() {}}, createElement: tag => new Element(tag),
    getElementById: id => id === 'status' ? status : id === 'surface' ? new Element('canvas') : null};
  const status = {textContent: 'Loading…'};
  const calls = [];
  const shown = [];
  let diagnostics;
  const peer = {origin: ORIGIN, workspace: 'current', userPublicKey: USER,
    host: {hostCert: {user_public_key: Array(32).fill(170), device_public_key: Array(32).fill(0xcd)}},
    createInvitation: async () => { calls.push('invite'); return 'signed-secret'; },
    // A member, not the owner, of a host that keeps its wait list to whoever
    // it lets invite.
    membership: {list: async () => ({name: 'Current', members: []}), mayInvite: async () => false, waitList: async () => { throw new Error('only someone the room lets invite lists the wait list'); }},
    workspaces: {create: async name => ({room: 'created', name}), enter: async () => calls.push('enter')},
    dispose: async () => calls.push('dispose')};
  const page = {peer: own ? null : {...peer, ...member}, entry: {base: 'https://web.example/'},
    mount: {setHostActions: actions => shown.push(actions), setOwnPerson: person => calls.push(['own', person]), flush: async () => calls.push('flush'), cleanup: async () => calls.push('cleanup')},
    router: {route: {target: {scope: own ? {kind: 'own'} : {kind: 'room', host: ORIGIN, room: 'current'}}}, stop: () => calls.push('stop')},
    link: {format: route => `https://web.example/#/${route.target.scope.room}`}};
  const prepared = [];
  const runtime = runtimeFake({
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
    forgetGoneWorkspaces: (list, origin, hostDevice) => {
      const all = JSON.parse(list);
      const gone = entry => entry.origin === origin && entry.hostDevice !== hostDevice;
      return JSON.stringify({entered: all.filter(entry => !gone(entry)), unchoose: all.filter(gone)});
    },
    rememberEntered: (list, entry) => {
      const {origin, room, hostDevice} = JSON.parse(entry);
      const {entered, unchoose} = JSON.parse(runtime.forgetGoneWorkspaces(list, origin, hostDevice));
      const same = kept => kept.origin === origin && kept.room === room;
      const name = JSON.parse(entry).name ?? entered.find(same)?.name;
      return JSON.stringify({entered: [{origin, room, hostDevice, ...(name ? {name} : {})}, ...entered.filter(kept => !same(kept))], unchoose});
    },
    nameEntered: (list, place, name) => {
      const {origin, room} = JSON.parse(place);
      return JSON.stringify({entered: JSON.parse(list).map(entry => entry.origin === origin && entry.room === room && !entry.name ? {...entry, name} : entry), unchoose: []});
    },
  });
  globalThis.__entryPageFixture = {page, runtime, mounted: [], prepared, identity: {userPublicKey: USER}, prepareGpu: () => prepared.push(document.documentElement.dataset.arrival),
    joinViaHost,
    boot: async options => {
      diagnostics = options.diagnostics;
      // The page loads its runtime as boot starts; the fixture's is the stub above.
      await options.deps.loadRuntime();
      return boot ? boot(options, page) : page;
    }};
  // A workspace an earlier visit entered, and those it kept as they were
  // stored, whichever host held them.
  localStorage.setItem('tilo-entered-workspaces', JSON.stringify([
    {origin: ORIGIN, room: 'other', hostDevice: 'cd'.repeat(32), name: 'Other'}, ...remembered]));
  let source = await readFile(new URL('./entry-page.mjs', import.meta.url), 'utf8');
  source = source
    .replace("import {boot, grammar} from './entry.mjs';", 'const boot = globalThis.__entryPageFixture.boot;')
    .replace("import {applicationName, loadRuntime, mountDesktop, prefetchApplication, prefetchDesktop, prefetchOfferedApplications, prepareGpu, reportPageErrors} from './host.mjs';", 'const applicationName = async () => null, loadRuntime = async () => globalThis.__entryPageFixture.runtime, mountDesktop = (...args) => globalThis.__entryPageFixture.mounted.push(args), prefetchApplication = () => {}, prefetchDesktop = () => {}, prefetchOfferedApplications = () => {}, prepareGpu = () => globalThis.__entryPageFixture.prepareGpu(), reportPageErrors = () => {};')
    .replace("import {failureMessage, startArrival, STOPPED_DRAWING} from './arrival.mjs';", "const startArrival = () => {}, failureMessage = error => error.message, STOPPED_DRAWING = 'stopped';")
    .replace("import {servedVersion, watchForUpdate} from './bundle-update.mjs';", "import {servedVersion} from './bundle-update.mjs';\nconst watchForUpdate = () => {};")
    .replace("import {rendererUnavailable} from './webgpu-safety.mjs';", 'const rendererUnavailable = async () => null;')
    .replace("import {browserIdentity, browserPerson, currentIdentity, fetchEnvelope, IdentityError, MAX_DISPLAY_NAME_BYTES, signInWithPasskey} from './identity.mjs';", 'const currentIdentity = () => globalThis.__entryPageFixture.identity;\nconst browserPerson = async () => globalThis.__entryPageFixture.identity;')
    .replace('hostToReconnect, joinGranted, joinViaHost,', 'hostToReconnect, joinGranted,')
    .replace('const PAGE_BUILD', 'const joinViaHost = (...args) => globalThis.__entryPageFixture.joinViaHost(...args);\nconst PAGE_BUILD')
    .replace(/from '(\.\/[^']+)'/g, (_all, path) => `from '${new URL(path, import.meta.url).href}'`);
  // A fresh module per fixture; the nonce rides in the source since Bun refuses a fragment on a base64 data URL.
  const {startEntryPage} = await import(`data:text/javascript;base64,${Buffer.from(`${source}\n// ${Math.random()}`).toString('base64')}`);
  // A page that cannot get in throws its cause on, to the console.
  const failure = await startEntryPage({mode: 'desktop'}).then(() => null, error => error);
  await tick();
  return {calls, shown, diagnostics, page, prepared, mounted: globalThis.__entryPageFixture.mounted, listeners, session: stored, failure};
}

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
  const text = node => typeof node === 'string' ? node : node.textContent ?? null;
  const shape = card => ({face: card.children[0].children[0].style, who: card.children[0].children.map(text), lead: text(card.children[1]),
    place: card.children[2].children.map(text), role: card.children[3].children.map(text),
    actions: card.children[4].children.map(button => button.textContent),
    primary: card.children[4].children[0].className});
  let offered;
  let answered;
  let declined;
  let notNow;
  const {failure} = await fixture({boot: async ({deps}, page) => {
    const answer = deps.confirmInvitation({workspaceName: 'Lounge', role: 'commenter', members: 4,
      inviter: {name: 'Nimble Panda', userPublicKey: USER}});
    offered = shape(cards()[0]);
    cards()[0].children[4].children[0].listeners.get('click')();
    answered = await answer;
    const other = deps.confirmInvitation({workspaceName: 'Commons', role: 'viewer', members: 1, inviter: null});
    declined = shape(cards()[0]);
    cards()[0].children[4].children[1].listeners.get('click')();
    notNow = await other;
    return page;
  }});
  assert.equal(failure, null);
  // Who asked them: their initial, in the colour presence gives them, and
  // their name as the host verified it.
  assert.deepEqual(offered.face, {background: '#ffd166', color: '#101114'},
    'the monogram is drawn in the inviter’s presence colour, dark on a light one');
  assert.deepEqual(offered.who, ['N', 'Nimble Panda']);
  assert.equal(offered.lead, 'invites you to');
  // Where, and how many people are already in it.
  assert.deepEqual(offered.place, ['Lounge', '4 people']);
  // As what, in the words a person reads: the part, then what it lets them do.
  assert.deepEqual(offered.role, ['Commenter', ' — you can add comments, but not edit shared work.']);
  assert.deepEqual(offered.actions, ['Join', 'Not now']);
  assert.equal(offered.primary, 'tilo-entry-primary', 'Join is the primary action');
  assert.equal(answered, true, 'Join joins');
  // A link that names no one, to a workspace of one: the card says so rather
  // than leaving the field out.
  assert.deepEqual(declined.who, ['S', 'Someone']);
  assert.deepEqual(declined.face, {}, 'a link that names no person falls back to the card’s own face');
  assert.deepEqual(declined.place, ['Commons', '1 person']);
  assert.deepEqual(declined.role, ['Viewer', ' — you can view shared work, but not comment or edit it.']);
  assert.equal(notNow, false, 'Not now does not join');
  assert.equal(cards().length, 0, 'either answer closes the card');
});

test('a wait-list invitation is the short link its site keeps, not the invitation’s text', async () => {
  const ed = {requestId: '05'.repeat(32), contact: 'ed@example.org', addedAtMs: 1_000};
  const {calls, diagnostics} = await fixture({member: {
    createInvitation: async () => 'SIGNED-invitation',
    membership: {list: async () => ({name: 'Current', members: []}),
      mayInvite: async () => true, waitList: async () => [ed], unlist: async () => {}, mailInvite: async () => { throw new Error('this door sends no mail'); }},
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
  assert.deepEqual(calls.filter(call => Array.isArray(call) && call[0] === 'sealed'),
    [['sealed', 'https://web.example/', ORIGIN, 'SIGNED-invitation']],
    'the invitation is sealed onto the page’s own site, naming the host it admits into');
  const share = document.body.children.find(form => form.dataset.card === 'share');
  assert.ok(share, 'a door that sends no mail shares the link instead');
  assert.equal(share.children.find(child => child.name === 'link').value, SEALED,
    'what the inviter sends is the short link, not the invitation’s text');
  assert.match(share.children[1].textContent, /as an editor/);
});

test('workspaces another host held at the origin of the page host are no longer offered', async () => {
  // The host at ORIGIN was set up again on fresh state twice since this
  // browser entered workspaces there; both workspaces went with the hosts before.
  const {shown} = await fixture({remembered: [
    {origin: ORIGIN, room: 'gone-1', hostDevice: 'e1'.repeat(32), name: 'Other'},
    {origin: ORIGIN, room: 'gone-2', hostDevice: 'e2'.repeat(32), name: 'Other'},
    {origin: 'https://elsewhere.example', room: 'away', hostDevice: 'e3'.repeat(32), name: 'Away'},
  ]});
  assert.deepEqual(shown.at(-1).map(action => action.title), ['Set up a machine', 'Open your desktop', 'Open Other', 'Open Away', 'New workspace', 'Join a workspace']);
  assert.deepEqual(enteredWorkspaces().map(entry => entry.room), ['current', 'other', 'away']);
});

test('the page keeps switch actions together and dispatches them', async () => {
  const {calls, shown, diagnostics} = await fixture();
  const actions = shown.at(-1);
  assert.deepEqual(actions.map(action => action.title), ['Set up a machine', 'Open your desktop', 'Open Other', 'New workspace', 'Join a workspace']);
  diagnostics.onHostAction(actions.find(action => action.title === 'Open Other').id);
  await tick();
  assert.deepEqual(calls, ['flush', 'stop', 'cleanup', 'dispose', 'history', 'reload']);
  assert.deepEqual(chosenWorkspace(), {origin: ORIGIN, room: 'other'});
});

test('a workspace offers the way back to the person’s own desktop, and their own desktop does not offer itself', async () => {
  const {calls, shown, diagnostics, page} = await fixture();
  page.link.format = route => `https://web.example/#/${route.target.scope.kind === 'own' ? 'me' : route.target.scope.room}`;
  chooseWorkspace({origin: ORIGIN, room: 'other'});
  const back = shown.at(-1).find(action => action.title === 'Open your desktop');
  assert.ok(back, 'a room is no dead end');
  const urls = [];
  globalThis.history.pushState = (_state, _title, url) => { urls.push(url); calls.push('history'); };
  diagnostics.onHostAction(back.id);
  await tick();
  assert.deepEqual(calls.slice(-6), ['flush', 'stop', 'cleanup', 'dispose', 'history', 'reload']);
  assert.deepEqual(urls, ['https://web.example/#/me']);
  assert.deepEqual(chosenWorkspace(), {origin: ORIGIN, room: 'other'}, 'the room a bare entry reopens stays');
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
  assert.deepEqual(calls, ['enter', 'flush']);
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
  assert.deepEqual(calls, ['enter', 'flush', 'stop', 'cleanup', 'dispose', 'history', 'reload']);
  assert.deepEqual(chosenWorkspace(), {origin: ORIGIN, room: 'created'});
  assert.equal(enteredWorkspaces().find(room => room.room === 'created').name, 'Created');
});


test('the person’s own desktop writes as their root identity and shows them by their name', async () => {
  let mountOwn;
  const {calls} = await fixture({own: true, boot: async (options, page) => { mountOwn = options.deps.mountDesktop; return page; }});
  // No workspace's peer certifies who writes there: the browser's root identity does.
  await mountOwn('surface', {onStatus() {}});
  const [[, options]] = globalThis.__entryPageFixture.mounted;
  assert.equal(options.author, USER);
  const own = calls.find(call => call[0] === 'own');
  assert.ok(own, 'the desktop is told who its person is');
  assert.equal(own[1].user, USER);
  assert.ok(own[1].label && own[1].label !== 'Guest', own[1].label);
});

test('the home workspace’s peer that carries reports from another workspace joins it without calls or a live plane', async () => {
  let homePeer;
  await fixture({boot: async (options, page) => { homePeer = options.deps.homePeer; return page; }});
  const home = {name: 'Lounge', host: {origin: 'https://lounge.example'}, admission: 'https://lounge.example/'};
  const joined = [];
  const carrier = {room: 'LOUNGE'};
  globalThis.__entryPageFixture.joinViaHost = async options => { joined.push(options); return carrier; };
  assert.equal(await homePeer(home), null, 'a person the home never let in gets no peer there');
  assert.deepEqual(joined, []);
  localStorage.setItem('tilo-host-memberships', JSON.stringify({'https://lounge.example': {origin: 'https://lounge.example', room: 'LOUNGE', userPublicKey: USER}}));
  assert.equal(await homePeer(home), carrier);
  assert.equal(joined.length, 1);
  // Nothing live or in calls, which only the workspace in front has.
  assert.equal(joined[0].background, undefined);
  assert.equal(joined[0].live, false);
  assert.equal(joined[0].voice, false);
  assert.equal(joined[0].workspace, home);
  await assert.rejects(joined[0].wait({attempt: 0}), /did not let this device in/, 'it never waits at the door');
});

test('an embedded workspace offers no invite sheet it could not carry out', async () => {
  let deps;
  await fixture({search: '?embed=1', boot: async (options, page) => { deps = options.deps; return page; }});
  const embedded = deps;
  const joining = {workspace: 'current', origin: ORIGIN, userPublicKey: USER};
  assert.equal(embedded.invite({peer: joining, entry: {base: 'https://web.example/'}}), null);
  await fixture({boot: async (options, page) => { deps = options.deps; return page; }});
  // Named no earlier than admission names the person; the sheet makes the
  // link either way, and says who made it once there is a name.
  assert.deepEqual(deps.invite({peer: joining, entry: {base: 'https://web.example/'}}),
    {base: 'https://web.example/', origin: ORIGIN, workspace: 'current', name: '', user: USER});
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
    // Nothing mounts. The page takes no seat and makes no person of its own.
    assert.deepEqual(await deps.enterViaHost({workspace: LOUNGE}), {idle: true});
    assert.equal(document.documentElement.dataset.entry, 'idle');
    assert.equal(document.getElementById('status').textContent, 'Click or type to join');
    return null;
  }});
  assert.equal(failure, null);
  assert.deepEqual(told, [{message: {type: 'tilo-embed', state: 'idle'}, origin: 'https://site.example'}]);
  assert.deepEqual(requests, [], 'no spectate, no door, no person');
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
  globalThis.fetch = async url => { requests.push(String(url)); throw new Error('no request before the door'); };
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
  assert.match(joined[0].displayName, /^[A-Z][a-z]+ [A-Z][a-z]+$/);
  assert.deepEqual(requests, [], 'no request was made for a visitor');
  assert.equal(session.has('tilo-embed-join'), false, 'the gesture is spent');
});

test('an uncaught page error reaches the door the site names', async () => {
  const beacons = [];
  globalThis.window = globalThis;
  globalThis.parent = {postMessage: () => {}};
  Object.defineProperty(globalThis, 'navigator', {configurable: true,
    value: {sendBeacon: (url, body) => { beacons.push({url: String(url), body}); return true; }, language: 'en'}});
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
  assert.equal(beacons[0].url, 'https://home.example/peer/v1/directory/lounge/errors', 'the door\'s own errors route');
  assert.deepEqual(JSON.parse(await beacons[0].body.text()), {
    kind: 'error', message: 'x is not a function', source: 'https://home.example/v/7/page.js', line: 12, column: 4,
    stack: 'at page.js:12', page: '/', build: 'development'});
  const rejection = JSON.parse(await beacons[1].body.text());
  assert.equal(rejection.kind, 'unhandledrejection');
  assert.equal(rejection.message, 'the host went away');
  assert.equal(rejection.page, '/');
  assert.equal(typeof rejection.stack, 'string');
  // A page that keeps failing says five things a load, never more.
  for (let i = 0; i < 10; i++) fire('error', {message: `again ${i}`});
  assert.equal(beacons.length, 5);
});

// A session's own status, as the peer publishes it (PeerSnapshot).
function sessionOf({phase = 'ready', transport = 'connected', admission = 'ready'} = {}) {
  return {phase, outgoingPending: 0, health: {version: 1, transport: {phase: transport}, admission: {phase: admission}, records: [], failures: []}};
}

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
