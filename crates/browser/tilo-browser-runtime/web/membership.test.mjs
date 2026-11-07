import assert from 'node:assert/strict';
import test from 'node:test';
import {createMembership, createWorkspaces, encodeInvitation, MAX_INVITATION_SECONDS} from './membership.mjs';

const WORKSPACE = '6f1d3a52-1c7e-4b8e-9d2a-0f4a8c3e5b71';
const key = byte => Array(32).fill(byte);

function fakeHost(replies) {
  const sent = [];
  return {
    sent,
    workspaces: async json => {
      const request = JSON.parse(json);
      sent.push(request);
      const kind = typeof request === 'string' ? request : Object.keys(request)[0];
      return JSON.stringify(replies[kind](request));
    },
  };
}

const room = {room: WORKSPACE, workspace_id: 7, name: 'Acme', generation: 3, members: [
  {user_public_key: key(0xaa), name: 'Ada', role: 'Editor', owner: true, removed: false},
  {user_public_key: key(0xbb), name: 'Bo', role: 'Viewer', owner: false, removed: false},
]};

test('an invitation names the inviter and comes back as base64url text', async () => {
  const host = fakeHost({Invite: () => ({Invitation: {invite_id: key(1), signed_invitation: [0xfb, 0xff, 0x01]}})});
  const membership = createMembership({workspaces: host.workspaces, workspace: WORKSPACE, displayName: ' Ada Lovelace ', now: () => 1_000});
  assert.equal(await membership.createInvitation({role: 'commenter', expiresIn: 60}), encodeInvitation([0xfb, 0xff, 0x01]));
  assert.equal(encodeInvitation([0xfb, 0xff, 0x01]), '-_8B');
  const {Invite: invite} = host.sent[0];
  assert.equal(invite.room, WORKSPACE);
  assert.equal(invite.role, 'Commenter');
  assert.equal(invite.inviter_name, 'Ada Lovelace');
  assert.equal(invite.expires_at_ns, 61_000 * 1e6);
  assert.equal(invite.request_id.length, 32);
  assert.equal('wait_list_entry' in invite, false);
  // An invitation for a wait-list entry names the entry.
  await membership.createInvitation({waitListEntry: '08'.repeat(32)});
  assert.deepEqual(host.sent.at(-1).Invite.wait_list_entry, key(8));
  await assert.rejects(membership.createInvitation({waitListEntry: 'not hex'}), TypeError);
  await assert.rejects(membership.createInvitation({role: 'owner'}), TypeError);
  await assert.rejects(membership.createInvitation({expiresIn: MAX_INVITATION_SECONDS + 1}), RangeError);
});

test('the owner lists invitations and people, invites an address and removes a person', async () => {
  const host = fakeHost({
    List: () => ({Rooms: [room]}),
    WaitList: () => ({WaitList: [{request_id: key(8), contact: 'Ed@example.org', added_at_ns: 6e6, invited_at_ns: null},
      {request_id: key(7), contact: 'fy@example.org', added_at_ns: 6e6, invited_at_ns: 8e6}]}),
    Unlist: () => ({WaitList: []}),
    MailInvite: () => ({WaitList: []}),
    Pending: () => ({Pending: [{invite_id: key(4), role: 'Editor', expires_at_ns: 9e9}]}),
    Notices: () => ({Notices: [{PreAdmitted: {user_public_key: key(0xdd), name: 'Di', label: 'di@example.com', role: 'Editor', at_ns: 7e6}}]}),
    Remove: request => ({Room: {...room, members: room.members.map(member =>
      member.user_public_key.every((byte, index) => byte === request.Remove.user_public_key[index]) ? {...member, removed: true} : member)}}),
    Cancel: () => ({Refused: 'invitation was already redeemed'}),
  });
  const membership = createMembership({workspaces: host.workspaces, workspace: WORKSPACE});
  const listed = await membership.list();
  assert.equal(listed.name, 'Acme');
  assert.deepEqual(listed.members[0], {userPublicKey: 'aa'.repeat(32), name: 'Ada', role: 'editor', owner: true, removed: false});
  // Addresses left at the door are a list of their own: nobody to admit.
  assert.deepEqual(await membership.waitList(), [{requestId: '08'.repeat(32), contact: 'Ed@example.org', addedAtMs: 6, invitedAtMs: null},
    {requestId: '07'.repeat(32), contact: 'fy@example.org', addedAtMs: 6, invitedAtMs: 8}]);
  await membership.unlist('08'.repeat(32));
  assert.deepEqual(host.sent.at(-1), {Unlist: {room: WORKSPACE, request_id: key(8)}});
  await membership.mailInvite('07'.repeat(32), 'https://tilo.example/?invite=AAEC');
  assert.deepEqual(host.sent.at(-1), {MailInvite: {room: WORKSPACE, wait_list_entry: key(7), link: 'https://tilo.example/?invite=AAEC'}});
  assert.deepEqual(listed.invitations, [{inviteId: '04'.repeat(32), role: 'editor', expiresAtMs: 9000}]);
  assert.deepEqual(listed.notices, [{kind: 'pre-admitted', userPublicKey: 'dd'.repeat(32), name: 'Di', label: 'di@example.com', role: 'editor', atMs: 7}]);

  const after = await membership.remove('bb'.repeat(32));
  assert.equal(after.members[1].removed, true);
  await assert.rejects(membership.cancelInvitation('04'.repeat(32)), /already redeemed/);
});

test('a peer without a workspace administers nothing', async () => {
  const membership = createMembership({workspaces: async () => { throw new Error('unreachable'); }});
  await assert.rejects(membership.list(), /no workspace/);
});

test('the host\'s workspaces are listed, created and entered without a workspace', async () => {
  const host = fakeHost({
    List: () => ({Rooms: [room]}),
    Create: request => ({Room: {...room, name: request.Create.name}}),
    Enter: () => ({Refused: 'room administration requires a device of the relay\'s user'}),
  });
  const workspaces = createWorkspaces({workspaces: host.workspaces});
  assert.deepEqual((await workspaces.list()).map(value => [value.room, value.name]), [[WORKSPACE, 'Acme']]);
  assert.equal((await workspaces.create(' Acme Inc ')).name, 'Acme Inc');
  await assert.rejects(workspaces.create(' '), TypeError);
  await assert.rejects(workspaces.enter(WORKSPACE), /relay's user/);
});

test('the owner pre-admits a person by their token\'s digest', async () => {
  const signed = [];
  const membership = createMembership({
    workspaces: async () => { throw new Error('unused'); }, workspace: WORKSPACE, displayName: 'Ada',
    preAdmit: async json => { signed.push(JSON.parse(json)); return JSON.stringify({Invitation: {invite_id: key(1), signed_invitation: [1, 2, 3]}}); },
  });
  const digest = 'ab'.repeat(32);
  assert.equal(await membership.preAdmit({tokenSha256: digest, label: ' cy@example.com ', expiresIn: 3600}), encodeInvitation([1, 2, 3]));
  assert.equal(signed[0].workspace, WORKSPACE);
  assert.equal(signed[0].role, 'Editor');
  assert.equal(signed[0].tokenSha256, digest);
  assert.equal(signed[0].label, 'cy@example.com');
  assert.equal(signed[0].expiresInSeconds, 3600);
  assert.equal(signed[0].inviterName, 'Ada');
  assert.match(signed[0].requestId, /^[0-9a-f]{64}$/);
  await assert.rejects(membership.preAdmit({tokenSha256: 'nope', label: 'x'}), TypeError);
  await assert.rejects(membership.preAdmit({tokenSha256: digest, label: ' '}), TypeError);
  await assert.rejects(createMembership({workspaces: async () => '{}', workspace: WORKSPACE}).preAdmit({tokenSha256: digest, label: 'x'}), /unlocked identity/);
});
