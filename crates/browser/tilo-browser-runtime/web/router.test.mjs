import assert from 'node:assert/strict';
import test from 'node:test';
import {FOCUS_SETTLE_MS, startRouter} from './router.mjs';

const BASE = 'https://tilo.so/';
// A stand-in for the runtime's Rust grammar: the router never reads routes.
const link = {
  format: ({mode, target}) => `${BASE}${mode === 'app' ? 'app/' : ''}#${encodeURIComponent(JSON.stringify(target))}`,
  parse: url => {
    const parsed = new URL(url);
    return {mode: parsed.pathname.endsWith('/app/') ? 'app' : 'desktop', target: JSON.parse(decodeURIComponent(parsed.hash.slice(1)))};
  },
};

function fakeWindow(href) {
  const listeners = new Map();
  const entries = [{url: href, state: null}];
  let index = 0;
  const window = {
    location: {get href() { return entries[index].url; }, reload() { window.reloaded = true; }},
    history: {
      pushState(state, _title, url) { entries.splice(index + 1); entries.push({url, state}); index += 1; window.writes.push(['push', url]); },
      replaceState(state, _title, url) { entries[index] = {url, state}; window.writes.push(['replace', url]); },
    },
    writes: [],
    entries,
    get index() { return index; },
    addEventListener(type, fn) { listeners.set(type, fn); },
    removeEventListener(type) { listeners.delete(type); },
    // A fragment typed into the address bar: a new entry, announced as a
    // browser does (popstate, then hashchange).
    edit(url) {
      entries.splice(index + 1);
      entries.push({url, state: null});
      return traverse(index + 1, entries[index].url);
    },
    back() { return traverse(index - 1); },
    forward() { return traverse(index + 1); },
  };
  // As a browser goes back or forward within the page: popstate, and when
  // the fragment changed, hashchange as well, in a task of its own that does
  // not wait for what the popstate listener started.
  function traverse(to, from = entries[index].url) {
    index = to;
    const heard = [listeners.get('popstate')?.()];
    if (new URL(from).hash !== new URL(entries[index].url).hash) heard.push(listeners.get('hashchange')?.());
    return Promise.all(heard);
  }
  return window;
}

function fakeNavigation(snapshot) {
  const listeners = new Set();
  return {
    getSnapshot: () => snapshot,
    subscribe(listener) { listeners.add(listener); return () => listeners.delete(listener); },
    emit(next) { snapshot = next; for (const listener of [...listeners]) listener(next); },
  };
}

const scope = {kind: 'room', host: 'https://team.example', room: '0b5e7a62-4d0f-4c55-9d2a-58d3a3f0c001'};
const route = (tab, extra = {}) => ({workspace: '1', strip: '2', group: `g${tab}`, tab, ...extra});
const desk = (windows, active) => ({
  generation: 'g', active: active === undefined ? null : route(active),
  entries: windows.map(([tab, app]) => ({route: route(tab), app, label: app})),
});
const chat = {scope, app: 'so.tilo.chat', record: '', resource: {kind: 'chat.channel', id: 'chat.channel:general'}};

test('a local open is one history entry', async () => {
  const navigation = fakeNavigation(desk([['1', 'so.tilo.notes']], '1'));
  const window = fakeWindow(link.format({mode: 'desktop', target: {scope, app: 'so.tilo.notes'}}));
  const router = startRouter({window, link, navigation, initial: {mode: 'desktop', target: {scope, app: 'so.tilo.notes'}},
    show: async () => { navigation.emit(desk([['1', 'so.tilo.notes'], ['2', 'so.tilo.chat']], '2')); }});
  window.writes.length = 0;
  await router.open({mode: 'desktop', target: chat});
  assert.deepEqual(window.writes, [['push', link.format({mode: 'desktop', target: chat})]],
    'opening launched a window, and the open is still one history entry');
  assert.equal(window.entries.length, 2);
  router.stop();
});

/** A clock the test moves by hand. */
function heldClock() {
  let at = 0;
  return {now: () => at, wait: ms => { at += ms; }};
}

test('Back and Forward move between the windows this person went to', async () => {
  const clock = heldClock();
  const windows = [['1', 'so.tilo.notes'], ['2', 'so.tilo.chat'], ['3', 'so.tilo.weather']];
  const notes = {scope, app: 'so.tilo.notes'};
  const weather = {scope, app: 'so.tilo.weather'};
  const navigation = fakeNavigation(desk(windows, '1'));
  const window = fakeWindow(link.format({mode: 'desktop', target: notes}));
  const shown = [];
  const router = startRouter({window, link, navigation, now: clock.now, initial: {mode: 'desktop', target: notes},
    show: async next => {
      shown.push(next.target.app);
      const tab = windows.find(([, app]) => app === next.target.app)[0];
      navigation.emit(desk(windows, tab));
    }});
  clock.wait(5000);
  navigation.emit(desk(windows, '2'));
  clock.wait(5000);
  navigation.emit(desk(windows, '3'));
  assert.deepEqual(window.entries.map(entry => link.parse(entry.url).target.app),
    ['so.tilo.notes', 'so.tilo.chat', 'so.tilo.weather'], 'each window gone to is a step');
  await window.back();
  assert.deepEqual(shown, ['so.tilo.chat'], 'Back shows the window before');
  assert.equal(window.index, 1);
  await window.back();
  assert.deepEqual(shown, ['so.tilo.chat', 'so.tilo.notes']);
  assert.equal(window.entries.length, 3, 'going back writes nothing');
  await window.forward();
  assert.deepEqual(shown.at(-1), 'so.tilo.chat', 'Forward goes the other way');
  assert.deepEqual(router.route, {mode: 'desktop', target: {scope, app: 'so.tilo.chat'}});
  // Going somewhere new from the middle drops what was ahead, as on the web.
  clock.wait(5000);
  navigation.emit(desk(windows, '3'));
  assert.deepEqual(window.entries.map(entry => link.parse(entry.url).target.app),
    ['so.tilo.notes', 'so.tilo.chat', 'so.tilo.weather']);
  assert.deepEqual(link.parse(window.entries.at(-1).url).target, weather);
  router.stop();
});

test('windows passed through on the way are not steps of their own', () => {
  const clock = heldClock();
  const windows = [['1', 'so.tilo.notes'], ['2', 'so.tilo.chat'], ['3', 'so.tilo.weather'], ['4', 'so.tilo.calendar']];
  const navigation = fakeNavigation(desk(windows, '1'));
  const window = fakeWindow(link.format({mode: 'desktop', target: {scope, app: 'so.tilo.notes'}}));
  startRouter({window, link, navigation, now: clock.now, initial: {mode: 'desktop', target: {scope, app: 'so.tilo.notes'}},
    show: async () => {}});
  clock.wait(5000);
  // A spun wheel: Chat and Weather each in front for a moment.
  for (const tab of ['2', '3', '4']) {
    navigation.emit(desk(windows, tab));
    clock.wait(120);
  }
  assert.deepEqual(window.entries.map(entry => link.parse(entry.url).target.app),
    ['so.tilo.notes', 'so.tilo.calendar'], 'one step, to where the person stopped');
  // Having stayed, the next move is a step again.
  clock.wait(FOCUS_SETTLE_MS);
  navigation.emit(desk(windows, '2'));
  assert.equal(window.entries.length, 3);
});

test('a window this person opens on the desktop pushes', () => {
  const navigation = fakeNavigation(desk([['1', 'so.tilo.notes']], '1'));
  const window = fakeWindow(link.format({mode: 'desktop', target: {scope, app: 'so.tilo.notes'}}));
  startRouter({window, link, navigation, initial: {mode: 'desktop', target: {scope, app: 'so.tilo.notes'}}, show: async () => {}});
  window.writes.length = 0;
  navigation.emit(desk([['1', 'so.tilo.notes'], ['3', 'so.tilo.calculator']], '3'));
  assert.deepEqual(window.writes, [['push', link.format({mode: 'desktop', target: {scope, app: 'so.tilo.calculator'}})]]);
});

test('windows others open and changes that keep the focus write nothing', () => {
  const navigation = fakeNavigation(desk([['1', 'so.tilo.chat']], '1'));
  const window = fakeWindow(link.format({mode: 'desktop', target: chat}));
  startRouter({window, link, navigation, initial: {mode: 'desktop', target: chat}, show: async () => {}});
  window.writes.length = 0;
  // A collaborator's agent opened a window on its own strip: focus stays.
  navigation.emit(desk([['1', 'so.tilo.chat'], ['9', 'so.tilo.notes']], '1'));
  // A remote write retitled the window.
  navigation.emit(desk([['1', 'so.tilo.chat'], ['9', 'so.tilo.notes']], '1'));
  assert.deepEqual(window.writes, [], 'the channel stays in the URL while Chat stays in front');
});

test('Back shows the URL target without writing history', async () => {
  const shown = [];
  const first = {scope, app: 'so.tilo.notes'};
  const window = fakeWindow(link.format({mode: 'app', target: first}));
  const router = startRouter({window, link, initial: {mode: 'app', target: first}, show: async next => { shown.push(next); }});
  await router.open({mode: 'app', target: chat});
  window.writes.length = 0;
  await window.back();
  assert.deepEqual(shown.at(-1), {mode: 'app', target: first});
  assert.deepEqual(window.writes, []);
  assert.deepEqual(router.route, {mode: 'app', target: first});
});

test('a move the browser announces twice shows its target once', async () => {
  const shown = [];
  const notes = {scope, app: 'so.tilo.notes'};
  const message = {...chat, resource: {kind: 'chat.message', id: 'chat.message:1'}};
  const window = fakeWindow(link.format({mode: 'desktop', target: notes}));
  const router = startRouter({window, link, initial: {mode: 'desktop', target: notes}, show: async next => { shown.push(next.target); }});
  // A link to a message, typed over the address: its view action runs once.
  await window.edit(link.format({mode: 'desktop', target: message}));
  assert.deepEqual(shown, [message]);
  await window.back();
  assert.deepEqual(shown, [message, notes], 'Back shows the window before, once');
  await window.forward();
  assert.deepEqual(shown, [message, notes, message], 'and Forward the message again, once');
  // Coming back to an address shown before is a move of its own.
  await router.open({mode: 'desktop', target: chat});
  await window.back();
  assert.deepEqual(shown, [message, notes, message, chat, message]);
  router.stop();
});

test('an entry of the other mode loads that page', async () => {
  const window = fakeWindow(link.format({mode: 'desktop', target: chat}));
  window.history.pushState(null, '', link.format({mode: 'app', target: chat}));
  startRouter({window, link, initial: {mode: 'app', target: chat}, show: async () => assert.fail('another page shows it')});
  await window.back();
  assert.equal(window.reloaded, true);
});

test('history entries never hold a secret', async () => {
  const window = fakeWindow(`${BASE}#x`);
  const router = startRouter({window, link, initial: {mode: 'app', target: chat}, show: async () => {}});
  await router.open({mode: 'app', target: {...chat, resource: {kind: 'chat.message', id: 'chat.message:1'}}});
  for (const entry of window.entries) assert.doesNotMatch(entry.url, /invite=|ticket=|token=/);
  assert.equal(router.url('desktop'), link.format({mode: 'desktop', target: router.route.target}));
});
