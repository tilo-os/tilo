import test from 'node:test';
import assert from 'node:assert/strict';
import {createWorkspaceActions, INVITE, NEW_WORKSPACE, WAITING} from './workspace-actions.mjs';

const ADA = 'aa'.repeat(32);
const ROOM = '6f1c2b9e-0d4a-4c1e-9b7a-2f3d1e5c8a90';
const bytes = hex => Array.from({length: 32}, (_, index) => parseInt(hex.slice(index * 2, index * 2 + 2), 16));
const ids = list => list.map(action => action.id);

function desktop() {
  const shown = [];
  const banners = [];
  return {shown, banners, mount: {setHostActions: actions => shown.push(actions), offerHostAction: offer => banners.push(offer)}};
}

/** Ada's page in her workspace on her own machine, as the host answers it. */
function adasRoom({owner = true} = {}) {
  const calls = [];
  let waiting = [{requestId: '02'.repeat(32), name: 'Bo', role: 'editor', userPublicKey: 'bb'.repeat(32)}];
  return {calls, peer: {
    origin: 'https://node.example:8443', room: ROOM, userPublicKey: ADA, host: {hostCert: {user_public_key: bytes(ADA)}},
    createInvitation: async options => { calls.push(['invite', options]); return 'SIGNED-invitation'; },
    membership: {
      list: async () => {
        if (!owner) throw new Error('only the owner lists requests');
        return {name: 'Acme', members: [{userPublicKey: ADA, owner: true}]};
      },
      requests: async () => waiting,
      admit: async requestId => { calls.push(['admit', requestId]); waiting = []; },
      decline: async requestId => { calls.push(['decline', requestId]); waiting = []; },
    },
  }};
}

test('the owner invites with a link, hears once who asks, and lets them in from the banner', async () => {
  const {peer, calls} = adasRoom();
  const {mount, shown, banners} = desktop();
  const polls = [];
  const shared = [];
  const actions = createWorkspaceActions({peer, every: run => { polls.push(run); return () => {}; },
    inviteLink: text => `https://tilo.so/#/@node.example:8443/w/${ROOM}?invite=${text}`,
    ui: {share: async offer => { shared.push(offer); }, fail: async text => assert.fail(text)}});
  await actions.attach(mount);
  assert.deepEqual(ids(shown.at(-1)), [NEW_WORKSPACE.id, INVITE.id, WAITING.id]);

  await actions.perform(INVITE.id);
  assert.deepEqual(calls, [['invite', {role: 'editor'}]]);
  assert.equal(shared[0].title, 'Invite someone to “Acme”');
  assert.equal(shared[0].link, `https://tilo.so/#/@node.example:8443/w/${ROOM}?invite=SIGNED-invitation`);

  await polls[0]();
  assert.equal(banners.length, 1, 'a request is offered once, however often the page asks');
  assert.equal(banners[0].title, 'Bo asks to join');
  assert.ok(actions.handles(banners[0].action.id));
  await actions.perform(banners[0].action.id);
  assert.deepEqual(calls.at(-1), ['admit', '02'.repeat(32)]);
  assert.equal(banners.at(-1).title, 'Bo joined');
});

test('someone who is not the owner may invite, and is never asked to let anyone in', async () => {
  const {peer} = adasRoom({owner: false});
  const {mount, shown, banners} = desktop();
  let polled = false;
  const actions = createWorkspaceActions({peer: {...peer, userPublicKey: 'bb'.repeat(32)},
    every: () => { polled = true; return () => {}; }, inviteLink: text => text, ui: {}});
  await actions.attach(mount);
  assert.deepEqual(ids(shown.at(-1)), [INVITE.id]);
  await actions.perform(WAITING.id);
  assert.equal(polled, false);
  assert.deepEqual(banners, []);
});

test('a new workspace is made on a machine the person set up, and opened', async () => {
  const origin = 'https://node.example:8443';
  const calls = [];
  const lobby = {
    host: {hostCert: {device_public_key: Array(32).fill(0xcd)}},
    workspaces: {
      create: async name => { calls.push(['create', name]); return {room: ROOM, name}; },
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
  assert.deepEqual(ids(shown.at(-1)), [NEW_WORKSPACE.id]);
  await actions.perform(NEW_WORKSPACE.id);
  assert.deepEqual(calls, [['lobby', origin], ['create', 'Acme'], ['enter', ROOM], ['dispose']]);
  // Opened as held by the machine's host, which the browser remembers it under.
  assert.deepEqual(opened, [{origin, room: ROOM, name: 'Acme', hostDevice: 'cd'.repeat(32)}]);
});
