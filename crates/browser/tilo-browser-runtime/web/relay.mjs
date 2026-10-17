/**
 * The page as a certified device of one tilo-relay room, in one call:
 * `connectRelayPeer` opens this profile's device key (`device-custody.mjs`),
 * loads the runtime and connects `peer.mjs`'s certified peer to `room`.
 * Pages written for the relay peer of the runtime this one replaced
 * (`tilo-room-service`'s `rooms.mjs`, the site) keep working unchanged:
 *
 * - `whenReady()`, `records()`, `hasRecord()`, `whenRecord()`: the room's
 *   admission and the records this page holds;
 * - `mountDesktop(target, options)`: `host.mjs`'s mount in the room; its
 *   records replicate through the peer, and it publishes this device's
 *   pointer and caret (or, with `agent`, the agent's) while it is the mount
 *   acted in last;
 * - `live`, `agentLive(agent)`: the room's live layers.
 *
 * A room on a loopback `http:` origin (local review, `tilo-relay room
 * --web-plain`) is accepted: the page then needs no certificate exception.
 */
import * as wasm from './pkg/tilo_browser_runtime.js';
import * as host from './host.mjs';
import {connectPeer} from './peer.mjs';
import {idbStorage, openDeviceCustody} from './device-custody.mjs';

/**
 * Connect this page to relay room `room` at `origin` as a device of the
 * room's user root `userPublicKey` (hex). `onPairingCode(code)` is called
 * while the host waits for an approver; a page with the certificate from an
 * earlier visit skips that. Resolves once the session started (observe
 * `getSnapshot().phase`, or `whenReady()`).
 */
export async function connectRelayPeer({
  origin, userPublicKey, room, label = 'Browser', profile = `tilo-relay:${room}`, device = 'relay',
  displayName = label, onPairingCode, onStatus, signal, fetch: fetcher, live = true,
}) {
  if (!origin || !userPublicKey || !room) throw new TypeError('origin, userPublicKey and room are required');
  signal?.throwIfAborted();
  const custody = await openDeviceCustody({profile, storage: idbStorage()});
  // Each device key of the profile (one per tab open at once) is a device of
  // its own, with a namespace of its own.
  if (custody.slot) device = `${device}#${custody.slot}`;
  const peer = await connectPeer({
    // The profile's storage opens through host.mjs, which loads the runtime
    // once for this page before the peer calls into it.
    origin, userPublicKey, room, label, profile, device, displayName, custody, runtime: wasm, desktop: host,
    onPairingCode, onStatus, signal, fetch: fetcher, live,
  });
  return Object.create(peer, {
    room: {value: room, enumerable: true},
    /** This peer in the runtime (`WasmPeer.id`). */
    id: {get: () => peer.runtimeId, enumerable: true},
  });
}
