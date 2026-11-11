/**
 * Getting into the workspace a link names, before anything about its target is
 * read. Which way, and the next when one fails, is the runtime's to say (the
 * rules every device takes, tilo-session-client's `way_in`); the page takes it
 * and says what happened. In the order the rules try them:
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
async function viaInvitation({origin, invitation, room = null, secrets, runtime, deps, onProgress}) {
  const offer = await deps.inspectInvitation(origin, invitation);
  if (!runtime.admittedAsAsked(room ?? undefined, offer.room ?? undefined)) {
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
async function viaSealedLink({scope, secrets, home, runtime, deps, onProgress}) {
  const opened = await openedAt({site: scope.site, secret: `${scope.handle}${secrets.invite}`, deps});
  const origin = opened.host ?? home?.host?.origin ?? scope.site;
  return viaInvitation({origin, invitation: opened.invitation, secrets, runtime, deps, onProgress});
}

/** `{peer}` once `peer` is ready to use. */
async function ready(peer) {
  // The desktop shows once the workspace's shared desktop record is here,
  // not after the whole workspace: the rest arrives while it is shown.
  await peer?.whenReady?.({shown: true});
  return {peer};
}

/** Admission through `workspace`'s host door, held to the workspace the
 * link names (`admittedAsAsked` in `runtime`). */
async function viaHost({workspace, room, runtime, deps, onProgress}) {
  onProgress({phase: 'joining'});
  // The workspace a link names, if any: what this browser kept for a host set up
  // again is offered to be joined again only from the bare entry, where the
  // door's workspace is the one the person asked for.
  const peer = await deps.enterViaHost(room ? {workspace, room} : {workspace});
  // The door has nothing to admit yet: an embed shows its embedder's fallback
  // until the visitor's first gesture (`entry-page.mjs`), and nothing mounts
  // before it.
  if (peer?.idle) return {peer: null, idle: true};
  if (!runtime.admittedAsAsked(room ?? undefined, peer?.workspace ?? undefined)) {
    await peer?.dispose?.();
    throw new AdmissionError('no-access', NO_ACCESS);
  }
  return ready(peer);
}

/** The person's own desktop: this browser's, with no peer. What syncs
 * across their devices lives in workspaces, as everything shared does.
 * Where the home lets only some in (`deps.ownDesktopOpen`), a browser no
 * host let in is sent to the home's door instead, which says how to get
 * in; a seat in the workspace is not a desktop of their own. */
async function ownDesktop({home, runtime, deps, onProgress}) {
  const gated = Boolean(home?.admission && deps.ownDesktopOpen) && !await deps.ownDesktopOpen(home);
  if (gated) {
    const {peer} = await viaHost({workspace: home, runtime, deps, onProgress});
    await peer?.dispose?.();
    if (!await deps.ownDesktopOpen(home)) throw new AdmissionError('no-access', NO_ACCESS);
  }
  return {peer: null};
}

/** What each way in that ends without one says, by its code. */
const REFUSED = {
  'malformed': ['malformed', 'This link’s invitation is damaged, so Tilo did not use it'],
  'malformed-pairing': ['malformed', 'This pairing link is damaged. Make a new one on your other device.'],
  'no-tickets': ['unsupported', 'This page cannot redeem guest tickets'],
  'no-pairing': ['unsupported', 'This page cannot pair a device'],
  'no-access': ['no-access', NO_ACCESS],
};

/**
 * Get into the workspace `scope` names, the way `runtime` (`wayIn`, the rules
 * every device takes) says from what the link carries and what this page
 * knows of the host; when a way fails, the next is the runtime's too
 * (`wayInAfter`). Taking each way, and what the person is shown, are the page's.
 */
export async function admit({scope, secrets = {}, tickets, home = null, runtime, deps, onProgress = () => {}}) {
  const origin = scope.kind === 'room' ? scope.host : null;
  const room = scope.kind === 'room' ? scope.room : null;
  const listed = origin ? deps.workspaceAt?.(origin) ?? null : null;
  const pairing = secrets.pair ? parsePairSecret(secrets.pair) : null;
  const here = {
    door: Boolean(listed?.admission),
    entered: Boolean(origin && room && deps.entered?.({origin, room})),
    tickets: Boolean(tickets),
    pairing: Boolean(deps.pairDevice),
  };
  const link = JSON.stringify({
    scope: scope.kind === 'room' ? {kind: 'room', room: room ?? undefined} : {kind: scope.kind},
    malformed: Boolean(secrets.malformed), invite: Boolean(secrets.invite), ticket: Boolean(secrets.ticket),
    pair: secrets.pair ? (pairing ? 'named' : 'damaged') : undefined,
  });
  let accounts = [];
  /** The error a way that got no one in left behind, said where nothing else is. */
  let busy = null;
  let way = runtime.wayIn(link, JSON.stringify(here));
  for (;;) {
    switch (way) {
      case 'own-desktop': return ownDesktop({home, runtime, deps, onProgress});
      case 'sealed-invitation': return viaSealedLink({scope, secrets, home, runtime, deps, onProgress});
      case 'invitation': {
        // A link's `invite` names the invitation its host keeps, sealed under a
        // key in the link; the page opens it before anything reads it. An
        // invitation's own text is its own secret.
        const opened = await openedAt({site: origin, secret: secrets.invite, deps});
        return viaInvitation({origin: opened.host ?? origin, invitation: opened.invitation, room, secrets, runtime, deps, onProgress});
      }
      case 'ticket':
        onProgress({phase: 'joining'});
        return ready(await deps.redeemTicket({origin, room, ticket: secrets.ticket, tickets}));
      case 'pair':
        onProgress({phase: 'pairing'});
        return ready(await deps.pairDevice({origin, room, ...pairing, workspace: listed}));
      case 'door':
        if (room) console.warn('admission: asking at the host\'s door');
        return viaHost({workspace: listed, room, runtime, deps, onProgress});
      case 'accounts':
        accounts = await deps.readAccounts(origin);
        way = runtime.wayIn(link, JSON.stringify({...here, accounts: accounts.length}));
        continue;
      case 'member': {
        const account = accounts.length === 1 ? accounts[0] : await deps.chooseAccount(accounts);
        onProgress({phase: 'connecting'});
        try {
          // A host that revoked this device answers the connection and refuses it
          // once the peer is certified: that, too, is no access.
          return await ready(await deps.connectMember({origin, room, account}));
        } catch (error) {
          // Other tabs of this browser hold every device key the account has here:
          // not the host's answer, and not no access.
          const held = error?.name === 'CustodyError' && error.code === 'custody-busy';
          // The host's reason could tell "no such workspace" from "not a member".
          if (!held) console.warn('admission: connecting failed', error?.code ?? error?.name ?? 'error');
          busy = held ? error : busy;
          way = runtime.wayInAfter(held ? 'member-busy' : 'member', JSON.stringify({...here, accounts: accounts.length}));
          continue;
        }
      }
      case 'reconnect': {
        let peer = null;
        let held = false;
        try {
          peer = await deps.reconnect?.({origin, room, onProgress});
        } catch (error) {
          // Other tabs hold every device key this browser can get in with: not
          // the host's answer.
          held = error?.name === 'CustodyError' && error.code === 'custody-busy';
          busy = held ? error : busy;
          // As below: the host's reason could tell a missing workspace from a private one.
          if (!held) console.warn('admission: reconnecting failed', error?.code ?? error?.name ?? 'error');
        }
        if (peer) return ready(peer);
        way = runtime.wayInAfter(held ? 'reconnect-busy' : 'reconnect', JSON.stringify({...here, accounts: accounts.length}));
        continue;
      }
      case 'busy':
        throw busy ?? new AdmissionError('no-access', NO_ACCESS);
      default: {
        const [code, message] = REFUSED[way] ?? REFUSED['no-access'];
        throw new AdmissionError(code, message);
      }
    }
  }
}
