import test from 'node:test';
import assert from 'node:assert/strict';
import {createWorkspaceActions, JOIN_WORKSPACE, NEW_WORKSPACE} from './workspace-actions.mjs';

const ADA = 'aa'.repeat(32);
const WORKSPACE = '6f1c2b9e-0d4a-4c1e-9b7a-2f3d1e5c8a90';
const bytes = hex => Array.from({length: 32}, (_, index) => parseInt(hex.slice(index * 2, index * 2 + 2), 16));
const ids = list => list.map(action => action.id);

function desktop() {
  const shown = [];
  const banners = [];
  return {shown, banners, mount: {setHostActions: actions => shown.push(actions), offerHostAction: offer => banners.push(offer)}};
}

test('a new workspace is made on a machine the person set up, and opened', async () => {
  const origin = 'https://node.example:8443';
  const calls = [];
  const lobby = {
    host: {hostCert: {device_public_key: Array(32).fill(0xcd)}},
    workspaces: {
      create: async name => { calls.push(['create', name]); return {room: WORKSPACE, name}; },
      enter: async room => { calls.push(['enter', room]); return {room}; },
    },
    dispose: async () => { calls.push(['dispose']); },
  };
  const opened = [];
  const {mount, shown} = desktop();
  const actions = createWorkspaceActions({peer: null, machines: () => [origin],
    lobby: async at => { calls.push(['lobby', at]); return lobby; },
    open: async where => { opened.push(where); },
    ui: {askWorkspaceName: save => save('Acme'), fail: async text => assert.fail(text)}});
  await actions.attach(mount);
  assert.deepEqual(ids(shown.at(-1)), [NEW_WORKSPACE.id, JOIN_WORKSPACE.id]);
  await actions.perform(NEW_WORKSPACE.id);
  assert.deepEqual(calls, [['lobby', origin], ['create', 'Acme'], ['enter', WORKSPACE], ['dispose']]);
  // Opened as held by the machine's host, which the browser remembers it under.
  assert.deepEqual(opened, [{origin, room: WORKSPACE, name: 'Acme', hostDevice: 'cd'.repeat(32)}]);
});

test('in someone else’s workspace a person with a machine can still make a workspace on it', async () => {
  const origin = 'https://node.example:8443';
  const calls = [];
  const lobby = {
    host: {hostCert: {device_public_key: Array(32).fill(0xcd)}},
    workspaces: {
      create: async name => { calls.push(['create', name]); return {room: WORKSPACE, name}; },
      enter: async room => { calls.push(['enter', room]); return {room}; },
    },
    dispose: async () => { calls.push(['dispose']); },
  };
  // The Lounge: a workspace on a host that is not Ada's.
  const lounge = {origin: 'https://lounge.example', room: 'lounge', userPublicKey: ADA,
    host: {hostCert: {user_public_key: bytes('bb'.repeat(32))}}};
  const opened = [];
  const {mount, shown} = desktop();
  const actions = createWorkspaceActions({peer: lounge, machines: () => [origin],
    lobby: async at => { calls.push(['lobby', at]); return lobby; },
    open: async where => { opened.push(where); },
    ui: {askWorkspaceName: save => save('Acme'), fail: async text => assert.fail(text)}});
  await actions.attach(mount);
  assert.ok(ids(shown.at(-1)).includes(NEW_WORKSPACE.id));
  await actions.perform(NEW_WORKSPACE.id);
  assert.deepEqual(calls, [['lobby', origin], ['create', 'Acme'], ['enter', WORKSPACE], ['dispose']]);
  assert.equal(opened[0].origin, origin);
});
