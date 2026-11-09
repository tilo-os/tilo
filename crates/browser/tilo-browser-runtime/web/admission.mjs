/**
 * Getting into the workspace a link names, before anything about its target is
 * read. In order:
 * 1. the person's own desktop (`/me`) is this browser's, with no peer,
 *    once the home's door let them in where it gates (`ownDesktop`);
 * 2. an invitation is inspected (what the host verified: workspace, role,
 *    inviter, how many people it has), confirmed by the person, then
 *    redeemed. An invitation is the approval of whoever made it: the host
 *    admits it at once, and nobody in the workspace answers for it. A link
 *    a site hands over (`https://<site>/i/<handle>#<key>`) names the site
 *    that keeps it, whose shelf answers with the invitation and the host it
 *    admits into (`viaSealedLink`); the long form names the invitation's own
 *    text, or its handle and key as `?invite=` after a workspace's route. A
 *    pre-admission `token` admits at once too; a workspace service guest
 *    `ticket` is spent on this page's pairing code at the redemption door
 *    the web host names (`entry.json` `tickets`); with `pair`, this browser
 *    becomes a new device of that person once one of their devices approves
 *    its pairing code (the host relays): the device that made the link,
 *    when it names an invite (`pairing.mjs`);
 * 3. a member connects with this browser's existing account for that host
 *    (where other tabs hold every device key it has there, this tab goes on
 *    to 4 and 5 as a new device of the person);
 * 4. a host whose listed workspace has an admission door (`workspaces.mjs`)
 *    admits this browser's person at once; a link that names a workspace there
 *    is followed only if that is the workspace the person was admitted to;
 * 5. a host this browser set up for its person, or joined as them through
 *    an invitation, is reconnected to with the root the page holds
 *    (`reconnect`); the host decides whether that person is in the workspace;
 * 6. anyone else is told they have no access and who can invite them.
 * A scope without a workspace (the web host's home workspace, whose workspace the
 * page learns on admission) can only be entered through its host's door.
 * A link never grants anything: each step is the workspace's own door. A failure
 * to connect reads the same whether the workspace is missing or private, so a
 * link never tells a stranger which workspaces exist. Only after admission does
 * the page read the workspace's catalogue to resolve the target.
 */
import {parsePairSecret} from './pairing.mjs';

export const NO_ACCESS = 'You don’t have access to this workspace. Ask the person who shared the link to invite you.';
/** An invitation the site no longer keeps, or one that does not open: the
 * same words for both, so a link tells nothing about invitations it does not
 * hold. */
const UNUSABLE_INVITATION = 'This link’s invitation is no longer there. Ask the person who invited you for a new one.';

export class AdmissionError extends Error {
  constructor(code, message) {
    super(message);
    this.name = 'AdmissionError';
    this.code = code;
  }
}

/** The invitation `secret` names, as the site at `site` keeps it: the
 * invitation's own text, and the host it admits into where the inviter named
 * one. */
async function openedAt({site, secret, deps}) {
  try {
    return await deps.openInvitation(site, secret);
  } catch (error) {
    console.warn('admission: the invitation did not open', error?.message ?? error);
    throw new AdmissionError('invitation-unusable', UNUSABLE_INVITATION);
  }
}

/** What `invitation` offers at `origin`, shown to the person, then joined
 * with: the invitation is the approval of whoever made it, so the host
 * admits it at once and nobody in the workspace answers for it. */
async function viaInvitation({origin, invitation, room = null, secrets, deps, onProgress}) {
  const offer = await deps.inspectInvitation(origin, invitation);
  if (room && offer.room && offer.room !== room) {
    throw new AdmissionError('mismatch', 'This invitation is for another workspace than the link names');
  }
  if (!await deps.confirmInvitation(offer)) throw new AdmissionError('declined', 'You did not join');
  return ready(await deps.joinWorkspace({
    origin, invitation, admissionToken: secrets.token,
    onProgress: event => onProgress(event),
  }));
}

/** A link a site hands over (`https://<site>/i/<handle>#<key>`): the site
 * keeps the invitation, sealed, and its shelf answers with the invitation —
 * never with the key, which only the link carries — and with where it admits
 * into. A shelf of a build that named no host is taken to mean the site's
 * home workspace's host, and the site itself where it has no home. */
async function viaSealedLink({scope, secrets, home, deps, onProgress}) {
  const opened = await openedAt({site: scope.site, secret: `${scope.handle}${secrets.invite}`, deps});
  const origin = opened.host ?? home?.host?.origin ?? scope.site;
  return viaInvitation({origin, invitation: opened.invitation, secrets, deps, onProgress});
}

/** `{peer}` once `peer` is ready to use. */
async function ready(peer) {
  await peer?.whenReady?.();
  return {peer};
}

/** Admission through `workspace`'s host door, held to the workspace the link names. */
async function viaHost({workspace, room, deps, onProgress}) {
  onProgress({phase: 'joining'});
  // The workspace a link names, if any: what this browser kept for a host set up
  // again is offered to be joined again only from the bare entry, where the
  // door's workspace is the one the person asked for.
  const peer = await deps.enterViaHost(room ? {workspace, room} : {workspace});
  // The door has nothing to admit yet: an embed shows its embedder's fallback
  // until the visitor's first gesture (`entry-page.mjs`), and nothing mounts
  // before it.
  if (peer?.idle) return {peer: null, idle: true};
  if (room && peer?.workspace && peer.workspace !== room) {
    await peer.dispose?.();
    throw new AdmissionError('no-access', NO_ACCESS);
  }
  return ready(peer);
}

/** The person's own desktop: this browser's, with no peer. What syncs
 * across their devices lives in workspaces, as everything shared does.
 * Where the home lets only some in (`deps.ownDesktopOpen`), a browser no
 * host let in is sent to the home's door instead, which says how to get
 * in; a seat in the workspace is not a desktop of their own. */
async function ownDesktop({home, deps, onProgress}) {
  const gated = Boolean(home?.admission && deps.ownDesktopOpen) && !await deps.ownDesktopOpen(home);
  if (gated) {
    const {peer} = await viaHost({workspace: home, deps, onProgress});
    await peer?.dispose?.();
    if (!await deps.ownDesktopOpen(home)) throw new AdmissionError('no-access', NO_ACCESS);
  }
  return {peer: null};
}

export async function admit({scope, secrets = {}, tickets, home = null, deps, onProgress = () => {}}) {
  if (secrets.malformed) {
    throw new AdmissionError('malformed', 'This link’s invitation is damaged, so Tilo did not use it');
  }
  if (scope.kind === 'own') return ownDesktop({home, deps, onProgress});
  if (scope.kind === 'sealed') return viaSealedLink({scope, secrets, home, deps, onProgress});
  const {host: origin, room} = scope;
  const listed = deps.workspaceAt?.(origin) ?? null;
  if (!room) {
    if (!listed?.admission) throw new AdmissionError('no-access', NO_ACCESS);
    return viaHost({workspace: listed, room, deps, onProgress});
  }
  if (secrets.invite) {
    // A link's `invite` names the invitation its host keeps, sealed under a
    // key in the link; the page opens it before anything reads it. An
    // invitation's own text is its own secret.
    const opened = await openedAt({site: origin, secret: secrets.invite, deps});
    return viaInvitation({origin: opened.host ?? origin, invitation: opened.invitation, room, secrets, deps, onProgress});
  }
  if (secrets.ticket) {
    if (!tickets) throw new AdmissionError('unsupported', 'This page cannot redeem guest tickets');
    onProgress({phase: 'joining'});
    return ready(await deps.redeemTicket({origin, room, ticket: secrets.ticket, tickets}));
  }
  if (secrets.pair) {
    if (!deps.pairDevice) throw new AdmissionError('unsupported', 'This page cannot pair a device');
    const pairing = parsePairSecret(secrets.pair);
    if (!pairing) throw new AdmissionError('malformed', 'This pairing link is damaged. Make a new one on your other device.');
    onProgress({phase: 'pairing'});
    return ready(await deps.pairDevice({origin, room, ...pairing, workspace: listed}));
  }
  /** Steps 4 to 6: the host's door, else the root this page holds. A
   * workspace of the host's other than its door's that this browser entered
   * before (the person's own, say) is reconnected to as the person, and the
   * door is the way in only when that does not get them in. */
  const viaDoor = () => viaHost({workspace: listed, room, deps, onProgress});
  const withoutAccount = async busy => {
    const enteredHere = Boolean(deps.entered?.({origin, room}));
    if (listed?.admission && !enteredHere) {
      if (room) console.warn('admission: this browser did not enter this workspace; asking at the host\'s door');
      return viaDoor();
    }
    let peer = null;
    try {
      peer = await deps.reconnect?.({origin, room, onProgress});
    } catch (error) {
      // Other tabs hold every device key this browser can get in with: not
      // the host's answer.
      if (error?.name === 'CustodyError' && error.code === 'custody-busy') throw error;
      // As below: the host's reason could tell a missing workspace from a private one.
      console.warn('admission: reconnecting failed', error?.code ?? error?.name ?? 'error');
    }
    if (!peer && listed?.admission) {
      if (enteredHere) console.warn('admission: a workspace entered here was not reconnected to; asking at the host\'s door');
      return viaDoor();
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
    // The host's reason could tell "no such workspace" from "not a member".
    console.warn('admission: connecting failed', error?.code ?? error?.name ?? 'error');
    throw new AdmissionError('no-access', NO_ACCESS);
  }
}
