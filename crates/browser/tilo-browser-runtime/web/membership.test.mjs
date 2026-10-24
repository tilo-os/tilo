import assert from 'node:assert/strict';
import test from 'node:test';
import {createMembership, createWorkspaces, encodeInvitation, MAX_INVITATION_SECONDS} from './membership.mjs';

const ROOM = '6f1d3a52-1c7e-4b8e-9d2a-0f4a8c3e5b71';
const key = byte => Array(32).fill(byte);

function fakeHost(replies) {
  const sent = [];
  return {
    sent,
    rooms: async json => {
      const request = JSON.parse(json);
      sent.push(request);
      const kind = typeof request === 'string' ? request : Object.keys(request)[0];
      return JSON.stringify(replies[kind](request));
    },
  };
}

const room = {room: ROOM, workspace_id: 7, name: 'Acme', generation: 3, members: [
  {user_public_key: key(0xaa), name: 'Ada', role: 'Editor', owner: true, removed: false},
  {user_public_key: key(0xbb), name: 'Bo', role: 'Viewer', owner: false, removed: false},
]};

test('an invitation names the inviter and comes back as base64url text', async () => {
  const host = fakeHost({Invite: () => ({Invitation: {invite_id: key(1), signed_invitation: [0xfb, 0xff, 0x01]}})});
  const membership = createMembership({rooms: host.rooms, room: ROOM, displayName: ' Ada Lovelace ', now: () => 1_000});
  assert.equal(await membership.createInvitation({role: 'commenter', expiresIn: 60}), encodeInvitation([0xfb, 0xff, 0x01]));
  assert.equal(encodeInvitation([0xfb, 0xff, 0x01]), '-_8B');
  const {Invite: invite} = host.sent[0];
  assert.equal(invite.room, ROOM);
  assert.equal(invite.role, 'Commenter');
  assert.equal(invite.inviter_name, 'Ada Lovelace');
  assert.equal(invite.expires_at_ns, 61_000 * 1e6);
  assert.equal(invite.request_id.length, 32);
  await assert.rejects(membership.createInvitation({role: 'owner'}), TypeError);
  await assert.rejects(membership.createInvitation({expiresIn: MAX_INVITATION_SECONDS + 1}), RangeError);
});

test('the owner lists, admits, declines and removes people', async () => {
  const host = fakeHost({
    List: () => ({Rooms: [room]}),
    Requests: () => ({Requests: [{request_id: key(9), user_public_key: key(0xcc), device_public_key: key(0xcd), name: 'Cy', role: 'Editor', requested_at_ns: 5e6},
      {request_id: key(8), user_public_key: key(0), device_public_key: key(0), name: 'ed@example.org', role: 'Editor', requested_at_ns: 6e6}]}),
    Pending: () => ({Pending: [{invite_id: key(4), role: 'Editor', expires_at_ns: 9e9}]}),
    Notices: () => ({Notices: [{PreAdmitted: {user_public_key: key(0xdd), name: 'Di', label: 'di@example.com', role: 'Editor', at_ns: 7e6}}]}),
    Admit: () => ({Room: room}),
    Decline: () => ({Requests: []}),
    Remove: request => ({Room: {...room, members: room.members.map(member =>
      member.user_public_key.every((byte, index) => byte === request.Remove.user_public_key[index]) ? {...member, removed: true} : member)}}),
    Cancel: () => ({Refused: 'invitation was already redeemed'}),
  });
  const membership = createMembership({rooms: host.rooms, room: ROOM});
  const listed = await membership.list();
  assert.equal(listed.name, 'Acme');
  assert.deepEqual(listed.members[0], {userPublicKey: 'aa'.repeat(32), name: 'Ada', role: 'editor', owner: true, removed: false});
  assert.deepEqual(listed.waiting, [
    {requestId: '09'.repeat(32), userPublicKey: 'cc'.repeat(32), devicePublicKey: 'cd'.repeat(32), name: 'Cy', role: 'editor', requestedAtMs: 5, contact: null},
    // Left at the door without an invitation: the address to invite.
    {requestId: '08'.repeat(32), userPublicKey: '00'.repeat(32), devicePublicKey: '00'.repeat(32), name: 'ed@example.org', role: 'editor',
      requestedAtMs: 6, contact: 'ed@example.org'},
  ]);
  assert.deepEqual(listed.invitations, [{inviteId: '04'.repeat(32), role: 'editor', expiresAtMs: 9000}]);
  assert.deepEqual(listed.notices, [{kind: 'pre-admitted', userPublicKey: 'dd'.repeat(32), name: 'Di', label: 'di@example.com', role: 'editor', atMs: 7}]);

  await membership.admit('09'.repeat(32));
  assert.deepEqual(host.sent.at(-1), {Admit: {room: ROOM, request_id: key(9)}});
  await membership.decline('09'.repeat(32));
  const after = await membership.remove('bb'.repeat(32));
  assert.equal(after.members[1].removed, true);
  await assert.rejects(membership.cancelInvitation('04'.repeat(32)), /already redeemed/);
  await assert.rejects(membership.admit('not hex'), TypeError);
});

test('a peer without a room administers nothing', async () => {
  const membership = createMembership({rooms: async () => { throw new Error('unreachable'); }});
  await assert.rejects(membership.list(), /no room/);
});

test('the host\'s workspaces are listed, created and entered without a room', async () => {
  const host = fakeHost({
    List: () => ({Rooms: [room]}),
    Create: request => ({Room: {...room, name: request.Create.name}}),
    Enter: () => ({Refused: 'room administration requires a device of the relay\'s user'}),
  });
  const workspaces = createWorkspaces({rooms: host.rooms});
  assert.deepEqual((await workspaces.list()).map(value => [value.room, value.name]), [[ROOM, 'Acme']]);
  assert.equal((await workspaces.create(' Acme Inc ')).name, 'Acme Inc');
  await assert.rejects(workspaces.create(' '), TypeError);
  await assert.rejects(workspaces.enter(ROOM), /relay's user/);
});

test('the owner pre-admits a person by their token\'s digest', async () => {
  const signed = [];
  const membership = createMembership({
    rooms: async () => { throw new Error('unused'); }, room: ROOM, displayName: 'Ada',
    preAdmit: async json => { signed.push(JSON.parse(json)); return JSON.stringify({Invitation: {invite_id: key(1), signed_invitation: [1, 2, 3]}}); },
  });
  const digest = 'ab'.repeat(32);
  assert.equal(await membership.preAdmit({tokenSha256: digest, label: ' cy@example.com ', expiresIn: 3600}), encodeInvitation([1, 2, 3]));
  assert.equal(signed[0].room, ROOM);
  assert.equal(signed[0].role, 'Editor');
  assert.equal(signed[0].tokenSha256, digest);
  assert.equal(signed[0].label, 'cy@example.com');
  assert.equal(signed[0].expiresInSeconds, 3600);
  assert.equal(signed[0].inviterName, 'Ada');
  assert.match(signed[0].requestId, /^[0-9a-f]{64}$/);
  await assert.rejects(membership.preAdmit({tokenSha256: 'nope', label: 'x'}), TypeError);
  await assert.rejects(membership.preAdmit({tokenSha256: digest, label: ' '}), TypeError);
  await assert.rejects(createMembership({rooms: async () => '{}', room: ROOM}).preAdmit({tokenSha256: digest, label: 'x'}), /unlocked identity/);
});
