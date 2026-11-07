import test from 'node:test';
import assert from 'node:assert/strict';
import {createWorkspaceActions, invitedLine, JOIN_WORKSPACE, NEW_WORKSPACE, WAITING} from './workspace-actions.mjs';

test('an invited wait-list entry says how long ago it was invited', () => {
  const now = Date.UTC(2026, 9, 24, 12);
  assert.equal(invitedLine(now - 20_000, now, 'en'), 'Invited just now');
  assert.equal(invitedLine(now - 5 * 60_000, now, 'en'), 'Invited 5 minutes ago');
  assert.equal(invitedLine(now - 3 * 3600_000, now, 'en'), 'Invited 3 hours ago');
  assert.equal(invitedLine(now - 26 * 3600_000, now, 'en'), 'Invited yesterday');
  assert.equal(invitedLine(now - 9 * 86_400_000, now, 'en'), 'Invited 9 days ago');
});

const ADA = 'aa'.repeat(32);
const WORKSPACE = '6f1c2b9e-0d4a-4c1e-9b7a-2f3d1e5c8a90';
const bytes = hex => Array.from({length: 32}, (_, index) => parseInt(hex.slice(index * 2, index * 2 + 2), 16));
const ids = list => list.map(action => action.id);

function desktop() {
  const shown = [];
  const banners = [];
  return {shown, banners, mount: {setHostActions: actions => shown.push(actions), offerHostAction: offer => banners.push(offer)}};
}

/** Ada's page in her workspace on her own machine, as the host answers it. */
function adasRoom({owner = true, letsIn = owner} = {}) {
  const calls = [];
  let waiting = [{requestId: '02'.repeat(32), name: 'Bo', role: 'editor', userPublicKey: 'bb'.repeat(32)}];
  return {calls, peer: {
    origin: 'https://node.example:8443', workspace: WORKSPACE, userPublicKey: ADA, host: {hostCert: {user_public_key: bytes(ADA)}},
    createInvitation: async options => { calls.push(['invite', options]); return 'SIGNED-invitation'; },
    membership: {
      list: async () => {
        if (!owner) throw new Error('only the owner lists requests');
        return {name: 'Acme', members: [{userPublicKey: ADA, owner: true}]};
      },
      requests: async () => {
        if (!letsIn) throw new Error('only someone the room lets invite lists requests');
        return waiting;
      },
      admit: async requestId => { calls.push(['admit', requestId]); waiting = []; },
      decline: async requestId => { calls.push(['decline', requestId]); waiting = []; },
    },
  }};
}

test('the owner hears once who asks, and lets them in from the banner', async () => {
  const {peer, calls} = adasRoom();
  const {mount, shown, banners} = desktop();
  const polls = [];
  const actions = createWorkspaceActions({peer, every: run => { polls.push(run); return () => {}; },
    inviteLink: text => `https://tilo.so/#/@node.example:8443/w/${WORKSPACE}?invite=${text}`,
    ui: {fail: async text => assert.fail(text)}});
  await actions.attach(mount);
  assert.deepEqual(ids(shown.at(-1)), [NEW_WORKSPACE.id, JOIN_WORKSPACE.id, WAITING.id]);

  await polls[0]();
  assert.equal(banners.length, 1, 'a request is offered once, however often the page asks');
  assert.equal(banners[0].title, 'Bo asks to join');
  assert.ok(actions.handles(banners[0].action.id));
  await actions.perform(banners[0].action.id);
  assert.deepEqual(calls.at(-1), ['admit', '02'.repeat(32)]);
  assert.equal(banners.at(-1).title, 'Bo joined');
});

test('a member the workspace lets invite hears who asks to join and lets them in', async () => {
  const {peer, calls} = adasRoom({owner: false, letsIn: true});
  const {mount, shown, banners} = desktop();
  const polls = [];
  const actions = createWorkspaceActions({peer: {...peer, userPublicKey: 'bb'.repeat(32)},
    every: run => { polls.push(run); return () => {}; }, inviteLink: text => text, ui: {fail: async text => assert.fail(text)}});
  await actions.attach(mount);
  assert.deepEqual(ids(shown.at(-1)), [JOIN_WORKSPACE.id, WAITING.id]);
  assert.equal(banners.length, 1);
  assert.equal(banners[0].title, 'Bo asks to join');
  await actions.perform(banners[0].action.id);
  assert.deepEqual(calls.at(-1), ['admit', '02'.repeat(32)]);
});

test('the wait list is listed with who waits, gets no banner, and each address is invited with a link of its own', async () => {
  const {peer, calls} = adasRoom({owner: false, letsIn: true});
  const ed = {requestId: '05'.repeat(32), contact: 'ed@example.org', addedAtMs: 1000};
  const member = {...peer, userPublicKey: 'bb'.repeat(32), membership: {...peer.membership, requests: async () => [],
    waitList: async () => [ed], unlist: async requestId => { calls.push(['unlist', requestId]); },
    mailInvite: async () => { throw new Error('this door sends no mail'); }}};
  const {mount, shown, banners} = desktop();
  const polls = [];
  const shared = [];
  let listed = null;
  const actions = createWorkspaceActions({peer: member, every: run => { polls.push(run); return () => {}; },
    inviteLink: text => `https://tilo.so/#/@node.example:8443/w/${WORKSPACE}?invite=${text}`,
    ui: {share: async offer => { shared.push(offer); }, waitingList: async list => { listed = list; }, fail: async text => assert.fail(text)}});
  await actions.attach(mount);
  await polls[0]();
  // Nobody to let in now: the inviter finds the address in the list.
  assert.deepEqual(ids(shown.at(-1)), [JOIN_WORKSPACE.id, WAITING.id]);
  assert.equal(banners.length, 0);
  await actions.perform(WAITING.id);
  assert.deepEqual(await listed.load(), {people: [], waitList: [ed]});
  assert.deepEqual(await listed.invite(ed), {mailed: false, link: `https://tilo.so/#/@node.example:8443/w/${WORKSPACE}?invite=SIGNED-invitation`});
  assert.deepEqual(calls.at(-1), ['invite', {role: 'editor', waitListEntry: ed.requestId}]);
  assert.equal(shared[0].title, 'Invite ed@example.org');
  assert.match(shared[0].body, /^Send this link to ed@example\.org\..+leave the wait list once they join\.$/);
  assert.equal(shared[0].link, `https://tilo.so/#/@node.example:8443/w/${WORKSPACE}?invite=SIGNED-invitation`);
  await listed.unlist(ed.requestId);
  assert.deepEqual(calls.at(-1), ['unlist', ed.requestId]);
});

test('a host that sends mail mails the invitation to the address, and nothing is shared', async () => {
  const {peer, calls} = adasRoom({owner: false, letsIn: true});
  const ed = {requestId: '05'.repeat(32), contact: 'ed@example.org', addedAtMs: 1000};
  const member = {...peer, userPublicKey: 'bb'.repeat(32), membership: {...peer.membership, requests: async () => [],
    waitList: async () => [ed], mailInvite: async (requestId, link) => { calls.push(['mail', requestId, link]); }}};
  const {mount} = desktop();
  const polls = [];
  let listed = null;
  const actions = createWorkspaceActions({peer: member, every: run => { polls.push(run); return () => {}; },
    inviteLink: text => `https://tilo.so/?invite=${text}`,
    ui: {share: async () => assert.fail('mailed, not shared'), waitingList: async list => { listed = list; }, fail: async text => assert.fail(text)}});
  await actions.attach(mount);
  await polls[0]();
  await actions.perform(WAITING.id);
  assert.deepEqual(await listed.invite(ed), {mailed: true, link: 'https://tilo.so/?invite=SIGNED-invitation'});
  assert.deepEqual(calls.at(-1), ['mail', ed.requestId, 'https://tilo.so/?invite=SIGNED-invitation']);
});

test('someone the workspace does not let invite is never asked to let anyone in', async () => {
  const {peer} = adasRoom({owner: false});
  const {mount, shown, banners} = desktop();
  let polled = false;
  const actions = createWorkspaceActions({peer: {...peer, userPublicKey: 'bb'.repeat(32)},
    every: () => { polled = true; return () => {}; }, inviteLink: text => text, ui: {}});
  await actions.attach(mount);
  assert.deepEqual(ids(shown.at(-1)), [JOIN_WORKSPACE.id]);
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
