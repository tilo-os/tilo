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
 * The new device reads the link (`parsePairSecret`), names itself
 * (`deviceLabel`: "Safari on iPhone"), shows the code the host derives and
 * waits; a refusal is said in plain words (`pairingFailure`).
 */

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

const hex = bytes => Array.from(bytes, byte => byte.toString(16).padStart(2, '0')).join('');

/** A new invite id: 16 random bytes as 32 hex characters. */
export function newInvite(random = bytes => globalThis.crypto.getRandomValues(bytes)) {
  return hex(random(new Uint8Array(16)));
}

/** The `pair` secret of a link: the person, then the invite if any. */
export const pairSecret = (userPublicKey, invite = null) => `${userPublicKey}${invite ?? ''}`;

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
    /** An invite this page made before it was loaded again (`{id, expiresAt}`), open again until it expires. */
    adopt({id, expiresAt}) {
      if (typeof id !== 'string' || !id || !(expiresAt > now())) return null;
      const invite = {id, expiresAt};
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
 * from its user agent string.
 */
export function deviceLabel(navigator = globalThis.navigator ?? {}) {
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
