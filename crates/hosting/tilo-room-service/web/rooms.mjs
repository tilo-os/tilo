// Short-lived shared rooms (tilo-room-service) for a page running the web
// core: the core with the relay peer and every app come from the signed
// registry, verified in the page; the room carries only document commits,
// certification and presence, and each browser renders everything itself.
// It sits next to host.mjs and relay.mjs (the web runtime's page modules).
import {configureRuntime, setAppResolver} from './host.mjs';
import {connectRelayPeer} from './relay.mjs';
import init, * as bindings from './registry_verifier.js';
import {createVerifier} from './registry-verifier.mjs';
import {createRegistryClient} from './registry-client.mjs';

export const WRITER = 'so.tilo.word-processor';
export const BOOKS = 'so.tilo.ebook';
const RELAY_RUNTIME = 'so.tilo.runtime.web-relay';

/**
 * Run the core with the relay peer and link apps from the signed registry
 * `config` names (`{baseUrl, ownerKeyHex, runtimeAbi, relayRuntimePackage}`,
 * by default ./registry.json next to this module). Call before the first
 * mount. Resolves to the registry client.
 */
export async function useRegistry(config) {
  config ??= await (await fetch(new URL('./registry.json', import.meta.url), {cache: 'no-store'})).json();
  await init({module_or_path: new URL('./registry_verifier_bg.wasm', import.meta.url)});
  const registry = createRegistryClient({
    baseUrl: config.baseUrl, ownerKeyHex: config.ownerKeyHex, runtimeAbi: config.runtimeAbi,
    runtimePackage: config.relayRuntimePackage ?? RELAY_RUNTIME, verifier: createVerifier(bindings),
  });
  const core = registry.resolveRuntime();
  const glue = core.then(({assets}) => {
    const name = [...assets.keys()].find(key => key.endsWith('tilo_browser_runtime.js'));
    if (!name) throw new Error('the registry core carries no glue asset');
    return assets.get(name);
  });
  configureRuntime({glue, module: core.then(({module}) => module)});
  setAppResolver(registry.resolveApp);
  return registry;
}

/** `POST <api>/rooms`: `{roomId, origin, room, userPublicKey, expiresAt,
 * pageTicket, joinUrl, admit, events, invitations, app}`. */
export async function createRoom(api, {signal} = {}) {
  const response = await fetch(new URL('rooms', `${api.replace(/\/$/, '')}/`), {method: 'POST', signal});
  if (!response.ok) throw new Error((await response.json().catch(() => ({}))).error ?? `rooms: HTTP ${response.status}`);
  return response.json();
}

/** A one-use guest link for `room`; with `record`, it opens that record full screen. */
export async function createInvitation(room, {record, signal} = {}) {
  const response = await fetch(room.invitations, {method: 'POST', signal});
  if (!response.ok) throw new Error((await response.json().catch(() => ({}))).error ?? `invitations: HTTP ${response.status}`);
  const {joinUrl} = await response.json();
  return record ? `${joinUrl}&record=${encodeURIComponent(record)}` : joinUrl;
}

/** What a join link names: `{origin, userPublicKey, room, ticket, admit, record}`. */
export function parseJoinLink(hash = location.hash) {
  const params = new URLSearchParams(hash.replace(/^#/, ''));
  const value = name => params.get(name) ?? undefined;
  return {origin: value('origin'), userPublicKey: value('user'), room: value('room'),
    ticket: value('ticket'), admit: value('admit'), record: value('record')};
}

/**
 * Join the room as a device of its user root: a fresh device key in this
 * browser, certified by the room once the service has spent `ticket` on the
 * pairing code this page shows. Resolves to relay.mjs's peer
 * (`whenReady`, `mountDesktop`, `records`, `whenRecord`, …).
 */
export function joinRoom({origin, userPublicKey, room, ticket, admit, label = 'Browser', signal, onStatus}) {
  if (!admit) throw new TypeError('admit is required');
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
 * Returns a function that stops watching. */
export function watchMembers(room, onMember, {onExpired} = {}) {
  const events = new EventSource(room.events);
  events.addEventListener('member', event => onMember(JSON.parse(event.data)));
  events.addEventListener('expired', () => { events.close(); onExpired?.(); });
  return () => events.close();
}
