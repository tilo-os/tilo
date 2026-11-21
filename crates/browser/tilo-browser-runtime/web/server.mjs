/**
 * The page as a certified device of one tilo-server workspace, in one call:
 * `connectServerPeer` opens this profile's device key (`device-key.mjs`),
 * loads the runtime and connects `peer.mjs`'s certified peer to `workspace`.
 * The peer's `whenReady()` resolves once the host admitted this page to the
 * workspace, and `live` is the workspace's live layer.
 *
 * A workspace on a loopback `http:` origin (local review, `tilo-server workspace
 * --web-plain`) is accepted: the page then needs no certificate exception.
 */
import * as host from './host.mjs';
import {connectPeer} from './peer.mjs';
import {idbStorage, openDeviceKey} from './device-key.mjs';

/**
 * Connect this page to relay workspace `workspace` at `origin` as a device of the
 * workspace's user root `userPublicKey` (hex). `onPairingCode(code)` is called
 * while the host waits for an approver; a page with the certificate from an
 * earlier visit skips that. Resolves once the session started (observe
 * `getSnapshot().phase`, or `whenReady()`).
 */
export async function connectServerPeer({
  origin, userPublicKey, workspace, label = 'Browser', profile = `tilo-server:${workspace}`, device = 'server',
  displayName = '', onPairingCode, onStatus, signal, fetch: fetcher, live = true,
}) {
  if (!origin || !userPublicKey || !workspace) throw new TypeError('origin, userPublicKey and workspace are required');
  signal?.throwIfAborted();
  const deviceKey = await openDeviceKey({profile, storage: idbStorage()});
  // Each device key of the profile (one per tab open at once) is a device of
  // its own, with a namespace of its own.
  if (deviceKey.slot) device = `${device}#${deviceKey.slot}`;
  const peer = await connectPeer({
    // The runtime, with the peer's code, and the profile's storage come
    // through host.mjs, which loads them once for this page.
    origin, userPublicKey, workspace, label, profile, device, displayName, deviceKey, desktop: host,
    onPairingCode, onStatus, signal, fetch: fetcher, live,
  });
  return Object.create(peer, {
    workspace: {value: workspace, enumerable: true},
    /** This peer in the runtime (`WasmPeer.id`). */
    id: {get: () => peer.runtimeId, enumerable: true},
  });
}
