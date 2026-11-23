/**
 * Pairing a person's new device, the parts both pages share.
 *
 * The device that has the person (holds their root) makes a pairing link:
 * the workspace link with `pair=<user public key><invite>`, 64 + 32 hex.
 * The invite is random, works once and for `INVITE_TTL_MS`; only the page
 * that made it offers to approve a request naming it, so the person is
 * always asked on the device that sent the link (`openInvites`,
 * `forInvites`). A 64-hex `pair` names no invite: any of the person's
 * devices may answer it.
 *
 * The new device reads the link (`parsePairSecret`) and, before it asks any
 * holder to let it in, says whose device it would become (`pairingWhose`) and
 * which server adds it (`pairingWhere`): a link is written by whoever made
 * it, the server it names too, and the device that follows one acts as that
 * person from then on. Only this site's own server is asked who that person
 * is (`ofThisSite`). Only when its person goes on does it name
 * itself (`deviceLabel`: "Safari on iPhone"), show the code the holder derives
 * and wait; a refusal is said in plain words (`pairingFailure`).
 */

import {hex} from './hex.mjs';

/** How long a pairing link works. */
export const INVITE_TTL_MS = 10 * 60 * 1000;
/** Said on the new device when its link was answered before: a link works once. */
export const PAIRING_USED = 'This link was used already. Make a new one on your other device.';
/** Said on the new device when none of the person's devices answered while
 * it waited: the link ran out, or nobody looked. */
export const PAIRING_UNANSWERED = 'No device of yours approved this one in time. Make a new link on your other device, then approve this device there.';
/** Said on the new device when the person declined it. */
export const PAIRING_DECLINED = 'Your other device declined. Make a new link to try again.';
/** Said on the new device when the host would not take a request dated by
 * this device's clock. Hosts allow a minute between the two clocks
 * (`CERTIFICATION_CLOCK_ALLOWANCE_NS`, tilo-user-identity). The link is not
 * spent: it works once the clock is right. */
export const PAIRING_CLOCK = 'The clock of this device and the clock of your workspace’s host differ by more than a minute. Check the date and time on this device, then open the link again.';
/** Said in a browser that opens a pairing link for the person it already
 * is (`pairViaHost`, workspaces.mjs): a link opened in the browser that
 * made it. No host was asked, so the link still works on the device it was
 * made for. */
export const PAIRING_SAME_PERSON = 'This browser is already signed in as you, so nothing was paired. Open the link on the device you want to add.';

/** Said on a device whose person stopped, when it had said whose device it
 * would become or while it waited (`pairViaHost`, workspaces.mjs). It keeps
 * nothing; one that waited has left its page, so its request is gone too. */
export const PAIRING_STOPPED = 'Nothing was paired. This device was added to nobody’s devices.';

/**
 * What a device says before it asks to become a device of a person, under
 * the sentence that names them (`pairingWhose`): what follows from going
 * on, what it means for a browser that is somebody already (`already`), and
 * the two ways on. Stopping asks no host and spends no link.
 */
export const PAIRING_WHOSE = Object.freeze({
  heading: 'Add this device?',
  consequence: 'Whoever made this link approves it on a device of theirs. From then on this device acts as them, and what you do here is theirs.',
  already: 'This browser is signed in as someone else. Here it would act as the person above instead.',
  go: 'Add this device',
  stop: 'Stop',
});

/**
 * A person's name as a sentence of this page holds it: one line of at most
 * 64 characters, with nothing in it that turns the text around it (the
 * characters that set a direction) or breaks its line. Empty when there is
 * no name to show.
 */
export function shownName(name) {
  const plain = String(name ?? '').replace(/[\u200E\u200F\u061C\u202A-\u202E\u2066-\u2069]/gu, '')
    .replace(/[\p{Cc}\s]+/gu, ' ').trim();
  return Array.from(plain).slice(0, 64).join('').trim();
}

/**
 * Whether the server at `origin` is one this site names as its own: the one
 * the page came from (`page`, its address), the site's base, or the server
 * that keeps the site's home workspace (`entry`, the site's `entry.json`).
 * A link is written by whoever made it and names any server
 * (`#/@<server>/w/…`), its maker's own too. So nothing a link says, and
 * nothing this browser kept from an earlier visit, makes a server the
 * site's own.
 */
export function ofThisSite(origin, {page = globalThis.location?.href, entry = null} = {}) {
  const at = address => {
    try {
      return new URL(address).origin;
    } catch {
      return null;
    }
  };
  const asked = at(origin);
  // An address without an origin of its own (`about:blank`) reads as "null".
  if (asked === null || asked === 'null') return false;
  return [page, entry?.base, entry?.home?.host?.origin].some(own => at(own) === asked);
}

/**
 * Whose device this one becomes, in one sentence: said before it asks to be
 * let in and again on its waiting card. `name` is what the server that keeps
 * that person answers for the key the link names (their own profile there,
 * `fetchProfile`), never a word of the link's own; a server that names
 * nobody leaves the sentence to say so. Only this site's own server is
 * taken at its word (`ofSite`, `ofThisSite`): a server the link alone names
 * answers whatever name its keeper likes, the reader's own too, so for any
 * other server the sentence names nobody, whatever `name` holds. A name is
 * no proof of who made the link, since everyone chooses their own: the
 * sentence asks the one question its reader can answer, whether that is
 * them.
 */
export function pairingWhose(name, {ofSite = false} = {}) {
  if (ofSite !== true) return 'You are adding this device to the devices of whoever made this link. If you did not make it yourself, stop.';
  const shown = shownName(name);
  return shown
    ? `You are adding this device to ${shown}’s devices. If that is not you, stop.`
    : 'You are adding this device to the devices of someone this host has no name for. If you did not make this link yourself, stop.';
}

/**
 * Which server adds the device, said under the sentence that says whose it
 * becomes: its name as its address writes it (a name spelled with letters
 * of another script reads as `xn--…`), and whether it is this site's own
 * (`ofSite`, `ofThisSite`). The page stays under the site's address
 * whichever server a link names, so this line alone tells its reader where
 * the device goes: for a server that is not the site's own it says so, and
 * that what they type afterwards goes there and not to this site.
 */
export function pairingWhere(origin, {ofSite = false} = {}) {
  let server = '';
  try {
    server = new URL(origin).host;
  } catch {
    // Said as what it is below: a server only the link names.
  }
  return ofSite === true && server
    ? `The server that adds it is this site’s own, ${server}.`
    : `This device would join another server${server ? `, ${server},` : ','} which is not this site’s own. What you type here afterwards goes to that server, not to this site. Go on only if you know that server is yours.`;
}

/**
 * What a device that opened a pairing link shows around its code while it
 * waits: where the person approves it, and what to do when nothing asks them
 * there. Only the page that made a link asks about the device that answers
 * it, and only while that link is open (`forInvites`), so that page's device
 * is where the person is sent. "Pair a new device" is named with where it is
 * found: on the person's own card and in the desktop's search (`PAIR`,
 * identity-actions.mjs).
 */
export const PAIRING_WAIT = Object.freeze({
  heading: 'Waiting for approval on your other device…',
  where: 'The device that made this link now asks you about this one. Approve there if it shows this code:',
  then: 'This page goes on by itself once you approve.',
  otherwise: 'Nothing asking there? Its link was made more than 10 minutes ago, or its page was closed. On a device where you are signed in, choose “Pair a new device” on your own card (your picture, at the top right) or in search, and open the new link here.',
});

/** What a tab its person signed in with their agent says while its
 * pairing goes through (`ownPairing`): nobody is asked, so it names no code
 * and no device to approve on. */
export const PAIRING_SIGNING_IN = 'Signing you in…';

/** What a tab says while it waits to become a new device of the person its
 * browser is paired to, having opened no link: its request names no invite,
 * so no page asks about it by itself, and a new link opened in the tab is
 * what gets it approved. */
export const pairingStatus = code => `Pairing code ${code}. This tab is a new device of yours and waits for your approval. On a device where you are signed in, approve ${code} where it asks. If nothing asks, choose “Pair a new device” there on your own card (your picture, at the top right) or in search, and open its link in this tab.`;

/** A new invite id: 16 random bytes as 32 hex characters. */
export function newInvite(random = bytes => globalThis.crypto.getRandomValues(bytes)) {
  return hex(random(new Uint8Array(16)));
}

/** The `pair` secret of a link: the person, then the invite if any. */
export const pairSecret = (userPublicKey, invite = null) => `${userPublicKey}${invite ?? ''}`;

/** Where a tab keeps the pairing it opens for itself (`askOwnPairing`). */
const OWN_PAIRING = 'tilo-own-pairing';

/**
 * Kept by a tab about to open a pairing link it made itself: the person
 * signed in here with their agent, whose device claimed this tab's sign-in
 * code and named the person and the invite (`pair`, its secret). The person
 * asked for this pairing on this very page a moment ago, so the page it
 * loads asks them nothing about whose device this becomes
 * (`ownPairing`). It is kept for this tab alone (`sessionStorage`), which
 * no link and no other page writes.
 */
export function askOwnPairing(userPublicKey, invite, storage = globalThis.sessionStorage) {
  try {
    storage?.setItem(OWN_PAIRING, pairSecret(userPublicKey, invite));
  } catch {
    // Not kept: the page asks whose device this becomes, as for any link.
  }
}

/**
 * Whether the pairing link this page opened, for `userPublicKey` with
 * `invite`, is the one this tab asked for itself (`askOwnPairing`), and not
 * a link someone handed its reader. Read once: what was kept is gone after,
 * so a later link, even one naming the same person, is asked about again.
 */
export function ownPairing(userPublicKey, invite, storage = globalThis.sessionStorage) {
  let kept = null;
  try {
    kept = storage?.getItem(OWN_PAIRING) ?? null;
    storage?.removeItem(OWN_PAIRING);
  } catch {
    return false;
  }
  return kept !== null && kept === pairSecret(userPublicKey, invite);
}

/** `{userPublicKey, invite}` (invite `null` when none) of a link's `pair`
 * secret, or `null` when it is neither 64 nor 96 lowercase hex. */
export function parsePairSecret(secret) {
  const match = /^([0-9a-f]{64})([0-9a-f]{32})?$/.exec(secret ?? '');
  return match ? {userPublicKey: match[1], invite: match[2] ?? null} : null;
}

/**
 * The invites this page made and has not spent, each open until `ttl`
 * after it was made. `open()` makes one; `spend(id)` closes it for good
 * (its request was answered); `prune()` drops the expired ones and
 * returns their ids.
 */
export function openInvites({now = () => Date.now(), ttl = INVITE_TTL_MS, random} = {}) {
  const open = new Map();
  return {
    open() {
      const id = newInvite(random);
      const invite = {id, expiresAt: now() + ttl};
      open.set(id, invite);
      return invite;
    },
    has: id => open.has(id) && open.get(id).expiresAt > now(),
    spend: id => open.delete(id),
    prune() {
      const expired = [...open.values()].filter(invite => invite.expiresAt <= now()).map(invite => invite.id);
      for (const id of expired) open.delete(id);
      return expired;
    },
    get size() { return open.size; },
  };
}

/** The waiting requests (`PairingHost.pending`) that answer one of `invites`:
 * the first per invite, since an invite admits one device. */
export function forInvites(pending, invites) {
  const seen = new Set();
  return pending.filter(request => {
    if (!request.invite || !invites.has(request.invite) || seen.has(request.invite)) return false;
    seen.add(request.invite);
    return true;
  });
}

/**
 * The address that hands a pairing link to Tilo Assistant on the device this
 * page is open on: `tilo-assistant://pair?link=<the link>`, every character
 * of the link written out, so nothing of it ends the address. The app takes
 * such an address only while it stands at its way in, and then as a pasted
 * link: it says whose devices it would join and asks its person before
 * anyone is asked to approve. `null` where there is no Tilo Assistant (it is
 * an app for the Mac, the iPhone and the iPad; the system is read as
 * `deviceLabel` reads it), and for anything that is no web address.
 */
export function assistantAddress(link, navigator = globalThis.navigator ?? {}) {
  if (typeof link !== 'string' || !/^https?:\/\/\S+$/.test(link)) return null;
  const agent = navigator.userAgent ?? '';
  const system = PLATFORMS[navigator.userAgentData?.platform] ?? navigator.userAgentData?.platform
    ?? SYSTEMS.find(([pattern]) => pattern.test(agent))?.[1] ?? '';
  if (!['Mac', 'iPhone', 'iPad'].includes(system)) return null;
  return `tilo-assistant://pair?link=${encodeURIComponent(link)}`;
}

const BROWSERS = [
  [/EdgA?\/|EdgiOS\//, 'Edge'], [/OPR\/|OPiOS\//, 'Opera'], [/SamsungBrowser\//, 'Samsung Internet'],
  [/Firefox\/|FxiOS\//, 'Firefox'], [/Chrome\/|CriOS\/|Chromium\//, 'Chrome'], [/Safari\//, 'Safari'],
];
// The brands browsers name in `userAgentData`, as people call them.
const BRANDS = {'Google Chrome': 'Chrome', 'Microsoft Edge': 'Edge', Opera: 'Opera', Brave: 'Brave', Chromium: 'Chromium',
  'Samsung Internet': 'Samsung Internet', 'Vivaldi': 'Vivaldi', 'YaBrowser': 'Yandex'};
const SYSTEMS = [
  [/iPhone|iPod/, 'iPhone'], [/iPad/, 'iPad'], [/Android/, 'Android'], [/CrOS/, 'ChromeOS'],
  [/Windows/, 'Windows'], [/Mac OS X|Macintosh/, 'Mac'], [/Linux|X11/, 'Linux'],
];
const PLATFORMS = {macOS: 'Mac', 'Chrome OS': 'ChromeOS', 'Chromium OS': 'ChromeOS'};

/**
 * What a new device is called where the person approves it: the browser
 * on the system, "Safari on iPhone", "Firefox on Linux"; at most 64
 * characters. Read from `userAgentData` where the browser has it, else
 * from its user agent string. A native app that holds this page as its
 * person's device names the device itself (`tiloNativeHost.name`):
 * "Tilo Assistant on Ada's Mac".
 */
export function deviceLabel(navigator = globalThis.navigator ?? {}, native = globalThis.tiloNativeHost) {
  const named = typeof native?.name === 'string'
    ? native.name.replace(/[\u0000-\u001f\u007f\u2028\u2029]+/g, ' ').trim().slice(0, 64) : '';
  if (named) return named;
  const agent = navigator.userAgent ?? '';
  const brands = navigator.userAgentData?.brands ?? [];
  const browser = brands.map(entry => BRANDS[entry.brand]).find(name => name && name !== 'Chromium')
    ?? BROWSERS.find(([pattern]) => pattern.test(agent))?.[1]
    ?? brands.map(entry => BRANDS[entry.brand]).find(Boolean)
    ?? 'Browser';
  let system = PLATFORMS[navigator.userAgentData?.platform] ?? navigator.userAgentData?.platform
    ?? SYSTEMS.find(([pattern]) => pattern.test(agent))?.[1] ?? navigator.platform ?? '';
  // An iPad asks for desktop sites as a Mac; only its touch screen tells.
  if (system === 'Mac' && navigator.maxTouchPoints > 1) system = 'iPad';
  return (system ? `${browser} on ${system}` : browser).slice(0, 64);
}

/** When a request was made, said relative to `now`: "just now", "2 minutes ago". */
export function whenRequested(requestedAtMs, {now = Date.now(), locale} = {}) {
  const seconds = Math.round((now - requestedAtMs) / 1000);
  if (!Number.isFinite(seconds) || seconds < 45) return 'just now';
  const format = new Intl.RelativeTimeFormat(locale, {numeric: 'auto'});
  const minutes = Math.round(seconds / 60);
  return minutes < 60 ? format.format(-minutes, 'minute') : format.format(-Math.round(minutes / 60), 'hour');
}

/** What to tell the new device when pairing failed with `error`, or `null`
 * when it is neither the host's answer to the pairing nor this browser's
 * own (a network failure). Each refusal is said as what happened, never as
 * another. */
export function pairingFailure(error) {
  // This browser's own answer, before any host was asked.
  if (error?.code === 'same-person') return PAIRING_SAME_PERSON;
  // Its person's own: they stopped.
  if (error?.code === 'stopped') return PAIRING_STOPPED;
  const text = String(error?.message ?? error ?? '');
  if (/declined/i.test(text)) return PAIRING_DECLINED;
  if (/used already/i.test(text)) return PAIRING_USED;
  // The new device dates its request and the host reads it a moment later.
  // One the host calls dated ahead, or expired as it arrives, was dated by a
  // clock that differs from the host's: the link itself has no date the host
  // reads. Hosts before the allowance said "expired or not yet valid" of both.
  if (/certification request (is|has) [^:]*(dated|expired|not yet valid)/i.test(text)) return PAIRING_CLOCK;
  if (/no approv\w* (device )?answered/i.test(text)) return PAIRING_UNANSWERED;
  return null;
}
