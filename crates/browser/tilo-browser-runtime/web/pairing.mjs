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
/** Said on the new device when the host no longer takes its link. */
export const PAIRING_EXPIRED = 'This link expired or was used already. Make a new one on your other device.';
/** Said on the new device when the person declined it. */
export const PAIRING_DECLINED = 'Your other device declined. Make a new link to try again.';

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

/** The waiting requests (`pendingPairings`) that answer one of `invites`:
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

/** The name of the country `code` (ISO 3166 alpha-2) in `locale`, or the
 * code where the browser cannot name it; `null` without one. */
export function countryName(code, {locale} = {}) {
  if (!code) return null;
  try {
    return new Intl.DisplayNames(locale, {type: 'region'}).of(code.toUpperCase()) ?? code;
  } catch {
    return code;
  }
}

/** What to tell the new device when pairing failed with `error`, or `null`
 * when it is not the host's answer to the pairing (a network failure). */
export function pairingFailure(error) {
  const text = String(error?.message ?? error ?? '');
  if (/declined/i.test(text)) return PAIRING_DECLINED;
  if (/used already|no approv\w* (device )?answered|expired/i.test(text)) return PAIRING_EXPIRED;
  return null;
}
