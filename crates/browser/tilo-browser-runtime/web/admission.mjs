/**
 * Getting into the room a link names, before anything about its target is
 * read. In order:
 * 1. the person's own room (`/me`) is this browser's own profile: no peer;
 * 2. an invitation is inspected (what the host verified: workspace, role,
 *    inviter), confirmed by the person, then redeemed; a pre-admission
 *    `token` admits at once, otherwise the owner admits while the page waits;
 *    a room service guest `ticket` is spent on this page's pairing code at
 *    the redemption door the web host names (`entry.json` `tickets`);
 *    with `pair`, this browser becomes a new device of that person once
 *    one of their devices approves its pairing code (the host relays);
 * 3. a member connects with this browser's existing account for that host
 *    (where other tabs hold every device key it has there, this tab goes on
 *    to 4 and 5 as a new device of the person);
 * 4. a host whose listed workspace has an admission door (`workspaces.mjs`)
 *    admits this browser's person at once; a link that names a room there
 *    is followed only if that is the room the person was admitted to;
 * 5. a host this browser set up for its person, or joined as them through
 *    an invitation, is reconnected to with the root the page holds
 *    (`reconnect`); the host decides whether that person is in the room;
 * 6. anyone else is told they have no access and who can invite them.
 * A scope without a room (the web host's home workspace, whose room the
 * page learns on admission) can only be entered through its host's door.
 * A link never grants anything: each step is the room's own door. A failure
 * to connect reads the same whether the room is missing or private, so a
 * link never tells a stranger which rooms exist. Only after admission does
 * the page read the room's catalogue to resolve the target.
 */
export const NO_ACCESS = 'You don’t have access to this workspace. Ask the person who shared the link to invite you.';

export class AdmissionError extends Error {
  constructor(code, message) {
    super(message);
    this.name = 'AdmissionError';
    this.code = code;
  }
}

/** `{peer}` once `peer` is ready to use. */
async function ready(peer) {
  await peer?.whenReady?.();
  return {peer};
}

/** Admission through `workspace`'s host door, held to the room the link names. */
async function viaHost({workspace, room, deps, onProgress}) {
  onProgress({phase: 'joining'});
  // The room a link names, if any: what this browser kept for a host set up
  // again is offered to be joined again only from the bare entry, where the
  // door's room is the one the person asked for.
  const peer = await deps.enterViaHost(room ? {workspace, room} : {workspace});
  if (room && peer?.room && peer.room !== room) {
    await peer.dispose?.();
    throw new AdmissionError('no-access', NO_ACCESS);
  }
  return ready(peer);
}

export async function admit({scope, secrets = {}, tickets, deps, onProgress = () => {}}) {
  if (secrets.malformed) {
    throw new AdmissionError('malformed', 'This link’s invitation is damaged, so Tilo did not use it');
  }
  if (scope.kind === 'own') return {peer: null};
  const {host: origin, room} = scope;
  const listed = deps.workspaceAt?.(origin) ?? null;
  if (!room) {
    if (!listed?.admission) throw new AdmissionError('no-access', NO_ACCESS);
    return viaHost({workspace: listed, room, deps, onProgress});
  }
  if (secrets.invite) {
    const offer = await deps.inspectInvitation(origin, secrets.invite);
    if (offer.room && offer.room !== room) {
      throw new AdmissionError('mismatch', 'This invitation is for another workspace than the link names');
    }
    if (!await deps.confirmInvitation(offer)) throw new AdmissionError('declined', 'You did not join');
    const peer = await deps.joinWorkspace({
      origin, invitation: secrets.invite, admissionToken: secrets.token,
      onProgress: event => onProgress(event),
    });
    return ready(peer);
  }
  if (secrets.ticket) {
    if (!tickets) throw new AdmissionError('unsupported', 'This page cannot redeem guest tickets');
    onProgress({phase: 'joining'});
    return ready(await deps.redeemTicket({origin, room, ticket: secrets.ticket, tickets}));
  }
  if (secrets.pair) {
    if (!deps.pairDevice) throw new AdmissionError('unsupported', 'This page cannot pair a device');
    onProgress({phase: 'pairing'});
    return ready(await deps.pairDevice({origin, room, userPublicKey: secrets.pair, workspace: listed}));
  }
  /** Steps 4 to 6: the host's door, else the root this page holds. */
  const withoutAccount = async busy => {
    if (listed?.admission) return viaHost({workspace: listed, room, deps, onProgress});
    let peer = null;
    try {
      peer = await deps.reconnect?.({origin, room, onProgress});
    } catch (error) {
      // Other tabs hold every device key this browser can get in with: not
      // the host's answer.
      if (error?.name === 'CustodyError' && error.code === 'custody-busy') throw error;
      // As below: the host's reason could tell a missing room from a private one.
      console.warn('admission: reconnecting failed', error?.code ?? error?.name ?? 'error');
    }
    if (!peer) throw busy ?? new AdmissionError('no-access', NO_ACCESS);
    return ready(peer);
  };
  const accounts = await deps.readAccounts(origin);
  if (!accounts.length) return withoutAccount();
  const account = accounts.length === 1 ? accounts[0] : await deps.chooseAccount(accounts);
  onProgress({phase: 'connecting'});
  try {
    // A host that revoked this device answers the connection and refuses it
    // once the peer is certified: that, too, is no access.
    return await ready(await deps.connectMember({origin, room, account}));
  } catch (error) {
    // Other tabs of this browser hold every device key the account has here:
    // not the host's answer, and not no access. A tab is a device of its
    // own, so this one gets in as a new device of the person, the way a
    // browser without the account would. Only where it cannot does it say
    // so. (An account with no device key here is no access, as a host's
    // refusal is.)
    if (error?.name === 'CustodyError' && error.code === 'custody-busy') return withoutAccount(error);
    // The host's reason could tell "no such room" from "not a member".
    console.warn('admission: connecting failed', error?.code ?? error?.name ?? 'error');
    throw new AdmissionError('no-access', NO_ACCESS);
  }
}
