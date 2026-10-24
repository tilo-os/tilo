/**
 * Who belongs to a room the host governs, administered by a device of the
 * host's own person (the owner): invitations, the people waiting to join,
 * members, their roles and removal. Every call is one `/rooms` request over
 * the peer's session; the host authorizes each one.
 *
 * `createMembership({rooms, room, displayName})` takes `rooms(requestJson)`
 * (the wasm peer's `rooms`, resolving to the reply's JSON), the room's uuid
 * and the name invitations record as their inviter. `createWorkspaces`
 * lists, creates and enters the rooms the host governs, before a peer is in
 * one.
 */

const ROLES = {viewer: 'Viewer', commenter: 'Commenter', editor: 'Editor', moderator: 'Moderator'};
const DAY_SECONDS = 24 * 60 * 60;
/** How long an invitation stays redeemable unless `expiresIn` says. */
export const DEFAULT_INVITATION_SECONDS = 7 * DAY_SECONDS;
/** The longest the host lets an invitation stay redeemable. */
export const MAX_INVITATION_SECONDS = 30 * DAY_SECONDS;

const hex = bytes => Array.from(bytes, byte => byte.toString(16).padStart(2, '0')).join('');
const bytesOf = text => {
  if (typeof text !== 'string' || !/^([0-9a-f]{2}){32}$/.test(text)) throw new TypeError('expected 64 hex digits');
  return Array.from({length: 32}, (_, index) => parseInt(text.slice(index * 2, index * 2 + 2), 16));
};
const requestId = () => Array.from(crypto.getRandomValues(new Uint8Array(32)));

/** Signed invitation bytes as the text people pass around (base64url, no padding). */
export function encodeInvitation(bytes) {
  let binary = '';
  for (const byte of bytes) binary += String.fromCharCode(byte);
  return btoa(binary).replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/, '');
}

const wireRole = role => {
  const wire = ROLES[role];
  if (!wire) throw new TypeError(`role must be one of ${Object.keys(ROLES).join(', ')}`);
  return wire;
};
const pageRole = role => role.toLowerCase();

const member = value => ({
  userPublicKey: hex(value.user_public_key),
  name: value.name ?? null,
  role: pageRole(value.role),
  owner: value.owner,
  removed: value.removed,
});
const waiting = value => ({
  requestId: hex(value.request_id),
  userPublicKey: hex(value.user_public_key),
  devicePublicKey: hex(value.device_public_key),
  name: value.name,
  role: pageRole(value.role),
  requestedAtMs: Math.floor(value.requested_at_ns / 1e6),
});
const listed = value => ({
  requestId: hex(value.request_id),
  contact: value.contact,
  addedAtMs: Math.floor(value.added_at_ns / 1e6),
});
const invitation = value => ({
  inviteId: hex(value.invite_id),
  role: pageRole(value.role),
  expiresAtMs: Math.floor(value.expires_at_ns / 1e6),
});

const notice = value => {
  const {PreAdmitted: admitted, Reported: reported} = value;
  if (reported) {
    return {
      kind: 'reported', userPublicKey: hex(reported.user_public_key), reason: reported.reason,
      atMs: Math.floor(reported.at_ns / 1e6),
    };
  }
  if (!admitted) return {kind: 'unknown'};
  return {
    kind: 'pre-admitted', userPublicKey: hex(admitted.user_public_key), name: admitted.name,
    label: admitted.label, role: pageRole(admitted.role), atMs: Math.floor(admitted.at_ns / 1e6),
  };
};

export function createMembership({rooms, room, displayName = '', preAdmit, now = () => Date.now()}) {
  const ask = async request => {
    if (!room) throw new Error('this peer joined no room');
    const reply = JSON.parse(await rooms(JSON.stringify(request)));
    if (reply.Refused !== undefined) throw new Error(reply.Refused);
    return reply;
  };
  const expect = (reply, kind) => {
    if (reply[kind] === undefined) throw new Error(`the host answered ${Object.keys(reply)[0]}, not ${kind}`);
    return reply[kind];
  };
  const summary = value => ({
    room: value.room, name: value.name, generation: value.generation,
    members: value.members.map(member),
  });
  const roomOf = reply => summary(expect(reply, 'Room'));

  return {
    /**
     * Issue an invitation to the room: resolves to its text, which
     * `inspectInvitation` and `joinWorkspace` take. It names this person
     * (`displayName`) as the inviter; anyone holding it may ask to join, and
     * the owner admits them.
     */
    async createInvitation({role = 'editor', expiresIn = DEFAULT_INVITATION_SECONDS} = {}) {
      if (!Number.isFinite(expiresIn) || expiresIn <= 0 || expiresIn > MAX_INVITATION_SECONDS) {
        throw new RangeError(`expiresIn must be 1 to ${MAX_INVITATION_SECONDS} seconds`);
      }
      const name = displayName.trim().slice(0, 80);
      const issued = expect(await ask({Invite: {
        room, request_id: requestId(), role: wireRole(role),
        expires_at_ns: Math.floor((now() + expiresIn * 1000)) * 1e6,
        inviter_name: name || null,
      }}), 'Invitation');
      return encodeInvitation(issued.signed_invitation);
    },
    /**
     * Pre-admit a person someone else vouches for: whoever redeems the
     * returned invitation text with the one-time token whose SHA-256 is
     * `tokenSha256` (hex) is admitted as `role` without the owner deciding
     * again. The owner's unlocked root signs the grant; `label` names the
     * person in the owner's notices (an email address, say).
     */
    async preAdmit({tokenSha256, label, role = 'editor', expiresIn = DEFAULT_INVITATION_SECONDS} = {}) {
      if (!preAdmit) throw new Error('pre-admitting needs the owner\'s unlocked identity');
      bytesOf(tokenSha256);
      if (typeof label !== 'string' || !label.trim()) throw new TypeError('a pre-admission needs a label');
      if (!Number.isFinite(expiresIn) || expiresIn <= 0 || expiresIn > MAX_INVITATION_SECONDS) {
        throw new RangeError(`expiresIn must be 1 to ${MAX_INVITATION_SECONDS} seconds`);
      }
      if (!room) throw new Error('this peer joined no room');
      const name = displayName.trim().slice(0, 80);
      const reply = JSON.parse(await preAdmit(JSON.stringify({
        requestId: hex(requestId()), room, role: wireRole(role), tokenSha256, label: label.trim(),
        expiresInSeconds: Math.floor(expiresIn), inviterName: name || null,
      })));
      if (reply.Refused !== undefined) throw new Error(reply.Refused);
      return encodeInvitation(expect(reply, 'Invitation').signed_invitation);
    },
    /**
     * The room as governance records it: `{room, name, generation, members,
     * waiting, invitations, notices}`. `members` include removed people
     * (`removed`); `waiting` are people who redeemed an invitation and wait
     * for `admit` or `decline`; `invitations` are those still redeemable;
     * `notices` tell the owner who joined under a pre-admission
     * (`{kind: 'pre-admitted', userPublicKey, name, label, role, atMs}`).
     */
    async list() {
      const [state, requests, pending, notices] = await Promise.all([
        ask('List').then(reply => {
          const found = expect(reply, 'Rooms').find(value => value.room === room);
          if (!found) throw new Error('the host does not govern this room');
          return summary(found);
        }),
        ask({Requests: {room}}).then(reply => expect(reply, 'Requests')),
        ask({Pending: {room}}).then(reply => expect(reply, 'Pending')),
        ask({Notices: {room}}).then(reply => expect(reply, 'Notices')),
      ]);
      return {
        ...state, waiting: requests.map(waiting), invitations: pending.map(invitation),
        notices: notices.map(notice),
      };
    },
    /** Only the people waiting to join, as `list().waiting`: one request. */
    async requests() {
      return expect(await ask({Requests: {room}}), 'Requests').map(waiting);
    },
    /** Admit a waiting person (`requestId` from `list().waiting`). */
    async admit(requestIdHex) {
      return roomOf(await ask({Admit: {room, request_id: bytesOf(requestIdHex)}}));
    },
    /** Turn a waiting person away. */
    async decline(requestIdHex) {
      expect(await ask({Decline: {room, request_id: bytesOf(requestIdHex)}}), 'Requests');
    },
    /** The addresses left at the room's door without an invitation, oldest
     * first: `{requestId, contact, addedAtMs}` each, nobody to admit but an
     * address to send an invitation to. For whoever may invite. */
    async waitList() {
      return expect(await ask({WaitList: {room}}), 'WaitList').map(listed);
    },
    /** Take an address off the wait list (`requestId` from `waitList`). */
    async unlist(requestIdHex) {
      expect(await ask({Unlist: {room, request_id: bytesOf(requestIdHex)}}), 'WaitList');
    },
    /** Change a member's role. */
    async setRole(userPublicKey, role) {
      return roomOf(await ask({SetRole: {
        room, request_id: requestId(), user_public_key: bytesOf(userPublicKey), role: wireRole(role),
      }}));
    },
    /** Remove a member and every device of theirs. */
    async remove(userPublicKey) {
      return roomOf(await ask({Remove: {room, request_id: requestId(), user_public_key: bytesOf(userPublicKey)}}));
    },
    /** Remove a person and keep them out until `unban`. The owner, or a
     * Moderator for people ranked below Moderator; the host decides. */
    async ban(userPublicKey) {
      return roomOf(await ask({Ban: {room, request_id: requestId(), user_public_key: bytesOf(userPublicKey)}}));
    },
    /** Let a banned person be admitted again. */
    async unban(userPublicKey) {
      return roomOf(await ask({Unban: {room, request_id: requestId(), user_public_key: bytesOf(userPublicKey)}}));
    },
    /** Withdraw an invitation nobody redeemed yet. */
    async cancelInvitation(inviteId) {
      return expect(await ask({Cancel: {room, request_id: requestId(), invite_id: bytesOf(inviteId)}}), 'Pending')
        .map(invitation);
    },
  };
}

/**
 * The workspaces (rooms) the host governs, for a device of its own person:
 * `list()` resolves to `[{room, name, generation, members}]`,
 * `create(name)` to the new one, `enter(room)` admits this device as the
 * room's owner and resolves to it. A peer connected with `room` then works
 * in it.
 */
export function createWorkspaces({rooms}) {
  const ask = async request => {
    const reply = JSON.parse(await rooms(JSON.stringify(request)));
    if (reply.Refused !== undefined) throw new Error(reply.Refused);
    return reply;
  };
  const workspace = value => ({
    room: value.room, name: value.name, generation: value.generation,
    members: value.members.map(member),
  });
  const one = reply => {
    if (reply.Room === undefined) throw new Error(`the host answered ${Object.keys(reply)[0]}, not Room`);
    return workspace(reply.Room);
  };
  return {
    async list() {
      const reply = await ask('List');
      if (reply.Rooms === undefined) throw new Error(`the host answered ${Object.keys(reply)[0]}, not Rooms`);
      return reply.Rooms.map(workspace);
    },
    async create(name) {
      if (typeof name !== 'string' || !name.trim()) throw new TypeError('a workspace needs a name');
      return one(await ask({Create: {name: name.trim()}}));
    },
    async enter(room) {
      return one(await ask({Enter: {room}}));
    },
  };
}
