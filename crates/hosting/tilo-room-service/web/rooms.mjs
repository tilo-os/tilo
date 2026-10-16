// Short-lived shared rooms (tilo-room-service) for a page running the
// browser runtime bundle (`build.sh --bundle desktop`: the runtime with its
// certified peer and every web app); the room carries only document commits,
// certification and presence, and each browser renders everything itself.
// It sits next to the bundle's host.mjs and relay.mjs.
import {connectRelayPeer} from './relay.mjs';

export const WRITER = 'so.tilo.word-processor';
export const BOOKS = 'so.tilo.ebook';

/**
 * Kept for pages written for registry-loaded cores: the bundle next to this
 * module already carries the runtime and every web app, so there is nothing
 * to resolve. Resolves to `null`.
 */
export async function useRegistry() {
  return null;
}

/** `POST <api>/rooms`: `{roomId, origin, room, userPublicKey, expiresAt,
 * pageTicket, joinUrl, admit, events, invitations, app}`. `invitations` and
 * `events` carry the room's owner key: keep them to this page, and use them
 * as given (the service refuses both routes without the key). */
export async function createRoom(api, {signal} = {}) {
  const response = await fetch(new URL('rooms', `${api.replace(/\/$/, '')}/`), {method: 'POST', signal});
  if (!response.ok) throw new Error((await response.json().catch(() => ({}))).error ?? `rooms: HTTP ${response.status}`);
  return response.json();
}

/** A one-use guest link for `room` (the `createRoom` answer), in the web link grammar
 * (docs/architecture/web-routing.md): the Writer full tab, on `record` when
 * given, with the guest ticket as the fragment's secret. */
export async function createInvitation(room, {record, signal} = {}) {
  const response = await fetch(room.invitations, {
    method: 'POST', headers: {'content-type': 'application/json'},
    body: JSON.stringify(record ? {record} : {}), signal,
  });
  if (!response.ok) throw new Error((await response.json().catch(() => ({}))).error ?? `invitations: HTTP ${response.status}`);
  return (await response.json()).joinUrl;
}

/**
 * Join the room as a device of its user root: a fresh device key in this
 * browser, certified by the room once the service has spent `ticket` on the
 * pairing code this page shows. Resolves to relay.mjs's peer
 * (`whenReady`, `mountDesktop`, `records`, `whenRecord`, …).
 */
export async function joinRoom({origin, userPublicKey, room, ticket, admit, label = 'Browser', signal, onStatus}) {
  if (!admit) throw new TypeError('admit is required');
  await untilReachable(origin, {signal});
  return connectRelayPeer({
    origin, userPublicKey, room, label, signal, onStatus,
    onPairingCode: async pairingCode => {
      const response = await fetch(admit, {
        method: 'POST', headers: {'content-type': 'application/json'},
        body: JSON.stringify({ticket, pairingCode}), signal,
      });
      if (!response.ok) onStatus?.({phase: 'failed', error: (await response.json().catch(() => ({}))).error ?? 'the room refused this device'});
    },
  });
}

/** Resolves once the room's relay answers: a room that just started (or a
 * port forward that has not noticed it yet) refuses connections briefly. */
async function untilReachable(origin, {signal, timeoutMs = 15_000} = {}) {
  const deadline = Date.now() + timeoutMs;
  for (let delay = 100; ; delay = Math.min(delay * 2, 1000)) {
    try {
      const response = await fetch(new URL('/peer/v1/host', origin), {method: 'POST', credentials: 'omit', cache: 'no-store', signal});
      if (response.ok) return;
    } catch (error) {
      if (signal?.aborted) throw error;
    }
    if (Date.now() + delay > deadline) return;
    await new Promise(resolve => setTimeout(resolve, delay));
  }
}

/** The room's record of `app` once this page holds one. */
export function recordOf(peer, app = WRITER, {timeoutMs = 30_000} = {}) {
  const find = () => peer.records().find(row => row.app === app)?.record;
  return new Promise((resolve, reject) => {
    const found = find();
    if (found) { resolve(found); return; }
    const deadline = Date.now() + timeoutMs;
    const timer = setInterval(() => {
      const record = find();
      if (record) { clearInterval(timer); resolve(record); }
      else if (Date.now() > deadline) { clearInterval(timer); reject(new Error(`no ${app} record in the room`)); }
    }, 100);
  });
}

/** Each device the room certifies, earlier ones first: `{label, device, at}`.
 * `room` is the `createRoom` answer, whose `events` URL is the owner's.
 * Returns a function that stops watching. */
export function watchMembers(room, onMember, {onExpired} = {}) {
  const events = new EventSource(room.events);
  events.addEventListener('member', event => onMember(JSON.parse(event.data)));
  events.addEventListener('expired', () => { events.close(); onExpired?.(); });
  return () => events.close();
}
