import test from 'node:test';
import assert from 'node:assert/strict';
import {createWorkspaceActions, invitedLine, INVITE_LINK, JOIN_WORKSPACE, NEW_WORKSPACE, WAITING} from './workspace-actions.mjs';

test('an invited wait-list entry says how long ago it was invited', () => {
  const now = Date.UTC(2026, 9, 24, 12);
  assert.equal(invitedLine(now - 20_000, now, 'en'), 'Invited just now');
  assert.equal(invitedLine(now - 5 * 60_000, now, 'en'), 'Invited 5 minutes ago');
  assert.equal(invitedLine(now - 3 * 3600_000, now, 'en'), 'Invited 3 hours ago');
  assert.equal(invitedLine(now - 26 * 3600_000, now, 'en'), 'Invited yesterday');
  assert.equal(invitedLine(now - 9 * 86_400_000, now, 'en'), 'Invited 9 days ago');
});

const ADA = 'aa'.repeat(32);
/** The short link a workspace's site keeps an invitation under. */
const SEALED = 'https://tilo.so/i/AbCdEfGhIjKlMnOp#s3cr3tKeyS3cr3tKeyS3cr3tKeyS3cr3tKeyS3cr';
const sealedLink = async () => SEALED;
const WORKSPACE = '6f1c2b9e-0d4a-4c1e-9b7a-2f3d1e5c8a90';
const bytes = hex => Array.from({length: 32}, (_, index) => parseInt(hex.slice(index * 2, index * 2 + 2), 16));
const ids = list => list.map(action => action.id);

function desktop() {
  const shown = [];
  const banners = [];
  return {shown, banners, mount: {setHostActions: actions => shown.push(actions), offerHostAction: offer => banners.push(offer)}};
}

/** Ada's page in her workspace on her own machine, as the host answers it. */
function adasRoom({owner = true, letsIn = owner, waitList = []} = {}) {
  const calls = [];
  return {calls, peer: {
    origin: 'https://node.example:8443', workspace: WORKSPACE, userPublicKey: ADA, host: {hostCert: {user_public_key: bytes(ADA)}},
    createInvitation: async options => { calls.push(['invite', options]); return 'SIGNED-invitation'; },
    membership: {
      list: async () => {
        if (!owner) throw new Error('only the owner lists people');
        return {name: 'Acme', members: [{userPublicKey: ADA, owner: true}]};
      },
      mayInvite: async () => letsIn,
      waitList: async () => {
        calls.push(['waitList']);
        if (!letsIn) throw new Error('only someone the room lets invite lists the wait list');
        return waitList;
      },
      unlist: async requestId => { calls.push(['unlist', requestId]); },
      mailInvite: async (requestId, link) => { calls.push(['mail', requestId, link]); },
    },
  }};
}

test('the wait list is listed, gets no banner, and each address is invited with a link of its own', async () => {
  const ed = {requestId: '05'.repeat(32), contact: 'ed@example.org', addedAtMs: 1000};
  const {peer, calls} = adasRoom({owner: false, letsIn: true, waitList: [ed]});
  const member = {...peer, userPublicKey: 'bb'.repeat(32),
    membership: {...peer.membership, mailInvite: async () => { throw new Error('this door sends no mail'); }}};
  const {mount, shown, banners} = desktop();
  const shared = [];
  let listed = null;
  const actions = createWorkspaceActions({peer: member, inviteLink: sealedLink,
    ui: {share: async offer => { shared.push(offer); }, waitingList: async list => { listed = list; }, fail: async text => assert.fail(text)}});
  await actions.attach(mount);
  assert.deepEqual(ids(shown.at(-1)), [INVITE_LINK.id, JOIN_WORKSPACE.id, WAITING.id]);
  assert.equal(banners.length, 0, 'nobody is waiting to be let in, so nothing is offered');
  await actions.perform(WAITING.id);
  assert.deepEqual(await listed.load(), [ed]);
  assert.deepEqual(await listed.invite(ed), {mailed: false, link: SEALED});
  assert.deepEqual(calls.at(-1), ['invite', {role: 'editor', waitListEntry: ed.requestId}]);
  assert.equal(shared[0].title, 'Invite ed@example.org');
  assert.match(shared[0].body, /^Send this link to ed@example\.org\..+as an editor\.$/);
  assert.equal(shared[0].link, SEALED, 'the short link is what is shared, not the invitation text');
  await listed.unlist(ed.requestId);
  assert.deepEqual(calls.at(-1), ['unlist', ed.requestId]);
});

test('a host that sends mail mails the invitation to the address, and nothing is shared', async () => {
  const ed = {requestId: '05'.repeat(32), contact: 'ed@example.org', addedAtMs: 1000};
  const {peer, calls} = adasRoom({owner: false, letsIn: true, waitList: [ed]});
  const member = {...peer, userPublicKey: 'bb'.repeat(32)};
  const {mount} = desktop();
  let listed = null;
  const actions = createWorkspaceActions({peer: member, inviteLink: sealedLink,
    ui: {share: async () => assert.fail('mailed, not shared'), waitingList: async list => { listed = list; }, fail: async text => assert.fail(text)}});
  await actions.attach(mount);
  await actions.perform(WAITING.id);
  assert.deepEqual(await listed.invite(ed), {mailed: true, link: SEALED});
  assert.deepEqual(calls.at(-1), ['mail', ed.requestId, SEALED]);
});

test('someone the workspace does not let invite is never offered the wait list', async () => {
  const {peer, calls} = adasRoom({owner: false});
  const {mount, shown} = desktop();
  const actions = createWorkspaceActions({peer: {...peer, userPublicKey: 'bb'.repeat(32)},
    inviteLink: sealedLink, ui: {}});
  await actions.attach(mount);
  assert.deepEqual(ids(shown.at(-1)), [JOIN_WORKSPACE.id], 'a host that keeps its door from this person offers nothing');
  assert.ok(!calls.some(([call]) => call === 'waitList'), 'nor asks the host for a list it would refuse');
  await actions.perform(WAITING.id);
  assert.deepEqual(ids(shown.at(-1)), [JOIN_WORKSPACE.id]);
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

test('a person on a host that keeps workspaces of people’s own makes their next one there', async () => {
  const origin = 'https://lounge.example';
  const calls = [];
  // The Lounge: a listed workspace on a host that keeps a workspace of each
  // person's own for the people its door lets in.
  const lounge = {origin, workspace: 'lounge', userPublicKey: ADA,
    host: {hostCert: {user_public_key: bytes('bb'.repeat(32)), device_public_key: Array(32).fill(0xcd),
      capabilities: {can_own_workspaces: true}}},
    enterOwnWorkspace: async ({name}) => { calls.push(['own', name]); return {workspace: 'studio-room', name}; }};
  const opened = [];
  const {mount, shown} = desktop();
  const actions = createWorkspaceActions({peer: lounge, machines: () => [],
    open: async where => { opened.push(where); },
    ui: {askWorkspaceName: save => save('Studio'), fail: async text => assert.fail(text)}});
  await actions.attach(mount);
  assert.ok(ids(shown.at(-1)).includes(NEW_WORKSPACE.id), 'the person is in a workspace of their own making on that host');
  await actions.perform(NEW_WORKSPACE.id);
  assert.deepEqual(calls, [['own', 'Studio']]);
  assert.deepEqual(opened, [{origin, room: 'studio-room', name: 'Studio', hostDevice: 'cd'.repeat(32)}]);
});

test('one press hands over an invite link: it was made before the press, and copied in it', async () => {
  const {peer, calls} = adasRoom({owner: false, letsIn: true});
  const copied = [];
  const shared = [];
  const {mount, shown} = desktop();
  const actions = createWorkspaceActions({peer, inviteLink: sealedLink,
    ui: {copy: async text => { copied.push(text); return true; },
      share: async offer => { shared.push(offer); },
      fail: async text => assert.fail(text)}});
  await actions.attach(mount);
  assert.ok(ids(shown.at(-1)).includes(INVITE_LINK.id), 'whoever may invite is handed a link');
  // Made when the page attached, so the press copies it inside the gesture.
  await new Promise(resolve => setTimeout(resolve, 0));
  assert.deepEqual(calls.filter(call => call[0] === 'invite').length, 1);
  await actions.perform(INVITE_LINK.id);
  assert.deepEqual(copied, [SEALED], 'the press copied the link, and nothing was asked');
  assert.equal(shared.length, 0, 'a copy that took needs no card');
  // Another is made for the next press, without anyone asking for it.
  await new Promise(resolve => setTimeout(resolve, 0));
  assert.equal(calls.filter(call => call[0] === 'invite').length, 2);
});

test('a press whose copy the browser refuses shows the link to copy by hand', async () => {
  const {peer} = adasRoom({owner: false, letsIn: true});
  const shared = [];
  const actions = createWorkspaceActions({peer, inviteLink: sealedLink,
    ui: {copy: async () => false, share: async offer => { shared.push(offer); },
      fail: async text => assert.fail(text)}});
  await actions.attach(desktop().mount);
  await new Promise(resolve => setTimeout(resolve, 0));
  await actions.perform(INVITE_LINK.id);
  assert.equal(shared.length, 1, 'the link is shown when the clipboard did not take it');
  assert.equal(shared[0].link, SEALED);
  assert.equal(shared[0].copied, true, 'the card tries the clipboard from its own opening');
});
