import assert from 'node:assert/strict';
import test from 'node:test';
import {boot, initialRoute, readEntry} from './entry.mjs';
import {UNAVAILABLE, openTarget, viewInvocation} from './open-target.mjs';

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
  const page = await boot({mode: 'desktop', window, status, deps});
  assert.deepEqual(entered, [home]);
  // Progress while joining, then the loading note again, which the first drawn frame clears.
  assert.ok(shown.includes('Joining…'), shown.join(' | '));
  assert.equal(status.text, 'Loading…');
  assert.deepEqual(formatted[0], {mode: 'desktop', target: {scope: {kind: 'room', host: 'https://rooms.example', room: 'ROOM'}}});
  assert.equal(replaced[0], 'https://rooms.example/open/#/@rooms.example/w/ROOM');
  assert.equal(mounts[0].workspaceName, 'Commons');
  page.router.stop?.();
});
