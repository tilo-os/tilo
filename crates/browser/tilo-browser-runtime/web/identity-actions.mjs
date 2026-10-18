/**
 * Keeping the person this browser is, as the desktop's host actions.
 *
 * The first visit already made a real identity (`browserIdentity`); these
 * actions only add ways to open it elsewhere. They live in the desktop like
 * its other commands: desktop search lists them. Nothing is asked on
 * arrival: an identity that made nothing loses nothing if the browser
 * clears it. Once the person has done enough to be worth keeping
 * (`ASK_AFTER_ACTIONS` actions, or they chose their name), and at the
 * next pause in what they are doing, one banner asks whether to keep it,
 * once per browser session; "Not now" leaves it in the notification centre
 * and the action in search. Choosing one runs here, inside the person's
 * gesture, so the passkey prompt may follow.
 *
 * - `keep-identity`: one tap adds a passkey whose PRF output wraps the root,
 *   then offers a recovery key beside it.
 * - `save-recovery-key`: a new recovery key, handed to the password manager
 *   where the browser has one to offer (`PasswordCredential`), else saved as
 *   a file. Never shown on the page for longer than that.
 * - `sign-in-passkey`: be, in this browser, a person kept with a passkey
 *   elsewhere; its envelope comes from a host that keeps it, and the page
 *   reloads to join as them.
 * - `set-name`: the name others see, in history and presence; once set,
 *   a banner offers to keep the person it names. Asked in a form, or taken
 *   as typed where the desktop already asked (the profile card's field).
 * - `pair-device`: let another device of the person in. Shows a sheet
 *   with a link, and its QR code, that opens this workspace there as a new
 *   device of this person. Each link names a fresh invite (`pairing.mjs`):
 *   it works once, for ten minutes, and only this page asks about a device
 *   that answers it, with what the host saw of that device and the code it
 *   shows. Approving signs its certificate with the root held here; the
 *   host the two share only relays the request and the certificate.
 *
 * Each change to the envelope goes to every host this person joined through,
 * so a new browser finds it wherever it asks. So does the name the person
 * chooses (their profile, signed by the root): each of their devices, paired
 * or signed in, comes up with it (`syncProfile`).
 */
import {forInvites, openInvites} from './pairing.mjs';
import {
  fetchEnvelope, fetchProfile, fitDisplayName, IdentityError, publishEnvelope, publishProfile, signInWithPasskey,
} from './root-identity.mjs';

export const KEEP = Object.freeze({id: 'keep-identity', title: 'Keep your name and work',
  subtitle: 'Add a passkey, so you come back as yourself on any browser', source: 'identity'});
export const RECOVERY = Object.freeze({id: 'save-recovery-key', title: 'Save a recovery key',
  subtitle: 'To your password manager, or as a file', source: 'identity'});
export const SIGN_IN = Object.freeze({id: 'sign-in-passkey', title: 'Sign in with a passkey',
  subtitle: 'Be the person you kept on another browser', source: 'identity'});
export const NAME = Object.freeze({id: 'set-name', title: 'Set your name',
  subtitle: 'How others see you in this workspace', source: 'identity'});
export const PAIR = Object.freeze({id: 'pair-device', title: 'Pair a new device',
  subtitle: 'A code or link that opens this workspace there as you', source: 'devices'});
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
/** How often, while a pairing link is open, to ask for devices waiting. */
const PAIR_POLL_MS = 3000;

/** Hand `recoveryKey` to the browser's password manager, else save a file. */
export async function saveRecoveryKey(recoveryKey, {userPublicKey, name = 'Tilo', window = globalThis} = {}) {
  const PasswordCredential = window.PasswordCredential;
  if (PasswordCredential && window.navigator?.credentials?.store) {
    try {
      await window.navigator.credentials.store(new PasswordCredential({
        id: `tilo-recovery-${userPublicKey?.slice(0, 12) ?? 'key'}`, name: `${name} (Tilo recovery key)`, password: recoveryKey,
      }));
      return 'password-manager';
    } catch {
      // Declined or unavailable: the file still keeps it.
    }
  }
  const document = window.document;
  const text = `Tilo recovery key\n\n${recoveryKey}\n\nIt opens your Tilo identity on any browser. Keep it somewhere safe.\n`;
  const url = URL.createObjectURL(new Blob([text], {type: 'text/plain'}));
  try {
    const link = document.createElement('a');
    link.href = url;
    link.download = 'tilo-recovery-key.txt';
    document.body.append(link);
    link.click();
    link.remove();
  } finally {
    setTimeout(() => URL.revokeObjectURL(url), 1000);
  }
  return 'file';
}

/**
 * The identity actions of `identity` (a `browserIdentity`). `hosts()` lists
 * the origins of the hosts this person joined through (they keep the
 * envelope), `lookups()` every host worth asking on sign-in. Attach the
 * desktop with `attach(mount)`; route its `onHostAction` to `perform`.
 * Tell it what the person does with `authored(count)` (their authored
 * changes so far) and `named(name)`; `pause()` says whether they are
 * between things (no input for a moment, nothing held down), so the ask
 * never lands mid-sentence or mid-drag. `searchChord` is how this page
 * opens search, said in the ask so the person can find it later.
 * `pairing` is the host side of pairing: `link(invite)`, `pending()` and
 * `answer(digest, approved)`. `showPairLink({link, expiresAt, copy})` shows
 * a new pairing link (`copy()` puts it on the clipboard, resolving whether
 * it could); `askPairing(request, decide)` asks the person about a device
 * that answered it, and `decide(approved)` answers, resolving to
 * `{approved, label}`. Both return `{close()}`, called once the link is
 * spent or expired.
 */
export function createIdentityActions({
  identity, hosts = () => [], lookups = hosts, rpId = globalThis.location?.hostname, name = () => undefined,
  publish = publishEnvelope, find = fetchEnvelope, signIn = signInWithPasskey,
  profile = publishProfile,
  save = saveRecoveryKey, reload = () => globalThis.location?.reload(), onError = error => console.warn('identity:', error),
  naming = null, pairing = null, copy = text => globalThis.navigator?.clipboard?.writeText(text), now = () => Date.now(),
  every = (run, ms) => { const timer = setInterval(run, ms); return () => clearInterval(timer); },
  pause = () => true, later = (run, ms) => setTimeout(run, ms), session = globalThis.sessionStorage,
  searchChord = 'Ctrl+K', showPairLink = () => ({close() {}}), askPairing = () => ({close() {}}),
  invites = openInvites({now}),
}) {
  let mount;
  const offered = new Set();
  const actions = () => [...(naming ? [NAME] : []), ...(identity.kept ? [RECOVERY] : [KEEP, RECOVERY, SIGN_IN]),
    ...(pairing ? [PAIR] : [])];
  const refresh = () => mount?.setHostActions(actions());
  // Each moment offers once per page: `moment` defaults to the action.
  const offer = (action, title, body, moment = action.id) => {
    if (!mount || offered.has(moment)) return;
    offered.add(moment);
    mount.offerHostAction({action, title, body});
  };
  const publishEverywhere = async () => {
    const results = await Promise.allSettled(hosts().map(origin => publish({origin, identity})));
    for (const result of results) if (result.status === 'rejected') onError(result.reason);
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
    if (!invites.size) { stopPairing?.(); stopPairing = null; return; }
    let pending;
    try { pending = await pairing.pending(); } catch (error) { onError(error); return; }
    for (const request of forInvites(pending, invites)) {
      if (asked.has(request.invite)) continue;
      asked.add(request.invite);
      shown.get(request.invite)?.close();
      shown.set(request.invite, askPairing(request, decide(request)));
    }
  };
  // What the person did: separate actions, and the name they chose.
  let done = 0;
  let lastCount = 0;
  let lastChange = -Infinity;
  let chosenName = null;
  let awaitingPause = false;
  const askedThisSession = () => { try { return session?.getItem(ASKED_KEY) === '1'; } catch { return false; } };
  const ask = () => {
    if (!mount || identity.kept || askedThisSession() || done < ASK_AFTER_ACTIONS) return;
    if (!pause()) {
      if (!awaitingPause) {
        awaitingPause = true;
        later(() => { awaitingPause = false; ask(); }, PAUSE_POLL_MS);
      }
      return;
    }
    try { session?.setItem(ASKED_KEY, '1'); } catch { /* asked once per page then */ }
    offer({...KEEP, title: 'Keep with passkey'},
      chosenName ? `Come back as “${chosenName}” next time?` : 'Keep your name and work?',
      `A passkey on this device brings you back as yourself, on any browser. Any time later: ${searchChord}, “${KEEP.title}”.`);
  };
  const controller = {
    actions,
    /** Show the actions in `mount`'s desktop. */
    async attach(target) {
      mount = target;
      refresh();
      if (identity.kept) void publishEverywhere();
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
    /** The person chose their name: who they are matters to them now. */
    named(chosen) {
      chosenName = chosen;
      done = Math.max(done, ASK_AFTER_ACTIONS);
      ask();
    },
    /** Run the host action `id`, with what the person already typed for it
     * (`value`, the name from the profile card); resolves when done (errors
     * go to `onError`). */
    async perform(id, value) {
      try {
        if (id === KEEP.id) {
          await identity.keepWithPasskey({rpId, userName: name()});
          refresh();
          await publishEverywhere();
          // The next step, asked as the passkey sheet closes.
          offer({...RECOVERY, title: 'Save key'}, 'Also save a recovery key?',
            'In case this passkey is ever lost: to your password manager, or as a file.');
        } else if (id === RECOVERY.id) {
          const recoveryKey = await identity.createRecoveryKey();
          await save(recoveryKey, {userPublicKey: identity.userPublicKey, name: name()});
          refresh();
          await publishEverywhere();
        } else if (id === NAME.id && naming) {
          // The form stays open until the name is kept: a host's refusal
          // is said there (`ask` shows what `save` throws). A name typed
          // in the desktop is kept as typed; a refusal reaches `onError`.
          const keep = async typed => {
            const fitted = fitDisplayName(typed);
            if (!fitted) throw new IdentityError('malformed', 'Choose a name');
            const results = await Promise.allSettled(hosts().map(origin => profile({origin, identity, displayName: fitted})));
            const refused = results.find(result => result.status === 'rejected' && result.reason?.code === 'publish-refused');
            // An unreachable host is reported, and does not block the name.
            for (const result of results) if (result.status === 'rejected' && result !== refused) onError(result.reason);
            if (refused) throw new IdentityError('name-refused', `This name was not kept: ${refused.reason.message}`);
            return fitted;
          };
          const chosen = typeof value === 'string' ? await keep(value) : await naming.ask(name(), keep);
          if (chosen) {
            const set = naming.set(chosen);
            controller.named(typeof set === 'string' && set ? set : chosen);
          }
        } else if (id === PAIR.id && pairing) {
          const invite = invites.open();
          const link = pairing.link(invite.id);
          if (!link) {
            invites.spend(invite.id);
            throw new IdentityError('no-workspace', 'Join a workspace before pairing a device to it');
          }
          // The sheet's Copy is a click, a gesture the clipboard honours.
          shown.set(invite.id, showPairLink({link, expiresAt: invite.expiresAt, copy: () => tryCopy(link)}));
          stopPairing ??= every(() => { void askPending(); }, PAIR_POLL_MS);
          await askPending();
        } else if (id === SIGN_IN.id) {
          const origins = [...new Set(lookups())];
          await signIn({rpId, fetchEnvelope: root => find(root, origins)});
          reload();
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
 * Resolves to the name applied, or `null`.
 */
export async function syncProfile({
  userPublicKey, origins, identity = null, chosen = null, apply, find = fetchProfile, publish = publishProfile,
  onError = error => console.warn('identity:', error),
}) {
  if (!userPublicKey) return null;
  const unique = [...new Set(origins)];
  const kept = await find(userPublicKey, unique);
  if (kept) {
    if (kept.display_name !== chosen) apply(kept.display_name);
    return kept.display_name;
  }
  if (chosen && identity?.userPublicKey === userPublicKey && identity.sign && !identity.locked) {
    const results = await Promise.allSettled(unique.map(origin => publish({origin, identity, displayName: chosen})));
    for (const result of results) if (result.status === 'rejected') onError(result.reason);
  }
  return null;
}
