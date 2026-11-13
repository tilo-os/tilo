/**
 * The page as a certified device of one tilo-relay workspace, in one call:
 * `connectRelayPeer` opens this profile's device key (`device-custody.mjs`),
 * loads the runtime and connects `peer.mjs`'s certified peer to `workspace`.
 * Pages written for the relay peer of the runtime this one replaced keep
 * working unchanged:
 *
 * - `whenReady()`, `records()`, `hasRecord()`, `whenRecord()`: the workspace's
 *   admission and the records this page holds;
 * - `mountDesktop(target, options)`: `host.mjs`'s mount in the workspace; its
 *   records replicate through the peer, and it publishes this device's
 *   pointer and caret while it is the mount acted in last;
 * - `live`: the workspace's live layer.
 *
 * A workspace on a loopback `http:` origin (local review, `tilo-relay room
 * --web-plain`) is accepted: the page then needs no certificate exception.
 */
import * as host from './host.mjs';
import {connectPeer} from './peer.mjs';
import {idbStorage, openDeviceCustody} from './device-custody.mjs';

/**
 * Connect this page to relay workspace `workspace` at `origin` as a device of the
 * workspace's user root `userPublicKey` (hex). `onPairingCode(code)` is called
 * while the host waits for an approver; a page with the certificate from an
 * earlier visit skips that. Resolves once the session started (observe
 * `getSnapshot().phase`, or `whenReady()`).
 */
export async function connectRelayPeer({
  origin, userPublicKey, workspace, label = 'Browser', profile = `tilo-relay:${workspace}`, device = 'relay',
  displayName = label, onPairingCode, onStatus, signal, fetch: fetcher, live = true,
}) {
  if (!origin || !userPublicKey || !workspace) throw new TypeError('origin, userPublicKey and workspace are required');
  signal?.throwIfAborted();
  const custody = await openDeviceCustody({profile, storage: idbStorage()});
  // Each device key of the profile (one per tab open at once) is a device of
  // its own, with a namespace of its own.
  if (custody.slot) device = `${device}#${custody.slot}`;
  const peer = await connectPeer({
    // The runtime, with the peer's code, and the profile's storage come
    // through host.mjs, which loads them once for this page.
    origin, userPublicKey, workspace, label, profile, device, displayName, custody, desktop: host,
    onPairingCode, onStatus, signal, fetch: fetcher, live,
  });
  return Object.create(peer, {
    workspace: {value: workspace, enumerable: true},
    /** This peer in the runtime (`WasmPeer.id`). */
    id: {get: () => peer.runtimeId, enumerable: true},
  });
}
