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

function nativeHost(device, agrees = true) {
  let pending;
  const host = Object.freeze({
    keepsDevice: true, device,
    confirmDevice(nonce) { return new Promise(resolve => { pending = {nonce, resolve}; }); },
  });
  const handler = {postMessage({kind, nonce}) {
    assert.equal(kind, 'device-challenge');
    assert.equal(nonce, pending.nonce);
    pending.resolve(agrees);
  }};
  return [host, handler];
}

test('a native host in the document is handed the device\'s entry, once, and no global gains anything', async () => {
  const {peer, asked} = peerWithHost();
  const taken = [];
  const before = Object.getOwnPropertyNames(globalThis).sort();
  assert.equal(await handToNativeHost({peer, ownWorkspace: async () => ({id: 'ws-own', name: 'Mine', origin: 'https://host.example'})},
    ...nativeHost(entry => { taken.push(entry); return true; })), true);
  assert.deepEqual(Object.getOwnPropertyNames(globalThis).sort(), before);
  assert.equal(taken.length, 1);
  const [entry] = taken;
  assert.ok(Object.isFrozen(entry));
  assert.deepEqual(Object.keys(entry).sort(), ['catalogue', 'changed', 'invoke', 'link', 'ownWorkspace', 'person', 'put', 'workspace', 'workspaceRecord']);
  assert.equal(entry.person, ADA);
  assert.equal(entry.workspace, 'ws-1');

  const target = {provider: 'tilo.vault', address: {resource: 'vault', action: 'add'}};
  const argument = {kind: 'key', belongsTo: ['https://api.example'], username: '', secret: 'typed into a protected field'};
  assert.deepEqual(await entry.invoke(target, argument, {invocation: 'i-1'}),
    {invocation: 'i-1', state: 'succeeded', output: {value: {ok: true}, resources: []}});
  assert.deepEqual(await entry.catalogue({provider: 'tilo.models'}), {actions: []});
  assert.deepEqual(await entry.ownWorkspace(), {id: 'ws-own', name: 'Mine', origin: 'https://host.example'});
  // An action goes the way that keeps nothing of it in the page, never the one that keeps its argument.
  assert.deepEqual(asked, [['once', target, argument, {invocation: 'i-1'}], ['catalogue', {provider: 'tilo.models'}]]);
});

test('a browser has no native host, so nothing is handed to anyone', async () => {
  const {peer, asked} = peerWithHost();
  assert.equal(globalThis.tiloNativeHost, undefined);
  assert.equal(await handToNativeHost({peer}), false);
  // Something of that name that offers no `device` is no host either.
  for (const none of [null, {}, {device: 'yes'}, 'host', 7]) assert.equal(await handToNativeHost({peer}, none), false);
  assert.deepEqual(asked, []);
});

test('a page whose workspace has no host to ask hands nothing over', async () => {
  let called = 0;
  const host = nativeHost(() => { called += 1; return true; });
  assert.equal(await handToNativeHost({peer: null}, ...host), false);
  assert.equal(await handToNativeHost({peer: {userPublicKey: ADA, workspace: 'ws-1'}}, ...host), false);
  // A peer whose actions cannot ask without keeping the argument is not handed over.
  assert.equal(await handToNativeHost({peer: {userPublicKey: ADA, actions: {invoke: () => {}, catalogue: () => {}}}}, ...host), false);
  assert.equal(deviceEntry({peer: {actions: undefined}}), null);
  assert.equal(called, 0);
});

test('a page global alone, a refused challenge and an unclaimed device give the host no session', async () => {
  const {peer} = peerWithHost();
  let taken = 0;
  const take = () => { taken++; return true; };
  const [host, handler] = nativeHost(take);
  assert.equal(await handToNativeHost({peer}, host, null), false);
  assert.equal(await handToNativeHost({peer}, {...host}, handler), false);
  assert.equal(await handToNativeHost({peer}, ...nativeHost(take, false)), false);
  assert.equal(taken, 0);
  assert.equal(await handToNativeHost({peer}, ...nativeHost(() => false)), false);
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

test('the device discovers the workspace record or opens it once on first use', async () => {
  const {peer} = peerWithHost();
  const app = 'so.tilo.chat';
  const provider = 'record:so.tilo.chat:';
  let offered = false;
  let opens = 0;
  let observed = 0;
  peer.actions.catalogue = async query => {
    assert.equal(query.app_id, app);
    if (query.max_results === 0) return {providers: [{app_id: app, browsable: true, document_id: 'another', provider: 'other'}]};
    assert.deepEqual(query.target, {resource: 'app.so.tilo.chat', action: 'open'});
    return {rows: offered ? [{app_id: app, provider: 'workspace:test', action: {address: query.target}}] : []};
  };
  peer.actions.once = async (target, argument) => {
    assert.equal(target.provider, 'workspace:test');
    assert.equal(argument, null);
    opens++;
    return {invocation: 'one', state: 'accepted'};
  };
  peer.actions.observe = async id => {
    assert.equal(id, 'one');
    observed++;
    return {state: 'succeeded', output: {value: {record: '', provider}}};
  };
  const entry = deviceEntry({peer});
  await assert.rejects(entry.workspaceRecord(app), /unknown_action/);
  offered = true;
  assert.deepEqual(await Promise.all([entry.workspaceRecord(app), entry.workspaceRecord(app)]), [provider, provider]);
  assert.equal(opens, 1);
  assert.equal(observed, 1);
  peer.actions.catalogue = async () => ({providers: [{app_id: app, browsable: true, provider}]});
  assert.equal(await deviceEntry({peer}).workspaceRecord(app), provider);
  assert.equal(opens, 1);
});

const KEPT = `tilo.content:${'ab'.repeat(32)}`;

/** A page's peer whose workspace keeps content: every step of a put and every link it was asked. */
const peerWithContent = () => {
  const asked = [];
  const peer = {userPublicKey: ADA, workspace: 'ws-1', actions: {once: async () => ({state: 'succeeded'})}};
  peer.actions.contentPut = async step => {
    asked.push(['contentPut', step]);
    if (step.begin) return {begun: {upload: 3}};
    if (step.append) return {appended: {written: step.append.offset + step.append.bytes.length / 2}};
    return {finished: {content: KEPT}};
  };
  peer.actions.contentLink = async link => {
    asked.push(['contentLink', link]);
    return {path: `/api/content/token/${link.name}`, expires_in_secs: 120};
  };
  return {peer, asked};
};

test('a native app puts a file in pieces into its person\'s workspace and asks a link to open one', async () => {
  const {peer, asked} = peerWithContent();
  const entry = deviceEntry({peer});
  assert.ok(Object.isFrozen(entry.put));
  assert.deepEqual(Object.keys(entry.put).sort(), ['append', 'begin', 'finish']);
  const said = [];
  const levels = ['log', 'info', 'warn', 'error', 'debug'];
  const loud = Object.fromEntries(levels.map(level => [level, console[level]]));
  for (const level of levels) console[level] = (...words) => said.push(words);
  try {
    assert.deepEqual(await entry.put.begin(5), {upload: 3});
    // "Hello" as base64: it crosses as hex, at the offset given.
    assert.deepEqual(await entry.put.append(3, 0, 'SGVsbG8='), {written: 5});
    assert.deepEqual(await entry.put.finish(3), {content: KEPT});
    assert.deepEqual(await entry.link(KEPT, 'plan.pdf'), {path: '/api/content/token/plan.pdf', expiresInSecs: 120});
  } finally {
    Object.assign(console, loud);
  }
  assert.deepEqual(asked, [
    ['contentPut', {begin: {size: 5}}],
    ['contentPut', {append: {upload: 3, offset: 0, bytes: '48656c6c6f'}}],
    ['contentPut', {finish: {upload: 3}}],
    ['contentLink', {name: 'plan.pdf', files: [{path: 'plan.pdf', hash: 'ab'.repeat(32)}], zip: false}],
  ]);
  // Nothing of the file, and no link, was said to a log.
  assert.deepEqual(said, []);
});

test('a piece too large for a frame, and what is no piece, are refused before anything is asked', async () => {
  const {peer, asked} = peerWithContent();
  const entry = deviceEntry({peer});
  const frame = 384 * 1024;
  await assert.rejects(entry.put.append(3, 0, Buffer.alloc(frame + 1).toString('base64')),
    /invalid_request: A piece carries at most 393216 bytes/);
  await assert.rejects(entry.put.append(3, 0, 'no base64 !'), TypeError);
  await assert.rejects(entry.put.append(3, 0, undefined), TypeError);
  await assert.rejects(entry.put.append(3, '0', 'SGVsbG8='), TypeError);
  await assert.rejects(entry.put.begin(-1), TypeError);
  await assert.rejects(entry.put.begin(1.5), TypeError);
  await assert.rejects(entry.put.finish('3'), TypeError);
  assert.deepEqual(asked, []);
  // A piece of exactly a frame's worth goes.
  assert.deepEqual(await entry.put.append(3, 0, Buffer.alloc(frame).toString('base64')), {written: frame});
});

test('a file too large is refused in the server\'s own words, and nothing more is asked', async () => {
  const {peer, asked} = peerWithContent();
  peer.actions.contentPut = async step => {
    asked.push(['contentPut', step]);
    throw Object.assign(new Error('one file is at most 33554432 bytes here, and this one is 33554433'), {code: 'invalid_request'});
  };
  const entry = deviceEntry({peer});
  await assert.rejects(entry.put.begin(33554433), {code: 'invalid_request'});
  assert.deepEqual(asked, [['contentPut', {begin: {size: 33554433}}]]);
});

test('a page that cannot put content or link it says so', async () => {
  const asked = [];
  const peer = {userPublicKey: ADA, workspace: 'ws-1', actions: {once: async () => { asked.push('once'); return {state: 'succeeded'}; }}};
  const entry = deviceEntry({peer});
  await assert.rejects(entry.put.begin(1), /unknown_action/);
  await assert.rejects(entry.put.append(3, 0, 'SGVsbG8='), /unknown_action/);
  await assert.rejects(entry.link(KEPT, 'plan.pdf'), /unknown_action/);
  assert.deepEqual(asked, []);
});
