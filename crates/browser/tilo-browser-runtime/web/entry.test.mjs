import assert from 'node:assert/strict';
import test from 'node:test';
import {boot, chosenRoute, initialRoute, readEntry, scopeHost} from './entry.mjs';
import {UNAVAILABLE, openTarget, untilSnapshot, untilViewable, viewInvocation} from './open-target.mjs';

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
      queueMicrotask(() => show({generation: 'g1', active: route, entries: [...windows, {route, app: command.app}]}));
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
  assert.deepEqual(commands, [{kind: 'open', app: 'so.tilo.chat', generation: 'g1'}]);
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

test('a bare entry opens the web host’s home workspace, its room learned on admission', () => {
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

test('the catalogue is never read before admission', async () => {
  const order = [];
  const target = {scope: {kind: 'room', host: 'https://h.example', room: 'r'}, app: 'so.tilo.chat', record: '', resource: message};
  const runtime = {
    parseLink: () => JSON.stringify({mode: 'app', target}),
    formatLink: () => 'https://tilo.so/app/#/@h.example/w/r/so.tilo.chat/-/chat.message:1',
  };
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
  const runtime = {
    parseLink: () => { throw Object.assign(new Error('no route'), {code: 'no-route'}); },
    formatLink: route => { formatted.push(JSON.parse(route)); return 'https://rooms.example/open/#/@rooms.example/w/ROOM'; },
  };
  const navigation = {getSnapshot: () => ({generation: 'g', active: null, entries: []}), subscribe: () => () => {}, dispatch: async () => {}};
  const mounts = [];
  const peer = {room: 'ROOM', whenReady: async () => {}, mountDesktop: async (element, options) => {
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
  assert.deepEqual(formatted[0], {mode: 'desktop', target: {scope: {kind: 'room', host: 'https://rooms.example', room: 'ROOM'}}});
  assert.equal(replaced[0], 'https://rooms.example/open/#/@rooms.example/w/ROOM');
  assert.equal(mounts[0].workspaceName, 'Commons');
  assert.equal(mounts[0].host, 'rooms.example');
  page.router.stop?.();
});

test('the page hears the peer the host let in before its desktop mounts, so a failed mount can name the room', async () => {
  const home = {name: 'Lounge', host: {origin: 'https://rooms.example'}, admission: 'https://rooms.example/'};
  const runtime = {
    parseLink: () => { throw Object.assign(new Error('no route'), {code: 'no-route'}); },
    formatLink: () => 'https://rooms.example/#/@rooms.example/w/ROOM',
  };
  const heard = [];
  const peer = {room: 'ROOM', workspaceName: 'Kaffeeküche', whenReady: async () => {},
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

test('a room on the site the page came from, or on its machines, is hosted by that site', () => {
  const room = host => ({kind: 'room', host, room: 'R'});
  assert.equal(scopeHost(room('https://eu1.tilo.coffee'), 'tilo.coffee'), 'tilo.coffee');
  assert.equal(scopeHost(room('https://tilo.coffee'), 'tilo.coffee'), 'tilo.coffee');
  assert.equal(scopeHost(room('https://rooms.example'), 'tilo.coffee'), 'rooms.example');
  assert.equal(scopeHost(room('https://notilo.coffee'), 'tilo.coffee'), 'notilo.coffee');
  assert.equal(scopeHost({kind: 'own'}, 'tilo.coffee'), null);
  // A room on another port of the page's machine is the site, by its name.
  assert.equal(scopeHost(room('http://localhost:9764'), 'localhost:8411'), 'localhost');
  assert.equal(scopeHost(room('https://rooms.example:8443'), 'tilo.coffee'), 'rooms.example:8443');
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
  const runtime = {parseLink: () => JSON.stringify({mode: 'desktop', target: {scope: {kind: 'own'}}}), formatLink: () => ''};
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

test('a room mount keeps Invite alongside host, About, presence and title forwarding', async () => {
  const home = {name: 'Commons', host: {origin: 'https://rooms.example'}, admission: 'https://rooms.example/'};
  const about = {source: 'https://example.org/source'};
  const navigation = {getSnapshot: () => ({generation: 'g', active: null, entries: []}), subscribe: () => () => {}, dispatch: async () => {}};
  let mounted;
  let invited = 0;
  const peer = {room: 'ROOM', whenReady: async () => {}, mountDesktop: async (_element, options) => { mounted = options; return {navigation}; }};
  const window = {
    __tiloLinkSecrets: Object.freeze({}), document: {title: 'Tilo'},
    location: {href: 'https://rooms.example/open/', hash: ''},
    history: {replaceState() {}, pushState() {}}, addEventListener() {}, removeEventListener() {},
  };
  const statuses = [];
  const page = await boot({mode: 'desktop', window, diagnostics: {onStatus: status => statuses.push(status)}, deps: {
    loadRuntime: async () => ({formatLink: () => 'https://rooms.example/open/#/ROOM'}),
    fetch: async () => ({ok: true, json: async () => ({home, about})}),
    enterViaHost: async () => peer, onInvite: () => { invited += 1; },
  }});
  mounted.onInvite();
  assert.equal(invited, 1);
  assert.equal(mounted.host, 'rooms.example');
  assert.deepEqual(mounted.about, about);
  assert.equal(mounted.presenceUrl, 'https://rooms.example/presence');
  mounted.onStatus({workspaceName: 'Commons'});
  assert.equal(window.document.title, 'Tilo · Commons');
  assert.deepEqual(statuses, [{workspaceName: 'Commons'}]);
  page.router.stop();
});

test('a bare entry under a home workspace asks the host’s door while the runtime still loads', async () => {
  const home = {name: 'Commons', host: {origin: 'https://rooms.example'}, admission: 'https://rooms.example/'};
  const order = [];
  let loaded;
  const runtime = {
    parseLink: () => { throw new Error('a home entry needs no grammar to be admitted'); },
    formatLink: () => 'https://rooms.example/open/#/@rooms.example/w/ROOM',
  };
  const navigation = {getSnapshot: () => ({generation: 'g', active: null, entries: []}), subscribe: () => () => {}, dispatch: async () => {}};
  const peer = {room: 'ROOM', whenReady: async () => {}, mountDesktop: async () => { order.push('mount'); return {navigation}; }};
  const window = {
    __tiloLinkSecrets: Object.freeze({}),
    location: {href: 'https://rooms.example/open/', hash: ''},
    history: {state: null, replaceState: () => order.push('canonical'), pushState() {}},
    addEventListener() {}, removeEventListener() {},
  };
  const deps = {
    loadRuntime: () => new Promise(resolve => { loaded = () => { order.push('runtime'); resolve(runtime); }; }),
    fetch: async () => ({ok: true, json: async () => ({home})}),
    enterViaHost: async () => {
      order.push('door');
      // The door's own wait for the runtime, before its first certificate.
      loaded();
      return peer;
    },
  };
  const page = await boot({mode: 'desktop', window, deps});
  // The router then keeps the address canonical as it goes.
  assert.deepEqual(order.slice(0, 4), ['door', 'runtime', 'canonical', 'mount']);
  page.router.stop?.();
});

test('any other link waits for the runtime’s grammar before admission', async () => {
  const order = [];
  const runtime = {
    parseLink: () => { order.push('parse'); return JSON.stringify({mode: 'desktop', target: {scope: {kind: 'own'}}}); },
    formatLink: () => 'https://tilo.so/#/me',
  };
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
      loadRuntime: () => new Promise(() => {}),
      prefetchApplication: app => order.push(`prefetch ${app}`),
      fetch: async () => ({ok: true, json: async () => ({home})}),
      enterViaHost: async () => { order.push('door'); return enterViaHost(); },
    };
    // An admitted boot goes on to wait for the runtime, which never loads here.
    void boot({mode: 'desktop', window, deps}).catch(error => order.push(error.message));
    await new Promise(resolve => setTimeout(resolve, 10));
    return order;
  };
  assert.deepEqual(await run(async () => ({room: 'r1'})), ['door', 'prefetch so.tilo.chat']);
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

function roomPage({href, hash, parse}) {
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
  const runtime = {parseLink: url => JSON.stringify(parse(url)), formatLink: () => window.location.href};
  return {window, listeners, calls, runtime, navigation};
}

test('a returning person’s bare entry opens the chosen room through their membership, not the home’s door', async () => {
  const home = {name: 'Lounge', host: {origin: 'https://rooms.example'}, admission: 'https://rooms.example/'};
  const page = roomPage({href: 'https://tilo.example/', hash: '', parse: () => { throw new Error('a chosen room needs no parse'); }});
  const connected = [];
  const peer = {room: 'A', whenReady: async () => {}, mountDesktop: async (element, options) => { options.onNavigation?.(page.navigation); return {navigation: page.navigation}; }};
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

test('Back or Forward to another room loads it anew, never inside the room in front', async () => {
  const scopes = {
    '#/@acme.example/w/A': {kind: 'room', host: 'https://acme.example', room: 'A'},
    '#/@globex.example/w/G': {kind: 'room', host: 'https://globex.example', room: 'G'},
  };
  const page = roomPage({href: 'https://tilo.example/#/@acme.example/w/A', hash: '#/@acme.example/w/A',
    parse: url => ({mode: 'desktop', target: {scope: scopes[new URL(url).hash]}})});
  const peer = {room: 'A', whenReady: async () => {}, mountDesktop: async (element, options) => { options.onNavigation?.(page.navigation); return {navigation: page.navigation}; }};
  const deps = {
    loadRuntime: async () => page.runtime, fetch: async () => ({ok: false}),
    readAccounts: async () => [{userPublicKey: 'ab'}], connectMember: async () => peer,
  };
  const shown = await boot({mode: 'desktop', window: page.window, deps});
  // The same room: shown in this desktop, no reload.
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
  const runtime = {
    parseLink: () => { throw Object.assign(new Error('no route'), {code: 'no-route'}); },
    formatLink: () => 'https://app.example/#/@rooms.example/w/ROOM',
  };
  const navigation = {getSnapshot: () => ({generation: 'g', active: null, entries: []}), subscribe: () => () => {}, dispatch: async () => {}};
  let session = 'feed';
  const peer = {
    room: 'ROOM', whenReady: async () => {},
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
