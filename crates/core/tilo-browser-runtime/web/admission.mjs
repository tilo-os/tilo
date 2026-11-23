/**
 * Getting into the workspace a link names, before anything about its target is
 * read. Which way, and the next when one fails, is the runtime's to say (the
 * rules every device takes, tilo-session-client's `way_in`); the page takes it
 * and says what happened. In the order the rules try them:
 * 1. the person's own desktop (`/me`) is this browser's, with no peer,
 *    once the home's entry let them in where it checks (`ownDesktop`);
 * 2. an invitation is inspected (what the host verified: workspace, role,
 *    inviter, how many people it has), confirmed by the person, then
 *    redeemed. An invitation is the approval of whoever made it: the host
 *    admits it at once, and nobody in the workspace answers for it. One sent
 *    off the workspace's wait list opens the person's own workspace instead
 *    (`deps.ownWorkspace`), unless the link names a place inside it. A
 *    link's `?invite=` after a workspace's route is the invitation's own
 *    text, or the secret of the one that workspace keeps, which its host
 *    answers with (a short link, `#/@<host>/w/<workspace>?invite=<secret>`).
 *    A pre-admission `token` admits at once too; a workspace service guest
 *    `ticket` is spent on this page's pairing code at the redemption entry
 *    the web host names (`entry.json` `tickets`); with `pair`, this browser
 *    becomes a new device of that person once one of their devices approves
 *    its pairing code (the host relays): the device that made the link,
 *    when it names an invite (`pairing.mjs`);
 * 3. a member connects with this browser's existing account for that host
 *    (where other tabs hold every device key it has there, this tab goes on
 *    to 4 and 5 as a new device of the person);
 * 4. a host whose listed workspace has an admission entry (`workspaces.mjs`)
 *    admits this browser's person at once; a link that names a workspace there
 *    is followed only if that is the workspace the person was admitted to;
 * 5. a host this browser set up for its person, or joined as them through
 *    an invitation, is reconnected to with the root the page holds
 *    (`reconnect`); the host decides whether that person is in the workspace;
 * 6. a browser where nobody is signed in, at any other workspace, is asked
 *    to sign in first (`sign-in`): nothing joins as a stranger no entry
 *    lets in, and the page comes back to the link as the person;
 * 7. anyone else is told they have no access and who can invite them.
 * A scope without a workspace (the web host's home workspace, whose workspace the
 * page learns on admission) can only be entered through its host's entry.
 * A link never grants anything: each step is the workspace's own entry. A failure
 * to connect reads the same whether the workspace is missing or private, so a
 * link never tells a stranger which workspaces exist. Only after admission does
 * the page read the workspace's catalogue to resolve the target.
 */
import {parsePairSecret} from './pairing.mjs';
import {deviceRemoved} from './session-refusal.mjs';

export const NO_ACCESS = 'You don’t have access to this workspace. Ask the person who shared the link to invite you.';
/** A link to a workspace no entry lets a stranger into, in a browser where
 * nobody is signed in (`sign-in`). */
export const SIGN_IN = 'Sign in to open this workspace.';
/** An invitation its workspace no longer keeps, or one its host refuses: the
 * same words for both, so a link tells nothing about invitations it does not
 * name. */
const UNUSABLE_INVITATION = 'This link’s invitation is no longer there. Ask the person who invited you for a new one.';

export class AdmissionError extends Error {
  constructor(code, message) {
    super(message);
    this.name = 'AdmissionError';
    this.code = code;
  }
}

/** The invitation `secret` names: its own text, or the invitation
 * `workspace` keeps at its host `origin`, which answers with it. */
async function openedAt({origin, workspace, secret, deps}) {
  try {
    return await deps.openInvitation(origin, workspace, secret);
  } catch (error) {
    console.warn('admission: the invitation did not open', error?.message ?? error);
    throw new AdmissionError('invitation-unusable', UNUSABLE_INVITATION);
  }
}

/** What `invitation` offers at `origin`, shown to the person, then joined
 * with: the invitation is the approval of whoever made it, so the host
 * admits it at once and nobody in the workspace answers for it. */
async function viaInvitation({origin, invitation, offered = null, id = null, deep = false, secrets, runtime, deps, onProgress}) {
  // Reading the invitation asks the workspace's server: the page is connecting to it now, and
  // says so rather than that it is still loading.
  if (!offered) onProgress({phase: 'connecting'});
  const offer = offered ?? await deps.inspectInvitation(origin, invitation);
  if (!runtime.admittedAsAsked(id ?? undefined, offer.workspace ?? undefined)) {
    throw new AdmissionError('mismatch', 'This invitation is for another workspace than the link names');
  }
  // Whether joining opens the person's own workspace in this one's place
  // (below). They asked to get in from the wait list, and this is the answer:
  // nothing is left to agree to, so they join now. A card to press first would
  // leave someone who closes the tab on it not joined, sent back to the wait
  // list when they return.
  const ownFirst = !deep && offer.waitList === true && Boolean(deps.ownWorkspace);
  if (!ownFirst && !await deps.confirmInvitation(offer, origin)) throw new AdmissionError('declined', 'You did not join');
  const joined = await deps.joinWorkspace({
    origin, invitation, admissionToken: secrets.token,
    onProgress: event => onProgress(event),
  });
  // An invitation sent off the wait list lets in someone new to the server:
  // it opens their own workspace, made now with this device in it, rather
  // than the one it let them into, which stays one switch away. Decided here,
  // before anything mounts, so the page opens one workspace and loads once.
  // A link that names a place inside the workspace (`deep`: an app, or a
  // resource of one) opens that place.
  if (ownFirst) {
    const own = await deps.ownWorkspace(joined);
    if (own?.workspace && own.workspace !== joined.workspace) {
      // One device key per server for this tab: the workspace the invitation
      // let them into lets it go before their own takes it.
      await joined.dispose?.();
      let entered = null;
      try {
        entered = await deps.reconnect?.({origin, workspace: own.workspace, onProgress});
      } catch (error) {
        if (deviceRemoved(error)) throw error;
        console.warn('admission: your own workspace did not let this device in', error?.code ?? error?.name ?? 'error');
      }
      if (entered) return {...await ready(entered), own: true};
      const back = await deps.reconnect?.({origin, workspace: joined.workspace, onProgress});
      if (!back) throw new AdmissionError('no-access', NO_ACCESS);
      return ready(back);
    }
  }
  return ready(joined);
}

/** `{peer}` once the host admitted `peer` to its workspace. */
async function ready(peer) {
  await peer?.whenReady?.();
  return {peer};
}

/** The listing `listed` with its entry naming `workspace`: a host serves every
 * one of its workspaces' entries under its directory (`…/directory/<workspace>/`),
 * and the entry the site lists is its own workspace's. */
function entryUrl(listed, workspace) {
  if (!listed?.admission || !workspace || listedWorkspace(listed) === workspace) return listed;
  const entry = new URL(listed.admission);
  const segments = entry.pathname.split('/');
  const at = segments.lastIndexOf('directory');
  if (at >= 0 && segments[at + 1]) segments[at + 1] = encodeURIComponent(workspace);
  else segments.splice(segments.length - 1, 0, 'directory', encodeURIComponent(workspace));
  entry.pathname = segments.join('/');
  // Another workspace: the listed one's name is not its name.
  return {...listed, workspace, admission: entry.href, name: null};
}

/** The listing the server at `origin` keeps of `workspace` in its public
 * directory (`deps.listings`), or `null`: not listed, or the directory not
 * read, which leaves the workspace closed to a stranger, as one not listed. */
async function publicListing({origin, workspace, deps}) {
  try {
    return (await deps.listings?.(origin))?.find(listing => listing.workspace === workspace) ?? null;
  } catch (error) {
    console.warn('admission: the directory at', origin, 'was not read:', error?.message ?? error);
    return null;
  }
}

/** The workspace `listed`'s entry admits to: the one it names, else the one
 * its admission URL names (`…/directory/<workspace>/`). */
function listedWorkspace(listed) {
  if (listed?.workspace) return listed.workspace;
  try {
    const segments = new URL(listed?.admission).pathname.split('/').filter(Boolean);
    const at = segments.lastIndexOf('directory');
    return at >= 0 ? decodeURIComponent(segments[at + 1] ?? '') || null : null;
  } catch { return null; }
}

/** Admission through `workspace`'s host entry, held to the workspace the
 * link names (`admittedAsAsked` in `runtime`). */
async function viaHost({workspace, id = null, runtime, deps, onProgress}) {
  onProgress({phase: 'joining'});
  // The workspace a link names, if any: what this browser kept for a host set up
  // again is offered to be joined again only from the bare entry, where the
  // entry's workspace is the one the person asked for.
  const peer = await deps.enterViaHost({workspace});
  // The entry has nothing to admit yet: an embed shows its embedder's fallback
  // until the visitor's first gesture (`entry-page.mjs`), and nothing mounts
  // before it.
  if (peer?.idle) return {peer: null, idle: true};
  if (!runtime.admittedAsAsked(id ?? undefined, peer?.workspace ?? undefined)) {
    await peer?.dispose?.();
    throw new AdmissionError('no-access', NO_ACCESS);
  }
  return ready(peer);
}

/** The person's own desktop: this browser's, with no peer. What syncs
 * across their devices lives in workspaces, as everything shared does.
 * Where the home lets only some in (`deps.ownDesktopOpen`), a browser no
 * host let in is sent to the home's entry instead, which says how to get
 * in; an admission to the workspace is not a desktop of their own. */
async function ownDesktop({home, runtime, deps, onProgress}) {
  const guarded = Boolean(home?.admission && deps.ownDesktopOpen) && !await deps.ownDesktopOpen(home);
  if (guarded) {
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
  'sign-in': ['sign-in', SIGN_IN],
};

/**
 * Get into the workspace `scope` names, the way `runtime` (`wayIn`, the rules
 * every device takes) says from what the link carries and what this page
 * knows of the host; when a way fails, the next is the runtime's too
 * (`wayInAfter`). Taking each way, and what the person is shown, are the page's.
 * `deep` says the link names a place inside the workspace (an app, a resource).
 */
export async function admit({scope, deep = false, secrets = {}, tickets, home = null, runtime, deps, onProgress = () => {}}) {
  const origin = scope.kind === 'workspace' ? scope.host : null;
  const id = scope.kind === 'workspace' ? scope.workspace : null;
  let listed = origin ? deps.workspaceAt?.(origin) ?? null : null;
  const pairing = secrets.pair ? parsePairSecret(secrets.pair) : null;
  // The link names the listed workspace, whose entry lets anyone in.
  const siteListed = Boolean(listed?.admission) && (!id || listedWorkspace(listed) === id);
  // Someone is signed in here: the page comes back as them where no entry lets a stranger in.
  const person = Boolean(await deps.holdsPerson?.({origin}));
  const entered = Boolean(origin && id && deps.entered?.({origin, workspace: id}));
  // Nobody here, at a workspace the site does not list: its own server's
  // directory says whether its entry lets anyone in, before anything joins.
  // Read only where that decides the way.
  const own = id && origin && !siteListed && !person && !entered && !secrets.invite && !secrets.ticket && !secrets.pair
    ? await publicListing({origin, workspace: id, deps}) : null;
  const open = own && !own.invitationOnly
    ? {...(entryUrl(listed, id) ?? {host: {origin}}), workspace: id, admission: own.entry, name: own.name} : null;
  // That entry is the one the link's workspace is entered through.
  if (open) listed = open;
  const here = {
    entry: Boolean(listed?.admission),
    listed: siteListed || Boolean(open),
    person,
    entered,
    tickets: Boolean(tickets),
    pairing: Boolean(deps.pairDevice),
  };
  // A link that names a workspace at a host this page lists nothing at has no
  // entry to ask: it can only come back as the person this device holds, and a
  // device that holds none is told it has no access. Said once, with what the
  // page knew, so that refusal names what it was missing rather than reading
  // like a workspace that turned the person away.
  if (id && origin && !here.entry && !open) {
    console.warn('admission: no entry to ask at', origin, JSON.stringify({listed: Boolean(listed), entered: here.entered}));
  }
  const link = JSON.stringify({
    scope: scope.kind === 'workspace' ? {kind: 'workspace', workspace: id ?? undefined} : {kind: scope.kind},
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
      case 'invitation': {
        // A link's `invite` names the invitation its workspace keeps at the
        // link's host, which the page asks for before anything reads it. An
        // invitation's own text is its own secret.
        const invitation = await openedAt({origin, workspace: id, secret: secrets.invite, deps});
        // An invitation names its server in itself: one reached only over iroh is dialled as the
        // invitation says, and what it offers is read from it.
        const invited = await deps.invitedServer?.(invitation, origin);
        return viaInvitation({
          origin: invited?.origin ?? origin, invitation, offered: invited?.offer ?? null,
          id, deep, secrets, runtime, deps, onProgress,
        });
      }
      case 'ticket':
        onProgress({phase: 'joining'});
        return ready(await deps.redeemTicket({origin, workspace: id, ticket: secrets.ticket, tickets}));
      case 'pair':
        onProgress({phase: 'pairing'});
        return ready(await deps.pairDevice({origin, workspace: id, ...pairing, listed}));
      case 'entry':
        // An entry admits into its own workspace. A link naming another
        // workspace of that host asks at that workspace's entry: the listed
        // one would let this browser into the listed workspace, and holding
        // it to the one the link names would then read as no access to it.
        return viaHost({workspace: entryUrl(listed, id), id, runtime, deps, onProgress});
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
          return await ready(await deps.connectMember({origin, workspace: id, account}));
        } catch (error) {
          // This browser was removed from its person's devices. Its server said so at the
          // handshake, before any workspace was named, so it tells nothing of this one:
          // the person reads that, and no other way in is tried.
          if (deviceRemoved(error)) throw error;
          // Other tabs of this browser hold every device key the account has here:
          // not the host's answer, and not no access.
          const held = error?.name === 'DeviceKeyError' && error.code === 'device-key-busy';
          // The host's reason could tell "no such workspace" from "not a member".
          if (!held) console.warn('admission: connecting failed', error?.code ?? error?.name ?? 'error');
          busy = held ? error : busy;
          // A tab is a device of its own: with every key the account has here
          // held, it gets in as a new device of the person, as a browser
          // without the account would.
          way = held ? runtime.wayIn(link, JSON.stringify({...here, accounts: 0}))
            : runtime.wayInAfter('member', JSON.stringify({...here, accounts: accounts.length}));
          continue;
        }
      }
      case 'reconnect': {
        let peer = null;
        let held = false;
        // Reaching the host now: a host that does not answer is waited on as
        // the connection, not as this device starting Tilo.
        onProgress({phase: 'connecting'});
        try {
          peer = await deps.reconnect?.({origin, workspace: id, onProgress});
        } catch (error) {
          // Removed from its person's devices: said, as above, and nothing else is tried.
          if (deviceRemoved(error)) throw error;
          // Other tabs hold every device key this browser can get in with: not
          // the host's answer.
          held = error?.name === 'DeviceKeyError' && error.code === 'device-key-busy';
          busy = held ? error : busy;
          // As below: the host's reason could tell a missing workspace from a private one.
          if (!held) console.warn('admission: reconnecting failed', error?.code ?? error?.name ?? 'error');
        }
        if (peer) return ready(peer);
        // Every key this browser could get in with is held by other tabs:
        // that is what to say, not the host's answer.
        if (held) throw busy;
        way = runtime.wayInAfter('reconnect', JSON.stringify({...here, accounts: accounts.length}));
        // Reached with an account only when its keys were all held elsewhere.
        if (way === 'no-access' && busy) throw busy;
        continue;
      }
      default: {
        const [code, message] = REFUSED[way] ?? REFUSED['no-access'];
        // Signing in pairs this device through the site's listed entry, as
        // any sign-in does; the link then opens as a device of the person.
        // The workspace's own listing, where its server lists it, names the
        // wait list someone with no invitation may ask on.
        if (way === 'sign-in') throw Object.assign(new AdmissionError(code, message), {entry: listed?.admission ?? null, listing: own});
        throw new AdmissionError(code, message);
      }
    }
  }
}
