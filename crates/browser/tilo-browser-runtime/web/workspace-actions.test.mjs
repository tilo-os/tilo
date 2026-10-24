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
function adasRoom({owner = true, letsIn = owner} = {}) {
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
      requests: async () => {
        if (!letsIn) throw new Error('only someone the room lets invite lists requests');
        return waiting;
      },
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

test('a member the room lets invite hears who asks to join and lets them in', async () => {
  const {peer, calls} = adasRoom({owner: false, letsIn: true});
  const {mount, shown, banners} = desktop();
  const polls = [];
  const actions = createWorkspaceActions({peer: {...peer, userPublicKey: 'bb'.repeat(32)},
    every: run => { polls.push(run); return () => {}; }, inviteLink: text => text, ui: {fail: async text => assert.fail(text)}});
  await actions.attach(mount);
  assert.deepEqual(ids(shown.at(-1)), [INVITE.id, WAITING.id]);
  assert.equal(banners.length, 1);
  assert.equal(banners[0].title, 'Bo asks to join');
  await actions.perform(banners[0].action.id);
  assert.deepEqual(calls.at(-1), ['admit', '02'.repeat(32)]);
});

test('the wait list is listed with who waits, gets no banner, and each address is invited with a link of its own', async () => {
  const {peer, calls} = adasRoom({owner: false, letsIn: true});
  const ed = {requestId: '05'.repeat(32), name: 'ed@example.org', role: 'editor', userPublicKey: '00'.repeat(32),
    contact: 'ed@example.org', requestedAtMs: 1000};
  const member = {...peer, userPublicKey: 'bb'.repeat(32), membership: {...peer.membership, requests: async () => [ed]}};
  const {mount, shown, banners} = desktop();
  const polls = [];
  const shared = [];
  let listed = null;
  const actions = createWorkspaceActions({peer: member, every: run => { polls.push(run); return () => {}; },
    inviteLink: text => `https://tilo.so/#/@node.example:8443/w/${ROOM}?invite=${text}`,
    ui: {share: async offer => { shared.push(offer); }, waitingList: async list => { listed = list; }, fail: async text => assert.fail(text)}});
  await actions.attach(mount);
  await polls[0]();
  // Nobody to let in now: the inviter finds the address in the list.
  assert.deepEqual(ids(shown.at(-1)), [INVITE.id, WAITING.id]);
  assert.equal(banners.length, 0);
  await actions.perform(WAITING.id);
  assert.deepEqual(await listed.load(), [ed]);
  await listed.invite(ed);
  assert.deepEqual(calls.at(-1), ['invite', {role: 'editor'}]);
  assert.equal(shared[0].title, 'Invite ed@example.org');
  assert.match(shared[0].body, /^Send this link to ed@example\.org\./);
  assert.equal(shared[0].link, `https://tilo.so/#/@node.example:8443/w/${ROOM}?invite=SIGNED-invitation`);
  await listed.decline(ed.requestId);
  assert.deepEqual(calls.at(-1), ['decline', ed.requestId]);
});

test('a member who just invited someone hears who asks to join, though the host did not show them at first', async () => {
  const {peer, calls} = adasRoom({owner: false, letsIn: false});
  let shownWaiting = false;
  const member = {...peer, userPublicKey: 'bb'.repeat(32), membership: {...peer.membership,
    requests: async () => { if (!shownWaiting) throw new Error('not yet'); return [{requestId: '03'.repeat(32), name: 'Cy', role: 'editor'}]; }}};
  const {mount, shown, banners} = desktop();
  const polls = [];
  const actions = createWorkspaceActions({peer: member, every: run => { polls.push(run); return () => {}; },
    inviteLink: text => text, ui: {share: async () => {}, fail: async text => assert.fail(text)}});
  await actions.attach(mount);
  assert.deepEqual(ids(shown.at(-1)), [INVITE.id]);
  shownWaiting = true;
  await actions.perform(INVITE.id);
  assert.deepEqual(calls, [['invite', {role: 'editor'}]]);
  await polls[0]?.();
  assert.deepEqual(ids(shown.at(-1)), [INVITE.id, WAITING.id]);
  assert.equal(banners.at(-1)?.title, 'Cy asks to join');
});

test('someone the room does not let invite is never asked to let anyone in', async () => {
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

/** A page in the public Lounge: the room refuses guests' invitations, as
 * the reviewer saw (`grant role:editor`), so inviting must never ask it. */
function lounge({owner = false} = {}) {
  const {peer, calls} = adasRoom({owner});
  const acknowledged = [];
  const {mount, shown, banners} = desktop();
  mount.acknowledge = text => acknowledged.push(text);
  return {calls, acknowledged, mount, shown, banners,
    peer: {...peer, userPublicKey: owner ? ADA : 'cc'.repeat(32),
      createInvitation: async () => { throw new Error(`room ${ROOM} kind:tilo.room grant role:editor:2 denied`); }}};
}

for (const [who, owner] of [['a guest', false], ['a moderator or owner', true]]) {
  test(`in a public workspace, ${who} invites by copying its link and is told so`, async () => {
    const {peer, mount, shown, acknowledged} = lounge({owner});
    const copied = [];
    const actions = createWorkspaceActions({peer, inviteLink: () => assert.fail('no invitation is made'),
      publicLink: () => 'https://tilo.coffee/', every: () => () => {},
      ui: {copy: async text => { copied.push(text); return true; }, share: async () => assert.fail('copied, not shown'),
        fail: async text => assert.fail(text)}});
    await actions.attach(mount);
    const invite = shown.at(-1).find(action => action.id === INVITE.id);
    assert.equal(invite.title, 'Invite people');
    assert.match(invite.subtitle, /anyone can join this public workspace/);
    await actions.perform(INVITE.id);
    assert.deepEqual(copied, ['https://tilo.coffee/']);
    assert.equal(acknowledged.length, 1);
    assert.match(acknowledged[0], /^Link copied: anyone can join .+ with it$/);
    assert.doesNotMatch(acknowledged[0], new RegExp(`${ROOM}|tilo\\.room|role:`));
  });
}

test('in a public workspace whose link cannot be copied, the link is shown to copy by hand', async () => {
  const {peer, mount} = lounge();
  const shared = [];
  const actions = createWorkspaceActions({peer, inviteLink: () => assert.fail('no invitation is made'),
    publicLink: () => 'https://tilo.coffee/',
    ui: {copy: async () => false, share: async offer => { shared.push(offer); }, fail: async text => assert.fail(text)}});
  await actions.attach(mount);
  await actions.perform(INVITE.id);
  assert.equal(shared.length, 1);
  assert.equal(shared[0].link, 'https://tilo.coffee/');
  assert.match(shared[0].body, /public: anyone with this link can join/);
});

test('a member of a workspace that is not public still invites with a signed invitation', async () => {
  const {peer, calls} = adasRoom({owner: false});
  const {mount, shown} = desktop();
  const shared = [];
  const actions = createWorkspaceActions({peer: {...peer, userPublicKey: 'bb'.repeat(32)}, inviteLink: text => text,
    ui: {share: async offer => { shared.push(offer); }, fail: async text => assert.fail(text)}});
  await actions.attach(mount);
  assert.equal(shown.at(-1)[0].title, INVITE.title);
  await actions.perform(INVITE.id);
  assert.deepEqual(calls, [['invite', {role: 'editor'}]]);
  assert.equal(shared[0].link, 'SIGNED-invitation');
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
