// Short-lived shared rooms (tilo-room-service), for a page next to the
// browser runtime bundle: create a room, join it as a certified device, and
// watch who joins. A page imports it from the service (`<api>/app/rooms.mjs`)
// or from its own copy of the bundle.
import init, * as runtime from './pkg/tilo_browser_runtime.js';
import {connectPeer} from './peer.mjs';
import {idbStorage, openDeviceCustody} from './device-custody.mjs';

export const WRITER = 'so.tilo.word-processor';

/** `POST <api>/rooms`: `{roomId, origin, room, userPublicKey, expiresAt,
 * pageTicket, joinUrl, admit, events, invitations, app}`. */
export async function createRoom(api, {signal} = {}) {
  const response = await fetch(new URL('rooms', `${api.replace(/\/$/, '')}/`), {method: 'POST', signal});
  if (!response.ok) throw new Error((await response.json().catch(() => ({}))).error ?? `rooms: HTTP ${response.status}`);
  return response.json();
}

/** A guest join link for `room` (`POST <room.invitations>`); with `record`,
 * the link opens that record full screen. */
export async function createInvitation(room, {record, signal} = {}) {
  const response = await fetch(room.invitations, {method: 'POST', signal});
  if (!response.ok) throw new Error((await response.json().catch(() => ({}))).error ?? `invitations: HTTP ${response.status}`);
  const {joinUrl} = await response.json();
  return record ? `${joinUrl}&record=${encodeURIComponent(record)}` : joinUrl;
}

/** The room a join link names: `{origin, userPublicKey, room, ticket, admit, record}`. */
export function parseJoinLink(hash = location.hash) {
  const params = new URLSearchParams(hash.replace(/^#/, ''));
  const value = name => params.get(name) ?? undefined;
  return {origin: value('origin'), userPublicKey: value('user'), room: value('room'),
    ticket: value('ticket'), admit: value('admit'), record: value('record')};
}

/**
 * Join the room as a device of its user root: a fresh device key kept in
 * this browser (profile `tilo-room:<room>`), certified by the room once the
 * service has spent `ticket` on the pairing code this page shows. Resolves
 * to the `CertifiedPeer` (`peer.mjs`); a reload joins again with the same
 * certificate.
 */
export async function joinRoom({origin, userPublicKey, room, ticket, admit, label = 'Browser', displayName = '', signal, onStatus}) {
  if (!origin || !userPublicKey || !room || !admit) throw new TypeError('origin, userPublicKey, room and admit are required');
  await init();
  const profile = `tilo-room:${room}`;
  const custody = await openDeviceCustody({profile, storage: idbStorage()});
  return connectPeer({
    origin, userPublicKey, room, label, displayName, custody, profile, runtime, signal, onStatus,
    onPairingCode: async pairingCode => {
      const response = await fetch(admit, {
        method: 'POST', headers: {'content-type': 'application/json'},
        body: JSON.stringify({ticket, pairingCode}), signal,
      });
      if (!response.ok) onStatus?.({phase: 'failed', error: (await response.json().catch(() => ({}))).error ?? 'The room refused this device'});
    },
  });
}

/** Resolves once `peer` is ready, rejects if it fails. */
export function whenReady(peer) {
  return new Promise((resolve, reject) => {
    const check = snapshot => {
      if (snapshot.phase === 'ready') { stop(); resolve(snapshot); }
      else if (snapshot.phase === 'failed') { stop(); reject(new Error(snapshot.error ?? 'The room refused this device')); }
    };
    const stop = peer.subscribe(check);
    check(peer.getSnapshot());
  });
}

/** The room's record of `app` once the peer knows one. */
export function recordOf(peer, app = WRITER, {timeoutMs = 30_000} = {}) {
  const find = snapshot => snapshot.health?.records?.find(row => row.record?.appId === app)?.record?.recordId;
  return new Promise((resolve, reject) => {
    const found = find(peer.getSnapshot());
    if (found) { resolve(found); return; }
    const timer = setTimeout(() => { stop(); reject(new Error(`no ${app} record in the room`)); }, timeoutMs);
    const stop = peer.subscribe(snapshot => {
      const record = find(snapshot);
      if (record) { clearTimeout(timer); stop(); resolve(record); }
    });
  });
}

/** Each device the room certifies, earlier ones first: `{label, device, at}`.
 * Returns a function that stops watching. */
export function watchMembers(room, onMember, {onExpired} = {}) {
  const events = new EventSource(room.events);
  events.addEventListener('member', event => onMember(JSON.parse(event.data)));
  events.addEventListener('expired', () => { events.close(); onExpired?.(); });
  return () => events.close();
}
