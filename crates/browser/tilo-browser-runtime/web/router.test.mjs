import assert from 'node:assert/strict';
import test from 'node:test';
import {startRouter} from './router.mjs';

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
    back() { index -= 1; return listeners.get('popstate')?.(); },
  };
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

test('only a local open pushes', async () => {
  const navigation = fakeNavigation(desk([['1', 'so.tilo.notes']], '1'));
  const window = fakeWindow(link.format({mode: 'desktop', target: {scope, app: 'so.tilo.notes'}}));
  const router = startRouter({window, link, navigation, initial: {mode: 'desktop', target: {scope, app: 'so.tilo.notes'}},
    show: async () => { navigation.emit(desk([['1', 'so.tilo.notes'], ['2', 'so.tilo.chat']], '2')); }});
  window.writes.length = 0;
  await router.open({mode: 'desktop', target: chat});
  assert.deepEqual(window.writes, [['push', link.format({mode: 'desktop', target: chat})]],
    'opening launched a window, and the open is still one history entry');
  navigation.emit(desk([['1', 'so.tilo.notes'], ['2', 'so.tilo.chat']], '1'));
  assert.deepEqual(window.writes.at(-1), ['replace', link.format({mode: 'desktop', target: {scope, app: 'so.tilo.notes'}})],
    'a focus change between open windows replaces');
  assert.equal(window.entries.length, 2);
  router.stop();
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
