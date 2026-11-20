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

/** Ada's page in her workspace on her own machine, as the host answers it. A
 * host that keeps workspaces of people's own is someone else's, and says so. */
function adasWorkspace({owner = true, letsIn = owner, waitList = [], keepsOwn = false} = {}) {
  const calls = [];
  return {calls, peer: {
    origin: 'https://node.example:8443', workspace: WORKSPACE, userPublicKey: ADA,
    host: {hostCert: {user_public_key: bytes(keepsOwn ? 'bb'.repeat(32) : ADA),
      ...(keepsOwn ? {device_public_key: Array(32).fill(0xcd), capabilities: {can_own_workspaces: true}} : {})}},
    createInvitation: async options => { calls.push(['invite', options]); return 'SIGNED-invitation'; },
    membership: {
      list: async () => {
        if (!owner) throw new Error('only the owner lists people');
        return {name: 'Acme', members: [{userPublicKey: ADA, owner: true}]};
      },
      mayInvite: async () => letsIn,
      waitList: async () => {
        calls.push(['waitList']);
        if (!letsIn) throw new Error('only someone the workspace lets invite lists the wait list');
        return waitList;
      },
      unlist: async requestId => { calls.push(['unlist', requestId]); },
      mailInvite: async (requestId, link) => { calls.push(['mail', requestId, link]); },
    },
  }};
}

test('the wait list is listed, gets no banner, and each address is invited with a link of its own', async () => {
  const ed = {requestId: '05'.repeat(32), contact: 'ed@example.org', addedAtMs: 1000};
  const {peer, calls} = adasWorkspace({owner: false, letsIn: true, waitList: [ed]});
  const member = {...peer, userPublicKey: 'bb'.repeat(32),
    membership: {...peer.membership, mailInvite: async () => { throw new Error('this entry sends no mail'); }}};
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
  const {peer, calls} = adasWorkspace({owner: false, letsIn: true, waitList: [ed]});
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
  const {peer, calls} = adasWorkspace({owner: false});
  const {mount, shown} = desktop();
  const actions = createWorkspaceActions({peer: {...peer, userPublicKey: 'bb'.repeat(32)},
    inviteLink: sealedLink, ui: {}});
  await actions.attach(mount);
  assert.deepEqual(ids(shown.at(-1)), [JOIN_WORKSPACE.id], 'a host that keeps its entry from this person offers nothing');
  assert.ok(!calls.some(([call]) => call === 'waitList'), 'nor asks the host for a list it would refuse');
  await actions.perform(WAITING.id);
  assert.deepEqual(ids(shown.at(-1)), [JOIN_WORKSPACE.id]);
});

/** A machine of the person's own, as its server answers: a workspace is placed
 * under the id the page chose, as the page asks, and the same id answers the
 * one already placed (`place`). */
function machine(calls, {failOnce = false} = {}) {
  let fail = failOnce;
  return {
    host: {hostCert: {device_public_key: Array(32).fill(0xcd)}},
    workspaces: {
      place: async ({workspace, name}) => {
        calls.push(['place', workspace, name]);
        if (fail) {
          fail = false;
          throw new Error('the server closed before it answered');
        }
        return {workspace, name, generation: 1, members: []};
      },
    },
    dispose: async () => { calls.push(['dispose']); },
  };
}

test('a new workspace is made on a machine the person set up, and opened', async () => {
  const origin = 'https://node.example:8443';
  const calls = [];
  const lobby = machine(calls);
  const opened = [];
  const {mount, shown} = desktop();
  const actions = createWorkspaceActions({peer: null, machines: () => [origin],
    lobby: async at => { calls.push(['lobby', at]); return lobby; },
    open: async where => { opened.push(where); },
    ui: {askWorkspaceName: save => save('Acme'), fail: async text => assert.fail(text)}});
  await actions.attach(mount);
  assert.deepEqual(ids(shown.at(-1)), [NEW_WORKSPACE.id, JOIN_WORKSPACE.id]);
  await actions.perform(NEW_WORKSPACE.id);
  assert.equal(calls[0][0], 'lobby');
  assert.equal(calls[1][0], 'place');
  assert.equal(calls[1][2], 'Acme', 'the name the person typed');
  assert.match(calls[1][1], /^[0-9a-f-]{36}$/, 'the page places it under an id of its own');
  assert.deepEqual(calls[2], ['dispose']);
  // Opened as held by the machine's host, which the browser remembers it under.
  assert.deepEqual(opened, [{origin, workspace: calls[1][1], name: 'Acme', hostDevice: 'cd'.repeat(32)}]);
});

test('a make on the person’s own machine is placed under one id, so a second press places the same one', async () => {
  const origin = 'https://node.example:8443';
  const calls = [];
  const lobby = machine(calls, {failOnce: true});
  const opened = [];
  const {mount} = desktop();
  const actions = createWorkspaceActions({peer: null, machines: () => [origin],
    lobby: async at => { calls.push(['lobby', at]); return lobby; },
    open: async where => { opened.push(where); },
    ui: {askWorkspaceName: async save => {
      await save('Acme').catch(() => {});
      return save('Acme');
    }, fail: async text => assert.fail(text)}});
  await actions.attach(mount);
  await actions.perform(NEW_WORKSPACE.id);
  const placed = calls.filter(([call]) => call === 'place');
  assert.equal(placed.length, 2);
  assert.equal(placed[0][1], placed[1][1], 'the same id is placed again, never a second workspace');
  assert.equal(opened.length, 1);
});

test('in someone else’s workspace a person with a machine can still make a workspace on it', async () => {
  const origin = 'https://node.example:8443';
  const calls = [];
  const lobby = machine(calls);
  // The Lounge: a workspace on a host that is not Ada's.
  const lounge = {origin: 'https://lounge.example', workspace: 'lounge', userPublicKey: ADA,
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
  assert.equal(calls[0][0], 'lobby');
  assert.equal(calls[1][2], 'Acme');
  assert.equal(opened[0].origin, origin);
});

test('a person on a host that keeps workspaces of people’s own makes their next one there', async () => {
  const origin = 'https://lounge.example';
  const calls = [];
  // The Lounge: a listed workspace on a host that keeps a workspace of each
  // person's own for the people its entry lets in.
  const lounge = {origin, workspace: 'lounge', userPublicKey: ADA,
    host: {hostCert: {user_public_key: bytes('bb'.repeat(32)), device_public_key: Array(32).fill(0xcd),
      capabilities: {can_own_workspaces: true}}},
    enterOwnWorkspace: async ({name}) => { calls.push(['own', name]); return {workspace: 'studio-workspace', name}; }};
  const opened = [];
  const {mount, shown} = desktop();
  const actions = createWorkspaceActions({peer: lounge, machines: () => [],
    open: async where => { opened.push(where); },
    ui: {askWorkspaceName: save => save('Studio'), fail: async text => assert.fail(text)}});
  await actions.attach(mount);
  assert.ok(ids(shown.at(-1)).includes(NEW_WORKSPACE.id), 'the person is in a workspace of their own making on that host');
  await actions.perform(NEW_WORKSPACE.id);
  assert.deepEqual(calls, [['own', 'Studio']]);
  assert.deepEqual(opened, [{origin, workspace: 'studio-workspace', name: 'Studio', hostDevice: 'cd'.repeat(32)}]);
});

test('a create the server did not answer for is asked again under the same invocation, so the second press makes nothing', async () => {
  const asked = [];
  let fail = true;
  const {peer} = adasWorkspace({owner: false, keepsOwn: true});
  const lounge = {...peer, enterOwnWorkspace: async ({name, invocation}) => {
    asked.push([name, invocation]);
    if (fail) {
      fail = false;
      throw Object.assign(new Error('the action\'s provider did not answer in time'),
        {name: 'DeviceRefusal', code: 'unanswered'});
    }
    return {workspace: 'studio-workspace', name};
  }};
  const opened = [];
  const {mount} = desktop();
  // The dialog stays open after the failure, and the person presses Create again:
  // the same make, not a second one beside it.
  const actions = createWorkspaceActions({peer: lounge, machines: () => [],
    open: async where => { opened.push(where); },
    ui: {askWorkspaceName: async save => {
      await save('Studio').catch(() => {});
      return save('Studio');
    }, fail: async text => assert.fail(text)}});
  await actions.attach(mount);
  await actions.perform(NEW_WORKSPACE.id);
  assert.equal(asked.length, 2);
  assert.match(asked[0][1], /^[0-9a-f-]{36}$/, 'the page keeps an invocation of its own for the make');
  assert.equal(asked[0][1], asked[1][1], 'the second press asks the same make');
  assert.deepEqual(opened.map(where => where.workspace), ['studio-workspace']);
});

test('a new workspace is numbered past the names the switcher already holds', async () => {
  const asked = [];
  const {peer} = adasWorkspace({owner: false, keepsOwn: true});
  const lounge = {...peer,
    enterOwnWorkspace: async ({name}) => { asked.push(name); return {workspace: 'studio-workspace', name}; }};
  const opened = [];
  const {mount} = desktop();
  const actions = createWorkspaceActions({peer: lounge, machines: () => [],
    held: () => ['Studio', 'Home'],
    open: async where => { opened.push(where); },
    ui: {askWorkspaceName: save => save('Studio'), fail: async text => assert.fail(text)}});
  await actions.attach(mount);
  await actions.perform(NEW_WORKSPACE.id);
  assert.deepEqual(asked, ['Studio 2'], 'the name a workspace already holds is numbered past, never shared');
  assert.equal(opened[0].name, 'Studio 2');
});

test('an invite link is made when it is pressed for, and never before', async () => {
  const {peer, calls} = adasWorkspace({owner: false, letsIn: true});
  const copied = [];
  const shared = [];
  const {mount, shown} = desktop();
  const actions = createWorkspaceActions({peer, inviteLink: sealedLink,
    ui: {copy: async text => { copied.push(text); return true; },
      share: async offer => { shared.push(offer); },
      fail: async text => assert.fail(text)}});
  await actions.attach(mount);
  await new Promise(resolve => setTimeout(resolve, 0));
  assert.ok(ids(shown.at(-1)).includes(INVITE_LINK.id), 'whoever may invite is offered a link');
  assert.equal(calls.filter(call => call[0] === 'invite').length, 0, 'opening the page invites nobody');
  await actions.perform(INVITE_LINK.id);
  assert.deepEqual(copied, [SEALED], 'the press copied the short link it made');
  assert.equal(shared.length, 0, 'a copy that took needs no card');
  await new Promise(resolve => setTimeout(resolve, 0));
  assert.equal(calls.filter(call => call[0] === 'invite').length, 1, 'one press, one invitation');
});

test('a site that keeps no link hands over no invitation text: the press says why', async () => {
  const {peer} = adasWorkspace({owner: false, letsIn: true});
  const failed = [];
  const actions = createWorkspaceActions({peer, inviteLink: async () => { throw new Error('the site kept nothing'); },
    onError: () => {},
    ui: {copy: async () => assert.fail('nothing to copy'), share: async () => assert.fail('nothing to share'),
      fail: async text => { failed.push(text); }}});
  await actions.attach(desktop().mount);
  await actions.perform(INVITE_LINK.id);
  assert.deepEqual(failed, ['That did not work: the site kept nothing']);
});

test('a press whose copy the browser refuses shows the link to copy by hand', async () => {
  const {peer} = adasWorkspace({owner: false, letsIn: true});
  const shared = [];
  const actions = createWorkspaceActions({peer, inviteLink: sealedLink,
    ui: {copy: async () => false, share: async offer => { shared.push(offer); },
      fail: async text => assert.fail(text)}});
  await actions.attach(desktop().mount);
  await actions.perform(INVITE_LINK.id);
  assert.equal(shared.length, 1, 'the link is shown when the clipboard did not take it');
  assert.equal(shared[0].link, SEALED);
  assert.equal(shared[0].copied, true, 'the card tries the clipboard from its own opening');
});
