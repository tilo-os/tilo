/**
 * Keeping the person this browser is, as the desktop's own actions.
 *
 * The first visit already made a real identity (`browserIdentity`); these
 * actions are the person's name, picture and accounts, the devices they pair,
 * and the agent they set up. They live in the desktop like its other
 * commands: desktop search lists them.
 *
 * - `set-name`: the name others see, in history and presence. Asked in a
 *   form, or taken as typed where the desktop already asked (the profile
 *   card's field).
 * - `set-photo` / `reset-photo`: the picture others see beside the name,
 *   chosen from an image file (`profile-photos.mjs`), or back to initials.
 *   Kept in the profile with the name, so it follows the person too.
 * - `set-linked`: the accounts elsewhere a workspace linked to the person, as
 *   its server answered (`{accounts, keep: true}`), or as the kept profile
 *   says (`{photoFrom, linked, keep: false}`). The profile names them, and
 *   wears an account's picture (Discord's avatar) until the person uploads
 *   their own; unlinking takes the account's picture away again.
 * - `pair-device`: let another device of the person in. Shows a sheet
 *   with a link, and its QR code, that opens this workspace there as a new
 *   device of this person. Each link names a fresh invite (`pairing.mjs`):
 *   it works once, for ten minutes, and only this page asks about a device
 *   that answers it, with what the server saw of that device and the code it
 *   shows. Approving signs its certificate with the root held here; the
 *   server the two share only relays the request and the certificate.
 *
 * The name the person chooses (their profile, signed by the root) goes to
 * every server they joined through: each of their devices, paired or signed
 * in, comes up with it (`syncProfile`).
 */
import {forInvites, openInvites} from './pairing.mjs';
import {fetchProfile, fitDisplayName, IdentityError, publishProfile} from './identity.mjs';

/** Not offered: the desktop's own card asks it, to keep the person's status. */
export const STATUS_ID = 'set-status';
export const LINKED_ID = 'set-linked';
export const NAME = Object.freeze({id: 'set-name', title: 'Set your name',
  subtitle: 'How others see you in this workspace', source: 'identity'});
export const PHOTO = Object.freeze({id: 'set-photo', title: 'Change your picture',
  subtitle: 'The picture people see beside your name', source: 'identity'});
export const RESET_PHOTO = Object.freeze({id: 'reset-photo', title: 'Use your initials again',
  subtitle: 'Take your picture away', source: 'identity'});
export const PAIR = Object.freeze({id: 'pair-device', title: 'Pair a new device',
  subtitle: 'A code or link that opens this workspace there as you', source: 'devices'});
/** Opening the agent setup again, in the person's card: the first screen's
 * own flow, with a fresh code (`showAgentSetup`). */
/**
 * The server an agent's setup code comes from, as the address a page asks it
 * at over HTTP: the workspace in front (or, on the person's own desktop,
 * which has no workspace peer, the site's home server).
 *
 * A workspace this page reached only over iroh — an invitation's own word for
 * a server with no web address, named `<endpoint>.iroh.invalid` — is named by
 * an address no HTTP request reaches, so the code comes from the site's home
 * server instead, as a page in a hosted site's workspace asks the server it
 * is in. A server of one's own that only an invitation reached has none to
 * ask, and offers no setup.
 */
export function setupOrigin({peer, home}) {
  const named = home?.host?.origin ?? null;
  const origin = peer?.origin ?? named;
  if (!origin || !namedByInvitation(origin)) return origin;
  // The same server, named twice: the site's home and the one the invitation
  // reached, by the endpoint each names.
  const bySite = JSON.parse(home?.host?.info ?? 'null')?.address?.endpointAddr?.id ?? null;
  const reached = peer?.host?.address?.endpointAddr?.id ?? null;
  return bySite !== null && bySite === reached ? named : null;
}

/** Whether `origin` is the name an invitation gives a server it reached only
 * over iroh (`<endpoint>.iroh.invalid`, `site-host.mjs`'s `invitedOrigin`): a
 * name that never resolves, so no HTTP request reaches it. */
function namedByInvitation(origin) {
  try {
    return /\.iroh\.invalid$/.test(new URL(origin).origin);
  } catch {
    return false;
  }
}

export const CONNECT_AGENT = Object.freeze({id: 'connect-agent', title: 'Connect an agent or computer',
  subtitle: 'A prompt it runs, or Tilo’s MCP address for an MCP client', source: 'devices'});
/**
 * How many actions make an identity worth keeping. One post or one moved
 * window is a visitor trying things; five separate ones (writing, sending,
 * opening and arranging windows) is someone at work, whose work and name
 * others now see as theirs.
 */
export const ASK_AFTER_ACTIONS = 5;
/** Changes closer together than this are one action: typing a message, dragging a window. */
export const ACTION_GAP_MS = 2000;
/** How often to look again for a pause once the ask is due. */
const PAUSE_POLL_MS = 1000;
/** Where this browser session remembers that it asked. */
const ASKED_KEY = 'tilo.identity.asked';
/** The desktop's ask to revoke one of the person's devices, by key (hex). */
export const REVOKE_DEVICE = 'revoke-device';
/** How often, while a pairing link is open, to ask for devices waiting. */
const PAIR_POLL_MS = 3000;

/**
 * The identity actions of `identity` (a `browserIdentity`). `hosts()` lists
 * the origins of the hosts this person joined through (they keep the
 * profile). Attach the desktop with `attach(mount)`; route its
 * `onHostAction` to `perform`.
 * `photos` is the person's picture: `current()` (its `data:` URL, or
 * null), `choose()` (asks for one, resolving to its URL or null) and
 * `show(url)` (wears it here at once).
 * Tell it what the person does with `authored(count)` (their authored
 * changes so far) and `named(name)`; `pause()` says whether they are
 * between things (no input for a moment, nothing held down), so the ask
 * never lands mid-sentence or mid-drag. `searchChord` is how this page
 * opens search, said in the ask so the person can find it later.
 * `pairing` is the server side of pairing: `link(invite)`, `pending()` and
 * `answer(digest, approved)`. `showPairLink({link, expiresAt, copy, cancel})` shows
 * a new pairing link (`copy()` puts it on the clipboard, resolving whether
 * it could, and `cancel()` withdraws it); `askPairing(request, decide)` asks the person about a device
 * that answered it, and `decide(approved)` answers, resolving to
 * `{approved, label}`. Both return `{close()}`, called once the link is
 * spent or expired.
 */
export function createIdentityActions({
  identity, hosts = () => [], name = () => undefined,
  profile = publishProfile,
  onError = error => console.warn('identity:', error),
  naming = null, photos = null, pairing = null, copy = text => globalThis.navigator?.clipboard?.writeText(text), now = () => Date.now(),
  every = (run, ms) => { const timer = setInterval(run, ms); return () => clearInterval(timer); },
  pause = () => true, later = (run, ms) => setTimeout(run, ms), session = globalThis.sessionStorage,
  searchChord = 'Ctrl+K', showPairLink = () => ({close() {}}), askPairing = () => ({close() {}}),
  invites = openInvites({now}),
  profiled = () => {}, showAgentSetup = null, setupDue = () => false,
}) {
  let mount;
  const offered = new Set();
  const actions = () => [...(naming ? [NAME] : []),
    ...(photos ? [PHOTO, ...(photos.current() ? [RESET_PHOTO] : [])] : []),
    ...(pairing ? [PAIR] : []), ...(showAgentSetup ? [CONNECT_AGENT] : [])];
  // Keep the profile (name and picture) on every server; a refusal throws,
  // a server not reached is only reported.
  // What the person says they are doing and how they share it, as their
  // profile keeps it beside their name and picture.
  let said = {status: '', sharing: ''};
  // Their linked accounts as the profile names them, and the service whose
  // account their picture is (`photoFrom`; null for one they uploaded).
  // `known` once the kept profile said so: before, an uploaded picture
  // could be taken for none.
  let links = {photoFrom: null, linked: [], known: false};
  let waitingAccounts = null;
  const keepProfile = async (displayName, photo, what) => {
    const at = now();
    const results = await Promise.allSettled(hosts().map(origin => profile({origin, identity, displayName, photo, ...said,
      photoFrom: photo ? links.photoFrom : null, linked: links.linked, now: at})));
    const refused = results.find(result => result.status === 'rejected' && result.reason?.code === 'publish-refused');
    for (const result of results) if (result.status === 'rejected' && result !== refused) onError(result.reason);
    if (refused) throw new IdentityError(`${what}-refused`, `This ${what} was not kept: ${refused.reason.message}`);
    // Kept: everyone here fetches it again when this device says it changed.
    if (results.some(result => result.status === 'fulfilled')) profiled(at);
  };
  // The workspace's host said which accounts are linked (`[{service, handle,
  // picture?}]`): the profile names them, and wears an account's picture
  // unless the person uploaded their own; an unlinked account's goes.
  const keepLinked = async accounts => {
    const linked = accounts.filter(a => typeof a?.service === 'string' && typeof a?.handle === 'string')
      .map(({service, handle}) => ({service, handle}));
    const current = photos?.current() ?? null;
    let photo = current;
    let photoFrom = links.photoFrom;
    if (!current || photoFrom) {
      const pictured = accounts.find(a => typeof a?.picture === 'string' && a.picture.startsWith('data:image/'));
      photo = pictured?.picture ?? null;
      photoFrom = pictured ? pictured.service : null;
    }
    const same = photo === current && photoFrom === links.photoFrom
      && JSON.stringify(linked) === JSON.stringify(links.linked);
    links = {photoFrom, linked, known: true};
    const named = fitDisplayName(name() ?? '');
    if (same || !named) return;
    await keepProfile(named, photo, 'picture');
    if (photo !== current) photos?.show(photo);
    refresh();
  };
  const refresh = () => mount?.setHostActions(actions());
  // Each moment offers once per page: `moment` defaults to the action.
  const offer = (action, title, body, moment = action.id) => {
    if (!mount || offered.has(moment)) return;
    offered.add(moment);
    mount.offerHostAction({action, title, body});
  };
  const tryCopy = link => Promise.resolve().then(() => copy(link)).then(() => true, () => false);
  // What shows each open invite: its sheet, then the ask about the device
  // that answered it. Either closes once the invite is spent or expired.
  const shown = new Map();
  const asked = new Set();
  const closeInvite = id => {
    shown.get(id)?.close();
    shown.delete(id);
    asked.delete(id);
  };
  let stopPairing = null;
  // An answer spends the invite, approved or not: the host takes no second
  // request naming it.
  const decide = request => async approved => {
    if (!invites.has(request.invite)) throw new IdentityError('expired', 'This pairing link has expired.');
    await pairing.answer(request.digest, approved);
    invites.spend(request.invite);
    shown.delete(request.invite);
    asked.delete(request.invite);
    return {approved, label: request.label};
  };
  // Devices waiting to be paired, asked for while one of this page's
  // invites is open; only one that answers such an invite is asked about.
  const askPending = async () => {
    for (const id of invites.prune()) closeInvite(id);
    if (!invites.size) { stopPairing?.(); stopPairing = null; void pairing.close?.(); return; }
    let pending;
    try { pending = await pairing.pending(); } catch (error) { onError(error); return; }
    for (const request of forInvites(pending, invites)) {
      if (asked.has(request.invite)) continue;
      asked.add(request.invite);
      shown.get(request.invite)?.close();
      shown.set(request.invite, askPairing(request, decide(request)));
    }
  };
  // What the person did: separate actions.
  let done = 0;
  let lastCount = 0;
  let lastChange = -Infinity;
  let awaitingPause = false;
  const askedThisSession = () => { try { return session?.getItem(ASKED_KEY) === '1'; } catch { return false; } };
  const ask = () => {
    if (!mount || askedThisSession() || done < ASK_AFTER_ACTIONS) return;
    if (!pause()) {
      if (!awaitingPause) {
        awaitingPause = true;
        later(() => { awaitingPause = false; ask(); }, PAUSE_POLL_MS);
      }
      return;
    }
    try { session?.setItem(ASKED_KEY, '1'); } catch { /* asked once per page then */ }
    // Someone who arrived by an invitation and never set up Tilo of their
    // own is asked that first, now that they used the workspace: the setup
    // gives them their own workspace with their agent in it, and keeps them.
    // It is about them, not about what they are doing, so it is asked from
    // their avatar (`source: 'identity'`), never as a banner over the note or
    // message they are in the middle of.
    if (showAgentSetup && setupDue()) {
      offer({...CONNECT_AGENT, source: 'identity', title: 'Set up with your agent'}, 'Want a workspace of your own?',
        `Paste one line into Claude Code, Codex or any agent. It installs Tilo and signs in as you, with your agent in a workspace of your own. Any time later: ${searchChord}, “${CONNECT_AGENT.title}”.`,
        'setup');
    }
  };
  const controller = {
    actions,
    /** A workspace session ended: withdraw this page's invitations and their unanswered asks. */
    withdrawInvites() {
      for (const id of shown.keys()) { invites.spend(id); closeInvite(id); }
      stopPairing?.();
      stopPairing = null;
      void pairing?.close?.();
    },
    /** Show the actions in `mount`'s desktop. */
    async attach(target) {
      mount = target;
      refresh();
      ask();
    },
    /** The person has authored `count` changes so far. Changes in quick
     * succession are one action. */
    authored(count) {
      if (!(count > lastCount)) return;
      lastCount = count;
      if (now() - lastChange >= ACTION_GAP_MS) done += 1;
      lastChange = now();
      ask();
    },
    /** The person's picture changed elsewhere: offer what fits it now. */
    pictured() { refresh(); },
    /** The person chose their name: who they are matters to them now. */
    named() {
      done = Math.max(done, ASK_AFTER_ACTIONS);
      ask();
    },
    /** Run the host action `id`, with what the person already typed for it
     * (`value`, the name from the profile card); resolves when done (errors
     * go to `onError`). */
    async perform(id, value) {
      try {
        if (id === NAME.id && naming) {
          // The form stays open until the name is kept: a host's refusal
          // is said there. A name typed in the desktop is kept as typed; a
          // refusal reaches `onError`.
          const keep = async typed => {
            const fitted = fitDisplayName(typed);
            if (!fitted) throw new IdentityError('malformed', 'Choose a name');
            await keepProfile(fitted, photos?.current() ?? null, 'name');
            return fitted;
          };
          const chosen = typeof value === 'string' ? await keep(value) : await naming.ask(name(), keep);
          if (chosen) {
            const set = naming.set(chosen);
            controller.named(typeof set === 'string' && set ? set : chosen);
          }
        } else if ((id === PHOTO.id || id === RESET_PHOTO.id) && photos) {
          const url = id === PHOTO.id ? await photos.choose() : null;
          if (id === PHOTO.id && !url) return;
          const named = fitDisplayName(name() ?? '');
          if (!named) throw new IdentityError('malformed', 'Choose a name first');
          // A picture they chose is theirs: no linked account's replaces it.
          links = {...links, photoFrom: null};
          await keepProfile(named, url, 'picture');
          photos.show(url);
          refresh();
        } else if (id === LINKED_ID) {
          if (!value?.keep) {
            // What their kept profile says, from any of their devices.
            links = {photoFrom: typeof value?.photoFrom === 'string' ? value.photoFrom : null,
              linked: Array.isArray(value?.linked) ? value.linked : [], known: true};
            if (!waitingAccounts) return;
          }
          const accounts = value?.keep ? value.accounts : waitingAccounts;
          if (!links.known) { waitingAccounts = accounts; return; }
          waitingAccounts = null;
          await keepLinked(Array.isArray(accounts) ? accounts : []);
        } else if (id === STATUS_ID) {
          // From the desktop: what they said (kept now, for their other
          // devices), or what their profile already holds (`keep: false`).
          const next = {status: String(value?.status ?? ''), sharing: String(value?.sharing ?? '')};
          if (next.status === said.status && next.sharing === said.sharing) return;
          said = next;
          const named = fitDisplayName(name() ?? '');
          if (value?.keep && named) await keepProfile(named, photos?.current() ?? null, 'status');
        } else if (id === PAIR.id && pairing) {
          const invite = invites.open();
          const link = pairing.link(invite.id);
          if (!link) {
            invites.spend(invite.id);
            throw new IdentityError('no-workspace', 'Join a workspace before pairing a device to it');
          }
          // The sheet's Copy is a click, a gesture the clipboard honours.
          shown.set(invite.id, showPairLink({link, expiresAt: invite.expiresAt, copy: () => tryCopy(link),
            cancel: () => { invites.spend(invite.id); closeInvite(invite.id); }}));
          stopPairing ??= every(() => { void askPending(); }, PAIR_POLL_MS);
          await askPending();
        } else if (id === REVOKE_DEVICE && typeof value?.key === 'string') {
          // A device of the person's, or an agent's grant, taken back: the
          // root revokes the key, for the reason the person chose.
          await identity.revokeDevice(value.key, value.reason);
        } else if (id === CONNECT_AGENT.id && showAgentSetup) {
          // The first screen again, with a fresh code: the person's agent runs
          // `tilo login`, or an MCP-only client is added with Tilo's address.
          showAgentSetup();
        } else {
          throw new IdentityError('unknown-action', `no identity action ${id}`);
        }
      } catch (error) {
        onError(error);
      }
    },
  };
  return controller;
}

/**
 * Bring this device's name for the person `userPublicKey` in line with the
 * profile their hosts keep (`origins`, asked in order). A kept profile wins:
 * it is the newest name any of the person's devices chose, so it is applied
 * here (`apply(name)`). Without one, a device holding the root (`identity`)
 * publishes the name it chose here (`chosen`), if any, to every origin.
 * `onKept(profile)` hears a kept profile, `onNone()` that the hosts answered
 * and keep none. Resolves to the name applied, or `null`.
 */
export async function syncProfile({
  userPublicKey, origins, identity = null, chosen = null, apply, find = fetchProfile, publish = publishProfile,
  onKept = () => {}, onNone = () => {}, onError = error => console.warn('identity:', error),
}) {
  if (!userPublicKey) return null;
  const unique = [...new Set(origins)];
  const kept = await find(userPublicKey, unique);
  if (kept) {
    // The rest of what is kept (their picture) is the caller's to wear.
    onKept(kept);
    if (kept.display_name !== chosen) apply(kept.display_name);
    return kept.display_name;
  }
  // Their hosts answered and keep none: nothing kept to keep apart from
  // what this device publishes (a linked account's picture, say).
  if (kept === null) onNone();
  if (chosen && identity?.userPublicKey === userPublicKey && identity.sign && !identity.locked) {
    const results = await Promise.allSettled(unique.map(origin => publish({origin, identity, displayName: chosen})));
    for (const result of results) if (result.status === 'rejected') onError(result.reason);
  }
  return null;
}
