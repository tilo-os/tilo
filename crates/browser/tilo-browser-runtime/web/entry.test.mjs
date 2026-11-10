import assert from 'node:assert/strict';
import test from 'node:test';
import {WORKSPACE_DOCUMENT_WAIT_MS, boot, chosenRoute, configureFeedback, initialRoute, present, readEntry, scopeHost, shownLink} from './entry.mjs';
import {UNAVAILABLE, openTarget, untilSnapshot, untilViewable, viewInvocation} from './open-target.mjs';
import {runtimeFake} from './runtime-surface.mjs';

/** The runtime's way in (tilo-session-host's `way_in`, tested there),
 * answering the ways this page's scenario takes, in turn and again for each
 * entry. */
const entering = (...ways) => {
  let next = 0;
  const way = () => ways[next++ % ways.length];
  return {wayIn: way, wayInAfter: way, admittedAsAsked: () => true};
};

const message = {kind: 'chat.message', id: 'chat.message:1'};
const views = [
  {kind: 'read', address: {resource: message.id, action: 'reveal'}, view: {id: 'message', open_action: 'reveal', primary: true,
    range: {action: 'reveal-range', start_param: 'from', end_param: 'to', unit_param: 'unit'}}},
  {kind: 'read', address: {resource: message.id, action: 'thread'}, view: {id: 'thread', open_action: 'thread', primary: false}},
  {kind: 'undoable', address: {resource: message.id, action: 'delete'}},
];

test('a link names its view by id and its range through the view’s range binding', () => {
  assert.deepEqual(viewInvocation(views, message), {action: 'reveal', args: {}});
  assert.deepEqual(viewInvocation(views, message, 'thread'), {action: 'thread', args: {}});
  assert.deepEqual(viewInvocation(views, {...message, span: {start: 2, end: 5, unit: 'Chars'}}),
    {action: 'reveal-range', args: {from: 2, to: 5, unit: 'Chars'}});
  assert.throws(() => viewInvocation(views, message, 'gone'), {message: UNAVAILABLE});
  assert.throws(() => viewInvocation([], message), {message: UNAVAILABLE});
});

function desktopNavigation(windows) {
  let snapshot = {generation: 'g1', active: null, entries: []};
  const listeners = new Set();
  const commands = [];
  const show = next => { snapshot = next; for (const listener of [...listeners]) listener(next); };
  return {commands, navigation: {
    getSnapshot: () => snapshot,
    subscribe(listener) { listeners.add(listener); return () => listeners.delete(listener); },
    async dispatch(command) {
      commands.push(command);
      const route = {workspace: '1', strip: '1', group: '7', tab: '8'};
      queueMicrotask(() => show({generation: 'g1', active: route, entries: [...windows, {route, app: command.args?.app}]}));
    },
  }};
}

test('desktop mode opens the app through navigation, then the view through its window’s door', async () => {
  const {commands, navigation} = desktopNavigation([]);
  const invoked = [];
  const mount = {
    actions: (resource, options) => { assert.deepEqual(options, {window: '8'}); return views; },
    invokeAction: (...args) => { invoked.push(args); return {state: 'completed'}; },
  };
  await openTarget({mount, navigation, mode: 'desktop', target: {app: 'so.tilo.chat', record: '', resource: message}});
  assert.deepEqual(commands, [{effect: 'show-app', args: {app: 'so.tilo.chat'}}]);
  assert.deepEqual(invoked, [[message.id, 'reveal', {}, {window: '8'}]]);
});

test('a refused view reads like a missing one', async () => {
  const mount = {actions: () => views, invokeAction: () => { throw new Error('permission: Viewer cannot'); }};
  await assert.rejects(openTarget({mount, mode: 'app', target: {app: 'so.tilo.chat', record: '', resource: message}}),
    {message: UNAVAILABLE});
});

// Catches a slow device losing the whole page ("Something went wrong
// opening Lounge") 10 s after its desktop drew, while the link's window
// was still on its way.
test('a link waits for its window as long as the desktop has not drawn the frames to show it', async () => {
  const pending = [];
  const timers = {setTimeout: fn => { pending.push(fn); return pending.length; }, clearTimeout: () => {}};
  const fire = () => pending.shift()?.();
  let frames = 1;
  let listener;
  const navigation = {getSnapshot: () => ({active: null}), subscribe: next => { listener = next; return () => { listener = null; }; }};
  let outcome = null;
  const shown = untilSnapshot(navigation, next => next.active === 'chat', {timers, frames: () => frames});
  shown.then(() => { outcome = 'shown'; }, () => { outcome = 'failed'; });
  // Long past the deadline, but the desktop drew one frame since: waiting.
  for (let i = 0; i < 50; i++) fire();
  frames = 3;
  for (let i = 0; i < 50; i++) fire();
  await Promise.resolve();
  assert.equal(outcome, null);
  listener({active: 'chat'});
  await shown;
  assert.equal(outcome, 'shown');
  // A desktop that drew its frames and still did not show it gives up.
  pending.length = 0;
  frames = 10;
  const never = untilSnapshot(navigation, next => next.active === 'chat', {timers, frames: () => frames});
  frames = 13;
  fire();
  await assert.rejects(never, /did not show the link in time/);
});

test('a link waits for its resource to reach the page, then says it is not there', async () => {
  const timers = {setTimeout: done => queueMicrotask(done)};
  let polls = 0;
  const arriving = {actions: () => (++polls < 3 ? [] : views)};
  const target = {app: 'so.tilo.chat', record: '', resource: message};
  assert.deepEqual(await untilViewable(arriving, target, undefined, {timers}), {action: 'reveal', args: {}});
  assert.equal(polls, 3);
  polls = 0;
  const missing = {actions: () => { polls++; return []; }};
  await assert.rejects(untilViewable(missing, target, undefined, {timers, waitMs: 500}), {message: UNAVAILABLE});
  assert.equal(polls, 6);
  await assert.rejects(untilViewable(arriving, {...target, view: 'gone'}, undefined, {timers}), {message: UNAVAILABLE});
});

test('an unbound desktop entry without a route is the person’s own desktop', () => {
  const link = {parse: () => { throw Object.assign(new Error('no route'), {code: 'no-route'}); }};
  assert.deepEqual(initialRoute({link, location: {hash: '', href: 'https://tilo.so/'}, mode: 'desktop'}),
    {mode: 'desktop', target: {scope: {kind: 'own'}}});
  assert.throws(() => initialRoute({link, location: {hash: '', href: 'https://tilo.so/app/'}, mode: 'app'}), {code: 'no-route'});
});

test('a bare entry opens the web host’s home workspace, its workspace learned on admission', () => {
  const link = {parse: () => { throw Object.assign(new Error('no route'), {code: 'no-route'}); }};
  const home = {name: 'Commons', host: {origin: 'https://rooms.example/'}, admission: 'https://rooms.example/'};
  const scope = {kind: 'room', host: 'https://rooms.example', room: null};
  assert.deepEqual(initialRoute({link, location: {hash: '', href: 'https://rooms.example/open/'}, mode: 'desktop', home}),
    {mode: 'desktop', target: {scope}});
  assert.deepEqual(initialRoute({link, location: {hash: '', href: 'https://rooms.example/open/app/'}, mode: 'app', home}),
    {mode: 'desktop', target: {scope}});
  assert.deepEqual(initialRoute({link, location: {hash: '', href: 'https://rooms.example/open/app/'}, mode: 'app', home: {...home, app: 'so.tilo.chat'}}),
    {mode: 'app', target: {scope, app: 'so.tilo.chat'}});
  assert.deepEqual(initialRoute({link, location: {hash: '', href: 'https://rooms.example/open/'}, mode: 'desktop', home: {...home, app: 'so.tilo.chat'}}),
    {mode: 'desktop', target: {scope, app: 'so.tilo.chat'}});
});

test('the entry is the page’s directory, with the home workspace the web host names', async () => {
  const home = {name: 'Commons', host: {origin: 'https://rooms.example'}, admission: 'https://rooms.example/'};
  const fetch = async url => ({ok: String(url) === 'https://rooms.example/open/entry.json', json: async () => ({home})});
  assert.deepEqual(await readEntry({mode: 'app', location: {href: 'https://rooms.example/open/app/?embed=1#/so.tilo.chat'}, fetch}),
    {base: 'https://rooms.example/open/', home});
  const tickets = async () => ({ok: true, json: async () => ({tickets: 'https://tilo.so/rooms/', home: {name: 'no host'}})});
  assert.deepEqual(await readEntry({mode: 'desktop', location: {href: 'https://tilo.so/'}, fetch: tickets}),
    {base: 'https://tilo.so/', tickets: 'https://tilo.so/rooms/'});
  assert.deepEqual(await readEntry({mode: 'desktop', location: {href: 'https://tilo.so/#/me'}, fetch: async () => ({ok: false})}),
    {base: 'https://tilo.so/'});
});

test('the entry names the embedding site only when it is exactly an https origin', async () => {
  const served = embedder => async () => ({ok: true, json: async () => ({embedder})});
  const read = embedder => readEntry({mode: 'desktop', location: {href: 'https://app.example/'}, fetch: served(embedder)});
  assert.deepEqual(await read('https://site.example'), {base: 'https://app.example/', embedder: 'https://site.example'});
  for (const refused of ['http://site.example', 'https://site.example/', 'https://site.example/page', 'site.example', 7]) {
    assert.deepEqual(await read(refused), {base: 'https://app.example/'}, String(refused));
  }
});

test('the entry lays the desktop out as the console only when it says so', async () => {
  const served = layout => async () => ({ok: true, json: async () => ({layout})});
  const read = layout => readEntry({mode: 'desktop', location: {href: 'https://app.example/'}, fetch: served(layout)});
  assert.deepEqual(await read('console'), {base: 'https://app.example/', layout: 'console'});
  for (const other of ['desktop', 'Console', '', null, 1]) {
    assert.deepEqual(await read(other), {base: 'https://app.example/'}, String(other));
  }
});

test('the catalogue is never read before admission', async () => {
  const order = [];
  const target = {scope: {kind: 'room', host: 'https://h.example', room: 'r'}, app: 'so.tilo.chat', record: '', resource: message};
  const runtime = runtimeFake({
    parseLink: () => JSON.stringify({mode: 'app', target}),
    formatLink: () => 'https://tilo.so/app/#/@h.example/w/r/so.tilo.chat/-/chat.message:1',
    ...entering('accounts', 'member'),
  });
  const peer = {
    whenReady: async () => order.push('ready'),
    whenRecord: async () => order.push('record'),
    mountDesktop: async () => {
      order.push('mount');
      return {actions: () => { order.push('catalogue'); return views; }, invokeAction: () => ({state: 'completed'})};
    },
  };
  const window = {
    __tiloLinkSecrets: Object.freeze({}),
    location: {href: 'https://tilo.so/app/#/@h.example/w/r/so.tilo.chat/-/chat.message:1', hash: '#/@h.example/w/r'},
    history: {replaceState() {}, pushState() {}},
    addEventListener() {}, removeEventListener() {},
  };
  const deps = {
    loadRuntime: async () => runtime, fetch: async () => ({ok: false}),
    readAccounts: async () => { order.push('accounts'); return [{userPublicKey: 'ab'}]; },
    connectMember: async () => { order.push('admitted'); return peer; },
  };
  await boot({mode: 'app', window, deps});
  assert.deepEqual(order, ['accounts', 'admitted', 'ready', 'record', 'mount', 'catalogue']);
});

test('the home workspace opens through its host door under its listed name, and the address becomes its canonical link', async () => {
  const home = {name: 'Commons', host: {origin: 'https://rooms.example'}, admission: 'https://rooms.example/'};
  const formatted = [];
  const replaced = [];
  const runtime = runtimeFake({
    parseLink: () => { throw Object.assign(new Error('no route'), {code: 'no-route'}); },
    formatLink: route => { formatted.push(JSON.parse(route)); return 'https://rooms.example/open/#/@rooms.example/w/WORKSPACE'; },
    ...entering('door'),
  });
  const navigation = {getSnapshot: () => ({generation: 'g', active: null, entries: []}), subscribe: () => () => {}, dispatch: async () => {}};
  const mounts = [];
  const peer = {workspace: 'WORKSPACE', whenReady: async () => {}, mountDesktop: async (element, options) => {
    mounts.push(options);
    options.onNavigation?.(navigation);
    return {navigation};
  }};
  const window = {
    __tiloLinkSecrets: Object.freeze({}),
    location: {href: 'https://rooms.example/open/', hash: ''},
    history: {state: null, replaceState: (state, title, url) => replaced.push(url), pushState() {}},
    addEventListener() {}, removeEventListener() {},
  };
  const entered = [];
  const deps = {
    loadRuntime: async () => runtime,
    fetch: async () => ({ok: true, json: async () => ({home})}),
    readAccounts: async () => [],
    enterViaHost: async ({workspace}) => { entered.push(workspace); return peer; },
  };
  const shown = [];
  const status = {
    text: 'Loading…',
    get textContent() { return this.text; },
    set textContent(text) { shown.push(text); this.text = text; },
  };
  const steps = [];
  const page = await boot({mode: 'desktop', window, status, deps, onStep: step => steps.push(step)});
  assert.deepEqual(entered, [home]);
  // Progress while joining, then the loading note again, which the first drawn frame clears.
  assert.ok(shown.includes('Joining…'), shown.join(' | '));
  assert.equal(status.text, 'Loading…');
  // The page hears it is joining through the door, then opening once admitted.
  assert.deepEqual([...new Set(steps)], ['join', 'open']);
  assert.equal(steps.at(-1), 'open');
  assert.deepEqual(formatted[0], {mode: 'desktop', target: {scope: {kind: 'room', host: 'https://rooms.example', room: 'WORKSPACE'}}});
  assert.equal(replaced[0], 'https://rooms.example/open/#/@rooms.example/w/WORKSPACE');
  assert.equal(mounts[0].workspaceName, 'Commons');
  assert.equal(mounts[0].host, 'rooms.example');
  page.router.stop?.();
});

test('a link whose document id is damaged opens its workspace and says what it did not use', async () => {
  const scope = {kind: 'room', host: 'https://eu1.tilo.sh', room: 'WORKSPACE'};
  const replaced = [];
  const runtime = runtimeFake({
    // The runtime's grammar names the workspace of a link whose document is damaged, and says so.
    parseLink: () => JSON.stringify({mode: 'desktop', target: {scope}, damaged: 'The document id in this link is damaged'}),
    formatLink: route => { assert.equal(JSON.parse(route).damaged, undefined); return 'https://tilo.sh/#/@eu1.tilo.sh/w/WORKSPACE'; },
    ...entering('accounts', 'member'),
  });
  const navigation = {getSnapshot: () => ({generation: 'g', active: null, entries: []}), subscribe: () => () => {}, dispatch: async () => {}};
  const mounts = [];
  const peer = {workspace: 'WORKSPACE', whenReady: async () => {}, mountDesktop: async (element, options) => {
    mounts.push(options);
    options.onNavigation?.(navigation);
    return {navigation};
  }};
  const window = {
    __tiloLinkSecrets: Object.freeze({}),
    location: {href: 'https://tilo.sh/#/@eu1.tilo.sh/w/WORKSPACE/so.tilo.pdf-viewer/application.pdf', hash: '#/@eu1.tilo.sh/w/WORKSPACE/so.tilo.pdf-viewer/application.pdf'},
    history: {state: null, replaceState: (state, title, url) => replaced.push(url), pushState() {}},
    addEventListener() {}, removeEventListener() {},
  };
  const status = {textContent: 'Loading…'};
  const page = await boot({mode: 'desktop', window, status, deps: {
    loadRuntime: async () => runtime,
    fetch: async () => ({ok: false}),
    readAccounts: async () => [{userPublicKey: 'ab'}],
    connectMember: async () => peer,
  }});
  assert.equal(mounts.length, 1, 'the workspace opens');
  assert.equal(replaced[0], 'https://tilo.sh/#/@eu1.tilo.sh/w/WORKSPACE', 'the address names what opened');
  assert.equal(status.textContent, 'The document id in this link is damaged, so the workspace opened without it.');
  page.router.stop?.();
});

test('the page hears the peer the host let in before its desktop mounts, so a failed mount can name the workspace', async () => {
  const home = {name: 'Lounge', host: {origin: 'https://rooms.example'}, admission: 'https://rooms.example/'};
  const runtime = runtimeFake({
    parseLink: () => { throw Object.assign(new Error('no route'), {code: 'no-route'}); },
    formatLink: () => 'https://rooms.example/#/@rooms.example/w/WORKSPACE',
    ...entering('door'),
  });
  const heard = [];
  const peer = {workspace: 'WORKSPACE', workspaceName: 'Kaffeeküche', whenReady: async () => {},
    mountDesktop: async () => { heard.push('mount'); throw new Error('the desktop did not start'); }};
  const window = {
    __tiloLinkSecrets: Object.freeze({}),
    location: {href: 'https://rooms.example/', hash: ''},
    history: {state: null, replaceState() {}, pushState() {}},
    addEventListener() {}, removeEventListener() {},
  };
  const deps = {
    loadRuntime: async () => runtime,
    fetch: async () => ({ok: true, json: async () => ({home})}),
    readAccounts: async () => [],
    enterViaHost: async () => peer,
    onAdmitted: admitted => heard.push(admitted.workspaceName),
  };
  await assert.rejects(boot({mode: 'desktop', window, status: null, deps}), /did not start/);
  assert.deepEqual(heard, ['Kaffeeküche', 'mount']);
});

test('a workspace on the site the page came from, or on its machines, is hosted by that site', () => {
  const room = host => ({kind: 'room', host, room: 'R'});
  assert.equal(scopeHost(room('https://eu1.tilo.sh'), 'tilo.sh'), 'tilo.sh');
  assert.equal(scopeHost(room('https://tilo.sh'), 'tilo.sh'), 'tilo.sh');
  assert.equal(scopeHost(room('https://rooms.example'), 'tilo.sh'), 'rooms.example');
  assert.equal(scopeHost(room('https://notilo.sh'), 'tilo.sh'), 'notilo.sh');
  assert.equal(scopeHost({kind: 'own'}, 'tilo.sh'), null);
  // A workspace on another port of the page's machine is the site, by its name.
  assert.equal(scopeHost(room('http://localhost:9764'), 'localhost:8411'), 'localhost');
  assert.equal(scopeHost(room('https://rooms.example:8443'), 'tilo.sh'), 'rooms.example:8443');
});

test('the entry offers only the web About links the host set', async () => {
  const about = {notify: 'https://example.org/notify', source: '', community: 'javascript:alert(1)'};
  const fetch = async () => ({ok: true, json: async () => ({about})});
  assert.deepEqual(await readEntry({mode: 'desktop', location: {href: 'https://example.org/'}, fetch}),
    {base: 'https://example.org/', about: {notify: 'https://example.org/notify'}});
  const none = async () => ({ok: true, json: async () => ({about: {notify: '', source: 'not a url'}})});
  assert.deepEqual(await readEntry({mode: 'desktop', location: {href: 'https://example.org/'}, fetch: none}),
    {base: 'https://example.org/'});
});

test('the desktop gets the entry’s About links, no host pill on the person’s own desktop, and the tab its workspace name', async () => {
  const about = {source: 'https://example.org/source'};
  const runtime = runtimeFake({parseLink: () => JSON.stringify({mode: 'desktop', target: {scope: {kind: 'own'}}}), formatLink: () => '',
    ...entering('own-desktop')});
  const mounts = [];
  const forwarded = [];
  const window = {
    __tiloLinkSecrets: Object.freeze({}),
    location: {href: 'https://example.org/#/me', hash: '#/me'},
    history: {state: null, replaceState() {}, pushState() {}},
    addEventListener() {}, removeEventListener() {},
    document: {title: 'Tilo'},
  };
  const navigation = {getSnapshot: () => ({generation: 'g', active: null, entries: []}), subscribe: () => () => {}, dispatch: async () => {}};
  const deps = {
    loadRuntime: async () => runtime,
    fetch: async () => ({ok: true, json: async () => ({about})}),
    readAccounts: async () => [],
    mountDesktop: async (element, options) => { mounts.push(options); return {navigation}; },
  };
  const page = await boot({mode: 'desktop', window, deps, diagnostics: {onStatus: status => forwarded.push(status)}});
  assert.deepEqual(mounts[0].about, about);
  assert.equal(mounts[0].host, undefined);
  mounts[0].onStatus({frames: 1});
  assert.equal(window.document.title, 'Tilo');
  mounts[0].onStatus({workspaceName: 'Garden'});
  assert.equal(window.document.title, 'Tilo · Garden');
  mounts[0].onStatus({workspaceName: 'Kitchen'});
  assert.equal(window.document.title, 'Tilo · Kitchen');
  assert.equal(forwarded.length, 3);
  page.router.stop?.();
});

test('a sealed link’s page is the site’s own, wherever the link reached it', async () => {
  const asked = [];
  const fetch = async url => { asked.push(String(url)); return {ok: false}; };
  assert.deepEqual(await readEntry({mode: 'desktop',
    location: {pathname: '/i/abcdefghijklmnop', href: 'https://tilo.sh/i/abcdefghijklmnop'}, fetch}),
    {base: 'https://tilo.sh/'});
  assert.deepEqual(asked, ['https://tilo.sh/entry.json']);
});

test('a sealed link admits through the site’s shelf and opens the workspace it names, as that workspace’s own link', async () => {
  const site = 'https://tilo.sh';
  const handle = 'abcdefghijklmnop';
  const key = 'A'.repeat(43);
  const opened = [];
  const inspected = [];
  const confirmed = [];
  const joined = [];
  const formatted = [];
  const replaced = [];
  let parsed = false;
  const runtime = runtimeFake({
    parseLink: () => { parsed = true; throw new Error('a sealed link names no route'); },
    formatLink: route => { formatted.push(JSON.parse(route)); return `${site}/#/@rooms.example/w/WORKSPACE`; },
    ...entering('sealed-invitation'),
  });
  const navigation = {getSnapshot: () => ({generation: 'g', active: null, entries: []}), subscribe: () => () => {}, dispatch: async () => {}};
  const offer = {room: 'WORKSPACE', workspaceName: 'Lounge', role: 'commenter', members: 4,
    inviter: {name: 'Nimble Panda', userPublicKey: 'ab'.repeat(32)}};
  const peer = {workspace: 'WORKSPACE', origin: 'https://rooms.example', userPublicKey: 'cd'.repeat(32),
    whenReady: async () => {}, mountDesktop: async () => ({navigation})};
  const window = {
    __tiloLinkSecrets: Object.freeze({invite: key}),
    location: {pathname: `/i/${handle}`, href: `${site}/i/${handle}#${key}`, hash: `#${key}`, host: 'tilo.sh'},
    history: {state: null, replaceState: (state, title, url) => replaced.push(url), pushState() {}},
    addEventListener() {}, removeEventListener() {},
  };
  const deps = {
    loadRuntime: async () => runtime,
    fetch: async () => ({ok: true, json: async () => ({home: {name: 'Lounge', host: {origin: 'https://rooms.example'}}})}),
    // The site keeps the invitation, not the link: the page opens it there,
    // with the handle and the key the fragment carried.
    openInvitation: async (at, secret) => { opened.push([at, secret]); return {invitation: 'OPENED', host: 'https://rooms.example'}; },
    inspectInvitation: async (origin, invitation) => { inspected.push([origin, invitation]); return offer; },
    confirmInvitation: async shown => { confirmed.push(shown); return true; },
    joinWorkspace: async options => { joined.push(options); return peer; },
  };
  const page = await boot({mode: 'desktop', window, deps});
  assert.equal(parsed, false, 'a sealed link reads no route of its own');
  assert.deepEqual(opened, [[site, handle + key]]);
  assert.deepEqual(inspected, [['https://rooms.example', 'OPENED']]);
  assert.deepEqual(confirmed, [offer], 'the person is shown the invitation before it is joined');
  assert.equal(joined[0].origin, 'https://rooms.example');
  assert.equal(joined[0].invitation, 'OPENED');
  assert.deepEqual(formatted[0], {mode: 'desktop', target: {scope: {kind: 'room', host: 'https://rooms.example', room: 'WORKSPACE'}}});
  assert.equal(replaced[0], `${site}/#/@rooms.example/w/WORKSPACE`);
  page.router.stop();
});

test('a workspace mount keeps its invite sheet alongside host, About and title forwarding', async () => {
  const home = {name: 'Commons', host: {origin: 'https://rooms.example'}, admission: 'https://rooms.example/'};
  const about = {source: 'https://example.org/source'};
  const navigation = {getSnapshot: () => ({generation: 'g', active: null, entries: []}), subscribe: () => () => {}, dispatch: async () => {}};
  let mounted;
  const peer = {workspace: 'WORKSPACE', origin: 'https://rooms.example', userPublicKey: 'ab'.repeat(32),
    whenReady: async () => {}, mountDesktop: async (_element, options) => { mounted = options; return {navigation}; }};
  const window = {
    __tiloLinkSecrets: Object.freeze({}), document: {title: 'Tilo'},
    location: {href: 'https://rooms.example/open/', hash: ''},
    history: {replaceState() {}, pushState() {}}, addEventListener() {}, removeEventListener() {},
  };
  const statuses = [];
  const page = await boot({mode: 'desktop', window, diagnostics: {onStatus: status => statuses.push(status)}, deps: {
    loadRuntime: async () => runtimeFake({formatLink: () => 'https://rooms.example/open/#/WORKSPACE', ...entering('door')}),
    fetch: async () => ({ok: true, json: async () => ({home, about})}),
    enterViaHost: async () => peer,
    // The invite sheet this page's desktop shows, made through the workspace's host.
    invite: ({peer: joined}) => joined?.workspace ? {base: 'https://rooms.example/open/', origin: joined.origin,
      workspace: joined.workspace, name: 'Reader', user: joined.userPublicKey} : null,
  }});
  assert.deepEqual(mounted.invite, {base: 'https://rooms.example/open/', origin: 'https://rooms.example',
    workspace: 'WORKSPACE', name: 'Reader', user: 'ab'.repeat(32)});
  assert.equal(mounted.host, 'rooms.example');
  assert.deepEqual(mounted.about, about);
  assert.equal(mounted.presenceUrl, undefined, 'presence travels over the session, not a door route');
  mounted.onStatus({workspaceName: 'Commons'});
  assert.equal(window.document.title, 'Tilo · Commons');
  assert.deepEqual(statuses, [{workspaceName: 'Commons'}]);
  page.router.stop();
});

test('a bare entry under a home workspace asks the host’s door once the runtime says that is the way in', async () => {
  const home = {name: 'Commons', host: {origin: 'https://rooms.example'}, admission: 'https://rooms.example/'};
  const order = [];
  let loaded;
  const runtime = runtimeFake({
    parseLink: () => { throw new Error('a home entry needs no grammar to be admitted'); },
    formatLink: () => 'https://rooms.example/open/#/@rooms.example/w/WORKSPACE',
    ...entering('door'),
  });
  const navigation = {getSnapshot: () => ({generation: 'g', active: null, entries: []}), subscribe: () => () => {}, dispatch: async () => {}};
  const peer = {workspace: 'WORKSPACE', whenReady: async () => {}, mountDesktop: async () => { order.push('mount'); return {navigation}; }};
  const window = {
    __tiloLinkSecrets: Object.freeze({}),
    location: {href: 'https://rooms.example/open/', hash: ''},
    history: {state: null, replaceState: () => order.push('canonical'), pushState() {}},
    addEventListener() {}, removeEventListener() {},
  };
  const deps = {
    loadRuntime: () => new Promise(resolve => { loaded = () => { order.push('runtime'); resolve(runtime); }; }),
    fetch: async () => { loaded(); return {ok: true, json: async () => ({home})}; },
    enterViaHost: async () => {
      order.push('door');
      return peer;
    },
  };
  const page = await boot({mode: 'desktop', window, deps});
  // The router then keeps the address canonical as it goes.
  assert.deepEqual(order.slice(0, 4), ['runtime', 'door', 'canonical', 'mount']);
  page.router.stop?.();
});

test('any other link waits for the runtime’s grammar before admission', async () => {
  const order = [];
  const runtime = runtimeFake({
    parseLink: () => { order.push('parse'); return JSON.stringify({mode: 'desktop', target: {scope: {kind: 'own'}}}); },
    formatLink: () => 'https://tilo.so/#/me',
    ...entering('own-desktop'),
  });
  const window = {
    __tiloLinkSecrets: Object.freeze({}),
    location: {href: 'https://tilo.so/#/me', hash: '#/me'},
    history: {state: null, replaceState() {}, pushState() {}},
    addEventListener() {}, removeEventListener() {},
  };
  const navigation = {getSnapshot: () => ({generation: 'g', active: null, entries: []}), subscribe: () => () => {}, dispatch: async () => {}};
  const deps = {
    loadRuntime: async () => { order.push('runtime'); return runtime; },
    fetch: async () => ({ok: false}),
    mountDesktop: async () => { order.push('mount'); return {navigation}; },
  };
  const page = await boot({mode: 'desktop', window, deps});
  assert.deepEqual(order, ['runtime', 'parse', 'mount']);
  page.router.stop?.();
});

test('the home workspace’s app in front is fetched once admitted, never before or when refused', async () => {
  const home = {name: 'Commons', host: {origin: 'https://rooms.example'}, admission: 'https://rooms.example/', app: 'so.tilo.chat'};
  const run = async enterViaHost => {
    const order = [];
    const window = {
      __tiloLinkSecrets: Object.freeze({}),
      location: {href: 'https://rooms.example/open/', hash: ''},
      history: {state: null, replaceState() {}, pushState() {}},
      addEventListener() {}, removeEventListener() {},
    };
    const deps = {
      loadRuntime: async () => runtimeFake({...entering('door')}),
      prefetchApplication: app => order.push(`prefetch ${app}`),
      fetch: async () => ({ok: true, json: async () => ({home})}),
      enterViaHost: async () => { order.push('door'); return enterViaHost(); },
    };
    // An admitted boot goes on past what this asks of it.
    void boot({mode: 'desktop', window, deps}).catch(error => order.push(error.message));
    await new Promise(resolve => setTimeout(resolve, 10));
    return order;
  };
  assert.deepEqual((await run(async () => ({workspace: 'r1'}))).slice(0, 2), ['door', 'prefetch so.tilo.chat']);
  assert.deepEqual(await run(async () => { throw new Error('closed'); }), ['door', 'closed']);
});

test('only a bare desktop entry under a home reopens the workspace the person last chose', () => {
  const home = {name: 'Lounge', host: {origin: 'https://rooms.example'}, admission: 'https://rooms.example/'};
  const chosen = {origin: 'https://acme.example', room: 'A'};
  const bare = {hash: '', href: 'https://tilo.example/'};
  assert.deepEqual(chosenRoute({location: bare, mode: 'desktop', home, chosen}),
    {mode: 'desktop', target: {scope: {kind: 'room', host: 'https://acme.example', room: 'A'}}});
  assert.equal(chosenRoute({location: {hash: '#/@rooms.example/w/X', href: 'https://tilo.example/#/@rooms.example/w/X'}, mode: 'desktop', home, chosen}), null);
  assert.equal(chosenRoute({location: bare, mode: 'app', home, chosen}), null);
  assert.equal(chosenRoute({location: bare, mode: 'desktop', home: null, chosen}), null);
  assert.equal(chosenRoute({location: bare, mode: 'desktop', home, chosen: null}), null);
});

function workspacePage({href, hash, parse}) {
  const listeners = {};
  const calls = {reloads: 0, dispatched: [], scopes: []};
  const navigation = {
    getSnapshot: () => ({generation: 'g', active: null, entries: []}),
    subscribe: () => () => {},
    dispatch: async command => { calls.dispatched.push(command); },
  };
  const window = {
    __tiloLinkSecrets: Object.freeze({}),
    location: {href, hash, reload: () => { calls.reloads += 1; }},
    history: {state: null, replaceState() {}, pushState() {}},
    addEventListener: (type, listener) => { listeners[type] = listener; },
    removeEventListener() {},
  };
  const runtime = runtimeFake({parseLink: url => JSON.stringify(parse(url)), formatLink: () => window.location.href,
    ...entering('accounts', 'member')});
  return {window, listeners, calls, runtime, navigation};
}

test('a returning person’s bare entry opens the chosen workspace through their membership, not the home’s door', async () => {
  const home = {name: 'Lounge', host: {origin: 'https://rooms.example'}, admission: 'https://rooms.example/'};
  const page = workspacePage({href: 'https://tilo.example/', hash: '', parse: () => { throw new Error('a chosen room needs no parse'); }});
  const connected = [];
  const peer = {workspace: 'A', whenReady: async () => {}, mountDesktop: async (element, options) => { options.onNavigation?.(page.navigation); return {navigation: page.navigation}; }};
  const deps = {
    loadRuntime: async () => page.runtime,
    fetch: async () => ({ok: true, json: async () => ({home})}),
    chosenWorkspace: () => ({origin: 'https://acme.example', room: 'A'}),
    onScope: seen => page.calls.scopes.push(seen),
    readAccounts: async origin => [{userPublicKey: 'ab', origin}],
    connectMember: async options => { connected.push(options); return peer; },
    enterViaHost: async () => { throw new Error('the home’s door is not asked'); },
  };
  const shown = await boot({mode: 'desktop', window: page.window, deps});
  assert.deepEqual(connected.map(({origin, room}) => [origin, room]), [['https://acme.example', 'A']]);
  assert.deepEqual(page.calls.scopes, [{scope: {kind: 'room', host: 'https://acme.example', room: 'A'}, entry: {base: 'https://tilo.example/', home}}]);
  assert.deepEqual(shown.router.route.target.scope, {kind: 'room', host: 'https://acme.example', room: 'A'});
  shown.router.stop();
});

test('Back or Forward to another workspace loads it anew, never inside the workspace in front', async () => {
  const scopes = {
    '#/@acme.example/w/A': {kind: 'room', host: 'https://acme.example', room: 'A'},
    '#/@globex.example/w/G': {kind: 'room', host: 'https://globex.example', room: 'G'},
  };
  const page = workspacePage({href: 'https://tilo.example/#/@acme.example/w/A', hash: '#/@acme.example/w/A',
    parse: url => ({mode: 'desktop', target: {scope: scopes[new URL(url).hash]}})});
  const peer = {workspace: 'A', whenReady: async () => {}, mountDesktop: async (element, options) => { options.onNavigation?.(page.navigation); return {navigation: page.navigation}; }};
  const deps = {
    loadRuntime: async () => page.runtime, fetch: async () => ({ok: false}),
    readAccounts: async () => [{userPublicKey: 'ab'}], connectMember: async () => peer,
  };
  const shown = await boot({mode: 'desktop', window: page.window, deps});
  // The same workspace: shown in this desktop, no reload.
  await page.listeners.popstate();
  assert.equal(page.calls.reloads, 0);
  page.window.location.href = 'https://tilo.example/#/@globex.example/w/G';
  await page.listeners.popstate();
  assert.equal(page.calls.reloads, 1);
  assert.deepEqual(page.calls.dispatched, []);
  shown.router.stop();
});

test('once admitted, the page names its host’s fetch proxy for the apps', async () => {
  const home = {name: 'Commons', host: {origin: 'https://rooms.example'}, admission: 'https://rooms.example/'};
  const runtime = runtimeFake({
    parseLink: () => { throw Object.assign(new Error('no route'), {code: 'no-route'}); },
    formatLink: () => 'https://app.example/#/@rooms.example/w/WORKSPACE',
    ...entering('door'),
  });
  const navigation = {getSnapshot: () => ({generation: 'g', active: null, entries: []}), subscribe: () => () => {}, dispatch: async () => {}};
  let session = 'feed';
  const peer = {
    room: 'WORKSPACE', whenReady: async () => {},
    fetchProxy: () => (session ? {endpoint: 'https://rooms.example/peer/v1/fetch', session} : null),
    mountDesktop: async (element, options) => { options.onNavigation?.(navigation); return {navigation}; },
  };
  const window = {
    __tiloLinkSecrets: Object.freeze({}),
    location: {href: 'https://app.example/', hash: ''},
    history: {state: null, replaceState() {}, pushState() {}},
    addEventListener() {}, removeEventListener() {},
  };
  const deps = {
    loadRuntime: async () => runtime,
    fetch: async () => ({ok: true, json: async () => ({home})}),
    readAccounts: async () => [],
    enterViaHost: async () => {
      assert.equal(window.__tiloFetchProxy, undefined, 'no proxy before admission');
      return peer;
    },
  };
  const page = await boot({mode: 'desktop', window, deps});
  assert.deepEqual(window.__tiloFetchProxy(), {endpoint: 'https://rooms.example/peer/v1/fetch', session: 'feed'});
  session = null;
  assert.equal(window.__tiloFetchProxy(), null);
  page.router.stop?.();
});

test('the home workspace sends reports to the channel its entry names, without holding the page', async () => {
  const asked = [];
  let settle;
  const peer = {origin: 'https://lounge.example', workspace: 'r1', configureFeedback: destination => {
    asked.push(destination);
    return new Promise((_, reject) => { settle = reject; });
  }};
  const home = {host: {origin: 'https://lounge.example'}, feedback: {channel: 'c_feedback'}};
  configureFeedback(peer, home, 'r1');
  assert.deepEqual(asked, [{room: 'r1', provider: 'record:so.tilo.chat:', channel: 'c_feedback'}]);
  const warn = console.warn;
  console.warn = () => {};
  settle(new Error('owner refused'));
  await new Promise(resolve => setTimeout(resolve, 0));
  console.warn = warn;
  configureFeedback(peer, {});
  configureFeedback({configureFeedback: peer.configureFeedback}, home);
  assert.equal(asked.length, 1, 'no channel, or no room, asks nothing');
  // Another workspace on the home's host, or a workspace elsewhere, has no such channel.
  configureFeedback({...peer, workspace: 'second'}, home, null);
  configureFeedback({...peer, origin: 'https://team.example'}, home, 'r1');
  assert.equal(asked.length, 1, 'only the home workspace asks');
});

test('reports from another workspace reach the home workspace’s channel through its peer there', async () => {
  const home = {name: 'Lounge', room: 'LOUNGE', host: {origin: 'https://rooms.example'}, admission: 'https://rooms.example/',
    feedback: {channel: 'c_feedback'}};
  const page = workspacePage({href: 'https://tilo.example/', hash: '', parse: () => { throw new Error('a chosen room needs no parse'); }});
  const through = [];
  const asked = [];
  let configured;
  const done = new Promise(resolve => { configured = resolve; });
  const lounge = {origin: 'https://rooms.example', workspace: 'LOUNGE', configureFeedback: async destination => {
    asked.push(destination);
    configured();
  }};
  const peer = {origin: 'https://acme.example', workspace: 'A', whenReady: async () => {},
    configureFeedback: async () => { throw new Error('the room in front has no such channel'); },
    sendFeedbackThrough: carrier => through.push(carrier),
    mountDesktop: async (element, options) => { options.onNavigation?.(page.navigation); return {navigation: page.navigation}; }};
  const homes = [];
  const deps = {
    loadRuntime: async () => page.runtime,
    fetch: async () => ({ok: true, json: async () => ({home})}),
    chosenWorkspace: () => ({origin: 'https://acme.example', room: 'A'}),
    readAccounts: async origin => [{userPublicKey: 'ab', origin}],
    connectMember: async () => peer,
    homePeer: async workspace => { homes.push(workspace.host.origin); return lounge; },
  };
  const shown = await boot({mode: 'desktop', window: page.window, deps});
  await done;
  assert.deepEqual(homes, ['https://rooms.example'], 'the home workspace’s peer is asked for once');
  assert.deepEqual(through, [lounge], 'the desktop’s reports go through it');
  assert.deepEqual(asked, [{room: 'LOUNGE', provider: 'record:so.tilo.chat:', channel: 'c_feedback'}],
    'to the home workspace’s own channel, not the room in front');
  shown.router.stop();
});

test('without a peer in the home workspace, another workspace sends no reports anywhere', async () => {
  const home = {host: {origin: 'https://lounge.example'}, feedback: {channel: 'c_feedback'}};
  const asked = [];
  const peer = {origin: 'https://lounge.example', workspace: 'own', configureFeedback: async destination => { asked.push(destination); },
    sendFeedbackThrough: () => asked.push('through')};
  let disposed = 0;
  const watching = {origin: 'https://lounge.example', workspace: null, dispose: async () => { disposed++; }};
  configureFeedback(peer, home, null, {homePeer: async () => null});
  configureFeedback(peer, home, null, {homePeer: async () => watching});
  configureFeedback({...peer, sendFeedbackThrough: undefined}, home, null, {homePeer: async () => { throw new Error('not asked'); }});
  await new Promise(resolve => setTimeout(resolve, 0));
  assert.deepEqual(asked, []);
  assert.equal(disposed, 1, 'a peer that cannot carry reports is let go');
});

test('a second workspace on the home’s host goes by no name or count of the home’s', async () => {
  const {listedAt} = await import('./workspaces.mjs');
  const home = {name: 'Lounge', host: {origin: 'https://lounge.example'}};
  const none = {getItem: () => null};
  assert.equal(listedAt([home], {kind: 'room', host: 'https://lounge.example', room: 'r1'}, 'r1', none), home);
  assert.equal(listedAt([home], {kind: 'room', host: 'https://lounge.example', room: 'second'}, null, none), null);
  assert.equal(listedAt([home], {kind: 'room', host: 'https://lounge.example', room: null}, null, none), home);
  assert.equal(listedAt([{...home, room: 'r1'}], {kind: 'room', host: 'https://lounge.example', room: 'r1'}, null, none).name, 'Lounge');
  assert.equal(listedAt([home], {kind: 'own'}, null, none), null);
});

test('the address names the app in front, and the view a link opened, as a link to it would', () => {
  const scope = {kind: 'room', host: 'https://acme.example', room: 'A'};
  assert.deepEqual(shownLink({scope}), {mode: 'desktop', target: {scope}});
  assert.deepEqual(shownLink({scope, app: 'so.tilo.chat'}), {mode: 'desktop', target: {scope, app: 'so.tilo.chat'}});
  assert.deepEqual(shownLink({scope, app: 'so.tilo.notes', record: 'n1'}),
    {mode: 'desktop', target: {scope, app: 'so.tilo.notes', record: 'n1'}});
  // A window a link's resource opened: its view, never the link's secrets.
  assert.deepEqual(shownLink({scope, app: 'so.tilo.chat', record: 'c1', resource: {kind: 'chat.message', id: 'chat.message:1'},
    view: 'thread', ticket: 'secret'}), {mode: 'desktop', target: {scope, app: 'so.tilo.chat', record: 'c1', view: 'thread'}});
});

test('a link opened inside the desktop names its view in the address, and focus elsewhere takes it back', async () => {
  const scope = {kind: 'room', host: 'https://acme.example', room: 'A'};
  const opened = {scope, app: 'so.tilo.chat', record: 'c1', resource: message};
  const page = workspacePage({href: 'https://acme.example/open/#/@acme.example/w/A', hash: '#/@acme.example/w/A',
    parse: () => ({mode: 'desktop', target: {scope}})});
  const written = [];
  page.runtime.formatLink = route => { written.push(JSON.parse(route).target); return `https://acme.example/open/#${written.length}`; };
  // The app's window is in front from the start, so showing the link's
  // resource needs no navigation command.
  const inFront = {workspace: '1', strip: '1', group: '7', tab: '8'};
  const snapshot = {generation: 'g', active: inFront, entries: [{route: inFront, app: 'so.tilo.chat'}]};
  const listeners = new Set();
  const navigation = {
    getSnapshot: () => snapshot,
    subscribe: listener => { listeners.add(listener); return () => listeners.delete(listener); },
    dispatch: async () => {},
  };
  const peer = {workspace: 'A', whenReady: async () => {}, mountDesktop: async (_element, options) => {
    options.onNavigation?.(navigation); return {navigation};
  }};
  const view = {kind: 'read', address: {resource: message.id, action: 'reveal'}, addressKey: 'k', view: {id: 'message', open_action: 'reveal', primary: true}};
  page.runtime.parseLink = () => JSON.stringify({mode: 'desktop', target: {scope}});
  const page_ = await boot({mode: 'desktop', window: page.window, deps: {
    loadRuntime: async () => page.runtime, fetch: async () => ({ok: false}),
    readAccounts: async () => [{userPublicKey: 'ab'}], connectMember: async () => peer,
  }});
  page.runtime.parseLink = () => JSON.stringify({mode: 'desktop', target: opened});
  page.runtime.formatLink = route => { written.push(JSON.parse(route).target); return `https://acme.example/open/#${written.length}`; };
  page_.mount.actions = () => [view];
  page_.mount.invokeAction = () => ({state: 'completed'});
  // Back to the link that names the view.
  page.window.location.hash = '#/@acme.example/w/A/so.tilo.chat/-/chat.message:1';
  // A typed fragment is a new address; the fake page's `href` stands for it.
  page.window.location.href = 'https://acme.example/open/#/@acme.example/w/A/so.tilo.chat/-/chat.message:1';
  await page.listeners.popstate();
  await new Promise(resolve => setImmediate(resolve));
  assert.deepEqual(written.at(-1), {scope, app: 'so.tilo.chat', record: 'c1'}, 'the address now names the view the link opened');
  // The person opens another window: the address is that window's again.
  const other = {workspace: '1', strip: '1', group: '9', tab: '10'};
  Object.assign(snapshot, {active: other, entries: [...snapshot.entries, {route: other, app: 'so.tilo.notes'}]});
  for (const listener of [...listeners]) listener(snapshot);
  assert.deepEqual(written.at(-1), {scope, app: 'so.tilo.notes'});
  page_.router.stop();
});

test('a link to an app’s workspace document nobody wrote yet opens the app; any other missing record is not there', async () => {
  const scope = {kind: 'room', host: 'https://h.example', room: 'r'};
  const asked = [];
  const peer = {
    whenRecord: async (app, record, options) => {
      asked.push([app, record, options?.timeoutMs]);
      throw Object.assign(new Error(`the workspace sent no ${app} record ${record}`), {code: 'no-record'});
    },
    mountDesktop: async (_surface, options) => ({options}),
  };
  const shown = await present({route: {mode: 'app', target: {scope, app: 'so.tilo.scratchpad', record: ''}}, peer});
  assert.equal(shown.mount.options.app, 'so.tilo.scratchpad');
  assert.deepEqual(asked, [['so.tilo.scratchpad', '', WORKSPACE_DOCUMENT_WAIT_MS]]);
  await assert.rejects(present({route: {mode: 'app', target: {scope, app: 'so.tilo.word-processor', record: 'uuid:1'}}, peer}),
    {message: UNAVAILABLE});
  // A connection that went away is not a record that is missing.
  const gone = {...peer, whenRecord: async () => { throw new Error('Shared resource connection closed'); }};
  await assert.rejects(present({route: {mode: 'app', target: {scope, app: 'so.tilo.scratchpad', record: ''}}, peer: gone}),
    /connection closed/);
});
