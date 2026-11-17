import test from 'node:test';
import assert from 'node:assert/strict';
import {deviceEntry, handToNativeHost} from './native-host.mjs';

const ADA = 'ad'.repeat(32);

/** A page's peer whose host actions record what they were asked. */
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

test('a native host standing in the document is handed the device\'s entry, once, and no global gains anything', async () => {
  const {peer, asked} = peerWithHost();
  const taken = [];
  const before = Object.getOwnPropertyNames(globalThis).sort();
  assert.equal(handToNativeHost({peer, ownWorkspace: async () => ({id: 'ws-own', name: 'Mine', origin: 'https://host.example'})},
    {device: entry => taken.push(entry)}), true);
  assert.deepEqual(Object.getOwnPropertyNames(globalThis).sort(), before);
  assert.equal(taken.length, 1);
  const [entry] = taken;
  assert.ok(Object.isFrozen(entry));
  assert.deepEqual(Object.keys(entry).sort(), ['catalogue', 'invoke', 'ownWorkspace', 'person', 'status', 'workspace']);
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
