import test from 'node:test';
import assert from 'node:assert/strict';
import {deviceEntry, handToNativeHost} from './native-host.mjs';

const ADA = 'ad'.repeat(32);

/** A page's peer whose host actions keep what they were asked. */
const peerWithHost = () => {
  const asked = [];
  const actions = {
    catalogue: async query => { asked.push(['catalogue', query]); return {actions: []}; },
    // The keeping kind, which holds an invocation's argument for a retry.
    invoke: () => { asked.push(['invoke']); throw new Error('the keeping invoke is not for a native host'); },
    once: async (target, argument, options) => { asked.push(['once', target, argument, options]); return {invocation: 'i-1', state: 'succeeded', output: {value: {ok: true}, resources: []}}; },
    observe: async invocation => { asked.push(['observe', invocation]); return {invocation, state: 'succeeded'}; },
  };
  return {asked, peer: {userPublicKey: ADA, workspace: 'ws-1', actions}};
};

test('a native host in the document is handed the device\'s entry, once, and no global gains anything', async () => {
  const {peer, asked} = peerWithHost();
  const taken = [];
  const before = Object.getOwnPropertyNames(globalThis).sort();
  assert.equal(handToNativeHost({peer, ownWorkspace: async () => ({id: 'ws-own', name: 'Mine', origin: 'https://host.example'})},
    {device: entry => taken.push(entry)}), true);
  assert.deepEqual(Object.getOwnPropertyNames(globalThis).sort(), before);
  assert.equal(taken.length, 1);
  const [entry] = taken;
  assert.ok(Object.isFrozen(entry));
  assert.deepEqual(Object.keys(entry).sort(), ['catalogue', 'changed', 'invoke', 'ownWorkspace', 'person', 'status', 'workspace']);
  assert.equal(entry.person, ADA);
  assert.equal(entry.workspace, 'ws-1');

  const target = {provider: 'tilo.vault', address: {resource: 'vault', action: 'add'}};
  const argument = {kind: 'key', belongsTo: ['https://api.example'], username: '', secret: 'typed into a protected field'};
  assert.deepEqual(await entry.invoke(target, argument, {invocation: 'i-1'}),
    {invocation: 'i-1', state: 'succeeded', output: {value: {ok: true}, resources: []}});
  assert.deepEqual(await entry.status('i-1'), {invocation: 'i-1', state: 'succeeded'});
  assert.deepEqual(await entry.catalogue({provider: 'tilo.models'}), {actions: []});
  assert.deepEqual(await entry.ownWorkspace(), {id: 'ws-own', name: 'Mine', origin: 'https://host.example'});
  // An action goes the way that keeps nothing of it in the page, never the one that keeps its argument.
  assert.deepEqual(asked, [['once', target, argument, {invocation: 'i-1'}], ['observe', 'i-1'], ['catalogue', {provider: 'tilo.models'}]]);
});

test('a browser has no native host, so nothing is handed to anyone', () => {
  const {peer, asked} = peerWithHost();
  assert.equal(globalThis.tiloNativeHost, undefined);
  assert.equal(handToNativeHost({peer}), false);
  // Something of that name that offers no `device` is no host either.
  for (const none of [null, {}, {device: 'yes'}, 'host', 7]) assert.equal(handToNativeHost({peer}, none), false);
  assert.deepEqual(asked, []);
});

test('a page whose workspace has no host to ask hands nothing over', () => {
  let called = 0;
  const host = {device: () => { called += 1; }};
  assert.equal(handToNativeHost({peer: null}, host), false);
  assert.equal(handToNativeHost({peer: {userPublicKey: ADA, workspace: 'ws-1'}}, host), false);
  // A peer whose actions cannot ask without keeping the argument is not handed over.
  assert.equal(handToNativeHost({peer: {userPublicKey: ADA, actions: {invoke: () => {}, catalogue: () => {}}}}, host), false);
  assert.equal(deviceEntry({peer: {actions: undefined}}), null);
  assert.equal(called, 0);
});

test('a host that fails to take the entry leaves the page as it was', () => {
  const {peer} = peerWithHost();
  const warned = console.warn;
  console.warn = () => {};
  try {
    assert.equal(handToNativeHost({peer}, {device: () => { throw new Error('the host is gone'); }}), false);
  } finally {
    console.warn = warned;
  }
});

test('the person\'s own workspace is asked for only where the page can ask', async () => {
  const {peer} = peerWithHost();
  const entry = deviceEntry({peer});
  assert.throws(() => entry.ownWorkspace(), /unknown_action/);
});

/**
 * A page's peer whose server keeps a feed of its documents' changes. `pages` are what the feed
 * answers this person next, in order (with none left: nothing new, and the cursor that was
 * asked from); `wake()` is the server's word that a write landed, which says nothing of what.
 */
const followedServer = () => {
  const reads = [];
  const pages = [];
  const waits = [];
  let wake = () => {};
  const actions = {
    catalogue: async () => ({actions: []}),
    once: async () => ({invocation: 'i-1', state: 'succeeded'}),
    observe: async invocation => ({invocation, state: 'succeeded'}),
    changes: async after => { reads.push(after); return pages.shift() ?? {changed: [], cursor: after ?? 'c-0'}; },
    landed: withinMs => {
      let over;
      const woke = new Promise(resolve => { over = resolve; });
      const timer = setTimeout(() => over(false), withinMs);
      const wait = {withinMs, released: false};
      waits.push(wait);
      wake = () => { clearTimeout(timer); over(true); };
      return {woke, release() { wait.released = true; clearTimeout(timer); over(false); }};
    },
  };
  return {reads, pages, waits, wake: () => wake(), peer: {userPublicKey: ADA, workspace: 'ws-1', actions}};
};
const soon = ms => new Promise(done => setTimeout(done, ms));
const TASKS = {app: 'so.tilo.tasks'};

test('a native app is told which of the documents it follows changed, by name alone', async () => {
  const served = followedServer();
  const entry = deviceEntry({peer: served.peer}, {holdMs: 400, settleMs: 20});
  // Nothing that happened before the first ask is news: its answer is where to ask on from.
  served.pages.push({changed: [{app: 'so.tilo.tasks', document: 'old'}], cursor: 'c-1'});
  assert.deepEqual(await entry.changed({of: [TASKS]}), {changed: [], cursor: 'c-1'});
  assert.deepEqual(served.reads, [null]);
  // An ask is held until the server says a write landed. The feed then names the document.
  const asked = entry.changed({of: [TASKS], after: 'c-1', waitMs: 300});
  let answered = false;
  asked.then(() => { answered = true; });
  await soon(60);
  assert.equal(answered, false);
  served.pages.push({changed: [{app: 'so.tilo.tasks', document: 'part-1'}], cursor: 'c-2'});
  served.wake();
  const told = await asked;
  assert.deepEqual(told, {changed: [{app: 'so.tilo.tasks', document: 'part-1'}], cursor: 'c-2'});
  // A word names a document and holds nothing else.
  assert.deepEqual(Object.keys(told).sort(), ['changed', 'cursor']);
  assert.deepEqual(told.changed.map(one => Object.keys(one).sort()), [['app', 'document']]);
  // One document of an app is followed by its name, and no other document of that app is told.
  served.pages.push({changed: [{app: 'so.tilo.tasks', document: 'part-1'}, {app: 'so.tilo.tasks', document: ''}], cursor: 'c-3'});
  assert.deepEqual(await entry.changed({of: [{app: 'so.tilo.tasks', document: ''}], after: 'c-2'}),
    {changed: [{app: 'so.tilo.tasks', document: ''}], cursor: 'c-3'});
});

test('a write to a document the server keeps from this person is no word: the ask is held to its end and names nothing', async () => {
  const served = followedServer();
  const entry = deviceEntry({peer: served.peer}, {holdMs: 400, settleMs: 20});
  // A write lands in a document this person may not read. The server's word that something was
  // written wakes everyone who waits; its feed tells this person nothing of that document.
  const began = Date.now();
  const asked = entry.changed({of: [{app: 'so.tilo.notes'}], after: 'c-1', waitMs: 150});
  await soon(30);
  served.pages.push({changed: [], cursor: 'c-2'});
  served.wake();
  assert.deepEqual(await asked, {changed: [], cursor: 'c-2'});
  // It is not answered at the write either: nobody tells by the clock that something was written.
  assert.ok(Date.now() - began >= 140, `held ${Date.now() - began} ms`);
  // The entry asked the server's feed and nothing else of the page: no other source names a document.
  assert.deepEqual(served.reads, ['c-1', 'c-1']);
  // Nor is a document named that changed and was not asked for.
  served.pages.push({changed: [{app: 'so.tilo.chat', document: 'part-0'}, {app: 'so.tilo.notes.more', document: ''}], cursor: 'c-3'});
  assert.deepEqual(await entry.changed({of: [{app: 'so.tilo.notes'}, {app: 'so.tilo.chat', document: ''}], after: 'c-2'}),
    {changed: [], cursor: 'c-3'});
});

test('many changes of one document while an answer settles are one word, and the server is asked once in that time', async () => {
  const served = followedServer();
  const entry = deviceEntry({peer: served.peer}, {holdMs: 1000, settleMs: 80});
  assert.deepEqual(await entry.changed({of: [TASKS]}), {changed: [], cursor: 'c-0'});
  const began = Date.now();
  // The first write of a burst, and what the feed holds a settling time later.
  served.pages.push({changed: [{app: 'so.tilo.tasks', document: 'part-1'}], cursor: 'c-1'});
  served.pages.push({changed: [{app: 'so.tilo.tasks', document: 'part-1'}, {app: 'so.tilo.tasks', document: 'part-2'},
    {app: 'so.tilo.tasks', document: 'part-1'}, {app: 'so.tilo.chat', document: ''}], cursor: 'c-9'});
  const told = await entry.changed({of: [TASKS], after: 'c-0', waitMs: 500});
  assert.deepEqual(told, {changed: [{app: 'so.tilo.tasks', document: 'part-1'}, {app: 'so.tilo.tasks', document: 'part-2'}], cursor: 'c-9'});
  // Two reads of the feed made this answer, each a settling time after the one before.
  assert.deepEqual(served.reads, [null, 'c-0', 'c-1']);
  assert.ok(Date.now() - began >= 140, `answered after ${Date.now() - began} ms`);
  // The wait that was out when the feed named the document is let go of.
  assert.deepEqual(served.waits.map(wait => wait.released), [true]);
});

test('an ask for changes ends by itself: at its wait, at the entry\'s own bound, and where the server never says what changed', async () => {
  const served = followedServer();
  const entry = deviceEntry({peer: served.peer}, {holdMs: 120, settleMs: 10});
  const began = Date.now();
  assert.deepEqual(await entry.changed({of: [TASKS], after: 'c-1', waitMs: 60}), {changed: [], cursor: 'c-1'});
  assert.ok(Date.now() - began >= 50, `held ${Date.now() - began} ms`);
  // A wait longer than the entry holds an ask is held no longer than that.
  assert.deepEqual(await entry.changed({of: [TASKS], after: 'c-1', waitMs: 3_600_000}), {changed: [], cursor: 'c-1'});
  assert.ok(served.waits.at(-1).withinMs <= 120, `waited ${served.waits.at(-1).withinMs} ms`);
  // A server that never says what changed: the ask ends, said so, and the next one is taken.
  served.peer.actions.changes = () => new Promise(() => {});
  await assert.rejects(entry.changed({of: [TASKS], after: 'c-1'}), /^Error: unanswered: /);
  served.peer.actions.changes = async after => ({changed: [], cursor: after});
  assert.deepEqual(await entry.changed({of: [TASKS], after: 'c-1'}), {changed: [], cursor: 'c-1'});
});

test('one ask for changes is out at a time, of what is named well; a page whose peer keeps no feed says so', async () => {
  const served = followedServer();
  const entry = deviceEntry({peer: served.peer}, {holdMs: 200, settleMs: 10});
  const first = entry.changed({of: [TASKS], after: 'c-1', waitMs: 80});
  await assert.rejects(entry.changed({of: [TASKS], after: 'c-1'}), /^Error: busy: /);
  await first;
  for (const none of [undefined, [], 'so.tilo.tasks', [{}], [{app: ''}], [{app: 'a', document: 7}], Array.from({length: 33}, () => ({app: 'a'}))]) {
    await assert.rejects(entry.changed({of: none, after: 'c-1'}), TypeError);
  }
  await assert.rejects(entry.changed(), TypeError);
  // None of these asked the server anything.
  assert.deepEqual(served.reads, ['c-1']);
  const feedless = {userPublicKey: ADA, workspace: 'ws-1', actions: {once: async () => ({}), catalogue: async () => ({})}};
  await assert.rejects(deviceEntry({peer: feedless}).changed({of: [TASKS]}), /^Error: unknown_action: /);
});
