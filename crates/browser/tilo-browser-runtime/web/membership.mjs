/**
 * Who belongs to a room the host governs, administered by a device of the
 * host's own person (the owner): invitations, the people waiting to join,
 * members, their roles and removal. Every call is one `/rooms` request over
 * the peer's session; the host authorizes each one.
 *
 * `createMembership({rooms, room, displayName})` takes `rooms(requestJson)`
 * (the wasm peer's `rooms`, resolving to the reply's JSON), the room's uuid
 * and the name invitations record as their inviter.
 */

const ROLES = {viewer: 'Viewer', commenter: 'Commenter', editor: 'Editor'};
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
const invitation = value => ({
  inviteId: hex(value.invite_id),
  role: pageRole(value.role),
  expiresAtMs: Math.floor(value.expires_at_ns / 1e6),
});

export function createMembership({rooms, room, displayName = '', now = () => Date.now()}) {
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
     * The room as governance records it: `{room, name, generation, members,
     * waiting, invitations}`. `members` include removed people (`removed`);
     * `waiting` are people who redeemed an invitation and wait for `admit`
     * or `decline`; `invitations` are those still redeemable.
     */
    async list() {
      const [state, requests, pending] = await Promise.all([
        ask('List').then(reply => {
          const found = expect(reply, 'Rooms').find(value => value.room === room);
          if (!found) throw new Error('the host does not govern this room');
          return summary(found);
        }),
        ask({Requests: {room}}).then(reply => expect(reply, 'Requests')),
        ask({Pending: {room}}).then(reply => expect(reply, 'Pending')),
      ]);
      return {...state, waiting: requests.map(waiting), invitations: pending.map(invitation)};
    },
    /** Admit a waiting person (`requestId` from `list().waiting`). */
    async admit(requestIdHex) {
      return roomOf(await ask({Admit: {room, request_id: bytesOf(requestIdHex)}}));
    },
    /** Turn a waiting person away. */
    async decline(requestIdHex) {
      expect(await ask({Decline: {room, request_id: bytesOf(requestIdHex)}}), 'Requests');
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
    /** Withdraw an invitation nobody redeemed yet. */
    async cancelInvitation(inviteId) {
      return expect(await ask({Cancel: {room, request_id: requestId(), invite_id: bytesOf(inviteId)}}), 'Pending')
        .map(invitation);
    },
  };
}
