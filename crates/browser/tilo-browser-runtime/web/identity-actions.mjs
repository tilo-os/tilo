/**
 * Keeping the person this browser is, as the desktop's host actions.
 *
 * The first visit already made a real identity (`browserIdentity`); these
 * actions only add ways to open it elsewhere. They live in the desktop like
 * its other commands: desktop search lists them, and at the moments that
 * matter (the person posted or created something, or the browser may clear
 * this site's storage) a banner offers to keep the identity, never blocking
 * anything. Choosing one runs here, inside the person's gesture, so the
 * passkey prompt may follow.
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
 *   a banner offers to keep the person it names.
 * - `pair-device`: let another device of the person in. Copies a link that
 *   opens this workspace there as a new device of this person; that device
 *   shows a pairing code, and a banner here offers to approve the device
 *   showing it. Approving signs its certificate with the root held here;
 *   the host the two share only relays the request and the certificate.
 *
 * Each change to the envelope goes to every host this person joined through,
 * so a new browser finds it wherever it asks. So does the name the person
 * chooses (their profile, signed by the root): each of their devices, paired
 * or signed in, comes up with it (`syncProfile`).
 */
import {
  fetchEnvelope, fetchProfile, fitDisplayName, IdentityError, publishEnvelope, publishProfile, signInWithPasskey, storagePersisted,
} from './root-identity.mjs';

export const KEEP = Object.freeze({id: 'keep-identity', title: 'Keep this identity',
  subtitle: 'Add a passkey, so you are yourself again on any browser', source: 'identity'});
export const RECOVERY = Object.freeze({id: 'save-recovery-key', title: 'Save a recovery key',
  subtitle: 'To your password manager, or as a file', source: 'identity'});
export const SIGN_IN = Object.freeze({id: 'sign-in-passkey', title: 'Sign in with a passkey',
  subtitle: 'Be the person you kept on another browser', source: 'identity'});
export const NAME = Object.freeze({id: 'set-name', title: 'Set your name',
  subtitle: 'How others see you in this workspace', source: 'identity'});
export const PAIR = Object.freeze({id: 'pair-device', title: 'Pair a new device',
  subtitle: 'Copy a link that opens this workspace there as you', source: 'devices'});
const APPROVE = 'approve-pairing:';
/** How often, and for how long after `pair-device`, to ask for devices waiting. */
const PAIR_POLL_MS = 3000;
const PAIR_WINDOW_MS = 10 * 60 * 1000;

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
 */
export function createIdentityActions({
  identity, hosts = () => [], lookups = hosts, rpId = globalThis.location?.hostname, name = () => undefined,
  publish = publishEnvelope, find = fetchEnvelope, signIn = signInWithPasskey, persisted = storagePersisted,
  profile = publishProfile,
  save = saveRecoveryKey, reload = () => globalThis.location?.reload(), onError = error => console.warn('identity:', error),
  naming = null, pairing = null, copy = text => globalThis.navigator?.clipboard?.writeText(text), now = () => Date.now(),
  every = (run, ms) => { const timer = setInterval(run, ms); return () => clearInterval(timer); },
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
  // Devices waiting to be paired, asked for while a pairing link is fresh.
  let pairUntil = 0;
  let stopPairing = null;
  const waiting = new Map();
  const askPending = async () => {
    if (now() > pairUntil) { stopPairing?.(); stopPairing = null; return; }
    let pending;
    try { pending = await pairing.pending(); } catch (error) { onError(error); return; }
    for (const device of pending) {
      waiting.set(device.digest, device);
      offer({id: `${APPROVE}${device.digest}`, title: `Approve ${device.pairingCode}`,
        subtitle: `Pair “${device.label}” as you`, source: 'devices'},
      `Pair “${device.label}”?`,
      `A device asks to be you. Approve only if it shows ${device.pairingCode}.`);
    }
  };
  const controller = {
    actions,
    /** Show the actions in `mount`'s desktop; offers to keep the identity
     * at once when the browser may clear this site's storage. */
    async attach(target) {
      mount = target;
      refresh();
      if (identity.kept) {
        void publishEverywhere();
      } else if (!await persisted()) {
        offer(KEEP, 'Keep this identity?', 'This browser may clear what this site stores. A passkey keeps you as yourself on any browser.');
      }
    },
    /** The person posted or created something: offer to keep who did it. */
    authored() {
      if (!identity.kept) offer(KEEP, 'Keep this identity?', 'Add a passkey, so you can come back as yourself on any browser.');
    },
    /** The person chose their name: offer to keep the person it names. */
    named(chosen) {
      if (!identity.kept) offer(KEEP, `Keep “${chosen}”?`, 'Add a passkey, so you are yourself again on any browser.', 'named');
    },
    /** Run the host action `id`; resolves when done (errors go to `onError`). */
    async perform(id) {
      try {
        if (id === KEEP.id) {
          await identity.keepWithPasskey({rpId, userName: name()});
          refresh();
          await publishEverywhere();
          offer(RECOVERY, 'Identity kept', 'Also save a recovery key, in case this passkey is ever lost.');
        } else if (id === RECOVERY.id) {
          const recoveryKey = await identity.createRecoveryKey();
          await save(recoveryKey, {userPublicKey: identity.userPublicKey, name: name()});
          refresh();
          await publishEverywhere();
        } else if (id === NAME.id && naming) {
          // The form stays open until the name is kept: a host's refusal
          // is said there (`ask` shows what `save` throws).
          const chosen = await naming.ask(name(), async typed => {
            const fitted = fitDisplayName(typed);
            if (!fitted) throw new IdentityError('malformed', 'Choose a name');
            const results = await Promise.allSettled(hosts().map(origin => profile({origin, identity, displayName: fitted})));
            const refused = results.find(result => result.status === 'rejected' && result.reason?.code === 'publish-refused');
            // An unreachable host is reported, and does not block the name.
            for (const result of results) if (result.status === 'rejected' && result !== refused) onError(result.reason);
            if (refused) throw new IdentityError('name-refused', `This name was not kept: ${refused.reason.message}`);
            return fitted;
          });
          if (chosen) {
            const set = naming.set(chosen);
            controller.named(typeof set === 'string' && set ? set : chosen);
          }
        } else if (id === PAIR.id && pairing) {
          const link = pairing.link();
          if (!link) throw new IdentityError('no-workspace', 'Join a workspace before pairing a device to it');
          await Promise.resolve(copy(link)).catch(() => {});
          // A new link shows again: its banner is the only place it is said.
          offered.delete(PAIR.id);
          offer(PAIR, 'Pairing link copied', `Open it on the new device: ${link}`);
          pairUntil = now() + PAIR_WINDOW_MS;
          stopPairing ??= every(() => { void askPending(); }, PAIR_POLL_MS);
          await askPending();
        } else if (id.startsWith(APPROVE) && pairing) {
          const digest = id.slice(APPROVE.length);
          const device = waiting.get(digest);
          await pairing.answer(digest, true);
          waiting.delete(digest);
          offered.delete(PAIR.id);
          offer(PAIR, 'Device paired', `“${device?.label ?? 'The device'}” is you now.`);
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
